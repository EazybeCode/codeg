# Neev agent release — 21 September 2026

## Verified deployment

- Public app: https://app.neev.so/workspace
- Source remote/branch: `workos` / `workos/team-workspace` (`EazybeCode/neev`).
- Backend/schema 12: `e1ad5748`; Projects UI: `d2d7c3a2`; runtime directory ownership fix: `7ec039de`.
- Release image: `neev-native-agent:projects-final-d2d7c3a2`.
- Live workspace SHA-256: `47f0110e44a93952736b4db4155cdabbecd24417bf5be491c5dd5ae9057e3813`.
- Deployed 21 September 2026 at 06:28 UTC; live schema marker verified as 12.
- Live compose: `/opt/neev-native-https-20260913/compose.yml` on the existing host.
- Keep the service on `neev-native-private-20260913`, address `10.0.2.4`; Caddy uses this address.
- Keep PostgreSQL certificate verification and its certificate bind mount.
- Docmost remains unchanged.

The container starts `neev_agent_container_init.py` as root. It starts the supervisor, then drops the web server to UID/GID 65532 with no effective, permitted, inheritable, or ambient capabilities. Provider processes run in isolated lanes; proxy processes drop all capability sets.

The tested profile uses a read-only root filesystem, private `/run` and `/tmp` tmpfs mounts, Docker's default seccomp policy, the bundled `neev-agent` AppArmor profile, and these nine capabilities: `CHOWN`, `DAC_OVERRIDE`, `KILL`, `SETGID`, `SETUID`, `SETPCAP`, `NET_ADMIN`, `SYS_ADMIN`, `MKNOD`. Use the same capabilities on the fixed launch helper. AppArmor must permit receiving runtime stop signals from the host's actual `runc` profile. Do not use the host network for an agent-enabled container.

Persistent host paths are `/var/lib/neev-native/agent-supervisor`, `/etc/neev-native/agent-keys`, `/var/lib/neev-native/agent-coordinator`, and `/var/lib/neev-native/agents`. Provisioning preserves existing keys and epochs. Never copy developer-machine provider credentials into this deployment.

## Acceptance evidence

- Both pinned provider CLIs produced real authorization challenges through the native HTTP API; cancellation and credential cleanup passed.
- Claude's approval link and code-entry controls were verified in the live browser after correcting the `authorizationCode` wire spelling.
- The UI retains pending sign-ins and Cancel after polling errors; Refresh retries. A connecting row without a local attempt offers Disconnect for recovery.
- Runtime stop-signal acceptance passed with a graceful exit in 0.05 seconds.
- Full-page documents/chats, nested-folder New chat, rich content persistence, organization joining, and team message delivery were tested on the same-host canary.
- Initial JavaScript: 27 files, approximately 442 KiB compressed, versus approximately 1.39 MiB before lazy loading.
- Real Claude authorization and connection Test passed. The production cap-helper path, running from a web-owned directory as UID 65532, returned the exact requested text “Neev connection works” with exit code 0.
- The live browser chat check subsequently passed: Claude returned the exact requested text through the normal web UI, including the new first-message creation flow.

- Final runtime suite: 518 passed, 3 skipped; focused directory ownership tests: 18 passed.
- Projects UI checks: 72 focused tests, then 57 landing/client tests; final production build passed.
- Same-host PostgreSQL schema 11→12 upgrade, session create/reload, project move, stale-revision rejection, invalid destination rejection, private-owner access and restart persistence passed.
- Real restart acceptance passed: committed credential sentinel preserved, interrupted login cleaned, fresh login and cancellation succeeded, no fixture mapping/store/loop leaks. Log: `/tmp/neev-agent-restart-acceptance.log` on the host.
- Final combined image passed Claude and Codex native HTTP approval/cancel checks. Its actual production init reaped an adopted child in the same-host canary.

## Backups and rollback

Pre-agent database backup: `/opt/neev-db-backups/neev-before-agent6e-schema11-20260921-014828.sql` (104606 bytes, private).

Compose backups are stored beside the live compose file, including `compose.yml.before-agent6e-20260921-014915` and `compose.yml.before-ui64fa-20260921-020310`. The runtime-off schema-11 rollback image is `neev-native:schema11-ui-rollback`. Do not start a schema-9 image against the current database. Preserve credential keys and coordinator state during rollback.

## Projects and Settings

The top-level navigation is Projects, with New project and the project tree. Documents, team chats and agent chats are reached through a project or folder. The empty workspace now opens on Projects. Settings has separate Organization, Members, Personal AI connections and Your profile sections. Existing private agent chats remain unfiled until their owner places them in a project; the association is stored on the server and survives restarts.

## Required proxy DNS configuration

Docker custom networks inject the main-namespace resolver `127.0.0.11`, which is unreachable from the isolated proxy namespace. The deployment binds `/etc/neev-native/proxy-resolv.conf` read-only to `/etc/netns/neev-login-proxy/resolv.conf`. Its IPv4 resolvers come from the host's configured upstreams. The `neev-agent` AppArmor profile permits that resolver bind. Guest DNS remains disabled. Preserve this bind and profile when recreating the service.

## Schema 12 recovery

The latest private database backup is `/opt/neev-db-backups/neev-before-projects-schema12-20260921-062805.sql` (110846 bytes). Compose was backed up as `compose.yml.before-projects12-20260921-062806`.

**Do not start a schema-11 image against the upgraded schema-12 database.** The earlier schema-11 rollback image is retained for historical recovery only. Preserve credentials, keys, coordinator state, documents and the proxy DNS bind. Recover the current schema by rolling forward with a compatible server; do not restore a database backup over newer writes without explicitly accounting for those writes.

## Reference-based workspace UI update — 11:12 UTC

- Source commits: `74a24d5c`, `7c95fcd4`; pushed to `workos/team-workspace`.
- Current live image: `neev-native-agent:ui-reference-7c95fcd4`.
- Current workspace SHA-256: `9ff01bf63c6a55435e562c7e56f2eb743ce1dab92944d035fe43ae802c063f32` (same-host canary and live matched).
- Frontend-only image layered on `projects-final-d2d7c3a2`; schema 12 and provider runtime unchanged. Previous image remains compatible for rollback.
- Compose backup: `compose.yml.before-ui-reference-20260921-111246`.
- Navigation combines documents, team chats, and private agent sessions under projects. Project names open their contents; menus offer each creation type. Agent sessions restore after reload.
- Claude/Codex creation opens a prompt-first composer; the first message creates and titles the saved chat. Disconnected providers link directly to `/settings?section=agents`.
- Document canvas uses a 760px focus width, larger title, and more generous spacing; existing rich-editor block styling is preserved. Full Conductor-style tabs/right context pane are not part of this update.
- Verification: 2,593 frontend tests passed before browser QA refinements; 96 affected tests passed after the refinements; production webpack export passed. Turbopack is blocked by the worktree dependency symlink.
- Browser checks passed for project navigation, retained mixed sidebar contents during agent chats, Back to folder, saved-chat restoration, and the AI connection deep link. Production first-message creation returned exactly “The new project chat works.” from Claude. Codex remains signed out and was not claimed as verified execution.

## Tabbed workspace release — 2026-09-21

- Frontend commits: `9f08057c`, `28cf485f`, `b49b4eab`. The tabbed shell supersedes the earlier reference-layout limitations above.
- Candidate/live image: `neev-native-agent:ui-reference-b49b4eab`; workspace artifact SHA-256 `7257261472721999a09a5ff8a28353f3ebf5888cacc4461a088c0413c5e04de1`.
- Compact Projects tree, breadcrumbs, persistent resource tabs, full-height agent chats and bottom composer. Document tools sit directly under tabs; a 760px paper-like canvas has a 40px title and generous writing space. Account menu offers Light/Dark/System.
- Switching tabs preserves in-memory drafts. Closing dirty tabs confirms in the correct panel; route changes guard all dirty panels. Restored tabs mount lazily; the twelve-tab limit does not silently evict work. Sidebar selection follows only the active tab.
- Verification: 101 affected tests passed after core refinements; final selection patch passed 83 tests across three affected suites. TypeScript, ESLint and production webpack export passed. A broad intermediate run had 2,595 passes and two failures (obsolete switch-discard expectation and settings timeout), both resolved in focused verification.
- Same-host browser checks: document draft survives switching to an agent and back; closing a background dirty document shows its confirmation; cancellation retains draft; discard removes only the unsaved test edit; selected tab and saved tabs restore after reload.
- Live Claude first-message creation and tab switching passed: a saved conversation returned exactly “Tabbed Claude chat works.” Codex remains signed out; its execution was not verified.
- Backend, Docmost, schema 12, provider credentials and isolation configuration are unchanged. This release does not claim complete Notion feature parity; team chat retains its existing inner controls.
- Released at 12:48 UTC; exact canary/live artifact hash matched. Compose backup: `compose.yml.before-ui-reference-20260921-124804`. Prior schema-compatible image remains available for rollback.

## Autosave and agent activity release — 15:32 UTC

- Live image: `neev-native-agent:ux-2d4608ff`; source integration `e3e05783`, pushed to remote `workos`, branch `workos/team-workspace`. UI commits `60350307`, `2d4608ff`; backend production through `c129c761`, test fixture corrections through `30a2deb6`.
- Exact same-host candidate/live workspace SHA-256: `54578cfa509a5778f613a239235893b77cfd524903841e6abf3f2af2bf00756c`.
- Documents autosave after a pause, serialize overlapping saves, retain edits while requests are pending, and recover an ambiguously committed save without overwriting newer text. Navigation flushes pending saves; failures retain the draft and expose retry. Editors remain writable and keep focus during autosave.
- New documents and team chats open immediately inside their project or nested folder. Their first content supplies an editable name, propagated to the page, sidebar, breadcrumb and tab. Manually supplied names are preserved. Enter sends in team and agent composers; Shift+Enter inserts a newline; composition input is guarded.
- Reduced document and chat top spacing. Restored resource tabs resolve their names even when their folder is collapsed.
- Real agent tool calls/results now pass through parsing, bounded run events, SSE and stored turn history. The UI groups and expands this activity. Unicode offsets, replay deduplication, truncated traces and slow subscribers are handled. Hidden model reasoning is not exposed.
- Schema 13 adds stored activity and raises the per-document revision ceiling to 100,000; the existing organization storage quota remains enforced. Actual same-host schema-12 upgrade retained existing revisions.
- Verification: 204 frontend tests across 11 affected suites; after the final cursor/tab fixes, 53 document/workspace tests passed, changed-file lint and production webpack export passed. Focused native suite: 591 passed, one Markdown parser timeout under build load; the exact failed test passed separately. Three explicit PostgreSQL tests passed: revision limit, populated document-format migration and v2 upgrade/foreign keys. Other external-integration tests remained ignored; the unrelated desktop test harness was not claimed as passing.
- Browser candidate checks passed: nested immediate creation, editor/composer focus, automatic naming throughout navigation, document save/reload, continued editability and retained cursor after save, Enter-send and Shift+Enter newline, and restored tab names. Live Claude executed a harmless Bash `printf`, displayed the actual `neev-trace-ok` tool result, and retained its expandable call/result activity after a full page reload. The live acceptance chat is `UX trace check [test]`.
- Codex launch arguments were corrected against the installed CLI, but Codex remains signed out; authenticated Codex execution was not verified. Docmost was not rebuilt.

### Schema 13 recovery

Private backup: `/opt/neev-db-backups/neev-before-ux-schema13-20260921-153209.sql`. Compose backup: `compose.yml.before-ux13-20260921-153209`.

Use `neev-native-agent:ux-schema13-rollback` for UI rollback: it combines the new schema-compatible server with the previous UI. **Do not run the old schema-12 server against schema 13.** Credentials, agent keys, isolation configuration, coordinator state, document volumes and the proxy DNS mount were preserved. A database restore must account for writes made after the backup.
