# Org knowledge storage: server root, Storage Box replica and backup

Org knowledge (schema 22) stores captured source snapshots and wiki page revisions
as immutable markdown blobs. This page covers where they live, how they reach the
existing Hetzner Storage Box, and how they are backed up and restored together
with the database rows that index them.

**Status: not deployed.** The Rust side (the knowledge `BlobStore`, schema 22 and
the native verifier's knowledge checks) is implemented and tested in the working
tree. Nothing is released yet: the host steps below are performed in the release
task, and nothing here changes a running service.

## Layout

| Copy | Location | Written by | Deletes? |
|---|---|---|---|
| Server (authoritative) | host `/var/lib/neev-native/knowledge/{org_id}/{uuid}.md`, mounted at `/data/knowledge` (`NEEV_KNOWLEDGE_ROOT`) | the app's knowledge `BlobStore`, write-once | never by these jobs |
| Storage Box replica | `neev-knowledge/{org_id}/{uuid}.md` on the existing box | `neev_storage_box.py replicate` with `/etc/neev-storage-box/knowledge.json` | never |
| Encrypted backup | `blobs-knowledge/{org}/{key}.md` inside each content backup, then the `neev-backups` Restic repository | `neev_backup.py` → `neev-content-backup.mjs` | Restic retention only (7 daily / 4 weekly / 3 monthly) |

- Directories are 0700, files 0600, owner 65532 (the app uid) on the server.
- Snapshots (captured shared docs and team chats, 0–65,536 bytes) and page
  revisions (1–65,536 bytes) are both `.md` blobs; the backup enforces the same
  bounds as the schema-22 CHECKs. The database rows in `neev_knowledge_snapshots`
  and `neev_knowledge_revisions` hold each blob's org, key, SHA-256 and length.
- `neev-knowledge/` is a new top-level folder on the box, separate from
  `neev-originals/`, `neev-backups/` and `workos-restic/`. One folder per org.

### The never-evict rule

Knowledge is **never evicted** from the server. The server always holds a full
copy, and the Storage Box folder is a second copy, not a cache tier:

- `knowledge.json` has **no `broker` and no `eviction` keys**.
  `neev_storage_box.py evict` refuses such a config before doing anything
  ("eviction is not configured"), and `purge` acts only on broker tombstones,
  which cannot exist without a broker.
- There is no `neev-knowledge-storage-box-evict` unit, and none may be added.
- The content backup has no broker fallback for knowledge: a referenced blob
  missing from the server root fails the backup.

## Storage Box replica

The **unchanged** `neev_storage_box.py` runs with a second config. Auth is reused
from the existing main-account config: same host, user, identity and known_hosts,
with `allow_main_account: true` (approved; no sub-account is needed). No key or
password is copied, printed or committed.

| Unit | Schedule | Does |
|---|---|---|
| `neev-knowledge-storage-box.{service,timer}` | hourly at `:30` (+≤5 min) | `replicate`: rsync `--ignore-existing` (temp file + rename, never `--delete`), then remote `sha256sum` of each new copy, recorded in the ledger |
| `neev-knowledge-storage-box-verify.{service,timer}` | daily 02:40 UTC (+≤10 min) | `verify`: re-hash every ledger entry on the box; a failure means remote loss or outage |

The `:30` replicate stays clear of the originals job's `:00–:05` window. Verify at
02:40 runs after the originals verify (02:20) and before the ZFS snapshot (~03:15
UTC, as observed), so the same night's box snapshot captures the verified state.
Units are line-for-line copies of the originals units: only `Description`, the
`--config` path, `ReadWritePaths` (the knowledge state dir only) and the timer
schedule differ. `scripts/tests/test_neev_knowledge_storage_box.py` enforces this.

## Release order

These steps happen in this exact order. Each gates the next, and none may be
reordered.

1. **Backup code first.** Install the v3 `neev-content-backup.mjs`, the matching
   `neev_backup.py` and `neev-content-backup-run.sh` **together**, before or with
   the schema-22 image, never after it.
   - The deployed v2 script lists only its three blob tables. On a schema-22 dump
     it silently ignores knowledge rows and still writes a "complete" backup
     without their blobs.
   - The v3 script under the deployed v2-only `neev_backup.py` fails
     `verify_local`, so the two cannot be split.
   - Run one manual `neev_backup.py backup` (no `knowledge_source` yet) and
     confirm it promotes a snapshot.
2. **Knowledge root.** Create `/var/lib/neev-native/knowledge` (65532:65532,
   0700). This must happen before the container is recreated with the mount.
3. **Configure the backup root.** Add `"knowledge_source":
   "/var/lib/neev-native/knowledge"` to `/etc/neev-backup/config.json`, then run
   a manual backup and confirm `knowledge_blobs: 0` and a promoted snapshot.
   This must happen **before capture is enabled**. Otherwise, once the first
   knowledge row exists, the nightly backup fails with
   `knowledge-not-configured`. That is fail-closed, but it is a predictable red
   night.
4. **Deploy the schema-22 image** with the bind mount and
   `NEEV_KNOWLEDGE_ROOT=/data/knowledge` (recreate the container).
5. **Storage Box replica.** Install `knowledge.json`, run `preflight
   --write-probe`, then do a manual replicate and verify and check `status`
   (below). Only then enable the two timers.
6. **Enable capture** for the org.
7. **Restore proof.** After the first capture, take a backup and restore it
   into an isolated database plus fresh roots (`restore-blobs --target …
   --knowledge-target …`). Then run `neev-content-verify` with
   `NEEV_VERIFY_KNOWLEDGE_ROOT` set to the restored knowledge root. The restore
   is proven only when it exits 0 and reports `knowledge_blobs` equal to the
   restored knowledge row count.
   - `restore-blobs` re-hashes against `complete.json`, but it does not re-derive
     the knowledge list from the dump. A `complete.json` with knowledge entries
     removed would restore without error.
   - The native verifier reads the restored rows and re-reads every knowledge
     blob (64 KiB bounds), so it is the step that catches that. The release's
     restore rehearsal is not complete without this run on the host.

### Install (release task; stop at any failure)

1. Create the server root before the app is deployed with the knowledge mount:

   ```sh
   install -d -o 65532 -g 65532 -m 0700 /var/lib/neev-native/knowledge
   install -d -m 0700 /var/lib/neev-storage-box-knowledge
   ```

2. Create `knowledge.json` from the existing config **without printing either**:

   ```sh
   umask 077
   jq '.remote_root="neev-knowledge"
       | .local_root="/var/lib/neev-native/knowledge"
       | .state_dir="/var/lib/neev-storage-box-knowledge"
       | del(.broker,.eviction)' \
     /etc/neev-storage-box/config.json > /etc/neev-storage-box/knowledge.json
   chmod 600 /etc/neev-storage-box/knowledge.json
   jq -r 'keys|join(" ")' /etc/neev-storage-box/knowledge.json   # names only
   ```

   Expect exactly `allow_main_account host identity_file known_hosts_file
   local_root remote_root state_dir user` (plus `port` if the original has it).
   `scripts/operations/neev-knowledge-storage-box.config.example.json` shows the
   shape with placeholders.

3. Preflight. `--write-probe` must pass before any timer is enabled. It uploads
   one random probe into `neev-knowledge/`, verifies its hash remotely and
   removes only that probe:

   ```sh
   python3 /opt/neev-storage-box/neev_storage_box.py \
     --config /etc/neev-storage-box/knowledge.json preflight --write-probe
   ```

4. Install the four units from `scripts/operations/` into `/etc/systemd/system/`,
   then run one replicate and one verify by hand and check status:

   ```sh
   systemctl daemon-reload
   systemctl start neev-knowledge-storage-box.service
   systemctl start neev-knowledge-storage-box-verify.service
   python3 /opt/neev-storage-box/neev_storage_box.py \
     --config /etc/neev-storage-box/knowledge.json status
   ```

   `status` must show the local count equal to the verified remote count,
   0 pending, `eviction: not configured`, and `last_verify.ok: true`.

5. Only then: `systemctl enable --now neev-knowledge-storage-box.timer
   neev-knowledge-storage-box-verify.timer`. Connect both units' failure state
   to the same alerting as the originals jobs (none is configured today).

## Backup and restore (DB and blobs together)

The database dump is the index of the knowledge blobs, so they are one recovery
point. There is no separate manifest or pointer file.

- Set `"knowledge_source": "/var/lib/neev-native/knowledge"` in
  `/etc/neev-backup/config.json` once the root exists (release task).
  `neev_backup.py` then passes it to the runner as `NEEV_BACKUP_KNOWLEDGE_SOURCE`,
  and the content backup copies every knowledge blob that the **dumped** rows
  reference into `blobs-knowledge/`, re-hashed (format `neev-content-copy-v3`).
- With `knowledge_source` set, `neev_backup.py` refuses an output that lacks the
  knowledge section, re-hashes every knowledge blob before upload, and its
  restore canary restores and re-hashes one knowledge blob from the new Restic
  snapshot (when any exist) before promoting it.
- If the dump contains knowledge rows but the backup is run without a knowledge
  root, it fails (`knowledge-not-configured`) rather than producing a backup that
  would restore rows pointing at nothing.

**Restore** needs the database from `neev-backups` plus either the content
backup's `blobs-knowledge/` or the `neev-knowledge/` Storage Box replica:

- From a content backup (preferred): `restore-blobs --target <documents>
  --knowledge-target <knowledge>`. Both roots are rebuilt from the same backup,
  every blob is re-hashed against the dump-derived manifest, and both carry
  `.restore-incomplete` until all are done. See
  [neev-content-backup.md](neev-content-backup.md).
- From the replica: copy `neev-knowledge/{org}/` into a fresh 0700 root (owner
  65532), then check each `{uuid}.md` against the restored rows' `sha256` and
  `byte_length`. The replica may hold blobs newer than the database backup, and
  orphans; only the rows decide what is referenced.

**Verify the restore** with `neev-content-verify`
([neev-content-backup.md](neev-content-backup.md), restore step 6), setting
`NEEV_VERIFY_KNOWLEDGE_ROOT` to the restored knowledge root:

- It reads every knowledge row from the restored database and re-reads each blob
  under its own org directory, checking exact length and SHA-256 with the
  65,536-byte bounds.
- Rows without a root fail at `NEEV_VERIFY_STAGE_KNOWLEDGE_ROOT`. A missing or
  altered blob fails at the blob read.
- The summary reports `knowledge_blobs` and `knowledge_bytes`.

This has been exercised on a disposable PostgreSQL 16.15 end-to-end restore
(3 knowledge rows). It has not yet run on the production host. The same check
applies after a restore from the replica.

## Withheld and revoked text

Revoking a source, or withholding a page, changes what the app shows and stages.
It does **not** remove text that was already captured:

- the server root keeps every blob (write-once);
- the `neev-knowledge/` replica keeps every copied blob (no purge in this
  release; remote orphans are not deleted);
- content backups and the `neev-backups` Restic repository keep them for their
  retention (up to about 3 months);
- the box's ZFS snapshots keep them for about 7 days, even after a future purge.

Removing such text requires a future, manual purge feature. Until then, treat
everything ever captured as retained in these copies.
