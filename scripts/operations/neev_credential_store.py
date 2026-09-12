"""Persistent, scope-bound credential-volume key custody for Neev."""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
import errno
import fcntl
import hashlib
import hmac
import json
import os
from pathlib import Path
import stat
from typing import Dict, Iterator, List, Optional, Tuple
import uuid


_SAFE_ERROR_CODES = frozenset(("invalid", "unavailable", "conflict", "busy", "quota"))
_HANDLE_LENGTH = 32
_KEY_LENGTH = 32
_MANIFEST_LIMIT = 4096
_MAX_ENTRIES = 64
_PRIVATE_DIRECTORY_MODE = 0o700
_PRIVATE_FILE_MODE = 0o600
_MANIFEST_FIELDS = frozenset(
    ("version", "org_id", "user_id", "provider", "handle", "phase", "luks_uuid", "mac")
)
_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)
_CLOEXEC = getattr(os, "O_CLOEXEC", 0)
_NONBLOCK = getattr(os, "O_NONBLOCK", 0)
_DIRECTORY = getattr(os, "O_DIRECTORY", 0)


@dataclass(frozen=True)
class Scope:
    org_id: int
    user_id: int
    provider: str


class StoreError(Exception):
    """Credential store failure containing only a fixed, non-sensitive code."""

    def __init__(self, code: str):
        if code not in _SAFE_ERROR_CODES:
            raise ValueError("StoreError requires a fixed safe code")
        self.code = code
        super().__init__(code)


class CredentialStore:
    """Owns a persistent master key and authenticated per-scope manifests."""

    def __init__(self, root: Path, key_path: Path):
        self._root = Path(root)
        self._key_path = Path(key_path)
        self._root_fd: Optional[int] = None

    def initialize(self) -> None:
        """Create private roots, lock metadata, and a key on a pristine store."""
        try:
            self._validate_configured_path(self._root, directory=True)
            self._validate_configured_path(self._key_path, directory=False)
            self._ensure_private_directory(self._root)
            self._ensure_private_directory(self._key_path.parent)
            root_fd = self._open_directory(self._root, private_final=True)
            try:
                self._ensure_lock_file(root_fd)
            finally:
                os.close(root_fd)

            with self.locked():
                key = self._read_master_key(optional=True, flush=True)
                if key is not None:
                    return
                assert self._root_fd is not None
                names = os.listdir(self._root_fd)
                if any(name != ".lock" for name in names):
                    raise StoreError("conflict")
                self._create_master_key()
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None

    @contextmanager
    def locked(self) -> Iterator["CredentialStore"]:
        """Hold the store's nonblocking exclusive metadata lock."""
        if self._root_fd is not None:
            raise StoreError("busy")
        root_fd: Optional[int] = None
        lock_fd: Optional[int] = None
        acquired = False
        try:
            self._validate_configured_path(self._root, directory=True)
            root_fd = self._open_directory(self._root, private_final=True)
            lock_fd = self._open_checked_file(root_fd, ".lock", os.O_RDWR, "unavailable")
            if os.fstat(lock_fd).st_size != 0:
                raise StoreError("unavailable")
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                if error.errno in (errno.EACCES, errno.EAGAIN, errno.EWOULDBLOCK):
                    raise StoreError("busy") from None
                raise StoreError("unavailable") from None
            self._root_fd = root_fd
            acquired = True
        except StoreError:
            if lock_fd is not None:
                os.close(lock_fd)
            if root_fd is not None:
                os.close(root_fd)
            raise
        except (OSError, ValueError, TypeError):
            if lock_fd is not None:
                os.close(lock_fd)
            if root_fd is not None:
                os.close(root_fd)
            raise StoreError("unavailable") from None
        try:
            yield self
        finally:
            self._root_fd = None
            if lock_fd is not None:
                if acquired:
                    try:
                        fcntl.flock(lock_fd, fcntl.LOCK_UN)
                    except OSError:
                        pass
                os.close(lock_fd)
            if root_fd is not None:
                os.close(root_fd)

    def allocate(self, scope: Scope) -> str:
        """Allocate a unique entry with an authenticated provisioning manifest."""
        self._require_lock()
        self._validate_scope(scope)
        assert self._root_fd is not None
        try:
            entries = [name for name in os.listdir(self._root_fd) if name != ".lock"]
            if len(entries) >= _MAX_ENTRIES:
                raise StoreError("quota")
            while True:
                handle = uuid.uuid4().hex
                try:
                    os.mkdir(handle, _PRIVATE_DIRECTORY_MODE, dir_fd=self._root_fd)
                    break
                except FileExistsError:
                    continue
            os.fsync(self._root_fd)
            entry_fd = self._open_entry(handle)
            try:
                self._write_manifest_to_entry(
                    entry_fd, scope, handle, "provisioning", None, initial=True
                )
            finally:
                os.close(entry_fd)
            return handle
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None

    def read_manifest(self, scope: Scope, handle: str) -> dict:
        """Return an authenticated manifest only for its exact owner scope."""
        self._require_lock()
        self._validate_scope(scope)
        self._validate_handle(handle)
        entry_fd = self._open_entry(handle)
        try:
            manifest, unused_key = self._read_manifest_from_entry(entry_fd, scope, handle)
            del unused_key
            return manifest
        finally:
            os.close(entry_fd)

    def write_manifest(
        self,
        scope: Scope,
        handle: str,
        phase: str,
        luks_uuid: str | None,
    ) -> None:
        """Atomically update an already authenticated manifest."""
        self._require_lock()
        self._validate_scope(scope)
        self._validate_handle(handle)
        self._validate_phase_and_uuid(phase, luks_uuid)
        entry_fd = self._open_entry(handle)
        try:
            self._write_manifest_to_entry(
                entry_fd, scope, handle, phase, luks_uuid, initial=False
            )
        finally:
            os.close(entry_fd)

    def volume_key(self, scope: Scope, handle: str) -> bytes:
        """Derive the entry key after authenticating its manifest and scope."""
        self._require_lock()
        self._validate_scope(scope)
        self._validate_handle(handle)
        entry_fd = self._open_entry(handle)
        try:
            unused_manifest, master_key = self._read_manifest_from_entry(
                entry_fd, scope, handle
            )
            del unused_manifest
        finally:
            os.close(entry_fd)
        return hmac.new(
            master_key,
            b"neev-volume-key-v1\x00" + self._scope_bytes(scope, handle),
            hashlib.sha256,
        ).digest()

    def entry_path(self, handle: str) -> Path:
        """Return the internal fixed-layout path for a validated handle."""
        self._validate_handle(handle)
        return self._root / handle

    def _require_lock(self) -> None:
        if self._root_fd is None:
            raise StoreError("busy")

    @staticmethod
    def _validate_configured_path(path: Path, directory: bool) -> None:
        if not path.is_absolute() or ".." in path.parts:
            raise StoreError("unavailable")
        if path == Path("/") or (not directory and path.name in ("", ".", "..")):
            raise StoreError("unavailable")

    @staticmethod
    def _validate_scope(scope: Scope) -> None:
        if not isinstance(scope, Scope):
            raise StoreError("invalid")
        for value in (scope.org_id, scope.user_id):
            if type(value) is not int or not 1 <= value <= 2147483647:
                raise StoreError("invalid")
        if type(scope.provider) is not str or scope.provider not in ("claude", "codex"):
            raise StoreError("invalid")

    @staticmethod
    def _validate_handle(handle: str) -> None:
        if type(handle) is not str or len(handle) != _HANDLE_LENGTH:
            raise StoreError("invalid")
        if any(character not in "0123456789abcdef" for character in handle):
            raise StoreError("invalid")

    @staticmethod
    def _validate_phase_and_uuid(phase: str, luks_uuid: str | None) -> None:
        if type(phase) is not str or phase not in ("provisioning", "ready"):
            raise StoreError("invalid")
        if phase == "provisioning":
            if luks_uuid is not None:
                raise StoreError("invalid")
            return
        if type(luks_uuid) is not str:
            raise StoreError("invalid")
        try:
            parsed = uuid.UUID(luks_uuid)
        except (ValueError, AttributeError, TypeError):
            raise StoreError("invalid") from None
        if str(parsed) != luks_uuid:
            raise StoreError("invalid")

    def _ensure_private_directory(self, path: Path) -> None:
        parent_fd = self._open_directory(path.parent, private_final=False)
        try:
            try:
                os.mkdir(path.name, _PRIVATE_DIRECTORY_MODE, dir_fd=parent_fd)
                os.fsync(parent_fd)
            except FileExistsError:
                pass
        finally:
            os.close(parent_fd)
        directory_fd = self._open_directory(path, private_final=True)
        os.close(directory_fd)

    def _open_directory(self, path: Path, private_final: bool) -> int:
        self._validate_configured_path(path, directory=True)
        flags = os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC
        current_fd = os.open("/", flags)
        try:
            self._validate_ancestor(os.fstat(current_fd))
            parts = path.parts[1:]
            for index, component in enumerate(parts):
                next_fd = os.open(component, flags, dir_fd=current_fd)
                os.close(current_fd)
                current_fd = next_fd
                metadata = os.fstat(current_fd)
                if private_final and index == len(parts) - 1:
                    self._validate_private_directory(metadata)
                else:
                    self._validate_ancestor(metadata)
            return current_fd
        except StoreError:
            os.close(current_fd)
            raise
        except (OSError, ValueError, TypeError):
            os.close(current_fd)
            raise StoreError("unavailable") from None

    @staticmethod
    def _validate_ancestor(metadata: os.stat_result) -> None:
        if not stat.S_ISDIR(metadata.st_mode):
            raise StoreError("unavailable")
        mode = stat.S_IMODE(metadata.st_mode)
        if mode & 0o022 and not mode & stat.S_ISVTX:
            raise StoreError("unavailable")

    @staticmethod
    def _validate_private_directory(metadata: os.stat_result) -> None:
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != _PRIVATE_DIRECTORY_MODE
        ):
            raise StoreError("unavailable")

    def _ensure_lock_file(self, root_fd: int) -> None:
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | _NOFOLLOW | _CLOEXEC | _NONBLOCK
        created = False
        try:
            lock_fd = os.open(".lock", flags, _PRIVATE_FILE_MODE, dir_fd=root_fd)
            created = True
        except FileExistsError:
            lock_fd = self._open_checked_file(root_fd, ".lock", os.O_RDWR, "unavailable")
        try:
            self._validate_private_file(os.fstat(lock_fd), "unavailable")
            if os.fstat(lock_fd).st_size != 0:
                raise StoreError("unavailable")
            if created:
                os.fsync(lock_fd)
                os.fsync(root_fd)
        finally:
            os.close(lock_fd)

    def _create_master_key(self) -> None:
        key_parent_fd = self._open_directory(self._key_path.parent, private_final=True)
        key_fd: Optional[int] = None
        try:
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | _NOFOLLOW | _CLOEXEC | _NONBLOCK
            key_fd = os.open(
                self._key_path.name,
                flags,
                _PRIVATE_FILE_MODE,
                dir_fd=key_parent_fd,
            )
            self._validate_private_file(os.fstat(key_fd), "unavailable")
            self._write_all(key_fd, os.urandom(_KEY_LENGTH))
            if os.fstat(key_fd).st_size != _KEY_LENGTH:
                raise StoreError("unavailable")
            os.fsync(key_fd)
            os.fsync(key_parent_fd)
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None
        finally:
            if key_fd is not None:
                os.close(key_fd)
            os.close(key_parent_fd)

    def _read_master_key(
        self, optional: bool = False, flush: bool = False
    ) -> Optional[bytes]:
        key_parent_fd = self._open_directory(self._key_path.parent, private_final=True)
        key_fd: Optional[int] = None
        try:
            key_fd = self._open_checked_file(
                key_parent_fd,
                self._key_path.name,
                os.O_RDONLY,
                "unavailable",
                missing_ok=optional,
            )
            if key_fd is None:
                return None
            key = self._read_bounded(key_fd, _KEY_LENGTH)
            if len(key) != _KEY_LENGTH:
                raise StoreError("unavailable")
            if flush:
                os.fsync(key_fd)
                os.fsync(key_parent_fd)
            return key
        except StoreError:
            raise StoreError("unavailable") from None
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None
        finally:
            if key_fd is not None:
                os.close(key_fd)
            os.close(key_parent_fd)

    def _open_entry(self, handle: str) -> int:
        self._require_lock()
        self._validate_handle(handle)
        assert self._root_fd is not None
        flags = os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC
        try:
            entry_fd = os.open(handle, flags, dir_fd=self._root_fd)
            self._validate_private_directory(os.fstat(entry_fd))
            return entry_fd
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("invalid") from None

    def _open_checked_file(
        self,
        directory_fd: int,
        name: str,
        flags: int,
        code: str,
        missing_ok: bool = False,
    ) -> Optional[int]:
        try:
            file_fd = os.open(
                name,
                flags | _NOFOLLOW | _CLOEXEC | _NONBLOCK,
                dir_fd=directory_fd,
            )
            try:
                self._validate_private_file(os.fstat(file_fd), code)
            except BaseException:
                os.close(file_fd)
                raise
            return file_fd
        except FileNotFoundError:
            if missing_ok:
                return None
            raise StoreError(code) from None
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError(code) from None

    @staticmethod
    def _validate_private_file(metadata: os.stat_result, code: str) -> None:
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != _PRIVATE_FILE_MODE
            or metadata.st_nlink != 1
        ):
            raise StoreError(code)

    @staticmethod
    def _read_bounded(file_fd: int, limit: int) -> bytes:
        os.lseek(file_fd, 0, os.SEEK_SET)
        chunks: List[bytes] = []
        remaining = limit + 1
        while remaining:
            chunk = os.read(file_fd, min(remaining, 1024))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        value = b"".join(chunks)
        if len(value) > limit:
            raise StoreError("invalid")
        return value

    def _read_manifest_from_entry(
        self, entry_fd: int, scope: Scope, handle: str
    ) -> Tuple[dict, bytes]:
        manifest_fd = self._open_checked_file(
            entry_fd, "manifest.json", os.O_RDONLY, "invalid"
        )
        assert manifest_fd is not None
        try:
            raw = self._read_bounded(manifest_fd, _MANIFEST_LIMIT)
        except (StoreError, OSError, ValueError, TypeError):
            raise StoreError("invalid") from None
        finally:
            os.close(manifest_fd)
        try:
            manifest = json.loads(raw.decode("utf-8"), object_pairs_hook=self._unique_object)
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError, TypeError):
            raise StoreError("invalid") from None
        self._validate_manifest(manifest)
        if (
            manifest["org_id"] != scope.org_id
            or manifest["user_id"] != scope.user_id
            or manifest["provider"] != scope.provider
            or manifest["handle"] != handle
        ):
            raise StoreError("invalid")
        master_key = self._read_master_key()
        assert master_key is not None
        expected = self._manifest_mac(master_key, manifest)
        if not hmac.compare_digest(manifest["mac"], expected):
            raise StoreError("invalid")
        return manifest, master_key

    @staticmethod
    def _unique_object(pairs: List[Tuple[str, object]]) -> Dict[str, object]:
        value: Dict[str, object] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError("duplicate JSON field")
            value[key] = item
        return value

    def _validate_manifest(self, manifest: object) -> None:
        if type(manifest) is not dict or frozenset(manifest) != _MANIFEST_FIELDS:
            raise StoreError("invalid")
        assert isinstance(manifest, dict)
        if type(manifest["version"]) is not int or manifest["version"] != 1:
            raise StoreError("invalid")
        scope = Scope(manifest["org_id"], manifest["user_id"], manifest["provider"])
        self._validate_scope(scope)
        self._validate_handle(manifest["handle"])
        self._validate_phase_and_uuid(manifest["phase"], manifest["luks_uuid"])
        mac = manifest["mac"]
        if type(mac) is not str or len(mac) != 64:
            raise StoreError("invalid")
        if any(character not in "0123456789abcdef" for character in mac):
            raise StoreError("invalid")

    def _write_manifest_to_entry(
        self,
        entry_fd: int,
        scope: Scope,
        handle: str,
        phase: str,
        luks_uuid: str | None,
        initial: bool,
    ) -> None:
        if initial:
            self._require_absent_manifest(entry_fd)
            master_key = self._read_master_key()
            assert master_key is not None
        else:
            unused_manifest, master_key = self._read_manifest_from_entry(
                entry_fd, scope, handle
            )
            del unused_manifest
        unsigned = {
            "version": 1,
            "org_id": scope.org_id,
            "user_id": scope.user_id,
            "provider": scope.provider,
            "handle": handle,
            "phase": phase,
            "luks_uuid": luks_uuid,
        }
        candidate = dict(unsigned)
        candidate["mac"] = self._manifest_mac(master_key, candidate)
        self._validate_manifest(candidate)
        payload = self._canonical_json(candidate)
        if not payload or len(payload) > _MANIFEST_LIMIT:
            raise StoreError("invalid")

        temporary_name = ".manifest-" + uuid.uuid4().hex + ".tmp"
        temporary_fd: Optional[int] = None
        try:
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | _NOFOLLOW | _CLOEXEC | _NONBLOCK
            temporary_fd = os.open(
                temporary_name,
                flags,
                _PRIVATE_FILE_MODE,
                dir_fd=entry_fd,
            )
            self._validate_private_file(os.fstat(temporary_fd), "unavailable")
            self._write_all(temporary_fd, payload)
            os.fsync(temporary_fd)
            os.close(temporary_fd)
            temporary_fd = None
            os.replace(
                temporary_name,
                "manifest.json",
                src_dir_fd=entry_fd,
                dst_dir_fd=entry_fd,
            )
            os.fsync(entry_fd)
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None
        finally:
            if temporary_fd is not None:
                os.close(temporary_fd)

    @staticmethod
    def _require_absent_manifest(entry_fd: int) -> None:
        try:
            os.stat("manifest.json", dir_fd=entry_fd, follow_symlinks=False)
        except FileNotFoundError:
            return
        except OSError:
            raise StoreError("unavailable") from None
        raise StoreError("conflict")

    @staticmethod
    def _write_all(file_fd: int, value: bytes) -> None:
        offset = 0
        while offset < len(value):
            written = os.write(file_fd, value[offset:])
            if written <= 0:
                raise OSError(errno.EIO, "short write")
            offset += written

    @staticmethod
    def _canonical_json(value: dict) -> bytes:
        return json.dumps(
            value,
            ensure_ascii=True,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("ascii")

    def _manifest_mac(self, master_key: bytes, manifest: dict) -> str:
        unsigned = {key: value for key, value in manifest.items() if key != "mac"}
        return hmac.new(
            master_key,
            b"neev-manifest-mac-v1\x00" + self._canonical_json(unsigned),
            hashlib.sha256,
        ).hexdigest()

    @staticmethod
    def _scope_bytes(scope: Scope, handle: str) -> bytes:
        return (
            str(scope.org_id).encode("ascii")
            + b"\x00"
            + str(scope.user_id).encode("ascii")
            + b"\x00"
            + scope.provider.encode("ascii")
            + b"\x00"
            + handle.encode("ascii")
        )
