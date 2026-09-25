# Neev Browser

Pick one open Chrome tab. Pick Claude Code or Codex — whichever you already
have installed and signed into. Describe what you want done. Watch a pointer
move across the page and do it. Press **Stop** whenever you like.

No API keys: the companion drives the CLI on your machine, using your own
subscription sign-in. Nothing is sent to a Neev server.

This is a first version. Read [Limitations](#limitations) before relying on it.

## What it is made of

```
extension/     an unpacked MV3 extension: side panel, scope guard, page actions
companion/     a Python native messaging host and the MCP server it owns
install/       registers the native host for one extension ID; nothing else
```

```
Chrome side panel
      │  chrome.runtime.connectNative("com.neev.browser")
      ▼
neev-browser-host        run state, scope guard, Stop
      │  unix socket in the run directory, 0600, per-run token
      ▼
neev_browser.mcp_server  the six browser tools
      ▲  MCP over stdio
      │
claude -p  /  codex exec   started in its own process group
```

## Setup

You need Chrome (or Chromium/Brave), Python 3.9+, and at least one of the
`claude` or `codex` CLIs installed and signed in.

**1. Load the extension.** Open `chrome://extensions`, turn on Developer mode,
choose **Load unpacked**, and select `tools/neev-browser/extension`.

**2. Copy the extension ID** Chrome shows on that card — 32 letters.

**3. Register the native host:**

```bash
tools/neev-browser/install/install.sh <extension-id>
```

That writes one file per browser profile: a native messaging host manifest
whose `allowed_origins` is pinned to your extension ID alone. It touches
nothing else — in particular it never edits your Claude or Codex config.

**4. Fully quit and reopen Chrome.**

The first time you open the panel it asks for permission to list your tabs —
that is an optional permission requested on a click, not something the install
grants, so Chrome shows no tab-reading warning up front. Without it Chrome
blanks the titles and URLs in the picker and you cannot tell the tabs apart.
Access to the page itself is a separate, per-origin grant asked for when you
press Run.

To undo: `tools/neev-browser/install/uninstall.sh`. Your runs are kept.

If the CLI is not where the companion expects, point it at the right binary:

```bash
export NEEV_BROWSER_CLAUDE_BIN=~/.local/bin/claude
export NEEV_BROWSER_CODEX_BIN=/Applications/ChatGPT.app/Contents/Resources/codex
```

Otherwise it tries, in order: that variable, `~/.local/bin`, your `PATH`, then
known app-bundle locations — and picks the first that answers `--version`. If
none does, the run fails immediately and tells you what it tried.

## What the agent can do

Six tools, and nothing else. There is no tool that runs JavaScript, navigates,
opens tabs, or reads cookies or storage.

| Tool | What it does |
| --- | --- |
| `browser_snapshot` | Reads the page: URL, title, and up to 200 elements with references |
| `browser_click` | Clicks one element |
| `browser_type` | Types into one field — never presses Enter, never submits |
| `browser_scroll` | Scrolls, or brings an element into view |
| `browser_screenshot` | JPEG of the visible area of your tab |
| `browser_request_user_action` | Asks *you* to do something it cannot |

Every action returns a fresh reading of the page, so the agent always sees the
result of what it just did.

### What it cannot see

- **No field contents, ever.** A text box is reported as filled or empty, with
  its label and placeholder — never its value.
- Password and hidden inputs are reduced to a redaction marker before the
  model sees them: no name, no value.
- No cookies, no `localStorage`, no `sessionStorage`, no raw HTML.

### What needs your say-so

**Every click and every keystroke.** This first version confirms all of them:
the run pauses, the side panel names the element, and nothing touches the page
until you press Approve. Declining is a normal answer the agent can work with,
not an error.

Actions that submit a form or leave the site are labelled as such in the
prompt, and the agent is told which elements are consequential before it picks
one — but the confirmation itself is unconditional.

That means **a run is never unattended**. It is the deliberate choice for a
first version: the approval step is the thing standing between a page that can
say anything and an agent acting on your behalf. If it proves too slow in
practice, narrowing it to consequential actions only is a small change — but it
is a change to make on evidence, not up front.

Approval lives entirely in the extension. The companion never sees the prompt
and cannot grant one, so nothing on the agent side has a path to it.

Navigation, sign-ins, payments and browser UI are outside the tool set
entirely. The agent has to ask you, through
`browser_request_user_action`, and wait.

### Scope

A run is bound to the one tab and the one origin you chose. The extension ships
with no host permissions at all; it asks for the single origin at attach time.
Access ends — pending calls rejected, overlay removed, agent killed — when the
tab navigates to another origin, the tab closes, the connection drops, or you
press Stop. It never resumes; you start a new run.

Within one origin, a page navigation invalidates every element reference, so a
reference from the old document can never be applied to the new one.

## Your run outputs are kept

Each run gets a private directory, outside `/tmp`, that is never cleaned up:

```
~/.neev/browser-runs/<run-id>/
    run.json           provider, target origin, CLI version, how it ended
    transcript.jsonl   one line per agent message and tool call
    agent.stdout.log   raw CLI output
    agent.stderr.log   raw CLI errors
    mcp-config.json    the scoped config that run used
    files/             the CLI's working directory — anything it writes is here
```

The run's IPC token lives in a `0600` file that is deleted when the run ends,
so no credential survives in the preserved output.

The side panel shows the directory path, and **Export transcript** saves a
Markdown copy.

## When the panel misbehaves

The panel is a module script, so one throw while it evaluates registers none of
the listeners below it: the panel paints correctly and then ignores every
click. Chrome reports that only in the panel's own devtools. To make it visible
without them, `sidepanel.html` loads exactly one script — `boot.js` — which
installs an error reporter and then imports `sidepanel.js` itself. Success and
failure are the two settlements of that import, so there is no timing guess
involved and a slow load is never mistaken for a dead one.

- **"The panel script did not start: …"** — the import rejected. The message
  after the colon is the real error. The panel is dead; reload the extension.
- **"Panel error: …"** — an uncaught error or rejection after load. The panel
  may be partly working.
- **A panel that is genuinely blank** — no header, no banner, no buttons — is
  none of the above, because all of those still leave the static HTML painted.
  Reading the source, everything but `#boot`, `#error` and the three
  conditional cards is unconditionally visible and no CSS rule hides a
  container at load, so a fully blank panel points at the document never
  loading rather than at the panel script. Observed once, on 2026-09-25, in
  Chrome Profile 5: the whole panel body painted in the browser's theme colour
  rather than either of `sidepanel.css`'s palettes, which is Chrome's own side
  panel background showing through because no extension document was rendered.
  Reloading the extension cleared it and it has not recurred. **No cause was
  established** — nothing in this tool's source was shown to produce it, and
  nothing here should be read as a fix for it.

### "Connection lost" is not necessarily a fault

The panel used to say "the background service worker disconnected — reload the
extension" and stop dead: the port was dropped and nothing ever reopened it, so
every button stayed inert until a manual reload. It now reconnects instead.

**Why a dropped port is expected.** Chrome's [service worker lifecycle
docs][lifecycle] put the idle shutdown at 30 seconds, and say that opening a
port does not reset that timer although messages over one do extend it. They
also note that `runtime.connectNative` keeps the worker alive while a native
messaging port is open (Chrome 105+), which is why an attached run is the case
least likely to be cut short. So for an idle panel, losing the port is routine.

**But the panel cannot prove that is what happened.** A closed port says only
that the port closed. A crashed worker and a reloaded extension look identical
from here, and the absence of a `chrome.runtime.lastError` narrows it without
settling it. Nothing in `lib/connection.js` or in what the panel shows you
names a cause, and nothing claims the page went untouched — an action
dispatched a moment before the drop may well have landed.

Four rules in `lib/connection.js`:

- **Nothing is replayed.** A message that was not delivered stays undelivered.
  Only **Grant access & run** may open a fresh connection, because it starts
  something new. Stop, Approve, Deny and the handoff buttons answer for state
  that lived in the connection that went away, so they are never re-sent — the
  panel says so instead.
- **Recovery is bounded, and never periodic.** An idle panel does not reconnect
  at all; it waits for the next thing you do. Reconnecting on a timer would buy
  another 30 seconds and then do it again, forever — an extension keeping its
  worker alive indefinitely, which [the same document][lifecycle] tells you not
  to build. A panel that had a live run reconnects with backoff — 0.25 s,
  0.75 s, 2 s, 5 s — then gives up and says so.
- **A reconnect counts only once the worker speaks.** `chrome.runtime.connect`
  hands back a port even when the worker will not start, so the backoff resets
  on the snapshot the worker posts on connect, not on the connect itself.
- **Only the live port is listened to.** Chrome leaves a superseded port's
  listeners attached, so both ends gate messages *and* disconnects on port
  identity. Otherwise a late message from a dead connection could overwrite
  newer state, or drive a run nobody is watching.

**What it says when a run was attached.** The panel does not read a fate into a
closed port. It reconnects, and if the worker's own snapshot comes back holding
no run, *that* is the evidence — the panel says "Connection lost. This panel is
no longer attached to the run. No commands were replayed. Start a new run to
continue," keeps the activity log so the transcript is still exportable, and
records the run as `ended / connection_lost` rather than guessing at a reason.

[lifecycle]: https://developer.chrome.com/docs/extensions/develop/concepts/service-workers/lifecycle

## Limitations

Read these; they are real.

- **Synthetic events.** Actions are dispatched from a content script, so
  `event.isTrusted` is false. Sites that specifically gate on trusted input
  will not respond. `chrome.debugger` would fix this; it is **not declared in
  the manifest at all**, so the install carries no debugger warning and no code
  path can reach for it.
- **No native browser UI.** File pickers, the address bar, extension dialogs
  and OS prompts are out of reach. The agent has to ask you.
- **Chrome only.** MV3, Chrome/Chromium/Brave. No Firefox or Safari.
- **Nothing is saved to Neev.** The side panel links to Neev and exports a
  transcript file. It does not create a chat, upload a run, or produce a child
  artifact.
- **One tab, one run.** No multi-tab orchestration.
- **Other agents.** The provider layer is written so a third CLI can be added.
  Only Claude and Codex are implemented; nothing else is claimed to work.

## Tests

Two suites, both offline, neither needing an installed extension, a browser, a
network, or a provider. Nothing to install for either.

```bash
tools/neev-browser/companion/run_tests.sh      # 156 tests, Python stdlib
tools/neev-browser/extension-tests/run_tests.sh # 111 tests, node --test
```

The extension suite syntax-checks every shipped `.js` file, then runs the
protocol builders, the scope guard, the run controller and the page agent
against a hand-built DOM — `background.js` is the only file that touches
`chrome`, so everything else is reachable from Node. Needs Node 18 or newer;
verified on v18.15.0.

The companion suite is standard library only, and no provider is ever invoked —
`tests/fake_agent.py` stands in for the CLI and speaks real MCP through the
real launcher, socket and host. The suite covers frame decoding, protocol
validation, the tab/origin scope guard, stale-reference refusal across
navigation, Stop semantics including stragglers and grandchild processes,
socket authentication, snapshot bounding and redaction, argument validation,
and the argv both provider adapters build.
