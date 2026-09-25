# Restrained purple accents — 2026-09-25

User clarified that brown message backgrounds should remain, while purple was
overused on tabs and navigation. Preserved the existing warm brown message
surface/text colors in both modes. Changed workspace backgrounds, navigation
selection/hover fills, agent badges and tool/loading marks to neutral gray.
Primary controls and focus retain purple; the active tab gets a 2px underline.
No interaction, API, database or cancellation changes.

Validation: changed TSX lint exit 0; production Next build with TypeScript
exit 0. No extra behavior tests added for this CSS-only presentation change.

Image: `neev-native-agent:ux-neutral-navigation-20260925`.
UI SHA256: `2427bdb0c93ee1e7fe3384461a3e1c16e77cb05ddeaab042035580212e108b10`.
Backend binary unchanged from the prior schema-16 release.

Live verification: no active runs/sign-ins before restart; health 200 and exact
UI hash afterward. Live screenshot confirmed brown messages preserved, neutral
sidebar/tab/agent badges, and thin purple active-tab underline.
