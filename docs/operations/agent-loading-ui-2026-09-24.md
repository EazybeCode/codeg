# Single loading indicator and composer spacing

Source: `5977adcb`. Shared ActivityTimeline now shows one gently pulsing agent
mark with the running label and existing elapsed clock. Removed duplicate run
labels and screen-reader announcements; Send keeps its name. Tool details,
cancellation and reconnect notices remain. No distinct Neev logo asset exists,
so this uses the existing Sparkles agent mark. Reduced motion disables animation.
Both transcripts now have 56px bottom padding, with top spacing unchanged.

## Verification and release

Independent focused frontend tests: 80 passed across four files; changed-source
ESLint and diff check passed. Claude reported TypeScript and static export passing.
Its broader suite encountered unrelated timing failures that passed individually;
no blanket full-suite pass is claimed.

Deployed UI-only image `neev-native-agent:ux-docs-speed-single-loader-20260924`,
based on the response-style release. UI SHA-256:
`de1aa4bcb20fb1a318afb73530410a0619a123d6b5ccbf8c9df0e52df0de3495`.
Deployment changed only the compose image, after zero running turns/sign-ins;
health returned 200 and document creation stayed enabled. Existing backend,
auth and document engine retained.

Live synthetic Codex run: one Working activity row with agent mark and elapsed
time, stable Send button and available Cancel. Completed response retained its
tool summary; running mark stopped. Screenshot inspection confirmed readable
paragraphs and clear separation above the composer. Animation is CSS-gated for
reduced motion; no claim of live OS reduced-motion testing.

Repository context: [org-memory: wiki/engineering/workos.md]; current source
verified directly because the wiki describes the older fork.


## Dotted indicator correction

User reference showed chasing dots next to elapsed seconds, not an icon pulse.
Commit `49f860d6` replaces the pulse with eight staggered dots and places the
elapsed clock immediately beside it. Generic Working text is screen-reader-only;
actual tool names still appear. Reduced motion leaves static dots.

Validation: 80 focused tests, ESLint, production export and diff check passed.
Live screenshot confirmed the dotted indicator beside `14.4s` during a command,
with one running row and a stable Send control.
Image: `neev-native-agent:ux-docs-speed-dot-loader-20260924`.
UI SHA-256: `45d9be5b379801e665912b3b88502ff3fce17cd8d36b8ca9b05033ad6585ff5f`.
Health 200, document creation enabled, deployed hash matched; image-only release.


## Compact initial correction

The user preferred a smaller Neev initial after reviewing the dots. Commit
`47781dac` replaces them with a lowercase n in a fixed 12px box, 14px type,
and a 6px gap to the elapsed clock. Subtle opacity pulse only while running;
reduced motion disables it. No second loading state.

68 focused chat tests, ESLint, static export and diff check passed.
Image `neev-native-agent:ux-docs-speed-n-loader-20260924` deployed successfully;
health 200, creation enabled, UI hash verified:
`e8f0bad26c6abe933f0a5181050457677201900c49cc5ce63eb0af945741f4b4`.
The earlier live run verified indicator lifecycle; this final visual replacement
was build-verified, without claiming a further live provider run.


## Final ring design

After the initial looked awkward to the user, `e7a755cb` replaces it with a
14px muted circular indicator: thin track, rounded arc, smooth rotation,
6px spacing to the clock. Reduced-motion preference keeps a static ring.
A local browser preview verified size and alignment before release. ESLint,
diff check, and production build passed; this SVG/CSS-only change retained the
previously tested running/completion logic.

Live image: `neev-native-agent:ux-docs-speed-ring-loader-20260924`.
UI SHA-256: `9e8693fe0dff517e99f327099d53a0047fa97474bfbd61d8e2263eb0d8255b9a`.
An initial release attempt used the wrong date suffix and failed before image
replacement; the correct existing image was then deployed. Final health 200,
UI hash verified, document creation remained enabled.

## Progress footer and sidebar — 2026-09-25

`610a0abd` + `a4a30573` place live activity after streaming output in both chat
surfaces, reserve clock width, and publish scoped live run state to sidebar rows.
The ring is shared; there remains one conversation activity indicator. Sidebar
state is client-local and clears on finish, cancel, or unmount; it does not
claim to monitor server runs from an unmounted chat.

Validation: 82 focused tests (26 private-chat/store, 56 team-chat/attachments/
activity), lint, diff check and production static export passed. Earlier mixed
workspace tests had jsdom BlockNote elementsFromPoint errors; standalone focused
suites passed. Live disposable chat ran two Codex commands: observed 12.5s +
Running command below output and sidebar running mark, then completion cleared
both. Test chat moved to recoverable trash and test browser tab closed.

Image: `neev-native-agent:ux-docs-speed-progress-footer-20260925`
UI SHA256: `3c9e577e3645e4be384760f70f7a0a321080543b671a8ca6aff20f89433abef7`.
Idle check 0 sign-ins/0 turns before restart; live health and UI hash passed.

The same release adds official checksummed Node v22.23.3 with npm 10.9.9.
Non-root image smoke test and actual agent sandbox command confirmed versions.
Playwright package/browser is NOT installed by this change.

Storage: removed 340 MB of unmounted provider build downloads and pruned the
stopped neev-native-20260912 BuildKit cache (9.337 GB reported). Free host disk
rose to ~9 GB. No application data, credentials, or application volumes removed.

## Completion stability and mentions — release candidate

Completed activity now remains below the answer in both chat surfaces, matching
its position during streaming. Tool results without displayable output show a
non-expandable Completed label. Human prompts use a quiet bordered background.

The same candidate adds explicit coworker mention chips in shared chats, a scoped
in-app inbox, and a paginated member picker. Private chats cannot notify coworkers.
Unread receipts are marked only once the destination chat is visible and loaded.
Notifications poll every 60 seconds; this is not push delivery.

Frontend verification: 525 tests in 29 suites reported by the Claude worker;
independent focused checks passed 38 tests, then a directory-pagination regression
passed with the 9 chat/inbox tests. Production export passed. Candidate UI SHA256:
`2e24f88e00329417041d799039ba3aa9cfa3f7ad28d5144b4cae6aa9fabb76d6`.

Backend review caught a schema-15 migration guard defect before release; an
isolated PostgreSQL schema-15 upgrade reproduced failure, then passed after the
fix, including a restart. Additional review fixed unread-first inbox ordering,
organization-aware validation cursors, and read-batch bounds. Empty mention arrays
are omitted from message DTOs for compatibility with existing browser clients;
messages with mentions require the new client. Do not use the old UI image as a
complete rollback after users have created mentions.

Deployment status: pending final Rust/PostgreSQL verification and backup.

## Conductor transcript — UI-only release 2026-09-25

Released the chronological transcript/compact tool rows/scroll override separately
from mentions. Main implementation `1d12c72a`; isolated release commit `75ac7ac6`
applies it to pre-mentions frontend `21fb4760`. One conflict in the team message
renderer retained TranscriptBody without the unreleased MentionedText branch.

Validation: 177 focused tests (8 suites), TypeScript, changed-file ESLint and
production static export passed. Turbopack refused the node_modules symlink in
the isolated checkout; `npm run build -- --webpack` passed with no config change.
The main combined tree passed 186 focused tests but is not this release.

Live image: `neev-native-agent:ux-docs-speed-conductor-ui-only-20260925`.
UI SHA256: `ae74be18ff025771baa779ff1f5f95b6e7ee5bb99c8b8985c13cbe3002f25659`.
Server SHA256 remains `6d20bbcdfa268a83830ff7ffe0d436ec329aa747992090f66019611046921239`.
Idle precheck: 0 sign-ins/0 running turns. Health 200, UI hash, document creation
flag verified after deployment. No database migration in this release.

Mentions remains withheld: relevant SQLite suites passed 255 tests, PostgreSQL
mention behavior passed, but the PostgreSQL migration test failed on first
reopen after a successful schema-15 upgrade. Root cause under investigation.
The combined image must not be deployed until this is resolved.

Known remaining UX gap: the team-chat preview-to-stored-message handover still
remounts expanded tool details; private agent-chat details now stay open through
completion. This is not a claim of full Conductor feature parity.

Live verification: disposable private node chat “Verification — Conductor transcript”,
Codex GPT-5.6-Luna, harmless printf/sleep commands. Observed Starting check →
printf FIRST row → First command finished → sleep/printf SECOND row → Check
complete, in that exact order. Run completed in 22s and the loader cleared.
Expanded first command row displayed its command and actual FIRST output.
Screenshot checked compact rows, inline monospace command previews and composer
spacing. Temporary chat moved to recoverable trash after verification.
