# Task 8 execution report: native content acceptance and release gates

Date: September 12, 2026

Branch: `workos/team-workspace`

Accepted base: `3db0b157` (`fix(neev): isolate verifier connections and require native tables`)

## Outcome and scope

Task 8 adds a bounded, real-HTTP native content acceptance workflow, a strict
GET-only fixture preflight, and release documentation that keeps actual-browser,
isolated-restore, hosted, provider, website, personal-agent, notification, wiki,
durable-job and upstream-automation gates distinct. Production runtime remains
`false`; no production seed, route, provider fixture flag, credential, mail/DNS,
GitHub, deployment or external application state was added or changed.

The controller's first browser run exposed three defects inside the already
approved privacy/UX acceptance scope. A recorded scope ruling permitted only a
narrow UI amendment: foreground content-authority `403` now forces session
revalidation and removes the stale organization workspace after membership loss;
revision comparison returns focus only to a valid same-scope opener; and document
`409` uses content-specific localized copy. A fourth accepted-spec gap adds a
visible, accessible localized warning that making content private cannot recall
copies people already downloaded. All four changes were regression-first and
passed a second actual-browser run.

The two native listeners now clone one fixture-owned `BlobStore` backed by one
owned temporary root. The regression writes a document revision through listener
1 and reads its body through listener 2, so a metadata-only or separate-root
fixture cannot pass accidentally. The `TempDir` lifetime extends until both
listeners have stopped or been aborted and awaited. The exact owned schema is
dropped only after pools close, and the exact owned root is asserted absent after
shutdown.

The real-HTTP workflow proves two synthetic participant logins, explicit native
organization invitation/join, project-bound chat/document creation, private
denial, organization sharing, two human author IDs in response and history,
cross-listener blob visibility, stale document save `409`, append-only restore
revision `3` with parent `2` and `restoredFrom` `1`, source-chat privacy denial
`404`, resharing, and post-membership-removal denial `403`. The pre-existing
real-HTTP tenant test remains the separate cross-organization denial control in
the same fresh full-native run.

The content preflight accepts only literal loopback HTTP origins with explicit
ports and exact native fixture-session syntax. It refuses credentials, paths,
queries, fragments, redirects and non-loopback/`localhost` hosts. It performs
only GETs, uses five-second per-request and 15-second overall abort signals,
checks `identityMode:"native"`, `runtimeEnabled:false`, chat/document capability,
and bounded first-page DTOs, and emits only mode flags plus aggregate counts. The
Rust workflow supplies its synthetic second-participant session only to a cleared
child environment and verifies the exact sanitized JSON result; it never reads a
browser cookie store or prints/persists a token.

Historical context used to distinguish the old operator/GitHub model from native
SaaS authority: [org-memory: wiki/engineering/workos.md].

## TDD evidence

The first Node invocation failed because the module did not yet exist. After the
test/API scaffold existed, the behavioral RED was:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH \
  node --test scripts/neev-content-preflight.test.mjs
```

Result: `0` passed, `3` failed because `preflight()` still reported the explicit
unimplemented behavior. No network fixture assertion was bypassed.

The backend behavioral RED used the prescribed isolated PostgreSQL URL and flags:

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
  neev::browser_fixture::native_browser_http_content_collaboration_conflict_restore_and_revocation \
  -- --exact --ignored --nocapture --test-threads=1
```

Cargo emitted
`src-tauri/target/debug/deps/codeg_lib-9b806c9f34534627`. The new real-HTTP
control failed at the cross-listener document GET: listener 1 had written the
revision, listener 2 returned `404` instead of `200` because the listeners shared
PostgreSQL metadata but opened different blob roots. Result: `0` passed, `1`
failed, `3975` filtered out, exit `101`; the fixture still completed exact
shutdown.

After sharing one owned store, the focused regression passed. Adding the real
HTTP Node child first exposed an exact session-validator mismatch: production
native sessions are `n1.session.<43 base64url characters>`, not a dot-free token.
Inspection of the existing native secret constructor identified that boundary;
the validator was corrected without changing production auth. An attempted
scoped-to-scoped organization switch later returned `401`; existing auth tests
confirmed that only identity-to-scoped rotation is allowed, so the out-of-scope
switch was removed rather than weakening production behavior. The final focused
run passed `1/1`, with `3975` filtered out, in `0.62s`, including the actual HTTP
preflight.

The controller browser REDs were converted into frontend regressions before any
UI implementation. This focused command initially produced `16` passing and `4`
failing tests:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH \
  pnpm exec vitest run \
  src/components/neev/native-workspace.test.tsx \
  src/components/neev/native-document.test.tsx \
  --maxWorkers=2 --minWorkers=1
```

The exact failures were: a foreground content `403` made only one bootstrap call
instead of revalidating and retained the old organization tree; two document
conflict cases rendered the identity/account-linking message; and Escape returned
focus to `BODY`, not the revision trigger. After correcting only a test fixture's
declared byte length, the isolated focus test still failed `0/1` with the exact
trigger-versus-`BODY` mismatch. The separately focused Make-private regression
failed `0/1` because the action had no accessible description explaining that
downloaded copies cannot be recalled.

The first broad native-component run usefully rejected a global `403` refresh:
`102/105` passed, while ordinary onboarding/settings permission denials lost their
alerts and a chat `403` mock with a forever-valid session looped. The fix was
narrowed to a dedicated content-authority handler used only by catalog/chat/
document hard-invalidating paths. Ordinary permission `403`s still render in
place. Chat/document fixtures now model the observed content `403` followed by
session `401`. The same-scope focus guard requires an authenticated, connected,
enabled, non-`inert` opener; an authority failure clears it before closing the
dialog. The final native component group passed `105/105` across `12` files; the
four directly amended files passed `67/67`.

## Fresh verification on the handed-off source

Node preflights and launcher:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH \
  node --test scripts/neev-content-preflight.test.mjs \
  scripts/neev-identity-preflight.test.mjs \
  scripts/neev-native-launcher.test.mjs
```

Result: `8/8` passed, `0` failed, `173.251ms`.

TypeScript and Task 8 lint/format checks:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec tsc --noEmit
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec prettier --check <Task8 files>
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec eslint \
  <Task8 JavaScript and TypeScript files>
```

Result: TypeScript passed; all scoped source/test/script/docs/locales passed
Prettier; all Task 8 JavaScript and TypeScript files passed ESLint with zero
warnings.

Full frontend, with no concurrent Rust compiler:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH \
  /opt/homebrew/opt/node@22/bin/pnpm exec vitest run \
  --maxWorkers=2 --minWorkers=1
```

Result on the final amended source: `465` test files passed, `6361` tests passed,
duration `266.44s`.
Preserved baseline output included Vite's CJS deprecation, React `act(...)`, HTML
nested-`div` hydration, jsdom canvas-not-implemented, KaTeX and expected mocked
error warnings.

Full repository ESLint was also attempted:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm exec eslint .
```

Result: failed with the existing repository baseline, `470` problems (`467`
errors, `3` warnings), dominated by historical `.superpowers` scripts and also an
unrelated existing Prettier finding in `src/lib/sidebar-view-mode-storage.ts`.
No unrelated lint debt was edited or hidden; the scoped Task 8 ESLint command
above passes.

Static export:

```sh
PATH=/opt/homebrew/opt/node@22/bin:$PATH pnpm build
```

Result on amended source: compilation and TypeScript passed, and `34/34` static
pages were generated. The output contains 35 HTML files because the error page is
included; this is not a new route.

Full native content gate, with the exact interactive driver excluded:

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
  'neev::' -- --include-ignored \
  --skip neev::browser_fixture::browser_acceptance_driver \
  --test-threads=1
```

Result: emitted the same exact test executable; `195` passed, `0` failed, `0`
ignored, `3781` filtered out, `10.68s`.

Named production builds, run serially:

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
cargo build --manifest-path src-tauri/Cargo.toml \
  --no-default-features --bin neev-server

PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 \
cargo build --manifest-path src-tauri/Cargo.toml \
  --no-default-features --bin neev-content-verify
```

Result: both exited `0`; server build `3m10s`, verifier build `1.74s`. Preserved
native baseline output included `73` warnings, an unused `DbBackend`, dead
`nodes::get`, a linker `__eh_frame` size warning in tests, and the
`proc-macro-error2` future-incompatibility notice. No baseline warning was erased.

Rust formatting used only the explicit task file and never `cargo fmt`:

```sh
rustfmt --edition 2021 --config skip_children=true \
  src-tauri/src/neev/browser_fixture.rs
git diff --check
```

Result: passed. Free disk stayed approximately `22GiB`, above the mandated
`3GiB` stop threshold.

## Ownership, cleanup and secret review

Automated fixture shutdown completed after every RED and GREEN run, and the new
regression asserted its exact temporary root absent. An initial controller-owned
interactive startup was stopped normally before browser/data work when the
timeout-helper ordering needed correction: PTY `37554`, schema
`neev_test_be397c0c35b34d3588f900bd004773ba`, root basename `.tmpA4HbN3`; driver
`1/1` passed in `11.37s` and reported exact schema/root cleanup.

The first frozen browser/backup handoff used emitted executable
`src-tauri/target/debug/deps/codeg_lib-9b806c9f34534627`, PTY `7004`, schema
`neev_test_b77f7cdd2e004a7ebf30a0d603cf42be`, canonical root basename
`.tmp8lvhd2`, app ports `58045`/`58046`, and issuer port `58047`. It was started by
the controller directly from the emitted test executable; no competing Cargo
invocation or implementation-agent browser control was involved. It stopped
normally with `1/1` passed in `999.26s`; the controller verified SQL count zero
for that exact schema and absence of that exact root.

A read-only exact-name inventory during that live fixture showed the owned active
schema plus `neev_test_0faf040a2bab4d28ba9be274b9967a2d` and
`neev_test_bdd70e258e494682899113ddb1413921`. The latter two had 14 tables each;
they were not created, claimed or deleted by Task 8 because ownership was
unknown. The active fixture had 19 tables and three idle database connections. A
final read-only inventory after both owned fixtures stopped showed only those two
unknown schemas; Task 8 did not alter them.

The amended browser retest used the same frozen emitted binary with PTY `64331`,
owned schema `neev_test_29b3d501e1094d5e8a07e97111bdafa6`, root basename
`.tmpcg8hD8`, app ports `61060`/`61061` and inbox port `61062`. It stopped
normally with `1/1` passed in `420.76s`; the controller again verified SQL count
zero and exact-root absence. A task-owned browser tab temporarily used for the
fixture was restored to its prior Resend API-key page; the B/inbox task tabs were
closed and viewport override reset. No credentials or real provider state were
used or changed.

A scoped scan of every Task 8 source/script/doc file found no Google/API/GitHub
token, private-key, encryption-key, password or exact native-session literal.
The preflight output contains no token, CSRF, user/org/resource ID or content
body. Untracked `.context/` was preserved.

## Controller-owned browser, restore and production-smoke evidence

The first Chrome run passed fresh org/user identity, recursive
Marketing/Campaigns/Launch hierarchy, Settings return, private chat denial,
explicit full-history sharing, two human author/time rows, loaded-chat clearing
after privacy restriction, `Meta+S` document save, cross-listener body read,
conflicting-draft preservation/comparison, and restore `1` to new revision `3`
while retaining revisions `1` and `2`. Raw HTML image/iframe syntax produced zero
app images/iframes, and the separate loopback probe counters remained `1/1`.

That run exposed the three UI REDs described above. The amended retest passed all
four scoped corrections: exact content-specific conflict text with the draft
preserved; Escape focus on `Compare revision 1`; visible downloaded-copy warning;
and complete sign-in reset about three seconds after member removal with no old
organization header/tree, New-project control, content, draft or dialog. At an
actual `375×812` viewport, app `scrollWidth` was `375`, the comparison dialog was
343 pixels wide without overflow, and mobile drawer/project/settings/invitation/
document/chat/send and `Meta+S` paths worked. Bounded console errors were browser
extension feature-flag/message-port stacks; no application stack was observed.

One minor follow-up remains: normal polling can clear the visible document `409`
message while the draft and **Compare with latest** control persist. The exact new
copy was observed immediately after a repeated conflicting save. The optional
source-bound-document UI flow, unchanged-authority dirty hide/show flow and
separate other-organization browser denial were not executed. Source privacy and
cross-organization denial pass real HTTP/native tests, but are not called browser
evidence.

The controller backed up three browser-created blobs. `metadata.dump` was 79,039
bytes with SHA-256
`e0cdf453012ef5cc982803809d7ce22a07477144682d34edbe2fae04dd056433`. The
fresh isolated database `neev_restore_9fcd806b118f4ba18718d1ac5039b3d4`
passed the native verifier with schema version `3`, `3/3` referenced blobs and
`608` referenced bytes; manifest SHA-256 was
`7d1f4aecf9c55451b1087dced6cd44d1cb8236fcc59676e92cb251eac139c235`. A
separate copied historical blob moved to quarantine made the verifier exit `1`.
The source and positive copy were untouched; the retained recovery artifact is
`/private/tmp/neev-content-recovery.R5JKHR`. An initial negative harness omitted a
`.md` suffix and failed `ENOENT` before mutation; inspecting exact paths corrected
the harness, so this was not a product defect.

The actual built non-test `neev-server` also passed a controller smoke against
fresh owned database `neev_smoke_1502463e257c4788b826f43bb548991d`: `/healthz`
returned `200`, no-store and `status:ok`; `/workspace` returned `200`; capabilities
were native, runtime false, chats/documents true; and `/__synthetic/google`
returned `404`. The clean-environment child was terminated and awaited and only
its exact owned database was dropped. This proves neither real provider behavior
nor hosted readiness.

Even after those local gates, the public release checklist intentionally leaves
runtime disabled and real Google/mail/HTTPS/domain, publication, website/icons,
hosted backup/capacity, personal subscription/agent controls, agent attribution,
notifications, privacy-safe wiki synthesis, durable jobs, receiving inboxes and
founder feedback open. Existing upstream workflow/docs prove read-only discovery
only; automatic adaptation, proposal, merge and deployment remain separate
maintenance work, and no live GitHub state was checked.

## Review fix round 1: bounded preflight child ownership

Independent review found that the real-HTTP workflow ran Node through
`spawn_blocking(Command::output)`. The installed Tokio 1.49 source confirms that
a started blocking task cannot be aborted and runtime shutdown may wait for it
indefinitely. Consequently, the workflow's 120-second action timeout could abort
only the surrounding async task while a stalled Node process and blocking worker
continued across fixture cleanup.

The behavioral RED added a real stalled child that wrote a marker if it survived
until after the fixture-owned document root disappeared. It used the exact native
test contract:

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
  neev::browser_fixture::stalled_preflight_child_is_reaped_before_exact_fixture_cleanup \
  -- --exact --ignored --nocapture --test-threads=1
```

RED result: emitted
`src-tauri/target/debug/deps/codeg_lib-9b806c9f34534627`; `0/1` passed, `3977`
filtered, `5.19s`, exit `101`. Before the assertion, the owned root was absent
and a read-only `pg_namespace` query returned zero for the exact generated schema;
the child then wrote `child outlived fixture cleanup`.

The fix removes `spawn_blocking` and `Command::output`. A test-only owner retains
the actual `std::process::Child`, writes stdout/stderr into a separate owned
temporary directory, caps each captured stream at 16 KiB, and polls `try_wait`
without blocking. The real preflight has a 20-second parent deadline around its
own 15-second JavaScript deadline. Deadline expiry performs explicit kill and
wait before returning. If the outer 120-second action task instead aborts this
future, the owner's `Drop` performs synchronous kill and wait before task
cancellation completes; fixture cleanup therefore cannot overtake the child.
The output directory is removed on every return/drop and contains no session
value because the preflight emits only sanitized counts.

The final regression exercises both paths with real Node children: one reaches
the two-second child deadline; a second has a 30-second child deadline but its
owning async task is aborted and awaited, mirroring the outer workflow timeout.
Only after both ownership paths return does the test stop listeners, drop the
exact UUID schema, and remove the exact root. It then waits through the children's
five-second would-have-written boundary and finds neither escape marker. Final
result: `1/1` passed, `3976` filtered, `7.20s`, exit `0` (fresh serial rebuild
`5m48s`).

The unchanged real-HTTP content workflow was rerun on that final binary with the
same environment/flags and its exact test name:

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
  neev::browser_fixture::native_browser_http_content_collaboration_conflict_restore_and_revocation \
  -- --exact --ignored --nocapture --test-threads=1
```

Result: `1/1` passed, `3976` filtered, `0.67s`, exit `0`; the actual Node
preflight DTO/count check, cross-listener document path and exact schema/root
cleanup all completed. Both final commands retained the three existing Rust
warnings (unused migration import, dead `nodes::get`, linker `__eh_frame`) plus
the existing future-incompatibility notice. No frontend source changed, so the
controller explicitly did not request another frontend/export/browser run.

Self-review checked the outer-cancellation ordering, normal completion, deadline
expiry, output-limit/error and helper-future-drop paths. The deliberate
test-fixture tradeoff is a short synchronous `kill`/`wait` during cancellation;
for this same-user owned child, that prevents a zombie or resource-cleanup race.
The normal async polling path does not block. Production server/runtime/provider
code and flags remain unchanged. [org-memory: wiki/engineering/workos.md]

## Exact Task 8 files

- `src-tauri/src/neev/browser_fixture.rs`
- `scripts/neev-content-preflight.mjs`
- `scripts/neev-content-preflight.test.mjs`
- `src/components/neev/native-auth-provider.tsx`
- `src/components/neev/native-chat.tsx`
- `src/components/neev/native-chat.test.tsx`
- `src/components/neev/native-content.tsx`
- `src/components/neev/native-content.test.tsx`
- `src/components/neev/native-document.tsx`
- `src/components/neev/native-document.test.tsx`
- `src/components/neev/native-workspace.test.tsx`
- `src/i18n/messages/{ar,de,en,es,fr,ja,ko,pt,zh-CN,zh-TW}.json`
- `docs/operations/neev-native-content-acceptance.md`
- `docs/operations/neev-native-identity-acceptance.md`
- `docs/operations/public-release-checklist.md`
- `.superpowers/sdd/2026-09-11-native-chats-and-documents/task-8-report.md`

No other file is part of the Task 8 commit.
