# Team Principal Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist organization-specific memberships and non-transferable team sessions, and resolve a checked principal without consulting legacy global role/org authority.

**Architecture:** Add new membership/session tables without altering existing session or user rows. A pure local authorization boundary validates a cookie, its bound membership and its revision against a real database; GitHub freshness is a separate mandatory check on the returned principal. This is the first independently testable foundation slice, not a live-router conversion or a team release.

**Tech Stack:** Rust, Axum HeaderMap, SeaORM/SeaQuery, SQLite and PostgreSQL, existing chrono/sha2 dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`

## Global Constraints

- Existing chats remain private. New chats default to private, with the audience visible before creation.
- No two organizations may share application-visible chat content, files, credentials, searches, live events, presence, execution authority or wiki inputs accidentally.
- Existing `users.org_id` and `users.role` fields remain compatibility data while legacy code is isolated; team authorization must never use them as its source of truth.
- A local administrator's block survives OAuth login and membership refresh; only an authorized administrator can lift it.
- A positive GitHub membership proof may be cached for at most 60 seconds, keyed by stable user, organization and credential generation.
- Old sessions that have no organization binding must reauthenticate/select their organization.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- Do not change the source origin, rename the branch, push, deploy, call real GitHub/provider services, or touch production data in these tasks.
- Use existing dependencies. Run one Cargo process at a time with `CARGO_INCREMENTAL=0`; local disk was previously exhausted by incremental cache growth.

## Scope coverage and interfaces

This plan implements the persistent membership/session identity and local principal checks from spec sections 1–2. OAuth onboarding, GitHub verification/cache renewal, route separation/CSRF, visibility UI/service, scoped events/files, isolated runners and jobs are subsequent plans. No live team request may be wired to this local-only resolver before a GitHub verification orchestrator exists; the type names distinguish local identity from fully authorized identity.

Use a separate `team_session` table/cookie instead of adding fields to legacy `sessions`; both old rows and existing desktop tests remain compatible. `workos_team_session` contains an opaque 64-character lowercase hex secret; only its SHA-256 digest is persisted as `token_hash`. Session creation/rotation and random-secret generation belong to the onboarding slice. This slice implements lookup and revocation, never issues sessions to real users.

Files:

- `src-tauri/src/db/migration/m20260908_000001_team_access.rs`: additive schema and safe down behavior.
- `src-tauri/src/db/migration/team_access_tests.rs`: isolated migration/constraint tests.
- `src-tauri/src/db/entities/organization_membership.rs`, `team_session.rs`: storage models, no serialized secret fields.
- `src-tauri/src/team_access/mod.rs`: local-principal types and bounded public interface.
- `src-tauri/src/team_access/session.rs`: cookie parsing, hashing and real-DB local resolution/revocation.
- `src-tauri/src/team_access/session_tests.rs`: cross-org/role/expiry/revocation regression tests.
- Existing migration/entity registries and `lib.rs`: module registration only.

### Task 1: Add membership and team-session storage without granting legacy access

**Files:**
- Create: `src-tauri/src/db/migration/m20260908_000001_team_access.rs`
- Create: `src-tauri/src/db/migration/team_access_tests.rs`
- Create: `src-tauri/src/db/entities/organization_membership.rs`
- Create: `src-tauri/src/db/entities/team_session.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`, `src-tauri/src/db/entities/mod.rs`

**Interfaces:**
- Consumes: existing `orgs`, `users`, `sessions`, `Migrator` and `db::test_helpers::fresh_in_memory_db`.
- Produces: `organization_membership::Model` and `team_session::Model` with exactly the schema below. Types use `i32` IDs, `i64` revisions/generations, `DateTimeUtc` timestamps, `String` text and `Option` for nullable proof fields. Entity models must not derive Serialize or Deserialize.

Schema:

```text
organization_membership
  id INTEGER auto primary key
  org_id INTEGER NOT NULL
  user_id INTEGER NOT NULL
  role TEXT NOT NULL CHECK role IN ('admin','member')
  state TEXT NOT NULL CHECK state IN ('active','revoked','blocked')
  authorization_revision BIGINT NOT NULL DEFAULT 0 CHECK >= 0
  credential_generation BIGINT NOT NULL DEFAULT 0 CHECK >= 0
  verified_at TIMESTAMPTZ NULL
  created_at TIMESTAMPTZ NOT NULL
  updated_at TIMESTAMPTZ NOT NULL
  UNIQUE(org_id,user_id)

team_session
  token_hash TEXT primary key
  user_id INTEGER NOT NULL
  org_id INTEGER NOT NULL
  membership_id INTEGER NOT NULL
  authorization_revision BIGINT NOT NULL CHECK >= 0
  credential_generation BIGINT NOT NULL CHECK >= 0
  csrf_hash TEXT NOT NULL
  created_at TIMESTAMPTZ NOT NULL
  expires_at TIMESTAMPTZ NOT NULL
  INDEX(membership_id)
  INDEX(expires_at)
```

Match the repository's app-enforced-reference convention; Task 2 explicitly verifies referenced user/org/membership rows and the full composite identity. Do not silently create an active membership from a legacy owner row. Schema defaults do not grant role/state: both are explicit on insert.

- [ ] **Step 1: Write behavioral migration tests first.** Register a test module; run prior migrations, seed an existing organization, legacy owner and legacy session, then apply the new migration. Assert the legacy rows are unchanged and both new tables are empty. Add real insert tests for duplicate `(org_id,user_id)`, bad role/state and negative revisions/generations. Verify the same user can have different memberships in two organizations and two bound sessions. The fail case is a migration that promotes legacy rows or omits a constraint.

```rust
assert_eq!(organization_membership::Entity::find().count(&db).await.unwrap(), 0);
assert_eq!(team_session::Entity::find().count(&db).await.unwrap(), 0);
assert_eq!(user::Entity::find_by_id(original.id).one(&db).await.unwrap().unwrap(), original);
assert!(duplicate_membership.insert(&db).await.is_err());
assert!(invalid_role.insert(&db).await.is_err());
```

Use complete real fixtures with manually specified IDs/roles and literal expected values. Add a rerun preservation test and a down test: populated new tables must cause down to refuse without deleting anything; empty new tables can be removed. Cross-backend schema-builder tests exercise SQLite and Postgres generated statements; run an isolated Postgres migration round-trip when local role air1 at localhost:5432 is available, using a generated exact-name test DB and the existing cleanup-guard pattern.

- [ ] **Step 2: Run the new test selection and observe RED.**

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access_migration -- --test-threads=1
```

Expected: missing new module/table/model or failed table/constraint assertions attributable to the absent migration, not unrelated compiler errors. Record the actual output in the task report.

- [ ] **Step 3: Implement schema and models.** Use `SchemaManager` and `Table::create().if_not_exists()` with `ColumnDef` definitions from the schema above, named `Expr` check constraints and named indexes. Register the migration after `m20260907_000004_workspace_repository_names`. Entity names follow SeaORM conventions:

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "organization_membership")]
pub struct Model {
    #[sea_orm(primary_key)] pub id: i32,
    pub org_id: i32,
    pub user_id: i32,
    pub role: String,
    pub state: String,
    pub authorization_revision: i64,
    pub credential_generation: i64,
    pub verified_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}
```

Use a string non-autoincrement primary key on `team_session.token_hash` and mirror the remaining schema fields directly. No entity relation is treated as authorization. The down operation checks that both tables are empty before dropping either one, returns `DbErr::Custom` otherwise, and never touches legacy tables. Keep schema-building helpers accessible to sibling migration tests without exporting them beyond the crate.

- [ ] **Step 4: Verify GREEN and compatibility.** Run the migration selection, organization settings tests and workspace catalog tests. Confirm no new memberships/sessions appear from migration alone and record test counts. Inspect migration SQL for both database backends and run the explicit local Postgres round-trip if available. No production DB tests.

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access_migration -- --test-threads=1
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib organization_settings_tests -- --test-threads=1
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib workspace_catalog_tests -- --test-threads=1
git diff --check
```

- [ ] **Step 5: Self-review and commit only Task 1 files.**

```bash
git add src-tauri/src/db/migration/m20260908_000001_team_access.rs src-tauri/src/db/migration/team_access_tests.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/organization_membership.rs src-tauri/src/db/entities/team_session.rs src-tauri/src/db/entities/mod.rs
git commit -m "feat(auth): add organization memberships and scoped session storage"
```

### Task 2: Resolve local principals and invalidate stale sessions safely

**Files:**
- Create: `src-tauri/src/team_access/mod.rs`
- Create: `src-tauri/src/team_access/session.rs`
- Create: `src-tauri/src/team_access/session_tests.rs`
- Modify: `src-tauri/src/lib.rs` (module registration only)

**Interfaces:**
- Consumes: Task 1 models and existing org/user entities.
- Produces the following crate-internal functions/types (no HTTP routes yet):

```rust
pub(crate) enum TeamRole { Admin, Member }
pub(crate) enum LocalSessionError { Unauthorized, Unavailable }
pub(crate) enum MembershipFreshness { Fresh, ReverificationRequired }
pub(crate) struct LocalPrincipal {
    pub session_id: String, // token hash, not the browser's secret
    pub user_id: i32,
    pub org_id: i32,
    pub membership_id: i32,
    pub role: TeamRole,
    pub authorization_revision: i64,
    pub credential_generation: i64,
    pub verified_at: Option<chrono::DateTime<chrono::Utc>>,
}
pub(crate) async fn resolve_local_session(
    db: &sea_orm::DatabaseConnection,
    headers: &axum::http::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<LocalPrincipal, LocalSessionError>;
pub(crate) fn membership_freshness(
    principal: &LocalPrincipal,
    now: chrono::DateTime<chrono::Utc>,
) -> MembershipFreshness;
pub(crate) async fn revoke_local_session(
    db: &sea_orm::DatabaseConnection,
    principal: &LocalPrincipal,
) -> Result<(), LocalSessionError>;
```

The `LocalPrincipal` is deliberately not a fully authorized request principal. Its docs must require the later GitHub verifier and route/resource policy. Never derive Deserialize for it or expose an external constructor. `TeamRole`, errors and freshness derive equality/debug for real outcome tests; errors have static display text without DB internals.

- [ ] **Step 1: Write real-DB regression tests.** Seed Alice in two organizations with distinct roles and sessions; intentionally leave legacy `users.org_id/role` pointing to org A/admin. A cookie bound to org B/member must resolve B/member. Bob/Carol in separate orgs must not alter this result. The production bug caught is reading the legacy user org/role, accepting a mismatched membership, ignoring a session revision or accidentally using an admin token.

```rust
let principal = resolve_local_session(&db, &alice_b_headers, now).await.unwrap();
assert_eq!(principal.org_id, 202);
assert_eq!(principal.role, TeamRole::Member);
assert_eq!(principal.user_id, 301);
assert_eq!(membership_freshness(&principal, now), MembershipFreshness::Fresh);
assert!(matches!(resolve_local_session(&db, &bearer_only, now).await,
    Err(LocalSessionError::Unauthorized)));
```

Cover absent/malformed cookie, duplicate named cookie within one header/across multiple headers, unrelated cookies, a legacy `workos_session` only, bearer only, an expired/future-created session, missing membership/org/user, mismatched org/user/membership ID, revoked/blocked membership, membership revision mismatch, credential generation mismatch, an unknown role, negative revisions, and a closed database. Use real database row updates/deletes and assert results, not source strings or mocked calls. To model corrupt role/revision rows despite Task 1 constraints, a dedicated in-memory SQLite fixture may temporarily use `PRAGMA ignore_check_constraints=ON` for the corrupt write and immediately restore it; keep this fault injection entirely in test code and isolate that database from other cases.

For freshness, use a controlled clock and literal ages 0, 59, 60, 61 seconds plus missing/future proof. At exactly 60 seconds, require revalidation. Revocation deletes only the session described by the principal: a second org-bound session survives and the revoked cookie fails next resolution.

- [ ] **Step 2: Run and observe RED.**

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::session_tests -- --test-threads=1
```

The missing resolver/module is the initial compilation RED; after interfaces exist, individual behavioral failures must be recorded before adding each check. Do not let an unrelated compilation failure substitute for a regression proof.

- [ ] **Step 3: Implement the local identity checks and freshness predicate.** Parse all Cookie header values, splitting pairs with `split_once('=')`; accept exactly one `workos_team_session` value, exactly 64 lowercase hex characters. Reject malformed named cookies and ambiguous/non-UTF8 headers. Never accept an operator Bearer token as a substitute. Hash only the validated cookie value with existing sha2; never log it.

```rust
let digest = format!("{:x}", sha2::Sha256::digest(raw_cookie.as_bytes()));
let session = team_session::Entity::find_by_id(digest).one(db).await?;
// Map storage errors to Unavailable; absent/invalid identity to Unauthorized.
```

Load the membership referenced by the session and require matching `user_id`, `org_id`, state `active`, equal nonnegative authorization revision and credential generation, and an exact allowed role. Require both referenced user and org to exist. Require `created_at <= now && now < expires_at`. Read the full identity graph in one parameterized joined SQL statement (or one SeaQuery-built joined SELECT), so default PostgreSQL READ COMMITTED cannot combine multiple snapshots. Join the session membership ID to membership, then the referenced user/org IDs to their entities, and validate the session/membership composite identity and revisions from that same result. Do not rely on several SELECTs merely wrapped in a default transaction. Local resolution never renews a session or membership.

```rust
match principal.verified_at {
    Some(at) if at <= now && now.signed_duration_since(at) < chrono::Duration::seconds(60)
        => MembershipFreshness::Fresh,
    _ => MembershipFreshness::ReverificationRequired,
}
```

`revoke_local_session` deletes only a row matching session hash, user, org and membership from the checked principal; it is idempotent. Do not delete other devices' sessions or another organization's memberships. A later route must add CSRF and external verification/revocation notifications before exposing it.

- [ ] **Step 4: Verify GREEN plus storage compatibility.**

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access -- --test-threads=1
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib oauth_state_tests -- --test-threads=1
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib workspace_catalog_tests -- --test-threads=1
git diff --check
```

Confirm the local resolver is not wired to existing live routes and the pilot runtime flag is unchanged. This is a deliberate foundational library deliverable; its tests do not prove deployed isolation or GitHub membership freshness.

- [ ] **Step 5: Self-review and commit only Task 2 files.**

```bash
git add src-tauri/src/team_access src-tauri/src/lib.rs
git commit -m "feat(auth): resolve organization-bound local session principals"
```

## Controller verification and handoff

- [ ] Independently review each task for spec compliance and code quality, then review the combined foundation diff.
- [ ] Rerun the final foundation tests and existing identity/catalog selections; verify source worktree contains only intended changes.
- [ ] Record actual RED/GREEN commands/counts and any ignored Postgres checks. A passed schema test is not a live team-authorization claim.
- [ ] Continue with a separate plan for the GitHub verification/onboarding orchestrator and team route/transport boundary, consuming these exact storage and principal interfaces. Do not deploy or expose a partial local-only authorization chain.

## Plan self-review

Spec coverage is intentionally limited to the foundation described under Scope coverage. The next plan owns real GitHub proof acquisition and the remaining sections listed there; isolation/visibility have not been silently declared complete. The two tasks share only the Task 1 entity models; Task 2 does not modify Task 1's schema. No production secret or external permission is required for either task. All new persistence is additive, empty on migration, and inert until the approved higher-level boundary is implemented.
