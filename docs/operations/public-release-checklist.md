# Public release checklist

September 12, 2026: not ready for public rollout. Native identity and native
content have separate evidence records. GitHub is optional; admission uses native
identity and explicit native membership. Keep `runtimeEnabled:false`.

## Stage 1 local review gate

- [x] Reviewed identity, session, linking and native organization code; four final
      Important findings fixed and independently re-reviewed through `abb69509`.
- [x] Fresh SQLite/PostgreSQL constraints, races and final-write authority tests.
- [x] Actual native HTTP Google fixture begin/callback/session, tenant denial and
      email scanner/explicit-confirm paths; actual browser hierarchy, invitation,
      conflict, stale-tab/logout, Settings return, keyboard and 375px checks.
- [x] Recorded full native/frontend suites and earlier export/`neev-server` build;
      final fixes additionally passed native 125/125, Settings 9/9, TypeScript and
      touched-file lint/format. Export, production binary and full frontend suite
      were not rebuilt/rerun after the final fixes; warnings/failures are retained.
- [x] Exact diff and secret findings reviewed; environment contract recorded in
      [native acceptance](neev-native-identity-acceptance.md).
- [x] Native content automated HTTP regression uses two participants over one
      schema/shared owned blob store; cross-listener read, author rows, stale-save
      conflict, append-only restore, privacy revocation and removal denial pass.
- [x] GET-only content preflight passes unit tests and runs against the actual
      owned HTTP fixture with an in-memory synthetic session and sanitized counts.
- [x] Controller recorded the actual Chrome content journey, 375px measurements,
      dialogs/keyboard, stale draft clearing and malicious-Markdown network
      observations, including scoped fixes/retest for three observed defects, in
      [native content acceptance](neev-native-content-acceptance.md).
- [x] Controller completed the fixture-backed metadata/blob backup and isolated
      restore/verifier exercise, including a missing-blob negative copy and exact
      source-fixture cleanup. This is local evidence, not hosted recovery custody.
- [ ] Reviewed passing source publication to Workos through current interactive
      `ship` gate. Never source codeg direct push; no automatic deployment.

Unchecked boxes remain release gates, not a claim of no partial evidence. Use the
acceptance record for exact results, never historical counts. Synthetic providers
do not certify real Google or delivery.

## Private native candidate gate

- [ ] Reviewed native image digest/CMD and matching export. Existing operator/team
      Docker targets are insufficient; no test fixture in the production image.
- [ ] Fresh native PG database, separate files and fresh encryption key. Old
      schema/cookies rejected; historical data and archives preserved.
- [ ] Private HTTPS, exact trusted proxy peer list and spoofed-forwarded-header
      denial; host-only cookies, exact callbacks and secret-free access logs.
- [ ] Owner-approved Google client/policy/consent. Basic `openid email profile`
      Testing sign-in is exempt from the ordinary test-user allowlist restriction;
      zero listed users alone is not a blocker. Actual HTTPS/account/workspace
      acceptance and public branding/policy readiness remain separate gates.
- [ ] Apex Resend domain all records verified, approved send-only key and
      `verification@neev.so` sender. Actual consenting recipient receives an
      unmodified link. Synthetic `support@neev.so` has no inbox; old verified
      `mail.neev.so` does not establish apex verification.
- [ ] Two actual consenting users exercise same-org access and other-org denial;
      joining remains explicit and linking never merges existing accounts.
- [ ] Fresh DB/files backups, independent recovery custody and isolated restore
      before retiring old jobs. Never incidentally delete schedules/archives.
- [ ] Hosted aggregate disk/capacity monitoring, alert ownership and backup-time
      headroom are accepted. Per-organization quotas do not prove finite shared
      server capacity for any number of organizations.
- [ ] Exact old Coolify auto-deploy/routing rechecked; native app/proxy/rollback
      reviewed. No old operator mounts, credential reuse or Docker socket.

## Full public product gate — later work

- [ ] Repository-optional chats and document revisions with private/org authority
      across reads, writes, downloads and events. Automated, local-browser and
      isolated-restore evidence exists; hosted/public gates remain open.
- [ ] Personal agents use each person's subscription and isolated credentials;
      no operator/org/shared fallback or another member's credential. Real
      provider/client acceptance and custody remain required.
- [ ] Personal subscription onboarding and visible chat agent/model/effort controls
      are backed by real supported provider capabilities, not invented controls.
- [ ] Prompt/reply attribution and collaborator visibility in shared chats prove
      author-scoped agent execution; native human-message rows are insufficient.
- [ ] User-controlled response notification/ping preferences and actual completion
      delivery do not reveal private content across accounts or organizations.
- [ ] Organization wiki generation/indexing respects source privacy; shared manual
      documents do not prove synthesis or private-data exclusion.
- [ ] Durable jobs, isolation, quotas, cancellation, restart/recovery, reconnect
      and abuse controls integrated and accepted before enabling runtime.
- [ ] Separate `neev.so` website/`www` redirect, privacy/terms and truthful content;
      app remains `app.neev.so` with host-only cookies. Neev favicon/app-icon
      consistency replaces the inherited public icon family.
- [ ] Google public branding/policy readiness, monitoring/recovery ownership and
      complete real-user public acceptance.

Founder feedback and inboxes are deferred. Historical team metadata, gVisor host
readiness and old backup proofs do not complete native runtime or new backup
acceptance. No launch date is asserted.

## Separate maintenance gate

- [x] Tracked upstream automation is read-only discovery on a source-defined
      six-hour schedule.
- [ ] Automatic adaptation, proposal, merge and deployment are implemented,
      reviewed and accepted separately. No current GitHub run/default-branch state
      was checked by the native content audit, and Task 8 performs no workflow,
      upstream-fetch, merge or deployment action.
