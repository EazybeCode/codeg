# Neev workspace UX design — Agent Chats · Team Chat · Documents

Status: **design, approved direction; not implemented.** Source of findings:
`.context/NEEV-UX-AUDIT.md` (source-linked). Scope: the **native** surface only
(`src/components/neev/*`, `src/lib/neev-*`, `src-tauri/src/neev/*`). **Docmost is
frozen** — nothing here touches it. References: Conductor (agent sessions), Slack
(team chat), Notion + Type.com (a shared **multiplayer AI workspace**, not just a
writing editor).

> Honesty note: this spec proposes work; it does not describe finished features.
> Where something exists today it is marked **[exists]**; where it is new it is
> **[new]**. The **source audit did not open the deployed app in a browser** — a
> visual desktop+mobile browser review of app.neev.so is a required acceptance gate
> (§10), not an assumption.

## 1. Goals / non-goals
- **Goals:** one coherent shell with three explicit sections — **Agent Chats**,
  **Team Chat**, **Documents** — full-canvas main views, collapsible navigation and
  a collapsible right context panel; durable multi-turn agent sessions with
  server-owned history and honest connection states; team-visible agent replies
  only when explicitly invoked; a fuller-but-safe document editor.
- **Non-goals (this spec):** implementation, deployment, enabling the agent runtime
  (still gated on runtime-enable + provider sign-in, §5.4), Docmost changes,
  reworking auth/identity, and any block that cannot persist under the existing
  validation+export invariants (§6).

## 2. Information architecture & shell
One shell (evolve `native-workspace.tsx`, already full-width `<main flex-1>`):
- **Left rail (collapsible)** — two tiers: (a) section switcher **Agent Chats /
  Team Chat / Documents** + org/workspace switcher; (b) the existing project/folder
  tree **[exists]** scoped to the active section.
- **Center = full canvas** — one primary surface per section (§3–5). The hard
  centered `max-w-*` container cap is removed; readability is preserved by a
  typographic paragraph measure rather than a container cap (§3, §7).
- **Right context panel (collapsible)** — contextual: the agent assistant attachable
  to the current doc/team-chat, or details/peek. Default collapsed on first load.
- Section + panel state persists per user (localStorage; server-side pref later).
- Resources stay `ResourceKind` rows; **Agent Chats** and **Team Chat** are distinct
  section presentations, not (yet) new backend kinds — see §5.3/§4.4 for the model.

## 3. Documents (Notion/Type-style canvas)
- **[new] Width preference — per-user PRESENTATION pref, NOT document metadata.**
  **Full-width ↔ Focus** is a viewer setting stored per user (localStorage, then a
  server-side user pref), never written onto the document — the same doc must render
  identically for every reader except for their own chosen width. Implemented by
  lifting/parameterizing the child cap at `native-document.tsx:521`
  (`[&>div]:max-w-3xl`) — container-only, no backend, no doc-schema change.
- **[new] Full-canvas ≠ edge-to-edge prose.** In Full-width, running text keeps a
  readable paragraph **measure** (typographic max line length, e.g. ~72–90ch) while
  **tables, images, code blocks, and embeds expand** to the available width. So
  "full canvas" widens the workspace and wide blocks, not the reading column.
- **[exists] Editor/rich-view impose no width limit** — they follow the container.
- **[exists] BlockNote** stays; keep the curated slash menu, formatting toolbar, side
  menu. **[new]** make the existing controls more discoverable (block handle on
  hover, `/` hint, keyboard shortcuts) — no schema change.
- Editor block roadmap: §6.
- Multiplayer (Type.com-style concurrent editing) is **explicitly deferred** — the
  current model is single-writer with revisions/restore **[exists]**; real-time
  co-editing is a separate large effort (§9, not in v1).

## 4. Team Chat (dedicated human conversations)
- **[exists] backend is already team-shareable:** `neev_messages` is org-scoped,
  multi-user, persisted, paged, quota'd; org-visibility = all members, private =
  creator (`messages.rs`, `access.rs:93-97`; tested `message_tests.rs:12-59`).
- **[new] presentation:** a dedicated Team Chat surface where the **human thread is
  the primary content** (full-height list + pinned composer), not an agent prompt
  box. The current agent controls move to the right panel (§5).
- **[new] real-time delivery:** replace 5s polling (`use-neev-content-poll.ts`) with
  server push (SSE or WebSocket) for messages. **Staged** (§9): v1 may keep polling
  with a clearly-labeled "updates every few seconds"; real-time is phase 2.
- **[new, staged & explicit]** unread indicators, threads, and @mentions are
  **named but deferred** to later phases (§9) — v1 does not claim them.
- **ACL honesty:** reuse current org/private ACLs. **Do NOT** present org-visible
  chats as member-restricted "channels" — call them **org conversations** until the
  backend enforces per-conversation membership. "Channels/membership" is a phase-2
  backend item; the UI must not imply access control the server doesn't enforce.

## 5. Agent Chats (Conductor-style sessions)
### 5.1 Session model **[new]**
- A durable **agent session** entity (server-owned): id, owner user, org, optional
  bound chat/doc, provider/model, created/updated, status. Holds an ordered,
  server-persisted **run history** (turns: prompt + streamed result + status),
  enabling multi-turn context and resume. Replaces today's one-shot, non-persisted
  `RunRequest`/`agentText` (`execution.rs:32-39`, `native-chat.tsx:62,179`).
- **Distinguish two gaps** (do not conflate): (a) **auth prerequisite** — the
  runtime is Disabled + provider sign-in absent (§5.4); (b) **missing session
  backend** — even with auth, sessions/history/resume do not exist yet and must be
  built. Both are required for the Conductor UX; they are independent work items.
- Concurrency: today a **global 2-lane** cap (`execution.rs:259-285`). Sessions are
  persistent (many can exist); only *active runs* consume a lane. **A "queued" state
  is shown ONLY if a real server-side run queue is implemented**; until then, when
  both lanes are busy the UI shows an honest **Busy — try again** (the backend
  returns `Unavailable`), never a fake queue or a fake "connected".

### 5.2 Connection & run-continuity states **[new] — honest, backend-confirmed**
Explicit, user-visible states per session, each mapped to real backend truth
(capability `runtimeEnabled` `http/mod.rs:218`, provider-auth status, and
run/session state) — never a placeholder "Connected" when the runtime is Disabled:
**Disabled** (runtime off) · **Not signed in** (provider auth missing) · **Connect**
(idle, ready) · **Testing…** (a bounded, side-effect-free provider check) ·
**Connected** · **Running** · **Reconnecting…** · **Failed (reason)**.
Required semantics (do not ship the state without the semantics):
- **Reconnect = reattach + replay, never silent re-run.** On a dropped stream the
  client reattaches by run id to the *same* server-owned run and replays already-
  persisted output from the run history (§5.1); it MUST NOT start a new run or
  re-execute a prompt (which could repeat side effects). If the run already finished,
  reconnect replays the stored final result. If the run is truly gone/unrecoverable,
  show **Failed** and require an explicit new run — do not auto-rerun.
- **Restart is explicit + user-initiated** (cancel + a new run), clearly labeled as a
  new turn — never automatic on disconnect.
- These require server-owned run history to exist first; today the SSE is consumed
  once and cannot be reattached, so this is **[new backend]**, not present.

### 5.3 Privacy model **[new, firm default]**
- **Private by default:** an Agent Chat session is the owner's private assistant;
  its prompts/results are visible only to the owner and are **never** written into
  team-visible `neev_messages`.
- **Explicit team invocation:** when a user explicitly asks the agent *inside a Team
  Chat* (e.g. `/agent …` or an "ask the team's agent" action), the result is
  persisted as a **team-visible message clearly labeled as agent-authored**
  (distinct author kind, not impersonating a human — today `author_id` is always a
  human account, `messages.rs:24`; a new agent author kind is required).
- **Never silently publish private context:** a team-invoked agent turn must not leak
  the user's private session history; only the explicit prompt + its reply are
  shared. The UI must make "this will be posted to the team" unmistakable before send.

### 5.4 Credentials **[new] — per-user/per-provider; no shared host login**
- **Default = per-user, per-provider credentials.** Each user signs in to each
  provider through an explicit sign-in workflow; credentials are stored per user and
  delivered only into that run's isolated 0700 workdir owned by the run uid, never
  world/group-readable. The single-shared-host-`claude`-login shortcut is
  **explicitly rejected** (it would silently reuse one identity across tenants).
- **Org-shared/service credentials are DEFERRED** — not part of this default and not
  built unless a separate, explicit decision adds them (with their own consent +
  isolation review). This spec assumes per-user only.
- **Team invocation uses the INVOKER's own authorized connection.** When a user
  invokes the agent in a Team Chat, the run executes under that invoker's per-user
  credentials; the credentials themselves are **never exposed** to other members —
  only the resulting labeled reply (per §5.3) is shared. No other member's or a
  shared identity's credentials are used.
- See `NEEV-PROVIDER-SIGNIN-ENABLE.md` for the current host prerequisite. The
  least-privilege sandboxed agent image + launcher **[exists, validated]**
  (unprivileged server + file-capped helper); this spec adds the per-user
  session/credential layer above it, not new isolation.

## 6. Editor v1 blocks & deferrals
Hard invariant **[exists]**: every block must pass all four layers (schema, UI, TS
adapter/model, Rust backend) **and render losslessly to Markdown** — Markdown export
gates persistence (`rich.rs:278-284`, `rich_export.rs`).

- **v1 — include (lossless Markdown, low risk):**
  - **Divider / thematic break** → `---`. Trivial across all four layers.
  - **Image** → `![alt](url)`. **NOTE — image storage/serving is NEW work, not
    reuse.** The existing `BlobStore` (`content/blob.rs`) stores the *document* blob
    only: it is sha256-addressed, per-org, **1 MiB** capped, and has **no
    content-type/mime field and no HTTP upload or serve route** (verified). Images
    therefore require new backend endpoints + validation, listed explicitly (do not
    assume any of these exist):
    - an **authenticated upload** endpoint (Origin+CSRF+session like other mutations;
      multipart; content-type sniff + allowlist e.g. png/jpeg/webp/gif; size limit;
      dimension/decoder-safety checks; per-org quota),
    - an **authenticated serve** endpoint enforcing the chat/doc **ACL** (only members
      who can see the resource can fetch its image; no public/unauth URLs),
    - **mime-aware storage** (extend `BlobInfo`/`BlobStore` with content-type, or a
      new image store) with an image-appropriate size cap,
    - adapter + TS model + Rust `RichBlock` variant + `rich_export` arm, and a
      persisted image reference that renders losslessly to `![alt](url)`.
    Medium–large; highest user value — but scoped as new work, not "reuse complete".
- **v1 — polish (no schema change):** discoverable slash/formatting/side controls,
  keyboard shortcuts, block drag handle.
- **Rollback safety for new blocks [required]:** see §9 — a divider/image block, once
  written, must remain readable/renderable by any binary a rollback could land on.
  Ship new-block support behind a **feature flag** and only after the *reader* is
  deployed everywhere; never roll back the server/client to a binary whose validators
  would reject already-stored divider/image content.
- **Deferred — require an export-model decision first (they have no lossless
  Markdown):** callouts, toggles/collapsibles, multi-column layouts, text color,
  alignment. Enabling any needs either (a) extending the persisted/export format to a
  documented superset (not plain Markdown), or (b) an explicit lossy-export policy —
  a deliberate architectural decision, out of v1.
- **Paste [current vs v1] — truthful, with a real fallback.**
  - **Current behavior [exists]:** paste is routed through a refuse-unknown Markdown
    allowlist; unsupported input (HTML, files, unknown syntax) is **rejected and
    NOTHING is inserted** (`native-rich-editor.tsx:98-121`). So the notice must NOT
    say "plain text was kept" — today nothing is kept.
  - **v1 [new] — deliberate "Paste as plain text" fallback:** on unsupported rich/
    HTML content, extract the clipboard's `text/plain` and insert THAT as plain text
    (formatting dropped), then show a truthful notice: *"Pasted as plain text —
    formatting was removed."* Only claim text was kept when it actually was.
  - **When there is no usable plain text (e.g. files/images-only clipboard):** insert
    nothing and show *"Couldn't paste <that content> here yet"* — do not claim a
    fallback that didn't happen.
  - **Rich-paste sanitization is future work:** do NOT describe HTML→blocks
    sanitization as available; it is only claimed once the sanitizer + its four-layer
    validation exist. Until then, rich paste degrades to the plain-text fallback above.

## 7. Responsive & layout
- **Desktop (≥1024px):** left rail + full-canvas center + optional right panel
  (three columns; panel overlays at ~1024–1280px).
- **Tablet (~768–1024px):** left rail collapses to icons; right panel becomes an
  overlay drawer.
- **Mobile (<768px):** single column; left rail + right panel are drawers (matches
  the existing mobile `Dialog` sidebar, `native-workspace.tsx:449-475`); composer and
  section switcher pinned; full-width canvas.
- Full-width docs must still cap line length for readability via typographic measure
  (not a hard container cap) so "full-width" ≠ unreadable long lines.

## 8. Cross-cutting states, keyboard & accessibility
- **Loading:** skeletons for lists/threads/canvas; never layout-shift on load.
- **Saving:** documents show explicit Saving…/Saved/Save-failed (retry) — reuse the
  existing revision save path; team messages show sending/sent/failed(retry);
  optimistic append with reconciliation on poll/push.
- **Error:** honest, specific (`agent_runtime_disabled` → "Agents are turned off",
  not a generic failure); access-denied → clear "you don't have access".
- **Motion:** panel/drawer transitions ≤200ms; respect `prefers-reduced-motion`;
  streaming agent text appends without scroll-jank.
- **Keyboard:** section switch, new doc/chat, send (Enter / Shift+Enter newline),
  slash menu, focus/full-width toggle, panel toggle — all reachable; visible focus
  rings; Esc closes drawers/dialogs.
- **A11y:** roles/labels for rail (nav), thread (log/feed with `aria-live` for new
  messages), agent stream (`aria-live=polite`), editor (existing BlockNote a11y);
  color-contrast AA in light+dark; agent-authored messages labeled for screen readers.

## 9. Phased rollout & rollback
- **Phase 0 (frontend-only, no gate):** shell sections + collapsible panels;
  full-width/focus doc toggle; make Team Chat's human thread primary and move agent
  controls to the right panel; editor control discoverability. Rollback: revert the
  frontend commit; no data/schema change.
- **Phase 1 (agent sessions + persistence):** server-owned session + run-history
  entity; persisted agent turns; honest Connect/Test/Reconnect; team-invoked
  agent-authored messages (new author kind) with the §5.3 privacy guarantees.
  Rollback: feature-flag the session UI off; agent falls back to the current
  one-shot path; the new tables are additive (old code ignores them).
- **Phase 2 (real-time + editor v1 blocks):** SSE/WebSocket message delivery
  (replace polling); divider + image blocks (all four layers + export).
  - Real-time rollback: disable the push transport → fall back to polling (no data
    change). Safe.
  - **Editor-block rollback is NOT symmetric and coordinated-rollback alone does NOT
    protect documents.** Once a divider/image block is written, a binary whose
    validators reject it can no longer *read* those documents (validation runs on
    decode/read, `rich.rs:285-293`). Therefore:
    - **Deploy the READER first, everywhere:** ship a version that can parse/render/
      round-trip the new block types (even if creation stays behind a flag) before any
      client can create them. Only enable *creation* after every reader is updated.
    - **Prohibited:** rolling the server or client back to any binary that cannot read
      already-written divider/image content. That is a **forward-only** step for the
      read path once such content exists.
    - **Feature flag gates CREATION, not the reader.** Turning the flag off stops new
      such blocks but must leave existing ones readable.
    - If an emergency downgrade past the reader is ever unavoidable, it requires a
      data **migration that rewrites new blocks into old-readable form** (e.g. image →
      a link/paragraph, divider → removed) BEFORE downgrading — never a bare binary
      swap.
- **Phase 3 (staged, explicit):** per-conversation membership/channels, unread,
  threads, @mentions; deferred editor blocks pending the export-model decision.
- Each phase behind a flag; DB changes additive; documents/DB/Docmost untouched by
  rollback. Provider-gated agent execution stays off until §5.4 is satisfied.

## 10. Acceptance scenarios (must pass before "done")
1. **Two users, team chat:** user A posts in an org conversation; user B (same org)
   sees it (via push in phase 2, or ≤poll-interval in phase 1); a foreign-org user is
   denied (reuse `message_tests` guarantees).
2. **Refresh persistence:** after a page reload, human messages AND persisted agent
   turns (team-invoked or in a saved session) survive; a *private* one-shot with no
   session does not leak into team history.
3. **Access denial:** a user without access to a private chat/doc gets a clear denial,
   not a blank/errored canvas; org-visible items are not labeled as restricted
   channels.
4. **Real authenticated agent run:** with the runtime enabled + provider sign-in,
   an explicit team invocation streams a reply that renders, is cancelable, and is
   persisted as a labeled agent-authored message visible to the team; a private
   session run stays private. **A stub/PONG does not satisfy this** — it requires a
   real provider run.
5. **Visual browser review (required):** desktop + mobile screenshots of the live
   app confirm full-canvas docs, primary team thread, agent panel, and responsive
   drawers — the source audit did not validate deployed layouts.

## 11. Self-review (contradictions / placeholders)
Review-round fixes (2026-09-20):
1. **Paste truthfulness (§6):** current behavior rejects unsupported paste and
   inserts NOTHING; the spec no longer claims "plain text was kept". v1 adds a real
   **Paste-as-plain-text** fallback with notices that match what actually happened;
   rich-paste sanitization is future work, not claimed.
2. **Rollback safety for new blocks (§6, §9):** removed the false "coordinated
   client/server rollback protects documents" claim. Reader-first deploy, feature-
   flag gates *creation* only, rollback to a binary that can't read stored divider/
   image content is **prohibited** (read path is forward-only once such content
   exists); emergency downgrade needs a rewrite migration first.
3. **Width prefs (§2, §3):** are **per-user presentation prefs, not document
   metadata**; full-canvas keeps a readable paragraph measure while tables/images/
   code expand. Fixed the bad "§6.1" reference (points to §3/§7).
4. **Credentials (§5.4):** default is **per-user/per-provider**; org-shared is
   deferred unless separately decided (removed the ambiguous "or per-org"); team
   invocation runs under the **invoker's** connection without exposing credentials.
5. **Run continuity (§5.1, §5.2):** reconnect **reattaches + replays** persisted
   output and **never silently re-runs** side effects; restart is explicit; a
   "queued" state is shown only if a real server queue exists — otherwise honest
   **Busy**. All require the (new) server-owned run history to exist first.
6. **Images (§6):** corrected — `BlobStore` is a 1 MiB, mime-less, route-less
   *document* store; image upload/serving is **new work** (auth upload + ACL serve +
   mime-aware storage + validation endpoints), not assumed-complete reuse.

Standing invariants:
- "Channels" wording vs ACLs: org conversations until membership is enforced (§4).
- Agent "Connected" honesty: states map to real backend truth (§5.2).
- Editor ambition vs export invariant: v1 = lossless blocks only; richer blocks gated
  on an export-model decision (§6) — not promised as done.
- Multiplayer co-editing (Type.com direction) explicitly deferred (§3/§9).
- No feature is described as existing unless marked **[exists]**; no placeholders
  remain unresolved.
