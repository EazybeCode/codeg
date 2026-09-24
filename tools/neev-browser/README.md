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

## Limitations

Read these; they are real.

- **Synthetic events.** Actions are dispatched from a content script, so
  `event.isTrusted` is false. Sites that specifically gate on trusted input
  will not respond. `chrome.debugger` would fix this and is deliberately not
  used in this version.
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

```bash
tools/neev-browser/companion/run_tests.sh
```

Offline, standard library only, and no provider is ever invoked —
`tests/fake_agent.py` stands in for the CLI and speaks real MCP through the
real launcher, socket and host. The suite covers frame decoding, protocol
validation, the tab/origin scope guard, stale-reference refusal across
navigation, Stop semantics including stragglers and grandchild processes,
socket authentication, snapshot bounding and redaction, argument validation,
and the argv both provider adapters build.
