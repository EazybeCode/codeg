#!/usr/bin/env node
// Unattended NON-CORE upstream updater.
//
// Daily: fetch pinned upstream branch into a dedicated bare repository, evaluate
// each new upstream commit whole, cherry-pick only presentation-only commits onto
// the exact currently released SHA inside a throwaway worktree, run the trusted
// validation and build hooks from local config, confirm every non-allowlisted
// path is byte-identical, check the active-runs gate, then deploy with rollback.
//
// It never pushes, never uses SSH, never edits an active checkout, never runs
// upstream-defined hooks or scripts, and never evaluates strings as commands.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { allowlistDigest, classifyChanges, matchesAllowlist, validateManifest } from './policy.mjs';
import { makeGit, verifyCommit, isAncestor, rawChanges, SHA_RE } from './git.mjs';
import { pendingCommits, walkCommits, contentRejections } from './evaluate.mjs';
import { escapeMarkdown } from './discover.mjs';

export const EXIT = Object.freeze({ ok: 0, error: 1, blocked: 2, locked: 3, halted: 4 });
const STATE_SCHEMA = 1;
const RUN_ID_RE = /^\d{8}T\d{6}Z-\d+$/;
const MANIFEST_PATH = '.github/workos-upstream.json';
const DEPLOYED_REF = 'refs/workos-upstream/deployed';
const OBSERVED_REF = 'refs/workos-upstream/observed';
const MAX_LEDGER = 5000;
const MAX_DEPLOYMENTS = 100;
const DEFAULT_LIMITS = Object.freeze({
  maxRangeCommits: 300, reportRetention: 30, maxLogBytes: 256 * 1024,
  hookTimeoutSeconds: 3600,
});

// ---------------------------------------------------------------- config

const CONFIG_KEYS = new Set(['schemaVersion', 'mode', 'repoPath', 'stateDir', 'reportsDir',
  'worktreesDir', 'upstream', 'policy', 'limits', 'committer', 'hookEnv', 'hooks']);
const HOOK_KEYS = new Set(['currentRelease', 'validate', 'build', 'activeRunsGate', 'deploy',
  'healthCheck', 'rollback']);
const DEPLOY_REQUIRED = ['activeRunsGate', 'deploy', 'healthCheck', 'rollback'];

function configError(message) {
  const error = new Error(`Invalid updater config: ${message}`);
  error.code = 'CONFIG';
  return error;
}

function isInside(child, parent) {
  const rel = path.relative(parent, child);
  return rel === '' || (!rel.startsWith('..') && !path.isAbsolute(rel));
}

function validateHook(name, hook, forbiddenRoots) {
  if (!hook || typeof hook !== 'object' || Array.isArray(hook)) throw configError(`${name} must be an object`);
  for (const key of Object.keys(hook)) {
    if (!['name', 'argv', 'timeoutSeconds'].includes(key)) throw configError(`${name} has unknown key ${key}`);
  }
  const { argv } = hook;
  if (!Array.isArray(argv) || argv.length === 0 || argv.length > 64
    || !argv.every((a) => typeof a === 'string' && a.length <= 4096 && !a.includes('\0'))) {
    throw configError(`${name}.argv must be a nonempty array of strings`);
  }
  if (!path.isAbsolute(argv[0]) || path.normalize(argv[0]) !== argv[0]) {
    throw configError(`${name}.argv[0] must be an absolute normalized executable path`);
  }
  if (forbiddenRoots.some((root) => isInside(argv[0], root))) {
    throw configError(`${name}.argv[0] must not live in the updater repository, worktrees or state`);
  }
  if (hook.timeoutSeconds !== undefined
    && !(Number.isInteger(hook.timeoutSeconds) && hook.timeoutSeconds > 0 && hook.timeoutSeconds <= 6 * 3600)) {
    throw configError(`${name}.timeoutSeconds must be an integer between 1 and 21600`);
  }
  if (hook.name !== undefined && !/^[a-z0-9][a-z0-9-]{0,40}$/.test(hook.name)) {
    throw configError(`${name}.name must be a short lowercase slug`);
  }
  return { ...hook, name: hook.name ?? name.replace(/[^a-z0-9-]/gi, '-').toLowerCase() };
}

export function loadConfig(configPath) {
  if (!path.isAbsolute(configPath)) throw configError('config path must be absolute');
  const stat = fs.statSync(configPath);
  if (!stat.isFile()) throw configError('config is not a regular file');
  if ((stat.mode & 0o022) !== 0) throw configError('config must not be group- or world-writable');
  const uid = typeof process.getuid === 'function' ? process.getuid() : stat.uid;
  if (stat.uid !== uid && stat.uid !== 0) throw configError('config must be owned by the service user or root');
  let raw;
  try { raw = JSON.parse(fs.readFileSync(configPath, 'utf8')); } catch { throw configError('not valid JSON'); }
  return validateConfig(raw);
}

export function validateConfig(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw configError('not an object');
  for (const key of Object.keys(raw)) if (!CONFIG_KEYS.has(key)) throw configError(`unknown key ${key}`);
  if (raw.schemaVersion !== 1) throw configError('schemaVersion must be 1');
  if (!['deploy', 'dry-run'].includes(raw.mode)) throw configError('mode must be deploy or dry-run');
  const dirs = ['repoPath', 'stateDir', 'reportsDir', 'worktreesDir'];
  for (const key of dirs) {
    if (typeof raw[key] !== 'string' || !path.isAbsolute(raw[key]) || path.normalize(raw[key]) !== raw[key]) {
      throw configError(`${key} must be an absolute normalized path`);
    }
  }
  for (const a of dirs) for (const b of dirs) {
    if (a !== b && isInside(raw[a], raw[b])) throw configError(`${a} must not be inside ${b}`);
  }
  const up = raw.upstream ?? {};
  if (Object.keys(up).some((k) => !['url', 'branch'].includes(k))) throw configError('upstream has unknown keys');
  if (up.branch !== 'main') throw configError('upstream.branch must be main');
  if (!(up.url === 'https://github.com/xintaofei/codeg.git' || (typeof up.url === 'string' && path.isAbsolute(up.url)))) {
    throw configError('upstream.url must be https://github.com/xintaofei/codeg.git or an absolute local mirror path');
  }
  if (raw.policy?.allowlistSha256 !== allowlistDigest() || Object.keys(raw.policy).length !== 1) {
    throw configError('policy.allowlistSha256 does not pin the trusted allowlist; review the policy change first');
  }
  const limits = { ...DEFAULT_LIMITS };
  for (const [key, value] of Object.entries(raw.limits ?? {})) {
    if (!(key in DEFAULT_LIMITS)) throw configError(`limits has unknown key ${key}`);
    if (!Number.isInteger(value) || value < 1 || value > 10_000_000) throw configError(`limits.${key} must be a positive integer`);
    limits[key] = value;
  }
  const committer = raw.committer ?? { name: 'Neev upstream updater', email: 'upstream-updater@neev.invalid' };
  if (typeof committer.name !== 'string' || typeof committer.email !== 'string'
    || !/^[\w .-]{1,80}$/.test(committer.name) || !/^[\w.+-]+@[\w.-]+$/.test(committer.email)) {
    throw configError('committer must have a plain name and email');
  }
  const hookEnv = raw.hookEnv ?? {};
  for (const [key, value] of Object.entries(hookEnv)) {
    if (!/^[A-Z_][A-Z0-9_]*$/.test(key) || key.startsWith('NEEV_UPSTREAM_') || key.startsWith('GIT_')
      || typeof value !== 'string' || value.includes('\0')) {
      throw configError(`hookEnv.${key} is not allowed`);
    }
  }
  const rawHooks = raw.hooks ?? {};
  for (const key of Object.keys(rawHooks)) if (!HOOK_KEYS.has(key)) throw configError(`hooks has unknown key ${key}`);
  const forbidden = [raw.repoPath, raw.worktreesDir, raw.stateDir, raw.reportsDir];
  const hooks = {};
  if (!rawHooks.currentRelease) throw configError('hooks.currentRelease is required');
  hooks.currentRelease = validateHook('currentRelease', rawHooks.currentRelease, forbidden);
  for (const list of ['validate', 'build']) {
    const value = rawHooks[list] ?? [];
    if (!Array.isArray(value)) throw configError(`hooks.${list} must be an array`);
    hooks[list] = value.map((hook, i) => validateHook(`${list}[${i}]`, hook, forbidden));
  }
  for (const key of DEPLOY_REQUIRED) {
    if (rawHooks[key]) hooks[key] = validateHook(key, rawHooks[key], forbidden);
  }
  if (raw.mode === 'deploy') {
    if (!hooks.validate.length || !hooks.build.length) throw configError('deploy mode requires validate and build hooks');
    for (const key of DEPLOY_REQUIRED) if (!hooks[key]) throw configError(`deploy mode requires hooks.${key}`);
  }
  return { ...raw, upstream: up, limits, committer, hookEnv, hooks };
}

// ---------------------------------------------------------------- state and lock

function writeJsonAtomic(file, value) {
  const tmp = `${file}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2) + '\n', { mode: 0o600 });
  fs.renameSync(tmp, file);
}

function emptyState() {
  return { schemaVersion: STATE_SCHEMA, baseline: null, halted: null, inFlight: null,
    lastRun: null, deployments: [], haltHistory: [], commits: {} };
}

export function loadState(config) {
  const file = path.join(config.stateDir, 'state.json');
  if (!fs.existsSync(file)) return emptyState();
  const state = JSON.parse(fs.readFileSync(file, 'utf8'));
  if (state.schemaVersion !== STATE_SCHEMA) throw new Error('Unsupported updater state schema');
  return state;
}

function saveState(config, state) {
  writeJsonAtomic(path.join(config.stateDir, 'state.json'), state);
}

function pidAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === 'EPERM'; }
}

/** Single-instance lock. Returns a release function, or null if another run holds it. */
export function acquireLock(config, now = new Date()) {
  const file = path.join(config.stateDir, 'updater.lock');
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const fd = fs.openSync(file, 'wx', 0o600);
      fs.writeFileSync(fd, JSON.stringify({ pid: process.pid, startedAt: now.toISOString(), host: os.hostname() }));
      fs.closeSync(fd);
      return () => { try { fs.unlinkSync(file); } catch { /* already gone */ } };
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      let holder = null;
      try { holder = JSON.parse(fs.readFileSync(file, 'utf8')); } catch { /* unreadable */ }
      // A live holder is never broken, however old. A dead holder's lock is stale;
      // an interrupted deployment is still caught by state.inFlight.
      if (!holder || pidAlive(holder.pid)) return null;
      fs.unlinkSync(file);
    }
  }
  return null;
}

// ---------------------------------------------------------------- hooks

function hookEnvironment(config, extra) {
  return {
    PATH: '/usr/local/bin:/usr/bin:/bin', HOME: path.join(config.stateDir, 'hook-home'),
    LANG: 'C.UTF-8', CI: '1', ...config.hookEnv, ...extra,
  };
}

/** Run a trusted hook argv without a shell. Output is bounded and written to a log file. */
export function runHook(hook, { cwd, env, logFile, maxLogBytes, defaultTimeoutSeconds }) {
  const timeoutMs = (hook.timeoutSeconds ?? defaultTimeoutSeconds) * 1000;
  const started = Date.now();
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(hook.argv[0], hook.argv.slice(1), {
        cwd, env, shell: false, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
      });
    } catch (error) {
      resolve({ name: hook.name, exitCode: null, error: error.code ?? 'spawn_failed', durationMs: 0, stdout: '' });
      return;
    }
    const chunks = [];
    let size = 0;
    let dropped = 0;
    let stdout = '';
    const collect = (stream, isStdout) => stream.on('data', (data) => {
      if (isStdout && stdout.length < 4096) stdout += data.toString('utf8').slice(0, 4096 - stdout.length);
      chunks.push(data);
      size += data.length;
      while (size > maxLogBytes && chunks.length > 1) { const d = chunks.shift(); size -= d.length; dropped += d.length; }
    });
    collect(child.stdout, true);
    collect(child.stderr, false);
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      try { process.kill(-child.pid, 'SIGKILL'); } catch { /* exited */ }
    }, timeoutMs);
    let settled = false;
    const finish = (exitCode, signal, error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      let body = Buffer.concat(chunks);
      if (body.length > maxLogBytes) { dropped += body.length - maxLogBytes; body = body.subarray(body.length - maxLogBytes); }
      if (logFile) {
        fs.writeFileSync(logFile, Buffer.concat([
          Buffer.from(dropped ? `[${dropped} earlier bytes truncated]\n` : ''), body]), { mode: 0o600 });
      }
      resolve({ name: hook.name, exitCode, signal: signal ?? null, timedOut, error: error ?? null,
        durationMs: Date.now() - started, stdout });
    };
    child.on('error', (error) => finish(null, null, error.code ?? 'spawn_failed'));
    child.on('close', (code, signal) => finish(code, signal));
  });
}

const hookPassed = (result) => result.exitCode === 0 && !result.timedOut && !result.error;

// ---------------------------------------------------------------- repository checks

function coreDigest(git, commit) {
  // Digest of every tree entry outside the NON-CORE allowlist.
  const entries = git(['ls-tree', '-r', '-z', '--full-tree', commit]).split('\0').filter(Boolean)
    .filter((entry) => !matchesAllowlist(entry.slice(entry.indexOf('\t') + 1)));
  return createHash('sha256').update(entries.join('\0')).digest('hex');
}

function readManifest(git, commit) {
  const res = git(['show', `${commit}:${MANIFEST_PATH}`], { allowFail: true });
  if (res.status !== 0) throw new Error('Released commit has no upstream manifest');
  return validateManifest(JSON.parse(res.stdout));
}

async function currentRelease(config, git, ctx) {
  const result = await ctx.hook('currentRelease', config.hooks.currentRelease, { cwd: config.stateDir });
  const sha = result.stdout.trim();
  if (!hookPassed(result) || !SHA_RE.test(sha)) throw new Error('currentRelease hook did not print a full SHA');
  verifyCommit(git, sha);
  return sha;
}

function fetchUpstream(config, git) {
  // Non-forced refspec: a rewritten upstream branch fails the fetch and blocks the run.
  git(['fetch', '--no-tags', '--no-recurse-submodules', '--no-write-fetch-head', '--quiet',
    config.upstream.url, `refs/heads/${config.upstream.branch}:${OBSERVED_REF}`], { timeoutMs: 600_000 });
  return git(['rev-parse', '--verify', `${OBSERVED_REF}^{commit}`]).trim();
}

function removeWorktree(git, dir) {
  git(['worktree', 'remove', '--force', '--force', dir], { allowFail: true });
  fs.rmSync(dir, { recursive: true, force: true });
  git(['worktree', 'prune'], { allowFail: true });
}

function cleanupStaleWorktrees(config, git) {
  git(['worktree', 'prune'], { allowFail: true });
  for (const name of fs.readdirSync(config.worktreesDir)) {
    if (RUN_ID_RE.test(name)) removeWorktree(git, path.join(config.worktreesDir, name));
  }
}

function pruneReports(config) {
  const runs = fs.readdirSync(config.reportsDir).filter((n) => RUN_ID_RE.test(n)).sort();
  for (const name of runs.slice(0, Math.max(0, runs.length - config.limits.reportRetention))) {
    fs.rmSync(path.join(config.reportsDir, name), { recursive: true, force: true });
  }
}

function makeRunId(now) {
  return `${now.toISOString().replace(/[-:]/g, '').replace(/\.\d+Z$/, 'Z')}-${process.pid}`;
}

function ensureDirs(config) {
  for (const dir of [config.stateDir, config.reportsDir, config.worktreesDir, path.join(config.stateDir, 'hook-home')]) {
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  }
}

function openRepo(config) {
  const git = makeGit(config.repoPath, config.stateDir);
  const bare = git(['rev-parse', '--is-bare-repository'], { allowFail: true });
  if (bare.status !== 0 || bare.stdout.trim() !== 'true') {
    throw configError('repoPath must be a dedicated bare repository, never an active checkout');
  }
  return git;
}

// ---------------------------------------------------------------- report

function renderSummary(report) {
  const lines = [
    `## Neev upstream updater: ${escapeMarkdown(report.status)}`, '',
    `- Mode: ${escapeMarkdown(report.mode)}`,
    `- Run: ${escapeMarkdown(report.runId)}`,
  ];
  for (const key of ['release', 'observed', 'baselineUpstream', 'candidate', 'deployed']) {
    if (report[key]) lines.push(`- ${key}: \`${report[key]}\``);
  }
  if (report.reason) lines.push('', escapeMarkdown(report.reason));
  const applied = report.commits.filter((c) => c.decision === 'applied');
  const skipped = report.commits.filter((c) => c.decision === 'skipped');
  lines.push('', `Applied ${applied.length}, skipped ${skipped.length}, already present ${report.alreadyPresent ?? 0}.`);
  if (report.commits.length) {
    lines.push('', '### Commits (first 100; full list in report.json)', '');
    for (const c of report.commits.slice(0, 100)) {
      const why = (c.reasons ?? []).slice(0, 3).map((r) => `${r.reason}${r.path ? ` ${r.path}` : ''}`).join('; ');
      lines.push(`- \`${c.sha.slice(0, 12)}\` ${escapeMarkdown(c.decision)} ${escapeMarkdown(c.subject ?? '')}${
        why ? ` (${escapeMarkdown(why)})` : ''}`);
    }
  }
  if (report.hooks.length) {
    lines.push('', '### Hooks', '');
    for (const h of report.hooks) {
      lines.push(`- ${escapeMarkdown(h.phase)}/${escapeMarkdown(h.name)}: exit ${h.exitCode}${h.timedOut ? ' (timed out)' : ''}`);
    }
  }
  return lines.join('\n') + '\n';
}

// ---------------------------------------------------------------- main run

function recordLedger(state, commits, runId, nowIso, rangeAll) {
  const inRange = new Set(rangeAll);
  for (const sha of Object.keys(state.commits)) if (!inRange.has(sha)) delete state.commits[sha];
  for (const c of commits) {
    const previous = state.commits[c.sha];
    state.commits[c.sha] = {
      decision: c.decision, reasons: (c.reasons ?? []).slice(0, 10),
      firstSeen: previous?.firstSeen ?? nowIso, lastSeen: nowIso, runId,
    };
  }
  const keys = Object.keys(state.commits);
  if (keys.length > MAX_LEDGER) for (const k of keys.slice(0, keys.length - MAX_LEDGER)) delete state.commits[k];
}

/**
 * Cherry-pick one eligible upstream commit inside the candidate worktree, then
 * prove the resulting commit changed exactly the upstream paths and modes.
 */
function trialApply(wgit, config, evaluation) {
  const before = wgit(['rev-parse', 'HEAD']).trim();
  const date = wgit(['log', '-1', '--format=%cI', evaluation.sha]).trim();
  const env = {
    GIT_COMMITTER_NAME: config.committer.name, GIT_COMMITTER_EMAIL: config.committer.email,
    GIT_COMMITTER_DATE: date,
  };
  const res = wgit(['cherry-pick', '-x', '--no-rerere-autoupdate', '--allow-empty-message',
    '--strategy=ort', evaluation.sha], { allowFail: true, env });
  if (res.status !== 0) {
    const unmerged = wgit(['diff', '--name-only', '--diff-filter=U'], { allowFail: true }).stdout ?? '';
    wgit(['cherry-pick', '--abort'], { allowFail: true });
    wgit(['reset', '--hard', '--quiet', before]);
    if (!unmerged.trim() && wgit(['status', '--porcelain', '--untracked-files=no']).trim() === '') {
      // Patch applies to nothing new: the release already contains this change.
      return { ok: false, decision: 'already_present', reason: 'already_present_or_empty', taint: false };
    }
    return { ok: false, reason: 'conflict' };
  }
  const after = wgit(['rev-parse', 'HEAD']).trim();
  const key = (c) => `${c.status} ${c.newMode} ${c.path}`;
  const expected = evaluation.changes.map(key).sort().join('\n');
  const actual = rawChanges(wgit, before, after).map(key).sort().join('\n');
  if (expected !== actual) {
    wgit(['reset', '--hard', '--quiet', before]);
    return { ok: false, reason: 'unexpected_merge_result' };
  }
  return { ok: true, candidate: after };
}

export async function runUpdate({ configPath, dryRun = false, now = () => new Date() } = {}) {
  const config = loadConfig(configPath);
  ensureDirs(config);
  const started = now();
  const unlock = acquireLock(config, started);
  if (!unlock) return { status: 'locked', exitCode: EXIT.locked, reason: 'Another updater run holds the lock.' };

  const runId = makeRunId(started);
  const reportDir = path.join(config.reportsDir, runId);
  const logsDir = path.join(reportDir, 'logs');
  fs.mkdirSync(logsDir, { recursive: true, mode: 0o700 });
  const mode = dryRun || config.mode === 'dry-run' ? 'dry-run' : 'deploy';
  const report = {
    schemaVersion: 1, runId, mode, status: 'error', reason: null, startedAt: started.toISOString(),
    finishedAt: null, release: null, observed: null, baselineUpstream: null, candidate: null, deployed: null,
    coreDigest: null, commits: [], alreadyPresent: 0, hooks: [], automaticDeploy: mode === 'deploy',
  };
  let state;
  let git;
  let worktree = null;
  const hookCtx = {
    async hook(phase, hook, { cwd, extraEnv = {} }) {
      const result = await runHook(hook, {
        cwd, env: hookEnvironment(config, { NEEV_UPSTREAM_RUN_ID: runId, NEEV_UPSTREAM_REPORT_DIR: reportDir, ...extraEnv }),
        logFile: path.join(logsDir, `${String(report.hooks.length).padStart(2, '0')}-${phase}-${hook.name}.log`),
        maxLogBytes: config.limits.maxLogBytes, defaultTimeoutSeconds: config.limits.hookTimeoutSeconds,
      });
      report.hooks.push({ phase, name: hook.name, exitCode: result.exitCode, signal: result.signal,
        timedOut: result.timedOut, error: result.error, durationMs: result.durationMs });
      return result;
    },
  };
  const finish = (status, code, reason = null) => {
    Object.assign(report, { status, reason });
    return { status, exitCode: code, reason, runId, report };
  };

  try {
    state = loadState(config);
    git = openRepo(config);
    cleanupStaleWorktrees(config, git);
    if (state.inFlight && !state.halted) {
      state.halted = { reason: 'A previous run stopped during deployment; state unknown.', at: started.toISOString(), runId };
      saveState(config, state);
    }
    if (state.halted) return finish('halted', EXIT.halted, `Automatic updates are halted: ${state.halted.reason}`);
    if (!state.baseline) return finish('blocked', EXIT.blocked, 'No baseline recorded. Run record-baseline first.');
    report.baselineUpstream = state.baseline.upstream;

    const releaseSha = await currentRelease(config, git, hookCtx);
    report.release = releaseSha;
    readManifest(git, releaseSha);

    let observed;
    try { observed = fetchUpstream(config, git); } catch {
      return finish('blocked', EXIT.blocked, 'Upstream fetch failed or upstream history was rewritten.');
    }
    report.observed = observed;
    verifyCommit(git, state.baseline.upstream);
    if (!isAncestor(git, state.baseline.upstream, observed)) {
      return finish('blocked', EXIT.blocked, 'Upstream no longer descends from the recorded baseline.');
    }

    const range = pendingCommits(git, { baselineUpstream: state.baseline.upstream, observed, release: releaseSha });
    report.alreadyPresent = range.alreadyPresent.length;
    if (range.pending.length > config.limits.maxRangeCommits) {
      return finish('blocked', EXIT.blocked,
        `${range.pending.length} pending upstream commits exceed the limit; do a reviewed manual catch-up and record a new baseline.`);
    }

    worktree = path.join(config.worktreesDir, runId);
    git(['worktree', 'add', '--detach', '--quiet', worktree, releaseSha]);
    const wgit = makeGit(worktree, config.stateDir);
    report.commits = walkCommits(git, range.pending, (evaluation) => trialApply(wgit, config, evaluation))
      .map((c) => ({ ...c, paths: c.paths.slice(0, 50), pathCount: c.paths.length, reasons: c.reasons.slice(0, 50) }));
    const candidate = wgit(['rev-parse', 'HEAD']).trim();
    report.candidate = candidate;
    recordLedger(state, report.commits, runId, started.toISOString(), range.all);

    if (candidate === releaseSha) {
      state.lastRun = { runId, status: 'current', at: started.toISOString() };
      saveState(config, state);
      return finish('current', EXIT.ok, 'No eligible NON-CORE upstream changes to apply.');
    }

    // Final whole-candidate verification, independent of per-commit checks.
    const changes = rawChanges(git, releaseSha, candidate);
    const pathCheck = classifyChanges(changes);
    const contentCheck = pathCheck.eligible ? contentRejections(git, releaseSha, candidate, changes) : [];
    const releaseDigest = coreDigest(git, releaseSha);
    report.coreDigest = { release: releaseDigest, candidate: coreDigest(git, candidate) };
    if (!pathCheck.eligible || contentCheck.length || report.coreDigest.release !== report.coreDigest.candidate) {
      return finish('blocked', EXIT.blocked, 'Candidate touched protected content; nothing deployed.');
    }

    const hookEnv = { NEEV_UPSTREAM_CANDIDATE_SHA: candidate, NEEV_UPSTREAM_CANDIDATE_DIR: worktree,
      NEEV_UPSTREAM_BASE_SHA: releaseSha };
    for (const phase of ['validate', 'build']) {
      for (const hook of config.hooks[phase]) {
        const result = await hookCtx.hook(phase, hook, { cwd: worktree, extraEnv: hookEnv });
        if (!hookPassed(result)) {
          return finish('validation_failed', EXIT.blocked, `${phase} hook ${hook.name} failed; nothing deployed.`);
        }
      }
    }
    // Tests and build must not have altered tracked files or moved HEAD.
    const dirty = wgit(['status', '--porcelain', '--untracked-files=no', '--ignore-submodules=none']).trim();
    if (dirty || wgit(['rev-parse', 'HEAD']).trim() !== candidate
      || coreDigest(git, candidate) !== releaseDigest) {
      return finish('blocked', EXIT.blocked, 'Validation or build modified tracked files; nothing deployed.');
    }

    if (mode === 'dry-run') {
      state.lastRun = { runId, status: 'dry_run_passed', at: started.toISOString() };
      saveState(config, state);
      const checks = config.hooks.validate.length + config.hooks.build.length
        ? 'passed configured validation and build hooks' : 'passed policy checks (no validation or build hooks configured)';
      return finish('dry_run_passed', EXIT.ok, `Dry run: candidate ${checks}. No gate, deploy or rollback hook ran.`);
    }

    const gate = await hookCtx.hook('gate', config.hooks.activeRunsGate, { cwd: config.stateDir, extraEnv: hookEnv });
    if (!hookPassed(gate)) {
      state.lastRun = { runId, status: 'deferred_active_runs', at: started.toISOString() };
      saveState(config, state);
      return finish('deferred_active_runs', EXIT.ok, 'Active runs gate did not clear; deployment deferred to the next run.');
    }

    state.inFlight = { runId, candidate, previous: releaseSha, at: now().toISOString() };
    saveState(config, state);
    const deployEnv = { ...hookEnv, NEEV_UPSTREAM_PREVIOUS_SHA: releaseSha };
    const deploy = await hookCtx.hook('deploy', config.hooks.deploy, { cwd: worktree, extraEnv: deployEnv });
    const healthy = hookPassed(deploy)
      && hookPassed(await hookCtx.hook('health', config.hooks.healthCheck, { cwd: config.stateDir, extraEnv: deployEnv }));
    if (!healthy) {
      const rollback = await hookCtx.hook('rollback', config.hooks.rollback, { cwd: config.stateDir, extraEnv: deployEnv });
      const rolledBack = hookPassed(rollback);
      const reason = rolledBack
        ? 'Deploy or health check failed; rolled back to the previous release.'
        : 'Deploy or health check failed and ROLLBACK FAILED; manual intervention required.';
      state.inFlight = null;
      state.halted = { reason, at: now().toISOString(), runId, candidate, previous: releaseSha };
      state.lastRun = { runId, status: rolledBack ? 'rolled_back' : 'rollback_failed', at: started.toISOString() };
      saveState(config, state);
      return finish(rolledBack ? 'rolled_back' : 'rollback_failed', EXIT.halted, reason);
    }

    git(['update-ref', DEPLOYED_REF, candidate]);
    git(['update-ref', `refs/workos-upstream/deployments/${runId}`, candidate]);
    state.inFlight = null;
    state.deployments = [...state.deployments, {
      runId, candidate, previous: releaseSha, at: now().toISOString(),
      upstreamCommits: report.commits.filter((c) => c.decision === 'applied').map((c) => c.sha),
    }].slice(-MAX_DEPLOYMENTS);
    const keepRefs = new Set(state.deployments.map((d) => `refs/workos-upstream/deployments/${d.runId}`));
    for (const ref of git(['for-each-ref', '--format=%(refname)', 'refs/workos-upstream/deployments/']).split('\n')) {
      if (ref && !keepRefs.has(ref)) git(['update-ref', '-d', ref]);
    }
    state.lastRun = { runId, status: 'deployed', at: started.toISOString() };
    saveState(config, state);
    report.deployed = candidate;
    return finish('deployed', EXIT.ok, 'Deployed NON-CORE upstream changes.');
  } catch (error) {
    // Messages from our own code only; Git stderr (untrusted paths) goes to a log file.
    if (error.stderr) fs.writeFileSync(path.join(logsDir, 'git-error.log'), error.stderr, { mode: 0o600 });
    return finish(error.code === 'CONFIG' ? 'config_error' : 'error', EXIT.error, error.message.slice(0, 500));
  } finally {
    if (worktree && git) removeWorktree(git, worktree);
    report.finishedAt = now().toISOString();
    writeJsonAtomic(path.join(reportDir, 'report.json'), report);
    fs.writeFileSync(path.join(reportDir, 'summary.md'), renderSummary(report), { mode: 0o600 });
    writeJsonAtomic(path.join(config.reportsDir, 'latest.json'), { runId, status: report.status });
    pruneReports(config);
    unlock();
  }
}

/** Record the upstream SHA already incorporated in the current release. */
export async function recordBaseline({ configPath, upstream, allowUnmerged = false, note = '', now = () => new Date() }) {
  const config = loadConfig(configPath);
  ensureDirs(config);
  const releaseLock = acquireLock(config, now());
  if (!releaseLock) throw new Error('Another updater run holds the lock.');
  try {
    const git = openRepo(config);
    const hooks = [];
    const ctx = { hook: (phase, hook, { cwd }) => runHook(hook, {
      cwd, env: hookEnvironment(config, {}), logFile: null, maxLogBytes: config.limits.maxLogBytes,
      defaultTimeoutSeconds: config.limits.hookTimeoutSeconds,
    }).then((r) => { hooks.push(r); return r; }) };
    const releaseSha = await currentRelease(config, git, ctx);
    const manifest = readManifest(git, releaseSha);
    const baseline = upstream ?? manifest.incorporated;
    fetchUpstream(config, git);
    verifyCommit(git, baseline);
    const observed = git(['rev-parse', OBSERVED_REF]).trim();
    if (!isAncestor(git, baseline, observed)) throw new Error('Baseline is not in upstream history.');
    if (!allowUnmerged && !isAncestor(git, baseline, releaseSha)) {
      throw new Error('Baseline is not merged into the current release. Pass --allow-unmerged-baseline to accept it.');
    }
    if (typeof note !== 'string' || note.length > 500) throw new Error('Note too long.');
    const state = loadState(config);
    state.baseline = { upstream: baseline, downstream: releaseSha, recordedAt: now().toISOString(),
      merged: isAncestor(git, baseline, releaseSha), note };
    state.commits = {};
    saveState(config, state);
    return state.baseline;
  } finally {
    releaseLock();
  }
}

export function clearHalt({ configPath, reason, now = () => new Date() }) {
  if (typeof reason !== 'string' || reason.trim().length < 3 || reason.length > 500) {
    throw new Error('clear-halt requires --reason');
  }
  const config = loadConfig(configPath);
  ensureDirs(config);
  const releaseLock = acquireLock(config, now());
  if (!releaseLock) throw new Error('Another updater run holds the lock.');
  try {
    const state = loadState(config);
    if (!state.halted && !state.inFlight) return false;
    state.haltHistory = [...state.haltHistory, { ...state.halted, clearedAt: now().toISOString(), clearReason: reason }].slice(-50);
    state.halted = null;
    state.inFlight = null;
    saveState(config, state);
    return true;
  } finally {
    releaseLock();
  }
}

function parseArgs(argv) {
  const [command, ...rest] = argv;
  const opts = {};
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === '--dry-run') opts.dryRun = true;
    else if (arg === '--allow-unmerged-baseline') opts.allowUnmerged = true;
    else if (['--config', '--upstream', '--note', '--reason'].includes(arg) && i + 1 < rest.length) {
      opts[arg.slice(2)] = rest[i += 1];
    } else throw new Error('Unknown argument');
  }
  if (!opts.config) throw new Error('--config is required');
  return { command, opts };
}

const USAGE = `Usage:
  updater.mjs run --config /abs/config.json [--dry-run]
  updater.mjs record-baseline --config /abs/config.json [--upstream SHA] [--allow-unmerged-baseline] [--note TEXT]
  updater.mjs status --config /abs/config.json
  updater.mjs clear-halt --config /abs/config.json --reason TEXT`;

async function main(argv) {
  let parsed;
  try { parsed = parseArgs(argv); } catch (error) { console.error(`${error.message}\n${USAGE}`); return EXIT.error; }
  const { command, opts } = parsed;
  try {
    if (command === 'run') {
      const result = await runUpdate({ configPath: opts.config, dryRun: opts.dryRun });
      console.log(`Neev upstream updater: ${result.status ?? 'unknown'}`);
      return result.exitCode;
    }
    if (command === 'record-baseline') {
      const baseline = await recordBaseline({ configPath: opts.config, upstream: opts.upstream,
        allowUnmerged: opts.allowUnmerged, note: opts.note ?? '' });
      console.log(JSON.stringify(baseline));
      return EXIT.ok;
    }
    if (command === 'status') {
      const config = loadConfig(opts.config);
      const state = loadState(config);
      console.log(JSON.stringify({ baseline: state.baseline, halted: state.halted, inFlight: state.inFlight,
        lastRun: state.lastRun, lastDeployment: state.deployments.at(-1) ?? null }, null, 2));
      return EXIT.ok;
    }
    if (command === 'clear-halt') {
      console.log(clearHalt({ configPath: opts.config, reason: opts.reason }) ? 'Halt cleared.' : 'Not halted.');
      return EXIT.ok;
    }
    console.error(USAGE);
    return EXIT.error;
  } catch (error) {
    console.error(`Neev upstream updater failed: ${String(error.message).slice(0, 500)}`);
    return EXIT.error;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2));
}
