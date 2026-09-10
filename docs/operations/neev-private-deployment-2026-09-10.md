# Neev private team-mode deployment — September 10, 2026

This record supersedes the live-status statements in the earlier
[0a719d71 rehearsal](neev-release-rehearsal-2026-09-10.md). It is a private
checkpoint, not authorization to invite users or enable agent execution.

## Reviewed and published candidate

Exact source: `425362198034f2706dab651a5cf819dd8a1ba434` on
`EazybeCode/Workos`, branch `workos/team-workspace`. Direct publication was
explicitly authorized by the owner. Remote read-back matched; the old codeg
origin, branch name and GitHub data were untouched.

Storage and fingerprint task reviews, the Docker target review, and the final
combined review passed with no Critical or Important finding. Fresh verification
on the production-code parent passed 337 selected backend tests including
PostgreSQL, and 6,165 frontend tests in 444 files. The packaging-only change passed
two Node configuration tests. Library/server checks passed. The five-commit
candidate range passed a redacted secret scan. Existing warnings and the two
deliberately staged fingerprint-variant warnings remain documented, not suppressed.

## Exact target rehearsal

The bounded, separate Hetzner builder built `git archive` of the exact candidate
with `--target team --platform linux/amd64`. Build launcher exited 0. Image:

```text
neev-release-candidate:42536219
sha256:4ba8014e40052a207d6a53f352b3a74374bcac5735a08a31e21963a4063982a8
```

The image itself supplies `codeg-server --team`, `WORKOS_SERVER_MODE=team`,
runtime disabled, and `STOPSIGNAL SIGINT`; the rehearsal did not override those.
The native live PostgreSQL backup `neev-pre-team-42536219.dmp` was created at
07:52:31 UTC, 105,807 bytes, mode 600, and fully validated by `pg_restore`.
It was restored into a new isolated database, not over live data. Startup migrated
50 → 54 migrations and retained scalar counts of one user, one organization and
zero catalog nodes/drafts/executions. Populated catalog preservation is proved by
synthetic SQLite/PostgreSQL regressions, not by this empty hosted catalog.

Eleven HTTP assertions passed: root redirect, health and team capabilities,
unauthenticated session/org denial with no-store, legacy agents/WebSocket denial,
and workspace/navigation-payload/settings/login delivery. Image-default SIGINT
shutdown exited 0 without OOM; restart passed all eleven checks and stable counts.
This does not test real OAuth, provider login, jobs or authenticated collaboration.

The rehearsal app ran non-root, read-only, capability-dropped, no-new-privileges,
bounded memory/CPU/PIDs/tmpfs and no external network, using synthetic credentials.
These controls are rehearsal evidence and must not be attributed to the live
Coolify container without inspecting it. App and builder were stopped afterward;
images, cache, isolated database and original backup were retained.

## GitHub and deployment configuration

After exact owner approval, the existing EazyWorkOS GitHub App homepage was saved
as `https://neev.so`, and `https://app.neev.so/auth/github/callback` was added.
Both existing local callbacks were preserved, all callback wildcards stayed off,
and no permissions, secrets or App identity were changed. GitHub's success state
and saved values were read back. This is configuration evidence, not real OAuth
acceptance through a working public HTTPS endpoint.

Coolify was pinned to the exact candidate with target `team`, health `/healthz`,
auto-deploy disabled and existing private routing preserved (no domain or host
ports). Only the production app-origin and callback URL environment values were
updated to the approved app.neev.so URLs; preview values, encryption key,
database URL and provider secrets were unchanged.

Deployment `uvehr1jvq0vkbtqhxpses3pm` was queued at 08:11:30 UTC and finished at
08:30:49 UTC, with the exact reviewed source commit. The new live container
`xcyeoebzuq1mpsezjoanrofs-081131021943` is healthy and uses:

```text
sha256:1fe5ab4c0033031e2a1cc07f311d236527d6e60ff6ba223a2e2dc09da65a8601
```

This is Coolify's build of the same source, not the rehearsal image digest.
Live inspection verified CMD `codeg-server --team`, SIGINT, team mode, runtime
disabled, approved HTTPS origin/callback, expected OAuth client ID and presence
of the existing client secret/database configuration without printing secrets.
All eleven live HTTP checks passed. Live database counts are now 54 migrations,
one user, one organization and zero nodes/drafts/executions/team sessions.
No public ports were added: only SSH and local DNS listeners were present.
Coolify read-back remains healthy, pinned, target team, no domain/host mapping,
auto-deploy off, and health path `/healthz`.

### Legacy credential mounts: removal blocked, data preserved

The live app still has legacy named volumes mounted at `/root/.claude` and
`/root/.codex`, in addition to `/data`. An exact-target API request to remove the
Claude mount configuration was rejected with HTTP 422 before the Codex request
was attempted. Fresh API read-back confirmed all three configurations unchanged;
Docker inspection confirmed both original named volumes and creation timestamps
unchanged. The UI confirms backup schedules are enabled for both provider volumes.

Coolify v4.3.18's controller refuses storage deletion when backup schedules exist;
its persistent-volume model requires deleting the schedule and archives first.
See the versioned [controller](https://github.com/coollabsio/coolify/blob/v4.3.18/app/Http/Controllers/Api/ApplicationsController.php)
and [model](https://github.com/coollabsio/coolify/blob/v4.3.18/app/Models/LocalPersistentVolume.php).
No backup schedule/archive or underlying volume was deleted to bypass this guard.
Safe backup retention/migration and mount detachment remain required before user
admission. No provider credential was read, imported or revoked.

The live container is non-privileged with 3 GiB memory, but runs as root and lacks
the rehearsal's read-only root, dropped capabilities, no-new-privileges and PID
limit. Do not claim production runtime hardening or per-user isolation from the
rehearsal results. Public admission and execution remain disabled.

## Remaining release boundary

Team catalog/chat authorization and UI, scoped downstream data access, personal
provider login and isolated durable execution, private founder feedback,
completion notifications, website/TLS and real two-user/two-org acceptance remain
open. Recovery-secret custody and an alert destination also need operator input.
The morning recurring offsite backup succeeded; this does not establish that the
new 07:52 dump has already reached offsite storage.

Context: [org-memory: wiki/engineering/workos.md]. Current reviewed team code and
this deployment evidence supersede the older operator-mode deployment guidance.
