# Neev publication and isolated Linux rehearsal

September10,2026. Historical record for `0a719d71`: reviewed source was published
and its isolated host rehearsal passed. The live application had not been
upgraded at that checkpoint. See the subsequent
[42536219 private deployment record](neev-private-deployment-2026-09-10.md) for
current deployment evidence; neither record authorizes opening the app to teams.

## Publication

The owner explicitly authorized direct push without the ship command for this
rollout. The clean publication clone targets EazybeCode/Workos; its branch remains
`workos/team-workspace`. It was fast-forwarded from15c3fb8e to
`0a719d711f6dd1d0798a5a16246aad43eb767709`, pushed successfully, and the remote ref
was independently read back at that exact commit. The source worktree's older
codeg origin was not pushed. No hook configuration or GitHub data was deleted.

Fresh pre-push verification on the matching accepted library binary passed262
selected backend/HTTP/migration checks,0failed/0ignored,33.62s, including explicitly
enabled disposable PostgreSQL. A further frontend run passed92tests in12files.
Diff check passed. This is not the full-repository suite. Existing Vite CJS and
disabled-runtime Rust warnings remain maintenance issues.

Gitleaks scanned81commits/1.67MB with redacted output. Its one generic-api-key
finding was the standard sample WebSocket handshake nonce in the synthetic
browser test at391c872d, not an account credential; source inspection confirmed
the finding. No scanner ignore/baseline rule was added. Evidence is retained
outside Git in the workspace publication record.

## Host and backup preflight

Coolify still reports the correct private pilot healthy, pinned to15c3fb8e with
auto-deploy off, no domain and no host port mapping. The application and PostgreSQL
have internal3080/5432ports only; the public proxy remains stopped. DNS checks for
neev.so and app.neev.so return the expected Hetzner IPv4 address, with no AAAA.

The normal September10 off-server backup timer fired04:30:49UTC and completed
04:34:47UTC with Resultsuccess/ExecMainStatus0. The next observed trigger was
September11 04:31:06UTC. This is one verified daily run, not perpetual monitoring.

Today's native PostgreSQL dump, timestamp00:17:33UTC and105630bytes, passed the
complete host `pg_restore --file=/dev/null` validation. It was restored into a
new, network-disabled PostgreSQL16container using the already-local pinned image,
not over the live database. Restore completed with exit0. Safe scalar counts were
50migrations,1user,1organization and0catalog nodes/drafts/executions. An initial
follow-up count query used the singular organization table name incorrectly;
inspection of the entity's `table_name` corrected the read-only query. That check
error did not change the successful restore or any live data.

## Image and migration rehearsal

A separate named BuildKit builder received only `git archive` of the exact
published commit—no untracked files or credentials. Its effective limits were
5GiB memory,6GiB combined memory/swap and twoCPUcores. The default builder and
existing applications were unchanged. These are trusted application-build limits,
not proof of user-agent isolation. Docker documents these controls in its
[container-driver reference](https://docs.docker.com/build/builders/drivers/docker-container/).

Server-side build history `0uc94ngud8n3qzl0i3ut3ozg8` reportsCompleted in18m12s.
The loaded1.31GB Linux/amd64 image is:

```text
neev-release-candidate:0a719d71
sha256:009e771089bd21af2f81d169cc1dd6b831cec1bd96d408ac4b6a4312ad0a3cfd
```

The original SSH transport lingered without output after the completed build and
loaded image were independently verified. Only that exact owned local SSH process
was terminated; its launcher exited255, not0. Build evidence is the completed
server-side history, loaded image and running-container acceptance below. Later
SSH checks use bounded keepalives. The Dockerfile still uses floating base-image
selectors; this record pins the resulting image, not a reproducible dependency
resolution claim.

The rehearsal app shared only the network-disabled database's network namespace.
It had no host ports, external network, production volumes or provider homes;
root filesystem was read-only, capabilities dropped, no-new-privileges set,
non-root UID65532, bounded temporary storage,768MiB memory and0.5CPU. It explicitly
ran `codeg-server --team` with runtimefalse. Synthetic OAuth/encryption values
were used; no real provider login or old credential decryption was exercised.

Startup migrated the isolated restored copy from50to53migrations. Fresh counts
remained1user/1org/0nodes/0drafts/0executions, with0team sessions. Health returned
200; capabilities reported team mode and runtimeEnabledfalse.

| HTTP check | Result |
| --- | --- |
| `/healthz`, `/api/workos/capabilities` | 200 |
| `/api/team/session`, `/api/team/organization` without a session | 401 and no-store |
| `/api/agents`, `/ws` with a synthetic bearer token | 404 |
| `/workspace`, `/workspace.txt`, `/settings` static export | 200 |

All nine assertions passed with exit0. Graceful stop/start then passed health
again and retained53migrations/1user/1org, showing startup rerun did not duplicate
migrations. This tests startup and HTTP behavior, not authenticated collaboration,
real GitHub/provider consent, prompt execution or job restart recovery.

## Retained resources and remaining cutover

The temporary app and named builder were stopped after the successful rehearsal;
their image/cache and the isolated database copy were retained for the next
candidate. The database remains network-disabled. Exact resources are
`neev-restore-20260910-app`, `neev-restore-20260910-db`,
`neev-restore-20260910-pgdata` and builder`neev-release-20260910`. Cleanup must target
only these verified rehearsal resources, never live volumes or backup archives.

Live Coolify configuration, image, database, public proxy and runtime enablement
remain unchanged. The existing Dockerfile defaults to the legacy supervisor;
deployment must select the explicit team command, not inherit that default.
Chat privacy persistence is being implemented after this published checkpoint.
Team catalog/chat HTTP/UI, scoped downstream consumers, personal provider runtime,
HTTPS/website and real hosted acceptance still gate team rollout.

Context: [org-memory: wiki/engineering/workos.md]. Its older operator deployment
does not authorize exposing the new team product through the legacy router.
