# Neev — website and application release plan

Date: 2026-09-09

Status: implementation in progress; not approved for user rollout. The owner has selected Neev, with neev.so for the website and app.neev.so for the application. Website apex/www and application DNS are verified; HTTPS and application cutover remain pending. This plan is a delivery sequence, not evidence that the listed features already work.

## Release outcome

The owner's team and explicitly admitted pilot organizations can sign in with GitHub, select their organization, organize projects and nested folders, connect repositories and their own agents, and run repository-backed conversations in the cloud. Private conversations remain private to their creator; organization-wide conversations are available only within the selected organization. Work continues without the user's laptop, with honest run status and recovery after a server restart.

Visible product branding becomes Neev. Keep existing database/storage keys, API paths, environment variable names, GitHub App identity, repository and branch names unchanged. Do not overwrite organization-specific names/logos with the product brand. The earlier discovery of similarly named agent products has been disclosed; selecting this domain is not a trademark clearance.

Use separate routes/resources for the public website at `https://neev.so` and authenticated workspace at `https://app.neev.so`. The website's sign-in link goes to `https://app.neev.so/login`; it does not handle session cookies or call protected team APIs. Keep application cookies host-only, with no `.neev.so` Domain attribute and no permissive cross-origin exception for the website. Public website content/implementation is separate from the current application onboarding task and is not yet built or approved for publication. `www.neev.so` redirects to the website, not the workspace.

## Current checkpoint

- Accepted authentication library work: `76b28dfe`; accepted team startup configuration and isolated database initialization: `ae82ba38`.
- The isolated team HTTP route task is accepted at `4c290570` after its scoped fix re-review. Fresh controller verification passed 188 targeted Rust tests (35 team-server, 138 team-access, 11 credential-migration and 4 legacy OAuth-state controls), including disposable PostgreSQL tests; the standalone binary target also passed `cargo check`. These are not an end-to-end product acceptance result or a full-project suite.
- Browser onboarding is accepted at `23675b01` after fixing the independently identified response-body timeout/cancellation gap and passing scoped re-review. Fresh controller verification passed 184 UI tests, full TypeScript, scoped lint, diff checks and a 34-page static export at that commit. The subsequent actual browser-to-server evidence is recorded below; neither result certifies a working team workspace. The following workspace-access conversion has not started implementation.
- Task4 actual Chromium testing found and fixed a production integration defect: exported Next navigation requests for `/workspace.txt` and `/login.txt` returned404 because the team static manifest omitted their payloads. Commit `391c872d` adds exactly four allowed payload paths and focused allow/deny regressions. Earlier fixture/selector/diagnostic failures are recorded separately from this real product failure.
- Owner disk cleanup resolved the compilation pause. Controller verification passed 189 targeted Rust tests, 184 UI tests, scoped lint, a 34-page static export/TypeScript, the standalone binary build and eight startup controls. Task4 driver fix `6f8a453c` closed all four independent review findings. Final whole-plan review found no blocker; final fix `b7a2f180` closed its two minor findings and passed scoped re-review. Fresh final-fix verification passed 37 server tests, six affected UI tests, scoped lint and actual desktop/mobile browser journeys. The earlier standalone/export checks were not rebuilt after the two-icon route fix. All four tasks and final review are accepted for continued development, not rollout. See [acceptance evidence](team-http-acceptance.md). No deployment occurred; real provider consent, protected workspace functionality and agent execution are not certified by these tests.
- The [current-organization settings implementation plan](../superpowers/plans/2026-09-09-team-organization-settings.md) is prepared for the next conversion slice: verified admin editing, member read-only access and session-scoped form state. Its HTTP/browser prerequisite is accepted; settings implementation has not started. Preparing the plan does not implement settings or replace the pending hierarchy/chat-privacy work.
- The current hosted pilot is private. Runtime execution is disabled; installed gVisor infrastructure has not yet been connected to application jobs.
- Encrypted off-server backups and one isolated restore have been verified. The September 9 recurring backup succeeded. Monitoring and independent recovery-secret custody still need completion.
- September 9 read-only host check: approximately 6.7 GiB available RAM, 50 GiB free disk, and no active application CPU load at sampling. This supports considering isolated staging on the existing host; it is not a concurrency/load-test result or a guarantee of agent-job capacity. No staging resources were created by this check.

## Delivery sequence and exit gates

| Stage | Deliverable | Evidence required before advancing |
| --- | --- | --- |
| 1. Identity and onboarding | Isolated team HTTP router, browser-bound GitHub login, explicit org choice, safe session/bootstrap/logout, visible person/org identity and settings return navigation. | Reviewed Rust/HTTP tests, PostgreSQL transaction races, browser tests with independent contexts, static export, no legacy token/router/provider fallback. |
| 2. Workspace and privacy | Existing projects → nested folders/working directories → repository-backed chats, current-org settings, private/org-wide audiences and one resource-authorization contract. | Lists, direct reads, counts, search, files and live updates cannot leak private or foreign-org resources; same-person/different-org and admin/private-chat negative controls. Preserve repository/default/branch snapshots and retries. |
| 3. Cloud execution | Personal provider connections, isolated per-job worktrees and credentials, durable prompt jobs, bounded concurrency, cancellation and restart recovery. | Actual application jobs use the intended gVisor runtime; no host/Docker socket/shared credential exposure; cross-org/private-file attacks denied. Test real provider run/edit/cancel/reconnect and restart behavior before enabling execution. |
| 4. Team product experience | Useful agent/model controls and authorship, Neev branding, approved completion sound plus optional desktop notification, private founder feedback/replies, authorized knowledge workflow. | Desktop/mobile and keyboard tests; notification opt-in, one completion alert for the prompt's author, no private response previews; feedback creator/founder-only access; no private material in org wiki or automatic Git publication. |
| 5. Release candidate | Reviewed immutable commit, migration rehearsal, protected deployment configuration, HTTPS and pilot enrollment restrictions. | All earlier gates pass, fresh verified backup, isolated restore/migration rehearsal, explicit allowlist/admission behavior for pilot orgs, static secret-free errors and documented rollback. |
| 6. Hosted acceptance | Candidate deployed privately using the existing Coolify/Hetzner infrastructure, then exercised through its actual public-origin/TLS configuration while access remains restricted. | Hosted smoke tests and real owner/second-user consent flow; two-org isolation, complete run flow, persistence/restart and operational monitoring. No production test identities or data in unrelated applications. |
| 7. Pilot rollout | Invite the owner's team first, then selected external organizations. | Operator sign-off on the acceptance record, recovery custody and alert destination; observe errors, queue/resource load and support reports before widening admission. |

Unrestricted public signup, billing, Google sign-in, an in-app org switcher, automatic production upstream updates and a public feedback board are not requirements for this first pilot. Do not substitute a read-only authentication shell for the working agent product promised above.

## Mandatory acceptance journeys

Use synthetic local fixtures first. Real hosted consent is a separate gate; fake GitHub/provider tests must be labeled as such.

1. New user signs in, explicitly selects the correct GitHub org, sees their name and the preserved org name/logo, and can return from settings to the workspace.
2. Create a project, folder, deeper subfolder and working directory; inherit or explicitly choose a repository/starting branch; create a chat in the selected location without an orphan global New chat action.
3. Connect a personal supported agent, send a prompt, receive streaming and final output, reload the chat, inspect authorized artifacts and Git changes, and deliberately publish only to the confirmed destination.
4. A colleague can collaborate in an organization-wide conversation with correct prompt authorship. Another member and even an org admin cannot see a private chat's title, count, content, files or events.
5. A second organization cannot access the first organization's resources, credentials, jobs, caches, files or notification events through guessed IDs, direct URLs, stale tabs or forged request fields.
6. A user belonging to both orgs logs out and signs back into the other org. Old content, drafts and subscriptions cannot appear or send in the new scope.
7. Enable completion sound/optional desktop alerts, verify permission-denied and unsupported-browser fallback, and confirm one alert for the initiating person with no private content preview. This version requires an open app tab; it is not browser-closed push.
8. Submit private founder feedback, receive a founder reply and unread indication, retry without duplicate messages, and deny access to teammates/other orgs/non-founder admins. Screenshot controls stay hidden until protected uploads are accepted.
9. Cancel a run, reconnect after a temporary interruption and restart the service during a controlled job. Report the persisted result honestly; do not double-execute prompts or claim exactly-once side effects without evidence.
10. Restore the candidate database and required volumes into isolated resources, verify application consistency, and record the exact recovery procedure without exposing secrets.

## Deployment procedure

1. Finish code/tests and independent review. Pin the candidate commit and record its test results, migration version and image reference.
2. Confirm neev.so ownership and DNS management. Use one explicitly configured HTTPS application origin and its exact GitHub callback. No wildcard callback, public HTTP fallback or placeholder domain with real user credentials.
3. Prepare an isolated staging resource/database/volumes on the existing host, subject to available capacity. Do not replace or reuse another production application's database, volumes, secrets or routes. If the host cannot safely hold staging alongside current services, stop and present the measured capacity issue.
4. Rehearse migration using a verified isolated restore. Confirm operator/member separation, runtime-disabled default and safe rollback boundaries.
5. Prepare the publication clone whose origin is EazybeCode/Workos. The source worktree's older codeg origin must not be pushed. Preserve the branch; do not rename the GitHub repository or delete GitHub data for branding.
6. The owner runs the required interactive `ship` gate in the prepared publication clone. Never bypass it with direct `git push` or publish unreviewed changes to satisfy a launch deadline.
7. Deploy the exact reviewed candidate through Coolify with admission restricted. Confirm migrations, health, HTTPS origin, cookies, OAuth callback and isolation configuration before hosted acceptance.
8. Execute the hosted journeys above. After sign-off, enable access for the initial pilot orgs and observe the service before admitting more.

Rollback must retain private/admission restrictions and preserve written tenant data. Do not automatically downgrade to a legacy shared-token image after team onboarding. Prefer a reviewed forward fix; use an isolated recovery rehearsal to establish whether a particular rollback is safe.

### neev.so website / app.neev.so application cutover

Application cutover prepared September 9; not applied. DNS history: an initial lookup returned NXDOMAIN. After the owner opened Namecheap and authorized setup, a fresh read-only lookup returned `dns1.registrar-servers.com` / `dns2.registrar-servers.com`, apex A `162.255.119.6`, and `www` CNAME `parkingpage.namecheap.com`. Existing MX records point to Namecheap forwarding hosts. These observations superseded the earlier DNS result; the owner's subsequent update is verified below.

Set the apex A record to `5.78.239.36` and `www` CNAME to `neev.so`, TTL Automatic, replacing only default parking/redirect records for those exact hosts. Preserve MX/TXT/email and unrelated records; do not change nameservers. The session has no connected Chrome-control tool, so these steps were supplied to the owner and no registrar write was performed. Verify the saved values and DNS resolution afterward. [Namecheap host-record setup](https://www.namecheap.com/support/knowledgebase/article/9837/46/how-to-connect-a-domain-to-a-server-or-hosting/).

Owner subsequently reported the DNS update complete. Fresh queries to both `dns1.registrar-servers.com` and `dns2.registrar-servers.com` returned apex A `5.78.239.36` and `www` CNAME `neev.so`; the previously observed five forwarding MX records are unchanged. Authoritative DNS is verified, not application availability or worldwide cache convergence. No HTTPS/proxy/GitHub callback change was performed at this checkpoint.

The owner then clarified the website/application split and added A host `app` → `5.78.239.36`, TTL Automatic. Following a brief no-answer propagation interval, both authoritative nameservers and the local resolver returned `app.neev.so` A `5.78.239.36`. This additional record is verified. The application must use the subdomain origin below, not the previously planned apex origin.

No wildcard or per-organization domain is needed for this pilot. Coolify documents A records pointing at the application host, not its hosted control plane. [Coolify DNS configuration](https://coolify.io/docs/knowledge-base/dns-configuration).

Configure the application route for `https://app.neev.so`, targeting the candidate's internal port 3080. Configure the website as a separate resource at `https://neev.so`, with `www` as a separately TLS-verified redirect to the website. Never map either website hostname to the legacy operator app. Coolify requests and renews certificates for HTTPS domains, but can serve a self-signed fallback on issuance failure: verify a publicly trusted certificate before any real sign-in. Keep PostgreSQL and the application container port off the public host interface; only the intended reverse-proxy route is exposed. [Coolify domains and HTTPS](https://coolify.io/docs/knowledge-base/domains).

| Setting | Candidate value / check |
| --- | --- |
| `WORKOS_SERVER_MODE` | `team` |
| Container command | `codeg-server --team`; explicitly override the current Dockerfile's `--supervise` command. Team configuration deliberately rejects the legacy supervisor flag. |
| `WORKOS_APP_ORIGIN` | `https://app.neev.so` |
| `GITHUB_CALLBACK_URL` | `https://app.neev.so/auth/github/callback` |
| `WORKOS_ALLOW_LOOPBACK_PREVIEW` | `false` for the HTTPS deployment |
| `CODEG_HOST`, `CODEG_PORT`, `CODEG_STATIC_DIR` | `0.0.0.0`, `3080`, `/app/web` inside the candidate container, behind its proxy |
| GitHub client credentials, `WORKOS_ENC_KEY`, `DATABASE_URL` | Existing protected configuration assigned to the correct isolated environment; never paste secrets into the plan, frontend or build logs. Do not accidentally rotate the encryption key protecting retained data. |
| `WORKOS_ENABLE_CATALOG_RUNTIME` | Keep `false` for migration/staging. The current auth-only code rejects an enabled runtime; the later reviewed runner integration must explicitly change that gate before agent execution can be enabled. |

In the existing EazyWorkOS GitHub App, add the exact HTTPS callback above and update its homepage to `https://neev.so` during cutover. Keep wildcard matching off. A callback URL is not the post-installation setup URL; the application must continue sending its configured `redirect_uri` explicitly when multiple callbacks exist. Retain a loopback callback only if local development still needs it. Do not rotate the client ID/secret or rename the GitHub App merely for visible branding. [GitHub App callback configuration](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/about-the-user-authorization-callback-url).

Before opening admission, verify the exact team capability response, trusted TLS, `/` redirect to `/workspace`, Secure/HttpOnly cookies, denied legacy routes, actual GitHub return flow, and restricted org admission from a browser outside the host. These are acceptance checks, not completed results. Also validate graceful container termination/restart with the eventual durable runner; the current auth startup implements Ctrl-C shutdown only.

## Personal agent connection acceptance

Owner decision (September 9): subscription-account sign-in is required for the
first pilot, with a Conductor-like experience. An API-key-only launch is not an
accepted substitute. Users sign in to Neev with GitHub, then separately connect
their own agent subscription through the provider's official client flow. This
choice is an architectural requirement, not evidence of implementation, provider
approval, commercial-terms acceptance or permission to reuse host credentials.

Owner-confirmed ownership model (September 9): every member must use their own
subscription for Claude, Codex or any other supported agent. Each member connects
their personal provider account within the selected organization; each submitted
prompt uses that person's connection. Organization-wide conversations share
authorized work and output, not access to another member's credentials or
subscription. No shared company/operator account or automatic fallback to another
person's connection is permitted. If a person is not connected, require their own
sign-in before running their prompt. Connections remain isolated by person and
organization. Other agents are eligible only after their official subscription
sign-in and hosted-use support are verified; listing an agent does not certify it.
The onboarding and chat UI should show connect/connecting/connected/reconnect
states. Ownership is approved; the full runner specification, implementation,
provider conditions and real-account acceptance remain separate requirements.

The first-stage [personal agent connection design](../superpowers/specs/2026-09-09-personal-agent-connections-design.md)
is now owner-approved for implementation. It covers isolated official-client login,
status, disconnect and onboarding/settings, with explicit separation from later
prompt-job execution. Metadata persistence and narrow historical rollback fixes
are accepted through `232aed8c`; see the [storage acceptance](personal-agent-storage-acceptance.md)
and [storage plan](../superpowers/plans/2026-09-09-personal-agent-persistence.md).
The [lifecycle plan](../superpowers/plans/2026-09-09-personal-agent-lifecycle.md)
defines the next transaction/recovery stage. Neither planning nor schema creation
constitutes a working provider sign-in or deployed functionality. The owner has
separately confirmed the Claude hosted-use terms; see the
[provider contract record](personal-agent-provider-contracts.md). Verified encrypted
credential storage and real-account acceptance remain required.

Read-only compatibility finding: the existing Claude registry installs
`@agentclientprotocol/claude-agent-acp@0.74.0` (`acp/registry.rs`), and the adjacent
dependency notes identify its Agent SDK dependency. Do not assume that this legacy
adapter is equivalent to the separately documented unmodified hosted Claude Code
binary path for subscription authentication. The runner design must verify the
actual execution/authentication mechanism and applicable conditions before reuse;
the adapter has not been removed, modified or enabled in team mode by this finding.

Candidate Codex seam verified in official documentation: the unmodified Codex
app-server exposes managed ChatGPT login, including `chatgptDeviceCode`, completion,
cancel, logout and account/rate-limit status. In managed mode Codex owns credential
persistence/refresh; Neev can present the verification link/code without inventing
its own ChatGPT OAuth client. Prefer that documented seam over scraping terminal
output or importing raw auth files, subject to the isolated-runner design and real
account acceptance. This is a design candidate, not a deployed connection.
[Codex app-server authentication](https://learn.chatgpt.com/docs/app-server#auth-endpoints).

Provider authentication and hosted execution are separate release gates. Never reuse the host operator's login across users or organizations. Confirm the supported integration and commercial conditions before presenting a connection method as available; successful local CLI login alone does not establish that a public hosted service is permitted or isolated.

OpenAI documents API-key authentication for programmatic CLI/CI use and a headless device-authentication flow where enabled. Cached CLI credentials are sensitive; they must not enter browser storage, logs, repository files or another user's job. Its security guidance warns against exposing Codex execution in untrusted/public environments. The runner design must explicitly address that deployment boundary before enabling it, rather than treating device-flow support as SaaS approval. [Codex authentication and security](https://learn.chatgpt.com/docs/auth).

Anthropic distinguishes hosting its unmodified Claude Code binary, with each user authenticating through the official flow and paying directly, from offering Claude.ai login or intermediating subscription credentials in a third-party application. Hosted Claude Code is subject to its stated commercial conditions; SDK/product integrations need their supported API/provider authentication. Do not collect Claude.ai session tokens, resell usage, alter built-in authentication, or imply provider endorsement. Resolve any ambiguity with the provider before launch; no terms have been accepted on the owner's behalf. [Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).

Keep unsupported or unverified agents visibly unavailable with an explanation, not a working-looking Connect button. The later reviewed runner/onboarding design must specify credential custody, disconnect/revocation, per-job scope, supported methods and real-user acceptance for each enabled agent.

## Owner inputs before opening access

- Website and app Namecheap DNS are owner-configured and verified. Candidate routing and trusted HTTPS still require acceptance; no domain purchase or registrar change was performed by the agent.
- GitHub organizations to admit to the pilot and a second participant for real consent/collaboration acceptance.
- Founder account selected and its immutable GitHub ID verified; apply only through the protected founder allowlist after implementation. GitHub org-admin status is not founder authority, and no live founder access has been enabled yet.
- Written private-feedback design approval if not yet supplied; existing approval establishes private-to-submitter visibility but not a configured founder role.
- Each participant's own provider consent/credentials, entered through the protected onboarding flow rather than shared in chat.
- First-pilot provider authentication choice is resolved: subscription-account sign-in is required, not an API-key-only fallback. The written isolated-runner/authentication design, supported hosted flows, applicable commercial conditions and real-user acceptance still require completion; no terms have been accepted on the owner's behalf.
- Alert destination and independent approved custody of recovery secrets.
- Interactive publication step and final pilot sign-off.

Detailed evolving state: [public-release-checklist.md](public-release-checklist.md). Authentication evidence: [team-auth-acceptance.md](team-auth-acceptance.md). Recovery evidence: [backups.md](backups.md).

Context: [org-memory: wiki/engineering/workos.md]. The memory describes the older operator-mode fork; current source, reviewed team specs and deployment evidence govern this release.
