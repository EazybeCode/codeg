# Conductor-style chat UX — read-only audit of the current Neev surfaces

Date: 2026-09-25. Scope: **audit only**. No application code was modified, no
branch was created, nothing was committed or pushed. Backend and extension
workers are active on this tree; every finding below is a reading of the working
tree as it stands.

## What the reference is, and what it is not

The reference is the user's own observation of the Conductor desktop app on
screen. No Conductor source was read, decompiled, or inferred, and nothing here
claims to describe its implementation. The target is a **layout and density
target**, stated as observed measurements:

| Element | Observed in Conductor |
| --- | --- |
| Left workspace sidebar | 248px |
| Central chat column | ~708px |
| Top tab row | one narrow row, ~40px |
| Right panel | optional files/terminal |
| User prompt | small, right-aligned, quiet tinted bubble |
| Assistant prose | left, system sans, 13–14px on ~22px line |
| Tool call | one compact row: terminal icon + `Bash` + muted, clipped monospace command on a subtle dark pill; output not expanded by default |
| Interleaving | commentary and tool rows in chronological order |
| Running state | one small spinner, then tabular elapsed — `1m`, `0.0s` |
| Composer | inset 16px left/right, radius 12px, subtle border, textarea on top, model bottom-left, plus + Stop bottom-right |
| Headers | none large |

Neev numbers below are read out of the source (Tailwind classes resolve to fixed
px: `h-9`=36, `h-10`=40, `max-w-3xl`=768, `text-sm`=14, `px-4`=16, `sm:px-8`=32,
`p-2`=8, `p-3`=12, `rounded-lg`=8, `rounded-xl`=12, `size-3.5`=14). No Neev
measurement here was taken from a screenshot; none was invented.

## The two surfaces

Neev has two chat surfaces and they are deliberately different products:

- **Team chat** — `src/components/neev/native-chat.tsx` (2009 lines). A shared
  thread: humans and agent replies in one list, agent replies attributed to the
  provider *and* the human who invoked them
  (`native-chat.tsx:1573-1582`). Mode toggle Agent/Team lives in the composer
  (`native-chat.tsx:1808-1846`).
- **Private agent chat** — `src/components/neev/native-agent-chats.tsx` (1982
  lines). Prompt/response turns, always private (`native-agent-chats.tsx:1592-1604`).

They already share the trace renderer: `ActivityTimeline` is defined in
`native-agent-chats.tsx:984` and re-exported through
`native-chat-activity.tsx:15` precisely so both draw a trace identically. That
shared seam is the single most valuable fact in this audit — one edit changes
both surfaces.

**Human/agent distinction must survive any restyle.** Today it is carried by
three things, all of which are load-bearing and none of which should be
collapsed into "a bubble": the avatar/initial vs `Sparkles`
(`native-chat.tsx:1546-1555`), the quiet fill + border on human messages only
(`native-chat.tsx:1562-1567`, with the reasoning in the comment above it), and
the `agentAuthored` attribution chip (`native-chat.tsx:1578-1582`).

## What already matches the target

Recording these so the plan does not churn code that is already right.

1. **Sidebar is already 248px** — `native-workspace.tsx:2346`
   (`w-[248px] shrink-0 border-e bg-sidebar`).
2. **Prose typography is already in range** — `native-markdown.tsx:198`:
   `text-sm leading-[1.65]` = 14px on 23.1px, against the 13–14/~22 target.
   Headings are deliberately near body size and carry hierarchy by weight
   (`[&_h1]:text-[17px]`, `[&_h2]:text-[15px]`, `[&_h3]:text-sm`), i.e. "no
   giant headers" is already the rule inside a reply.
3. **Real status, not fake thinking.** `isNoise` in
   `src/lib/neev-agent-activity.ts:143` drops the provider's `model`/`stop`/
   `result` bookkeeping; `runningTool` (`:163`) names the tool actually in
   flight by finding the last call with no result. Nothing synthesises a log
   line. This property is the thing most at risk from a cosmetic rewrite and is
   called out again in the plan.
4. **Elapsed clock is already honest about precision.** `elapsedDisplay(ms,
   precise)` (`neev-agent-activity.ts:199`) shows a tenth only for the live
   clock and whole seconds for a duration recomputed from stored second-
   resolution timestamps. `ElapsedClock` (`native-agent-chats.tsx:931`) counts
   on `performance.now()` so an NTP step cannot make the number jump, ticks at
   `ELAPSED_TICK_MS`=100 in its own component so one `<span>` repaints, and is
   `aria-hidden` so a live region does not announce it ten times a second.
5. **Streaming markdown is correct.** `NativeMarkdown` passes
   `parseIncompleteMarkdown` only while streaming and explains why it must be
   false for stored text (`native-markdown.tsx:148-170`).
6. **The chronological data model already exists, end to end.** Every activity
   item carries `offset`, a Unicode-scalar position into the reply text:
   assigned server-side in `src-tauri/src/neev/agents/activity.rs:81` and
   `:639`, streamed on the same SSE connection as the text chunks
   (`src/lib/neev-agent-sessions-client.ts:2008-2029`, one stream, two
   callbacks, so ordering is preserved), typed at
   `neev-agent-sessions-client.ts:140-181`, and sliced scalar-safely by
   `sliceByScalars` (`:191`).

## Gaps, in priority order

### P0-1 — Tools are not interleaved with prose; they are a single group below it

Both surfaces render the whole reply, then one `ActivityTimeline` under it:
`native-agent-chats.tsx:1650-1684` (live: `liveText` markdown, then the timeline
in a `pt-3` wrapper) and `native-chat.tsx:1640-1690` (same shape). The stored
case is the same (`native-chat.tsx:1617-1636`).

The function that fixes this **already exists and is already tested but is
called from no production file**: `splitByActivity`
(`src/lib/neev-agent-activity.ts:45`), exercised in
`src/lib/neev-agent-activity.test.ts:62,73,80`. A repo-wide grep for
`splitByActivity` finds only the definition and the test.

So the architecture question the brief asks — "what is needed for chronological
segments without fake logs" — has the answer: **nothing new**. The server
already records where in the text each tool ran; the client already knows how to
cut the text there; the renderer just does not call it. No log line would be
invented, because every segment boundary is a real recorded offset and every row
is a real recorded item.

**Ordering hazard, evaluated.** Two hazards are worth naming and both are
already contained:

- *Transport ordering.* Text and activity arrive on one SSE stream
  (`openAgentRunStream`, `neev-agent-sessions-client.ts:2008`) through two
  callbacks, so a tool row can never be delivered before the text that precedes
  it. There is no cross-stream race to fix.
- *Clamping during stream.* An item's offset can momentarily exceed the text
  received so far. `splitByActivity` already clamps with
  `Math.max(cursor, Math.min(item.offset, length))` (`:57`), so such an item
  renders at the tail and settles into place as text arrives. The visible cost
  is a short reflow, not a wrong order. This is strictly better than today's
  behaviour, where *every* tool sits below *all* text permanently.
- *Reattach doubling.* `mergeActivity` (`:25`) upserts on `seq`, so the replayed
  prefix does not duplicate rows. Already handled.

### P0-2 — The agent chat force-scrolls and ignores explicit user scroll

Team chat gets this right: `followHistory` is set from an `onScroll` handler
with a 48px bottom threshold (`native-chat.tsx:1523-1527`) and the
`useLayoutEffect` only pins when `followHistory.current` is true (`:411-415`).

The agent chat does not: `native-agent-chats.tsx:1404-1407` sets
`element.scrollTop = element.scrollHeight` on every change of `turns` or
`liveText`, with no follow flag and no scroll listener. Scrolling up to read
during a live run yanks the view back on the next token. This is a correctness
bug in its own right and gets worse under P0-1, since interleaved rows change
height more often.

### P1-3 — Tool rows have no tool identity and no pill

Expanded rows today are `name` in `font-medium` plus `describeToolInput(...)` in
`font-mono text-xs text-muted-foreground`, in a plain `hover:bg-accent` button
(`native-agent-chats.tsx:1128-1138`). There is no per-tool icon and no tinted
surface behind the command; `describeToolInput`
(`neev-agent-activity.ts:102-124`) already extracts the right field
(`command`/`file_path`/`path`/`pattern`/`url`/`query`/`prompt`) and clips at 80
scalars, so the content is right and only the chrome is missing.

Also: rows are only visible after opening the group. Conductor shows each tool
as a compact row *in place*, always. That is P0-1's job; this item is the row's
own appearance once it is there.

### P1-4 — Elapsed never renders minutes

`activityElapsed` is `"{number}s"` (`src/i18n/messages/en.json:187`) and
`elapsedDisplay` returns a bare seconds count. A ten-minute run reads `612.4s`
where the target reads `10m 12.4s`. Ten locale files carry this string, so the
fix is a message-shape change across `src/i18n/messages/*.json`, not a one-file
edit. `src/components/neev/native-locales.test.ts` guards message parity — check
it before changing the placeholder set.

### P1-5 — Vertical chrome above the transcript is ~112px against a ~40px target

Stacked, currently: breadcrumb bar `h-10`=40px
(`native-workspace.tsx:2353`), tab strip `h-9`=36px (`:2427`), chat header
`h-9`=36px (`native-chat.tsx:1387` and `native-agent-chats.tsx:1578`). The tab
strip is already narrow and each bar is individually justified; the gap is that
there are three of them. Merging breadcrumb into the tab row is the obvious
candidate and is a workspace-shell change, not a chat change — it is sequenced
last for that reason.

### P2-6 — Composer geometry differs between the two surfaces

| | Team chat | Agent chat | Target |
| --- | --- | --- | --- |
| Form inset | `px-4 sm:px-8` (`native-chat.tsx:1699`) | `px-4 sm:px-8` (`native-agent-chats.tsx:1701`) | 16px |
| Box | `rounded-xl border p-2` (`:1704`) | `rounded-lg border p-3` (`:1706`) | radius 12, subtle border |
| Min height | `min-h-14` (`:1765`) | `min-h-16` (`:1735`) | — |

`rounded-xl` is 12px and matches; `rounded-lg` is 8px and does not. Two surfaces
disagreeing on the radius and padding of the same control is the actual defect;
the target value settles which way to converge.

### P2-7 — Stop is a mid-row ghost button, not a bottom-right control

`Cancel` renders inline before Send in both surfaces
(`native-chat.tsx:1936-1945`, `native-agent-chats.tsx:1833-1841`), with Send
pushed right by `ms-auto`. The target pairs plus and Stop at bottom-right.
Note Send's label is deliberately always "Send" and only goes disabled while
busy (comment at `native-chat.tsx:1949-1951`) — a Send→Stop morph would undo a
decision already made on purpose. Prefer Stop *beside* Send.

### P2-8 — Human prompt is a full-width left block, not a small right-aligned bubble

Agent chat: `rounded-lg bg-muted/60 px-4 py-3` full column width
(`native-agent-chats.tsx:1644`). Team chat: avatar + `rounded-lg border
bg-muted/30 px-3 py-2`, left (`native-chat.tsx:1545-1567`).

This is the one gap where the target should be adopted **only in the agent
chat**. In team chat, right-alignment would mean "me" — and a shared thread has
several people in it. Right-aligning one human's messages there would either
misrepresent the others or require per-reader alignment, which breaks the
shared-transcript property. Recommendation: right-align the prompt bubble in the
private agent chat, leave team chat left-aligned with avatars.

### P3-9 — Chat column is 768px against a ~708px target

`max-w-3xl` on each item (`native-chat.tsx:1545`, `native-agent-chats.tsx:1643`)
inside a `max-w-[832px]` list (`native-agent-chats.tsx:1640`). 60px of measure.
Cosmetic; do it with P1-3 or not at all.

### Out of scope, stated rather than silently dropped

Conductor's optional right files/terminal panel has no Neev counterpart: tabs
are `empty`/`project`/`resource`/agent-session
(`native-workspace.tsx:2497-2535`), and Neev is a document-and-chat workspace,
not a code IDE. Replicating it is a product decision, not a UX gap, and is not
planned here.

## Bounded implementation plan

Four slices. Each is independently shippable and independently revertable.

### Slice A (the recommended first UI slice) — chronological segments + scroll override

One file pair, one shared component, no schema and no API change.

1. Extract the per-item row from `ActivityTimeline` into an exported
   `ActivityRow` in `native-agent-chats.tsx` (it is already the shared home via
   `native-chat-activity.tsx`).
2. Add a `TranscriptBody` that takes `{ text, items, streaming }`, calls
   `splitByActivity(text, visibleActivity(items))`, and renders
   `NativeMarkdown` + `ActivityRow[]` per segment. `streaming` continues to gate
   `parseIncompleteMarkdown` exactly as today.
3. Swap the four call sites: `native-agent-chats.tsx:1650-1684` (live and
   stored) and `native-chat.tsx:1617-1636` + `:1671-1687`.
4. Keep the run footer — spinner, live clock, running-tool name — where it is,
   below the newest output and near the composer. The comments at
   `native-chat.tsx:1674-1678` and `native-agent-chats.tsx:1657-1658` record why
   it sits there; interleaving the rows does not change that reasoning.
5. Keep the group expander for the full input/output `<pre>`
   (`native-agent-chats.tsx:1154-1165`) as the per-row disclosure. `aria-expanded`
   stays on the row button; `disabled` when there is nothing to show stays too
   (`:1120-1122`).
6. Port `followHistory` from `native-chat.tsx:409-415,1523-1527` into
   `native-agent-chats.tsx`, replacing the unconditional scroll at `:1404-1407`.

Explicitly preserved by Slice A: `isNoise` filtering (no fabricated rows), the
`aria-hidden` clock outside the live region, `role="status"` on the busy
sentence, upsert-on-`seq` merging, human/agent distinction.

### Slice B — row appearance

Tool icon per name (terminal for shell-shaped tools, file for read/write, search
for grep/glob, globe for fetch; a neutral default, never a guessed one), muted
clipped mono command on a subtle tinted pill, error rows keep `text-destructive`.
Column width 768→708 if wanted. `describeToolInput` is unchanged.

### Slice C — clock and composer

`elapsedDisplay`/`activityElapsed` gain a minutes form across all ten locale
files; converge the two composer boxes on one radius/padding/min-height; move
Stop to sit beside Send.

### Slice D — shell

Merge breadcrumb into the tab row. Touches `native-workspace.tsx` only; the
riskiest for tab-state regressions, hence last.

### Separate, non-UI tasks (do not bundle into the slices above)

- **Schema** — none required. `offset` already exists and is already persisted;
  nothing in Slices A–D needs a migration. If per-tool timing (start/end per
  call, for a per-row duration) is wanted later, that *is* a schema task on
  `ActivityItem` in `src-tauri/src/neev/agents/activity.rs` plus its store, and
  must be planned on its own.
- **Engine** — none required. The mention/inbox work is already pending release
  (`docs/operations/agent-loading-ui-2026-09-24.md`, "Completion stability and
  mentions — release candidate": pending final Rust/PostgreSQL verification and
  backup, with a stated no-rollback-after-mentions constraint). **Slices A–D
  must ship after that candidate lands, not alongside it.**
- **Auth** — none required. No slice touches session, CSRF, or provider
  connection. The Connect/Reconnect chip
  (`native-chat.tsx:1899-1933`, `native-agent-chats.tsx:1799-1832`) is left
  exactly as is.

## Acceptance criteria

Measurable, and each a **before/after screenshot pair at the same viewport**
(1440×900 desktop; repeat at 390×844 for AC-1, AC-6, AC-7). "Before" = current
`main`-tree build; "after" = the slice under test. State the slice on every
pair.

| # | State | Criterion |
| --- | --- | --- |
| AC-1 | **Idle** | Empty transcript + composer. Chrome above the transcript measured in the screenshot; after Slice D it is one row. Composer radius and inset identical between team and agent chat after Slice C. |
| AC-2 | **Send** | Immediately after submit: the prompt is visible in the transcript, Send is disabled, and exactly one run indicator is on screen — not one per surface and not one per turn. |
| AC-3 | **First tool** | Within one tool call: one compact row, tool name legible, command clipped to one line, output **not** expanded. Row is keyboard-focusable and `aria-expanded="false"`. |
| AC-4 | **Mid-stream interleave** | A reply with ≥2 tool calls separated by prose shows prose → row → prose → row **in the order the offsets record**. Verified against the same turn's stored `activity` array, not by eye alone. |
| AC-5 | **Completed** | Live clock stops; the row shows a whole-second duration (no tenth); "Completed" appears once; error count, if any, is separate from the outcome. Regression guard for the defect recorded at `native-agent-chats.tsx:1005-1015`. |
| AC-6 | **Error** | A failed tool inside a completed turn: the row is `text-destructive`, the turn still reads as completed, and the error text is reachable by expanding. A failed *turn* shows `outcomeLabel` (`native-agent-chats.tsx:1927`) instead. |
| AC-7 | **Long scroll** | ≥40 turns. (a) Pinned at bottom → new output keeps the view pinned. (b) Scrolled up ≥48px during a live run → the view **does not move** for at least 10s of streaming. (c) Scrolling back to the bottom re-pins. This is the direct P0-2 regression test. |

Non-screenshot gates, all of which must pass before any slice is proposed for
release:

- `pnpm eslint .`
- `pnpm test` — with `src/lib/neev-agent-activity.test.ts`,
  `src/components/neev/native-chat-activity.test.tsx`,
  `src/components/neev/native-chat.test.tsx`,
  `src/components/neev/native-agent-chats.test.tsx` and
  `src/components/neev/native-locales.test.ts` named individually in the report
- `pnpm build` (static export)
- A reduced-motion pass: `prefers-reduced-motion: reduce` must still stop the
  spinner (`src/app/globals.css:2793-2800`)
- No new string that describes activity the server did not record. Grep the diff
  for added literals under `activity*` keys and justify each.

## What Slices A and B actually shipped, and what they did not

Implemented on 2026-09-25, in four source files: `src/lib/neev-agent-activity.ts`
(`pairResults`), `src/components/neev/native-agent-chats.tsx` (footer-only
`ActivityTimeline`, new `TranscriptBody`/`ActivityRow`/`OutputBlock`/`ToolIcon`,
scroll follow override, unified live/completed tree),
`src/components/neev/native-chat.tsx` (two render sites), and
`src/components/neev/native-chat-activity.tsx` (re-export).

Three decisions worth recording, because each was a judgement and not a
mechanical port:

1. **`ActivityTimeline` is now the progress footer, not a trace.** With rows
   drawn in place, a collapsed drawer holding the same rows would be the run
   described twice. What is left on the footer is only what the rows cannot
   say: that something is still happening, the live clock, the duration, the
   error aggregate, and the turn's outcome. The running-tool sentence is
   `sr-only` — the tool in flight already has its own visible row, but a reader
   who cannot see that row still needs to be told.
2. **A result is folded into the call it answers** (`pairResults`), so one
   invocation is one row. Two things are deliberately NOT folded away and keep
   their own row: a result whose `callId` matches no call in the trace, and a
   second result for a call that already has one. Hiding either would drop
   recorded output to shorten a list.
3. **Segments that would draw neither a row nor text are filtered out.** A
   result recorded at the end of the text so far — the ordinary shape of a live
   run, not an edge case — leaves a segment with no rows (folded) and no text.
   Left in, each one is an empty `<div>` in a `space-y-2` stack, so the reply
   grows a blank gap every time a tool answers. The pre-filter index is kept
   for the streaming mark, and the React key is the segment's leading item
   `seq`, which does not shift as segments come and go.

### Known limitation, not fixed — team-chat live-to-stored handover

In the **private agent chat**, running and finished are one tree with
conditional props, so a tool row the reader opened stays open when the run ends
underneath them. This is covered by a regression test that was verified to fail
against the previous two-branch structure.

The **team chat** has no equivalent guarantee. Its live preview is a separate
`<li>` (`native-chat.tsx`, the `agentBusy || agentText` item) and the stored
reply is a different `<li>` in the message list, so the handover is a genuine
remount: an expanded tool row closes when the run completes. Unifying those two
list items is a larger change than Slice A's scope — it touches how the preview
is matched to the message that replaces it — and it was deliberately left out
rather than half-done.

**Follow-up:** unify the team-chat preview and stored-message items so the
private chat's stability property holds there too. Until then, AC-3's expander
state is guaranteed on the agent chat only.

### Verification recorded for this change

Focused suite (10 files: `neev-agent-activity`, `neev-agent-sessions-client`,
`native-agent-chats`, `native-chat`, `native-chat-activity`,
`native-chat-mentions`, `native-mentions`, `native-inline-composer-mentions`,
`native-chat-attachments`, `native-agent-chats-attachments`) — 186 passed, exit
0. `tsc --noEmit` — exit 0. `eslint` over the changed files — exit 0. Each was
run without a pipe so the reported status is the command's own: an earlier
`| tail` had masked a real prettier failure in a test file, which is why the
gates above should be read the same way. No whole-repo run is claimed.

Screenshot acceptance (AC-1…AC-7) is **not** claimed — those pairs have not
been captured.

## One-line summary

The data model for Conductor-style chronological tool rows is already built and
tested in this repo and is simply not wired up; the first slice is wiring
`splitByActivity` into the two transcripts and giving the agent chat the
scroll-override the team chat already has.
