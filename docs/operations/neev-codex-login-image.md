# Codex managed-login image

This is a credential-free image substrate, not an enabled personal connection or
prompt runtime. The application still reports `runtimeEnabled:false`.

## Reproducible inputs

Build only for `linux/amd64`, using the five public inputs in a separate empty
build context: `Dockerfile.codex-login`, `codex`, `LICENSE`, `NOTICE`, and
`ca-certificates.crt`. Copy the accompanying Dockerfile-specific ignore file too.
Never use an application environment, host home, credential volume, repository
archive, or `.context` directory as a provider build input.

- Base: Debian Bookworm slim, exact digest in the Dockerfile; inspected as
  `linux/amd64` on the deployment host.
- Executable: unmodified Codex 0.154.0 Linux x86-64 musl executable from the
  previously provenance-verified official package. Its exact SHA-256 is enforced
  during the build.
- Licensing: unmodified [LICENSE](https://raw.githubusercontent.com/openai/codex/rust-v0.154.0/LICENSE)
  and [NOTICE](https://raw.githubusercontent.com/openai/codex/rust-v0.154.0/NOTICE)
  from the same official source tag, with both hashes enforced during the build.
- Public CA bundle: 224,449 bytes, SHA-256
  `714d457d580922dbf1d0be8bd35ba236a842b50b0072ae791582a19adef772a5`,
  copied from `/etc/ssl/certs/ca-certificates.crt` of accepted native image
  `sha256:3799be212231ffa7aa374ad41ddd17c5e48fcc5833a36617679816eb560c4b1a`.
  Extract only that regular file, not image configuration or other files. Reject
  extra tar members, links, or a different size/hash. The image does not require
  access to a private registry containing the application image.

The build command, from that isolated context, is:

```sh
docker build --platform=linux/amd64 --network=none --pull=false \
  -f Dockerfile.codex-login -t neev-codex-login:0.154.0 .
docker image inspect --format '{{.Id}} {{.Os}} {{.Architecture}}' \
  neev-codex-login:0.154.0
```

Require `linux amd64` before running; resolve and use the resulting immutable
image ID. Do not rely on the tag or a different machine's default platform.

## Runtime prerequisites

The process runs as UID/GID 65532. With a read-only image root, mount the allocated
private profile at the exact `CODEX_HOME`, `/home/neev/.codex`, before starting.
That directory must already exist and be writable by UID/GID 65532. Mounting an
empty directory at its parent `/home/neev` alone does not satisfy this requirement.
An empty, mode-0700, 64-MiB tmpfs at the exact path is suitable only for offline
credential-free tests, not a persistent connection.

The supervisor must enforce the reviewed gVisor runtime, read-only root, no
capabilities, no-new-privileges, one CPU/1 GiB memory/128 processes, bounded scratch,
no core dumps, no container logs, exact environment, and no host credential or
repository mounts. The full official executable remains unmodified; the image
alone does not restrict app-server methods. Only the fixed managed-login protocol
may reach its stdio. A separately verified provider-only egress boundary is
required before real login; `network=none` is only an offline test setting.

## Remaining gates

This artifact does not supply the supervisor, enforced egress, authenticated
application routes, browser connection controls, durable prompt jobs, or real
subscription acceptance. Image startup is not proof of any of those features.
Normal restart persistence also requires the independently reviewed encrypted
credential-volume lifecycle and a fresh official account-status check.

Context: [org-memory: wiki/engineering/workos.md]. The approved native isolation
design supersedes that page's legacy operator runtime.
