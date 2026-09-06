# Repository-backed Chat Execution and UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the approved project/folder/repository/chat model usable through a Notion-inspired interface, with isolated local branches/worktrees and verified first-send/file routing.

**Architecture:** Bridge a catalog chat draft to one execution record, one hidden physical folder, and one existing conversation. Reuse the established conversation panel only after provisioning succeeds. This is a local/trusted delivery slice: public multi-tenant release remains blocked on complete legacy-route authorization, isolated personal provider credentials, and runner sandboxing.

**Tech Stack:** Existing Next.js/React/TypeScript, Rust/Axum/SeaORM, Git CLI, Postgres/SQLite fixtures, Vitest, and Playwright. No new runtime dependency.

**Spec:** `docs/superpowers/specs/2026-09-07-repository-backed-workspace-design.md`

## Global Constraints

- Organization: the company workspace and tenant boundary, not a project.
- No workspace-level New chat and no standalone New page.
- Logical nesting across repository boundaries must not imply nested Git clones.
- Before any file-writing agent runs, allocate a unique conversation branch and isolated worktree from its recorded starting commit.
- Viewing or joining a conversation reuses its context and does not switch branches or take over an agent.
- Publishing is explicit: review changes, commit and push the conversation branch, then optionally open a PR. Never automatically merge into the starting branch.
- Preserve legacy IDs, conversations, aliases, and file paths during migration.
- No public deployment, actual GitHub push/PR, guessed repository binding, or destructive migration during implementation/testing.
- The user authorized continuing implementation and testing while asleep without further routine approval gates. Missing external credentials remain verification limits, not permission to invent successful provider connections.

## Prerequisite and release boundary

Complete and review the catalog plan first. Its service contracts and API clients are the inputs here. Catalog drafts remain private to their creator in this slice. Existing private history is not shared with teammates or copied into GitHub.

Enable new execution only with `WORKOS_ENABLE_CATALOG_RUNTIME=true`, on a server explicitly operated as a trusted/local instance. Default is disabled. This flag is not a security sandbox and must not be advertised as public SaaS authorization. New catalog/runtime endpoints require cookie-derived scope, the existing installation boundary where applicable, same-origin mutation checks, and explicit repository access. Never make the host's agent connections available to untrusted tenants.

All tests use disposable databases, local temporary Git repositories, and fake provider boundaries. Real provider acceptance is reported separately. No fake provider implementation is compiled into production handlers.

## File boundaries

| Unit | Files |
| --- | --- |
| Execution persistence | `src-tauri/src/workspace_execution/{mod,types,service,tests}.rs`, new entity `db/entities/workspace_chat_execution.rs`, migration `m20260907_000002_workspace_execution.rs` |
| Git allocation | `src-tauri/src/workspace_execution/{git,git_tests}.rs` |
| Legacy bridge | `src-tauri/src/workspace_execution/legacy_bridge.rs`, focused changes to folder/conversation models and services |
| Runtime authorization | `src-tauri/src/web/handlers/workspace_execution.rs`, focused ACP handler changes and execution lease helpers |
| Client flow | `src/lib/workspace-execution.ts`, `src/hooks/use-workspace-catalog.ts`, `src/components/workspace/{workspace-catalog-sidebar,workspace-node-dialog,workspace-chat-dialog,workspace-chat-launcher}.tsx` and colocated tests |
| Existing integration | `src/components/layout/sidebar.tsx`, `src/components/workspace/deep-link-bootstrap.tsx`, `src/stores/app-workspace-store.ts`, `src/stores/tab-store.ts`, `src/lib/folder-display.ts`, `src/lib/types.ts`, `src/components/chat/conversation-context-bar.tsx` |

Module/migration/entity/router registrations accompany their owning task. Preserve every pre-existing dirty hunk. Split tests by concern; do not grow a single catalog service or test file into the entire execution subsystem.

## Runtime contract

Keep the draft as immutable intent. Execution state has one authoritative owner in the new execution table; do not independently update draft state and execution state.

```typescript
export type WorkspaceExecutionState = "unprovisioned" | "provisioning" | "ready" | "failed"
export interface WorkspaceChatRuntime {
  draft: WorkspaceChatDraft
  state: WorkspaceExecutionState
  folder: FolderDetail | null
  conversation: DbConversationSummary | null
  branch_name: string | null
  starting_commit: string | null
  error_code: string | null
}
```

POST endpoints: `get_workspace_chat_runtime {draftId}`, `provision_workspace_chat {draftId,agentType}`, `get_workspace_runtime_capabilities {}`. Capabilities returns `{ execution_enabled: boolean }`. DTOs do not expose credentials. Runtime returns folder/conversation only in ready state. Draft IDs, folder IDs and conversation IDs are never interchangeable.

## Task 1: Execution state and atomic legacy bridge

**Interfaces:** async `get_runtime(db: &DatabaseConnection, scope: &Scope, draft_id: i32) -> Result<WorkspaceChatRuntime, ExecutionError>` and `reserve(db: &DatabaseConnection, scope: &Scope, draft_id: i32, agent_type: AgentType, starting_commit: &str) -> Result<workspace_chat_execution::Model, ExecutionError>`. State service finalizes allocations by draft ID. `ExecutionError` has Unauthorized, NotFound, Conflict, Disabled, RepositoryUnavailable, AllocationMismatch, Busy, Failed, Database variants with scrubbed client codes.

- [ ] Add failing fixture tests for creator/org scoping, one execution per draft, ready finalization atomicity, and retry reuse. Seed catalog nodes/repositories/drafts using reviewed test helpers, never legacy source paths.

```rust
let first = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
let again = reserve(&db.conn, &scope, draft.id, AgentType::ClaudeCode, &commit).await.unwrap();
assert_eq!(first.allocation_id, again.allocation_id);
assert_eq!(first.branch_name, again.branch_name);
assert!(reserve(&db.conn, &other_scope, draft.id, AgentType::ClaudeCode, &commit).await.is_err());
```

Here `commit` is a hand-created local Git commit SHA; `reserve(db,scope,draft_id,agent_type,starting_commit)` returns the execution entity. Scope fixture helpers use actual matching org/user rows.
- [ ] Run the named execution test filter RED with compilable stubs, then create the additive entity/migration. Execution row fields: `draft_id` primary key, `org_id`, `agent_type`, `state`, unique `allocation_id`, `starting_commit`, `branch_name`, `worktree_relpath`, nullable unique `legacy_folder_id` and `legacy_conversation_id`, `revision`, `started_at`, `updated_at`, nullable `last_error_code`, `provisioning_owner_token` and `provisioning_lease_expires_at`. Persist allocation before filesystem mutation. Draft must remain unchanged. An existing reservation keeps its original commit even if the starting branch advances; a changed agent conflicts.
- [ ] Add `FolderKind::WorkspaceChat` / wire value `workspace_chat`. Create a transaction-aware bridge in `legacy_bridge.rs` that inserts exactly one hidden physical folder and one regular legacy conversation, then marks the execution ready in the same transaction. Reuse or narrowly generalize the existing SeaORM row-construction helpers to accept `ConnectionTrait`; do not invoke `create_conversation_core`, which derives context from arbitrary physical folders.
- [ ] Keep hidden execution folders in `allFolders`, exclude them from visible legacy folder queries and filters. Do not use the scratch `Chat` kind: Files/Git need normal conversation behavior. Add Rust/TS tests that closing the last workspace-chat tab cannot seed a new blank conversation on the same worktree. Block legacy `create_conversation` and folderless-chat creation from reusing a workspace-chat folder.

```typescript
expect(filterVisibleFolders([regular, workspaceChat])).toEqual([regular])
expect(replacement.folderId).not.toBe(workspaceChat.id)
```

Use existing visibility helper naming (`excludeChatFolders`) if retained; tests assert behavior, not a new helper name. Derive `replacement` through the real tab-store close action, not a duplicate test implementation.
- [ ] Run full execution/catalog filters and focused folder/tab tests GREEN, then commit only owned files/hunks. Record exact helper signatures for the Git/provisioning task.

## Task 2: Repository cache and recoverable worktree provisioning

**Interfaces:** `ExecutionRepositoryAccess` verifies the acting user's repository and resolves a branch to a full commit SHA. `GitAllocator` takes only server-derived paths, canonical identity, pinned commit and reserved branch. Production implementations use existing GitHub token refresh and explicit askpass; fixture implementations use local repositories.

Define async trait `ExecutionRepositoryAccess::resolve(&self, scope: &Scope, repository_id: i32, starting_branch: &str) -> Result<VerifiedExecutionRepository, ExecutionError>`, where `VerifiedExecutionRepository { github_repository_id: String, full_name: String, starting_commit: String }` contains no token. Define async `GitAllocator::allocate(&self, scope: &Scope, reservation: &workspace_chat_execution::Model, repository: &VerifiedExecutionRepository) -> Result<AllocatedWorktree, ExecutionError>`. `AllocatedWorktree { allocation_id: String, worktree_path: PathBuf, branch_name: String }` is server-internal, never a browser request. The production allocator owns access to the scoped credential provider; credentials are not part of the repository DTO. Both traits are Send + Sync using the existing async-trait dependency.

- [ ] Add failing temporary-Git tests for two chats in one repo producing different worktrees/branches; repeated reservation/provision yielding the same result; recovery after Git succeeds but DB finalization fails; and mismatch/symlink/path collisions preserving all files.

```rust
assert_ne!(first.worktree_path, second.worktree_path);
assert_ne!(first.branch_name, second.branch_name);
std::fs::write(first.worktree_path.join("plan.md"), "first chat").unwrap();
assert!(!second.worktree_path.join("plan.md").exists());
assert_eq!(retry.allocation_id, first.allocation_id);
```

- [ ] Run RED, then implement server-owned layout:

```text
<data_dir>/orgs/<org_id>/repositories/<github_repository_id>/repo.git
<data_dir>/orgs/<org_id>/workspace-chats/<draft_id>/worktree
branch: workos/chat/<draft_id>
```

Canonicalize existing ancestors; reject symlink escape. Use GitHub stable IDs, not basenames. Verify existing cache remote identity with current authorized GitHub metadata before reuse. No user-supplied clone URL, checkout root, or branch-allocation name. Never use `/tmp` for production storage.
- [ ] Implement `provision` as short database reservation/finalization transactions around Git work, not one transaction held across network requests. Serialize by execution and repository cache where mutations share Git metadata. A persisted provisioning lease prevents parallel/restarted workers from racing the same allocation; only its owner may finalize. On expiry, reauthorize and reconcile the reserved values before retrying.
- [ ] Pin and persist the verified full starting commit before worktree creation. Use Git argv, explicit commit and explicit credentials. Disable inherited global/system Git config and hooks for managed Git commands; remove inherited Git directory overrides. Do not embed tokens in URLs, args, logs or persisted remotes. Extract existing askpass mechanics without reusing the browser-controlled clone endpoint.
- [ ] Reconcile: absent branch/path creates both; reserved branch at pinned commit with absent path reattaches; existing path is accepted only when common repo, symbolic branch and pre-ready HEAD match reservation. Any mismatch fails without deleting, force-resetting, suffixing, or checking out another branch. Ready executions may have subsequent commits; a normal ready lookup must not demand HEAD still equal the initial commit.
- [ ] Expose the three scoped runtime endpoints; flag-disabled returns Disabled before filesystem work. Validate selected `AgentType` through existing enum/status checks. Repeated provision with another agent conflicts once reserved. Finalize the legacy bridge only after verified worktree allocation. Do not start ACP here.
- [ ] Run temporary Git and catalog/execution tests GREEN; commit. Report actual network acceptance separately from fixture tests.

## Task 3: Scoped ACP entry and single-writer protection

**Interfaces:** Add optional `workspaceChatId` to frontend ACP connection context. On workspace connects, server derives ready folder/conversation/agent from scoped execution, ignoring caller-controlled runtime IDs/paths. Existing trusted legacy calls retain their current path without gaining access to workspace allocations.

Define `ReadyExecutionContext { draft_id: i32, folder_id: i32, conversation_id: i32, agent_type: AgentType, working_dir: PathBuf }`, internal only. Async trait `WorkspaceRunner` has `spawn(&self, context: &ReadyExecutionContext) -> Result<String, ExecutionError>` and `disconnect(&self, connection_id: &str) -> Result<(), ExecutionError>`. Async `connect_workspace(db: &DatabaseConnection, scope: &Scope, draft_id: i32, runner: &dyn WorkspaceRunner) -> Result<String, ExecutionError>` uses real scope/lease logic before invoking the external runner. Add writer lease entity/migration `workspace_writer_lease` / `m20260907_000003_workspace_writer_lease.rs` with draft_id PK, org_id, creator_user_id, unique owner_token, nullable unique connection_id, expires_at and updated_at. Lease mutations must compare owner_token/connection_id so a stale disconnect cannot release a successor's lease.

- [ ] Write failing tests with a fake process-spawn boundary: no spawn before ready; wrong creator rejected; second writer for one execution rejected; different executions can run; disconnect releases only its own lease. Test generic ACP connection cannot bypass the workspace path restriction.

```rust
assert!(connect_workspace(&db.conn, &scope, draft.id, &runner).await.is_ok());
assert_eq!(connect_workspace(&db.conn, &scope, draft.id, &runner).await.unwrap_err(), ExecutionError::Busy);
assert_eq!(runner.spawn_count(), 1);
```

Use a recording fake only for external process spawn; lease acquisition and scoped database resolution remain real.
- [ ] Run RED, then implement an execution-keyed persisted writer lease with owner token, connection ID, heartbeat and expiry. Reserve before spawn, finalize connection identity after success, release on failed spawn/disconnect, and require matching live owner on prompts. A viewer cannot acquire write authority simply by opening a chat. Do not equate the existing per-connection prompt lock with per-conversation ownership.
- [ ] Extend ACP connect/prompt/disconnect/touch handling for workspace-owned connections. Derive IDs and cwd from execution, enforce the caller's session scope, and reject mismatched posted IDs. Reuse existing `build_session_runtime_env`, installation checks, `spawn_agent`, and `send_prompt_linked_with_message_id` after authorization. Generic ACP with a cwd inside managed workspace-chat roots must be rejected unless using the scoped path.
- [ ] In the frontend connection lifecycle, carry canonical workspace chat identity from the runtime mapping. Ready conversations must already have a legacy conversation ID, so first send must not invoke legacy `createConversation`. Block branch switching and legacy fork/reuse actions that could retarget this managed worktree; route new work through New chat.
- [ ] Run ownership/lifecycle tests GREEN, plus existing connection lifecycle tests. Do not run a real provider unless an authorized disposable acceptance setup exists; report fake-runner results honestly. Commit.

## Task 4: Notion-inspired catalog sidebar and creation flow

**Interfaces:** Hook `useWorkspaceCatalog` consumes reviewed catalog clients; separate request states for tree, expanded-node drafts, creation and errors. Runtime launcher consumes `WorkspaceChatRuntime`. All queries use active session/org context; clear caches on identity change and ignore stale responses.

- [ ] Add failing RTL tests for an empty organization creating a repo-free project, nested folders, a working-directory repo/branch selection, mandatory repo selection for an unbound chat, inherited defaults, explicit overrides, and retryable auth/upstream failures.

```typescript
await user.click(screen.getByRole("button", { name: "New project" }))
await user.type(screen.getByLabelText("Project name"), "Marketing")
await user.click(screen.getByRole("button", { name: "Create project" }))
expect(await screen.findByRole("button", { name: "Marketing" })).toBeVisible()
expect(screen.queryByRole("button", { name: "New page" })).not.toBeInTheDocument()
```

Mock only transport responses, not the catalog components or their state/validation behavior. Existing GitHub picker helpers can be reused for loading/error affordances; don't assume one-page results are complete or require cloning to create a project.
- [ ] Run RED, then build a compact tree with restrained row styling, expand/collapse controls, keyboard-accessible contextual menus, differentiated folder/repository icons, breadcrumbs, inline loading/error states, and useful empty-state copy. No dashboards, duplicate Documents section, extra root New chat, or nonfunctional buttons. Preserve theme tokens and visible signed-in person/org identity.
- [ ] Root action is New project. Inside nodes offer New folder, New working directory, New chat. Project repository connection is optional; working-directory connection required; chats show inherited repo/starting branch or require selection. A plain folder can nest below a working directory. Keep repository override separate from display naming.
- [ ] Load drafts only for expanded nodes to avoid one eager API request per tree node. Refetch affected lists after mutations; keep local drafts/form input on errors. Unauthenticated state offers GitHub login, not an empty organization or fabricated name. Maintain the existing sidebar as a collapsed clearly labelled legacy/local section, preserving history and recovery. Hide its duplicate root creation action when catalog mode is active.
- [ ] Preserve reachability of unpinned legacy folderless chats as well as physical-folder chats inside the legacy/history area; merely nesting the current Projects-only list would hide those records. Do not add a new global chat creation action to restore history visibility. Add nested logical-folder expand/collapse coverage.
- [ ] Creating a chat records the draft and opens `/workspace?chatId=<id>`. The launcher shows agent selection and provisioning progress/errors; no ACP effect mounts before ready. Capability-disabled state is informative and does not offer a button that can never work. Ready result upserts returned folder/conversation into existing stores and opens the existing tab with `openTab(folder.id, conversation.id, agentType, true, title)`.
- [ ] Extend `src/components/workspace/deep-link-bootstrap.tsx` for canonical chat links without treating the chat ID as a legacy ID or invoking `addFolderToWorkspaceById`. Preserve shareable chat identity in the URL while supporting existing legacy links. Add browser-back/forward and repeat-open tests to prevent duplicate provisioning/tabs.
- [ ] Add managed-context header: logical breadcrumb, repository, local conversation branch, unpublished status, Files. No remote branch/file links before they exist. Existing BranchDropdown must not switch the managed branch. Files opens this chat's worktree using the established auxiliary panel; no duplicate document records.
- [ ] Update all supported locale files for new user-facing keys. Run focused RTL, typecheck and lint GREEN; visually inspect both themes, mobile width and keyboard focus. Commit only intended changes and preserve earlier dirty work.
- [ ] Repair the seven recorded stale baseline test files against the approved UI contract, preserving behavioral coverage rather than deleting failures: Projects-only/default and legacy-history visibility, translated labels, agent dropdown selection/disabled/fallback states, confirmed Delete/cancel/status menu, and Quick Actions replacement routes. Root diagnostic report is carried into this plan's task contract before dispatch. Existing passing fallback and isolation assertions remain intact.

## Task 5: End-to-end verification and usable local preview

- [ ] Create an isolated test server/database plus local Git/fake-provider fixture harness. Exercise project -> folder -> working directory -> chat -> ready runtime -> first prompt -> generated Markdown -> Files. Browser assertions must check actual rendered output, correct selected chat/worktree, and absence of console exceptions, not screenshots alone.
- [ ] Open two chats for the same repository and prove their file contents differ without cross-contamination. Attempt a second writer to the first chat and assert Busy. Reload deep links, restart the isolated backend, and verify retained mappings/unpublished files. Failures must retain user input and expose retry/reconnect without deleting history.
- [ ] Run catalog and execution Rust suites, server build/check, affected frontend suites, full frontend suite once, typecheck/lint and diff checks. Compare failures to the documented baseline. Record exact commands and results; no clean-suite claim if unrelated failures remain.
- [ ] Perform final independent review of the integrated slice, including ignored/uncommitted earlier UI changes that the preview uses. Fix scoped findings with tests. Keep public deployment blocked on the remaining auth/runner/provider boundaries.
- [ ] Prepare a local preview on a free port with durable data under the workspace, not `/tmp`. Do not migrate `workos_local`. If a realistic preview needs existing identity/history, use a separately named isolated database copy with credentials kept out of logs/Git; never publish that copy or treat copied sessions as onboarding verification. Keep the original service available unless a controlled local restart is needed and safe; inspect active work before touching its exact PID, never use the old broad-pkill startup script.
- [ ] Verify the final URL in a browser. Clearly distinguish an actual connected-user preview from fixture-only tests; never seed fake org identity into the user's live workspace or silently use their account to push test branches. Leave concise launch/config instructions and a precise implemented/unverified checklist for the user to test after waking.

## Coverage still gated after this plan

The user-facing local workflow can be complete without being public-SaaS-ready. Before untrusted teams join, complete explicit GitHub organization selection/membership/revocation, organization visibility policy, personal encrypted provider connections, all file/Git/terminal/event authorization boundaries, and process sandboxing. Guided remote publication and wiki compilation must have their own verified implementation before showing their actions in the UI; do not add placeholder controls or auto-push in this slice.

The approved product goal remains active beyond this plan. These gates are honest release boundaries, not permission to label partial local work as a complete hosted service.
