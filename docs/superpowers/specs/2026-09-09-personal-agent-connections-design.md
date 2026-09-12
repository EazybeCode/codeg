# Neev personal agent connections

Date: 2026-09-09. Status: **owner-approved for implementation; not an implementation or deployment result**.

## Approved intent and delivery boundary

Every member uses their own subscription for Claude, Codex or another supported
agent. Multiple providers may be connected. Sharing a chat never grants use of
another person's connection. Each future prompt must use its submitting member's
connection within the selected organization, with no operator or teammate fallback.

This is the first cloud-runner subproject: personal sign-in, connection status,
disconnect, isolated credential custody and onboarding/settings UI. Prompt jobs,
repo/worktree provisioning and chat execution are a subsequent integration stage,
dependent on the reviewed workspace/privacy conversion. A working connection
screen alone must not be described as a complete working team product.

The existing team router has authentication routes only and reports runtime off.
Do not mount the legacy ACP provider, dispatcher, shared AppState or agent settings
to supply the missing behavior. Keep the existing operator/native mode unchanged.

## Architecture and alternatives

Recommended: on-demand, per-connection login sandboxes using the installed
`workos-runsc` runtime on the existing Hetzner host. Persistent private storage
belongs to `(organization, member, provider)`; a stopped sandbox does not consume
an always-running per-user process. A narrow runner supervisor owns lifecycle.

A dedicated VM per person has a stronger machine boundary and simpler personal
home ownership, but adds provisioning and idle capacity; it is not the selected
first-pilot approach. A shared client process with environment-variable switching
is rejected because cached credentials and process state can cross accounts.

gVisor is a defense layer, not an authorization system: it does not prevent access
to files deliberately mounted inside a container. Mount, network, process and
application authorization policies must all be tested. The host administrator
remains trusted; this design does not promise protection from a compromised host.
[gVisor security model](https://gvisor.dev/docs/architecture_guide/security/).

## Separate control and credential planes

1. The existing team server derives member/organization from its checked session.
   It stores only connection metadata and lifecycle intent in PostgreSQL.
2. A separate supervisor accepts a fixed, authenticated operation protocol, never
   arbitrary commands, arguments, paths, images or mount specifications. Only this
   restricted service can request container lifecycle operations. The web app and
   guest containers must not receive the Docker socket or host SSH credentials.
3. The official provider client runs in an isolated login sandbox. It owns native
   credential creation, persistence, refresh and logout. Neev does not implement
   an OAuth token exchange for the provider or import credentials from the host.
4. Each connection gets a random, supervisor-allocated storage handle. Neither
   display names nor user-supplied path segments select directories or volumes.
   The credential volume contains no repository, chat transcript or private wiki.

Credentials must be stored on encrypted-at-rest storage with protected key custody,
not merely encrypted backups. Existing host encryption is not assumed. Real login
remains disabled until encrypted storage and recovery custody are verified.
Database records, telemetry, app logs, browser storage, screenshots, backups of
ordinary app data and Git must never contain provider credentials or login codes.
Provider credential volumes are excluded from ordinary backup/restore; recovery
requires personal reauthentication. Do not claim instant secure erasure of media.

Owner clarification, September 12: ordinary application, supervisor and host
restarts must preserve completed personal connections and their credential state.
RAM-only unlock-key custody that requires login again after every host reboot is
not acceptable. The supervisor must recover protected persistent key custody and
recheck the official client's account status before admitting work. A missing or
unreadable key must fail closed without replacing the key or deleting ciphertext.
Provider expiry or revocation may still require personal reauthentication. This
restart requirement does not change the separate disaster-recovery/backup boundary
above, and does not claim that persistent custody has already been implemented.

The owner subsequently approved keeping the persistent unlock key on the existing
Hetzner host, accessible only to the privileged supervisor, with the host
administrator trusted. No external paid key-management service is required for
this pilot. This does not protect against full host compromise or a full-machine
snapshot containing both the unlock key and encrypted credential storage.

## Provider contracts

### Codex

Use the unmodified official Codex app-server with its managed ChatGPT device-code
login. The private adapter allows only account read, login start/cancel and logout
for this stage; no turn, command, plugin/import or thread methods are exposed.
Neev displays the official verification link/code transiently and waits for the
matching login result, then checks account state. API-key/external-token modes are
not substitutes for the required personal subscription flow. Do not invent a Neev
ChatGPT OAuth client or use the host's cached login.
[Codex account/authentication API](https://learn.chatgpt.com/docs/app-server#auth-endpoints).

### Claude Code

Use the unmodified official native client, not an assumed-compatible Agent SDK
adapter. Run the fixed `claude auth login`, `claude auth status` and
`claude auth logout` operations. An auth process may accept the one-time code that
the official flow asks the user to enter; that input goes only to that process's
stdin and is not persisted or echoed. There is no shell endpoint and no invocation
of interactive `claude`, `-p`, `setup-token`, remote-control or arbitrary commands
in this login service. Status comes from the dedicated machine-readable command,
not a substring match on a terminal banner.
[Claude CLI reference](https://code.claude.com/docs/en/cli-reference),
[official container/remote sign-in](https://code.claude.com/docs/en/authentication).

Hosted use requires the applicable commercial conditions, an unmodified binary,
preserved built-in authentication methods and each user's own direct account.
Neev must not intermediate/resell inference or collect Claude.ai session tokens.
No provider terms have been accepted on the owner's behalf. Do not modify or hide
native authentication methods to enforce Neev policy: a non-subscription account
is reported as an unsupported connection for this pilot, without pretending the
native client lacks that method. Provider ambiguity blocks enablement, not a
fallback to the legacy SDK path.
[Claude hosted-use conditions](https://code.claude.com/docs/en/legal-and-compliance).

### Other agents

Use a fixed server-owned provider registry with explicit support states and
reviewed per-provider adapters. An enabled entry requires a pinned official
client, verified login/logout/status contracts, supported hosted use and real-user
acceptance. Unknown versions or changed output schemas fail closed. No user-defined
binary, arbitrary command or base URL is accepted in this stage. Unverified entries
show “Not available yet” with a reason rather than an actionable Connect button.

## Ownership and lifecycle

One connection row per organization/member/provider holds an opaque ID, generation,
state, safe provider account label, checked-at time, safe error code and internal
storage handle. Account labels are visible only to that member, not org admins or
chat collaborators. Provider identity supplements authentication state; it cannot
change the Neev member or organization to which the connection belongs.

States are disconnected, connecting, connected, reconnect-required, disconnecting
and unavailable. A connected label means the official client verified subscription
authentication; it does not promise remaining quota or a ready execution runtime.
No state is derived solely from the existence of a credential file.
Opening the connection settings triggers a bounded official status check; timeout
shows unavailable, not a false connected result or destructive logout. Future job
admission must recheck authentication rather than trust a cached UI status.

Each start/cancel/disconnect changes a monotonic generation. Login attempts are
bound to the initiating session, member, organization, connection and generation.
Late success after cancel, logout, replacement login, membership loss or disconnect
must not reactivate a connection. One active mutation lease per connection prevents
concurrent writers; browser retries reuse a scoped idempotency key. Reusing a key
with different input returns conflict. Login codes/URLs are memory-only and expire
after the earlier of provider expiry or ten minutes. Server restart expires pending
attempts instead of recovering their codes from logs or the database.

Expiring an unfinished login must not erase an already completed connection.
Saved application content must likewise survive restart. The subsequent durable
job design must preserve submitted work and distinguish interrupted or unknown
outcomes; it must not blindly replay actions that may already have external effects.

Disconnect first makes the connection unusable transactionally, then schedules
native logout and credential-volume cleanup. Cleanup failure leaves it unusable
and visibly pending retry; it must not restore connected state. Provider-wide
revocation is only claimed when verified, otherwise explain that Neev's local
connection was removed. Logging out of Neev invalidates active login attempts but
does not silently delete a previously connected provider account.

## HTTP and authorization contract

The proposed private routes are GET `/api/team/agents`, POST
`/api/team/agents/:provider/connect`, GET
`/api/team/agents/:provider/attempts/:attempt`, POST the same attempt path with
`/input` or `/cancel`, and POST `/api/team/agents/:provider/disconnect`.
They expose typed operations only, never the provider's general RPC or terminal.

All responses are no-store. Mutations require exact Origin, session-bound CSRF,
bounded JSON and a currently verified principal. Attempt/code responses additionally
require the exact initiating session. Member/org/role supplied by the client are
never authority. Foreign or nonexistent opaque resources return the same 404;
expired own attempts have an actionable safe response. Provider output is parsed
into allowlisted DTOs; raw stdout/stderr, tokens, paths and remote errors are not
returned. A bounded in-memory auth-code channel is the only input passthrough.

Reuse the accepted team principal/transaction guard and its lock ordering. Do not
hold database locks while contacting providers or starting containers. Commit
intent first, perform a generation-bound supervisor operation, then recheck current
authority and generation in the final transaction before accepting completion.
A supervisor refusal, timeout or malformed result never becomes authenticated.
Provider URLs must match the pinned profile's exact HTTPS host/path rules; deny
URL userinfo, access/refresh tokens in URLs, arbitrary redirects, IP/local-network
destinations and non-HTTPS schemes. The official flow's bounded ephemeral OAuth
parameters are allowed only on those validated provider URLs, never copied into
Neev's own route query strings or persisted in analytics.

## Sandbox and operational limits

Login sandboxes have no repositories, shared homes, host data mounts, cloud keys,
database access, published ports or privileged capabilities. Use non-root execution,
read-only image root, no-new-privileges and separate bounded scratch space. Images
are pinned by digest with verified provider binaries; no runtime package install.
Only the allocated private credential volume is persistent.
Launch with a minimal server-owned environment, never inherited operator variables
or credentials. In particular, host provider API keys, alternate base URLs, proxy
credentials, Git/SSH helpers and cloud credential variables must not reach a guest.
Credential volumes must sit outside every ordinary application backup include path;
test the effective backup scope before allowing real login.

Use a dedicated network with enforced egress to the provider's verified auth/API
destinations only. Block loopback-to-host, metadata, private/link-local destinations,
other sandboxes and application/database networks, including IPv6 and DNS-rebinding
paths. Browser consent takes place on the provider site, not inside the container.
Do not equate configured proxy variables with an enforced network boundary.

Initial operator-configurable ceilings: one active login per connection, two active
login sandboxes globally, one CPU/1 GiB memory/128 processes per sandbox, 64 MiB
scratch and 64 MiB persistent state per connection, ten-minute attempt lifetime,
five starts per member per fifteen minutes and 8 KiB maximum auth input. Also cap
output buffering and frame sizes at 64 KiB, with an explicit overflow error. These
are pilot safeguards to validate under load, not measured capacity guarantees.
Queue fairly by member; display busy/retry state rather than spawning beyond limits.

## Product experience

After GitHub/org onboarding, show “Connect your agents” with a clear explanation
that each account is personal. Allow connecting more than one agent or skipping to
explore the workspace; sending a prompt will require that sender's connection.
Settings → Agents provides connect/status/reconnect/disconnect and Back to workspace.
Use compact cards, keyboard-accessible controls and status labels, not a raw tools
console. Codes have copy actions and expiry; never put codes in query strings,
notifications, persisted drafts or analytics. Clear them on navigation, invalidation
or logout. Same-scope reload may resume polling but must not recover a lost code
from persistent storage; offer a fresh attempt.

The later chat integration uses the same personal connection projection and shows
the sender's selected agent. No collaborator account labels or auth controls are
published through chat events. Until protected chat routes exist, do not mount the
legacy chat merely to display the selector. Preserve the accepted team client
abort/timeout and stale-scope invalidation behavior; support the existing locales.

## Acceptance and release gates

- Unit/protocol tests: every lifecycle transition, quota, malformed provider reply,
  account-mode mismatch, cancellation, restart and idempotency conflict.
- SQLite plus guarded PostgreSQL tests: same-person/different-org, different-person/
  same-org, admin/other-user denial, simultaneous starts, logout/completion and
  disconnect/completion ordering. Final writes must reject stale authority.
- Actual-browser fixtures: both provider flows, cancel/expiry/retry, multiple
  personal connections, mobile/keyboard, no leaked codes, stale-tab transitions and
  collaborator denial. Invented provider evidence is labeled as such.
- Supervisor integration: inspect actual runtime/mounts, deny forged operations,
  foreign volume handles, other containers, metadata/database/private networking;
  enforce memory/process/disk/output limits and survive supervisor restart.
- Before real sign-in: owner accepts applicable provider conditions, encrypted
  credential custody is verified, pinned clients and egress profiles pass review.
  Only then can the owner and another participant authenticate in private staging.
- Real-account acceptance: each enabled provider's sign-in/status/disconnect works;
  another person/org cannot view or use it. No real credentials are supplied in chat.
- Prompt use remains disabled until workspace/privacy and per-job execution tests
  establish that a job uses only its author's connection and authorized chat data.
  Never mount an entire personal provider home, cross-chat history or another chat's
  unpublished Git objects into an agent job. That execution stage has its own design.

Keep provider-connection enablement separate from `runtimeEnabled`; neither is
enabled merely because this document is approved. Publication uses the Workos
clone; the owner explicitly authorized direct push without the ship command on
September 12. Normal repository hooks still apply, and force-push or unrelated
repository changes are not authorized. This spec itself authorizes no cloud write.

Context: [org-memory: wiki/engineering/workos.md]. Its legacy operator-mode notes
are not the authorization contract for the new multi-tenant service.
