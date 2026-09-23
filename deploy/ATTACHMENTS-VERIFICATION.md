# Agent Chat attachments — release verification

Date: 2026-09-23. Production: https://app.neev.so/workspace.

## Behavior

Both chat composers accept dropped files, pasted images, and the attachment picker.
Uploads show progress, errors, removal and retry controls. Files are linked atomically
to saved messages/turns and displayed after reload. A file-only send is supported.
The runtime stages real bytes inside the isolated run; Codex images use `--image`,
and staged file paths are supplied to the provider. Existing subscription-based
connections remain the authentication mechanism; no API-key billing path was added.

Supported: PNG, JPEG, WebP, PDF, UTF-8 text/code. Limits: five files, 10 MiB each,
25 MiB per turn, 256 MiB per organization. Unreferenced uploads expire after 24 hours.
Every download checks the current parent conversation permissions.

## Release artifacts

- Runtime: `neev-native-agent:ux-docs-speed-3d010ec6`.
- Database schema: 15 (additive attachment tables/indexes).
- Workspace HTML SHA-256: `d0c28bf08c5b84585404a130e3245274324f7705e6d84a9c7c111b747669a6b4`.
- Compatible UI fallback: `neev-native-agent:attachments-fallback-3d010ec6`.
  This retains the schema-15 server with the previous UI. A schema-14 image is
  **not** a database-compatible rollback after migration. Do not restore the
  pre-release database over new writes without an explicit recovery procedure.
- Pre-release custom-format PostgreSQL backup was verified with `pg_restore --list`.
  Private host metadata: `backups/attachment-backup-latest.json` under the native
  deployment directory. SHA-256:
  `e1bcb2ab2ab277bef9fed26b6fdeef6816a48fa26e4a1d48c5aae4a1bfe43139`.
- Caplaunch binary hash and file capabilities match the preceding release.

## Verification

- Frontend: 206 focused tests passed; TypeScript and lint on changed files passed.
- Static export: Node 22, `next build --webpack`, exit 0.
- Rust attachment filter: 29 passed, two PostgreSQL tests ignored locally.
- Database regressions: 91 passed, 14 isolated-PostgreSQL tests ignored locally.
- Backup regressions: 12 passed, 10 isolated-PostgreSQL tests ignored locally.
- Execution tests: 27 passed, one authenticated-provider test ignored.
- HTTP unit tests: nine content tests and 32 session tests passed; two supervisor
  tests ignored. Real provider completion was not simulated or claimed.
- PostgreSQL canary on the production host: 42 HTTP assertions passed, covering
  upload/read ACLs, permission revocation, invalid input, file-only messages,
  atomic persistence, reload, idempotency and cross-conversation refusal.
- Canary restart with persisted attachment rows: healthy; saved image remained
  visible and decoded in the browser after reload.
- Isolated restored-database verifier: schema 15, 14 blobs, 763 bytes verified;
  manifest hash `723f451095b2fd9c66f708a4b5f832efa02b245a0e4d798f7100cbbff1e3fa0b`.
- Linux real caphelper acceptance: seven checks passed, including dropped-user
  access to staged bytes and separate-run isolation. Disposable credential fixtures
  only; no customer credentials or model invocation.
- Browser: image paste, upload completion, Enter-to-send, persisted image and
  reload verified in the canary. Live upload of a disposable image succeeded;
  removed from the draft without sending a message.
- Integrated branch: `cargo check --no-default-features --features test-utils --tests` passed.
- Combined canary: new documents explicitly reported `engine: lasuite`; document
  frame and API returned 200; another organization was refused. The 42 attachment
  assertions passed again and the combined server restarted successfully.
- Live health returned 200 and deployed HTML matched the release hash.
- Live document creation opened the La Suite editor with its editable title and
  document body. The disposable verification document was moved to recoverable
  trash, and the live chat was left open with Attach files available.

## Review fixes

Fixed the schema-15 startup inventory guard and added repeated-initialization
regressions; classification work cannot cross a conversation switch; changed
prompts/files receive a new retry ID; repeated runtime staging uses separate
subdirectories; Unicode filenames fit filesystem limits; backup verification checks
attachment existence, lengths and hashes. Older migration fixtures now explicitly
remove the attachment layer when constructing historical schemas.

## Known verification limits

Chrome automation file-picker upload was blocked by the extension's file-URL
permission. That permission was not changed. Browser paste and HTTP uploads passed;
drag/drop and picker logic have focused component coverage.

The broad frontend suite reports 61 failures in unrelated conversations/diff/chat/
import/forge areas; two were reproduced alone. Full-tree lint reports five existing
formatting errors in untouched terminal-linkification tests. Neither broad check is
claimed green. Default Turbopack cannot follow this worktree's external node_modules
symlink; webpack export succeeded without replacing shared dependencies.

Codex is still disconnected in the live user's current chat. This release does not
resolve the separate request for a simpler subscription sign-in flow.

## Integration correction

The first attachment server was built from a UI-worker checkout that did not
contain the existing La Suite backend. The mismatch was caught when integrating
the source. The final server was rebuilt from `workos/team-workspace` commit
`3d010ec6`, preserving the document engine and adding attachments. A production
audit found zero document updates during the mismatched server's lifetime.
Future native-server releases must build from the integrated branch, not an
individual UI worker checkout.
