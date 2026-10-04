#!/usr/bin/env python3
"""Stdio <-> socket relay an agent runs as a shared integration's MCP server.

The agent's MCP config says only `python3 -I neev_mcp_bridge.py <key>`: no URL,
no header, no token. This relays newline-delimited JSON-RPC between the agent's
stdio and the run's integration broker (`neev_mcp_broker`), which holds the
credential under another uid. If the broker cannot be reached, every request is
answered with a JSON-RPC error whose `data.neevState` is `broker_unavailable`, so
the agent (and the reader) see why the tool is missing instead of a hang.
"""

import json
import re
import socket
import sys
import threading
import time

KEY_RE = re.compile(r"[a-z0-9][a-z0-9_-]{0,23}\Z")
CONNECT_TIMEOUT_SECONDS = 20.0
MAX_LINE_BYTES = 8 * 1024 * 1024


def socket_address(key, socket_dir=None):
    if socket_dir:
        return socket_dir.rstrip("/") + "/" + key + ".sock"
    return "\0neev-mcp/" + key


def _connect(key, socket_dir, timeout):
    deadline = time.monotonic() + timeout
    while True:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(socket_address(key, socket_dir))
            return sock
        except OSError:
            sock.close()
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)


def _unavailable(key, stdin, stdout):
    for line in iter(lambda: stdin.readline(MAX_LINE_BYTES + 1), b""):
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if isinstance(message, dict) and "id" in message and "method" in message:
            reply = {"jsonrpc": "2.0", "id": message["id"], "error": {
                "code": -32001,
                "message": "Integration %s unavailable: broker_unavailable — the "
                           "integration broker is not running for this run" % key,
                "data": {"neevState": "broker_unavailable", "httpStatus": None}}}
            stdout.write((json.dumps(reply) + "\n").encode("utf-8"))
            stdout.flush()
    return 0


def relay(key, stdin, stdout, socket_dir=None, connect_timeout=CONNECT_TIMEOUT_SECONDS,
          linger=None):
    """Relay until the agent closes stdin (then wait for replies) or the broker
    closes. `linger` bounds the wait for replies after stdin EOF (tests)."""
    sock = _connect(key, socket_dir, connect_timeout)
    if sock is None:
        return _unavailable(key, stdin, stdout)
    done = threading.Event()

    def downstream():
        try:
            while True:
                chunk = sock.recv(65536)
                if not chunk:
                    break
                stdout.write(chunk)
                stdout.flush()
        except (OSError, ValueError):
            pass
        finally:
            done.set()

    reader = threading.Thread(target=downstream, daemon=True)
    reader.start()
    try:
        for line in iter(lambda: stdin.readline(MAX_LINE_BYTES + 1), b""):
            sock.sendall(line if line.endswith(b"\n") else line + b"\n")
    except OSError:
        pass
    if linger is None:
        try:
            sock.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        done.wait()
    else:
        # Tests: give in-flight replies a moment, then close.
        deadline = time.monotonic() + linger
        while not done.is_set() and time.monotonic() < deadline:
            time.sleep(0.05)
    sock.close()
    return 0


def main(argv=None, stdin=None, stdout=None):
    argv = sys.argv[1:] if argv is None else argv
    socket_dir = None
    if len(argv) == 3 and argv[0] == "--socket-dir":  # local tests only
        socket_dir, argv = argv[1], argv[2:]
    if len(argv) != 1 or not KEY_RE.match(argv[0]):
        sys.stderr.write("neev_mcp_bridge: invalid integration key\n")
        raise SystemExit(2)
    return relay(argv[0], stdin or sys.stdin.buffer, stdout or sys.stdout.buffer,
                 socket_dir=socket_dir)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
