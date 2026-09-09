# Public-release gates — 2026-09-09

Current decision: **not ready for public signup**. The private hosted pilot and infrastructure are not equivalent to accepted multi-tenant application behavior. Keep catalog agent execution disabled until its integration and acceptance gates pass.

## Delivery order

| Gate | Current evidence / remaining work |
| --- | --- |
| Authentication correctness | Library orchestration accepted at `76b28dfe`: final cross-account session-rotation race fixed, both PostgreSQL ordering regressions reproduced before the fix and passed afterward, and scoped review found no remaining findings. Fresh controller verification passed 152 targeted tests. This is library evidence, not HTTP/UI/runtime acceptance; see `team-auth-acceptance.md`. |
| Team HTTP and onboarding | Task 1 configuration/isolated database initialization accepted at `ae82ba38` after review fixes and fresh controller verification of 171 targeted Rust tests (including disposable PostgreSQL). This does not yet wire standalone startup, HTTP routes or browser UI. Continue the existing `2026-09-08-team-http-onboarding.md` plan for GitHub login, explicit org choice, safe browser session/bootstrap/logout and no legacy token fallback. Do not deploy the resulting shell as the completed product. |
| Workspace access and multiplayer | Convert catalog/settings, nested projects/folders/repo mappings, chats, visibility, scoped events, search and files to checked team principals. Private and org-wide chat rules must hold for list/detail/search/download/events, not just UI hiding. Account/logout transitions clear old caches/subscriptions. In-app org switcher is intentionally deferred. |
| Feedback for founders | Private to submitter and authorized founders only; org admins have no implicit access. Design proposed in `2026-09-09-founder-feedback-design.md`; owner confirmed visibility, written design review pending. Implement persistence, safe user/founder views, replies/status/unread after team authority is accepted. |
| Prompt-completion notifications | Owner requested an optional setting to ping when a prompt receives its completed response. Existing sound preferences/player and desktop helper were found in the legacy UI; sound versus desktop versus both is awaiting owner choice. Reuse suitable components through reviewed team completion events, derive the prompt author server-side, reauthorize visibility and deduplicate alerts. Do not mount the legacy global ACP provider merely to obtain notifications, and do not promise delivery with the browser/laptop closed. |
| Cloud agent execution | gVisor runtime is installed and synthetic isolation tests passed; application jobs do not yet use it. Implement/test per-job and per-org filesystem/credential boundaries, durable queue/recovery, cancellation, quotas, bounded concurrency, safe network access and real provider connection. A provider shown in a dropdown is not a working provider integration. |
| Git and knowledge workflow | Prove per-conversation working branch/worktree mapping, concurrent work without cross-user private-file leaks, deliberate publish/PR actions, and useful org wiki compilation. Private chats and private founder feedback must not leak into shared wiki/Git/agent context. Do not automatically promote unreviewed generated knowledge. |
| Public operational boundary | HTTPS origin and matching GitHub callbacks; safe secrets handling; error/uptime/backup monitoring with an owner; verified encrypted restore; documented recovery and release rollback; rate limits and storage/job abuse limits. Review public privacy/data deletion/export expectations and provider credential consent before inviting external organizations. |
| Acceptance and deployment | Test two people in one org and people in two separate orgs with actual browser sessions. Test real GitHub/provider consent, run/edit/cancel/reconnect/restart/restore flows and denied cross-org access. Review changes, use the required interactive ship gate to publish Workos (not upstream codeg), deploy tested build and smoke-test the actual HTTPS deployment. |

## Infrastructure already established

- Hetzner host, Coolify-managed private application and PostgreSQL exist; current application is healthy with agent catalog execution disabled.
- Named gVisor runtime is installed; the default Docker runtime and live database are unchanged. This proves only host runtime readiness, not application job isolation.
- Native local archive schedules exist. Missing midnight database execution was investigated: a 02:40 UTC scheduled probe passed; final daily DB time changed to 00:17 UTC. Next overnight execution requires observation.
- Hetzner Storage Box authentication and encrypted Restic repository are working. Snapshot `941b108f` was downloaded and verified with the separate recovery-password copy; full pack read and isolated PostgreSQL/tar restore passed. The normal September 9 daily-window run completed successfully at 04:38 UTC with snapshot `92a203ae`, beyond the earlier transient probe. The recurring timer and 7/4/3 retention gate remain enabled. See `backups.md` for retained-snapshot evidence and the still-required operator monitoring/independent key-vault handoff.

## Decisions and external inputs

- No in-app org switching for v1: sign out and sign in again to choose another organization.
- No mandatory Google sign-in, billing, public feedback board, Slack/email feedback integration or automatic upstream production updates for the first release.
- Founder identities must be explicitly configured using verified immutable GitHub IDs, never organization-admin status.
- Owner still needs to finalize the public domain, retain recovery secrets in an approved independent vault, provide a notification destination, participate in real second-user/provider acceptance and run the interactive publication gate.
- Mac cleanup completed: 16GiB available at the resumed Rust build preflight. Continue monitoring available storage; stop on renewed disk exhaustion rather than repeating unchanged builds or deleting personal/cloud/GitHub data to make room.

An invite-only external beta can follow successful tenant/runtime/operational acceptance. Unrestricted public signup should not precede those tests or abuse controls. No launch date or completion percentage is asserted by this checklist.
