# Neev Browser — first version

A standalone tool under `tools/neev-browser/`. It lets a locally installed
Claude Code or Codex CLI read and act on one Chrome tab the user picks, with a
visible pointer tied to real dispatched actions and a Stop that actually stops.
Nothing in the deployed app, its core, or the Rust binaries was touched.

Spec: `docs/superpowers/specs/2026-09-24-neev-browser.md`.
Plan: `docs/superpowers/plans/2026-09-24-neev-browser.md`.
Wire contract between the two halves: `tools/neev-browser/PROTOCOL.md`.

## What shipped

An MV3 side panel talks to a Python native messaging host over Chrome's
length-prefixed stdio protocol. The host owns run state, starts the agent CLI
in its own process group, and serves a per-run unix socket. A separate MCP
stdio server — spawned by the CLI, not by Chrome — exposes six allowlisted
browser tools across that socket.

All original code. Python 3.9 standard library only; no third-party
dependency in either half.

## Security boundaries, and what enforces them

- **Scope.** A run binds to one `tabId` and one origin. The extension declares
  no host permissions and requests the single origin at attach time. Access is
  revoked — pending calls rejected, process group killed, overlay removed — on
  cross-origin navigation, tab loss, port disconnect, or Stop, and never
  resumes.
- **Stale references.** Element refs carry the snapshot epoch that produced
  them. Same-origin navigation invalidates every outstanding ref, so one from
  the previous document cannot land on whatever now occupies that position.
  Epochs must strictly advance; a replayed epoch is refused.
- **Redaction.** No form field value ever reaches the model — text fields
  report only filled or empty. Password and hidden inputs are reduced to a
  marker with no name. No cookies, storage, or raw HTML. Snapshots are capped
  at 200 nodes, 120 chars per string, 48 KB, with truncation reported.
- **Tool surface.** Six tools, allowlisted by name. No JavaScript evaluation,
  no navigation, no tab creation, no storage access. Arguments are validated in
  the MCP server before the socket is touched — the schema is treated as
  advisory, since nothing guarantees the CLI enforced it. A browser reply that
  does not carry what the tool promised is reported as a failure rather than
  reading as success.
- **Every click and keystroke** needs a human Approve in the side panel — not
  only the consequential ones. The shipped extension confirms all of them; the
  snapshot still marks submit controls, forms and off-site links as
  consequential so the model knows what it is choosing, and the prompt says so,
  but the confirmation is unconditional. A run is therefore never unattended,
  which is the deliberate first-version choice. Approval lives entirely in the
  extension; the host never sees it and cannot grant it.
- **IPC.** AF_UNIX inside the run directory, mode 0600, per-run token in the
  first frame. No TCP listener. The token lives in a 0600 file that is deleted
  when the run ends, and is never placed on a command line, so it appears
  neither in `ps` nor in the preserved run output.
- **Run directories** are created fresh at 0700 — a collision is an error, not
  a reuse — with symlinked or loose-permission bases refused, and every run
  file opened `O_NOFOLLOW`.
- **Provider scoping.** Claude runs with `--strict-mcp-config --tools ""
  --allowedTools <the six> --permission-mode default --no-session-persistence`.
  Codex runs with `--ignore-user-config --ephemeral --sandbox read-only`.
  No global config is written or edited, and no permission- or sandbox-bypass
  flag is used — a test asserts the banned flags never appear in either argv.

## Verification

`tools/neev-browser/companion/run_tests.sh` — **151 tests pass** on Python
3.9.6, offline, with no provider invoked. Coverage: frame codec including
truncation and oversize; protocol validation; origin normalisation rejecting
non-http schemes, userinfo, and inconsistent IPv6 forms; the scope guard and
permanent revocation; stale-reference refusal across same-origin navigation;
Stop — process-group kill, pending-call rejection, straggler refusal, and a
Stop landing during a slow CLI launch; socket token authentication; snapshot
bounding and redaction; MCP argument validation and malformed-reply handling;
and both adapters' argv.

Eight of those are genuine end-to-end tests with a **real child process**
(`tests/fake_agent.py`) standing in for the CLI: it discovers the per-run
launcher exactly as each real CLI would, spawns it, speaks JSON-RPC over
stdio, and its calls travel the launcher → MCP server → unix socket → host →
scope guard path. They confirm the agent sees exactly six tools, that a stale
ref and an unlisted tool are both refused before reaching the page, that a
denied approval comes back as a usable result, that an origin drift revokes the
run, that stdout really streams to the panel and to the run log, and that Stop
kills the agent's MCP grandchild.

CLI interfaces were validated live with `--version` / `--help` only —
`claude` 2.1.247 and `codex-cli` 0.154.0-alpha.6.2. No credential file was
read and no paid run was made.

**Not verified, and not claimed:** no run against a real Chrome, a real page,
or a real provider has been performed. The extension has not been loaded. The
native host has not been registered. Installation is the user's explicit final
step and no part of it was carried out here.

## Environment notes

The PATH entry is not assumed to be the working binary: on this machine
`/usr/local/bin/codex` is a launcher whose platform package is missing and
which is SIGKILLed on start, while a working `codex` ships inside ChatGPT.app,
and the current `claude` sits at `~/.local/bin` ahead of an older
`/usr/local/bin` copy. The companion tries `NEEV_BROWSER_{CLAUDE,CODEX}_BIN`,
`~/.local/bin`, PATH, then known app-bundle locations, and picks the first that
answers `--version`. A broken install is stepped over, never repaired; if none
works the run fails immediately, naming what it tried.

## Known limitations

Documented in `tools/neev-browser/README.md` and worth repeating here:

- Actions are dispatched from a content script, so `event.isTrusted` is false.
  Sites that gate on trusted input will not respond. `chrome.debugger` would
  fix this and is deliberately not used in this version.
- Native browser UI — file pickers, the address bar, extension dialogs — is out
  of reach. The agent must ask the user via `browser_request_user_action`.
- Chrome/Chromium/Brave only.
- **Nothing is saved to a Neev server.** The side panel links out to Neev and
  exports a Markdown transcript. It does not create a chat, upload the run, or
  produce a child artifact. The panel says so in plain text.
- One tab, one run. The provider layer is shaped so a third CLI could be
  added; only Claude and Codex are implemented and no broader support is
  claimed.
