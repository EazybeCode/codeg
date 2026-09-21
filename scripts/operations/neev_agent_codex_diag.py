#!/usr/bin/env python3
"""Diagnostic: run the REAL pinned `codex app-server` in the production sandbox with its
stderr CAPTURED (the production sandbox sends stderr to /dev/null, so a driver reading
stdout only cannot see why codex exits). Drives the app-server device flow far enough to
either surface a device challenge or reveal the failure, then dumps the bounded combined
output + exit code. No credentials are supplied and none are authorized — device-code
stage output is public. Run as root in the pinned image (Linux)."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_credentials as credentials
import neev_agent_network_bootstrap as bootstrap
import neev_agent_sandbox as sandbox
import neev_agent_supervisor_main as main
import neev_codex_login_protocol as codex_proto
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module
import neev_slot_proxy as slot_proxy

_ARGV = ("sh", "-c", "exec codex app-server 2>&1")  # merge stderr so we SEE it
_CAPTURE_BYTES = 8000
_DEADLINE = 25.0


def _env(workspace_root):
    return {
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/ipc/control.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/ipc/control.key",
        "NEEV_SUPERVISOR_FENCE_DIR": "/run/neev/fence",
        "NEEV_SUPERVISOR_STORE_ROOT": "/var/lib/neev-supervisor/credentials",
        "NEEV_SUPERVISOR_MASTER_KEY": "/etc/neev-supervisor/credential-master.key",
        "NEEV_AGENT_WORKSPACE_ROOT": workspace_root,
        "NEEV_SUPERVISOR_REGISTRY": os.path.join(workspace_root, "diag-registry.json"),
        "NEEV_SUPERVISOR_CONTROL_GID": "65532", "NEEV_SUPERVISOR_PEER_UID": "65532",
        "NEEV_SUPERVISOR_PEER_GID": "65532", "NEEV_SUPERVISOR_LANE_UID": "61000",
        "NEEV_SUPERVISOR_LANE_GID": "61000", "NEEV_SUPERVISOR_SLOT": "a",
        "NEEV_SUPERVISOR_UPLINK": "eth0", "NEEV_SUPERVISOR_EGRESS_IPV4": "1.1.1.1",
    }


def run():
    config = main.config_from_env(_env("/data/agents"))
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    plan = bootstrap.plan(config.uplink)
    runner = bootstrap.CommandRunner()
    bootstrap.apply(plan, runner)
    store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
    store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    scope = store_module.Scope(1, 2, "codex")
    handle = None
    try:
        handle = volumes.create(scope)
        tenant = "org:1/user:2"
        workdir = tempfile.mkdtemp(prefix="run-", dir=config.workspace_root)
        home = sandbox.home_path(workdir)
        with slot_proxy.slot_proxy("codex", "login", config.slot) as proxy_env:
            with credentials.deliver(
                volumes, tenant, "codex", handle,
                config.lane_uid, config.lane_gid, home_as=home,
            ) as delivery:
                spec = main._login_spec(config, tenant, _ARGV, workdir)
                env = {"PATH": "/usr/local/bin:/usr/bin:/bin"}
                env.update(proxy_env)
                env.update(credentials.provider_env(home, "codex"))
                child = sandbox.launch(spec, env=env, bind_source=delivery.mount, stdin_pipe=True)
                os.set_blocking(child.stdout.fileno(), False)
                proto = codex_proto.CodexLoginProtocol()
                _write(child, proto.initialize())
                buffer = bytearray()
                sent_start = False
                exit_code = None
                deadline = time.monotonic() + _DEADLINE
                while time.monotonic() < deadline:
                    try:
                        chunk = os.read(child.stdout.fileno(), 65536)
                    except (BlockingIOError, OSError):
                        chunk = b""
                    if chunk:
                        buffer.extend(chunk)
                    if not sent_start and b'"result"' in bytes(buffer):
                        sent_start = _write(child, proto.start_login())
                    try:
                        exit_code = child.wait(timeout=0)
                    except subprocess.TimeoutExpired:
                        exit_code = None  # still running
                    if exit_code is not None and not chunk:
                        break
                    time.sleep(0.2)
                print("=== argv ===", " ".join(_ARGV))
                print("=== reached start_login write ===", sent_start)
                print("=== exit code ===", exit_code if exit_code is not None else "still running")
                print("=== combined stdout+stderr (bounded %d bytes) ===" % _CAPTURE_BYTES)
                sys.stdout.buffer.write(bytes(buffer)[:_CAPTURE_BYTES])
                sys.stdout.buffer.write(b"\n=== end ===\n")
                sys.stdout.flush()
                child.teardown()
    finally:
        try:
            bootstrap.teardown(plan, runner)
        except Exception:
            pass
        if handle is not None:
            try:
                volumes.close(scope, handle)
            except Exception:
                pass
            try:
                volumes.revoke(scope, handle)
            except Exception:
                pass


def _write(child, data):
    try:
        child.stdin.write(data)
        child.stdin.flush()
        return True
    except OSError:
        return False


if __name__ == "__main__":
    if os.environ.get("NEEV_AGENT_CODEX_DIAG") != "1":
        print("set NEEV_AGENT_CODEX_DIAG=1")
        raise SystemExit(2)
    run()
