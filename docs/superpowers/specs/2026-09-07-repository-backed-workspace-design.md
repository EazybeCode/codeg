# Repository-backed organization workspace

Status: consolidated product model approved in conversation on 2026-09-07;
implementation and testing authorized while the user sleeps. This is a design, not
a statement that SaaS isolation or onboarding has shipped.

This spec supersedes conflicting product rules in
`2026-09-06-workos-workspace-design.md` and the parent workspace's
`.context/WORKOS-TEAM-ONBOARDING-DESIGN.md`. In particular, projects need not have
repositories, all new agent chats must have repositories, and a separate Pages
or Documents feature is not included.

## Approved product model

- Organization: the company workspace and tenant boundary, not a project.
- Project: a category such as Marketing, Sales, or Tech. It may have a repository
  connection but does not require one.
- Folder: a nestable organizational container. It need not represent a physical
  directory or have its own repository connection.
- Working directory: a repository-backed context with a selected repository and
  starting branch. It is a logical entry point, not a checkout shared by writers.
- Chat: belongs to a project or nested container and always resolves to one
  authorized GitHub repository before it can start an agent.
- Generated files: repository files produced or changed by the chat, including
  Markdown, shown in that chat's Files view without a second document system.

Workspace creation is New project only. Within projects and nested containers,
offer New folder, New working directory, and New chat. No workspace-level New
chat and no standalone New page. Display the signed-in person and organization.
Keep Tools secondary and advanced Git controls out of the default creation flow.

## Repository resolution

A project can establish a repository default. A working directory establishes
an explicit repository boundary. Plain folders organize content without creating
checkouts; their contained chats can inherit the nearest enclosing repository
default. Show that effective default clearly without claiming the plain folder
has its own repository connection.

When a new chat has no enclosing repository context, require the user to select
an accessible repository and starting branch. Store that choice on the chat;
do not silently bind its parent or create a default repository. Allow an explicit
override of an inherited repository during chat creation.

Snapshot the resolved repository identity and starting reference on creation.
Later folder moves, renames, or changes to defaults must not retarget existing
chats. Resolve the starting reference to a commit when provisioning execution.
Repository identity must distinguish owners and survive display-name changes;
do not treat a folder name, checkout path, or repository basename as identity.
Logical nesting across repository boundaries must not imply nested Git clones.

Repository authorization is checked on selection and again before Git access
or agent execution. Missing access offers reconnection or a new authorized
selection, never an empty-success result or a fallback to host credentials.

## Conversation execution and publication

Every chat records a repository association. Before any file-writing agent runs,
allocate a unique conversation branch and isolated worktree from its recorded
starting commit. A genuinely enforced read-only conversation can defer branch
allocation; otherwise provision isolation before starting the agent.

Track provisioning separately from conversational status: unprovisioned,
provisioning, ready, or failed. Creation retries must reuse the same allocation
identity, not spawn duplicate branches. Failed provisioning must be recoverable
without deleting conversation history or unrelated filesystem content.

Viewing or joining a conversation reuses its context and does not switch branches
or take over an agent. Permit at most one file-writing run in a conversation at
a time; independent conversations must not share writable checkouts. Changing
repositories or branches for an existing run requires an explicit new/forked
conversation rather than a checkout switch beneath a running agent.

Generated files retain their repository-relative paths. The Files view opens the
conversation's version, not whichever branch a shared project checkout happens
to have selected. Conversation/file associations are application metadata; full
transcripts are not automatically committed to the repository.

Publishing is explicit: review changes, commit and push the conversation branch,
then optionally open a PR. Never automatically merge into the starting branch.
The chat has an application URL from creation. Only display GitHub branch/file/PR
links when the relevant remote objects exist; show local changes as unpublished.
Archiving a chat must not automatically discard uncommitted or unpushed work.

## Identity and team boundaries

Initial onboarding uses GitHub login and explicit GitHub organization selection.
There is one selected organization workspace in the UI, containing its projects.
Do not automatically pick the first GitHub organization or treat organization
selection as proof of authorization. Keep internal identities distinct from
provider identities so later Google sign-in does not redefine projects.

There are no hardcoded customer organizations, project repositories, or wiki
repositories. A deployment operator configures the application once; customers
connect their own authorized GitHub resources and supported personal agents.
Provider sign-in mechanisms require separate validation; this spec does not
promise that every agent's CLI login can be embedded in a browser.

Tenant and resource authorization must cover API requests, files, Git commands,
terminals, downloads, agent execution, and live event subscriptions. A visible
organization label or shared bearer token does not implement these boundaries.
Viewing shared work must not expose another person's provider credentials.
Existing private conversations must not become organization-visible on upgrade.

Worktrees isolate Git working files, not malicious processes. Public SaaS release
requires execution isolation for tenant files, secrets, processes, and resource
usage in addition to application authorization. Runner sandbox design and
membership/visibility policy are separate delivery slices, not hidden additions
to the hierarchy implementation.

## Persistence and migration

Postgres stores workspace relationships, repository bindings, conversations, and
execution metadata. Durable storage preserves repository caches, worktrees,
attachments, and agent session state; database persistence alone is insufficient.
Do not use temporary directories for durable customer work. Credentials must be
encrypted and scoped to their authorized owner, with deployment-managed keys.

Preserve legacy IDs, conversations, aliases, and file paths during migration.
Backfill repository bindings only from verified evidence. Legacy chats with
unknown bindings or missing checkouts remain visible but cannot execute until
explicitly repaired. Never invent a repository, substitute an empty directory,
mass-create branches, or publish old transcripts as part of migration.

The local missing-directory incident is recovery work, not a requirement for a
standard SaaS repository. No destructive cleanup or production migration is
authorized merely by approval of this design.

## Current-code boundaries

The existing folder entity requires a unique physical path and mixes logical
subfolder ancestry with worktree ancestry. Conversation execution is coupled to
`folder_id`, with optional branch/original-cwd metadata. These are compatibility
constraints, not the new product model. Introduce explicit logical hierarchy,
repository binding, and execution-context boundaries rather than giving plain
folders fake filesystem paths. Final schema/migration steps belong in the plan.

Current user records carry one `org_id`; folder/conversation models do not yet
provide complete tenant ownership. Existing local UI and repository-context fixes
must be preserved, but do not prove multi-tenant safety. Keep this development
slice behind the current trusted/local deployment boundary until authorization
and execution-isolation acceptance gates are met.

## Delivery boundaries

1. Logical hierarchy and repository-bound chat contract: persistent organization
   ownership, project/container types, repository defaults and chat snapshots,
   creation UI, Files context, and non-destructive legacy compatibility. No new
   SaaS exposure or unisolated execution paths.
2. Conversation execution: recoverable branch/worktree provisioning, per-chat
   write serialization, correct file routing, explicit publication and retention.
3. SaaS onboarding and security: verified membership and visibility policy,
   resource/event authorization, personal agent connections, isolated runners,
   and Hetzner/Coolify persistence and restart validation.
4. Organization wiki: reviewed knowledge assembled from eligible work into an
   organization-selected destination. No automatic full-transcript publication
   and no separate manually authored Pages system in the current slice.

Each slice needs its own implementation plan and verification; completion of one
does not imply the others are delivered. Google login, billing, enterprise SSO,
and real-time document co-editing are outside this spec's implementation scope.

## Acceptance tests

- Create Marketing without a repository, nest folders, and preserve the hierarchy
  across reloads without creating fake checkout directories.
- Create a working directory with a selected repository and starting branch;
  display its binding distinctly from a plain folder.
- Starting a chat without a resolved repository requests selection and never
  starts an agent. Inherited and explicitly overridden bindings are persisted.
- Moving a chat or changing a parent's default does not change its recorded repo.
- Same-named repos from different owners cannot collide in storage or selection.
- Two writing conversations have different worktrees and branches; their Files
  views show their own changes. Repeated provisioning requests are idempotent.
- Viewing a teammate's authorized conversation neither forks it nor takes over
  execution. Concurrent writes within one conversation are rejected or queued.
- Generated Markdown is visible within its chat; no duplicate Documents/Pages
  entry is created. No full transcript is silently pushed to GitHub.
- Unpublished branches have no misleading GitHub links. Publishing failures
  preserve local work and are retryable without force-pushing or auto-merging.
- Missing legacy directories or unknown repositories show actionable recovery
  states while preserving conversation history.
- Before SaaS release, cross-tenant IDs, paths, credentials and subscriptions are
  denied, and membership revocation stops further protected access.
- Before SaaS release, restart/restore tests preserve metadata, worktrees,
  unpublished changes, and agent connections without user terminal setup.

Verification uses temporary fixtures and isolated test tenants, not destructive
operations on the user's database or existing repositories.
