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
