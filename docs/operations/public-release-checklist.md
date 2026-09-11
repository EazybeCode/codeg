# Public release checklist

September 11, 2026: not ready for public rollout. Stage 1 native identity is a
separate milestone. GitHub is optional; admission uses native identity and explicit
native membership. Keep `runtimeEnabled:false`.

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
- [ ] Exact old Coolify auto-deploy/routing rechecked; native app/proxy/rollback
      reviewed. No old operator mounts, credential reuse or Docker socket.

## Full public product gate — later work

- [ ] Repository-optional chats and document revisions with private/org authority
      across reads, writes, downloads and events.
- [ ] Personal agents use each person's subscription and isolated credentials;
      no operator/org/shared fallback or another member's credential. Real
      provider/client acceptance and custody remain required.
- [ ] Durable jobs, isolation, quotas, cancellation, restart/recovery, reconnect
      and abuse controls integrated and accepted before enabling runtime.
- [ ] Separate `neev.so` website/`www` redirect, privacy/terms and truthful content;
      app remains `app.neev.so` with host-only cookies.
- [ ] Google public branding/policy readiness, monitoring/recovery ownership and
      complete real-user public acceptance.

Founder feedback and inboxes are deferred. Historical team metadata, gVisor host
readiness and old backup proofs do not complete native runtime or new backup
acceptance. No launch date is asserted.
