# Native personal-agent lifecycle acceptance

September12,2026. Internal milestone accepted through `9ab11000`; this does not
enable agent login, provider credentials, chat execution or public release.

## Accepted implementation

- Native schema5 adds connection, attempt and operation metadata; historical
  content3/4 snapshots remain independently verifiable without migration.
- Connection list/start/cancel/disconnect use checked native identity, scoped
  ownership, exact initiating-session access, idempotency and member-wide quotas.
- Completion/failure recheck current authority, generation, every ticket binding,
  lease and expiry. Late results cannot undo cancellation/logout/removal.
- Durable cleanup supports lease takeover/retry after original expiry, preserves
  newer connected state, and does not require an active member to remove access.
- Logout atomically invalidates only its own pending login attempts. Startup
  recovery is explicit and callable, not wired into constructors or a daemon.

Task commits: schema `9b4197c1`, fixture correction `7493b96d`, intents
`85c7d875`, PG authority coverage `f812576b`, recovery/logout `225ed087`, bounded
PG contender joins `9ab11000`. Fresh task reviews and the whole-milestone review
found no remaining Critical/Important issue.

## Verification evidence

At `225ed087`: recovery14/14 including four guarded PostgreSQL matrices;
native session7/7; full native233/233,0ignored, only the interactive browser
driver excluded. Controller independently repeated233/233 in27.80seconds.
At the test-only `9ab11000` fix: the exact guarded PostgreSQL admission/replay/
quota/cleanup contract passed1/1 and scoped re-review accepted all timeout sites.
The final integration checkpoint (same source, documentation throughb9c616af)
again passed233/233,0failed/0ignored,23.62seconds, with the same disclosed warnings.
Native frontend/content-client baseline passed119/119 across13files,9.35seconds.
These frontend tests do not imply a provider-connection interface exists.

All database/provider evidence is invented and isolated. Tests exercise real
SQLite/PostgreSQL storage and observed lock overlap; no provider client login or
real credential was used. Successful owned PG fixtures were cleaned; new task
fixtures retain their UUID schema on failure.

Historical retained schema3 restore was verified read-only by the built content
verifier:3blobs,608bytes, unchanged manifest
`7d1f4aecf9c55451b1087dced6cd44d1cb8236fcc59676e92cb251eac139c235`.
That is historical archive evidence, not production backup coverage.

Known nonblocking debt: direct expired/generation-invalidated begin-replay tests,
settled replay operation-count assertions, existing compiler/linker/future-
compatibility warnings. The non-test verifier build also emits expected unwired-
agent warnings; neither build output is pristine.

## Remaining integration boundary

Still required: authenticated supervisor, pinned official-client adapters,
encrypted credential storage/key custody, enforced egress/resources, bounded
native HTTP/onboarding/settings controls, real subscription acceptance, durable
author-scoped jobs, native Linux deployment and multi-organization acceptance.
No capability flag was enabled by this milestone. Do not mount legacy shared
credentials or the legacy dispatcher to fill those gaps.

Context: [org-memory: wiki/engineering/workos.md]. Native identity/authorization
and this evidence supersede that page's historical operator-mode guidance.
