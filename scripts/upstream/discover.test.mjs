import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync, existsSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { discover, renderSummary } from './discover.mjs';

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'workos-upstream-test-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '-b', 'main');
  git('config', 'user.name', 'Upstream fixture');
  git('config', 'user.email', 'fixture@example.invalid');
  git('config', 'commit.gpgsign', 'false');
  const commit = (path, content, message = 'fixture change') => {
    writeFileSync(join(cwd, path), content);
    git('add', '--', path);
    git('commit', '-m', message);
    return git('rev-parse', 'HEAD');
  };
  const base = commit('README.md', 'Initial\n');
  return { cwd, git, commit, base };
}

test('unchanged upstream reports current without moving incorporated checkpoint', (t) => {
  const f = fixture(t);
  const report = discover({ cwd: f.cwd, incorporated: f.base, observed: f.base, downstream: f.base });
  assert.equal(report.status, 'current');
  assert.equal(report.commitCount, 0);
  assert.deepEqual(report.changedPaths, []);
  assert.equal(report.incorporated, f.base);
  assert.equal(report.automaticMerge, false);
});

test('updates are reported for human review without modifying refs or files', (t) => {
  const f = fixture(t);
  const observed = f.commit('README.md', 'A correction\n');
  const before = f.git('show-ref');
  const args = { cwd: f.cwd, incorporated: f.base, observed, downstream: f.base };
  const report = discover(args);
  assert.equal(report.status, 'updates_available');
  assert.equal(report.commitCount, 1);
  assert.deepEqual(report.changedPaths, ['README.md']);
  assert.equal(report.risk, 'human_review_required');
  assert.equal(report.automaticMerge, false);
  assert.equal(report.incorporated, f.base);
  assert.equal(report.observed, observed);
  assert.deepEqual(discover(args), report);
  assert.equal(f.git('show-ref'), before);
  assert.equal(f.git('status', '--porcelain'), '');
});

test('rewritten upstream and an unincorporated downstream fail closed', (t) => {
  const f = fixture(t);
  f.git('checkout', '--orphan', 'rewritten');
  const other = f.commit('README.md', 'Unrelated history\n');
  for (const args of [
    { incorporated: f.base, observed: other, downstream: f.base },
    { incorporated: f.base, observed: f.base, downstream: other },
  ]) {
    const report = discover({ cwd: f.cwd, ...args });
    assert.equal(report.status, 'blocked');
    assert.equal(report.automaticMerge, false);
    assert.equal(report.risk, 'human_review_required');
    assert.equal(report.incorporated, f.base);
    assert.ok(report.reason.length > 0);
  }
});

test('untrusted filenames and commit titles cannot run commands or inject Markdown', (t) => {
  const f = fixture(t);
  const path = '$(touch PWNED) [link](evil)\n### injected.md';
  const observed = f.commit(path, 'text', '$(touch PWNED)');
  const report = discover({ cwd: f.cwd, incorporated: f.base, observed, downstream: f.base });
  assert.deepEqual(report.changedPaths, [path]);
  const summary = renderSummary(report);
  assert.equal(existsSync(join(f.cwd, 'PWNED')), false);
  assert.equal(summary.includes('\n### injected'), false);
  assert.equal(summary.includes('[link](evil)'), false);
  assert.match(summary, /Human review required/);
});

test('rejects abbreviated refs, options, nonexistent objects and noncommit objects', (t) => {
  const f = fixture(t);
  const tree = f.git('rev-parse', 'HEAD^{tree}');
  for (const invalid of ['HEAD', f.base.slice(0, 8), '--help', '0'.repeat(40), tree]) {
    assert.throws(() => discover({ cwd: f.cwd, incorporated: invalid, observed: f.base, downstream: f.base }));
  }
});

test('CLI persists complete report and summary, and exits unsuccessfully for blocked history', (t) => {
  const f = fixture(t);
  const script = fileURLToPath(new URL('./discover.mjs', import.meta.url));
  execFileSync(process.execPath, [script, f.base, f.base, f.base], { cwd: f.cwd });
  const reportPath = join(f.cwd, 'upstream-report/report.json');
  assert.equal(existsSync(reportPath), true);
  assert.equal(JSON.parse(readFileSync(reportPath, 'utf8')).status, 'current');
  assert.match(readFileSync(join(f.cwd, 'upstream-report/summary.md'), 'utf8'), /Upstream is current/);
  f.git('checkout', '--orphan', 'unrelated');
  const other = f.commit('README.md', 'Different\n');
  assert.throws(() => execFileSync(process.execPath, [script, f.base, other, f.base], {
    cwd: f.cwd, stdio: ['ignore', 'pipe', 'pipe'],
  }), (error) => error.status === 2);
  assert.equal(JSON.parse(readFileSync(reportPath, 'utf8')).status, 'blocked');
});
