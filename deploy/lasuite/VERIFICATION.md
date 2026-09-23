# La Suite release verification — 2026-09-22

New documents use La Suite Docs v5.7.0. Existing native documents remain native;
no document migration or Docmost change was performed.

## Initial document-engine deployment

- Backend source: `d9665d3b`; initial released UI: `5ebe10a6`.
- Runtime image: `neev-native-agent:ux-docs-d9665d3b`, image ID `0eb3ddf10ad1`.
- Native `workspace.html` SHA-256:
  `7fa24c0cb0334c0d5fb9ec663ac305de3815c6800bd1013144b9e46e3c66b673`.
- Embedded Docs artifact: `2d1a0c74feb3`; 4,160 files verified against its manifest.
- Production creation enabled; HTTP health, read-only artifact mount, agent helper
  capabilities, devices, and private Docs services verified.
- All 30 baseline native revision hashes remain unchanged; native schema is 14.

## Evidence

- Focused workspace/editor UI suites: 50 passed. Coordinator independently ran
  all 27 editor tests and the restored-tab rename regression test successfully.
- Same-host acceptance: 23 passed, 0 failed, 1 skipped. The skipped outage case
  was exercised separately: editor returned retryable HTML 503; native health
  and legacy reads stayed 200; editor and durable content recovered to 200.
- Two-actor collaboration converged. Revoking an active actor prevented later
  edits and disconnected that actor. Cross-document comment IDs were refused.
- Comment create, own-comment PUT edit, reactions, and non-author restrictions
  passed. Unknown body fields were refused by the native proxy.
- Browser: prompt-free creation, editable title, compact embedded layout, title
  synchronization after immediately leaving the frame, and restored-tab titles.
- Browser: table expanded from two rows to three without the former formatting
  refusal. Text, expanded table, and a comment survived reload in the canary.
- Live browser: created a document in Tech, renamed it, entered text, inserted a
  table, and added a third row without a formatting refusal. Reload preserved
  the title, paragraph, all three rows, and the added row's text.
- Creation-off rollback was rehearsed on the engine-aware image: mapped documents
  stay on La Suite and native content routes cannot overwrite them.

## Limits

Read-only users retain read-only access; upstream does not provide a separate
read-only comments role. Full native integration-test compilation requires its
existing `test-utils` feature; this release does not claim that entire suite passed.
The native content-menu rename/tab refresh issue observed at this release was
subsequently fixed and verified in the September 23 release below.

## Release-script corrections verified during rollout

The successful one-shot storage initializer is checked separately from persistent
services. Docker capability names are normalized before comparison. Temporary-file
cleanup traps capture their path and remove themselves rather than outliving local
variables. The final enable-creation command completed with exit 0.

## Current UI: explicit New Tab navigation

- UI source: `2d1ad0da` following `9cb1c03e`; backend remains `d9665d3b`.
- Live image: `neev-native-agent:ux-docs-tabs-2d1ad0da`, ID `253458a3a684`.
- Native `workspace.html` SHA-256:
  `0aaf48bbb42adda1220a32b64123e416d582b5961d2d940a8180bc4e15aa9265`.
- Ordinary document/chat/project navigation replaces the active view. Explicit
  New Tab adds an empty tab, which the next selection fills. Selecting an already
  open item focuses it and consumes the unused blank.
- Replacing a view uses its unsaved-edit guard. A pending navigation is abandoned
  if background work changes the tab list while the confirmation is open; the
  regression fails when that protection is removed and passes when restored.
- Focused workspace/content/tab tests: 79 passed; coordinator independently ran
  the workspace/tab suites: 40 passed. TypeScript, ESLint, and production build
  completed successfully with their actual exit codes captured.
- Canary browser: opening a chat kept 7 tabs; New Tab increased 7 to 8; creating
  a document filled the blank at 8; a subsequent document replaced it at 8.
  Selecting an already open project consumed the unused blank (9 to 8).
- Live browser: New Tab increased 6 to 7; selecting the existing project returned
  to 6 with no blank left; opening the review document kept 6 tabs and selected
  the correct document. Saved document content remained present.
- Promotion changed only the native image. Live health returned 200, the deployed
  HTML hash matched, and document creation remained enabled. Rollback image:
  `neev-native-agent:docs-before-tab-navigation-20260922`.


## September 23: item actions, compact documents, opening speed

Native UI source `21d9f3c6`; backend remains `d9665d3b`. Live image
`neev-native-agent:ux-docs-speed-21d9f3c6` (ID
`30a0b1221a1bcbc4dd3aef1ae86446a53c038cb7fcb35f3a526bc86931376d42`).
Native workspace HTML SHA-256:
`7afa6481b287109543495fd940b8477e3133b3460ad1fae98867ad3f976176bd`.

- Sidebar item menus expose Rename and recoverable Move to trash/Restore;
  Show trash reveals archived items. Header and sidebar mutations update open
  pane, breadcrumb and tabs in both directions without remounting the editor.
- Live checks passed: document header rename, chat sidebar/header rename,
  document trash (body contenteditable=false) and restore (true). All named
  disposable test items were moved to recoverable trash; user content preserved.
- Embedded editor title moved from y=210 to 132 and body from y=294 to 180
  at the tested desktop viewport. At 390px width, controls did not overlap.
- Chat composer appeared 499ms after New chat in a live browser check. The
  authorization gate still prevents sending before resource access is verified.
  Ordinary navigation kept the tab count unchanged.
- Native production build, TypeScript and ESLint passed. Focused five-file tests:
  150 assertions passed, no failed assertions, but process exit 1 from seven
  pre-existing BlockNote/jsdom elementsFromPoint errors. Baseline had 3 failed
  assertions and 138 passed. Independent engine/tab tests: 43 passed, exit 0.
  Do not describe the full test runner as clean.
- Live health 200, document creation enabled, native server binary and file
  capabilities unchanged. All 30 baseline immutable revision hashes unchanged;
  schema remains 14. No document migration or Docmost changes.
- Editor static assets now receive compression and private immutable caching
  only for successful hash-named assets. Auth failures and HTML/API stay no-store.
  Exact production Caddy image passed six delivery fixtures before rollout.
- Existing hashed editor chunks are retained for already-open browser sessions.
  Original artifact backup remains `/tmp/neev-docs-artifact-before-ux` on host.

### Final editor startup artifact

Patch 0009 (worker `62125297`, integration `a5e55fb6`) starts the exact dynamic
editor import and existing authenticated content query alongside metadata.
Query keys/staleTime are unchanged and rejected speculative imports are caught.
Three focused startup tests passed; final upstream production build v5 exited 0
(actual child status captured). All 4160 output files matched the SHA-256 manifest
on the live host after publication; `/healthz` returned 200.

Live browser verified a fresh document had both editable title and body, title
rename propagated, and Move to trash succeeded. Review document reopened with
its existing content. The attempted final timing run lost browser attachment;
no reliable post-optimization latency is claimed. Before this final overlap
change, document creation to writable body measured 6335ms (single sample).
Full initial workspace boot remains distinct from opening an item.

### Compact header follow-up

UI `b2300a16` (integration `79c6a823`) removes normal workspace Back to folder
controls from document/chat/agent headers. Document toolbar is 32px high with
28px controls; privacy and item actions remain. Standalone legacy folder view
retains its necessary exit; error recovery controls remain.

Native production build and TypeScript/ESLint exited 0. Focused assertions:
196 passed, none failed; runner still exits 1 for the same seven pre-existing
BlockNote/jsdom elementsFromPoint errors. Live image
`neev-native-agent:ux-docs-speed-b2300a16`, ID
`163719c5896f42f524a3ded155ce74694fae5098d20e5a417f8104999c2e02ef`;
workspace HTML SHA `e046b846dbf1432bc8be84a116238a6e169dbddcd39565be9c0c7014b1b748fb`.

On explicit user request, all eight active legacy-editor documents across their
two organizations were moved through the product's recoverable trash flow.
The two La Suite documents remain active. Other organizations outside the
user's memberships were not modified. No document bodies were deleted.
