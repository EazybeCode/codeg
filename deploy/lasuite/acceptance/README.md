# Integration acceptance for the document engine

Runs against an isolated canary over loopback and asks whether the engine
actually behaves: documents get created on it, permissions hold, two people can
edit at once, and the things Neev kept doing still work.

**Status: run against the canary.** Last run 10 passed, 3 failed, 4 skipped —
the failures are real and tracked (a page chunk that 404s, and collaboration
sockets that never open). Skipped is not passed; see below.

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
answers on the base URL *and* that `/document-engine/api/v1.0/` is mounted — not
the bare prefix, which 404s on a perfectly healthy candidate and once made this
refuse to run against a live engine. If the
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
| editor assets | every asset the page references loads, with the right MIME — one missing chunk leaves the editor as a spinner forever |
| new document in a subfolder | the same, and it lands in the node asked for |
| retried creation | one document, not two, in Neev **and** on the engine |
| CSRF | an unsafe request with no token is refused by Neev *and* by Docs through the prefix |
| rename | a title changed in the engine reaches Neev's metadata — what the UI's focus-return refresh depends on |
| refused rename | a rejected write left **both** titles as they were, not just returned an error |
| editor boot content read | `GET .../content/` answers 200 — the read that gates whether the editor starts at all |
| durability | written content comes back byte for byte, decodes as Yjs, and revalidates |
| live revocation | a connected member's edits stop reaching the owner, and their socket closes |
| shared-editor rename | a shared member may rename, and both stores follow |
| archived | the body is frozen; metadata still changes, and both stores follow |
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

**Renaming is deliberately not creator-only**, and an earlier version of this
harness asserted the opposite. Neev's `resources::update` restricts visibility,
archiving and moving to the creator; the title is left to anyone who may edit the
document. That matches the Docs editor role, which includes `partial_update`, so
the two models agree rather than diverge — a shared editor renaming a document
they may already rewrite is consistency, not a bypass.

So the case asserts the rename is **accepted** and that both stores end up
holding it.

**Archiving freezes the body, not the metadata**, and the distinction is exact.
Neev's `resources::update` deliberately still accepts metadata on an archived
resource — it has to, or nothing could ever be unarchived. So a rename going
through on an archived document is the policy working, not a hole, and an
earlier version of this harness expected a refusal and was inventing a
restriction the product does not have. What must hold is narrower: the **body**
cannot be written (`content_patch: false`, from the server's own abilities), and
whatever the metadata becomes, both stores agree on it. Docs separately refuses
a title write on an archived document because the grant is read-only there; that
is consistent, and it is not the same claim as "all metadata is immutable",
which would be false.

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

## What the last run established

Against the canary, with real actors:

- documents are created on the engine in a project and in a subfolder, and a
  retried creation makes one document, known to both sides
- CSRF is enforced on **both** Neev and Docs, and the token is genuinely what is
  checked — with an Origin present and the token absent the answer is 401
- a rename in the engine reaches Neev's metadata, which is what the UI's
  focus-return refresh depends on
- a refused rename leaves both titles untouched, and two concurrent renames
  settle on one title that both stores agree on
- another organization is refused on Neev's API, the engine's API and the page
- the legacy document is untouched, still `native`, still readable
- Neev's own save and restore refuse an engine document

Open failures at that run: one page chunk 404s because its percent-encoded name
is not decoded before the file is looked up, and no collaboration socket can be
established at all.

The harness's own guards were checked too, since a guard nobody has seen fire is
a guard nobody knows the state of: the version gate refuses a decoy tree
declaring provider 3.0.0, both preflight branches fire, and each fixture
validation refuses on its own reason rather than on a missing file.

### Four harness bugs that had been reporting as product failures

Worth recording, because each was indistinguishable from a real defect:

- **no `Origin` header.** Node's fetch sends none and a browser always does, and
  Neev checks it. Identical request: without Origin 403, with Origin 201. Three
  cases failed and twelve skipped behind them.
- **Docs has its own CSRF**, a `csrftoken` cookie echoed in `X-CSRFToken`.
  Neev's `x-neev-csrf` means nothing to it, so every engine-side write was
  refused.
- **Neev's resource update takes the whole record**, not the field being
  changed. Sending one key is a 400 that reads exactly like a refusal to share.
- **socket failures all looked alike.** Every one reported "closed", so the
  cases asserting a socket is *refused* were passing because collaboration was
  broken. They now skip unless a known-good socket opens first, and close codes
  are reported: 1005/1006 means the path, `4001 Origin not allowed` means the
  header, `4001 No cookies` means the cookie, `permission denied` means a real
  denial.
