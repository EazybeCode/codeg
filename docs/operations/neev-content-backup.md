# Native content backup and isolated restore

This procedure backs up the native PostgreSQL metadata and the immutable blobs
it references: document revisions (`.md`), editor images (`.img`) and chat/agent
attachments (`.att`). It does not enable a hosted backup schedule, attach a provider account,
retire an archive, or establish hosted recovery readiness. Keep existing archives
and jobs until a separately reviewed operational recovery exercise succeeds.

## Create a backup

Use Node 22 and a compatible PostgreSQL `pg_dump` on PATH. Run as the blob-store
owner on the machine holding both the native database connection and document
root. All paths must be absolute, normalized, and free of symlink components
(on macOS use `/private/tmp`, not its `/tmp` alias). Source root and organization
directories must already be owned by this user and mode0700. Final blobs must be
owned regular mode0600 files, with one hard link and canonical
`{uuid}.{md|img|att}` names. Any other entry fails the run. The source is trusted
to obey the application's immutable-file protocol; do not run against directories
another process actively replaces. A `pg_restore` matching the `pg_dump` version
must also be on PATH.

Supply the private connection through `NEEV_BACKUP_DATABASE_URL` in the process
environment from your existing secret manager. Never put it in a command argument,
shell history, screenshot, report, or artifact. No application/provider/encryption
key is copied by this script. The metadata dump itself contains private content
and identity/session data and must be treated as sensitive recovery material.

The URL is converted into libpq `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, and
`PGPASSWORD` environment values. Supported URL options are `host`, `port`, `user`,
`password`, `sslmode`, `sslrootcert`, `sslcert`, `sslkey`, `sslcrl`, `sslcrldir`,
`channel_binding`, and `connect_timeout`. Unsupported, duplicate, and conflicting
options are refused. Explicit TLS settings are preserved. Inherited `PG*` and
provider environment variables are stripped; default password files are disabled.

```sh
node scripts/neev-content-backup.mjs \
  --source /absolute/native-documents \
  --output /absolute/backups/new-unique-directory \
  --schema public
```

`--schema` defaults to `public`. It selects exactly one PostgreSQL schema;
accepted identifiers match `[a-z_][a-z0-9_]{0,62}`. It is passed literally to
`pg_dump --strict-names --schema`, with custom format, no owner and no privileges.
Use the exact owned UUID fixture schema for shared local test databases. A
connection's `search_path` alone does **not** limit a dump to that schema.

The output parent must already exist. The script exclusively creates a new0700
output; existing paths are refused, including interrupted runs. Metadata is dumped
first into `metadata.dump`.

The script then reads the blob references **from that dump**, not from the live
database. `pg_restore --list` finds the schema's `TABLE DATA` entries, and
`pg_restore --data-only --file=-` emits the rows of `neev_document_revisions`,
`neev_image_blobs` and `neev_attachments` (whichever the snapshot's schema
version has). `pg_restore` reads only the local archive and receives no
connection settings. Each row's org, UUID key, SHA-256 and byte length must be
well formed. Per-kind bounds match the runtime reads that the restore verifier
uses: documents up to1MiB and images up to5MiB. Attachments have no fixed cap;
the exact row length is their bound.

Every referenced blob is then streamed into `blobs/{org}/{key}.{kind}`. The copy
never reads past the row's length, and the copied file is re-read from disk. Both
the source and the copy must match the row's length and hash. The run fails, and
leaves no complete marker, on any of the following. Each prints a reason code and
nothing else:

- a mismatch (`size-mismatch`, `hash-mismatch`) or a missing referenced blob
  (`missing-blob`);
- a malformed or conflicting row (`snapshot-row`, `snapshot-size`,
  `snapshot-conflict`);
- a table listed without data, or an unparseable listing (`snapshot-copy`,
  `snapshot-list`);
- `.img`/`.att` bytes on disk while the snapshot lacks their table
  (`snapshot-tables`).

Blobs are read with `O_NOATIME` (Linux), so a backup never looks like use to the
Storage Box evictor. If `NEEV_ORIGINALS_BROKER_SOCKET` is set, a referenced blob
that was evicted from the live root is streamed from the root-side originals
broker directly into `blobs/`. It is length-bounded, hashed and re-read against
the row, and it is never republished into the live root. Without the broker, or
if it refuses (never replicated, deleted and tombstoned, or remote mismatch), the
run fails with `missing-blob`. See [neev-storage-box.md](neev-storage-box.md).

Well-formed finals that have no snapshot row (uploads committed after the dump)
are not part of this recovery point. They are counted as `unreferenced_skipped`
and not copied. If a 24-hour orphan sweep deletes a dumped row's blob during the
run, the run fails with `missing-blob`; retry into a new output. `.tmp` files are
excluded. No source mutation, garbage collection, or historical deletion occurs.

`complete.json` is the completion marker. Format `neev-content-copy-v2` contains
the dump hash and size, the blob tables found, and an org/key/kind/hash/size
inventory, with no Markdown, emails, file names, or auth secrets. `INCOMPLETE` is
created at the start and remains as a warning that absence of `complete.json`
means failure. On failure, keep the incomplete directory for diagnosis and retry
with a new output path. Do not accept a backup on exit status or an inventory
alone if its complete marker is absent. Earlier `neev-content-copy-v1` archives
hold documents only; the old script refused roots containing images or
attachments.

## Restore into an isolated destination

1. Verify the complete marker and the SHA-256/byte length of `metadata.dump`
   against `complete.json` before importing. Preserve the original backup.
2. Provision a fresh isolated PostgreSQL database named
   `neev_restore_<32 lowercase UUID hex digits>`. Restore the original selected
   schema into that database. A shared disposable database is also supported if
   the restored schema itself has that `neev_restore_<32hex>` form. `pg_restore`
   does not remap schema names; prefer a new database when backing up an existing
   differently named schema. Do not use `--clean`, restore over the source, or
   silently import into a different database/schema than the one being verified.
3. Import using `pg_restore --dbname RESTORE_DATABASE_NAME --exit-on-error --no-owner --no-privileges metadata.dump`, with the
   restored connection supplied through separate `PGHOST`, `PGPORT`, `PGDATABASE`
   (database name only), `PGUSER`, and any password/TLS settings in its environment.
   A URL assigned directly to `PGDATABASE` is not expanded by libpq.
   `--dbname` contains only the isolated database name and is required to perform
   an import; without it, pg_restore defaults to emitting SQL instead.
   Do not start the application or call its database initializer/migrator.
4. Rebuild a fresh document root, running as the user that will own it (the
   application user):

   ```sh
   node scripts/neev-content-backup.mjs restore-blobs \
     --backup /absolute/backups/that-directory \
     --target /absolute/new-document-root
   ```

   This refuses a missing/unknown `complete.json`, a dump whose hash or size
   differs, and an existing target. It creates0700 org directories and0600
   single-link files, and re-hashes every v1/v2 manifest blob while copying. A
   `.restore-incomplete` marker stays in the target until every file and
   directory is synced. The backup and Storage Box tools refuse a root that still
   has the marker; discard such a target. Do not use `cp -l` or copy `blobs/` by
   hand.
5. Build the named native binary without desktop features:

   ```sh
   cd src-tauri
   CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
     cargo build --no-default-features --bin neev-content-verify
   ```

6. Supply `NEEV_VERIFY_DATABASE_URL` and `NEEV_VERIFY_DOCUMENT_ROOT` through the
   environment. The verifier never falls back to live application variables.
   Pass the actual database name and restored schema explicitly:

   ```sh
   target/debug/neev-content-verify \
     --database neev_restore_0123456789abcdef0123456789abcdef \
     --schema public --confirm-isolated
   ```

The verifier checks the selected schema and connected database against both
arguments. At least one name must satisfy the isolated naming rule. A mismatch,
missing confirmation, unsupported URL type, legacy/unknown/partial/mixed schema,
wrong schema marker, missing blob, cross-organization lookup, or size/hash failure
causes a nonzero exit. It does not initialize/migrate the database, acquire a
write-intent/admission lock, or start providers/server services.

On Unix the verifier replaces its process with the same executable in a cleared
environment containing only the dedicated URL/root and an internal invocation
marker. The restarted process checks this environment contract before constructing
connection options. On macOS only, it also tolerates the single
`__CF_USER_TEXT_ENCODING` key added by CoreFoundation during startup; that key is
never forwarded or used for connection settings. The marker conveys no
authorization. Inherited PostgreSQL
settings, `HOME`, and password files cannot supply connection authority or secrets;
the connection uses SQLx's no-password-file constructor and an explicitly built
pool, without its URL parser or SeaORM's URL connection path. There is no child
process left behind when verification ends.

The native verifier URL must explicitly supply a host, user, and database, either
in the URL authority/path or using `host` and `user` query parameters. The local
Unix-socket form `postgresql:///DATABASE?host=/absolute/socket-directory&port=PORT&user=USER`
is supported. A missing port means 5432; a missing password means no password.
Supported native URL options are only `host`, `port`, `user`, `password`, `sslmode`,
`sslrootcert`, `sslcert`, and `sslkey`. The native list is narrower than the backup
script's libpq list: unsupported options fail before connecting. Duplicate or
conflicting values and nonliteral database paths are refused. Percent decoding
preserves literal `+` characters. Explicit TLS settings are retained; the default
mode is `prefer`. Native `sslmode=allow` is refused because the installed SQLx
version does not implement its TLS fallback; use a supported explicit mode.
SQLx applies TLS negotiation to Unix sockets as well as TCP connections. The
checked `--schema` supplies the only startup search path.

It requires actual ordinary native tables (not same-name views, foreign tables,
or partitioned relations), and accepts exactly four snapshot contracts: historical
schema3 and schema4 with the base/content table inventory, schema5 with
the base/content/personal-agent metadata inventory, and schema6 with that inventory
plus `neev_document_formats`. Schema6 requires exactly one valid format/version row
for every document revision, with no orphan rows. It reads all historical revision
references in a read-only snapshot transaction, then verifies each file through
the same no-follow BlobStore API used by native content. Extra copied orphan
files and temporary files do not affect reference verification. The deterministic
referenced-key manifest contains only org/key/hash/size; stdout emits its SHA-256,
schema version, reference count, and total bytes, not the inventory or body data.
The reported version is the snapshot's actual marker. Historical schema3, schema4, and schema5
restores remain verifiable without initialization or migration. Application startup
upgrades exact schema3 through transactional source-link hardening and personal-agent
metadata, then backfills document format metadata as schema6; exact schema4 and schema5
follow the remaining forward migrations. Existing revisions are tagged `markdown` version1
without changing their bytes, hashes, quotas, or receipts. Verification never migrates an archive.

Rich revisions store a validated `blocks` version1 JSON envelope, not Markdown.
The verifier checks each rich envelope after its immutable byte length and SHA-256,
including supported fields, IDs, depth, tables, links, and the1MiB stored/export limits.
Markdown history keeps its existing byte validation; JSON-looking Markdown is never
guessed to be rich content. Restoring either format appends a new revision with the
selected revision's exact stored bytes and format metadata. Unsupported or corrupt
rich envelopes fail verification and restore even when their stored hash matches.

Acceptance is successful only after the complete dump import and verifier both
succeed against the named isolated destination and fresh root. Record the
sanitized summary, tool versions and owned resource identifiers. The controller
performs the actual local restore exercise separately from unit/integration tests.

## Recovery custody

Keep application encryption keys and any provider credentials in separate recovery
custody with independently controlled access. Do not bundle them with this
artifact or print them in recovery evidence. Hashes detect byte mismatch; they
are not signatures, encryption, or proof of trustworthy custody. Transfer and
retention policy, encryption at rest, external storage access, credential rotation,
and hosted disaster-recovery readiness require their own operational review.
