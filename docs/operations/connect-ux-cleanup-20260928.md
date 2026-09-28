# Connection UX and scheduled disk cleanup — 2026-09-28

Chat chooses a connected provider when its remembered/default provider is not
connected, until the person explicitly picks another provider. Selecting a
disconnected provider opens an in-place, provider-filtered connection panel.
Sending rechecks connection state before invoking: disconnected accounts open
the dialog without posting a message or clearing the draft/attachments.
The compact Connect/Reconnect button opens the same dialog.

Verification: 84 chat and connection tests passed; additional model-menu test
confirms selecting disconnected Claude opens its dialog. Existing invocation
fixtures now explicitly declare connected accounts.

Server cleanup installed:
- /usr/local/sbin/neev-disk-cleanup
- /etc/cron.d/neev-disk-cleanup: minute 17 each hour (server timezone)
- /etc/logrotate.d/neev-disk-cleanup: four compressed rotations
- /var/log/neev-disk-cleanup.log

Uses flock to prevent overlap. Removes unused build cache on known builders,
dangling images older than 24 hours, and archived system journals beyond 14 days
or 200 MB. Under 5 GB free, unused cache has no age restriction. Never removes
containers, volumes, uploads, database files, backups, or tagged release images.
First run increased free space from 530 MB to 1.4 GB. Cron is active; shell syntax
and logrotate config validated. Capacity remains tight: retained images/data
need a separate retention decision or a larger disk if growth continues.

Production build/typecheck and lint passed. Released frontend-only image
`neev-native-agent:connect-ux-20260928`; live workspace SHA256
`7b0faf95bcaba0254dcdf6d45a052a76c7a69c8b9ab11e180f1f089d8aaacbff`.
Guard confirmed no running turns/sign-ins; fresh backup verified. Schema17 and
backend binary unchanged. Live browser showed connected Codex selected; selecting
Claude Default opened Connect · Claude with Not signed in and Connect claude.
No test prompt was posted or sign-in started. Popup left open for user review.
