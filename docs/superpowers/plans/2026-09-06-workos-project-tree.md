# WorkOS Project Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the approved GitHub-backed nested folder experience with a Notion-inspired workspace UI.

**Architecture:** Preserve existing folder/conversation identities and add explicit repository-context resolution. Improve the established sidebar and conversation shell rather than create a competing application. Keep backend fixes and UI changes in separate reviewable tasks.

**Tech Stack:** Next.js 16, React 19, TypeScript, Rust/Axum/SeaORM, existing Postgres runtime, Vitest and Rust test utilities.

**Spec:** `docs/superpowers/specs/2026-09-06-workos-workspace-design.md`

## Global Constraints

- Work only in the isolated `workos/team-workspace` checkout. Do not rename the Conductor branch, push, restart the user's server, or mutate existing repos/data during tests.
- Child folders inherit their enclosing repository and directory; a repo-folder override establishes a new repository root.
- Preserve current folder IDs, conversations, aliases, checkouts, keyboard navigation, and light/dark preferences.
- Use existing UI components and tokens. No decorative dashboard or inactive placeholder controls.
- Full transcripts are not copied into the organization wiki.
- Logical organization and runtime worktree ancestry are distinct.
- Existing audit baseline: 58 frontend test failures and 16 TypeScript diagnostics at `f3c5e15d`. Repair affected expectations/fixtures deliberately; do not erase behavior tests just to get green.
- Source writes use apply_patch. Test local temporary directories and a separate preview, never live Git mutations.

### Task 1: Notion-inspired project navigation and accurate context

**Files:** `src/components/layout/sidebar.tsx`, `src/components/conversations/sidebar-section-header.tsx`, `sidebar-conversation-list.tsx`, `src/components/chat/conversation-context-bar.tsx`, `src/components/layout/github-project-dialog.tsx`, `src/components/conversations/repo-subfolder-dialog.tsx`, `src/lib/folder-display.ts`, `src/lib/api.ts`, `src/lib/types.ts`, narrowly scoped workspace styles/components and their tests, locale message files.

**Interfaces:** Consume existing store/actions and `get_folder_repo_context` via `getFolderRepoContext(folderId: number): Promise<FolderRepoContext>`, using the spec's exact response shape. Until that endpoint is available, gracefully retain known folder context rather than blank the header. Produce a persistent GithubProjectDialog entry, full nested breadcrumb, and a real branch action using the existing BranchDropdown in the workspace/sidebar context area (not the composer).

- [ ] Write and run failing behavior tests: a Subfolder displays/selects its own identity while a regular worktree displays its parent; a populated workspace can open GitHub project selection; missing GitHub session renders a reconnect link rather than “No repositories found.” Example new literal assertion:

```typescript
expect(resolveFolderDisplayName({ name: "Analytics", parent_id: 3, kind: "subfolder" }, [{ id: 3, name: "docs" }])).toBe("Analytics")
```

- [ ] Implement a clean workspace identity/header and restrained navigation. Make New project persistently visible; keep existing advanced routes discoverable under secondary tools rather than deleting their functionality. Extract small new components when this avoids further enlarging the existing sidebar. Apply restrained spacing, rounded rectangles rather than pills for navigation, readable hierarchy, muted icons, clear hover/focus states, and a document-like conversation header. Keep both themes accessible.
- [ ] Implement breadcrumb resolution with alias-aware logical ancestors and a bounded/cycle-safe traversal. Do not mutate actual tab.folderId/path while correcting display. Add repo/branch context using the agreed endpoint; branch interaction uses actual folder/path. Keep composer clean.
- [ ] Consolidate GitHub dialog loading/error behavior into a focused helper where useful: preserve status, recognize 401/re-auth sentinel, offer `/auth/github/login`, show retry on non-auth errors, distinguish empty lists, and ignore responses after dialog close or repo switch.
- [ ] Update affected tests and locale keys across all supported locales, retaining meaningful old navigation contracts where still applicable. Run focused UI tests and type check. Commit only this task's files; report RED/GREEN evidence and files changed.

### Task 2: Repository context and reliable folder lifecycle

**Files:** `src-tauri/src/commands/folders.rs`, a focused new `src-tauri/src/workspace_repo.rs` module and its tests, `src-tauri/src/lib.rs`, `src-tauri/src/web/handlers/folders.rs`, `src-tauri/src/web/router.rs`, GitHub handlers, folder service if needed. Avoid frontend files owned by Task 1.

**Interfaces:** Implement `POST /api/get_folder_repo_context` with `{folderId: number}` and the exact response in the spec. Context is derived from validated folder records and actual Git HEAD/root/remote; the response never contains credentials. Emit the existing `folder://changed` upsert payload after successful create/rename operations.

- [ ] Add regression tests before production edits: ordinary nested repo inheritance; nested explicit override; logical project ancestry separate from worktree ancestry; non-Git existing folder returns null repo; missing folder errors; repeated ancestor terminates with error; malformed GitHub names rejected; duplicate folder returns invalid input rather than database error.
- [ ] Implement the resolver using git commands as argv (no shell), canonical directory paths, safe relative paths, and parsing of HTTPS/SSH GitHub remotes that drops credentials. For `docs/Analytics/Test1`, expected logical root is docs and relative path is `Analytics/Test1`. For docs/wiki-mirror, expected repo is the override and relative path is empty.
- [ ] Fix duplicate plain subfolders before directory/row creation; preserve the existing dir. Require GitHub backing for new inherited folders in the web handler while preserving lower-level compatibility for desktop callers and existing data.
- [ ] Route folder create/alias events through existing broadcaster with complete details, avoiding updates on failed writes. Keep API/desktop signatures coherent. Cover event payload/absence on failure at an appropriate boundary.
- [ ] Fix top-level cloning: validate `owner/repo`, use owner-aware paths, validate a legacy existing clone's actual remote before reuse, reject mismatches rather than return unrelated code. Do not accept an arbitrary clone_url carrying GitHub credentials to another host. Fetch all bounded paginated repo/branch pages with explicit error handling.
- [ ] Run targeted Rust tests and server-mode cargo check. Commit only this task's files; report exact commands and output.

### Task 3: Integration, baseline repairs, and browser verification

**Files:** Only affected test fixtures/expectations, isolated preview scripts under the parent `.context`, and any integration fixes sent to the owning implementer.

- [ ] Independently review Tasks 1 and 2 for spec compliance and correctness. Send actionable findings to the owner and verify the covering tests after fixes.
- [ ] Bring existing FolderDetail fixtures up to date with `repo_full_name: null`; fix the established translation parity/type mismatch. Adapt tests that assumed removed Chat/Recent UI to meaningful supported behavior without dropping lower-level grouping coverage.
- [ ] Run `./node_modules/.bin/vitest run --reporter=dot`, `./node_modules/.bin/tsc --noEmit --incremental false`, appropriate lint, and `next build` using the installed Node 22 toolchain. Record known residual failures accurately if an unrelated failure remains.
- [ ] Start an isolated preview on a free port with API/WebSocket forwarding to 3080, using browser-scoped test authorization. Do not alter the running service's assets. Exercise nested breadcrumb/repo inheritance, persistent project picker, error recovery, branch availability, folder controls, keyboard interactions, desktop/mobile sizing, and light/dark themes. Save screenshots and a concise reproducible browser harness.
- [ ] Verify server mode build/tests and clean diff. Obtain a final independent review of the whole branch. Keep work available locally; summarize what is implemented and which approved platform slices remain.

## Following slices

The approved full design remains active. Write separate concrete plans for authenticated team access and wiki publication after this foundational slice's actual interfaces are established. Do not represent an API prototype or a static knowledge page as completed multi-tenancy or an automatic wiki pipeline.
