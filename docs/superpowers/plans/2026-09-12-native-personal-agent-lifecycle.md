# Native Personal Agent Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the approved personal-connection metadata and lifecycle behavior to independent Neev identity, with current native authority and tested cleanup fencing.

**Architecture:** Native metadata belongs exclusively to the native database and uses its checked organization session. Three constrained tables hold lifecycle intent, never provider credentials. Internal functions commit intent before external work and recheck ownership, session, generation and time before accepting a verified result; this plan exposes no provider route or process.

**Tech Stack:** Existing Rust, SeaORM/SeaQuery, SQLite/PostgreSQL, UUID and Tokio; no dependency additions.

**Spec:** `docs/superpowers/specs/2026-09-09-personal-agent-connections-design.md`, with the independent identity authority in `docs/superpowers/specs/2026-09-11-independent-identity-and-organizations-design.md` superseding references to GitHub/team principals. Prerequisite: accepted native chats/documents plan, including its backup verifier.

**Status:** Ready for execution after content prerequisite accepted at4a50e649 (all four final findings addressed; documentation-only residuals recorded). Main inspected current db initialization and read-only snapshot interfaces: content_migration::SCHEMA_VERSION is3, HARDENED_SCHEMA_VERSION is4, and validate_content_snapshot returns the actual i64 marker. This stage uses schema5. This is not a new provider or execution approval.

## Global Constraints

- Every member uses their own subscription for Claude, Codex or another supported agent.
- Sharing a chat never grants use of another person's connection.
- Database records, telemetry, app logs, browser storage, screenshots, backups of ordinary app data and Git must never contain provider credentials or login codes.
- No state is derived solely from the existence of a credential file.
- Each start/cancel/disconnect changes a monotonic generation.
- Do not hold database locks while contacting providers or starting containers.
- Native identity is authoritative. Never call the legacy migrator, team principal service, provider dispatcher or shared credential store.
- Foreign or nonexistent opaque resources return the same 404; attempt/code access additionally requires the exact initiating session.
- This plan adds internal functions only. No provider HTTP routes, runtime enablement, provider process, cloud mutation, publication or real credentials.
- Lock admission first, then native account/organization/membership/session authority, then scoped connection/attempt/operation rows. Fresh clocks follow blocking locks; no filesystem/network work inside transactions.
- Preserve all existing data. Empty or exactly known native schemas may migrate forward; legacy, partial and unknown databases must be refused. No down migration or cleanup of real data.
- One serial Rust build: `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`, `--lib --no-default-features`; stop before available disk reaches3GiB. Explicit-file rustfmt with `--config skip_children=true`, not whole-crate formatting.
- All database/provider evidence in tests is invented and isolated. Reuse guarded native SQLite/PostgreSQL fixture helpers; retain and identify failed owned fixtures without dumping complete account records.
- PostgreSQL tests use exactly `NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1'` and owned UUID schemas, never the configured app database. Full native gate is `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev:: -- --include-ignored --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1` with the global build variables; do not run the entire legacy library.
- No new package or global test setting. Commit exact task files only; preserve unrelated changes and root-owned documents.

## File and interface map

`src-tauri/src/neev/db/agent_migration.rs` owns native schema5 statements and indexes; `agent_tests.rs` tests actual constraints/upgrades. Native `db/mod.rs` registers the version chain. No legacy entity files change.

`src-tauri/src/neev/agents/types.rs` holds bounded internal values; `store.rs` owns scoped row decoding and locks; `intents.rs` owns member operations; `completion.rs` owns final result admission; `recovery.rs` owns durable cleanup and pending-attempt invalidation. `mod.rs` registers those modules and is registered in `neev/mod.rs`. Task-local `test_support.rs`, `tests.rs` and `recovery_tests.rs` use native fixtures. No content test helpers are made production APIs.

Use free async functions taking `&ServerState`; there is no second principal service or cached authority. All time fields are checked Unix seconds in i64, not legacy chrono timestamps. Provider enum is `agents::types::Provider`, distinct from Google/email identity providers.

### Task 1: Native metadata schema and contracts

Controller structure clarification: test-only `db/agent_tests/{support,constraints,migrations}.rs`
may split fixture helpers, direct state constraints and version/inventory cases
under the unchanged `agent_tests` module and filter if the root test file grows
beyond focused-file intent. Preserve the initial missing-table regression and all
behavioral assertions. No runtime restructuring is authorized by this allowance.

**Files:** Create `src-tauri/src/neev/db/{agent_migration.rs,agent_tests.rs}` and `src-tauri/src/neev/agents/{mod.rs,types.rs}`. Modify `neev/{mod.rs,db/mod.rs}` for registration and exact version migration; change existing native schema tests only where the latest version is asserted, preserving explicit historical-version tests. Modify `neev/content/{backup.rs,backup_tests.rs}` and `docs/operations/neev-content-backup.md` for native5 verification and explicit historical native3/4 acceptance. These files are owned by the content prerequisite and must exist before this task begins.

**Interfaces:** `agent_migration::{TABLE_NAMES,SCHEMA_VERSION,upgrade(&DatabaseTransaction) -> Result<(),AuthError>}` with version5. Its `upgrade` creates only the new tables/indexes inside the caller's transaction and never updates the marker independently.

- [ ] Write a behavioral missing-table test using actual native initialization, not a source-text assertion:

```rust
let database = crate::neev::test_support::database().await;
let rows = database.query_all(crate::neev::db::statement(
    &database, "SELECT id FROM neev_agent_connections", vec![],
)).await.expect("native personal metadata table exists");
assert!(rows.is_empty());
```

Run the `neev::db::agent_tests` filter and record an actual missing-table failure before implementation.

- [ ] Build the three native tables with SeaQuery's existing portable named-check/index/FK pattern. Column names below are the exact store contract. Text has no automatic ownership/state defaults; IDs are server UUID simple hex32, generations/times i64, organization/user i32. Optional columns alone permit NULL.

```text
neev_agent_connections:
id PK, org_id, user_id, provider, generation, state,
account_label?, checked_at?, error_code?, storage_handle?, created_at, updated_at

neev_agent_attempts:
id PK, connection_id, generation, session_id, state,
created_at, expires_at, updated_at

neev_agent_operations:
id PK, connection_id, kind, request_key, target_attempt_id?,
target_generation, result_generation, state, lease_owner?, lease_expires_at?,
error_code?, created_at, expires_at, updated_at
```

Connections: org_id/user_id/generation positive; provider `claude|codex`; state `disconnected|connecting|connected|reconnect_required|disconnecting|unavailable`; account_label1..160Unicode scalars when present; storage_handle exactly32ASCII hex service-validated and database length32. Connected requires label/checked_at/handle and NULL error; disconnected requires all four NULL. Other states require account_label/checked_at NULL and permit only optional error_code/storage_handle cleanup metadata. Direct INSERT/UPDATE tests must reject transitional labels/check times. created_at>=0, updated_at>=created_at, optional checked_at>=0. Unique(org_id,user_id,provider); FK(org_id,user_id)→neev_memberships(org_id,user_id), RESTRICT. This does not treat an inactive membership as authorized; services recheck active membership.

Attempts: positive generation; state `pending|completed|canceled|expired|failed`; created_at>=0, expires_at>created_at, updated_at>=created_at; ID32, connection_id32, session_id1..128. Unique(connection_id,generation) and unique(connection_id,id). FK connection_id→connections.id RESTRICT. Session is an opaque exact-session binding retained with cleanup intent; no cascade or inferred session authority.

Operations: kind `connect|cancel|disconnect`; state `queued|leased|completed|abandoned`; positive target_generation, result_generation>=target_generation; ID32, connection_id32, optional target_attempt_id32, optional lease_owner32, request_key1..64. Leased requires both lease fields, other states require both NULL; optional lease_expires_at>=0. Connect/cancel require target_attempt_id; disconnect requires NULL. Same time checks as attempts. Unique(connection_id,kind,request_key). FK connection_id→connections.id and composite(connection_id,target_attempt_id)→attempts(connection_id,id), RESTRICT. This prevents a cleanup operation naming another connection's attempt; NULL disconnect target remains valid.

Both error fields permit only NULL or `provider_unavailable|reauth_required|unsupported_account|expired|canceled|cleanup_failed|interrupted`. All opaque ID/lease syntax is additionally parsed by service code; database checks do not replace typed validation. Add named lookup indexes on attempts(state,expires_at), operations(state,lease_expires_at), connections(user_id,org_id). No code/URL/token/raw-output/JSON column.

Every state-dependent CHECK must explicitly reject unintended NULL, not rely on
SQL equality evaluating false. In particular connected requires each of label,
checked_at and handle `IS NOT NULL`; leased requires BOTH lease fields `IS NOT
NULL`; connect/cancel require target_attempt_id `IS NOT NULL`. Exercise direct
INSERT and UPDATE with each required field independently NULL and both half-null
lease combinations. These are constraint tests, not merely service validation.

- [ ] Extend initialization transactionally: empty→base2+content3+hardening4+agent5; exact2→content3+hardening4+agent5; exact3→hardening4+agent5; exact4→agent5; exact5 unchanged. Preserve the content hardening migration's valid-row check and constraints, including its transactional refusal of malformed source pairs. Compare the exact allowed table set with its matching marker before any mutation. Reject marker1, marker>5, marker/table mismatch, partial and legacy sets. On any error rollback all new tables and marker work. Never accept a database merely because a marker says5.
- [ ] Extend the accepted read-only snapshot helper's verified-version result, without invoking initialization or acquiring write intent. Preserve ordinary-table checks on PostgreSQL/SQLite, exact single marker `(id=1)`, and the exact inventory matching that marker. Accept historical content3/4 (base+content tables) or native5 (base+content+agent tables); reject2/unknown/mixed/partial schemas unchanged. The verifier must not migrate archived3/4 evidence to5.

```rust
pub(crate) async fn validate_content_snapshot<C: ConnectionTrait>(
    db: &C,
) -> Result<i64, AuthError>;

// In content::backup::verify_snapshot, within its existing read-only snapshot:
let schema_version = db::validate_content_snapshot(&tx).await?;
// Keep existing reference collection, rollback, canonical sort and BlobStore reads.
Ok(Manifest { schema_version, blobs })
```

Keep the verifier's accepted explicit-option/environment isolation unchanged.
Add actual historical3/4 and new5 manifest assertions with nonempty saved revisions,
and refusal tests for marker3/4+agent tables and marker5 missing an agent table.
Re-run existing same-name-view and controlled-connection regressions alongside
these new tests. Document all three supported snapshot versions, not a generic latest
version promise. No change to the Node dump/copy/complete-marker format is needed.

- [ ] Define the internal enums and values used by Tasks2/3. All fields crate-visible, no serde. Only enums/errors and label-free AttemptView may derive Debug. Account-bearing and ticket types may derive Clone/Eq/PartialEq but tests must not format them wholesale.

```rust
enum Provider { Claude, Codex }
enum ConnectionState {
    Disconnected, Connecting, Connected, ReconnectRequired, Disconnecting, Unavailable,
}
enum AgentError {
    Auth(crate::neev::error::AuthError), NotFound, InvalidInput,
    Conflict, RateLimited, Unavailable,
}
struct ConnectionView {
    provider: Provider, state: ConnectionState, account_label: Option<String>,
    checked_at: Option<i64>, error_code: Option<String>,
}
struct LoginTicket {
    connection_id: String, provider: Provider, generation: i64,
    org_id: i32, user_id: i32, session_id: String,
    attempt_id: String, operation_id: String, lease_owner: String, expires_at: i64,
}
enum BeginOutcome { Pending(LoginTicket), Settled(ConnectionView) }
struct MutationReceipt { operation_id: String, generation: i64 }
struct AttemptView { state: String, expires_at: i64 }
struct VerifiedSubscription { account_label: String, storage_handle: String }
struct CleanupTicket {
    operation_id: String, connection_id: String, provider: Provider,
    org_id: i32, user_id: i32, target_generation: i64, result_generation: i64,
    lease_owner: String, lease_expires_at: i64,
}
```

- [ ] Cover fresh/rerun/exact2/exact3/exact4 preservation on SQLite and guarded PostgreSQL. Populate identity, memberships, projects, private/source-bound content and revisions before3→4→5 and4→5; verify all prior IDs, counts, metadata and source constraints survive while the marker advances. Exercise each CHECK/NULL shape, duplicate scope, same user in another org, another user in same org, invalid membership FK, cross-connection attempt FK, overflow/bounds and rollback of failed writes. Refuse partial/unknown/legacy sets unchanged. Backup acceptance must continue to verify schema3/4 historical archives explicitly and recognize schema5 without dropping tables from a manifest. Run exact full native gate and diff/format checks, then commit only listed implementation/tests.

### Task 2: Native own-account projections and lifecycle intents

**Files:** Create `neev/agents/{store.rs,intents.rs,test_support.rs,tests.rs}`; register in agents/mod.rs. Shared native fixture additions require narrowly scoped cfg(test) helpers only.

**Interfaces:** These free async functions live in intents.rs and use Task1 types:

```rust
async fn list(state: &ServerState, actor: &OrgContext) -> Result<Vec<ConnectionView>, AgentError>;
async fn attempt(state: &ServerState, actor: &OrgContext, provider: Provider, attempt_id: &str) -> Result<AttemptView, AgentError>;
async fn begin(state: &ServerState, actor: &OrgContext, provider: Provider, request_key: &str) -> Result<BeginOutcome, AgentError>;
async fn cancel(state: &ServerState, actor: &OrgContext, provider: Provider, attempt_id: &str, request_key: &str) -> Result<MutationReceipt, AgentError>;
async fn disconnect(state: &ServerState, actor: &OrgContext, provider: Provider, request_key: &str) -> Result<MutationReceipt, AgentError>;
```

- [ ] Add a task-local Harness using native_account, organization, add_membership, scoped_context, TestClock and fixture-owned preview_config. Keep `state:Arc<ServerState>`, `clock:Arc<TestClock>`, and AliceA/AliceB/BobA/AdminA/second-session-AliceA contexts. Each is current native authority, not a forged body. Add callable unavailable stubs solely for behavioral RED, then exercise:

```rust
let BeginOutcome::Pending(ticket) = intents::begin(
    &h.state, &h.alice_a, Provider::Codex, "first-login",
).await.unwrap() else { panic!("new login must be pending"); };
for actor in [&h.alice_b, &h.bob_a, &h.admin_a, &h.alice_second_session] {
    assert!(matches!(intents::attempt(
        &h.state, actor, Provider::Codex, &ticket.attempt_id,
    ).await, Err(AgentError::NotFound)));
}
assert_eq!(intents::attempt(&h.state, &h.alice_a, Provider::Codex,
    &ticket.attempt_id).await.unwrap().state, "pending");
```

- [ ] Implement store::authorized_transaction(state,actor) with db::begin→lock_admission_guard→session::recheck(&tx,actor,state.clock.now_unix()); return the transaction, not cached principal proof. Scope connection lookup by BOTH actor.org_id and actor.identity.user_id before any FOR UPDATE. Recheck current authority with a fresh clock just before commit; malformed/decode/overflow values use fixed safe errors. Connection locking precedes attempt/operation locking, sorted by ID for sets. Helpers are pub(super) for Task3, not general public APIs.
- [ ] List exactly Claude then Codex, synthesizing disconnected views without creating rows. Only connected exposes own safe label/checked_at; no IDs, handles, leases or other member labels in projection. Attempt lookup scopes connection/provider/owner then exact session; foreign and absent are NotFound. This metadata method does not claim a live provider status check.
- [ ] Implement begin under native serialized admission. Request key1..64ASCII `[A-Za-z0-9_-]`, reject `__neev_` reserved prefix. First handle same connection/kind/key receipt: exact-session pending live leased unchanged generation→same ticket; completed attempt+operation unchanged generation→Settled current own view, without lease/deadline requirement or new quota charge. Other-session, changed input/target/generation, expired or invalidated pending receipt→Conflict.
- [ ] New begin requires disconnected/no handle/no queued or leased operation. Missing connection starts generation1; existing uses checked_add(1). Count this user's attempts across organizations/providers with created_at>now-900; at five return RateLimited. Use clock after blocking row locks and checked_add(600) deadline. Insert one pending exact-session attempt and one leased connect operation, target=result=new generation, UUID owner/IDs, lease expires with attempt. Commit together before returning a ticket. No external work.
- [ ] Cancel requires exact own pending attempt/current connecting generation. Same-key replay validates identical attempt and initiating session. Advance generation, cancel attempt, abandon connect operation/clear lease, set disconnecting, clear visible label/checked_at, queue cancel targeting old generation+attempt with new result generation. Disconnect requires existing own provider; same-key replay returns receipt; new key conflicts with active cleanup. It advances generation, invalidates pending login, clears display metadata and queues disconnect targeting old generation. A truly disconnected/no-work/no-handle connection may complete disconnect immediately while retaining advanced generation. Queue lifetime600seconds does not permit deletion of uncompleted cleanup after expiry.
- [ ] Tests cover all methods after session/member/org/account invalidation, admin denial, positive own controls, scope/provider separation, current and settled replay row/charge counts, changed cancel target, key validation, generation/time overflow rollback, exact quota boundary across organizations, simultaneous starts, canceled/disconnected state, and zero stored challenge fields. Test both SQLite and PostgreSQL authority/admission contracts; run exact full native gate and commit only task files.

### Task 3: Final-result fencing, cleanup and native logout

**Files:** Create `neev/agents/{completion.rs,recovery.rs,recovery_tests.rs}`; extend agents/{mod.rs,store.rs,test_support.rs} narrowly. Modify `neev/auth/session.rs` for admission-first atomic pending-login invalidation on revoke, `neev/http/auth.rs` for its clock argument, and native auth/session or HTTP tests covering that integration. Do not wire startup recovery to every ServerState constructor or change provider capability flags.

**Interfaces:** Free async functions using Task1 types and Task2 locked store helpers:

```rust
async fn complete(state: &ServerState, actor: &OrgContext, ticket: &LoginTicket, account: VerifiedSubscription) -> Result<(),AgentError>;
async fn fail(state: &ServerState, actor: &OrgContext, ticket: &LoginTicket, error_code: &str) -> Result<(),AgentError>;
async fn claim_cleanup(state: &ServerState, operation_id: &str) -> Result<Option<CleanupTicket>,AgentError>;
async fn finish_cleanup(state: &ServerState, ticket: &CleanupTicket, succeeded: bool) -> Result<(),AgentError>;
async fn expire_pending(state: &ServerState) -> Result<u64,AgentError>;
async fn recover_startup(state: &ServerState) -> Result<u64,AgentError>;
async fn invalidate_session_in_transaction(tx: &DatabaseTransaction, session_id: &str, clock: &dyn crate::neev::clock::Clock) -> Result<(),AgentError>;
```

- [ ] Establish RED by begin→cancel→complete(original ticket, invented subscription), expect Conflict and unchanged unusable state; include successful own completion positive control. No real client/evidence. Safe label1..160Unicode without controls or bidi formatting; storage handle UUID simplehex32. Verify helper predicates against those exact values, not generic nonempty checks.

```rust
let outcome = completion::complete(&h.state, &h.alice_a, &ticket,
    VerifiedSubscription { account_label: "synthetic account".into(),
        storage_handle: uuid::Uuid::new_v4().simple().to_string() }).await;
assert!(matches!(outcome, Err(AgentError::Conflict)));
let own = intents::list(&h.state, &h.alice_a).await.unwrap();
assert!(own.iter().all(|v| v.state != ConnectionState::Connected));
```

- [ ] Completion/failure first acquire current native authority and fully scoped connection/attempt/operation locks, then sample fresh time. Require every ticket field to match context and row, exact initiating session, pending attempt/live expiry, connecting matching generation, leased connect operation/live lease/owner and exact target=result generation. A foreign session/owner is NotFound, not a state-dependent oracle. Success stores safe label/current checked_at/handle, sets connected/no error and atomically completes attempt/operation/clears lease. Duplicate completion is Conflict. Failure allowlist only, advances generation, fails attempt/abandons connect lease, makes disconnecting and queues server-reserved disconnect cleanup for old generation. Failed authority/expiry/tampering leaves rows unchanged.
- [ ] Cleanup needs no active member because removal must not strand authorized cleanup. It never fabricates OrgContext. Acquire admission then scoped connection/operation locks from trusted stored ownership; reject ticket tuple mismatch. Claim queued/expired-leased cancel/disconnect, never connect/live lease; issue random owner and now+30second lease with checked arithmetic after locks. Ignore original cleanup expiry as a deletion signal. Finish verifies operation/tuple/generations/owner/live deadline. Success completes operation; failure queues retry with cleared lease. Change connection only when current generation=result_generation AND state=disconnecting: success clears all optional metadata/disconnected; failure sets cleanup_failed and remains unusable. Both outcomes must leave a newer connected row entirely unchanged.
- [ ] Expire only pending expired attempts; startup recovery invalidates all pending because challenges are memory-only. Bounded pages100IDs, per-item guarded transaction/fresh clock; no unbounded table transaction. Recheck current generation/state and enqueue reserved-key cleanup; repeat is idempotent, never changes previously connected accounts or overwrites newer state. Keep startup recovery an explicit callable for the later single-supervisor startup contract, not multiple-server unfenced background work.
- [ ] In native session::revoke add admission guard immediately after db::begin, before account/session locks. Change its third argument from `now:i64` to `clock:&dyn crate::neev::clock::Clock`; the single current production caller in `neev/http/auth.rs` passes `state.clock.as_ref()`. Sample after account/session locks for session validation, then again after connection/attempt locks for invalidation timestamps and final revocation. After current identity/session checks, invalidate only this session's pending attempts before setting revoked_at in the same transaction. Invalidation locks connection IDs in ascending order and never reacquires earlier authority locks. Do not silently delete connected credentials or another session's attempt. On error rollback revoke; fixed Auth errors only. Existing logout exact-Origin/CSRF/no-store behavior remains unchanged. Cancellation must invalidate even an already-expired pending attempt; expiry never permits late completion.
- [ ] Deterministic PostgreSQL lock-overlap tests: simultaneous begin, same-key replay, member-wide quota, completion vs logout/cancel/disconnect/member removal in both orderings, cleanup claims, expiry crossing while blocked (599live/600expired) for both complete AND fail. Independently hold another user's connection lock and same-user/different-org connection lock; foreign lookups must return without waiting. Use lock observations/barriers and bounded timeouts, not probabilistic loops. SQLite additionally covers tampered fields, malformed labels/handles, restart idempotency, exact-session logout, expired cleanup takeover, stale cleanup success/failure, retries after original expiry and arithmetic rollback. Inspect unchanged rows with selected safe fields, never format full account entities.
- [ ] Run focused recovery/logout tests, then exact full native suite with guarded PostgreSQL and only interactive browser driver excluded. Record actual commands/counts/warnings, format/diff checks and commit source/tests. Main performs task and whole-plan review. No result here means official sign-in, credential encryption, gVisor egress or author-scoped jobs are accepted.

## Downstream release boundaries

The next connection plan must implement the fixed authenticated supervisor, pinned official clients, verified account-mode parsing, memory-only challenges, global/fair concurrency limits, encrypted private credential custody and enforced network/resource policy. A subsequent HTTP/UI plan mounts only bounded typed operations with exact Origin/CSRF/session scope and actual browser acceptance. Real sign-in requires terms/custody/profile gates and consenting participants. Prompt jobs still require their own credential-safe author/data execution design. These are release requirements, not scope reductions or capabilities enabled by this plan.

Publication remains through the Workos publish clone and the current interactive ship gate. Context: [org-memory: wiki/engineering/workos.md].
