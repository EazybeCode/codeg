# Neev originals on a Hetzner Storage Box

Status (2026-09-30):

- **Replication is live** (root report). All 50 originals are copied and verified
  under `neev-originals/`, and the hourly timer is enabled.
- **Read-through and eviction are implemented and tested in source, but not
  deployed.** They need a new server build, the broker service, the container
  mount and the eviction timer. See "Deploy requirements".
- No local original has been evicted in production.

This Storage Box folder is separate from the encrypted Restic backups
([backups.md](backups.md)). Replication is not a backup: after eviction, the
Storage Box copy (plus any earlier backups) is the only copy of an idle file.
Enable Storage Box snapshots.

## What is durable and what is cache

| Data | Where | Class | Storage Box? |
| --- | --- | --- | --- |
| Document revisions `{uuid}.md`, editor images `{uuid}.img`, chat/agent attachments incl. generated deliverables `{uuid}.att` | `NEEV_DOCUMENT_ROOT` (`/data/documents`) `/{org_id}/` | Durable originals, write-once, 0600 in 0700 per-org dirs. The local copy is a cache once it is verified remotely. | Copied, verified, evictable |
| Blob metadata (sha256, length, refs, archive flag) | PostgreSQL | Durable, the trusted expectation for every read | No (pg_dump/Restic) |
| `*.tmp` (app and broker temporaries) | same dirs | Unpublished | Skipped |
| Agent run dirs, spool, build cache | host | Rebuildable, cleaned by existing jobs | No |
| Provider homes, credential volumes, Storage Box key | root / volumes | Secret | Never mounted into the app or agents |

## Design

```
app (uid 65532, no credentials)            root host services
BlobStore::read_* ── local hit ──> file     replicate (hourly): copy + remote sha256 → ledger
     │ ENOENT (row proves it exists)        verify (daily): re-hash remote copies → last-verify.json
     └─> broker: hydrate {org,uuid,kind,    evict (daily): pressure-bounded, health-gated unlink
                 sha256,len} ──────────────> broker: ledger(evicted_at) → ssh cat → verify → rename
remove_attachment ─> broker: forget ───────> tombstones.json (never hydrate/stream/replicate again)
content backup ────> broker: stream ───────> verified bytes into the backup, not into the cache
```

1. **Local disk stays primary.** There is no network filesystem. Every read opens
   with no-follow and checks the DB row: owner, 0600, one link, exact length and
   SHA-256 (documents, images and attachments).
2. **Read-through** (`src-tauri/src/neev/content/{blob,originals}.rs`) is on only
   when `NEEV_ORIGINALS_BROKER_SOCKET` is set. On a local miss the store asks the
   broker to `hydrate`, then reopens and verifies the file.
   - The request carries identity plus the row's sha256 and length, never a path.
   - Every broker failure is `ContentError::Storage`, which is 503 (retryable),
     never `NotFound`.
   - Callers: document read/download, `image::load` and `attachment::load`. The
     last covers downloads, previews, agent staging and transcript context.
     Authorization runs before any read (tested).
3. **Broker** (`neev_originals_broker.py`, root). The socket is 0660
   `root:<app gid>`, and the peer uid is checked on every connection. Only
   `client_uid` may hydrate, stream or forget; root may only ping. Paths are
   derived from `remote_root` or `local_root` plus a validated org, UUID and kind.
   - `hydrate` only restores a ledger entry the evictor marked `evicted_at`, with
     the same sha256 and size, and not tombstoned. A blob that is missing but was
     never evicted is lost, not cached, so it is refused.
   - Each request waits on a per-blob lock and re-checks whether the file is
     already present, then takes a transfer slot. Waiting for a slot is bounded
     by `queue_timeout_seconds` and ends in `busy`. Concurrent readers of one
     blob fetch once and never hold slots.
   - The fetch streams `ssh … -- cat` into an app-owned 0600 temp file, bounded
     by the expected length. The broker checks the hash and fsyncs, then
     publishes with an atomic **no-replace rename** (`renameat2 RENAME_NOREPLACE`
     on Linux). That means no second link and no symlink following. It never
     overwrites a local file.
   - On startup it removes its own `*.fetch-*.tmp` leftovers older than the fetch
     timeout. It caches the ledger and tombstone files by inode, size and mtime.
   - `stream` sends ledger-verified remote bytes to the caller without
     publishing them. The content backup uses it, so a backup never refills the
     cache.
   - `forget` records a tombstone. The app sends it from `remove_attachment` (the
     24 h orphan sweep, after the row is deleted) **before** the local unlink, so
     a racing read-through cannot resurrect the blob. `forget` takes the same
     per-blob lock as `hydrate`: it waits for an in-flight fetch to publish, so
     the unlink that follows it always removes what was published. The broker
     also re-checks tombstones just before it publishes. A tombstoned blob is never
     hydrated, streamed, replicated or evicted again.
4. **Eviction** (`neev_storage_box.py evict`, daily). It refuses to run unless
   the broker answers a ping **and** the last `verify` succeeded within
   `max_verify_age_seconds` (default 26 h). It only acts under pressure (over
   `max_local_bytes` or under `min_free_bytes`), freeing just the shortfall,
   least recently used first, up to `max_evictions_per_run`. A candidate must be
   an allowed kind (default `att`, `img`), not tombstoned, match the ledger, and
   have atime and mtime older than `min_idle_seconds` (at least 1 day). It must
   also re-hash correctly remotely in this run and be unchanged just before
   unlink.
   - **Crash safety:** `evicted_at` is written to the ledger before any unlink, so
     a crash cannot leave an unrestorable blob.
   - **Access tracking:** the app sets atime on each read (at most hourly).
     Replication, eviction, the broker and the content backup all read with
     `O_NOATIME`, so their reads never count as use.
   - **Scanners:** replicate and the backup skip a final that has two links while
     a matching publish temp exists (the app's link-then-unlink instant). Any
     other extra link still fails closed.
5. **Verify** (daily timer) re-hashes every ledger entry remotely and records
   the result in `last-verify.json`, whether ok, mismatch or unavailable. A
   failed unit means remote loss or an outage: alert on it. Eviction stays
   blocked until a verify succeeds.
6. **Purge** (`purge --older-than-days 30`, manual or scheduled after review)
   deletes remote copies, one fixed path each, only for blobs that are
   tombstoned and past the grace period. The ledger entry is dropped only after
   the remote file is confirmed gone. The tombstone stays.
7. **Locking.** Hourly `replicate` skips a busy window. `verify`, `evict` and
   `purge` wait up to 30 min for the lock instead of failing.
8. **SSH trust** is unchanged: `-F /dev/null`, `StrictHostKeyChecking=yes` with a
   pinned key matching Hetzner's published fingerprints, `BatchMode`, no agent or
   forwarding.

## Durability after eviction (read before enabling it)

An evicted original's copies are the Storage Box copy, Storage Box snapshots if
enabled, and any backup made before the eviction. The existing Restic job uploads
only its configured archive directories. Check on the host whether those archives
include the Neev document volume. If they do, eviction shrinks later archives,
and retention (7 daily / 4 weekly / 3 monthly) eventually expires the last
archive that holds an evicted file.

To keep the Restic guarantee, do one of these before enabling eviction:

- **Preferred:** take the Neev content backup with
  `NEEV_ORIGINALS_BROKER_SOCKET` set. It streams evicted originals into the
  backup output, so every backup stays complete. Restic then covers that output
  directory.
- Keep eviction disabled for that data.

Storage Box snapshots are a separate prerequisite, handled as configuration.
The SSH key can `rm`, so snapshots, which the key cannot delete, protect against
key misuse.

## Not covered (explicit)

- **Encryption at the provider.** Originals are plaintext bytes on the Storage
  Box.
- **Documents and images are never deleted by the app.** Only attachments (via
  the orphan sweep) reach `forget`. A future hard delete or data-subject erasure
  must call the same `remove_attachment`/`forget` path.
- **The restore verifier** (`neev-content-verify`) reads a restored root without
  a broker. Restore with `restore-blobs`, which copies every blob.

## Deploy requirements (not done here)

Order matters; stop at any failure.

1. **Server.** Build and release from this branch (there is no schema change).
   Set `NEEV_ORIGINALS_BROKER_SOCKET=/run/neev-originals/broker.sock` in the app
   container and bind-mount the host directory `/run/neev-originals` read-write
   at the same path. Do **not** mount it into agent sandboxes; their uids differ
   and would be refused anyway.
2. **Broker.** Install `neev_originals_broker.py` beside `neev_storage_box.py`
   in `/opt/neev-storage-box/`.
   - Add a `broker` section to `/etc/neev-storage-box/config.json`. `client_uid`
     and `socket_gid` are the app's host uid and gid (65532 unless
     user-namespace remapping is active).
   - Install `neev-originals-broker.service`, replacing
     `@NEEV_DOCUMENT_ROOT_HOST_PATH@` with the `local_root` value.
   - Start it and run `neev_originals_broker.py ping`.
3. **Verify health.** Install and enable `neev-storage-box-verify.{service,timer}`.
   Run it once and confirm `status` shows `last_verify.ok: true`. Connect the
   unit's failure state to monitoring.
4. **Prerequisites for eviction:** Storage Box snapshots are enabled, and the
   backup decision in "Durability after eviction" is made.
5. **Live acceptance.** Evict exactly one idle attachment with a temporary copy
   of the config: `max_local_bytes: 0`, `max_evictions_per_run: 1`,
   `kinds: ["att"]`.
   - Run `evict --dry-run`, then `evict`, with `--config` pointing at the copy.
   - Download that attachment through the app. Confirm it was restored with the
     same sha256, 0600 permissions and one link.
   - Stop the broker and confirm that an evicted blob returns 503 and not 404.
     Then restart it.
6. **Eviction.** Add the real `eviction` section and run `evict --dry-run`.
   Install and enable `neev-storage-box-evict.{service,timer}` (same placeholder).
7. **Purge (optional).** Run `purge --dry-run`, then schedule it if wanted.

## Operator commands

```sh
python3 /opt/neev-storage-box/neev_storage_box.py status
python3 /opt/neev-storage-box/neev_storage_box.py verify --sample 100
python3 /opt/neev-storage-box/neev_storage_box.py evict --dry-run
python3 /opt/neev-storage-box/neev_storage_box.py purge --dry-run
python3 /opt/neev-storage-box/neev_originals_broker.py ping
journalctl -u neev-originals-broker -u neev-storage-box -u neev-storage-box-verify -u neev-storage-box-evict
```

Exit codes: 0 ok, 1 configuration, 2 local integrity, 3 remote or broker
unavailable, 4 remote verification mismatch.

## Content backup

`scripts/neev-content-backup.mjs` copies `.md`, `.img` and `.att` blobs against
the exact rows in its own dump. It reads with `O_NOATIME` and streams evicted
originals from the broker into the output, re-hashing them, without refilling
the live root. It also has a `restore-blobs` command. See [neev-content-backup.md](neev-content-backup.md).

Sources: [Storage Box SSH/rsync access](https://docs.hetzner.com/storage/storage-box/access/access-ssh-rsync-borg/),
[general info and host key fingerprints](https://docs.hetzner.com/storage/storage-box/general/),
[SSH keys](https://docs.hetzner.com/storage/storage-box/backup-space-ssh-keys/) (fetched 2026-09-30).
