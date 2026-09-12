# Neev Notion-style documents

Date:2026-09-12. Owner approved the BlockNote/editor-storage direction in
conversation. This written refinement is pending owner review; no editor code or
dependency has been added. Existing native content/identity authorization remains
binding. This extends document editing, not the agent lifecycle plan.

## Product experience

Keep organization → project → recursively nested folder → documents and chats.
A document opens as a clean, readable page inside Neev, with its title, audience,
folder breadcrumb, editing surface, save status and version history. GitHub is not
required. There is no separate login or embedded third-party workspace.

Use the open-source BlockNote core/React UI through pinned packages, retaining
upstream notices and source references. Do not copy a whole Notion clone, its
authentication, data model or telemetry. Avoid XL/GPL/commercial-only packages in
this release. Core is MPL-2.0; any modifications to upstream covered source files
must meet its source-availability requirements. Prefer our own adapter and styles
without changing the upstream source. Verify exact package licenses before install.

Initial rich editing: paragraphs, headings, bold/italic/strike/inline code, safe
links, ordered/bullet/check lists, quotes, code blocks, simple text tables, slash
menu and drag-to-reorder. Expose only blocks that the validated storage/export
adapter supports. No raw HTML, arbitrary embeds, remote image/font fetches,
file uploads, paid AI features or automatic third-party calls. Unsupported blocks
must not silently disappear on save/import/export.

Alternatives: existing Tiptap packages reduce dependencies but require building
more block UI; a complete knowledge-base application introduces unrelated
identity/storage boundaries and possibly incompatible SaaS licensing. BlockNote
is selected for the requested ready-made editing experience.

## Storage and interoperability

New rich documents store versioned, validated structured blocks as their canonical
immutable content. Markdown is an export for agents and portability, not the
round-trip source for rich documents. Preserve block identity and supported
formatting across save/reload; do not serialize rich documents through a lossy
Markdown conversion on each edit.

Existing Markdown revisions remain byte-identical and readable/editable. An
explicit Convert to rich document action previews conversion and appends a new
revision; it never rewrites previous history. If conversion cannot represent
content faithfully, keep Markdown editing and explain the unsupported content.
New revision metadata identifies format and format version. Historical revisions
without the new field retain Markdown meaning through a forward-only native
migration. Never reinterpret legacy databases or merely relabel schema markers.

Stored blocks are untrusted input. The backend independently validates allowed
types/attributes, depth/count and UTF-8 byte limits, rejects unknown fields and
unsafe links, and creates deterministic Markdown exports from accepted content.
The existing1MiB revision limit applies to stored bytes, including the structured
envelope; exported content must also be bounded. A client cannot upload arbitrary
JSON or a second conflicting Markdown body and have either silently trusted.
Files remain protected, immutable, organization-scoped blobs with size/hash
metadata; no direct filesystem paths or public blob URLs enter the browser.

## Save, history and collaboration

Preserve current explicit Save and Cmd/Ctrl+S, in-memory dirty drafts, server
idempotency, expected authorization/content revisions and final access checks.
Typing alone does not create history entries. Navigation confirms dirty-draft
discard; no browser-persistent private drafts. Loading another revision never
overwrites unsaved content. Editor callbacks from stale documents/sessions are
ignored, and the editor is destroyed/cleared when authorization is lost.

A stale save remains409 and preserves the rich draft. Compare shows the current
draft and latest saved revision, with format-aware rendering and safe text export
available. Copy/export recovery must explicitly identify any lossy Markdown
conversion; a structured copy preserves rich content. Restore appends the exact
chosen historical format as a new revision. No overwritten history, automatic
merge or live-cursor/keystroke co-editing claim. Existing five-second bounded
metadata polling remains; importing BlockNote alone does not enable multiplayer
transport or relax the approved conflict-safe collaboration contract.

Document audience remains native private/organization scope, with no admin
override for private documents. Origin-chat documents continue to inherit that
chat's audience on every list/read/history/export/restore. Rich format must not
introduce another authorization path. Sharing, archive, member removal, session
expiry and cross-organization denial retain the same tested semantics.

## Verification and rollout

Test schema upgrades/rollback and readonly historical backup compatibility on
SQLite and real guarded PostgreSQL. Test structured save/reload equivalence,
every supported block, malformed/oversized/deep input, unsafe links, unsupported
conversion refusal, mixed-format history/restore, idempotency and stale-save
draft recovery. Preserve existing source-bound and tenant-isolation tests.

Actual browser acceptance: create nested folder/document; edit using slash menu,
keyboard and drag handle; save/reload; two-user conflict and sharing; archive;
logout/member removal; history/restore;375px layout and focus management. Monitor
network requests to verify no external content/telemetry requests. Existing
Markdown content and history must remain intact. Extend the protected-store
backup verifier and actual isolated restore for rich revisions before release.

Public release is not implied by adding this editor. Provider connections/jobs,
native deployment, real email/provider acceptance, backup custody, public HTTPS
and publication requirements remain independent release gates.

Sources inspected2026-09-12:
- https://github.com/TypeCellOS/BlockNote
- https://github.com/TypeCellOS/BlockNote/blob/main/LICENSE.txt
- https://www.blocknotejs.org/docs
- Existing native content spec2026-09-11 and current native-document.tsx.
