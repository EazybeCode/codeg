# Personal-agent lifecycle verification

September 10, 2026. **Both tasks accepted; final combined review pending.**
This is not a provider-login, runtime, deployment or release acceptance record.

Plan: [personal-agent lifecycle](../superpowers/plans/2026-09-09-personal-agent-lifecycle.md).
Prerequisite: [accepted metadata storage](personal-agent-storage-acceptance.md).
The owner-approved [connection design](../superpowers/specs/2026-09-09-personal-agent-connections-design.md)
remains the binding product and isolation contract.

## Accepted personal intents

Task 1 source: `7d4a4fb9`, based on `448f0361`. It adds only the internal
`team_access::agents` service and its tests; no personal-provider route is mounted.

- Member/organization-scoped projections and exact-session attempt access.
- Ordered Claude/Codex metadata without creating rows for disconnected providers.
- Atomic generation-bound login intent, user-wide quota and scoped idempotency.
- Cancel/disconnect invalidation with durable cleanup intent and immediate hiding
  of account metadata; actual cleanup is not performed by these methods.
- Final principal rechecks and fixed errors; no serialized internal tickets or
  Debug output for account-bearing service types.

Independent task review found spec compliance and approved quality, with no
critical or important finding. Its named integration checks confirmed the reused
membership/session lock order and compatibility with storage constraints.
PostgreSQL runtime races are required in Task 2; they were not run for Task 1.

Behavioral RED used the temporary unavailable implementation and failed at the
new begin call, with zero passes and one failure. After implementation, the first
covering run passed 12 tests. Self-review added expired-pending replay and stronger
no-new-row checks. The final worker run passed 13 tests in 0.84s:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::tests -- --test-threads=1
```

Fresh controller verification from `src-tauri`, using the library-test executable
just rebuilt from that same source, exited 0:

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 team_access::agents::tests --test-threads=1
13 passed; 0 failed; 0 ignored; 3681 filtered out; 1.28s
```

Working/index whitespace checks passed. Validation retains the existing unused
migration `DbBackend` import, oversized linker `__eh_frame` and dependency
future-compatibility warnings. These are deferred maintenance, not new Task 1
warnings or proof that every project target passes.

## Existing browser shell characterization

The existing Node 22 browser driver was rerun with cached Playwright 1.60.0, the
same Task 1 library-test executable and the unchanged static export. It exited 0.
The fresh wrapper artifact directory is `.context/neev-auth-smoke.CkR58u`.

All 11 awaited JSON projections, expected authorization denials, zero canary and
unexpected remote-lookup traffic, and exact stale-request cancellation passed.
The controller read the full JSON and inspected desktop, mobile and settings-return
screenshots. Browser and fixture processes exited; only their owned synthetic
runtime was removed. JSON and screenshots were retained.

These tests use invented loopback GitHub evidence. The screenshots show the
intentionally unavailable migration workspace, with person/organization identity
and return navigation. They do not show working projects, chats or provider login.

## Task 2 and downstream boundaries

Task 2 implementation is committed at `115b3c6d`, with its verification report at
`ef0b9a4d`, based on `e58b449b`. It adds final verified-result checks, cleanup
claims/completion, expiry/startup invalidation and the existing logout hook.
Its behavioral RED
expected Conflict for a canceled login's late completion but got the temporary
Unavailable result: zero passes, one failure. An intermediate full run passed 29
and failed two tests whose fixtures did not reach the intended assertion; the
fixtures were corrected. Final self-review also added stale-pending recovery
without overwriting a newer connected row. Both required Cargo library suites
then passed on the amended source: 31 agent tests (including six PostgreSQL races)
and nine HTTP/logout tests (including two PostgreSQL orderings).

Fresh controller verification on that exact library-test executable exited 0:

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 team_access::agents --include-ignored --test-threads=1
31 passed; 0 failed; 0 ignored; 3681 filtered out; 5.37s
./target/debug/deps/codeg_lib-732148cb61475fb4 team_server::http_tests --include-ignored --test-threads=1
9 passed; 0 failed; 0 ignored; 3703 filtered out; 1.71s
```

The full output was read; working/index whitespace checks passed. Independent
Task 2 review requested three fixes: uniform not-found denial for another
session's attempt during completion/failure; deterministic overlap and reverse
positive controls in the required PostgreSQL races; and preservation of failed
fixtures before propagating their captured errors. Fix round 1 was started;
At that checkpoint Task 2 acceptance and whole-plan review remained pending. These 40 distinct checks
are not the full repository suite. The worker's exploratory `cargo check --tests`
also encountered `codex_corpus_differential` calling missing
`CodexParser::with_base_dir`; that integration-target failure was not fixed here.

Broader fresh controller regressions at `ef0b9a4d`, using the same executable and
`--include-ignored --test-threads=1`, also exited 0: `team_access` 169 tests in
20.86s; `team_server --skip team_browser_fixture_server` 37 tests in 3.63s; combined
`personal_agent_connections_tests team_credentials_tests` 27 tests in 6.69s.
These total 233 distinct backend checks and include the 40 focused checks above;
do not add them twice. No full-project, standalone rebuild or new actual-browser
acceptance is implied by these backend results.

### Reviewed fix round and latest verification

Source fix `b9aa61d0` and report `38811933` address all three findings. A focused
RED reproduced Conflict instead of NotFound for another-session completion. The
fix scopes attempt selection before state checks without changing Task 1 replay.
Start/replay/quota/cleanup races now require two observed PostgreSQL user-lock
waiters; two additional tests verify completion-first orderings. Failed captured
results retain and identify their test database before returning the error.

An intermediate test run failed three setup queries because they used `user`
instead of the actual `users` table. The corrected suite passed. Those three
failed fixtures remain preserved (exact generated names in the committed Task 2
report); neither successful results nor this record silently delete them.

Final worker gates on the amended source passed 21 recovery, 34 total agent and
nine HTTP/logout tests. Independent scoped re-review marked every finding
addressed, with no new critical/important issue or out-of-scope observation.
Task 2 is accepted; final combined lifecycle review remains pending.

Fresh controller verification at `38811933` used the rebuilt executable:

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 team_access team_server personal_agent_connections_tests team_credentials_tests --include-ignored --skip team_browser_fixture_server --test-threads=1 --quiet
236 passed; 0 failed; 0 ignored; 3479 filtered out; 39.99s; exit 0
```

Working/index whitespace checks passed. The 236 checks supersede the earlier
233-check run; they are not additive. They do not include the opt-in browser
fixture, all repository targets, a standalone rebuild or real providers.

The service accepts trusted internal provider evidence only. Official-client
authentication, encrypted credential storage, enforced sandbox access controls,
provider connection routes/UI and per-author cloud jobs remain separate gates.
The owner's Claude terms confirmation is recorded in
[provider contracts](personal-agent-provider-contracts.md); no account was
connected by that confirmation. See the [sandbox investigation](personal-agent-sandbox-investigation.md)
for unresolved network and backup-compatibility checks.

Unauthenticated HTTPS checks from the owner's Mac could not connect to either
public origin on port 443, although both domains resolved to the expected server
address. The server-side cause was not established and no hosting configuration
was changed. A follow-up pinned-key SSH check found no host listener on 80/443,
the application and PostgreSQL healthy with only internal container ports, and
the public proxy stopped. This explains the absent public endpoint, not the
reason for the earlier stop. No service was started or configuration changed.
No source publication, deployment or actual GitHub deletion occurred
for this lifecycle work.

## Rulings carried forward

1. Begin returns either a pending ticket or the settled own-account projection.
   A completed same-key retry must not require the cleared login lease or launch
   another login. Cost if wrong: internal service/HTTP interface rework; no runtime
   was enabled by this decision.
2. Task 2 may make the three AgentService fields visible within the agents module
   and descendants. The struct remains in `intents.rs`; sibling completion/recovery
   implementations need its database, principal service and clock. Cost if wrong:
   wider internal field access and local interface rework, not a public/serialized
   API or change to Task 1 method behavior.
3. Failed old cleanup uses the same connection-generation/state fence as successful
   old cleanup. Its operation may retry without overwriting a newer connection's
   state or metadata. Cost if wrong: the old failure is visible in its operation
   metadata rather than as the newer connection's error.

Context: [org-memory: wiki/engineering/workos.md]. Legacy operator-mode authority
and provider homes are not the new multi-tenant authorization boundary.
