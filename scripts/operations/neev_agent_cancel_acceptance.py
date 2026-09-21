#!/usr/bin/env python3
"""End-to-end acceptance for CANCELLATION of a running agent, driven the way the
native runtime actually cancels: the signal originates from the WEB-SERVER uid
(65532, zero effective caps) that spawned the helper — NOT from a root fixture.

This exercises the two independent facts a real cancel needs, together:

  1. The web parent can SIGNAL the launcher at all. The launcher elevates to
     ruid/euid 0 for the credential/mount phase but keeps the caller's uid as its
     SAVED set-uid (`neev_agent_launch._become_credential_owner`), so kill(2) from
     the web uid is permitted. `setresuid(0,0,0)` would make the launcher
     unsignalable by its parent, and no amount of CAP_KILL on the helper fixes that
     (CAP_KILL lets the launcher signal the provider, never the parent signal the
     launcher).
  2. The launcher can then KILL the uid-dropped provider child. Its SIGTERM handler
     process-group-kills the lane-uid child, which requires CAP_KILL in the helper's
     ambient set (root without CAP_KILL cannot signal a process it does not own by
     uid).

Topology under test (each arrow a real exec):

    web parent (uid 65532, no-new-privs OFF)
      -> caplaunch helper (FILE caps raise ambient: 9 caps incl KILL + MKNOD)
        -> python launcher (ruid/euid 0, SAVED-uid 65532)
          -> sandbox (unshare + setpriv drop)
            -> provider: `sleep` (runs as the distinct lane uid)

Then the web parent sends SIGTERM to the launcher and we verify the ENTIRE tree
exits promptly and the run workdir is removed (no lane-uid leftover). Root only
provisions the root-owned fixture volume; every helper/cancel action runs as the web
uid via `setpriv --reuid=65532 --regid=65532 --clear-groups` (no --no-new-privs), so
the helper's file-cap raise is the only source of privilege — the deployed path.

deny-all egress (no --provider-ipv4) is used deliberately: this isolates the cancel
path from the slot proxy, which has its own acceptance.

Run as ROOT inside the pinned image with the EXACT 9-cap profile + named AppArmor +
default seccomp:

    NEEV_AGENT_CANCEL_ACCEPTANCE=1 python3 neev_agent_cancel_acceptance.py \
        --caphelper /usr/local/bin/neev-agent-caplaunch \
        --store-root /var/lib/neev-supervisor/credentials \
        --key-path /etc/neev-supervisor/credential-master.key \
        --workspace-root /data/agents --provider claude --uid 61000 --gid 61000

Exit 0 = cancel worked end to end; nonzero = a step failed (prints which) or a
prerequisite was unmet (--allow-skip forces 0)."""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module

SETPRIV = "/usr/bin/setpriv"
_WEB_UID = 65532
_TEST_ORG = 525252
_TEST_USER = 535353
# How long the web parent lets the tree come up before cancelling, and how long it
# then waits for the launcher to exit. Bounded so a hang fails rather than blocks.
_SETTLE_SECONDS = 4.0
_EXIT_TIMEOUT_SECONDS = 20.0
_SLEEP_SECONDS = "600"  # the provider "runs" long enough to be cancelled mid-flight


def _drop_to_web(argv):
    """Run argv as the web uid, bounding caps retained, no-new-privs OFF, so an exec'd
    caphelper's FILE capabilities take effect (the deployed privilege path)."""
    return [
        SETPRIV, "--reuid=%d" % _WEB_UID, "--regid=%d" % _WEB_UID, "--clear-groups",
        *argv,
    ]


# --------------------------------------------------------------------------- #
# Web-parent mode: runs AS uid 65532; spawns the helper and cancels it.
# --------------------------------------------------------------------------- #


def web_parent(args):
    """Executed as the web uid. Spawn the real helper on a long-running provider, let
    it come up, then SIGTERM the launcher and report whether the signal was permitted
    and the launcher exited. Prints machine-readable lines the root harness parses."""
    launcher_args = [
        args.caphelper,
        "--tenant", args.tenant,
        "--slot", "a",
        "--workdir", args.workdir,
        "--uid", str(args.uid),
        "--gid", str(args.gid),
        "--provider", args.provider,
        "--credential-handle", args.credential_handle,
        "--", "sleep", _SLEEP_SECONDS,
    ]
    proc = subprocess.Popen(launcher_args)
    # Let the credential open + bind + setpriv drop + `sleep` start.
    deadline = time.monotonic() + _SETTLE_SECONDS
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            print("PARENT: launcher exited before cancel rc=%s" % proc.returncode)
            return 1
        time.sleep(0.1)
    # The cancel itself: from the web uid to the launcher. This is the assertion —
    # setresuid(0,0,0) in the launcher would make this raise PermissionError.
    try:
        os.kill(proc.pid, signal.SIGTERM)
        print("PARENT: KILL_OK")
    except PermissionError:
        print("PARENT: KILL_EPERM (web parent cannot signal launcher)")
        try:
            proc.kill()
        except OSError:
            pass
        return 2
    except OSError as error:
        print("PARENT: KILL_ERR %s" % error)
        return 2
    try:
        rc = proc.wait(timeout=_EXIT_TIMEOUT_SECONDS)
        print("PARENT: LAUNCHER_EXIT rc=%s" % rc)
        return 0
    except subprocess.TimeoutExpired:
        print("PARENT: LAUNCHER_HANG (did not exit after SIGTERM)")
        try:
            proc.kill()
        except OSError:
            pass
        return 3


# --------------------------------------------------------------------------- #
# Root mode: provision the fixture, run the web parent, verify the aftermath.
# --------------------------------------------------------------------------- #


def _lane_procs(uid):
    """PIDs owned by the lane uid, via ps (fail-open to empty)."""
    try:
        out = subprocess.run(
            ["ps", "-u", str(uid), "-o", "pid=,comm="], capture_output=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return []
    return [line for line in out.stdout.decode("utf-8", "replace").splitlines() if line.strip()]


def run(args):
    failures = []

    def check(name, ok, detail=""):
        print(("PASS" if ok else "FAIL"), name, ("- " + detail) if (detail and not ok) else "")
        if not ok:
            failures.append(name)

    probe = subprocess.run(_drop_to_web(["id", "-u"]), capture_output=True, timeout=30)
    check(
        "preflight: helper invocation starts at web uid %d" % _WEB_UID,
        probe.returncode == 0 and probe.stdout.strip() == str(_WEB_UID).encode(),
        "got %r" % probe.stdout.strip(),
    )
    if failures:
        return failures

    store = store_module.CredentialStore(args.store_root, args.key_path)
    if not args.key_path.exists():
        store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    tenant = "org:%d/user:%d" % (_TEST_ORG, _TEST_USER)
    scope = store_module.Scope(_TEST_ORG, _TEST_USER, args.provider)

    handle = None
    run_dir = os.path.join(args.workspace_root, "run-cancel-%d" % os.getpid())
    try:
        handle = volumes.create(scope)
        os.mkdir(run_dir)

        before = _lane_procs(args.uid)
        # The cancel is issued BY the web uid (this whole invocation runs as 65532).
        parent = subprocess.run(
            _drop_to_web([
                sys.executable, "-I", os.path.abspath(__file__), "--web-parent",
                "--caphelper", args.caphelper,
                "--tenant", tenant,
                "--workdir", run_dir,
                "--uid", str(args.uid), "--gid", str(args.gid),
                "--provider", args.provider, "--credential-handle", handle,
            ]),
            capture_output=True, timeout=int(_SETTLE_SECONDS + _EXIT_TIMEOUT_SECONDS + 60),
        )
        stdout = parent.stdout.decode("utf-8", "replace")
        sys.stdout.write(stdout)
        check(
            "web uid (zero eff caps) could SIGTERM the root launcher",
            "PARENT: KILL_OK" in stdout,
            "web-parent said: %r" % stdout.strip()[-200:],
        )
        check(
            "launcher exited promptly after the web parent's cancel",
            "PARENT: LAUNCHER_EXIT" in stdout,
            "web-parent said: %r" % stdout.strip()[-200:],
        )
        # Give the launcher's finally-teardown + workdir cleanup a moment to complete.
        for _ in range(50):
            if not os.path.exists(run_dir):
                break
            time.sleep(0.1)
        check(
            "run workdir removed by the launcher on cancel (no lane CWD leftover)",
            not os.path.exists(run_dir),
            "still present: %s" % run_dir,
        )
        # The entire child tree is gone: no lane-uid process the run introduced.
        after = _lane_procs(args.uid)
        introduced = [line for line in after if line not in before]
        check(
            "no lane-uid provider process survives the cancel",
            not introduced,
            "leftover lane procs: %r" % introduced,
        )
    finally:
        with_suppress(lambda: volumes.close(scope, handle) if handle else None)
        with_suppress(lambda: shutil.rmtree(store.entry_path(handle)) if handle else None)
        with_suppress(lambda: shutil.rmtree(run_dir) if os.path.exists(run_dir) else None)
    return failures


def with_suppress(call):
    try:
        call()
    except Exception:
        pass


def main(argv=None):
    parser = argparse.ArgumentParser(prog="neev_agent_cancel_acceptance")
    parser.add_argument("--web-parent", action="store_true",
                        help="internal: run the web-uid cancel step (spawned by the root harness)")
    parser.add_argument("--caphelper")
    parser.add_argument("--tenant")
    parser.add_argument("--store-root")
    parser.add_argument("--key-path")
    parser.add_argument("--workspace-root")
    parser.add_argument("--workdir")
    parser.add_argument("--credential-handle", dest="credential_handle")
    parser.add_argument("--provider", choices=("claude", "codex"))
    parser.add_argument("--uid", type=int, default=61000)
    parser.add_argument("--gid", type=int, default=61000)
    parser.add_argument("--allow-skip", action="store_true")
    args = parser.parse_args(argv)

    if args.web_parent:
        return web_parent(args)

    unmet = None
    if os.environ.get("NEEV_AGENT_CANCEL_ACCEPTANCE") != "1":
        unmet = "set NEEV_AGENT_CANCEL_ACCEPTANCE=1 (real helper cancel end-to-end)"
    elif platform.system() != "Linux":
        unmet = "Linux required"
    elif os.geteuid() != 0:
        unmet = "must run as root (root provisions the store; the cancel runs as the web uid)"
    elif not args.caphelper or not os.path.exists(args.caphelper):
        unmet = "caphelper not found at %s" % args.caphelper
    elif not os.path.exists(SETPRIV):
        unmet = "setpriv (util-linux) required"
    elif not (args.store_root and args.key_path and args.workspace_root and args.provider):
        unmet = "missing --store-root/--key-path/--workspace-root/--provider"
    if unmet is not None:
        print("UNMET:", unmet)
        return 0 if args.allow_skip else 2

    args.store_root = Path(args.store_root)
    args.key_path = Path(args.key_path)
    failures = run(args)
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
