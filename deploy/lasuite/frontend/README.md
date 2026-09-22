# La Suite Docs as Neev's document engine — the frontend

La Suite Docs v5.7.0, built as a static export that Neev serves at
`/document-engine/` and frames same-origin. Upstream's editor, save path and
real-time collaboration are untouched; what changes is where it is mounted and
how much of its own application it draws.

This directory is the whole frontend side: the pinned source, a seven-patch
series against it, the build script, the environment, and the serving
requirements. It owns no Neev code. Resource creation, identity, ACL and the
Neev-side frame wrapper belong to the backend work, not here.

The preview that established this is a sound basis — authentication, the
permission boundary, two accounts editing concurrently, reconnection,
server-side persistence — is written up in
[`docs/lasuite-docs-preview/README.md`](../../../docs/lasuite-docs-preview/README.md).
Read it before changing anything here; several of the constraints below are
consequences of what it found.

## The routes contract

| concern | URL |
|---|---|
| document page | `/document-engine/docs/<uuid>/` |
| REST API | `/document-engine/api/v1.0/` |
| collaboration socket | `ws(s)://<host>/document-engine/collaboration/ws/?room=<uuid>` |
| media | `/document-engine/media/` |

Every one of them is same-origin and composed at runtime from
`window.location.origin`. **No origin is baked into the bundle.** That is the
property that lets one artifact serve production HTTPS and a loopback test
without a rebuild, and it is worth protecting: the moment a host is compiled in,
the artifact stops being portable and the test stops resembling production.

Verified in the built output, not just intended — the bundle contains

```js
i = () => `${window.location.origin}/document-engine`,
a = (e = '1.0') => `${i()}/api/v${e}/`
```

and, for the socket, `"wss":"ws"}://${window.location.host}/document-engine/collaboration/ws`.

### Root-relative asset paths, swept

`basePath` rewrites `next/link`, `next/image` and `/_next/` URLs. It does **not**
rewrite a bare string literal used as a `src`. Any hardcoded `/assets/…` in the
source therefore survives into the bundle unprefixed and would 404 under the
prefix, which is the kind of thing that only shows up in a browser.

The built bundle was swept for exactly that. There are **eleven** such literals
and no more, and none of them is reachable from the embedded editor route:

| literal(s) | rendered by | reachable in the embed? |
|---|---|---|
| `/assets/on-boarding/step_{1..4}_{EN,FR}.{webm,webp}` | `FirstConnection`, mounted from `Auth.tsx` on `is_first_connection` | no — the bridge sets that false for Neev accounts (`05b6e1db`) |
| `/assets/SC1-{en,fr}.webm` | `HomeContent` | no — the home page is never framed |
| `/assets/Docs-lottie.json` | `HeaderIcon` ← `HeaderFloatingBar` ← `PageLayout` / `pages/docs/index` | no — the editor route uses `MainLayout`, never `PageLayout` |

Verified by tracing each one to its renderer, not by assuming the embed is
minimal. `pages/docs/[id]/index.tsx` contains zero references to `PageLayout`.

One honest residue: the error pages (`401`, `404`, `500`) *do* use `PageLayout`,
so if the editor redirects to one inside the frame, the Lottie animation 404s.
That is cosmetic — a missing animation on an error page — and it is not worth a
rebuild, but it is real and it is why this table says "the editor route" rather
than "the artifact".

### What the backend must not do — agreed, and now enforced there

The prefix is added on this side, once. Two ways to break that:

- **`attachment-upload` must keep returning a root-relative `file`**, i.e.
  `/media/<...>`. Media URLs are composed client-side as
  `${backendUrl()}${attachment.file}` (upstream `useUploadFile.tsx:30`, which no
  patch here touches). If the backend also prefixes it the result is
  `/document-engine/document-engine/media/...`.
- **`COLLABORATION_WS_URL` in `/api/v1.0/config/` is best left unset.** The
  patched fallback already builds the prefixed URL and derives `ws`/`wss` from
  the page. If it is set it wins, and it must then be a full absolute URL — which
  re-pins the artifact to one host, giving up the portability above.

`NEXT_PUBLIC_API_ORIGIN` keeps its upstream meaning: an origin, scheme and host
only, never a path. The embedded build leaves it empty. The prefix lives in
`NEXT_PUBLIC_BACKEND_BASE_PATH`.

Both are settled rather than assumed. The backend work (`deploy/lasuite/backend`,
commit `e6b06bbb`) confirmed the concatenation semantics and went further than
agreement: it leaves `MEDIA_BASE_URL` and `COLLABORATION_WS_URL` unset, verified
in the running image (`MEDIA_URL == "/media/"`, `MEDIA_BASE_URL is None`), and
**refuses to start** if either is set, with the reason on stderr. So the
double-prefix failure is not a convention someone has to remember — it is a
container that will not boot.

`SESSION_COOKIE_NAME` stays `docs_sessionid` (y-provider looks for that exact
string), with cookies scoped `Path=/document-engine/`, `SameSite=Lax`, `Secure`
in production. No Docs frontend or nginx service exists in that compose, and
nothing in it publishes a port: Neev serves this artifact and is the sole public
entry.

## The patch series

Seven patches against pinned v5.7.0. Every behavioural change is inert unless
its environment variable is set, so the patched tree still builds and behaves as
upstream when built without `env/embed.env`.

| patch | what it does |
|---|---|
| `0001-env-typings` | declares the two new `NEXT_PUBLIC_*` variables |
| `0002-serve-under-a-path-prefix` | `basePath` from `NEXT_PUBLIC_BASE_PATH` |
| `0003-backend-base-path` | splits `backendOrigin()` from `backendUrl()`; the latter appends the prefix |
| `0004-collaboration-socket-url` | socket fallback carries the prefix and follows the page scheme |
| `0005-embed-mode` | renders the editor without Docs' own chrome |
| `0006-embed-compact-header` | embed-only: a shorter floating bar and document header, and no role/last-updated row |
| `0007-embed-host-metadata-signal` | tells the host, after the server confirms, that a document's metadata changed |

Two of these are worth explaining rather than just listing.

**`0003` is one change that fixes two paths.** Upstream composes both the REST
base and media URLs from `backendUrl()`, so prefixing that single function covers
both. Nothing about media needed its own patch, which is why there isn't one.

**`0004` changes the fallback in two ways, and the second is easy to miss.**
Upstream hardcodes `wss://`. A plain-HTTP loopback deployment cannot open a
`wss:` socket to itself, so the prefix alone would have left the socket
unreachable in exactly the environment used to test it.

**`0007` sends an identity, never a title.** The host shows the title too and
cannot see a rename that happened inside the frame. Waiting for focus to leave
is not enough either — renaming and immediately clicking something in the host
makes the host's re-read race the save and lose, which is how a tab keeps the
old name until a reload. So the editor posts `neev-doc-metadata-updated` with
the resource id once the **server has confirmed**, and nothing else. A frame
that could name itself could name itself anything; the message says "go and
ask", and the host answers from its own API. `targetOrigin` is the page's own
origin rather than `'*'`, so only a same-origin embedder ever receives it.

### Embedded mode

`NEXT_PUBLIC_EMBED=true`. Neev owns navigation, document listing, identity and
sharing, so Docs does not draw them:

- **gone** — left panel and its resizer, share button, copy-link, share, star,
  duplicate, move (both), leave, delete
- **kept** — the editor, title editing, autosave, formatting, comments, the
  table of contents, present, download, print, history

**Hiding a control is not a permission boundary.** Every hidden action is still
authorised server-side and a hidden button changes nothing about what a request
is allowed to do. Nothing here broadens or narrows any ACL, and none of it should
be read as enforcement — the backend enforces, as it did before.

`isEmbedded()` reads a `NEXT_PUBLIC_*` variable, which Next inlines at build
time, so the hidden branches are eliminated from the bundle rather than merely
skipped.

No service worker is registered or emitted. Upstream registers at
`/service-worker.js` — the root scope — so in an embedded deployment it would
take over caching for the whole host application rather than just this prefix.
`NEXT_PUBLIC_SW_DEACTIVATED=true` skips both the registration and workbox's
`InjectManifest`, so the artifact contains no `service-worker.js` to register.

## Building

```bash
export PATH=/opt/homebrew/opt/node@22/bin:$PATH
./build.sh /tmp/neev-lasuite-build          # ~4 min from cold
KEEP_INSTALL=1 ./build.sh /tmp/neev-lasuite-build   # ~1.5 min, reuses node_modules
```

Upstream's toolchain is **yarn 1**, not pnpm — the repo pins it in
`packageManager` and the workspace layout depends on it.

**Budget ~12 GB of free disk**, and check before starting. The yarn fetch cache
alone expands to ~7.3 GB on top of a 2.0 GB `node_modules` and a 1-2 GB webpack
cache; the script keeps all of it under the work directory and deletes the two
caches at the end, but the peak is real. This is not a guess — an earlier run
filled this Mac's disk completely and took the shell down with it.

Budget 10+ minutes of wall clock, and do not run it under a shorter foreground
timeout. One run was SIGTERMed mid-install for exactly that reason.

The script verifies the pinned tarball's hash before it builds, and checks the
output against the routes contract before it packages: the `docs/[id]/index.html`
the UUID rewrite depends on, prefixed assets, no double prefix, a prefixed
socket, a runtime-derived API base, and no service worker.

### On reproducibility

The build is **pinned and verifiable, not bit-reproducible**, and the difference
matters. Upstream derives the Next build id from `crypto.randomBytes`, so chunk
names and contents differ on every run. Two builds of identical sources will not
produce identical bytes, and no amount of `tar` flag tuning changes that.

What is guaranteed instead:

- the source is pinned by hash and the build refuses to start on a mismatch
- the patch series applies cleanly to that pin and reconstructs, exactly, the
  tree the shipped artifact was built from — verified by extracting the pin
  fresh, applying the five patches and diffing against the built tree. The only
  differences were `next-env.d.ts` and `tsconfig.tsbuildinfo`, both generated by
  Next and tsc.
- every file in the output is hashed into a manifest, so the bytes deployed can
  be checked against the bytes built

## The artifact in this directory

`artifact-v5.7.0-2d1a0c74feb3.sha256` — a sha256 per file, **4160 files**, of
the build described above. The tarball is 43 MB and is not committed; it is at
`/tmp/neev-document-engine-frontend-v5.7.0-2d1a0c74feb3.tar.gz`, sha256
`4ff116449ca90cd9328234e02527a601b03f141efd6b70a31d84be76dde1a8ac`. Rebuild it
with `build.sh` rather than treating that path as durable.

It is larger than the first artifact (24 MB, 367 files) for one reason: this
build runs upstream's `prebuild`, which copies the Apple emoji set into
`assets/fonts/emoji/`. That is ~3800 small files and is the whole difference.
They are still unreferenced by a `PUBLISH_AS_MIT=true` bundle — see the sweep
above — so the growth buys nothing except fidelity to upstream's own build.
Dropping `prebuild` would take it back to 24 MB if the size ever matters more
than that fidelity.

## Verification

What actually ran, with results:

| check | result |
|---|---|
| `tsc --noEmit` | exit 0 |
| `eslint .` | exit 0 |
| 13 new tests, patched | 13 pass |
| the same 13 against **pristine** upstream | 8 fail, 5 pass |
| `next build --webpack` | exit 0, 13 routes exported |
| output vs. routes contract | all checks pass |
| patch series vs. built tree | identical |

The pristine run is the one that matters. Tests that pass against unpatched
sources prove nothing about the patches, so they were run both ways: the 8
failures are the new behaviour, and the 5 that pass on both are deliberate
regression guards that upstream's unprefixed behaviour is unchanged.

### Not verified

Stated plainly, because a gap read as covered is worse than a gap:

- **Nothing was exercised in a browser.** The editor was not loaded, no document
  was opened, nothing was typed, saved or collaborated on against this build. The
  contract is verified by reading the built bundle and by unit tests, which is
  not the same thing as it working.
- **The full existing test suite never completed** — the disk filled during the
  run. Only the 13 new tests, the type-check and the lint were run to completion.
- **Attachment upload is untested**, as it was in the preview.
- The serving config in `serving/` has not been run by an nginx anywhere. It is
  a statement of what the artifact needs, not a tested file — and the framing
  headers in it are load-bearing. The framed document is the static export, so
  `frame-ancestors 'self'` and `X-Frame-Options: SAMEORIGIN` have to be set
  there; the backend keeps upstream's `frame-ancestors 'none'`, which is
  harmless only because its responses are fetched, never framed. Point the
  iframe at a backend-served URL and that policy refuses it.
