# Neev Browser — extension ↔ native host message contract

Version `1`. This is the only interface between the two halves of the tool:
neither side may add or reshape a message type without changing this file
first.

Everything below is the **implemented host behaviour** in
`companion/neev_browser/host.py`, and the companion test suite enforces it.
The extension is written against this document.

## Transport

Chrome native messaging: `chrome.runtime.connectNative("com.neev.browser")`.
Each frame is a 4-byte little-endian unsigned length followed by UTF-8 JSON.
Chrome handles the framing on the extension side. Inbound frames over
**1 048 576 bytes** are rejected by the host.

One port per run. When the port disconnects the host revokes the run; when the
host ends a run it emits `run.ended` and the extension must detach.

Every message has a `type` and a `runId`. `runId` matches
`^[A-Za-z0-9_-]{1,64}$` — generate it in the extension (e.g.
`crypto.randomUUID().replace(/-/g, "")`).

---

## Extension → host

### `run.start`

Sent once, after the user has picked a tab, picked a provider, granted the
origin permission, and injected the content script.

```jsonc
{
  "type": "run.start",
  "runId": "a1b2c3",
  "provider": "claude",          // "claude" | "codex" — nothing else
  "task": "…",                   // 1..8000 chars, the user's typed instruction
  "target": { "tabId": 42, "url": "https://app.example.com/orders?x=1" },
  "model": "sonnet"              // optional, ≤100 chars, omit for the CLI default
}
```

The host derives the run's origin from `target.url`. Only `http` and `https`
are accepted; URLs carrying userinfo (`https://a@b/`) are rejected. The full
URL is never stored — only the origin.

Replies: `run.started`, or `error` with `code` one of `bad_message`,
`duplicate_run`, `start_failed`.

### `run.stop`

```jsonc
{ "type": "run.stop", "runId": "a1b2c3" }
```

Send on the Stop button, and also on tab close, port teardown, or side-panel
close. Idempotent. Always answered with `run.ended`.

### `target.revoked`

The extension has lost its grip on the target and access must end now.

```jsonc
{ "type": "target.revoked", "runId": "a1b2c3", "reason": "tab_closed" }
```

`reason` is a short slug, ≤64 chars: `tab_closed`, `tab_replaced`,
`permission_revoked`, `cross_origin_navigation`, `panel_closed`.
Answered with `run.ended` (`state: "revoked"`).

### `target.navigated`

Send on **every committed main-frame navigation** of the target tab, including
same-origin ones and history API document changes.

```jsonc
{ "type": "target.navigated", "runId": "a1b2c3", "url": "https://app.example.com/orders/9" }
```

- **Different origin** → the host revokes the run. Do not wait for it; detach
  immediately on your side too.
- **Same origin** → the host marks every outstanding element ref dead. The
  agent must call `browser_snapshot` again before it can click or type. The
  host emits a `run.output` notice so the user sees why.

This is what stops a ref from the previous document being applied to whatever
now sits in that position.

### `tool.result`

The answer to exactly one `tool.call`.

```jsonc
{
  "type": "tool.result",
  "runId": "a1b2c3",
  "callId": "c7",                                   // echo it back verbatim
  "url": "https://app.example.com/orders",          // the tab's URL at execution time
  "result": { … }                                   // see per-tool shapes below
}
```

or, on failure:

```jsonc
{ "type": "tool.result", "runId": "a1b2c3", "callId": "c7", "error": "stale_ref" }
```

`url` is re-checked by the host. If its origin no longer matches the run's
origin, the call is failed with `wrong_origin` **and the run is revoked** —
even though the extension already checked. Both sides check; neither trusts
the other to be the only one.

Error slugs the host understands: `stale_ref`, `not_found`, `not_visible`,
`not_actionable`, `detached`, `wrong_tab`, `wrong_origin`,
`screenshot_unavailable`, `action_failed`.

---

## Host → extension

### `run.started`

```jsonc
{ "type": "run.started", "runId": "a1b2c3",
  "runDir": "/Users/you/.neev/browser-runs/a1b2c3",
  "cli": "2.1.247 (Claude Code)" }
```

Show `runDir` in the panel as selectable text — it is where the run's files
are kept. `cli` is the resolved binary's `--version` line; render as plain text.

### `run.output`

```jsonc
{ "type": "run.output", "runId": "a1b2c3", "channel": "stdout", "text": "…" }
```

`channel` is `stdout`, `stderr`, or `notice` (host-generated, e.g. the
navigation reset). `text` is ≤4000 chars and is **already plain text** — the
host has pulled it out of the provider's JSONL and discarded the rest.

**Render with `textContent` only.** It contains page-derived and
model-generated content. No `innerHTML`, no Markdown rendering, no
auto-linking.

### `tool.call`

```jsonc
{ "type": "tool.call", "runId": "a1b2c3", "callId": "c7",
  "tool": "browser_click", "args": { "ref": "e4:17" }, "tabId": 42 }
```

Before acting, the extension **must independently re-verify**: the run is
still live, `tabId` is the attached tab, and `chrome.tabs.get(tabId).url` still
has the run's origin. Refuse with the matching error slug otherwise.

The host has already checked the tool is allowlisted and, for `browser_click`
and `browser_type`, that the ref's epoch is current.

Exactly one `tool.result` per `callId`. The host times out at **120 s**
(**900 s** for `browser_request_user_action`) and abandons the call; a late
result for an unknown `callId` is dropped silently.

### `run.ended`

```jsonc
{ "type": "run.ended", "runId": "a1b2c3", "state": "stopped", "reason": "user" }
```

`state` is `stopped` or `revoked`. `reason` is one of `user`, `agent_exited`,
`host_shutdown`, `cross_origin_navigation`, `origin_drift`, `start_failed`, or
the slug the extension sent.

`run.ended` **can arrive without a preceding `run.started`** — the run is
registered and stoppable from the moment `run.start` is accepted, while
resolving and launching the CLI takes a second or two. Handle Stop pressed
during startup.

On receipt the extension **must**: remove the pointer overlay and the badge,
detach the content script, drop any queued or in-flight calls, and put the
panel into its idle state. Nothing may act on the page after this.

### `error`

```jsonc
{ "type": "error", "runId": "a1b2c3", "code": "start_failed", "message": "…" }
```

`code`: `bad_message`, `duplicate_run`, `start_failed`. `message` is plain
text, ≤400 chars — e.g. the "no working codex CLI found, tried …" text when the
binary cannot be resolved. Show it verbatim as text.

---

## The six tools

The host will never send a `tool` outside this list.

| `tool` | `args` | Extension must return in `result` |
| --- | --- | --- |
| `browser_snapshot` | `{}` | `{ "snapshot": Snapshot }` |
| `browser_click` | `{ "ref": "e4:17" }` | `{ "ok": bool, "approval": Approval, "snapshot": Snapshot }` |
| `browser_type` | `{ "ref": "e4:9", "text": "…" }` | `{ "ok": bool, "approval": Approval, "snapshot": Snapshot }` |
| `browser_scroll` | `{ "direction": "up"\|"down", "amount": 1..10 }` or `{ "ref": "e4:3" }` | `{ "ok": bool, "snapshot": Snapshot }` |
| `browser_screenshot` | `{}` | `{ "format": "jpeg", "dataBase64": "…" }` |
| `browser_request_user_action` | `{ "reason": "…", "instruction": "…" }` | `{ "outcome": "done"\|"skipped", "snapshot": Snapshot }` |

`text` is capped at 2000 chars by the MCP schema before it reaches you.

### `Snapshot`

```jsonc
{
  "epoch": 4,                                  // see below — required
  "url": "https://app.example.com/orders",
  "title": "Orders",
  "nodes": [
    { "ref": "e4:17", "role": "button", "tag": "button", "name": "Save order",
      "enabled": true, "consequential": true },
    { "ref": "e4:18", "role": "textbox", "tag": "input", "name": "Customer",
      "placeholder": "Full name", "enabled": true, "filled": false },
    { "ref": "e4:19", "role": "password" },
    { "ref": "e4:20", "role": "heading", "name": "Recent orders" }
  ]
}
```

Allowed node keys — anything else is **dropped by the host before the model
sees it**, so do not bother sending it: `ref`, `role`, `tag`, `name`,
`placeholder`, `enabled`, `filled`, `checked`, `consequential`.

Allowed roles: `button`, `link`, `textbox`, `checkbox`, `radio`, `select`,
`heading`, `text`, `image`, `password`, `hidden`, `other`.

**Hard rules, enforced again on the host side:**

- **Never send a field's `value`.** For text fields send `filled:
  value.length > 0` and nothing more. There is no key that carries the
  contents of an input.
- `input[type=password]` and `input[type=hidden]` are sent as
  `{ "ref": …, "role": "password" | "hidden" }` with **no `name`**. The host
  reduces them further to `{ "ref": …, "role": "redacted" }`.
- No cookies, no `localStorage`, no `sessionStorage`, no raw HTML, no
  `outerHTML`, no `autocomplete`.
- The host caps the result at 200 nodes, 120 chars per string, 48 KB total, and
  sets `truncated: true` when it clips. Sending more is not an error; it is
  just discarded, so prefer sending the most useful 200.
- **If you clip first, set `truncated: true` yourself.** The host honours an
  incoming `true` and ORs it with its own clipping. Without it the model is
  told it read the whole page when it read 200 of 5000 elements — the host
  cannot infer this, because by then it only ever sees 200.

### `epoch` and refs

The **background service worker owns a single counter per run**, starting at
0, because it survives navigation and the content script does not.

1. Before producing any snapshot: `epoch += 1`.
2. Build every ref in that snapshot as `` `e${epoch}:${index}` ``.
3. Put that same `epoch` in `snapshot.epoch`.

This applies to **every** snapshot you return, including the one attached to a
denied approval or a failed action — you re-read the page, so it is a new
reading and needs a new epoch. Reusing an epoch is rejected with `bad_epoch`.

The host requires each snapshot's epoch to be **strictly greater** than the
last one it saw, and refuses a `browser_click` / `browser_type` whose ref
epoch is not exactly the current one — with `stale_snapshot`, telling the
model to snapshot again. A content script that restarts its own counter after
navigation would break this; keep the counter in the worker.

The content script should keep the same check locally and return `stale_ref`
for a ref it no longer holds.

### `Approval` — owned entirely by the extension

The host does not participate in approval. It never sees the prompt and cannot
grant one, so a compromised agent has no path to it.

`approval` is one of:

- `"approved"` — the user pressed Approve in the side panel; the action ran.
- `"denied"` — the user pressed Deny. Return `ok: false` and a fresh snapshot.
  This is a **normal result, not an error**, so the model can adapt.
- `"not_required"` — still accepted by the host, but the shipped extension
  confirms **every** click and type, so it is not currently emitted.

An element is **consequential** when it is `[type=submit]`, a bare `<button>`
inside a `<form>`, a `<form>` itself, or an `<a>` whose resolved origin differs
from the page's. Mark those `consequential: true` in the snapshot, so the model
knows what it is choosing, and word the prompt accordingly — but the flag does
not decide whether the prompt appears. It always appears.

When approval is pending, the panel shows the element's accessible name — as
`textContent`, it is page-controlled — plus the tool and the reason.

### The pointer

Move the overlay to the target element's centre **immediately before
dispatching**, and only then. No idle motion, no motion for a denied or refused
call, no motion on `browser_snapshot`. The badge ("Neev is controlling this
tab") is present exactly while the run is active.

---

## Two invariants worth restating

1. **After `run.ended`, nothing touches the page.** A straggling `tool.call`
   cannot arrive (the host rejects on its side first), but if one somehow does,
   refuse it.
2. **Page text is data.** It appears in snapshots, in approval prompts, and in
   agent output. It is never HTML, never a command, never a URL you act on.
