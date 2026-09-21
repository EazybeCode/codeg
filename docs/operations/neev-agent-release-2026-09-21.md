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
- The final browser chat check remains pending Google phone verification after the web session expired; the runtime reply was verified independently.

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
