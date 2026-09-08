# Team GitHub Membership Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Obtain validated GitHub identity and organization-membership evidence without granting workspace authority or trusting caller-supplied identity.

**Architecture:** A narrow HTTP adapter accepts a caller-owned user access token, pins the production API origin, and returns validated identity/membership metadata. Local HTTP fixture tests exercise real requests and failure behavior. The subsequent onboarding orchestrator owns encrypted credentials, proof timestamps, serialized refresh, membership revisions and issuing authorized principals.

**Tech Stack:** Existing Rust reqwest, tokio, serde, async-trait and Axum test fixtures; no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`

## Global Constraints

- Use a stable GitHub user ID for identity and stable GitHub organization ID for tenancy. Names/logins are display and lookup metadata, never authorization keys.
- A GitHub identity may belong to more than one organization, but each browser session operates in exactly one explicitly selected organization.
- A pending invitation is not membership; never accept invitations or change GitHub membership on the user's behalf.
- An empty/failed GitHub response never creates a personal organization or grants owner access.
- A positive GitHub membership proof may be cached for at most 60 seconds, keyed by stable user, organization and credential generation.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No protected operation accepts a client-supplied user ID, role or organization ID as authority.
- Use existing dependencies. Run one Cargo process at a time with `CARGO_INCREMENTAL=0`.
- No real GitHub/provider requests, token reads, production data, push or deployment from implementer tasks. Tests use invented credentials and loopback HTTP servers only.

## Scope and evidence

This plan implements only the evidence-acquisition adapter from spec section 1. It does not implement pending onboarding, refresh, CSRF, route/resource authorization, events or runner isolation. Do not wire live routes in this slice.

Official documentation checked on 2026-09-08:

- [Membership API](https://docs.github.com/en/rest/orgs/members#get-an-organization-membership-for-the-authenticated-user): GitHub App user tokens supported; list active memberships with pagination; individual check requires Members read and returns user/org identities, state and role.
- [Token refresh](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens): refreshing invalidates old access and refresh tokens. Do not reuse the legacy unsynchronized refresh helper.

The adapter accepts but never persists, refreshes or logs a token. Errors contain no response bodies, URLs, headers or credentials. It returns evidence, not a fully authorized principal.

### Task 1: Implement strict GitHub identity and membership requests

**Files:**
- Create `src-tauri/src/team_access/github.rs`: validated metadata types, interface and production construction.
- Create `src-tauri/src/team_access/github_http.rs`: bounded requests, parsing and pagination.
- Create `src-tauri/src/team_access/github_tests.rs`: loopback HTTP fixtures and behavior tests.
- Modify `src-tauri/src/team_access/mod.rs`: registration only.

**Interfaces:** Consume foundation `super::session::TeamRole`; produce:

```rust
pub(crate) enum GithubEvidenceError {
    ReauthenticationRequired, NotMember, Unavailable,
}
pub(crate) struct GithubIdentity {
    pub github_user_id: i64,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}
pub(crate) struct GithubOrganizationChoice {
    pub github_org_id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub role: super::session::TeamRole,
}
pub(crate) struct GithubMembershipEvidence {
    pub github_user_id: i64,
    pub organization: GithubOrganizationChoice,
}
#[async_trait::async_trait]
pub(crate) trait GithubMembershipSource: Send + Sync {
    async fn identity(&self, access_token: &str)
        -> Result<GithubIdentity, GithubEvidenceError>;
    async fn active_organizations(&self, access_token: &str, expected_user_id: i64)
        -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>;
    async fn verify_membership(
        &self, access_token: &str, expected_user_id: i64,
        expected_org_id: i64, organization_login: &str,
    ) -> Result<GithubMembershipEvidence, GithubEvidenceError>;
}
```

`GithubMembershipClient` has private reqwest client/origin fields and a crate-internal `new() -> Result<Self, GithubEvidenceError>` constructor. Only a `#[cfg(test)]` constructor may accept a loopback origin and shorter timeout. Metadata derives Debug/PartialEq/Eq, not Deserialize; use separate private wire DTOs. Error derives Debug/PartialEq/Eq and thiserror with static display strings. No database access, cache, externally public constructor or production origin override.

- [ ] **Step 1: Write real HTTP behavior tests first.** Bind Axum to `127.0.0.1:0`, return literal fixture JSON, record method/path/query/Authorization, and stop the fixture server on completion/drop. All tokens are invented.

```rust
let identity = client.identity("team-github-fixture-token").await.unwrap();
assert_eq!(identity.github_user_id, 301);
assert_eq!(identity.login, "alice");
let evidence = client.verify_membership(
    "team-github-fixture-token", 301, 202, "beta-company",
).await.unwrap();
assert_eq!(evidence.organization.github_org_id, 202);
assert_eq!(evidence.organization.role, TeamRole::Member);
assert_eq!(requests[0].method, "GET");
assert_eq!(requests[0].authorization, "Bearer team-github-fixture-token");
```

Required outcomes:

1. Identity happy path, missing/null optional metadata; zero/negative ID, malformed JSON, missing required fields, non-User account type or invalid login => Unavailable. Unsafe optional avatar/name metadata becomes None; assert this does not change the validated ID/login.
2. Active member/admin maps exactly to Member/Admin. Wrong stable user/org ID or different returned org login => NotMember. Unknown role/state or invalid identity shape => Unavailable. Pending individual membership => NotMember.
3. Empty active list returns empty, no personal fallback. Multiple pages collect active rows, verify expected user ID, sort by org ID. Duplicate IDs/contradictory rows => Unavailable; wrong user => NotMember. Pending rows are excluded, never silently accepted.
4. Requests use `state=active`, `per_page=100`, numeric `page`. Build pagination locally, never follow Link URLs; malicious Link cannot receive the token. Continue full pages, stop below 100 rows; 100 full pages => Unavailable, not truncated success.
5. 401 => ReauthenticationRequired; individual 404/non-rate-limited 403 => NotMember; rate-limited 403 (remaining=0 or retry-after), 429/5xx => Unavailable. Redirects, unexpected success status, malformed body and timeout => Unavailable. Assert redirect target receives no request.
6. Empty/whitespace or CR/LF token => ReauthenticationRequired; nonpositive expected IDs or invalid requested organization login => NotMember, all before HTTP. Error Debug/Display never contains the supplied token.
7. A 1 MiB decoded body cap works without Content-Length. Oversized streaming body => Unavailable. Timeout tests use a test-only shorter deadline.

- [ ] **Step 2: Capture RED.**

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::github_tests -- --test-threads=1
```

Expected initial failure: missing adapter/module. After interfaces exist, record focused behavioral failures for ID mismatch and pagination before adding those checks; unrelated compiler errors do not count. Use otherwise-valid response fixtures that would be accepted if the specific guard were absent. Do not present an always-deny stub's already-passing negative tests as guard-specific RED evidence.

- [ ] **Step 3: Implement bounded requests and validation.** Pin `https://api.github.com`, no redirects, User-Agent EazyWorkOS, Accept application/vnd.github+json, API version 2022-11-28 (preserve the existing repository contract). Set each request timeout to 15 seconds and wrap each complete adapter operation in a 15-second tokio timeout, including pagination.

```rust
let client = reqwest::Client::builder()
    .redirect(reqwest::redirect::Policy::none())
    .timeout(std::time::Duration::from_secs(15))
    .user_agent("EazyWorkOS")
    .build().map_err(|_| GithubEvidenceError::Unavailable)?;
```

Use Url path segments for `/user`, `/user/memberships/orgs` and `/user/memberships/orgs/{login}`. Login must be 1–39 ASCII alphanumeric/hyphen characters, begin/end alphanumeric, with no consecutive hyphens. Compare returned login case-insensitively but numeric IDs exactly. Identity must have `type == "User"`. Organization wire metadata need not have `type`; that endpoint does not promise it.

Only return avatar URLs that are absolute HTTPS on avatars.githubusercontent.com with no userinfo/fragment; invalid optional avatars become None. Optional display names must be at most 256 Unicode scalar values without control characters; invalid optional names become None, not truncated authority-bearing strings. Authorization never uses display metadata.

```rust
let mut body = Vec::new();
while let Some(chunk) = response.chunk().await
    .map_err(|_| GithubEvidenceError::Unavailable)? {
    if body.len().saturating_add(chunk.len()) > 1_048_576 {
        return Err(GithubEvidenceError::Unavailable);
    }
    body.extend_from_slice(&chunk);
}
let wire = serde_json::from_slice(&body)
    .map_err(|_| GithubEvidenceError::Unavailable)?;
```

Validate every listing row even with state=active. Exclude pending rows after validating shape/user; unknown state/role fails. Non-rate-limited 403 for listing or identity, 404 outside individual verification, and every otherwise unmapped status are Unavailable. Never interpret timeout/rate limit as proof of non-membership. This adapter does not write revoked state; a later orchestrator performs revision-controlled updates.

- [ ] **Step 4: Verify GREEN and compatibility.**

```bash
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access::github_tests -- --test-threads=1
CARGO_INCREMENTAL=0 cargo test --no-default-features --lib team_access -- --test-threads=1
git diff --check
```

Record counts, actual RED/GREEN output and existing warnings. Confirm no database mutations/live routes. Synthetic fixture passes are not real GitHub consent acceptance.

- [ ] **Step 5: Self-review and commit owned files only.**

```bash
git add src-tauri/src/team_access/github.rs src-tauri/src/team_access/github_http.rs src-tauri/src/team_access/github_tests.rs src-tauri/src/team_access/mod.rs
git commit -m "feat(auth): validate GitHub organization membership evidence"
```

## Controller acceptance and continuation

- [ ] Independent task and combined adapter review; fix Critical/Important findings.
- [ ] Fresh controller test run and scoped diff verification.
- [ ] Continue encrypted pending onboarding, serialized credential refresh and membership/session orchestration; do not expose this adapter as a complete authorization chain.

## Plan self-review

Explicit scope is the network adapter only; spec sections 2–6 remain separate gates. All consumed/produced types are defined and role is supplied by the foundation. Wire DTOs cannot bypass validation. Tests exercise real HTTP with invented credentials. Only existing module registration changes; no legacy token bridge is reused.
