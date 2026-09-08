# Internal team workspace delivery map

Status: product direction approved 2026-09-08. This maps delivery slices; it does not claim implementation or replace each architectural slice's detailed design/review.

## Product contract

Keep the approved repository-backed workspace spec from 2026-09-07. Organization is the company workspace; projects are categories; folders nest arbitrarily; every executing chat resolves to an authorized repository and a distinct writable conversation context. Generated Markdown stays with its chat/repository; eligible reviewed knowledge contributes to the organization wiki later. Existing private chats never become shared automatically.

Use Notion-inspired interaction principles: stable sidebar hierarchy, restrained chrome, clear breadcrumbs and identity, contextual creation, predictable return navigation, and understandable empty/error states. No decorative collaborator avatars or enabled-looking integrations without real backing state.

## Slice 1 — Navigation and private-pilot correctness

- Back to workspace from all web settings pages, including direct URLs/mobile and the originating chat where available. Preserve native desktop behavior.
- Resolve the browser acceptance failures around sidebar hydration, panel context and Files after restart; distinguish test-driver races from product bugs with regression tests.
- Require a bound OAuth login state; installation-originated entry starts a normal bound flow rather than accepting an unbound callback.
- Separate personal preferences, personal agent connections and operator-only settings. Render unsupported optional integrations as unavailable/not connected; do not invent credentials.
- Verify a real GitHub login, selected repository, one authorized agent prompt, generated files, two separate conversation branches, disconnect/reconnect and persisted state.

## Slice 2 — Identity and authorization (internal-team gate)

- Replace shared browser admin-token access with session-derived user and organization authorization. Operator administration is separate from ordinary-member APIs.
- Explicit verified organization selection; never choose the first returned organization as a policy. Model membership and owner/admin/member permissions separately from provider identity.
- Membership revocation blocks subsequent protected operations and live subscriptions. GitHub failures fail closed without silently making a personal organization or broadening access.
- Protect every enabled API, file/download, Git, terminal, provider setting and event stream. Deny or disable legacy surfaces that lack equivalent checks.
- Personal provider credentials remain inaccessible to collaborators. A deliberate organization-managed provider connection is a separate administrator-controlled option, with disclosed usage attribution.
- Acceptance: two independent users in one organization collaborate within their rights; an unauthorized/non-member session cannot list/read/write/control/subscribe; removal stops access; private chats remain private.

## Slice 3 — Agent onboarding and prominent chat controls

- Guided flow: GitHub → verified organization → authorized repositories → connect one or more agents → connection test → start a chat.
- Reuse supported native provider authorization mechanisms, not an embedded password-collection form or copied operator credentials. Device authorization and API-key routes are capability-dependent; connection UI reports pending/connected/expired/failed honestly and redacts logs.
- Chat shows agent, model, supported effort, supported fast mode, and connection/reconnect state near the composer. Hide unsupported controls; disclose cost implications when supported capabilities provide that information.
- Selecting an unconnected agent opens the same setup flow and returns to the retained draft. Changes during an active run apply to the next run or require an explicit stop/restart; never silently retarget a live writer.
- Acceptance: connect/reconnect/disconnect, expiry, denied consent and retry; drafts persist; unsupported controls never issue unsupported backend settings; one user's credentials are never exposed to another.

## Slice 4 — Real shared chat

- Shared conversation header: creator, latest prompt author, active viewers and current run initiator. Presence is ephemeral and expires on disconnect; it is not an audit record or proof that an agent is running.
- Persist immutable server-derived prompt author, organization, conversation, message ID, timestamps and run identity. Do not trust a client-supplied display name/user ID and do not infer authorship from the agent transcript alone.
- Use organization/conversation-authorized event channels for live responses, presence, comments and mentions; reconnect replays durable message/run events with cursor/deduplication rather than duplicating prompts.
- Exactly one active writing run per conversation. Multiple members can submit through a visible server-managed queue; queued prompts record their author/provider authority and are reauthorized at execution. A viewer does not inherit the initiator's credentials or writer control.
- Owners/admins can stop or hand off under explicit policy; a handoff never gives someone another member's provider secret. Running work and queued draft ownership are visible.
- Sharing is explicit. New chats in deliberately shared project contexts disclose their audience at creation; private contexts remain private. Preserve existing private visibility on migration.
- Acceptance: simultaneous sends produce ordered, attributed prompts; reconnect does not duplicate a run; unauthorized users cannot see presence or events; revoked queued work does not execute; viewer navigation does not stop or steal a writer.

## Slice 5 — Cloud execution and operations

- Isolated runners with scoped files, credentials, processes, network policy and resource limits. Worktrees are Git isolation, not a process-security boundary. Avoid an always-on VM per organization where an independently validated sandboxed-runner pool meets the boundary.
- Durable jobs, leases, idempotency, cancellation and explicit interrupted/retry states. Do not automatically replay a potentially side-effecting prompt after a crash without known execution state and a safe policy.
- Private/internal HTTPS or VPN access, off-server database and repository/provider-volume backups, restore rehearsal and operational visibility.
- Internal release requires scoped authorization, one real provider run, two-user tests, persistent-file/history verification and the existing browser regression suite green. External organizations additionally require validated cross-tenant runner isolation and adversarial security tests.

## Slice 6 — Knowledge and upstream maintenance

- Reviewed publication of conversation branches/files; no automatic merges into starting branches or publication of full private transcripts.
- Organization chooses its wiki destination. Compile only eligible shared/reviewed work with provenance, redaction, permission-aware reads and an explicit review/publish boundary.
- Upstream monitoring is specified separately in `2026-09-08-upstream-update-automation-design.md`; it must not bypass any release gate above.

## Outstanding operator inputs

Real provider choice/authorization and any usage budget for automated adaptation; GitHub App installation/repository grants; shared-access domain or VPN choice; off-server backup destination. None permits weakening membership checks or using another person's credentials.
