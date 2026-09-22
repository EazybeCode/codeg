# Integration acceptance for the document engine

Runs against an isolated canary over loopback and asks whether the engine
actually behaves: documents get created on it, permissions hold, two people can
edit at once, and the things Neev kept doing still work.

**Status: prepared, not yet run.** The canary is still the baseline, so no case
here has produced a result. Nothing below should be read as a passing test.

```bash
./run.sh                 # against http://127.0.0.1:3100
./run.sh --json          # machine-readable, still redacted
./run.sh --only=CSRF     # one case by substring
```

From a workstation, point it at a tunnel — the actor files stay on the host:

```bash
ssh -L 3100:127.0.0.1:3100 <host>
NEEV_ACCEPTANCE_BASE=http://127.0.0.1:3100 ./run.sh
```

## What it refuses to do

**It will not run against a baseline.** The preflight checks that something
answers on the base URL *and* that `/document-engine/` is served. If the
candidate is not installed it exits **2** — not 1 — so "could not run" is
distinguishable from "ran and failed" by anything scripting it. A wall of red
that means "nobody deployed yet" is worse than no output: it teaches people to
ignore the harness.

**It will not print a secret.** Every value that reaches stdout goes through
`redact` first, including on failure, where the temptation to dump the whole
request is strongest. `actors.json` and `fixtures.json` are read from the canary
host, are checked for group/other permissions before being read at all, and are
never copied.

**It will not install anything.** The collaboration client is the one the app
ships, borrowed from the pinned upstream build tree
(`/tmp/neev-lasuite-build/work/node_modules`, or `NEEV_ACCEPTANCE_MODULES`). The
run aborts unless `@hocuspocus/provider` is exactly **3.4.4** and `yjs` exactly
**13.6.32** — a convergence result from a different protocol version is a
statement about a client nobody runs.

**It will not write to the legacy fixture.** There is no migration, and that
document is the evidence legacy documents were left alone. The case that reads
it is read-only throughout; writing to it would destroy the only baseline
proving the claim.

**It will not report a pass for something it did not exercise.** A case that
cannot reach its state returns `skipped` with a reason, and the summary says in
words that skipped cases proved nothing.

## The cases

| | what it establishes |
|---|---|
| new document in a project | created on the engine, editor page reachable, and it carries `frame-ancestors` — without which the iframe has no policy at all |
| new document in a subfolder | the same, and it lands in the node asked for |
| retried creation | one document, not two, in Neev **and** on the engine |
| CSRF | an unsafe request with no token is refused by Neev *and* by Docs through the prefix |
| rename | a title changed in the engine reaches Neev's metadata — what the UI's focus-return refresh depends on |
| refused rename | a rejected write left **both** titles as they were, not just returned an error |
| non-creator rename | a shared member changes neither title |
| concurrent renames | two at once settle on one title, identical on both sides |
| other organization | refused on Neev's API, the engine's API, and the socket |
| second user | shares to the organization, both connect, **both write without waiting**, and converge |
| revocation | after access is withdrawn, the next connection is refused |
| foreign document name | a socket authorised for one document cannot carry frames naming another |
| archived | read-only, not writable |
| legacy | still native, still readable, never written to |
| native save routes | Neev's own save and restore refuse an engine document |
| engine unavailable | the error page carries the marker the UI needs to offer a retry |

### Two that deserve explanation

**The foreign-document-name case.** The document name travels as a leading
varstring in *every* message, not once per socket, so one socket can carry frames
for a document it was never authorised for. Upstream already defends this —
`hocuspocusServer.onConnect` rejects `documentName !== roomParam` with "Wrong
room name: Unauthorized", and that hook runs per document rather than per socket.
So the case asserts a defence **expected to hold**, and exists to catch a proxy
that terminates the socket in a way that loses it. It is written to fail loudly
rather than to confirm a suspicion.

This is also the one place a frame is not produced by ordinary provider use, and
it has to be, because the provider always names the document it was constructed
with. Even there the frame is built by the official library, by attaching a
second provider to the same socket — the library's own multiplexing path. **No
frame in this harness is hand-assembled.**

**The title cases read both stores, every time.** A title is the one piece of
state Neev and Docs both display, so "they disagree" is invisible unless
something looks at both. A refused write that nevertheless landed on one side
would pass a status-code check and still leave the two out of step.

The non-creator case may legitimately fail, and the message says so rather than
asserting a verdict. Renaming is creator-only in Neev, but sharing to the
organization gives a member the `editor` role in Docs, and upstream's editor role
includes `partial_update` — which covers the title. If that case fails it is a
policy decision about what a shared editor may do, not a defect to patch blindly.

The concurrent case does **not** require both renames to succeed. A conflict
response is a perfectly good answer. What it requires is that the two stores
agree afterwards, on a title somebody actually asked for.

**The revocation case is about new connections only.** Whether an already-open
socket is dropped at the moment access is revoked is a different question and
this does not answer it; the result says so in its own evidence. The earlier
preview evaluation left the same question open and it has not been closed since.

## What it cannot tell you

- **Nothing here is a browser.** No page is rendered, no editor is loaded,
  nothing is typed. The convergence cases drive real Yjs documents over real
  sockets, which proves relay, merge and authorisation — and proves nothing
  about the editor's own save path, because a headless client never runs it.
  Those are separate claims and neither substitutes for the other.
- **Durability is not covered.** Convergence between two clients says nothing
  about what is stored. The durable copy is written by the editor, in a browser.
- **Attachment upload is not covered**, as it was not in the preview.
- The unavailability case needs the engine deliberately stopped
  (`NEEV_ACCEPTANCE_ENGINE_STOPPED=1`); otherwise it skips rather than inventing
  a result.

## Verified so far

Without a canary, only the harness itself:

- all five modules parse; `run.sh` parses
- the official provider, its websocket constructor, Yjs and the `ws` polyfill all
  load from the pinned tree, and a `Y.Doc` round-trips
- the version gate **fires** — pointed at a decoy tree declaring provider 3.0.0
  it refuses to run, so the assertion is not vacuous
- both preflight branches fire: nothing listening → exit 2 with the reason; and a
  stand-in server answering Neev but 404ing `/document-engine/` → "this looks
  like the baseline, not the candidate", exit 2
