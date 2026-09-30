// Hardened Git invocation. No inherited environment, no global/system config,
// no hooks, no fsmonitor, no submodule recursion, no rename-following merges,
// no SSH and no credential prompts. Arguments are always argv, never a shell.
import { spawnSync } from 'node:child_process';

export const SHA_RE = /^[a-f0-9]{40}$/;

const HARDENING = [
  'core.hooksPath=/dev/null', 'core.fsmonitor=false', 'core.attributesFile=/dev/null',
  'core.quotePath=false', 'core.pager=cat', 'color.ui=false', 'submodule.recurse=false',
  'rerere.enabled=false', 'merge.renames=false', 'diff.renames=false', 'diff.external=',
  'commit.gpgSign=false', 'tag.gpgSign=false', 'gc.auto=0', 'maintenance.auto=false',
  'fetch.recurseSubmodules=false', 'protocol.ext.allow=never', 'advice.detachedHead=false',
].flatMap((setting) => ['-c', setting]);

export class GitError extends Error {
  constructor(args, status, stderr) {
    // stderr can echo untrusted paths; callers log it to files, never to Markdown.
    super(`git ${args[0]} exited with ${status}`);
    this.status = status;
    this.stderr = String(stderr ?? '').slice(0, 4000);
  }
}

export function gitEnv(home, extra = {}) {
  return {
    PATH: process.env.PATH || '/usr/local/bin:/usr/bin:/bin',
    HOME: home,
    LANG: 'C', LC_ALL: 'C', TZ: 'UTC',
    GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_TERMINAL_PROMPT: '0', GIT_SSH_COMMAND: 'false', GIT_ASKPASS: 'false',
    GIT_ALLOW_PROTOCOL: 'https:file', GIT_NO_REPLACE_OBJECTS: '1', GIT_LFS_SKIP_SMUDGE: '1',
    ...extra,
  };
}

/**
 * Create a git runner bound to a directory.
 * git(args, { cwd, env, allowFail, buffer, timeoutMs })
 */
export function makeGit(cwd, home) {
  const baseEnv = gitEnv(home);
  return function git(args, opts = {}) {
    const res = spawnSync('git', [...HARDENING, ...args], {
      cwd: opts.cwd ?? cwd,
      env: { ...baseEnv, ...(opts.env ?? {}) },
      encoding: opts.buffer ? 'buffer' : 'utf8',
      maxBuffer: 128 * 1024 * 1024,
      timeout: opts.timeoutMs ?? 300_000,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    if (res.error) throw res.error;
    if (opts.allowFail) return res;
    if (res.status !== 0) throw new GitError(args, res.status, res.stderr);
    return res.stdout;
  };
}

export function isAncestor(git, before, after) {
  const res = git(['merge-base', '--is-ancestor', before, after], { allowFail: true });
  if (res.status === 0) return true;
  if (res.status === 1) return false;
  throw new GitError(['merge-base'], res.status, res.stderr);
}

export function verifyCommit(git, sha) {
  if (typeof sha !== 'string' || !SHA_RE.test(sha)) throw new Error('Expected a full immutable commit SHA');
  const res = git(['cat-file', '-t', sha], { allowFail: true });
  if (res.status !== 0 || res.stdout.trim() !== 'commit') throw new Error('Expected an existing commit object');
  return sha;
}

/** Parse `git diff-tree -r -z --raw --no-renames` output into change records. */
export function parseRawDiff(output) {
  const parts = output.split('\0');
  const changes = [];
  for (let i = 0; i < parts.length - 1; i += 2) {
    const meta = parts[i];
    if (!meta.startsWith(':')) throw new Error('Unexpected raw diff format');
    const [oldMode, newMode, oldBlob, newBlob, status] = meta.slice(1).split(' ');
    changes.push({ oldMode, newMode, oldBlob, newBlob, status: status[0], path: parts[i + 1] });
  }
  return changes;
}

export function rawChanges(git, from, to) {
  return parseRawDiff(git([
    'diff-tree', '-r', '-z', '--raw', '--no-renames', '--no-ext-diff', '--full-index', from, to,
  ]));
}
