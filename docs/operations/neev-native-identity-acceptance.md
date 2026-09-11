# Native identity acceptance and environment contract

September 11, 2026. Local synthetic acceptance is separate from real providers,
hosted deployment and public product readiness. Runtime remains false. This
milestone does not implement chats, document revisions or agent jobs.

## Reproducible local gate

Use Node 22.23.2, prepending `/opt/homebrew/opt/node@22/bin` to PATH for all child
processes. Use serial Rust (`CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=1`), existing
`src-tauri/target`, and monitor disk. `NEEV_TEST_DATABASE_URL` must identify only
the isolated database `neev_identity_test`. Tests create UUID schemas, close
pools and drop only exact owned schemas. Never substitute a production URL.

```sh
node --test scripts/neev-identity-preflight.test.mjs scripts/neev-native-launcher.test.mjs
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features 'neev::' -- --include-ignored --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1
pnpm exec vitest run
pnpm exec tsc --noEmit
pnpm build
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --bin neev-server
git diff --check
```

Run touched-file ESLint/Prettier/Rustfmt. Record the actual emitted Rust test
binary/counts and run explicit native filters. Only the exact long-running browser
driver is excluded above; every other guarded PG test runs.

## Owned synthetic browser fixture

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev::browser_fixture::browser_acceptance_driver -- --exact --ignored --nocapture
```

Launch with a PTY: stdin EOF stops the driver. It prints two app origins and a
capture-inbox origin, never token values. Type a line to stop; a 45-minute timeout
also stops listeners, closes pools and removes the newly owned schema. Do not
kill the process as ordinary cleanup. After abnormal exit, verify exact schema
ownership before any cleanup; never broadly delete old schemas or volumes.

Actual native routers listen on literal `127.0.0.1` and `[::1]` ephemeral ports,
sharing one fresh schema/key. The synthetic issuer URL is `localhost`, bound to
loopback, so native host-only cookies do not reach it. Ports alone do not isolate
cookies. Verify actual browser host/cookie behavior. Two hosts in one profile are
synthetic participants, not two real users or independent browsers. Same-origin
tabs cover BroadcastChannel/stale-session behavior.

For synthetic Google browser checks, open `/__synthetic/google` on the IPv4 native
app host; use captured email confirmation for the isolated IPv6 participant.
This explicitly labelled test-only launcher calls real preauth and login/link
endpoints, uses the current session's CSRF for linking, and permits only the exact
owned issuer `/authorize` URL before navigation. The normal production Google
button correctly rejects the local issuer through its strict Google URL allowlist;
that button's redirect behavior is covered by component tests, not certified by
this synthetic browser launcher. After callback, the real onboarding/workspace
UI is used. Production router and emitted binary must return404 for both launcher
page and script. No production client allowlist or runtime flag is changed.

Known fixture limitation: Chrome completed the IPv4 chooser/callback flow, but
the IPv6 chooser return remained blocked despite its exact-origin CSP header.
Literal IPv6 is not supported by the specified
[CSP host-source matching](https://www.w3.org/TR/2026/WD-CSP3-20260309/#match-hosts).
HTTP header equality is not proof of browser CSP matching. Dual-Google browser
coverage is incomplete; the observed IPv4 result is not a cross-browser guarantee.
Do not broaden CSP, change browser protections or add another origin to hide this
limitation. Use the established email flow for participant two.

The test issuer implements authorize, a labelled fixed-account chooser, one-use
short-lived codes, PKCE, nonce-bound signed tokens, discovery and JWKS. Real
native begin/callback/session routes do the authentication. Three closed fixture
identities have distinct subjects/emails. Only two exact owned local callbacks
are accepted. Issuer, captured mail and driver are `cfg(test)` only and absent
from the production server. There is no native seed route or browser cookie
injection.

Captured mail contains synthetic fragment links only to registered local origins.
Alternate-origin links are explicitly rewritten for isolation tests; this is not
nonrewritten delivery evidence. Never save token URLs, OAuth state/code/nonce,
mail bodies or cookies in logs/screenshots. Use provided CUA controls with one
browser controller; no invented contexts or cookie editing. No external providers,
mail or network configuration are involved.

Run `NEEV_PREFLIGHT_ORIGIN=<printed-app-origin> node scripts/neev-identity-preflight.mjs`.
This credential-free read-only script accepts only explicit literal loopback HTTP
origins with ports, refuses redirects/credentials/paths, checks native/runtime-false
capabilities and legacy route absence.

Required browser evidence: Google fixture login → company → project → folder →
subfolder; email request → scanner GET → confirm; recipient two → explicit join;
other-org denial; linking conflict; stale session/logout; Settings return;
keyboard and 375px. Capture token-free screenshots and console/network summaries;
distinguish extension errors. Zero GitHub calls requires connected network/service
observations, not an unused counter or arbitrary-socket claim. Execution results
are recorded after the integrated gate.

## Consolidated local evidence awaiting final review

Current emitted-binary native gate: 121 passed, 0 failed, 0 ignored; 3,781 filtered,
5.53s, exit0, with only the exact interactive driver excluded. The controller ran
the guarded PostgreSQL native filter after the fixture fixes; this includes
constraints, concurrent proof consumption, inviter removal and final-write checks.
Historically the initial full native gate passed119 and the focused fixture filter
passed2/2. After fixture
fixes through `5b6ab8bc`, the focused filter passed4/4, 0 failed, 0 ignored: actual
HTTP identity/hierarchy/invitation/magic-link roundtrip with chooser-policy checks,
production-router launcher absence, server-panic cleanup, and unrelated/native
cookie handling. The current 121-test full native run includes those added
regressions. It does not imply a new frontend or production-binary build. HTTP
results are not browser layout or CSP-enforcement evidence.

Frontend: 459 files / 6,279 tests passed with `--maxWorkers=2 --minWorkers=1` and no
concurrent Rust compilation. The initial default-worker full run had 6,278 passes
and one 5,000ms native-settings pagination timeout while Rust compiled. The unchanged
exact test and settings suite passed independently, followed by the complete
6,279-test rerun. No assertions or timeouts were weakened. TypeScript and the five
preflight/launcher Node tests passed. Existing Vite deprecation, React test/HTML
warnings and Rust unused-import/linker/future-compatibility notices remain recorded.

`pnpm build` passed compilation, TypeScript and all 34 static pages. The production
`neev-server` build passed; its actual-binary smoke passed 16/16 against a newly
created isolated PostgreSQL database, including both synthetic launcher paths
returning404, old-cookie denial and safe callback recovery. The smoke removed its
exact owned database and temporary assets. This is a local development binary,
not a hosted image or deployment acceptance.

Controller CUA completed the following actual Chrome checks across recorded
synthetic runs; these are not real provider/delivery/users or independent browsers:

- On `5b6ab8bc`, IPv4 Google launcher → chooser → callback → onboarding as support
  → Neev Google Acceptance → Marketing/Campaigns/Launch passed, including keyboard
  Return, autofocus and hierarchical breadcrumbs.
- Earlier captured-email request/scanner GET/explicit confirmation, organization
  creation, persisted rename, Settings return and 375px Settings/workspace checks
  passed with no horizontal overflow. On `e971582a`, native file-picker selection
  → logo preview → save → reload visibly rendered the image; removal/save/reload
  also passed. Earlier picker failures were controller interactions, not a proven
  product defect; no logo code change was needed.
- Recipient two used email/explicit confirmation on IPv6 and created Independent
  Sales, without access to the first organization's catalog before admission.
  Linking that recipient's existing email to support's Google account produced a
  safe generic failure; reload retained support's Google identity only, with no
  merge. The named invitation required explicit Join and retained the recipient's
  current second organization. The owner saw Member; administrator → member role
  changes persisted on reload.
- Recipient sign-out cleared another same-origin participant-two tab. Email
  sign-in again showed both organization choices; explicit selection of the first
  organization displayed Marketing. This supplies actual stale-tab/logout and
  explicit organization-selection evidence.

The final consolidated Chrome run at `c38025ff` repeated synthetic Google login
through callback/onboarding, created Neev Final Check and a Sales project, and
opened Settings. Native DevTools Responsive Width was set to375; actual
`innerWidth` and `scrollWidth` both measured375 for workspace and Settings, with
screenshots confirming both. Enter opened navigation, New project focused Name,
Return saved and restored trigger focus, Escape closed navigation and restored
Open navigation focus, and Enter navigated to Settings. These measured checks
supersede the earlier unsuccessful controller-API viewport override. Emulation
was disabled and DevTools closed; desktop width/scrollWidth returned to1440.

Network recording with preserved log began before that Google flow. Native API
reads/login/select returned200 and creates201; a GitHub substring URL filter found
0 matches among207 recorded requests. Captured console errors were extension
`feature_flags` TypeErrors, not observed application errors. Earlier safesearch
iframe interference remains disclosed. This is bounded recorded-flow evidence,
not arbitrary-socket absence, a pristine browser or real-provider certification.
Direct top-level API navigation in another owned tab was browser-blocked and is
inconclusive, not a passed tenant-denial check. Tenant denial is supported by the
actual HTTP regression and the recorded two-organization browser journeys.

Local synthetic milestone evidence is consolidated for independent whole-branch
review, not final approval. IPv6 Google return remains a fixture limitation; use
email for participant two. That limitation, the HTTP fixture timeout Minor and
other deferred findings require final-review triage. Older and final fixtures
stopped normally with exact schema/listener cleanup; owned tabs were closed and
native emulation/DevTools reset. No active acceptance fixture remains. These
results do not approve Task 8, implement subproject two, authorize deployment or
certify real Google/mail, hosted Stage 2 or public readiness.

The native hierarchy's connected outbound-boundary regression passed: its positive
OAuth control observes real fixture requests, then native catalog operations add
zero calls to that configured transport. GitHub stays unmounted and hierarchy
needs no GitHub identity. Together with the recorded browser flow, this is bounded
transport/source/URL evidence, not proof of arbitrary socket absence or all
possible browser traffic.

## Fresh deployment contract

| Configuration/resource | Native candidate requirement |
| --- | --- |
| `NEEV_APP_ORIGIN` | `https://app.neev.so`; exact Google `/auth/google/callback`, optional GitHub `/auth/github/callback` |
| `NEEV_DATABASE_URL` | New explicit PostgreSQL database; reject old markers instead of migrating |
| Native storage | Separate fresh prefix/volume; no old operator/repository/credential mounts |
| `NEEV_ENC_KEY` | Fresh 32-byte secret as 64 lowercase hex characters, separate recovery custody |
| `NEEV_BIND` | Reviewed private listener; no public publication before route review |
| `NEEV_TRUSTED_PROXY_IPS` | Exact normalized peer IPs; actual hop and spoofed X-Forwarded-For denial, no broad subnet trust |
| `NEEV_STATIC_ROOT` | Absolute matching reviewed export directory |
| `NEEV_ALLOW_LOOPBACK_PREVIEW` | Unset/false on deployment; fixture uses existing local preview support |
| Google client | Owner-approved paired `NEEV_GOOGLE_CLIENT_ID`/`NEEV_GOOGLE_CLIENT_SECRET`, basic scopes only |
| Mail | Approved send-only `NEEV_RESEND_API_KEY`, `NEEV_MAIL_FROM=verification@neev.so`, apex all records verified |
| `NEEV_RUNTIME_ENABLED` | Unset/false until isolated-agent/durable-job gates pass |

Never reuse old keys/cookies/databases/storage. No legacy migration/drop is
authorized. Prove fresh initialization and old marker/credential refusal, new
DB/files backup and isolated restore. Retain old jobs/archives until replacement
coverage/restore is proven; never DELETE schedules as incidental detachment.

Real Google policy/consent/client and private HTTPS acceptance remain open. Basic
scope Testing login is exempt from usual test-user allowlisting; zero listed users
alone is not a blocker. Account/workspace restrictions can apply; public branding/
policy is separate. Real mail needs approved DNS/key, all-record verification,
an actual consenting recipient and an unmodified delivered link. Synthetic
`support@neev.so` has no inbox and proves no receipt.

Do not edit live Coolify services until staged acceptance and scoped deployment
review. Existing Dockerfile targets are legacy; reviewed native image/entrypoint
and hardening are still required. Source publication uses the Workos clone and
current interactive ship gate, never codeg direct push. See the
[release plan](neev-release-plan.md) for later chats/docs/jobs, website, backup and
full public gates. Founder feedback/inboxes are deferred.

Historical context: [org-memory: wiki/engineering/workos.md]; its old operator and
mandatory GitHub model is superseded by the approved native direction.
