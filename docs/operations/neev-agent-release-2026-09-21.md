# Neev agent release — 21 September 2026

## Verified deployment

- Public app: https://app.neev.so/workspace
- Source remote/branch: `workos` / `workos/team-workspace` (`EazybeCode/neev`).
- Agent runtime recovery: `a95e62f2`; orphan reaping: `7b495f6e`; approval UI and concurrent sign-in guard: `ff745d1b`.
- Release image: `neev-native-agent:restart-final`.
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
- A successfully authenticated model response still requires the user's provider authorization; an approval challenge alone is not a completed account connection.

- Final runtime suite: 499 passed, 3 skipped; container-init tests: 3 passed.
- Real restart acceptance passed: committed credential sentinel preserved, interrupted login cleaned, fresh login and cancellation succeeded, no fixture mapping/store/loop leaks. Log: `/tmp/neev-agent-restart-acceptance.log` on the host.
- Final combined image passed Claude and Codex native HTTP approval/cancel checks. Its actual production init reaped an adopted child in the same-host canary.

## Backups and rollback

Pre-agent database backup: `/opt/neev-db-backups/neev-before-agent6e-schema11-20260921-014828.sql` (104606 bytes, private).

Compose backups are stored beside the live compose file, including `compose.yml.before-agent6e-20260921-014915` and `compose.yml.before-ui64fa-20260921-020310`. The runtime-off schema-11 rollback image is `neev-native:schema11-ui-rollback`. Do not start a schema-9 image against the current database. Preserve credential keys and coordinator state during rollback.
