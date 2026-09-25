# Neev brand theme and Stop control — 2026-09-25

User selected warm plum + coral, then requested brown user prompts, neutral
assistant text and a visible Stop icon while a query executes.

## Palette

| Role | Light | Dark |
| --- | --- | --- |
| Primary plum | #783274 | #d285cb |
| Coral project mark | #dc604a | #f48e76 |
| Reading background | #fdfaf4 | #120f12 |
| Body text | #221a22 | #f7f5f1 |
| User prompt surface | #f8ebe1 | #35261d |
| User prompt text | #482f1f | #f0e4d7 |

The workspace root marker enables semantic tokens on the document root through
`:has`, including portalled menus. Other pages keep existing theme presets.
Light/dark mode is preserved. Agent marks and focus use plum; tools use subdued
slate-teal. No text animation is introduced. The existing 14px single rotating
ring, timer, and reduced-motion behavior are preserved.

Both chat composers now show a filled square Stop generation button while a run
is active, using their existing run cancellation endpoints. Labels are localized
in all ten supported locales. Send remains alongside it; prompts are preserved.

## Verification

- Stop tests failed before implementation (two missing Stop generation controls).
- Chat and locale tests: 87 passed, exit 0 after implementation.
- Changed chat TSX/tests and workspace TSX lint: exit 0.
- Integration run: 119 assertions passed across four files, but command exit 1
  due seven `elementsFromPoint` exceptions from BlockNote mouse handling in jsdom
  workspace tests. No broad-suite success claim; editor behavior was not changed.
- Production `next build` (Turbopack, Node 22): exit 0.

Candidate UI SHA256:
`61e39dc1fad5f141fe2a7a2d95696ca1abc0f8aedbb1f318f0202f14a3ea7bd5`.
Image: `neev-native-agent:ux-brand-plum-coral-20260925`.
Backend binary remains unchanged from schema-16 release.

## Live verification

Released after schema-16 and zero active runs/sign-ins check. Health 200 and
exact UI hash verified. Live screenshot confirmed warm prompt surfaces, dark
reply text, plum selected rows/agent marks and coral project icons. A disposable
private chat ran `sleep 30` on GPT-5.6-Luna: the square Stop generation control
was visible, one in-chat ring/timer and one sidebar activity mark were visible.
Clicking Stop removed the live run and both activity marks; the model control
became available again. Test chat was archived and closed. Existing chat reopened
for user review. Dark-mode colors were checked numerically, not in live screenshot.
