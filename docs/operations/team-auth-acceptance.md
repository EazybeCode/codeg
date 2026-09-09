# Authentication library acceptance — 2026-09-09

Accepted source: `76b28dfe` (`fix(auth): serialize final guard with session rotation`). Scope: the credential/onboarding/principal implementation in `2026-09-08-team-auth-orchestration.md`, not the team HTTP router, browser UI, catalog privacy or agent runtime.

The final review's cross-account session-rotation race is fixed. Final mutation guards lock membership then the exact session in the caller-owned transaction. PostgreSQL session deletion conflicts with the session-row lock; a guard behind a committed deletion rejects it. SQLite obtains writer serialization and checks session existence. No subsequent user/org locks or new authority are introduced.

Both real PostgreSQL orderings failed behaviorally before the fix and passed afterward. Tests also prove two distinct cookies share one in-flight membership proof without the initiating cookie's revocation denying the surviving cookie, and the largest retained onboarding payload fits the encrypted envelope without creating workspace authority.

## Fresh verification

Run in `src-tauri` on the accepted commit, with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`:

| Command | Result |
| --- | --- |
| `cargo test --no-default-features --lib team_access -- --include-ignored --test-threads=1` | 137 passed, 0 failed, 0 ignored |
| `cargo test --no-default-features --lib team_credentials -- --include-ignored --test-threads=1` | 11 passed, 0 failed, 0 ignored |
| `cargo test --no-default-features --lib oauth_state_tests -- --test-threads=1` | 4 passed, 0 failed |

All commands exited zero: 152 distinct targeted tests, not the entire application suite. Real PostgreSQL fixtures ran locally; a final read-only catalog check found no remaining auth/access/session/credential fixture databases. `git diff --check` was clean. Independent scoped review found the Important race and two coverage minors addressed, with no new findings.

The prior disk blocker was resolved by user cleanup (16GiB available at resumption; about 13GiB after verification). Rustfmt remains unavailable. Existing unused-import/dead-code, linker-unwind and dependency future-compatibility warnings remain; no pristine-format or warning-free claim is made.

## Recorded rulings and trade-offs

1. Migration fixtures use an explicit named target instead of positional rollback when extended. Cost if mistaken: fixture rework; no production data deletion is authorized. The inherited credential fixture's named-target cleanup remains due when its next migration is added.
2. No stored credential means no prior generation, not generation zero. Cost if mistaken: explicit reauthentication for old zero-generation fixtures; orphan authority is not silently promoted.
3. Preserve typed unique/serialization/deadlock database failures as Conflict; other storage failures remain Unavailable. Cost if mistaken: retry rather than unavailable UI for a rolled-back operation, not an authority grant.
4. The final guard locks membership then exact session, rather than taking another account's membership lock during selection. Cost: an extra locked query and possible contention; if incorrect, implementation rework. Existing identity, revision, generation, CSRF and current-time checks remain binding.

## Continuation and exclusions

Execute the dedicated team HTTP/browser onboarding plan next. Do not expose the legacy router or catalog with new team cookies, and do not deploy an intermediate signed-in shell as a finished product. Catalog/settings/private-org chats, scoped files/events, per-job isolation, real-provider consent and multi-person browser acceptance remain separate release gates.

The reviewed work has not been pushed or deployed. Publication remains subject to the interactive organization ship gate and must target Workos, never the original codeg remote.

Context: [org-memory: wiki/engineering/workos.md]. The wiki describes the older single-operator fork; current source and the approved team specification govern this acceptance.
