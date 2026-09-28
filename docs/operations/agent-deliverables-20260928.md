# Agent deliverables and composer fixes — 2026-09-28

## Problem and behavior

Generated HTML was left in the agent's disposable working directory. A reply could
claim completion and show a temporary path, but the next turn had no file.

The launcher now exports regular files from `outputs/` and `neev-output/` before
cleaning the run directory. A server-owned spool carries a size/hash manifest.
The server validates it, rechecks the invoker's access, stores attachments, and
replaces temporary links with authenticated downloads before reporting completion.
Private outputs belong to the session; shared-chat outputs belong to the chat and
are attached to the assistant message. Existing conversation context loading then
carries those files into subsequent runs. Persistence failure fails the turn.
Previously discarded files cannot be recovered by this change.

Supported output bytes use the existing attachment classifier: UTF-8 text/code/HTML,
PDF, PNG, JPEG, and WebP. Office archives are not newly supported. Output export is
bounded by the existing organization storage budget and 100 files per turn.

## Other changes

- Enter continues numbered paragraphs and exits an empty numbered item. Ordinary
  Enter sends; Shift+Enter inserts a line break; Cmd/Ctrl+Enter sends a list.
- Elapsed time displays minutes and remaining seconds.
- Preinstalled `neev-preview` checks local HTML without downloading browser packages
  during agent runs. External page requests are blocked. Its default screenshot is
  a working artifact, not an automatically exported deliverable.
- The launcher process and virtual address-space defaults accommodate Chromium.
  The container's physical memory and network controls remain in force.
- Idempotent reference-position CHECK relaxation lets generated outputs coexist
  with input uploads. Schema marker remains 16. SQLite's earlier byte-cap upgrade
  now explicitly preserves child references when rebuilding the parent table.

## Verification before deployment

- Frontend: 147 tests passed across composer, numbering, timing, Markdown links,
  attachment parsing and private/shared chat rendering; TypeScript and targeted ESLint passed.
- Launcher: 77 tests passed, including a 22 MiB output, FIFO/symlink rejection,
  both output directories, and failure without partial export.
- Linux handoff: a 22 MiB output survived source cleanup; UID 65532 could read it,
  a different agent UID could not. Repeated with CAP_FOWNER removed to match
  the production helper. A live test exposed and corrected ownership-before-chmod
  ordering and propagation of launcher export errors before the final release.
- Offline Chromium: successful preview as an agent UID with networking disabled;
  helper validation tests passed.
- Focused Rust: 160 passed, 32 ignored, 82 filtered, in a disposable memory-bounded
  build. Tests cover distinct runtime/durable IDs, failed exports, six private and
  six chat outputs, current ACLs, message references and database re-open.
- A broader selected Rust run exposed three failures in unchanged legacy rich-export
  parser tests (blank paragraphs/emphasis/whitespace). Those are excluded from the
  focused result above; this is not a claim that the full Rust suite passes.
- PostgreSQL migration matching was checked against the actual quoted position
  constraint. A database backup was created and its archive list verified.

- Independent Claude review found no critical or important defects in the export,
  persistence, authorization and migration changes.

## Release and live acceptance

Deployed `neev-native-agent:deliverables-composer-20260928-r2` to app.neev.so.
Server SHA256: `d2de7a30ac9eca7d4a41333a3ee46095229d7e3a479c4f78084773c0aaa977a4`.
Workspace HTML SHA256: `9f87cc048e666366b1e119936634137d53e31e8d1477b7d0260f55ab26952654`.
Health and deployed hashes verified. Existing 148-second turn displays `2m 28s`.
Live numbered input continued 1 → 2 → 3 and exited an empty item without sending.
An 80-byte generated HTML appeared as a real chat attachment, retained its formatted
reply/tool trace, and remained available after reloading the page.
A fresh live turn opened the saved file from its staged conversation context and
reported its exact 80-byte size and heading without recreating it or requesting an
upload. Original homepage regeneration completed successfully, with a persisted V105
HTML attachment. The user’s Chrome download UI reported an organisation security
policy block for that HTML; no bypass or alternative delivery channel was attempted.
Agent-reported offline checks found no JavaScript errors or mobile overflow, but
17 external resource requests were blocked and remain unverified.

## Chat-generated file hierarchy

Added expandable generated-file children to resource chats and private agent
sessions in the workspace sidebar. Lists load only when expanded, paginate through
history, deduplicate attachment keys, exclude uploaded inputs, and refresh with
run state. Expansion is scoped to the current user/organisation and requests are
aborted on collapse. Links retain the existing authenticated download endpoint;
no HTML preview, alternate delivery channel or security-policy bypass was added.

Focused tests: 56 passed, including workspace navigation and the new file list.
The existing JSDOM hit-testing shim was extended with `elementsFromPoint` for
BlockNote's sidebar events. TypeScript and targeted ESLint passed.

Released the hierarchy UI as `neev-native-agent:deliverables-composer-20260928-r3`.
Production build passed; health and workspace hash verified:
`b20c59fc892e97fa7d0af9d4d03db3e104b363ee9f3954cfc238b96fc23f3969`.
Live browser verification: expanding the originating chat lists the 80-byte
verification HTML and the 21.6 MB V105 HTML as children. The blocked HTML download
was not retried. No claim is made that the browser security-policy block is fixed.
