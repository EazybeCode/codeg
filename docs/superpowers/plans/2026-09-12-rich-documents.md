# Neev Rich Documents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Embed the open-source BlockNote editor in native Neev documents without weakening history, privacy, or conflict-safe saves.

**Architecture:** A small Neev-owned block format is validated in Rust, stored as immutable JSON, and adapted to BlockNote in the browser. A native format-metadata table distinguishes historical Markdown from rich revisions. Existing authority, blob leases, quotas, and resource routes remain the only content access path.

**Tech Stack:** Rust/SeaORM/serde_json, React19, TypeScript, BlockNote core/react/shadcn0.54.2 MPL-2.0, aligned Tiptap3.31.3, existing Vitest/native PostgreSQL fixtures.

**Spec:** `docs/superpowers/specs/2026-09-12-notion-style-documents-design.md`.

## Global Constraints

- Keep organization → project → recursively nested folder → documents and chats. GitHub is not required.
- No raw HTML, arbitrary embeds, remote image/font fetches, file uploads, paid AI features or automatic third-party calls.
- New rich documents store versioned, validated structured blocks as their canonical immutable content. Markdown is an export, not the round-trip source for rich documents.
- Existing Markdown revisions remain byte-identical. Conversion and restore append new revisions, never rewrite history.
- The existing1MiB revision limit applies to stored bytes, including the structured envelope; exported content must also be bounded.
- Preserve explicit Save/Cmd/Ctrl+S, in-memory dirty drafts, request idempotency, expected authorization/content revisions, final access checks and409 conflict preservation. No autosave or browser-persistent private drafts.
- Source-chat documents inherit the chat's audience on every access. No private-document admin override, public blob URLs, provider credentials or legacy identity integration.
- Historical schemas3/4/5 are verified read-only without initialization; new schema6 is forward-only. Refuse legacy, partial, unknown, mixed-format metadata inventories. Preserve all prior data.
- No live cursor/keystroke co-editing claim; retain five-second bounded metadata polling.
- One implementation worker/serial Rust build. Use Node22 PATH; CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0. Stop before3GiB available disk. Explicit-file rustfmt only.
- Guarded PostgreSQL: NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1'. Isolated invented fixtures only; retain failed owned fixtures.
- No cloud mutation, real account authorization, dependency auto-fix, push, or publication in this plan. Dependencies only in Task3, with exact versions/license checks.

Exact full native gate (after focused iterations):

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev:: -- --include-ignored --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1
```

## File and interface map

`content/rich.rs` owns bounded Neev JSON decoding, canonical serialization, validation and deterministic Markdown export. Split export into `rich_export.rs` if needed; no frontend/server parser reuse claim.
`db/document_format_migration.rs` creates/backfills the format table. `content/documents.rs` retains the single lease/final-authority save implementation, and `content/backup.rs` verifies body/metadata consistency. `http/content.rs` projects typed document bodies on existing routes. `lib/neev-rich-document.ts` validates client data/adapts basic Markdown; `components/neev/native-rich-editor.tsx` alone imports BlockNote. The existing native document controller owns lifetime, drafts, save/retry/history/authority, not the editor widget.

### Task 1: Validated block format and immutable persistence

**Files:** Create `src-tauri/src/neev/content/{rich.rs,rich_tests.rs,rich_document_tests.rs}` and `src-tauri/src/neev/db/{document_format_migration.rs,document_format_tests.rs}`. Optional focused export file `content/rich_export.rs`. Modify `content/{mod.rs,types.rs,documents.rs,backup.rs,backup_tests.rs}` and `db/mod.rs`; update native schema/backup assertions only where current version changes. Preserve explicit historical fixtures. Update `docs/operations/neev-content-backup.md`.

**Interfaces produced:**

```rust
// content::rich: typed, deny_unknown_fields throughout; no unchecked JSON storage.
pub(crate) struct RichDocument { pub version: u8, pub blocks: Vec<RichBlock> }
// RichBlock uses serde tag="type" with variants listed below. All variants have
// id:String (canonical UUID), children:Vec<RichBlock>, and variant-specific fields.
pub(crate) enum DocumentFormat { Markdown, Blocks }
pub(crate) struct RichSaveInput {
    pub request_id: String,
    pub expected_content_revision: i64,
    pub expected_auth_revision: i64,
    pub document: RichDocument,
}
pub(crate) fn encode(document: &RichDocument) -> Result<Vec<u8>, ContentError>;
pub(crate) fn decode(bytes: &[u8]) -> Result<RichDocument, ContentError>;
pub(crate) fn export_markdown(document: &RichDocument) -> Result<String, ContentError>;
// documents.rs preserves existing save(SaveInput), read, restore signatures.
pub(crate) async fn save_rich(state: &ServerState, actor: &OrgContext,
    id: &str, input: RichSaveInput) -> Result<Revision, ContentError>;
// Revision gains format:DocumentFormat, format_version:u8.
// Markdown=1, blocks=1 only. read still returns stored bytes; consumers interpret
// by metadata, never by guessing whether Markdown happens to look like JSON.
```

**Exact v1 wire/storage contract:** Envelope `{version:1,blocks:[...]}`. Block variants:

```json
{"type":"paragraph","id":"11111111-1111-4111-8111-111111111111","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}
```

Text-bearing variants `paragraph`, `heading`, `bulletListItem`, `numberedListItem`, `checkListItem`, `quote` all have id/content/children. Heading additionally has `level`1..3. Check list additionally has `checked:boolean`. No unrelated fields on other variants. Code block has id/type/children/text/language; language ASCII `[A-Za-z0-9_+-]{0,32}`. Table has id/type/children/rows where rows is rectangular arrays of cells and each cell is an array of inline runs; no rowspan/colspan or styling on cells. Tables and code blocks require empty children. Inline run exactly `{text:string,styles:string[],href:string|null}`; styles are unique canonical order from `bold,italic,strike,code`. Inline code cannot combine other styles; links cannot contain control/whitespace and permit only absolute http/https or fragment `#...`. Validate URLs using existing URL library, including nonempty host, no credentials, no encoded control bypass. No arbitrary attributes/properties/colors/alignment/extensions. Reject unknown fields everywhere, duplicate IDs/styles, unsupported version/type, malformed UUID and wrong variant fields.

Limits:1..1000 total blocks, maximum8levels (top-level=1), maximum10000 total runs,100rows/table,20columns/table, rectangular nonempty tables, no line breaks in table cells. UTF-8 stored envelope and generated Markdown each<=1048576bytes. Text may include tabs/newlines except table cells; reject NUL and other control chars except tab/LF/CR. Validate depth before recursive descent into caller-provided values; serde recursion limit remains enabled. Empty new documents use one empty paragraph, not zero blocks.

- [ ] Write RED format tests against unavailable encode/decode/export stubs and a behavioral save_rich test against an unavailable stub. Verify failures assert the requested behavior, not compilation errors.

```rust
let source = br##"{"version":1,"blocks":[{"type":"paragraph","id":"11111111-1111-4111-8111-111111111111","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}]}"##;
let rich = rich::decode(source).expect("supported rich document");
assert_eq!(rich::encode(&rich).unwrap(), source);
assert_eq!(rich::export_markdown(&rich).unwrap(), "Hello\n");
// Also assert unknown field/type, javascript URL, duplicate IDs,9levels,
// oversized output, malformed table, and incorrect version return Invalid.
```

- [ ] Implement typed decoding and deterministic safe export. Escape Markdown punctuation in plain text, delimit styled text without treating user text as markup, select code fences longer than any body backtick run, indent children, use explicit list/checklist markers and escaped pipe-separated tables. Do not emit HTML. Incremental output budget prevents amplified allocation beyond the limit. Preserve block IDs and run ordering in canonical JSON; never discard unsupported properties to make input fit.
- [ ] Add schema6 `neev_document_formats(org_id,document_id,revision,format,format_version)` composite PK/FK to revisions with RESTRICT. Nonnull positive IDs/revision and CHECK `(format IN ('markdown','blocks') AND format_version=1)`. Create/backfill every historical revision as markdown1 in the same migration transaction. Old tables/blobs/hashes/quotas/receipts untouched. Exactly empty/known2..5 migrate through existing chain then format6;6 reruns unchanged; unknown/partial/mismatched inventories refuse. New format table is part of schema6 exact inventory, absent in3/4/5. Validate one format row per revision at6 initialization/snapshot; invalid/orphan/missing metadata refuses. All service-created revisions insert the format row atomically with publication.
- [ ] Refactor the single save_revision path to accept prepared stored bytes + DocumentFormat internally. Existing save(SaveInput) wraps Markdown unchanged, save_rich validates/encodes before entering the same reservation code. Include format/version in new idempotency hashes without breaking existing Markdown receipts: retain the historical Markdown digest recipe exactly; rich uses a distinct fixed format discriminator. Restore uses selected revision format and exact stored bytes, not Markdown conversion. Revalidate stored rich data before restore; input corruption maps to Storage. Keep all file I/O outside SQL transactions and all existing final lease/session checks.
- [ ] Extend backup read-only validation for schema6 body/format consistency. Historical3/4/5 query path must not query nonexistent format table or mutate archive. Verify all hashes/lengths and every rich envelope. Markdown historical byte validation remains unchanged. Manifest schema_version returns actual inspected version; do not relabel old snapshots.
- [ ] Add SQLite+guarded PostgreSQL mixed-history contract: save Markdown1,rich2; read exact bytes for each; restore1→3 and2→4; verify format/hash/content and source metadata; replay same rich save does not charge twice; changed content/format under same request conflicts; stale head/auth conflicts. Same-origin private/shared docs, foreign/admin denial and authority loss during I/O must exercise rich through existing save/read boundaries. Migration preservation uses populated historical data, not changing only a current marker. Failed fixture retention is before cleanup.
- [ ] Run focused rich/db/backup tests, then the exact full native suite with browser driver excluded. Explicit-file formatting/diff checks, commit only task files and report commands/results. No frontend dependency updates in this task.

### Task 2: Format-aware native HTTP and client contracts

**Files:** Modify `src-tauri/src/neev/http/content.rs`, `src-tauri/src/neev/http/content_tests.rs`, `src/lib/neev-content-client.ts` and `src/lib/neev-content-client.test.ts`. Create `src/lib/neev-rich-document.ts` and `src/lib/neev-rich-document.test.ts`. Mechanically update existing Markdown response fixtures in `src/components/neev/native-document.test.tsx` and `src/components/neev/native-content.test.tsx` to the mandatory metadata/null-body contract; preserve their assertions and defer editor behavior to Task3. In `native-document.tsx`, narrowly type the existing Markdown-only retry ref as `Extract<SaveNativeDocumentInput,{markdown:string}> | null` so the new union compiles without changing its current behavior; Task3 generalizes this controller. Keep backend persistence from Task1 unchanged except a concrete integration fix reported for review.

**Consumes:** Task1 Revision.format/format_version, rich::{decode,encode,export_markdown}, documents::{save,save_rich,read,restore}.
**Produces:** Existing route paths and client function names retained. `NativeRevision` gains mandatory `format:"markdown"|"blocks"`, `formatVersion:1`. Content response becomes `{revision,markdown,document}` with document:null for Markdown and validated RichDocument for blocks; Markdown field for blocks is server-produced export only. Save requests are an exact union: existing Markdown keys OR `{requestId,expectedContentRevision,expectedAuthRevision,document}`. Both/neither bodies are rejected. No new authorization path.

- [ ] Add failing response/parser tests before implementation:

```ts
expect(() => parseDocumentContent({revision: {...revision, format:"blocks", formatVersion:1}, markdown:"Hello\n", document:{version:2,blocks:[]}})).toThrow()
expect(() => parseDocumentContent({revision: {...revision, format:"markdown", formatVersion:1}, markdown:"seed", document:{version:1,blocks:[]}})).toThrow()
```

Use the existing valid revision fixture and independent body hashes/lengths. For rich responses, byteLength corresponds to canonical envelope, not exported Markdown. Verify structural limits and format agreement; browser validation is defense-in-depth, not permission to weaken server validation.

- [ ] Map save union to existing native mutation extractor and exact Origin/CSRF/no-store/request/body-size guards. Parse into typed rich input, no raw JSON blobs accepted. Content read decodes by metadata; rich export endpoint emits safe bounded Markdown with existing attachment/nosniff/private headers. History/restore include exact format/version; corrupt body returns StorageUnavailable, not404 that discards a draft.
- [ ] Implement bounded client RichDocument parser with exact keys and matching shape/limits, in a standalone module without BlockNote imports. `saveNativeDocument` accepts union and preserves request identity, original head/auth and abort behavior. All existing response fixture shapes are mechanically updated to explicit Markdown1/null; no assertions weakened. Reject malformed/unknown formats and cross-org resources unchanged.
- [ ] HTTP tests exercise actual native router rich save/read/download/history/restore, malformed body and unsupported blocks, quota/body bound, Origin/CSRF, foreign/private/source-chat denial, safe errors and headers. Run native focused HTTP and TypeScript client suites, typecheck, diff check; commit and report.

### Task 3: BlockNote editing surface, history and acceptance

**Files:** Create `src/components/neev/{native-rich-editor.tsx,native-rich-editor.test.tsx,native-rich-view.tsx}` and focused `src/lib/neev-rich-adapter.ts`/`.test.ts` if adapter does not fit the format parser's responsibility. Modify `native-document.tsx`/`.test.tsx`, native locale messages and locale consistency tests. Update package.json/pnpm-lock.yaml for the exact BlockNote packages and aligned Tiptap3.31.3; add `docs/operations/neev-rich-documents.md` with versions/licenses/source links and acceptance evidence.

**Consumes:** Task2 format-aware client and RichDocument type. No browser calls to direct blob paths, third-party services or legacy content APIs.
**Produces:** A lazy client-only controlled-lifetime rich editor. Widget props: `document:RichDocument,readOnly:boolean,onChange:(next:RichDocument)=>void`; parent keys the editor by native scope/document/load generation. `NativeRichView` renders read-only canonical structure safely with React, no HTML injection or editor instance needed for history/comparison.

- [ ] Check exact package licenses/peer compatibility from pinned manifests, then add core/react/shadcn0.54.2 MPL-2.0 only. Align all existing direct Tiptap pins to3.31.3; pin next/eslint-config-next16.3.4 to remove the identified vulnerable build dependency without relaxing the existing release-age policy. No XL/commercial package, no Yjs provider/collaboration backend. Preserve lockfile determinism; inspect installation scripts rather than blanket trusting new builds. Dependency audit findings affecting these packages must be resolved or explicitly reachability-triaged, not hidden by audit settings.
- [ ] Write RED adapter tests using every supported block and formatting combination; unsupported BlockNote block/props must throw rather than strip. A round trip `toNeev(fromNeev(document))` retains IDs, structure and supported styles. The editor schema removes unsupported blocks/styles, color/alignment controls, embeds, image/file upload and AI controls. Restrict paste: validate parsed candidate and refuse unsupported content with visible explanation; never silently drop attachments/HTML features. Use only bundled CSS/fonts already installed; no remote emoji/data/syntax-highlighter requests.

```ts
const rich = parseRichDocument(validAllBlocksFixture)
expect(toNeev(fromNeev(rich))).toEqual(rich)
expect(() => toNeev([{id:crypto.randomUUID(),type:"image",props:{url:"https://example.test/a.png"},children:[]}])).toThrow()
```

- [ ] Implement BlockNoteSchema with only allowed block/inline/style specs, and ShadCN BlockNoteView loaded with Next dynamic ssr:false. Adapt Neev canonical blocks to/from package block props. Disable table cell merge/color, font/background, unsupported heading levels and unavailable actions. Custom slash list exposes only supported blocks. Code syntax coloring remains local/plain if no bundled supported tokenizer. Prop changes do not overwrite a dirty editor: parent deliberately remounts only when its existing draft rules permit setBody.
- [ ] Extend document controller draft/base/comparison/retry to a typed body union, retaining existing generation guards and suspension behavior. Empty unsaved documents open rich by default; saved Markdown stays Markdown with explicit Convert to rich document preview/confirm. Parse supported Markdown via existing mdast-util-from-markdown; reject HTML/images/definitions/unsupported styles/structures; do not make a lossy import look successful. Confirmation changes only the in-memory draft; Save appends the revision. Existing Markdown editing remains fully functional.
- [ ] Render format-aware history and conflict comparison. Keep original expected head/auth on stale save, never automatically replace drafts. Offer copy structured JSON for rich recovery and explicitly labeled Markdown export (formatting may be simplified). Restore selected historical format using existing server operation. Clear editor/drafts/dialogs on authority loss; no localStorage/sessionStorage/private draft persistence. Keep title/breadcrumb/audience, keyboard focus, mobile toolbar wrapping and readable page width consistent with existing native shell.
- [ ] Test mounted document controller rich save/reload, lost-response exact retry,409 draft preservation, stale callback after navigation/logout, keyboard Save, conversion refusal/preview, mixed history/restore and source audience. Use actual editor in targeted widget tests where DOM supports it; controller mocks cannot replace actual browser acceptance.
- [ ] Run all native frontend suites, typecheck and static build, targeted legacy editor tests affected by Tiptap upgrade, audit, and actual isolated browser acceptance on375px and desktop: slash edit, drag reorder, save/reload/history, two-user conflict/share, archive and authority loss. Observe network to verify no unsolicited external requests. Use synthetic Google/mail fixtures without real credentials. Record blocked browser infrastructure explicitly; never mark a test passed because its fixture exited normally.
- [ ] Extend actual isolated database/blob restore rehearsal with at least one rich and one Markdown revision using built verifier; historical schema3 retained archive still verifies unchanged. Document actual commands/manifests and rollback (preserve database+blobs, no down migration). Commit exact files; task review then whole-plan review precedes deployment.

## Plan review checklist

Controller dependency correction: the initially selected Next16.3.5 was published
September11 and the unchanged package-manager age policy rejects it on September12.
Use16.3.4, published August31, instead of adding release-age exceptions. Both
identified critical advisories mark16.3.3 patched; the registry advisory query for
next/eslint-config-next16.3.4 returned no direct advisories. This does not certify
the transitive graph; Task3 still audits that graph and tests the actual build.
Sources: [Windows advisory](https://github.com/advisories/GHSA-p293-qw3h-jr36),
[AVIF advisory](https://github.com/advisories/GHSA-2xp9-vwfh-vxw4),
[16.3.4 release](https://github.com/vercel/next.js/releases/tag/v16.3.4),
[16.3.5 release](https://github.com/vercel/next.js/releases/tag/v16.3.5).

- Task1→Task2: Revision metadata and rich body signatures agree; old Markdown save/read remain wrappers over a single authority path.
- Task2→Task3: DTO fields are mandatory and body union is exact; every affected test fixture must be updated rather than accepting multiple unchecked shapes.
- Task1→Task3: default new rich document has one paragraph; conversion appends through normal Save; restore preserves original bytes and format.
- Migration and backup: new table distinguishes6 inventory from5 without ALTERing old revision blobs; backfill happens once transactionally. Historical verification does not initialize.
- Security tests cover both formats and source-bound access; editor validation is not an authorization decision.
- Deployment/provider jobs are separate release gates and are not declared complete by this plan.
