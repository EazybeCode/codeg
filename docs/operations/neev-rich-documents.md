# Native rich documents

Task 3 implementation, September 12, 2026. This document distinguishes automated
checks from the controller's actual synthetic browser and isolated recovery checks.
Scoped automated checks, actual synthetic browser acceptance and mixed-format
recovery passed, with the limitations below. Scoped task re-review is clean;
whole-plan review remains a release gate. No deployment or real provider-account
authorization is included.

## Formats and operation

New empty documents open as a version-1 structured document with one paragraph.
Explicit Save (including Cmd/Ctrl+S) appends an immutable revision. Existing
Markdown remains byte-identical until an author previews and confirms conversion
in memory, then explicitly saves. Unsupported CommonMark nodes and unsupported
extension syntax cause visible conversion refusal. HTML, attachments, definitions,
link titles, heading levels above three and unsupported structures are not silently
removed. Markdown with GFM tables (including optional outer pipes), strike or
ordered/unordered task syntax is conservatively refused by syntax-aware parsing;
code and escaped literal punctuation remain valid. Rich tables, strike and
checklist blocks are available directly.

Canonical rich blocks support paragraphs, headings 1–3, bullet/numbered lists,
checklists, quotes, plain code blocks and rectangular tables. Inline styles are
bold/italic/strike or exclusive inline code, plus validated HTTP(S)/fragment links.
The view renders canonical React structure, without HTML injection or a history
editor instance. Code highlighting is plain/local. Only installed local CSS and
fonts are used. Table merge, headers, cell colors, column resizing and unsupported
toolbar/menu actions are disabled; new tables start at 3×3. No upload, embed, AI,
emoji picker, collaboration provider or automatic third-party call is configured.

The editor validates candidate transactions before they change the document.
Paste accepts only plain-text clipboard data, parsed conservatively before
insertion; HTML/files are refused without constructing a resource-loading DOM.
External drops are refused, including externally supplied BlockNote HTML.
Internal block dragging remains available. Rejected or over-limit changes retain
the last valid visible document and parent draft with a visible explanation.

Drafts are in memory only. A lost save response retries the exact request ID and
original expected content/auth revisions; 400/409 responses retain the draft.
Metadata polling retains its existing five-second bound. Comparison and explicit
rebase/reload actions determine when a dirty editor may be replaced. Authority
loss clears the editor, comparisons, history, draft and saved export cache.
Source-chat documents continue to display and inherit their source audience.

Recovery copying is format-specific: a dirty rich draft copies compact canonical
JSON, preserving the stored byte bound without indentation expansion.
Markdown export is the native-produced export of an exact loaded saved revision;
its label names that revision, excludes unsaved changes and warns that formatting
may be simplified. No client Markdown exporter or draft-export endpoint was added.
After Save/restore, this export is unavailable until that saved body is loaded.
Restore uses the existing native operation and appends the selected original format.

## Dependency record

| Package | Exact version | SPDX license | Primary source |
| --- | --- | --- | --- |
| `@blocknote/core` | 0.54.2 | MPL-2.0 | [manifest](https://registry.npmjs.org/@blocknote/core/0.54.2) |
| `@blocknote/react` | 0.54.2 | MPL-2.0 | [manifest](https://registry.npmjs.org/@blocknote/react/0.54.2) |
| `@blocknote/shadcn` | 0.54.2 | MPL-2.0 | [manifest](https://registry.npmjs.org/@blocknote/shadcn/0.54.2) |
| Direct Tiptap packages | 3.31.3 | MIT | [core manifest](https://registry.npmjs.org/@tiptap/core/3.31.3) |
| `next` / `eslint-config-next` | 16.3.4 | MIT | [Next release](https://github.com/vercel/next.js/releases/tag/v16.3.4) |

Direct Tiptap core, extension-placeholder, markdown, pm, react, starter-kit and
suggestion are aligned. ProseMirror model is converged at 1.25.11 and view at
1.42.3 to avoid duplicate runtime constructors/private types. The lockfile uses
existing valid resolutions; no bundler alias, dependency override, new build
approval or workspace-policy change was introduced. A trial transitive update
that also refreshed unrelated Babel/Rollup dependencies was discarded.

The original Next 16.3.5 pin failed the existing minimum-release-age policy.
Controller plan correction `ecdf1541` selected 16.3.4, published August 31, and
removed no policy protections. Frozen install then verified 1,423 entries. Registry
manifests for all 93 new resolved versions had no preinstall/install/postinstall
hooks. Their SPDX licenses are MPL-2.0, MIT, ISC, Apache-2.0, and the Sharp/libvips
LGPL-3.0-or-later variants (including combined Apache/MIT expressions). The existing
root postinstall only copies Monaco assets locally. No XL/commercial package was
added. Optional Yjs peers do not configure a provider.

`pnpm peers check` still reports the existing React-19 warning from
`@xyflow/react@12.11.5 → zustand@4.4.0 → use-sync-external-store@1.2.0`.
BlockNote/Tiptap use `use-sync-external-store@1.6.0`, and their React/Tailwind peers
match installed React 19.2.4 and Tailwind 4.x.

### Audit reachability

The final inspected `pnpm audit --json` reported 144 findings: 14 low, 77 moderate,
52 high and one critical. It is **not** a clean audit. No finding's dependency path
included the new BlockNote or upgraded Tiptap packages. The native rich document
surface does not execute build/lint tooling against document content.

Remaining findings on Next/ESLint paths are in build/lint dependencies:

- minimatch, brace-expansion and picomatch: glob/ReDoS advisories
  `GHSA-3ppc-4f35-3m26`, `GHSA-7r86-cg39-jmmj`, `GHSA-23c5-xmqv-rm74`,
  `GHSA-f886-m6hf-6m8v`, `GHSA-3jxr-9vmj-r5cp`, `GHSA-mh99-v99m-4gvg`,
  `GHSA-rgw5-rvv9-x895`, `GHSA-3v7f-55p6-f55p`, `GHSA-c2c7-rcm5-vvqj`.
  Their inputs here are repository lint/build patterns, not native document bodies.
- ajv (`GHSA-2g4f-4pwh-qvx6`), flatted (`GHSA-25h7-pfq9-p65f`,
  `GHSA-rf6f-7fwh-wjgh`), js-yaml (`GHSA-h67p-54hq-rp68`,
  `GHSA-52cp-r559-cp3m`, `GHSA-5p4m-2wfm-xmqj`, `GHSA-2883-xcg3-v3hh`):
  lint configuration/cache parsing, unavailable to the static document UI.
- Babel core (`GHSA-4x5r-pxfx-6jf8`), browserslist (`GHSA-c83g-rgw3-j3cx`,
  `GHSA-73wf-gq98-2v4g`), baseline-browser-mapping (`GHSA-w5vr-8v7q-w6rv`):
  build-time repository source/target configuration, not a native content route.
- humanfs (`GHSA-p498-v437-472g`): lint filesystem operation, not a blob API.

The remaining critical finding is existing Vitest `GHSA-5xrq-8626-4rwp`, reached
through dev test/UI dependencies. This task uses local `vitest run`, and exports
static application assets; it does not expose a Vitest UI/server. Other existing
graph findings remain outside this editor change. No audit settings, automatic
audit fix or blanket dependency upgrades hid these findings. Advisory details are
available from `https://github.com/advisories/<GHSA-ID>`.

## Acceptance evidence

Automated commands use `PATH=/opt/homebrew/opt/node@22/bin:$PATH` from the source root.
Detailed RED/GREEN results are in the task-3 report.

```sh
pnpm install --frozen-lockfile --ignore-scripts
./node_modules/.bin/vitest run src/components/neev src/lib/neev- \
  src/components/chat/composer src/components/tasks/task-message-composer.test.tsx \
  src/components/message/composer-to-bubble-roundtrip.test.ts \
  src/components/automations/composer-invocations-popup.test.tsx
./node_modules/.bin/tsc --noEmit
pnpm build
pnpm audit --json
```

- Initial Task 3 gate after browser fixes: 46 files, 534 tests passed, exit 0 (13.06 seconds).
  This includes adapter 17, actual rich widget 10, document controller 26,
  native content integration 37 and all native locale tests.
- TypeScript: passed with the converged graph.
- Scoped ESLint and `git diff --check`: passed, exit 0.
- Initial static build after browser fixes: passed, Next 16.3.4, 34 routes exported. Compilation 4.1 seconds,
  TypeScript 5.1 seconds. Existing legacy composer React `act` warnings and the
  Vite CJS warning remain disclosed; no test failures or unhandled errors.
  Build artifact: `out/index.html`, 310,449 bytes, SHA-256
  `b3f9253e12fd01fbaaf41ae3023a3d97ae4695a27c4f616397c267130d6b11a9`.
- Actual synthetic browser desktop/375px, slash edit, dragging, save/reload/history,
  conflict/share/archive/authority loss: passed, controller observed via the existing
  [driver runbook](neev-native-content-acceptance.md). Zero external requests and
  no application page errors observed; synthetic issuer CSP limitation below.
- Actual mixed Markdown/rich database+blob restore: passed, controller observed;
  schema 6, six referenced blobs, 1,900 bytes; detailed record below.
- Existing schema-3 retained archive: controller verified unchanged with the newly
  built verifier, as recorded below.

### Historical schema-3 verification (controller observed)

```sh
NEEV_VERIFY_DATABASE_URL='postgresql:///neev_restore_9fcd806b118f4ba18718d1ac5039b3d4?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
NEEV_VERIFY_DOCUMENT_ROOT='/private/tmp/neev-content-recovery.R5JKHR/restored-blobs' \
src-tauri/target/debug/neev-content-verify \
  --database neev_restore_9fcd806b118f4ba18718d1ac5039b3d4 \
  --schema neev_test_b77f7cdd2e004a7ebf30a0d603cf42be --confirm-isolated
```

Exit 0: schema_version 3, referenced_blobs 3, referenced_bytes 608,
manifest_sha256 `7d1f4aecf9c55451b1087dced6cd44d1cb8236fcc59676e92cb251eac139c235`.
The original 79,039-byte dump and its hash remained unchanged. No initialization,
migration, archive mutation or deletion was performed. The verifier was built at
accepted native commit `f286f3d9`.

## Actual synthetic browser acceptance

The controller observed actual slash Heading2 editing, keyboard save/reopen and
pointer drag reorder persisted in revision2. Shared content was read across both
listeners. Actual409 preserved a rich draft/comparison; owner Make private cleared
the teammate editor/draft/dialog. Rich→Markdown restoration kept exact original
bytes and appended revision3. Archive disabled editing/Save, hid active content and
included it in Show archived. An HTML ClipboardEvent injected into the actual
widget was visibly refused with unchanged content and zero img/iframe/external
requests; this is a synthetic browser event, not OS clipboard acceptance.

The second participant used synthetic native email login/invite after the IPv6
synthetic Google callback encountered a CSP form-action failure. No app policy
was weakened; real provider behavior remains outside this acceptance. Original
Markdown was seeded through authenticated same-origin native APIs within the
synthetic page, then conversion and restore ran through the real UI.

The controller found BlockNote's light surface mismatched the dark shell. The
source now binds the app's resolved theme; a real-widget dark→light regression
and controller suite passed 36/36. On the controller's safe-to-rebuild signal,
`pnpm build` passed: 4.5-second compilation, 5.9-second TypeScript, 34 routes.
Intermediate theme artifact `out/index.html`: 310,449 bytes, SHA256
`6d9d969280cd94e217ee46cd29baafbe6be5b2ff4de53f14992375f88984b2f6`.
Actual theme recheck passed (dark shell, dark BlockNote, background rgb31).
The final mobile fix and its passing retest are below. Full reload returning to
the workspace root is a navigation UX observation; reopening retained content.
History comparison at 375×812 had scrollWidth 375 and dialog width 343; Escape
restored focus to Compare revision 1 after its close transition.

With a dirty rich draft and Reload latest confirmation open, signout in another
tab in the same browser context cleared the original editor, draft, dialog and
New project controls. This is not a hidden-tab/visibilityState transition claim;
that supplemental infrastructure check remains inconclusive and was not mocked.
Across the monitored journey, zero external origins and no application page errors
were observed. The synthetic issuer CSP errors are disclosed separately above.

## Observed mixed-format recovery

The controller backed up source schema
`neev_test_65f068bcb4ef402abbb5f04e80b4189b`, with source root
`/private/var/folders/67/wq2pylbj7c72k_7k726w45100000gn/T/.tmptNWGJy`, into
`/private/tmp/neev-rich-recovery.L2aufp/snapshot`. The completed manifest records
a 98,882-byte `metadata.dump`, SHA256
`e9d94f983a45b9eb63134201ca34e5a7c6e99bb54efa6b38e3debae416d9379e`,
and six blobs totaling 1,900 bytes (456, 468, 55, 456, 410, 55). The worker read the
complete marker independently without modifying it.

The fresh isolated restore destination is database
`neev_restore_7190a4163db24157869b6bc6187a84ce` and root
`/private/tmp/neev-rich-recovery.L2aufp/restored-blobs`. `pg_restore` exited0;
no application initializer ran. The built current verifier exited0, reporting
schema_version 6, referenced_blobs 6, referenced_bytes 1900 and manifest_sha256
`cd262f44b2c8395cd43e1a96e45c6a240632bab7a078e153bea144bdfd4cbed0`.
Read-only restored metadata aggregation counted four blocks and two Markdown
revisions. Backup and restored artifacts are retained separately from the historical
schema3 artifact. The controller confirmed these actual standard runbook commands,
with the dedicated backup/restore/verifier connection environments:

```sh
node scripts/neev-content-backup.mjs \
  --source /private/var/folders/67/wq2pylbj7c72k_7k726w45100000gn/T/.tmptNWGJy \
  --output /private/tmp/neev-rich-recovery.L2aufp/snapshot \
  --schema neev_test_65f068bcb4ef402abbb5f04e80b4189b
pg_restore --dbname neev_restore_7190a4163db24157869b6bc6187a84ce \
  --exit-on-error --no-owner --no-privileges \
  /private/tmp/neev-rich-recovery.L2aufp/snapshot/metadata.dump
cp -Rp /private/tmp/neev-rich-recovery.L2aufp/snapshot/blobs \
  /private/tmp/neev-rich-recovery.L2aufp/restored-blobs
NEEV_VERIFY_DOCUMENT_ROOT=/private/tmp/neev-rich-recovery.L2aufp/restored-blobs \
src-tauri/target/debug/neev-content-verify \
  --database neev_restore_7190a4163db24157869b6bc6187a84ce \
  --schema neev_test_65f068bcb4ef402abbb5f04e80b4189b --confirm-isolated
```

### Mobile clipping correction and final cleanup

The actual 375px page had scrollWidth 375, but the saved-export button was 664.07px
wide and ended at 684.07px, clipped by the outer container. White-space wrapping
alone did not constrain the inherited non-shrinking flex item. The source now
sets minimum width 0, maximum width 100% and allows this button to shrink; all label
text remains unchanged. The existing mounted controller recovery test verifies
full saved-revision/unsaved-exclusion labeling and correct copied content.
On the final artifact identified above, the controller observed button width 335,
right edge 355, height 62 at viewport 375; the complete label was visible on
wrapped lines, and page scrollWidth was 375. Its screenshot was visually inspected.
This is actual browser RED/GREEN evidence; jsdom does not calculate this geometry.

The controller closed its owned browser normally, then sent `stop` to driver
PTY 29462. The driver exited 0; read-only checks confirmed source schema count 0
and absence of the exact temporary source root. Only the owned synthetic source
fixture was cleaned up. Backup/restored artifacts and historical schema-3 evidence
remain retained; no user or production data was removed.

The controller independently repeated the final frontend gate: 534/534 tests in
46 files passed in 14.83 seconds; typecheck and `git diff --check` exited 0.

### Task-review correction round 1

Review identified optional-pipe tables/ordered tasks bypassing a raw Markdown
heuristic, valid code being refused by that heuristic, and pretty-printed rich
recovery JSON expanding a boundary draft from 1,048,576 to 1,048,691 bytes.
Behavioral RED reproduced all findings before changes. The adapter now uses the
existing unified/remark-parse/remark-gfm dependencies to recognize AST syntax;
unsupported nodes and checked list items are refused. Recovery JSON is compact.
No dependencies, native storage or authority contracts changed.

Post-fix focused adapter/controller suites passed 52/52 tests (25 adapter,
27 controller) in 4.10 seconds. The boundary test checks exactly 1,048,576 copied
bytes and successful canonical parsing with the complete original draft text.
Scoped ESLint, typecheck and `git diff --check` also exited 0. The earlier browser
journey describes the pre-review artifact; the controller's focused browser smoke
for this correction passed as recorded below.

The correction's `pnpm build` exited 0: Next 16.3.4 compiled in 4.0 seconds,
TypeScript finished in 5.5 seconds, and 34 routes were statically exported.
Fresh `out/index.html` is 310,449 bytes, SHA256
`d4568515e121977e127cccceb06e2c66a03b516168888b034da680946e18f02c`,
mtime `2026-09-12T11:26:27.449Z`. No full frontend/native suite was rerun for
this scoped correction. Code and regression tests are in commit `95f382c9`;
the subsequent evidence update is documentation only.

Scoped re-review of `18a46d56` was clean: both Important findings addressed and
no new breakage reported. Actual browser smoke on artifact `d4568515` visibly
refused optional-pipe table and ordered-task plain-text ClipboardEvents without
changing the draft; fenced code containing `~~literal~~` was accepted as code.
This is injected browser clipboard-event coverage, not an OS clipboard claim.
Monitored external requests and application page errors were both zero.

That first smoke's follow-up incorrectly awaited an editor while Reload latest
had opened its confirmation dialog. The controller recognized the harness step
error and normally cleaned up that synthetic fixture; no reload pass or app fix
is claimed from that step.

A fresh fixture then verified persistence through the correct UI: create a
synthetic organization/project/document, paste fenced text code containing
`~~literal~~`, wait for Saved, navigate Back to folder, and reopen the document.
The actual preformatted text remained exactly `~~literal~~`. External requests
and application page errors were zero. Its source schema was
`neev_test_5913a881751b4b3cbd2a5b60e7e9fc29`, temporary root basename `.tmpPEQA2t`.
The controller closed its browser and normally stopped PTY 19567 (exit 0);
read-only checks confirmed schema count 0 and absence of the owned source root.
Only synthetic fixture data was cleaned up. This resolves the earlier harness
step without source changes; whole-rich-plan review follows, not public release.

## Recovery and rollback

Preserve the native database and its matching immutable blob root together. Verify
the isolated restored pair with the built read-only verifier before using it.
Retain historical backups and manifests. Schema 6 is forward-only: do not run a
down migration, rewrite old revisions, initialize an old verification target or
delete rich blobs when reverting frontend code. A code rollback must retain a
reader that understands stored rich revisions; the previous Markdown-only UI
cannot safely edit those revisions. Deployment/provider gates remain separate.
