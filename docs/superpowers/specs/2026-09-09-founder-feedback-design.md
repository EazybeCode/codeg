# Private Feedback for founders

Status: proposed design for owner review. Visibility approved: private to the submitter and authorized founders replying. This is separate from the existing agent/session feedback widgets.

## Outcome and approach

Add an in-app sidebar section named **Feedback for founders**. People can send product feedback and have a private conversation with founders without leaving the workspace. The submitter's teammates, organization administrators, and other organizations cannot browse, search, subscribe to, or download the thread.

Recommended approach: store private support threads in the existing PostgreSQL team application and serve them through the reviewed team authentication layer. Alternatives are forwarding to an external support service (adds an account, processor and notification integration) or GitHub issues (unfamiliar workflow and unsuitable default privacy). Neither alternative is required for v1.

This is a new subsystem, implemented after the accepted team HTTP/authentication boundary. Do not attach it to the legacy operator-token router or reuse agent review/feedback endpoints. Do not expose it publicly while team authority conversion is incomplete.

## User experience

- Sidebar: a persistent Feedback for founders item with an unread-reply badge.
- Main view: the user's threads, newest activity first; status, subject, last reply and unread state. Empty state explains who can read feedback.
- New feedback: subject, category (bug, idea, question), message and optional screenshot upload once the protected attachment layer is verified. Category selection must not slow down submission.
- Thread: readable message timeline, clearly labeled submitter/founder identities, timestamps, composer and status. Preserve unsent text on recoverable errors; pending submission cannot be double-posted.
- Founder inbox: separate authorized view for incoming threads, unread indicator, category/status filters and replies. No organization admin inherits this view.
- Statuses: Open, Reviewing, Planned, Resolved. Founders can change status; the submitter can reply and reopen a resolved thread.
- Notify in-app about replies and status changes. Email, Slack, public voting, public roadmaps, AI-generated replies and internal staff notes are deferred.
- Accessible keyboard navigation, focus management, mobile layout, clear loading/error/retry states and existing locale support.

## Authority and privacy

The submitter is derived from the verified server-side principal; never accept author/user/org/staff identity from request bodies. Preserve the organization context at creation as audit context, not as an access grant. A thread belongs to its submitting account, not to all organization members. No implicit transfer to an organization administrator when a member leaves.

Founder access is a separate explicit server-managed allowlist of immutable verified GitHub user IDs. An empty or invalid configuration grants no founder access. Names, email domains and organization-admin roles are not founder credentials. Configure the owner's verified GitHub ID through a protected deployment setting before acceptance testing; no self-service role elevation.

Each read, reply, status update, unread operation, upload and download must re-check the current principal and thread permission. Only the submitter or an allowlisted founder can act on the thread. Unknown and unauthorized thread IDs return the same not-found response. Founders can access feedback across organizations only through this explicit support boundary; this does not grant access to any private chat, repository, file, credential or agent execution endpoint.

Use current session/Origin/CSRF protections on writes and authenticate subscription delivery. If the scoped event system is not accepted yet, use authenticated bounded polling for unread/activity updates. Never publish feedback content to organization-wide channels, GitHub, the organization wiki or automatic agent context.

Show before submission: “Only you and the founders/support team can read this. Do not include passwords, API keys or other secrets.” Do not automatically capture transcripts, repository contents, environment variables, logs or browser storage. Support staff access/replies/status changes have an audit trail containing actor and resource identifiers, not message or credential content.

## Persistence and API boundaries

Use dedicated tables for feedback threads, messages and per-participant read state; add attachments only through the accepted protected-file service. Keep thread/message IDs opaque. Store server timestamps, submitter identity, creation organization context, category, status, message author and authoritative founder-role projection.

Thread creation and first message commit atomically. Reply creation, activity timestamp and unread state update atomically. Require an idempotency key for thread creation/replies so retrying a request cannot create duplicate messages. Do not execute feedback content as commands or forward it to agents.

API responsibilities: list own threads, create thread, get authorized thread, reply, mark read, and founder-only inbox/status updates. All list/detail responses use explicit safe projections; never serialize complete user/org/database entities. Enforce pagination, bounded text lengths, request limits and per-account submission limits. No global feedback counts leak to non-founders.

If screenshots ship, limit to verified PNG/JPEG/WebP images, maximum 5 MB each and three per message; verify actual file format and strip metadata/re-encode. Store under private opaque keys and authorize every download. No SVG/HTML, executable files, public object URLs or arbitrary remote-URL fetching. If the existing protected upload layer cannot meet this contract, ship text feedback first and keep upload hidden until separately verified.

## Acceptance gates

1. Submitter A creates feedback and sees only A's threads; submitter B in the same organization and C in a different organization cannot read/reply/subscribe/download by guessing IDs.
2. An organization admin has no founder inbox or thread access without explicit founder authorization.
3. An authorized founder can read and reply; removing founder authorization revokes that access. Founder role does not grant any non-feedback workspace permission.
4. Replies, unread badges, statuses, retries and resolved-thread reopening work in two independent browser contexts.
5. Cross-account logout/login clears cached feedback and pending subscriptions; stale requests cannot populate the next account's view.
6. Unauthenticated/expired/revoked sessions, forged author/org/role fields, CSRF failures and oversized requests fail closed.
7. Plain text/Markdown is rendered safely without raw HTML execution; attachment access and format tests pass before enabling screenshots.
8. No feedback content enters an organization event, wiki, GitHub repository, provider prompt or operational log by default.
9. New migrations and restore rehearsal include the feedback tables; deletion/export behavior must be covered by the application's public-launch privacy/data-management work before external release.

## Delivery sequence

1. Finish the current auth final-guard fix and team HTTP/browser onboarding plan.
2. Complete tenant-scoped catalog/chat/event/file routes and their access tests.
3. Implement feedback persistence/authorization, then user UI, then founder inbox and notifications, using test-first changes.
4. Run multi-person and multi-organization browser acceptance and review alongside the public-release gates.

No billing system, organization switcher or public feedback board is required for this version.
