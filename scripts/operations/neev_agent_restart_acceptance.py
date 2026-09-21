#!/usr/bin/env python3
"""Restart/recovery acceptance for an ACTIVE (unapproved) login across a real container
stop + restart with the SAME encrypted state/registry/workdir.

Two phases, run as two separate containers sharing host-mounted state
(/var/lib/neev-supervisor, /etc/neev-supervisor, /data/agents):

  --phase start   : build the supervisor on the persistent state, drive a REAL codex
                    login to AWAITING_USER (unapproved), then EXIT HARD (os._exit) with
                    the login still active — no teardown, exactly like an ungraceful
                    stop mid-login. The credential volume is left OPEN (its dm/loop live
                    in the kernel; its /dev/mapper node lived in THIS container), the
                    registry holds the pending attempt, and the run workdir remains.

  --phase restart : build a NEW supervisor on the SAME state in a FRESH container (whose
                    /dev has NONE of the previous container's mapper nodes). recover()
                    the durable registry, purge abandoned run workdirs, then CLEAN the
                    interrupted attempt (close + durable revoke) — this is the concrete
                    risk: can the volume be closed when its mapper node is not in this
                    container's /dev? Finally start a NEW real codex login to prove the
                    slot/lane is reusable, and report any lingering leak.

No host credentials; no approval is performed. Run as root in the pinned image with an
orphan-reaping init as PID 1 (production's container-init, or `docker run --init` here):
a sandboxed provider killed on cancel reparents to PID 1, which must reap it, exactly as
the container-init does in production. Expose enough loop devices that cryptsetup's
auto-loop finds a free one without contending with any other tenant.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_network_bootstrap as bootstrap
import neev_agent_supervisor_main as main
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module
import neev_supervisor_dto as dto

SESSION = "123e4567-e89b-42d3-a456-426614174000"
COORD = dto.Coordinator(dto.Counter("1"), dto.Id("1" * 32))
_ORG, _USER = 1, 2
_DEVICE_URL = "https://auth.openai.com/codex/device"
_SENTINEL = b"NEEV-COMMITTED-SURVIVES-RESTART"
_COMMITTED_HANDLE_FILE = "/var/lib/neev-supervisor/committed-handle"


def _scope():
    return dto.Scope(dto.OwnerId(_ORG), dto.OwnerId(_USER), dto.Provider.CODEX)


def _store_scope():
    return store_module.Scope(_ORG, _USER, "codex")


def _binding(attempt, conn):
    return dto.LoginBinding(
        _scope(), dto.Id(conn), dto.Counter("1"), dto.SessionId(SESSION),
        dto.Id(attempt), dto.Id("6" * 32),
    )


def _ticket(attempt, conn):
    return dto.LoginTicketDto(_binding(attempt, conn), dto.Id("7" * 32), dto.UnixSec(2000000000))


def _req(body, seq, rid):
    return dto.Request(COORD, dto.Counter(seq), dto.Id(rid), body)


def _config():
    env = {
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/ipc/control.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/ipc/control.key",  # control MAC key (unused here)
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
    return main.config_from_env(env)


def _lane_procs(uid):
    found = {}
    for pid in [e for e in os.listdir("/proc") if e.isdigit()]:
        try:
            ruid, comm, state = None, "?", "?"
            with open("/proc/%s/status" % pid, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    if line.startswith("Name:"):
                        comm = line.split("\t", 1)[-1].strip()
                    elif line.startswith("State:"):
                        state = line.split("\t", 1)[-1].strip()
                    elif line.startswith("Uid:"):
                        ruid = int(line.split()[1])
                        break
            if ruid == uid:
                found[pid] = "%s(%s)" % (comm, state)
        except (OSError, ValueError):
            continue
    return found


def _supervisor(config, boot_hex):
    store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
    store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    supervisor = main.build_supervisor(config, volumes, dto.Id(boot_hex), now=lambda: 1000)
    return store, volumes, supervisor


def _drive_login_to_awaiting(supervisor, attempt, conn, seq0, timeout=60):
    start = supervisor.handle(_req(dto.StartLoginRequest(_ticket(attempt, conn)), str(seq0), "a" * 32))
    if start.outcome is not dto.Outcome.OK:
        return None, "start_login %s" % getattr(start, "error", None)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        obs = supervisor.handle(_req(dto.ObserveLoginRequest(_binding(attempt, conn)), str(seq0 + 1), "b" * 32))
        if obs.outcome is dto.Outcome.OK and obs.body.state is dto.LoginState.AWAITING_USER:
            return obs.body.challenge.verification_url, None
        if obs.outcome is dto.Outcome.OK and obs.body.state is dto.LoginState.FAILED:
            return None, "login failed"
        time.sleep(0.2)
    return None, "timeout"


def phase_start(config):
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    bootstrap.apply(bootstrap.plan(config.uplink), bootstrap.CommandRunner())
    store, volumes, supervisor = _supervisor(config, "3" * 32)
    supervisor.recover()
    url, err = _drive_login_to_awaiting(supervisor, "5" * 32, "4" * 32, 1)
    if err is not None:
        print("START: FAILED to reach awaiting_user:", err)
        return 1
    # Also seed a COMMITTED-like volume left OPEN at the stop, to prove committed
    # credentials survive the restart (open() must clear the orphan + reopen the image).
    committed_scope = store_module.Scope(_ORG, _USER + 1, "codex")
    committed_handle = volumes.create(committed_scope)
    mount = volumes.open(committed_scope, committed_handle)
    with open(os.path.join(str(mount), "sentinel"), "wb") as fh:
        fh.write(_SENTINEL)
        fh.flush()
        os.fsync(fh.fileno())
    os.sync()  # flush through dm-crypt/loop to the image before the hard stop
    with open(_COMMITTED_HANDLE_FILE, "w") as fh:
        fh.write(committed_handle)

    handles = sorted(os.listdir(config.store_root)) if os.path.isdir(config.store_root) else []
    print("START: awaiting_user url=%s" % url)
    print("START: committed handle=%s" % committed_handle)
    print("START: store entries=%r" % [h for h in handles if len(h) == 32])
    print("START: OK — exiting HARD with the login + committed volume still open")
    sys.stdout.flush()
    os._exit(0)  # ungraceful stop: no context __exit__, both volumes left open


def phase_restart(config):
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    failures = []

    def check(name, ok, detail=""):
        print(("PASS" if ok else "FAIL"), name, ("- " + detail) if (detail and not ok) else "")
        if not ok:
            failures.append(name)

    entries_before = [h for h in sorted(os.listdir(config.store_root)) if len(h) == 32]
    check("an interrupted attempt's volume survived the stop", bool(entries_before),
          "store entries=%r" % entries_before)

    bootstrap.apply(bootstrap.plan(config.uplink), bootstrap.CommandRunner())
    store, volumes, supervisor = _supervisor(config, "9" * 32)
    supervisor.recover()  # rebuilds the pending attempt as INTERRUPTED (fail-closed)
    check("recover() succeeded on the persisted registry", True)
    main.purge_abandoned_runs(config.workspace_root)

    login_scope = _store_scope()
    committed_scope = store_module.Scope(_ORG, _USER + 1, "codex")
    try:
        committed_handle = Path(_COMMITTED_HANDLE_FILE).read_text().strip()
    except OSError:
        committed_handle = None
    login_handles = [h for h in entries_before if h != committed_handle]

    # 1) COMMITTED credentials must survive: reopen the volume left open at the stop.
    #    open() clears the authenticated kernel orphan (its /dev node lived in the dead
    #    container) then reopens the PRESERVED image; the seeded sentinel must be intact.
    if committed_handle:
        try:
            mount = volumes.open(committed_scope, committed_handle)
            data = Path(os.path.join(str(mount), "sentinel")).read_bytes()
            check("committed credentials survived restart (image reopened, sentinel intact)",
                  data == _SENTINEL, "sentinel=%r" % data[:40])
            volumes.close(committed_scope, committed_handle)
            volumes.revoke(committed_scope, committed_handle)
        except Exception as error:
            check("committed credentials survived restart (image reopened, sentinel intact)",
                  False, "%s: %s" % (type(error).__name__, error))
    else:
        check("committed handle recorded by phase start", False, "missing marker file")

    # 2) The interrupted PENDING login: close (orphan-clear, across the fresh /dev) +
    #    durable revoke — from a container whose /dev lacks the old mapper node.
    for handle in login_handles:
        try:
            volumes.close(login_scope, handle)
            volumes.revoke(login_scope, handle)
            check("closed + revoked the interrupted login volume (handle %s)" % handle[:8], True)
        except Exception as error:
            check("closed + revoked the interrupted login volume (handle %s)" % handle[:8],
                  False, "%s: %s" % (type(error).__name__, error))

    entries_after = [h for h in sorted(os.listdir(config.store_root)) if len(h) == 32]
    check("all interrupted/committed entries durably deleted", not entries_after,
          "still present=%r" % entries_after)

    # A NEW login must work on the same slot/lane after recovery.
    before = set(_lane_procs(config.lane_uid))
    url, err = _drive_login_to_awaiting(supervisor, "a" * 32, "b" * 32, 10)
    check("a NEW login reaches awaiting_user after restart", err is None and url == _DEVICE_URL,
          "err=%s url=%r" % (err, url))
    # Cancel the new login cleanly.
    cleanup = supervisor.handle(_req(dto.CleanupConnectionRequest(dto.CleanupTicketDto(
        _scope(), dto.Id("b" * 32), dto.Id("7" * 32),
        dto.CleanupTarget(dto.CleanupKind.CANCEL, dto.Id("a" * 32)),
        dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
    )), "12", "c" * 32))
    check("new login cancelled + revoked cleanly", cleanup.outcome is dto.Outcome.OK
          and cleanup.body.state is dto.CleanupState.COMPLETED,
          "state=%s" % getattr(cleanup.body, "state", None))
    leftover = {}
    for _ in range(100):
        procs = _lane_procs(config.lane_uid)
        leftover = {pid: procs[pid] for pid in set(procs) - before}
        if not leftover:
            break
        time.sleep(0.1)
    check("no lane-uid process left after the new login's cancel",
          not leftover, "leftover=%r" % leftover)

    final = [h for h in sorted(os.listdir(config.store_root)) if len(h) == 32]
    check("no store entries leaked at the end", not final, "entries=%r" % final)
    return failures


def main_entry(argv=None):
    parser = argparse.ArgumentParser(prog="neev_agent_restart_acceptance")
    parser.add_argument("--phase", choices=("start", "restart"), required=True)
    args = parser.parse_args(argv)
    if os.environ.get("NEEV_AGENT_RESTART_ACCEPTANCE") != "1":
        print("set NEEV_AGENT_RESTART_ACCEPTANCE=1")
        return 2
    config = _config()
    if args.phase == "start":
        return phase_start(config)
    failures = phase_restart(config)
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main_entry())
