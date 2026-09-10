# Team organization settings — acceptance record

September 10, 2026. All three settings tasks and the final combined review/fix
wave are accepted through `63a51bbf` for continued development, not rollout.
No deployment or runtime enablement follows from this record. The full shared
workspace, personal-agent and cloud execution release gates remain unfinished.

Earlier frontend checkpoint: candidate `d28e5467` passed 122 fresh named
regressions, 11 legacy return-navigation tests, scoped lint and a 34-page build
including TypeScript. Independent review nevertheless found two Important defects:
retained account A state could enter the first committed account B render before
passive-effect cleanup; automatic post-save revalidation cleared the success
announcement. Fix round 1 at
`e204e648` closed both, with focused RED/GREEN regressions and scoped re-review
confirming both addressed with no new breakage. Controller verification passed
135 tests in 14 files, amended lint/diff checks and a 34-page export including
TypeScript at that commit. Task 2 is now accepted; browser Task 3 is active.
Passing tests/export did not
substitute for the review gate.

Plan: [organization settings](../superpowers/plans/2026-09-09-team-organization-settings.md).
Binding design: [team access](../superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md).

## Backend candidate

`e112272c..5069eede` adds the explicit current-org GET/PATCH route, bounded pure
branding validation, transactional identity/org/principal checks, and static
DTO/error projection. Admin PATCH requires Fresh proof, exact Origin and session
CSRF. Members can read but cannot mutate; legacy operator authority is not reused.
The full Alice/admin A+B, Bob/member A, Carol/admin B topology goes through real
loopback OAuth/onboarding and the actual router with invented identities.

Independent Task 1 review returned spec compliant and quality Approved, no Critical
or Important findings. Named dependency checks covered SQLite serialization,
membership/session final guards and the shared budget/timeout/no-store wrapper.

Fresh controller verification on the emitted library executable from `5069eede`:

```text
codeg_lib-732148cb61475fb4 team_access team_server personal_agent_connections_tests team_credentials_tests organization_settings organization_branding --include-ignored --skip team_browser_fixture_server --test-threads=1 --quiet
262 passed; 0 failed; 0 ignored; 3472 filtered out; 20.76s; exit 0
```

This is one deduplicated run covering the selected Rust/HTTP/SQLite/disposable
PostgreSQL checks, not an additive sum of overlapping filters or a full-repository
suite. Full output was read. Implementer also reports standalone no-default-
features codeg-server cargo check exit 0 and exact-source focused regressions.
Root working/index diff checks were clean. No UI/export/browser assertion comes
from these backend tests.

Real behavioral RED found tagged `keep` accepting string-valued extra data;
strict private wire conversion fixed it. Other failed runs were test expectations
and controlled provider-proof queue mistakes, not additional production fixes.
The trivial enum-inequality test was removed; real role/legacy-authority assertions
and combined-persona tests remain.

## Historical observations and final disposition

1. `organization_settings.rs:49`: `Option<String>` still treats explicit `data:null`
   as absent for keep/remove, despite the implementation report claiming any
   data field is rejected. Reviewer classified this as Minor, no authority bypass.
   Final fix `63a51bbf` now rejects explicit null with a present-string
   deserializer and RED/GREEN regression, preserving absent-data keep/remove.
2. Builds retain acknowledged baseline migration-import, linker, dependency and
   disabled-runtime dead-code warnings. No new Task 1 warning remains. This record
   does not describe the build output as pristine.

Reviewer “cannot verify” items were resolved to their actual scope: the fresh run
confirms selected command outcomes; source/config checks retain runtime denial,
and implementation tools made no external changes. This is not a refreshed live-
host observation. Ten-locale/static-export requirements were separately verified
in Task 2; actual browser acceptance remains Task 3. Neither was inferred from
backend test results.

## Execution ruling

Ruling: initially leave a newly noticed unused test import for review rather than
perform a full relink solely for cosmetic cleanup. Reason: preserve the exact
candidate's evidence and bundle eventual fixes. Cost if wrong: a warning and later
cleanup. Subsequently the explicitly required combined multi-org HTTP test needed
a new relink, so import removal was bundled and the warning no longer remains.
This ruling never allowed omission of required tests; the missing combined
topology was completed before the Task 1 commit and review.

## Release boundary

- Continue workspace/chat privacy conversion. This settings slice does not certify folders/chat access,
  personal provider login, isolated cloud execution or hosted readiness.

Task 2's scoped controller/form, member read-only controls, ten locales, transport
and lifetime tests, TypeScript/lint/export and independent review are accepted
through `e204e648`. The Vite CJS deprecation notice remains a deferred tooling
observation, not a new runtime failure or a claim of pristine test output.

## Browser evidence and review history

At `0c60f740`, the controller independently ran the frozen browser driver with
the compiled test fixture and unchanged Task 2 static export. The command exited
0 with all three PASS lines; the fixture and browser were released and the exact
owned synthetic runtime removed. Evidence is retained at the wrapper workspace's
`.context/neev-settings-root-20260910/evidence.json`.

The run exercises five browser contexts, including two Alice A-to-B journeys,
an independent Alice B session, Bob A and Carol B. It checks 19 authentication
DTO projections, 11 settings DTO projections, expected authorization/image/outage
denials and two held-response cancellations. HTTP and WebSocket canaries are
both zero; unexpected remote descriptor lookups are zero. The controller read
the evidence and inspected fresh saved-admin, read-only-member, B-relogin and
375px mobile screenshots. Nine screenshots are retained.

Seven unsuccessful worker attempts remain preserved. Their failures were test
proof scheduling and final cancellation diagnostics, not invented production
RED results. A narrowly classified login-script cancellation is tied to the
current generated chunk hash and explicit OAuth navigation phase; this coupling
is an open review concern. It is not a blanket allowance for network failures.

Fresh controller Rust verification on this fixture's emitted binary passed the
same 262 selected checks in 21.29s, exit 0; syntax, scoped lint and diff checks
also passed. These fake-loopback results do not establish real GitHub consent,
working team chats, personal provider login or cloud execution.

Task 3 review found three test gaps at `0c60f740`: member-logo checks occurred
after removal; oversized-file rejection could reuse an existing SVG error;
legacy GET/PATCH diagnostics shared a phase without exact cardinalities.
Fix `416b94a0` adds an independent member session while the uploaded logo exists,
an explicit error-clear transition and separate counted probes. Scoped review
confirmed all three addressed without new breakage. The controller's fresh run
at `.context/neev-settings-root-fix1-20260910` exited 0, checked 22 authentication
and 11 settings projections across six contexts, and retained ten screenshots.
The new inspected screenshot shows Bob's identity, updated name and uploaded logo
together. Each legacy probe has exactly one expected HTTP and console error.
Syntax/lint/diff checks passed. The Rust binary and frontend export are unchanged
by this test-only fix.

## Final acceptance at 63a51bbf

Combined review of `e112272c..416b94a0` approved the settings slice with no
Critical/Important findings. The initial reviewer hit a usage limit before a
verdict; an available fallback independently read the complete diff and evidence.
The single final fix wave rejected explicit null logo data and added the
[browser-maintenance runbook](team-settings-browser-maintenance.md). Scoped
re-review confirmed the fix and documentation, with no new breakage.

Actual RED ran one fully qualified test and failed because null was accepted.
An earlier unqualified `--exact` launcher selected zero tests and was explicitly
not counted as RED. GREEN passed 17 focused settings tests (one guarded PG test
excluded by that worker filter) and all six HTTP tests.

The controller then verified the final binary independently: 262 selected tests
passed, zero failed/ignored, 3472 filtered, 26.39s, exit 0, including disposable
PostgreSQL. The final browser run at wrapper
`.context/neev-settings-root-final-20260910` passed and exited 0, retaining the
six contexts, 22 auth/11 settings projections, ten screenshots, zero canaries
and exact owned-process/runtime cleanup. Fresh mobile settings and member-logo
screenshots were inspected. Binary SHA256:
`da3d53270974296d8ee74a18f71a3bd21e8ba8aa058929c70e95f373adceb6b8`.
The driver remains `33f90c5eaad28d519575edca8fec9d025ee0b3dbfd0afc193695f6cd5847b2c1`;
the frontend is the unchanged previously verified 34-page export.

## Retained final-review rulings

1. Retain the exact generated login-chunk cancellation predicate with its
   reviewed runbook; current evidence matches and wildcard exceptions would
   weaken diagnostics. Cost if wrong: future exports can fail acceptance until
   their exact asset/navigation evidence is reviewed again.
2. Defer baseline Rust/linker/dependency/Vite warnings and rustfmt installation/
   cleanup; none originated in this slice, and broad toolchain changes are not
   part of this fix. Cost if wrong: noisy diagnostics, future compatibility
   maintenance and manual Rust formatting checks remain.

These are documented residual maintenance costs, not claims that the underlying
coupling or warnings were fixed. All work/evidence remains preserved; no merge,
push, deployment, real provider connection or public admission occurred.

Context: [org-memory: wiki/engineering/workos.md]. The old single-operator model
does not authorize exposing legacy settings or catalog routes to team sessions.
