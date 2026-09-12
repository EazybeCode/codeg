# Neev Native Chats and Documents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Native members collaborate on private/organization chats and versioned Markdown documents inside the existing nested project/folder hierarchy without GitHub.

**Architecture:** Native PostgreSQL metadata owns resource identity, authorization, revisions, quotas and leases. An application-only immutable blob store supplies document bytes; short transactions reserve and publish around filesystem work. Existing native sessions, HTTP protections and frontend auth invalidation remain authoritative; legacy routers and AI execution stay disabled.

**Tech Stack:** Existing Rust/Axum/SeaORM/SQLite+PostgreSQL tests, Tokio, SHA-256, UUID, Next/React, next-intl, Vitest; Node22.23.2. No new runtime dependency without a concrete need and controller ruling.

**Spec:** `docs/superpowers/specs/2026-09-11-native-chats-and-documents-design.md`

## Global Constraints

- Organization → project → recursively nested folder remains the only hierarchy. Every chat/document belongs to one existing native node and org.
- "Administrators have no private-content override."
- "No GitHub repository, branch or organization is deleted or modified."
- "Every user brings their own AI-tool subscription; no shared/operator AI credential fallback."
- "No edit/delete of individual historical messages in this stage."
- "Do not persist private drafts to localStorage."
- "No blob garbage collection or physical history deletion in the initial release."
- "An expired lease cannot publish."
- Final-review schema amendment: fresh databases initialize through3 then4; unknown/native1/legacy schemas remain refused.
- "No unreviewed environment switch disables quotas."
- "RuntimeEnabled:false" remains authoritative; do not simulate agent replies, enable jobs, attach GitHub or expose legacy files.
- Body: document1MiB UTF-8; message32KiB UTF-8; title100 Unicode scalar values without controls. Organization1GiB charged bytes,10,000 documents,10,000 chats,100,000 messages. Document1,000 revisions. Page100. Lease60s. Poll5s only while visible.
- Existing positive i32 user/org/node IDs remain; resource/message IDs are canonical random UUID strings. Content/auth revisions are nonnegative/positive i64 respectively, canonical decimal strings on HTTP. No database row or blob key serialized to clients.
- Lock order: admission guard → existing session::recheck account/org/member/session locks → resource rows in UUID lexical order. Never hold these locks across filesystem/provider/network work. All resource writes take admission guard; existing membership changes remain serialized by final recheck.
- Unauthorized resource IDs/cursors return the same404 as missing; hidden rows filtered before pagination. Expired session401, mutation CSRF/origin protections unchanged, all responses no-store. Content privacy does not depend on node visibility.
- Origin-bound docs recheck source chat on every read/write/list/download. No detach. Chat audience/archive changes atomically increment attached docs' auth revisions and reject any active source/attached write lease. Attached document audience cannot change independently.
- Work only in existing `.context/workos-src`, branch unchanged. Preserve untracked `.context/`. Source origin codeg must never receive a push; publication only reviewed Workos flow with interactive ship. No external writes by implementers.
- Rust commands MUST include `--no-default-features`, existing target, `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1`. Stop/report before <3GiB free; no guessed cleanup. Native test PG URL must be exact `postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1`; owned UUID schemas only. Do not invoke default desktop features.
- Tests first with observed behavioral RED, then minimal implementation. Build focused tests while iterating and run full native suite once per backend task before commit. Reviewers do not duplicate suite runs. Main controls browser exclusively.
- Full native suite means the `neev::` test filter, with `--include-ignored --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1`; never the entire legacy library suite. Preserve the guarded PostgreSQL URL. Use `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` consistently on rebuilt native test commands. Format only explicit task files with `rustfmt --config skip_children=true`, never whole-crate `cargo fmt`.

## File responsibility map

`neev/db/content_migration.rs`: version3 table/index definitions, version2 upgrade, and version4 source-link guards.
`neev/content/types.rs`: typed inputs/results and limits; no HTTP serialization.
`neev/content/access.rs`: scoped transaction/resource authorization and budgets.
`neev/content/resources.rs`: create/list/get/metadata transitions for chat/doc rows.
`neev/content/messages.rs`: append-only human discussion and cursor reads.
`neev/content/blob.rs`: restrictive immutable filesystem store, no authorization decisions.
`neev/content/documents.rs`: reserve/write/final-publish and authorized revisions/restore.
`neev/http/content.rs`: strict native DTOs/routes/errors; no SQL or user paths.
`src/lib/neev-content-client.ts`: validated wire shapes and bounded content requests.
`src/components/neev/native-content.tsx`: folder content list and resource selection.
`native-chat.tsx`, `native-document.tsx`, `native-markdown.tsx`: discussion, editor/history, safe renderer.
`neev/content/backup.rs` + native backup binary: metadata manifest/store verification without network credentials.
Tests stay beside production units or in dedicated `*_tests.rs`; fixture utilities under cfg(test).

### Task 1: Version3 content schema and typed contracts

**Files:** Create `src-tauri/src/neev/db/content_migration.rs`, `src-tauri/src/neev/content/{mod.rs,types.rs}`, `src-tauri/src/neev/db/content_tests.rs`. Modify `neev/{mod.rs,db/mod.rs,db/migration.rs,db/tests.rs}`. Preserve existing v2 definitions as the exact upgrade input.

**Interfaces:** Produces `content::types::{Resource,ResourceKind,Visibility,ResourceInput,ResourcePatch,Message,Revision,SaveInput,ContentError,Page}`. Produces version4 initialization through existing `db::initialize`, including exact3 forward hardening; no startup/config change yet.

- [ ] Write failing migration tests using existing `test_support::database`/guarded PG helpers: initialize exact v2 tables and seed an account/org/member/node, run initialize, assert version4 plus unchanged IDs/name, run again idempotently; reconstruct historical3 and verify populated3 upgrades4 while malformed3 refuses unchanged; insert/update half-NULL and invalid source references and assert constraints reject; unknown/version1/extra/partial schema remains refused with no new tables. Inject an invalid upgrade statement in a test-only transaction to prove rollback leaves v2 and its data intact.

Implement test-local query helpers `schema_version(&DatabaseConnection)->i64`,
`node_name(&DatabaseConnection,i32)->String`,
`insert_cross_org_resource(&DatabaseConnection)->Result<(),AuthError>` and
`insert_document_revision_for_chat(&DatabaseConnection)->Result<(),AuthError>`
using real `db::one`/`db::execute` and fixed fixtures. They are not production APIs.

```rust
// Assertions in the SQLite and PostgreSQL upgrade tests, against real queries:
assert_eq!(schema_version(&db).await, 3);
assert_eq!(node_name(&db, original_node_id).await, "Marketing");
assert!(insert_cross_org_resource(&db).await.is_err());
assert!(insert_document_revision_for_chat(&db).await.is_err());
```

- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev::db:: -- --test-threads=1` with mandated build env. Record actual behavioral RED (not a missing import).
- [ ] Add these exact logical tables with backend-neutral SeaQuery types/checks; use no cascading deletion. Existing tables/columns stay unchanged:

```text
neev_resources: org_id i32, id text UUID, kind text(chat|document), node_id i32,
 creator_id i32, title text, visibility text(private|organization),
 origin_chat_id nullable text, origin_kind nullable text,
 auth_revision bigint>=1, content_revision bigint>=0, archived bool,
 created_at bigint>=0, updated_at bigint>=0.
 PK(org_id,id), UNIQUE(org_id,id,kind), FK(org_id,node_id)->neev_nodes(org_id,id),
 FK creator_id->neev_accounts(id), FK org_id->neev_organizations(id),
 FK(org_id,origin_chat_id,origin_kind)->neev_resources(org_id,id,kind).
 CHECK (origin_chat_id IS NULL AND origin_kind IS NULL) OR
       (origin_chat_id IS NOT NULL AND origin_kind IS NOT NULL
        AND kind='document' AND origin_kind='chat').
neev_messages: org_id, chat_id text, resource_kind text CHECK=chat, id text UUID,
 sequence bigint>=1, author_id i32, body text, byte_length bigint 1..32768,
 created_at bigint>=0; PK(org_id,chat_id,sequence), UNIQUE(org_id,id),
 FK(org_id,chat_id,resource_kind)->neev_resources(org_id,id,kind), author FK.
neev_document_revisions: org_id, document_id text, resource_kind text CHECK=document,
 revision bigint 1..1000, blob_key text, sha256 text, byte_length bigint 0..1048576,
 author_id i32, created_at bigint>=0, parent_revision nullable bigint,
 restored_from nullable bigint; PK(org_id,document_id,revision),
 UNIQUE(org_id,blob_key), FK(org_id,document_id,resource_kind)->resources,
 FK(org_id,document_id,parent_revision)->same table,
 FK(org_id,document_id,restored_from)->same table; author FK.
neev_content_usage: org_id PK/FK, charged_bytes bigint0..1073741824,
 chats bigint0..10000, documents bigint0..10000, messages bigint0..100000.
neev_content_leases: org_id, resource_id, id text UUID, actor_id i32,
 request_id text UUID, body_hash text, blob_key text,
 reserved_bytes bigint0..1048576, expected_content_revision bigint>=0,
 expected_auth_revision bigint>=1, expires_at bigint>=0;
 PK(org_id,resource_id), UNIQUE(org_id,id), FK(org_id,resource_id)->resources,
 actor FK. Expired row can be replaced; charged_bytes is never decremented here.
```

Indexes: resources `(org_id,node_id,kind,archived,id)` and `(org_id,origin_chat_id,id)`; messages/revisions primary keys supply ordered reads. `neev_requests` is reused for actor/action/request UUID/body-hash/resource receipts; content receipts have no expiry cleanup in this stage (use maximum i64 expiry). Receipt resource IDs encode resulting resource/message/revision identity, never content bytes.

```rust
pub(crate) enum ResourceKind { Chat, Document }
pub(crate) enum Visibility { Private, Organization }
pub(crate) struct Resource {
 pub id:String, pub org_id:i32, pub node_id:i32, pub kind:ResourceKind,
 pub creator_id:i32, pub title:String, pub visibility:Visibility,
 pub origin_chat_id:Option<String>, pub auth_revision:i64,
 pub content_revision:i64, pub archived:bool, pub created_at:i64, pub updated_at:i64,
}
pub(crate) struct ResourceInput {
 pub node_id:i32, pub kind:ResourceKind, pub title:String,
 pub origin_chat_id:Option<String>, pub request_id:String,
}
pub(crate) struct ResourcePatch {
 pub expected_auth_revision:i64, pub title:String, pub node_id:i32,
 pub visibility:Visibility, pub archived:bool,
}
pub(crate) struct Message {
 pub id:String, pub sequence:i64, pub author_id:i32,
 pub author_name:String, pub body:String, pub created_at:i64,
}
pub(crate) struct Revision {
 pub revision:i64, pub author_id:i32, pub author_name:String,
 pub byte_length:i64, pub sha256:String, pub created_at:i64,
 pub parent_revision:Option<i64>, pub restored_from:Option<i64>,
}
pub(crate) struct SaveInput {
 pub request_id:String, pub expected_content_revision:i64,
 pub expected_auth_revision:i64, pub markdown:String,
}
pub(crate) struct Page<T> { pub items:Vec<T>, pub next_after:Option<String> }
pub(crate) enum ContentError {
 Auth(crate::neev::error::AuthError), NotFound, Invalid, Conflict,
 Quota, Archived, Storage,
}
```

- [ ] Final-review amendment: initialize fresh and exact2 through3 then4; exact3 upgrades4; exact4 is idempotent. Validate existing source pairs before advancing the marker; refuse malformed3 transactionally. PostgreSQL adds a named validated CHECK; SQLite adds NULL-safe INSERT/UPDATE guards without rebuilding the FK-linked table. Read-only backup verification accepts exact3/4 and reports the actual version. Do not silently repair partial/unknown/legacy schemas. Preserve PG advisory initialization lock and SQLite transaction behavior.
- [ ] Run database tests including ignored guarded PG cases, then full native suite once; update obsolete v2-only test expectations with explicit v2→3 preservation tests. Commit exact task files.

### Task 2: Protected immutable blob store and startup configuration

**Files:** Create `neev/content/{blob.rs,blob_tests.rs}`; modify `neev/{config.rs,startup.rs,test_support.rs,browser_fixture.rs}`, `content/mod.rs`, native launcher/preflight fixtures only where required by mandatory root configuration.
**Interfaces:** `BlobStore::open(root:&Path)->Result<Self,ContentError>`, `put(org_id:i32,key:&str,bytes:&[u8])->Result<BlobInfo,ContentError>`, `read(org_id:i32,key:&str,expected_size:i64,expected_sha256:&str)->Result<Vec<u8>,ContentError>`. `BlobInfo { key:String, sha256:String, byte_length:i64 }`. Add `ServerState.blobs:Arc<BlobStore>` and `Config.document_root:PathBuf` (`NEEV_DOCUMENT_ROOT` mandatory). Tests own TempDir via a fixture guard; no default to user home/repository.

- [ ] Write real temp-directory tests for traversal/noncanonical key refusal, root/parent/file symlinks, permissive ownership/mode refusal, duplicate key no-overwrite, read size/hash mismatch and zero-byte valid Markdown. Verify existing destination bytes remain identical after a rejected duplicate write.

```rust
let original = b"# First revision";
store.put(1, &key, original).await.unwrap();
assert!(store.put(1, &key, b"replacement").await.is_err());
assert_eq!(store.read(1, &key, 16, &expected_hash).await.unwrap(), original);
assert!(store.put(1, "../outside", original).await.is_err());
```

- [ ] Observe RED with focused native blob tests; do not count trivial missing-module failures as behavioral RED.
- [ ] Use canonical UUID key and positive org ID path components only. Open application-owned root and directories with no-follow semantics, validate ownership=current effective UID and mode0700 for directories/0600 files on deployed Unix. Reject non-Unix deployment rather than silently weakening checks. Use existing libc/nix capability if installed, no shell commands. Operate relative to verified directory FDs (`openat`, `mkdirat`, no-follow) to prevent path-swap races. Dedicated root cannot equal static root or an ancestor/descendant of it; no automatic chmod of user directories.

```text
root/<org-id>/<uuid>.tmp  exclusive0600 create → bounded write → sync_all
  → no-overwrite finalize to <uuid>.md → directory sync_all → BlobInfo.
read: verified directory descriptors → no-follow regular file → bounded bytes
  → exact recorded size/hash verification. Never return physical paths to HTTP.
```

No public failpoint knobs. Inject file-operation failure hooks only under cfg(test), checking failure before/after write/sync/finalize/directory-sync never overwrites published bytes. Main production startup validates root before accepting HTTP. Extend test helpers with owned0700 temp roots and update config tests to reject absent/relative/symlink roots.

- [ ] Run focused config/blob tests and full native suite; format touched launcher tests if changed; commit. Report platform assumptions and actual disk footprint.

### Task 3: Resource authority, sharing and human conversations

**Files:** Create `neev/content/{access.rs,resources.rs,messages.rs,resource_tests.rs,message_tests.rs,test_support.rs}`; modify `content/mod.rs` and expose only required existing helpers from `org/service.rs` (`digest_parts`, `normalize_request_id`) if reuse is safer than duplication.
**Interfaces:** All functions return `Result<_,ContentError>`, consume `&ServerState,&OrgContext`. `resources::{create(input:ResourceInput)->Resource,get(id:&str)->Resource,list(node_id:i32,kind:ResourceKind,archived:bool,after:Option<&str>)->Page<Resource>,update(id:&str,patch:ResourcePatch)->Resource}`. `messages::{append(chat_id:&str,request_id:&str,expected_auth_revision:i64,body:&str)->Message,list(chat_id:&str,after:Option<i64>)->Page<Message>}`. `access::authorized(tx:&DatabaseTransaction,actor:&OrgContext,id:&str,write:bool)->Resource` requires caller to hold admission/session locks; verifies origin chat for documents and archive semantics.

- [ ] Write real SQLite+PG tests with two orgs/three actors: private admin denial, all foreign IDs404, hidden titles/counts/cursors, member-removal final-write denial, same UUID retry and conflicting body, org discussion authorship/sequence, same-org node move only, creator-only audience/archive, archived read/no append, usage ceilings and two concurrent inserts at final quota boundary.

```rust
assert!(matches!(resources::get(&state,&other_org,&chat.id).await, Err(ContentError::NotFound)));
assert!(matches!(resources::get(&state,&same_org_admin,&chat.id).await, Err(ContentError::NotFound)));
let first = messages::append(&state,&creator,&chat.id,&request,1,"Hello team").await.unwrap();
let retry = messages::append(&state,&creator,&chat.id,&request,1,"Hello team").await.unwrap();
assert_eq!(first.id,retry.id);
assert_eq!(first.author_id,creator.identity.user_id);
```

- [ ] Observe RED then implement private-by-default creation, canonical IDs/100-character titles, transactionally initialized usage row, receipt binding to full action/org/node/kind/title/source/body. Receipt replays still require current authority. Filter audience before100+1 pagination. Cursors are base64url JSON `[org_id,node_id,kind,archived,last_id]`, validated against current requested scope and currently visible last row; malformed/foreign/hidden cursors404. No cursor content contains private title/count.
- [ ] Chat metadata updates lock source+attached resources in canonical order; verify no unexpired source/attached leases; update attached auth revisions atomically. Attached documents inherit audience through source on every access; creator-only document placement/archive remains separate. Disallow origin field update and independent origin-bound visibility mutation. Metadata changes use expected auth revision and bump it (including rename/move) to prevent lost updates.
- [ ] Message insertion validates nonempty≤32768-byte UTF-8 and records server actor/time/sequence, charges org bytes/message counters in same tx, updates chat content revision, writes durable receipt. Reads join author displayName server-side, never accept author or role from client. No assistant/tool variants yet.
- [ ] Verify focused tests plus full native suite, preserving identity gates; commit exact files.

### Task 4: Conflict-safe documents, immutable history and restore

**Files:** Create `neev/content/{documents.rs,document_tests.rs}`; modify `content/mod.rs` and access helper only for exact shared lease/resource locking.
**Interfaces:** `documents::save(state,actor,document_id:&str,input:SaveInput)->Revision`; `history(state,actor,id,after:Option<i64>)->Page<Revision>`; `read(state,actor,id,revision:Option<i64>)->(Revision,Vec<u8>)`; `restore(state,actor,id,source_revision:i64,request_id:&str,expected_content_revision:i64,expected_auth_revision:i64)->Revision`. Return ContentError. New document starts head0; first save creates revision1.

- [ ] Tests: saved bytes survive store reopen, two stale writers yield one success/one409 and preserve first content; restore appends revision3 referencing revision1 and leaves1/2 untouched; same key returns same revision without new charge; private/source-chat audience denies all history/content/download variants; expired lease cannot publish; removal/sharing races at paused filesystem boundary; orphan reservation charged after each injected failure; exact1MiB/1000 revisions/1GiB quota concurrent boundaries.

```rust
let revision = documents::save(&state,&alice,&doc.id,input.clone()).await.unwrap();
assert_eq!(revision.revision,1);
assert!(matches!(documents::save(&state,&bob,&doc.id,stale).await, Err(ContentError::Conflict)));
assert_eq!(documents::history(&state,&alice,&doc.id,None).await.unwrap().items.len(),1);
```

Do not derive Clone on secret structs; SaveInput may clone in tests or be reconstructed explicitly.

- [ ] Observe RED; implement reserve→put→publish. Reservation transaction takes admission guard/session authority and resource/source rows, checks archive/auth/head/1000 revisions, existing committed receipt, lease expiry and bytes budget, charges bytes and stores60s lease+body digest+server blob key. Commit before `BlobStore::put`. A same-request active lease conflicts rather than launching a second write; expired takeover charges anew, never refunds abandoned bytes.
- [ ] Publish in a new admission/session/resource transaction, require same live lease ID/actor/body/key/expected revisions and current originating audience, insert revision and receipt, update head/time, delete only exact lease. No I/O in tx. A DB failure leaves charged orphan and cannot expose a partial revision.
- [ ] History/read authorize transactionally, capture metadata, release locks for bounded store read, then recheck authority/auth revision before returning bytes. Restore reads an authorized revision and calls same guarded save path with restored_from recorded and request digest including source revision; it creates a fresh immutable blob/revision. Any source privacy change during restore denies publication.
- [ ] Run focused failure/race tests and full native suite; commit exact files and report lock/write ordering evidence.

### Task 5: Strict native content HTTP and validated client adapter

**Files:** Create `neev/http/{content.rs,content_tests.rs}`, `src/lib/neev-content-client{.ts,.test.ts}`; modify `neev/http/{mod.rs,catalog.rs}`, `src/lib/neev-client{.ts,.test.ts}` and capability fixtures/tests touched by the new flags.
**Interfaces:** Native capability catalog adds `documents:true` and changes `chats:true` only with implemented routes; `runtimeEnabled:false` and repositoryDefaults:false unchanged. Existing version1 native client accepts bool chats/documents, safely treats missing documents as false for pre-stage2 fixture compatibility. Services from Tasks3/4 supply all authority.

```text
GET/POST /api/neev/nodes/:nodeId/chats
GET/POST /api/neev/nodes/:nodeId/documents
GET/PATCH /api/neev/resources/:id
GET/POST /api/neev/chats/:id/messages
GET /api/neev/documents/:id/revisions
GET /api/neev/documents/:id/content?revision=<canonical positive decimal>
GET /api/neev/documents/:id/download?revision=<canonical positive decimal>
POST /api/neev/documents/:id/revisions
POST /api/neev/documents/:id/restore
```

List response `{items:[...],nextAfter:string|null}`. Creation input `{title,requestId,originChatId?}`; originChatId only valid for documents. Patch `{expectedAuthRevision,title,nodeId,visibility,archived}`. Message `{requestId,expectedAuthRevision,body}`. Save `{requestId,expectedContentRevision,expectedAuthRevision,markdown}`. Restore `{requestId,expectedContentRevision,expectedAuthRevision,sourceRevision}`. Resource DTO all Resource fields except internal origin_kind; effective audience only. Revision DTO as typed contract, strings for all revisions/byteLength, no blob key. Content JSON `{revision:RevisionDto,markdown:string}`. Downloads `text/markdown;charset=utf-8`, `attachment; filename="document.md"`, nosniff/no-store.

- [ ] Write native HTTP tests for strict unknown-field/author injection denial, anonymous401, wrong-origin/CSRF denial, invisible404 including cursors, size400, quota429 with `content_quota`, archive409 with `archived`, conflict409, safe500/503 `storage_unavailable`,30s body deadline reuse. Add client parser tests rejecting malformed UUID/revisions/node IDs and never including raw body/errors in logs.

```typescript
expect(() => parseResource({ ...valid, authRevision: "01" })).toThrow()
expect(() => parseResource({ ...valid, orgId: -1 })).toThrow()
expect(parseResource(valid).visibility).toBe("private")
```

- [ ] Observe RED; implement dedicated DTOs deny_unknown_fields, canonical UUID/revision helpers and shared `http/catalog` organization auth helpers exposed pub(super). Add explicit body-limit parameter to org mutation helper; use bounded JSON collection limit `6*1048576+4096` to accommodate escaped1MiB Markdown, followed by decoded-byte validation. No body logs, request paths in errors, physical files or legacy routes.
- [ ] Add `neev-content-client.ts` DTO types matching the wire, `contentGet/contentMutation` with existing AbortSignal/sessionCSRF semantics and65s client timeout; classify404 separately so UI clears only resource while401 clears session. Validate response scope IDs against requested org/node/resource before committing state. Do not weaken existing native parser for operator/team modes.
- [ ] Run focused native HTTP/client tests, full native suite, TypeScript and changed-file lint/format; commit. Retain earlier fixture tests by updating capability expectations truthfully, not suppressing them.

### Task 6: Notion-inspired folder content, chat and Markdown history UI

**Files:** Create `src/components/neev/{native-content.tsx,native-content.test.tsx,native-chat.tsx,native-chat.test.tsx,native-document.tsx,native-document.test.tsx,native-markdown.tsx,native-markdown.test.tsx}`, `src/hooks/use-neev-content-poll.ts`; modify native-workspace.tsx/tests and ten `src/i18n/messages/*.json` files. Native-auth-provider changes only if a typed error invalidation hook is needed; preserve its generation/cancellation contract.
**Interfaces:** `NativeContent({node:NativeNode,onRequestNavigationGuard})` owns selected chat/doc within node; `NativeChat({resource,onBack,onChanged})`; `NativeDocument({resource,onBack,onChanged,onDirtyChange})`; `NativeMarkdown({markdown:string})`. Poll hook takes auth signal+generation, resource scope and callback; never stores cross-org/private bodies in shared/global caches.

- [ ] Write real component tests with strict URL/body fetch boundaries: tabs/actions only inside selected node; create private chat and human message author/time; disabled Ask agent plus personal-connection Settings link; explicit sharing confirmation naming org/full history; archive/reopen/move creator actions; documentSave shortcut/history compare/restore;409 retains draft; unsaved navigation/reload confirmation;401/authgeneration and403/404 clear old content; polling only visible with5s fake timers, cleanup on unmount/logout; responsive controls/keyboard focus. Test real UI, not mock markers.

```typescript
await user.click(screen.getByRole("button", { name: "Save" }))
expect(screen.getByRole("textbox", { name: "Document content" })).toHaveValue(
  "my unsaved changes"
)
expect(
  screen.getByRole("button", { name: "Compare with latest" })
).toBeVisible()
expect(
  screen.queryByText("other organization's secret")
).not.toBeInTheDocument()
```

- [ ] Observe RED then build compact tabs, restrained typography/spacing, breadcrumbs/back, text privacy badges, visible org/person retained. Context menus New subfolder/New chat/New document pass explicit nodeId; no global creation. Reuse dialogs/menu/buttons/focus behavior from native components. Poll incremental messages using server sequence; refresh resource metadata/revisions first to detect audience/archival changes. Resources denied by403/404 are erased and polling stopped.
- [ ] Markdown editor is explicit controlled textarea+preview, not rich-text/CRDT. Use installed Markdown parser only with raw HTML disabled and custom element allowlist; if none safe, implement escaped plain Markdown preview first with explicit supported subset. Never mount img/iframe/object/embed/style/script or remote font nodes. Safe link URL parser allows only absolute http/https and anchors rendered without script/data/custom protocols, with noopener/noreferrer. Remote images render inert label + explicit safe external link. Compare uses escaped text lines, not unsafe HTML. Test malicious payloads and instrument zero external image fetches.
- [ ] Include all new visible/accessibility strings in ten existing locales; native locale test enforces keys and placeholder parity. Save state and drafts exist only in component memory. Handle resource selection, folder selection, Settings navigation and beforeunload unsaved guards without blocking logout or exposing stale private drafts. Explicit reload latest/discard dialogue; Copy draft only on intentional click via normal clipboard API.
- [ ] Run focused native component/client/locale tests, TypeScript/lint/format and then one bounded2-worker full frontend run while Rust idle. Commit exact files; provide screenshots only from main browser acceptance later, no self-certified visual claims.

### Task 7: Native content backup manifest and isolated restore verification

**Files:** Create `neev/content/{backup.rs,backup_tests.rs}`, `src-tauri/src/bin/neev-content-verify.rs`, `scripts/neev-content-backup.mjs`, `scripts/neev-content-backup.test.mjs`, `docs/operations/neev-content-backup.md`; modify `neev/mod.rs`, `src-tauri/Cargo.toml` only for native binary entry. No cloud/provider/key mutation.
**Interfaces:** `content::backup::verify_snapshot(db:&DatabaseConnection,store:&BlobStore)->Result<Manifest,ContentError>` reads all referenced revision org/key/hash/size and verifies files; manifest contains no Markdown/user emails/auth secrets. CLI consumes explicit restored database/root and emits bounded count/hash summary, refusing production URL by requiring a named isolated destination and operator confirmation flag. Backup script accepts explicit private PostgreSQL URL via environment and absolute new output directory; never logs URL.

- [ ] Write tests using disposable stores and PG schemas: valid restore accepted, missing/corrupt/cross-org blob rejected, orphan extra files ignored, temp files excluded, manifest deterministic, metadata dump precedes final blob copy. Script test uses controlled child processes with recorded argv/order and verifies credential env not stdout/argv; no real network destination.

```javascript
assert.deepEqual(
  events.map((e) => e.operation),
  ["pg_dump", "copy-finalized", "manifest"]
)
assert.equal(result.stdout.includes("postgresql://"), false)
assert.equal(result.exitCode, 0)
```

- [ ] Observe RED; build backup into new restrictive directory only, dump metadata first with pg_dump custom format from env, copy only verified final files without mutation/GC, atomic complete manifest marker after success. Interrupted runs remain explicitly incomplete; refuse overwriting an existing output path. Never attach encryption keys or provider credentials to the artifact. Document separate recovery custody.

The copy manifest inventories copied finalized blobs and the database-dump hash;
it may include harmless orphan/late blobs. Do not generate reference metadata from
a newer live DB after copying. The referenced-key manifest is generated by the
verifier against the restored dump, and every referenced file must match copied
bytes before restore succeeds. This separates snapshot membership from copy time.

- [ ] Restore acceptance uses separate exact test database/schema and fresh store; pg_restore/import metadata then verifier checks every referenced hash/size and native schema marker before accepting success. Do not restore over original DB, retire old archives, create StorageBox credentials or claim hosted backup readiness.
- [ ] Run Node script tests and focused native backup tests plus full native gate. Commit, document deployment operator sequence and test evidence; main performs real isolated local restore using disposable named resources.

### Task 8: End-to-end content acceptance and release gate update

**Files:** Modify `neev/browser_fixture.rs`/test helpers for bounded content fixtures, `docs/operations/{neev-native-identity-acceptance.md,public-release-checklist.md}`; create `docs/operations/neev-native-content-acceptance.md` and `scripts/neev-content-preflight{.mjs,.test.mjs}`.
**Interfaces:** Existing synthetic Google/email native fixture gains owned store roots with exact cleanup; no production seed/runtime fixture flag. Preflight receives explicit loopback origin and native session only from consenting local test fixture, never reads real browser cookies or dumps tokens. Main controls actual browser.

- [ ] Write regression for two native participants/orgs sharing messages/documents, source-chat privacy revocation, conflict/restore and post-removal denial through real HTTP. Bounded request and overall fixture deadlines guarantee abort/await before exact schema/store cleanup. Own temp-root cleanup is test-only and never targets a configured production directory.

```javascript
assert.equal(sharedMessage.author.id, member.id)
assert.equal(hiddenResourceResponse.status, 404)
assert.equal(staleSaveResponse.status, 409)
assert.equal(restoredRevision.parentRevision, "2")
assert.equal(restoredRevision.restoredFrom, "1")
```

- [ ] Observe RED then implement only needed fixture/preflight support. Main browser acceptance: create project/nested folder; private/chat org share; two-user author rows; create/edit/save doc; conflicting draft preserved; compare/restore appends; change privacy and erase stale loaded content; keyboard dialogs/save/escape/back; actual375px measurement; malicious Markdown no network/image/operator paths; browser screenshots for visual defects. Real provider or public use is not proven by synthetic fixture.
- [ ] Run full native with ignored PG (skip exact interactive driver), full frontend with2workers and no concurrentRust, TypeScript/lint/format, static export and native production binary build with mandatory document root test contract. Main exact binary smoke and local isolated backup restore; review exact source diff and secret findings. Do not hide baseline warnings or retest failures as successes.
- [ ] Update release checklist: native identity/content locally accepted only when evidence exists; personal-agent/durable-job stage, domain/mail/Google/live HTTPS, website and hosted backup gates remain explicit. Commit exact files; report fixture IDs/cleanup and browser handoff to main, not an unsupported all-done claim.

## Self-review and execution

Coverage: schema/tenancy1+3; protected files2; leases/history/conflicts4; HTTP isolation5;
Notion-like async UX/safe Markdown6; metadata-first backup7; real browser/native gates8.
Interfaces use the same Resource/Revision fields and strict UUID/i64 wire formats.
No whole-repo refactor, AI credentials, mail DNS or provider changes are delegated.
The user requested continuous completion and approved the written design; continue
the existing subagent-driven execution workflow without another execution-choice
pause. External login/credential/security actions retain their own approval gates.

Context: [org-memory: wiki/engineering/workos.md]; historical GitHub/operator mode
is not the current native SaaS authority model.
