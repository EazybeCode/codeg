# Neev: independent identity and organizations

Date: 2026-09-11. Status: Google sign-in, email magic links and optional GitHub
approved in conversation. Owner explicitly chose a fresh start without existing
GitHub-organization compatibility. Owner approved this written design on September11
("sagar.007007@gmail.com is fine / approced design").
Not implemented.

## Outcome and delivery boundaries

A non-technical person signs into Neev without a GitHub account, creates a Neev
organization or accepts an invitation, and sees their own organization and nested
project/folder hierarchy. Company membership is not inferred from an email domain,
Google Workspace membership, a GitHub login, or repository access.

This is subproject one of the approved GitHub-optional direction:

1. Provider-independent accounts, Google/email login, native organizations and
   invitations; adapt the existing principal/session and hierarchy authorization.
2. Repository-optional chats and server-owned documents/revisions, with protected
   files, authorship, compare/restore and deliberate optional GitHub publication.
3. Personal-agent supervision and durable isolated jobs consuming those boundaries;
   then collaboration, wiki, feedback, notifications and hosted release acceptance.

Subproject two preserves projects → recursively nested folders → chats/documents.
Generated documents retain their originating chat and its privacy; folder placement
does not grant broader access. PostgreSQL owns metadata/permissions/revision links;
protected server storage owns file contents. Restoring a revision creates a new
revision. History is not a substitute for off-server backups. Its precise storage,
concurrency and retention contracts require a separate focused design.

Subproject one does not enable prompts, loosen existing repository-required chat
DTOs, create fake repositories, or claim GitHub-free chat execution already works.
The remaining release gates are not waived by approval of this direction.

Owner reconfirmed that each person brings their own AI-tool subscription. Neev
does not supply, fund or fall back to a shared AI account/API key. Users may explore
the workspace without connecting an agent; a prompt requires its author's own
supported connection. Sharing a conversation never shares provider credentials.

## Choice of approach

Selected: Neev-owned identity/membership plus optional GitHub integration. This
supports non-technical teams without asking them to learn repository concepts.
Keep external sign-in identity separate from authorization to company resources.

Rejected: mandatory hidden GitHub accounts/repositories. This preserves the old
implementation but contradicts genuinely optional GitHub and adds onboarding work.
Server-side Git as the universal document-history engine is also not selected:
application-managed revisions better express private documents and restoration.
Git remains useful inside deliberately connected technical working directories.

## Existing source constraints

At630838b4, users.github_user_id, users.org_id and orgs.github_org_id are required.
PrincipalService resolves a personal GitHub credential and membership proof for
every principal; lock_user_identity also requires a positive GitHub ID. TeamSession
and OrganizationMembership already contain stable user/org/member IDs and revision
checks. Catalog admission independently binds every admitted resource to an org.
ChatDto still requires repository/GitHub identity and starting branch fields.

Use fresh native application tables and storage for the new release; no import of
existing GitHub organizations, users or catalog data is required. Reuse tested
revision checks and deny-by-default admission concepts, not the old GitHub identity
dependency. Do not bypass authorization with an unverified Google/email user object.
Old schemas/compatibility routes need not be supported by the new team application.
Retain old databases/volumes and backups offline rather than erase them as part of
setup. No GitHub repository, branch or organization is deleted or modified.

## Accounts and sign-in identities

An internal user ID identifies a person. External identities are unique by provider,
issuer and subject: Google uses its validated subject; GitHub uses its numeric ID;
email login uses a verified email identity under Neev's issuer. Display names and
email strings are never substitute provider subject IDs.

Use dedicated native account, organization, membership, sign-in identity and
verified-email records. Accounts need not belong to an organization before
onboarding. GitHub subjects and repository grants live in optional integration
records, never mandatory account fields or negative/sentinel IDs. Old operator
routes, sessions, tables and token fallback must not be accepted by this application.
There is no email/identity backfill from historical application records.

Do not merge accounts because email strings match. Adding another sign-in method
requires a recently authenticated existing account and successful proof of the new
method, with state bound to that account/session. A provider identity already linked
elsewhere returns a conflict. Email collisions offer an explicit account-linking
journey; they never silently move memberships, credentials or private resources.
GitHub sign-in remains available independently from granting GitHub repository scopes.

Google authentication uses the official OpenID Connect flow with fixed issuer,
validated signature/audience/expiry/nonce, one-use state and exact registered HTTPS
callback. Use an established maintained verification library, selected and pinned
in the implementation plan. Request only openid/email/profile for basic sign-in.
Require a verified email before accepting an email-addressed invitation; an account
claim alone is insufficient. No automatic joining or role assignment by domain.

Email magic links use cryptographically random 256-bit secrets, hash-only server
storage, ten-minute expiration, one-use atomic consumption and bounded attempts.
Bind the request to a pre-auth browser session. Explain that the link must be opened
in the requesting browser; offer a new request on another device. Mail-scanner GETs
must not consume the token or sign in: show a confirmation page and consume by POST.
Carry the secret in a URL fragment and clear it immediately after loading into
memory; no token in logs, analytics, localStorage, referrers or ordinary app backups.
Use a dedicated no-store/no-referrer page without third-party scripts. Email delivery
necessarily sends the one-use link to the configured mail service and recipient;
do not log its body or maintain a plaintext durable outbox.

Use uniform email-request responses independent of account existence. Initial
limits: three requests/address/15minutes, twenty/source-IP/15minutes, and a combined
login/invitation delivery budget of80/day (UTC), ten/minute and two concurrent sends
for the initial free-mail configuration; use keyed address digests for rate keys, bounded cardinality
and expiry. Honor source IP only from explicitly trusted proxies. Invalid/expired
links produce safe actionable errors; provider/mail failures never become sign-in.

## Organizations, membership and invitations

New native organizations are owned by a Neev user, with a separate stable org ID,
name and optional logo. Creation atomically adds the creator as administrator and
records owner identity. Existing admin/member resource permissions remain; being
an administrator does not reveal another member's private chats/documents.

New-organization creation is idempotent and rate-limited (three/user/day initially).
Default invited role is member. Native owner can explicitly grant/revoke admin;
admins may invite/remove ordinary members, not escalate themselves or replace the
owner. Owner removal or leaving is refused until a separately supported ownership
transfer exists. At least one active administrator must remain.

Invitations bind org ID, inviting actor, exact verified recipient email, requested
role, expiry and a hashed random token. Expire after seven days; allow explicit
revocation. Opening the link grants nothing. Acceptance requires sign-in, verified
recipient ownership and explicit named-organization confirmation, then rechecks
the inviter's current authority, organization state and unused invite transactionally.
Revoked/expired invitations and mismatched recipients cannot join. Retried acceptance
returns the same result, not duplicate membership. Limit outstanding invitations
to100/org and sends to20/admin/day initially; revocation does not reset send quotas.

Native membership is authoritative in PostgreSQL. Removal/role change increments
authorization revisions, invalidates affected sessions and pending login/agent
attempts, and denies in-flight write completion after the final recheck. Never hold
database locks across external authentication or email delivery. Establish and test
one user/org/member lock order across login, invite, removal and catalog mutation.

## Optional GitHub integration

All organizations in the fresh application are native Neev organizations. No
github_managed mode, roster synchronization, historical conversion or legacy
organization compatibility is required. Ordinary workspace/settings access needs
no GitHub token.
Only operations involving a connected repository require the acting person's
GitHub connection and repository permission proof. Native membership does not grant
access to a repository; GitHub repository access does not grant Neev membership.
Disconnecting GitHub must not remove a native membership or its documents.

New repository-backed chats retain immutable repository snapshots and visibility
rules. Optional-repository chat support and native-org repository proof
adaptation land in subproject two before advertising those actions to native users.
Until then, show explanatory unavailable actions, not controls guaranteed to fail.

## Session and UI contract

Use an identity-only onboarding session before organization selection, then rotate
to the existing host-only Secure/HttpOnly team session bound to one active org and
membership revision. Retain exact Origin, CSRF, no-store responses and safe DTOs.
Provider credential generation belongs to optional integrations; it must not be
confused with native membership or sign-in-session revocation generations.

Login shows Google, email and optional GitHub. Onboarding offers create organization
or select an existing membership; invitation acceptance shows the exact company.
Settings retain Back to workspace and visible person/org identity, add sign-in
methods and native-member management, and never expose another member's provider
account details. Preserve ten-locale coverage, keyboard controls and mobile layout.
No in-app org switcher is added in this subproject; sign-out/sign-in clears old org
state and allows selection. Do not auto-select based on a matching company name.

## Acceptance and operations

Test fresh schema creation and constraints on SQLite and disposable PostgreSQL.
Deploy into a separate fresh Neev database/storage scope; the old binary must never
point at that database. Establish backups for the new database and document store,
then restore them into isolated resources and test consistency before release.
Preserve old backup archives. Retiring old scheduled jobs must not delete archives
or leave the new application without verified backup coverage.

Cover wrong issuer/audience/nonce, expired/replayed OAuth and magic links, scanner
GETs, wrong-browser links, identity/email collisions, linking CSRF, conflicting
link races, invite replay/revocation/wrong email, owner/last-admin protection,
membership-removal/write races, two-user/two-org access and stale browser state.
Native organization access must work with no GitHub credential or GitHub request.
Existing operator/session credentials must not authenticate against the new app.

Browser acceptance covers Google and email synthetic fixtures clearly labelled as
such, organization creation/invite acceptance, folder nesting, sign-out, keyboard,
mobile, no leaked tokens and useful provider/delivery errors. Real Google, mail
delivery and second-user acceptance require a private HTTPS candidate and actual
participant consent; synthetic tests do not certify those integrations.

Deployment inputs: owner-controlled Google OAuth client/consent configuration and
exact app.neev.so callback; a verified sending domain and transactional email service;
secrets supplied outside Git/chat through the protected deployment mechanism. No
new paid service, mail broadcast, consent acceptance or cloud permission expansion
is authorized by this document. Disabled/unconfigured providers remain visibly
unavailable rather than falling back to operator credentials or development tokens.

Owner has no existing Google Cloud project or mail service and requested setup help.
Initial mail provider is Resend Free, whose currently documented sending ceiling
is100/day. Keep the application budget below that ceiling and show a safe retry
state if exhausted; do not silently upgrade billing. Prefer a dedicated sending
subdomain under neev.so, verified using the service's exact DNS values, preserving
apex website, app and existing mail records. DNS sending authorization needs explicit
confirmation when the exact records are available. Google project neev-508219 and
the Resend sending domain mail.neev.so have now been created. Owner approved the
Google support/contact email and three scoped mail DNS records; those records are
saved and resolve authoritatively. Resend verification remains partially complete.
Google's User Data Policy awaits owner acceptance. No OAuth client, mail API key,
paid resource or application deployment has been created by this preparation.

Keep runtime disabled until the existing isolated-agent and durable-job gates pass.
This design does not claim the remaining website, feedback, notifications or wiki
work complete. Follow the source/publication distinction in the release handoff.

## Sources and review

- Current source: db/entities/{user,org,organization_membership,team_session}.rs;
  team_access/{core,principal,session}.rs; team_server/catalog/{authority,types}.rs.
- Google contract: https://developers.google.com/identity/openid-connect/openid-connect
- Mail limits and domain setup: https://resend.com/pricing and
  https://resend.com/docs/dashboard/domains/introduction
- Prior team-access and personal-agent designs retain their privacy/isolation
  requirements; the owner's September11 direction supersedes mandatory GitHub for
  new workspace identity, membership and future document/chat storage.
- Self-review: separate identity/storage/jobs scope; explicit fresh-start boundary;
  no automatic email linking/domain admission; complete failure/acceptance boundaries;
  no unspecified provider endpoint, native runtime enablement or cloud mutation.

Context: [org-memory: wiki/engineering/workos.md]. Its operator/GitHub-first model
is historical; the latest approved product direction governs this design.
