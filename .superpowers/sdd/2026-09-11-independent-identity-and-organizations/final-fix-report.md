# Neev native identity final-fix report

Date: 2026-09-11

Base: `0a557fcd7527b764a3860b42e1c10384328bf6f0`

Implementation commit: `2e73505c2e3353762ea15bb5db7d717d9bbc7cb3`

Status: implementation and scoped verification complete; independent review pending.
Runtime remains disabled. No deployment or final-review approval is claimed.

## Root causes and changes

1. Native body-read capacity
   - Root cause: every mutation handler acquired one of 32 shared native request
     permits before `axum::body::to_bytes`, while bounded body collection had no
     time bound. A matching-Origin pending Google-login body could retain a permit
     indefinitely before browser-cookie/CSRF validation.
   - Change: all native bounded JSON body reads, including logout, now use a fixed
     30-second deadline. The deadline covers body collection only, so it does not
     cancel database, OAuth, or mail work after upload. Expiry returns private
     HTTP 408 JSON (`request_timeout`), drops the pending body, and releases the
     handler permit. Origin/content-type checks and byte limits remain before/on
     body collection.
   - Regression: an owned pending stream under paused Tokio time takes the only
     test permit, makes a concurrent request receive `Limited`, then times out at
     virtual 31 seconds. The test asserts the private 408 body and proves a later
     request reaches normal cookie validation (`Unauthorized`) instead of staying
     limited.

2. Unicode invitation subject
   - Root cause: organization names permit 100 Unicode scalar values, but
     `Join {organization} on Neev` was checked by the Resend adapter against a
     200-byte subject limit. One hundred `界` characters produced a subject over
     that byte limit.
   - Change: invitations use the bounded generic subject `You're invited to Neev`.
     The complete organization name remains in the text body.
   - Regression: the invitation service sends an organization named with 100
     `界` characters through the real Resend adapter to a loopback Axum endpoint.
     The endpoint observes the generic subject, exact recipient, and complete
     organization name in the text body.

3. Mail-disabled invitation persistence and UI
   - Root cause: `invites::send` committed quota, invitation, and idempotency rows
     before checking `state.mail`; a same-ID retry then replayed the committed ID.
     Settings showed an actionable invite form even when capabilities reported no
     email provider.
   - Change: after current session/role/owner authorization and organization load,
     the service requires configured mail before any invitation, quota, or request
     bookkeeping. Configured-mail idempotency and the existing commit-before-send
     delivery boundary are unchanged; no database lock crosses email delivery.
     Settings derives availability from native capabilities, disables the invite
     fields/action, guards programmatic submission, and renders translated
     provider-unavailable copy in all ten existing locales.
   - Regressions: initial disabled-mail send and same-request-ID retry both return
     `Unavailable`, with invitation/request/quota tables remaining at zero after
     each attempt. The component regression fills an email while capability email
     is false, observes the disabled action and copy, and proves no invitation POST.

4. OAuth exchange-secret serialization
   - Root cause: `ExchangeSecrets` derived general-purpose `Serialize`, contrary to
     the secret payload binding, even though its current use encrypted the bytes.
   - Change: `ExchangeSecrets` now derives only `Deserialize`. Encryption emits the
     same JSON field names and declaration order through a field-by-field
     `SerializeMap` local to the encryption function, then uses the unchanged
     AES-256-GCM nonce, ciphertext, AAD, base64, and error paths.
   - Regressions: a compile-time ambiguity guard fails if `ExchangeSecrets` ever
     implements `serde::Serialize`; focused encryption verifies roundtrip and
     ciphertext tamper rejection. Existing end-to-end OAuth tests continue to
     cover browser/provider AAD binding and ciphertext scrubbing.

## TDD evidence

All Rust commands used the existing `src-tauri/target`,
`CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=1`, and `--no-default-features` after the
resource correction described below.

### RED

- Secret trait guard:

  ```sh
  cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    'neev::auth::oauth::encoding_tests::exchange_secret_encoding_roundtrips_and_rejects_tampering' \
    -- --exact --nocapture
  ```

  Compile failed with `E0283`: both the blanket and `Serialize`-bounded
  `AmbiguousIfSerialize` implementations applied to `ExchangeSecrets`. This is
  the intended compile-time contract failure. A separate initial test-helper
  `IntoFuture` import error was corrected and is not counted as RED evidence.

- Pending body:

  ```sh
  cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    'neev::http::tests::pending_json_body_times_out_and_releases_native_request_capacity' \
    -- --exact --nocapture
  ```

  Failed at `pending body retained its request permit` after virtual 31 seconds.
  An earlier version paused Tokio before SQLite setup and failed with
  `PoolTimedOut`; setup was moved before `tokio::time::pause`, and that setup error
  is not counted as RED evidence.

- Unicode invitation through Resend:

  ```sh
  cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    'neev::org::tests::unicode_organization_invitation_uses_bounded_subject_through_resend' \
    -- --exact --nocapture
  ```

  Failed because invitation send returned `Invalid` before the loopback endpoint
  could accept the oversized subject.

- Mail-disabled persistence/retry:

  ```sh
  cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    'neev::org::tests::mail_disabled_invitation_and_same_id_retry_leave_no_durable_bookkeeping' \
    -- --exact --nocapture
  ```

  Failed with invitation row count `1`, expected `0`, after the first unavailable
  attempt.

- UI disabled action:

  ```sh
  PATH=/opt/homebrew/opt/node@22/bin:$PATH \
    pnpm test src/components/neev/native-settings.test.tsx --run
  ```

  Failed only the new case: the Invite button was not disabled. The other eight
  settings tests passed.

### GREEN

- Each of the four focused Rust regressions passed individually: `1 passed`,
  `0 failed` each. The pending-body case completed in virtual time; the Unicode
  case traversed the real Resend adapter and loopback HTTP endpoint.
- The focused Settings suite passed `9/9`, `0 failed`.

## Full final verification

- Full native suite, including ignored PostgreSQL cases and skipping only the
  exact interactive browser driver:

  ```sh
  NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
  CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
  cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
    'neev::' -- --include-ignored \
    --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1
  ```

  Result: `125 passed; 0 failed; 0 ignored; 3,781 filtered out`; 5.49 seconds;
  exit 0. The ignored PostgreSQL tests visibly ran.

- Frontend:
  - `pnpm test src/components/neev/native-settings.test.tsx --run`:
    9 passed, 0 failed.
  - `pnpm exec tsc --noEmit`: exit 0.
  - `pnpm exec eslint src/components/neev/native-settings.tsx
src/components/neev/native-settings.test.tsx`: exit 0.
  - `pnpm exec prettier --check` on the two changed TS files and ten changed locale
    JSON files: all matched, exit 0.
  - `pnpm exec prettier --check
docs/operations/neev-native-identity-acceptance.md`: all matched, exit 0.
- Rust exact-file `rustfmt` completed; `git diff --check` passed.
- Disk after builds: 4.4 GiB free, above the 3 GiB stop threshold.

## Resource correction

The first focused Rust attempt accidentally omitted `--no-default-features`, so it
started compiling desktop/Tauri dependencies in the correct existing debug target.
It was interrupted before linking as free space approached the stop guard.

With controller confirmation of the user's prior cache-removal authorization, the
fixed cutoff `2026-09-11 18:28:10` identified only regular files generated by that
owned command under `src-tauri/target/debug/{deps,build,.fingerprint}`. The resolved
set was 4,699 files / 2,975,181,572 bytes; there were zero symlinks. Those files and
the exact zero-byte generated `src-tauri/target/debug/codeg-mcp` placeholder were
deleted. No directory, source, test, user data, older cache, or out-of-target path
was removed. Zero cutoff-matching candidates remained, and free space rose from
3.9 GiB to 6.5 GiB. All later Rust commands used the native-only baseline.

## Files

- `docs/operations/neev-native-identity-acceptance.md`
- `src-tauri/src/neev/auth/oauth.rs`
- `src-tauri/src/neev/http/auth.rs`
- `src-tauri/src/neev/http/mod.rs`
- `src-tauri/src/neev/http/tests.rs`
- `src-tauri/src/neev/org/invites.rs`
- `src-tauri/src/neev/org/tests.rs`
- `src/components/neev/native-settings.tsx`
- `src/components/neev/native-settings.test.tsx`
- `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`
- `.superpowers/sdd/2026-09-11-independent-identity-and-organizations/final-fix-report.md`

## Limitations and retained boundaries

- No browser, live DNS, real mail/provider, remote, push, deployment, fixture
  daemon, production binary, static export, or full frontend-suite operation was
  performed. Historical results remain explicitly labelled in the acceptance doc.
- Existing baseline warnings remained: one unrelated Rust unused import, macOS
  linker compact-unwind warning, Rust future-incompatibility notice, and Vite CJS
  deprecation notice. No new test failure or product breakage was observed.
- `.context/` remained untracked and untouched. No legacy or GitHub operation was
  performed. Runtime remains disabled.
- Shared engineering context confirms WorkOS publication belongs to the Eazybe fork
  and is gated; no publication was requested or attempted.
  [org-memory: wiki/engineering/workos.md]
