# Chat Privacy Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist explicit private/organization chat audience and immutable creation-audience metadata, preserving every existing chat's identity and retry format without exposing team workspace routes.

**Architecture:** Add constrained columns to the existing `workspace_chat_draft` table, then extract its existing pure request fingerprint algorithm and add an explicitly versioned audience-aware format. Legacy/native creation remains private and uses its original format; a subsequent reviewed team catalog service must atomically create version2 metadata under verified authority.

**Tech Stack:** Existing Rust, SeaORM/sea-query, SQLite/PostgreSQL and sha2. No new dependency, network operation, credential access or filesystem/Git mutation.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`, sections3,5,6. The written team design is owner-approved. This is its persistence/retry slice, not complete resource authorization or product acceptance.

Status: prepared September10 while organization settings is implemented. Do not execute concurrently with that plan's source/index/Cargo owner. Complete settings acceptance first. No privacy implementation or migration has run from this document.

## Global Constraints

- Organization is the company workspace, not a project. Preserve organization → projects/categories → arbitrarily nested folders/working directories → repository-backed chats.
- Existing chats remain private. New chats default to private, with the audience visible before creation. Being inside a shared project/folder does not silently broaden a chat's audience.
- No protected operation accepts a client-supplied user ID, role or organization ID as authority.
- No two organizations may share application-visible chat content, files, credentials, searches, live events, presence, execution authority or wiki inputs accidentally.
- Backfill every existing managed chat to private without changing its creator, repository snapshot, branch/worktree or transcript mapping.
- Records lacking a proven tenant/creator mapping remain quarantined from team routes; do not infer ownership from a filesystem path or expose legacy rows as shared.
- Version creation fingerprints so legacy private requests still retry against their original format. The original creation audience is immutable request metadata, distinct from current visibility: retrying creation never resets an audience changed later through Share.
- Never automatically push a private chat's transcript, generated Markdown or unpublished files, or feed them into the organization wiki.
- Existing desktop/native mode stays separate for compatibility. Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No team route, provider, job, event/file adapter, legacy authentication change, cloud write, push or deployment in these tasks. No actual GitHub data is deleted or changed.
- One Cargo process at a time, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`; stop before available disk approaches2GiB. Use existing guarded disposable SQLite/PostgreSQL fixtures, preserve failures, and never log entity models or secrets.

## Source preflight and interfaces

`workspace_chat_draft` already owns `id:i32`, `org_id:i32`, `node_id:i32`, `creator_user_id:i32`, `repository_id:i32`, string GitHub repository ID/full name/starting branch, title, request ID/fingerprint, execution state and timestamps. IDs are not UUIDs; request IDs are UUIDs. `workspace_chat_execution` and writer leases reference the draft ID. Keep these links and rows untouched.

`workspace_catalog/draft_service.rs::normalize_input` normalizes the UUID/title and computes an unversioned SHA256 over node ID, optional title and optional repository override. Its exact encoding is the v1 format below. Its creator-private legacy list predicate remains unchanged; neither `verify_scope` nor `begin_scoped_write` is valid team authority because they use `users.org_id`.

The old catalog migration does not enforce foreign-key ownership relationships. Adding audience columns cannot prove any old row's tenant/creator mapping. This plan deliberately makes no admission decision and mounts no team catalog route. The subsequent resource-authorizer must verify those relationships under the reviewed principal and deny uncertain records before any title, count, file or event is exposed. No new quarantine-release or owner-reassignment mechanism is introduced here.

### Task 1: Add constrained audience and immutable creation metadata

**Files:** Create `src-tauri/src/db/migration/m20260910_000001_chat_privacy.rs` and `src-tauri/src/db/migration/chat_privacy_tests.rs`. Register both in `db/migration/mod.rs`. Add the four fields below to `db/entities/workspace_chat_draft.rs`. Update only the explicit legacy insert in `workspace_catalog/draft_service.rs` and its direct fixture insert in `web/handlers/workspace_catalog_tests.rs` to supply private/v1 defaults. If compilation identifies another exhaustive draft model constructor, update only those same four defaults and name the site in the report. Do not change legacy DTO serialization or other entity fields/derives.

**Produces:**

```rust
// Additional workspace_chat_draft::Model fields; existing fields unchanged.
#[sea_orm(column_type = "Text")]
pub visibility: String,                 // private | organization
pub access_revision: i64,               // >= 0, initially 0
#[sea_orm(column_type = "Text")]
pub creation_visibility: String,        // immutable request audience
pub creation_fingerprint_version: i32,  // 1 | 2
```

Each new column is NOT NULL. Defaults are respectively `private`,0,`private`,1. Database checks enforce both audience allowlists, a nonnegative i64 revision, version1|2 and version1 implying private creation audience. Use SQLite integer storage checks where necessary to prevent a fractional/text revision passing only an arithmetic check; PostgreSQL uses its bigint type. Do not constrain current visibility to original creation visibility: a private-created chat may later be explicitly shared. Do not relabel old request fingerprints as version2 or recompute them during migration.

Add an index on `(org_id, visibility, creator_user_id, id)` for the later authorization predicate. Use the existing row's authoritative columns; no duplicate creator/org fields or new cross-org move feature. Fresh migration and upgrade from the preceding schema must produce identical constraints/defaults. Use supported per-column ALTER statements for SQLite rather than a multi-action ALTER unsupported by its builder. Backfill is the constant defaults only; it never grants access, infers ownership or rewrites existing data.

- [ ] **Step1: Add failing real migration tests.** Build the schema through the immediately preceding personal-agent migration, insert two organizations' existing hierarchy/draft/execution mappings using synthetic values, record selected safe scalar snapshots, and run the new migration. Assert new defaults and every old identity/repository/branch/request/execution link unchanged. Cover an orphan creator, foreign repository/node relationship and cyclic node hierarchy: these remain unchanged/private, not repaired or admitted by migration. Do not deserialize/log full user or credential entities.

```rust
// Query only audience metadata by the known synthetic draft ID.
let row = conn.query_one(Statement::from_sql_and_values(
    conn.get_database_backend(),
    "SELECT visibility, access_revision, creation_visibility, creation_fingerprint_version FROM workspace_chat_draft WHERE id = $1",
    [draft_id.into()],
)).await?.expect("existing draft");
assert_eq!(row.try_get::<String>("", "visibility")?, "private");
assert_eq!(row.try_get::<i64>("", "access_revision")?, 0);
assert_eq!(row.try_get::<String>("", "creation_visibility")?, "private");
assert_eq!(row.try_get::<i32>("", "creation_fingerprint_version")?, 1);
```

Also test invalid/null audiences, negative/overflow revisions, unsupported versions and v1 with organization creation audience are rejected. On SQLite, explicitly reject stored fractional/text revisions using the storage-type check. On PostgreSQL, verify bigint storage and rejection of malformed integer input/overflow; do not assert that an explicit numeric-to-bigint cast must reject a fraction. PostgreSQL may round that expression before the column sees it. Future HTTP expected-revision inputs must independently require integer JSON values without coercion. Valid v2 private/organization insertions succeed; version1/private creation with current organization visibility succeeds, preserving future Share semantics. Test current visibility updates do not implicitly alter creation metadata; immutability is a service contract and these tests must not pretend a general SQL user cannot rewrite it.

- [ ] **Step2: Run RED.** From src-tauri, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features chat_privacy_tests -- --test-threads=1`. A missing module/column establishes initial wiring RED; final behavioral assertions must execute on both real backends.
- [ ] **Step3: Implement the additive migration and narrow legacy constructors.** The legacy insert supplies `Set("private".into())`, `Set(0)`, `Set("private".into())`, `Set(1)` for the new fields. No unchanged retry updates current visibility or these creation fields. Use static migration failure messages. Keep the new entity's fields internal to database code; existing explicit legacy projection remains unchanged.
- [ ] **Step4: Implement safe down and rerun controls.** Before dropping any index/column, select only `id LIMIT1` from workspace_chat_draft. Refuse down with a static error if any row exists, even if private/defaulted; dropping audience evidence on a populated database could enable an unsafe older server. On an empty table, remove the owned index and four columns in dependency-safe order (version before creation visibility), without dropping any catalog/execution row or unrelated schema. Test empty up/down/up and populated refusal without partial schema mutation. A normal Migrator::up rerun must leave an already-shared v2 row and its original creation fields unchanged; do not make a manual second invocation of an already-applied migration silently reset state.
- [ ] **Step5: Run GREEN and commit.** Run real SQLite and explicitly opted-in disposable PostgreSQL privacy migration tests, existing catalog/draft/handler tests and team credential/personal-agent migration controls on the matching library executable; use `--list` to select actual filters. Preserve failed fixture names and clean only successful exact owned fixtures. Run standalone `cargo check --bin codeg-server --no-default-features`, diff check, and review the migration order. Commit only Task1 files as `feat(privacy): persist chat audience and creation metadata`.

### Task 2: Preserve legacy retries and add audience-aware version2 fingerprints

**Files:** Create `src-tauri/src/workspace_catalog/request_fingerprint.rs` with focused pure tests in the same module. Register its crate-private module in `workspace_catalog/mod.rs`. Replace only the old private fingerprint function/body in `workspace_catalog/draft_service.rs` with the v1 helper call. Extend `workspace_catalog/draft_tests.rs` for unchanged legacy retry/no-row behavior. Do not change current request DTOs, runtime state, repository verification or team route manifest.

**Produces:**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CreationAudience { Private, Organization }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintVersion { LegacyV1, AudienceV2 }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintError { IncompatibleAudience }

pub(crate) fn fingerprint(
    version: FingerprintVersion,
    node_id: i32,
    title: Option<&str>,
    repository_override: Option<&RepositoryDefault>,
    creation_audience: CreationAudience,
) -> Result<String, FingerprintError>;
```

These types are pure internal values, not serde request authority. The caller supplies already-normalized creation input, never the current Share audience. For a later team retry, require the requested creation audience to equal the persisted original creation audience before accepting the fingerprint; do not overwrite a changed request audience with the stored value merely to make it match. LegacyV1 with Organization returns IncompatibleAudience; later team retry code maps that incompatibility to Conflict. This task does not add that team API or certify its replay behavior.

Exact v1 encoding: signed node i32 big-endian; one byte0 for absent title, or byte1 followed by an unsigned u64 big-endian UTF-8 byte length and title bytes; one byte0 for no repository override, or byte1 followed by repository i32 big-endian and starting-branch length/bytes encoded the same way. SHA256 output is64 lowercase hex characters. Preserve the old algorithm byte-for-byte, including absence versus an explicitly empty string; input normalization still belongs to its existing caller.

Exact v2 encoding: SHA256 over UTF-8 bytes `neev:chat-create:v2` followed by a zero byte, then the **raw32-byte** v1 digest, then a single audience byte0(private) or1(organization). It is not the ASCII v1 hex string. Output is64 lowercase hex. Domain separation and original-audience inclusion prevent a replay from creating a differently shared chat.

- [ ] **Step1: Write failing golden and differentiation tests.** Synthetic fixed input: node17, title `Launch plan`, repository override29/`main`. Expected digests, independently computed with Node's SHA256 during plan preparation:

```rust
let repository = RepositoryDefault { repository_id: 29, starting_branch: "main".into() };
assert_eq!(fingerprint(FingerprintVersion::LegacyV1, 17, Some("Launch plan"), Some(&repository), CreationAudience::Private).unwrap(),
    "50eae8d13ba7e2089c6a8878a7b6a14b4b3192b9d69800ea9dcbef772dacc1f8");
assert_eq!(fingerprint(FingerprintVersion::AudienceV2, 17, Some("Launch plan"), Some(&repository), CreationAudience::Private).unwrap(),
    "62dfdd3d12f583c1ae5a00ffb497c649d9df9134f2e3b4a40718c49017491599");
assert_eq!(fingerprint(FingerprintVersion::AudienceV2, 17, Some("Launch plan"), Some(&repository), CreationAudience::Organization).unwrap(),
    "9bdee5dcea68026e9272d2452c2c1ddb8d7910be9b9b64904eb4b022b823ce2e");
```

Test v1 rejects organization creation; changes to node/title/repository/branch/audience change v2; absent/empty/Unicode title cases preserve byte-length encoding; repeated identical values are stable. A future Share changes no fingerprint input: use an immutable creation audience in that control, not a duplicate production authorization helper.
- [ ] **Step2: Run RED.** `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features request_fingerprint -- --test-threads=1`. Then establish that the legacy adapter's existing draft retry tests still cover actual persisted fingerprints, not only mock calls.
- [ ] **Step3: Extract the original algorithm and implement the exact v2 wrapper.** Reuse a private helper returning `[u8;32]` to avoid parsing hex or maintaining two encoders. Legacy normalize_input calls fingerprint with LegacyV1/Private; map its static impossible-in-this-call error to CatalogError::Conflict without logging. Do not introduce a generic serialization/hash framework or change request UUID/title/branch normalization.
- [ ] **Step4: Run GREEN/regressions.** Focused golden tests and existing draft creation/retry/inherited-repository/concurrency tests must pass; check no new row or changed original request fingerprint on legacy retry. Verify the old legacy DTO has not grown audience fields or started accepting team authority. Run diff check and matching no-default-features library/standalone compile controls; record exact results, not whole-project claims.
- [ ] **Step5: Self-review and scoped commit** `refactor(privacy): version chat creation fingerprints`.

## Controller acceptance and continuation

Use independent task reviews, fresh covering SQLite/PostgreSQL/legacy retry verification and one final combined review. Record the exact source and migration evidence before accepting persistence. Neither task admits old data or mounts team endpoints.

The next team catalog/resource-authorizer plan must implement proven tenant/creator mapping and quarantine, principal-derived repository authority, metadata projections, recursive hierarchy services, version-aware atomic chat creation, creator-only expected-revision Share and active-writer rejection. HTTP/UI must use the exact same authorization predicate for lists/counts/direct reads. Events/files/search/wiki/jobs remain separately gated and cannot reuse the legacy global transport. Preserve immutable original audience during all later creation retries and never silently reset current visibility.

## Self-review

- This plan covers persistence and retry format from spec sections3/5 only. No broader visibility, provider or rollout completion is claimed; downstream authorization requirements are explicitly carried forward.
- Task1's four column defaults preserve legacy rows and exhaustive inserts. Task2 consumes the existing RepositoryDefault and does not reinterpret current visibility as creation audience. There is no unsupported UUID draft-ID assumption.
- Per-column constraints allow legitimate later sharing while rejecting invalid audience/version combinations. Populated down fails before any owned schema mutation, preserving the safer rollback boundary.
- Both task test cycles have concrete assertions/commands and unchanged legacy controls. The intended tests do not grant real credentials, fabricate a team principal or use production databases.
- Source admission is intentionally unresolved by this inert storage plan: no positive mapping is inferred from users.org_id, path names or existing application roles. The later authority plan must prove admission before exposing a single legacy record.

Context: [org-memory: wiki/engineering/workos.md]. The old single-operator model does not authorize new team access.

## Additional source preflight, September10

- Exact exhaustive draft inserts confirmed at `workspace_catalog/draft_service.rs:257`
  and `web/handlers/workspace_catalog_tests.rs:513`. The execution provisioning
  test converts a loaded model into ActiveModel rather than exhaustively
  constructing one, so it should not need a defaults-only change.
- `workspace_catalog/types.rs` uses an explicit legacy WorkspaceChatDraft DTO;
  adding entity fields need not expose them to the existing transport.
- Existing node shapes distinguish a root Project with optional repository,
  a non-root Folder without its own repository, and a non-root WorkingDirectory
  with a repository. `resolve_default` follows the full ancestor chain and rejects
  cycles even after finding the nearest default. Preserve these behaviors.
- The later team repository adapter cannot reuse
  `workspace_catalog/repository_access.rs::token`: it filters users.org_id and
  invokes legacy ensure_github_token. The reviewed CredentialService::access
  already returns a non-Debug internal AccessGrant bound to immutable user ID,
  GitHub ID and generation; consuming it still requires server-derived session
  authority and a final generation check. This is a downstream integration
  constraint, not authorization to add an adapter in this persistence task.
- Existing legacy CatalogError database conversion logs DbErr, and node
  projection logs unknown persisted kind. Later team services need static error
  conversion and bounded explicit projections, not those legacy logging paths.
  These observations do not change the current persistence task or legacy mode.
- Migration-test preflight: on the existing local test harness's PostgreSQL
  endpoint, a read-only `SELECT CAST(1.5 AS bigint), pg_typeof(CAST(1.5 AS bigint))`
  returned2/bigint,exit0. No table/fixture/schema was created or changed. The
  test wording now distinguishes invalid stored types from an upstream SQL
  cast; this preserves the nonnegative integer storage requirement instead of
  specifying an impossible rejection of already-cast input. PostgreSQL documents
  assignment/explicit conversions in its [cast reference](https://www.postgresql.org/docs/current/sql-createcast.html).
