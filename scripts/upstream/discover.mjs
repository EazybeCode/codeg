import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

function git(cwd, args) {
  return execFileSync('git', ['--no-replace-objects', ...args], {
    cwd, encoding: 'utf8', timeout: 60_000, maxBuffer: 32 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function verifyCommit(cwd, sha) {
  if (typeof sha !== 'string' || !/^[a-f0-9]{40}$/.test(sha)) {
    throw new Error('Expected a full immutable commit SHA');
  }
  if (git(cwd, ['cat-file', '-t', sha]).trim() !== 'commit') {
    throw new Error('Expected a commit object');
  }
}

function isAncestor(cwd, before, after) {
  try {
    git(cwd, ['merge-base', '--is-ancestor', before, after]);
    return true;
  } catch (error) {
    if (error.status === 1) return false;
    throw error;
  }
}

export function discover({ cwd, incorporated, observed, downstream }) {
  for (const sha of [incorporated, observed, downstream]) verifyCommit(cwd, sha);
  const report = {
    schemaVersion: 1, incorporated, observed, downstream,
    automaticMerge: false, automaticDeploy: false,
    status: 'blocked', risk: 'human_review_required',
    commitCount: null, changedPaths: [],
  };
  if (!isAncestor(cwd, incorporated, downstream)) {
    return { ...report, reason: 'The recorded incorporated commit is not in downstream history.' };
  }
  if (!isAncestor(cwd, incorporated, observed)) {
    return { ...report, reason: 'Upstream history no longer descends from the incorporated checkpoint.' };
  }
  const commitCount = Number(git(cwd, ['rev-list', '--count', `${incorporated}..${observed}`]).trim());
  const changedPaths = git(cwd, [
    'diff', '--no-ext-diff', '--no-textconv', '--name-only', '-z', incorporated, observed, '--',
  ]).split('\0').filter(Boolean);
  return {
    ...report, status: commitCount === 0 ? 'current' : 'updates_available',
    risk: commitCount === 0 ? 'none' : 'human_review_required', commitCount, changedPaths,
  };
}

function escapeMarkdown(value) {
  return value.replace(/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/g, ' ')
    .replace(/[&<>]/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[char])
    .replace(/[\\`*_{}[\]()#+.!|~-]/g, '\\$&');
}

export function renderSummary(report) {
  const title = report.status === 'current' ? 'Upstream is current'
    : report.status === 'blocked' ? 'Discovery blocked' : 'Human review required';
  const lines = [
    `## WorkOS upstream: ${title}`, '',
    'Read-only monitoring. No upstream code was executed, incorporated, merged or deployed.', '',
    `- Incorporated checkpoint: \`${report.incorporated}\``,
    `- Observed upstream: \`${report.observed}\``,
    `- Downstream inspected: \`${report.downstream}\``,
  ];
  if (report.reason) lines.push('', escapeMarkdown(report.reason));
  if (report.commitCount !== null) {
    lines.push(`- Pending commits: ${report.commitCount}`, `- Changed paths: ${report.changedPaths.length}`);
  }
  if (report.changedPaths.length) {
    lines.push('', '### Changed paths (first 100; complete list in JSON artifact)', '');
    lines.push(...report.changedPaths.slice(0, 100).map((path) => `- ${escapeMarkdown(path)}`));
  }
  return lines.join('\n') + '\n';
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    if (args.length !== 3) throw new Error('Usage: discover.mjs INCORPORATED_SHA OBSERVED_SHA DOWNSTREAM_SHA');
    const [incorporated, observed, downstream] = args;
    const report = discover({ cwd: process.cwd(), incorporated, observed, downstream });
    mkdirSync('upstream-report', { recursive: true });
    writeFileSync('upstream-report/report.json', JSON.stringify(report, null, 2) + '\n');
    writeFileSync('upstream-report/summary.md', renderSummary(report));
    console.log(`Upstream discovery: ${report.status}; automatic merge/deploy disabled.`);
    if (report.status === 'blocked') process.exitCode = 2;
  } catch {
    // Git errors may contain untrusted paths. Keep runner log commands free of them.
    console.error('Upstream discovery failed. Check full commit IDs and available Git history.');
    process.exitCode = 1;
  }
}
