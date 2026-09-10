# Task1 native schema and identity report

Implemented native-only module under src-tauri/src/neev with independent SeaQuery
schema creation, foreign keys/unique constraints, protected account/identity/email
resolution and purpose-bound256bit tokens. No old data migration, routes, provider
calls, deployment or runtime enabled. No real support mailbox proof is claimed.

Files: lib.rs module declaration; neev/{mod,error,types,clock,test_support}.rs;
neev/db/{mod,migration,entities,tests}.rs;
neev/auth/{mod,email,identity,secrets,tests}.rs. The account query projection is
kept in db/entities.rs and used directly by identity resolution; no redundant ORM
entities or entity serialization are introduced. Shared native ID, context,
organization, node and clock contracts are present for subsequent tasks.

Implemented behaviors:
- Reject nonempty legacy/partial/unknown schema without native writes; native
  marker and PostgreSQL advisory transaction lock protect initialization.
- SQLite write intent before reads; PostgreSQL account then session locks; no
  network in transactions. Concurrent email/identity unique conflicts roll back.
- Provider/issuer/subject uniqueness, conservative shared ASCII email normalization,
  no email-automerge or domain membership. Native account can exist without org.
- Linking requires current recent native session and account revision; revoked,
  stale, invented sessions and another account's subject/email refused.
- Random tokens have typed purposes; SHA256hash-only lookup values reject wrong
  purpose and malformed/canonical-base64 violations. Secret structs not Debug.

## TDD evidence

1. Root cargo test native suite against unavailable stubs:1passed8failed, build
   3m21s. Auth tests at this stage failed at schema fixture; not meaningful authRED.
2. After schema implementation:4schemaPASS,5authFAIL (resolve Unavailable) build
   1m25s. Wrong issuer assertion expected Invalid but got Unavailable.
3. After identity implementation + emailhelper:11PASS build1m30s. Standalone
   rustc email test previously failed expected Some(support@neev.so), gotNone;
   after implementation2PASS.
4. Added token stub and SQL-null session regression:15PASS2FAIL2guardedPGignored,
   build2m54s. Failures: token generatorUnavailable; partiallyscoped session insert
   unexpectedly succeeded because SQL CHECK accepts UNKNOWN. Explicit IS NOT NULL
   revision checks fixed the latter.
5. Scratch harness compiles actual source via path, no copied implementation.
   Initial offline resolution failed yankedcachedspin; copied existing root lock
   into scratch via apply_patch, no rootlock/dependency changes. Harness then
   reproduced both failures; corrected source17PASS2guardedPGignored in1.59s build.
6. Explicit PG harness run2PASS0ignored in0.09s. Real local PostgreSQL16 private
   Unixsocket, testDBneev_identity_test; each test creates/cleans its own schema.
   Tests cover schema reinitialization/FK denial and concurrentemailcollision with
   exactly one resulting account/identity. No production DB used.

Final root command completed:
NEEV_TEST_DATABASE_URL=<isolated socket DB> cargo test --manifest-path
src-tauri/Cargo.toml --lib --no-default-features neev:: -- --include-ignored
--test-threads=1. Result19passed0failed0ignored, build2m50s, tests0.37s, exit0.
This includes both actual PostgreSQL cases in the real application test binary.

Warnings: existing DbBackendunusedimport, largeunwindlinker, proc-macro future
incompatibility. New TokenPurpose variants and Forbidden/Limited unused until later
tasks. No warnings suppressed. rustfmt installed throughrustup; nativefmt/check and
gitdiffcheck pass. Sourcepreexisting .context not staged. Scratch files not committed.

Self-review found SQLnullcheck issue, now covered/fixed; no identity mapping via
email alone. Review should scrutinize lock/collision/session authority contracts
and task1 completeness. The source remains a foundation, not a working login/UI.

## Review fix round 1

Addressed all three Important findings and the token-coverage Minor without adding
runtime behavior, routes, provider calls or external effects:

- Added the complete shared-context type surface plus Clock/SystemClock, and moved
  the actual Account query projection from identity.rs to db/entities.rs.
- Replaced the PostgreSQL test guard's substring check with URL parsing, fragment
  rejection and exact percent-decoded database-name validation before connection.
  A wrong database whose query contains `/neev_identity_test?` is rejected.
- Added a distinct new-subject/foreign-owned-email link test and verified that the
  tentative identity insert rolls back on Conflict. The prior test reached the
  foreign-subject guard and did not prove this path.
- Exercised Session, Csrf, Browser, OAuth, Magic and Invite token purposes.

Observed TDD/verification evidence:

1. The new link rollback characterization passed against the unchanged resolver:
   1 passed, proving the behavior existed while the required branch coverage did
   not.
2. The shared-contract/URL-guard test compile failed with unresolved clock,
   entities, shared types and isolated_postgres_database_name symbols (exit 101).
3. After the minimal implementation, the source-linked scratch harness passed
   20 tests with the 2 explicitly gated PostgreSQL cases ignored (exit 0).
4. Final root command with the isolated local PostgreSQL socket:
   `NEEV_TEST_DATABASE_URL=<isolated-test-url> CARGO_INCREMENTAL=0
   CARGO_BUILD_JOBS=1 cargo test --manifest-path src-tauri/Cargo.toml --lib
   --no-default-features neev:: -- --include-ignored --test-threads=1`.
   Result: 22 passed, 0 failed, 0 ignored, 3780 filtered; build 3m48s,
   tests 0.21s, exit 0. Both PostgreSQL cases executed.

Warnings remain limited to the pre-existing unused migration import, intentionally
future AuthError variants, linker compact-unwind size, and proc-macro future
compatibility. No warning was suppressed. Formatting and git diff checks pass.
