# Workspace Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a tested, tenant-scoped logical project/folder catalog and immutable repository-bound chat drafts without coupling organizational containers to filesystem paths.

**Architecture:** Add an independent workspace catalog alongside legacy physical folders. Repository bindings and chat drafts use stable IDs; no draft becomes an executable legacy conversation until the execution slice provisions its isolated worktree. This first backend/API slice has no new sidebar entry point and cannot start agents, so it does not expose a partially functioning replacement chat UI.

**Tech Stack:** Existing Rust/Axum/SeaORM, Postgres and SQLite test support, Next.js/TypeScript transport, Rust tests and Vitest. No new runtime dependencies.

**Spec:** `docs/superpowers/specs/2026-09-07-repository-backed-workspace-design.md`

## Global Constraints

- Organization: the company workspace and tenant boundary, not a project.
- Chat: belongs to a project or nested container and always resolves to one authorized GitHub repository before it can start an agent.
- No workspace-level New chat and no standalone New page.
- Logical nesting across repository boundaries must not imply nested Git clones.
- Later folder moves, renames, or changes to defaults must not retarget existing chats.
- Do not use temporary directories for durable customer work.
- Preserve legacy IDs, conversations, aliases, and file paths during migration.
- Never invent a repository, substitute an empty directory, mass-create branches, or publish old transcripts as part of migration.
- Work only in the existing `.context/workos-src` worktree, branch `workos/team-workspace`; preserve all pre-existing uncommitted implementation edits.
- No push, public deployment, live database migration, or live repository mutation. User-run `ship` is required for publishing this development branch.

## Scope and sequencing

The approved spec contains four delivery slices. Split its first slice into catalog/API foundation (this plan) and a user-facing integration plan paired with execution provisioning. This avoids fake directory paths and avoids enabling new chats on a shared checkout.

This plan delivers real persisted catalog operations and typed API clients, tested without GitHub writes. It deliberately does not claim a new end-to-end chat experience. The next plan must cover actual worktree provisioning plus sidebar/composer/Files integration before exposing the new workflow. Membership onboarding, exhaustive legacy endpoint authorization, runner sandboxing, publishing, and wiki generation retain separate acceptance gates.

Do not derive tenant ownership of existing records from `/tmp/orgs/1`, folder names, or whichever user happens to log in. Leave legacy records unchanged. Existing `/auth/me` identity and `current_user` are a compatibility bridge for the local-only catalog API; they are not verified multi-organization onboarding.

## File map

| File | Responsibility |
| --- | --- |
| `src-tauri/src/workspace_catalog/types.rs` | Exact request/response/error types |
| `src-tauri/src/workspace_catalog/resolve.rs` | Pure logical ancestry and repository-default resolution |
| `src-tauri/src/workspace_catalog/service.rs` | Transactions, scoped CRUD, immutable draft creation |
| `src-tauri/src/workspace_catalog/repository_access.rs` | Read-only verification of repository and branch access |
| `src-tauri/src/workspace_catalog/tests.rs` | In-memory schema, service, and resolver tests |
| `src-tauri/src/workspace_catalog/mod.rs` | Module exports only |
| `src-tauri/src/db/entities/workspace_node.rs` | Logical container persistence |
| `src-tauri/src/db/entities/workspace_repository.rs` | Tenant-owned repository connections |
| `src-tauri/src/db/entities/workspace_chat_draft.rs` | Repository-bound draft, separate from executable conversation |
| `src-tauri/src/db/migration/m20260907_000001_workspace_catalog.rs` | Additive portable schema |
| `src-tauri/src/web/handlers/workspace_catalog.rs` | Session identity, scoped API mapping; no agent execution |
| `src/lib/workspace-catalog.ts` | TypeScript DTOs and web transport wrappers |
| `src/lib/workspace-catalog.test.ts` | Transport contract tests |

Registration changes: `src-tauri/src/lib.rs`, `src-tauri/src/db/entities/mod.rs`, `src-tauri/src/db/migration/mod.rs`, `src-tauri/src/web/handlers/mod.rs`, and `src-tauri/src/web/router.rs`. Do not add web-only catalog calls to desktop commands until a desktop identity model is designed.

## Shared interfaces

Use snake_case in JSON responses and camelCase request fields, following existing transport conventions. IDs below are catalog IDs, never legacy `folder_id` or `conversation_id` values.

```typescript
export type WorkspaceNodeKind = "project" | "folder" | "working_directory"
export interface RepositoryDefault { repository_id: number; starting_branch: string }
export interface WorkspaceNode {
  id: number; org_id: number; parent_id: number | null;
  kind: WorkspaceNodeKind; name: string; repository_default: RepositoryDefault | null;
  revision: number;
}
export interface WorkspaceRepository {
  id: number; org_id: number; github_repository_id: string; full_name: string;
}
export interface WorkspaceChatDraft {
  id: number; org_id: number; node_id: number; creator_user_id: number;
  repository_id: number; github_repository_id: string; full_name: string;
  starting_branch: string; title: string | null;
  execution_state: "unprovisioned";
}
export interface CreateNodeInput {
  parentId: number | null; kind: WorkspaceNodeKind; name: string;
  repositoryDefault: RepositoryDefault | null;
}
export interface UpdateNodeInput {
  nodeId: number; expectedRevision: number; parentId: number | null;
  name: string; repositoryDefault: RepositoryDefault | null;
}
export interface CreateDraftInput {
  nodeId: number; repositoryOverride: RepositoryDefault | null;
  title: string | null; requestId: string;
}
```

Rust DTOs mirror these exactly; use `i32` for catalog IDs, `i64` for revisions, and store GitHub numeric IDs as validated decimal strings at this boundary to avoid JavaScript integer truncation. `RepositoryDefault` uses `i32` and `String`. `Scope { org_id: i32, user_id: i32 }` is derived on the server, never accepted from the request.

Define `CatalogError` variants: `Unauthorized`, `NotFound`, `InvalidInput(String)`, `Conflict`, `RepositoryRequired`, `RepositoryUnavailable`, `ReauthRequired`, `UpstreamUnavailable`, and `Database`. Never serialize database errors or upstream credential-bearing bodies.

In the signatures below, abbreviated `db` means `&DatabaseConnection` and `scope` means `&Scope`. Drafts are creator-private in this foundation: `list_drafts` filters both organization and creator. Shared viewing is not enabled until the team visibility policy and runtime authorization are implemented together.

## Task 1: Pure hierarchy and repository resolution

**Files:** Create catalog `mod.rs`, `types.rs`, `resolve.rs`, and `tests.rs`; register module in `src-tauri/src/lib.rs`.

**Interfaces:** Produce `resolve_default(nodes: &[WorkspaceNode], scope: &Scope, node_id: i32) -> Result<Option<RepositoryDefault>, CatalogError>` and `validate_node_shape(kind: &WorkspaceNodeKind, parent_id: Option<i32>, default: &Option<RepositoryDefault>) -> Result<(), CatalogError>`.

- [ ] Add this failing test and equivalent cases for nearest override, wrong organization, missing ancestor, and cycle. Keep a test helper `node(id, parent_id, kind, repository_default)` returning `WorkspaceNode` with `org_id=1`, name `node-{id}`, and revision `0`.

```rust
#[test]
fn catalog_plain_folder_inherits_without_becoming_a_checkout() {
    let binding = RepositoryDefault { repository_id: 7, starting_branch: "main".into() };
    let nodes = vec![
        node(1, None, WorkspaceNodeKind::Project, Some(binding.clone())),
        node(2, Some(1), WorkspaceNodeKind::Folder, None),
    ];
    let scope = Scope { org_id: 1, user_id: 2 };
    assert_eq!(resolve_default(&nodes, &scope, 2).unwrap(), Some(binding));
    assert!(nodes[1].repository_default.is_none());
}
```

- [ ] Run `cargo test --no-default-features --lib catalog_plain_folder_inherits_without_becoming_a_checkout` from `src-tauri`; confirm RED is a missing implementation/assertion failure, not toolchain failure.
- [ ] Implement traversal using `HashSet<i32>` for visited IDs and an organization-filtered lookup. Validate the entire ancestor chain even after finding the nearest default. Require an ancestor project, reject cycles/missing IDs, and return `NotFound` for cross-org IDs. A project has no parent; a folder/working directory requires a parent; a working directory requires a default; a plain folder cannot own a default. Projects may optionally own one. Node kind is immutable.

```rust
let mut cursor = Some(node_id);
let mut seen = std::collections::HashSet::new();
let mut resolved = None;
while let Some(id) = cursor {
    if !seen.insert(id) { return Err(CatalogError::InvalidInput("hierarchy cycle".into())); }
    let node = nodes.iter().find(|n| n.id == id && n.org_id == scope.org_id)
        .ok_or(CatalogError::NotFound)?;
    validate_node_shape(&node.kind, node.parent_id, &node.repository_default)?;
    if resolved.is_none() { resolved = node.repository_default.clone(); }
    cursor = node.parent_id;
}
Ok(resolved)
```

- [ ] Run `cargo test --no-default-features --lib workspace_catalog`; confirm all resolver cases GREEN. Validate nonempty trimmed labels, maximum 200 Unicode characters, and branch values with no control characters. Repository/API validation is Task 3, not guessed here.
- [ ] Commit only these new modules and the precise registration hunk: `feat: define logical workspace hierarchy`.

## Task 2: Additive persistence and tenant-scoped catalog mutations

**Files:** Create the three entities, migration, and `service.rs`; modify module registries and catalog tests.

**Interfaces:** Produce `list_nodes(db: &DatabaseConnection, scope: &Scope) -> Result<Vec<WorkspaceNode>, CatalogError>`, `create_node(db, scope, input: CreateNodeInput) -> Result<WorkspaceNode, CatalogError>`, and `update_node(db, scope, input: UpdateNodeInput) -> Result<WorkspaceNode, CatalogError>`. All functions are async. Add `Database` conversion that logs internal failures without returning their text to clients.

- [ ] Use existing `crate::db::test_helpers::fresh_in_memory_db()` for tests. Seed two organizations with `org::ActiveModel` (GitHub IDs 100 and 200), and users through the existing model; do not use `DATABASE_URL`. Add a failing test demonstrating that a project persists with no physical folder rows:

```rust
let db = fresh_in_memory_db().await;
let scope = Scope { org_id: 1, user_id: 1 };
let input = CreateNodeInput { parent_id: None, kind: WorkspaceNodeKind::Project,
    name: "Marketing".into(), repository_default: None };
let before = folder::Entity::find().count(&db.conn).await.unwrap();
let created = create_node(&db.conn, &scope, input).await.unwrap();
assert_eq!(created.name, "Marketing");
assert_eq!(folder::Entity::find().count(&db.conn).await.unwrap(), before);
assert_eq!(list_nodes(&db.conn, &Scope { org_id: 2, user_id: 2 }).await.unwrap().len(), 0);
```

- [ ] Run the named test with `cargo test --no-default-features --lib catalog_project_without_filesystem`; confirm RED. Place the snippet inside a test of that name after seeding the organization/user records.
- [ ] Implement the migration with SeaORM portable table/index builders, not Postgres-only SQL. All tables have created/updated timestamps. Required schema:

```text
workspace_node:
  id integer PK, org_id integer, parent_id nullable integer, kind text,
  name text, repository_id nullable integer, starting_branch nullable text,
  revision bigint default 0
workspace_repository:
  id integer PK, org_id integer, github_repository_id text, full_name text
  unique(org_id, github_repository_id)
workspace_chat_draft:
  id integer PK, org_id integer, node_id integer, creator_user_id integer,
  repository_id integer, github_repository_id text, full_name text,
  starting_branch text, title nullable text, request_id text,
  execution_state text default 'unprovisioned'
  unique(org_id, creator_user_id, request_id)
```

Index `org_id` on all tables, `(org_id,parent_id)` on nodes, and `(org_id,node_id)` on drafts. Validate references and matching org ownership in service transactions; no cascade deletion or automatic legacy-row backfill. `down` must refuse if any new table contains rows; an empty test schema can drop draft, node, then repository tables.
- [ ] Implement scoped create/update operations. Verify scope against an existing user/org row. Serialize node mutations per organization: on Postgres lock the org row in a transaction; on SQLite make a no-op org-row update before hierarchy reads to acquire the write lock. Use expected revision in the UPDATE condition and increment revision. Re-read parent/default relationships inside the transaction to prevent concurrent cycle creation. No filesystem calls belong in this module.
- [ ] Add tests for stale revisions, cross-tenant parent/default IDs, self-parenting, descendant moves, root rules, migration rerun, and preservation of seeded legacy folders/conversations. Run `cargo test --no-default-features --lib workspace_catalog` to GREEN. Check migration SQL with both SQLite and Postgres query builders; run real Postgres acceptance in Task 6.
- [ ] Commit only task-owned changes: `feat: persist tenant-scoped workspace catalog`.

## Task 3: Verify and connect repositories without cloning

**Files:** Create `repository_access.rs`; extend `service.rs`, types, and tests. Reuse `github_auth::ensure_github_token` without exposing token fields in DTOs.

**Interfaces:** Define `VerifiedRepository { github_repository_id: String, full_name: String, starting_branch: String }`. Define async trait `RepositoryAccess: Send + Sync` with `verify(&self, scope: &Scope, full_name: &str, branch: &str) -> Result<VerifiedRepository, CatalogError>`. Produce `connect_repository(db, scope, access: &dyn RepositoryAccess, full_name: &str, branch: &str) -> Result<WorkspaceRepository, CatalogError>`.

- [ ] Implement a test-only `FakeRepositoryAccess` holding `HashMap<(String,String), VerifiedRepository>`; `verify` returns a cloned matching record or `RepositoryUnavailable`. Add RED tests: same basename/different owners creates distinct records; repeat verified ID updates its display name without changing catalog ID; unavailable repository creates no record.

```rust
let first = connect_repository(&db.conn, &scope, &access, "acme/docs", "main").await.unwrap();
let renamed = connect_repository(&db.conn, &scope, &access, "acme/handbook", "main").await.unwrap();
assert_eq!(first.id, renamed.id);
assert_eq!(renamed.full_name, "acme/handbook");
```

For this test, seed both fake names with the same verified ID `"123"`; seed `other/docs` with `"456"` for the collision test.
- [ ] Run `cargo test --no-default-features --lib catalog_repository`; confirm RED.
- [ ] Implement the production adapter with `reqwest`, HTTPS `api.github.com` only, a 15-second timeout, no automatic redirects, existing encrypted user-token lookup, and no anonymous/host-token fallback. Accept only two nonempty owner/name components containing ASCII alphanumeric, hyphen, underscore or dot; reject `.` and `..`. Build request paths using URL path segments, never concatenate a user-supplied URL.

```text
GET /repos/{owner}/{repo} -> verify canonical id and full_name
GET /repos/{canonical_owner}/{canonical_repo}/branches/{encoded_branch}
-> require named branch to exist before returning VerifiedRepository
```

Use the official GitHub response schemas linked in References. Metadata access alone is not permission to run an agent or publish; those operations reauthorize in the execution plan. For now empty repositories with no branch return `RepositoryUnavailable`, with actionable copy in future UI; do not silently initialize them.
- [ ] Implement unique-ID upsert in a transaction. Map missing session/token to `ReauthRequired`; 401 to `ReauthRequired`, 404 to `RepositoryUnavailable`, and network/429/5xx to `UpstreamUnavailable`. For 403, distinguish rate-limit headers from access denial; never expose token/request headers. Treat redirects as unavailable and ask for reselection instead of forwarding credentials.
- [ ] Test HTTP status parsing with a localhost fake HTTP server passed only to a test adapter constructor; production origin must not be configurable by a browser. Verify slash-containing branches are encoded as one path segment, malicious names produce no request, and upstream failures persist nothing. Run `cargo test --no-default-features --lib workspace_catalog` to GREEN.
- [ ] Commit: `feat: verify workspace repository connections`.

## Task 4: Immutable, idempotent repository-bound chat drafts

**Files:** Extend catalog `service.rs`, `types.rs`, and `tests.rs`.

**Interfaces:** Produce async `create_draft(db, scope, access: &dyn RepositoryAccess, input: CreateDraftInput) -> Result<WorkspaceChatDraft, CatalogError>` and `list_drafts(db, scope, node_id: i32) -> Result<Vec<WorkspaceChatDraft>, CatalogError>`.

- [ ] Add failing tests using Task 3's fake verifier: missing default/override returns `RepositoryRequired`; inherited repository is captured; override does not alter parent; changing a project default or moving a plain folder does not retarget an existing draft; cross-org repository IDs fail.

```rust
let draft = create_draft(&db.conn, &scope, &access, CreateDraftInput {
    node_id: nested.id, repository_override: None, title: Some("Launch plan".into()),
    request_id: "5fdbf5b3-e44d-47b2-8b7b-5af5a2cd31b8".into(),
}).await.unwrap();
assert_eq!(draft.repository_id, connected.id);
assert_eq!(draft.execution_state, "unprovisioned");
assert_eq!(conversation::Entity::find().count(&db.conn).await.unwrap(), 0);
```

Seed `nested` under a project with `connected.id` and branch `main`. This proves a draft does not create an executable legacy conversation with a fake folder ID.
- [ ] Run `cargo test --no-default-features --lib catalog_draft`; confirm RED.
- [ ] Implement override-first/default-second resolution. Require request IDs parse as UUIDs; use a trimmed optional title of at most 200 characters. Load the scoped repository connection and reverify with the caller's token before persisting. Compare verified GitHub ID with the stored ID; reject mismatches. Snapshot repository identity/display name/starting branch, never access tokens or a fabricated remote branch URL.
- [ ] Serialize the final insert with the org transaction from Task 2. After network verification, re-read nodes and defaults; return `Conflict` if the inherited selection changed during verification. Enforce the unique request key in the database. A repeated key with the same node/title/explicit override returns the existing draft after access checks; a different request body returns `Conflict`. To compare inherited retries, use the original stored selection rather than resolving a new parent default. Store a normalized request fingerprint alongside the draft, adding a non-null `request_fingerprint` text column to this still-unreleased migration.
- [ ] Add concurrent retry tests and a fake access revocation between calls. Two equal retries yield one row; conflicting payloads fail. Run `cargo test --no-default-features --lib workspace_catalog` to GREEN. No provisioning, Git mutations, messages, or draft-to-legacy mappings in this task.
- [ ] Commit: `feat: persist repository-bound chat drafts`.

## Task 5: Authenticated HTTP contract and TypeScript clients

**Files:** Create handler and TypeScript files from the map; modify handlers registry/router only at precise registration points.

**Interfaces:** POST endpoints under the existing protected `/api` router:

```text
list_workspace_nodes       {} -> WorkspaceNode[]
create_workspace_node      CreateNodeInput -> WorkspaceNode
update_workspace_node      UpdateNodeInput -> WorkspaceNode
connect_workspace_repository {fullName:string,startingBranch:string} -> WorkspaceRepository
create_workspace_chat_draft CreateDraftInput -> WorkspaceChatDraft
list_workspace_chat_drafts  {nodeId:number} -> WorkspaceChatDraft[]
```

- [ ] Write RED handler tests for no cookie, expired cookie, valid cookie, and cross-org node IDs. Seed real session/org/user rows following `workspace_identity.rs` tests. A shared development bearer token without a user cookie must not authorize these endpoints. Never read real browser cookies in tests.
- [ ] Implement `scope_from_headers(headers, db) -> Result<Scope, CatalogError>` using `current_user` and a real org lookup. All new handlers require it, accept no client org/user identity, and map DTOs explicitly. Error body is `{ "error": "repository_required" }` or corresponding stable snake_case code; map Unauthorized/ReauthRequired=401, NotFound/RepositoryUnavailable=404, InvalidInput=400, Conflict=409, RepositoryRequired=422, UpstreamUnavailable=502, Database=500. InvalidInput may include a scrubbed validation message, never paths or upstream bodies.
- [ ] Before accepting a repository default in create/update-node handlers, load its organization-scoped repository record and call Task 3's verifier with the acting user's credentials. Reject revoked access and changed GitHub IDs. Keep the network request outside the catalog transaction, then revalidate the stored repository identity in the transaction. Add tests proving that another user's existing connection does not authorize an otherwise inaccessible repository. Draft lists must exclude drafts owned by another member even within the same organization in this foundation.
- [ ] Add `#[serde(deny_unknown_fields)]` to request DTOs. Apply same-origin validation to new browser mutations: reject a mismatched Origin against trusted configured app origin, never against arbitrary forwarded-host headers; require JSON content type. Add missing/forged-origin tests with an explicit policy allowing non-browser test clients only when authenticated and without browser fetch metadata. Do not claim this fixes existing endpoints.
- [ ] Implement typed clients using the established transport, with exported types from Shared interfaces. Re-export existing `AgentType` only if consumed; no provider selection is persisted in this catalog draft contract.

```typescript
import { getTransport } from "./transport"
export function createWorkspaceNode(input: CreateNodeInput): Promise<WorkspaceNode> {
  return getTransport().call("create_workspace_node", input)
}
export function createWorkspaceChatDraft(input: CreateDraftInput): Promise<WorkspaceChatDraft> {
  return getTransport().call("create_workspace_chat_draft", input)
}
```

Implement `listWorkspaceNodes()`, `updateWorkspaceNode(input)`, `connectWorkspaceRepository(fullName, startingBranch)`, and `listWorkspaceChatDrafts(nodeId)` with the endpoint table's exact parameters and return types.
- [ ] Add this transport assertion plus rejected-request propagation tests. Do not create a sidebar button wired to drafts before isolated execution exists.

```typescript
const input: CreateNodeInput = {
  parentId: null, kind: "project", name: "Marketing", repositoryDefault: null,
}
await createWorkspaceNode(input)
expect(call).toHaveBeenCalledWith("create_workspace_node", input)
```

Use `vi.hoisted(() => ({ call: vi.fn().mockResolvedValue({}) }))` and `vi.mock("./transport", () => ({ getTransport: () => ({ call }) }))` in this test file; reset the mock before each test.
- [ ] Run Rust catalog tests, `vitest run src/lib/workspace-catalog.test.ts`, and `tsc --noEmit --incremental false`; confirm GREEN before committing: `feat: expose authenticated workspace catalog API`.

## Task 6: Integration verification and next-slice handoff

**Files:** Catalog tests, any task-owned corrective changes, and `.context` test scripts/results. No live database configuration edits.

- [ ] Capture `git diff --stat`, task commit IDs and all commands in the progress ledger. Preserve the pre-existing source changes; never stage unrelated sidebar/runtime work as part of this plan.
- [ ] Run the full catalog Rust test filter, server-mode build/check, focused TypeScript tests/typecheck/lint, and `git diff --check`. Use Node 22 at `/opt/homebrew/opt/node@22/bin`, Cargo at `/Users/air1/.cargo/bin`, and OpenSSL at `/opt/homebrew/opt/openssl@3`. Set PATH with a quoted assignment; do not repurpose HOME or other system variables.

```bash
cargo test --no-default-features --lib workspace_catalog
cargo check --no-default-features --bin codeg-server
./node_modules/.bin/vitest run src/lib/workspace-catalog.test.ts
./node_modules/.bin/tsc --noEmit --incremental false
./node_modules/.bin/eslint src/lib/workspace-catalog.ts src/lib/workspace-catalog.test.ts
git diff --check
```

Cargo commands run in `src-tauri`; JavaScript commands run at the app root. Record existing failures separately rather than weakening tests. Last observed full-suite baseline had 58 failures; it is not a current pass claim.
- [ ] Run migration/service/concurrency tests against a newly created disposable Postgres database named `workos_catalog_test_<uuidhex>` on the available local server. Generate the UUID in the harness, create that exact database, and connect directly; never inherit the live DATABASE_URL. Before any cleanup, assert both the exact generated name and its prefix. Preserve the database on failure for diagnosis; report it. If local Postgres is unavailable, report unverified rather than switching to `workos_local`.
- [ ] Exercise the new API using test sessions against an isolated server backed exclusively by that disposable database. Create org A/B fixtures, project without repo, nested folder, fake-verified repository, inherited/overridden draft, stale update, and cross-tenant request. Assert no physical folder rows or filesystem checkouts were created, and no legacy conversation can be launched from a draft ID.
- [ ] Obtain focused code review of schema, scope enforcement, retry concurrency, and non-destructive compatibility. Fix findings with regression tests and rerun affected checks.
- [ ] Hand off the exact new catalog interfaces for the next execution/UI plan. That plan must cover durable checkout paths, unique branch/worktree provisioning and locking, safe draft-to-runtime mapping, repository reauthorization, actual Files routing, repo selection dialogs, and preservation of legacy private visibility. Do not enable the new sidebar flow until those are verified together.
- [ ] Summarize actual delivered APIs and tests, pending user-facing/runtime/security work, and any remaining failures. No public deployment or push.

## Coverage review

| Approved requirement | Coverage |
| --- | --- |
| Org-owned categories without mandatory repo/physical path | Tasks 1–2 |
| Distinct plain folder and working directory | Tasks 1–2 |
| Nearest default and explicit chat override | Tasks 1, 4 |
| Every new chat has a repo; parent edits do not retarget it | Task 4 draft contract; execution integration must enforce at runtime |
| Stable repository identity and no owner/name collisions | Task 3 |
| Tenant-aware new catalog API and typed UI contract | Task 5; not all existing APIs |
| Non-destructive legacy preservation | Tasks 2, 6; no guessed backfill |
| Actual sidebar, composer, Files view, worktree/local branch | Next paired execution/UI plan; intentionally not enabled here |
| Remote publication, retention, PR flow | Execution/publication plan |
| Verified GitHub org onboarding, personal agents, sandboxing | SaaS security/onboarding plan |
| Wiki compilation | Wiki plan; no standalone Documents subsystem |

## References

- GitHub repository lookup: https://docs.github.com/en/rest/repos/repos#get-a-repository
- GitHub branch lookup: https://docs.github.com/en/rest/branches/branches#get-a-branch
- Local identity bridge: `src-tauri/src/web/handlers/github_auth.rs`, `workspace_identity.rs`.
- Existing physical model: `src-tauri/src/db/entities/folder.rs`, `conversation.rs`.
- Existing temporary test DB helper: `src-tauri/src/db/test_helpers.rs`.
