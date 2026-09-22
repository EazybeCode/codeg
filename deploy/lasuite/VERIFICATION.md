# La Suite release verification — 2026-09-22

New documents use La Suite Docs v5.7.0. Existing native documents remain native;
no document migration or Docmost change was performed.

## Deployed document engine

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
  table, and added a third row without a formatting refusal.
- Creation-off rollback was rehearsed on the engine-aware image: mapped documents
  stay on La Suite and native content routes cannot overwrite them.

## Limits

Read-only users retain read-only access; upstream does not provide a separate
read-only comments role. Full native integration-test compilation requires its
existing `test-utils` feature; this release does not claim that entire suite passed.
The native content-menu rename dialog has a pre-existing tab-label refresh issue;
the new La Suite editor's inline title path is verified.

## Release-script corrections verified during rollout

The successful one-shot storage initializer is checked separately from persistent
services. Docker capability names are normalized before comparison. Temporary-file
cleanup traps capture their path and remove themselves rather than outliving local
variables. The final enable-creation command completed with exit 0.
