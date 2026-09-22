# Releasing the document engine into production

The procedure `release.sh` performs, written down so it can be reviewed before it runs and repeated
afterwards. No credential appears in this file or in that script: both read secrets from root-only
files at run time and report names, lengths and modes rather than values.

The shape of the release is the point. Two things could go wrong independently — the new Neev image
might misbehave, or creating documents on the engine might — so they are separated. `rollout` leaves
Neev running the engine-aware image with **creation off**: documents already on the engine open,
nothing new is created there. `enable-creation` is a second, deliberate command, and
`disable-creation` is its exact inverse with no image change.

## Before anything

```
release.sh preflight --image <candidate>
```

Reads only. It refuses to continue unless all of this holds:

| check | why it is there |
|---|---|
| candidate image present locally | compose sets `pull_policy: never`, so a missing image fails *after* the old container is gone |
| entrypoint present in the candidate | the entrypoint is set by compose, not the image; without it the container starts and dies |
| launch helper keeps its file capabilities | `neev-agent-caplaunch` carries `eip`. A rebuild that drops them yields an image that starts, passes every health check, then cannot launch an agent |
| binary hashes, candidate vs deployed | a changed agent binary or launcher is visible before the swap, not after |
| workspace root agrees | a candidate built against a different root starts and then finds no agents |
| compose anchors present | the edit assumes `image`, `env_file` and `volumes` exist on that service |
| capabilities and devices as configured | `SYS_ADMIN NET_ADMIN MKNOD`, four devices, one cgroup rule |
| docs stack valid, no published ports | Neev being the only way in is what makes header identity legitimate |
| `ALLOWED_HOSTS` contains the backend alias | the proxy dials the alias, so Django sees it as `Host`; without it every call is a 400 |
| artifact has `docs/[id]/index.html`, no service worker | the editor route must exist; a service worker must not be served |
| engine env complete, mode 600, creation off | a short bridge key is a 503 at the far end rather than an obvious error |
| database reachable, client at least the server's major | a client older than the server cannot dump it |
| free disk | a backup that runs out of space is not a backup |

The candidate checks run one disposable container per image (`--rm --network none --read-only`)
which reads its own filesystem and exits.

## Backup

```
release.sh backup --yes
```

Dumps the native database with `pg_dump --format=custom`, using the host client when it is at least
the server's major version and the pinned `postgres:16` image otherwise, with the CA certificate
mounted read-only. It then verifies the dump rather than assuming it: non-trivial size, at least ten
objects listed, and `neev_resources` table data actually present. A `.meta` file records the row
count read from the live database *before* the dump, the byte size, the object count and the image
in place at the time.

Every backup is kept. Nothing in this procedure deletes one.

## Rollout

```
release.sh rollout --image <candidate> --yes
```

1. Preflight again, then require a backup less than two hours old.
2. Start the private docs stack and confirm the backend passes its own `manage.py check`.
3. Install the engine env at `/etc/neev-native/document-engine.env`, mode 600, creation **off**.
4. Build the compose edit **beside** the live file and validate it there — the live file is not the
   thing being tested. Three fields may differ on the `neev` service: `image`, one appended
   `env_file` entry, one appended read-only volume. Every other key of that service, every other
   service, and every top-level key must be identical. Re-running changes nothing further.
5. Only then install the staged file, keeping a timestamped copy of the previous one.
6. Recreate **only** the `neev` service. The `https` container keeps serving throughout.
7. Health: Neev must answer HTTP, and the running container must still hold its capabilities, its
   devices and a read-only artifact mount.

What it reports is a summary of the allowed changes, never a diff: `environment:` can inline
credentials, and a diff would put them on someone's terminal.

## Enabling creation

```
release.sh enable-creation --yes
```

New documents are created on the engine from the moment Neev restarts.

## Rollback

1. `release.sh disable-creation --yes` — creation off, engine still wired, **same image**. Documents
   already on the engine keep opening. Reverting the image instead would remove the proxy that
   serves them, which is why rollback does not touch it.
2. `release.sh detach --yes` — remove the engine env and the artifact mount, same image. Neev stops
   being configured for the engine. Documents created there become unreachable *from Neev*; no data
   is destroyed. Step 1 is what avoids ever needing this with a population to strand.
3. `cd <docs stack> && docker compose stop` — volumes kept. `down -v` destroys the documents and is
   never part of a rollback.

## Two operational notes that cost time to learn

- **Compose does not notice a changed file.** `restart` keeps the environment the container was
  created with, and `up -d` sees an unchanged configuration and leaves a bind-mounted package
  already imported. Changing `env.d/` or `backend/neev_bridge/` needs
  `up -d --force-recreate`. A fix verified by the test suite — which runs in a fresh container —
  can look absent through the running service for exactly this reason.
- **Migrations are not run by the image.** After a version bump, run
  `docker compose run --rm backend python manage.py migrate` before bringing the stack up: the
  healthcheck is `manage.py check` and it fails against an unmigrated schema.

## What the script deliberately does not do

Touch the `https` service, change an existing environment value, remove or alter a volume, relax a
capability or device, revert the image during a rollback, delete a backup, or go near the preview
and canary stacks.
