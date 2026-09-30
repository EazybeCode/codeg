// Whole-commit eligibility for upstream commits. Shared by the read-only
// discovery report and the unattended updater. Upstream content is data only:
// it is read through `git cat-file` and `git diff`, never checked out or run here.
import { classifyChanges, checkBlob, scanAddedLines } from './policy.mjs';
import { rawChanges, GitError } from './git.mjs';

const SUBJECT_MAX = 200;

function subjectOf(git, sha) {
  return git(['log', '-1', '--format=%s', sha]).trim().slice(0, SUBJECT_MAX);
}

function parentsOf(git, sha) {
  return git(['rev-list', '--parents', '-n', '1', sha]).trim().split(' ').slice(1);
}

export function addedLines(git, parent, sha, path) {
  const out = git([
    'diff', '--no-color', '--no-ext-diff', '--no-textconv', '--no-renames', '-U0', parent, sha, '--', path,
  ]);
  const lines = [];
  let inHunk = false;
  for (const line of out.split('\n')) {
    if (line.startsWith('@@')) { inHunk = true; continue; }
    if (!inHunk) continue;
    if (line.startsWith('+')) lines.push(line.slice(1));
  }
  return { lines, binary: !inHunk && /^Binary files /m.test(out) };
}

/** Check contents of every added or modified blob in an otherwise path-eligible change set. */
export function contentRejections(git, parent, sha, changes) {
  const rejections = [];
  for (const change of changes) {
    if (change.status === 'D') continue;
    const blob = git(['cat-file', 'blob', change.newBlob], { buffer: true });
    const blobReason = checkBlob(change.path, blob);
    if (blobReason) { rejections.push({ path: change.path, reason: blobReason }); continue; }
    if (/\.(tsx|md)$/.test(change.path)) {
      const { lines, binary } = addedLines(git, parent, sha, change.path);
      if (binary) { rejections.push({ path: change.path, reason: 'binary_text' }); continue; }
      for (const reason of scanAddedLines(change.path, lines)) rejections.push({ path: change.path, reason });
    }
  }
  return rejections;
}

/**
 * Evaluate one upstream commit in isolation.
 * decision: 'eligible' | 'ineligible' | 'empty' | 'merge_clean'
 */
export function evaluateCommit(git, sha) {
  const parents = parentsOf(git, sha);
  const subject = subjectOf(git, sha);
  const base = { sha, subject, parents };
  if (parents.length === 0) {
    return { ...base, decision: 'ineligible', paths: [], changes: [], rejections: [{ path: '', reason: 'root_commit' }] };
  }
  if (parents.length > 1) {
    // A merge whose tree equals Git's own clean merge of its parents adds nothing
    // beyond the individually evaluated commits. Anything else (conflict
    // resolution, octopus, evil merge) is reviewed by a human and taints paths.
    let computed = null;
    if (parents.length === 2) {
      const res = git(['merge-tree', '--write-tree', '--no-messages', parents[0], parents[1]], { allowFail: true });
      if (res.status === 0) computed = res.stdout.split('\n')[0].trim();
    }
    const actual = git(['rev-parse', `${sha}^{tree}`]).trim();
    if (computed && computed === actual) return { ...base, decision: 'merge_clean', paths: [], changes: [], rejections: [] };
    const changes = rawChanges(git, computed ?? parents[0], sha);
    return {
      ...base, decision: 'ineligible', changes, paths: changes.map((c) => c.path),
      rejections: [{ path: '', reason: computed ? 'merge_with_own_changes' : 'merge_not_clean' }],
    };
  }
  const changes = rawChanges(git, parents[0], sha);
  const paths = changes.map((c) => c.path);
  if (changes.length === 0) return { ...base, decision: 'empty', paths, changes, rejections: [] };
  const { eligible, rejections } = classifyChanges(changes);
  if (!eligible) return { ...base, decision: 'ineligible', paths, changes, rejections };
  const content = contentRejections(git, parents[0], sha, changes);
  if (content.length) return { ...base, decision: 'ineligible', paths, changes, rejections: content };
  return { ...base, decision: 'eligible', paths, changes, rejections: [] };
}

/**
 * List pending upstream commits after `baselineUpstream` up to `observed`, oldest
 * first, dropping those whose patch already exists in `release`.
 */
export function pendingCommits(git, { baselineUpstream, observed, release }) {
  const all = git(['rev-list', '--reverse', '--topo-order', `${baselineUpstream}..${observed}`])
    .split('\n').filter(Boolean);
  const present = new Set();
  for (const line of git(['cherry', release, observed, baselineUpstream]).split('\n')) {
    if (line.startsWith('- ')) present.add(line.slice(2).trim());
  }
  return { all, pending: all.filter((sha) => !present.has(sha)), alreadyPresent: all.filter((sha) => present.has(sha)) };
}

/**
 * Walk pending commits in order. Any skipped commit taints its paths; a later
 * commit touching a tainted path is skipped too, so we never ship a partial
 * slice of a dependency chain. `apply(evaluation)` performs (or simulates) the
 * trial application and returns { ok: true } or { ok: false, reason, taint }.
 */
export function walkCommits(git, shas, apply) {
  const tainted = new Set();
  const results = [];
  for (const sha of shas) {
    let evaluation;
    try {
      evaluation = evaluateCommit(git, sha);
    } catch (error) {
      if (!(error instanceof GitError)) throw error;
      evaluation = { sha, subject: '', parents: [], decision: 'ineligible', paths: [], changes: [],
        rejections: [{ path: '', reason: 'evaluation_failed' }], taintAll: true };
    }
    const record = {
      sha, subject: evaluation.subject, decision: evaluation.decision,
      paths: evaluation.paths, reasons: evaluation.rejections,
    };
    if (evaluation.taintAll) {
      // Cannot know what it touched: stop applying anything after it.
      results.push({ ...record, decision: 'skipped' });
      for (const rest of shas.slice(shas.indexOf(sha) + 1)) {
        results.push({ sha: rest, subject: '', decision: 'skipped', paths: [],
          reasons: [{ path: '', reason: 'after_unevaluable_commit' }] });
      }
      return results;
    }
    if (evaluation.decision === 'empty' || evaluation.decision === 'merge_clean') {
      results.push(record);
      continue;
    }
    if (evaluation.decision === 'ineligible') {
      evaluation.paths.forEach((p) => tainted.add(p));
      results.push({ ...record, decision: 'skipped' });
      continue;
    }
    const dependent = evaluation.paths.filter((p) => tainted.has(p));
    if (dependent.length) {
      evaluation.paths.forEach((p) => tainted.add(p));
      results.push({ ...record, decision: 'skipped',
        reasons: dependent.map((path) => ({ path, reason: 'depends_on_skipped_change' })) });
      continue;
    }
    const outcome = apply(evaluation);
    if (outcome.ok) {
      results.push({ ...record, decision: outcome.decision ?? 'applied', candidate: outcome.candidate });
    } else {
      if (outcome.taint !== false) evaluation.paths.forEach((p) => tainted.add(p));
      results.push({ ...record, decision: outcome.decision ?? 'skipped',
        reasons: [{ path: '', reason: outcome.reason }] });
    }
  }
  return results;
}
