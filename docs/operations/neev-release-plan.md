# Neev staged release plan

September 11, 2026. Public rollout is not accepted. Native identity and native
organizations are Stage 1; completing them does not deliver the full workspace
product. This plan supersedes historical mandatory GitHub admission, legacy
database migration and direct-push instructions.

## Stage 1: independent identity and organizations

Google or email signs a person into a native identity. GitHub is optional and does
not grant native organization membership. Membership is explicit, by company
creation or an invitation addressed to a verified identity. Matching email text
never merges accounts; domains never confer roles. Native projects, folders and
subfolders require no repository. Runtime remains disabled.

Stage 1 includes session/CSRF authority, sign-in-method linking, invitations,
organization branding/member controls and the browser shell. No in-app company
switcher: sign out and sign in to select another company. See
[native acceptance](neev-native-identity-acceptance.md) for evidence and limits.
Historical [team catalog](team-catalog-acceptance.md),
[team HTTP](team-http-acceptance.md) and
[private deployment](neev-private-deployment-2026-09-10.md) records remain available;
their counts, roster authority, credentials and storage do not certify native work.

## Private native candidate

1. Complete local native SQLite/PostgreSQL and actual HTTP/browser acceptance,
   current frontend validation, export and `neev-server` build. Review the exact
   diff and fix scoped blockers; preserve unrelated failures explicitly.
2. Prepare a reviewed native image/entrypoint with `neev-server` and matching
   export using the Node 22 baseline. Existing Dockerfile operator/team targets
   and legacy Hetzner compose are not native candidates. Use committed source;
   never upload scratch, credentials, `.context/` or `.superpowers/` as context.
3. Use a new PostgreSQL database, separate native storage prefix/volume and fresh
   encryption key. Reject old schema markers and cookies. Preserve old databases,
   credential volumes and archives; do not automatically migrate legacy data.
4. Review Google/client, mail sender, exact trusted proxy IPs, HTTPS routing,
   host-only cookies and hardening. Confirm no test issuer/driver in the image.
   Keep runtime false.
5. Recheck the exact old Coolify app's watched branch, automatic deployment and
   proxy routes. September 11 read-only inspection found automatic deployment
   false and the old team target selected; revalidate at release. Do not restart
   the historical whole local Coolify stack or unrelated applications.
6. Publish only reviewed passing Stage 1 source to `EazybeCode/Workos` from the
   publication clone. Never push source origin `EazybeCode/codeg`. Current org
   instructions require interactive `ship`; no direct push or hook bypass.
   Publishing source is not deployment.
7. After scoped deployment review, stage the exact native image on the permitted
   existing unlaunched host with private HTTPS. Verify digest/CMD, non-root UID,
   read-only root, no-new-privileges, dropped capabilities, CPU/memory/PID limits,
   private PG network and no operator credentials or Docker socket.
8. Exercise actual Google and delivered email with two consenting real users in
   same/different companies. Verify callbacks/cookies/proxy, linking conflict,
   invitation, removal and stale-session denial. Fixtures cannot pass these gates.
9. Establish fresh DB/files backups plus isolated restore proof before retiring
   old jobs. Keep old schedules and archives intact; never DELETE a backup
   schedule to detach resources if that removes archives.

The local acceptance task makes no live Coolify/provider/DNS/key mutation, push
or deployment. Prefer a reviewed forward fix after native writes; never downgrade
native users into a shared-token server. Retain the old application offline for
rollback and rehearse recovery without touching original data.

## Later subprojects and public acceptance

Repository-optional chats, documents and revision history require focused designs
and implementation. Personal agent execution requires isolated credential custody,
each person's own subscription, no shared fallback, durable jobs, recovery,
cancellation and real provider acceptance. Runtime stays false until these gates
pass; host gVisor installation alone is insufficient.

Website `neev.so` and the `www` redirect are separate from app `app.neev.so`.
Website sign-in links go to `https://app.neev.so/login`; the website must not handle
app cookies or protected APIs. No `.neev.so` cookie Domain or cross-origin auth
relaxation. Website content, privacy/terms and Google's public branding/policy
readiness remain separate gates.

Founder feedback and inboxes are deferred by latest owner direction. They are not
hidden Stage 1 prerequisites or counted as delivered. Full product acceptance
still requires real chats/docs/jobs tenant/privacy tests, restart/reconnect/cancel/
restore, abuse limits, monitoring and recovery ownership. See
[public checklist](public-release-checklist.md).
