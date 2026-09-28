# Chat file preview and recoverable deletion

## Behaviour

- File references in the chat and generated-file children open a Neev viewer.
  Download is a separate action. Supported previews: plain text, PNG/JPEG/WebP,
  and PDF pages rendered to canvas with PDF.js 6.3.289.
- HTML/XHTML/SVG previews are unavailable. A known organisation-blocked HTML
  download is not fetched through a preview or an alternate delivery channel.
- Delete archives a file; Deleted files lists recoverable entries within the
  conversation, and Restore brings the same attachment back. Blobs and references
  are retained; this does not reclaim storage quota.
- Current conversation permissions protect all reads, archive and restore.
  Archived downloads fail and archived attachments are omitted from later agent
  context and message/turn attachment listings.
- Text previews render up to 2 MiB for responsiveness, without changing upload
  or storage limits. PDFs load only when opened and render one page at a time.

## Implementation

Schema 17 adds a default-active `archived` column and trash-list indexes. Both
SQLite and PostgreSQL receive a forward migration. Restore does not duplicate
storage or references. Archive uses a transaction with a fresh authorization check,
plus existing cookie/origin/CSRF checks at the HTTP boundary.

The UI uses the ordinary authenticated attachment URL. No embedded HTML document,
PDF annotation layer, external viewer service or alternate download is added.
The packaged PDF worker is a same-origin build asset; the server serves `.mjs`
under `_next/static` with the JavaScript MIME type and `nosniff`.

## Verification

- Frontend: 130 integration tests passed; 23 initial focused tests passed and nine
  final preview/action tests passed after the added delete test and loading update.
- Production frontend build and type checking passed. Targeted ESLint passed.
- Independent Claude review checked permission, migration and preview boundaries.
  Its SQLite boolean-literal concern was rejected after verifying that FALSE/TRUE
  evaluate to 0/1 on the supported SQLite runtime; server tests cover those queries.
- PostgreSQL backup verified before deployment: 260052 bytes, SHA256
  `972906fd5c111feb24930606cbea3b8d266f3fc63d9d6268dce76fea803d15bf`.

- Focused Rust suite: 146 passed, 0 failed, 31 ignored. This includes resource
  chat and private-agent session archive routes, SQLite migration/reopen, backup
  round trips, quota preservation, and PDF worker MIME restrictions. Three known
  unrelated legacy rich-export parser failures were excluded; this is not a claim
  that the entire Rust suite passed.
- PostgreSQL schema16 → 17 migration/archive/restore test: 1 passed, 0 failed,
  against an isolated PostgreSQL 16 instance.

## Release

Production UI SHA256:
`acaabfe88db467cb4d7a7c826894aa841b59e2b8f3d39158d3ce204bae3a973a`.

The release uses an additive schema migration. The deployment guard requires no
active sign-ins or running turns and takes a fresh verified database backup.
Automatic rollback may reset the marker to16 only while the service is stopped
and no attachments have been archived; once archive state exists, repair forward
with schema17 rather than silently exposing deleted files through an old binary.

Released `neev-native-agent:file-actions-20260928` to app.neev.so. Live health,
workspace hash, server binary hash and schema17 verified after deployment.
Server SHA256: `b8a38be3570d9d2c00bc2f2bca6931fabcd8e09fcd1898328017464f7afbce42`.

The isolated production clone migrated with all10 attachment rows and10 refs
preserved. New-server restart passed. A guarded marker rollback with no archived
rows allowed the old release to start. The first canary attempt failed because
its temporary blob directory had0755 permissions; changing that test directory
to the required0700 resolved it. No production configuration was changed.

Fresh pre-release backup: `before-file-actions-20260928T062455Z.dump`,262094 bytes,
SHA256 `f6e072139320995c0b75d7ec5b6a8770e9867a24aae1fabcd7cecafa2698b85f`.

Live UI acceptance passed on app.neev.so:
- Text attachment chip opened a dialog containing “Preview verified”.
- Delete closed the dialog and showed Restore; the active sidebar omitted it.
- Deleted files listed the text file; Restore removed it from trash and restored
  the chat attachment chip.
- PDF chip opened a one-page canvas preview visibly displaying “Preview verified”,
  with Download and Delete separate from opening. No download was triggered.
- The disposable verification chat was archived after testing.

Removed the disposable PostgreSQL test container/network and275MB local build
cache. No production data was pruned.
