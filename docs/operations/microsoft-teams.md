# Microsoft Teams integration: setup

Neev runs inside Microsoft Teams as a bot. People can message the Neev bot
directly, or @mention it in a channel where it has been enabled.

- **Channel threads are shared.** A channel root post and its replies form one
  shared thread. Everyone in the thread contributes to the same task context.
  Each request runs **as the person who asked**, in that person's own Neev chat,
  with their own Neev access and connection. Nobody's request ever runs on
  someone else's account or credentials.
- **1:1 chats are private.** A personal chat with the bot runs in that person's
  own Neev chat, and its answers go only to that chat.
- **Answers arrive by themselves.** Neev acknowledges a request, then posts the
  final answer (and links any generated files) into the same thread. Nobody has
  to click anything to get it.

Every request goes through the same launcher as the web chat
(`launch_session_turn`, Auto routing included). It runs through an ordinary org
session for the linked member, so every membership, role and folder check still
applies. Teams does not get its own access path: a tenant, Team, channel or
person reaches Neev only through a binding a Neev admin created (and, for Teams
and channels, that Microsoft Graph confirmed), or that the person created by
linking their own account. Neev never matches people by email, UPN or name.

## What works in this build

| Capability | Status |
|---|---|
| Bot Connector JWT verification (RS256, issuer, audience, lifetime ±5 min, `serviceurl` binding, `msteams` key endorsement) | Implemented |
| Bindings in the database (schema 25): tenant → org, Team → project, channel enablement, Entra user → Neev account | Implemented (admin HTTP API below; no admin UI yet) |
| Linking a Team requires a Neev admin who Graph confirms is an **owner** of that Team (via the admin's own linked Teams identity). The Team's Entra group id comes from the Bot Connector, never from input | Implemented |
| Enabling a channel requires a verified Team owner and a Graph-confirmed **live standard channel with no guests and no members from other tenants** | Implemented |
| Every channel message and card click is authorized on BOTH sides, fresh: Graph (in-tenant, non-guest Team member; channel audience) and Neev (active membership) | Implemented. Fails closed when Graph cannot verify |
| Guest/external accounts refused at account linking (Entra `idp` claim) | Implemented |
| Durable inbox: an accepted message is committed **before** the HTTP 200. If it cannot be stored Neev answers 503 and the Bot Connector retries | Implemented |
| Shared channel threads: one root post is one shared task context, inputs attributed, roots isolated | Implemented |
| One live run per thread; later messages are queued, labelled honestly, then run **in order, each as its own author**, with the previous answer as context | Implemented |
| Worker: launches, settles and delivers from the database; resumes after a restart; a repeated launch replays the same run (request id) | Implemented |
| Durable outbox: acknowledgement, queued notes, final answers and file links; bounded retries honouring `Retry-After`; Teams message ids stored | Implemented (at-least-once, see Known gaps) |
| Auto routing for Teams turns, idempotent across retries; a retried request with different attachments is a conflict | Implemented |
| Status and Stop buttons, bound to the run's persisted origin (same personal chat, or same channel + root thread); Stop only by the member it runs as; answers shown only to the clicker | Implemented |
| Source gate for channel answers and files (see below) | Implemented |
| Generated files published to the channel's SharePoint Files and linked in the thread | Implemented, **off until file permissions are approved** (`NEEV_TEAMS_GRAPH_FILES`) |
| Suspension on Teams events (app removed from the Team, Team deleted/archived, channel deleted) | Implemented. Nothing is deleted |
| Approval cards for agent actions | **Not implemented**: Neev has no pending-action records to approve |
| Steering a live run from Teams | **Not implemented**. Follow-ups queue and run next |
| Throttled progress-card updates | **Not implemented**. One acknowledgement, then the final answer |
| Group chats, private channels, shared channels, guests, external members | Refused |

## Server configuration (`NEEV_TEAMS_*`)

If `NEEV_TEAMS_ENABLED` is absent or `false`, no Teams route exists and no worker
runs. If it is `true`, every required key must be valid or the server **refuses
to start** (`Microsoft Teams configuration rejected` names the key).

| Variable | Required | Value |
|---|---|---|
| `NEEV_TEAMS_ENABLED` | yes | Exactly `true` or `false` |
| `NEEV_TEAMS_BOT_APP_ID` | yes | The Azure Bot's Microsoft App ID (client ID GUID) |
| `NEEV_TEAMS_BOT_TENANT_ID` | yes | Tenant that owns the **single-tenant** registration |
| `NEEV_TEAMS_BOT_CLIENT_SECRET` | yes | Client secret of that registration. Store as a secret |
| `NEEV_TEAMS_SERVICE_URL_HOSTS` | no | Allowed Bot Connector hosts. Default `smba.trafficmanager.net` |
| `NEEV_TEAMS_TAB_APP_ID_URI` | no | `api://<host>/<bot app id>` (or `…/botid-<id>`). Enables tab SSO status and account linking |
| `NEEV_TEAMS_GRAPH_ENABLED` | for channels | `true` turns on the Graph authority. **Without it every channel request and every Team/channel binding is refused** (`teams_unverifiable`); 1:1 chats still work |
| `NEEV_TEAMS_GRAPH_CLIENT_ID` | with Graph | The Entra app whose Graph permissions were approved. This **may be the same app as the bot**; the setting is separate so the Graph token is requested explicitly (Graph audience) and never reuses the Bot Connector token |
| `NEEV_TEAMS_GRAPH_CLIENT_SECRET` | with Graph | Its client secret |
| `NEEV_TEAMS_GRAPH_FILES` | no | `true` only after the file permissions below are **approved**. Off: generated files stay in Neev and the thread says so. `true` without Graph is a startup error |
| `NEEV_TEAMS_MAPPING_FILE` | **retired** | Setting it stops the server from starting |

### Graph permissions to request (and get approved) — nothing broader

Neev requests a client-credentials token for `https://graph.microsoft.com/.default`
from the installing tenant. Its power is exactly what the tenant admin granted.

| Purpose | Endpoint | Permission (least privilege) | Kind |
|---|---|---|---|
| Team owner / member / guest / tenant check | `GET /teams/{group}/members?$filter=(microsoft.graph.aadUserConversationMember/userId eq '<oid>')` | `TeamMember.Read.Group` | RSC (granted per Team at app install) |
| Channel type, archive, guests, external members | `GET /teams/{group}/channels/{id}?$select=id,membershipType,isArchived` and `?$select=id,summary` | `ChannelSettings.Read.Group` | RSC |
| Channel files folder discovery (files only) | `GET /teams/{group}/channels/{id}/filesFolder` | `File.Read.Group` | RSC |
| Create folders, upload, read back (files only) | `POST /drives/{d}/items/{i}/children`, `…:/{name}:/createUploadSession`, `GET /drives/{d}/items/{i}:/{name}` | `Sites.Selected` (application) **plus** a per-site `write` grant on the pilot Team's SharePoint site by a tenant admin | Application, site-scoped |
| Prove an uncertain upload is ours (files only) | `GET /drives/{d}/items/{i}/content` → `302` to a pre-authenticated `https://*.sharepoint.com` download URL, fetched without the Graph token | Same `Sites.Selected` site grant (a `write` site role includes read) | Application, site-scoped |

The RSC permissions are declared in the app manifest by the packaging owner.
The `Sites.Selected` site grant is a separate, explicit admin action. Neev never
requests `Files.ReadWrite.All`, `Sites.ReadWrite.All`, `Group.Read.All` or
message-reading permissions, and never creates sharing links or invitations.
Verify each endpoint in the test tenant after consent; a refusal shows as
`teams_unverifiable` (membership/channel) or `teams_files_forbidden` (files).
The Graph documentation lists these permissions for these endpoints, but none
has been exercised against a real tenant yet: the pilot must confirm in
particular that `filesFolder` answers with the RSC `File.Read.Group` grant and
that the site-scoped grant covers folder creation, upload sessions, path reads
and `/content` downloads (including the redirect host being the tenant's
`*.sharepoint.com`).

### Bindings (schema 25)

Schema 25 adds nine tables and changes no existing row:

| Table | Purpose |
|---|---|
| `neev_msteams_installations` | tenant → org, default agent, last signed Bot Connector `service_url` |
| `neev_msteams_project_links` | Team (`team_id`) → project; verified `graph_group_id`, `verified_owner`, `verified_at` |
| `neev_msteams_channel_links` | enabled channels; `verified_at` |
| `neev_msteams_principals` | Entra object id → Neev member |
| `neev_msteams_threads` | one shared thread per root post; `live_sequence` claim |
| `neev_msteams_thread_messages` | the thread's shared context (inputs + posted answers) |
| `neev_msteams_inbox` | accepted messages and their run (`queued/pending/launched/settled/refused`) |
| `neev_msteams_outbox` | messages to post (`pending/sent/withheld/failed`), Teams message id, error code. A final answer stores no text |
| `neev_msteams_documents` | generated files of channel runs and their publication (`pending/uploading/published/withheld/failed`); `sealed_session` holds the AES-GCM sealed upload capability only while an upload is in flight |

Every child row is bound to its org by composite foreign keys. Roll back with
`neev-schema-downgrade --from 25 --to 24 --confirm`; it refuses while any Teams
row exists.

Admin endpoints (Neev admin session + CSRF; mounted only while Teams is enabled;
every write idempotent):

| Endpoint | Body | Effect |
|---|---|---|
| `GET /api/neev/teams/bindings` | — | Everything bound for your org |
| `POST /api/neev/teams/bindings/tenant` | `{tenantId, provider, model}` | Connect a tenant (one org per tenant) |
| `POST /api/neev/teams/bindings/tenant/state` | `{tenantId, enabled}` | Suspend or resume |
| `POST /api/neev/teams/bindings/team` | `{tenantId, teamId, nodeId}` | Link a Team to a project. Requires: the bot has heard from the tenant (service URL known), the admin has linked their own Teams account, and Graph confirms they own the Team |
| `POST /api/neev/teams/bindings/team/state` | `{tenantId, teamId, enabled}` | Suspend or resume a Team link |
| `POST /api/neev/teams/bindings/channel` | `{tenantId, teamId, channelId, enabled}` | Enable (verified owner + eligible standard channel) or disable (always allowed) |
| `POST /api/neev/teams/unlink` | `{tenantId, userId}` | Remove a person's link |

Verification refusals come back as `{"error": "<code>", "message": …}` with
403/404/503 (`teams_not_team_owner`, `teams_channel_not_eligible`,
`teams_unmapped_user`, `teams_unverifiable`, `teams_unmapped_channel`).

**What every channel message checks before it is stored:** tenant connected and
enabled → channel and Team link enabled → reported Team matches → sender linked
→ Graph: channel still eligible, sender an in-tenant non-guest Team member →
Neev: account enabled and an active member. The worker repeats the binding,
principal, Graph and Neev checks at launch (a delegated org session), and again
before posting anything.

## Answers, files and the source gate

A channel answer is read by everyone in the channel, and a Team's members need
not be members of the Neev organization or project. So a channel run is given
**nothing from the workspace**: no organization or project name, no item
titles, no compiled knowledge, no shared-library skills or integrations, no
session transcript or earlier turns of its session (an earlier answer may have
been withheld), and no supervisor. Its only context is the thread the channel
already reads (the request quotes it) plus standing instructions. Neev also
gates what it posts on what the run actually did. A run executes
on the requester's provider account, whose own connectors Neev cannot switch
off, and Neev has not proven its sandboxed file/shell/fetch tools see only
channel-safe material. So an answer — and any file it produced — is posted only
when its complete, stored tool trace contains no tool that reads a source:
`Write`, `TodoWrite`, `ExitPlanMode`, `WebSearch` (Codex: `web_search`,
`update_plan`). Any read, search, edit, shell, `WebFetch`, sub-agent, **any MCP
tool (including the org's shared integrations)**, an unknown tool or a
truncated trace withholds it: the thread gets "Answer kept private", the
requester keeps the answer in their own Neev chat, and the outbox records
`teams_unclassified_source` or `teams_trace_incomplete`. Withheld answers are
not added to thread context. This is deliberately strict for the pilot.

**Usability limit (pilot):** ordinary document generation that uses the shell
(for example a `Bash` step that runs a script to build a `.docx`, `.xlsx` or
`.pptx`) is withheld — answer and files — because the run sandbox is not yet
proven to isolate the shell from non-channel material. A channel answer is
posted, and its files published, only for runs that write output directly
(`Write`) and/or search the public web. Office generation from a channel is
therefore **not** a supported workflow in this build; it works in the
requester's own Neev chat.

Final answers and files are also withheld (nothing posted) if, at send time, the
binding was suspended, the channel gained guests or external members, or the
requester lost Team or Neev membership.

**Files (when `NEEV_TEAMS_GRAPH_FILES=true`).** For a completed, source-safe
channel run, each generated file is uploaded to the channel's own SharePoint
folder (`filesFolder` → `Neev/` → `Run <id>/`) with create-only upload sessions
in 5 MiB fragments (no application size cap; Graph's limits apply), then linked
in the thread with its SharePoint URL (existing permissions; no sharing links).
Bytes are read through the attachment API as the requester and must match the
stored SHA-256. The upload capability URL is sealed before any byte is sent, so
a restart resumes the same session. An uncertain completion is reconciled from
the destination: a file there is accepted as the artifact only when its
content, downloaded through Graph, has exactly the stored length and SHA-256
and its item id and ETag are unchanged around that read. A same-named file
with other content is a conflict (`teams_document_conflict`; never overwritten
or linked; the thread is told the file stayed in Neev). A missing file after a
possibly-committed upload is **not** treated as proof that nothing committed:
the sealed session is kept, only resumed or reconciled, never replaced by a
second session; after 8 attempts the row fails as `teams_document_uncertain`
(and the thread is told). Graph `Retry-After` on 429/5xx sets the next attempt.

## Microsoft admin steps

1. **Register the app** (single tenant) and **create a client secret**.
2. **Create the Azure Bot** (single tenant, messaging endpoint
   `https://<neev-host>/api/teams/messages`, add the Microsoft Teams channel).
3. **(Optional) Tab SSO**: Application ID URI `api://<neev-host>/<client id>`,
   scope `access_as_user`, authorize the Teams client IDs
   `1fec8e78-bce4-4aaf-ab1b-5451cc387264` and `5e3ce6c0-2b1f-4285-8d4b-75ee78787346`,
   set `NEEV_TEAMS_TAB_APP_ID_URI`.
4. **Graph authority**: after the tenant admin approves the RSC permissions
   above (and, for files, the `Sites.Selected` site grant), set
   `NEEV_TEAMS_GRAPH_*`. Do not set `NEEV_TEAMS_GRAPH_FILES=true` before the
   file grants exist.
5. **Configure Neev**, restart, connect the tenant. `POST /api/teams/messages`
   without a token must return `401` (a `404` means Teams is off).
6. **Package and publish** the app (`scripts/teams/build_app_package.py`, owned
   by the packaging work stream), install it in the pilot Team so the bot
   receives a signed activity (this records the service URL).
7. **Link accounts and bind**: the admin links their own Teams account, links
   the Team to a project, then enables each standard channel. Each person links
   their own account.

## What people see when Neev declines

| Code | Cause / fix |
|---|---|
| `teams_unmapped_tenant` | The tenant is not connected |
| `teams_suspended` | Never sent; suspended places stay silent |
| `teams_guest_sender` | Guest, external or no Entra object id |
| `teams_unmapped_user` | The person has not linked their account |
| `teams_unmapped_channel` | Channel not enabled, deleted, or reported under another Team |
| `teams_unknown_thread` | No thread root id |
| `teams_group_chat` | Group chats are not supported |
| `teams_not_a_member` | The linked Neev account is disabled or not an active member |
| `teams_not_team_member` | Not (or no longer) a member of the Team |
| `teams_channel_not_eligible` | Not a live standard channel free of guests/external members |
| `teams_unverifiable` | Graph not configured, consent missing/revoked, or Team unknown |
| `teams_not_team_owner` | Linking/enabling requires a Team owner |
| `teams_not_run_owner` | Only the requester can stop their run |
| `teams_runtime_unavailable` | No agent runtime on this server |
| `teams_busy`, `teams_limited`, `teams_unavailable`, `teams_session_unavailable` | Transient; launches are retried with backoff before refusing |
| `teams_run_not_found` | A button named a run from another conversation |
| `teams_wrong_bot`, `teams_unknown_action` | Another bot, or a malformed action |

Outbox/document error codes (operators): `teams_unclassified_source`,
`teams_trace_incomplete`, `teams_files_forbidden`, `teams_document_conflict`,
`teams_document_changed`, `teams_document_unavailable`, `teams_files_unavailable`,
`teams_document_uncertain`, `teams_files_error`,
`throttled_or_server_error`, `uncertain`, `refused`.

## Security notes

- Every Bot Connector token is verified before Neev acts; only RS256; no switch
  to turn verification off. Key refresh is floored at 5 minutes; with no key set
  ever fetched, requests get `503`.
- Replies go only to an HTTPS `serviceUrl` on an allowed host that matches the
  inbound token.
- Graph and Bot Connector tokens are separate (audience, cache, configuration).
  Graph errors never include response bodies or URLs; redirects are disabled;
  pagination links are never followed.
- Graph membership answers fail closed: a missing/invalid/other `tenantId`, an
  unknown or non-string role (even beside `owner`), or a paged answer is never
  treated as an internal member or owner.
- Channel audience is cached for 120 s for admission only; every publication
  re-reads it.
- The delegated run session lasts 12 h (runs have no wall-clock cap); its token
  is discarded. Delivery-time checks use a read-only membership check.

## Known gaps in this build

- **No live tenant run yet.** Graph RSC consent, the `Sites.Selected` grant and
  a work-account Teams login are external blockers; all behaviour above is
  covered by automated tests against loopback Bot Connector and Graph servers.
- Delivery is at-least-once: if the Bot Connector times out after posting, the
  retry can post a duplicate (Teams offers no idempotency key). Agent execution
  is never repeated.
- The strict source gate withholds most tool-using answers from channels;
  widening it needs proof of sandbox isolation or per-source audience metadata.
- File bytes are held in memory during upload (no streaming upload); the
  reconciliation download is streamed into a hash and stops past the expected
  length.
- A row that fails as `teams_document_uncertain` may correspond to a file that
  did commit (left unlinked in `Run <id>/`); Neev never guesses.
- `Retry-After` from the Entra token endpoint is not read (only Graph's).
- No approval cards, steering, or progress-card updates.
- Each launch adds one `neev_sessions` row (not pruned on any surface).
