#!/usr/bin/env python3
"""Direct, in-process proof that the SUPERVISOR'S login dispatch drives the real
login lifecycle — start_login -> observe -> cancel — independent of the native Rust
HTTP/transport layer.

Two modes, both building the real `Supervisor` (via
`neev_agent_supervisor_main.build_supervisor`) with a REAL `LoginChannel` (real
slot-proxy login listener + credential deliver + sandbox):

  * --provider claude (default): runs the ACTUAL pinned `claude auth login`. No
    credentials are supplied and none are authorized — we only drive the CLI far
    enough to emit its PUBLIC OAuth approval URL (observe -> AWAITING_USER), then
    cancel. This proves the real CLI works end to end through the sandbox + slot proxy.
  * --provider codex --canned: points the channel at a canned codex app-server
    (`_fake_codex_provider`) instead of the real CLI, to prove the dispatch/lifecycle
    itself without a second real provider.

In both cases we assert start_login is ACCEPTED and spins a lane-uid provider up,
observe reaches AWAITING_USER with the challenge URL, and cleanup_connection(CANCEL)
COMPLETES: driver cancelled, channel torn down (no lane-uid process survives, no run
workdir left), volume revoked, entry DISPOSED in reconcile. If this passes but the
native HTTP path 503s with empty daemon logs, the fault is the transport/coordinator,
not the Python login lifecycle.

Run as ROOT inside the pinned image with the 9-cap profile + named AppArmor + default
seccomp + explicit LUKS devices + --sysctl net.ipv4.ip_forward=1:

    NEEV_AGENT_LOGIN_DISPATCH_PROOF=1 python3 neev_agent_login_dispatch_proof.py \
        --store-root /var/lib/neev-supervisor/credentials \
        --key-path /etc/neev-supervisor/credential-master.key \
        --workspace-root /data/agents --uplink eth0 --provider claude
"""

from __future__ import annotations

import argparse
import os
import platform
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_network_bootstrap as bootstrap
import neev_agent_supervisor_main as main
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module
import neev_supervisor_dto as dto

_HERE = os.path.dirname(os.path.abspath(__file__))
FAKE_ARGV = ("/usr/bin/python3", os.path.join(_HERE, "_fake_codex_provider.py"))

SESSION = "123e4567-e89b-42d3-a456-426614174000"
COORD = dto.Coordinator(dto.Counter("1"), dto.Id("1" * 32))
_ORG, _USER = 1, 2
_CODEX_DEVICE_URL = "https://auth.openai.com/codex/device"
# The public authorize origins the pinned Claude CLI emits (no auth performed).
_CLAUDE_URL_PREFIXES = (
    "https://claude.com/", "https://claude.ai/", "https://console.anthropic.com/",
)


def _scope(provider):
    return dto.Scope(dto.OwnerId(_ORG), dto.OwnerId(_USER), provider)


def _binding(provider):
    return dto.LoginBinding(
        _scope(provider), dto.Id("4" * 32), dto.Counter("1"),
        dto.SessionId(SESSION), dto.Id("5" * 32), dto.Id("6" * 32),
    )


def _ticket(provider):
    return dto.LoginTicketDto(_binding(provider), dto.Id("7" * 32), dto.UnixSec(2000000000))


def _cleanup_ticket(provider):
    return dto.CleanupTicketDto(
        _scope(provider), dto.Id("4" * 32), dto.Id("7" * 32),
        dto.CleanupTarget(dto.CleanupKind.CANCEL, dto.Id("5" * 32)),
        dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
    )


def _req(body, seq, rid):
    return dto.Request(COORD, dto.Counter(seq), dto.Id(rid), body)


def _lane_procs(uid):
    found = []
    try:
        pids = [entry for entry in os.listdir("/proc") if entry.isdigit()]
    except OSError:
        return found
    for pid in pids:
        try:
            ruid = None
            with open("/proc/%s/status" % pid, "r", encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    if line.startswith("Uid:"):
                        ruid = int(line.split()[1])
                        break
            if ruid == uid:
                found.append(pid)
        except (OSError, ValueError):
            continue
    return found


def _config(args):
    env = {
        "NEEV_SUPERVISOR_SOCKET": "/run/neev/ipc/control.sock",
        "NEEV_SUPERVISOR_KEY": "/run/neev/ipc/control.key",
        "NEEV_SUPERVISOR_FENCE_DIR": "/run/neev/fence",
        "NEEV_SUPERVISOR_STORE_ROOT": args.store_root,
        "NEEV_SUPERVISOR_MASTER_KEY": args.key_path,
        "NEEV_AGENT_WORKSPACE_ROOT": args.workspace_root,
        "NEEV_SUPERVISOR_REGISTRY": os.path.join(args.workspace_root, "proof-registry.json"),
        "NEEV_SUPERVISOR_CONTROL_GID": "65532",
        "NEEV_SUPERVISOR_PEER_UID": "65532",
        "NEEV_SUPERVISOR_PEER_GID": "65532",
        "NEEV_SUPERVISOR_LANE_UID": str(args.uid),
        "NEEV_SUPERVISOR_LANE_GID": str(args.gid),
        "NEEV_SUPERVISOR_SLOT": "a",
        "NEEV_SUPERVISOR_UPLINK": args.uplink,
        "NEEV_SUPERVISOR_EGRESS_IPV4": "1.1.1.1",
    }
    return main.config_from_env(env)


def _url_ok(provider, url, canned):
    if url is None:
        return False
    if canned:
        return url == _CODEX_DEVICE_URL
    if provider is dto.Provider.CLAUDE:
        return any(url.startswith(prefix) for prefix in _CLAUDE_URL_PREFIXES)
    return url.startswith("https://")


def run(args):
    failures = []

    def check(name, ok, detail=""):
        print(("PASS" if ok else "FAIL"), name, ("- " + detail) if (detail and not ok) else "")
        if not ok:
            failures.append(name)

    provider = dto.Provider.CODEX if args.provider == "codex" else dto.Provider.CLAUDE
    config = _config(args)
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root

    plan = bootstrap.plan(config.uplink)
    runner = bootstrap.CommandRunner()
    bootstrap.apply(plan, runner)  # login slot proxy needs the bridges + proxy netns
    try:
        store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
        store.initialize()
        volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())

        channel_factory = None
        if args.canned:
            def channel_factory(prov, store_scope, handle, argv):
                # REAL channel (slot proxy + deliver + sandbox); canned provider argv.
                return main.LoginChannel(config, volumes, prov, store_scope, handle, FAKE_ARGV)

        supervisor = main.build_supervisor(
            config, volumes, dto.Id("3" * 32),
            channel_factory=channel_factory, now=lambda: 1000,
        )
        supervisor.recover()

        label = "canned codex app-server" if args.canned else ("real `%s`" % args.provider)
        before = set(_lane_procs(args.uid))
        start = supervisor.handle(_req(dto.StartLoginRequest(_ticket(provider)), "1", "a" * 32))
        check(
            "start_login ACCEPTED (%s via real channel/slot-proxy/deliver/sandbox)" % label,
            start.outcome is dto.Outcome.OK
            and getattr(start.body, "disposition", None) is dto.Disposition.ACCEPTED,
            "outcome=%s err=%s" % (start.outcome, getattr(start, "error", None)),
        )

        state, url = None, None
        deadline = time.monotonic() + args.observe_timeout
        while time.monotonic() < deadline:
            obs = supervisor.handle(_req(dto.ObserveLoginRequest(_binding(provider)), "2", "b" * 32))
            if obs.outcome is dto.Outcome.OK:
                state = obs.body.state
                if state is dto.LoginState.AWAITING_USER:
                    url = obs.body.challenge.verification_url
                    break
                if state is dto.LoginState.FAILED:
                    break
            time.sleep(0.2)
        check("observe reached AWAITING_USER via the real provider protocol",
              state is dto.LoginState.AWAITING_USER, "state=%s" % state)
        check("public approval/challenge URL surfaced through the sandbox",
              _url_ok(provider, url, args.canned), "url=%r" % url)
        check("a lane-uid provider process is running during the login",
              bool(set(_lane_procs(args.uid)) - before), "no new lane procs")

        cleanup = supervisor.handle(
            _req(dto.CleanupConnectionRequest(_cleanup_ticket(provider)), "3", "c" * 32)
        )
        check(
            "cleanup_connection(CANCEL) COMPLETED (driver cancelled, volume revoked)",
            cleanup.outcome is dto.Outcome.OK
            and cleanup.body.state is dto.CleanupState.COMPLETED,
            "outcome=%s state=%s" % (cleanup.outcome, getattr(cleanup.body, "state", None)),
        )

        for _ in range(50):
            if not (set(_lane_procs(args.uid)) - before):
                break
            time.sleep(0.1)
        check("no lane-uid provider process survives the cancel",
              not (set(_lane_procs(args.uid)) - before),
              "leftover: %r" % sorted(set(_lane_procs(args.uid)) - before))
        runs = [n for n in os.listdir(config.workspace_root) if n.startswith("run-")]
        check("no run workdir left behind after cancel", not runs, "runs=%r" % runs)
        page = supervisor.handle(_req(dto.ReconcilePageRequest(None, None, 32), "4", "d" * 32))
        phases = {e.phase for e in page.body.entries} if page.outcome is dto.Outcome.OK else set()
        check("cancelled entry DISPOSED in reconcile", dto.RegistryPhase.DISPOSED in phases,
              "phases=%s" % phases)
    finally:
        try:
            bootstrap.teardown(plan, runner)
        except Exception:
            pass
    return failures


def main_entry(argv=None):
    parser = argparse.ArgumentParser(prog="neev_agent_login_dispatch_proof")
    parser.add_argument("--store-root", required=True)
    parser.add_argument("--key-path", required=True)
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--uplink", default="eth0")
    parser.add_argument("--provider", choices=("claude", "codex"), default="claude")
    parser.add_argument("--canned", action="store_true",
                        help="use the canned codex app-server instead of the real CLI")
    parser.add_argument("--observe-timeout", type=float, default=90.0)
    parser.add_argument("--uid", type=int, default=61000)
    parser.add_argument("--gid", type=int, default=61000)
    parser.add_argument("--allow-skip", action="store_true")
    args = parser.parse_args(argv)

    unmet = None
    if os.environ.get("NEEV_AGENT_LOGIN_DISPATCH_PROOF") != "1":
        unmet = "set NEEV_AGENT_LOGIN_DISPATCH_PROOF=1"
    elif platform.system() != "Linux":
        unmet = "Linux required"
    elif os.geteuid() != 0:
        unmet = "must run as root (the daemon runs as root 0:0)"
    if unmet is not None:
        print("UNMET:", unmet)
        return 0 if args.allow_skip else 2

    failures = run(args)
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main_entry())
