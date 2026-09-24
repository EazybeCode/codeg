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
