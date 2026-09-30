#!/usr/bin/env python3
"""Root-side broker: serve evicted Neev originals from the Storage Box.

The unprivileged app never holds Storage Box credentials. It talks to this
broker over a Unix socket authenticated by peer uid. Every request names one
blob by (org_id, uuid key, kind); the broker derives both paths itself (fixed
remote root, fixed local document root). Nothing else can be addressed.

Protocol: one JSON line per connection.
  {"v":1,"op":"ping"}                                   root or app
  {"v":1,"op":"hydrate", <blob>, "sha256":h, "byte_length":n}   app only
      Restore an EVICTED original (ledger entry with evicted_at, same hash and
      size, not forgotten): fetch, verify, publish with no replace.
  {"v":1,"op":"stream", <blob>, "sha256":h, "byte_length":n}    app only
      Send verified-by-ledger remote bytes to the caller (content backup)
      without publishing them into the live root. The caller re-hashes.
  {"v":1,"op":"forget", <blob>}                          app only
      Tombstone a blob the app deleted (after its row is gone) so it can never
      be hydrated, streamed or re-replicated; `purge` later removes the remote.
  <blob> = "org_id":int, "key":uuid, "kind":"md"|"img"|"att"
Reply: {"ok":true,"result":"pong"|"present"|"fetched"|"forgotten"} or
       {"ok":true,"result":"stream","byte_length":n} followed by n raw bytes, or
       {"ok":false,"error":CODE}
CODE: invalid | forbidden | not_verified | forgotten | local_mismatch |
      remote_mismatch | unavailable | busy
"""
import argparse
import ctypes
import ctypes.util
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import socketserver
import stat
import struct
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import neev_storage_box as box  # noqa: E402  (installed side by side)

KEY = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
SHA = re.compile(r"^[0-9a-f]{64}$")
FETCH_TEMP = re.compile(r"^[0-9a-f-]{36}\.(md|img|att)\.fetch-[0-9a-f]{16}\.tmp$")
KINDS = ("md", "img", "att")
REQUEST_LIMIT = 1024
CHUNK = 1 << 20
READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_NOATIME", 0)


class BrokerError(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def peer_uid(connection):
    if hasattr(socket, "SO_PEERCRED"):  # Linux
        data = connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
        return struct.unpack("3i", data)[1]
    # macOS/BSD: SOL_LOCAL=0, LOCAL_PEERCRED=1 -> struct xucred {version, uid, ...}
    data = connection.getsockopt(0, 1, 76)
    return struct.unpack_from("II", data)[1]


def parse_request(raw):
    try:
        request = json.loads(raw)
    except ValueError:
        raise BrokerError("invalid") from None
    if not isinstance(request, dict) or request.get("v") != 1:
        raise BrokerError("invalid")
    op = request.get("op")
    if op == "ping" and set(request) == {"v", "op"}:
        return request
    blob = {"v", "op", "org_id", "key", "kind"}
    expected = {"forget": blob, "hydrate": blob | {"sha256", "byte_length"},
                "stream": blob | {"sha256", "byte_length"}}.get(op)
    org = request.get("org_id")
    if (
        expected is None or set(request) != expected
        or type(org) is not int or not 1 <= org <= 2147483647
        or not isinstance(request["key"], str) or not KEY.match(request["key"])
        or request["kind"] not in KINDS
    ):
        raise BrokerError("invalid")
    if op != "forget":
        length = request["byte_length"]
        if (not isinstance(request["sha256"], str) or not SHA.match(request["sha256"])
                or type(length) is not int or not 0 <= length <= (1 << 40)):
            raise BrokerError("invalid")
    return request


def open_directory(path):
    """Open an absolute directory one component at a time, refusing symlinks."""
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    fd = os.open("/", flags)
    try:
        for part in Path(path).parts[1:]:
            child = os.open(part, flags, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except OSError:
        os.close(fd)
        raise


def _hash_fd(fd, limit):
    digest, size = hashlib.sha256(), 0
    while True:
        chunk = os.read(fd, CHUNK)
        if not chunk:
            return digest.hexdigest(), size
        size += len(chunk)
        if size > limit:
            return None, size
        digest.update(chunk)


_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)


def publish_noreplace(directory, temporary, name):
    """Atomically rename temporary -> name in one directory, never replacing.

    A rename (not link + unlink) means the final name never has two links, so
    readers and scanners that require st_nlink == 1 never see a window.
    Raises FileExistsError if name exists.
    """
    old, new = os.fsencode(temporary), os.fsencode(name)
    if hasattr(_libc, "renameat2"):  # Linux: RENAME_NOREPLACE = 1
        result = _libc.renameat2(directory, old, directory, new, 1)
    elif hasattr(_libc, "renameatx_np"):  # macOS: RENAME_EXCL = 4
        result = _libc.renameatx_np(directory, old, directory, new, 4)
    else:  # no atomic primitive: link without following symlinks, then unlink
        os.link(temporary, name, src_dir_fd=directory, dst_dir_fd=directory, follow_symlinks=False)
        os.unlink(temporary, dir_fd=directory)
        return
    if result != 0:
        code = ctypes.get_errno()
        if code == errno.EEXIST:
            raise FileExistsError(code, os.strerror(code))
        raise OSError(code, os.strerror(code))


class CachedJson:
    """A state file re-read only when its inode, size or mtime changes."""

    def __init__(self, path, field):
        self.path, self.field = path, field
        self.identity, self.value = None, {}

    def get(self):
        try:
            info = os.stat(self.path)
        except FileNotFoundError:
            self.identity, self.value = None, {}
            return self.value
        identity = (info.st_ino, info.st_size, info.st_mtime_ns)
        if identity != self.identity:
            with open(self.path, encoding="utf-8") as handle:
                self.value = json.load(handle)[self.field]
            self.identity = identity
        return self.value


class Broker:
    def __init__(self, config, fetch_argv=None):
        self.config = config
        self.settings = config["broker"]
        self.fetch_argv = fetch_argv or self._ssh_cat
        self.locks = {}
        self.locks_guard = threading.Lock()
        self.tombstone_guard = threading.Lock()
        self.slots = threading.BoundedSemaphore(self.settings.get("max_parallel", 4))
        self.queue_timeout = self.settings.get("queue_timeout_seconds", 60)
        self.ledger = CachedJson(os.path.join(config["state_dir"], "verified.json"), "verified")
        self.tombstones = CachedJson(box.tombstone_path(config), "forgotten")

    def _ssh_cat(self, relative):
        return box.remote_command(self.config, ["cat", f"{self.config['remote_root']}/{relative}"])

    def _lock(self, name):
        with self.locks_guard:
            return self.locks.setdefault(name, threading.Lock())

    def _entry(self, relative, request):
        if relative in self.tombstones.get():
            raise BrokerError("forgotten")
        entry = self.ledger.get().get(relative)
        if not entry or entry["sha256"] != request["sha256"] or entry["size"] != request["byte_length"]:
            raise BrokerError("not_verified")
        return entry

    def _slot(self):
        # Bounded queue: wait for a transfer slot rather than failing at once.
        if not self.slots.acquire(timeout=self.queue_timeout):
            raise BrokerError("busy")

    def handle(self, request, uid):
        if request["op"] == "ping":
            # Root may health-check (the evictor does); only the app may act.
            if uid not in (self.settings["client_uid"], 0):
                raise BrokerError("forbidden")
            return "pong"
        if uid != self.settings["client_uid"]:
            raise BrokerError("forbidden")
        name = f"{request['key']}.{request['kind']}"
        relative = f"{request['org_id']}/{name}"
        if request["op"] == "forget":
            # Same per-blob lock as hydrate: a forget waits for an in-flight
            # fetch to publish (or fail), so the app's unlink that follows the
            # forget always removes what was published. It can never land after.
            with self._lock(relative), box.blob_lock(self.config, relative):
                return self._forget(relative)
        entry = self._entry(relative, request)
        if request["op"] == "stream":
            return ("stream", relative, request)
        # hydrate: only an original the evictor removed comes back. A blob that
        # was never evicted and is missing is lost locally: an operator matter.
        if not entry.get("evicted_at"):
            raise BrokerError("not_verified")
        # Thread lock plus the cross-process lock the evictor also takes.
        with self._lock(relative), box.blob_lock(self.config, relative):
            if relative in self.tombstones.get():  # forgotten while queued
                raise BrokerError("forgotten")
            return self._hydrate(request, name, relative)

    def _forget(self, relative):
        with self.tombstone_guard:
            forgotten = dict(self.tombstones.get())
            if relative not in forgotten:
                forgotten[relative] = {"at": int(time.time())}
                box.save_json(box.tombstone_path(self.config), {"version": 1, "forgotten": forgotten})
        return "forgotten"

    def _hydrate(self, request, name, relative):
        root = open_directory(self.config["local_root"])
        try:
            owner = os.fstat(root)
            if owner.st_uid != self.settings["client_uid"] or owner.st_mode & 0o7777 != 0o700:
                raise BrokerError("unavailable")
            org_name = str(request["org_id"])
            try:
                os.mkdir(org_name, 0o700, dir_fd=root)
                os.chown(org_name, owner.st_uid, owner.st_gid, dir_fd=root, follow_symlinks=False)
                os.fsync(root)
            except FileExistsError:
                pass
            org = os.open(org_name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root)
        finally:
            os.close(root)
        try:
            info = os.fstat(org)
            if info.st_uid != owner.st_uid or info.st_mode & 0o7777 != 0o700:
                raise BrokerError("unavailable")
            # Checked under the per-blob lock: concurrent requests for one blob
            # wait here and then find it present, without taking a slot.
            if self._existing(org, name, request):
                return "present"
            self._slot()
            try:
                self._fetch(org, name, relative, request, owner)
            finally:
                self.slots.release()
            return "fetched"
        finally:
            os.close(org)

    def _existing(self, org, name, request):
        try:
            fd = os.open(name, READ_FLAGS, dir_fd=org)
        except FileNotFoundError:
            return False
        except OSError:
            raise BrokerError("local_mismatch") from None
        try:
            info = os.fstat(fd)
            digest, size = _hash_fd(fd, request["byte_length"])
        finally:
            os.close(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or size != request["byte_length"] or digest != request["sha256"]):
            # Never overwrite: a local blob that disagrees needs an operator.
            raise BrokerError("local_mismatch")
        return True

    def _remote_chunks(self, relative, limit):
        """Yield remote bytes; stop early (caller sees a short read) on overrun."""
        try:
            process = subprocess.Popen(self.fetch_argv(relative), stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        except OSError:
            raise BrokerError("unavailable") from None
        timer = threading.Timer(self.settings.get("fetch_timeout_seconds", 600), process.kill)
        timer.start()
        size = 0
        try:
            while True:
                chunk = process.stdout.read(CHUNK)
                if not chunk:
                    break
                size += len(chunk)
                if size > limit:
                    process.kill()
                    raise BrokerError("remote_mismatch")
                yield chunk
            if process.wait() != 0:
                # The ledger says the remote copy exists, so a failed read is an
                # outage (network, auth, timeout), retryable by the caller.
                raise BrokerError("unavailable")
        finally:
            timer.cancel()
            if process.poll() is None:
                process.kill()
                process.wait()
            process.stdout.close()

    def _fetch(self, org, name, relative, request, owner):
        temporary = f"{name}.fetch-{secrets.token_hex(8)}.tmp"
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=org)
        published = False
        try:
            os.fchown(fd, owner.st_uid, owner.st_gid)
            os.fchmod(fd, 0o600)
            digest, size = hashlib.sha256(), 0
            for chunk in self._remote_chunks(relative, request["byte_length"]):
                size += len(chunk)
                digest.update(chunk)
                view = memoryview(chunk)
                while view:
                    view = view[os.write(fd, view):]
            if size != request["byte_length"] or digest.hexdigest() != request["sha256"]:
                raise BrokerError("remote_mismatch")
            os.fsync(fd)
            os.close(fd)
            fd = -1
            # Defence in depth (forget already serialises on the per-blob lock).
            if relative in self.tombstones.get():
                raise BrokerError("forgotten")
            try:
                publish_noreplace(org, temporary, name)
            except FileExistsError:
                # Published concurrently by someone else; accept only equal bytes.
                if not self._existing(org, name, request):
                    raise BrokerError("local_mismatch") from None
            published = True
        finally:
            if fd >= 0:
                os.close(fd)
            try:
                os.unlink(temporary, dir_fd=org)
            except FileNotFoundError:
                pass
            if published:
                os.fsync(org)

    def stream_to(self, connection, relative, request):
        """Header, then the remote bytes. The broker also checks the hash, but it
        cannot retract bytes already sent: the caller must verify them."""
        self._slot()
        try:
            chunks = self._remote_chunks(relative, request["byte_length"])
            first = next(chunks, b"")  # an immediate outage is still a clean error
            connection.sendall((json.dumps({"ok": True, "result": "stream",
                                            "byte_length": request["byte_length"]}) + "\n").encode())
            try:
                connection.sendall(first)
                for chunk in chunks:
                    connection.sendall(chunk)
            except (BrokerError, OSError):
                # Header already sent: close early; the caller sees a short read.
                return
        finally:
            self.slots.release()

    def cleanup_stale_temporaries(self, now=None):
        """Remove broker temporaries left by a crash (older than the fetch timeout)."""
        now = time.time() if now is None else now
        cutoff = now - self.settings.get("fetch_timeout_seconds", 600)
        removed = 0
        root = open_directory(self.config["local_root"])
        try:
            for org_name in os.listdir(root):
                if not box.ORG.match(org_name):
                    continue
                try:
                    org = os.open(org_name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root)
                except OSError:
                    continue
                try:
                    for name in os.listdir(org):
                        if not FETCH_TEMP.match(name):
                            continue
                        info = os.stat(name, dir_fd=org, follow_symlinks=False)
                        if stat.S_ISREG(info.st_mode) and info.st_mtime < cutoff:
                            os.unlink(name, dir_fd=org)
                            removed += 1
                finally:
                    os.close(org)
        finally:
            os.close(root)
        return removed


class _Handler(socketserver.BaseRequestHandler):
    def handle(self):
        broker = self.server.broker
        self.request.settimeout(10)
        try:
            uid = peer_uid(self.request)
            raw = b""
            while b"\n" not in raw:
                chunk = self.request.recv(REQUEST_LIMIT)
                if not chunk:
                    break
                raw += chunk
                if len(raw) > REQUEST_LIMIT:
                    raise BrokerError("invalid")
            request = parse_request(raw.split(b"\n", 1)[0].decode("utf-8", "strict"))
            self.request.settimeout(None)
            result = broker.handle(request, uid)
            if isinstance(result, tuple):
                try:
                    broker.stream_to(self.request, result[1], result[2])
                except BrokerError as error:
                    # Before the header this is a clean error reply; after it the
                    # connection just closes and the caller sees a short read.
                    self._reply({"ok": False, "error": error.code})
                return
            reply = {"ok": True, "result": result}
        except BrokerError as error:
            reply = {"ok": False, "error": error.code}
        except (OSError, UnicodeDecodeError, ValueError):
            reply = {"ok": False, "error": "unavailable"}
        self._reply(reply)

    def _reply(self, reply):
        try:
            self.request.sendall((json.dumps(reply) + "\n").encode())
        except OSError:
            pass


class BrokerServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def __init__(self, broker):
        self.broker = broker
        path = broker.settings["socket"]
        try:
            if stat.S_ISSOCK(os.lstat(path).st_mode):
                os.unlink(path)  # stale socket from a previous run only
        except FileNotFoundError:
            pass
        old = os.umask(0o177)
        try:
            super().__init__(path, _Handler)
        finally:
            os.umask(old)
        # Only the app group may connect; the peer uid is still checked per call.
        os.chown(path, os.getuid(), broker.settings["socket_gid"])
        os.chmod(path, 0o660)


def validate_broker(config):
    settings = config.get("broker")
    if not isinstance(settings, dict):
        raise box.ConfigError("config has no broker section")
    allowed = {"socket", "client_uid", "socket_gid", "fetch_timeout_seconds", "max_parallel",
               "queue_timeout_seconds"}
    if not {"socket", "client_uid", "socket_gid"} <= settings.keys() <= allowed:
        raise box.ConfigError(f"broker keys must be within {sorted(allowed)}")
    if not isinstance(settings["socket"], str) or not box.SAFE_PATH.match(settings["socket"]):
        raise box.ConfigError("broker.socket must be an absolute path without spaces")
    for key, low in (("client_uid", 1), ("socket_gid", 0), ("fetch_timeout_seconds", 10),
                     ("max_parallel", 1), ("queue_timeout_seconds", 1)):
        if key in settings and (type(settings[key]) is not int or settings[key] < low):
            raise box.ConfigError(f"broker.{key} must be an integer >= {low}")
    return settings


def _connect(socket_path, payload, timeout):
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(timeout)
    client.connect(socket_path)
    client.sendall((json.dumps(payload) + "\n").encode())
    return client


def _read_line(client, limit=4096):
    data = b""
    while not data.endswith(b"\n") and len(data) <= limit:
        chunk = client.recv(1)
        if not chunk:
            break
        data += chunk
    return json.loads(data)


def request(socket_path, payload, timeout=30):
    """Client helper (evictor health check, operator probes, tests)."""
    with _connect(socket_path, payload, timeout) as client:
        return _read_line(client)


def stream(socket_path, payload, timeout=60):
    """Client helper for `stream`: returns (header, bytes)."""
    with _connect(socket_path, payload, timeout) as client:
        header = _read_line(client)
        data = b""
        while header.get("ok"):
            chunk = client.recv(CHUNK)
            if not chunk:
                break
            data += chunk
        return header, data


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", default="/etc/neev-storage-box/config.json")
    parser.add_argument("command", choices=["serve", "ping"])
    options = parser.parse_args(argv)
    try:
        config = box.load_config(options.config)
        validate_broker(config)
        if options.command == "ping":
            print(json.dumps(request(config["broker"]["socket"], {"v": 1, "op": "ping"})))
            return 0
        broker = Broker(config)
        removed = broker.cleanup_stale_temporaries()
        server = BrokerServer(broker)
        print(f"neev originals broker listening; removed {removed} stale temporaries", flush=True)
        server.serve_forever()
        return 0
    except box.ConfigError as error:
        print(f"neev_originals_broker: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"neev_originals_broker: {error.strerror}", file=sys.stderr)
        return 3


if __name__ == "__main__":
    sys.exit(main())
