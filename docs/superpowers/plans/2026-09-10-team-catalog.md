# Team catalog and chat privacy implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Execute task-by-task with independent reviews.

**Goal:** Make projects, recursive folders, repository mapping and private/organization chat creation and sharing usable through the existing team boundary.

**Architecture:** Keep the existing catalog tables and pure hierarchy/fingerprint utilities. Add an empty positive-admission relation populated only transactionally by new principal-authorized creation; old rows stay quarantined. A dedicated team catalog service uses the accepted principal transaction guard and personal GitHub credential service, never legacy `Scope` authorization or shared token helpers. Wire explicit HTTP routes and a team-only Notion-style workspace; no legacy ACP provider.

**Tech Stack:** Rust, SeaORM SQLite/PostgreSQL, Axum, Next static export, React, next-intl, Vitest.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md` (owner approved); repository hierarchy from `2026-09-07-repository-backed-workspace-design.md`.

## Global Constraints

- Organization is the company workspace, not a project. Preserve organization → projects/categories → arbitrarily nested folders/working directories → repository-backed chats.
- Existing chats remain private. New chats default to private, with the audience visible before creation. Being inside a shared project/folder does not silently broaden a chat's audience.
- No protected operation accepts a client-supplied user ID, role or organization ID as authority.
- Admins do not get an ordinary-app backdoor into private chats.
- A shared conversation may be viewed by the selected organization, but browsing unrelated repository contents, cloning, pushing or executing still requires the acting user's independently verified repository authority.
- Never automatically push a private chat's transcript, generated Markdown or unpublished files, or feed them into the organization wiki.
- Keep the existing Notion-inspired sidebar hierarchy and contextual New chat actions under projects/folders. Do not restore a global orphan chat action or add a separate Documents hierarchy.
- Static Next export remains supported using fixed pages and query parameters. All labels/errors are translated in the existing ten locales. Keyboard access and narrow-screen behavior are acceptance requirements.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No cloud mutation, deployment, source-origin push, provider login, package installation or legacy data deletion by implementers. Existing untracked `.context/` belongs to prior work. All local edits use apply_patch. One local Cargo owner at a time.

## Slice boundary

This plan implements catalog and chat metadata, not transcript/file/event/prompt execution, provider supervision, feedback or notifications. Those consumers remain unavailable until separately authorized. No completion claim extends to those other release gates. The previously accepted fingerprint encoder must be used, not reimplemented.

### Task 1: Principal-scoped catalog persistence and repository authority

**Files:**
- Create `src-tauri/src/db/migration/m20260910_000002_team_catalog_admission.rs` and migration tests `team_catalog_tests.rs`; register in migration/mod.rs.
- Create entity `src-tauri/src/db/entities/team_catalog_admission.rs`; register in entities/mod.rs.
- Create `src-tauri/src/team_server/catalog/{mod,types,authority,repository,nodes,chats,tests}.rs`; register module in team_server/mod.rs. Split tests into `repository_tests.rs`/`postgres_tests.rs` if required to keep responsibilities focused.
- Expose the accepted fingerprint module crate-internally in workspace_catalog/mod.rs. Do not change the encoder or legacy callers.

**Interfaces:**
Consumers use `TeamState`, `AuthContext` and `ApiError` already defined. New service functions, visibility `pub(in crate::team_server)`:

```rust
async fn list_nodes(state: &TeamState, context: &AuthContext) -> Result<Vec<NodeDto>, ApiError>;
async fn list_repositories(state: &TeamState, context: &AuthContext) -> Result<Vec<RepositoryDto>, ApiError>;
async fn connect_repository(state: &TeamState, context: &AuthContext, access: &dyn TeamRepositoryAccess, input: ConnectRepositoryInput) -> Result<RepositoryDto, ApiError>;
async fn create_node(state: &TeamState, context: &AuthContext, access: &dyn TeamRepositoryAccess, input: CreateNodeInput) -> Result<NodeDto, ApiError>;
async fn update_node(state: &TeamState, context: &AuthContext, access: &dyn TeamRepositoryAccess, node_id: i32, input: UpdateNodeInput) -> Result<NodeDto, ApiError>;
async fn list_chats(state: &TeamState, context: &AuthContext, node_id: Option<i32>, after_id: Option<i32>) -> Result<Vec<ChatDto>, ApiError>;
async fn read_chat(state: &TeamState, context: &AuthContext, chat_id: i32) -> Result<ChatDto, ApiError>;
async fn create_chat(state: &TeamState, context: &AuthContext, access: &dyn TeamRepositoryAccess, input: CreateChatInput) -> Result<ChatDto, ApiError>;
async fn change_visibility(state: &TeamState, context: &AuthContext, chat_id: i32, input: ChangeVisibilityInput) -> Result<ChatDto, ApiError>;
```

All DTOs serialized camelCase; all inputs Deserialize camelCase + deny_unknown_fields, including nested repository defaults. `Visibility` is private|organization, default Private. Numeric revisions are i64 in storage, serialize as decimal strings to avoid JavaScript precision loss; expectedRevision/accessRevision request values likewise canonical nonnegative i64 strings. IDs are positive i32.

```text
RepositoryDefaultInput {repositoryId:i32, startingBranch:String}
ConnectRepositoryInput {fullName:String, startingBranch:String}
CreateNodeInput {parentId:Option<i32>, kind:project|folder|working_directory, name:String, repositoryDefault:Option<RepositoryDefaultInput>}
UpdateNodeInput {expectedRevision:String, parentId:Option<i32>, name:String, repositoryDefault:Option<RepositoryDefaultInput>}
CreateChatInput {nodeId:i32, repositoryOverride:Option<RepositoryDefaultInput>, title:Option<String>, requestId:String, visibility:Visibility}
ChangeVisibilityInput {expectedRevision:String, visibility:Visibility}
NodeDto {id,parentId,kind,name,repositoryDefault,revision:String}
RepositoryDto {id,githubRepositoryId:String,fullName}
ChatDto {id,nodeId,title,repositoryId,githubRepositoryId,fullName,startingBranch,visibility,accessRevision:String,canShare:bool,executionState:String}
```

Do not return creator identifiers unnecessarily, authorization/session/credential fields, request fingerprints/IDs, paths or legacy IDs. `canShare` is server-derived. Immutable author projection for later messages is outside this task.

- [ ] Write failing migration and service tests before production changes. Reuse `principal_fixture`, `team_state`, ALICE_A_TOKEN/ALICE_B_TOKEN/BOB_A_TOKEN/CAROL_B_TOKEN patterns from organization_settings_tests.rs; do not construct an unverified principal from request data.

Representative observable sequence (real service calls; fake only external GitHub source):

```rust
let private = create_chat(&state, &alice, &access, private_input).await.unwrap();
assert!(matches!(read_chat(&state, &bob, private.id).await, Err(ApiError::NotFound)));
assert!(list_chats(&state, &bob, None, None).await.unwrap().is_empty());
let shared = change_visibility(&state, &alice, private.id, ChangeVisibilityInput {
    expected_revision: "0".into(), visibility: Visibility::Organization,
}).await.unwrap();
assert_eq!(shared.access_revision, "1");
assert_eq!(read_chat(&state, &bob, shared.id).await.unwrap().id, shared.id);
assert!(matches!(read_chat(&state, &alice_b, shared.id).await, Err(ApiError::NotFound)));
```

Test cases: old valid-looking private AND org-visible rows unadmitted; orphan/foreign/cyclic node ancestry denied; two organizations plus same user in both; legacy users.org_id intentionally mismatched with valid active membership still works in selected org; no admin private override; defaults inherit nearest ancestor; working-directory requires explicit repo; non-project root/project below parent rejected; no hierarchy cycles; stale node revision denied; private→org→private transitions; noncreator and unknown IDs indistinguishable404; active/provisioning/writer state blocks sharing; changed-input retry409; retry after sharing does not reset audience; immutable repo/branch snapshots; foreign repository/node rejected before network calls; revoked/expired principal denied at final transaction; database failures static errors/no sensitive logging.

- [ ] Run focused RED from src-tauri with `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::catalog -- --test-threads=1`; record actual missing-behavior failure. Use installed toolchain and existing guarded PostgreSQL conventions. Do not run all unrelated Rust tests or delete failed fixture databases.
- [ ] Add admission schema: `team_catalog_admission(resource_kind TEXT CHECK IN ('repository','node','chat'),resource_id INTEGER CHECK >0,org_id INTEGER CHECK >0,creator_user_id INTEGER CHECK >0,created_at timestamp)`; composite primary key `(resource_kind,resource_id)`, index `(org_id,resource_kind,resource_id)`. No automatic backfill. Down refuses if any admission exists; empty down only removes owned table. SQLite and PostgreSQL up/rerun/down-refusal/fresh-upgrade data-preservation tests.
- [ ] Implement one transaction authority helper following organization_settings lock ordering: begin → lookup stable GitHub user ID → `lock_user_identity` → org row lock → `principals.recheck_in_transaction`. Reads and writes both use it; never call legacy verify_scope/begin_scoped_write. Scope comes only from AuthContext. Do not hold DB locks across network calls. Before returning/committing after long work recheck proof/time. Service errors map DbErr to static ApiError::Unavailable without rendering the DbErr.
- [ ] Implement admission reads/writes in that transaction. Only successful new node/chat creation or independently verified repo connection inserts an admission row with the principal org/user. Old node/chat rows cannot be admitted by update/read. A repository may be explicitly reconnected only after personal GitHub proof matches numerical identity; its old nodes/chats remain hidden. Require admission org/resource mapping, creator equality for chats, same-org admitted complete acyclic ancestry and consistent admitted repo identity before projecting a chat. All lists use the exact same predicate as direct reads; no hidden titles/counts.
- [ ] Implement `TeamRepositoryAccess` as async trait `verify(&self, context:&AuthContext, full_name:&str, branch:&str)->Result<VerifiedRepository,ApiError>`, with `GithubTeamRepositoryAccess::new(state:&TeamState)->Result<Self,ApiError>`. Use `state.credentials.access(user_id, stable_github_user_id)`; require returned user/github IDs and generation equal context, verify exact fixed HTTPS GitHub API `/repos/{owner}/{repo}` and `/branches/{branch}` with no redirects, 15s timeout, bounded1MiB response, numeric positive repoID, canonical fullname/branch; reject 401/403/404/rate limits/malformed safely. No host token or `ensure_github_token`. Credential generation is rechecked before persistence. Tests use local HTTP fixtures in a cfg(test) constructor and a fake verifier for service tests, never real GitHub or real tokens.
- [ ] Implement repository/node/chat functions. Reuse pure validate_label/validate_branch/validate_node_shape logic and versioned fingerprint. Organization row lock serializes tree/default mutations. Validate/admit read targets before network; verify selected repository by personal credentials outside locks; final transaction checks hierarchy/default/revision and exact numeric repo identity again. List maximum1000 nodes/repos; reject creation when org node/repo cap or creator chat cap1000 reached (409). Chats use pages of up to200 visible admitted rows, ordered ID ascending strictly after optional positive after_id; apply admission/visibility before the page limit. No hidden count or cursor based on private rows; verify pagination with interleaved private and visible records. No fixed depth limit; visited-set ancestry traversal handles arbitrary finite depth and corruption. Request IDs are canonical UUIDs scoped by existing org/creator uniqueness. New chat stores v2 fingerprint and original audience; legacy v1 retry semantics remain untouched. Sharing increments checked revision, rejects overflow, and rejects any non-draft execution_state, any execution row, or active writer lease. This is conservative until new job service defines terminal-safe sharing.
- [ ] Verify focused tests + guarded PostgreSQL transaction/race controls + adjacent organization/principal/catalog regressions and standalone server check. Record commands/output and retained warnings. Commit only task files and report.

### Task 2: Explicit catalog HTTP routes and adversarial integration tests

**Files:** create `src-tauri/src/team_server/handlers/catalog.rs`, `http_catalog_tests.rs`; modify team_server/router.rs, handlers/mod.rs, mod.rs, state.rs, limiter.rs, browser_fixture.rs and test_server.rs as necessary for route admission and controlled external repo verification. A repository-proof override or loopback API origin is allowed only behind `cfg(test)`; production construction remains fixed to GitHub. Preserve existing TeamState construction callers.

**Consumes:** Task1 service/DTO interface exactly above. **Produces:** explicit no-store team routes:

```text
GET /api/team/catalog/nodes
POST /api/team/catalog/nodes
PATCH /api/team/catalog/nodes/{id}
GET /api/team/catalog/repositories
POST /api/team/catalog/repositories
GET /api/team/chats?nodeId=<positive i32>&afterId=<positive i32>  (both optional, reject unknown/duplicate query keys)
POST /api/team/chats
GET /api/team/chats/{id}
PATCH /api/team/chats/{id}/visibility
```

- [ ] RED tests using actual Axum router: protected reads401 without cookie; unauthenticated-write tests supply otherwise valid Origin/JSON/double-submit CSRF headers but no valid session and expect401. An invalid Origin remains403 before session resolution, matching the existing security sequencing. Synthetic shared bearer does not unlock; exactOrigin+CSRF required on every write; forged org/user/role and unknown/nested fields400; malformed/duplicate IDs/query invalid; same-resource notfound controls; Alice/Bob/Carol/dual-org sharing journey and stale revision409. Lists are `{items:[...]}`; detail/create/patch directly DTO. No extra legacy path becomes reachable.
- [ ] Run from src-tauri `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features http_catalog -- --test-threads=1`, record RED.
- [ ] Implement route handlers matching organization.rs security sequencing but no admin restriction for ordinary catalog operations. GET cached membership; all mutations fresh membership. Bounded32KiB JSON, exact JSON content-type and Origin, duplicate/ambiguous cookies and matching session CSRF. Strict path/query parsing. Route layer `bounded_catalog` reuses the existing bounded_auth timeout/private-header/static-log helper with a distinct catalog token bucket. Authentication keeps burst100/refill1per second; catalog uses burst200/refill20per second. Both buckets share the SAME32-permit semaphore (not32 per pool), and the same monotonic clock; no client-selected limits or forwarding-header trust. Preserve existing auth behavior and 60s timeout. Test bucket independence in both directions, bounded refill and shared total concurrency, plus actual catalog/auth router responses after exhaustion; no unbounded new limiter maps. These are provisional safety ceilings, not a load-tested public capacity claim. HEAD denied explicitly. Do not move routes into legacy server. Safe service errors retain404/409/503/no-store behavior.

```rust
.route("/api/team/chats", on(MethodFilter::GET, handlers::catalog::list_chats)
    .on(MethodFilter::POST, handlers::catalog::create_chat)
    .on(MethodFilter::HEAD, handlers::method_not_allowed))
```

- [ ] Run GREEN HTTP integration and all team-server tests, verify no changes to runtime capabilities, static exports, cookie policies. Commit/report.

### Task 3: Team-only Notion-style workspace and Share experience

**Files:** create `src/lib/team-catalog-client.ts` and tests; `src/components/team/team-workspace.tsx`, `team-workspace-tree.tsx`, `team-workspace-dialogs.tsx`, `use-team-catalog.ts`, their focused tests; modify team-signed-in-shell.tsx and tests and ten locale JSONs in messages/. Extend `scripts/tests/team-auth-e2e.cjs` or add a focused adjacent browser driver using the same guarded fixture; `team_server/browser_fixture.rs` and `test_server.rs` may gain test-only repository responses needed for that journey. Reuse existing UI primitives and visual conventions; do not mount legacy workspace providers or ACP.

**Consumes:** Task2 exact DTO/API contract; existing TeamAuthProvider, timeout/abort-safe request transport in team-client.ts. If sharing transport requires an export, expose only the generic safe request function/types rather than copying timeout logic. **Produces:** authenticated `/workspace` displays responsive left hierarchy + main chat metadata, person/org identity, settings/back/logout.

- [ ] RED UI tests: initial project-only global create; nested contextual new-folder/working-directory/new-chat; repository and branch input with nearest default displayed; working-directory requires repository; chat creation defaultsPrivate; org label accurate; private returned rows absent in colleague view; only creator Share; Share dialog discloses entire-history sharing and separate GitHub audience; expectedRevision sent; failed mutation preserves draft; stale scope aborts requests and clears all content/drafts; loading/error/retry and keyboard/mobile navigation. Pure client validators reject malformed DTOs/revisions and accidental authority input.
- [ ] Run focused Vitest to capture RED.
- [ ] Implement muted Notion-inspired sidebar (roughly260px desktop), collapsible project/folder tree, contextual menus, selected node/chat breadcrumbs, clean blank states and compact visibility chip/Share button. At root only New project. Within node allow folder/working directory/chat, not another root project. “Working directory” explicitly associates repository; normal folder may inherit/no repo. Dialog accepts repo owner/name + starting branch and connects through verified endpoint before node/chat mutation; errors never clear entered values. No dummy agent send control: show runtime-disabled explanation until the separately integrated job feature is ready. No separate documents hierarchy.
- [ ] Contextual Edit details uses the already implemented node PATCH to rename a project/folder/working directory or change its repository default/starting branch. Keep node kind and parent unchanged in this first edit UI; do not add delete/reparent endpoints. Send the current node revision, preserve the entered draft on409/failure, and explain that existing chats keep their saved repository/starting-branch snapshot. Working-directory edits still require an explicit repository. Test successful edit, exact revision/parent payload and stale-revision recovery. This makes the existing update API usable without changing hierarchy or deleting data.
- [ ] Team catalog hook owns abort controller, request epoch and current auth.scope; suppress stale completions after logout/org change. Gate rendered data by its scope synchronously (or remount the workspace by scope), rather than waiting for an effect to clear old data. A catalog401 clears protected content and refreshes authentication. Bounded polling every15s while visible refreshes list/detail for multiplayer sharing revocation, with no global stream. When selected chat becomes404, remove its metadata immediately; a concurrently delayed older list response must not reinsert it. Test that out-of-order list/detail case and scope changes while a mutation completes. Stop polling when hidden; refresh on visibility. No localStorage persistence of private titles/drafts. Query-param selections `node` and `chat` parsed as positive IDs; render no stale content while new scope loads.
- [ ] Chat list provides accessible Load more when a page has200 items; next afterId is last visible ID. Refresh replaces currently loaded results safely (never retains revoked rows); either re-fetch loaded pages or reset to page1 with truthful pagination. Do not derive counts or cursors from hidden rows.
- [ ] Existing `TeamSignedInShell` authentication gates remain; settings children continue rendering with return navigation, while default workspace child becomes TeamWorkspace. Preserve tests for idle/unauthorized/unavailable and scope lifecycle. Ten locales have matching new namespace keys; do not alter org branding. All interactive controls have accessible names, focus restoration and modal keyboard handling via existing Dialog components.
- [ ] Run affected/full Vitest once, TypeScript, scoped lint and static export. Add actual Chromium fixture journey using existing team browser test infrastructure for two people/private→org→private, nested folders and settings navigation. Fixture provider proof is synthetic and labeled; it does not replace actual hosted/provider acceptance. Commit/report with exact commands, screenshots paths and any browser console errors.

## Completion gate

Review each task, then combined diff once. Record remaining deferred provider/job/file/event/feedback requirements honestly. Root performs migration rehearsal and verified publication/deployment later; this plan never authorizes opening public access by itself.
