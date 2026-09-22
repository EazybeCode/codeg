# Embedding the document engine in Neev's UI

The Neev-side half of the document engine: which editor a document opens in, and
what Neev keeps owning once the upstream one is on screen.

Paired with [`README.md`](README.md), which covers the artifact that gets framed.

## Which editor opens

The server decides, per resource, and the client obeys:

```jsonc
{ "id": "…", "kind": "document", "engine": "lasuite" | "native", … }
```

- `lasuite` → `EngineDocument`, a same-origin frame at
  `/document-engine/docs/<id>/`. The resource id **is** the upstream document
  id; there is no second id and no mapping table.
- `native` → the existing editor, unchanged.
- **absent** → `native`. An older server that has never heard of the field keeps
  working, which is the only reason the field is optional rather than required.

### The parser change this needed, which is easy to get wrong

`parseResource` validates with `exact()`, and `exact()` rejected **unknown**
keys, not just missing ones. Adding `engine` to the resource response would
therefore have made every resource fail with `invalid_response` on any client
that had not shipped the matching change — **chats included**, since they share
the parser. A field addition would have looked like a total outage.

`exact()` now takes an explicit optional-key list, and `engine` is on it.
Strictness is otherwise untouched: an unexpected key is still a refusal, and so
is a missing required one. Both are still tested.

An `engine` value this client does not recognise is **refused**, not coerced to
`native`. Coercing would open Neev's editor against content it cannot read — a
blank document where one exists, which reads as data loss and invites saving
over it. Failing loudly is the safe direction and matches how `kind` and
`visibility` have always been handled.

## What Neev keeps, and what it gives up

Given up to upstream, because upstream does it inside the frame:

- the document's content, and everything about saving it
- the **title**, including editing it

Kept, because Neev owns them and the embedded build hides upstream's versions:

- share with organization / make private
- archive and reopen, move

That second list is load-bearing rather than decorative. The embed build hides
Docs' own sharing and public-link UI, so if Neev did not offer sharing here, a
newly created private document could not be shared with anyone — it would be
permanently reachable only by its creator.

**No Neev-side rename, and no Neev-side title.** Upstream draws the title inside
the frame and edits it in place. A second editable title above the frame, backed
by a different store, is two names free to disagree. This is the one place the
UI contract in `/tmp/neev-docs-engine-contract.md` §F4 and root's instruction
disagreed — §F4 has the title edited through Neev's rename action — and root's
reading is the one implemented.

Creator-only actions are gated on `isCreator`, exactly as the native document
gates them. The server refuses archive and move for anyone else, so showing
them to an organization member would be offering controls that can only fail.

### Keeping the tab and sidebar honest about the title

A rename happens inside the frame, where Neev cannot see it. When focus comes
back **out** of the frame, Neev re-reads the resource from its own API and pushes
it through the existing `onChanged` → `onResourceChanged` path that the tabs,
sidebar and breadcrumb already listen to. No reload.

Bounded on purpose: at most one request per visit to the editor, not a poll, and
only when focus was actually inside the frame — a bare window focus is someone
returning to the browser tab, not someone leaving the editor.

### Focus alone was not enough

Rename, then immediately click something in Neev, and the focus-driven read is
issued *before* the save lands. It answers with the old title, publishes it, and
the tab keeps the stale name until a reload. That is a real bug and it was found
in a browser, not in a test.

So the editor also says so directly: once the **server has confirmed** a title
change, it posts `neev-doc-metadata-updated` with the resource id to its parent
(upstream patch `0007`).

**The message carries no title, and none is read from it.** A frame that could
name itself could name itself anything, and Neev would be rendering a string
chosen by a document's contents. The signal says "go and ask"; the answer still
comes from Neev's own API. Three things are checked before it counts:

- `event.source` is this frame's own window — any frame on the page can post
- `event.origin` is our own origin — the embed is same-origin by design
- the `resourceId` is the document this component is showing — so a stale
  message from a previously-open document cannot land here

The earlier rule here said "not driven by `postMessage`". That was too broad: the
thing never to trust is a frame-supplied **title**, not every message. An
origin-verified, invalidate-only signal is safe, and is what closes the race.

**A confirmation arriving mid-read is queued, not dropped.** If a read is already
in flight when the message lands, that read was issued before the change and will
answer with the old title — so dropping the signal would reinstate exactly the
race it exists to close. One follow-up is queued and runs when the in-flight read
settles. It is triggered by a confirmed event rather than a timer, so it cannot
become a poll. There is a test for precisely that interleaving.

## Audit of the document creation paths

Root asked for this before anything changed, because switching engines must not
leave a create path that produces a document no editor will open.

There are **two** paths that create a document, and both are safe:

| path | title | opens |
|---|---|---|
| `native-workspace.tsx:1182` ("New document") | no prompt; defaults to `Untitled document` | the returned resource, `fresh: true` |
| `native-content.tsx:441` (the dialog) | existing prompt | the returned resource |

Both call the same `createNativeResource` and then select **the row the server
returned**, so whatever `engine` the server stamps flows straight through and the
right editor opens. Neither writes content, so neither can hit a native save
route that would refuse a La Suite document — content saving lives entirely
inside `NativeDocument`, which never mounts for an engine document.

No new title prompt was added. The prompt-free path already exists and is the
one the "New document" button uses.

**Save-from-chat does not exist today.** `CreateNativeResourceInput` carries an
optional `originChatId` and the client sends it, but no non-test caller passes
it — the only live uses of `originChatId` read it back to decide whether a
document is shareable. So the route root was worried about is not currently
reachable. If one is added later, it has the same shape as the two above and
inherits the same safety, but it would need its own test.

One behavioural difference worth naming: `fresh` makes a new **native** document
take its name from the first line typed. That does not happen for an engine
document — `fresh` is not part of the engine component's contract, because
naming is upstream's. A new engine document stays `Untitled document` until
someone titles it in the editor.

## Tests

`src/components/neev/engine-document.test.tsx`, 27 tests. The ones that carry
weight:

- **no content, revision or autosave request at all** — asserted against *every*
  request the component causes, not a list of paths written in advance, so a
  fetch added later fails this rather than slipping past a stale list. This is
  the parallel-writer guarantee, checked rather than promised.
- engine `lasuite` opens the frame; `native` and **absent** both open the native
  editor, driven through the real content view
- the parser accepts `engine` on documents *and* on chats, still accepts a
  response without it, refuses an engine it cannot render, and still refuses both
  an unexpected field and a missing required one
- the title is re-read on focus returning out of the frame, and not re-read when
  focus never entered it
- a confirmation from the frame re-reads without waiting for focus, and one that
  arrives **while a read is already in flight** still produces a follow-up that
  publishes the new title — the interleaving that caused the original bug
- a confirmation is ignored from a different origin, from a window that is not
  the frame, and for a different document's id; a `message` claiming a title
  still changes nothing
- sharing, archive and move survive; rename does not; an organization member sees
  neither the menu nor the share button
- a frame that loads the proxy's error page offers a retry instead of a blank
  panel, and an ordinary page does not — the component reads one marker and
  never the body, so a document that merely mentions an error is still a document

## Known gaps — not covered, and not to be read as covered

- **An HTTP error inside the frame is not detected.** `onError` does not fire for
  an HTTP error: a 403 or a 503 is a response, and the frame loads it like any
  other page, so `onLoad` means "a document arrived", not "the editor is there".
  The component looks for one explicit marker —
  `<meta name="neev-document-engine-error">` — and nothing else, because sniffing
  the body would make an ordinary document that happened to say the wrong thing
  indistinguishable from a failure. **Until the proxy emits that marker, an HTTP
  error page renders inside the frame as itself and no retry appears.** The
  load-timeout path only catches a frame that never finishes loading.
- **Nothing here has run in a browser.** The frame has never displayed the real
  editor. In particular the focus handling — `window` blur with
  `document.activeElement` being the iframe — is proven in jsdom, where the event
  order is simulated. Real browsers differ on this, and it needs confirming
  against a running engine.
- **The retry has never been tested against a genuinely unavailable upstream.**
- The engine field has never been served by a real backend; every test supplies
  it from a mock.
