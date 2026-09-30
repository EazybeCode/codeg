# codeg upstream updates

Two separate pieces:

1. **Discovery (GitHub Action, read-only).** `.github/workflows/workos-upstream.yml` runs daily at 03:17 UTC, and on demand with **Run workflow**. It fetches public `xintaofei/codeg` `main` and writes a report that includes a NON-CORE eligibility preview. It never checks out or runs upstream code, merges, pushes or deploys.
2. **NON-CORE updater (server, unattended).** `scripts/upstream/updater.mjs` runs once a day from a server cron. It applies only presentation-only upstream commits to the exact released SHA, tests and builds them, then deploys them through trusted local hooks. It rolls back on failure.

Status: the code and tests are in the repository. **Neither the Action nor the server cron is live yet.**
- The Action only runs from the default branch of `EazybeCode/neev`, which is `multitenancy`. This workflow file must be merged there before it runs.
- The updater runs only after the release owner installs it using `.context/steering-restart/updater-contract.md`.

## Policy: what can ship unattended

The trusted allowlist is a code constant in `scripts/upstream/policy.mjs`. `.github/workos-upstream.json` must repeat it exactly, and the server config must pin its SHA-256. If any of the three disagree, the run fails closed.

| Allowed path | Why |
|---|---|
| `src/components/ui/*.tsx` | Shared UI primitives. One directory level only. |
| `docs/readme/README.*.md`, `docs/chat-channels/*.md`, `docs/url-scheme.md` | Upstream user docs. |
| `docs/images/*.{png,jpg,gif}` | README images. Checked by file signature. No SVG. |

The whole upstream commit is the unit. A commit is skipped and reported if **any** of these is true:
- **Path:** any file outside the allowlist, or any protected path. Protected paths are:
  - Neev core
  - `src/app` (globals and layouts), `src/lib`, `src/i18n`, `src/hooks`
  - `src-tauri`
  - database, schema or migration files
  - auth, credential, session or security names
  - dependencies and lockfiles
  - build config, Docker, workflows, scripts, deploy files
  - agent instruction files, dotfiles
  - CSS, JS/TS (non-TSX), JSON, YAML, SVG, HTML

  Matching is case-folded.
- **File type or change:**
  - a symlink, submodule, executable bit, type change or rename
  - an edit or delete of an existing test (new tests are allowed)
  - a file over 2 MiB
- **Content:** any added line that fails the content scan:
  - network calls or external URLs
  - dynamic imports or `eval`
  - raw HTML
  - browser storage access
  - Tauri/`invoke` bridges
  - obfuscation patterns
  - imports outside a fixed list: React, Radix/Base UI, lucide, cva, cmdk, `@/lib/utils`, `@/components/ui/*`

  This scan adds defense in depth. It is not a sandbox.
- **Dependency on a skipped change:** the commit touches a path that an earlier skipped commit also touched.
- **Merge commits:** a merge whose tree differs from Git's own clean merge of its parents. Clean merges add nothing and are ignored.
- **Trial merge problems:**
  - the cherry-pick conflicts, or
  - the resulting commit touches different paths or modes than the upstream commit did (for example, a rename-following merge)

Widening the allowlist needs four things: a reviewed code change, a matching manifest change, a new pinned digest in server config, and tests. The updater never widens it by itself.

## Updater run, step by step

1. **Lock.** Takes a single-instance lock (`state/updater.lock`). A lock held by a live process is never broken.
2. **Stop if halted.** If the updater is halted, it stops. A failed deploy halts it, and so does a deploy interrupted mid-way (`inFlight`).
3. **Read the release.** Reads the released SHA from the trusted `currentRelease` hook. Validates the release's upstream manifest.
4. **Fetch upstream.** Fetches upstream `main` with a non-forced refspec into a **dedicated bare repository**. Rewritten upstream history blocks the run.
5. **List pending commits.** These are commits since the recorded baseline, minus patches the release already contains. If there are more than `maxRangeCommits` (default 300), the run is blocked.
6. **Build the candidate.** Checks out the release SHA in a throwaway worktree, then cherry-picks eligible commits one by one (`-x`). Git runs with no global/system config, no hooks, no fsmonitor, no rename detection, no submodules, no SSH and no inherited environment.
7. **Verify the candidate as a whole.** Every changed path must be allowlisted. The digest of all non-allowlisted tree entries must equal the release's.
8. **Validate and build.** Runs the configured validation hooks, then the build hooks, inside the worktree with a minimal environment. Afterwards, tracked files must be unchanged and HEAD must still be the candidate.
9. **Gate.** Runs the active-runs gate. If it doesn't pass, deployment is deferred to the next day.
10. **Deploy and check health.** Runs the deploy hook, then the health check. If either fails, it runs the rollback hook with the previous SHA, then halts. If rollback also fails, the report says so explicitly.
11. **Record the result.** On success it records the deployment and moves `refs/workos-upstream/deployed`.

After every run:
- The report is written to `reports/<runId>/{report.json,summary.md,logs/}`. Hook logs are bounded, and only the newest `reportRetention` runs are kept.
- The worktree is always removed.

A second run on the same inputs is a no-op, because applied patches are detected in the release.

`--dry-run` (or `"mode": "dry-run"`) runs steps 1–8 only. It never runs the gate, deploy or rollback hooks.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | `current`, `deployed`, `dry_run_passed` or `deferred_active_runs` |
| 1 | error or config error |
| 2 | blocked or validation failed |
| 3 | locked |
| 4 | halted, rolled back or rollback failed |

## Commands

```bash
node --test scripts/upstream/*.test.mjs            # trusted-code tests (no network, no builds)
node scripts/upstream/updater.mjs run --config /etc/neev-upstream/config.json [--dry-run]
node scripts/upstream/updater.mjs record-baseline --config ... [--upstream SHA] [--allow-unmerged-baseline] [--note TEXT]
node scripts/upstream/updater.mjs status --config ...
node scripts/upstream/updater.mjs clear-halt --config ... --reason "what was fixed"
node scripts/upstream/discover.mjs <incorporated> <observed> <downstream>   # read-only report
```

## Backlog note

On 2026-09-30 the recorded checkpoint `ccc420c2` was 530 commits behind upstream `478ff2fd`. A read-only preview found:
- 2 eligible commits (docs image refreshes)
- 448 skipped
- the remaining 80 were clean merges or empty

This backlog exceeds the 300-commit limit, so the updater stays blocked until one of these happens:
- **Manual catch-up merge:** the audit recommends this. Afterwards, run `record-baseline` with the merged upstream SHA.
- **Start from the current tip:** run `record-baseline --upstream <current tip> --allow-unmerged-baseline`, so only new upstream commits are considered.

The backlog stays human work in both cases.
