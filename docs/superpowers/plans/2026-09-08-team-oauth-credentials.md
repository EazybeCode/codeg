# Team OAuth Credential Primitives Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide encrypted, identity-bound storage and a strict OAuth exchange/refresh client for the approved team onboarding flow.

**Architecture:** Add separate team OAuth-attempt, pending-onboarding and credential tables without promoting legacy sessions/tokens. A typed AES-GCM vault binds ciphertext to its intended record identity. A bounded OAuth client returns validated secret-bearing token sets, never workspace authority. The next service layer consumes these to implement single-use selection, serialized refresh and verified request principals.

**Tech Stack:** Existing Rust SeaORM/SQLite/PostgreSQL, aes-gcm, rand, sha2, base64, reqwest, tokio, serde, async-trait, thiserror; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`

## Global Constraints

- Use a stable GitHub user ID for identity and stable GitHub organization ID for tenancy. Names/logins are display and lookup metadata, never authorization keys.
- Keep tokens encrypted on the server.
- Onboarding records authorize only membership discovery and selection, never workspace operations.
- A local administrator's block survives OAuth login and membership refresh; only an authorized administrator can lift it.
- Existing `users.org_id` and `users.role` fields remain compatibility data while legacy code is isolated; team authorization must never use them as its source of truth.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No real credentials, GitHub/provider requests, cloud writes, push or deployment in implementation tasks. Tests use invented credentials and isolated local databases/HTTP servers only.
- Use existing dependencies and one Cargo process at a time with `CARGO_INCREMENTAL=0`.

## Verified protocol context

GitHub's documented authorization-code flow supports PKCE S256. A refresh invalidates the refresh token used and the previous access token; a missing newly rotated refresh token must not be replaced by the old one. Both token lifetimes are returned by GitHub. Nonexpiring user tokens omit all refresh/expiry fields. Error responses may use HTTP 200 with an error field.

Sources checked 2026-09-08: [user access tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app), [refresh](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens).

## Scope and next integration

No production callers or route registration in this plan. Storage models are not authorization. The subsequent orchestrator must atomically consume pending onboarding on successful selection, keep local blocks, persist rotated credentials before use, and invalidate stale membership proofs. It must serialize refresh across requests/processes using the credential state/lease fields; an ambiguous lost refresh outcome requires reconnect, never retrying the old refresh token.

### Task 1: Add encrypted team credential storage and typed secret bindings

**Files:**
- Create `src-tauri/src/db/entities/team_oauth_attempt.rs`, `team_pending_onboarding.rs`, `team_github_credential.rs`.
- Create `src-tauri/src/db/migration/m20260908_000002_team_credentials.rs` and `team_credentials_tests.rs`.
- Create `src-tauri/src/team_access/secrets.rs` and `secrets_tests.rs`.
- Modify entity/migration registries and `team_access/mod.rs` for registration only.
- Modify `src-tauri/src/db/migration/team_access_tests.rs` only to find the foundation migration by name when preparing the preceding schema; its current `migrations().len() - 1` assumes foundation is last and must not drift when another migration is added.

**Storage contract:** All fields NOT NULL unless marked optional. No foreign keys or serde derives on entity models. Preserve all existing rows unchanged and create no credentials from legacy encrypted-token columns.

| Table | Exact columns |
| --- | --- |
| team_oauth_attempt | state_hash TEXT primary key nonauto; browser_hash TEXT; encrypted_verifier TEXT; created_at, expires_at DateTimeUtc |
| team_pending_onboarding | token_hash TEXT primary key nonauto; csrf_hash TEXT; github_user_id i64; encrypted_payload TEXT; created_at, expires_at DateTimeUtc |
| team_github_credential | user_id i32 primary key nonauto; github_user_id i64; generation i64; state TEXT; encrypted_payload optional TEXT; refresh_owner optional TEXT; refresh_expires_at optional DateTimeUtc; created_at, updated_at DateTimeUtc |

Named checks: pending/credential GitHub IDs positive, credential user_id positive, generation positive, state in `active`/`refreshing`/`reauth_required`. One named shape check: active => nonnull payload and no lease; refreshing => nonnull payload/owner/deadline; reauth_required => all three null. Unique index credential.github_user_id. Expiry indexes on OAuth attempts and pending onboarding. No state/generation defaults: callers must choose explicitly. Migration rerun preserves data; down refuses if any of the three tables contains data, checking all before dropping any. Use the foundation's trusted static named-column-check pattern where SeaQuery cannot name checks directly; no untrusted SQL fragments.

**Vault interfaces:**

```rust
pub(crate) enum SecretBinding<'a> {
    OAuthAttempt { state_hash: &'a str, browser_hash: &'a str },
    PendingOnboarding { token_hash: &'a str, github_user_id: i64 },
    GithubCredential { user_id: i32, github_user_id: i64, generation: i64 },
}
pub(crate) struct SecretVault { cipher: aes_gcm::Aes256Gcm }
pub(crate) enum SecretError { InvalidInput, Unavailable }
impl SecretVault {
    pub(crate) fn from_hex_key(key: &str) -> Result<Self, SecretError>;
    pub(crate) fn seal(&self, binding: SecretBinding<'_>, plaintext: &[u8])
        -> Result<String, SecretError>;
    pub(crate) fn open(&self, binding: SecretBinding<'_>, blob: &str)
        -> Result<Vec<u8>, SecretError>;
}
pub(crate) fn random_browser_secret() -> Result<String, SecretError>;
pub(crate) fn hash_browser_secret(secret: &str) -> Result<String, SecretError>;
pub(crate) fn pkce_challenge(verifier: &str) -> Result<String, SecretError>;
```

SecretError has Copy/Clone/Debug/Eq/PartialEq/thiserror with static messages only. Key exactly 64 ASCII hex chars (either case), decode to 32 bytes; no env access/fallback inside vault. Browser secrets and all binding hashes exactly 64 lowercase hex chars. IDs/generation in bindings positive. Verifier is a browser secret generated by the same secure generator. `pkce_challenge` is BASE64URL_NO_PAD(SHA256(verifier ASCII)), 43 characters. Random secret uses 32 OsRng bytes; entropy failure returns Unavailable.

Wire ciphertext is `v1.` plus standard base64(nonce[12] || authenticated ciphertext). Plaintext is 1..=16384 bytes. Reject oversized encodings before decode/allocation; decoded payload at most 12+16+16384. Open failure/tampering/unknown version => Unavailable, never include blob/key/context/plaintext in error Debug/Display. Binding validation failure => InvalidInput.

Authenticated associated data is a fixed domain prefix plus a typed, unambiguous identity encoding:

```rust
let aad = match binding {
    SecretBinding::OAuthAttempt { state_hash, browser_hash } =>
        format!("workos/team/v1/oauth/{state_hash}/{browser_hash}"),
    SecretBinding::PendingOnboarding { token_hash, github_user_id } =>
        format!("workos/team/v1/pending/{github_user_id}/{token_hash}"),
    SecretBinding::GithubCredential { user_id, github_user_id, generation } =>
        format!("workos/team/v1/credential/{user_id}/{github_user_id}/{generation}"),
};
let payload = aes_gcm::aead::Payload { msg: plaintext, aad: aad.as_bytes() };
```

Validate bindings before building AAD. Fresh random nonce per seal; do not expose a production nonce override. No custom cryptographic primitives or key logging.

- [ ] **Step 1: Write behavior tests before implementation.** Real SQLite migration/roundtrip tests, invalid rows and downgrade preservation. Vault tests encrypt an invented token, recover it under the same binding, and reject wrong key, user, GitHub ID, generation, pending hash/browser hash, cross-purpose copy and modified ciphertext. Reject invalid key/hashes/IDs, empty/oversized plaintext, oversized/invalid base64, truncated ciphertext and unknown versions. Independently known SHA256/PKCE vectors; assert generated secret valid shape and independent samples differ. No raw fixture value in returned errors. Include a data-bearing rerun and all three populated down cases.

```rust
let blob = vault.seal(SecretBinding::GithubCredential {
    user_id: 301, github_user_id: 9301, generation: 2,
}, b"invented-access-token").unwrap();
assert_eq!(vault.open(SecretBinding::GithubCredential {
    user_id: 301, github_user_id: 9301, generation: 2,
}, &blob).unwrap(), b"invented-access-token");
assert_eq!(vault.open(SecretBinding::GithubCredential {
    user_id: 302, github_user_id: 9301, generation: 2,
}, &blob), Err(SecretError::Unavailable));
```

- [ ] **Step 2: Run RED** with `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_credentials -- --test-threads=1` and the `team_access::secrets_tests` filter. Record actual missing-module and behavior failures; don't claim already-passing negative tests against a denial stub as guard RED.
- [ ] **Step 3: Implement schema and AES-GCM vault only**, keeping encryption failure fatal to the caller. Use existing entity/migration conventions and registries; fix the foundation test's migration-position assumption by exact name lookup, not a new numeric offset.
- [ ] **Step 4: Run GREEN** for `team_credentials`, `team_access`, and explicit isolated PostgreSQL storage roundtrip. Use `postgres://air1@localhost:5432/postgres`, generate exact `workos_team_credentials_test_` plus UUID-simple suffix; connect to and check current_database against that exact name before DROP. Preserve a failed fixture for controller-directed cleanup; never target production/existing DBs. Test the actual named checks and shape constraint in PostgreSQL, not SQL string snapshots alone. Run `git diff --check`.
- [ ] **Step 5: Self-review and commit only the listed files** with message `feat(auth): add identity-bound encrypted credential storage`. Report counts, RED/GREEN, PostgreSQL cleanup evidence and warnings.

### Task 2: Add strict GitHub OAuth exchange and refresh client

**Files:** Create `src-tauri/src/team_access/oauth.rs`, `oauth_http.rs`, `oauth_tests.rs`; register in `team_access/mod.rs` only.

**Interfaces:** Consume Task 1's `pkce_challenge` for request construction/tests where appropriate. No database calls. Produce:

```rust
pub(crate) enum OAuthError { ReauthenticationRequired, Unavailable }
pub(crate) struct GithubTokenSet {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) access_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) refresh_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}
#[async_trait::async_trait]
pub(crate) trait GithubOAuthSource: Send + Sync {
    async fn exchange_code(&self, code: &str, verifier: &str,
        now: chrono::DateTime<chrono::Utc>) -> Result<GithubTokenSet, OAuthError>;
    async fn refresh(&self, refresh_token: &str,
        now: chrono::DateTime<chrono::Utc>) -> Result<GithubTokenSet, OAuthError>;
}
pub(crate) struct GithubOAuthClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    callback_url: reqwest::Url,
    origin: reqwest::Url,
    timeout: std::time::Duration,
}
impl GithubOAuthClient {
    pub(crate) fn new(client_id: String, client_secret: String,
        callback_url: reqwest::Url) -> Result<Self, OAuthError>;
    pub(crate) fn authorization_url(&self, state: &str, verifier: &str)
        -> Result<reqwest::Url, OAuthError>;
}
```

Client and token set must not derive Debug/Serialize/Deserialize. Use private Deserialize wire DTOs; the next storage orchestrator deliberately serializes secret fields only inside the vault. Error derives Copy/Clone/Debug/Eq/PartialEq/thiserror with static strings. A test-only constructor may replace OAuth origin with a validated loopback URL and shorten deadlines. Declare oauth_http.rs as a private child of oauth.rs using a path attribute so request implementation can access the parent's private fields without exposing production origin/config overrides.

Production origin fixed `https://github.com`. Callback URL allows HTTPS or explicit literal loopback HTTP (127.0.0.1 or ::1) only; no userinfo/query/fragment, path exactly `/auth/github/callback`. No request-controlled callback. Client ID/secret nonempty ASCII graphic characters, max4096 bytes. Authorization URL fixed `/login/oauth/authorize`, exact configured client_id/redirect_uri, supplied valid 64-lowerhex state, PKCE S256 challenge, method=S256; no legacy scope parameter and no automatic account/org selection.

POST `/login/oauth/access_token` uses form body, never query strings for secrets. Exchange body client_id/client_secret/code/redirect_uri/code_verifier. Refresh body client_id/client_secret/grant_type=refresh_token/refresh_token. User-Agent EazyWorkOS, Accept application/json. No redirects; 15-second request and whole-operation deadlines, 64KiB decoded response cap, stream chunks before JSON parse. Token/code input must be nonempty ASCII graphic, max4096 bytes; code verifier must pass the browser-secret validation. Invalid input => ReauthenticationRequired before HTTP (invalid constructor config => Unavailable).

Successful response: status200, token_type `bearer`, nonempty ASCII-graphic access token <=4096. Expiring set requires all of positive expires_in, nonempty refresh_token, positive refresh_token_expires_in. Cap both durations at 366 days and use checked time addition. Nonexpiring exchange response may omit all three fields; partial sets are Unavailable. A refresh response always requires the complete new expiring set; never retain the submitted refresh token as a fallback. Unknown extra response fields are ignored; invalid known-field types fail.

HTTP401 and known OAuth errors bad_verification_code/bad_refresh_token/access_denied/expired_token => ReauthenticationRequired. Rate limit, 5xx, config errors such as incorrect_client_credentials/redirect_uri_mismatch, unknown error, non200 status, timeout, malformed/oversized body, redirect => Unavailable. Never return external error_description or include secrets in errors. Both error and token fields in a response still fail. Storage failure handling/leases are explicitly not this client's job.

- [ ] **Step 1: Write actual loopback HTTP tests first** for authorization URL/PKCE, exact form fields and absence from URL, expiring/nonexpiring exchange, complete refresh replacement, missing rotated refresh token, partial/negative/overflow lifetimes, invalid token_type/token fields, HTTP200 OAuth errors, redirects with zero receiver calls, streaming body cap and timeouts. Fixture tokens/codes only; servers abort on drop.

```rust
let tokens = client.refresh("invented-refresh-token", fixture_now).await.unwrap();
assert_eq!(tokens.access_token, "invented-new-access");
assert_eq!(tokens.refresh_token.as_deref(), Some("invented-new-refresh"));
assert_eq!(tokens.access_expires_at, Some(fixture_now + chrono::Duration::seconds(28800)));
assert_eq!(requests[0].query, None);
assert_eq!(requests[0].form["grant_type"], "refresh_token");
```

- [ ] **Step 2: Run RED** using `CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::oauth_tests -- --test-threads=1`. Record expected missing functionality and meaningful response/guard failures before implementation.
- [ ] **Step 3: Implement only the bounded OAuth adapter** with separate private wire DTOs and strict response validation. Reuse dependency APIs and Task 1 PKCE helper; do not import legacy encrypt/decrypt/ensure_github_token or call live GitHub.
- [ ] **Step 4: Run GREEN** for `team_access::oauth_tests`, `team_access`, `team_credentials` and existing `oauth_state_tests`. Run `git diff --check`; record actual output and known warnings.
- [ ] **Step 5: Self-review and commit only the four owned files** with message `feat(auth): add bounded GitHub OAuth code and refresh exchanges`.

## Controller acceptance and continuation

Each task receives independent spec/quality review, then a combined plan review and fresh controller verification. Continue to atomic onboarding/credential orchestration; no production deployment of disconnected primitives.

## Plan self-review

The plan implements storage/crypto and OAuth protocol portions of spec section1, not live authorization/UI or runner safety. Table lease fields support the stated distributed-refresh contract without pretending it is implemented. Both backends have real migration coverage; old tests are made robust to the additional migration by identity, not index arithmetic. Secret types cannot be automatically serialized into HTTP projections. Task2 uses Task1's PKCE helper and no other unprovided interface. Later CSRF can use a separate HttpOnly random-token cookie bound by the existing session csrf_hash and returned only through same-origin no-store identity responses; it does not require unsafe custom cryptography.
