# Private-pilot backups

Configured and exercised on 2026-09-08 for the WorkOS Coolify application and its PostgreSQL database. These jobs run in the cloud; the laptop and SSH preview tunnel are not required.

## Current coverage

| Data | UTC schedule | Retention | Backup behavior |
| --- | --- | --- | --- |
| PostgreSQL `workos` | 00:00 daily | Seven local dumps | Logical custom-format `pg_dump` |
| Application `/data` | 01:00 daily | Seven local archives | Stop application while archiving, then restart |
| Claude `/root/.claude` | 02:00 daily | Seven local archives | Stop application while archiving, then restart |
| Codex `/root/.codex` | 03:00 daily | Seven local archives | Stop application while archiving, then restart |

Volume jobs have a 30-minute timeout and separate hourly windows. The private pilot can briefly become unavailable during each volume archive. Do not manually trigger these backups while a deployment or agent run is active, and run manual volume backups sequentially. Before enabling durable team jobs, coordinate backups with job draining and a coherent database/files recovery point; these independent native schedules are not an atomic snapshot of an actively running job.

Archives live under the WorkOS team's directories in `/data/coolify/backups/databases/` and `/data/coolify/backups/volumes/`. The WorkOS team directories are root-owned with mode `0700`; verify that remains true after backup-system changes. Provider archives can contain credentials and transcripts. Do not upload them to Git, attach them to tickets, or give collaborators general access to them.

Other providers require their own verified persistent profile/storage coverage before use. Showing an agent in the catalog does not mean its account is connected or its native state is backed up. The Claude/Codex homes were unconnected at the initial rehearsal.

## Verification performed

- A fresh database backup completed and restored into a newly created, isolated temporary database: 50 migrations, one user and one organization were verified. The live database was not overwritten. Only the temporary restored database was removed; the dump was retained.
- A scheduled probe at 06:05 UTC ran without a manual trigger and succeeded. The schedule was then restored to explicit `0 0 * * *`. The earlier unproven `daily` alias configuration is no longer used.
- All three volume archives were created, checked with `tar -tzf`, and extracted into a private temporary directory. Only the temporary extracted copies were removed. The archives and live volumes were preserved, and the app returned to healthy after each backup.
- The application stays private with catalog agent execution disabled. This rehearsal is not a real-provider or multi-user execution test.

## Missing disaster recovery boundary

On 2026-09-09 the owner selected a Hetzner Storage Box for off-server backups. Destination creation and its hostname/username are still pending. A dedicated backup SSH key was generated on the app host at `/root/.ssh/workos-storagebox-ed25519`; only its public key was shared for adding during Storage Box creation. This is preparation, not a completed remote backup. The backup key must never enter an agent container or a Git repository. Use SSH support on port 23, keep unused protocols disabled, and initially limit reachability to the Hetzner network. See [Hetzner's creation guide](https://docs.hetzner.com/storage/storage-box/getting-started/creating-a-storage-box/).

**All current data backups are on the same server as the live data. They do not protect against loss of that server.** No S3-compatible destination has been configured. An independently authorized destination, restricted credentials, encryption at rest, off-server retention and a download/restore rehearsal remain required.

Use a dedicated encrypted bucket or an encrypted backup repository. Do not assume HTTPS transport means archive contents are encrypted at rest. Retain the stable application encryption key and required OAuth/deployment recovery secrets separately in an approved secret store; encrypted database credential columns are unusable without their key.

## Operator checks and restore order

1. Inspect Coolify's backup execution records and actual archive sizes after each scheduled window. A configured schedule or queued job is not proof of a successful archive. Investigate a failed/missed job before relying on the next window.
2. Keep track of the matching source commit/image, database dump, application archive, provider archives and recovery secrets. Source in GitHub does not include unpublished worktrees, attachments or database rows.
3. Restore to an isolated database and empty temporary volumes first, never over the live resources. Validate archive structure, migration/user/org counts, representative files, permissions and the ability to decrypt the required application secrets without logging their values.
4. Run the restored application privately with outbound agent execution disabled. Check organization identity, catalog structure, chat history and file mappings. Connecting a real provider and resuming jobs require explicit authorization and current access checks; do not automatically replay interrupted prompts.
5. Only after the rehearsal passes should an operator plan a live recovery window. Preserve the old deployment/data until the replacement is verified. Record outcomes and remove only the temporary recovery copies, not the backup archives.
