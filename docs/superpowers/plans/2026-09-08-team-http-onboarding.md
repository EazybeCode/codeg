# Team HTTP and Browser Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide a running, deny-by-default team server with browser-bound GitHub onboarding, explicit organization selection and organization-scoped session bootstrap.

**Architecture:** A dedicated TeamState and router consume the reviewed authentication services without constructing legacy AppState. Standalone startup selects team mode before legacy supervisor/helper/path/restore/agent initialization. A public capability handshake selects a separate browser authentication shell before any legacy global providers mount.

**Tech Stack:** Existing Rust/Axum/SeaORM/Tokio, static Next export, React, next-intl, Vitest and existing cached Playwright. No new production dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`

Execution checkpoint (2026-09-09): Tasks1–2 accepted through `4c290570`; Task3 accepted through `23675b01`; Task4 accepted through `6f8a453c` after scoped fix re-review and fresh actual-browser verification. Final whole-plan review and its one fix wave are accepted through `b7a2f180`, with no open findings. The task steps below preserve the implementation recipe; acceptance is scoped, not a public-release claim.

Final checkpoint: owner disk cleanup resolved the earlier build pause. `391c872d` fixes missing navigation payloads; `6f8a453c` strengthens four browser-test gaps; `b7a2f180` adds two advertised icon routes and strengthens the operator publication-order test. Fresh final-fix controller verification passed 37 server tests, six affected UI tests, scoped lint and browser evidence `artifacts/team-auth-e2e-BnjlvV`. Earlier 189 Rust/184 UI tests, 34-page export/TypeScript, standalone build and eight startup controls are recorded with their provenance in `docs/operations/team-http-acceptance.md`; the standalone binary was not rebuilt after the final icon fix. All reviews are clean. Nothing has been deployed; real-provider acceptance and complete workspace functionality remain separate.

## Global Constraints

- Team mode is an explicit server configuration, validated at startup. The cloud team deployment cannot fall back to the legacy router because authentication configuration is missing.
- Existing desktop/native mode stays separate for compatibility.
- A shared operator token never authenticates a team route.
- Onboarding records authorize only membership discovery and selection, never workspace operations.
- State-changing requests require the exact configured origin plus a session-bound CSRF token; same-origin WebSocket upgrades are mandatory.
- Session cookies are HttpOnly, SameSite=Lax and Secure for HTTPS. HTTP is permitted only for the explicitly configured loopback private-preview origin, never as a generic public-cloud fallback. Authentication responses are no-store.
- No protected operation accepts a client-supplied user ID, role or organization ID as authority.
- Static Next export remains supported using fixed pages and query parameters. All labels/errors are translated in the existing ten locales.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No real credentials, GitHub/provider requests, cloud writes, push or deployment in implementation tasks. Use invented credentials, isolated local databases and loopback fake providers.
- One Cargo process at a time, `CARGO_INCREMENTAL=0`; preserve unrelated edits and use existing dependencies.

## Prerequisites, boundaries and ownership

User scope update (2026-09-09): omit the in-app organization switcher for v1. Keep explicit organization selection during sign-in and safe logout/re-login, including prior-cookie rotation and cross-tab invalidation. Browser organization-transition tests use logout followed by actual sign-in/selection, not a new switch endpoint.

Execute after `2026-09-08-team-auth-orchestration.md` is accepted. Consume its CredentialService, OnboardingService, PrincipalService, TeamClock and static TeamAuthError interfaces without weakening them. No migration is added here.

This plan exposes authentication and a safe signed-in shell only. Catalog/settings/chat audience routes are the immediately following conversion plan; until reviewed they remain absent from the team manifest, not forwarded to legacy services. No team runtime/event/file/provider route is enabled here. Do not deploy an intermediate shell as a finished product.

### Task 1: Validate team configuration and initialize only its database

**Files:** Create `src-tauri/src/team_server/mod.rs`, `config.rs`, `database.rs`, `config_tests.rs`, `database_tests.rs`. Add public `team_server` module to lib.rs; inner modules/types remain crate-visible except the later standalone entry functions. If the accepted auth fixtures are narrower than crate-visible, modify only cfg(test) visibility in team_access/mod.rs/test_auth.rs so the sibling team_server tests can reuse them; do not duplicate their clock/database/provider infrastructure. No codeg_server.rs wiring yet.

**Interfaces:**

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerMode { Operator, Team }
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TeamConfigError {
    #[error("invalid or conflicting WORKOS_SERVER_MODE")]
    Mode,
    #[error("team mode cannot run supervisor or credential-helper flags")]
    Flag,
    #[error("invalid WORKOS_APP_ORIGIN or loopback-preview configuration")]
    Origin,
    #[error("GITHUB_CALLBACK_URL must match WORKOS_APP_ORIGIN/auth/github/callback")]
    Callback,
    #[error("invalid GITHUB_CLIENT_ID or GITHUB_CLIENT_SECRET")]
    Github,
    #[error("invalid WORKOS_ENC_KEY")]
    EncryptionKey,
    #[error("DATABASE_URL must identify a PostgreSQL database")]
    Database,
    #[error("invalid CODEG_HOST or CODEG_PORT")]
    Bind,
    #[error("CODEG_STATIC_DIR must be an existing absolute directory")]
    StaticRoot,
    #[error("WORKOS_ENABLE_CATALOG_RUNTIME must remain disabled")]
    Runtime,
}
pub fn select_server_mode(args: &[String], configured: Option<&str>)
    -> Result<ServerMode, TeamConfigError>;
pub fn select_server_mode_from_os(args: &[String], configured: Option<&std::ffi::OsStr>)
    -> Result<ServerMode, TeamConfigError>;
pub(crate) struct TeamConfig {
    pub(crate) origin: reqwest::Url,
    pub(crate) callback: reqwest::Url,
    pub(crate) github_client_id: String,
    pub(crate) github_client_secret: String,
    pub(crate) encryption_key: String,
    pub(crate) database_url: String,
    pub(crate) bind: std::net::SocketAddr,
    pub(crate) static_root: std::path::PathBuf,
}
impl TeamConfig {
    pub(crate) fn from_values(values: &std::collections::HashMap<String, String>)
        -> Result<Self, TeamConfigError>;
    pub(crate) fn from_environment() -> Result<Self, TeamConfigError>;
}
pub(crate) async fn initialize_database(config: &TeamConfig)
    -> Result<sea_orm::DatabaseConnection, crate::team_access::core::TeamAuthError>;
```

TeamConfig has no Debug or Serde. `select_server_mode` accepts `--team` or WORKOS_SERVER_MODE=team; missing mode with no team flag retains existing standalone operator compatibility. Explicit operator and `--team` conflict. Unknown/empty configured mode is fatal. When Team is selected, reject `--supervise` and `--credential-helper`; never invoke either legacy code path. The deployment must set both `--team` command and WORKOS_SERVER_MODE=team so removing one cannot silently enable operator mode.

At the environment boundary distinguish an absent variable from a non-Unicode value: a configured but undecodable mode is Mode, not None/operator fallback. Required undecodable configuration values fail their static field group; do not log raw bytes. Tests can exercise pure value validation without mutating process environment alongside other tests.

Task1 owns the pure public `select_server_mode_from_os` adapter and re-exports it from team_server for the separate standalone binary crate. It converts a present OsStr with `to_str().ok_or(TeamConfigError::Mode)` and otherwise delegates to `select_server_mode`; it does not read the environment. Include absent/valid/invalid-Unicode controls (Unix OsStrExt bytes where supported). Task2 reads WORKOS_SERVER_MODE once with `var_os` before legacy startup and passes `as_deref()` into this adapter. Do not drop undecodable values with `.ok()` or `.and_then(to_str)`.

Read only these environment values once: WORKOS_SERVER_MODE (selection outside config), WORKOS_APP_ORIGIN, GITHUB_CALLBACK_URL, GITHUB_CLIENT_ID, GITHUB_CLIENT_SECRET, WORKOS_ENC_KEY, DATABASE_URL, CODEG_HOST, CODEG_PORT, CODEG_STATIC_DIR, WORKOS_ALLOW_LOOPBACK_PREVIEW, WORKOS_ENABLE_CATALOG_RUNTIME. Reject any runtime value other than absent/false in this slice.

Require explicit origin, callback, OAuth configuration, key, Postgres URL and absolute existing static root. Validate OAuth/key through reviewed constructors without network calls; startup diagnostics identify the static field group above but never display configured values or underlying parser/database errors. Origin must be a tuple HTTPS origin or, only with explicit loopback-preview=true, HTTP host literal127.0.0.1/::1. No userinfo, query, fragment or non-root path. Callback must equal this origin plus `/auth/github/callback`. Reject localhost aliases, public HTTP and wildcard hosts. DATABASE_URL accepts only postgres/postgresql scheme, host and database name present; preserve its original value for the driver without logging it. CODEG_HOST must parse as an IP address, default0.0.0.0; CODEG_PORT default3080, otherwise valid nonzero u16. Explicit loopback preview can bind the container's private interface for an SSH tunnel; it does not authorize publishing the HTTP port.

Dedicated DB initialization uses one migration connection and an independent runtime pool, existing Migrator::up, max runtime connections5,10second connect timeout, SQL logging false. Require Postgres; test fixtures may directly construct SQLite state but production initialization never falls back. Do not call db::init_database, apply pending restores, write global app metadata, hydrate custom agents/folder links, inspect provider caches or create legacy data paths. Static startup errors do not leak URLs/keys/SQL. Close migration connection before constructing runtime pool.

- [ ] **Step1: Write configuration and initialization tests.** Include mode conflicts/missing auth/no fallback, publicHTTP/aliases/wrong callback, secret-free errors, malformed DB/key/port, and rejection of team supervisor/helper/runtime flags. Use complete valid controls, e.g.:

```rust
assert_eq!(select_server_mode(&["codeg-server".into(), "--team".into()], None), Ok(ServerMode::Team));
assert!(select_server_mode(&["codeg-server".into(), "--team".into()], Some("operator")).is_err());
let valid = valid_loopback_values(&exported_fixture_dir);
assert!(TeamConfig::from_values(&valid).is_ok());
let mut public_http = valid.clone();
public_http.insert("WORKOS_APP_ORIGIN".into(), "http://192.0.2.10".into());
assert!(TeamConfig::from_values(&public_http).is_err());
```

Postgres test uses the accepted auth test helper's exact disposable database guard, proves migration/runtime pool works and legacy credential/provider/filesystem rows are not promoted/hydrated. No test production identities or automatic restore.
- [ ] **Step2: Run RED** `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_server -- --test-threads=1`; record actual failure.
- [ ] **Step3: Implement focused modules** using the signatures and validation rules above; keep static error conversion at config/database boundary.
- [ ] **Step4: Run GREEN** focused config/database tests, explicit ignored disposable Postgres test and existing team_access regressions; `git diff --check`.
- [ ] **Step5: Self-review and scoped commit** `feat(server): add validated isolated team startup configuration`.

### Task 2: Serve the explicit authentication manifest with safe cookies

**Files:** Create `src-tauri/src/team_server/state.rs`, `router.rs`, `cookies.rs`, `handlers/mod.rs`, `handlers/oauth.rs`, `handlers/onboarding.rs`, `handlers/session.rs`, `projection.rs`, `limiter.rs`, `static_files.rs`, `startup.rs`, `http_tests.rs`, `test_server.rs`. Update `src-tauri/src/team_server/mod.rs` exports, `src-tauri/src/bin/codeg_server.rs` early mode dispatch, `src-tauri/src/web/router.rs` only to expose the safe operator capability response outside its bearer middleware, and `src-tauri/src/team_access/session.rs` plus `session_tests.rs` for the shared strict cookie parser. test_server is cfg(test)-only and reuses auth test fixture infrastructure; no duplicate database/clock/fake-source implementations.

Keep responsibilities explicit: router.rs owns the sole method/path/policy manifest; handlers/mod.rs owns shared static response/error and extraction handling; handlers/oauth.rs owns login/callback; handlers/onboarding.rs owns pending choices/selection; handlers/session.rs owns session bootstrap and synchronized local logout. cookies.rs owns named-cookie and Origin/CSRF wire handling without duplicating the reviewed secret primitives. projection.rs owns safe JSON DTOs, decimal GitHub ID validation and scope fingerprint; limiter.rs owns the shared bounded auth budget; static_files.rs owns the exact static page/asset boundary. startup.rs constructs only team services/runtime and handles graceful shutdown. Module-internal helper signatures may stay private; no second router manifest or legacy command dispatcher is introduced. This decomposition keeps the route families independently readable without changing their contracts below.

Review follow-up file split: the required zero/multiple-org, prior-session/other-device rotation, malformed-cookie restart and preserved-branding HTTP cases may live in a focused cfg(test) module `src-tauri/src/team_server/http_onboarding_tests.rs`, registered in mod.rs. Reuse test_server.rs fixtures/flow helpers; do not duplicate clocks/providers/database scaffolding. Cleanup cancellation tests must cover an in-progress operation through the same private future helper consumed by startup, not only cancellation during the timer wait.

Task1 exposed only the PostgreSQL fixture helpers it needed. Task2 additionally owns necessary `cfg(test)` visibility adjustments in `src-tauri/src/team_access/test_auth.rs` for its shared controlled clock, fake sources and fixtures. Raise only reused items to pub(crate); do not duplicate that infrastructure or expose a production test-provider override. Task4 separately owns its specified loopback HTTP-adapter constructor visibility.

Task2 additionally owns the minimal `src-tauri/src/team_access/onboarding.rs` interface change to return the existing pending row's `csrf_hash` in crate-private `OnboardingChoices`. HTTP bootstrap uses it to verify the supplied HttpOnly pending-CSRF secret before projecting that secret. The stored hash and internal pending fields must not enter serialized HTTP DTOs or logs. Cover a valid pending-cookie/wrong-CSRF-cookie rejection and response secret/hash absence. Do not change selection or credential authority for this projection.

Task2 also owns crate-private `OnboardingService::cancel_attempt(&self, state: &str, browser_secret: &str) -> Result<(), TeamAuthError>` in onboarding.rs for browser-bound provider-denial callbacks. Reuse authenticated hashing, live_attempt and the existing atomic conditional consume_attempt; no code exchange, decryption, pending/session issuance or duplicated HTTP persistence. Only a successfully bound live cancellation can produce access_denied; invalid/expired/wrong-browser/replayed attempts map invalid_login and database failures unavailable. Reject ambiguous code+error callbacks. HTTP tests prove one-time valid denial, forged binding cannot consume the genuine attempt, zero exchanges/pending/sessions, and recovery through a fresh login. No new lock protocol or schema change is needed.

Startup wiring may enable Tokio's already-installed `signal` feature in Cargo.toml for graceful Ctrl-C and remove the obsolete dead-code suppression on GithubMembershipClient::new. No new package or provider endpoint override is authorized. Preserve static TeamConfigError field-group diagnostics without echoing configured values. Startup tests must exercise the path actually consumed by main, not a duplicate unused selector.

**Interfaces consumed:** Task1 TeamConfig/initialize_database and completed authentication services. **Produces:** `pub fn run_team_server() -> std::process::ExitCode`; crate-private `build_router(state: Arc<TeamState>) -> axum::Router`; TeamState contains db/config/vault/oauth/onboarding/credentials/principals/clock and a bounded authentication limiter, but never legacy AppState, global credentials or event broadcaster. Task3 consumes the JSON contracts below.

One manifest lists exact method/path/policy and constructs only these handlers:

| Method/path | Policy / response |
| --- | --- |
| GET `/` | Public fixed303 redirect to `/workspace`; no cookie issuance, client-controlled destination or workspace authority. The protected browser shell performs its normal session bootstrap. |
| GET `/api/workos/capabilities` | Public, no-store `{version:1,mode:"team",runtimeEnabled:false}`; operator router returns mode:"operator" separately. |
| GET `/healthz` | Public static `{status:"ok"}`, no configuration/details. |
| GET `/auth/github/login` | Begin attempt, set browser cookie, redirect to reviewed authorization_url; ignores all client redirect/actor/org parameters. |
| GET `/auth/github/callback` | Strict unique bounded state/code or static provider-error handling; consume valid browser-bound attempt and issue pending cookies;303to/onboarding. An entry with neither state nor attempt-browser cookie restarts at `/auth/github/login` without exchanging any supplied code. |
| GET `/api/team/onboarding` | Browser-bound pending lookup; `{identity:{githubUserId,login,name,avatarUrl},organizations:[{githubOrgId,login,avatarUrl,role}],csrf}`. GitHub IDs are decimal strings; no local user ID exists yet. |
| POST `/api/team/onboarding/select` | Exact Origin + pending cookie and CSRF header; JSON only `{githubOrgId:"canonical-positive-i64-decimal"}` with unknown fields rejected; selection and browser-session rotation. Return `{redirect:"/workspace"}` only after commit. |
| GET `/api/team/session` | Verified Cached principal; `{user:{id,login,name,avatarUrl},organization:{id,login,name,avatarUrl},role,scope,csrf}` with local i32 IDs and a validated current CSRF secret. |
| POST `/api/team/logout` | Local strict session cookie+Origin+matching CSRF, exact session deletion; no GitHub verification dependency. |

Return stable JSON error codes/statuses: invalid_request400, unauthorized401, forbidden403, not_found404, conflict409, unavailable503; body never echoes paths/queries/provider errors. Map Query/JSON/body-limit extraction failures into the static invalid_request response instead of exposing framework rejection diagnostics. Rate limiting returns429 with static retry copy and Retry-After. No wildcard CORS. GET authentication responses and errors have Cache-Control:no-store and Referrer-Policy:no-referrer. Response projections never serialize database entities or internal secret-bearing structs. User projection `{id,login,name,avatarUrl}`; org projection `{id,login,name,avatarUrl}`; local i32 IDs remain JSON numbers and role is `admin|member`. The session scope fingerprint is a SHA256-derived opaque value of the checked session hash, not the raw session cookie; it changes on browser-session rotation, not ordinary GitHub credential refresh. Existing branding is preserved by the service and shown here. Choice rows contain githubOrgId/login/avatarUrl/role, never local forged authority. Project GitHub i64 IDs as canonical decimal strings to avoid JavaScript precision loss; selection accepts that string shape only (1..19digits, no leadingzero, checked positive i64 parse) before passing the exact integer to the service. Include values beyond Number.MAX_SAFE_INTEGER and i64::MAX boundary tests, as well as unknown/forged org rejection.

Use host-only cookies with Path=/,HttpOnly,SameSite=Lax,Secure iff validated HTTPS, explicit max-age matching service expiry; no Domain attribute. Names: workos_team_session, workos_team_csrf, workos_team_oauth_browser, workos_team_pending, workos_team_pending_csrf. On successful callback expire attempt browser cookie; on selection expire pending cookies and set new session/CSRF; logout expires session/CSRF. Failures never issue a workspace cookie. Factor the existing strict session cookie scan into `pub(crate) fn browser_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, LocalSessionError>` in session.rs; names are always server constants. Existing validated_cookie wraps it for workos_team_session and maps absence to Unauthorized. Preserve every existing resolver test; reject duplicated/malformed named cookie across headers. Do not accidentally treat malformed prior session as valid None at selection: restart flow clears malformed old cookies before beginning a fresh attempt.

CSRF raw secrets stay in HttpOnly cookies; bootstrap may return only the secret whose SHA256 matches the checked session or pending row. Require exactly one X-WorkOS-CSRF header and constant-shape validated secret, exact configured Origin on writes, application/json content type,16KiB request body bound. Callback rejects duplicate fields before code exchange. Preserve the reviewed installation-entry behavior: when both state and browser-attempt cookie are genuinely absent, discard any installation-originated/unbound code and303to the local login-start route. Missing state with an existing cookie, missing cookie with state, malformed/duplicate cookie or field, and empty values remain failures; none is an invitation to exchange unbound code. Provider descriptions never enter output or logs. No query/body/header can supply a prior session or arbitrary actor; select passes the actual parsed cookie to OnboardingService.select. Logout works with revoked/expired membership or provider outage by checking the exact local session row's CSRF and deleting only it; a database failure is503 and must not claim server-side revocation succeeded.

Require exactly one Origin header and byte-for-byte equality with `config.origin.origin().ascii_serialization()` (which correctly excludes the URL root slash); do not normalize an incoming attacker-supplied URL into an allowed origin or trust forwarded-host fields. Test duplicate Origin headers, userinfo/path/query/fragment/comma variants and normal browser serialization as the positive control.

Browser-only callback failures redirect303 to fixed local `/login?reason=invalid_login`, `/login?reason=access_denied` or `/login?reason=unavailable`, never a supplied destination or provider description. No workspace cookie is issued; expire the attempt-browser cookie when its flow cannot continue. Map only fixed internal outcomes to these reasons, and log only those static outcome labels. This preserves friendly retry/reconnect after denied consent, invalid state or upstream outage; the JSON API routes retain the exact error statuses above. Add explicit callback-failure tests proving zero unbound exchanges and no session issuance, as well as successful recovery through a new login attempt.

Serialize logout against the reviewed final mutation guard: inside its local database transaction, acquire the referenced membership row lock when that row exists, then reread the exact cookie-selected session and its CSRF binding before deleting it and committing. Acquire no user/org locks afterward. The session may have expired or its membership may be revoked/blocked; logout still terminates it without GitHub verification. A missing membership cannot make an otherwise CSRF-authorized session impossible to terminate. Do not reuse the older session-deletion helper as a substitute for this synchronization. Include deterministic PostgreSQL cases in which logout waits behind an already-authorized mutation, and in which a guard waiting behind logout rejects the deleted session after acquiring the lock. The operation grants no workspace authority and never deletes another session.

Bound begin with an in-process token bucket capacity100/refill1persecond, monotonic time, and maximum32 concurrent auth operations; no forwarded-IP trust. Wrap authentication handler execution, including request body reading, in a60second tokio timeout returning static503. This permits the service's separately bounded GitHub calls while bounding slow body/DB waits; do not spawn detached mutation tasks to escape cancellation. Background task calls cleanup_expired every60seconds with shutdown cancellation; rate limit before storing attempts. Service errors remain static. These are single-instance pilot load bounds, not a multi-replica global quota guarantee.

Static serving is explicit: exported `/login`, `/onboarding`, `/workspace`, `/settings` and assets rooted in validated static directory; no universal index.html fallback. Unknown `/api`, `/auth`, `/ws`, downloads, proxies and paths return404 even if the SPA could render them. Reject unsupported methods. Do not inherit legacy file/terminal/ACP/settings/backups/updater routes. Request logging includes only matched route labels/status, never raw URI/query/headers/body.

Standalone main parses version first, then selects team mode before prime_self_exe, supervisor/helper, PATH/data-dir mutation and legacy logging. Read `std::env::var_os("WORKOS_SERVER_MODE")` once and pass its `as_deref()` to Task1's public `team_server::select_server_mode_from_os(&args, configured)`; a non-Unicode value must reach its Mode error rather than disappear through an optional string conversion. Team branch runs its own runtime/stderr logging, config/database/service setup/router and graceful Ctrl-C shutdown; it returns directly and never enters legacy async_main. Team startup failure returns nonzero with static diagnostic. Operator branch retains existing behavior. Only informational version requests (`--version` and its existing `-V` alias) are allowed to bypass required team configuration; both print the package version and exit before side effects, never start a router/helper/supervisor. Include both aliases as startup controls.

- [ ] **Step1: Write HTTP tests** using real Axum routing, real isolated DB and controlled provider sources. Assert exact cookies/no operator token, browser binding, pending-only access, CSRF/origin/ambiguous cookies/oversized JSON, explicit organization selection for0/1/multiple, preserved branding, rotation/otherdevices, logout during upstream failure, route denial and no fallback. Include:

```rust
let forbidden = server.post("/api/team/onboarding/select")
    .add_header("Origin", "https://attacker.invalid")
    .json(&serde_json::json!({"githubOrgId":"10201"})).await;
forbidden.assert_status_forbidden();
server.post("/api/acp_connect").add_header("Authorization", "Bearer invented-operator").await
    .assert_status_not_found();
assert_eq!(oauth.exchange_calls(), exchanges_before_forged_request);
```

Use unit fixture clock for limiter, no60second sleeps. Startup tests exercise invalid team mode without touching provider/global filesystem state. Named-cookie parser tests must retain previous resolver behavior.
- [ ] **Step2: Run RED** focused team_server HTTP tests, showing invalid cases otherwise have a valid authenticated control.
- [ ] **Step3: Implement manifest/state/handlers/startup** exactly as above; no generic command dispatch fallback.
- [ ] **Step4: Run GREEN** team_server, team_access including previous session tests, explicit PostgreSQL auth route transaction test, standalone compile, and diffcheck. Real GitHub calls remain absent.
- [ ] **Step5: Self-review and scoped commit** `feat(server): serve isolated team authentication routes`.

### Task 3: Mount the team browser shell before legacy providers

**Files:** Create `src/lib/team-client.ts`, `src/lib/team-client.test.ts`, `src/components/team/team-mode-boundary.tsx`, `team-mode-boundary.test.tsx`, `team-auth-provider.tsx`, `team-auth-provider.test.tsx`, `team-onboarding.tsx`, `team-onboarding.test.tsx`, `team-signed-in-shell.tsx`, and `src/app/onboarding/page.tsx`. Modify `src/app/layout.tsx`, `src/app/login/page.tsx`, `src/app/workspace/layout.tsx`, `src/app/settings/layout.tsx` entry boundaries only, `src/lib/transport/index.ts` for the lifecycle/deny gate with new `src/lib/transport/index.test.ts`, and dictionaries `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`. Keep existing nested workspace/catalog presentation files unchanged here; their authorized wiring is the next conversion plan.

File-ownership refinement: create `src/components/team/team-local-providers.tsx` with a focused test for local-only locale/theme setup, and `src/components/team/team-login.tsx` with a focused test for the GitHub sign-in/fixed callback-reason view. Keep the capability decision in team-mode-boundary, session lifecycle in team-auth-provider and organization selection in team-onboarding; do not combine these distinct concerns into one oversized component. These files implement the already-required boundaries below and do not introduce new APIs or features.

Test-file ownership refinement: `src/components/appearance-provider.test.tsx` may add the required positive control proving the existing full operator/native provider restores stored custom CSS after mounting. No production AppearanceProvider behavior change is included; this complements the pre-hydration denial tests below.

Root-entry ruling: also modify `src/app/page.tsx` and create its focused `src/app/page.test.tsx`. Although normal team GET `/` is a fixed server303, the exported root page currently reads codeg_token and POSTs `/api/health`. Apply the same pre-hook native/operator/team split so client-side root navigation cannot run that legacy work. The team entry only redirects to `/workspace`; unknown mode mounts no legacy effect. Preserve explicit operator/native behavior in a separate component. Test positive operator/native controls and absence of token reads/legacy HTTP in team/unknown. No new server route or authentication policy is introduced.

Owner branding update: visible product name is **Neev**. The website is **neev.so** and the application is **app.neev.so**. Use Neev in the root browser title/description and new team onboarding/shell copy. Task3 also owns the existing AppBootLoading wordmark in `src/components/layout/app-boot-loading.tsx` and its focused render test. Keep organization-specific names/logos intact. Do not rename internal workos/codeg keys, API paths, GitHub App links, repository/branch names, native package IDs or existing integration configuration. No new logo asset or marketing site is required in this application task. The browser acceptance driver must check the rendered product title; the application origin is configuration, not a hardcoded request base. Application cookies remain host-only and the website gets no protected API/CORS exception.

**Interfaces:** team-client exports safe typed `loadCapabilities`, `loadPendingChoices`, `selectOrganization(githubOrgId: string, csrf: string)`, `loadTeamSession`, `logoutTeamSession(csrf: string)` using exact Task2 contracts. Keep GitHub IDs as validated decimal strings end-to-end in browser state/requests; never Number/parseInt them. No generic arbitrary command/URL or operator-token API. Requests credentials:same-origin,cache:no-store, static errors, AbortController timeout65seconds (server handler budget60seconds); mutations send explicit CSRF. Capability response validated version/mode; unknown/missing/malformed/failed response remains a retry/error screen, never operator fallback.

Task3 additionally owns `src/lib/appearance-script.ts` and its focused tests only for safe pre-hydration initialization. Root layout currently injects APPEARANCE_INIT_SCRIPT before React, including raw stored custom CSS. The capability boundary cannot undo network/style side effects already caused by that script. Use only allowlisted theme/color/zoom initialization before browser mode is known; do not inject stored arbitrary CSS, background URLs or custom style overrides in unknown/team mode. The existing full AppearanceProvider can restore operator/native preferences after its confirmed branch mounts. Preserve stored preferences rather than deleting them. Add a positive operator/native control and a browser unknown/team regression with stored CSS containing a remote URL, proving it is never injected/requested; include this in browser acceptance. This is part of the existing safe-local-theme boundary, not a new team customization feature.

TeamModeBoundary runs before AppI18nProvider/AppearanceProvider/WebConnectionGuard or any workspace legacy providers. Detect native Tauri separately and retain existing native tree. A browser confirms explicit operator capability before mounting existing operator providers. On team mode, clear old codeg_token storage, destroy existing legacy web transport if present through a scoped lifecycle function, and mount only safe local i18n/theme and TeamAuthProvider. Browser-local language/appearance preferences are sufficient here; do not enable global settings APIs to satisfy inherited hooks. Browser imports alone are lazy in current transport; test that no legacy HTTP/WS starts while mode is unknown or team.

Separate workspace/settings entry wrappers from their existing operator component functions so the decision occurs before their hooks run, not as a late return after mounting legacy providers. TeamModeBoundary receives distinct `operator: React.ReactNode` and `team: React.ReactNode` branches and mounts neither during capability loading/failure; root layout passes the route children into the appropriate provider tree. For this slice, `setBrowserServerMode(mode: "unknown"|"operator"|"team")` in transport/index.ts destroys a cached browser shell transport on a change; getShellTransport rejects browser unknown/team instead of creating WebTransport. Native Tauri selection and its remote transport are unchanged. Team requests use team-client directly; the following catalog conversion can add a narrow TeamTransport manifest. Reset mode to unknown in the existing test-only transport reset, and set explicit operator mode in any affected direct factory test fixtures.

Publish the validated browser transport mode before setting React state that mounts its branch; do not defer mode publication to a parent effect, because child render/effects can request transport first. Test a child that requests transport immediately on mounting. Login/onboarding are public-flow pages: do not redirect them to the old signed-in workspace merely because a previous session cookie remains valid during organization/account switching. Keep old workspace content out of those pages and cancel old session-bootstrap responses before displaying a new account's selected scope.

Apply the same browser-mode denial before `getTransport` can return its cached remote transport; guarding only `getShellTransport` leaves that earlier return unchecked. On browser entry to unknown/team mode, destroy any cached remote transport and clear its configuration as well as the shell transport. Refuse creation of a remote desktop transport in a non-native unknown/team browser. Preserve native Tauri remote-desktop behavior and test both the cached-remote bypass attempt and the valid native control. This gate is separate from the following catalog plan's direct GitHub-picker fetch conversion; do not mount that legacy picker in this intermediate shell.

Team onboarding matches existing restrained centered sign-in visual style: clear GitHub identity, labelled organization choices, search when list is long, explicit Continue button disabled until selection, recovery for zero membership/pending invite/unavailable/reconnect, and Sign out/restart link. Even one org requires explicit choice. Display organization login distinctly from preserved display name after selection. Keyboard navigation, focus on validation errors, aria-busy and mobile375px layout. Keep unsent choice on retryable errors;401restarts onboarding without inventing organization.

For zero eligible organizations, explain that an active organization membership and access for the EazyWorkOS GitHub App are needed. Offer a clearly labelled external link to the user's existing app installation page `https://github.com/apps/eazyworkos/installations/new`, alongside Retry and Restart sign-in. The user/organization owner performs any installation approval in GitHub; the app never auto-installs or accepts invitations. Keep this fixed official destination out of query parameters, and do not claim installation alone proves membership. The callback's already-specified unbound-installation-entry restart handles the return flow. Include a zero-choice UI test for this recovery path.

The install-link format is documented by [GitHub](https://docs.github.com/en/apps/sharing-github-apps/sharing-your-github-app). Real acceptance must still confirm this app is available to the intended organizations; a public metadata response alone does not prove installation approval.

The existing static login/page.tsx contains an operator password-token form, while today's standalone legacy server overrides /login with generated GitHub HTML. The team static /login branch must render a translated GitHub sign-in link to `/auth/github/login`, never the operator token input or `/api/health` token validation. Preserve the operator component separately for its explicit mode; do not infer that the current hosted HTML means the static page is already safe.

Team login reads only the three fixed reason values from Task2, maps them to translated friendly text, and ignores all unknown values; never echo arbitrary query text. A retry starts a new bound attempt. Cover denied consent and upstream failure recovery in the browser driver, not just successful login.

Authenticated safe shell displays person's name and organization branding, Sign out, and a useful status explaining that workspace routes are not yet enabled in this intermediate slice. Do not show a Change organization control in v1; users log out and sign in again to choose a different organization. Do not mount legacy project/chat providers to display their data. Fixed /workspace and /settings team branches route through this shell until the next catalog/settings conversion; browser Back to workspace remains present. The next plan replaces the intermediate status with preserved authorized hierarchy before release.

Session scope key includes current opaque session fingerprint,org anduser. On scope change, clear previous content synchronously before showing new scope; a BroadcastChannel message is only an invalidation signal, never authority. On focus/visibility resume, refresh session bootstrap and clear on401/changed scope;503does not silently keep expired authority interactive. Notify other tabs after successful login/select/logout, never broadcast secrets/user content. Cancel in-flight prior-scope fetches and ignore stale responses. Successful logout navigates/login; server503shows honest failure and does not claim every session revoked.

- [ ] **Step1: Write Vitest behavior tests** before components/client implementation. Include deny-unknown capability, no legacy transport calls in team/unknown states, stale bootstrap races, explicit single-org choice, wrong/expired pending flow, displayed user/org, logout503, prior-scope content clearing, keyboard accessible selectors and narrow-screen classes.

```tsx
render(<TeamModeBoundary operator={legacyTree} team={teamTree} />);
expect(legacyTransport.call).not.toHaveBeenCalled();
await screen.findByRole("button", {name:/retry/i}); // failed capability response
expect(legacyTransport.call).not.toHaveBeenCalled();
```

- [ ] **Step2: Run RED** Node22 Vitest focused files with --maxWorkers=1 --minWorkers=1, record behavior failures.
- [ ] **Step3: Implement safe client/components and translations** without introducing dynamic route segments or restoring orphan New chat/Documents navigation.
- [ ] **Step4: Run GREEN** focused tests, existing hierarchy/sidebar/identity/settings-return regressions, locale coverage, static Next export and diffcheck. Unsupported team workspace operations stay absent, not mocked as working.
- [ ] **Step5: Self-review and scoped commit** `feat(ui): add explicit organization onboarding and scoped team shell`.

### Task 4: Verify the real browser-to-team route boundary

**Files:** Create `src-tauri/src/team_server/browser_fixture.rs` (cfg(test)-only), `scripts/tests/team-auth-e2e.cjs`; modify test_server.rs only for reusable fake provider endpoints, and team_server/mod.rs for cfg(test)-only browser_fixture registration. No production provider origin override.

Browser-discovered production fix authorization: Task4 also modifies `team_server/static_files.rs` and creates `team_server/static_files_tests.rs` (registered cfg(test) in mod.rs). Real client navigation requests `/login.txt` and `/workspace.txt`, but the existing exact static manifest omits them despite their presence in the export. Add exact ServeFile mappings for `/login.txt`, `/onboarding.txt`, `/workspace.txt`, `/settings.txt` to those four files only; preserve text/plain response content appropriate to the installed Next static-export client. Do not add a static-root catch-all, enable other exported legacy pages, or mount any runtime/API. Add actual-router tests for exact payload/body/content type, query-bearing RSC requests, missing allowed payload and denied arbitrary/unported `.txt` paths even when a sentinel file exists. Keep the browser's unexpected-HTTP/console checks strict and rerun its complete journeys. The existing browser failure is product RED; fixture/selector errors remain separately classified. No other production fix is included without a new bounded diagnosis.

Shared-clock file-map refinement: this task may raise only `TestClock::advance` in `src-tauri/src/team_access/test_auth.rs` from pub(super) to pub(crate) if needed for the required expired-membership-proof/outage journey. The module remains cfg(test); reuse the controlled clock rather than introducing a second clock or waiting sixty real seconds.

Browser acceptance carried from Task3: assert the rendered Neev title and fixed root-to-workspace navigation; seed stored arbitrary CSS containing a remote URL and old operator/remote configuration, then prove no custom-CSS request or legacy HTTP/WebSocket connection occurs. Exercise denied consent and unavailable-provider retry, zero-choice fixed GitHub App installation/retry/restart controls, explicit single/multiple-org choice, person/org branding, settings return, stale prior-scope responses and focus/visibility refresh. Keep login/onboarding free of prior-cookie workspace content. These are actual browser/network checks, not jsdom assertions. Website/app origins stay configuration; do not implement the public website in this fixture task.

The accepted OAuth and membership test constructors in `team_access/oauth.rs` and `team_access/github.rs` are pub(super). Permit only changing those cfg(test)-only constructor visibilities to pub(crate) for this sibling fixture's real local HTTP adapters; do not add a production environment URL override or duplicate the adapters. Their existing loopback-origin validation remains enforced.

In browser secret-absence assertions, distinguish OAuth access/refresh tokens, operator tokens, encryption material and raw session cookies (never projected) from the explicitly authorized CSRF bootstrap field (returned only after its matching binding is verified). Do not make the acceptance test contradict the session/pending contracts by rejecting that checked CSRF field. None of these secrets belongs in localStorage or BroadcastChannel messages.

If reusable fake HTTP helpers are reached through the currently private `team_access::test_http` module, this task may change only the necessary cfg(test) module/helper visibility to pub(crate), preserving the existing helper behavior and protocol tests. Prefer that visibility adjustment to a duplicate fake-server implementation.

**Interfaces:** Existing cached Playwright through PLAYWRIGHT_MODULE; opt-in Rust test binary path through WORKOS_E2E_TEST_BINARY. Use dynamic127.0.0.1:0 listeners and driver-owned temporary directories, not existing hardcoded3097 legacy fixture. Driver exercises actual HTTP and page navigation; separate contexts for Alice A/B, Bob A and Carol B. Fake GitHub authorization/code/token/identity/membership endpoints produce invented evidence but the application performs its actual OAuth attempt, pending, selection and session logic. Do not inject legacy owner/session/operator cookies or bypass onboarding to pass acceptance.

Start the browser fixture with `crate::db::test_helpers::fresh_in_memory_db().await.conn`, which migrates without seeding identities; do not call `team_access::test_auth::sqlite_fixture`, which intentionally seeds malformed/legacy authority for unit regressions. Reuse the shared clock and fake HTTP infrastructure independently. Only organization branding may be preseeded for the preservation scenario; all browser users, membership bindings, credentials and sessions must arise through the actual onboarding flow. No new database harness is needed.

- [ ] **Step1: Write failing browser driver** covering public login→fake provider→explicit picker→session shell, same-user A/Bswitch with stale tabs, Bob notadmin, organization branding preservation, session cookie HttpOnly/SameSite, secret absence from localStorage/JSON, unsupported legacy routes remain404 with operator bearer, forged Origin/CSRF rejected, provider outage then logout recovery. Capture screenshots and console/network failures without logging token/cookie/querycode values.
- [ ] **Step2: Record the first actual-browser characterization** against the new server/shell. Fail on unexpected browser console/page exceptions and cross-scope stale content, not just missing fixture startup. Tasks1–3 already own implementation RED/GREEN; this combined acceptance task may pass its product assertions without identifying a new product defect. Report fixture, selector or diagnostic-classification failures separately and never manufacture or mislabel a product RED. Preserve deliberate negative controls and investigate any real product failure before requesting an out-of-scope fix.
- [ ] **Step3: Implement the test fixture/driver support and the explicitly authorized static-payload fix above**. Fixture endpoints remain physically absent from production builds; shutdown closes servers and deletes exact owned synthetic temp data. Existing workspace-e2e helper's legacy authority must not be reused.
- [ ] **Step4: Run GREEN** desktop1440px and mobile375px flows, two independent sessions plus dual-org switch, and record exact commands/screenshots. Label all external evidence fake; real GitHub App consent is a later release gate.
- [ ] **Step5: Self-review and scoped commit** `test(auth): verify browser onboarding and organization isolation`.

## Controller acceptance and continuation

Task reviews and combined review must verify no fallback to legacy startup/router/providers and no claim that the intermediate shell is rollout ready. Fresh controller Rust/browser/static-export verification; preserve screenshot/evidence artifacts. Then convert catalog/settings authority and chat audiences while keeping the approved nested hierarchy. Runtime remains disabled until isolated agent/job acceptance. Publication/deployment and real provider consent are not owned by implementation agents.

## Self-review status

Draft prepared while authentication orchestration executes; do not dispatch until the controller rechecks final consumed interfaces and performs the normal plan preflight. The approved spec's HTTP/session/initial UI requirements map to tasks1–4. Catalog/branding editing/private-org chat/event/file/runner controls are explicitly next slices, not certified here. No live server changes are authorized by this document alone.

Provisional interface/self-consistency checks (repeat against the accepted authentication commit before execution):

| Tasks | Shared interface or internal check | Result |
| --- | --- | --- |
| 1 → 2 | TeamConfig, dedicated pool, public entry/module visibility | Config stays nonserializable; the binary imports only the public mode/entry functions. Dedicated startup bypasses legacy initialization before any side effects. |
| 1 → 4 | Test-only DB and provider fixture visibility | Reuse the existing disposable-DB guard; browser fixture uses unseeded fresh_in_memory_db. Necessary test-only sibling visibility is explicitly scoped. |
| 2 → 3 | Capabilities, session/pending JSON, error codes and cookies | GitHub IDs remain decimal strings; opaque session scope is not a raw cookie; browser timeout65s exceeds server handler budget60s. |
| 2 → 4 | Actual OAuth flow, fake HTTP adapters and static routes | No seeded operator identity/cookie; callback failure redirects and zero-choice installation recovery are included, not only successful sign-in. |
| 3 → 4 | Static export, independent contexts, scope invalidation | Driver exercises exported fixed pages and actual API, including delayed prior-scope responses and mobile375px. |
| 1 | Pure mode/config validation versus initialization tests | Non-Unicode mode never means absent; production DB is Postgres-only. Test SQLite construction is separate and does not weaken startup validation. |
| 2 | Route manifest, strict parsing and startup file ownership | Uses codeg_server.rs, shared named-cookie parser, exact serialized Origin, bounded bodies and explicit404 fallback. No legacy runtime route is mounted. |
| 3 | Provider ordering and transport factory behavior | Root capability gate precedes hooks; browser mode is published before branch mount. Both shell and cached-remote paths are denied before team-safe providers render. |
| 4 | Fixture scope and acceptance claims | Only cfg(test) origins/helpers are adjustable. Fake-provider browser success does not establish real GitHub/provider consent or rollout readiness. |
