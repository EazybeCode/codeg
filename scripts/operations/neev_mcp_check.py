#!/usr/bin/env python3
"""An admin's "Test" of shared integrations, run INSIDE the agent sandbox.

It is the agent's own path, minus the model: as the run's lane uid, in the run's
network namespace, it connects to each integration exactly as the bridge does,
sends `initialize`, `notifications/initialized` and `tools/list`, and prints one
JSON line per integration:

    {"neevIntegrationCheck":1,"key":"github","state":"ready","toolCount":42,"httpStatus":null}

`state` is the broker's own classification (`auth_failed`, `not_found`, ...) or
`ready`; a silent or garbled server is `protocol_error`, a missing broker
`broker_unavailable`. Nothing the server returns is printed except a tool count.
"""

import json
import os
import re
import socket
import sys
import time

# `python3 -I` leaves the script's own directory off sys.path; the sibling
# modules live beside this file in the same root-owned directory.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import neev_mcp_bridge as bridge

TIMEOUT_SECONDS = 60.0
KEY_RE = re.compile(r"[a-z0-9][a-z0-9_-]{0,23}\Z")


def _exchange(sock, reader, message, deadline):
    sock.sendall((json.dumps(message) + "\n").encode("utf-8"))
    while True:
        sock.settimeout(max(0.1, deadline - time.monotonic()))
        line = reader.readline(bridge.MAX_LINE_BYTES + 1)
        if not line:
            raise EOFError
        try:
            reply = json.loads(line)
        except ValueError:
            continue
        if isinstance(reply, dict) and reply.get("id") == message["id"]:
            return reply


def _failed(reply):
    data = (reply.get("error") or {}).get("data") or {}
    state = data.get("neevState") if isinstance(data, dict) else None
    status = data.get("httpStatus") if isinstance(data, dict) else None
    return (state if isinstance(state, str) and re.fullmatch(r"[a-z_]{1,32}", state)
            else "protocol_error",
            status if isinstance(status, int) else None)


def check(key, socket_dir=None, timeout=TIMEOUT_SECONDS, connect_timeout=20.0):
    result = {"neevIntegrationCheck": 1, "key": key, "state": "protocol_error",
              "toolCount": None, "httpStatus": None}
    sock = bridge._connect(key, socket_dir, connect_timeout)
    if sock is None:
        result["state"] = "broker_unavailable"
        return result
    deadline = time.monotonic() + timeout
    try:
        reader = sock.makefile("rb")
        init = _exchange(sock, reader, {
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "neev-integration-check", "version": "1"}}},
            deadline)
        if "error" in init:
            result["state"], result["httpStatus"] = _failed(init)
            return result
        sock.sendall(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        tools = _exchange(sock, reader, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
                          deadline)
        if "error" in tools:
            result["state"], result["httpStatus"] = _failed(tools)
            return result
        listed = (tools.get("result") or {}).get("tools")
        if not isinstance(listed, list):
            return result
        result["state"], result["toolCount"] = "ready", len(listed)
        return result
    except (OSError, EOFError, socket.timeout):
        return result
    finally:
        sock.close()


def main(argv=None):
    keys = sys.argv[1:] if argv is None else argv
    if not keys or len(keys) > 32 or not all(KEY_RE.match(key) for key in keys):
        sys.stderr.write("neev_mcp_check: invalid keys\n")
        return 2
    for key in keys:
        sys.stdout.write(json.dumps(check(key), separators=(",", ":")) + "\n")
        sys.stdout.flush()
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
