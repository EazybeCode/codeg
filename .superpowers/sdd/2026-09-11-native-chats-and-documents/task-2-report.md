# Task 2 report: protected immutable blob storage and startup configuration

## Status

DONE.

Implemented an application-owned, immutable document blob store and made its
dedicated root mandatory at native startup. The work stays within the Task 2
files and preserves the untracked `.context/` tree.

## Implementation

- Added `BlobStore::open`, `put`, and `read`, plus `BlobInfo`.
- The Unix implementation walks every configured root component from `/` via
  `openat(O_DIRECTORY | O_NOFOLLOW)`. It never converts the verified directory
  descriptors back into paths for blob operations.
- The configured root and per-organization directories must be owned by the
  effective UID and have exactly mode `0700`, including rejection of special
  permission bits. Blob files must be regular, single-link, current-EUID-owned,
  exactly mode `0600`, and the recorded size must match `fstat` before reading.
- Organization IDs must be positive and blob keys must be canonical lowercase
  hyphenated UUIDs. Writes and recorded read sizes are bounded to 1 MiB.
- Writes use `<uuid>.tmp` with exclusive `0600` creation, bounded write,
  `sync_all`, and an atomic no-replace `linkat` publication to `<uuid>.md`.
  The temporary link is removed and the organization directory is synced.
  Existing destinations therefore cannot be replaced.
- Reads use descriptor-relative no-follow opens, bounded reads, exact size
  checks, and lowercase SHA-256 verification.
- Added test-only, non-public failure injection at before/after write, file
  sync, finalize, and directory sync. Tests prove pre-publication failures
  clean up the temporary entry and post-publication failures cannot make a
  retry overwrite the published bytes.
- Added mandatory `NEEV_DOCUMENT_ROOT` parsing and `Config.document_root`.
  Missing, empty, relative, nonexistent, symlinked-leaf, or symlinked-parent
  roots are rejected. The document and static roots cannot be equal or an
  ancestor/descendant of each other.
- `ServerState` now owns `Arc<BlobStore>` and opens it before the listener is
  bound, so invalid permissions, ownership, inode type, or platform abort
  startup rather than accepting HTTP.
- Extended native test and browser fixtures with owned `TempDir` roots at
  `0700`. macOS temporary paths are canonicalized before configuration so the
  system `/var -> /private/var` alias is not accidentally supplied as a
  symlinked ancestor.
- Non-Unix builds keep the interface but `BlobStore::open` fails closed with
  `ContentError::Storage`; no weaker pathname fallback exists.

## TDD evidence

### Initial blob RED

Command (after adding only a compilable stub seam and real filesystem tests):

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features neev::content::blob_tests -- --test-threads=1
```

Relevant output:

```text
running 5 tests
... accepts_zero_byte_markdown_and_rejects_noncanonical_locations ... FAILED
... refuses_permissive_files_and_detects_size_or_hash_mismatch ... FAILED
... refuses_symlinked_or_permissive_org_directories_and_files ... ok
... refuses_symlinked_root_or_parent_and_permissive_root ... FAILED
... writes_reads_and_never_overwrites_an_immutable_blob ... FAILED
test result: FAILED. 1 passed; 4 failed; 3916 filtered out
```

The expected behavioral failures were the seam returning `Storage` for real
writes/reads and accepting a symlinked root. A preceding unresolved test import
was corrected and is deliberately not counted as RED.

### Configuration RED

Command:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features \
neev::config::tests::requires_a_dedicated_absolute_nonsymlink_document_root \
-- --exact --test-threads=1
```

Relevant output:

```text
running 1 test
... requires_a_dedicated_absolute_nonsymlink_document_root ... FAILED
left: None
right: Some(DocumentRoot)
test result: FAILED. 0 passed; 1 failed; 3921 filtered out
```

This was the expected behavioral failure: configuration still accepted an
absent dedicated document root.

### Exact-mode mutation RED

Final self-review noticed that masking only `0777` would accept setuid,
setgid, or sticky bits despite the exact `0700`/`0600` contract. Tests were
added before changing production code.

Command:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features neev::content::blob_tests::refuses_ \
-- --test-threads=1
```

Relevant output:

```text
running 3 tests
... refuses_permissive_files_and_detects_size_or_hash_mismatch ... FAILED
... refuses_symlinked_or_permissive_org_directories_and_files ... ok
... refuses_symlinked_root_or_parent_and_permissive_root ... FAILED
test result: FAILED. 1 passed; 2 failed; 3921 filtered out
```

The expected failures showed that `04600` files and `02700` directories were
accepted. Production masks were then tightened to `07777`.

## GREEN verification

Focused blob command:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features neev::content::blob_tests -- --test-threads=1
```

Output after the base implementation:

```text
test result: ok. 7 passed; 0 failed; 0 ignored; 3917 filtered out
```

Focused config tests, reusing the emitted native test binary:

```text
./target/debug/deps/codeg_lib-9b806c9f34534627 'neev::config::tests' --test-threads=1
```

```text
test result: ok. 6 passed; 0 failed; 0 ignored; 3918 filtered out
```

Exact-mode covering GREEN after the final mask correction:

```text
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features neev::content::blob_tests::refuses_ \
-- --test-threads=1
```

```text
test result: ok. 3 passed; 0 failed; 0 ignored; 3921 filtered out
```

Final full native suite:

```text
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib --no-default-features neev:: -- --include-ignored \
--skip neev::browser_fixture::browser_acceptance_driver --test-threads=1
```

Final output:

```text
Finished `test` profile [unoptimized] target(s) in 4m 30s
running 143 tests
test result: ok. 143 passed; 0 failed; 0 ignored; 3781 filtered out; finished in 7.12s
```

The output contains only the two pre-existing warnings already identified in
Task 1 review: the legacy unused `DbBackend` import and macOS compact-unwind
linker size warning. Task 2's staged `ServerState.blobs` field has a narrow
documented `dead_code` allowance until the next content handler consumes it;
Task 2 introduces no compiler warning.

Formatting and whitespace verification:

```text
rustfmt --edition 2021 --config skip_children=true \
src/neev/content/mod.rs src/neev/content/blob.rs src/neev/content/blob_tests.rs \
src/neev/config.rs src/neev/startup.rs src/neev/test_support.rs \
src/neev/browser_fixture.rs
git diff --check
```

`git diff --check` produced no output.

## Files changed

- `src-tauri/src/neev/content/blob.rs` (new)
- `src-tauri/src/neev/content/blob_tests.rs` (new)
- `src-tauri/src/neev/content/mod.rs`
- `src-tauri/src/neev/config.rs`
- `src-tauri/src/neev/startup.rs`
- `src-tauri/src/neev/test_support.rs`
- `src-tauri/src/neev/browser_fixture.rs`
- `.superpowers/sdd/2026-09-11-native-chats-and-documents/task-2-report.md`

## Self-review and mutation checks

- Removing `O_NOFOLLOW`, canonical UUID validation, positive-org validation,
  exact mode/owner/type/link checks, no-replace publication, size/hash checks,
  the 1 MiB bound, or any failure-stage cleanup/preservation behavior is
  covered by a real filesystem test.
- Existing destination bytes are read back after both an ordinary duplicate
  and every injected post-publication failure.
- Root and parent symlinks, organization-directory symlinks, destination-file
  symlinks, regular-file roots, permissive modes, and special mode bits are
  covered.
- No shell filesystem operation is used by production storage code, no public
  failpoint exists, and no physical blob path is returned. Completion review
  also moved the failure enum and every hook call site fully behind `cfg(test)`;
  production builds contain no no-op failure-injection path.
- No unrelated files were edited, and the pre-existing untracked `.context/`
  remains untouched.

## Platform assumptions, footprint, and concerns

- Verified on `Darwin 23.5.0 arm64`. The Unix contract depends on `openat`,
  `mkdirat`, `linkat`, `unlinkat`, `O_NOFOLLOW`, descriptor `fstat`, and
  directory `fsync`; these are supplied by the existing `libc` dependency.
- Per controller clarification, ancestor directories are traversed without
  following symlinks but are not required to be app-owned or mode `0700`;
  those ownership/mode checks apply only to the dedicated root and org
  directories. Requiring `/`, `/Users`, or `/private` to be user-owned `0700`
  would make deployment impossible.
- Foreign-UID rejection is implemented directly against `fstat.st_uid` and
  current `geteuid`. An unprivileged test process cannot safely manufacture a
  foreign-owned inode, so the suite exercises current ownership positively and
  mode/type/symlink failures behaviorally; no privileged `chown` was attempted.
- The final `src-tauri/target` footprint is `1.6G`; the emitted native test
  binary is `287,205,840` bytes. Free disk was 26 GiB immediately before the
  final suite and 25 GiB afterward (well above the 3 GiB stop threshold); it
  varied between 25-27 GiB during builds due to filesystem rounding/build
  activity.
- Blob operations intentionally stay within the 1 MiB contract. They currently
  perform descriptor-based filesystem and sync operations inline in their
  async methods; this is correct and bounded, but production load testing can
  determine whether a later task should move them to a dedicated blocking pool.

The WorkOS architecture and trusted-network boundary in organizational memory
were consulted while preserving the native-server-only scope.
[org-memory: wiki/engineering/workos.md]
