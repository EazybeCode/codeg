# Task 2 report: verified completion, durable cleanup and invalidation recovery

Date completed: 2026-09-10 IST

Implementation commit: `115b3c6d` (`feat(team-access): recover personal agent lifecycle`)

## Scope delivered

- Added internal-only verified completion and provider-result failure handling. `VerifiedSubscription` and `CleanupTicket` are non-serialized and deliberately do not derive `Debug`.
- Completion performs the final principal recheck and atomically binds the exact owner, provider, connection, attempt, session, operation, generation and live lease before storing a validated label and UUID-simple storage handle.
- Failure advances generation, terminalizes the attempt/connect lease, stores only an allowlisted error and queues generation-bound cleanup with a reserved `__neev_` server key.
- Added principal-free durable cleanup claim/finish methods. Claims use fresh UUID owners and 30-second leases; stale successful and failed finishes never overwrite newer connection state.
- Added bounded (100-ID) expiry and startup recovery. Startup terminalizes all pending attempts, including stale pending attempts attached to a newer connected row, without modifying that connected account.
- Added the existing logout hook before exact-session deletion. Logout samples the supplied `TeamClock` after the session lock, invalidates only that session's pending attempts, and aborts deletion on invalidation failure.
- Added SQLite behavior coverage, deterministic PostgreSQL lock/race coverage and real HTTP logout coverage. No provider process, HTTP agent route, runtime enablement, cloud write or credential/path API was added.

## TDD evidence

### Behavioral RED

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests::canceled_login_cannot_complete_with_late_provider_result -- --exact --test-threads=1
```

Relevant output (exit 101):

```text
running 1 test
test team_access::agents::recovery_tests::canceled_login_cannot_complete_with_late_provider_result ... FAILED

thread 'team_access::agents::recovery_tests::canceled_login_cannot_complete_with_late_provider_result' panicked at src/team_access/agents/recovery_tests.rs:42:5:
assertion `left == right` failed
  left: Err(Unavailable)
 right: Err(Conflict)

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3694 filtered out; finished in 0.11s
```

The missing behavior was specific: a canceled generation's late verified result did not yet return the required conflict. The existing cancellation state assertions confirmed that the connection remained unusable.

### Intermediate covering run and corrections

The first compiling full matrix ran 31 tests: 29 passed and 2 failed. All six guarded PostgreSQL tests passed. The two failures were test-fixture issues, not production regressions:

```text
cleanup_ticket_fields_are_immutable:
  left: Err(InvalidInput)
 right: Err(Conflict)

completion_rejects_ticket_and_lease_tampering_and_expired_proof:
  left: Err(Auth(Unauthorized))
 right: Err(Conflict)

test result: FAILED. 29 passed; 2 failed; 0 ignored; 0 measured; 3681 filtered out
```

Corrections:

- Kept the mutated cleanup target/result generations structurally valid so the test reaches the immutable-ticket conflict check.
- Used a service-only controlled clock for expired login proof while leaving the principal clock live, so the test reaches the intended completion lease/attempt expiry check.

A later 31-pass run was treated as intermediate because final self-review added explicit stale-pending startup terminalization coverage. Both required commands below were then rerun on the final committed source.

## Final verification on committed source

### Agent lifecycle suite, including guarded PostgreSQL

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 31 tests
test team_access::agents::recovery_tests::canceled_login_cannot_complete_with_late_provider_result ... ok
test team_access::agents::recovery_tests::cleanup_claim_failure_retry_and_success_are_lease_bound ... ok
test team_access::agents::recovery_tests::cleanup_expired_lease_is_taken_over_and_operation_expiry_is_not_abandonment ... ok
test team_access::agents::recovery_tests::cleanup_ticket_fields_are_immutable ... ok
test team_access::agents::recovery_tests::completion_rejects_malformed_safe_metadata_without_mutation ... ok
test team_access::agents::recovery_tests::completion_rejects_ticket_and_lease_tampering_and_expired_proof ... ok
test team_access::agents::recovery_tests::completion_requires_the_still_live_initiating_session ... ok
test team_access::agents::recovery_tests::expiry_and_startup_recovery_are_idempotent_and_preserve_connected_accounts ... ok
test team_access::agents::recovery_tests::own_verified_completion_is_atomic_private_and_single_use ... ok
test team_access::agents::recovery_tests::postgres_cancel_and_completion_orderings_never_reactivate_canceled_state ... ok
test team_access::agents::recovery_tests::postgres_completion_waiting_behind_credential_generation_change_rejects ... ok
test team_access::agents::recovery_tests::postgres_completion_waiting_behind_session_deletion_rejects ... ok
test team_access::agents::recovery_tests::postgres_member_quota_serializes_across_providers ... ok
test team_access::agents::recovery_tests::postgres_parallel_cleanup_claims_issue_one_live_lease ... ok
test team_access::agents::recovery_tests::postgres_starts_serialize_different_keys_and_replay_the_same_key ... ok
test team_access::agents::recovery_tests::provider_failure_invalidates_generation_and_queues_owned_cleanup ... ok
test team_access::agents::recovery_tests::recovery_generation_overflow_rolls_back_every_mutation ... ok
test team_access::agents::recovery_tests::stale_cleanup_success_and_failure_never_overwrite_newer_connection_state ... ok
test team_access::agents::tests::all_methods_recheck_changed_authority_and_session_or_credential_staleness ... ok
test team_access::agents::tests::begin_replays_pending_and_settled_without_new_rows_or_quota_debit ... ok
test team_access::agents::tests::cancel_is_scoped_idempotent_and_records_invalidation_metadata ... ok
test team_access::agents::tests::disconnect_invalidates_login_preserves_handle_and_replays_receipt ... ok
test team_access::agents::tests::disconnected_without_work_records_an_immediate_disconnect ... ok
test team_access::agents::tests::expired_pending_replay_conflicts_without_replacing_its_identity ... ok
test team_access::agents::tests::list_is_ordered_virtual_scoped_and_bounded ... ok
test team_access::agents::tests::login_attempt_is_visible_only_to_the_initiating_member_org_and_session ... ok
test team_access::agents::tests::malformed_stored_projection_fails_closed ... ok
test team_access::agents::tests::quota_is_user_wide_cross_org_provider_and_excludes_exact_boundary ... ok
test team_access::agents::tests::request_keys_and_generation_overflow_fail_without_mutation ... ok
test team_access::agents::tests::same_person_other_session_cannot_read_or_replay_a_login ... ok
test team_access::agents::tests::simultaneous_sqlite_starts_replay_one_ticket ... ok

test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 3681 filtered out; finished in 5.15s
```

The six `postgres_*` tests used disposable exact-name-checked fixtures and completed their guarded cleanup.

### Team HTTP/logout suite, including guarded PostgreSQL

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::http_tests -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 9 tests
test team_server::http_tests::local_logout_removes_only_the_cookie_selected_session_when_membership_is_missing ... ok
test team_server::http_tests::login_ignores_client_authority_and_callback_requires_unique_browser_binding ... ok
test team_server::http_tests::manifest_isolated_public_and_static_errors_do_not_leak_operator_routes ... ok
test team_server::http_tests::onboarding_bootstrap_selection_session_and_local_logout_are_browser_bound ... ok
test team_server::http_tests::postgres_guard_waiting_behind_logout_rejects_the_deleted_session ... ok
test team_server::http_tests::postgres_logout_waits_behind_an_authorized_mutation_guard ... ok
test team_server::http_tests::provider_denial_is_single_use_browser_bound_and_recovers_with_a_fresh_attempt ... ok
test team_server::http_tests::unsupported_methods_and_unknown_protected_paths_are_not_found ... ok
test team_server::http_tests::writes_require_exact_origin_csrf_json_and_canonical_github_id ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 3703 filtered out; finished in 1.16s
```

### Diff and scope gate

Command:

```text
git diff --check
```

Output: empty, exit 0.

The implementation commit staged exactly these nine authorized files:

```text
src-tauri/src/team_access/agents/completion.rs
src-tauri/src/team_access/agents/intents.rs
src-tauri/src/team_access/agents/mod.rs
src-tauri/src/team_access/agents/recovery.rs
src-tauri/src/team_access/agents/recovery_tests.rs
src-tauri/src/team_access/agents/store.rs
src-tauri/src/team_access/agents/types.rs
src-tauri/src/team_server/handlers/session.rs
src-tauri/src/team_server/http_tests.rs
```

Root-owned modified/untracked documentation was excluded from the implementation commit. `Cargo.toml` and `Cargo.lock` were unchanged. Module index and Cargo ownership are released after this report commit.

## Requirements and self-review

- Verified completion: exact owner/provider/generation/attempt/session/operation/lease checks; nonexpired proof; safe metadata; atomic completed state; replay conflicts.
- Failure: fixed allowlist, checked generation advance, failed attempt, abandoned connect lease, disconnecting connection, reserved-key cleanup.
- Cleanup: no fabricated `AuthContext`; user then org then connection then operation lock order; queued/expired-lease claims only; no operation-expiry abandonment; fresh 30-second lease; immutable finish; retry; generation/state fence on both outcomes.
- Recovery: controlled-clock expiry, explicit startup recovery, bounded 100-ID pages, idempotency, checked overflow rollback, stale-pending terminalization without newer-row overwrite.
- Logout: membership/session precede sorted connection cleanup locks; invalidator runs before deletion; clock sampled after lock; exact session only; orphaned session still removable; no principal or provider call.
- Boundaries: metadata-only internal APIs; no route, runtime, container, provider, cloud, GitHub or filesystem cleanup implementation.

Self-review changes made before final GREEN:

1. Corrected failure cleanup to use the schema-valid `disconnect` operation target shape (`target_attempt_id = NULL`).
2. Corrected newer-state cleanup fixtures to a valid `reconnect_required` row while asserting preservation of label, handle, status and error.
3. Kept tampered cleanup generations structurally valid so the conflict branch is tested rather than input-shape rejection.
4. Separated the completion expiry clock from the principal clock to test the intended expired provider-proof branch.
5. Refetched a connection between multiple logout attempts for one connection, avoiding reuse of a stale in-transaction model.
6. Terminalized stale startup/expiry attempts as `expired` while leaving a newer connected account untouched; added an explicit assertion.

## Warnings and concerns

Baseline maintenance warnings reproduced unchanged:

```text
unused import: `DbBackend`
  src/db/migration/m20260817_000001_work_task_conversation_title.rs

linker stderr: ld: __eh_frame section too large (max 16MB) to encode dwarf unwind offsets in compact unwind table

the following packages contain code that will be rejected by a future version of Rust:
proc-macro-error2 v2.0.1
```

An exploratory `cargo check --tests --no-default-features` also reached an unrelated existing integration-target feature mismatch:

```text
tests/codex_corpus_differential.rs:83:35
no associated function or constant named `with_base_dir` found for struct `CodexParser`
```

That command is not a Task 2 acceptance gate; both required `--lib` suites are green.

Concern for downstream work: these APIs only record verified metadata and cleanup custody. A later reviewed supervisor must perform and prove actual provider/file cleanup before calling `finish_cleanup(..., true)`. Startup recovery must be wired as an explicit single-active-server pre-admission step; it is intentionally not invoked by `AgentService::new` in this task.

[org-memory: wiki/engineering/workos.md]

---

# Task 2 final whole-plan review wave

Date: 2026-09-10 IST

Fix base: `3214d4bc`

Source and release-evidence commit: `376af7df`

## Findings addressed

1. `complete` and `fail` now sample the service clock only after `completion_rows` has acquired the connection, attempt, and operation locks. Deadline validation therefore uses post-lock time for both the attempt and connect lease.
2. The member completion connection query includes exact `org_id` and `user_id` predicates before PostgreSQL applies `FOR UPDATE`. The existing unscoped helper remains available only to already-authorized cleanup/recovery paths; shared cleanup and replay semantics are unchanged.
3. The release summary now says explicitly that the synthetic browser characterization ran against Task 1 source `7d4a4fb9`; it no longer implies that browser evidence covered amended Task 2/logout source.

No provider call, route, runtime enablement, filesystem cleanup, parser change, cloud write, or unrelated warning cleanup was added.

## Behavioral RED: post-lock deadline

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests::postgres_completion_samples_time_after_connection_locks -- --exact --include-ignored --test-threads=1
```

Relevant output (exit 101):

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 8m 15s
running 1 test
test team_access::agents::recovery_tests::postgres_completion_samples_time_after_connection_locks ... FAILED

called `Result::unwrap()` on an `Err` value: "Complete accepted a ticket that expired behind its connection lock: Ok(()); preserved PostgreSQL database workos_team_auth_test_1dc82adf8a66457f8a036d81fcd4a3e8"

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3716 filtered out; finished in 0.75s
```

The regression held the real `team_agent_connection` row, observed the completion query waiting, advanced only the service clock to the exact attempt/lease deadline, and then released the lock. The pre-fix method incorrectly returned `Ok(())` and mutated the rows. The failing fixture was preserved and was not deleted:

```text
workos_team_auth_test_1dc82adf8a66457f8a036d81fcd4a3e8
```

The final test exercises the same deadline boundary and unchanged-row assertions for both `complete` and `fail`, followed by a separate still-live control for each method that waits behind the same lock and advances the service clock only 9 minutes 59 seconds.

## Behavioral RED: owner-scoped lock

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests::postgres_completion_scopes_foreign_connection_before_locking -- --exact --include-ignored --test-threads=1
```

Relevant output (exit 101):

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 0.96s
running 1 test
test team_access::agents::recovery_tests::postgres_completion_scopes_foreign_connection_before_locking ... FAILED

called `Result::unwrap()` on an `Err` value: "foreign completion waited on another owner's connection row; eventual result Err(NotFound); preserved PostgreSQL database workos_team_auth_test_5b643603a79c4350b7125d39168e3699"

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3716 filtered out; finished in 1.75s
```

Alice's completion attempt used Bob's otherwise valid ticket while Bob's connection row was held. Before the fix it waited on Bob's row and returned the opaque `NotFound` only after release. The failing fixture was preserved and was not deleted:

```text
workos_team_auth_test_5b643603a79c4350b7125d39168e3699
```

## Focused GREEN

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features postgres_completion_s -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 10m 56s
running 2 tests
test team_access::agents::recovery_tests::postgres_completion_samples_time_after_connection_locks ... ok
test team_access::agents::recovery_tests::postgres_completion_scopes_foreign_connection_before_locking ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 3715 filtered out; finished in 1.74s
```

The relink was required by the production and test source changes. No source changed after this focused GREEN, so the covering and exact gates below used the same final source and binary.

## Covering recovery matrix

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 23 tests
test team_access::agents::recovery_tests::postgres_completion_samples_time_after_connection_locks ... ok
test team_access::agents::recovery_tests::postgres_completion_scopes_foreign_connection_before_locking ... ok
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 3694 filtered out; finished in 8.44s
```

All raw SQL and observer fragments were checked against the real entity/schema names before the expensive builds: `users`, `organization_membership`, `team_agent_connection`, and `team_session`. Successful fixtures used exact-name cleanup; propagated SQL, lock-observation, timeout, and assertion failures retained the existing named-preservation path.

## Final exact agents gate

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 36 tests
test team_access::agents::recovery_tests::postgres_completion_samples_time_after_connection_locks ... ok
test team_access::agents::recovery_tests::postgres_completion_scopes_foreign_connection_before_locking ... ok
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 3681 filtered out; finished in 12.60s
```

## Final exact HTTP/logout gate

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::http_tests -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 9 tests
test team_server::http_tests::local_logout_removes_only_the_cookie_selected_session_when_membership_is_missing ... ok
test team_server::http_tests::login_ignores_client_authority_and_callback_requires_unique_browser_binding ... ok
test team_server::http_tests::manifest_isolated_public_and_static_errors_do_not_leak_operator_routes ... ok
test team_server::http_tests::onboarding_bootstrap_selection_session_and_local_logout_are_browser_bound ... ok
test team_server::http_tests::postgres_guard_waiting_behind_logout_rejects_the_deleted_session ... ok
test team_server::http_tests::postgres_logout_waits_behind_an_authorized_mutation_guard ... ok
test team_server::http_tests::provider_denial_is_single_use_browser_bound_and_recovers_with_a_fresh_attempt ... ok
test team_server::http_tests::unsupported_methods_and_unknown_protected_paths_are_not_found ... ok
test team_server::http_tests::writes_require_exact_origin_csrf_json_and_canonical_github_id ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 3708 filtered out; finished in 1.68s
```

## Diff, scope, and self-review

Command:

```text
git diff --check
```

Output: empty, exit 0.

The source/evidence commit staged exactly:

```text
docs/operations/neev-release-plan.md
src-tauri/src/team_access/agents/completion.rs
src-tauri/src/team_access/agents/recovery_tests.rs
src-tauri/src/team_access/agents/store.rs
```

Self-review confirmed:

- Both terminal methods sample once, after every completion resource lock and immediately before the shared pending/deadline validation.
- The clock regression uses an independent service clock, leaving the principal/session clock fixed and authorized throughout.
- Exact-deadline calls return `Conflict` without changing connection, attempt, or connect-operation state; 9:59 calls succeed for both terminal methods and reach their expected final states.
- Removing either post-lock clock move causes the corresponding complete/fail iteration to fail; moving the sample before `completion_rows` reproduces the RED.
- The member connection query includes both owner fields in SQL before `FOR UPDATE`. Removing either predicate permits inappropriate foreign-row contention and defeats the owner regression.
- Other-session attempts remain opaque through the earlier exact-session attempt query. The shared Task 1 attempt/replay helper and unscoped authorized cleanup/recovery helper are unchanged.
- The only release-plan edit is the browser-source sentence, now explicitly naming `7d4a4fb9`.
- `Cargo.toml`, `Cargo.lock`, module indexes, other documentation, runtime/provider boundaries, and cloud state are unchanged.

## Warnings and concerns

Baseline maintenance warnings reproduced unchanged:

```text
unused import: `DbBackend`
  src/db/migration/m20260817_000001_work_task_conversation_title.rs

linker stderr: ld: __eh_frame section too large (max 16MB) to encode dwarf unwind offsets in compact unwind table

the following packages contain code that will be rejected by a future version of Rust:
proc-macro-error2 v2.0.1
```

No new warning was introduced. The two named RED databases above intentionally remain available for investigation, in addition to the three already recorded in fix round 1. The exploratory integration-target error remains explained by its missing `test-utils` configuration and is outside the required `--lib` gates; no parser change was made. Optional future logout coverage remains deferred as directed.

[org-memory: wiki/engineering/workos.md]

---

# Task 2 review fix round 1

Date: 2026-09-10 IST

Fix base: `ef0b9a4d`

Source fix commit: `b9aa61d0`

## Findings addressed

1. Completion and failure now select an attempt by connection, attempt ID, and the exact initiating `session_id` before any state validation. A same-owner attempt created by another session is therefore indistinguishable from a missing attempt (`NotFound`) for both terminal paths. The shared Task 1 attempt/replay helper is unchanged.
2. Guarded PostgreSQL start/quota races now hold the user row, release both contenders through a three-party barrier, and require the observer to see two sessions waiting on the generated `FROM \"users\"` query before releasing the blocker. The parallel cleanup-claim race uses the same two-waiter proof. Session-deletion and credential-generation races now also have reverse-order positive controls proving completion succeeds when it acquires the session or membership guard first.
3. Guarded PostgreSQL fixtures are dropped only after a successful captured result. SQL, lock-observation, timeout, and propagated assertion errors now return an error naming the preserved database.

No provider call, HTTP route, runtime enablement, filesystem cleanup, or cloud write was added.

## Behavioral RED

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests::completion_and_failure_hide_other_session_attempts_like_missing_attempts -- --exact --test-threads=1
```

Relevant output (exit 101):

```text
running 1 test
test team_access::agents::recovery_tests::completion_and_failure_hide_other_session_attempts_like_missing_attempts ... FAILED

assertion `left == right` failed
  left: Err(Conflict)
 right: Err(NotFound)

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3712 filtered out
```

The RED isolated the opaque-resource defect: an existing attempt owned by another session reached later state checks, while a nonexistent attempt returned `NotFound`.

## Intermediate guarded run and preserved fixtures

The first amended recovery run compiled and executed the complete matrix, but the raw blocker query used the incorrect singular table name `user`. The result was 18 passed and 3 failed, with PostgreSQL reporting `relation \"user\" does not exist`. This was a test identifier defect, not a production failure.

The new failure-preservation behavior retained these exact databases for investigation, as required; they were not deleted:

```text
workos_team_auth_test_8d46a56232c74de4a9cc66bed020ee5d
workos_team_auth_test_1ba39307f9a04ee38b35e39d69db96de
workos_team_auth_test_b5fb19f65942442abe28f022dbe6be5f
```

Before the next expensive relink, all raw SQL relation names were checked against the repository entities/schema: `users`, `organization_membership`, `team_agent_connection`, and `team_session`. The lock observer deliberately kept the generated-query fragment `FROM \"users\"` and the blocker query was corrected to `SELECT id FROM users WHERE id = $1 FOR UPDATE`.

The first full amended build was needed for the new tests and source fix. A second relink was needed after correcting the test-only raw SQL identifier; all final agents and HTTP results below then used that same final source and test binary.

## Covering recovery matrix

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents::recovery_tests -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 21 tests
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 3694 filtered out; finished in 5.95s
```

This run covered the cross-session complete/fail controls, both two-waiter start modes, cross-provider quota, two-waiter cleanup claims, both reject-after-wait orderings, both completion-first positive controls, cancel/completion orderings, cleanup retry/takeover, stale cleanup fencing, expiry/startup recovery, and overflow rollback.

## Final exact agents gate

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_access::agents -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
running 34 tests
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 3681 filtered out; finished in 7.66s
```

## Final exact HTTP/logout gate

Command:

```text
cd src-tauri
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::http_tests -- --include-ignored --test-threads=1
```

Relevant output (exit 0):

```text
Finished `test` profile [unoptimized + debuginfo] target(s) in 1.48s
running 9 tests
test team_server::http_tests::local_logout_removes_only_the_cookie_selected_session_when_membership_is_missing ... ok
test team_server::http_tests::login_ignores_client_authority_and_callback_requires_unique_browser_binding ... ok
test team_server::http_tests::manifest_isolated_public_and_static_errors_do_not_leak_operator_routes ... ok
test team_server::http_tests::onboarding_bootstrap_selection_session_and_local_logout_are_browser_bound ... ok
test team_server::http_tests::postgres_guard_waiting_behind_logout_rejects_the_deleted_session ... ok
test team_server::http_tests::postgres_logout_waits_behind_an_authorized_mutation_guard ... ok
test team_server::http_tests::provider_denial_is_single_use_browser_bound_and_recovers_with_a_fresh_attempt ... ok
test team_server::http_tests::unsupported_methods_and_unknown_protected_paths_are_not_found ... ok
test team_server::http_tests::writes_require_exact_origin_csrf_json_and_canonical_github_id ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 3706 filtered out; finished in 2.34s
```

The HTTP command reused the final build; it did not relink after the agents gate.

## Diff, scope, and self-review

Command:

```text
git diff --check
```

Output: empty, exit 0.

The source fix commit staged exactly:

```text
src-tauri/src/team_access/agents/recovery_tests.rs
src-tauri/src/team_access/agents/store.rs
```

Self-review confirmed:

- `completion_rows` performs the exact-session selection before state validation and keeps the existing row lock on PostgreSQL.
- Complete and fail both share the corrected selection path, and tests compare another-session access with a missing-attempt control while confirming no connection mutation.
- Each required start/quota race proves both contenders are simultaneously waiting on the held user row; sequential execution cannot satisfy the observer count.
- Reverse-order controls observe completion holding `team_session` or `organization_membership` before the competing deletion/generation transaction proceeds, then assert the connection remains `connected`.
- Failure databases are not passed to exact cleanup; successful fixtures retain the existing exact-name-checked teardown.
- Task 1 replay semantics and the shared attempt helper remain unchanged.
- Root-owned documentation, `Cargo.toml`, and `Cargo.lock` were excluded from the source commit.

## Warnings and concerns

Baseline maintenance warnings reproduced unchanged:

```text
unused import: `DbBackend`
  src/db/migration/m20260817_000001_work_task_conversation_title.rs

linker stderr: ld: __eh_frame section too large (max 16MB) to encode dwarf unwind offsets in compact unwind table

the following packages contain code that will be rejected by a future version of Rust:
proc-macro-error2 v2.0.1
```

No new warning was introduced. The three named databases above intentionally remain available because their failed runs must be preserved; cleanup requires an explicit later decision. The existing unrelated integration-target feature mismatch remains outside this Task 2 gate and scope.

[org-memory: wiki/engineering/workos.md]
