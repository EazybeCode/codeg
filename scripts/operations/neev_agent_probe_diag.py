#!/usr/bin/env python3
"""Diagnostic: exercise the REAL ClaudeAccountProbe / check_connection machinery
(slot proxy + credential deliver + sandbox + `claude auth status --json` + parse) against
a FRESH volume (no credentials -> loggedIn:false). It prints the probe result or the
exact exception, so we can tell whether check_connection's UNAVAILABLE comes from the
probe machinery itself or is specific to the committed credential state. No real
credentials are used or printed. Run as root in the pinned image (with --init)."""

from __future__ import annotations

import os
import sys
import traceback
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_network_bootstrap as bootstrap
import neev_agent_supervisor_main as main
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module
import neev_supervisor_dto as dto


def _env():
    return {
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/ipc/control.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/ipc/control.key",
        "NEEV_SUPERVISOR_FENCE_DIR": "/run/neev/fence",
        "NEEV_SUPERVISOR_STORE_ROOT": "/var/lib/neev-supervisor/credentials",
        "NEEV_SUPERVISOR_MASTER_KEY": "/etc/neev-supervisor/credential-master.key",
        "NEEV_AGENT_WORKSPACE_ROOT": "/data/agents",
        "NEEV_SUPERVISOR_REGISTRY": "/var/lib/neev-supervisor/registry.json",
        "NEEV_SUPERVISOR_CONTROL_GID": "65532", "NEEV_SUPERVISOR_PEER_UID": "65532",
        "NEEV_SUPERVISOR_PEER_GID": "65532", "NEEV_SUPERVISOR_LANE_UID": "61000",
        "NEEV_SUPERVISOR_LANE_GID": "61000", "NEEV_SUPERVISOR_SLOT": "a",
        "NEEV_SUPERVISOR_UPLINK": "eth0", "NEEV_SUPERVISOR_EGRESS_IPV4": "1.1.1.1",
    }


def run():
    config = main.config_from_env(_env())
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    plan = bootstrap.plan(config.uplink)
    runner = bootstrap.CommandRunner()
    bootstrap.apply(plan, runner)
    store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
    store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    scope = store_module.Scope(1, 2, "claude")
    handle = None
    try:
        handle = volumes.create(scope)  # fresh volume: loggedIn should be false
        _driver_for, probe_for = main.make_factories(config, volumes)
        probe = probe_for(dto.Provider.CLAUDE, scope, handle)
        try:
            result = probe.probe()
            print("PROBE RESULT:", result)  # expect "disconnected" for a fresh volume
        except Exception as error:
            print("PROBE RAISED:", type(error).__name__, str(error))
            traceback.print_exc()
    finally:
        try:
            bootstrap.teardown(plan, runner)
        except Exception:
            pass
        if handle is not None:
            for step in (lambda: volumes.close(scope, handle), lambda: volumes.revoke(scope, handle)):
                try:
                    step()
                except Exception:
                    pass


if __name__ == "__main__":
    if os.environ.get("NEEV_AGENT_PROBE_DIAG") != "1":
        print("set NEEV_AGENT_PROBE_DIAG=1")
        raise SystemExit(2)
    run()
