# Team access and chat visibility

Date: 2026-09-08

Status: written specification approved by the user ("looks fine"). This document is a design, not a claim that these controls are implemented or deployed.

## Approved contract

- Organization is the company workspace, not a project. Preserve organization → projects/categories → arbitrarily nested folders/working directories → repository-backed chats.
- Verified GitHub organization members may join automatically. GitHub organization owners map to WorkOS admins; other members map to WorkOS members. Never assign administrator rights merely because someone is new.
- Chats have two explicit audiences: `private` and `organization`. The UI labels them **Private** and **Everyone in [organization name]**. There is no public-link, guest, cross-organization sharing or cross-organization move in this release.
- Existing chats remain private. New chats default to private, with the audience visible before creation. Being inside a shared project/folder does not silently broaden a chat's audience.
- Organization membership is necessary even for a person's own private chat. A former member cannot keep accessing that organization's work through a saved URL or an old session.
- No two organizations may share application-visible chat content, files, credentials, searches, live events, presence, execution authority or wiki inputs accidentally.

## Scope and chosen approach

Add a deny-by-default team HTTP/event boundary to the existing Rust backend and use the existing workspace UI and catalog. Do not expose the legacy router to team sessions and then rely on scattered handler fixes. Converting every legacy feature first would preserve more optional functionality but expand the initial audit substantially; replacing the backend wholesale would discard tested repository/workspace behavior. The separate team surface is the selected approach.

This specification covers identity, tenant scope, chat visibility and their client/API authorization contract. Isolated provider runners, durable jobs, collaborative prompt scheduling and off-server recovery remain separate implementation slices that must pass their own release gates. Chat visibility may be implemented before running agents is safe; it must not enable the runtime by itself.

## 1. Identity and organization membership

Use a stable GitHub user ID for identity and stable GitHub organization ID for tenancy. Names/logins are display and lookup metadata, never authorization keys. A GitHub identity may belong to more than one organization, but each browser session operates in exactly one explicitly selected organization. Membership in A and B is not permission to use A resources while the session is scoped to B.

Introduce an organization-membership relation keyed uniquely by `(org_id, user_id)`, with role, active/revoked/locally-blocked state, verification timestamp and authorization revision. A local administrator's block survives OAuth login and membership refresh; only an authorized administrator can lift it. Automatic joining must not undo a local block. Bind sessions to a specific organization and membership. Existing `users.org_id` and `users.role` fields remain compatibility data while legacy code is isolated; team authorization must never use them as its source of truth. In particular, switching organizations must not rewrite a user's global tenant field and reinterpret existing sessions/resources.

OAuth flow:

1. Keep the existing browser-bound OAuth state validation before exchanging a code.
2. Resolve the GitHub identity. Keep tokens encrypted on the server. A new identity awaiting organization selection uses a browser-bound, single-use encrypted onboarding record expiring after ten minutes, not a fabricated organization or privileged workspace session. Onboarding records authorize only membership discovery and selection, never workspace operations. Successful selection consumes the record transactionally; expired records and their encrypted credentials are cleaned up.
3. List the authenticated user's active memberships with pagination. Show an explicit organization picker, including when there is one eligible organization. A pending invitation is not membership; never accept invitations or change GitHub membership on the user's behalf.
4. Revalidate the selected organization's stable ID, active membership and role server-side. Only then transactionally create/update the identity, organization and membership and issue a fresh organization-bound session. Preserve existing organization display branding.
5. If membership cannot be verified, show a retry/reconnect or access-denied state. An empty/failed GitHub response never creates a personal organization or grants owner access.

GitHub documents active/pending membership states and supports checking the authenticated user's organization membership with a GitHub App user token and the Members read permission. The implementation will use those documented endpoints and verify response identity, rather than infer membership from a client-supplied organization name. [GitHub organization membership API](https://docs.github.com/en/rest/orgs/members#get-an-organization-membership-for-the-authenticated-user).

A positive GitHub membership proof may be cached for at most 60 seconds, keyed by stable user, organization and credential generation. Concurrent requests share a verification request. Expired proof is never extended on a timeout, rate limit, malformed response or upstream failure; protected activity waits or fails closed. Admin actions and new execution/publication requests require a fresh proof. Local revocation is authoritative immediately for subsequent authorization checks. Existing streams cannot send while proof is expired and renewal is pending. This is bounded revalidation, not a promise of instantaneous detection of GitHub-side changes.

Old sessions that have no organization binding must reauthenticate/select their organization. Old owner roles are not trusted during this transition. Existing organization rows and private chat ownership are preserved, not reassigned to the latest person signing in.

## 2. Session and route boundary

One central resolver produces `AuthContext { session_id, user_id, org_id, membership_id, role, authorization_revision }` from the session cookie and current membership. Duplicate/ambiguous cookies, expired/revoked sessions, mismatched membership and database failures fail closed. No protected operation accepts a client-supplied user ID, role or organization ID as authority.

Team mode is an explicit server configuration, validated at startup. The cloud team deployment cannot fall back to the legacy router because authentication configuration is missing. Existing desktop/native mode stays separate for compatibility.

- Team routes are registered in a single explicit manifest with their action policy; unknown/unported routes are denied. The member API, personal-provider API and operator functions are distinct.
- Initially enable only verified identity/org selection, current-org settings, repository/catalog metadata and chat create/list/read/visibility operations. Each additional route family is enabled only with its resource authorization tests.
- File/Git/terminal, provider installation/settings, global backups, system updates, generic ACP, public download tickets, office proxies and global event streams are not inherited from the legacy router. Unsupported UI controls must be absent or explicitly unavailable.
- A shared operator token never authenticates a team route. Remove the OAuth response's `CODEG_TOKEN` bridge, stop storing/sending it in team browsers and clear old browser token storage on entry. Native desktop transport is unchanged. Break-glass server operations stay outside the browser/team surface and are disabled by default.
- Team HTTP and WebSocket requests use the session cookie. State-changing requests require the exact configured origin plus a session-bound CSRF token; same-origin WebSocket upgrades are mandatory. Do not trust arbitrary forwarded-host headers or use wildcard CORS.
- Session cookies are HttpOnly, SameSite=Lax and Secure for HTTPS. HTTP is permitted only for the explicitly configured loopback private-preview origin, never as a generic public-cloud fallback. Authentication responses are no-store.
- Logout revokes the server session and closes its subscriptions. Switching organizations rotates the scoped session and clears previous-org client caches, tabs, subscriptions and drafts from the rendered workspace. Browser tabs detect a changed session/org and reload their scope instead of reusing stale state.

Use stable public errors: unauthenticated 401; denied known action 403; missing or unauthorized resource 404; stale revision/busy transition 409; unavailable verification/storage 503. Never include other-organization names, resource existence, token material or host paths in error responses.

## 3. Visibility, ownership and repository authority

Persist chat `visibility` with a database constraint limiting it to `private` or `organization`, plus a monotonic access revision. Backfill every existing managed chat to private without changing its creator, repository snapshot, branch/worktree or transcript mapping. Records lacking a proven tenant/creator mapping remain quarantined from team routes; do not infer ownership from a filesystem path or expose legacy rows as shared.

| Action | Creator in current org | Other active member of same org | Admin of same org | Anyone in another org |
| --- | --- | --- | --- | --- |
| Read private chat and its metadata | Yes | No | No special override | No |
| Read organization-wide chat | Yes | Yes | Yes | No |
| Change a chat's audience | Yes | No | No special override | No |
| Manage organization settings/membership | Only if admin | No | Yes | No |
| Submit an agent run | Subject to visibility, own provider/repo authority and runner gate | Organization-wide only, same prerequisites | No credential override | No |

Admins do not get an ordinary-app backdoor into private chats. This is application authorization, not end-to-end encryption against infrastructure operators with database/backup access.

Expose a single resource-authorizer used by lists, direct reads, modifications and subscription setup. It resolves the chat under the authenticated organization before evaluating visibility and returns narrowly scoped capabilities, never a blanket global path permission. Lists, counts, recent items, search, notifications and sidebar badges apply the same predicate, so even titles/existence do not leak. Cache keys include organization, user, resource and access revision where applicable.

Audience changes require creator authorization, explicit confirmation showing the organization name and an expected access revision. Changing private → organization shares the complete app-visible chat history and declared chat artifacts, not only future messages; the dialog states this. Changing organization → private revokes other viewers and removes the chat from their future lists/subscriptions. It cannot recall already downloaded content. Visibility changes are rejected as busy while a writer/job is active; do not silently move an active run into a changed audience. Future queued prompts must be reauthorized before execution.

Audience changes never move the chat to another organization or grant use of the creator's provider/GitHub credentials. A shared conversation may be viewed by the selected organization, but browsing unrelated repository contents, cloning, pushing or executing still requires the acting user's independently verified repository authority. Shared chat output is an explicit publication to the selected audience; it is not a blanket repository-access grant.

**Git privacy boundary:** Private means private in WorkOS. A GitHub branch is not made private by this setting. Never automatically push a private chat's transcript, generated Markdown or unpublished files, or feed them into the organization wiki. Explicit Git publication must disclose the destination and its separate audience. Changing a chat back to private does not remove previously published GitHub content. Never delete actual GitHub data to simulate revocation.

## 4. Events, files and downstream consumers

Do not expose the current global WebSocket broadcaster or unscoped attach/replay to team sessions. New subscriptions resolve authorized organization/chat IDs; clients cannot select a global channel or arbitrary connection ID. Reauthorize on subscription, replay, every outbound batch and changes to membership/session/access revision. Purge server-side pending output after revocation. Previously delivered bytes cannot be recalled.

Scoped file/download/preview routes must derive their root from the authorized resource, not accept a caller's absolute path. Canonical containment, symlink safety and resource ownership checks are required; download capabilities are scoped, short-lived and reauthorized at consumption. Do not enable these routes until their tests pass.

Search, presence, collaborator names, run authorship, attachments, history, audit projections and wiki compilation must all consume the same authorization contract. Prompt authorship is server-derived and immutable. Presence alone is not execution authority. Private chats are excluded from organization wiki ingestion. A shared chat made private must cease being a source for future compilation; previously published knowledge has a separate, disclosed publication/retraction workflow and is not silently deleted.

Tenant isolation must also hold beneath HTTP: jobs, credentials, caches and filesystem roots carry immutable organization/user/resource identities. A later runner must not mount another org's data or a shared Git object database containing another private chat's unpublished commits. Worktrees isolate writes but are not a confidentiality boundary. This specification does not certify runner isolation; execution stays disabled until the separate sandbox/job acceptance succeeds.

## 5. UI contract

- Keep the existing Notion-inspired sidebar hierarchy and contextual New chat actions under projects/folders. Do not restore a global orphan chat action or add a separate Documents hierarchy.
- Creation dialog includes a labelled audience control, defaults to Private and displays the exact organization for the organization-wide choice. Include visibility in retry/idempotency fingerprints so a retried request cannot create a differently shared chat.
- Version creation fingerprints so legacy private requests still retry against their original format. The original creation audience is immutable request metadata, distinct from current visibility: retrying creation never resets an audience changed later through Share.
- Chat header displays a lock or organization icon with a text label. Creator can open Share and confirm audience changes; other members see the current audience without an enabled-looking sharing control.
- A private chat's project/folder may be shared, but its title/count/preview is excluded from other members' navigation. The UI must not imply that organization admins can read it.
- Preserve unsent drafts on recoverable errors. On membership/org/session change remove inaccessible content from view; do not carry it into another organization's chat or automatically send it after reauthentication.
- Static Next export remains supported using fixed pages and query parameters. All labels/errors are translated in the existing ten locales. Keyboard access and narrow-screen behavior are acceptance requirements.

## 6. Tests and release criteria

Use isolated SQLite and PostgreSQL fixtures and independent browser contexts for Alice/admin and Bob/member in org A, Carol/admin in org B, and a user belonging to both organizations.

Required tests:

1. Login never exports an operator token. Pending/unverified/failed memberships cannot join. All new users are not automatically owners. Organization choices are revalidated by immutable ID.
2. Session scope cannot be switched by JSON/header/query forgery. Expired/revoked cookies and ambiguous cookie headers fail closed. One person's org switch cannot retarget another session or existing resources.
3. Private chats are creator-only, including titles, counts, search, files and events; org admins have no implicit read override. Organization-wide chats are visible only to active members of that same org.
4. Reused/guessed resource IDs, another org's repository/node, mismatched connection IDs and forged creator fields cannot cross tenants. Test a person who belongs to both orgs while actively scoped to only one.
5. Audience creation retries preserve the original audience. Concurrent audience changes use revisions; non-creators are denied; active writers block visibility changes. Old chats remain private after migration and rerun.
6. Demotion/revocation/logout closes or suppresses subsequent event delivery and denies new actions. Simulate stale proof, upstream outages and a controlled clock; never extend expired membership proofs on error.
7. Route-manifest integration tests show shared tokens cannot unlock team endpoints, unknown legacy routes remain denied and public capability/proxy routes cannot bypass the boundary. Origin/CSRF/CORS and download consumption are tested explicitly.
8. When file/event routes are introduced, test cross-org paths, traversal, symlink escape, foreign chat files and arbitrary attach/replay, including reads raced with visibility changes.
9. Two-browser tests cover private creation, explicit sharing, colleague access, return to private, revoked access, stale deep links, cache clearing and same-user org switching. No hidden global-stream compatibility path is permitted to make tests pass.
10. Existing hierarchy/navigation, branch/worktree identity, chat reload/restart and ten-locale regressions stay green. Tests with fake providers are labelled and do not substitute for real-provider or adversarial sandbox acceptance.

Only reviewed, tested immutable commits may be deployed. Keep the current pilot private and runtime disabled during migration and intermediate slices. Before migration, create and verify a backup; use an isolated restore for migration tests, never production test identities. Rollback must not expose newly written tenant data through an older shared-token server: preserve private network restrictions and do not automatically downgrade to a legacy-access image after team onboarding.

## Implementation sequence after written-spec approval

1. Membership/session schema, pending onboarding and verified selection; centralized principal resolver and migration tests.
2. Team route manifest, session/CSRF web transport, scoped identity/settings/catalog bootstrap and fail-closed legacy separation.
3. Chat visibility persistence, shared/private authorization, creator-only audience mutation and Notion-style sharing UI.
4. Scoped event/read-resource adapters and two-org browser/HTTP acceptance. Unported functionality remains disabled.
5. Independently designed isolated providers/runners and durable collaborative jobs; then real-user/provider acceptance, off-server recovery and team rollout.

No extra server credentials are needed to write this slice. Real GitHub App organization access must be validated with the user during acceptance. Off-server storage and each person's provider consent remain external inputs for later release gates.

## Design self-review

- Product direction preserves nested folders, repository-backed chats and explicit private/organization visibility.
- All organization boundaries use the same session-derived principal; historical user fields are not mixed with the new membership authority.
- Migration preserves data and defaults to private; no existing GitHub content is deleted or privacy promises made about already published branches.
- Separate runtime/job/recovery gates are explicit; completing this design or the first API slice does not claim team readiness.
- Tests include a dual-organization user, administrators versus private chats, secondary metadata/event leaks and revocation races, not only happy-path row filtering.

Context: [org-memory: wiki/engineering/workos.md]. The wiki describes the older single-operator fork and SQLite storage; the inspected source at 15c3fb8e supports PostgreSQL and is authoritative for this design.
