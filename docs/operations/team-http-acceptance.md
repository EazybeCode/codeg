# Neev team HTTP and browser acceptance

Date: 2026-09-09. Status: **HTTP/onboarding slice accepted for continued development; not accepted for rollout**.

This record covers the isolated authentication server and initial browser shell.
It does not certify catalog/settings editing, chat visibility, collaborative
execution, the website, real provider consent or hosted user admission.

## Candidate and evidence

Plan base: `a2a06bcd`. Current source candidate: `b7a2f180`.

- Team configuration/database accepted at `ae82ba38`.
- Team HTTP routes/standalone dispatch accepted at `4c290570`.
- Browser boundary/onboarding accepted at `23675b01` after the response-body
  cancellation/timeout fix and scoped re-review.
- Task4 actual browser acceptance found missing Next static-export navigation
  payloads. Commit `391c872d` adds only login/onboarding/workspace/settings `.txt`
  mappings; unported static pages and legacy APIs remain denied.
- Independent Task4 review requested stronger remote-activation, JSON-secret,
  prior-cookie-content and deterministic stale-response checks. Driver-only fix
  `6f8a453c` and scoped re-review closed all four findings. The controller's fresh
  browser run passed again. All four task reviews are accepted.
- Final whole-plan review found no Critical/Important issue and two Minor issues:
  missing advertised icon routes and a test comparing initial unknown-mode
  publication instead of operator-mode publication. One fix wave `b7a2f180`
  resolved both; the scoped re-review found both addressed and no new breakage.

Fresh final-fix controller verification at `b7a2f180`: all 37 team-server tests
passed, including guarded PostgreSQL tests and both static-file regressions;
the six affected frontend tests, scoped ESLint and fix-range diff check passed.
Actual-browser acceptance exited 0 using the freshly linked library fixture.
Artifact `team-auth-e2e-BnjlvV` retains the full JSON and four screenshots; root
read the JSON and inspected desktop/mobile screenshots. The 11 JSON projections,
one descriptor positive probe, zero unexpected lookup/canary traffic and exact
stale-request cancellation passed again. Tests use invented loopback providers.
The standalone executable/export checks below precede this two-icon route fix;
they are not claimed as rebuilt final-candidate artifacts. No frontend production
source changed in the final wave. The final review accepts inherited warnings as
deferred maintenance; the positional migration fixture is due with the next
migration. This slice adds no migration.

Fresh controller verification at `391c872d`:

| Check | Result |
| --- | --- |
| team_server, including guarded PostgreSQL and static-route regression | 36 passed |
| team_access, including guarded PostgreSQL | 138 passed |
| team_credentials migration controls | 11 passed |
| Legacy OAuth-state controls | 4 passed |
| Scoped frontend tests | 184 passed across 22 files |
| Scoped ESLint | Exit 0 |
| Static production export, including TypeScript | Exit 0, 34 pages |
| Standalone no-default-features binary build | Exit 0, 6m 54s |
| Fresh executable version/configuration rejection controls | 8 passed |
| Actual-browser journeys at final driver commit `6f8a453c` | Exit 0; scoped fix review clean |

Rust commands used `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`, `cargo test --lib
--no-default-features FILTER`, and `--test-threads=1`; the first three filters
included ignored PostgreSQL cases. The server filter explicitly skipped the
opt-in `team_browser_fixture_server`, which runs only through its driver.

Browser command: Node 22 `scripts/tests/team-auth-e2e.cjs` with the existing cached
Playwright module and the freshly emitted library test binary. Controller
artifact `team-auth-e2e-YU3phK` contains full diagnostic JSON and four screenshots.
Desktop 1440px/mobile 375px screenshots were inspected for correct identity/branding
and overflow. These screenshots show an intentionally unavailable migration
workspace, not a completed product. All provider evidence is invented loopback
traffic; real GitHub App consent remains separate.

The final driver validates 11 explicitly awaited successful onboarding/session
projections against current and previously observed cookies and invented secret
markers; only the cookie-bound CSRF field is permitted. It records one legacy
descriptor positive probe, zero unexpected remote lookups/canary traffic, and the
exact stale request's cancellation. Duplicate background responses are not drained
or counted as validated, as clarified by the original reviewer. Earlier attempts
at a generic body/lifecycle drain hung or timed out and were removed; those were
test-harness failures, not additional production defects. Browser/fixture processes
and exact synthetic runtime data were released after the final run.

The production binary was tested with an otherwise empty environment: both
version aliases exit 0; missing team configuration via flag/environment, team
supervisor/helper flags, conflicting mode and invalid mode exit 2. The first
diagnostic script expected the wrong log prefix for mode rejection; corrected
per-case assertions passed. This was a verification-script error, not a product
defect or a production code change.

Existing unused-import/dead-code, compact-unwind/future-compatibility and Vite CJS
warnings were triaged as nonblocking maintenance by final review. No warning suppression, dependency
installation, source-wide cleanup or change to the native/operator mode was
performed for these checks.

## Decisions made while executing the plan

These are recorded for owner visibility; none authorizes publication or broader
tenant access.

1. Add a pure OS-string mode selector so undecodable configuration fails closed.
   Cost if wrong: small public adapter and wiring/test rework.
2. Preserve the existing `-V` version alias. Cost: small CLI/test/documentation
   compatibility rework.
3. Project the pending record's CSRF hash internally for binding validation.
   Cost: internal interface/test rework; the hash is not a public DTO field.
4. Add a fixed root 303 redirect to `/workspace`. Cost: one public route/test;
   it grants no session or resource authority.
5. Add internal browser-bound OAuth-attempt cancellation for provider denial.
   Cost: one narrow service interface and tests; no credential exchange on denial.
6. Guard the exported client root before legacy hooks, even though the server
   normally redirects it. Cost: one entry wrapper and regression test.
7. Record honest browser characterization instead of manufacturing a new product
   failure after earlier feature tests. Cost: initially less before-fix browser
   evidence; an actual missing-payload defect was subsequently reproduced.
8. Add exactly four exported navigation payload routes for that browser defect.
   Cost: four public build-artifact paths and one focused test module; no catch-all.
9. Resume the saved build at 13 GiB after owner cleanup with a 2 GiB stop threshold,
   rather than require the conservative 15 GiB estimate literally. Cost: possible
   canceled build/time; subsequent samples reached 15 GiB and both builds completed.
10. Prior-cookie tests exclude old workspace/session content, but permit the prior
    organization as a legitimate eligible picker choice. Cost: a narrower DOM
    exclusion to revise; no production authority or picker behavior change.

## Release boundary

Keep the hosted pilot private and runtime disabled. The existing gVisor installation
is not yet an application-job acceptance result. No push, deployment, GitHub data
deletion, live credential change or real provider request was part of this work.
The source origin points at the older codeg repository; only the prepared Workos
publication clone may be used with the owner's interactive ship gate.

Website remains neev.so; application remains app.neev.so. Verified DNS is not TLS,
callback or hosted flow acceptance. Next current-org settings work is prepared in
the separate settings plan; its implementation has not started. Branch integration
and publication are not performed here. Preserve this plan's required ignored
evidence until the release record is safely retained.

Context: [org-memory: wiki/engineering/workos.md]. The older operator-mode wiki is
not a source of team authorization; current code and the approved team-access
specification govern this migration.
