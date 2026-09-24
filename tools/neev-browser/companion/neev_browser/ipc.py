"""Per-run IPC between the native host and the MCP server it owns.

AF_UNIX inside the run directory, mode 0600, with a per-run token that must
arrive in the first frame. Nothing listens on TCP. The socket is unlinked when
the run ends, so it does not outlive the credential it protects.
"""

import hmac
import os
import secrets
import socket
import threading

from .framing import read_message, write_message


class AuthError(Exception):
    pass


def new_token():
    return secrets.token_hex(24)


class _SocketStream(object):
    """Adapts a socket to the read/write/flush interface `framing` expects."""

    def __init__(self, sock):
        self._sock = sock

    def read(self, count):
        return self._sock.recv(count)

    def write(self, data):
        self._sock.sendall(data)

    def flush(self):
        pass


class IpcServer(object):
    def __init__(self, socket_path, token, handler):
        self.socket_path = socket_path
        self._token = token
        self._handler = handler
        self._sock = None
        self._closed = threading.Event()

    def start(self):
        if os.path.exists(self.socket_path):
            os.unlink(self.socket_path)
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.bind(self.socket_path)
        os.chmod(self.socket_path, 0o600)
        self._sock.listen(4)
        thread = threading.Thread(target=self._accept_loop)
        thread.daemon = True
        thread.start()

    def _accept_loop(self):
        while not self._closed.is_set():
            try:
                conn, _ = self._sock.accept()
            except (OSError, AttributeError):
                return
            worker = threading.Thread(target=self._serve, args=(conn,))
            worker.daemon = True
            worker.start()

    def _serve(self, conn):
        stream = _SocketStream(conn)
        try:
            hello = read_message(stream)
            if (not isinstance(hello, dict)
                    or hello.get("type") != "hello"
                    or not hmac.compare_digest(str(hello.get("token") or ""),
                                               self._token)):
                return
            write_message(stream, {"type": "welcome"})
            while not self._closed.is_set():
                request = read_message(stream)
                if request is None:
                    return
                write_message(stream, self._handler(request))
        except Exception:
            return
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def close(self):
        self._closed.set()
        sock, self._sock = self._sock, None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        if os.path.exists(self.socket_path):
            try:
                os.unlink(self.socket_path)
            except OSError:
                pass


class IpcClient(object):
    def __init__(self, socket_path, token, timeout=None):
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        if timeout is not None:
            self._sock.settimeout(timeout)
        self._sock.connect(socket_path)
        self._stream = _SocketStream(self._sock)
        write_message(self._stream, {"type": "hello", "token": token})
        if read_message(self._stream) != {"type": "welcome"}:
            self.close()
            raise AuthError("IPC handshake rejected")

    def call(self, payload):
        write_message(self._stream, payload)
        reply = read_message(self._stream)
        if reply is None:
            raise AuthError("IPC connection closed by the host")
        return reply

    def close(self):
        try:
            self._sock.close()
        except OSError:
            pass
