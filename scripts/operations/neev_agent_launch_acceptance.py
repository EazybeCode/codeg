#!/usr/bin/env python3
"""End-to-end acceptance for the WHOLE privileged launch path, driven through the
REAL caplaunch helper (never direct-root python), so a cap/mount/path mismatch
cannot hide behind a friendlier test harness.

It proves, on the pinned agent image, that:

  caplaunch (ambient caps, euid 65532)
    -> python launcher elevates to euid 0
    -> opens the caller's OWN LUKS credential volume (root-owned store)
    -> binds it PRIVATELY onto <workdir>/home in the sandbox mount namespace
    -> drops to the distinct unprivileged lane uid (setpriv, empty caps)
    -> runs the provider, which reads the bound HOME

Concretely it seeds a sentinel file inside a freshly created credential volume, then
launches `sh -c 'cat "$HOME/<sentinel>"'` as the lane uid via the caphelper and
checks the sentinel comes back — i.e. the lane uid really reached its bound HOME
(the 0700 secret store was NOT made world-traversable). A second launch runs the
real provider `--version` to prove the pinned binary executes inside the sandbox.

Image-specific paths (helper, launcher workspace root, store root/key) are PARAMETERS
because root owns them on the image; the exact final caphelper cap profile is used
(no FOWNER; MKNOD only if the image lacks /dev/mapper/control + /dev/loop-control).

Opt-in (never CI); root runs it as the WEB-SERVER uid on the canary:

    NEEV_AGENT_LAUNCH_ACCEPTANCE=1 sudo -u '#65532' python3 \
        neev_agent_launch_acceptance.py \
        --caphelper /usr/local/bin/neev-agent-caplaunch \
        --store-root /var/lib/neev-supervisor/credentials \
        --key-path /etc/neev-supervisor/credential-master.key \
        --workspace-root /data/agents --provider claude --uid 61000 --gid 61000

Exit 0 = the whole path worked; nonzero = a step failed (prints which) or a
prerequisite was unmet (so a skip can never look like a pass; --allow-skip forces 0).
"""

from __future__ import annotations

import argparse
import os
import platform
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_credential_store as store_module
import neev_credential_volumes as volumes_module

_SENTINEL_NAME = ".neev-acceptance-marker"
_SENTINEL_BODY = b"NEEV-LAUNCH-ACCEPTANCE-4f1c9a"
_TEST_ORG = 424242
_TEST_USER = 434343


def _run_via_helper(args, handle, run_dir, provider_argv):
    """Invoke the REAL caplaunch helper with the launcher's arguments."""
    argv = [
        args.caphelper,
        "--tenant", "org:%d/user:%d" % (_TEST_ORG, _TEST_USER),
        "--slot", "a",
        "--workdir", run_dir,
        "--uid", str(args.uid),
        "--gid", str(args.gid),
        "--provider", args.provider,
        "--credential-handle", handle,
        "--", *provider_argv,
    ]
    # The helper clears and sets its own env; nothing here is inherited.
    return subprocess.run(argv, capture_output=True, timeout=120)


def run(args):
    failures = []

    def check(name, ok, detail=""):
        print(("PASS" if ok else "FAIL"), name, ("- " + detail) if detail and not ok else "")
        if not ok:
            failures.append(name)

    store = store_module.CredentialStore(args.store_root, args.key_path)
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    scope = store_module.Scope(_TEST_ORG, _TEST_USER, args.provider)

    handle = None
    try:
        handle = volumes.create(scope)
        # Seed a sentinel INSIDE the volume so we can prove the lane uid reaches the
        # bound HOME. Open the volume, write the marker under the mount, close it.
        mountpoint = volumes.open(scope, handle)
        marker = os.path.join(str(mountpoint), _SENTINEL_NAME)
        with open(marker, "wb") as handle_file:
            handle_file.write(_SENTINEL_BODY)
        os.chmod(marker, 0o600)
        volumes.close(scope, handle)

        run_dir = os.path.join(args.workspace_root, "run-acceptance-%d" % os.getpid())
        os.mkdir(run_dir)

        # 1. The lane uid reaches its bound HOME (proves euid0 -> open -> bind -> drop).
        read = _run_via_helper(
            args, handle, run_dir, ["sh", "-c", 'cat "$HOME/%s"' % _SENTINEL_NAME]
        )
        check(
            "bound HOME readable by the lane uid via the real caphelper",
            read.returncode == 0 and _SENTINEL_BODY in read.stdout,
            "rc=%d stderr=%r" % (read.returncode, read.stderr[:200]),
        )

        # 2. The real pinned provider binary runs inside the sandbox.
        version = _run_via_helper(args, handle, run_dir, [args.provider, "--version"])
        check(
            "pinned %s --version runs inside the sandbox" % args.provider,
            version.returncode == 0,
            "rc=%d stderr=%r" % (version.returncode, version.stderr[:200]),
        )
    finally:
        if handle is not None:
            try:
                volumes.close(scope, handle)
            except Exception:
                pass
    return failures


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
    elif not os.path.exists(args.caphelper):
        unmet = "caphelper not found at %s" % args.caphelper
    if unmet is not None:
        print("UNMET:", unmet)
        return 0 if args.allow_skip else 2

    from pathlib import Path

    args.store_root = Path(args.store_root)
    args.key_path = Path(args.key_path)
    failures = run(args)
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
