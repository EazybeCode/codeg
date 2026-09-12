# Native Neev image implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Package the accepted native application and recovery verifier without inheriting the historical operator image.

**Architecture:** A dedicated multi-stage Dockerfile builds the existing static frontend and two native Rust binaries, then copies only runtime artifacts into a non-root Debian image. The image cannot enable agent execution and does not contain credentials. Actual-image acceptance and later private deployment remain distinct gates from source/configuration checks.

**Tech Stack:** Node22.23.2, pnpm11.9.0, Rust1.98.1, Debian bookworm, existing Cargo/pnpm locks, Docker.

**Spec:** `docs/superpowers/specs/2026-09-11-independent-identity-and-organizations-design.md`, specifically Acceptance and operations. This plan implements only its native packaging slice; it does not implement personal-agent supervision or claim public release.

## Global Constraints

- Use fresh native application tables and storage for the new release; no import of existing GitHub organizations, users or catalog data is required.
- Old operator routes, sessions, tables and token fallback must not be accepted by this application.
- Retain old databases/volumes and backups offline rather than erase them as part of setup.
- No GitHub repository, branch or organization is deleted or modified.
- Keep runtime disabled until the existing isolated-agent and durable-job gates pass.
- No new paid service, mail broadcast, consent acceptance or cloud permission expansion is authorized by this document.
- Secrets supplied outside Git/chat through the protected deployment mechanism.
- Build context must be an exact committed Git archive, never `.context`, local credentials or existing build output.
- Preserve the existing Dockerfile and legacy deployment configuration; create a separate native entrypoint.

## File responsibilities

`Dockerfile.native` owns build stages, runtime dependencies and image defaults.
`Dockerfile.native.dockerignore` owns an allowlisted build context.
`scripts/neev-native-image.test.mjs` performs explicit actual-image structural and fail-closed startup checks using the local Docker CLI; it never builds, pulls, authenticates or reaches a database.
`docs/operations/neev-native-image.md` records exact commands, acceptance evidence and deployment gates.

### Task 1: Native image and actual-image contract tests

**Files:**
- Create: `Dockerfile.native`
- Create: `Dockerfile.native.dockerignore`
- Create: `scripts/neev-native-image.test.mjs`
- Create: `docs/operations/neev-native-image.md`

**Interfaces:**
- Consumes existing `pnpm build`, root `postinstall`, `src-tauri/Cargo.lock`, native config and named binaries `neev-server` / `neev-content-verify`.
- Produces image default `USER 65532:65532`, `CMD ["/usr/local/bin/neev-server"]`, static root `/app/web`, document root `/data/documents`, bind `0.0.0.0:3080`, runtime disabled. No origin/database/key/provider defaults.
- Actual-image tests consume a local image reference in `NEEV_IMAGE_UNDER_TEST`; absent input is an explicit skipped integration gate, never success evidence for an image. No automatic pull or registry login.

- [ ] **Step 1: Write actual-image tests before the Dockerfile.** Use Node's built-in test/assert/child_process modules, asynchronous or bounded synchronous Docker calls, and a unique test-owned container name. Require the supplied image exists locally with `docker image inspect`, resolve its immutable ID, and use that ID thereafter. Assert the following concrete contract:

```js
assert.equal(config.User, "65532:65532")
assert.deepEqual(config.Cmd, ["/usr/local/bin/neev-server"])
assert.ok(config.Env.includes("NEEV_RUNTIME_ENABLED=false"))
assert.ok(config.Env.includes("NEEV_STATIC_ROOT=/app/web"))
assert.ok(config.Env.includes("NEEV_DOCUMENT_ROOT=/data/documents"))
```

Reject baked secret/provider/legacy configuration and declared volumes. Run the actual default command, without an entrypoint or user override, with network none, read-only root, all capabilities dropped, no-new-privileges, 128 PIDs, 1 CPU, 1GiB memory and memory-swap limit, 64MiB noexec/nosuid/nodev tmpfs. With no database/key/origin it must exit nonzero, not start the legacy server. Do not require an exact human error message. Cleanup only the exact created container in `finally`; no images/volumes/prune. A separate diagnostic run may override entrypoint to `/bin/sh` solely to assert effective UID65532, both native binaries/static index readable, root write refused, Node/npm/git/ssh/docker/rustc/cargo absent, and `ldd` has no missing shared libraries. Label that diagnostic separately from default-command acceptance.

- [ ] **Step 2: Demonstrate RED or explicitly record unavailable Docker.**

```sh
NEEV_IMAGE_UNDER_TEST=neev-native:candidate node --test scripts/neev-native-image.test.mjs
```

Before implementation this must fail for a missing image or contrary image contract; an absent Docker daemon is infrastructure evidence, not behavioral RED. Do not invent test results or create production test-auth routes to satisfy a gate. Controller may arrange a private Linux build after the scoped source is committed.

- [ ] **Step 3: Implement the dedicated stages.** Use these official manifest digests (registry manifest headers verified September12; actual pull/build still required):

```dockerfile
FROM node:22.23.2-bookworm-slim@sha256:83f487e0a63425e5b4d146fb5e5be574bcbe1b7b843d3ebafdd95eaf7767a7e5 AS frontend
FROM rust:1.98.1-slim-bookworm@sha256:ebd900bae66fd508b466cef82d64a83a5fb34682e4c8b2797a42908bddc95a57 AS backend
FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS native
```

Frontend: enable bundled corepack, activate exact pnpm11.9.0, frozen install with `--ignore-scripts`, then explicitly execute the inspected root Monaco `postinstall` and build. Do not change package policy/locks or run additional dependency hooks. Copy only required source/public/config inputs. Preserve LICENSE and installed BlockNote core/react/shadcn LICENSE files in `/usr/share/doc/neev/`; include an attribution text identifying pinned package versions and upstream source URLs. Do not modify upstream packages.

Backend: install build-essential/pkg-config/libssl-dev/ca-certificates with no recommended packages; `CARGO_BUILD_JOBS=1` and incremental/debug info disabled for bounded builds. Copy Cargo manifest/lock, build.rs, src, resources, experts and science because the shared crate embeds those inputs. Build only named native binaries with `--locked --release --no-default-features`; do not enable `test-utils` or desktop features. Runtime: install only CA certificates and required native shared libraries (initial expected OpenSSL3/libgcc/libstdc++; actual `ldd` decides). Copy native binaries and frontend static output, root-owned nonwritable to UID65532. Create `/data/documents` owner65532:65532 mode0700, separate from static root. Never chown an existing user mount automatically. No provider clients, operator binary, SSH home, Docker socket, compiler, Node, package tree, baked app key or VOLUME declaration. Set the exact defaults above and `STOPSIGNAL SIGINT`.

Allowlist context starting with `**` exclusion, then exact Dockerfile/package/lock/config/LICENSE inputs plus src/public and the required Rust subtrees. Re-exclude `.env*`, `.context`, `.git`, node_modules, target, out, logs and known key suffixes recursively; runtime copies must not include source/test files. Document that allowlisting complements, not replaces, a secret-scanned exact Git archive.

- [ ] **Step 4: Record build/run procedures and the unpassed gates.**

```sh
git archive --format=tar HEAD | docker build --file Dockerfile.native --tag neev-native:candidate -
NEEV_IMAGE_UNDER_TEST=neev-native:candidate node --test scripts/neev-native-image.test.mjs
```

Run from source only after scoped commit and secret scanning. Archive contains no untracked files; never send a whole workspace. Record exact commit/image ID/base digests and test output when actually available. Runtime deployment additionally needs a fresh native database, verify-full PostgreSQL certificate, distinct protected encryption key, configured origin/providers, exact proxy allowlist, owned document mount and resource limits. Do not include real values or make placeholder examples executable. No public port/proxy cutover in this task.

Actual authenticated production-binary acceptance needs real owner-controlled Google/mail configuration and participant consent. Synthetic routes deliberately are not compiled into this binary: report the gap rather than injecting sessions or shipping a bypass. Restart/content recovery, mixed-format isolated restore, wrong DB certificate rejection, two-org privacy, HTTPS and personal-agent jobs remain release gates beyond this packaging test.

- [ ] **Step 5: Verify source and commit only scoped files.**

```sh
node --check scripts/neev-native-image.test.mjs
node --test scripts/neev-native-image.test.mjs
git diff --check
git add Dockerfile.native Dockerfile.native.dockerignore scripts/neev-native-image.test.mjs docs/operations/neev-native-image.md
git commit -m "build(neev): add isolated native server image"
```

The no-image Node run must explicitly skip the actual-image gate. It cannot be described as passing integration acceptance. Return DONE_WITH_CONCERNS if actual Docker evidence is unavailable; the controller must resolve the image gate before accepting deployability. Do not push or mutate cloud resources as part of implementation.

## Self-review

This slice covers native packaging, secret/context separation and a reproducible diagnostic gate. Identity/storage/lifecycle features remain in their existing accepted plans; real provider, database activation, backup scheduling, production proxy and agent job implementation are explicitly outside this task. The image test consumes only immutable local image IDs, never secrets; its default-command check cannot accidentally pull a remote candidate or start a networked operator. No shared task interface or file ownership conflict exists because this is one cohesive packaging task.
