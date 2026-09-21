#!/usr/bin/env python3
"""End-to-end acceptance for the WHOLE privileged launch path, driven through the
REAL caplaunch helper, so a cap/mount/path mismatch cannot hide behind a friendlier
harness.

It proves, on the pinned agent image, that:

  (web uid 65532, bounding caps retained, no-new-privs OFF)
    -> exec caplaunch, whose FILE capabilities raise on exec (this is the profile
       under test — running as root would bypass it)
    -> python launcher elevates to euid 0
    -> opens the caller's OWN LUKS credential volume (root-owned store)
    -> binds it PRIVATELY onto <workdir>/home in the sandbox mount namespace
    -> drops to the distinct unprivileged lane uid (setpriv, empty caps)
    -> runs the provider, which reads the bound HOME

Root provisions the fixture (store, key, a freshly created + sentinel-seeded LUKS
volume — a web-uid process cannot touch the root-owned store), but EACH ACTUAL helper
invocation is wrapped in `setpriv --reuid=65532 --regid=65532 --clear-groups` (no
--no-new-privs, no bounding-set change) so the caphelper's file-cap raise is what
grants privilege — exactly the deployed path. A preflight asserts the wrapper really
lands at uid 65532 before the helper runs. The test volume is revoked on cleanup.

Run it as ROOT inside the pinned image with the EXACT 7-cap profile + explicit device
nodes + network none:

    NEEV_AGENT_LAUNCH_ACCEPTANCE=1 python3 neev_agent_launch_acceptance.py \
        --caphelper /usr/local/bin/neev-agent-caplaunch \
        --store-root /var/lib/neev-supervisor/credentials \
        --key-path /etc/neev-supervisor/credential-master.key \
        --workspace-root /data/agents --provider claude --uid 61000 --gid 61000

Exit 0 = the whole path worked; nonzero = a step failed (prints which) or a
prerequisite was unmet (--allow-skip forces 0)."""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module

SETPRIV = "/usr/bin/setpriv"
_SENTINEL_NAME = ".neev-acceptance-marker"
_SENTINEL_BODY = b"NEEV-LAUNCH-ACCEPTANCE-4f1c9a"
_WEB_UID = 65532
_TEST_ORG = 424242
_TEST_USER = 434343


def _drop_to_web(argv):
    """Wrap argv so it runs as the web uid with bounding caps retained and
    no-new-privs OFF — so an exec'd caphelper's FILE capabilities take effect."""
    return [
        SETPRIV, "--reuid=%d" % _WEB_UID, "--regid=%d" % _WEB_UID, "--clear-groups",
        *argv,
    ]


_run_counter = 0


def _fresh_run_dir(args):
    global _run_counter
    _run_counter += 1
    run_dir = os.path.join(args.workspace_root, "run-acc-%d-%d" % (os.getpid(), _run_counter))
    os.mkdir(run_dir)
    return run_dir


def _via_helper(args, tenant, handle, provider_argv):
    """Create a fresh run workdir, invoke the REAL caphelper (dropped to the web uid),
    and return (result, run_dir). The launcher removes run_dir on success."""
    run_dir = _fresh_run_dir(args)
    launcher_args = [
        args.caphelper,
        "--tenant", tenant,
        "--slot", "a",
        "--workdir", run_dir,
        "--uid", str(args.uid),
        "--gid", str(args.gid),
        "--provider", args.provider,
        "--credential-handle", handle,
        "--", *provider_argv,
    ]
    result = subprocess.run(_drop_to_web(launcher_args), capture_output=True, timeout=180)
    return result, run_dir


def run(args):
    failures = []

    def check(name, ok, detail=""):
        print(("PASS" if ok else "FAIL"), name, ("- " + detail) if (detail and not ok) else "")
        if not ok:
            failures.append(name)

    # Preflight: the wrapper truly lands at the web uid (so we test the file-cap
    # path, not accidental root privilege).
    probe = subprocess.run(_drop_to_web(["id", "-u"]), capture_output=True, timeout=30)
    check(
        "preflight: helper invocation starts at web uid %d" % _WEB_UID,
        probe.returncode == 0 and probe.stdout.strip() == str(_WEB_UID).encode(),
        "got %r" % probe.stdout.strip(),
    )
    if failures:
        return failures  # do not run the helper if the drop is wrong

    store = store_module.CredentialStore(args.store_root, args.key_path)
    if not args.key_path.exists():
        store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    tenant_a = "org:%d/user:%d" % (_TEST_ORG, _TEST_USER)
    tenant_b = "org:%d/user:%d" % (_TEST_ORG, _TEST_USER + 1)
    scope_a = store_module.Scope(_TEST_ORG, _TEST_USER, args.provider)
    scope_b = store_module.Scope(_TEST_ORG, _TEST_USER + 1, args.provider)

    handles = {}
    try:
        # Provision two volumes (two tenants) + seed a sentinel in A's.
        for scope in (scope_a, scope_b):
            handles[scope] = volumes.create(scope)
        mp = volumes.open(scope_a, handles[scope_a])
        marker = os.path.join(str(mp), _SENTINEL_NAME)
        with open(marker, "wb") as sink:
            sink.write(_SENTINEL_BODY)
        os.chmod(marker, 0o600)
        volumes.close(scope_a, handles[scope_a])

        # 1. The lane uid reaches its bound HOME (euid0 -> open -> bind -> drop).
        read, _rd = _via_helper(args, tenant_a, handles[scope_a],
                                ["sh", "-c", 'cat "$HOME/%s"' % _SENTINEL_NAME])
        check(
            "bound HOME readable by the lane uid via the real caphelper",
            read.returncode == 0 and _SENTINEL_BODY in read.stdout,
            "rc=%d stderr=%r" % (read.returncode, read.stderr[-300:]),
        )
        # 2. The pinned provider binary runs inside the sandbox (network none).
        version, _rd = _via_helper(args, tenant_a, handles[scope_a], [args.provider, "--version"])
        check(
            "pinned %s --version runs inside the sandbox" % args.provider,
            version.returncode == 0,
            "rc=%d stderr=%r" % (version.returncode, version.stderr[-300:]),
        )
        # 3. No CWD leak across two tenants sharing the SAME lane uid: tenant A writes
        #    a sentinel into its run CWD; the launcher must remove A's run dir, so a
        #    later run reusing the uid cannot read it. Then tenant B (same uid, other
        #    volume) must NOT see A's file.
        leak_body = "LEAK-%d" % os.getpid()
        a_run, a_dir = _via_helper(args, tenant_a, handles[scope_a],
                                   ["sh", "-c", 'printf %s "%s" > "$PWD/leak.txt"' % ("%s", leak_body)])
        check(
            "run A completes and its run dir is removed by the launcher (no CWD leftover)",
            a_run.returncode == 0 and not os.path.exists(a_dir),
            "rc=%d exists=%s" % (a_run.returncode, os.path.exists(a_dir)),
        )
        b_run, b_dir = _via_helper(args, tenant_b, handles[scope_b],
                                   ["sh", "-c", 'ls -a "$PWD"; test ! -e "$PWD/leak.txt"'])
        check(
            "tenant B (same lane uid) sees no leftover CWD file from tenant A",
            b_run.returncode == 0 and b"leak.txt" not in b_run.stdout,
            "rc=%d stdout=%r" % (b_run.returncode, b_run.stdout[-200:]),
        )
    finally:
        for scope, handle in handles.items():
            with_suppress(lambda h=handle: volumes.close(scope, h))
            with_suppress(lambda h=handle: shutil.rmtree(store.entry_path(h)))
    return failures


def with_suppress(call):
    try:
        call()
    except Exception:
        pass


def main(argv=None):
    parser = argparse.ArgumentParser(prog="neev_agent_launch_acceptance")
    parser.add_argument("--caphelper", required=True)
    parser.add_argument("--store-root", required=True)
    parser.add_argument("--key-path", required=True)
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--provider", required=True, choices=("claude", "codex"))
    parser.add_argument("--uid", type=int, default=61000)
    parser.add_argument("--gid", type=int, default=61000)
    parser.add_argument("--allow-skip", action="store_true")
    args = parser.parse_args(argv)

    unmet = None
    if os.environ.get("NEEV_AGENT_LAUNCH_ACCEPTANCE") != "1":
        unmet = "set NEEV_AGENT_LAUNCH_ACCEPTANCE=1 (real helper end-to-end)"
    elif platform.system() != "Linux":
        unmet = "Linux required"
    elif os.geteuid() != 0:
        unmet = "must run as root (root provisions the store; helper runs are dropped to web uid)"
    elif not os.path.exists(args.caphelper):
        unmet = "caphelper not found at %s" % args.caphelper
    elif not os.path.exists(SETPRIV):
        unmet = "setpriv (util-linux) required"
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
