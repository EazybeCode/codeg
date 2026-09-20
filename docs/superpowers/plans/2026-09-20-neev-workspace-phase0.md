# Neev Workspace Phase 0 (frontend) Implementation Plan

> **For agentic workers:** implement task-by-task; steps use `- [ ]`. Reviewed
> workflow (behavioral tests + independent review + real-browser acceptance).

**Goal:** Restructure the native web UI into one coherent shell with explicit
**Agent Chats / Team Chat / Documents** sections, full-canvas main views with a
per-user width preference, responsive collapsible panels, a human-first team
thread (agent moved to a right context panel), discoverable editor controls, and
honest paste behavior — **frontend only, no backend/schema/runtime changes.**

**Architecture:** Evolve the existing `native-workspace.tsx` single-page shell
(already full-width). Add a client-only per-user preferences store
(localStorage). No new backend kinds; Agent Chats / Team Chat are section
*presentations* of existing resources. No editor schema change.

**Tech Stack:** Next.js 16 static export, React 19, TypeScript strict, Tailwind
v4, BlockNote (unchanged schema), Vitest.

**Spec:** `docs/superpowers/specs/2026-09-20-neev-workspace-ux-design.md`

## Global Constraints
- Docmost frozen; do not touch it. Native path only (`src/components/neev/*`,
  `src/lib/neev-*`).
- No faked agent "Connected" state; do not label org-visible chats as private
  "channels" (call them org conversations). Both are display-honesty rules.
- No backend/schema/runtime change in Phase 0. Runtime stays Disabled.
- Width is a per-user PRESENTATION pref (localStorage), never document metadata.
- Full-canvas keeps a readable paragraph measure (~72–90ch) while tables/images/
  code/embeds expand.
- All existing tests stay green; `pnpm eslint .`, `pnpm test`, `pnpm build` clean.
- TS strict: `noUnusedLocals`/`noUnusedParameters`; Prettier (no semicolons,
  es5 trailing commas, 2-space, 80col).

---

### Task A: Per-user preferences store (width + panel + section)
**Files:**
- Create: `src/lib/neev-workspace-prefs.ts`
- Test: `src/lib/neev-workspace-prefs.test.ts`

**Interfaces — Produces:**
- `type DocWidth = "focus" | "full"`
- `getDocWidth(): DocWidth` (default `"full"`), `setDocWidth(w: DocWidth): void`
- `getRightPanelOpen(): boolean` (default `false`), `setRightPanelOpen(b): void`
- `getActiveSection(): Section` where `type Section = "agents" | "team" | "docs"`
  (default `"docs"`), `setActiveSection(s): void`
- All read/write `localStorage` under `neev.ws.*` keys, SSR-safe (guard
  `typeof window`), tolerate parse errors → default.

- [ ] Write failing tests: defaults when storage empty; round-trip set/get;
  bad value → default; SSR guard (no throw when `window` undefined via mock).
- [ ] Implement the store (pure, no React).
- [ ] `pnpm test src/lib/neev-workspace-prefs.test.ts` green; eslint clean.
- [ ] Commit.

### Task B: Section navigation in the shell
**Files:**
- Modify: `src/components/neev/native-workspace.tsx` (left rail `:504-542`,
  tree `:54`)
- Test: `src/components/neev/native-workspace.test.tsx`

**Interfaces — Consumes:** `getActiveSection/setActiveSection` (Task A).
**Produces:** a section switcher (Agent Chats / Team Chat / Documents) at the top
of the left rail; the folder tree + list scope to the active section; section
persists. Team Chat section labels items "Conversations" (NOT "Channels").

- [ ] Failing test: rendering the workspace shows three section controls with
  accessible names "Agent Chats", "Team Chat", "Documents"; clicking one sets
  active section (assert via `getActiveSection` or rendered `aria-current`).
- [ ] Failing test: Team Chat section does not render the word "Channel(s)".
- [ ] Implement the two-tier left rail (section switcher + existing tree). Keep
  the existing collapsible behavior (`sidebarOpen`).
- [ ] Tests green; eslint/build clean; commit.

### Task C: Full-canvas + per-user width preference (Documents)
**Files:**
- Modify: `src/components/neev/native-document.tsx:521` (the `[&>div]:max-w-3xl`)
- Modify: `src/components/neev/native-rich-editor.css` (add a `.neev-prose`
  readable-measure rule; wide blocks — tables/images/pre — override to full width)
- Test: `src/components/neev/native-document.test.tsx`

**Interfaces — Consumes:** `getDocWidth/setDocWidth` (Task A).
**Produces:** a per-user Full-width ↔ Focus toggle on the document view; in Full
mode the container is edge-to-edge (padding only) and prose uses a max measure
while tables/images/code expand; in Focus mode the ~768px column returns. The
same document renders identically for all readers except each reader's own width.

- [ ] Failing test: toggling to Full removes the `max-w-3xl` column cap
  (assert the wrapper class set changes) and persists via `setDocWidth`; toggling
  to Focus restores it; default is Full.
- [ ] Replace the hard `[&>div]:max-w-3xl` with a width-mode-driven class; add the
  `.neev-prose` measure + wide-block overrides in CSS.
- [ ] Tests green; eslint/build clean; commit.

### Task D: Human-first Team Chat thread; agent → right context panel
**Files:**
- Modify: `src/components/neev/native-chat.tsx` (human log `:292-329`, composer
  `:331-394`, agent panel `:356-431`)
- Create: `src/components/neev/native-context-panel.tsx` (collapsible right panel)
- Test: `src/components/neev/native-chat.test.tsx`,
  `src/components/neev/native-context-panel.test.tsx`

**Interfaces — Consumes:** `getRightPanelOpen/setRightPanelOpen` (Task A).
**Produces:** the human message thread is the primary content (full-height list +
pinned composer); the "Ask agent" control + agent stream move into
`NativeContextPanel` (collapsed by default), toggled by a header button. The agent
disabled state stays honest (button disabled + "Agents are turned off" copy when
`runtimeEnabled` is false — unchanged logic, relocated).

- [ ] Failing test: with the panel closed, the chat renders the human thread +
  composer and NO agent controls in the main column; a "context panel" toggle is
  present.
- [ ] Failing test: opening the panel renders the agent controls; when
  `runtimeEnabled=false` the Ask-agent button is disabled with honest copy (not a
  fake "connected").
- [ ] Implement `NativeContextPanel` + move agent UI into it; keep all existing
  agent run/stream/cancel behavior intact.
- [ ] Tests green; eslint/build clean; commit.

### Task E: Responsive collapsible panels (rail + context)
**Files:**
- Modify: `src/components/neev/native-workspace.tsx` (mobile drawer `:449-475`),
  `src/components/neev/native-context-panel.tsx`
- Test: `native-workspace.test.tsx`, `native-context-panel.test.tsx`

**Produces:** desktop = rail + canvas + optional panel; tablet = rail collapses to
icons, panel overlays; mobile (<768px) = rail + panel as drawers, single-column
canvas. Reuse the existing mobile `Dialog` sidebar pattern. `prefers-reduced-motion`
respected; Esc closes drawers; visible focus rings.

- [ ] Failing test: at a narrow width the context panel renders as an overlay
  drawer (role=dialog) and closes on Escape; the rail toggle is reachable.
- [ ] Implement responsive classes + drawer behavior.
- [ ] Tests green; eslint/build clean; commit.

### Task F: Honest paste (plain-text fallback + truthful notices) + control discoverability
**Files:**
- Modify: `src/components/neev/native-rich-editor.tsx` (paste handler `:98-121`,
  slash/formatting/side controls `:172-288`)
- Test: `src/components/neev/native-rich-editor.test.tsx`

**Produces:** on unsupported rich/HTML paste, extract clipboard `text/plain` and
insert it as plain text (formatting dropped) with notice "Pasted as plain text —
formatting was removed"; when there is no usable plain text, insert nothing and
show "Couldn't paste that here yet" — never claim text was kept when it wasn't. No
schema change; rich-paste sanitization NOT added. Also: surface the existing block
controls (hover block handle, `/` hint, keyboard shortcut hints).

- [ ] Failing test: pasting HTML with a `text/plain` alternative inserts the plain
  text and shows the "Pasted as plain text" notice.
- [ ] Failing test: pasting a files-only clipboard inserts nothing and shows the
  "Couldn't paste" notice (no "kept" claim).
- [ ] Implement the fallback in the paste handler; update notice copy; add control
  discoverability affordances.
- [ ] Tests green; eslint/build clean; commit.

### Task G: Phase 0 gate — full suite + build + independent review + browser acceptance
- [ ] `pnpm eslint .` clean; `pnpm test` all green (≥ prior 178 + new); `pnpm build`
  static export clean.
- [ ] Independent review (subagent) of the Phase 0 diff for correctness/honesty
  (no faked states, no "channels" label, width is per-user not doc metadata,
  Docmost untouched). Fix findings.
- [ ] **Real-browser visual acceptance** (desktop + mobile): screenshots + interaction
  checks of full-canvas docs, width toggle, human-first team thread, agent panel,
  responsive drawers — saved under `.context/ux-phase0-screens-<date>/`.
- [ ] Secret scan the outgoing diff; push to Neev remote `workos/team-workspace`
  (no force); build + deploy the new image with rollback; live smoke on app.neev.so.
- [ ] Update `.context/NEEV-UX-IMPL-STATUS.md` (commits, evidence, screenshots).

## Self-review (spec coverage)
- Nav sections A/B; full-canvas+width C; human-first chat + agent panel D;
  responsive E; discoverable controls + honest paste F; gate/review/browser/deploy
  G. Covers spec §2 (shell), §3 (docs width/measure, controls, paste), §4
  (human-first, org-conversation wording), §7 (responsive), §8 (a11y/states/motion,
  partially — full a11y verified in G), §10 acceptance (two-user + real agent run
  are Phase 1+; Phase 0 browser acceptance covers layout/panels/paste).
- Deferred to later phases (not Phase 0): agent sessions/persistence/reconnect
  (§5), real-time delivery, divider/image blocks, channels/unread/threads.
- No placeholders: each task names files, the change, and concrete test assertions.
