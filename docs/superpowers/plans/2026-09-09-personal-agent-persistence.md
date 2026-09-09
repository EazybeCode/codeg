# Personal Agent Connection Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add metadata-only personal connection, login-attempt and idempotent operation storage with tested SQLite/PostgreSQL constraints and safe rollback.

**Architecture:** New SeaORM entities and one additive migration represent ownership and generation-bound lifecycle intent without storing provider credentials. No HTTP route, provider process or runtime is enabled by this foundation.

**Tech Stack:** Existing Rust 2021, SeaORM/SeaQuery, SQLite, PostgreSQL, chrono and UUID dependencies.

**Spec:** `docs/superpowers/specs/2026-09-09-personal-agent-connections-design.md` (owner approved).

## Global Constraints

- Every member uses their own subscription for Claude, Codex or another supported agent.
- Sharing a chat never grants use of another person's connection.
- Database records, telemetry, app logs, browser storage, screenshots, backups of ordinary app data and Git must never contain provider credentials or login codes.
- No state is derived solely from the existence of a credential file.
- Each start/cancel/disconnect changes a monotonic generation.
- No legacy AppState, agent dispatcher, provider process, shared host login, new route or runtime enablement.
- Add no dependency; preserve native/operator behavior and unrelated edits. All database fixtures are invented and isolated. No live database/cloud/provider/GitHub access, push or deployment.
- One Cargo process, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`; stop owned compilation before available disk approaches 2 GiB. Do not install dependencies or delete unrelated data to make room.

## Delivery coverage

The approved connection subsystem is executed in consecutive testable plans:

1. This plan: storage, ownership constraints, generation/lease metadata, migration compatibility.
2. Connection orchestration: checked principals, transition/idempotency/quota rules, late-event rejection, restart and disconnect recovery.
3. Isolated supervisor and official-client adapters: fixed protocol, custody, network/resource policies and local synthetic acceptance.
4. Team HTTP/onboarding/settings UI: typed projections, official login challenges, invalidation and actual-browser acceptance.
5. Private hosted connection acceptance: gated on owner provider terms, encrypted credential custody and actual participant sign-in.

This is not a claim that the later stages or full team workspace are implemented.
Repo-backed prompt execution remains a separate approved-design boundary. Root owns
the delivery plans/spec/status docs; workers own only their task source and report.

### Task 1: Add constrained personal-agent metadata storage

**Files:**
- Create `src-tauri/src/db/entities/team_agent_connection.rs`.
- Create `src-tauri/src/db/entities/team_agent_attempt.rs`.
- Create `src-tauri/src/db/entities/team_agent_operation.rs`.
- Create `src-tauri/src/db/migration/m20260909_000001_personal_agent_connections.rs`.
- Create `src-tauri/src/db/migration/personal_agent_connections_tests.rs`.
- Modify `src-tauri/src/db/entities/mod.rs` and `src-tauri/src/db/migration/mod.rs` for registration only.
- Modify `src-tauri/src/db/migration/team_credentials_tests.rs` only to remove the existing positional-down assumption now that credentials is no longer the last migration.

**Interfaces:** SeaORM entities in `crate::db::entities::{team_agent_connection, team_agent_attempt, team_agent_operation}`. Each has `Model`, `ActiveModel`, empty `Relation` and `ActiveModelBehavior`, following the existing team entities; no Serialize/Deserialize on persistence models. The migration exposes `Migration` and crate-local table/index statement helpers matching existing migration conventions. No new public runtime/service methods.

- [ ] **Step 1: Add behavioral database tests before the migration.** Register only the test module initially. Use actual SQL so the first failing test builds before the entities exist; query the new table after full `Migrator::up` and require a usable empty table. The intended RED is missing-table SQL failure, not a missing Rust import. Example core assertion:

```rust
let db = Database::connect("sqlite::memory:").await.unwrap();
Migrator::up(&db, None).await.unwrap();
let rows = db.query_all(Statement::from_string(
    DbBackend::Sqlite,
    "SELECT id FROM team_agent_connection".to_owned(),
)).await.expect("personal agent metadata table must be usable");
assert!(rows.is_empty());
```

Run from `src-tauri`:
`CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features personal_agent_connections_tests -- --test-threads=1`.
Confirm the missing-table failure, then implement the schema below. No synthetic
failure on a previously working feature and no source-text tests.

- [ ] **Step 2: Add the three entities with exactly these metadata fields.** All IDs and operation keys are non-auto-increment Text. All time values are `DateTimeUtc` / `timestamp_with_time_zone`; nullable values use `Option`. All string columns use Text. No credential/code/URL/raw-error/payload/JSON column.

```rust
// team_agent_connection::Model
pub id: String,
pub org_id: i32,
pub user_id: i32,
pub provider: String,
pub generation: i64,
pub state: String,
pub account_label: Option<String>,
pub checked_at: Option<DateTimeUtc>,
pub error_code: Option<String>,
pub storage_handle: Option<String>,
pub created_at: DateTimeUtc,
pub updated_at: DateTimeUtc,

// team_agent_attempt::Model
pub id: String,
pub connection_id: String,
pub generation: i64,
pub session_id: String,
pub state: String,
pub created_at: DateTimeUtc,
pub expires_at: DateTimeUtc,
pub updated_at: DateTimeUtc,

// team_agent_operation::Model
pub id: String,
pub connection_id: String,
pub kind: String,
pub request_key: String,
pub target_attempt_id: Option<String>,
pub target_generation: i64,
pub result_generation: i64,
pub state: String,
pub lease_owner: Option<String>,
pub lease_expires_at: Option<DateTimeUtc>,
pub error_code: Option<String>,
pub created_at: DateTimeUtc,
pub expires_at: DateTimeUtc,
pub updated_at: DateTimeUtc,
```

The operation is the durable idempotency/result and recovery intent. Its target
generation identifies the resource to start/revoke; result generation identifies
the state mutation returned to an HTTP retry. Keeping both avoids losing cleanup
identity when cancel/disconnect advances the connection generation. No provider
challenge is persisted. Retain attempts after logout: session_id is an opaque
binding, NOT a cascading FK to team_session. Authorization is the next plan's job.

- [ ] **Step 3: Implement additive migration and constraints.** Use the existing
SeaQuery named-check/foreign-key/index pattern. IDs/handles/lease owners/request
keys have length 1..64; session_id length 1..128. No defaults silently fill ownership,
provider, generation or state. Foreign keys use RESTRICT, not cascade: connection
org_id→org.id and user_id→user.id; attempt/operation connection_id→connection.id.
Do not delete provider cleanup intent through a user/org/session cascade.

Connection checks: org_id/user_id/generation > 0; provider in `claude`,`codex`;
state in `disconnected`,`connecting`,`connected`,`reconnect_required`,
`disconnecting`,`unavailable`; optional account_label length 1..160;
optional error_code is one of `provider_unavailable`,`reauth_required`,
`unsupported_account`,`expired`,`canceled`,`cleanup_failed`,`interrupted`.
Connected requires non-null account_label, checked_at and storage_handle and null
error_code. Disconnected requires all four optional status/storage fields null.
Other states can retain bounded cleanup metadata; later service projections decide
what can be shown. updated_at >= created_at.

Attempt checks: generation > 0; state in `pending`,`completed`,`canceled`,`expired`,
`failed`; expires_at > created_at and updated_at >= created_at. Unique
`(connection_id,generation)` prevents duplicate attempts for one lifecycle generation.

Operation checks: generations > 0 and result_generation >= target_generation;
kind in `connect`,`cancel`,`disconnect`; state in `queued`,`leased`,`completed`,
`abandoned`; expires_at > created_at and updated_at >= created_at. Leased requires
both lease_owner and lease_expires_at; other states require both null. Connect and
cancel require target_attempt_id; disconnect forbids it. Optional error_code uses
the connection's fixed allowlist. target_attempt_id is NOT a single-column FK:
cross-connection attempt binding is checked by the next service plan in a locked
transaction (a plain FK would not establish ownership). It is bounded text here.

Unique connection `(org_id,user_id,provider)`; unique operation
`(connection_id,kind,request_key)`. Add lookup indexes on attempt `(state,expires_at)`
and operation `(state,lease_expires_at)` for expiry/recovery. Name indexes and checks
with `idx_team_agent_*` / `ck_team_agent_*`. Use portable SQL comparisons and
length expressions for both SQLite and PostgreSQL; test null handling explicitly.
No seed/backfill from legacy agents or credentials.

Migration up uses if-not-exists table/index creation and preserves data on rerun.
Down checks ALL three tables for any row before dropping ANY table, using scalar
historical-column queries (not latest entity model projections). If populated,
return a fixed credential-free error. On empty schema drop operations, attempts,
connections only. Do not change older migrations or drop retained user/org data.

- [ ] **Step 4: Complete the actual-database regression matrix.** Use literal
invented UUIDs, organizations, members and timestamps. Create valid parents, then
assert insert/update outcomes through SQL/SeaORM, not rendered SQL snapshots.

Test: fresh full chain; pre-migration legacy identity/data survives; empty tables
after migration; valid rows each state; duplicates rejected in one scope; same user
in another org and another member in same org allowed; invalid ownership/FKs,
provider/state/generation/bounds, connected/disconnected shapes, attempt uniqueness,
lease null combinations, operation kind/target shape and timestamp ordering rejected.
Verify a failed write leaves prior row unchanged. Verify idempotency keys can be
reused in a different connection/kind but not duplicated in the same scope.
These are storage constraints, NOT proof of authenticated service isolation.

Test populated down refusal separately for each table, with all tables and parent
rows preserved; data-bearing up rerun retains rows; empty down removes only new
tables, up restores them, and complete empty rollback of these three tables works.
Task2 below separately restores and verifies full-chain historical rollback. Name the target
migration instead of assuming it is the last entry. In team_credentials_tests,
invoke the named CredentialsMigration down for its own round-trip check and assert
its tables are absent; do not let a new final migration silently change the test's
meaning. Never use a latest-model projection against a historical schema.

Mirror critical constraints/rerun/empty rollback on guarded disposable PostgreSQL:
fixed local admin URL `postgres://air1@localhost:5432/postgres`, unique database
prefix `workos_personal_agents_test_` plus UUID simple hex. Require exact equality
between generated and connected database names plus valid prefix/hex before any
DROP; preserve the database on test failure. Reuse the existing pattern locally
in the new test module; no global fixture refactor. Never target configured app DB.

- [ ] **Step 5: Verify and commit the scoped change.** Run all new SQLite tests,
the new explicitly ignored guarded PostgreSQL test, and existing team_credentials
and team_access migration modules with `--include-ignored --test-threads=1`, one
Cargo process at a time. Run `git diff --check`. Do not repeat frontend/build/browser
suites for this storage-only task. Report exact RED/GREEN commands and outputs,
existing warnings, migrations tested, and no live rollout claim. Commit only the
listed source/test files; exclude root-owned plan/spec/status documents.

### Task 2: Fix the historical catalog rollback guard exposed by full-chain testing

**Files:** modify `src-tauri/src/db/migration/m20260907_000001_workspace_catalog.rs`
only its `down` row-presence guard; extend
`src-tauri/src/db/migration/personal_agent_connections_tests.rs` with full-chain
rollback and historical guard regressions. No other migration/runtime changes.

**Interfaces:** consume registered Migrator and named catalog `Migration`, plus
the guarded PostgreSQL fixture already in the test module. Keep its exact-name
database cleanup protections. Preserve the existing catalog error message and
drop order. This task is the explicit narrow exception to Task1's prohibition on
changing older migrations; do not change the repository-name migration.

- [ ] **Step 1: Reproduce full empty rollback before fixing the guard.** On a
fresh SQLite connection, execute the complete registered up/down chain:

```rust
let db = Database::connect("sqlite::memory:").await.unwrap();
Migrator::up(&db, None).await.unwrap();
Migrator::down(&db, None).await.expect("complete empty migration chain is reversible");
assert!(Migrator::get_applied_migrations(&db).await.unwrap().is_empty());
```

Run just this new test under the existing single-Cargo controls. Expected RED:
`no such column: workspace_repository.verified_names` (driver rendering may omit
the table qualifier). The later names migration already dropped that column;
the earlier catalog guard incorrectly selects the current full entity model.
This defect predates Task1 and was discovered by its broader rollback test.

- [ ] **Step 2: Replace only the historical presence guard.** Do not project
current entity models in this historical migration. Use fixed scalar SQL:

```rust
use sea_orm::{ConnectionTrait, Statement};
let conn = manager.get_connection();
for table in ["workspace_chat_draft", "workspace_node", "workspace_repository"] {
    let present = conn.query_one(Statement::from_string(
        conn.get_database_backend(), format!("SELECT id FROM {table} LIMIT 1"),
    )).await?.is_some();
    if present {
        return Err(DbErr::Custom(
            "workspace catalog migration cannot be reverted while catalog rows exist".into(),
        ));
    }
}
```

The table names are this fixed compile-time list, not request/configuration input.
All presence checks remain before any drop. Preserve down's three table drops
unchanged. No up/backfill/schema/authorization behavior is changed.

- [ ] **Step 3: Verify both directions and data safety.** Extend the SQLite
full-chain test: after down, assert no applied migrations and no catalog/personal
tables; run full up again and require usable empty personal tables. Add a focused
historical fixture containing only an `id` column in each of the three catalog
tables, plus an unrelated sentinel table/row. For each catalog table independently,
insert one invented ID and require the original fixed refusal error with all
tables/rows intact; the empty version drops only the three catalog tables and
preserves the sentinel. These are actual SQL fixtures, not SQL-string snapshots.

Extend the existing ignored PostgreSQL test after its scoped personal rollback:
recreate the personal schema with named up, remove only that disposable fixture's
invented parent rows, run full Migrator::down(None), assert applied migrations
empty and named catalog/personal tables absent, then full up and usable empty
personal tables. Keep the generated/connected database-name equality and hex-prefix
guard before DROP; preserve any failure database. Do not duplicate the whole
PostgreSQL setup/cleanup block or touch a configured application database.

- [ ] **Step 4: Run and commit the focused fix.** Run all
`personal_agent_connections_tests` with `--include-ignored --test-threads=1` plus
`git diff --check`. Verify normal readable Rust formatting manually if rustfmt is
unavailable; no toolchain/dependency install. Record RED/GREEN exact commands and
results. Commit only the two listed files and release Cargo/index ownership.

## Plan self-review

Storage/ownership/lifecycle metadata and historical migration safety map to Task1.
Task2 fixes the single historical-model guard identified by the failing full-chain
test and restores full empty-chain coverage. It shares only the regression test
module with Task1; implementation is sequential. Service authorization, provider protocols/custody, HTTP/UI and real deployment are
explicit subsequent plans in the coverage sequence, not claimed by this task.
All entities, columns, state spellings, indexes and verification commands consumed
by this task are specified above. No shared source is assigned to another worker.
