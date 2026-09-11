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

For synthetic Google, open `/__synthetic/google` on the chosen native app host.
This explicitly labelled test-only launcher calls real preauth and login/link
endpoints, uses the current session's CSRF for linking, and permits only the exact
owned issuer `/authorize` URL before navigation. The normal production Google
button correctly rejects the local issuer through its strict Google URL allowlist;
that button's redirect behavior is covered by component tests, not certified by
this synthetic browser launcher. After callback, the real onboarding/workspace
UI is used. Production router and emitted binary must return404 for both launcher
page and script. No production client allowlist or runtime flag is changed.

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

## Local execution evidence and open browser gate

Current native gate: 119 passed, 0 failed, 0 ignored; 3,781 filtered, with only the exact
interactive driver excluded from the native filter. This includes the isolated PG
constraints, concurrent proof consumption, inviter removal and final-write checks.
The emitted test binary's browser-fixture filter separately passed2/2, including
actual HTTP identity/hierarchy/invitation/magic-link roundtrip and production-router
launcher absence. These HTTP results are not browser layout evidence.

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

Controller CUA observed actual captured-email request/explicit confirmation for
two synthetic people on separate literal hosts, no automatic organization join,
company creation, project→folder→subfolder, persisted company rename, Settings
return, input/menu focus and authenticated Settings/workspace at 375px with no
horizontal overflow. These are synthetic local accounts, not real delivery/users.
The first fixture was then stopped normally; its listeners/pools/exact owned schema
and synthetic account/catalog/mail state were removed. Evidence was retained.

Still pending: Google launcher browser flow, invitation explicit acceptance,
complete cross-org/linking/logout/stale-tab journeys, final keyboard checks and
console/network summary. Logo upload was blocked by Chrome's file-upload permission;
an extension popup subsequently blocked further CUA actions. No visual pass is
inferred from backend or component tests. The owner must restore browser control
before the remaining browser acceptance can be recorded.

The native hierarchy's connected outbound-boundary regression passed: its positive
OAuth control observes real fixture requests, then native catalog operations add
zero calls to that configured transport. GitHub stays unmounted and hierarchy
needs no GitHub identity. This is bounded transport/source evidence, not proof of
arbitrary socket absence or a completed browser-wide network audit.

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
