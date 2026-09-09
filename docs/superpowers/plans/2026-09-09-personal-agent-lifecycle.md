# Personal Agent Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement an authenticated, generation-fenced personal connection state machine and durable cleanup intent, without launching a provider or mounting a route.

**Architecture:** A crate-private module under `team_access` consumes checked principals and the three personal-agent metadata tables. It commits ownership and mutation intent before returning an internal login ticket. Verified provider completion is a separate transaction; cancellation, logout, expiry and restart invalidate that ticket and retain cleanup work.

**Tech Stack:** Existing Rust, SeaORM, SQLite/PostgreSQL, chrono, UUID, tokio and async-trait dependencies; no new packages.

**Spec:** `docs/superpowers/specs/2026-09-09-personal-agent-connections-design.md` (approved). Prerequisite: accepted `2026-09-09-personal-agent-persistence.md` implementation.

## Global Constraints

- Every member uses their own subscription for Claude, Codex or another supported agent.
- Sharing a chat never grants use of another person's connection.
- Database records, telemetry, app logs, browser storage, screenshots, backups of ordinary app data and Git must never contain provider credentials or login codes.
- No state is derived solely from the existence of a credential file.
- Each start/cancel/disconnect changes a monotonic generation.
- Reuse the accepted team principal/transaction guard and its lock ordering. Do not hold database locks while contacting providers or starting containers.
- Foreign or nonexistent opaque resources return the same 404; attempt/code access additionally requires the exact initiating session.
- This module supplies internal lifecycle operations only: no new agent HTTP routes, provider processes or legacy dispatcher. Task2 adds only the explicitly specified existing logout invalidation hook. No enabled runtime, network/cloud/GitHub access, push or deployment.
- One Cargo process, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`; stop owned compilation before free disk approaches 2 GiB. Root owns docs; implementers own only task source/tests.

## File and interface map

Create `src-tauri/src/team_access/agents/{mod.rs,types.rs,store.rs,intents.rs,completion.rs,recovery.rs,tests.rs,recovery_tests.rs}`.
Register `pub(crate) mod agents` in `team_access/mod.rs` only.
`types.rs` holds bounded internal values/errors/projections; `store.rs` holds database locking and row conversion; `intents.rs` holds member-authorized read/start/cancel/disconnect; `completion.rs` accepts verified results; `recovery.rs` claims/completes cleanup and invalidates expired/session-bound login work. Tests use real local databases and existing `team_access::test_auth` principals. No production test helpers or mutable globally configured clock.

The metadata tables and fields are exactly those in the prerequisite persistence plan. `team_agent_operation` is the idempotency record, not a provider RPC envelope. `target_generation` identifies the old resource to clean; `result_generation` identifies the new state returned to the caller. `session_id` binds the attempt to a session hash but is not a cascading FK.

### Task 1: Personal projections and transactional login intents

**Files:** create `agents/{mod.rs,types.rs,store.rs,intents.rs,tests.rs}`; modify `team_access/mod.rs` registration only.

**Interfaces:** the following crate-private types are not HTTP DTOs and have no serde derives. Persistence and internal tickets must never be serialized to clients. Public-in-crate fields may be used by the later fixed supervisor integration, not by untrusted request deserialization.

Do not derive Debug for the new service types carrying account labels or internal
tickets. Test their safe fields/results directly rather than printing those values
through an assertion formatter. AgentError and label-free AttemptView may implement
Debug; fixed errors must not retain raw database/provider errors.

```rust
enum Provider { Claude, Codex } // exact database strings claude/codex
enum ConnectionState {
    Disconnected, Connecting, Connected, ReconnectRequired, Disconnecting, Unavailable,
}
enum AgentError {
    Auth(TeamAuthError), NotFound, InvalidInput, Conflict, RateLimited, Unavailable,
}
struct ConnectionView {
    provider: Provider, state: ConnectionState,
    account_label: Option<String>, checked_at: Option<DateTime<Utc>>,
    error_code: Option<String>,
}
struct LoginTicket {
    connection_id: String, provider: Provider, generation: i64,
    org_id: i32, user_id: i32,
    attempt_id: String, operation_id: String, lease_owner: String,
    expires_at: DateTime<Utc>,
}
struct MutationReceipt { operation_id: String, generation: i64 }
enum BeginOutcome { Pending(LoginTicket), Settled(ConnectionView) }
struct AttemptView { state: String, expires_at: DateTime<Utc> }
struct AgentService {
    db: DatabaseConnection,
    principals: Arc<PrincipalService>, clock: Arc<dyn TeamClock>,
}
// Constructor is synchronous; remaining methods are async with receiver &self.
fn new(db: DatabaseConnection, principals: Arc<PrincipalService>, clock: Arc<dyn TeamClock>) -> Self;
fn list(context: &AuthContext) -> Result<Vec<ConnectionView>, AgentError>;
fn attempt(context: &AuthContext, provider: Provider, attempt_id: &str) -> Result<AttemptView, AgentError>;
fn begin(context: &AuthContext, provider: Provider, request_key: &str) -> Result<BeginOutcome, AgentError>;
fn cancel(context: &AuthContext, provider: Provider, attempt_id: &str, request_key: &str) -> Result<MutationReceipt, AgentError>;
fn disconnect(context: &AuthContext, provider: Provider, request_key: &str) -> Result<MutationReceipt, AgentError>;
```

- [ ] **Step 1: Write and run a failing ownership test.** Use a real `test_auth::principal_fixture` with current controlled proof and resolve Alice, Bob and Alice's other organization via `PrincipalService::resolve(..., ProofRequirement::Cached)`. The service expects a context already resolved by trusted server code; HTTP Fresh requirements will be enforced at the route boundary. A minimal compiling unavailable implementation is permitted solely to establish behavioral RED and is replaced before commit.

```rust
let BeginOutcome::Pending(first) =
    service.begin(&alice_a, Provider::Codex, "first-login").await.unwrap()
else { panic!("new connection must create one pending login"); };
assert_eq!(service.attempt(&bob_a, Provider::Codex, &first.attempt_id).await,
           Err(AgentError::NotFound));
assert_eq!(service.attempt(&alice_b, Provider::Codex, &first.attempt_id).await,
           Err(AgentError::NotFound));
assert_eq!(service.attempt(&bob_a, Provider::Codex, "missing-attempt").await,
           Err(AgentError::NotFound));
assert_eq!(service.attempt(&alice_a, Provider::Codex, &first.attempt_id).await.unwrap().state, "pending");
```

Run `cargo test --lib --no-default-features team_access::agents::tests -- --test-threads=1` with the global Cargo environment. Record the failing assertion, not just compiler errors.

- [ ] **Step 2: Implement locked authority and bounded projections.** Start a transaction; read the user's immutable GitHub identity from the database and call `core::lock_user_identity`, then lock the selected org row (PostgreSQL `FOR UPDATE`; SQLite already holds the user no-op write lock). Call `principals.recheck_in_transaction` to lock/recheck membership then exact session, and only then lock/select the scoped connection. Always recheck again immediately before commit after row work. Reject database/decode/overflow errors with fixed errors, never format SQL/provider text into them. Scope every connection lookup by both context user_id and org_id; role never grants access to another member's connection.

`list` returns exactly Claude and Codex in that order, using virtual disconnected rows for providers never started; it does not create database rows. It exposes no connection ID, handle, session, operation, lease or other member label. Unknown/malformed stored values fail closed. For states other than connected, return no account_label/checked_at. `attempt` joins to the scoped provider connection and requires the exact initiating session. It returns only state/expiry, not a login code, and treats a foreign or absent attempt identically. No provider status check is claimed by this metadata-only method; later orchestration must perform the bounded official check before displaying an authenticated status.

Sample the service clock for lease, attempt, quota and mutation timestamps after
acquiring the required ownership/session/connection locks. A timestamp captured
before waiting for a lock must not authorize an attempt or lease that has since
expired. Keep the existing final principal recheck before commit as well.

- [ ] **Step 3: Implement serialized, idempotent begin.** Request keys are 1..64 ASCII bytes, characters `[A-Za-z0-9_-]`; all resource/lease IDs are server-generated UUID v4 simple hex. First lookup the scoped connection/provider and any operation `(connection,connect,key)`. An identical live leased attempt from the same exact session replays Pending with the original internal ticket without a new generation or quota debit. A completed operation with a completed attempt from that same session and an unchanged current result generation replays Settled with the own connection projection; do not require the old login lease or attempt expiry to remain live after successful completion, and do not return a new login ticket. A reused key bound to another session, an expired/invalidated pending ticket, changed generation, or incompatible input returns Conflict; never start fresh work under an old key.

Reject request keys beginning `__neev_`; that namespace is reserved for Task2's server-generated recovery operations. For a new begin require disconnected state with no queued/leased cleanup or active login operation. Never overwrite connected, unavailable-with-cleanup, connecting or disconnecting state; require explicit disconnect/cleanup first. Insert a missing connection with generation1; otherwise checked_add(1). Count all attempts for this user across organizations/providers with created_at strictly newer than now minus15minutes; five means RateLimited. The user lock serializes this quota across all member sessions/orgs. Current time comes from TeamClock, never request input. Insert pending attempt bound to context.session_id and expiring now+10minutes, and a connect operation with target=result=new generation, state leased, server UUID owner, lease expiry equal to attempt expiry. All writes commit together. The ticket contains only metadata; the future supervisor must treat repeated ticket delivery as one idempotent native login operation, never launch a second process.

- [ ] **Step 4: Implement invalidation intents.** Cancel requires the exact own pending attempt and matching current connecting generation. Before mutating, replay an existing same-key cancel only when target_attempt_id is identical and that attempt belongs to the same initiating session; a different target is Conflict. A successful cancel increments generation, marks the old attempt canceled and its connect operation abandoned (clear both lease fields), sets the connection disconnecting, clears account label/checked_at, and queues a cancel operation targeting the old generation/attempt with the new result generation. This serializes cancellation against completion.

Disconnect operates on an existing own provider connection; absent is NotFound. Same-key replay returns its stored receipt; no new generation. A new disconnect increments generation and invalidates any pending attempt/connect lease. When already disconnected with no work/handle, the operation may complete immediately while retaining the new generation. Otherwise set disconnecting and queue disconnect with target=old generation/result=new generation, target_attempt null. Clear visible account metadata immediately; keep opaque storage_handle internally until verified cleanup. If cleanup is already queued/leased, a different key returns Conflict rather than replacing cleanup identity. Do not contact or claim logout from a provider. Cleanup operations have expiry now+10minutes for bounded lease allocation, but must remain retryable after that time; expiry is not permission to discard required cleanup.

- [ ] **Step 5: Test and commit.** Add positive controls for each user/org, admin denial, same-person second-session attempt denial, provider separation, scoped idempotent replay (both pending and a seeded completed operation/attempt, with no new row or quota debit), reused cancel key with another target, invalid/reserved keys, checked generation overflow, quota across providers/orgs with exact window boundary, simultaneous SQLite starts on one service, stale membership/session/credential rejection, and cancel/disconnect invalidation metadata. A completed retry after generation change must be Conflict. Assert actual rows and unchanged state on failure. Exercise all methods after authority changes. Run the focused tests and `git diff --check`; commit only Task1 files.

### Task 2: Verified completion, durable cleanup and invalidation recovery

**Files:** create `agents/{completion.rs,recovery.rs,recovery_tests.rs}`; modify `agents/{mod.rs,store.rs,types.rs}` for these interfaces and shared narrowly scoped helpers; modify `agents/intents.rs` only to make the three `AgentService` fields `pub(super)` so sibling implementation modules can use them; modify `team_server/handlers/session.rs` to call the invalidator before session deletion within the existing logout transaction; modify `team_server/http_tests.rs` only for logout integration and clock-argument updates.

**Interfaces:** consume Task1 AgentService, typed provider/errors/internal login ticket and locked store helpers. Add these non-serialized internal types and async methods:

Task1 defines `AgentService` in `intents.rs`. Keep that location and its existing
methods; only its `db`, `principals` and `clock` field visibility becomes
`pub(super)`, restricted to the containing agents module and its descendants.
Reuse `store::authorized_transaction`, `store::final_recheck_and_commit`, the
scoped row selectors and row mutation helpers. Extend store helpers for cleanup
without fabricating a principal or duplicating the guarded member transaction.

```rust
struct VerifiedSubscription { account_label: String, storage_handle: String }
struct CleanupTicket {
    operation_id: String, connection_id: String, provider: Provider,
    org_id: i32, user_id: i32,
    target_generation: i64, result_generation: i64,
    lease_owner: String, lease_expires_at: DateTime<Utc>,
}
fn complete(context: &AuthContext, ticket: &LoginTicket, account: VerifiedSubscription) -> Result<(), AgentError>;
fn fail(context: &AuthContext, ticket: &LoginTicket, error_code: &str) -> Result<(), AgentError>;
fn claim_cleanup(operation_id: &str) -> Result<Option<CleanupTicket>, AgentError>;
fn finish_cleanup(ticket: &CleanupTicket, succeeded: bool) -> Result<(), AgentError>;
fn expire_pending() -> Result<u64, AgentError>;
fn recover_startup() -> Result<u64, AgentError>;
// Separate function for the already-locked logout transaction:
fn invalidate_session_in_transaction(tx: &DatabaseTransaction, session_id: &str, now: DateTime<Utc>) -> Result<(), AgentError>;
```

`VerifiedSubscription` is trusted internal evidence supplied only after a later official-client adapter validates account mode/status; this service does not claim it performs provider authentication. A forged HTTP body can never deserialize into it. Validate account_label 1..160 characters with no control or bidi formatting characters, storage_handle exactly a UUID simple hex. No Debug derive on these new service/provider-result types carrying account labels or internal tickets; error_code must be a fixed schema allowlist value. Do not modify the accepted persistence entity derives or log entity models.

Both internal ticket types carry org_id/user_id from the locked connection row,
never from an untrusted body. Completion validates these against its context and
row; cleanup validates them against its row. This lets the later supervisor bind
every generation resource to its complete owner tuple without needing database
credentials or accepting a user-selected path.

- [ ] **Step 1: Establish behavioral RED for late completion.** Begin a login, cancel it, and try completion with the original ticket and invented provider label/handle; require Conflict and an unusable connection. Separately delete the exact initiating session, and require Auth Unauthorized with no connected row. Add successful own completion positive control before broad negative matrices. Run `team_access::agents::recovery_tests` serially and record the specific missing-behavior failure.

- [ ] **Step 2: Implement final guarded completion/failure.** Under the Task1 lock order and final principal recheck, require exact connection/provider/generation, attempt id/generation/session, pending nonexpired attempt, leased connect operation id/owner, unexpired lease and matching target/result generation. Reject ticket field tampering, replay after invalidation and expired proof. On success, set connected with safe label, current checked_at and private handle; mark attempt and operation completed and clear lease fields, all in one commit. No provider calls occur here. Repeated completion of an already-completed ticket is Conflict, not an overwrite of account identity.

On a valid failure, advance generation, mark attempt failed and connect operation abandoned, make the connection disconnecting with the safe error, and enqueue a disconnect cleanup operation targeting the old generation. The supervisor will clean using `(connection_id,provider,target_generation)` even if a storage handle was never returned before failure. Cleanup must not use a caller-supplied filesystem path. Fixed generated recovery keys must not collide with valid caller keys: reserve server keys beginning `__neev_` and reject that prefix in Task1 key validation.

- [ ] **Step 3: Implement durable cleanup leases.** Claim only queued cancel/disconnect operations or their expired leases, using row locks under user→org→connection→operation order; no principal is needed to finish cleanup that was already authorized. Do not construct an AuthContext from database rows or supervisor input. Allocate a fresh UUID lease owner and 30second lease per claim. A live lease is never stolen. Do not abandon a cleanup merely because its original operation expires_at has passed. Return no login ticket/credential/path from this API.

Finish requires matching operation/owner, a still-live lease and immutable target/result generations; repeated or stale finish is Conflict. On failure return operation to queued, clear lease fields, leave connection disconnecting/error cleanup_failed, never connected. On success mark operation completed. Change connection to disconnected with all status/storage optional fields null only if current generation equals result_generation and it remains disconnecting; otherwise do not touch newer state. Marking old cleanup complete never removes a newer generation resource. Actual file/provider cleanup must be proved by the future supervisor before it calls successful finish.

The connection-generation/state condition applies to failure updates too: a failed
old cleanup may queue its own operation for retry, but may set cleanup_failed on
the connection only at its matching result generation in disconnecting state.
Never overwrite a newer connection's status, label, handle or error on either
successful or failed old cleanup. Include both outcomes in the newer-state test.

- [ ] **Step 4: Implement fail-closed recovery and logout.** Expire pending attempts whose expiry<=now; startup recovery expires all pending attempts because memory-only challenges are lost. Each invalidation locks the connection, rechecks exact pending generation, advances generation, abandons connect leases, marks attempt expired and queues cleanup as in cancel (server-generated reserved key). Work from bounded pages of100IDs; never hold a full-table transaction or external work under locks. Repeated recovery is idempotent and never modifies a previously connected account. Startup recovery is an explicit pre-admission call, not called merely by constructing AgentService; it will only be wired into the single active team-server startup until a separately reviewed multi-server leader/fencing design exists.

For logout, the existing transaction already locks membership then session. Call `invalidate_session_in_transaction` before deleting that exact session. It acquires only that session's connection/operation locks, in sorted connection-ID order, and never requests earlier user/org/membership locks. Task1/completion always acquire membership/session before connections, preserving order. Invalidation uses the same generation-bound cleanup helper, state canceled/error canceled; errors abort logout rather than falsely clear cookies while login attempts remain active. Do not invalidate another session's attempts or delete an existing connected account. Do not invoke PrincipalService or GitHub during logout; cleanup can proceed without an active login.

Add `clock: &dyn TeamClock` as the fourth argument of existing `synchronized_logout`; the HTTP handler passes `state.clock.as_ref()`. Thread it into the private transaction function and sample after locking the session, not before waiting on locks. Update its three existing direct test call sites with controlled `TestClock::new(fixed_now())`. Map invalidator errors to existing fixed TeamAuthError values (Auth passthrough, Conflict→Conflict, other failures→Unavailable), never include their database inputs. Orphaned expired sessions with no matching attempts must still be removable.

- [ ] **Step 5: Verify races in real databases.** SQLite tests cover successful connection, malformed safe metadata, wrong billing evidence represented as fail unsupported_account, stale ticket fields/lease, cancel/fail/expiry/logout/startup interleavings, duplicate cleanup claims, expired lease takeover, stale cleanup result, cleanup retries after original expires_at, generation overflow rollback and no secrets in persisted columns/projections. Use controlled TeamClock rather than sleep for expiry.

Guarded disposable PostgreSQL tests use existing `create_postgres_principal_fixture` and exact-name-checked `drop_postgres_fixture`; preserve failure databases. Use barriers/lock observations, not probabilistic loops: two starts give one ticket/one Conflict; same-key concurrent starts return the same ticket; member-wide quota across two providers serializes; completion blocked behind session deletion rejects; completion blocked behind credential-generation change rejects; cancel/complete ordering never reactivates a canceled connection; parallel cleanup claims yield one lease. Both orderings should have explicit positive controls. No live database or provider access.

Run `cargo test --lib --no-default-features team_access::agents -- --include-ignored --test-threads=1`, then `cargo test --lib --no-default-features team_server::http_tests -- --include-ignored --test-threads=1` and `git diff --check`. The latter includes real HTTP logout and both PostgreSQL logout/guard orderings. Report exact counts and warning baseline, then commit only this task's files. No browser/frontend suite is required for an unmounted Rust lifecycle API.

## Self-review and downstream gates

Task1 covers internal ownership, transactional start/cancel/disconnect, quota and idempotency. Task2 covers final authority/generation checks, cleanup custody metadata and logout/restart races. Both use the same store lock ordering and metadata fields. Task2's reserved server-key prefix must be included in Task1's implementation brief before dispatch, not retroactively left ambiguous.

Provider code/URL channels, fixed client parsing, hosted terms, encrypted storage, enforced egress/resource quotas, actual containers and fair global scheduling are the next supervisor/adapters plan. Typed HTTP, official live status refresh and ten-locale UI/actual-browser flows are the subsequent UI plan. None of those requirements is satisfied or enabled by this lifecycle module. Per-author prompt execution remains separately gated on workspace/privacy conversion.
