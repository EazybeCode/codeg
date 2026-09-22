# Agent model, effort and speed controls — 2026-09-22

## Behavior

Agent composers have a grouped model menu, a reasoning-effort menu (including High and Very High where supported), and Standard/Fast controls. First-message requests carry the complete selection. Stored chats retain settings, and failed settings writes preserve the prompt. Fast defaults off and never silently changes the selected model.

Codex models are discovered through the pinned CLI's model/list protocol under the caller's provider credentials. The catalogue cache is scoped to user, organization, provider and connection generation. Missing discovery is explained in the menu. Claude models use supported CLI aliases. This is a Conductor-style control surface, not a guarantee that every Conductor model entitlement exists for every connected account.

## Verification before deployment

- Frontend: 144 affected tests passed, TypeScript and changed-file lint passed on Node 22.
- Integrated static export: 34 routes, successful production build at UI commit `9ffef6c1`.
- Backend: 633 tests passed, zero failures, 88 ignored in the focused native harness. The final agent suite including cancellation retention passed 257 tests. Linux release build passed.
- Independent review fixed first-invoke revision accounting, per-model validation on every write path, catalogue/client bounds, duplicate accessibility IDs and discovery-process ownership during cancellation.
- No paid provider turn is needed for metadata or settings acceptance checks.

## Release status

Deployed `neev-native-agent:ux-controls-cb464684` to https://app.neev.so, schema 14. UI commit `9ffef6c1`, backend commit `cb464684`. Verified live workspace SHA-256: `a3568344311a32e2efdb4e9556ccbd0fe94312741fddc71144447e2dcaabdf9b`.

The new server upgrades to schema 14. Image-only rollback is insufficient: stop the sole writer, transactionally drop only the added effort/fast columns, set version 13, and start the previous image. The release procedure takes a database and compose backup first. On-host 14→13→14 rollback/re-upgrade passed with session, turn and message counts preserved, including successful startup of the previous image. API checks passed for capability lists, High→Very High/Fast persistence, stale revisions, invalid combinations, and restoring default effort/Standard speed. Browser candidate checks confirmed grouped providers, first-message controls, Fast automatically turning off for Haiku with an explicit notice, and the draft remaining intact.

Backups: `neev-before-agent-controls-20260922-063856.sql` and `compose.yml.before-agent-controls-20260922-063856`. Rollback image: `neev-native-agent:ux-editor-51874cee`; stop the writer and apply the reviewed schema downgrade before using it.

Provider protocol and metadata were checked with the deployed CLI versions. No paid provider turn was performed in acceptance testing; an authenticated provider run is not claimed by these results.

## Workspace cleanup
Removed 4.42 GiB of obsolete generated artifacts plus 4.0 GiB of completed Rust incremental cache. Mac free space increased from 7.3 GiB to 17 GiB after filesystem accounting settled. Source, active release artifacts, test binaries, and rollback images were retained.

## Live browser check and cleanup
A newly created private Agent Chat opened without a title prompt and showed the model, reasoning and speed controls before the first message. Existing test document content survived the release and reload. Both providers report disconnected for the signed-in live account, so account-authenticated catalogue discovery and a paid turn remain unverified; connect through Personal AI connections in Settings.

The temporary canary review session was revoked, its credential files removed, and the review proxy/tunnel and three task-owned Claude workers stopped. The live browser tab was kept open for review.
