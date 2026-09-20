"""Root-only, bounded lifecycle for Neev credential volumes."""

from __future__ import annotations

import argparse
import errno
import json
import os
import re
from pathlib import Path
import selectors
import signal
import stat
import subprocess
import sys
import time
from typing import Dict, List, Optional, Tuple
import uuid

from neev_credential_store import CredentialStore, Scope, StoreError


CRYPTSETUP = "/usr/sbin/cryptsetup"
MKFS = "/usr/sbin/mkfs.ext4"
MOUNT = "/usr/bin/mount"
UMOUNT = "/usr/bin/umount"
FINDMNT = "/usr/bin/findmnt"
LOSETUP = "/usr/sbin/losetup"
MAPPER_ROOT = Path("/dev/mapper")

_STORE_ROOT = Path("/var/lib/neev-supervisor/credentials")
_KEY_PATH = Path("/etc/neev-supervisor/credential-master.key")
_IMAGE_NAME = "volume.luks"
_MOUNT_NAME = "mount"
_IMAGE_BYTES = 96 * 1024 * 1024
_FREE_FLOOR_BYTES = 512 * 1024 * 1024
_OUTPUT_LIMIT = 64 * 1024
_INPUT_LIMIT = 64 * 1024
_TIMEOUT_SECONDS = 30.0
_SAFE_CODES = frozenset(("invalid", "unavailable", "conflict", "busy", "quota"))
_MINIMAL_ENV = {"PATH": "/usr/sbin:/usr/bin", "LC_ALL": "C"}
_ALLOWED_TOOLS = frozenset((CRYPTSETUP, MKFS, MOUNT, UMOUNT, FINDMNT, LOSETUP))
_TOOL_TARGETS = {
    CRYPTSETUP: (Path(CRYPTSETUP), 0o755),
    MKFS: (Path("/usr/sbin/mke2fs"), 0o755),
    # Non-setuid on the native agent image (Dockerfile.native.agent strips ALL
    # setuid/setgid bits); the privileged mount/umount syscalls are authorized by
    # the caplaunch helper's scoped file capabilities, not by a setuid bit. Require
    # exactly 0755 so a re-introduced setuid binary (04755) is REJECTED, not
    # silently trusted.
    MOUNT: (Path(MOUNT), 0o755),
    UMOUNT: (Path(UMOUNT), 0o755),
    FINDMNT: (Path(FINDMNT), 0o755),
    LOSETUP: (Path(LOSETUP), 0o755),
}


class CommandError(Exception):
    """Internal subprocess failure with no captured output or path details."""

    def __init__(self, kind: str, returncode: Optional[int] = None):
        if kind not in ("failed", "timeout", "overflow", "missing"):
            raise ValueError("unsafe command error")
        self.kind = kind
        self.returncode = returncode
        super().__init__(kind)


class FixedToolRunner:
    """Run only audited storage tools with bounded I/O and a minimal environment."""

    def run(self, argv: List[str], input_bytes: Optional[bytes] = None) -> bytes:
        if type(argv) is not list or not argv or argv[0] not in _ALLOWED_TOOLS:
            raise CommandError("missing")
        if any(type(argument) is not str or "\x00" in argument for argument in argv):
            raise CommandError("failed")
        self._verify_tool(argv[0])
        try:
            return self._execute(argv, input_bytes, _TIMEOUT_SECONDS)
        except CommandError as error:
            if argv[0] == FINDMNT and error.kind == "failed" and error.returncode == 1:
                return b'{"filesystems":[]}'
            raise

    def _run_for_test(
        self,
        argv: List[str],
        input_bytes: Optional[bytes] = None,
        timeout: float = _TIMEOUT_SECONDS,
    ) -> bytes:
        """Exercise the bounded process transport without permitting production use."""
        if type(argv) is not list or not argv or not Path(argv[0]).is_absolute():
            raise CommandError("missing")
        return self._execute(argv, input_bytes, timeout)

    @staticmethod
    def _verify_tool(program: str) -> None:
        expected_target, expected_mode = _TOOL_TARGETS[program]
        try:
            configured = Path(program)
            metadata = configured.lstat()
            if program == MKFS:
                if not stat.S_ISLNK(metadata.st_mode):
                    raise CommandError("missing")
            elif stat.S_ISLNK(metadata.st_mode):
                raise CommandError("missing")
            resolved = configured.resolve(strict=True)
            if resolved != expected_target:
                raise CommandError("missing")
            target = resolved.stat()
            if (
                not stat.S_ISREG(target.st_mode)
                or target.st_uid != 0
                or stat.S_IMODE(target.st_mode) != expected_mode
            ):
                raise CommandError("missing")
        except CommandError:
            raise
        except (OSError, RuntimeError, ValueError):
            raise CommandError("missing") from None

    @staticmethod
    def _execute(argv: List[str], input_bytes: Optional[bytes], timeout: float) -> bytes:
        if input_bytes is not None and (
            type(input_bytes) is not bytes or len(input_bytes) > _INPUT_LIMIT
        ):
            raise CommandError("failed")
        process = None
        transport_complete = False
        selector = selectors.DefaultSelector()
        output = bytearray()
        pending = memoryview(input_bytes or b"")
        deadline = time.monotonic() + timeout
        try:
            try:
                process = subprocess.Popen(
                    argv,
                    stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    env=dict(_MINIMAL_ENV),
                    shell=False,
                    start_new_session=True,
                    close_fds=True,
                )
            except FileNotFoundError:
                raise CommandError("missing") from None
            except (OSError, ValueError):
                raise CommandError("failed") from None
            assert process.stdout is not None
            os.set_blocking(process.stdout.fileno(), False)
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            if process.stdin is not None:
                os.set_blocking(process.stdin.fileno(), False)
                if pending:
                    selector.register(process.stdin, selectors.EVENT_WRITE, "stdin")
                else:
                    process.stdin.close()

            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise CommandError("timeout")
                events = selector.select(remaining)
                if not events:
                    raise CommandError("timeout")
                for key, unused_mask in events:
                    if key.data == "stdin":
                        try:
                            count = os.write(key.fileobj.fileno(), pending[:4096])
                            pending = pending[count:]
                        except BrokenPipeError:
                            pending = pending[len(pending):]
                        if not pending:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                    else:
                        try:
                            chunk = os.read(key.fileobj.fileno(), 8192)
                        except BlockingIOError:
                            continue
                        if not chunk:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                            continue
                        output.extend(chunk)
                        if len(output) > _OUTPUT_LIMIT:
                            raise CommandError("overflow")

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise CommandError("timeout")
            try:
                returncode = process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                raise CommandError("timeout") from None
            transport_complete = True
            if returncode != 0:
                raise CommandError("failed", returncode=returncode)
            return bytes(output)
        except BaseException as error:
            if process is not None and not transport_complete:
                FixedToolRunner._terminate_owned_process_group(process)
            if isinstance(error, CommandError):
                raise
            if isinstance(error, Exception):
                raise CommandError("failed") from None
            raise
        finally:
            try:
                selector.close()
            except Exception:
                pass
            if process is not None:
                for stream in (process.stdin, process.stdout):
                    if stream is not None and not stream.closed:
                        try:
                            stream.close()
                        except Exception:
                            pass

    @staticmethod
    def _terminate_owned_process_group(process: subprocess.Popen) -> None:
        """Kill the owned group even when its leader has already exited."""
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (OSError, ProcessLookupError):
            pass
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            try:
                process.kill()
            except (OSError, ProcessLookupError):
                pass
            try:
                process.wait(timeout=1)
            except (OSError, subprocess.TimeoutExpired):
                pass
        except OSError:
            pass


class CredentialVolumes:
    """Serialize and validate each encrypted-volume lifecycle transition."""

    def __init__(self, store: CredentialStore, runner: object):
        self._store = store
        self._runner = runner

    def create(self, scope: Scope) -> str:
        with self._store.locked():
            self._check_capacity()
            handle = self._store.allocate(scope)
            entry = self._store.entry_path(handle)
            image = entry / _IMAGE_NAME
            mountpoint = entry / _MOUNT_NAME
            self._create_mountpoint(mountpoint)
            self._create_image(image)
            key = self._store.volume_key(scope, handle)
            mapping = self._mapping_name(handle)
            mapping_path = MAPPER_ROOT / mapping
            if os.path.lexists(str(mapping_path)):
                raise StoreError("conflict")
            opened = False
            try:
                self._run([
                    CRYPTSETUP, "luksFormat", "--type", "luks2", "--batch-mode",
                    "--cipher", "aes-xts-plain64", "--key-size", "512", "--pbkdf",
                    "pbkdf2", "--pbkdf-force-iterations", "100000", "--key-file", "-",
                    str(image),
                ], key)
                self._run([
                    CRYPTSETUP, "open", "--type", "luks2", "--key-file", "-",
                    str(image), mapping,
                ], key)
                opened = True
                self._run([
                    MKFS, "-q", "-b", "4096", "-m", "0", str(mapping_path), "16384"
                ])
                luks_uuid = _parse_uuid(self._run([CRYPTSETUP, "luksUUID", str(image)]))
            except StoreError:
                if opened:
                    self._close_created_mapping(mapping, image)
                raise
            self._validate_mapping(mapping, image)
            self._run([CRYPTSETUP, "close", mapping])
            self._store.write_manifest(scope, handle, "ready", luks_uuid)
            return handle

    def open(self, scope: Scope, handle: str) -> Path:
        with self._store.locked():
            manifest, image, mountpoint = self._ready_entry(scope, handle)
            self._require_uuid(image, manifest["luks_uuid"])
            mapping = self._mapping_name(handle)
            mapping_path = MAPPER_ROOT / mapping
            mounts = self._mounts(mountpoint)
            _validate_mountpoint(mountpoint, mounted=bool(mounts))
            mapping_exists = os.path.lexists(str(mapping_path))
            if mounts:
                if not mapping_exists:
                    raise StoreError("conflict")
                self._validate_mapping(mapping, image)
                self._validate_owned_mount(mounts, mountpoint, mapping_path)
                return mountpoint
            self._require_empty_mountpoint(mountpoint)
            opened = False
            if mapping_exists:
                self._validate_mapping(mapping, image)
            else:
                key = self._store.volume_key(scope, handle)
                self._run([
                    CRYPTSETUP, "open", "--type", "luks2", "--key-file", "-",
                    str(image), mapping,
                ], key)
                opened = True
                self._validate_mapping(mapping, image)
            try:
                self._run([
                    MOUNT, "-t", "ext4", "-o", "nodev,nosuid,noexec",
                    str(mapping_path), str(mountpoint),
                ])
                _secure_filesystem_root(mountpoint)
                mounted = self._mounts(mountpoint)
                self._validate_owned_mount(mounted, mountpoint, mapping_path)
                _validate_mountpoint(mountpoint, mounted=True)
            except StoreError:
                self._cleanup_failed_open(mapping, image, mountpoint, opened)
                raise
            return mountpoint

    def close(self, scope: Scope, handle: str) -> None:
        with self._store.locked():
            manifest, image, mountpoint = self._ready_entry(scope, handle)
            self._require_uuid(image, manifest["luks_uuid"])
            mapping = self._mapping_name(handle)
            mapping_path = MAPPER_ROOT / mapping
            mounts = self._mounts(mountpoint)
            _validate_mountpoint(mountpoint, mounted=bool(mounts))
            mapping_exists = os.path.lexists(str(mapping_path))
            if not mapping_exists:
                if mounts:
                    raise StoreError("conflict")
                return
            self._validate_mapping(mapping, image)
            if mounts:
                self._validate_owned_mount(mounts, mountpoint, mapping_path)
                self._unmount(mountpoint)
                if self._mounts(mountpoint):
                    raise StoreError("busy")
            self._run([CRYPTSETUP, "close", mapping])

    def status(self, scope: Scope, handle: str) -> str:
        with self._store.locked():
            manifest, image, mountpoint = self._ready_entry(scope, handle)
            self._require_uuid(image, manifest["luks_uuid"])
            mapping = self._mapping_name(handle)
            mapping_path = MAPPER_ROOT / mapping
            mounts = self._mounts(mountpoint)
            _validate_mountpoint(mountpoint, mounted=bool(mounts))
            if not os.path.lexists(str(mapping_path)):
                if mounts:
                    raise StoreError("conflict")
                return "locked"
            self._validate_mapping(mapping, image)
            if not mounts:
                raise StoreError("conflict")
            self._validate_owned_mount(mounts, mountpoint, mapping_path)
            return "open"

    def _check_capacity(self) -> None:
        try:
            root = self._store.entry_path("0" * 32).parent
            usage = os.statvfs(str(root))
            available = usage.f_bavail * usage.f_frsize
        except (OSError, ValueError, TypeError, AttributeError):
            raise StoreError("unavailable") from None
        if available < _IMAGE_BYTES + _FREE_FLOOR_BYTES:
            raise StoreError("quota")

    @staticmethod
    def _create_mountpoint(path: Path) -> None:
        try:
            path.mkdir(mode=0o700)
            os.chmod(str(path), 0o700, follow_symlinks=False)
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None

    @staticmethod
    def _create_image(path: Path) -> None:
        flags = os.O_RDWR | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        flags |= getattr(os, "O_CLOEXEC", 0)
        file_fd = None
        try:
            file_fd = os.open(str(path), flags, 0o600)
            os.fchmod(file_fd, 0o600)
            metadata = os.fstat(file_fd)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise StoreError("conflict")
            _preallocate(file_fd, 0, _IMAGE_BYTES)
            if os.fstat(file_fd).st_size != _IMAGE_BYTES:
                raise StoreError("unavailable")
            os.fsync(file_fd)
            directory_fd = os.open(str(path.parent), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        except FileExistsError:
            raise StoreError("conflict") from None
        except StoreError:
            raise
        except OSError as error:
            if error.errno in (errno.ENOSPC, errno.EDQUOT):
                raise StoreError("quota") from None
            raise StoreError("unavailable") from None
        finally:
            if file_fd is not None:
                os.close(file_fd)

    def _ready_entry(self, scope: Scope, handle: str) -> Tuple[dict, Path, Path]:
        manifest = self._store.read_manifest(scope, handle)
        if manifest["phase"] != "ready":
            raise StoreError("conflict")
        entry = self._store.entry_path(handle)
        image = entry / _IMAGE_NAME
        mountpoint = entry / _MOUNT_NAME
        _validate_image(image)
        return manifest, image, mountpoint

    def _require_uuid(self, image: Path, expected: str) -> None:
        actual = _parse_uuid(self._run([CRYPTSETUP, "luksUUID", str(image)]))
        if actual != expected:
            raise StoreError("conflict")

    def _validate_mapping(self, mapping: str, image: Path) -> None:
        raw = self._run([CRYPTSETUP, "status", mapping])
        loop_device = _parse_crypt_status(raw, mapping)
        raw_loops = self._run([
            LOSETUP, "--json", "--list", "--output",
            "NAME,BACK-FILE,BACK-INO,BACK-MAJ:MIN,MAJ:MIN", loop_device,
        ])
        data = _parse_system_json(raw_loops)
        if set(data) != {"loopdevices"} or type(data["loopdevices"]) is not list:
            raise StoreError("conflict")
        entries = data["loopdevices"]
        if len(entries) != 1 or type(entries[0]) is not dict:
            raise StoreError("conflict")
        item = entries[0]
        if set(item) != {"name", "back-file", "back-ino", "back-maj:min", "maj:min"}:
            raise StoreError("conflict")
        metadata = image.stat()
        expected_device = "%d:%d" % (os.major(metadata.st_dev), os.minor(metadata.st_dev))
        # util-linux (e.g. Debian 2.38) space-pads the JSON MAJ:MIN columns; trim
        # only these device-number fields (strict digits:digits) and keep every
        # other field byte-exact. Normalizing "maj:min" also subsumes its old
        # str-type check (a padded or malformed value is a conflict).
        back_device = _normalized_device_number(item["back-maj:min"])
        _normalized_device_number(item["maj:min"])
        if (
            item["name"] != loop_device
            or type(item["back-file"]) is not str
            or Path(item["back-file"]) != image
            or type(item["back-ino"]) is not int
            or item["back-ino"] != metadata.st_ino
            or back_device != expected_device
        ):
            raise StoreError("conflict")

    def _mounts(self, mountpoint: Path) -> List[dict]:
        raw = self._run([
            FINDMNT, "--json", "--list", "--output",
            "TARGET,SOURCE,FSTYPE,OPTIONS,MAJ:MIN", "--submounts", str(mountpoint),
        ])
        data = _parse_system_json(raw)
        if set(data) != {"filesystems"} or type(data["filesystems"]) is not list:
            raise StoreError("conflict")
        mounts = data["filesystems"]
        for item in mounts:
            if type(item) is not dict or set(item) != {
                "target", "source", "fstype", "options", "maj:min"
            }:
                raise StoreError("conflict")
            if any(type(item[field]) is not str for field in item):
                raise StoreError("conflict")
        return mounts

    @staticmethod
    def _validate_owned_mount(mounts: List[dict], mountpoint: Path, mapping_path: Path) -> None:
        if len(mounts) != 1:
            raise StoreError("conflict")
        item = mounts[0]
        required = {"nodev", "nosuid", "noexec"}
        options = set(item["options"].split(","))
        if (
            item["target"] != str(mountpoint)
            or item["source"] != str(mapping_path)
            or item["fstype"] != "ext4"
            or not required.issubset(options)
            or _normalized_device_number(item["maj:min"]) != _mapping_major_minor(mapping_path)
        ):
            raise StoreError("conflict")

    @staticmethod
    def _require_empty_mountpoint(mountpoint: Path) -> None:
        try:
            if os.listdir(str(mountpoint)):
                raise StoreError("conflict")
        except StoreError:
            raise
        except (OSError, ValueError, TypeError):
            raise StoreError("unavailable") from None

    def _cleanup_failed_open(
        self, mapping: str, image: Path, mountpoint: Path, opened: bool
    ) -> None:
        if not opened:
            return
        try:
            self._validate_mapping(mapping, image)
            mounts = self._mounts(mountpoint)
            if mounts:
                mapping_path = MAPPER_ROOT / mapping
                self._validate_owned_mount(mounts, mountpoint, mapping_path)
                self._run([UMOUNT, str(mountpoint)])
            self._run([CRYPTSETUP, "close", mapping])
        except StoreError:
            pass

    def _close_created_mapping(self, mapping: str, image: Path) -> None:
        try:
            self._validate_mapping(mapping, image)
            self._run([CRYPTSETUP, "close", mapping])
        except StoreError:
            pass

    @staticmethod
    def _mapping_name(handle: str) -> str:
        return "neev-credential-" + handle

    def _run(self, argv: List[str], input_bytes: Optional[bytes] = None) -> bytes:
        try:
            return self._runner.run(argv, input_bytes=input_bytes)
        except CommandError:
            raise StoreError("unavailable") from None
        except StoreError:
            raise
        except Exception:
            raise StoreError("unavailable") from None

    def _unmount(self, mountpoint: Path) -> None:
        try:
            self._runner.run([UMOUNT, str(mountpoint)], input_bytes=None)
        except CommandError as error:
            if error.kind == "failed" and error.returncode == 32:
                raise StoreError("busy") from None
            raise StoreError("unavailable") from None
        except StoreError:
            raise
        except Exception:
            raise StoreError("unavailable") from None


def _validate_image(path: Path) -> None:
    try:
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_nlink != 1
            or metadata.st_size != _IMAGE_BYTES
        ):
            raise StoreError("conflict")
    except StoreError:
        raise
    except (OSError, ValueError, TypeError):
        raise StoreError("conflict") from None


def _preallocate(file_fd: int, offset: int, length: int) -> None:
    """Use Linux allocation; kept as a narrow seam for non-Linux unit tests."""
    try:
        allocator = os.posix_fallocate
    except AttributeError:
        raise StoreError("unavailable") from None
    allocator(file_fd, offset, length)


def _validate_mountpoint(path: Path, mounted: bool) -> None:
    try:
        metadata = path.lstat()
        expected_identity = (
            (65532, 65532) if mounted else (os.geteuid(), os.getegid())
        )
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or _mountpoint_identity(metadata) != expected_identity
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise StoreError("conflict")
    except StoreError:
        raise
    except (OSError, ValueError, TypeError):
        raise StoreError("conflict") from None


def _mountpoint_identity(metadata: os.stat_result) -> Tuple[int, int]:
    return metadata.st_uid, metadata.st_gid


def _secure_filesystem_root(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    directory_fd = None
    try:
        directory_fd = os.open(str(path), flags)
        os.fchown(directory_fd, 65532, 65532)
        os.fchmod(directory_fd, 0o700)
        os.fsync(directory_fd)
    except (OSError, ValueError, TypeError):
        raise StoreError("unavailable") from None
    finally:
        if directory_fd is not None:
            os.close(directory_fd)


_DEVICE_NUMBER_RE = re.compile(r"[0-9]+:[0-9]+")


def _normalized_device_number(value: object) -> str:
    """Canonicalize one MAJ:MIN device-number field from ``losetup``/``findmnt``.

    Debian util-linux (e.g. 2.38) pads the JSON MAJ:MIN string columns with spaces
    (``"       0:93 "``, ``"  7:0  "``). Trim ONLY these validated device-number
    fields and require a strict decimal ``digits:digits`` shape; anything else is a
    conflict. Every other identity field (name/back-file/back-ino) is still compared
    untrimmed and byte-exact, so no identity check is weakened."""
    if type(value) is not str:
        raise StoreError("conflict")
    trimmed = value.strip()
    if _DEVICE_NUMBER_RE.fullmatch(trimmed) is None:
        raise StoreError("conflict")
    return trimmed


def _mapping_major_minor(path: Path) -> str:
    try:
        metadata = path.stat()
        if not stat.S_ISBLK(metadata.st_mode):
            raise StoreError("conflict")
        return "%d:%d" % (os.major(metadata.st_rdev), os.minor(metadata.st_rdev))
    except StoreError:
        raise
    except (OSError, ValueError, TypeError):
        raise StoreError("conflict") from None


def _parse_uuid(raw: bytes) -> str:
    try:
        value = raw.decode("ascii").strip()
        parsed = uuid.UUID(value)
        if str(parsed) != value:
            raise ValueError("noncanonical uuid")
        return value
    except (UnicodeDecodeError, ValueError, AttributeError, TypeError):
        raise StoreError("conflict") from None


def _parse_crypt_status(raw: bytes, mapping: str) -> str:
    try:
        text = raw.decode("ascii")
    except (UnicodeDecodeError, AttributeError):
        raise StoreError("conflict") from None
    lines = text.splitlines()
    active_headers = (
        "/dev/mapper/%s is active." % mapping,
        "/dev/mapper/%s is active and is in use." % mapping,
    )
    if not lines or lines[0] not in active_headers:
        raise StoreError("conflict")
    fields: Dict[str, str] = {}
    allowed = frozenset((
        "type", "cipher", "keysize", "key location", "device", "loop",
        "sector size", "offset", "size", "mode", "flags",
    ))
    for line in lines[1:]:
        if not line.startswith("  ") or ":" not in line:
            raise StoreError("conflict")
        key, value = line.strip().split(":", 1)
        key = key.strip()
        value = value.strip()
        if key not in allowed or key in fields or not value:
            raise StoreError("conflict")
        fields[key] = value
    if fields.get("type") != "LUKS2" or fields.get("mode") != "read/write":
        raise StoreError("conflict")
    device = fields.get("device")
    if device is None or not device.startswith("/dev/loop"):
        raise StoreError("conflict")
    suffix = device[len("/dev/loop"):]
    if not suffix.isdigit() or not suffix:
        raise StoreError("conflict")
    return device


def _parse_system_json(raw: bytes) -> dict:
    def unique(pairs: List[Tuple[str, object]]) -> dict:
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate")
            result[key] = value
        return result

    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=unique)
        if type(value) is not dict or len(value) != 1:
            raise ValueError("not object")
        if set(value) == {"filesystems"}:
            fields = {"target", "source", "fstype", "options", "maj:min"}
        elif set(value) == {"loopdevices"}:
            fields = {"name", "back-file", "back-ino", "back-maj:min", "maj:min"}
        else:
            raise ValueError("unknown schema")
        items = next(iter(value.values()))
        if type(items) is not list:
            raise ValueError("invalid items")
        for item in items:
            if type(item) is not dict or set(item) != fields:
                raise ValueError("invalid item")
        return value
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError, TypeError):
        raise StoreError("conflict") from None


class _SafeArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise ValueError("invalid arguments")


def _parser() -> argparse.ArgumentParser:
    parser = _SafeArgumentParser(add_help=False)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    subparsers.add_parser("init", add_help=False)
    for name in ("create", "open", "close", "status"):
        command = subparsers.add_parser(name, add_help=False)
        command.add_argument("--org-id", required=True, type=int)
        command.add_argument("--user-id", required=True, type=int)
        command.add_argument("--provider", required=True)
        if name != "create":
            command.add_argument("--handle", required=True)
    return parser


def run_cli(argv: List[str], euid: Optional[int] = None) -> Tuple[str, int]:
    """Return bounded JSON and status; euid is an internal unit-test seam."""
    result: dict
    exit_code = 0
    try:
        if (os.geteuid() if euid is None else euid) != 0:
            raise StoreError("unavailable")
        arguments = _parser().parse_args(argv)
        store = CredentialStore(_STORE_ROOT, _KEY_PATH)
        if arguments.operation == "init":
            store.initialize()
            result = {"state": "locked"}
        else:
            scope = Scope(arguments.org_id, arguments.user_id, arguments.provider)
            volumes = CredentialVolumes(store, FixedToolRunner())
            if arguments.operation == "create":
                handle = volumes.create(scope)
                result = {"handle": handle, "state": "locked"}
            else:
                handle = arguments.handle
                if arguments.operation == "open":
                    volumes.open(scope, handle)
                    state = "open"
                elif arguments.operation == "close":
                    volumes.close(scope, handle)
                    state = "locked"
                else:
                    state = volumes.status(scope, handle)
                result = {"handle": handle, "state": state}
    except ValueError:
        result = {"error": "invalid"}
        exit_code = 2
    except StoreError as error:
        result = {"error": error.code if error.code in _SAFE_CODES else "unavailable"}
        exit_code = 1
    except Exception:
        result = {"error": "unavailable"}
        exit_code = 1
    return json.dumps(result, sort_keys=True, separators=(",", ":")), exit_code


def main() -> int:
    output, exit_code = run_cli(sys.argv[1:])
    sys.stdout.write(output + "\n")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
