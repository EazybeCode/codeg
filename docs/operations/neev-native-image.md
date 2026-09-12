# Neev native image operations

This image packages only the accepted `neev-server` and
`neev-content-verify` binaries with the static frontend. It is separate from
the historical operator image and does not contain an agent runtime, provider
clients, build toolchain, credentials, or legacy server defaults.

## Reproducible source boundary

Build only a secret-scanned, exact Git archive from a scoped source commit.
Never submit the working tree, `.context`, local build output, or local
credentials as the Docker context. The allowlist in
`Dockerfile.native.dockerignore` is defense in depth; it complements the exact
archive and secret scan rather than replacing either control.

After the four packaging files are committed and the commit passes the
approved repository secret scan, run from the repository root:

```sh
git archive --format=tar HEAD | docker build --file Dockerfile.native --tag neev-native:candidate -
NEEV_IMAGE_UNDER_TEST=neev-native:candidate node --test scripts/neev-native-image.test.mjs
```

The test first verifies that the supplied reference already exists locally,
resolves it to an immutable image ID, and uses only that ID thereafter. It does
not build, pull, log in to a registry, reach a database, or remove anything
except its uniquely named test containers. An unset `NEEV_IMAGE_UNDER_TEST`
explicitly skips every actual-image test; that source-only result is not image
acceptance.

For every candidate, retain the following evidence in the private release
record:

- Exact source commit from `git rev-parse HEAD`.
- Immutable candidate ID from `docker image inspect --format '{{.Id}}'
  neev-native:candidate`.
- Complete image-contract test output.
- The three base manifest digests:
  - Node 22.23.2 bookworm-slim:
    `sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5`
  - Rust 1.98.1 slim-bookworm:
    `sha256:ebd900bae66fd508b466cef82d64a83a5fb34682e4c8b2797a42908bddc95a57`
  - Debian bookworm-slim:
    `sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171`

Do not claim deployability until a Linux build records all of this evidence.
The image contract also checks the runtime libraries with `ldd`; those results
decide whether the minimal runtime library set needs an explicitly reviewed
change.

## Image defaults

The image runs as `65532:65532`, executes
`/usr/local/bin/neev-server`, binds `0.0.0.0:3080`, serves static files from
`/app/web`, stores documents under `/data/documents`, and keeps the application
runtime disabled. It deliberately bakes no application origin, database URL,
encryption key, provider configuration, proxy allowlist, test identity, or
volume declaration. With those protected settings absent, its default command
must exit nonzero.

The static tree and binaries are root-owned and not writable by the runtime
UID. `/data/documents` is a separate mode-0700 directory owned by UID/GID 65532.
A deployment must supply a mount already owned by that identity; the image does
not recursively change ownership of an existing user mount.

## Accepted candidate evidence: 2026-09-12

The accepted candidate was built from the secret-scanned exact archive at
commit `0b6028a6da76401bafba81779b5f74f1f7a26055`. The corrected host-side
contract at `ce2269f2685ddf2e35b5dcd9ea3a12130b9d2655` changed no Dockerfile or
image input.

- Immutable image ID:
  `sha256:331dfa73071ae6fe6aa8b654da23b11af07f302a26ac9cf41fe55041ccfa6d54`
- Image size: 446,528,953 bytes.
- Successful build history ID: `zd7zj5gx19gmwxivasn4yk6cr`, completed at
  2026-09-12 12:43:28 UTC.
- The Rust release build took 15m04s and emitted 99 library warnings plus a
  future-incompatibility notice for `proc-macro-error2`.

The first isolated build attempt used a 4 GiB physical-memory limit with the
same memory-plus-swap limit and was killed while compiling `codeg_lib` after
783.8 seconds. This was builder resource exhaustion, not an image-contract
failure. An otherwise identical retry retained the 4 GiB physical-memory and
2-CPU limits while allowing 7 GiB total memory plus swap; it completed. The
builder was then stopped, with its cache and candidate image retained.

The corrected Node contract ran through the strict SSH Docker wrapper against
the immutable image ID above and exited 0 in 20.504 seconds: 7 tests passed and
none failed or skipped. This includes the metadata, fail-closed default command,
runtime diagnostic, and cleanup behavior checks.

A separate positive private-startup check used an internal-only Docker network
and disposable PostgreSQL 16 container (image ID prefix `cf78e766`, UID 70)
with a 512 MiB memory limit and 256 MiB data tmpfs. The native candidate ran
with its default UID and command, a read-only root, all capabilities dropped,
no-new-privileges, 128 PIDs, 1 CPU, a 1 GiB memory/swap limit, no published
ports, and a mode-0700 document tmpfs owned by UID/GID 65532. Only invented,
nonsecret configuration was supplied.

That startup created 23 fresh public tables. `/healthz` and `/workspace`
returned 200; workspace capabilities reported native identity mode, runtime
disabled, and all providers disabled. `/api/neev/session` returned 401, while
the legacy `/api/settings`, `/api/team/session`, and synthetic
`/__synthetic/google` routes each returned 404.

A fresh headed Chromium session reached the app through an owner-controlled
loopback SSH tunnel at a 375-pixel mobile viewport. It made no external requests
and reported no page errors; Google and email controls were visibly unavailable.
An initial navigation timeout and an overly exact button-name lookup were
harness errors; DOM inspection and regex assertions confirmed the intended UI.
The browser and tunnel were closed. A container restart followed by a 200
health check proves startup-only restart behavior, not content persistence or
recovery.

The exact test application and database containers and their test network were
removed. The follow-up network list was empty, and the container list contained
only the stopped BuildKit builder. The legacy deployment and dedicated
PostgreSQL service were untouched. A stale CLI stream after the completed build
was resolved by checking builder history and the actual image before
terminating the owned SSH session; it was not a client-exit-255 build failure.

This evidence accepts the packaging contract and constrained private startup
only. It does not accept real authentication, mail delivery, content snapshots,
recovery, or production deployment.

## Deployment prerequisites and remaining release gates

Before any private deployment, operators must provide through the protected
deployment mechanism:

- A fresh native PostgreSQL database and a verify-full PostgreSQL certificate.
- A distinct protected encryption key.
- The application origin and owner-controlled provider configuration.
- An exact trusted-proxy allowlist.
- An owned document mount and enforced CPU, memory, PID, filesystem, capability,
  and network limits.

Real authenticated production-binary acceptance also requires owner-controlled
Google/mail configuration and participant consent. Synthetic authentication
routes are intentionally not compiled into this binary; do not inject sessions
or ship a bypass to satisfy that gate.

Restart and content recovery, mixed-format isolated restore, rejection of the
wrong database certificate, two-organization privacy, HTTPS, backup scheduling,
and personal-agent jobs remain release gates outside this packaging task. Keep
the runtime disabled. This task authorizes neither a public port/proxy cutover
nor any cloud mutation. Existing databases, volumes, and backups remain
preserved and untouched; the legacy deployment, including its PostgreSQL
container, continues running until a separately authorized transition.
