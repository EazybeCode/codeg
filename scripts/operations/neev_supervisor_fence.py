"""Durable supervisor coordinator fence (supervisor Task 7).

Exclusively owns and durably compares the supervisor's
``(maximum_native_epoch, native_coordinator_boot_id)`` pair using one
in-place 64-byte checksummed record beside a zero-length lock inode under a
preprovisioned, protected local-filesystem parent.

Scope: this is the storage/compare primitive only. It adds no daemon, socket,
dispatch loop, network operation, DTO change, readiness marker, native-authority
conversion, journal, scheduler, custody/provider/process or database. Holding a
``Registration``/``CheckedAdmission`` proves neither transport authentication nor
native lock/member/session authority nor permission for any effect. The real
runtime filesystem provisioning, launcher identity/mount proof, whole-supervisor
singleton ownership, reconciliation readiness and async worker drainage remain
separate, later, reviewed gates. Python private objects are a code-discipline
API, not a security boundary against hostile in-process code.

Record format (exactly 64 bytes, no JSON / history / two-slot fallback)::

    0   8   ASCII  NEEVSF01
    8   8   u64 big-endian maximum native epoch
    16  16  raw native coordinator boot bytes
    32  32  SHA-256 over the preceding 32 bytes

The only sentinel is epoch 0 with all-zero boot: the explicit preprovisioned
never-registered state, never a request candidate. The checksum detects
accidental damage/torn writes only; it is not authentication and cannot detect a
hostile rollback of a whole valid record (that is a deployment-policy concern).

Errors are fixed categories with no paths/bytes/OS prose: Invalid, Busy, Stale,
Conflict, Unavailable, Closed, Unsupported.

Linux is the only production entry point; elsewhere ``open`` returns Unsupported
before any filesystem access. Tests exercise the parser/state/compare/fault logic
by injecting a fake filesystem seam; real protected-path acceptance is a Linux
deployment gate performed elsewhere.
"""

import fcntl
import hashlib
import os
import sys
import threading
from dataclasses import dataclass

try:  # Narrow import of the accepted DTO scalars; never modified here.
    from neev_supervisor_dto import Coordinator, Counter, Id
except ImportError:  # pragma: no cover - import path shim for test discovery
    _HERE = os.path.dirname(os.path.abspath(__file__))
    if _HERE not in sys.path:
        sys.path.insert(0, _HERE)
    from neev_supervisor_dto import Coordinator, Counter, Id  # noqa: F401

MARKER = b"NEEVSF01"
RECORD_SIZE = 64
_BODY_SIZE = 32
_MAX_EPOCH = 9223372036854775807  # signed 64-bit maximum (accepted DTO domain)
_ZERO_BOOT = b"\x00" * 16
_LOCK_BASENAME = "coordinator-fence.lock"
_RECORD_BASENAME = "coordinator-fence.record"
_MAX_PARENT_BYTES = 4096
_MAX_PARENT_COMPONENTS = 32

_CATEGORIES = frozenset(
    {"Invalid", "Busy", "Stale", "Conflict", "Unavailable", "Closed", "Unsupported"}
)


class FenceError(Exception):
    """Fixed-category fence failure. Carries no path/bytes/OS detail."""

    def __init__(self, category):
        if category not in _CATEGORIES:
            raise AssertionError("unknown fence category")
        super().__init__(category)
        self.category = category


def _fail(category):
    raise FenceError(category)


# --------------------------------------------------------------------------- #
# Deployment inputs (trusted launcher/deployment mapping; never request/argv/env)
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class Identity:
    device: int
    inode: int


@dataclass(frozen=True)
class FenceDeployment:
    parent_absolute_path: str
    parent_identity: Identity
    protected_directory_uid: int
    parent_gid: int
    parent_mode: int
    supervisor_uid: int
    supervisor_gid: int
    lock_identity: Identity
    record_identity: Identity


def _u(value, bits):
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value < (1 << bits)


def _validate_identity(identity):
    return (
        isinstance(identity, Identity)
        and _u(identity.device, 64)
        and _u(identity.inode, 64)
    )


def _validate_deployment(deployment):
    if not isinstance(deployment, FenceDeployment):
        _fail("Invalid")
    d = deployment
    if not (
        isinstance(d.parent_absolute_path, str)
        and _validate_identity(d.parent_identity)
        and _u(d.protected_directory_uid, 32)
        and _u(d.parent_gid, 32)
        and _u(d.parent_mode, 32)
        and _u(d.supervisor_uid, 32)
        and _u(d.supervisor_gid, 32)
        and _validate_identity(d.lock_identity)
        and _validate_identity(d.record_identity)
    ):
        _fail("Invalid")
    if d.lock_identity == d.record_identity:
        _fail("Invalid")
    _validate_parent_path(d.parent_absolute_path)
    # A directory mode must contain no special bits or group/other write bits.
    if d.parent_mode & 0o7000 or d.parent_mode & 0o022:
        _fail("Invalid")


def _validate_parent_path(path):
    encoded = path.encode("utf-8", "surrogatepass")
    if len(encoded) > _MAX_PARENT_BYTES or b"\x00" in encoded:
        _fail("Invalid")
    if not path.startswith("/") or path == "/":
        _fail("Invalid")
    if "//" in path or path.endswith("/"):
        _fail("Invalid")
    components = [c for c in path.split("/") if c != ""]
    if not components or len(components) > _MAX_PARENT_COMPONENTS:
        _fail("Invalid")
    for component in components:
        if component in (".", ".."):
            _fail("Invalid")


# --------------------------------------------------------------------------- #
# Pure record codec
# --------------------------------------------------------------------------- #


def _encode_record(epoch, boot16):
    if not (0 <= epoch <= _MAX_EPOCH) or len(boot16) != 16:
        _fail("Unavailable")
    body = MARKER + epoch.to_bytes(8, "big") + boot16
    return body + hashlib.sha256(body).digest()


def _parse_record(raw):
    """Return ``(epoch, boot16)`` for a whole valid record or raise Unavailable."""
    if len(raw) != RECORD_SIZE:
        _fail("Unavailable")
    body = raw[:_BODY_SIZE]
    if raw[:8] != MARKER:
        _fail("Unavailable")
    if hashlib.sha256(body).digest() != raw[_BODY_SIZE:]:
        _fail("Unavailable")
    epoch = int.from_bytes(raw[8:16], "big")
    boot16 = raw[16:32]
    if epoch > _MAX_EPOCH:
        _fail("Unavailable")
    if epoch == 0 and boot16 != _ZERO_BOOT:
        # Zero epoch with any non-zero boot is corrupt, never a reset.
        _fail("Unavailable")
    return epoch, boot16


def _candidate_pair(candidate):
    """Validate a DTO Coordinator candidate into ``(epoch, boot16)``.

    A candidate must be a positive-epoch coordinator; the zero sentinel is never
    an acceptable request candidate.
    """
    if not isinstance(candidate, Coordinator):
        _fail("Invalid")
    epoch_text = candidate.epoch.value
    boot_text = candidate.boot_id.value
    try:
        epoch = int(epoch_text)
        boot16 = bytes.fromhex(boot_text)
    except (ValueError, TypeError):
        _fail("Invalid")
    if not (1 <= epoch <= _MAX_EPOCH) or len(boot16) != 16:
        _fail("Invalid")
    return epoch, boot16


# --------------------------------------------------------------------------- #
# Filesystem seam (real syscalls in production; fake injected by tests)
# --------------------------------------------------------------------------- #


class _RealFilesystem:
    """Thin wrapper over the exact syscalls the fence is permitted to use."""

    def platform_supported(self):
        return sys.platform.startswith("linux")

    def geteuid(self):
        return os.geteuid()

    def getegid(self):
        return os.getegid()

    def open_root(self):
        return os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)

    def open_child_dir(self, parent_fd, name):
        return os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=parent_fd,
        )

    def open_file(self, parent_fd, name):
        flags = os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC
        if hasattr(os, "O_NONBLOCK"):
            flags |= os.O_NONBLOCK
        return os.open(name, flags, dir_fd=parent_fd)

    def fstat(self, fd):
        return os.fstat(fd)

    def flock_exclusive_nonblocking(self, fd):
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def pread(self, fd, count, offset):
        return os.pread(fd, count, offset)

    def pwrite(self, fd, data, offset):
        return os.pwrite(fd, data, offset)

    def fsync(self, fd):
        os.fsync(fd)

    def close(self, fd):
        os.close(fd)


def _stat_matches_file(info, identity, supervisor_uid, supervisor_gid, expected_size):
    import stat as _stat

    return (
        info.st_dev == identity.device
        and info.st_ino == identity.inode
        and _stat.S_ISREG(info.st_mode)
        and info.st_nlink == 1
        and info.st_uid == supervisor_uid
        and info.st_gid == supervisor_gid
        and _stat.S_IMODE(info.st_mode) == 0o600
        and info.st_size == expected_size
    )


# --------------------------------------------------------------------------- #
# Registration / admission capabilities
# --------------------------------------------------------------------------- #


class Registration:
    """Non-forgeable proof that a coordinator tuple is the owner's active tuple.

    No public factory, mutable fields, serialization, equality-as-authority or
    ready flag. Bound to one owner instance and one active generation.
    """

    __slots__ = ("_owner_key", "_generation", "_epoch", "_boot16")

    def __init__(self, _owner_key, _generation, _epoch, _boot16):
        self._owner_key = _owner_key
        self._generation = _generation
        self._epoch = _epoch
        self._boot16 = _boot16


class CheckedAdmission:
    """Non-constructible guard for one synchronous check-and-commit boundary."""

    __slots__ = ("_owner_key", "_generation")

    def __init__(self, _owner_key, _generation):
        self._owner_key = _owner_key
        self._generation = _generation


class _AdmissionContext:
    def __init__(self, owner, registration):
        self._owner = owner
        self._registration = registration
        self._guard = None

    def __enter__(self):
        self._guard = self._owner._enter_admission(self._registration)
        return self._guard

    def __exit__(self, exc_type, exc, tb):
        self._owner._exit_admission(self._guard)
        self._guard = None
        return False


# --------------------------------------------------------------------------- #
# Fence owner
# --------------------------------------------------------------------------- #


class FenceOwner:
    def __init__(self, _deployment, _fs):
        self._deployment = _deployment
        self._fs = _fs
        self._owner_key = object()  # identity for O(1) capability invalidation
        self._mutex = threading.Lock()
        self._lock_fd = None
        self._record_fd = None
        self._record = None  # last known-good on-disk bytes
        self._epoch = None  # active durable maximum epoch
        self._boot16 = None  # active durable boot
        self._generation = 0  # bumped on every accepted higher registration
        self._registered = False
        self._in_admission = False
        self._poisoned = False
        self._closed = False

    # -- lifecycle -------------------------------------------------------- #

    @classmethod
    def open(cls, deployment, *, _fs=None):
        fs = _fs if _fs is not None else _RealFilesystem()
        if not fs.platform_supported():
            _fail("Unsupported")
        _validate_deployment(deployment)
        if fs.geteuid() != deployment.supervisor_uid or fs.getegid() != deployment.supervisor_gid:
            _fail("Unavailable")
        owner = cls(deployment, fs)
        try:
            owner._open_locked()
        except FenceError:
            owner._discard_descriptors()
            raise
        except Exception:
            owner._discard_descriptors()
            _fail("Unavailable")
        return owner

    def _open_locked(self):
        import stat as _stat

        d = self._deployment
        parent_fd = self._walk_parent()
        try:
            info = self._fs.fstat(parent_fd)
            if not (
                info.st_dev == d.parent_identity.device
                and info.st_ino == d.parent_identity.inode
                and _stat.S_ISDIR(info.st_mode)
                and info.st_uid == d.protected_directory_uid
                and info.st_gid == d.parent_gid
                and _stat.S_IMODE(info.st_mode) == d.parent_mode
                and not (info.st_mode & 0o022)
                and not (info.st_mode & 0o7000)
            ):
                _fail("Unavailable")

            lock_fd = self._fs.open_file(parent_fd, _LOCK_BASENAME)
            self._lock_fd = lock_fd
            if not _stat_matches_file(
                self._fs.fstat(lock_fd), d.lock_identity, d.supervisor_uid, d.supervisor_gid, 0
            ):
                _fail("Unavailable")
            # Acquire the lifetime lock before any record read.
            try:
                self._fs.flock_exclusive_nonblocking(lock_fd)
            except BlockingIOError:
                _fail("Busy")
            except OSError:
                _fail("Unavailable")

            record_fd = self._fs.open_file(parent_fd, _RECORD_BASENAME)
            self._record_fd = record_fd
            if not _stat_matches_file(
                self._fs.fstat(record_fd),
                d.record_identity,
                d.supervisor_uid,
                d.supervisor_gid,
                RECORD_SIZE,
            ):
                _fail("Unavailable")
            self._record = self._read_record()
            epoch, boot16 = _parse_record(self._record)
            # Adopt durable state, but mint no active registration at open.
            self._epoch, self._boot16 = epoch, boot16
        finally:
            self._fs.close(parent_fd)

    def _walk_parent(self):
        components = [c for c in self._deployment.parent_absolute_path.split("/") if c]
        fd = self._fs.open_root()
        try:
            import stat as _stat

            for index, name in enumerate(components):
                child = self._fs.open_child_dir(fd, name)
                self._fs.close(fd)
                fd = child
                info = self._fs.fstat(fd)
                # Every ancestor (and final parent) is owned by the protected
                # directory identity and grants no group/other write.
                if info.st_uid != self._deployment.protected_directory_uid or (
                    info.st_mode & 0o022
                ):
                    _fail("Unavailable")
                if not _stat.S_ISDIR(info.st_mode):
                    _fail("Unavailable")
            return fd
        except Exception:
            self._fs.close(fd)
            raise

    def _read_record(self):
        # Bounded positional read of at most 65 bytes; require exactly 64.
        chunk = self._fs.pread(self._record_fd, RECORD_SIZE + 1, 0)
        if len(chunk) != RECORD_SIZE:
            _fail("Unavailable")
        return chunk

    def _revalidate_disk(self):
        """Re-read the record and require it identical to the retained copy.

        A change outside this owner's successful update is unavailable even with
        a valid checksum.
        """
        import stat as _stat

        d = self._deployment
        info = self._fs.fstat(self._record_fd)
        if not (
            info.st_dev == d.record_identity.device
            and info.st_ino == d.record_identity.inode
            and _stat.S_ISREG(info.st_mode)
            and info.st_nlink == 1
            and info.st_uid == d.supervisor_uid
            and info.st_gid == d.supervisor_gid
            and _stat.S_IMODE(info.st_mode) == 0o600
            and info.st_size == RECORD_SIZE
        ):
            _fail("Unavailable")
        current = self._read_record()
        if current != self._record:
            _fail("Unavailable")
        _parse_record(current)

    # -- registration ----------------------------------------------------- #

    def register(self, candidate):
        epoch, boot16 = _candidate_pair(candidate)
        if not self._mutex.acquire(blocking=False):
            _fail("Busy")
        try:
            self._require_live()
            self._revalidate_disk()
            durable_epoch, durable_boot = self._epoch, self._boot16
            if epoch < durable_epoch:
                _fail("Stale")
            if epoch == durable_epoch:
                if durable_epoch == 0:
                    # Sentinel is never matched by a (positive) candidate.
                    _fail("Stale")
                if boot16 != durable_boot:
                    _fail("Conflict")
                return self._reconnect_same_tuple()
            return self._accept_higher(epoch, boot16)
        finally:
            self._mutex.release()

    def _reconnect_same_tuple(self):
        # Equal epoch + same boot: durably acknowledge by fsync of the unchanged
        # record before returning a fresh capability. No write, no new boot.
        try:
            self._fs.fsync(self._record_fd)
        except Exception:
            self._poison()
            _fail("Unavailable")
        self._registered = True
        return Registration(self._owner_key, self._generation, self._epoch, self._boot16)

    def _accept_higher(self, epoch, boot16):
        # Revoke all previously minted authority BEFORE the first write.
        self._generation += 1
        self._registered = False
        record = _encode_record(epoch, boot16)
        try:
            self._write_all(record)
            self._fs.fsync(self._record_fd)
            self._revalidate_after_write(record)
        except FenceError:
            self._poison()
            raise
        except Exception:
            self._poison()
            _fail("Unavailable")
        # Install the new active tuple only after complete write + sync + readback.
        self._record = record
        self._epoch, self._boot16 = epoch, boot16
        self._registered = True
        return Registration(self._owner_key, self._generation, epoch, boot16)

    def _write_all(self, record):
        offset = 0
        while offset < RECORD_SIZE:
            written = self._fs.pwrite(self._record_fd, record[offset:], offset)
            if not isinstance(written, int) or written <= 0:
                _fail("Unavailable")
            offset += written
        if offset != RECORD_SIZE:
            _fail("Unavailable")

    def _revalidate_after_write(self, expected):
        import stat as _stat

        info = self._fs.fstat(self._record_fd)
        if not (
            _stat.S_ISREG(info.st_mode)
            and info.st_nlink == 1
            and _stat.S_IMODE(info.st_mode) == 0o600
            and info.st_size == RECORD_SIZE
            and info.st_dev == self._deployment.record_identity.device
            and info.st_ino == self._deployment.record_identity.inode
        ):
            _fail("Unavailable")
        persisted = self._read_record()
        if persisted != expected:
            _fail("Unavailable")

    # -- admission -------------------------------------------------------- #

    def admission(self, registration):
        return _AdmissionContext(self, registration)

    def _enter_admission(self, registration):
        if not self._mutex.acquire(blocking=False):
            _fail("Busy")
        try:
            self._require_live()
            if not self._registered or not self._valid_registration(registration):
                _fail("Unavailable")
            self._revalidate_disk()
            self._in_admission = True
            return CheckedAdmission(self._owner_key, self._generation)
        except BaseException:
            self._mutex.release()
            raise

    def _exit_admission(self, guard):
        try:
            self._in_admission = False
        finally:
            self._mutex.release()

    def _valid_registration(self, registration):
        return (
            isinstance(registration, Registration)
            and registration._owner_key is self._owner_key
            and registration._generation == self._generation
            and registration._epoch == self._epoch
            and registration._boot16 == self._boot16
        )

    # -- shutdown / poison ------------------------------------------------ #

    def close(self):
        if not self._mutex.acquire(blocking=False):
            # Contention (including from inside an entered admission) is Busy;
            # the lifetime lock is retained.
            _fail("Busy")
        try:
            if self._closed:
                return
            # Revoke authority first, then release owned descriptors.
            self._generation += 1
            self._registered = False
            self._epoch = None
            self._boot16 = None
            self._record = None
            self._closed = True
            self._discard_descriptors()
        finally:
            self._mutex.release()

    def _poison(self):
        self._poisoned = True
        self._generation += 1
        self._registered = False

    def _require_live(self):
        if self._closed:
            _fail("Closed")
        if self._poisoned:
            _fail("Unavailable")

    def _discard_descriptors(self):
        for attr in ("_record_fd", "_lock_fd"):
            fd = getattr(self, attr)
            if fd is not None:
                try:
                    self._fs.close(fd)
                except Exception:
                    pass
                setattr(self, attr, None)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        try:
            self.close()
        except FenceError:
            pass
        return False
