# Neev native chats and document history

Date: 2026-09-11. Proposed detailed design; not implemented.
The owner approved the scope in conversation: native chats and Markdown documents
inside existing folders, private by default, explicit organization sharing,
immutable revisions, compare/restore and conflict-safe saves without keystroke
co-editing. This written contract requires owner review before implementation.

## Outcome and boundaries

A signed-in organization member can create a chat or document inside an existing
project or recursively nested folder without GitHub. Colleagues can read and add
human discussion to organization-wide chats and edit organization-wide documents.
Private work remains available only to its creator while that person is an active
member. Administrators have no private-content override.

This stage supplies persistent collaboration and the storage boundary that agent
jobs will consume. It does not enable AI execution. The next isolated-job stage
adds each person's own supported AI subscription, durable execution, cancellation
and provider login. Until then the UI explicitly distinguishes a human discussion
message from an unavailable agent prompt. It must not display simulated AI replies.

GitHub connection/publication remains an optional later adapter; no hidden repo,
sentinel repository ID, automatic sync or actual GitHub deletion. Existing legacy
operator routes and filesystem APIs remain inaccessible to native users. Public
release still requires agent execution, real provider/email/HTTPS acceptance,
verified backup/restore, website and deployment review.

## Approach

Selected: PostgreSQL metadata and immutable protected server blobs. This extends
the previously approved native architecture and permits consistent backups without
sharing a Git object database between private conversations.

Alternatives considered: PostgreSQL document bodies simplify atomic backups but
depart from the selected separate content store; universal Git history complicates
private-document access and leaks unrelated objects into worktrees. Neither is used.

## Hierarchy and ownership

Keep organization → project → recursively nested folder. A project/folder detail
shows accessible chats and documents with New chat and New document actions. No
global New chat or competing Documents tree. Native node IDs remain server-issued;
new resource IDs are opaque random UUIDs, never filesystem paths.

Chats have a creator, node placement, title, private/organization visibility,
authorization revision, content revision and archive state. Documents have the
same placement and ownership fields, current revision, and optional originating
chat ID. Composite organization/resource foreign keys prevent cross-tenant links.
Moving or renaming changes placement only, not ownership or visibility. Cross-org
moves/copies are not supported. Existing folder names are organization-visible;
private resource names, counts, previews and history are filtered before pagination.

Standalone documents have their own private/organization audience. A document
generated from a chat has no independent audience: every read, list, revision,
download and restore rechecks the originating chat. Folder placement cannot widen
that audience. Users cannot detach an originating chat to evade this constraint.

Archive hides the item by default and is reversible by its creator; it does not
delete revisions or free storage. Archived chat content remains readable to its
authorized audience but cannot receive new messages or edits until restored.
Origin-bound document reads continue to enforce the archived chat's audience.

## Authorization and sharing

All requests require the native organization-scoped session. Reuse the established
account/org/member lock order and final authority recheck. Every resource query
includes organization and audience constraints; unauthorized IDs return the same
not-found result as nonexistent IDs. Counters and cursors cannot expose hidden rows.
Session/API responses remain no-store and same-origin/CSRF protected.

Only a creator can change visibility, archive or relocate their resource. Any
active member may read and contribute to organization-wide work. Contributors can
create document revisions, but cannot change its creator or audience. Private work
cannot be transferred in this release. Organization owners cannot recover private
content for a removed member through ordinary product APIs.

Private→organization requires a confirmation naming the organization and explaining
that the complete app-visible chat history and its attached documents will become
visible. Organization→private explains that previously downloaded copies cannot be
recalled. The change increments authorization revision and is rejected while an
active editor write reservation or future agent job owns a resource write lease.
The final transaction rechecks the expected authorization revision. No visibility
change may race a write into publishing previously unauthorized content.

## Chats and authorship

Human messages are append-only, identified by UUID and ordered by server sequence.
The server records author, creation time and message kind; clients cannot claim
another author or assistant/tool identity. A client idempotency key prevents a
retry from duplicating a message. Conflicting reuse of the key returns a conflict.
No edit/delete of individual historical messages in this stage.

Title and audience are visible in the chat header. Message rows show author and
time. A compact mode selector distinguishes Team message from Ask an agent; the
latter is visibly unavailable with a link to personal connections until runtime
capability is truly enabled. Creating a chat does not require a provider account.
The future agent endpoint must use the submitting person's own authorized
connection, never a teammate's or an organization-wide fallback.

Refresh newly visible messages/revisions with a bounded five-second poll only
while the page is visible; cancel on navigation, logout or auth-generation change.
Poll by cursor/revision rather than reloading complete histories. This is async
collaboration, not keystroke co-editing or presence. No fake online avatars.

## Document editing and revisions

Start with a Markdown editor and safe preview, using installed UI primitives but
not legacy absolute-path file components. Explicit Save with unsaved/working/saved
states; keyboard Cmd/Ctrl+S submits the same guarded save. Warn before discarding
an unsaved in-memory draft. Do not persist private drafts to localStorage.

Each revision records immutable blob key, SHA-256, UTF-8 byte length, author,
timestamp, parent revision and optional restored-from revision. Display revision
numbers and authors; compare two authorized revisions as escaped text/Markdown
diff. Restore creates a new revision against the current expected revision. It
never rewrites history. Recheck access on each history/content fetch.

Saves require both expected content and authorization revisions. A stale save
returns409, retains the author's draft, and offers Compare with latest, Copy draft
and Reload latest (with discard confirmation). Never silently overwrite another
person's revision. No automatic merge or simultaneous keystroke editing claim.

Render without raw HTML execution. Block javascript/data/file/custom-protocol
links; allow deliberate http/https navigation with noopener/noreferrer. Do not
automatically fetch remote images, embeds, fonts or tracking resources. Display a
safe placeholder with an explicit external link instead. Code is escaped text.
Initial documents are Markdown only; binary attachments are a later extension.
Downloads use authenticated resource/revision IDs and attachment disposition,
not public blob URLs or server paths.

## Immutable storage and crash consistency

Require a dedicated NEEV_DOCUMENT_ROOT, separate from legacy data and agent mounts.
Application-owned root/directories are not writable by guest jobs or other app
users; validate restrictive ownership/permissions and refuse symlinks. Generate
organization-scoped opaque keys on the server; no cross-org deduplication. File
creation uses exclusive/no-follow semantics; reject traversal and symlink targets.
Never mount the store root into an agent job.

Save ordering:

1. Validate bounded UTF-8 body and request; authorize and reserve the exact bytes
   and a short-lived resource write lease in a short transaction.
2. Outside database locks, write a new exclusive temporary blob, flush file data,
   atomically finalize without overwrite, then flush the containing directory.
3. In a new transaction, recheck native authority, live write lease, resource
   audience and expected revisions, then publish immutable revision metadata and
   its new head atomically. Release the lease but retain storage accounting.

An expired lease cannot publish. Competing/stale saves return409. Crash/failure
before publication may leave an unreachable blob; a visible revision must never
point to an incomplete file. Failed reservations remain conservatively charged
until a separately reviewed reconciliation operation; do not free quota while a
possibly finalized blob exists. File errors return actionable failures and preserve
the draft. Retry of a committed idempotency key returns the existing revision.

No blob garbage collection or physical history deletion in the initial release.
This deliberately bounds complexity and makes backup ordering dependable. It is
not secure erasure. Limits count archived/history/orphan-reserved bytes as well
as live content, preventing silent unbounded disk growth.

## Initial limits and failure behavior

Per document revision:1 MiB UTF-8. Per human message:32 KiB UTF-8. Titles:100 Unicode
characters without control characters. Organization:1 GiB total reserved/stored
content bytes (message bodies plus blobs),10,000 documents,10,000 chats,100,000
messages. Message insertion charges the same organization budget transactionally.
Document:1,000 revisions.
List pages:100 items; cursors are scoped and validated before use. A resource
write lease lasts60 seconds; expired leases cannot complete a write or block
sharing indefinitely. Limits are enforced transactionally and return specific
safe UI feedback, never truncate content or silently discard old revisions.

No unreviewed environment switch disables quotas. Raising initial limits is an
operator decision with storage/backup capacity review, not a billing upgrade.
Polling pauses on403/404 and erases loaded resource content; bootstrap401 clears
the whole session boundary. No cross-org cache keys or stale response commits.

## Schema and backup lifecycle

Add a reviewed native-only schema version3 forward migration from the exact
version2 marker, preserving accounts, organizations, memberships and nodes.
Fresh database initializes version3; unknown/native1/legacy schemas remain refused.
Do not invoke the legacy migrator or reset an existing native database. Test
transactional migration/restart and foreign-key constraints on disposable SQLite
and PostgreSQL, including rollback and preserved identity data.

Before deployment, back up native PostgreSQL plus the protected store. With no
deletion or overwrite, dump metadata first and copy all finalized blobs second;
concurrent saves can add harmless extra blobs but cannot remove content needed
by the earlier snapshot. Exclude temporary/incomplete files from restored live
paths. Produce a manifest of referenced keys, sizes and hashes and verify every
reference during an isolated restore before accepting traffic.

Backup credentials and encryption/recovery custody stay outside Git. Existing
Hetzner storage can be evaluated for the destination; this design does not grant
new credentials, paid storage or deletion of old archives/schedules. Retiring any
old backup requires demonstrated replacement coverage and restore evidence.

## UI and release acceptance

Retain Neev branding, visible person/org, Settings return, accessible dialogs,
ten-locale strings and responsive layout. Folder detail lists tabs for Chats and
Documents without exposing hidden counts; context menus create nested folders,
chats or documents. Private/organization badges use text, not color alone.
Empty states explain capabilities honestly. A deterministic accessible-document
index can later seed an organization wiki; AI synthesis is outside this stage.

Tests cover two users/two organizations, hidden titles/counts/cursors, private
admin denial, membership removal during saves, sharing/write races, idempotent
message/save retry, stale-save recovery, restore appends, malicious Markdown and
zero external-image/operator-file requests, symlink/path attacks, process crashes
at each storage boundary, quota races, native migration and isolated backup restore.
Browser acceptance uses real native fixtures for two users, shared messages,
document edits/conflicts/history/restore, privacy changes and375px/keyboard flows.
Synthetic users are not proof of production delivery or provider subscription use.

## Source alignment and self-review

Extends src-tauri/src/neev/{db,catalog,auth/session,http} and native workspace/UI
components. Do not widen legacy ChatDto or operator filesystem authority. The
native identity spec and reviewed session lock/revision contracts remain binding.
Scope excludes binary attachments, CRDT co-editing, public links, private-content
admin recovery, automatic GitHub publication and AI jobs. No undefined retention
deletion or legacy migration is implied. Detailed implementation tasks and exact
DTO/schema definitions follow only after approval of this design.

Context: [org-memory: wiki/engineering/workos.md]; historical GitHub/operator
assumptions are superseded by the approved native direction.
