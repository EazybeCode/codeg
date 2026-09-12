# Combined final content-review fix wave

Base: `5c2ef7880a44adbdb1e1ae234830a98ae9e4f12e`. Status: DONE.
No subagents, browser operations, pushes, publication, provider credentials, or
legacy behavior changes. Existing `.context/` remains untracked and untouched.

## Implementation

- Resources and messages recheck session authority against a fresh clock reading
  immediately before each successful transaction commit, including resource
  create/get/list/update, message append/list, and both committed receipt replays.
  Existing admission/session/resource lock order is unchanged.
- Document reads retain the blob result through the final authority transaction.
  Authorized metadata with missing/corrupt physical storage becomes `Storage`;
  inaccessible or missing resources/revisions remain not-found. Content, download,
  and restore share this classification without exposing unauthorized storage state.
- Document polling revalidates resource authority every tick, skips unchanged
  body/history work, and fetches newly appended revisions after the previously
  observed head. Explicit pagination merges another page without refetching the
  loaded window. The cache lives only inside the component keyed by exact auth
  scope/resource. A new request lifetime explicitly requests full body refresh;
  existing outer suspension and authority gates remain intact. The controller
  clarified that the approved pre-reveal proof is exact user/scope/org/role/CSRF
  continuity, refreshed selected-resource/source authority, and loaded catalog
  validation. Immutable cached body/comparison bytes may survive that gate;
  a new body/comparison wait gate is explicitly out of scope. Failed metadata or
  authority validation still keeps the retained surface hidden or hard-clears it.
  Dirty drafts and
  ambiguous retries keep their original content/auth preconditions.
- Unresolved conflicts are tracked separately from transient errors. Polls preserve
  the explanation; unavailable foreground Compare keeps the draft and Save, Copy,
  Reload, and Compare controls. Successful save/restore or deliberate adoption of a
  comparison basis clears the conflict.
- Schema4 forward hardening adds a validated named PostgreSQL CHECK or SQLite
  INSERT/source-field UPDATE guards. The predicate is explicitly NULL-safe and
  also corrects new v3 DDL. Existing invalid pairs refuse the transaction before
  advancing the marker. Fresh/exact2 apply3 then4, exact3 upgrades4, exact4 reruns
  idempotently, and partial/unknown/legacy inventories remain refused.
- Read-only snapshot verification accepts exact historical3 and current4 and
  reports the actual marker. No backup initialization/migration occurs. The
  content spec, plan predicate/lifecycle, and backup runbook are updated; prior
  schema3 acceptance evidence remains historical evidence.
- Task8 report now accurately says 16 KiB bounds post-exit reads, not tempfile
  disk growth. Cancellation/kill/reaping code and its accepted tests are untouched.

## TDD evidence

Common Rust environment:

```sh
export PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1'
```

### 1. Post-admission expiry

RED: `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features postgres_resource_and_message_post_admission_expiry -- --include-ignored --test-threads=1`.
Observed `FAILED`, `left: Ok(())`, `right: Err(Auth(Unauthorized))`, operation
`create`. The real PostgreSQL receipt INSERT waited after admission until the
test clock reached session expiry, then incorrectly committed. The regression
covers eight operation variants, checking no successful result, no changed
resource title/head/count, and no additional resource/message quota charges.

GREEN: final native gate199/199 passed, including all eight real PostgreSQL
post-admission wait variants.

### 2. Missing physical document storage

RED: freshly compiled test binary
`src-tauri/target/debug/deps/codeg_lib-9b806c9f34534627 content_routes_publish_strict_safe_wire_dtos --test-threads=1`.
Observed `Expected status code ... 503 ... received 404 ... content?revision=2`
with `{"code":"not_found"}`. The fixture uses a syntactically valid UUID key
in authorized revision metadata whose physical file does not exist. The test
also exercises download/restore503 and inaccessible/missing revision404.
The equivalent Cargo command was also queued against the pre-fix read path.

Frontend RED included missing `Compare with latest` after foreground storage503;
the dirty text, Copy, Save, and Reload remained but the Compare control vanished.
GREEN: focused frontend51/51; final native gate199/199 passed, including
content/download/restore storage classification and authority tests.

### 3. Incremental polling and conflict retention

RED: `pnpm test src/components/neev/native-document.test.tsx --maxWorkers=2 --minWorkers=2`.
Observed `4 failed | 10 passed`: both409 tests lost their alert on the next poll,
the unchanged-head request-count regression failed, and unavailable Compare lost
its recovery control. The amended history fixture follows the server's ascending
cursor order instead of entrenching complete history refetches.

GREEN: `pnpm exec vitest run src/components/neev/native-document.test.tsx src/components/neev/native-content.test.tsx --maxWorkers=2 --minWorkers=2`:
`Test Files 2 passed (2)`, `Tests 51 passed (51)`.
Coverage includes unchanged-head request counts, cursor fetch of a new revision,
retained older pages,403/404 erasure/poll stop, dirty preconditions, delayed poll
versus successful save, previous-resource responses, and existing suspension and
identical-session ambiguous retry scenarios. TS/scoped ESLint passed at this point.
During final self-review, the adapted delayed-body fixture was corrected to
return the requested revision2 body and a realistic409 for a later save based on1.
It now asserts that edits made during that body read retain their original1/1
preconditions. This test-only correction ran after that file's full-suite run:
`pnpm exec vitest run src/components/neev/native-document.test.tsx --maxWorkers=2 --minWorkers=2`
passed14/14 at09:54:15, duration8.07s. No production code changed for that correction;
the full frontend suite passed against the same production code.

### 4. Versioned NULL-safe origin hardening

RED: `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev::db::content_tests -- --include-ignored --test-threads=1`:
`6 passed; 5 failed`. Fresh, exact2, and populated historical3 initialization stayed
at3 (`left: 3`, `right: 4`) on SQLite/PostgreSQL. Historical3 tests reconstruct
the previously committed weak CHECK rather than relying on corrected fresh DDL.

Additional RED: compiled binary `snapshot_schema_is_exact_and_read_only
--test-threads=1` refused schema4 with `Auth(Unavailable)`.

GREEN: final native gate199/199 passed. Regressions cover valid populated3 preservation,
invalid3 refusal/no marker advancement, repeat4 initialization, both half-NULL
shapes, nonexistent/wrong-kind/cross-org sources on direct INSERT and UPDATE,
valid linked documents, and3/4 verification without marker changes.

## Full verification

- Full native: `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features neev:: -- --include-ignored --skip neev::browser_fixture::browser_acceptance_driver --test-threads=1`
  passed199/199,0failed,0ignored,3781filtered, runtime31.29s. The completed
  process handle54661 was unavailable after the session resumed; its terminal
  log conclusively reports this passing result. No completed gate was repeated.
- Full frontend: `pnpm exec vitest run --maxWorkers=2 --minWorkers=2` passed
  `465` files / `6362` tests, duration `652.42s` (started09:44:05).
  The corrected document fixture passed14/14 after its file had run within this
  suite, before the remaining files completed, as detailed above.
- TypeScript: `pnpm exec tsc --noEmit`, exit0.
- Scoped ESLint on the three touched frontend files, exit0.
- Scoped Prettier check: all matched files use Prettier style.
- Explicit `rustfmt --check --edition 2021 --config skip_children=true` on the
  eleven touched Rust files, exit0. No whole-tree formatter was used.
- `git diff --check`, exit0.
- Frontend export: `pnpm build`, exit0; production compilation17.6s,
  TypeScript passed and34/34 static pages generated. Source/export are frozen
  for the controller's separate actual-browser acceptance.
- Named native builds: `cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --bin neev-server --bin neev-content-verify`, exit0,
  duration3m02s, existing73 library warnings. Builds ran serially after the native
  gate and export; free disk remained at least22GiB throughout, above the3GiB floor.
- Self-review complete. One scoped local commit contains this wave; the controller
  performs the independent review, actual-browser checks and retained-schema3
  verifier acceptance. No push/ship was run.

## Scope and concerns

Changed files:

- `src-tauri/src/neev/content/{resources.rs,messages.rs,documents.rs,backup.rs}`
- `src-tauri/src/neev/content/{resource_tests.rs,backup_tests.rs}`
- `src-tauri/src/neev/db/{mod.rs,content_migration.rs,content_tests.rs,tests.rs}`
- `src-tauri/src/neev/http/content_tests.rs`
- `src/components/neev/{native-document.tsx,native-document.test.tsx,native-content.test.tsx}`
- `docs/superpowers/specs/2026-09-11-native-chats-and-documents-design.md`
- `docs/superpowers/plans/2026-09-11-native-chats-and-documents.md`
- `docs/operations/neev-content-backup.md`
- This report and sibling `task-8-report.md`.

Self-review confirmed every resource/message success commit has the fresh session
check; blob errors still pass final authority checks; unchanged polling never
advances dirty/ambiguous preconditions; history caches stay inside the exact
auth/resource component; and the migration preserves FK targets rather than
rebuilding the resource table. The delayed-body test correction above was the
only self-review fix after production code froze. No open correctness concern
was found. The controller's independent review remains a separate gate.

Deferred nonblocking findings remain: construction/assertion-heavy content types
test; migration string type codes; unused catalog `nodes::get`; obsolete startup
allowance/comment. No unrelated refactoring was performed.

The fixture capture tempfile disk bound remains deferred and is now stated
accurately. Existing Rust warnings include a legacy migration unused `DbBackend`
import, unused native `nodes::get`, macOS linker unwind-table warning, and
`proc-macro-error2` future incompatibility. Existing frontend tooling emits the
Vite CJS API deprecation. The full frontend run also retains existing React act
warnings, expected negative-path console output, and KaTeX diagnostic warnings;
all465 files passed. No new scoped lint/type/format warning was introduced.
The existing document component is large (726 lines); this wave keeps the
established file structure and confines changes to refresh/history/conflict state.

The WorkOS org-memory page informed inherited source/build conventions;
its historical Git/operator architecture is superseded by the approved native
content spec. [org-memory: wiki/engineering/workos.md]
