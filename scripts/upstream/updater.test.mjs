import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { allowlistDigest } from './policy.mjs';
import { clearHalt, recordBaseline, runUpdate, validateConfig, loadState } from './updater.mjs';

const MANIFEST = fs.readFileSync(new URL('../../.github/workos-upstream.json', import.meta.url), 'utf8');
const UPDATER = fileURLToPath(new URL('./updater.mjs', import.meta.url));
const BUTTON = 'import { cn } from "@/lib/utils"\n\nexport function Button() {\n  return <button className={cn("px-2")} />\n}\n';
const CARD = 'export function Card() {\n  return <div className="rounded" />\n}\n';

const HOOK_SCRIPT = `
import fs from 'node:fs';
import path from 'node:path';
const phase = process.argv[2];
const root = process.env.FIXTURE_ROOT;
fs.appendFileSync(path.join(root, 'hook-log.jsonl'), JSON.stringify({
  phase, cwd: process.cwd(), candidate: process.env.NEEV_UPSTREAM_CANDIDATE_SHA ?? null,
  base: process.env.NEEV_UPSTREAM_BASE_SHA ?? null, previous: process.env.NEEV_UPSTREAM_PREVIOUS_SHA ?? null,
  leaked: process.env.PARENT_SECRET ?? null,
}) + '\\n');
const control = path.join(root, 'control', phase);
const code = fs.existsSync(control) ? Number(fs.readFileSync(control, 'utf8')) : 0;
if (phase === 'release') process.stdout.write(fs.readFileSync(path.join(root, 'release-sha'), 'utf8'));
if (phase === 'validate') console.log('button now:', fs.readFileSync('src/components/ui/button.tsx', 'utf8'));
if (phase === 'build' && fs.existsSync(path.join(root, 'control', 'build-mutate'))) {
  fs.appendFileSync('src/lib/utils.ts', '// tampered by build\\n');
}
if (phase === 'deploy' && code === 0) fs.writeFileSync(path.join(root, 'release-sha'), process.env.NEEV_UPSTREAM_CANDIDATE_SHA);
if (phase === 'rollback' && code === 0) fs.writeFileSync(path.join(root, 'release-sha'), process.env.NEEV_UPSTREAM_PREVIOUS_SHA);
process.exit(code);
`;

const FIXTURE_GIT_ENV = {
  PATH: process.env.PATH, HOME: os.tmpdir(), GIT_CONFIG_GLOBAL: '/dev/null', GIT_CONFIG_NOSYSTEM: '1',
  GIT_AUTHOR_NAME: 'Upstream author', GIT_AUTHOR_EMAIL: 'upstream@example.invalid',
  GIT_COMMITTER_NAME: 'Upstream author', GIT_COMMITTER_EMAIL: 'upstream@example.invalid',
};

function fixture(t, { limits } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'neev-updater-test-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const upstream = path.join(root, 'upstream');
  const bare = path.join(root, 'updater', 'repo.git');
  fs.mkdirSync(upstream);
  const gitIn = (cwd, ...args) => execFileSync('git', args, {
    cwd, env: FIXTURE_GIT_ENV, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
  const git = (...args) => gitIn(upstream, ...args);
  const commit = (files, message = 'change') => {
    for (const [file, content] of Object.entries(files)) {
      const full = path.join(upstream, file);
      if (content === null) fs.rmSync(full);
      else { fs.mkdirSync(path.dirname(full), { recursive: true }); fs.writeFileSync(full, content); }
    }
    git('add', '-A');
    git('commit', '-q', '--allow-empty', '-m', message);
    return git('rev-parse', 'HEAD');
  };
  git('init', '-q', '-b', 'main');
  const base = commit({
    '.github/workos-upstream.json': MANIFEST, 'src/components/ui/button.tsx': BUTTON,
    'src/components/ui/card.tsx': CARD, 'src/lib/utils.ts': 'export const cn = (...a) => a.join(" ")\n',
    'package.json': '{"name":"fixture"}\n', 'docs/url-scheme.md': '# URL scheme\n',
  }, 'base');
  git('branch', 'release');

  const hooksDir = path.join(root, 'trusted-hooks');
  fs.mkdirSync(hooksDir);
  fs.mkdirSync(path.join(root, 'control'));
  const hookScript = path.join(hooksDir, 'hook.mjs');
  fs.writeFileSync(hookScript, HOOK_SCRIPT);
  const hook = (phase) => ({ argv: [process.execPath, hookScript, phase], timeoutSeconds: 60 });
  const configPath = path.join(root, 'config.json');
  const config = {
    schemaVersion: 1, mode: 'deploy', repoPath: bare,
    stateDir: path.join(root, 'updater', 'state'), reportsDir: path.join(root, 'updater', 'reports'),
    worktreesDir: path.join(root, 'updater', 'worktrees'),
    upstream: { url: upstream, branch: 'main' }, policy: { allowlistSha256: allowlistDigest() },
    limits: { hookTimeoutSeconds: 60, ...limits }, hookEnv: { FIXTURE_ROOT: root },
    hooks: {
      currentRelease: hook('release'), validate: [{ name: 'tests', ...hook('validate') }],
      build: [{ name: 'build', ...hook('build') }], activeRunsGate: hook('gate'), deploy: hook('deploy'),
      healthCheck: hook('health'), rollback: hook('rollback'),
    },
  };
  const writeConfig = (overrides = {}) => {
    fs.writeFileSync(configPath, JSON.stringify({ ...config, ...overrides }, null, 2), { mode: 0o600 });
  };
  writeConfig();

  const onRelease = (files, message = 'downstream change') => {
    git('checkout', '-q', 'release');
    const sha = commit(files, message);
    git('checkout', '-q', 'main');
    return sha;
  };
  const init = async () => {
    fs.mkdirSync(path.dirname(bare), { recursive: true });
    gitIn(root, 'clone', '-q', '--bare', upstream, bare);
    fs.writeFileSync(path.join(root, 'release-sha'), git('rev-parse', 'release'));
    return recordBaseline({ configPath, upstream: base });
  };
  const control = (phase, value) => {
    const file = path.join(root, 'control', phase);
    if (value === null) fs.rmSync(file, { force: true }); else fs.writeFileSync(file, String(value));
  };
  const hookLog = () => {
    const file = path.join(root, 'hook-log.jsonl');
    const lines = fs.existsSync(file) ? fs.readFileSync(file, 'utf8').split('\n').filter(Boolean) : [];
    fs.rmSync(file, { force: true });
    return lines.map((line) => JSON.parse(line));
  };
  const releaseSha = () => fs.readFileSync(path.join(root, 'release-sha'), 'utf8').trim();
  const show = (rev, file) => gitIn(bare, 'show', `${rev}:${file}`);
  const changed = (from, to) => gitIn(bare, 'diff', '--name-only', from, to).split('\n').filter(Boolean);
  const run = (opts = {}) => runUpdate({ configPath, ...opts });
  return { root, upstream, bare, base, git, gitIn, commit, onRelease, init, control, hookLog, releaseSha, show,
    changed, run, configPath, config, writeConfig, hook };
}

const phases = (log) => log.map((entry) => entry.phase);

test('presentation-only commit deploys after tests, build and gate, and a rerun is a no-op', async (t) => {
  const f = fixture(t);
  await f.init();
  const previous = f.releaseSha();
  const upstreamSha = f.commit({ 'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-3') }, 'style(button): wider');
  process.env.PARENT_SECRET = 'must-not-leak';
  t.after(() => { delete process.env.PARENT_SECRET; });
  f.hookLog();

  const first = await f.run();
  assert.equal(first.status, 'deployed', first.reason);
  assert.equal(first.exitCode, 0);
  const log = f.hookLog();
  assert.deepEqual(phases(log), ['release', 'validate', 'build', 'gate', 'deploy', 'health']);
  const candidate = first.report.candidate;
  assert.notEqual(candidate, previous);
  assert.ok(log.slice(1).every((e) => e.candidate === candidate && e.base === previous));
  assert.equal(log.find((e) => e.phase === 'deploy').previous, previous);
  assert.ok(log.every((e) => e.leaked === null), 'service environment is not inherited by hooks');
  assert.equal(log.find((e) => e.phase === 'validate').cwd, path.join(f.config.worktreesDir, first.runId));
  assert.deepEqual(f.changed(previous, candidate), ['src/components/ui/button.tsx']);
  assert.match(f.show(candidate, 'src/components/ui/button.tsx'), /px-3/);
  assert.match(f.gitIn(f.bare, 'log', '-1', '--format=%B', candidate), new RegExp(`cherry picked from commit ${upstreamSha}`));
  assert.equal(first.report.coreDigest.release, first.report.coreDigest.candidate);
  assert.equal(f.gitIn(f.bare, 'rev-parse', 'refs/workos-upstream/deployed'), candidate);
  assert.equal(fs.readdirSync(f.config.worktreesDir).length, 0, 'candidate worktree is removed');
  assert.equal(f.gitIn(f.bare, 'worktree', 'list').split('\n').length, 1);
  const state = loadState(f.config);
  assert.equal(state.deployments.length, 1);
  assert.deepEqual(state.deployments[0].upstreamCommits, [upstreamSha]);
  assert.equal(state.commits[upstreamSha].decision, 'applied');

  const second = await f.run();
  assert.equal(second.status, 'current', second.reason);
  assert.deepEqual(phases(f.hookLog()), ['release'], 'no revalidation or redeploy of applied commits');
  assert.equal(f.releaseSha(), candidate);
  assert.equal(loadState(f.config).deployments.length, 1);
  const summary = fs.readFileSync(path.join(f.config.reportsDir, first.runId, 'summary.md'), 'utf8');
  assert.match(summary, /deployed/);
});

test('mixed commits are skipped whole and taint later commits touching the same paths', async (t) => {
  const f = fixture(t);
  await f.init();
  const previous = f.releaseSha();
  const mixed = f.commit({
    'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-4'),
    'src/lib/utils.ts': 'export const cn = (...a) => a.filter(Boolean).join(" ")\n',
  }, 'feat: button plus utils');
  const dependent = f.commit({ 'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-5') }, 'style: follow-up');
  const independent = f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-lg') }, 'style: card');
  const deps = f.commit({ 'package.json': '{"name":"fixture","dependencies":{"x":"1"}}\n' }, 'deps');
  const hooksDir = f.commit({ '.githooks/post-checkout': '#!/bin/sh\ntouch PWNED\n' }, 'hooks');

  const result = await f.run();
  assert.equal(result.status, 'deployed', result.reason);
  const byName = Object.fromEntries(result.report.commits.map((c) => [c.sha, c]));
  assert.equal(byName[mixed].decision, 'skipped');
  assert.deepEqual(byName[mixed].reasons, [{ path: 'src/lib/utils.ts', reason: 'protected_path' }]);
  assert.equal(byName[dependent].decision, 'skipped');
  assert.equal(byName[dependent].reasons[0].reason, 'depends_on_skipped_change');
  assert.equal(byName[independent].decision, 'applied');
  assert.equal(byName[deps].decision, 'skipped');
  assert.equal(byName[hooksDir].decision, 'skipped');
  assert.deepEqual(f.changed(previous, result.report.candidate), ['src/components/ui/card.tsx']);
  assert.equal(f.show(result.report.candidate, 'src/components/ui/button.tsx'), BUTTON.trim());
  assert.equal(fs.existsSync(path.join(f.root, 'PWNED')), false);
  assert.equal(loadState(f.config).commits[mixed].decision, 'skipped');
});

test('a conflicting commit is skipped and reported, and nothing deploys', async (t) => {
  const f = fixture(t);
  f.onRelease({ 'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-8') }, 'downstream button');
  await f.init();
  const previous = f.releaseSha();
  const conflicting = f.commit({ 'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-3') }, 'upstream button');
  f.hookLog();
  const result = await f.run();
  assert.equal(result.status, 'current', result.reason);
  assert.deepEqual(result.report.commits.map((c) => [c.sha, c.decision, c.reasons[0].reason]),
    [[conflicting, 'skipped', 'conflict']]);
  assert.deepEqual(phases(f.hookLog()), ['release']);
  assert.equal(f.releaseSha(), previous);
  assert.equal(fs.readdirSync(f.config.worktreesDir).length, 0);
});

test('failing tests or build prevent deployment; a later passing run deploys', async (t) => {
  const f = fixture(t);
  await f.init();
  const previous = f.releaseSha();
  f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-md') }, 'style: card');
  f.hookLog();

  f.control('validate', 1);
  const failedTests = await f.run();
  assert.equal(failedTests.status, 'validation_failed');
  assert.equal(failedTests.exitCode, 2);
  assert.deepEqual(phases(f.hookLog()), ['release', 'validate']);

  f.control('validate', null);
  f.control('build', 3);
  const failedBuild = await f.run();
  assert.equal(failedBuild.status, 'validation_failed');
  assert.deepEqual(phases(f.hookLog()), ['release', 'validate', 'build']);
  assert.equal(f.releaseSha(), previous);
  assert.equal(loadState(f.config).deployments.length, 0);
  const logs = fs.readdirSync(path.join(f.config.reportsDir, failedBuild.runId, 'logs'));
  assert.ok(logs.some((name) => name.includes('build')));

  f.control('build', null);
  const passing = await f.run();
  assert.equal(passing.status, 'deployed', passing.reason);
});

test('validation that modifies tracked files blocks deployment', async (t) => {
  const f = fixture(t);
  await f.init();
  f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-sm') }, 'style: card');
  f.control('build-mutate', 1);
  f.hookLog();
  const result = await f.run();
  assert.equal(result.status, 'blocked');
  assert.match(result.reason, /modified tracked files/);
  assert.deepEqual(phases(f.hookLog()), ['release', 'validate', 'build']);
});

test('active runs gate defers; deploy failure rolls back and halts until cleared', async (t) => {
  const f = fixture(t);
  await f.init();
  const previous = f.releaseSha();
  f.commit({ 'docs/url-scheme.md': '# URL scheme\n\nClarified wording.\n' }, 'docs: wording');
  f.hookLog();

  f.control('gate', 75);
  const deferred = await f.run();
  assert.equal(deferred.status, 'deferred_active_runs');
  assert.deepEqual(phases(f.hookLog()), ['release', 'validate', 'build', 'gate']);

  f.control('gate', null);
  f.control('deploy', 1);
  const failed = await f.run();
  assert.equal(failed.status, 'rolled_back');
  assert.equal(failed.exitCode, 4);
  const log = f.hookLog();
  assert.deepEqual(phases(log), ['release', 'validate', 'build', 'gate', 'deploy', 'rollback']);
  assert.equal(log.at(-1).previous, previous);
  assert.equal(f.releaseSha(), previous);

  f.control('deploy', null);
  const halted = await f.run();
  assert.equal(halted.status, 'halted');
  assert.deepEqual(phases(f.hookLog()), [], 'a halted updater runs no hooks');

  assert.equal(clearHalt({ configPath: f.configPath, reason: 'deploy script fixed' }), true);
  const recovered = await f.run();
  assert.equal(recovered.status, 'deployed', recovered.reason);
  assert.equal(loadState(f.config).haltHistory.length, 1);
});

test('failed health check and failed rollback both halt; rollback failure is explicit', async (t) => {
  const f = fixture(t);
  await f.init();
  f.commit({ 'docs/url-scheme.md': '# URL scheme v2\n' }, 'docs');
  f.control('health', 1);
  f.control('rollback', 1);
  const result = await f.run();
  assert.equal(result.status, 'rollback_failed');
  assert.match(result.reason, /ROLLBACK FAILED/);
  assert.ok(loadState(f.config).halted);
});

test('an interrupted deployment halts the next run', async (t) => {
  const f = fixture(t);
  await f.init();
  const state = loadState(f.config);
  state.inFlight = { runId: 'x', candidate: f.base, previous: f.base, at: new Date().toISOString() };
  fs.writeFileSync(path.join(f.config.stateDir, 'state.json'), JSON.stringify(state));
  const result = await f.run();
  assert.equal(result.status, 'halted');
});

test('dry run validates and builds but never gates, deploys or rolls back', async (t) => {
  const f = fixture(t);
  await f.init();
  const previous = f.releaseSha();
  f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-xl') }, 'style');
  f.hookLog();
  const result = await f.run({ dryRun: true });
  assert.equal(result.status, 'dry_run_passed', result.reason);
  assert.equal(result.report.automaticDeploy, false);
  assert.deepEqual(phases(f.hookLog()), ['release', 'validate', 'build']);
  assert.equal(f.releaseSha(), previous);
  assert.equal(loadState(f.config).deployments.length, 0);
});

test('a live lock prevents overlapping runs and a dead holder lock is recovered', async (t) => {
  const f = fixture(t);
  await f.init();
  const lock = path.join(f.config.stateDir, 'updater.lock');
  fs.writeFileSync(lock, JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString() }));
  f.hookLog();
  const locked = await f.run();
  assert.equal(locked.status, 'locked');
  assert.equal(locked.exitCode, 3);
  assert.deepEqual(f.hookLog(), []);
  assert.equal(fs.existsSync(lock), true, 'the live holder keeps its lock');

  const dead = execFileSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], { encoding: 'utf8' });
  fs.writeFileSync(lock, JSON.stringify({ pid: Number(dead), startedAt: '2000-01-01T00:00:00.000Z' }));
  const recovered = await f.run();
  assert.equal(recovered.status, 'current');
  assert.equal(fs.existsSync(lock), false);
});

test('repository hooks never run and an active checkout is refused', async (t) => {
  const f = fixture(t);
  await f.init();
  const marker = path.join(f.root, 'HOOK-RAN');
  for (const name of ['post-checkout', 'pre-commit', 'post-commit', 'reference-transaction', 'pre-auto-gc']) {
    const file = path.join(f.bare, 'hooks', name);
    fs.writeFileSync(file, `#!/bin/sh\ntouch '${marker}'\n`, { mode: 0o755 });
  }
  f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-2xl') }, 'style');
  const result = await f.run();
  assert.equal(result.status, 'deployed', result.reason);
  assert.equal(fs.existsSync(marker), false);

  f.writeConfig({ repoPath: f.upstream });
  const refused = await f.run();
  assert.equal(refused.status, 'config_error');
  assert.match(refused.reason, /bare repository/);
});

test('rewritten upstream history and oversized backlogs block without deploying', async (t) => {
  const f = fixture(t, { limits: { maxRangeCommits: 2 } });
  await f.init();
  for (let i = 0; i < 3; i += 1) f.commit({ 'docs/url-scheme.md': `# URL ${i}\n` }, `docs ${i}`);
  f.hookLog();
  const backlog = await f.run();
  assert.equal(backlog.status, 'blocked');
  assert.match(backlog.reason, /exceed the limit/);

  f.git('checkout', '-q', '--orphan', 'rewrite');
  f.commit({ 'docs/url-scheme.md': '# rewritten\n' }, 'rewrite');
  f.git('branch', '-f', 'main', 'rewrite');
  const rewritten = await f.run();
  assert.equal(rewritten.status, 'blocked');
  assert.match(rewritten.reason, /rewritten/);
  assert.ok(!phases(f.hookLog()).includes('deploy'));
});

test('a release whose manifest widens the allowlist is refused', async (t) => {
  const f = fixture(t);
  const widened = JSON.parse(MANIFEST);
  widened.noncorePolicy.allowlist.push('src/lib/*.tsx');
  f.onRelease({ '.github/workos-upstream.json': JSON.stringify(widened) }, 'widen');
  await assert.rejects(f.init(), /refusing to widen/);
});

test('reports are bounded by retention', async (t) => {
  const f = fixture(t, { limits: { reportRetention: 2 } });
  await f.init();
  const ids = [];
  for (let i = 0; i < 4; i += 1) {
    ids.push((await f.run({ now: () => new Date(Date.UTC(2026, 8, 1, 3, 0, i)) })).runId);
  }
  const kept = fs.readdirSync(f.config.reportsDir).filter((n) => n !== 'latest.json').sort();
  assert.deepEqual(kept, ids.slice(-2).sort());
});

test('config accepts only explicit trusted argv hooks and a pinned policy digest', (t) => {
  const f = fixture(t);
  const base = structuredClone(f.config);
  assert.ok(validateConfig(base));
  const bad = [
    (c) => { c.hooks.deploy = { argv: 'deploy.sh --prod' }; },
    (c) => { c.hooks.deploy = { argv: ['deploy.sh'] }; },
    (c) => { c.hooks.deploy = { argv: [path.join(c.repoPath, 'deploy.sh')] }; },
    (c) => { c.hooks.deploy = { argv: [path.join(c.worktreesDir, 'x', 'scripts', 'deploy.sh')] }; },
    (c) => { c.hooks.deploy = { command: '/bin/sh -c deploy' }; },
    (c) => { delete c.hooks.rollback; },
    (c) => { delete c.hooks.activeRunsGate; },
    (c) => { c.hooks.validate = []; },
    (c) => { c.policy.allowlistSha256 = '0'.repeat(64); },
    (c) => { c.upstream.url = 'https://github.com/someone/else.git'; },
    (c) => { c.hookEnv.NEEV_UPSTREAM_CANDIDATE_SHA = 'x'; },
    (c) => { c.hookEnv.GIT_DIR = '/tmp'; },
    (c) => { c.worktreesDir = path.join(c.repoPath, 'wt'); },
    (c) => { c.extra = true; },
  ];
  for (const mutate of bad) {
    const config = structuredClone(base);
    mutate(config);
    assert.throws(() => validateConfig(config), /Invalid updater config/, mutate.toString());
  }
  const dry = structuredClone(base);
  dry.mode = 'dry-run';
  delete dry.hooks.deploy; delete dry.hooks.rollback; delete dry.hooks.activeRunsGate; delete dry.hooks.healthCheck;
  assert.ok(validateConfig(dry), 'dry-run needs no deployment hooks');
  fs.chmodSync(f.configPath, 0o666);
  return assert.rejects(f.run(), /world-writable/);
});

test('CLI exits with documented codes', async (t) => {
  const f = fixture(t);
  await f.init();
  const cli = (...args) => {
    try {
      execFileSync(process.execPath, [UPDATER, ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
      return 0;
    } catch (error) { return error.status; }
  };
  assert.equal(cli('run', '--config', f.configPath), 0);
  assert.equal(cli('status', '--config', f.configPath), 0);
  assert.equal(cli('run'), 1);
  assert.equal(cli('run', '--config', f.configPath, '--deploy-now'), 1);
});

test('clean merges add nothing, evil merges taint, and patches already released are not reapplied', async (t) => {
  const f = fixture(t);
  const released = f.onRelease({ 'docs/url-scheme.md': '# URL scheme\n\nShared fix.\n' }, 'downstream already has it');
  await f.init();
  const previous = f.releaseSha();
  f.git('checkout', '-q', 'main');
  const same = f.commit({ 'docs/url-scheme.md': '# URL scheme\n\nShared fix.\n' }, 'upstream same fix');
  f.git('checkout', '-q', '-b', 'feature');
  const featureCommit = f.commit({ 'src/components/ui/card.tsx': CARD.replace('rounded', 'rounded-3xl') }, 'feature card');
  f.git('checkout', '-q', 'main');
  f.git('merge', '-q', '--no-ff', '-m', 'Merge feature', 'feature');
  const cleanMerge = f.git('rev-parse', 'HEAD');
  f.git('checkout', '-q', '-b', 'evil');
  f.commit({ 'docs/chat-channels/a.md': '# A\n' }, 'evil side');
  f.git('checkout', '-q', 'main');
  f.git('merge', '-q', '--no-ff', '--no-commit', 'evil');
  fs.writeFileSync(path.join(f.upstream, 'src/components/ui/button.tsx'), BUTTON.replace('px-2', 'px-9'));
  f.git('add', '-A');
  f.git('commit', '-q', '-m', 'Evil merge');
  const evilMerge = f.git('rev-parse', 'HEAD');
  const afterEvil = f.commit({ 'src/components/ui/button.tsx': BUTTON.replace('px-2', 'px-10') }, 'touches evil path');

  const result = await f.run();
  assert.equal(result.status, 'deployed', result.reason);
  const by = Object.fromEntries(result.report.commits.map((c) => [c.sha, c]));
  assert.equal(by[same], undefined, 'equivalent patch already in release is not pending');
  assert.equal(result.report.alreadyPresent, 1);
  assert.equal(by[featureCommit].decision, 'applied');
  assert.equal(by[cleanMerge].decision, 'merge_clean');
  assert.equal(by[evilMerge].decision, 'skipped');
  assert.equal(by[afterEvil].decision, 'skipped');
  assert.deepEqual(f.changed(previous, result.report.candidate).sort(),
    ['docs/chat-channels/a.md', 'src/components/ui/card.tsx']);
  assert.ok(released);
});

test('the shipped example config stays valid and pins the current allowlist', () => {
  const example = JSON.parse(fs.readFileSync(new URL('./updater.config.example.json', import.meta.url), 'utf8'));
  assert.equal(example.mode, 'dry-run', 'example defaults to dry-run');
  assert.ok(validateConfig(example));
  assert.ok(validateConfig({ ...example, mode: 'deploy' }));
});
