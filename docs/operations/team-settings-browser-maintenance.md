# Team settings browser maintenance

This runbook records two retained maintenance costs from the private,
runtime-disabled team organization-settings candidate. It does not certify a
deployment, hosted provider consent, or any route outside the reviewed settings
slice.

## Generated login-chunk cancellation

The actual-browser driver in `scripts/tests/team-auth-e2e.cjs` accepts one named
login-script cancellation through an exact predicate. For the current static
export, that predicate identifies
`/_next/static/chunks/44818f6346fe33c5.js` and also requires all of the recorded
request and navigation metadata: a `GET` script request, not a navigation
request, the Alice B relogin phase, exact `/login` page-at-request-start,
frame-at-request-start, and page-at-failure URLs, an `ERR_ABORTED` failure, and
the marker proving that GitHub OAuth navigation had started. The subsequent B
workspace and settings render must still succeed.

This is intentionally coupled to the generated candidate export. Do not replace
the chunk name or any accompanying condition with a wildcard, a general static
asset exception, or a broad cancellation allowance. A changed hash or changed
metadata is a new observation that requires review; it is not evidence that the
old predicate should be loosened.

After rebuilding the static export, revalidate in this order:

1. Reproduce the named cancellation with the rebuilt export and preserve the
   candidate-specific diagnostics. Do not classify an unrelated failure as the
   known cancellation.
2. Inspect the specific chunk actually served by that export and confirm its
   content and role in the login page. Inspect the request method/resource type,
   navigation flag, driver phase, request/frame/failure URLs, OAuth-navigation
   marker, and failure text.
3. If the exact chunk or metadata changed, review any proposed new exact
   predicate against those observations. Keep every discriminating condition;
   do not introduce a wildcard or general exception.
4. Run a fresh actual-browser acceptance against that exact export and matching
   Rust test binary. Require the existing diagnostic/canary/process-cleanup
   controls, then inspect the fresh desktop and mobile screenshots rather than
   relying only on their creation.

The coupling is a retained minor maintenance cost, not fixed runtime behavior.

## Baseline build and test warnings

The accepted settings work has repeatedly observed these existing warnings:

- Rust reports an unused `DbBackend` import in the unrelated migration
  `m20260817_000001_work_task_conversation_title.rs`.
- The macOS linker reports that the `__eh_frame` section is too large for compact
  unwind offsets.
- Cargo reports a future-incompatibility notice for
  `proc-macro-error2 v2.0.1`.
- No-default-features server checks report existing disabled-runtime dead-code
  warnings.
- Vitest reports the existing Vite CommonJS Node API deprecation notice.
- The active stable Rust toolchain does not have the `rustfmt` component
  installed, so `cargo fmt --check` cannot run in this environment.

Treat those messages as baseline only when they match the known source and form
above. Capture and report any new warning separately, especially one originating
in a changed settings file; do not relabel it as baseline. The settings files
must still be manually checked against surrounding Rust formatting and compiled
by the matching test harness while `rustfmt` is unavailable.

Dependency upgrades, tool installation, warning suppression, and whole-module
formatting are outside this maintenance wave. Cleanup of the underlying baseline
warnings remains deferred and is not claimed complete here.
