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
