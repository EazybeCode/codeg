# Neev independent identity and organizations implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a person sign into a fresh Neev application without GitHub, create or join a native organization, and manage its nested project/folder hierarchy securely.

**Architecture:** Implement a fresh native database and explicitly separate native identity, membership and integration credentials. A dedicated `neev-server` entry point mounts only the new authenticated router; old operator/team binaries remain offline rollback artifacts, never fallback routes. Reuse the existing React shell and catalog UX, but not GitHub-dependent admission or legacy database identities.

**Tech Stack:** Existing Rust 2021, Axum 0.8, SeaORM 1.1, PostgreSQL, SQLite unit tests, Next/React/TypeScript, Vitest. Pin `openidconnect = "=4.0.1"` for Google verification; pin `hmac = "=0.12.1"` for keyed email rate identifiers. Existing rand 0.8, sha2 0.10 and AES-GCM handle randomness/hash/encrypted short-lived OAuth exchange material.

**Spec:** `docs/superpowers/specs/2026-09-11-independent-identity-and-organizations-design.md`, approved September 11. This implements subproject one only, not documents, prompt execution or public release.

## Global Constraints

- Every user brings their own AI-tool subscription; no shared/operator AI credential fallback.
- "No GitHub repository, branch or organization is deleted or modified."
- "Do not merge accounts because email strings match."
- "No automatic joining or role assignment by domain."
- "Never hold database locks across external authentication or email delivery."
- "Keep runtime disabled until the existing isolated-agent and durable-job gates pass."
- "No in-app org switcher is added in this subproject; sign-out/sign-in clears old org state and allows selection."
- Google callback is exactly `https://app.neev.so/auth/google/callback`; GitHub sign-in callback remains `/auth/github/callback` on that origin. Basic Google scopes only: `openid email profile`.
- Magic links: 256 random bits, hash-only persistence, 10 minutes, single use, requesting-browser binding, fragment transport, explicit POST consumption. Pending OAuth: 10 minutes, one-use state and PKCE; linking requires authentication within 10 minutes.
- Invites: 7 days, 100 outstanding/org, 20 sends/admin/day. Org creation: 3/user/UTC day. Mail: 3/address/15 minutes, 20/source IP/15 minutes, 80/day UTC, 10/minute, 2 concurrent sends across login and invitations.
- Production: fresh PostgreSQL database and separate storage; never migrate or drop the old database/archives in this plan. Real email/provider tests need participant consent and private HTTPS.
- Frontend verification uses Node 22.23.2 and the existing pnpm installation. Do not rewrite unrelated tests to hide the previously observed intermittent organization-settings failures.
- Work in the existing isolated `.context/workos-src` worktree. Publish only through `.context/workos-publish` to EazybeCode/Workos; source origin is codeg and must not receive a push.

## File and interface map

New implementation lives under `src-tauri/src/neev/`, not in the old `team_access` service graph. `mod.rs` exports only `run_server`; internal services stay crate-private. Shared mechanical cryptography can be extracted with regression tests, but native code must never call `PrincipalService`, legacy `Migrator`, GitHub roster lookup or operator `AppState`.

| File | Responsibility |
| --- | --- |
| `neev/db/{mod,migration,entities,tests}.rs` | Native schema and repositories; independent migrator |
| `neev/{types,error,clock}.rs` | Safe DTOs, opaque error codes, injected clock |
| `neev/auth/{identity,session,secrets,tests}.rs` | Provider identity linking and native sessions |
| `neev/auth/{oauth,google,github,oauth_tests}.rs` | Browser-bound OAuth exchange and verified subject proof |
| `neev/mail/{mod,resend,quota,tests}.rs` | In-memory message delivery and persistent delivery reservations |
| `neev/auth/{magic,magic_tests}.rs` | Browser-bound email proof |
| `neev/org/{service,invites,tests}.rs` | Ownership, roles, invitations and revisions |
| `neev/catalog/{nodes,tests}.rs` | Tenant-scoped native project/folder tree |
| `neev/http/{mod,cookies,auth,org,catalog,static_files,tests}.rs` | Explicit routes, Origin/CSRF checks, safe responses |
| `neev/{config,startup,test_support}.rs` | Fresh-mode startup, dependency injection, cfg(test) fixtures |
| `src-tauri/src/bin/neev_server.rs` | Native-only executable |
| `src/lib/neev-client.ts` | Strict native DTO parsing and bounded HTTP transport |
| `src/components/neev/*` | Native login/onboarding/settings boundary, using existing visual primitives |
| `public/neev-auth/confirm.{html,js,css}` | Isolated no-third-party magic/invite fragment landing |

Use positive `i32` native user/org/member/node IDs to fit existing UI conventions; do not reuse legacy IDs. Revisions are `i64` in Rust and canonical decimal strings on the wire. UUID request IDs bind retries to actor/action/body digest. No SQL row models are serialized to HTTP.

Shared Rust contracts (define in `neev/types.rs`; all structs crate-private):

```rust
type UserId = i32;
type OrgId = i32;
type MemberId = i32;
type NodeId = i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role { Admin, Member }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider { Google, Github, Email }
#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthError { Unauthorized, Forbidden, Conflict, Invalid, Limited, Unavailable }

struct VerifiedIdentity {
    provider: Provider,
    issuer: String,
    subject: String,
    verified_email: Option<String>,
    display_name: String,
    avatar_url: Option<String>,
}
struct IdentityContext { user_id: UserId, session_id: String, auth_revision: i64, authenticated_at: i64 }
struct OrgContext { identity: IdentityContext, org_id: OrgId, member_id: MemberId, member_revision: i64, org_revision: i64, role: Role }
enum SignInIntent { Login, Link { user_id: UserId, session_id: String, auth_revision: i64 } }
struct SessionSecrets { session: String, csrf: String, expires_at: i64 }
struct Organization { id: OrgId, name: String, owner_user_id: UserId, revision: i64 }
enum NodeKind { Project, Folder, WorkingDirectory }
struct NodeInput { parent_id: Option<NodeId>, kind: NodeKind, name: String }
struct Node { id: NodeId, org_id: OrgId, parent_id: Option<NodeId>, kind: NodeKind, name: String, revision: i64 }
```

Secret-bearing structs (`SessionSecrets`, OAuth exchange payload, incoming fragments) MUST NOT derive Debug or Serialize. `VerifiedIdentity` is constructed only inside verified provider adapters/test fixtures, never from HTTP JSON. Keep clock injectable with `fn now_unix(&self) -> i64`.

## Task 1: Fresh schema and native identity resolution

**Files:** Create `neev/mod.rs`, `neev/{types,error,clock}.rs`, `neev/db/{mod,migration,entities,tests}.rs`, `neev/auth/{mod,identity,secrets,tests}.rs`; modify `src-tauri/src/lib.rs` to declare `pub mod neev`.

**Consumes:** Existing SeaORM database connection and rand/sha2; no legacy rows.
**Produces:** `db::initialize(&DatabaseConnection) -> Result<(), AuthError>`; `identity::resolve(&DatabaseConnection, VerifiedIdentity, SignInIntent, i64) -> Result<UserId, AuthError>`; `secrets::random_token() -> Result<String, AuthError>`; `secrets::digest(&str) -> Result<String, AuthError>`.

- [ ] Write database/identity tests first, with a SQLite memory connection per test. Define `test_support::database()` as connect + native initialize and `google(subject,email)` as a test-only verified proof constructor.

```rust
#[tokio::test]
async fn same_email_does_not_merge_distinct_subjects() {
    let db = test_support::database().await;
    let first = identity::resolve(&db, test_support::google("subject-a", "owner@example.test"), SignInIntent::Login, 1).await.unwrap();
    let again = identity::resolve(&db, test_support::google("subject-a", "owner@example.test"), SignInIntent::Login, 2).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(identity::resolve(&db, test_support::google("subject-b", "owner@example.test"), SignInIntent::Login, 3).await, Err(AuthError::Conflict));
}
```

- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev::db` and `... neev::auth::tests`; capture actual initial failure, not an invented RED transcript.
- [ ] Implement independent migrator with native marker `neev_schema_version`, rejecting any DB containing `seaql_migrations`, `users`, `orgs` or operator/team credential tables before writing. It must also reject unknown/newer native versions. No call to the old migration list.
- [ ] Create the following tables with foreign keys and indexes via SeaQuery (SQLite foreign keys ON). All timestamps are UTC epoch BIGINT, all secrets represented as lowercase SHA-256 hex of a versioned purpose-prefixed token. Tables use `neev_` prefix; names below omit prefix for readability:

```text
accounts(id PK, display_name, avatar_url NULL, auth_revision>=1, disabled bool, created_at)
identities(id PK, user_id FK accounts, provider, issuer, subject, created_at,
           UNIQUE(provider,issuer,subject), CHECK provider IN google/github/email)
verified_emails(id PK, user_id FK accounts, canonical_email UNIQUE, verified_at)
organizations(id PK, owner_user_id FK accounts, name, logo_data NULL,
              revision>=1, disabled bool, created_at)
memberships(id PK, org_id FK organizations, user_id FK accounts,
            role IN admin/member, revision>=1, active bool, UNIQUE(org_id,user_id))
sessions(id text PK, token_hash UNIQUE, csrf_hash, user_id FK accounts,
         account_revision, org_id NULL FK organizations, member_id NULL FK memberships,
         member_revision NULL, org_revision NULL, authenticated_at, expires_at, revoked_at NULL,
         CHECK org/member/revision fields all NULL or all NOT NULL)
oauth_attempts(state_hash PK, browser_hash, provider, intent, link_user_id NULL,
               link_session_id NULL, link_revision NULL, exchange_enc, expires_at, consumed_at NULL)
magic_attempts(token_hash PK, browser_hash, canonical_email, intent, link_user_id NULL,
               link_session_id NULL, link_revision NULL, expires_at, consumed_at NULL)
invitations(id text PK, org_id FK organizations, inviter_id FK accounts,
            recipient_email, role IN admin/member, token_hash UNIQUE, expires_at,
            revoked_at NULL, accepted_by NULL FK accounts, accepted_at NULL)
quota_buckets(kind, key_hash, window_start, used, expires_at,
              PRIMARY KEY(kind,key_hash,window_start))
mail_leases(id text PK, expires_at)
requests(user_id FK accounts, action, request_id, body_hash, resource_id,
         expires_at, PRIMARY KEY(user_id,action,request_id))
nodes(id PK, org_id FK organizations, parent_id NULL FK nodes,
      kind IN project/folder/working_directory, name, revision>=1, created_by FK accounts)
```

- [ ] Implement purpose-bound random tokens using 32 bytes from `OsRng`, URL-safe-no-pad base64. Reject malformed/oversize tokens before hashing. Identity resolution transaction: find subject → lock existing account → validate active/revisions; else reject existing verified-email ownership → insert account/identity/email. Unique constraint races return safe Conflict (caller can retry identical subject); never silently move an email. For Link, recheck current session/account revision and recent auth, then insert only into that account. Existing foreign-linked subject or email is Conflict. Concurrent same-email first login must yield one account, no orphan.
- [ ] Implement a documented lock protocol for subsequent tasks: account rows in ascending ID, then org, then memberships in ascending ID, then session/attempt/invite/resource. PostgreSQL uses FOR UPDATE; SQLite takes write intent at transaction start. Unique-key creation paths must retry by re-resolving identities without holding external I/O or reversing locks.
- [ ] Add tests for fresh/no-GitHub account, duplicate subjects, linking CSRF-bound intent/stale auth/foreign identity, concurrent creation rollback, unknown schema and foreign-database refusal; repeat constraint tests on disposable PG using `NEEV_TEST_DATABASE_URL` pointing only to an isolated test DB. Missing PG means explicitly not-run, not acceptance.
- [ ] Run focused tests and `git diff --check`, then commit exact touched paths: `feat(neev): add independent identity schema and account resolution`.

## Task 2: Native sessions, startup and deny-by-default HTTP boundary

**Files:** Create `neev/{config,startup,test_support}.rs`, `neev/auth/session.rs`, `neev/http/{mod,cookies,auth,static_files,tests}.rs`, `src-tauri/src/bin/neev_server.rs`; modify `src-tauri/Cargo.toml` bin list and `neev/mod.rs`.

**Consumes:** Task1 schema/types.
**Produces:** `session::issue(db,user_id,Option<OrgId>,now) -> Result<SessionSecrets,AuthError>`; `session::identity(db,token,now) -> Result<IdentityContext,AuthError>`; `session::organization(db,token,now) -> Result<OrgContext,AuthError>`; `session::recheck(tx,&OrgContext,now) -> Result<(),AuthError>`; `http::router(Arc<State>) -> axum::Router`; `run_server() -> ExitCode`.

- [ ] Add a real-router test (Axum test server injected DB/clock, no browser-created auth bypass in production):

```rust
#[tokio::test]
async fn legacy_cookie_cannot_authenticate_native_routes() {
    let server = test_support::http_server().await;
    server.get("/api/neev/session")
        .add_header("cookie", "workos_team_session=old; workos_session=old")
        .await.assert_status_unauthorized();
    server.get("/api/settings").await.assert_status_not_found();
    server.get("/api/team/session").await.assert_status_not_found();
}
```

- [ ] Run `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev::http`; record RED.
- [ ] Implement native config exclusively using `NEEV_APP_ORIGIN`, `NEEV_DATABASE_URL`, `NEEV_ENC_KEY`, `NEEV_BIND`, `NEEV_STATIC_ROOT` and optional provider configs. HTTPS origin with no credentials/path/query; exact callbacks derived, not user-controlled. Production PostgreSQL only; allow loopback HTTP solely under explicit local-preview flag. No `DATABASE_URL`/operator-secret fallback. Provider configs are all-or-none and visibly disabled if absent. Any runtime-enable request is rejected in this milestone.
- [ ] Add binary declaration and minimal entry point:

```toml
[[bin]]
name = "neev-server"
path = "src/bin/neev_server.rs"
required-features = []
```

```rust
fn main() -> std::process::ExitCode { codeg_lib::neev::run_server() }
```

- [ ] Implement new cookie names `__Host-neev_session`, `__Host-neev_browser` with Secure/HttpOnly/Path=/, no Domain, SameSite=Lax; preview uses distinct non-__Host names and cannot authenticate production. Identity session TTL30 minutes; organization session TTL12 hours; proof timestamp unchanged by organization selection. Issuing org session atomically revokes the old identity session and rotates secrets; logout revokes server row and clears browser state. Bootstrap returns CSRF secret from a separate HttpOnly double-submit cookie or encrypted session envelope, never its hash. For simplicity use a separate purpose-bound raw CSRF cookie and store its hash in session row; validate both cookie and header against DB on mutations. Duplicate cookie/header rejected.
- [ ] Require exactly one trusted Origin and JSON content type plus `x-neev-csrf` on all authenticated mutations. Pre-auth bootstrap sets a browser cookie plus pre-auth CSRF; OAuth initiation and magic request are POST-only and same-origin. No cross-origin wildcard CORS. GET/HEAD callbacks never mutate membership. Set no-store and no-referrer on auth responses and static confirmation; mask error bodies to `code` only. Apply existing safe bounded-concurrency concepts without sharing mutable legacy limiter state.
- [ ] Route only `/healthz`, `/api/workos/capabilities`, `/api/neev/session`, `/api/neev/logout`, preauth/provider routes added by later tasks, org/catalog native routes and allowlisted static pages. Capabilities include `identityMode:"native"`, `runtimeEnabled:false`, configured provider booleans. Legacy routes return404; no universal SPA fallback to operator pages. Native DB identity/member/org rechecked on every protected request and before every committed write.
- [ ] Test missing/duplicate cookie, wrong Origin, missing CSRF, identity session denied org access, revoked/moved membership, stale org role/revision, invalid config, body bounds, no secret Debug/logs, no old routes, parallel logout/write races. Run focused suite and native binary build, then commit `feat(neev): isolate native sessions and server entrypoint`.

## Task 3: Google and optional GitHub identity login/linking

**Files:** Create `neev/auth/{oauth,google,github,oauth_tests}.rs`; modify native config, auth routes, Cargo.toml/lock. Do not modify old team GitHub app service.

**Consumes:** Identity resolution, browser/CSRF/session functions, short-lived encrypted attempt payloads.
**Produces:** `oauth::begin(state,Provider,SignInIntent,browser_secret) -> Result<String,AuthError>` (authorization URL); `oauth::complete(state,Provider,state_token,code,browser_secret) -> Result<SessionSecrets,AuthError>`; verified proof constructors restricted to adapters.

- [ ] Add protocol tests using a local mock HTTP issuer under cfg(test) only, locally signed RSA JWT fixtures, and captured requests. Use library `CoreRsaPrivateSigningKey` for signing fixtures, never production credentials. Verify at least these table-driven cases:

```rust
#[test]
fn google_verifier_rejects_untrusted_claims() {
    for mutation in ["issuer", "audience", "nonce", "expiry", "signature", "algorithm", "subject"] {
        let fixture = test_support::signed_google_fixture(mutation);
        assert!(google::verify_fixture(&fixture).is_err(), "{mutation}");
    }
}
```

`signed_google_fixture` returns a test-only struct containing metadata/JWKS, ID token, expected client ID/nonce and frozen time; its mutation changes exactly one expected-valid field. `verify_fixture` calls the SAME verifier used by the real exchange, not a duplicate mock verifier.

- [ ] Run `... neev::auth::oauth_tests` and record RED; then pin `openidconnect = { version = "=4.0.1", default-features = false, features = ["reqwest", "rustls-tls"] }` after verifying enabled feature names from the pinned crate manifest.
- [ ] Implement Google discovery at fixed `https://accounts.google.com`, TLS, no redirects, bounded timeout/body, cached metadata/JWKS with bounded refresh on unknown key. Use the library's ID token verifier, audience/issuer/expiry/nonce checks and supported RS256 key verification. Request code flow with PKCE S256 and only openid/email/profile; discard access tokens after proof. Never trust browser-supplied JWT claims or fetch arbitrary metadata URLs.

```rust
// Core of the adapter, with token_response/client/nonce from the pinned library:
let id_token = token_response.id_token().ok_or(AuthError::Unauthorized)?;
let claims = id_token.claims(&client.id_token_verifier(), &nonce)
    .map_err(|_| AuthError::Unauthorized)?;
let subject = claims.subject().as_str().to_owned();
let email = if claims.email_verified() == Some(true) {
    claims.email().map(|value| value.as_str().to_owned())
} else { None };
```

- [ ] Store state/browser hashes and intent/link session binding; encrypt only PKCE verifier + nonce with AAD including state/browser/provider/intent. Atomically claim attempt before network exchange; failed exchange requires a fresh attempt, no replay. Release DB locks before exchange; recheck linking session after exchange. Callback consumes only same-provider, same-browser, unexpired attempt. Bound code/state length and redirect only to fixed `/onboarding` or `/settings` after success. Scrub code/query from application/proxy logs.
- [ ] Optional GitHub sign-in uses a separately verified numeric `/user` identity and verified primary `/user/emails` result, with no roster or repository grant dependency. No GitHub token retained in an org account row. If the configured GitHub App cannot provide verified email with its currently approved permissions, show explicit unavailable/linking guidance; do not expand app scopes automatically. Repository integration credential acquisition remains subproject2.
- [ ] Test provider denial/unavailable, state replay, provider substitution, wrong browser, nonce mismatch, changed membership/link session during network wait, duplicate callback concurrency and matching-email conflict. Inspect mock requests for scope minimization and zero roster calls. Commit `feat(neev): add browser-bound Google and optional GitHub identity login`.

## Task 4: Resend delivery quotas and scanner-safe email login

**Files:** Create `neev/mail/{mod,resend,quota,tests}.rs`, `neev/auth/{magic,magic_tests}.rs`, `public/neev-auth/confirm.{html,js,css}`; modify native config/routes/static allowlist and Cargo manifest for HMAC.

**Consumes:** Native identity/session, requesting-browser secret and preauth CSRF.
**Produces:** `MailSender::send(MailMessage) -> Result<(),AuthError>` async trait; `MailMessage { recipient:String, subject:String, text:String }` (no Debug/Serialize); `magic::request(state,email,browser,intent) -> Result<(),AuthError>`; `magic::confirm(state,token,browser) -> Result<SessionSecrets,AuthError>`; quota reservation used by invitations too.

- [ ] Add real HTTP scanner/replay test; `test_support::MailCapture` implements the mail trait, exposes bodies only within tests, and never logs them:

```rust
#[tokio::test]
async fn scanner_get_does_not_consume_magic_link() {
    let mut h = test_support::MagicHarness::new().await;
    let fragment = h.request("person@example.test").await;
    h.get_confirmation().await.assert_status_ok();
    h.confirm_from_other_browser(&fragment).await.assert_status_unauthorized();
    h.confirm(&fragment).await.assert_status_ok();
    h.confirm(&fragment).await.assert_status_unauthorized();
}
```

Define MagicHarness as native HTTP server + two separate cookie jars + MailCapture + injected clock. `request` performs preauth bootstrap and real POST, extracts only the fragment from captured text; confirmation helpers call the real POST. No DB shortcut bypasses the protocol under test.

- [ ] Run `... neev::mail` and `... neev::auth::magic_tests`, record RED. Implement canonical email handling: trim, ASCII casefold consistently, reject CR/LF/display-name syntax/multiple addresses/over254bytes. Do not remove dots or plus suffixes. Same canonicalization in verified identities and invites.
- [ ] Implement transactionally reserved quota rows for address HMAC, trusted source IP, UTC minute/day and actor/org invites. Refuse untrusted X-Forwarded-For; production trusted proxy addresses explicitly configured. Key storage has expiry, indexed cleanup, and hard active bucket cap10,000; cap hit refuses uniformly. Acquire one of two shared DB mail leases with TTL30s, network timeout10s; count reservations even on provider failure, do not retry invisibly. Delete lease after completion; crashed lease expires. No DB lock held during HTTP.
- [ ] Implement Resend adapter with fixed `https://api.resend.com/emails`, send-only domain-restricted key from config, sender `Neev <login@mail.neev.so>`, text-only escaped subject/body, no attachments, no recipient lists, no tracking, no durable plaintext outbox. Never expose API response body/Authorization/link in logs. Free-tier exhaustion returns uniform accepted request copy without existence disclosure; UI explains delivery may be delayed and offers a later retry. Delivery failures revoke pending token. HTTP202 body identical for existing, new, collision, rate-limit and provider failure addresses; operational metrics carry only coarse reason counters.
- [ ] Store only purpose-bound token/browser hashes, canonical recipient, expiry and linking intent. Atomic consumed_at update includes unused + browser + expiry checks; wrong-browser does not consume legitimate link. Failed identity-linking conflict consumes proof but grants nothing. GET never consumes. Confirmation page immediately removes fragment and retains token only in JS memory:

```js
const token = location.hash.startsWith("#token=") ? location.hash.slice(7) : "";
history.replaceState(null, "", location.pathname);
document.querySelector("form").addEventListener("submit", async event => {
  event.preventDefault();
  // Bootstrap returns same-browser CSRF, not a token or account-existence result.
  const bootstrap = await fetch("/api/neev/preauth", { credentials: "same-origin", cache: "no-store" });
  const { csrf } = await bootstrap.json();
  const response = await fetch("/api/neev/email/confirm", {
    method: "POST", credentials: "same-origin", cache: "no-store",
    headers: { "Content-Type": "application/json", "x-neev-csrf": csrf },
    body: JSON.stringify({ token }),
  });
  if (response.ok) location.replace("/onboarding");
  else document.querySelector('[role="alert"]').textContent = "This link is expired, already used, or open in a different browser. Request a new link.";
});
```

The final implementation disables repeated submissions, validates bootstrap/JSON and reports network failures without exposing token. CSP `default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'`; no analytics, third-party resources, localStorage, query token or inline JS. Static response Cache-Control no-store, Referrer-Policy no-referrer.
- [ ] Test 10minute expiration, malformed/high-cardinality attempts, concurrent one-use, signed-in linking intent, mail timeout, quota boundaries across two server instances, scanner behavior, CSP, URL-clearing and no raw secret in DB/log fixture. Commit `feat(neev): add bounded transactional email and one-use magic login`.

## Task 5: Organizations, roles and exact-recipient invitations

**Files:** Create `neev/org/{mod,service,invites,tests}.rs`, `neev/http/org.rs`; modify router.

**Consumes:** IdentityContext/OrgContext and session final recheck; Task4 quota/mail.
**Produces:** `org::create(state,&IdentityContext,request_id,name) -> Result<Organization,AuthError>`; `org::select(state,&IdentityContext,OrgId) -> Result<SessionSecrets,AuthError>`; `org::set_role(state,&OrgContext,MemberId,Role) -> Result<(),AuthError>`; `org::remove(state,&OrgContext,MemberId) -> Result<(),AuthError>`; `invites::send(state,&OrgContext,email,Role,request_id) -> Result<String,AuthError>` (opaque invite ID, never token); `invites::accept(state,&IdentityContext,token,confirmed_org_id) -> Result<OrgId,AuthError>`.

- [ ] Define test_support::OrgHarness around real DB/services, with `owner()`, `member()`, `other_org_owner()` native fixtures, `invite(email)`, `accept(user,token,org)`, and captured mail token helper. Write refusal tests before services:

```rust
#[tokio::test]
async fn invite_acceptance_requires_exact_verified_recipient_and_current_inviter() {
    let h = test_support::OrgHarness::new().await;
    let invite = h.invite("invitee@example.test").await;
    assert_eq!(h.accept(h.wrong_recipient(), &invite).await, Err(AuthError::Forbidden));
    h.revoke_inviter_membership().await;
    assert_eq!(h.accept(h.invitee(), &invite).await, Err(AuthError::Forbidden));
}
```

- [ ] Run `... neev::org` for RED. Implement create request ledger atomically with owner-admin membership and orgcreation quota. Trim company names,1–100chars; no uniqueness-by-name/domain and no automatic org lookup admission. Return only organizations where caller has active membership. Selection rotates session and returns exact selected name/logo/role.
- [ ] Implement mutations in global lock order. Owner protected from leave/remove; only owner can grant/revoke admins; admins invite/remove ordinary members only. Last-admin check runs locked. Role/removal increments target membership revision and revokes all that user's sessions for that org; cancel pending scoped attempts and leave other-org sessions intact. Final recheck on writes must observe removal. Org edits validate revision and limit logo to existing safe raster validation rules; no arbitrary SVG/file path or outbound remote-logo fetching.
- [ ] Implement recipient-bound invites with random256bit hashed token,7days, memberdefault. Opaque invite preview after sign-in returns exact company name only when verified recipient matches; no membership until explicit POST with confirmed_org_id. Sending stores invite and commits before network, with no plaintext outbox. Revocation doesn't reset quotas. Acceptance locks actor/inviter accounts in ascending order then org/members/invite; revalidates inviter's current role. Unique membership upsert reactivates only by this explicit authorized action; replay by same accepted user is idempotent, other actor denied. Do not return raw pending token to admin API.
- [ ] Expose create/list/select, native member list, role/remove, invite/send/revoke/preview/accept routes with strict DTOs, CSRF and bounded pagination. Member list contains user display identity/role only, not agent or sign-in secrets. Tokens on invite URLs use isolated fragment page; never include them in server-rendered pages or query logs.
- [ ] Test owner/admin/member matrix, replay/revoke/wrongemail/expiredinviter, private resource non-expansion, same-name orgs, create retries/body mismatch, invite caps, org1→org2 stale session, concurrency on last admin and membership removal. Run SQLite and isolatedPG race tests, commit `feat(neev): add native organizations and secure invitations`.

## Task 6: Native folder hierarchy and honest repository/runtime capability boundary

**Files:** Create `neev/catalog/{mod,nodes,tests}.rs`, `neev/http/catalog.rs`; modify capabilities and native test support.

**Consumes:** OrgContext + session::recheck + native nodes table.
**Produces:** `nodes::list(state,&OrgContext,after:Option<NodeId>) -> Result<Vec<Node>,AuthError>`; `nodes::create(state,&OrgContext,NodeInput) -> Result<Node,AuthError>`; `nodes::update(state,&OrgContext,NodeId,expected_revision:i64,NodeInput) -> Result<Node,AuthError>`.

- [ ] Add nested-tree/cross-org test:

```rust
#[tokio::test]
async fn folders_can_nest_without_any_github_identity() {
    let h = test_support::OrgHarness::new().await;
    let root = nodes::create(h.state(), h.owner_context(), NodeInput { parent_id: None, kind: NodeKind::Project, name: "Marketing".into() }).await.unwrap();
    let folder = nodes::create(h.state(), h.owner_context(), NodeInput { parent_id: Some(root.id), kind: NodeKind::Folder, name: "Campaigns".into() }).await.unwrap();
    assert!(nodes::create(h.state(), h.other_org_context(), NodeInput { parent_id: Some(folder.id), kind: NodeKind::Folder, name: "Escape".into() }).await.is_err());
}
```

- [ ] Run `... neev::catalog` for RED. Implement admission with org ID on every query/write and ancestor lock/check. Root nodes must be projects; folders must have a same-org parent; no project beneath folder, cycles or descendant moves. Names1–100chars, max1000nodes/org. Retain cursor paging200, no global data walk. All active members can create folders; preserve admin/private boundaries from spec, no implicit sharing of chats through folders.
- [ ] Use the native DB tree, not old catalog metadata. At this milestone reject WorkingDirectory creation and repository-default fields with a safe capability-unavailable response; creation UI offers Project/Folder only. Preserve enum/schema slot for subproject2 without fabricating repository rows. Empty chat lists can return an explicit supported=false envelope, not pretend successful execution. No chat, document or prompt mutation endpoint is mounted.
- [ ] Test project→folder→folder, move/revision conflict, cross-org parent/cursor, cycles, removed member finalwrite denial and no outbound HTTP on any hierarchy action. Add mock GitHub client counter as a forbidden dependency assertion (native state must not even construct roster client). Commit `feat(neev): preserve nested folders under native tenant authorization`.

## Task 7: Native onboarding, sign-in methods and team settings UI

**Files:** Create `src/lib/neev-client{,.test}.ts`, `src/components/neev/{native-boundary,native-auth-provider,native-login,native-onboarding,native-settings,native-workspace}.tsx` and matching tests; modify `src/components/team/team-mode-boundary.tsx`, relevant login/onboarding/workspace/settings entry pages and existing ten locale dictionaries (locate exact files through tracked `TeamLogin` keys).

**Consumes:** Native capabilities/DTOs and routes; no calls to `/api/team/*` from native mode.
**Produces:** Usable Google/email/optionalGitHub onboarding plus named native org hierarchy and member management. Native boundary selected ONLY from server capabilities, never a localStorage flag or guessed URL.

- [ ] Write tests using Testing Library/MSW-equivalent existing fetch spies:

```tsx
it("shows the person and organization without GitHub", async () => {
  mockNativeSession({ user: { id: 1, displayName: "Sagar" }, organization: { id: 3, name: "Neev demo" }, role: "admin", csrf: "synthetic-csrf" });
  render(<NativeBoundary><NativeWorkspace /></NativeBoundary>);
  expect(await screen.findByText("Neev demo")).toBeVisible();
  expect(screen.getByText("Sagar")).toBeVisible();
  expect(screen.getByRole("button", { name: "New project" })).toBeEnabled();
  expect(screen.queryByRole("button", { name: "New chat" })).not.toBeInTheDocument();
});
```

Define mockNativeSession in colocated test helpers using the actual bootstrap DTO; all other unknown requests throw. No mocked GitHub org is injected into native session fixtures.

- [ ] Run `pnpm exec vitest run src/lib/neev-client.test.ts src/components/neev` for RED. Implement strict parsing (positiveIDs, enumroles, revisionsstrings, no unknown auth state accepted), 65s request timeout and AbortController/generation cancellation adapted from existing team auth provider. Native DTO: `{user:{id,displayName,avatarUrl},organization:{id,name,avatarUrl}|null,role:null|"admin"|"member",scope,csrf}`. Pre-org session routes to onboarding; no stale previous-org render while session reloads.
- [ ] Implement three login methods with configured/unavailable status. Google prominence first, email input second, GitHub optional secondary. Email submit uniformly says "If this address can receive a sign-in link, it will arrive shortly. Open it in this browser." Do not claim message sent on API202. Loading/delivery/provider-error states are keyboard announced. Never place email/token in telemetry, storage or URL.
- [ ] Onboarding shows create organization and exact existing membership choices, no GitHub install requirement. Invitation preview explicitly names company and requires confirm. Settings includes Back to workspace, profile/name/logo, owner/admin/member controls, own linked sign-in methods and link flow (recent-auth requirement), plus personal-agent section explaining runtime is not yet enabled. No in-app switcher; signout/signin clears all query/store/broadcast scope and shows memberships again.
- [ ] Reuse the current Notion-inspired sidebar, typography, dialog focus handling and nested tree visual components through explicit native adapters. Org header always visible; top-level action only New project; folder menu New subfolder; no broken repo/chat/tool controls. Show explanatory "Agent chats are being enabled" status instead of a send box that fails. Do not label this stage ready for public usage.
- [ ] Test noGitHub onboarding, orgcreate/select, inviteaccept, sameemail conflict/link guidance, owner protection, wrongpermission hiding + server errors, backnavigation, logout failure, lostsession, abortedoldorgresponse, tabBroadcastChannel invalidation, keyboard and375px layout. Translate each added key in all10existinglocale files. Run targeted Vitest, TS, ESLint/Prettier and static export; commit `feat(neev): add native onboarding and organization workspace UX`.

## Task 8: Integration evidence and private candidate readiness

**Files:** Create `docs/operations/neev-native-identity-acceptance.md`, native browser fixture tests and `scripts/neev-identity-preflight.mjs`; modify `docs/operations/neev-release-plan.md` and `docs/operations/public-release-checklist.md` with separate stage1 vs release gates. No production deploy in this task.

**Consumes:** All prior deliverables; isolated PostgreSQL and synthetic fixtures.
**Produces:** Auditable test evidence, environment contract and bounded deployment handoff. This task does not certify real providers from synthetic mocks.

- [ ] Add preflight assertions before fixing integration defects:

```js
const base = process.env.NEEV_PREFLIGHT_ORIGIN;
if (!base || !new URL(base).hostname.endsWith(".test")) {
  throw new Error("Use an explicitly isolated synthetic test origin");
}
const response = await fetch(`${base}/api/workos/capabilities`);
const capabilities = await response.json();
if (capabilities.identityMode !== "native" || capabilities.runtimeEnabled !== false) {
  throw new Error("Wrong server identity/runtime boundary");
}
for (const route of ["/api/team/session", "/api/settings", "/api/chat/send"]) {
  if ((await fetch(`${base}${route}`)).status !== 404) throw new Error(`Legacy route exposed: ${route}`);
}
```

- [ ] Run actual full nativeRust suite and emitted binary with test filters, not guessed test counts. IsolatedPG tests must run for constraints/concurrentconsumption/inviterremoval/finalwritechecks. Delete only explicitly created synthetic test resources after evidence capture; never clean old production volumes.
- [ ] Run `pnpm exec vitest run`, `pnpm exec tsc --noEmit`, touched-file ESLint/Prettier, `pnpm build`, native server build and `git diff --check`. Preserve any unrelated existing failures with exact outputs and independently reproduce; no false aggregate-green claim.
- [ ] Run synthetic browser paths: Google fixture login→createorg→project→folder→subfolder; emailrequest→scannerGET→confirm; invitation recipient2→explicitjoin; org2 denial; linking conflict; stale session logout; Settings backbutton; keyboard/mobile. Capture screenshots and console/network-error summary without token URLs. Verify native operations make zero GitHub calls.
- [ ] Write the fresh deployment environment contract and migration safety gate. Private candidate needs `NEEV_DATABASE_URL` to a new DB, separate native storage prefix/volume, fresh encryption key, approved Google client/secret, verified Resend sender/send-only key and exact trusted proxy list. Explicitly refuse old DB marker and old cookies; keep runtimefalse. Do not edit Coolify live service until staged acceptance and scoped deployment review.
- [ ] Record external blockers truthfully: owner Google policy/consent/client; Resend allrecordsverified + nonrewritten deliveredlink + actual recipient; HTTPSprivatecandidate; twoactualusers; isolatedagentcredentialcustody/durablejobs; repooptionalchats/docrevisions; website and fullrelease acceptance. Backups for freshDB+files and isolated restore proof precede retiring old jobs. No DELETE backup schedule action that removes archives.
- [ ] Request the required implementation code review, fix only scoped blockers, rerun affected gates, then commit `test(neev): verify native identity and tenant isolation boundary`. Publish only reviewed passing stage1 work to Workos; public rollout remains gated by later subprojects.

## Self-review and coverage

- Identity/schema/linking/collisions → Task1; sessions/revocation/freshdatabase/oldcredentialdenial → Task2.
- Googleverifiedproof + optionalGitHub + protocolattacks → Task3; magiclinks/mailquotas/scanners/privacy → Task4.
- Ownership/invites/rolechanges/races/organizationbranding → Task5; native hierarchy/noGitHubaccess → Task6.
- Login/settings/onboarding/orgidentity/locales/keyboard/mobile/staleclient → Task7.
- SQLite/PG/browser/security/real-provider distinction/freshdeployment-backup gate → Task8.
- Repository-optional chats, document history, isolated personal-agent jobs and public launch are explicitly OUTSIDE this approved subproject-one spec and require their focused designs. They are not silently counted as implemented by this plan.
- Library contract checked against [openidconnect 4.0.1 documentation](https://docs.rs/openidconnect/4.0.1/openidconnect/); no home-grown JWT verifier. Google contract: [official OIDC documentation](https://developers.google.com/identity/openid-connect/openid-connect). Mail contract: [Resend domain documentation](https://resend.com/docs/dashboard/domains/introduction).
- Plan self-review: all spec sections mapped; no implicit legacy migration, email merge, repo grant, production deployment, shared AI fallback or paid/terms permission. Interface types defined above; test fixtures must call the production services/protocol rather than mock the result being asserted.

Context: [org-memory: wiki/engineering/workos.md]. The September11 approved fresh-start direction supersedes its historical mandatory GitHub/operator model.
