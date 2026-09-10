# Team catalog combined final-fix report

Preserved from the completed plan's scratch workspace after scoped re-review.
Controller aggregate results and acceptance are in `team-catalog-acceptance.md`.

Status: **DONE_WITH_CONCERNS**

Base: `95b6d49ef144dad653feb3b72d42cceb25b00b2a`

## Outcome

Addressed the five Important final-review findings in one scoped wave:

1. Both private→organization and organization→private visibility changes now
   reject non-unprovisioned chats, persisted execution rows, and active writer
   leases. Idle transitions retain their existing revision behavior.
2. New-chat creation exposes a labelled, private-by-default audience selector;
   organization creation sends the selected visibility that participates in the
   existing version-two creation fingerprint. Creation, Share, and selected-chat
   metadata name the exact organization as `Everyone in {organization}`. The
   organization choice exposes the full-history and per-person GitHub-access
   disclosures before submission.
3. Folder create/edit no longer expose or submit owned-repository configuration.
   Projects remain optional and working directories remain mandatory.
4. A successful repository connection is immediately upserted into scope-owned
   state. A list refresh begun before the connection cannot erase that upsert, so
   inherited repository metadata is available to New chat without a poll.
5. Chat pagination now keeps an independent server-page cursor, and creation
   reconciles by ID. A high-ID creation cannot skip unloaded middle pages, and an
   idempotent retry cannot duplicate a row discovered by refresh.

Minor2 was updated in `docs/operations/public-release-checklist.md`: local reviewed
catalog evidence is separate from still-pending search, files/downloads, events,
runtime, real-provider, deployment, and public multi-tenant acceptance. The text
explicitly does not claim that this catalog is deployed to the private pilot.
Minor1 remains deferred; the client execution-state DTO was not widened.

## TDD evidence

### Frontend RED

After adding regressions and before changing frontend production code:

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm vitest run src/components/team/team-workspace-dialogs.test.tsx src/components/team/team-workspace.test.tsx src/components/team/use-team-catalog.test.tsx
```

Result: exit 1; 3 files failed, 10 tests failed and 19 passed. Failures matched
folder repository controls/proof, missing private/default and exact-organization
creation UI, generic Share/header audience labels, missing repository upsert,
rendered-row-derived pagination, and duplicate retry reconciliation.

The bounded pre-connect refresh race was added after the controller requested an
explicit check:

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm vitest run src/components/team/use-team-catalog.test.tsx -t "does not let a refresh started before repository connection erase the upsert"
```

RED result: exit 1; 1 failed, 16 skipped. The older repository list erased the
successful connection upsert.

GREEN result after the repository sequence guard: exit 0; 1 passed, 16 skipped.

### Rust RED

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::catalog::matrix_tests::making_an_organization_chat_private_rejects_execution_rows_and_active_writers -- --exact --test-threads=1
```

Working directory: `src-tauri`

Result: exit 101 after 3m42s; 1 failed, 0 passed, 3,779 filtered. The new
organization→private regression failed at the execution-row assertion, proving
the old direction-specific guard.

## GREEN and affected verification

The first three-file frontend GREEN attempt ran the same command as frontend RED.
Production regressions passed, but one old broad `/private to you/` query became
ambiguous after the new radio label: 1 failed and 28 passed. The test was corrected
to assert the exact helper sentence; no production change was made for that failure.
The rerun passed 3 files and 29 tests.

The final affected frontend command was:

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm vitest run src/lib/team-catalog-client.test.ts src/lib/team-client.test.ts src/components/team/use-team-catalog.test.tsx src/components/team/team-workspace-tree.test.tsx src/components/team/team-workspace-dialogs.test.tsx src/components/team/team-workspace.test.tsx src/components/team/team-signed-in-shell.test.tsx
```

Result: exit 0; 7 files passed, 87 tests passed.

The covering Rust GREEN invocation was the only required post-fix compilation:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 cargo test --lib --no-default-features team_server::catalog -- --test-threads=1
```

Result: exit 0 after 3m55s build; 16 passed, 0 failed, 1 ignored. The ignored test
is the existing guarded disposable-PostgreSQL race. The emitted executable was:

```text
/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/workos-src/src-tauri/target/debug/deps/codeg_lib-732148cb61475fb4
```

Adjacent checks reused that executable directly without compiling:

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 team_server::http_catalog_tests --test-threads=1
```

Result: exit 0; 9 passed, 0 failed.

```text
./target/debug/deps/codeg_lib-732148cb61475fb4 workspace_catalog::request_fingerprint --test-threads=1
```

Result: exit 0; 6 passed, 0 failed.

Rust retained only the previously documented warnings: unused `DbBackend`, the
macOS compact-unwind/`__eh_frame` size warning, and the upstream
`proc-macro-error2` future-incompatibility notice. Rustfmt remained unavailable and
was not installed.

### TypeScript, lint, formatting, locales, and export

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec tsc --noEmit
```

The first captured run exited 1 because six test calls supplied unsupported
`exact` options to `getByRole`. Anchored accessible-name matchers fixed the tests;
the final run exited 0.

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec prettier --check scripts/tests/team-catalog-e2e.cjs src/components/team/team-workspace-dialogs.test.tsx src/components/team/team-workspace-dialogs.tsx src/components/team/team-workspace.test.tsx src/components/team/team-workspace.tsx src/components/team/use-team-catalog.test.tsx src/components/team/use-team-catalog.ts docs/operations/public-release-checklist.md
```

Result: exit 0; all matched source/docs use project Prettier style. An earlier
check also included the intentionally compact locale JSON and reported those ten
files plus three TS/TSX files. The three source files were corrected through
`apply_patch`; locale files retained their established compact layout.

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec eslint scripts/tests/team-catalog-e2e.cjs src/components/team/team-workspace-dialogs.test.tsx src/components/team/team-workspace-dialogs.tsx src/components/team/team-workspace.test.tsx src/components/team/team-workspace.tsx src/components/team/use-team-catalog.test.tsx src/components/team/use-team-catalog.ts
```

Result: exit 0.

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH node --check scripts/tests/team-catalog-e2e.cjs
```

Result: exit 0.

Locale validation parsed every JSON dictionary and compared its `TeamWorkspace`
keys to English. Result: `Locale key parity PASS: 10 locales, 57 TeamWorkspace keys`.

```text
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm run build
```

Result: exit 0. Next 16.1.6 compiled successfully, TypeScript completed, and 34/34
static routes were generated, including `/workspace`.

`git diff --check` passed.

## Synthetic browser proof

The exact final command used a new explicit empty artifact directory under the
source plan and the emitted Rust executable:

```text
test ! -e /Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/workos-src/.superpowers/sdd/2026-09-10-team-catalog/artifacts/final-fix-team-catalog-e2e-1 && PLAYWRIGHT_MODULE=/Users/air1/.npm/_npx/5e2e484947874241/node_modules/playwright WORKOS_E2E_TEST_BINARY=/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/workos-src/src-tauri/target/debug/deps/codeg_lib-732148cb61475fb4 WORKOS_TEAM_E2E_ARTIFACTS=/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/workos-src/.superpowers/sdd/2026-09-10-team-catalog/artifacts/final-fix-team-catalog-e2e-1 PATH=/opt/homebrew/opt/node@22/bin:$PATH node scripts/tests/team-catalog-e2e.cjs
```

Result: exit 0 with `Team catalog browser journey PASS`.

Artifact root:

```text
/Users/air1/conductor/workspaces/new-idea-v1/rio-de-janeiro/.context/workos-src/.superpowers/sdd/2026-09-10-team-catalog/artifacts/final-fix-team-catalog-e2e-1
```

`evidence.json` records `"result": "PASS"`, synthetic-loopback-only evidence,
and an empty `browserConsoleErrors` array. The fixture-owned synthetic runtime was
removed. The journey asserts private-default creation, immediate inherited
`atlas/repo · main` availability after working-directory repository connection,
exact `Everyone in Atlas Operations` labels, colleague absence/share/revocation,
and a settled Settings return. The controller independently inspected the JSON,
driver waits, and shared/private mobile plus restored/settings desktop screenshots
and confirmed the intended settled evidence. No extra browser rerun was requested.

## Self-review

- Re-read the five Important findings, Minor2, and binding global constraints;
  each has a direct code/test or documentation disposition above.
- Confirmed the visibility busy guard applies symmetrically while creator,
  organization, revision, and server-side authority checks remain unchanged.
- Confirmed creation sends only the existing strict DTO fields; no user, role, or
  organization authority was added to requests. The selected visibility reaches
  the existing audience-versioned server fingerprint.
- Confirmed folder repository UI is absent for both create and edit; project
  defaults and required working-directory mappings remain intact.
- Confirmed repository and chat upserts are scope-owned, ID-reconciled, and stale
  scope completions still reject through the existing mutation guard.
- Confirmed the page cursor advances only from server list pages, including raw
  page rows filtered from display after a selected-detail 404.
- Confirmed private-by-default semantics, full-history/GitHub disclosures,
  exact-organization labels, keyboard-native radio controls, and narrow-screen
  metadata rendering remain present.
- Confirmed runtime remains disabled, Minor1 remains deferred, and no migrations,
  dependencies, provider logins, cloud state, deployment, push, production/test
  data, or protected root acceptance/release-plan documents were changed.
- Audited the complete diff and scoped file list. Existing untracked `.context/`
  remains unmodified and unstaged.

## Concerns and deferred work

- Browser proof is synthetic loopback evidence only. Real hosted/provider,
  migration, two-user, and public acceptance remain root-owned release gates.
- Search, files/downloads, events, execution/runtime, and knowledge publication
  remain intentionally outside this final catalog fix wave.
- Root owns the final full frontend aggregate and scoped re-review after this
  commit.

## Organization memory consulted

Read `/Users/air1/eazybe-wiki/wiki/engineering/workos.md`. It informed the boundary
to keep team catalog metadata separate from runtime/provider execution and to avoid
claiming synthetic evidence as deployed or public readiness.
[org-memory: wiki/engineering/workos.md]
