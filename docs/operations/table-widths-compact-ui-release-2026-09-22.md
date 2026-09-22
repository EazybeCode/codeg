# Table column widths and compact UI — 2026-09-22

## Behavior

Dragging a table column border used to make every later save of that document
fail. The codec refused any width that had actually been set, and the reader was
told their content contained unsupported formatting, files, HTML or exceeded
document limits — none of which they had done. The document silently stopped
autosaving. Widths are now carried: one optional number per column, rounded and
bounded to something a column could be. A table nobody has resized stores no
widths at all, so every existing document is byte-identical.

Alongside it, three things the shell was spending space or truth on. A tab now
takes its icon from the resource's real kind, so a team chat stops wearing a
document page in the strip while wearing a speech bubble in the sidebar. The
document header is one row — the title, with audience, save state and actions at
its right — instead of a standing band that existed to print the word "Private",
a gap, and then the title. The agent composer is one slim row of Model,
Reasoning and Standard/Fast; the paragraphs that used to stand under it are now
the chips' own tooltips and accessible descriptions.

Below `sm` the audience words give way to a lock or people icon rather than
disappearing: a shared conversation must never look like a private one.

Fast still defaults off, still states its cost before use, and still never turns
itself on or changes the selected model to become possible.

## Storage

The stored key is `widths` on a table block, `Option<Vec<Option<u32>>>`, inside
the existing rich-document JSON. `columnWidths` is BlockNote's in-memory name
and is never persisted. There is no migration: schema is 14 before and after.

That is also the rollback constraint. An image older than the table build
**rejects** a document carrying widths, so once a real resize is saved,
`neev-native-agent:ux-controls-cb464684` stops being a usable rollback target.
`neev-native-agent:ux-table-55408df5` was built, canaried and retained for
exactly this purpose and is the compatible fallback from here on.

## Verification before deployment

- Backend: 15 rich tests passed, 2 ignored, including
  `rich_keeps_a_resized_column_width_and_bounds_it`. Independently confirmed.
- Frontend: TypeScript clean and 76 tests green on the integrated tree on Node
  22 — rich editor 38, agent controls 16, workspace 22 — proving the two
  workers' changes hold together. The cherry-pick was clean; they touched
  disjoint files.
- Table-only image, browser: a 3x3 table inserted in canary document
  `577ba525-fa13-4570-8ea8-78fd62036609`, text typed, column border dragged
  417→476, saved with no error, and reload preserved both width and text.
- Candidate, browser: document header one row; chat tab speech bubble versus
  document page; conversation header one row; compact Model / Reasoning /
  Standard-Fast controls with no standing helper paragraphs. Checked at
  375x812, where the privacy lock stays visible and the controls stay on screen.
- Live, after deployment: the new header, tab icons and compact controls were
  confirmed on https://app.neev.so, and a column resize there reached Saved with
  no error. This is the check that matters most, because it is the only one run
  against production data and the real proxy rather than a canary.
- Live reload after deployment: the resized column kept its width and its text,
  with no error. That closes the loop the original bug opened — a resize that
  saved, survived a reload, and never told the reader their document was
  unsupported.
- Rollback rehearsed against real width data: with the resized document present,
  the canary was rolled from the candidate back to `ux-table-55408df5`. It
  served, schema stayed 14, and every row count was unchanged. No migration is
  undone because none is applied, and no path strips a width.

## Release status

Deployed `neev-native-agent:ux-compact-90d1da2a` to https://app.neev.so, schema
14. UI commit `90d1da2a`, table commit `55408df5`, backend builder
`neev-build:table-55408df5`. Verified live workspace SHA-256:
`62c9a871378d30dd5157a15485b57e49c8828b1fc7d1d4396214af9835e4f71a`, confirmed
from outside the host.

Production data after the swap: schema 14, 8 agent sessions, 13 turns, 9
messages, 29 document revisions — unchanged across the swap. Pre-swap `pg_dump`
at `/opt/neev-db-backups/neev-before-table-20260922-074140.sql` (135,975 bytes);
previous compose retained as `compose.yml.before-table-20260922-074140`.

Rollback target from here: `neev-native-agent:ux-table-55408df5`.

A production resize has now been saved, so `neev-native-agent:ux-controls-cb464684`
is retired as a rollback target as of this release rather than in principle: it
would reject the document that resize produced.

## Teardown

The temporary review session is revoked and the canary is gone: the
`neev-canary` and `neev-canary-relay` containers were removed, and
`neev-canary-pg` was removed **with its volume**. That volume was anonymous and
used by nothing else, so the whole throwaway canary database went with it —
including review fixture `577ba525-fa13-4570-8ea8-78fd62036609`. Removing it
that way needed no SQL and no guessed identifiers, and could not reach
production. Ports 3100 and 3102 no longer listen, so the proxy and the tunnel
are stopped.

Production, its fallback image and the pre-swap backup are all untouched by any
of it.

## Release tooling

`/tmp/neev-controls-release.py` could not be reused. It pins the outgoing image
to `ux-editor-51874cee` and aborts on anything else, so it could not run against
today's production at all; and its failure path runs the schema-13 downgrade,
which drops `effort` and `fast` — destructive, and unrelated to any failure this
release could have. `/tmp/neev-table-release.py` takes the outgoing image as an
argument, never runs a downgrade, asserts schema 14 before and after, compares
row counts, and on failure restores the image only, stopping rather than
improvising if the schema has moved.
