# Chat privacy persistence — acceptance checkpoint

Date: September 10, 2026.

Status: storage Task1 accepted through `a7a3d505`; fingerprint Task2 independently accepted at `82f252c8`. Final combined integration review remains pending. This record does not certify shared/private team routes, a working team workspace, agent execution or public rollout.

## Accepted storage change

`bddcaedc` adds four constrained columns to `workspace_chat_draft`: current visibility, access revision, original creation visibility and creation-fingerprint version. Legacy rows and legacy/native inserts receive private/0/private/1. The ordered organization/visibility/creator/id index supports the subsequent resource predicate. No old creator, repository, branch, request or execution mapping is reassigned, and the migration makes no positive admission decision about old rows.

Current audience may differ from original creation audience; this preserves explicit future Share semantics. Version1 requires private original creation. SQLite rejects non-integer stored revisions, and PostgreSQL uses BIGINT. Populated rollback refuses before removing any schema evidence; empty rollback removes only the owned index/columns in dependency-safe order.

The first independent task review found three test gaps, not a reported defect in production migration code: incomplete preservation snapshots, incomplete PostgreSQL schema/parity checks, and unnamed failed fixtures on panic. Test-only fix `a7a3d505` closes all three. Scoped re-review marked each ADDRESSED with no new breakage. The database fixtures include two organizations and orphan/foreign/cyclic historical records; those remain unchanged and private, not repaired or admitted.

## Verification evidence

| Check | Source / result |
| --- | --- |
| Initial RED | Missing new migration module, E0432, before implementation. |
| Worker focused SQLite | 8 passed, 0 failed, 1 explicit PostgreSQL opt-in ignored. |
| Worker explicit PostgreSQL | 1 passed, 0 failed, 0 ignored; fresh-versus-upgrade columns/defaults/checks/index order, preservation, refused-down equality and empty down/up. |
| Root fresh covering storage/catalog selection | Amended executable at `a7a3d505`: 96 passed, 0 failed, 0 ignored, 5.42s; includes selected guarded PostgreSQL controls. |
| Root adjacent team auth/server selection | `bddcaedc`: 228 passed, 0 failed, 0 ignored, 37.80s; excludes the intentionally long-lived browser-fixture server. The subsequent fix changes only tests. |
| Standalone server check | Production code at `bddcaedc`, `cargo check --bin codeg-server --no-default-features`, exit0. |
| Diff check | Both storage implementation and test-fix ranges pass `git diff --check`. |

The matching library test executable was `src-tauri/target/debug/deps/codeg_lib-732148cb61475fb4`. The root covering filter was `chat_privacy_tests workspace_catalog db::migration::team_credentials_tests db::migration::personal_agent_connections_tests --include-ignored --test-threads=1 --quiet`. These are selected suites, not a whole-repository test claim.

An initial PostgreSQL helper incorrectly decoded INT4 IDs as i64; it was corrected before GREEN. Its failed disposable fixture remains preserved as `workos_chat_privacy_test_4cec4fefc9eb47fa9c8523e9d231ebda`. Subsequent successful exact fixtures were verified and removed. The first review-fix compile also caught an oversized Rust tuple and an accidental unit unwrap; both were corrected before the reported final run.

The standalone check has 63 existing warnings. Root independently inspected the cached Cargo diagnostics: every located warning points to the older title migration, inactive team-agent lifecycle modules, or team-access session code, not a Task1 file. Existing linker/future-compatibility noise is also retained as a maintenance observation; test output is not represented as pristine.

## Accepted creation fingerprints

`82f252c8` extracts the exact legacy byte encoder into a pure module and adds an audience-aware version2 format. Version1 retains the original signed node/repository IDs, big-endian byte lengths and absent-versus-empty title distinction. Version2 hashes the NUL-terminated `neev:chat-create:v2` domain separator, raw version1 digest and original creation-audience byte. Version1 rejects organization-wide creation. The unchanged legacy adapter still calls version1/private; no new client authority or team route is added.

Six pure tests passed, including independent fixed and Unicode goldens. Eleven persisted draft tests passed, including retries/conflicts that keep the stored fingerprint, original metadata and row count unchanged. Library and standalone server checks exited0. Independent task review found no Critical/Important issue or unverifiable requirement. Two new dead-code warnings for the deliberately staged Organization/AudienceV2 variants remain documented, with no blanket suppression.

Root fresh verification at `82f252c8` passed337 selected backend tests,0failed/0ignored,34.75s, including guarded PostgreSQL checks. The full frontend suite passed6165tests in444files,166.88s, exit0. The latter includes retained operator UI and emits existing-code React act/hydration, synthetic error-path, jsdom canvas, KaTeX and Vite warnings; it is not pristine-output evidence. The narrower team UI run passed92tests in12files. The new three-commit privacy range passed a redacted secret scan with no leaks.

## Remaining boundary

The later team catalog must prove tenant/creator mapping, quarantine uncertain legacy rows, use session-derived authority and independent repository permissions, and apply one private/org predicate to lists, direct reads and counts. Creator-only Share requires expected revisions and active-writer rejection. Files/events/search/wiki/jobs require the same scope and remain disabled until separately accepted.

No private transcript or generated artifact is automatically published to GitHub or the organization wiki by this change. No GitHub content, live database, deployment or runtime was changed by either storage commit. The earlier reviewed application checkpoint was published and rehearsed separately; see [release rehearsal](neev-release-rehearsal-2026-09-10.md).

Context: [org-memory: wiki/engineering/workos.md]. Current source and the approved team-visibility design govern the migration; the older operator model does not authorize team access.
