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
