# Team catalog acceptance — September 10, 2026

Status: persistence/service accepted through `36fbd3ae`, protected HTTP through
`7de2329d`, and browser workspace through review fix `01116f90`. All three task
gates pass; combined review and final aggregate are pending. This record is not permission to
open signup or evidence of working prompts, personal agents, or public HTTPS.

## Accepted service boundary

`83907ee1` adds principal-scoped repositories, projects, recursive folders and
working directories, plus repository-backed private/organization chat metadata.
The new admission relation starts empty. Legacy nodes/chats are neither deleted
nor automatically granted team access. Repository reconnection requires personal
GitHub proof of the same numeric repository identity.

Authority derives from the checked session, selected organization and current
personal credential generation. Writes recheck authority after external proof;
database locks are not held across that network request. Admin status does not
grant access to another member's private chats. Shared chat visibility does not
grant permission to clone, execute or push using someone else's credentials.

The service supports private → organization → private changes by the creator,
canonical string revisions, immutable creation/repository snapshots, scoped
idempotent creation, and visible-row cursor pagination. No transcript, generated
Markdown, unpublished file or feedback is automatically published to Git or wiki.

## Review and test evidence

Initial independent review found two Important defects/gaps: database errors were
swallowed as missing rows or partial lists, and the required adversarial test
matrix was incomplete. Fix `36fbd3ae` propagates static `Unavailable` errors while
keeping invalid/foreign ancestry hidden, and adds the missing tests. Scoped
re-review marked both findings addressed with no new Critical/Important issue.

Implementer verification on the corrected code:

- Catalog tests: 15 passed; one guarded PostgreSQL test ignored in the ordinary
  run and explicitly passed separately.
- Adjacent organization/principal/legacy-catalog tests: 82 passed; four guarded
  tests excluded from that ordinary adjacent run.
- Standalone `codeg-server` check: passed.
- Earlier migration tests on unchanged migration code: two ordinary tests and the
  explicitly invoked PostgreSQL upgrade/constraints/down-refusal test passed.
- Successful new PostgreSQL fixtures were removed through the guarded helper;
  older failed fixtures and user data were preserved.

Fix-round RED reproduced the database-error defect with 14 passed, one failed and
one ignored; the final covering GREEN used `CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::catalog --
--test-threads=1`. These are implementer runs checked against the review diff,
not a second independent execution or a hosted acceptance result.

Rustfmt is unavailable in the installed toolchain. Existing compiler/linker
warnings and staged dead-code warnings remain visible; output is not pristine.
The historical initial missing-module RED used `test-utils` before the controller
corrected the baseline; it must not be presented as a no-feature run.

## Protected HTTP acceptance

`28cfceb2` adds the explicit catalog/chat route manifest, no-store responses,
strict DTO/query validation, exact Origin/CSRF protection, cached read and fresh
write membership checks, and separate auth/catalog token budgets sharing one
concurrency semaphore. Synthetic provider endpoints exist only behind `cfg(test)`.
No legacy or agent runtime route was enabled.

Independent review found one Important canonical-path defect: Axum decoded `%31`
to `1` before the handler validated its spelling. Fix `7de2329d` reads the raw URI
segment in all three affected handlers, after the unchanged security checks.
Actual-router tests reject `%31`, `%30%31`, and `%2B1` with valid authenticated
requests and mutation bodies; canonical controls still reach the service.
Scoped re-review confirmed the finding addressed with no new breakage.

- Before the fix: focused 10/10 and all-team-server 75 passed, six guarded ignored.
- Fix RED: 10 passed, one failed because encoded chat ID returned 404 rather than 400.
- Fix GREEN: 11 passed, zero failed using `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
  cargo test --lib --no-default-features http_catalog -- --test-threads=1`.
- Controller standalone production-configuration checks passed at `28cfceb2` and
  again after the raw-path fix plus Task3's test-only fixture wiring. The latter
  command was `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo check --bin
  codeg-server --no-default-features`, exit0 in1m23s; this is not a browser test.
- Existing import/linker/future-compatibility warnings remain; no suppression.

These are local service/router checks, not hosted GitHub or real-provider tests.

## Browser workspace verification and scoped review

At `8be5175e`, the workspace includes projects, recursive folders/working
directories, verified repository defaults, private/organization chat metadata,
contextual creation/editing/sharing, visible member/org identity and settings
return navigation. Agent execution remains explicitly disabled.

Controller verification at that commit passed all 6,202 frontend tests in 449
files (136 seconds), and the fresh Rust fixture binary passed 76 team-server tests
with six guarded tests ignored (31.65 seconds). The earlier controller frontend
attempt was interrupted by SIGTERM, not an assertion failure. The implementer's
earlier 6,201-test aggregate predates its final added focused test. These distinct
runs must not be conflated with validation of subsequent review fixes.

Independent review identified five Important findings: stale detail/navigation
and deep-link Share handling; permanent revocation tombstones preventing reshare;
stale pagination reintroducing rows after refresh; browser private-state checks
executed with the tree collapsed; and failure to prioritize a parallel 401 over
other request errors. Root verified them against source and requested one scoped
fix round with regressions. No browser-task acceptance or publication is claimed
until that fix passes re-review. The previously recorded synthetic browser PASS
does not establish the two private-state legs affected by the fourth finding.

Fix `01116f90` addresses all five findings. Seven regressions first failed against
the original code (11 passed), then the focused tests passed 18/18 and the broader
Task3 slice passed 80/80. TypeScript, scoped lint/formatting and the 34-page static
export passed. No Rust source changed or fixture rebuild was required. Scoped
re-review marked all five findings addressed with no new breakage.

The corrected exact-driver browser run expands all ancestors before both private
absence checks, and waits for the selected chat heading and Organization label
before asserting the colleague lacks Share. It passed with zero console errors.
Root independently inspected its JSON plus shared/private mobile and
private-restored/settings-return desktop screenshots. These artifacts are under
the wrapper workspace's `.superpowers/sdd/2026-09-10-team-catalog/artifacts/task-3-team-catalog-fix1-final-2`,
not the source tree's similarly named directory. The earlier fix1 capture was
premature and is retained as intermediate evidence, not the final proof.

## Decisions and costs

- Quarantine unproven legacy resources rather than automatic admission. Cost:
  existing data requires explicit future admission/repair; no data is deleted.
- Bound organization nodes/repositories and per-creator chats at 1000, rejecting
  additional creation. Cost: larger organizations need later quota changes.
- Block sharing for any existing execution row, active writer, or non-draft
  state until reviewed job integration defines safe terminal states. Cost:
  conservative sharing restrictions for executed chats.
- Page chat lists at 200 visible admitted rows, using the last visible ID. Cost:
  explicit UI pagination is needed; hidden rows do not supply counts/cursors.
- Use `cfg(test)` fixtures without the additional `test-utils` feature. Cost:
  revisit only if a particular future fixture requires that feature.
- Permit test-only repository-proof wiring for actual HTTP/browser journeys.
  Cost: a small maintained fixture surface; production origin remains fixed.
- Preserve and accept the historical initial RED discrepancy as missing-module
  evidence. Cost: a less precise historical baseline, not fabricated chronology.
- Task2 amendment: give catalog polling its own burst200/refill20-per-second
  budget, preserving authentication's burst100/refill1-per-second budget and
  sharing one 32-permit concurrency semaphore. Cost: provisional quotas need
  adjustment based on load tests; no public capacity guarantee is implied.

## Remaining release boundary

The owner subsequently selected open GitHub signup without an organization
allowlist. This supersedes earlier invite-only enrollment plans. Verified GitHub
organization membership, private-chat authorization and tenant isolation remain
mandatory; public signup does not grant access to arbitrary organizations.

The Notion-style catalog workspace task and corrected synthetic collaboration
journey are accepted; combined review remains pending. Public domains remain unrouted, with the private
deployment pinned to `42536219` and runtime disabled. Personal provider supervision,
encrypted credential custody, isolated durable jobs, files/events, notifications,
private founder feedback and real hosted two-user/provider acceptance are separate
remaining gates. No GitHub content or legacy data was deleted by this work.

Context: [org-memory: wiki/engineering/workos.md]. Its legacy operator-mode notes
are historical context, not the new team authorization contract.
