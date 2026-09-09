# Private-pilot backups

Configured and exercised on 2026-09-08 for the WorkOS Coolify application and its PostgreSQL database. These jobs run in the cloud; the laptop and SSH preview tunnel are not required.

## Current coverage

| Data | UTC schedule | Retention | Backup behavior |
| --- | --- | --- | --- |
| PostgreSQL `workos` | 00:17 daily | Seven local dumps | Logical custom-format `pg_dump` |
| Application `/data` | 01:00 daily | Seven local archives | Stop application while archiving, then restart |
| Claude `/root/.claude` | 02:00 daily | Seven local archives | Stop application while archiving, then restart |
| Codex `/root/.codex` | 03:00 daily | Seven local archives | Stop application while archiving, then restart |
| Encrypted off-server copy | 04:30 daily (+0–2 min) | 7 daily / 4 weekly / 3 monthly verified snapshots | Restic SFTP upload, integrity check, promotion, scoped retention |

Volume jobs have a 30-minute timeout and separate hourly windows. The private pilot can briefly become unavailable during each volume archive. Do not manually trigger these backups while a deployment or agent run is active, and run manual volume backups sequentially. Before enabling durable team jobs, coordinate backups with job draining and a coherent database/files recovery point; these independent native schedules are not an atomic snapshot of an actively running job.

Archives live under the WorkOS team's directories in `/data/coolify/backups/databases/` and `/data/coolify/backups/volumes/`. The WorkOS team directories are root-owned with mode `0700`; verify that remains true after backup-system changes. Provider archives can contain credentials and transcripts. Do not upload them to Git, attach them to tickets, or give collaborators general access to them.

Other providers require their own verified persistent profile/storage coverage before use. Showing an agent in the catalog does not mean its account is connected or its native state is backed up. The Claude/Codex homes were unconnected at the initial rehearsal.

## Verification performed

- A fresh database backup completed and restored into a newly created, isolated temporary database: 50 migrations, one user and one organization were verified. The live database was not overwritten. Only the temporary restored database was removed; the dump was retained.
- A scheduled probe at 06:05 UTC ran without a manual trigger and succeeded. The schedule was then restored to explicit `0 0 * * *`. The earlier unproven `daily` alias configuration is no longer used.
- All three volume archives were created, checked with `tar -tzf`, and extracted into a private temporary directory. Only the temporary extracted copies were removed. The archives and live volumes were preserved, and the app returned to healthy after each backup.
- The application stays private with catalog agent execution disabled. This rehearsal is not a real-provider or multi-user execution test.

On 2026-09-09 the off-server rehearsal additionally downloaded snapshot `941b108f` using the independent recovery-password copy, verified all four files, read all encrypted data packs with no errors, restored the database into a separate network-isolated PostgreSQL 16 container (50 migrations, one user, one org), and extracted all three volume archives. The temporary database container was removed; live resources and backup archives were retained. The separate application encryption-key copy was compared equal to the live key without exposing either value; a full restored-application/provider login exercise is still a separate release gate. An intentionally truncated database dump copy was rejected by the host validator.

The transport's 21 tests cover source completeness/freshness, source mutation, duplicate coverage, failed uploads/checks, missing snapshot IDs and quarantine across successive runs. All passed, and independent review accepted the final safety fixes with no open findings. Source/deployed script checksums matched before the final scheduled service probe. This evidence does not certify tenant isolation or agent execution in the application.

The final script was launched automatically by a transient server timer at 02:57:35 UTC, completed at 03:01:28 UTC with exit 0, and promoted snapshot `71bf128b` only after validation and integrity passed. Peak memory was 50.7 MB. The final recurring timer is enabled for 04:30 UTC (10:00 IST) with up to two minutes of randomized delay; the restore-verification gate is installed. A real retention dry run accepted the 7/4/3 policy and kept both current snapshots. No backup snapshot or native archive was removed during setup. The next actual daily-window run and future failures still require operator monitoring; one probe is not perpetual proof of reliability.

## Encrypted off-server extension

The owner approved a Hetzner Storage Box and encrypted Restic SFTP backups on 2026-09-09. SSH support on port 23 and a dedicated backup key were tested from the app host. The host key was checked against [Hetzner's published fingerprints](https://docs.hetzner.com/storage/storage-box/general/), then pinned with strict checking; no agent forwarding or admin private-key copy to the server is used.

The extension is `scripts/operations/workos_backup.py`, with the adjacent systemd service/timer. It uploads only the latest archive in each of four explicitly configured, distinct WorkOS directories. Missing, empty, symlinked, duplicate/hardlinked, younger-than-one-minute, or older-than-eight-hour archives fail the run. Before uploading, `pg_restore --file=/dev/null` reads the complete database dump without connecting to a database; `gzip -t` and `tar -tzf` validate volume archives without extraction or printing their contents. Device/inode/size/mtime/ctime must remain stable through validation plus a two-second interval and again after upload; changed sources cannot trigger integrity/retention or refresh the success record. At the nightly 04:30 UTC window (up to two minutes randomized delay), all four native archive windows should have completed. No live volume is read by this transport job, and the job does not stop the application. A host-local lock prevents overlapping service runs.

The first installation uses distribution-packaged Restic 0.18.1. Configuration and a random encryption password are root-only under `/etc/workos-backup`; script/units are under `/opt/workos/backup`; cache/state are under `/var/cache/workos-backup` and `/var/lib/workos-backup`. These paths and the backup SSH key must never be mounted into agent containers. The service has memory/CPU limits, no new privileges, and a read-only host filesystem except its state/cache/runtime directories. Do not give collaborators root access or the backup account.

Every upload starts with a `workos-pending` quarantine tag. The script captures its exact snapshot ID from Restic's JSON summary, checks post-upload source stability, and requires a repository metadata integrity check before promoting that exact snapshot to `workos-daily`. Failed/unverified uploads cannot satisfy a later daily/weekly/monthly retention slot. Pending snapshots are retained for operator investigation, not silently removed; monitor them and storage usage. Retention is seven daily, four weekly and three monthly snapshots (overlapping categories, not fourteen guaranteed distinct snapshots), restricted to the configured host and `workos-daily` tag. Retention/pruning is disabled until `/var/lib/workos-backup/restore-verified` contains the exact configured repository string following a successful restore rehearsal. A failed source check, upload, integrity check or promotion does not run pruning or refresh `last-success.json`. The initial repository must be explicitly initialized; failed authentication must never silently create a new repository.

Restic encryption protects stored archive contents; SSH transport alone would not. The write-capable backup credential is **not immutable/ransomware-proof storage**: a compromised host-root account could delete remote backups. Storage Box snapshots or independent immutable copies would be an additional protection, not part of this verified boundary. No S3-compatible target is configured.

A separate recovery copy of the Restic password and dedicated SSH key was saved outside Git on the owner's Mac in the private WorkOS configuration directory. Move/copy that material and the existing application encryption/OAuth/deployment recovery secrets into an approved password manager or offline vault. Losing the Restic password loses access to the encrypted repository. Losing the separate stable application encryption key prevents decrypting database credential columns even when the database can be restored. Never store the only recovery-key copy on the app host or in the encrypted repository it unlocks.

### Database schedule investigation (2026-09-09)

The expected midnight database execution was absent from both the Coolify API and host filesystem, while the application and Claude native volume jobs ran. The existing database schedule was temporarily set to `40 2 * * *` without `backup_now`; it ran successfully at 02:40:34 UTC and produced a 105630-byte dump. Its final daily schedule is now `17 0 * * *`, retaining seven local dumps. No duplicate database schedule was added. This avoids the missed midnight slot but does **not** establish the upstream cause or prove the next overnight execution; inspect the next 00:17 UTC record. Off-server source freshness will reject a missed window instead of uploading yesterday's dump as current.

### Host operator commands

Run as the authorized host-root operator. Initialization, restore, and retention-marker changes are explicit operator actions, not unattended fallback behavior.

Provision a new/recovery host before installing or starting the service: install Python 3, Restic, the PostgreSQL client tools (a `pg_restore` version supporting the producer's archive format), gzip, tar and flock. The initial host uses the distribution PostgreSQL 18 client to validate PostgreSQL 16 dumps; its live database server remains PostgreSQL 16. Create the writable directories **before** systemd applies `ProtectSystem=strict`/`ReadWritePaths`:

```bash
install -d -m 700 /opt/workos/backup /etc/workos-backup /var/cache/workos-backup /var/lib/workos-backup
```

Install the script, exact resource-specific configuration, private SSH settings and encryption password with root-only access; install the service/timer under `/etc/systemd/system` and run `systemctl daemon-reload`. Never overwrite existing recovery secrets to repair a login failure. Directory provisioning and an actual hardened-service start were verified during initial installation; syntax validation alone is insufficient. Enable the timer only after the isolated restore rehearsal passes.

```bash
python3 /opt/workos/backup/workos_backup.py sources
python3 /opt/workos/backup/workos_backup.py dry-run
systemctl start workos-backup.service
systemctl status workos-backup.service workos-backup.timer
journalctl -u workos-backup.service --since yesterday
python3 /opt/workos/backup/workos_backup.py restic snapshots
```

`systemctl` must show the service finished successfully, not merely `activating` or a queued start. Inspect both the systemd result and timestamp in `/var/lib/workos-backup/last-success.json`; an old success file does not prove today's run succeeded. A failed unit is visible in systemd/journal, but no email/Slack delivery destination has been configured. Arrange an operator check or connect an approved monitoring destination. Do not run repository maintenance concurrently with the service or remove a live Restic lock.

For recovery, stop concurrent backup/maintenance jobs, select an exact snapshot ID and restore into a newly created private empty directory with `restic restore SNAPSHOT --target DIRECTORY --verify`. Use a separate isolated PostgreSQL container/database and empty test volume directories. A rehearsal must validate downloaded bytes, archive extraction, schema/user/org counts and recovery-password usability before enabling retention. Do not automatically run restored agents or replay prompts.

## Operator checks and restore order

1. Inspect Coolify's backup execution records and actual archive sizes after each scheduled window. A configured schedule or queued job is not proof of a successful archive. Investigate a failed/missed job before relying on the next window.
2. Keep track of the matching source commit/image, database dump, application archive, provider archives and recovery secrets. Source in GitHub does not include unpublished worktrees, attachments or database rows.
3. Restore to an isolated database and empty temporary volumes first, never over the live resources. Validate archive structure, migration/user/org counts, representative files, permissions and the ability to decrypt the required application secrets without logging their values.
4. Run the restored application privately with outbound agent execution disabled. Check organization identity, catalog structure, chat history and file mappings. Connecting a real provider and resuming jobs require explicit authorization and current access checks; do not automatically replay interrupted prompts.
5. Only after the rehearsal passes should an operator plan a live recovery window. Preserve the old deployment/data until the replacement is verified. Record outcomes and remove only the temporary recovery copies, not the backup archives.
