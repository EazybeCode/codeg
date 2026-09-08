# Team Authentication Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the reviewed session, membership and OAuth primitives into atomic organization onboarding and a fully verified request principal.

**Architecture:** CredentialService owns identity-bound encrypted token persistence and a database refresh lease. OnboardingService consumes browser-bound one-time attempts/pending choices and writes identity, preserved organization branding, membership, credentials and scoped session in one transaction. PrincipalService combines local session validation with generation-bound GitHub membership proof and local revocation checks. These services have no legacy AppState dependency; the next team router constructs them explicitly.

**Tech Stack:** Existing Rust, SeaORM, SQLite/PostgreSQL, Tokio, futures-util, chrono, serde, async-trait, thiserror; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`

## Global Constraints

- Use a stable GitHub user ID for identity and stable GitHub organization ID for tenancy. Names/logins are display and lookup metadata, never authorization keys.
- Keep tokens encrypted on the server.
- Onboarding records authorize only membership discovery and selection, never workspace operations.
- A local administrator's block survives OAuth login and membership refresh; only an authorized administrator can lift it.
- Existing `users.org_id` and `users.role` fields remain compatibility data while legacy code is isolated; team authorization must never use them as its source of truth.
- A positive GitHub membership proof may be cached for at most 60 seconds, keyed by stable user, organization and credential generation. Concurrent requests share a verification request.
- Expired proof is never extended on a timeout, rate limit, malformed response or upstream failure; protected activity waits or fails closed.
- Admin actions and new execution/publication requests require a fresh proof.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No real credentials, GitHub/provider requests, cloud writes, push or deployment in implementation tasks. Tests use invented credentials and isolated local databases/fake sources only.
- Use existing dependencies and one Cargo process at a time with `CARGO_INCREMENTAL=0`.

## Prerequisites and ownership

Execute only after the OAuth credential primitives plan has passed its combined review. Its `GithubTokenSet`, `GithubOAuthSource`, `SecretVault`, `SecretBinding` and tables are consumed without changing their protocol/storage contracts. The reviewed `GithubMembershipSource` returns evidence, not authority. `resolve_local_session` remains a local consistency check.

No HTTP routes, production startup, UI, visibility, provider runner or deployment changes are owned here. Complete these services then continue to the dedicated team startup/router plan; this is not a team-readiness claim.

All database writes in these services acquire the user identity row before organization then membership rows when multiple identities are involved. A portable no-op `users.id = users.id` update, filtered by local and immutable GitHub IDs and requiring exactly one affected row, acquires the user write lock inside an existing transaction. Never hold a database transaction during a GitHub call or a polling/singleflight wait. Future catalog/admin operations must not introduce an inverse organization→user lock order.

### Task 1: Persist and rotate credentials with a fail-closed lease

**Files:** Create `src-tauri/src/team_access/core.rs`, `credentials.rs`, `credential_store.rs`, `credentials_tests.rs`, and `test_auth.rs`. Register modules in `team_access/mod.rs`. `credential_store.rs` is a private child of credentials.rs. `test_auth.rs` is cfg(test)-only shared controlled-clock, fake-source, seeded DB and disposable-PG helpers for all three tasks; do not copy fixture infrastructure into later test files.

**Interfaces produced:**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TeamAuthError {
    #[error("authentication required")]
    Unauthorized,
    #[error("access denied")]
    Forbidden,
    #[error("authentication state changed; retry")]
    Conflict,
    #[error("authentication service unavailable")]
    Unavailable,
}
pub(crate) trait TeamClock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}
pub(crate) struct SystemClock;
impl TeamClock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> { chrono::Utc::now() }
}
pub(crate) struct AccessGrant {
    pub(crate) user_id: i32,
    pub(crate) github_user_id: i64,
    pub(crate) generation: i64,
    pub(crate) access_token: String,
}
pub(crate) struct CredentialService {
    db: sea_orm::DatabaseConnection,
    vault: std::sync::Arc<super::secrets::SecretVault>,
    oauth: std::sync::Arc<dyn super::oauth::GithubOAuthSource>,
    clock: std::sync::Arc<dyn super::core::TeamClock>,
    locks: std::sync::Mutex<std::collections::HashMap<i32,
        std::sync::Weak<tokio::sync::Mutex<()>>>>,
}
impl CredentialService {
    pub(crate) fn new(db: sea_orm::DatabaseConnection,
        vault: std::sync::Arc<super::secrets::SecretVault>,
        oauth: std::sync::Arc<dyn super::oauth::GithubOAuthSource>,
        clock: std::sync::Arc<dyn super::core::TeamClock>) -> Self;
    pub(crate) async fn access(&self, user_id: i32, github_user_id: i64)
        -> Result<AccessGrant, super::core::TeamAuthError>;
    pub(crate) async fn install_in_transaction(&self,
        transaction: &sea_orm::DatabaseTransaction,
        user_id: i32, github_user_id: i64,
        tokens: &super::oauth::GithubTokenSet,
        now: chrono::DateTime<chrono::Utc>)
        -> Result<i64, super::core::TeamAuthError>;
    pub(crate) async fn invalidate_if_generation(&self,
        user_id: i32, github_user_id: i64, generation: i64)
        -> Result<bool, super::core::TeamAuthError>;
}
```

AccessGrant and CredentialService have no Debug/Serde. Put the following shared helper in core.rs so later onboarding can acquire its lock before an organization lock:

```rust
pub(crate) async fn lock_user_identity(
    transaction: &sea_orm::DatabaseTransaction,
    user_id: i32, github_user_id: i64,
) -> Result<(), TeamAuthError>;
```

**Persistence contract:** `StoredTokens` in credentials.rs is `pub(super)` solely for the sibling onboarding vault payload; fields remain private. It derives Serialize/Deserialize but not Debug and uses integer version1, access_token, optional refresh_token/access_expires_at/refresh_expires_at. Produce `StoredTokens::from_tokens(&GithubTokenSet) -> Result<Self,TeamAuthError>` and `StoredTokens::to_tokens(&self) -> Result<GithubTokenSet,TeamAuthError>` as pub(super) validated conversions. Both validate version1, nonempty ASCII graphic token values<=4096 and a complete expiring set or all expiry/refresh fields absent; the former sets version1. These shared conversions prevent duplicated pending/credential token-shape logic. Serialize only immediately before vault sealing, never into public DTOs/errors/logs. Installation separately checks refresh expiry not already expired and rejects expired access as Unauthorized. Invalid/corrupt stored payload, identity mismatch, version mismatch, encryption and DB errors fail closed with static Unavailable; never use legacy token columns. Do not require a refresh expiry later than access expiry if the protocol did not establish that relationship.

New credential generation1; existing generation increments with checked_add. `install_in_transaction` locks the exact user first, rejects mismatched credential.github_user_id, replaces credential with active encrypted payload/no lease, and advances only this user's session and membership bindings whose credential_generation equals the previous stored generation. Clear those membership verified_at fields. Never update existing session authorization_revision, repair mismatched generations, alter membership role/state or change users.org_id/role. The caller owns commit; no token is returned before a successful outer commit.

**Refresh algorithm:** Acquire the weak keyed per-user in-process lock; prune dead weak entries when accessing the map. Load identity and credential. Active nonexpiring or access expiry>now+60seconds returns the persisted grant. Otherwise a complete unexpired refresh pair is required. Begin a short transaction, lock user, re-read current generation/state, CAS active→refreshing with random64-lowerhex owner and now+45seconds lease, then commit before calling OAuth refresh with the trusted clock. Every later persisted result must match user, GitHub ID, generation, state and owner; lease must still be live. Persist complete replacement at generation+1 with the same generation-filtered proof/session advancement before returning it. A stale result losing the CAS is discarded and current state is reloaded; never return the losing tokens.

Refreshing rows are not access grants even if the old access token is unexpired. An unexpired lease owned by another process waits with50ms bounded sleeps, re-reading state, with a20second monotonic operation wait cap; no transaction held. An expired lease, missing/expired refresh pair or any refresh error requires reconnect: conditional transition to reauth_required, payload/owner/deadline cleared, proofs for that generation cleared and sessions for that generation deleted in one transaction. Return Unauthorized after successful invalidation; on losing CAS reload instead of erasing newer login. A failed replacement persistence returns Unavailable and no token; leave the lease to expire if its outcome cannot be resolved safely. A stale401 calls `invalidate_if_generation`, which returns false for a newer generation and never changes it. Membership states/roles/local blocks are untouched by credential invalidation.

- [ ] **Step1: Write behavior tests and shared real-DB fixtures.** Seed Alice in A/B with matching and deliberately mismatched session generations, Bob and unrelated rows. Fake OAuth records refresh inputs and has deterministic barriers/replies; the clock can advance while a call is in flight. Tests cover active/nonexpiring grant,60second refresh boundary, actual vault-bound payload, complete rotated pair and generation advancement, preservation of stale auth revisions, mismatched generations and local blocks, missing/expired refresh, expired lease, ambiguous OAuth failure, failed DB commit, newer login superseding delayed refresh, and stale invalidation. Two separate CredentialService instances sharing the same DB must submit only one refresh; use a barrier, not an arbitrary timing sleep.

```rust
assert_eq!(oauth.refresh_calls(), 1);
assert_eq!(first.generation, 12);
assert_eq!(second.generation, 12);
assert_eq!(first.access_token, "invented-rotated-access");
assert_eq!(db_session_with_previous_generation.credential_generation, 12);
assert_eq!(db_session_with_previous_generation.authorization_revision, 7);
assert_eq!(db_session_with_mismatched_generation.credential_generation, 5);
assert_eq!(blocked_membership.state, "blocked");
assert_eq!(blocked_membership.verified_at, None);
```

- [ ] **Step2: Run RED** `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::credentials_tests -- --test-threads=1`. Record missing implementation and meaningful observed failure; already-passing denial-only tests do not establish guard RED.
- [ ] **Step3: Implement core, store and service**, with short atomic transactions and private serialized payload. Use chrono checked arithmetic and static errors. Test failures caused by storage races are not swallowed as successful refresh. Shared test fixtures create and migrate a real isolated SQLite DB; PG helper uses `postgres://air1@localhost:5432/postgres`, exact prefix `workos_team_auth_test_`+UUID-simple, checks current_database before dropping, and preserves failed fixtures for controller cleanup.
- [ ] **Step4: Run GREEN** credentials_tests, team_access, team_credentials, plus explicitly ignored PostgreSQL credential/concurrency tests. Assert PG replacement/invalidation atomicity and two-instance refresh, not SQL snapshots. Run git diff --check. Report exact disposable database cleanup and actual warnings.
- [ ] **Step5: Self-review and scoped commit** `feat(auth): serialize encrypted GitHub credential rotation`.

### Task 2: Complete browser-bound OAuth and atomic organization selection

**Files:** Create `src-tauri/src/team_access/onboarding.rs`, `onboarding_store.rs`, `onboarding_tests.rs`. Modify test_auth.rs only to add shared onboarding fixtures and mod.rs registration. onboarding_store is a private child of onboarding.rs. No migration change needed.

**Interfaces consumed:** CredentialService constructor/install_in_transaction, StoredTokens::from_tokens/to_tokens, lock_user_identity, TeamClock/TeamAuthError, all reviewed vault/OAuth/membership primitives. Token set has no Serde; use a separate private pending payload containing version1, validated identity fields and a nested StoredTokens value. Reuse its conversions, do not duplicate token-shape validation.

**Interfaces produced:**

```rust
pub(crate) struct OAuthAttemptStart {
    pub(crate) state: String,
    pub(crate) browser_secret: String,
    pub(crate) verifier: String,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
}
pub(crate) struct PendingStart {
    pub(crate) browser_secret: String,
    pub(crate) csrf_secret: String,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
}
pub(crate) struct SessionStart {
    pub(crate) session_secret: String,
    pub(crate) csrf_secret: String,
    pub(crate) expires_at: chrono::DateTime<chrono::Utc>,
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
}
pub(crate) struct OnboardingChoices {
    pub(crate) identity: super::github::GithubIdentity,
    pub(crate) organizations: Vec<super::github::GithubOrganizationChoice>,
}
pub(crate) struct OnboardingService {
    db: sea_orm::DatabaseConnection,
    vault: std::sync::Arc<super::secrets::SecretVault>,
    oauth: std::sync::Arc<dyn super::oauth::GithubOAuthSource>,
    github: std::sync::Arc<dyn super::github::GithubMembershipSource>,
    credentials: std::sync::Arc<super::credentials::CredentialService>,
    clock: std::sync::Arc<dyn super::core::TeamClock>,
}
impl OnboardingService {
    pub(crate) fn new(db: sea_orm::DatabaseConnection,
        vault: std::sync::Arc<super::secrets::SecretVault>,
        oauth: std::sync::Arc<dyn super::oauth::GithubOAuthSource>,
        github: std::sync::Arc<dyn super::github::GithubMembershipSource>,
        credentials: std::sync::Arc<super::credentials::CredentialService>,
        clock: std::sync::Arc<dyn super::core::TeamClock>) -> Self;
    pub(crate) async fn begin(&self) -> Result<OAuthAttemptStart, super::core::TeamAuthError>;
    pub(crate) async fn complete(&self, state: &str, browser_secret: &str, code: &str)
        -> Result<PendingStart, super::core::TeamAuthError>;
    pub(crate) async fn choices(&self, browser_secret: &str)
        -> Result<OnboardingChoices, super::core::TeamAuthError>;
    pub(crate) async fn select(&self, browser_secret: &str, csrf_secret: &str,
        github_org_id: i64) -> Result<SessionStart, super::core::TeamAuthError>;
    pub(crate) async fn cleanup_expired(&self) -> Result<(), super::core::TeamAuthError>;
}
```

No Debug/Serde on service or secret-bearing results. HTTP integration later projects only safe choice fields, redirects with configured OAuth authorization_url(state,verifier), and stores browser/session/CSRF secrets in correctly scoped HttpOnly cookies. These internal structs are not HTTP response DTOs.

**Begin/complete:** Generate independent state/browser/verifier secrets. Persist only hashed state/browser and encrypted verifier bound to both, ten-minute lifetime using clock.now(). Completing requires both validated secrets, still-live record and created_at<=now; decrypt before an atomic conditional delete requiring exactly one row. Consume before code exchange. Ambiguous exchange never retries the consumed code. Exchange, resolve identity, then encrypt pending identity+tokens bound to independent generated browser hash and immutable GitHub user ID. Store separate csrf hash and ten-minute pending expiry. Recheck token validity/time before persisting. No user/org/membership/session is created during begin/complete/choices.

**Choices:** Resolve pending by hashed browser secret, validate time and decrypt binding/payload identity. Request active_organizations from GitHub for immutable user ID; return identity+choices, including empty list or single org. Never auto-select, create personal fallback, accept invitations or treat a client login as authority. Pending ReauthenticationRequired maps Unauthorized; unavailable maps Unavailable. Cleanup deletes only expired attempts/pending records using clock.now, never sessions/credentials/legacy rows.

**Select:** Validate browser, csrf and positive orgID. Load pending, compare csrf hash, then obtain a fresh active_organizations list and find exact immutable orgID. Call verify_membership for that choice's current validated login and immutable user/org IDs; validate returned IDs/role agreement. Capture proof timestamp before the final verification request, not at response completion. If it is already60seconds old or in future at commit time, reject Unavailable. No GitHub call inside transaction.

Inside one transaction conditionally delete still-live pending matching token hash, csrf hash and GitHub ID, exactly one row. Resolve/create user by stable GitHub ID; existing users update login/name/avatar only, preserving org_id,role,email and legacy credentials. New users have compatibility role `member`, selected org_id and no legacy credentials. Resolve/create org by stable GitHub ID; refresh login only for existing rows, preserving name/avatar branding. For a new user, resolving the selected org before inserting the new identity is allowed because no existing user lock can be acquired; rollback on a concurrent uniqueness conflict and return Conflict, never reuse another transaction's unverified state. For an existing user, lock user before org. All subsequent credential updates use the same identity lock.

Membership keyed (user_id,org_id): blocked=>Forbidden with whole transaction rolled back; revoked may reactivate after fresh verified evidence and increments authorization_revision; active role changes also increment revision; unchanged active role retains revision. New membership revision1, role derived from GitHub; never infer admin from first user. Checked arithmetic. Install credentials within the same transaction, then explicitly bind selected membership to returned generation/fresh proof and insert a newly random session+csrf hash with selected member ID/revision/generation. Session lifetime30days, matching existing product login lifetime; positive proof still expires at60seconds and local checks run per request. Only after commit return raw new secrets. Existing sessions retain their organization and authorization_revision; no global tenant rewriting. Failed selection rolls back pending consumption and all other writes, allowing retry/another organization. Unique/serialization conflicts return static Conflict; other DB errors Unavailable. Do not blanket-map every DB error to conflict.

- [ ] **Step1: Write tests** for independent hashed/encrypted attempt secrets, wrong/duplicate/replayed/expired browser state, zero exchanges on invalid state, consumed code after uncertain exchange, pending-only complete, choices explicit when0/1/multiple, forged orgID/current login changes, pending invite/mismatched user/org denied, CSRF mismatch, expiration during provider wait, and selection racing local block. Test new GitHub member is not admin, existing display branding survives, same GitHub user selects A/B without rewriting users.org_id or other session org, local block survives, failed transaction leaves pending retryable, successful selection consumes once, concurrent selection has only one session, secret absence from returned error strings, exact cleanup scope.

```rust
assert_eq!(users_before_selection, 0);
assert_eq!(choices.organizations.len(), 1);
assert_eq!(sessions_before_selection, 0);
let issued = service.select(&pending.browser_secret, &pending.csrf_secret, 10201).await.unwrap();
assert_eq!(created_membership.role, "member");
assert_eq!(created_legacy_user.role, "member");
assert_eq!(created_session.org_id, issued.org_id);
assert_eq!(created_session.membership_id, created_membership.id);
assert!(service.select(&pending.browser_secret, &pending.csrf_secret, 10201).await.is_err());
```

- [ ] **Step2: Run RED** `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::onboarding_tests -- --test-threads=1`; record exact failures before implementation.
- [ ] **Step3: Implement service and private store**, using shared fixtures/core rather than legacy auth helpers. No plaintext tokens in database lookup columns or diagnostics. Selection time checks use a fresh clock.now after upstream awaits.
- [ ] **Step4: Run GREEN** onboarding_tests, team_access, team_credentials and explicit PostgreSQL selection rollback/concurrent consume tests through the existing Task1 helper. Verify disposable fixture cleanup and git diff --check.
- [ ] **Step5: Self-review and scoped commit** `feat(auth): complete verified organization onboarding atomically`.

### Task 3: Resolve externally verified principals without stale authority repair

**Files:** Create `src-tauri/src/team_access/principal.rs`, `principal_store.rs`, `principal_tests.rs`. Modify test_auth.rs for shared principal fixtures and mod.rs registration. principal_store is a private child of principal.rs. Do not change local session resolver semantics.

**Interfaces consumed:** Task1 CredentialService/AccessGrant/core and reviewed session/GitHub source. Task2 establishes consistent session/membership/credential state; principals must still reject independently malformed database rows.

**Interfaces produced:**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProofRequirement { Cached, Fresh }
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthContext {
    pub(crate) session_id: String,
    pub(crate) user_id: i32,
    pub(crate) org_id: i32,
    pub(crate) membership_id: i32,
    pub(crate) role: super::session::TeamRole,
    pub(crate) authorization_revision: i64,
    pub(crate) credential_generation: i64,
    pub(crate) verified_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Clone, Copy, Hash, Eq, PartialEq)]
struct ProofKey {
    github_user_id: i64,
    github_org_id: i64,
    membership_id: i32,
    authorization_revision: i64,
    credential_generation: i64,
}
type ProofFuture = futures_util::future::Shared<futures_util::future::BoxFuture<'static,
    Result<super::github::GithubMembershipEvidence, super::core::TeamAuthError>>>;
struct ProofFlight { future: ProofFuture }
pub(crate) struct PrincipalService {
    db: sea_orm::DatabaseConnection,
    credentials: std::sync::Arc<super::credentials::CredentialService>,
    github: std::sync::Arc<dyn super::github::GithubMembershipSource>,
    clock: std::sync::Arc<dyn super::core::TeamClock>,
    flights: std::sync::Mutex<std::collections::HashMap<ProofKey,
        std::sync::Weak<ProofFlight>>>,
}
impl PrincipalService {
    pub(crate) fn new(db: sea_orm::DatabaseConnection,
        credentials: std::sync::Arc<super::credentials::CredentialService>,
        github: std::sync::Arc<dyn super::github::GithubMembershipSource>,
        clock: std::sync::Arc<dyn super::core::TeamClock>) -> Self;
    pub(crate) async fn resolve(&self, headers: &axum::http::HeaderMap,
        requirement: ProofRequirement) -> Result<AuthContext, super::core::TeamAuthError>;
    pub(crate) async fn recheck_in_transaction(&self,
        transaction: &sea_orm::DatabaseTransaction, context: &AuthContext)
        -> Result<(), super::core::TeamAuthError>;
}
```

No Debug/Serde on PrincipalService or private flight state. No public constructor for AuthContext from arbitrary client data; fields are crate-visible internal authority, routes accept only resolver outputs.

**Resolve:** First resolve_local_session with current time, then join exact user/org/membership/credential identities. Positive local IDs and stable GitHub IDs, active credential generation>0 and matching member/session generations required. For refresh access, call CredentialService then re-resolve original cookie from DB; legitimate generation advancement may now be consistent, but changed role/revision, session removal or switched org cannot be silently repaired. Ensure new checked cookie identities equal initial user/org/member/session and its authorization_revision unchanged. Compare the grant generation to current active credential and member/session before using evidence.

Cached mode accepts only membership_freshness Fresh (0<=age<60seconds), after current local/credential checks. Fresh mode always requests new evidence unless an equivalent request is currently in flight. Key by immutable user/org IDs, local member ID, authorization revision and credential generation so unrelated identities/revisions cannot share. Use the short-held std mutex map of Weak<ProofFlight>; each consumer retains a strong Arc for its entire wait and polls a clone of the wrapper's Shared future. Retain completed proof only in persisted membership verified_at; reuse an upgraded existing flight only when its unpolled wrapper future.peek() is None, otherwise replace it so a new Fresh request cannot consume a completed future. The installed futures-util0.3 Shared::peek API was verified locally. One shared future must include the proof persistence step, not merely a result followed by duplicate writes by every waiter. Prune dead weak entries; never hold map/DB locks during network awaits. Barriers prove two concurrent Fresh consumers issue one HTTP-source request, while a subsequent Fresh call issues another.

Capture proof_started_at immediately before GitHub verify_membership. On success, short transaction rechecks exact user/org/member/session state, authorization revision and generation; local block/revocation racing the request cannot be overwritten. If role changes, increment member authorization_revision and persist role/proof, but do not rewrite sessions to new revision; original callers return Unauthorized and reauthenticate. Otherwise persist proof timestamp with CAS state/revision/generation. Do not write proof that is already60seconds old/future. Each awaiting consumer then re-resolves its own cookie and rechecks all current bindings before returning AuthContext; one consumer's valid session cannot authorize another revoked session.

NotMember conditionally transitions only the checked membership to revoked with incremented authorization_revision and null proof, preserving blocked; other org memberships unaffected. ReauthenticationRequired invokes generation-conditional credential invalidation. Unavailable does not change proof timestamps into freshness or grant access; return Unavailable. Failed CAS/state changes re-read once and fail Unauthorized/Conflict without an unbounded automatic proof retry. Never return secrets, host paths or upstream bodies in errors/context.

`recheck_in_transaction` is a no-network final mutation guard. Re-read joined session/member/user/org/credential and ensure exact identities, role, authorization_revision, generation, valid session times and current proof age<60seconds. Acquire the membership row lock in the transaction before checking so revocation cannot slip between the check and the caller's write; do not acquire user/org locks after it. This method cannot by itself satisfy a Fresh route: caller first resolves Fresh then takes its normal resource locks and calls this guard immediately before mutation. Reject invalid state with Unauthorized, storage failures Unavailable. Local revocation writers must take the same membership lock before mutation.

- [ ] **Step1: Write tests** for cached0/59seconds vs60seconds/future/missing, forced new proof, concurrent Fresh sharing and subsequent fresh call, identity/org/generation mismatch, legacy roles/token never authority, upstream timeout/rate/error fail closed, refresh invalidates cached proof, local block/revocation/logout during verification, stale401 after newer login, GitHub role change invalidates old session revision, NotMember scoped to one org, same user different active-org cookie isolation, and final transaction recheck after revision/state/generation changes. Use Alice/admin/Bob/member in A, Carol/admin B and Alice A/B cookies. Positive controls ensure rejected cases otherwise form a consistent principal.

```rust
let before = service.resolve(&alice_a_cookie, ProofRequirement::Cached).await.unwrap();
assert_eq!(before.org_id, 201);
assert_eq!(github.verification_calls(), 0);
clock.advance(chrono::Duration::seconds(60));
github.reply_unavailable();
assert_eq!(service.resolve(&alice_a_cookie, ProofRequirement::Cached).await,
    Err(TeamAuthError::Unavailable));
let stored = crate::db::entities::organization_membership::Entity::find_by_id(before.membership_id)
    .one(&db).await.unwrap().unwrap();
assert_eq!(stored.verified_at, Some(before.verified_at));
```

- [ ] **Step2: Run RED** `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::principal_tests -- --test-threads=1`; record missing/behavior failures before implementation.
- [ ] **Step3: Implement principal and private persistence module**. Common helper avoids duplicating joined authority validation between post-proof and transaction recheck. No route wiring, token bridge, actor-supplied authority, global cache or completed-proof shortcut for Fresh.
- [ ] **Step4: Run GREEN** principal_tests, team_access, team_credentials and explicit PostgreSQL principal CAS/revocation tests using shared helper; git diff --check. Verify races with deterministic synchronization and actual database state, not only fake-source call assertions.
- [ ] **Step5: Self-review and scoped commit** `feat(auth): enforce verified organization-bound request principals`.

## Controller continuation and acceptance

Task-scoped review after each implementation; combined review includes credential/login/principal races and tests. Fresh controller verification before accepting this plan. Then implement early dedicated team startup, route manifest/cookies/CSRF and safe frontend bootstrap, preserving the nested workspace UI. No disconnected-services deployment.

## Plan self-review

Covered approved identity/onboarding/fresh proof portions; HTTP/UI/privacy/jobs are explicitly subsequent, not falsely certified here. All cross-task interfaces are declared. Secret structs are deliberately nonserializable and only private payloads reach the vault. Existing membership/session tables and named state checks are reused; no legacy token promotion. Tests include dual-org identity, stale generation/revision preservation, two-process refresh and transaction rollback. Private helper modules keep persistence separate without producing duplicate fixture servers/DB harnesses. The user/org lock-order exception for creating a previously nonexistent user is explicit and concurrent insert failures roll back safely.
