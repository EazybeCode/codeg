#!/usr/bin/env python3
"""Build and provision the supervisor's protected filesystem deployment.

`neev_agent_supervisor_main` needs two trusted descriptions before it can serve:
a `neev_supervisor_fence.FenceDeployment` and a
`neev_supervisor_transport.ControlDeployment`. Both are *trusted input* — the
fence and the transport re-stat everything and refuse anything that does not
match exactly — so this module's job is to put the real files on disk and then
describe what is actually there. It never invents an identity: every dev/inode
is read back from the provisioned file.

Two directories, deliberately separate:

  * the **fence directory** is supervisor-private (root:root 0700) and holds
    `coordinator-fence.lock` and `coordinator-fence.record`, both root:root 0600.
    Nothing but the supervisor may read, let alone write, the epoch record.
  * the **IPC directory** is shared with the coordinator's group
    (root:<control_gid> 0750) and holds `control.key` (root:<control_gid> 0440)
    and, once the daemon binds it, `control.sock`. The group can traverse and
    connect; it can write neither.

The web server owns its own coordinator epoch files elsewhere; nothing here
touches them.

Two rules run through all of it:

  * **Create-only.** An existing key, record or lock is adopted after
    verification and NEVER rewritten. Re-provisioning a live deployment must not
    roll the epoch back or mint a new MAC key out from under a running peer.
  * **Never trust a symlink.** Every directory is opened `O_NOFOLLOW|O_DIRECTORY`
    and every file is created and opened relative to that descriptor, so a
    swapped component is a failure rather than a redirection.

Production defaults match the deployed service (supervisor root 0:0, control
group 65532, coordinator peer 65532:65532); every id and mode is a parameter so
the whole module is exercisable as an unprivileged user in tests.
"""

from __future__ import annotations

import hashlib
import os
import secrets
import stat as stat_module
from dataclasses import dataclass, replace
from typing import Optional

import neev_supervisor_fence as fence_mod
import neev_supervisor_transport as transport


# The coordinator (web server) runs as this uid:gid and is the ONLY peer the
# transport will accept on the control socket.
DEFAULT_PEER_UID = 65532
DEFAULT_PEER_GID = 65532
# The group that may traverse the IPC directory and read the MAC key.
DEFAULT_CONTROL_GID = 65532

FENCE_LOCK_BASENAME = "coordinator-fence.lock"
FENCE_RECORD_BASENAME = "coordinator-fence.record"
KEY_BASENAME = "control.key"
SOCKET_BASENAME = "control.sock"

# Mirrors of the policies the fence and the transport enforce. They are stated
# here so provisioning writes exactly what verification will demand; if either
# library moves, `verify()` fails loudly rather than this module drifting.
FENCE_DIRECTORY_MODE = 0o700
FENCE_FILE_MODE = 0o600
IPC_DIRECTORY_MODE = 0o750
KEY_MODE = 0o440
KEY_SIZE = 32


class DeploymentError(Exception):
    """A deployment that cannot be trusted. Never carries a path or key."""

    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


@dataclass(frozen=True)
class DeploymentPolicy:
    """Who owns the deployment. Defaults are the production service."""

    supervisor_uid: int = 0
    supervisor_gid: int = 0
    control_gid: int = DEFAULT_CONTROL_GID
    peer_uid: int = DEFAULT_PEER_UID
    peer_gid: int = DEFAULT_PEER_GID
    fence_directory_mode: int = FENCE_DIRECTORY_MODE
    ipc_directory_mode: int = IPC_DIRECTORY_MODE

    def for_current_process(self) -> "DeploymentPolicy":
        """The same policy owned by whoever is running. Test/dev convenience:
        production runs as root and needs no substitution."""
        return replace(
            self,
            supervisor_uid=os.geteuid(),
            supervisor_gid=os.getegid(),
            control_gid=os.getegid(),
        )


@dataclass(frozen=True)
class DeploymentPaths:
    """Absolute paths of the two protected directories."""

    fence_directory: str
    ipc_directory: str


def _fail(reason: str):
    raise DeploymentError(reason)


def validate_absolute_path(path: str) -> None:
    """Absolute, normal and NUL-free — checked BEFORE any syscall.

    Not merely defensive: `os.mkdir` raises `ValueError` rather than `OSError`
    for an embedded NUL, so an unvalidated path would escape the filesystem
    error handling entirely.
    """
    if not isinstance(path, str) or not path.startswith("/") or path == "/":
        _fail("invalid")
    if "\x00" in path or "//" in path or path.endswith("/"):
        _fail("invalid")
    components = [part for part in path.split("/") if part]
    if not components or any(part in (".", "..") for part in components):
        _fail("invalid")


def _open_directory(path: str) -> int:
    """Open a directory without following a symlink at any component.

    The final component is opened `O_NOFOLLOW` directly; the ancestors are
    walked one at a time from `/` so a symlinked parent cannot silently
    redirect the whole lookup.
    """
    validate_absolute_path(path)
    components = [part for part in path.split("/") if part]
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for component in components:
            nxt = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=fd,
            )
            os.close(fd)
            fd = nxt
    except OSError:
        os.close(fd)
        _fail("metadata")
    return fd


def _identity(info) -> tuple:
    return (info.st_dev, info.st_ino)


def _ensure_directory(path: str, uid: int, gid: int, mode: int) -> None:
    """Create the directory if absent, then hold it to the declared owner/mode.

    An existing directory is never chowned or chmodded into compliance: a
    deployment that does not already match is a provisioning error a human
    should look at, not something to paper over.
    """
    validate_absolute_path(path)
    created = True
    try:
        os.mkdir(path, mode)
    except FileExistsError:
        created = False
    except OSError:
        _fail("metadata")
    fd = _open_directory(path)
    try:
        info = os.fstat(fd)
        if not stat_module.S_ISDIR(info.st_mode):
            _fail("metadata")
        if created:
            # `mkdir`'s mode is masked by umask and the new directory inherits
            # the caller's gid, so settle both here. Only ever for a directory
            # this call just made.
            try:
                os.fchmod(fd, mode)
                os.fchown(fd, uid, gid)
            except OSError:
                _fail("owner")
            info = os.fstat(fd)
        # A pre-existing directory is held to the policy, never corrected into
        # it: a live deployment whose ownership or mode has drifted is
        # something to look at, not something to quietly widen.
        if (
            stat_module.S_IMODE(info.st_mode) != mode
            or (info.st_uid, info.st_gid) != (uid, gid)
            or info.st_mode & 0o022
        ):
            _fail("metadata")
    finally:
        os.close(fd)


def _create_only(
    parent_fd: int, name: str, content: bytes, uid: int, gid: int, mode: int
) -> None:
    """Create `name` with `content` if it does not exist; otherwise leave it.

    `O_EXCL|O_NOFOLLOW` means this can neither follow a planted symlink nor
    clobber an existing file — which is the whole point for a MAC key and an
    epoch record.
    """
    try:
        fd = os.open(
            name,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            mode,
            dir_fd=parent_fd,
        )
    except FileExistsError:
        return
    except OSError:
        _fail("metadata")
    try:
        if content:
            written = os.write(fd, content)
            if written != len(content):
                _fail("metadata")
        os.fchmod(fd, mode)
        try:
            os.fchown(fd, uid, gid)
        except OSError:
            _fail("owner")
        os.fsync(fd)
    finally:
        os.close(fd)


def _zero_fence_record() -> bytes:
    """The record a never-registered fence must contain.

    Built from the fence module's own public constants so it cannot drift:
    marker, epoch 0, the zero boot id, and the body digest the parser checks.
    Epoch 0 is only accepted alongside the zero boot id, which is exactly the
    "no coordinator has ever registered" state.
    """
    body = fence_mod.MARKER + (0).to_bytes(8, "big") + b"\x00" * 16
    record = body + hashlib.sha256(body).digest()
    if len(record) != fence_mod.RECORD_SIZE:
        _fail("invalid")
    return record


def provision(
    paths: DeploymentPaths, policy: Optional[DeploymentPolicy] = None
) -> None:
    """Put the deployment on disk. Idempotent, and create-only for every secret.

    Safe to run on every service start: an already-provisioned deployment is
    left exactly as it is, including its epoch record and its MAC key.
    """
    policy = policy or DeploymentPolicy()

    # Fence: supervisor-private. The record holds the coordinator epoch, so the
    # control group must not even be able to read it.
    _ensure_directory(
        paths.fence_directory,
        policy.supervisor_uid,
        policy.supervisor_gid,
        policy.fence_directory_mode,
    )
    fence_fd = _open_directory(paths.fence_directory)
    try:
        _create_only(
            fence_fd,
            FENCE_LOCK_BASENAME,
            b"",
            policy.supervisor_uid,
            policy.supervisor_gid,
            FENCE_FILE_MODE,
        )
        _create_only(
            fence_fd,
            FENCE_RECORD_BASENAME,
            _zero_fence_record(),
            policy.supervisor_uid,
            policy.supervisor_gid,
            FENCE_FILE_MODE,
        )
    finally:
        os.close(fence_fd)

    # IPC: shared with the coordinator's group, which may traverse and read the
    # key but write nothing.
    _ensure_directory(
        paths.ipc_directory,
        policy.supervisor_uid,
        policy.control_gid,
        policy.ipc_directory_mode,
    )
    ipc_fd = _open_directory(paths.ipc_directory)
    try:
        _create_only(
            ipc_fd,
            KEY_BASENAME,
            secrets.token_bytes(KEY_SIZE),
            policy.supervisor_uid,
            policy.control_gid,
            KEY_MODE,
        )
    finally:
        os.close(ipc_fd)


def _stat_file(parent_fd: int, name: str, expect_regular: bool = True):
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent_fd)
    except OSError:
        _fail("metadata")
    try:
        info = os.fstat(fd)
    finally:
        os.close(fd)
    if expect_regular and not stat_module.S_ISREG(info.st_mode):
        _fail("metadata")
    if info.st_nlink != 1:
        _fail("metadata")
    return info


def fence_deployment(
    paths: DeploymentPaths, policy: Optional[DeploymentPolicy] = None
) -> "fence_mod.FenceDeployment":
    """Describe the provisioned fence directory for `FenceOwner.open`.

    Every identity is read back from the real file, so the description cannot
    disagree with what the fence will re-stat.
    """
    policy = policy or DeploymentPolicy()
    fd = _open_directory(paths.fence_directory)
    try:
        parent = os.fstat(fd)
        if not stat_module.S_ISDIR(parent.st_mode):
            _fail("metadata")
        lock = _stat_file(fd, FENCE_LOCK_BASENAME)
        record = _stat_file(fd, FENCE_RECORD_BASENAME)
    finally:
        os.close(fd)
    if _identity(lock) == _identity(record):
        _fail("metadata")
    if lock.st_size != 0 or record.st_size != fence_mod.RECORD_SIZE:
        _fail("metadata")
    for info in (lock, record):
        if (info.st_uid, info.st_gid) != (
            policy.supervisor_uid,
            policy.supervisor_gid,
        ):
            _fail("owner")
        if stat_module.S_IMODE(info.st_mode) != FENCE_FILE_MODE:
            _fail("metadata")
    deployment = fence_mod.FenceDeployment(
        parent_absolute_path=paths.fence_directory,
        parent_identity=fence_mod.Identity(parent.st_dev, parent.st_ino),
        protected_directory_uid=parent.st_uid,
        parent_gid=parent.st_gid,
        parent_mode=stat_module.S_IMODE(parent.st_mode),
        supervisor_uid=policy.supervisor_uid,
        supervisor_gid=policy.supervisor_gid,
        lock_identity=fence_mod.Identity(lock.st_dev, lock.st_ino),
        record_identity=fence_mod.Identity(record.st_dev, record.st_ino),
    )
    # The directory must be owned and moded as declared, or the fence will
    # refuse it at open() and the daemon would fail later instead of here.
    if (parent.st_uid, parent.st_gid) != (
        policy.supervisor_uid,
        policy.supervisor_gid,
    ):
        _fail("owner")
    if stat_module.S_IMODE(parent.st_mode) != policy.fence_directory_mode:
        _fail("metadata")
    return deployment


def control_deployment(
    paths: DeploymentPaths, policy: Optional[DeploymentPolicy] = None
) -> "transport.ControlDeployment":
    """Describe the provisioned IPC directory for `SupervisorListener.adopt`.

    The socket itself is NOT required to exist: the daemon binds and publishes
    it. Only the directory and the key are described here, both read back from
    disk and then run through the transport's own checkers.
    """
    policy = policy or DeploymentPolicy()
    fd = _open_directory(paths.ipc_directory)
    try:
        parent = os.fstat(fd)
        if not stat_module.S_ISDIR(parent.st_mode):
            _fail("metadata")
        key = _stat_file(fd, KEY_BASENAME)
        key_size = key.st_size
    finally:
        os.close(fd)
    if key_size != KEY_SIZE:
        _fail("metadata")
    protected = transport.ProtectedParent(
        absolute_path=paths.ipc_directory,
        identity=transport.FileIdentity(parent.st_dev, parent.st_ino),
        gid=parent.st_gid,
        # `check_protected_parent` compares this against the raw `st_mode`, so
        # the raw value is what belongs here — not the permission bits alone.
        mode=parent.st_mode,
    )
    deployment = transport.ControlDeployment(
        socket_parent=protected,
        key_parent=protected,
        key_identity=transport.FileIdentity(key.st_dev, key.st_ino),
        control_gid=policy.control_gid,
        self_uid=policy.supervisor_uid,
        self_gid=policy.supervisor_gid,
        peer_uid=policy.peer_uid,
        peer_gid=policy.peer_gid,
        socket_owner_uid=policy.supervisor_uid,
    )
    verify_control(deployment, parent, key)
    return deployment


def _snapshot(info, *, is_regular=False, is_socket=False):
    return transport.MetaSnapshot(
        uid=info.st_uid,
        gid=info.st_gid,
        mode=info.st_mode,
        device=info.st_dev,
        inode=info.st_ino,
        nlink=info.st_nlink,
        is_socket=is_socket,
        is_regular=is_regular,
        is_symlink=False,
    )


def verify_control(deployment, parent_info, key_info) -> None:
    """Run the transport's OWN validators over what is on disk.

    Re-implementing the policy here would let the two drift; borrowing the
    library's checkers means a change in `neev_supervisor_transport` surfaces as
    a provisioning failure instead of a runtime refusal.

    Note that those checkers require the IPC directory and the key to be owned
    by **uid 0** specifically — that is hardcoded in the transport, not a policy
    knob — so `control_deployment` only succeeds for a root-owned deployment.
    Unprivileged callers get `DeploymentError("metadata")`, which is the honest
    answer: the transport would refuse the same deployment at adopt time.
    """
    try:
        transport.check_protected_parent(deployment.key_parent, _snapshot(parent_info))
        transport.check_key_file(
            deployment, _snapshot(key_info, is_regular=True), key_info.st_size
        )
    except transport.TransportError as error:
        raise DeploymentError(str(error)) from error


def socket_path(paths: DeploymentPaths) -> str:
    """Where the daemon should bind and publish its control socket."""
    return paths.ipc_directory.rstrip("/") + "/" + SOCKET_BASENAME


def from_config(config) -> tuple:
    """`(DeploymentPaths, DeploymentPolicy)` for a `neev_agent_supervisor_main.Config`.

    The daemon's config carries full paths; this derives the two protected
    directories and checks the things the transport silently assumes:

      * the socket and the key must live in the SAME directory — the transport
        describes both with one `ProtectedParent`, so split directories would
        validate one and use the other;
      * the key's basename must be `control.key`, because `_load_verified_key`
        opens that name literally and would otherwise find nothing.

    Duck-typed on purpose: the daemon owns `Config`, and importing it here would
    make this module depend on the process it exists to serve.
    """
    key_path = getattr(config, "key_path", None)
    socket = getattr(config, "socket_path", None)
    fence_dir = getattr(config, "fence_dir", None)
    if not isinstance(key_path, str) or not isinstance(socket, str):
        _fail("invalid")
    if not isinstance(fence_dir, str):
        _fail("invalid")
    validate_absolute_path(key_path)
    validate_absolute_path(socket)
    validate_absolute_path(fence_dir)
    ipc_directory = os.path.dirname(key_path)
    if os.path.dirname(socket) != ipc_directory:
        _fail("invalid")
    if os.path.basename(key_path) != KEY_BASENAME:
        _fail("invalid")
    if os.path.basename(socket) != SOCKET_BASENAME:
        _fail("invalid")
    if fence_dir == ipc_directory:
        # The fence record must not be readable by the control group.
        _fail("invalid")
    policy = DeploymentPolicy(
        control_gid=int(getattr(config, "control_gid")),
        peer_uid=int(getattr(config, "peer_uid")),
        peer_gid=int(getattr(config, "peer_gid")),
    )
    return DeploymentPaths(fence_directory=fence_dir, ipc_directory=ipc_directory), policy
