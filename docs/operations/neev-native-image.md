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
nor any cloud mutation, and old databases, volumes, and backups remain offline
and untouched.
