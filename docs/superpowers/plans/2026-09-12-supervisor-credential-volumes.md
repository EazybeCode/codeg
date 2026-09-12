# Supervisor credential volumes implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide a tested privileged, local-only encrypted volume helper that preserves completed provider credentials across restarts.

**Architecture:** The approved trusted-host key is an exclusively created persistent secret. Each supervisor-allocated handle owns a signed scope manifest and a separate LUKS2-backed, bounded ext4 filesystem. Fixed root-only CLI operations provision, open, inspect and close those volumes; no web route, provider process, arbitrary command, deletion or public service is added.

**Tech Stack:** Python 3 standard library; installed Linux cryptsetup, util-linux and e2fsprogs. Existing native Rust metadata is unchanged.

**Spec:** `docs/superpowers/specs/2026-09-09-personal-agent-connections-design.md`, separate control/credential planes and owner-approved restart-persistent key custody. The owner subsequently approved supervisor-only persistent key custody on the existing trusted Hetzner host. This plan implements the storage slice, not the entire supervisor or authenticated agent execution.

## Global Constraints

- Every member uses their own subscription for Claude, Codex or another supported agent.
- Sharing a chat never grants use of another person's connection.
- Database records, telemetry, app logs, browser storage, screenshots, backups of ordinary app data and Git must never contain provider credentials or login codes.
- No state is derived solely from the existence of a credential file.
- Provider credential volumes are excluded from ordinary backup/restore; recovery requires personal reauthentication.
- A missing or unreadable key must fail closed without replacing the key or deleting ciphertext.
- No real provider credentials, provider clients, database changes, web routes, runtime enablement, paid services or cloud writes by the implementer.
- Preserve every legacy service, repository, volume and backup. No formatting existing files/devices, force/lazy unmount, automatic cleanup of incomplete persisted state, global scanning of containers or deletion command.
- The default CLI is root-only, with fixed paths `/etc/neev-supervisor/credential-master.key` and `/var/lib/neev-supervisor/credentials`; nothing under ordinary app storage. No caller-selected root, key path, command, binary, environment, mount option or image path in the CLI.
- Host administrator and full-host compromise remain outside the protection claim. This is host-key encryption, not TPM-backed encryption. Sector encryption is not a claim of tamper-proof or forensic-erased media.
- New Python files use only standard library dependencies and explicit executable paths. Tests never invoke privileged storage tools unless an explicit isolated Linux acceptance fixture is separately arranged by the controller.

## File responsibilities

- `scripts/operations/neev_credential_store.py`: safe persistent key, scope validation, allocation, signed manifests, exclusive operation lock.
- `scripts/tests/test_neev_credential_store.py`: real temporary filesystem tests for key/scope/manifest protection.
- `scripts/operations/neev_credential_volumes.py`: bounded fixed-command runner, Linux volume lifecycle and root-only CLI.
- `scripts/tests/test_neev_credential_volumes.py`: fixture command boundary, lifecycle/failure behavior and opt-in isolated Linux acceptance.
- `docs/operations/neev-credential-volumes.md`: threat boundary, install/use, exact acceptance and pending integration.

### Task 1: Persistent key and authenticated scope store

**Files:** create `scripts/operations/neev_credential_store.py` and `scripts/tests/test_neev_credential_store.py`.

**Interfaces:**

```python
@dataclass(frozen=True)
class Scope:
    org_id: int
    user_id: int
    provider: str

class StoreError(Exception):
    # Fixed safe code only: invalid, unavailable, conflict, busy, quota.
    code: str

class CredentialStore:
    def __init__(self, root: Path, key_path: Path): ...
    def initialize(self) -> None: ...
    def locked(self): ...  # context manager, flock LOCK_EX | LOCK_NB
    def allocate(self, scope: Scope) -> str: ...
    def read_manifest(self, scope: Scope, handle: str) -> dict: ...
    def write_manifest(self, scope: Scope, handle: str, phase: str, luks_uuid: str | None) -> None: ...
    def volume_key(self, scope: Scope, handle: str) -> bytes: ...
    def entry_path(self, handle: str) -> Path: ...  # internal only
```

The internal constructor supports temporary test roots; CLI in Task2 never exposes these paths. Security ownership is the effective UID of the local process; Task2 requires that UID to be zero. No application imports this helper or gains root access merely by adding it to the repository.

- [ ] **Step 1: Test persistent custody before implementing it.** Write unittest cases using private TemporaryDirectory roots and invented data. Example behavior:

```python
store.initialize()
scope = Scope(11, 22, "claude")
with store.locked():
    handle = store.allocate(scope)
    before = store.volume_key(scope, handle)
reopened = CredentialStore(root, key_path)
with reopened.locked():
    assert reopened.volume_key(scope, handle) == before
    with self.assertRaises(StoreError):
        reopened.read_manifest(Scope(12, 22, "claude"), handle)
```

Run `python3 -m unittest discover -s scripts/tests -p 'test_neev_credential_store.py' -v`; record the initial missing implementation failure honestly, then behavioral RED for missing-key-with-existing-state, unsafe modes/links and corrupted manifest before adding each guard.

- [ ] **Step 2: Implement the guarded store.** Scope fields must be genuine integers (not bool), 1..2147483647; provider exactly claude or codex; handle exactly 32 lowercase hex characters generated internally with UUID4. No user text becomes a path. Bound manifests to 4096 bytes; reject duplicate JSON fields, unknown fields, incorrect types, unknown phase/version, noncanonical handles/UUIDs and invalid MAC.

Persistent files are regular, current-UID owned, exactly0600, single-link; private store/key directories are current-UID owned0700. Reject symlink components and unsafe filesystem objects before reading/writing. Use O_NOFOLLOW and fd-based fstat checks, bounded reads; directories outside the private roots are trusted system ancestors but must not be writable by untrusted users (a sticky temporary ancestor is permissible for isolated test roots). Do not chmod/repair existing unsafe paths silently.

`initialize` is explicit, never called by normal open/status operations. It creates a cryptographically random32-byte key only with O_EXCL after proving there is no existing credential state; preexisting valid key is retained byte-for-byte. Missing key with any existing entry or malformed/incomplete initialization must refuse, preserving state. Flush file and containing directory; failure does not replace anything on retry. Hold an exclusive lock protecting initialization and metadata operations, fail busy rather than waiting indefinitely. Never print or implement repr containing key bytes.

Allocate a unique private directory, at most64 entries, and an authenticated provisioning manifest. Fields: version1, org_id, user_id, provider, handle, phase (`provisioning`/`ready`), luks_uuid (null until ready), mac (64lowercasehex). Derive MAC and volume keys using distinct domain-separated HMAC-SHA256 contexts from the master key and canonical scope/handle bytes. Use `hmac.compare_digest`; wrong scope/key/modified manifest must fail before returning a usable handle. Volume key is32random-lookingbytes and is never stored beside the volume. Manifest updates use exclusive random temporary file, fsync, atomic replace of the owned single-link manifest and directory fsync. Do not overwrite an unverified existing manifest. No remove/rename-to-other-scope operation.

- [ ] **Step 3: Complete behavioral coverage and commit.** Cover initialization replay; newly constructed store after restart; missing/truncated/changed key; empty/malformed/symlink/hardlink/nonregular/world-readable key and manifests; scope/provider/type/handle traversal rejection; wrong org, wrong user, wrong provider; MAC tampering/unknown fields; quota; concurrent lock busy; partial state preservation; injected write/fsync errors. Use actual filesystem outcomes and key equality without printing secret values. Run the whole new unittest module and syntax compilation with PYTHONDONTWRITEBYTECODE=1, then `git diff --check`. Commit only the two scoped files. No Python cache or private test artifacts in Git.

### Task 2: Bounded encrypted volume lifecycle and Linux acceptance

**Files:** create `scripts/operations/neev_credential_volumes.py`, `scripts/tests/test_neev_credential_volumes.py`, `docs/operations/neev-credential-volumes.md`; modify Task1 store only for a concrete interface defect with controller agreement.

**Interfaces:** consumes CredentialStore/Scope/StoreError. Produces `CredentialVolumes(store, runner)` with create(scope)->handle, open(scope,handle)->internal mount path, close(scope,handle)->None, status(scope,handle)->`locked` or `open`. The trusted runner has `run(argv: list[str], input_bytes: bytes | None = None)->bytes`. The root-only CLI exposes `init`, `create --org-id N --user-id N --provider P`, and `open|close|status` with those options plus `--handle H`. Output is bounded JSON containing only handle/state/error; never keys, arbitrary paths, provider account state or tool output.

- [ ] **Step 1: Test a real orchestration boundary before implementation.** The fixture runner records fixed commands and simulates exact kernel/tool status; real temporary files verify exclusive allocation and state preservation. Assert wrong scope refuses before any tool call; open never formats; repeated create never reuses an existing image; tool failure preserves provisioning state; close refuses foreign mapping/mount and busy mounts; reopened helper uses existing key/image without format. Also test runner output overflow, timeout, missing tool and hostile inherited environment using harmless subprocess fixtures through the runner's internal test seam, not real storage tools.

- [ ] **Step 2: Implement fixed Linux lifecycle.** Create only a new exclusive0600 regular image `volume.luks` in the newly allocated entry, preallocate96MiB (refuse if less than512MiB free remains), format it once, open a unique mapping and create a64MiB ext4 filesystem, then close mapping and mark the manifest ready with the verified LUKS UUID. The exact filesystem command limits the filesystem itself, not merely guest scratch:

```text
/usr/sbin/cryptsetup luksFormat --type luks2 --batch-mode --cipher aes-xts-plain64 --key-size 512 --pbkdf pbkdf2 --pbkdf-force-iterations 100000 --key-file - IMAGE
/usr/sbin/cryptsetup open --type luks2 --key-file - IMAGE neev-credential-HANDLE
/usr/sbin/mkfs.ext4 -q -b 4096 -m 0 /dev/mapper/neev-credential-HANDLE 16384
/usr/sbin/cryptsetup luksUUID IMAGE
```

Supply only derived key bytes on stdin to cryptsetup, never argv/env/logs/key files. Verify tool locations on the acceptance host before invoking; if canonical binaries are symlinks to fixed system paths, document their resolution instead of adding PATH search. No --allow-discards, --disable-locks, detached header, force-existing-format or user flags. ext4 metadata reduces usable capacity below64MiB; do not claim64MiB user payload guaranteed.

Open requires a ready authenticated manifest and matching actual LUKS UUID. Mount only its own verified mapping at an empty protected entry mountpoint with nodev,nosuid,noexec; filesystem root owned65532:65532 mode0700. Refuse unexpected nested mounts. Validate an existing mapping/mount before reuse: actual underlying loop device must refer to the exact image device/inode and mounted source must be that mapping's major/minor. Unknown or mismatched resources fail closed, never unmount/close them. Parse bounded fixed-schema system output with LC_ALL=C; status parser tests include malformed and ambiguous output.

Close checks these same ownership relationships, then normal unmount and cryptsetup close only its own mapping. A busy unmount leaves the volume open and returns busy; never lazy/force it. Failed initialization may close only an exact mapping demonstrably created by that invocation; image/manifest remain for inspection. No automated filesystem repair or lost-key replacement. ext4 journal recovery on ordinary remount is allowed, not a claim that unflushed application writes survive power loss.

All subprocesses use absolute fixed binaries, minimal env PATH/LC_ALL only, no shell, bounded stdin,30second deadline and64KiB combined stdout/stderr cap. Kill/reap the owned process group on timeout/overflow; raw output/errors never reach CLI. API errors are fixed safe codes. A global store lock serializes every lifecycle operation; no lock held across user/browser interaction because none exists here.

- [ ] **Step 3: Run source tests and opt-in real Linux fixture.** Ordinary unittest never runs cryptsetup/mount. Provide an explicit Linux acceptance entrypoint gated by `NEEV_CREDENTIAL_VOLUME_ACCEPTANCE=1` plus root and a newly created private temporary root, never production paths. It must run real create/open/write-fsync/close, instantiate a new helper, reopen and read identical invented bytes, reject a wrong scope, verify image does not contain the invented plaintext marker, inspect mount flags/ownership, and fill the filesystem to ENOSPC within its64MiB ceiling. Remove only filler data inside this exact invented fixture to verify it becomes writable again. Check corrupt/missing-key refusal without modifying production state. Close owned mappings/mounts in finally; on failed cleanup retain artifact and report safe resource identifiers. Keep encrypted acceptance artifact/key under its private temp directory for controller inspection; do not claim host reboot or provider-refresh acceptance from process reopen alone.

Run both unittest modules plus `git diff --check`. Controller runs the privileged acceptance on Linux only after review of committed, secret-scanned source. Worker must report actual Linux gate unpassed until that occurs. Document required install permissions, explicit init vs normal restart, signed scope binding, persistent root key trust, volume size/quota, safe recovery, excluded backups, and missing supervisor/provider/HTTP/job/reboot/public-release gates. Commit only scoped files.

## Self-review and dependency mapping

Task1 produces the key and signed scope store consumed by Task2; names above are its fixed interface. File ownership is separate except a controller-approved interface repair. No runtime feature or provider can consume this helper until authenticated supervisor, egress, session/generation and real-account gates pass. The plan's test-only root constructor does not become a CLI escape hatch. Explicit provisioning formats only an exclusively created image, never an existing resource. The existing native image and application sources are unchanged.
