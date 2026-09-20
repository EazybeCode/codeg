# Neev organization join code — implementation plan (grounded)

Spec: `docs/superpowers/specs/2026-09-20-neev-workspace-ux-design.md` §12.
Admission decision (resolved): authenticated users enter a code → preview/confirm
org → **join immediately as a regular `member`**. Admins control code
generate/rotate/disable. No admin-role grants via code, no bypass of
suspension/removal/admission, idempotent membership, explicit org switch protects
unsaved drafts.

## Reuse (audited in tree)
- Membership insert/reactivate + gateway revocation event: `org/invites.rs::accept`
  (idempotent; `INSERT ... neev_memberships(... role,revision=1,active=true)` for
  new, `UPDATE ... active=true,revision=revision+1` for reactivation). Join-by-code
  reuses this exact logic with `role='member'` forced.
- Preview/confirm shape: `org/invites.rs::{preview,accept(token,confirmed_org_id)}`.
- Admin authz: `session::recheck(&tx, actor, now)` + `actor.role != Role::Admin →
  Forbidden`; owner-only checks via `lock_organization_details`.
- Secrets: `auth::secrets::{random_token,digest}` (prefix + base64url(32) → hex
  sha256). New `TokenPurpose::JoinCode` (prefix `n1.join.`).
- Encrypt-at-rest for admin re-display: mirror `auth/oauth.rs::{encrypt,decrypt}`
  (Aes256Gcm + random nonce, `config.encryption_key`, AAD = "org.join_code" + org id).
- Rate limit: `mail/quota.rs::reserve_in_transaction` + new `Bucket` variants
  `JoinCodeSourceIp(IpAddr)` and `JoinCodeActor(UserId)` (attempt buckets, not mail).
- Idempotent admin writes: `neev_requests` (action `org.join_code.rotate`) like
  `invites.rs::send`.

## Schema — NEW migration module `db/org_code_migration.rs`
Do NOT edit the v2 baseline `migration.rs`. Add a module mirroring
`gateway_migration.rs`: `TABLE_NAMES`, `SCHEMA_VERSION = gateway::SCHEMA_VERSION+1`,
`upgrade(tx)` (idempotent CREATE TABLE IF NOT EXISTS + index). Wire into
`db/mod.rs::initialize` after `gateway_migration::upgrade` in BOTH the fresh-install
branch and the incremental-upgrade match arms; update the terminal version insert
and the version/table-set expectations (follow how gateway was chained).

Table `neev_org_join_codes` (one active code per org):
- `org_id i` PK, FK→neev_organizations.id
- `code_hash t` (sha256 hex, unique index) — O(1) lookup on join
- `code_enc t` (aead ciphertext for admin re-display)
- `disabled b`
- `created_by i` FK→neev_accounts.id
- `created_at l`
- `revision l` (>=1; ++ on rotate)
Checks: `length(code_hash)=64`, `revision>=1`. Unique: `code_hash`.

## Service — NEW `org/join_code.rs`
- `ensure/rotate(state, actor: &OrgContext, request_id) -> PlainCode` — admin only;
  generate unguessable code (base32 Crockford, 12 chars, no ambiguous chars — human
  shareable, unguessable ≥ 60 bits), store hash+enc, revision++ on rotate; idempotent
  via neev_requests.
- `view(state, actor) -> {code, disabled, created_at, revision}` — admin only;
  decrypt for display. (If none exists yet → None; UI offers Generate.)
- `set_enabled(state, actor, enabled)` — admin only; flip `disabled`.
- `preview(state, identity, code) -> {org_id, org_name}` — any authenticated user;
  rate-limited by IP+actor; reject unknown/disabled; do NOT reveal which failed
  (uniform Forbidden). Never log the code.
- `join(state, identity, code, confirmed_org_id) -> OrgId` — resolve by hash, require
  enabled + confirmed_org_id matches, then reuse invite membership insert/reactivate
  as `member` (idempotent; already-active member → Ok no-op). Rate-limited.

Denials to test: cross-org confirmed_org_id mismatch; invalid code; rotated code
(old hash no longer resolves); disabled code; existing active member (idempotent Ok);
non-admin generate/rotate/disable/view → Forbidden; suspended/removed member cannot
re-enter if org rules forbid; rate-limit trips after N attempts.

## HTTP — `http/` routes (org-scoped for admin ops; identity-scoped for join)
- `GET /api/neev/org/join-code` (admin view), `POST .../rotate`, `POST .../disable`
  + `.../enable`. `POST /api/neev/join/preview {code}` and
  `POST /api/neev/join {code, confirmedOrgId}` (identity session; CSRF-guarded;
  code only in JSON body — never query/URL/logs).

## Frontend
- Settings (admin): show current code (masked with reveal), Copy, Rotate, Disable/
  Enable, reusing the existing invite/member UI section. `neev-org-client.ts` calls.
- Join: authenticated "Join organization by code" → enter code → preview org name →
  confirm → joins as member; clear invalid/expired/disabled/already-member states;
  explicit org switch runs the unsaved-draft guard. i18n across all 10 locales.

## Tests
Rust unit/integration in `org/tests.rs` (cross-org denial, invalid/rotated/disabled,
existing member idempotent, non-admin denial, rate-limit, owner/admin authz).
Frontend vitest for the settings controls + join preview/confirm + error states +
locale parity.

## EXACT migration wiring (reverse-engineered from db/mod.rs::initialize)
The live prod DB is at `gateway_migration::SCHEMA_VERSION = 8`. Current design:
each intermediate arm (v2..v7) advances to the status7 state then FALLS THROUGH to
a shared tail that runs `gateway_migration::upgrade` + sets version 8; the v8 arm is
TERMINAL (`validate…; return Ok`). To add v9 without breaking the v8 prod DB:
1. `org_code_migration.rs`: `SCHEMA_VERSION = 9`, `TABLE_NAMES=["neev_org_join_codes"]`,
   `upgrade(tx)` (CREATE TABLE + unique index on code_hash), `validate(tx)` (bounded
   scalar/type scan like the others; simpler — no authority store).
2. `mod org_code_migration;` in db/mod.rs.
3. Add `v9_expected = v8_expected ∪ org_code TABLE_NAMES`; add `&& names != v9_expected`
   to the inventory guard.
4. Fresh-install branch: after `gateway_migration::upgrade`, add
   `org_code_migration::upgrade(&tx)`; change the marker insert to
   `org_code_migration::SCHEMA_VERSION`.
5. Move `gateway_migration::upgrade` OUT of the shared tail and INTO the END of each
   v2..v7 arm (so after any intermediate arm, gateway tables exist). The v8 arm keeps
   its validates but DROPS `return Ok` (falls through). New shared tail becomes
   `org_code_migration::upgrade(&tx)` + `UPDATE neev_schema_version SET version =
   org_code_migration::SCHEMA_VERSION`. Add a TERMINAL v9 arm:
   `org_code_migration::SCHEMA_VERSION if names == v9_expected => { …validate all incl
   org_code; return Ok }`.
6. Update `validate_content_snapshot` chain if it must recognize v9 (check its callers).
GUARDRAIL: the db migration tests (search `initialize` / version-upgrade tests in
`db/*_tests.rs`) MUST cover fresh→v9 AND v8→v9. Run
`cargo test --no-default-features --bin codeg-server --lib` green before shipping; a
mistake here breaks prod startup on next deploy.

## Status
NOT STARTED beyond this plan. Backend compiles locally
(`cargo check --no-default-features --bin codeg-server` = clean baseline). Implement
in verified slices: (1) migration module + `cargo test --lib` migration test; (2)
service + Rust tests; (3) HTTP routes; (4) frontend + vitest; (5) canary + browser
accept. Do not mark done until tested + browser-verified.
