# Team Organization Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a verified organization admin safely edit the current organization's display name and logo, with a useful read-only view for members and a clear return to the workspace.

**Architecture:** Reuse the existing branding validation and visual controls, but route team requests through the isolated team manifest and the reviewed session principal. A transaction-scoped service checks current authority before committing branding changes; a team-only settings controller cancels obsolete requests and keeps recoverable form drafts bound to their original session scope.

**Tech Stack:** Existing Rust/Axum/SeaORM/image, React/next-intl, static Next export, Vitest and cached Playwright. No new dependencies or migrations.

**Spec:** `docs/superpowers/specs/2026-09-08-team-access-and-chat-visibility-design.md`, particularly sections1–2 and5; owner-approved organization name/logo editing.

Status: prepared during HTTP/browser acceptance. Do not execute before that plan's final review and fresh verification pass. This is the current-org settings part of the following workspace conversion, not a replacement for catalog/chat privacy or cloud execution.

## Global Constraints

- Existing desktop/native mode stays separate for compatibility.
- A shared operator token never authenticates a team route.
- No protected operation accepts a client-supplied user ID, role or organization ID as authority.
- State-changing requests require the exact configured origin plus a session-bound CSRF token; same-origin WebSocket upgrades are mandatory.
- Admin actions and new execution/publication requests require a fresh proof.
- Static Next export remains supported using fixed pages and query parameters. All labels/errors are translated in the existing ten locales.
- Keep the current pilot private and runtime disabled during migration and intermediate slices.
- No real credentials, GitHub/provider requests, cloud writes, push or deployment in implementation tasks. Use invented credentials, isolated local databases and loopback fake providers.
- One Cargo process at a time, `CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=1`; preserve unrelated edits and use existing dependencies.
- Preserve the organization → projects/categories → arbitrarily nested folders/working directories → repository-backed chats hierarchy. This plan does not change that hierarchy or expose its unported routes.

## Interfaces and preflight

Consume the accepted `PrincipalService::resolve(headers, ProofRequirement::{Cached,Fresh})`, `AuthContext`, `PrincipalService::recheck_in_transaction(transaction, context)`, `TeamClock`, static `TeamAuthError`, and `team_access::core::lock_user_identity(transaction,user_id,github_user_id)`.

`AuthContext` supplies local user/org/membership IDs, role, authorization revision, credential generation, session hash and CSRF hash. It does not supply GitHub IDs. Obtain the stable GitHub user ID from the selected database identity, never a request field or a fabricated context. The final guard locks membership then exact session. Any earlier write locks must follow user → organization → membership → session, matching onboarding/credential operations. Do not acquire an organization lock and then call a helper that starts another transaction.

Existing organization normalization in `web/handlers/organization_settings.rs` is reusable business logic; its `current_user_result`, `users.org_id`, `users.role` and `BrowserMutationPolicy` are not team authority. The current React OrganizationSettings component is also not a team transport: it directly calls `/auth/organization` and has unscoped save/FileReader callbacks. Do not mount it unchanged.

The current TeamAuthProvider clears the displayed session during focus/revalidation. A form's controller must therefore live above the signed-in shell's conditional content, keeping same-scope drafts hidden while verification is pending, and clearing them on a confirmed different session/org/user or unauthenticated state. Do not change the fail-closed authority behavior merely to keep a form mounted.

### Task 1: Expose bounded current-organization settings through team authority

**Files:** Create `src-tauri/src/organization_branding.rs`, `src-tauri/src/team_server/organization_settings.rs`, `src-tauri/src/team_server/organization_settings_tests.rs`, `src-tauri/src/team_server/handlers/organization.rs`, `src-tauri/src/team_server/http_organization_tests.rs`. Modify lib.rs only to register the crate-private pure branding module; modify existing `web/handlers/organization_settings.rs` to delegate its pure validators; keep existing legacy authorization intact. Register modules in team_server/mod.rs and handlers/mod.rs, and exact routes in team_server/router.rs. Tests may minimally expose already-existing cfg(test) helpers needed for controlled clocks/disposable PostgreSQL; no new production test hooks or provider overrides.

**Produces:**

```rust
// organization_branding.rs: static errors, no authority or I/O.
pub(crate) enum BrandingError { InvalidName, InvalidLogo }
pub(crate) fn validate_display_name(name: &str) -> Result<String, BrandingError>;
pub(crate) fn normalize_logo_data_uri(data: &str) -> Result<String, BrandingError>;

// team_server/organization_settings.rs: JSON DTO fields use camelCase.
pub(super) struct OrganizationSettingsDto {
    pub(super) id: i32,
    pub(super) login: String,
    pub(super) name: String,
    pub(super) avatar_url: Option<String>,
    pub(super) can_edit: bool,
}
pub(super) struct OrganizationPatchInput {
    pub(super) name: String,
    pub(super) logo: OrganizationLogoInput,
}
pub(super) enum OrganizationLogoInput { Keep, Remove, Upload { data: String } }
pub(super) async fn read_current(
    state: &TeamState, context: &AuthContext,
) -> Result<OrganizationSettingsDto, ApiError>;
pub(super) async fn update_current(
    state: &TeamState, context: &AuthContext, input: OrganizationPatchInput,
) -> Result<OrganizationSettingsDto, ApiError>;
```

Input struct and tagged `{action:"keep"|"remove"|"upload",data?:string}` enum reject unknown fields. Do not derive Debug for uploaded-image inputs or include raw submitted values in errors. Response never serializes an entity, session or credential. The display name is the stored name with login fallback when it is absent; canEdit derives solely from the verified TeamRole.

Preserve trim/1–80 Unicode-codepoint/control-character name rules. Preserve supported PNG/JPEG/WebP data URIs, 1 MiB decoded upload maximum, 1024×1024 maximum dimensions and 16 MiB image allocation limit, MIME/content matching and normalization to PNG. Bound normalized PNG output to 5 MiB before base64 projection. Do not fetch remote logos or accept SVG/HTML. Pure normalization runs outside the database transaction; if moved to spawn_blocking, preserve the bounded input/allocation and map task failure statically. No user content in logging.

The team manifest adds only GET and PATCH `/api/team/organization`; explicitly deny HEAD as existing protected routes do. Apply the existing shared request budget/60-second timeout/no-store/static logging wrapper. GET resolves Cached proof; PATCH resolves Fresh proof and requires admin. PATCH requires exactly configured Origin, JSON content type and the same session-bound CSRF cookie/header/hash checks as existing team mutations. Read its body with an explicit 2 MiB cap, mapping malformed/oversized/unknown-field input to static invalid_request400. Do not reuse the auth-only16KiB body bound for logo payloads. No org selector in path/query/body becomes authority.

Both services perform their final principal check in the transaction that reads/writes the organization projection. Read the server identity's immutable GitHub ID, acquire the existing user identity lock, lock the selected organization row (Postgres row lock; SQLite write serialization), then call recheck_in_transaction. Require admin again for mutation. Keep the row/authority locks until commit. For `keep`, preserve the logo from this locked row, not a pre-normalization snapshot. Set only name/avatar_url/updated_at: never change GitHub org ID/login, user tenant fields, membership role or repository mappings. Database failures map to static ApiError; no legacy database logging conversion. No network call while holding write locks.

- [ ] **Step1: Add failing service and actual-router tests.** Use the accepted unseeded HTTP onboarding helpers for Alice/admin A+B, Bob/member A and Carol/admin B. Complete real service selection before constructing requests; no legacy owner cookie. The decisive request shape is:

```rust
let response = server.patch("/api/team/organization")
    .add_header(header::COOKIE, &alice.cookies)
    .add_header(header::ORIGIN, "https://workos.example")
    .add_header("x-workos-csrf", &alice.csrf)
    .json(&serde_json::json!({"name":"Atlas Team","logo":{"action":"keep"}}))
    .await;
response.assert_status_ok();
assert_eq!(response.json::<serde_json::Value>()["name"], "Atlas Team");
```

Test missing/old/operator/pending/duplicate cookies, wrong/missing/duplicate Origin or CSRF, unknown actor/org/role fields, non-JSON and malformed/oversized bodies. Test member read-only and denied writes despite forged legacy owner role, independently scoped dual-org sessions, only A changing, preserved GitHub IDs/repositories, fresh proof even when the prior proof is cached, demotion/revocation/credential-generation/logout before final guard, upstream failure without a write, and full rollback on rejected authority. Test `keep` against an intervening logo update and remove/upload round trips. Include empty/long/control/Unicode names and mismatched/oversized/corrupt/unsupported images plus valid PNG/JPEG/WebP. Preserve existing legacy normalization/handler controls.

For final-guard tests resolve a real AuthContext, commit the controlled revocation/demotion/session deletion/credential change, and call the same update_current service used by the handler with that stale context; assert no write. Use disposable PostgreSQL for lock/order and rollback controls as well as SQLite, without a parallel Cargo runner or live database. Do not create a second test database harness.

- [ ] **Step2: Run RED.** `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --no-default-features --lib organization_settings -- --test-threads=1`. Record actual failures; a missing symbol can establish initial wiring RED but cannot replace the subsequent authorization assertions.
- [ ] **Step3: Implement the shared pure validators, transaction service and manifest handlers above.** Keep the pure code's errors separate from HTTP status conversion; delegate legacy validators without changing legacy authority. The handler passes only its resolved context into the service:

```rust
let context = state.principals.resolve(request.headers(), ProofRequirement::Fresh).await?;
if context.role != TeamRole::Admin { return Err(ApiError::Forbidden); }
// Verify the bounded request's Origin/CSRF/JSON, then:
update_current(&state, &context, input).await
```

- [ ] **Step4: Run GREEN and regressions.** Cover organization_settings and http_organization tests, explicit guarded PostgreSQL cases, existing team_server (`--include-ignored --skip team_browser_fixture_server`), team_access, credential migrations and legacy organization_settings tests. Use the matching test filter names emitted by `--list`, record exact commands and exits, then `git diff --check` and standalone no-default-features check. Browser fixture is opt-in and runs through its driver in Task3.
- [ ] **Step5: Self-review and scoped commit** `feat(settings): authorize current organization branding in team mode`.

### Task 2: Add useful team settings with scoped draft and upload lifetimes

**Files:** Modify `src/lib/team-client.ts` and `.test.ts`, `src/components/team/team-signed-in-shell.tsx` and `.test.tsx`, and `src/app/settings/layout.tsx`; create `src/components/team/team-organization-settings.tsx`, `.test.tsx`, `src/components/team/use-team-organization-settings.ts`, `.test.tsx`. Add a focused settings-layout test if one is absent. Reuse shared existing setting-card/section/input/button styles; add needed copy in all ten src/i18n/messages locale files. Do not change or mount the legacy OrganizationSettings transport, SettingsShell, update provider, agent provider or global settings API.

**Produces:**

```typescript
export interface TeamOrganizationSettings {
  id: number; login: string; name: string;
  avatarUrl: string | null; canEdit: boolean;
}
export type TeamOrganizationLogo =
  | { action: "keep" } | { action: "remove" }
  | { action: "upload"; data: string };
export function loadTeamOrganizationSettings(
  signal?: AbortSignal
): Promise<TeamOrganizationSettings>;
export function updateTeamOrganizationSettings(
  input: {name: string; logo: TeamOrganizationLogo},
  csrf: string, signal?: AbortSignal
): Promise<TeamOrganizationSettings>;
export function TeamOrganizationSettingsPage(): React.JSX.Element;
```

Use the existing private team-client request/response lifetime implementation, not another fetch wrapper. Paths are fixed `/api/team/organization`; mutations use PATCH and x-workos-csrf. Validate all DTO fields, reject malformed responses and check returned org ID against the controller's current verified scope before displaying it. Never Number/parseInt a GitHub ID; this DTO does not contain one. Keep timeout/cancellation through JSON body consumption, same-origin credentials, no-store and static error mapping.

Team settings layout renders TeamOrganizationSettingsPage for team mode and ignores all legacy settings child pages. Preserve native/operator layout and the unknown-mode no-effects branch. The team view clearly says Organization settings. Keep fixed `/settings` and Back to workspace; add a visible Settings link in the authenticated workspace shell. Unsupported legacy deep paths such as `/settings/agents` remain404 in the team server; do not broaden the static manifest to make them appear supported. A layout unit test may pass legacy child elements to prove they never mount, but that is not a promise to serve their URLs.

Refine TeamSignedInShell to accept optional authenticated children while retaining existing login/unavailable/loading controls and safe user/org identity. Its existing runtime-unavailable explanation remains when children are omitted. TeamOrganizationSettingsPage calls a focused useTeamOrganizationSettings controller above this conditional child boundary, so the controller can retain a hidden draft during same-scope revalidation. The page renders a controlled form inside the shell; the hook must not independently activate/deactivate authentication (the shell already owns that lifecycle).

The form shows immutable GitHub organization login, editable display name, current logo preview, Upload/Remove, member-read-only explanation and Save. The controller owns loading/saving/file-reading/errors and the original session scope key. Scope comprises the checked session fingerprint + org + user, not org alone. Abort requests and file reads and invalidate their generation when scope becomes unavailable or changes; ignore late results. Keep drafts only in component memory: never localStorage, URLs, BroadcastChannel or another account. Hide all form content during authority revalidation; preserve same-scope drafts on recoverable load/save failures. Clear on confirmed different session/org/user, logout/401 or component destruction. Do not silently submit after reauthentication.

Read a chosen file only after client type/1MiB validation. Cancel/ignore late FileReader completion on replacement, remove, save, scope loss or unmount. Server normalization is authoritative; use only its returned image after save. Avoid displaying unsanitized SVG/remote uploaded URLs. Save uses the current session's CSRF and disabled/busy guards, preserves the draft on retryable failure, focuses the translated error, and announces success accessibly. After a successful same-scope save, refresh session branding and emit the existing invalidation-only signal; never broadcast the settings payload. Other tabs update on existing focus/channel refresh. If authority changes during save, do not attach its result to the new scope.

- [ ] **Step1: Write failing client/controller/render tests.** Include exact route/method/body/CSRF, bad DTOs, request/body aborts; member read-only with no enabled upload/save; name/logo/remove/save; duplicate-submit guard; same-scope retry preserving unsent data; stale read/save/FileReader after logout or dual-org login; hidden retained draft during pending revalidation; different-session same-org draft clearing; no unscoped identity event or legacy requests. Test the team settings layout renders only the supported view, ignores passed legacy child elements before their hooks mount, and keeps Back to workspace.

```tsx
expect(screen.getByLabelText(/GitHub organization/i)).toHaveAttribute("readonly");
await user.clear(screen.getByLabelText(/display name/i));
await user.type(screen.getByLabelText(/display name/i), "Atlas Team");
await user.click(screen.getByRole("button", {name:/save/i}));
expect(updateTeamOrganizationSettings).toHaveBeenCalledWith(
  {name:"Atlas Team",logo:{action:"keep"}}, checkedCsrf, expect.any(AbortSignal)
);
```

Use mock DTOs with real validated field shapes and controlled promises/readers, not source-string assertions. Verify keyboard labels/tab activation, aria-busy, loading/error/success states and mobile-width layout. Preserve the existing shell's user/org branding and no-legacy-provider tests.

- [ ] **Step2: Run RED** with Node22 Vitest on the focused client/controller/components, max/minWorkers1; record behavioral failures.
- [ ] **Step3: Implement client and controller, then the controlled form and exact team layout branch.** The layout guard remains before legacy hooks:

```tsx
if (mode === "unknown") return null;
if (mode === "team") return <TeamOrganizationSettingsPage />;
return <OperatorSettingsLayout>{children}</OperatorSettingsLayout>;
```

- [ ] **Step4: Run GREEN.** Focused tests, TeamAuthProvider/shell/layout/mode/transport regressions, existing legacy organization/settings-return tests and locale coverage, TypeScript, scoped ESLint, static Next export and diff check. Do not relabel a jsdom pass as browser acceptance.
- [ ] **Step5: Self-review and scoped commit** `feat(settings): add scoped organization branding controls`.

### Task 3: Exercise organization settings in independent real browsers

**Files:** Modify the accepted `scripts/tests/team-auth-e2e.cjs`; modify cfg(test) `team_server/browser_fixture.rs` or its accepted fake-provider helper file only when needed for a controlled role/outage scenario. Extend the existing fixture and actual OAuth flow; do not start a second mock application or insert session/operator cookies. Preserve all accepted prior onboarding/security journeys.

**Consumes:** Task1 exact organization routes and Task2 rendered team settings. Use existing PLAYWRIGHT_MODULE, WORKOS_E2E_TEST_BINARY, driver-owned dynamic loopback listeners and preserved evidence directories. Build one matching no-default-features library test binary and the exact candidate static export.

- [ ] **Step1: Add actual-browser settings assertions.** Alice/admin A uploads an invented bounded PNG, changes name, sees save success, returns to workspace and sees updated branding. Bob/member A sees the update after focus/reload, read-only controls and denied direct PATCH even when supplying forged role/org fields. Carol/B and Alice's separate B session keep B branding. Alice removes A logo and preserves GitHub login. Verify malicious SVG/corrupt/oversized file rejection and a recoverable save outage without lost same-scope text.

```javascript
await alice.getByRole('link', {name: 'Settings', exact: true}).click();
await alice.getByLabel('Display name', {exact: true}).fill('Atlas Team');
await alice.getByRole('button', {name: 'Save changes', exact: true}).click();
await alice.getByRole('link', {name: 'Back to workspace', exact: true}).click();
await expect(alice.getByText('Atlas Team', {exact: true})).toBeVisible();
```

Use the exact finalized English labels in the driver. Add a pending-settings-response/logout/relogin race and confirm previous org text/logo/unsent name do not enter the new scope. Verify direct unsupported legacy settings paths remain404 without their old providers or APIs. Preserve the fixture's no-real-external-request checks and secret-free console/network collection; any expected negative HTTP response is narrowly matched to its deliberate action.

- [ ] **Step2: Run the first actual-browser acceptance and record its result honestly.** Task2 owns feature RED/GREEN; these combined browser assertions may pass on their first completed run. Do not reset/move the shared worktree, rebuild an obsolete candidate or manufacture a production failure to call this RED. If a production defect appears, preserve its bounded reproduction and request an in-scope fix assignment before editing files outside this test task.
- [ ] **Step3: Keep only minimal test-only controls needed for deterministic timing/outages.** No production URL override, auth bypass, or legacy provider mount. Bound shutdown and clean only exact owned synthetic runtime files/processes, preserving screenshots/diagnostics.
- [ ] **Step4: Run GREEN** desktop1440 and mobile375, full prior onboarding acceptance plus these settings journeys, console/page/network checks, and exact process cleanup. Inspect screenshots rather than relying only on screenshot creation. Record command, candidate, evidence paths and fake-provider limitation.
- [ ] **Step5: Self-review and scoped commit** `test(settings): verify scoped organization branding in browsers`.

## Controller acceptance and continuation

Run task-scoped independent reviews, fresh covering Rust/HTTP/PostgreSQL/UI/browser verification and one final combined review. Record candidate and evidence in the release checklist. No production deployment or runtime enablement follows from settings acceptance. Proceed to catalog resource authority/chat audience conversion, preserving the existing nested structure and private defaults.

## Self-review

Coverage: current-org settings authority/branding/return navigation are covered by tasks1–3; identity and explicit selection consume the accepted preceding plan. Catalog, visibility, events/files and runner isolation remain their separate approved-spec slices, not certified here. No new migration, org switcher, website or founder-role configuration is introduced.

Interface check: Task1 produces the sole GET/PATCH camelCase DTO; Task2 consumes those exact fields and fixed paths; Task3 exercises the same real router and export. Shared pure normalization retains legacy controls without exposing legacy authority. The frontend draft controller sits above the conditional shell and does not own a competing auth lifecycle. Recheck the accepted browser fixture's final helper filenames before Task3 dispatch.

Context: [org-memory: wiki/engineering/workos.md]. The older operator-mode memory does not authorize mounting legacy settings in the team app.
