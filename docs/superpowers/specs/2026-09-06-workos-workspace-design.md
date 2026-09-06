# WorkOS team workspace

Approved by the user on 2026-09-06, including repository inheritance with overrides. The user additionally requested a Notion-inspired UI/UX.

## Product contract

The organization workspace contains projects, arbitrarily nested folders, conversations, and repository-backed documents. A project has GitHub backing. Child folders inherit their enclosing repository and directory; a repo-folder override establishes a new repository root. Logical folder hierarchy and runtime worktree ancestry must remain distinct.

The experience is a calm, readable workspace: a restrained sidebar, persistent New project action, contextual create/rename actions, accurate breadcrumbs, clear repo/branch context, and progressive disclosure for advanced tools. Preserve existing keyboard navigation and light/dark preferences. No decorative dashboard or inactive placeholder controls.

Existing conversations, folders, aliases, and checkouts survive upgrades. Same-named repositories from different owners remain distinct. Duplicate folder names yield a useful validation error. Authentication failures offer reconnection, not an empty-repo message. Folder additions and renames synchronize between browsers.

Team work uses authenticated organization membership and scoped API/events/execution. Shared work is inspectable by permitted teammates; viewing never takes over an agent. Tabs remain personal. Parallel work uses separate execution checkouts. No existing private transcript is silently published.

Project documents are versioned Markdown. Organization knowledge follows cited candidate → scrub/scan → human review → consolidated wiki update. Raw transcripts are not copied into the wiki. Each organization designates its own wiki repository. Reuse existing memory conventions. [org-memory: wiki/engineering/org-memory-system.md]

Cloud deployment retains Next.js/Rust/Postgres plus persistent working storage and background workers. A deployment destination has not been supplied. Preparing local code and deployment artifacts is authorized; publishing, pushing, and operating external infrastructure are separate actions.

## Delivery slices

1. GitHub-backed project tree and Notion-inspired workspace experience.
2. Authenticated team access, personal navigation state, shared activity, and execution isolation.
3. Repository-backed documentation and the reviewed organization wiki loop.
4. Cloud deployment and backup/restore validation at the selected destination.

Each slice must have independent verification. Do not claim later slices are complete because underlying APIs exist.

## First slice boundaries

Introduce an explicit resolved repository context for logical folders; do not conflate nested folders with runtime worktrees. Preserve the current folder IDs and use the resolver as the compatibility boundary for the existing records. A dedicated project/membership persistence model follows in the team slice so ownership is not guessed from filesystem paths.

Resolved context response:

```typescript
interface FolderRepoContext {
  folder_id: number
  project_folder_id: number
  repo_folder_id: number
  repo_full_name: string | null
  branch: string | null
  relative_path: string
  is_inherited: boolean
  git_root: string | null
}
```

`project_folder_id` is the outermost logical ancestor; `repo_folder_id` is the closest explicit repository boundary, or the enclosing logical root. Runtime worktree folders keep their actual execution root. `relative_path` is relative to the actual repository root. Missing/non-Git legacy folders resolve honestly to null repository values instead of invented GitHub backing. New inherited subfolders require valid GitHub backing in web WorkOS mode; existing folders remain recoverable.

The first slice includes fixing the audited missing repo entry point, incorrect nested breadcrumbs, inaccessible branch control, duplicate-name error, repo-name collisions, and missing folder-change events. The broader multi-tenant safety issues remain explicit work for slice 2; the first slice must not be advertised for untrusted multi-org deployment.

## Verification

- Focused tests establish regressions before fixes, including nested contexts, explicit repo overrides, missing ancestors, cycles, duplicate names, and conflicting repository owners.
- Frontend tests and type checks finish with no new failures; update outdated fixtures/expectations when the approved UI changed the contract.
- Server-mode Rust checks/test targets validate backend changes against temporary SQLite/test directories, never the user's production database.
- Browser review covers populated and empty workspaces, nested conversations, repo errors, keyboard creation/rename, responsive layout, and both themes.
- Preview the isolated frontend against the existing API without restarting the running service or copying credentials into source files.
