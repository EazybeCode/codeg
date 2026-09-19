"""Authenticated local supervisor transport — Python adopted listener/session.

Composes the accepted wire framing (`neev_supervisor_wire`) and six-operation
DTO channels (`neev_supervisor_dto`) over a **pre-bound, already-listening**
AF_UNIX/SOCK_STREAM socket supplied by a trusted future launcher. It never
binds, chmods, chowns, unlinks, provisions a key or generates a service unit.

Scope (per `.context/neev-supervisor-transport-brief-proposal.md`): connect/adopt,
validate filesystem/peer/key inputs, authenticate frames and correlate typed
requests/responses. It decides no effect: the first request's coordinator tuple
is exposed as an *unregistered candidate*; a future registration owner approves it
before any dispatch. There is no registered/ready flag, effect handler, queue,
journal or seventh operation here.

Linux is the production target for real filesystem/`SO_PEERCRED` acceptance; other
platforms raise `unsupported` from the production adopter. The `_adopt_unchecked`
entry is a **test-only** seam (no env switch) that adopts an already-verified
socket to exercise the portable framing/handshake/channel/deadline logic.
"""

import asyncio
import hmac
import os
import platform
import secrets
import socket
import stat
import uuid
from dataclasses import dataclass

import neev_supervisor_dto as dto
import neev_supervisor_wire as wire

_MAGIC_LEN = 8
_CLIENT_HELLO_SIZE = 40
_SERVER_HELLO_SIZE = 120
_LEN_PREFIX = 4
_MAC_SIZE = 32
_REQUEST_PAYLOAD_MAX = 8192
_KEY_SIZE = 32

# Fixed deadlines (seconds), absolute-monotonic per phase; never reset per byte.
_HANDSHAKE_DEADLINE = 5.0
_EXCHANGE_DEADLINE = 10.0
_IDLE_DEADLINE = 10.0
_PHASE_DEADLINE = 5.0
_MAX_SESSIONS = 32

_KEY_MODE = 0o440
_SOCKET_MODE = 0o660


class TransportError(Exception):
    """Fixed, sanitized categories only; never carries a path/key/peer detail."""

    _CATEGORIES = frozenset(
        [
            "invalid",
            "auth",
            "peer",
            "metadata",
            "timeout",
            "closed",
            "busy",
            "io",
            "unsupported",
        ]
    )

    def __init__(self, category):
        if category not in self._CATEGORIES:
            category = "invalid"
        super().__init__(category)
        self.category = category

    def __repr__(self):
        return f"TransportError({self.category})"


@dataclass(frozen=True)
class FileIdentity:
    device: int
    inode: int


@dataclass(frozen=True)
class ProtectedParent:
    absolute_path: str
    identity: FileIdentity
    gid: int
    mode: int


@dataclass(frozen=True)
class ControlDeployment:
    socket_parent: ProtectedParent
    key_parent: ProtectedParent
    key_identity: FileIdentity
    control_gid: int
    self_uid: int
    self_gid: int
    peer_uid: int
    peer_gid: int
    socket_owner_uid: int


# ---- pure metadata policy (testable with invented snapshots) ----

_MAX_PATH_COMPONENTS = 32
_MAX_SOCKET_PATH_BYTES = 107


def validate_parent_path(path):
    """A protected parent path is absolute, normal, bounded; else metadata error."""
    if not isinstance(path, str) or not path.startswith("/"):
        raise TransportError("metadata")
    if "\x00" in path:
        raise TransportError("metadata")
    components = [c for c in path.split("/") if c != ""]
    if not components or len(components) > _MAX_PATH_COMPONENTS:
        raise TransportError("metadata")
    for component in components:
        if component in (".", ".."):
            raise TransportError("metadata")
    # Reject repeated/trailing separators by exact rejoin comparison.
    if "/" + "/".join(components) != path.rstrip("/") or path.endswith("/"):
        raise TransportError("metadata")
    return components


@dataclass(frozen=True)
class MetaSnapshot:
    """Local stat facts a caller reads; compared against trusted deployment input."""

    uid: int
    gid: int
    mode: int
    device: int
    inode: int
    nlink: int
    is_socket: bool = False
    is_regular: bool = False
    is_symlink: bool = False


def check_protected_parent(parent, snapshot):
    """Final parent must match trusted dev/inode, be UID0, exact GID/mode, no world/group write."""
    validate_parent_path(parent.absolute_path)
    if snapshot.is_symlink:
        raise TransportError("metadata")
    if (snapshot.device, snapshot.inode) != (
        parent.identity.device,
        parent.identity.inode,
    ):
        raise TransportError("metadata")
    if snapshot.uid != 0 or snapshot.gid != parent.gid:
        raise TransportError("metadata")
    if snapshot.mode != parent.mode or snapshot.mode & 0o022:
        raise TransportError("metadata")


def check_key_file(deployment, snapshot, byte_len):
    """Key file: regular, nlink1, exactly 32 bytes, UID0/control_gid/mode0440, exact identity."""
    if snapshot.is_symlink or not snapshot.is_regular or snapshot.nlink != 1:
        raise TransportError("metadata")
    if byte_len != _KEY_SIZE:
        raise TransportError("metadata")
    if snapshot.uid != 0 or snapshot.gid != deployment.control_gid:
        raise TransportError("metadata")
    if stat.S_IMODE(snapshot.mode) != _KEY_MODE:
        raise TransportError("metadata")
    if (snapshot.device, snapshot.inode) != (
        deployment.key_identity.device,
        deployment.key_identity.inode,
    ):
        raise TransportError("metadata")


def check_published_socket(deployment, snapshot):
    """Published socket: filesystem socket, nlink1, exact owner/gid/mode0660."""
    if snapshot.is_symlink or not snapshot.is_socket or snapshot.nlink != 1:
        raise TransportError("metadata")
    if snapshot.uid != deployment.socket_owner_uid or snapshot.gid != deployment.control_gid:
        raise TransportError("metadata")
    if stat.S_IMODE(snapshot.mode) != _SOCKET_MODE:
        raise TransportError("metadata")


def check_peer_credentials(deployment, pid, uid, gid):
    """SO_PEERCRED: complete, PID>0, exact expected UID/GID; else peer error."""
    if pid is None or uid is None or gid is None or pid <= 0:
        raise TransportError("peer")
    if uid != deployment.peer_uid or gid != deployment.peer_gid:
        raise TransportError("peer")


def _monotonic_deadline(seconds):
    return asyncio.get_event_loop().time() + seconds


def _remaining(deadline):
    remaining = deadline - asyncio.get_event_loop().time()
    if remaining <= 0:
        raise TransportError("timeout")
    return remaining


async def _read_exact(sock, count, deadline):
    loop = asyncio.get_event_loop()
    buffer = bytearray()
    while len(buffer) < count:
        try:
            chunk = await asyncio.wait_for(
                loop.sock_recv(sock, count - len(buffer)), _remaining(deadline)
            )
        except asyncio.TimeoutError:
            raise TransportError("timeout")
        except (OSError, ConnectionError):
            raise TransportError("io")
        if not chunk:
            raise TransportError("io")
        buffer.extend(chunk)
    return bytes(buffer)


async def _write_all(sock, data, deadline):
    loop = asyncio.get_event_loop()
    try:
        await asyncio.wait_for(loop.sock_sendall(sock, data), _remaining(deadline))
    except asyncio.TimeoutError:
        raise TransportError("timeout")
    except (OSError, ConnectionError):
        raise TransportError("io")


async def _read_frame(sock, deadline):
    """Read [4-byte len][payload][32 MAC]; reject bad length before body allocation."""
    header = await _read_exact(sock, _LEN_PREFIX, deadline)
    declared = int.from_bytes(header, "big")
    if declared == 0 or declared > _REQUEST_PAYLOAD_MAX:
        raise TransportError("invalid")
    body = await _read_exact(sock, declared + _MAC_SIZE, deadline)
    return header + body


class PendingRequest:
    """A validated request awaiting exactly one response, bound to its session.

    Not constructible through any exported factory; the session mints it and
    retains the exact authenticated request payload bytes for response-MAC
    binding. It carries no registration assertion.
    """

    __slots__ = ("_request", "_candidate", "_payload", "_session")

    def __init__(self, session, request, candidate, payload):
        self._session = session
        self._request = request
        self._candidate = candidate
        self._payload = payload

    @property
    def request(self):
        return self._request

    @property
    def coordinator_candidate(self):
        return self._candidate


class ServerSession:
    """One accepted connection: handshake, then a single outstanding request."""

    def __init__(self, listener, conn):
        self._listener = listener
        self._conn = conn
        self._context = None
        self._channel = None
        self._pending = None
        self._pending_payload = None
        self._closed = False
        self._deadline_handle = None

    async def _handshake(self):
        deadline = _monotonic_deadline(_HANDSHAKE_DEADLINE)
        hello = await _read_exact(self._conn, _CLIENT_HELLO_SIZE, deadline)
        try:
            client_nonce = wire.decode_client_hello(hello)
        except wire.WireError:
            raise TransportError("invalid")
        challenge = secrets.token_bytes(32)
        context = wire.WireContext(client_nonce, challenge, self._listener._boot_bytes)
        try:
            server_hello = wire.encode_server_hello(self._listener._key, context)
        except wire.WireError:
            raise TransportError("invalid")
        await _write_all(self._conn, server_hello, deadline)
        self._context = context

    def _arm(self, seconds):
        self._cancel_deadline()
        loop = asyncio.get_event_loop()
        self._deadline_handle = loop.call_at(loop.time() + seconds, self._on_deadline)

    def _cancel_deadline(self):
        if self._deadline_handle is not None:
            self._deadline_handle.cancel()
            self._deadline_handle = None

    def _on_deadline(self):
        # Bounds idle/pending lifetime even if the caller never calls back.
        self._deadline_handle = None
        self.close()

    async def receive(self):
        if self._closed:
            raise TransportError("closed")
        if self._pending is not None:
            raise TransportError("busy")
        self._arm(_IDLE_DEADLINE)
        deadline = _monotonic_deadline(_EXCHANGE_DEADLINE)
        frame = await _read_frame(self._conn, deadline)
        try:
            payload = wire.decode_request(self._listener._key, self._context, frame)
            request = dto.decode_request(payload)
        except wire.WireError as error:
            self._poison()
            raise TransportError("auth" if error.args[0] == "auth" else "invalid")
        except dto.DtoError:
            self._poison()
            raise TransportError("invalid")
        candidate = request.coordinator
        if self._channel is None:
            # Channel pinning from the first request's candidate + listener boot.
            self._channel = dto.ServerChannel(candidate, self._listener.supervisor_boot_id)
        try:
            self._channel.accept(request)
        except dto.DtoError:
            self._poison()
            raise TransportError("invalid")
        self._pending = PendingRequest(self, request, candidate, payload)
        self._pending_payload = payload
        self._arm(_IDLE_DEADLINE)
        return self._pending

    async def respond(self, pending, response):
        if self._closed:
            raise TransportError("closed")
        if pending is not self._pending or self._pending is None:
            raise TransportError("invalid")
        try:
            self._channel.finish(response)
        except dto.DtoError:
            self._poison()
            raise TransportError("invalid")
        try:
            response_payload = dto.encode_response(response)
            frame = wire.encode_response(
                self._listener._key, self._context, self._pending_payload, response_payload
            )
        except (dto.DtoError, wire.WireError):
            self._poison()
            raise TransportError("invalid")
        deadline = _monotonic_deadline(_PHASE_DEADLINE)
        try:
            await _write_all(self._conn, frame, deadline)
        except TransportError:
            self._poison()
            raise
        self._pending = None
        self._pending_payload = None
        self._arm(_IDLE_DEADLINE)

    def _poison(self):
        if self._channel is not None:
            self._channel.poison()
        self.close()

    def close(self):
        if self._closed:
            return
        self._closed = True
        self._cancel_deadline()
        self._pending = None
        self._pending_payload = None
        if self._channel is not None:
            self._channel.poison()
        try:
            self._conn.close()
        except OSError:
            pass
        self._listener._forget(self)

    async def aclose(self):
        self.close()


class SupervisorListener:
    """Adopts one pre-bound listening socket; accepts bounded sessions."""

    def __init__(self, sock, key, deployment):
        self._sock = sock
        self._key = key
        self._deployment = deployment
        self._boot_uuid = uuid.uuid4()
        self._boot_bytes = self._boot_uuid.bytes
        # 32 lowercase hex (no hyphens), the canonical dto.Id form; the same UUID
        # supplies the raw 16 wire bytes.
        self._boot_id = dto.Id(self._boot_uuid.hex)
        self._sessions = set()
        self._closed = False
        self._accepting = False

    @property
    def supervisor_boot_id(self):
        return self._boot_id

    @classmethod
    def _adopt_unchecked(cls, bound_socket, key, deployment=None):
        """Test-only adoption: the socket/key are already verified by the caller.

        Physically test-only — production callers use `adopt`, which performs the
        Linux filesystem/peer checks. No environment switch selects this path.
        """
        if not isinstance(key, (bytes, bytearray)) or len(key) != _KEY_SIZE:
            raise TransportError("metadata")
        _require_listening_socket(bound_socket)
        return cls(bound_socket, bytes(key), deployment)

    @classmethod
    def adopt(cls, bound_socket, deployment):
        """Production adoption (Linux only). Other platforms are unsupported."""
        if platform.system() != "Linux":
            raise TransportError("unsupported")
        if not isinstance(deployment, ControlDeployment):
            raise TransportError("invalid")
        if os.geteuid() != deployment.self_uid or os.getegid() != deployment.self_gid:
            raise TransportError("metadata")
        _require_listening_socket(bound_socket)
        key = _load_verified_key(deployment)
        return cls(bound_socket, key, deployment)

    async def accept(self):
        if self._closed:
            raise TransportError("closed")
        if self._accepting:
            raise TransportError("busy")
        if len(self._sessions) >= _MAX_SESSIONS:
            raise TransportError("busy")
        self._accepting = True
        loop = asyncio.get_event_loop()
        try:
            conn, _ = await loop.sock_accept(self._sock)
        except (OSError, ConnectionError):
            raise TransportError("io")
        finally:
            self._accepting = False
        conn.setblocking(False)
        if self._deployment is not None and platform.system() == "Linux":
            try:
                _check_peer(conn, self._deployment)
            except TransportError:
                conn.close()
                raise
        session = ServerSession(self, conn)
        self._sessions.add(session)
        try:
            await session._handshake()
        except TransportError:
            session.close()
            raise
        return session

    def _forget(self, session):
        self._sessions.discard(session)

    def close(self):
        if self._closed:
            return
        self._closed = True
        for session in list(self._sessions):
            session.close()
        try:
            self._sock.close()
        except OSError:
            pass

    async def aclose(self):
        self.close()


def _require_listening_socket(sock):
    if not isinstance(sock, socket.socket):
        raise TransportError("metadata")
    if sock.family != socket.AF_UNIX or sock.type != socket.SOCK_STREAM:
        raise TransportError("metadata")
    try:
        accepting = sock.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN)
    except OSError:
        # SO_ACCEPTCONN is not queryable on every platform (e.g. darwin used for
        # portable tests). Where it is unavailable we cannot assert it here; the
        # Linux production path (where it is available) still rejects a
        # non-listening socket below.
        accepting = None
    if accepting == 0:
        raise TransportError("metadata")
    sock.setblocking(False)


def _load_verified_key(deployment):  # pragma: no cover - Linux production path
    parent = deployment.key_parent
    fd = os.open(parent.absolute_path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        key_fd = os.open(
            "control.key",
            os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
            dir_fd=fd,
        )
    finally:
        os.close(fd)
    try:
        info = os.fstat(key_fd)
        snapshot = MetaSnapshot(
            uid=info.st_uid,
            gid=info.st_gid,
            mode=info.st_mode,
            device=info.st_dev,
            inode=info.st_ino,
            nlink=info.st_nlink,
            is_regular=stat.S_ISREG(info.st_mode),
            is_symlink=stat.S_ISLNK(info.st_mode),
        )
        data = os.read(key_fd, _KEY_SIZE + 1)
        check_key_file(deployment, snapshot, len(data))
    finally:
        os.close(key_fd)
    return data


def _check_peer(conn, deployment):  # pragma: no cover - Linux production path
    raw = conn.getsockopt(
        socket.SOL_SOCKET, socket.SO_PEERCRED, struct_ucred_size()
    )
    pid, uid, gid = _parse_ucred(raw)
    check_peer_credentials(deployment, pid, uid, gid)


def struct_ucred_size():  # pragma: no cover - Linux production path
    import struct

    return struct.calcsize("3i")


def _parse_ucred(raw):  # pragma: no cover - Linux production path
    import struct

    return struct.unpack("3i", raw)
