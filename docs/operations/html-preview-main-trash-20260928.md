# HTML preview and main Trash

## Behavior

HTML attachments that the user confirmed open normally in Chrome now have a
static preview inside Neev. Clicking the attachment card opens the viewer;
Download remains a separate action. Scripts, forms, navigation and external
resources are disabled. Inline CSS, inline SVG graphics and embedded raster
images remain visible. The entire HTML is read rather than truncating at the
2 MiB text-preview bound, which would corrupt the user's 21.6 MiB page.

Deleted attachments disappear from chat chips and child-file lists. The existing
main Show Trash control still reveals archived resources and agent sessions, and
now includes an attachment section with Restore. This preserves the original
chat/document restore flow. No schema change or permanent deletion is added.

## Isolation and permissions

HTML is parsed in an inert template, stripped of active/navigating elements and
attributes, then inserted into an iframe with an empty sandbox attribute. A CSP
placed ahead of user content allows only inline styling and embedded images;
there is no script permission, same-origin permission, or external fetch access.
The application's parent CSP stays unchanged. Reference:
[MDN srcdoc isolation](https://developer.mozilla.org/en-US/docs/Web/API/HTMLIFrameElement/srcdoc).

The new organization trash endpoint rechecks membership within a transaction,
then applies the existing parent authorization to every result. Other members'
private chats and private agent sessions are excluded. Stored MIME metadata is
used rather than guessing content type from the filename. Pagination advances
through scanned rows, including rows excluded by access checks.

The first Claude draft required corrections during root review: the shared-trash
query needed parent ACL checks; HTML needed full-file reads and stronger inert
sanitization; existing resource Trash controls needed preserving; locale objects
needed preserving rather than conversion to arrays. None of that draft was
published.

## Verification and release

- Full 22 MiB preview, active-content removal, and inline styling/embedded raster
  preservation tests passed (3/3).
- Focused Rust tests:147 passed,0 failed,31 ignored. Existing three unrelated
  rich-export parser tests were excluded as in the previous release.
- Frontend regression tests:65 passed across workspace, attachment chips,
  child-file lists, shared Trash and API parsing. Five final attachment-action
  tests passed after restoring a meaningful delete-from-preview assertion.
- Targeted ESLint passed. Initial concurrent frontend runs hit laptop-load
  timeouts; the single-worker regression run passed.
- Root review caught an additional shared-Trash response parser incompatibility:
  parent metadata must be removed before the strict base attachment validator.
  A positive endpoint-response test now covers that.
- Production frontend build/type checking passed. Release deployed as
  `neev-native-agent:html-trash-20260928`, schema17 unchanged.
- Live health and release hashes verified:
  UI `6deef67a06a3f24cb4ff11e79ca267d29ad5764d46d32a2f64ef172dccf52cca`;
  server `2f779c1ae3b7ced9c37471d4cabecce001273cc3d4f54908e75c0c0c4937850a`.
- No active sign-ins or running turns at deployment. Verified262927-byte backup
  `before-html-trash-20260928T100558Z.dump`, SHA256
  `3b253632f8ff61f12e519a010c439eac740e1880b31e71fe0a457523af75e842`.
- Live UI acceptance passed: V10621.6 MiB HTML rendered inside the static
  preview without triggering Download. Its visual layout was visible. JavaScript
  generated portions remain absent by design; fully interactive rendering is
  outside this static preview.
- Deleted the disposable text fixture from chat, verified it appeared in the main
  Trash with the user's existing deleted files, restored it there, and verified
  its chat card returned. Existing user-deleted files were not altered. Re-archived
  the disposable test chat and closed the dedicated review tab.
- Removed generated local build cache and scoped remote build-transfer directories
  after deployment. Production data, backups, and rollback images retained.
