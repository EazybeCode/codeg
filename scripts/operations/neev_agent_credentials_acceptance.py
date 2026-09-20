#!/usr/bin/env python3
"""Linux, root-only acceptance for the serialized per-scope credential ownership
transfer (`neev_agent_credentials.deliver`). Proves, with REAL uids (via setpriv)
and a REAL chown, the properties root asked to see before accepting the design:

  1. LEND       — during the run the volume is readable by the run's lane uid.
  2. ISOLATION  — no OTHER lane uid can read it while it is lent (0700, single owner)
                  and a second run for the SAME scope is refused BUSY (lock held).
  3. RESTORE    — after the run the canonical owner (65532) is restored; the lane
                  uid can no longer read it.
  4. REFRESH    — a token the run WRITES survives and becomes owner-readable again
                  (so the supervisor reads refreshed credentials next time).
  5. RECOVERY   — a crash-interrupted lend (tree left owned by a lane uid) is
                  repaired to canonical under the lock on the next lend.
  6. PERSISTENCE— delivery never deletes the volume's contents.

Not run in CI (needs root + spare uids + setpriv). Root runs it on the canary:
    sudo python3 neev_agent_credentials_acceptance.py [--uid-a 61000 --uid-b 61001]
Exit 0 = all properties held; nonzero = a property failed (prints which).
"""

from __future__ import annotations

import argparse
import contextlib
import os
import platform
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_credentials as credentials

CANON = credentials.CANONICAL_OWNER_UID
SETPRIV = "/usr/bin/setpriv"


class _Volume:
    """Minimal stand-in for CredentialVolumes.open: returns a fixed, already-mounted
    directory. The real LUKS layer has its own root acceptance
    (LinuxCredentialVolumeAcceptance); this isolates the ownership-transfer layer."""

    def __init__(self, path):
        self._path = path

    def open(self, scope, handle):
        return self._path


def _can_read_as(uid, path):
    """True iff a process dropped to `uid` can read `path`."""
    result = subprocess.run(
        [SETPRIV, "--reuid=%d" % uid, "--regid=%d" % uid, "--clear-groups",
         "cat", path],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0


def _write_as(uid, path, content):
    """True iff a process dropped to `uid` can create+write `path`."""
    result = subprocess.run(
        [SETPRIV, "--reuid=%d" % uid, "--regid=%d" % uid, "--clear-groups",
         "sh", "-c", "printf %s \"$1\" > \"$2\"", "sh", content, path],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0



def run(uid_a, uid_b):
    failures = []

    def check(name, ok):
        print(("PASS" if ok else "FAIL"), name)
        if not ok:
            failures.append(name)

    tmp = tempfile.mkdtemp()
    # mkdtemp is 0700 root-owned; a lane uid could not even TRAVERSE it to reach the
    # volume, masking the real test. The isolation boundary is the volume's own 0700,
    # not the scratch parent, so make the parent world-traversable (0755) and keep
    # the volume 0700 (exactly as the production document/credential roots are laid
    # out: 0755 ancestry, 0700 tenant dir).
    os.chmod(tmp, 0o755)
    lock_dir = os.path.join(tmp, "locks")
    vol = os.path.join(tmp, "vol")
    os.mkdir(vol, 0o700)
    os.chmod(vol, 0o700)  # exact, independent of umask
    token = os.path.join(vol, "token")
    with open(token, "w") as handle:
        handle.write("SECRET-CRED")
    os.chmod(token, 0o600)
    # Rest at the canonical owner (as the supervisor leaves it).
    credentials._chown_tree(vol, CANON, CANON)
    volumes = _Volume(vol)

    def deliver(run_uid):
        return credentials.deliver(
            volumes, "org:3/user:7", "claude", "h1", run_uid, run_uid,
            lock_dir=lock_dir,
        )

    # 1+2: lend to A; A can read, B cannot; a second same-scope run is BUSY.
    with deliver(uid_a):
        check("lend: run uid can read its credentials", _can_read_as(uid_a, token))
        check("isolation: another lane uid cannot read", not _can_read_as(uid_b, token))
        # A concurrent run for the SAME scope must not get a second lend.
        saved = credentials.RUN_LOCK_TIMEOUT_SECONDS
        credentials.RUN_LOCK_TIMEOUT_SECONDS = 0.3
        busy = False
        try:
            with deliver(uid_b):
                pass
        except credentials.CredentialError as error:
            busy = error.category == "busy"
        finally:
            credentials.RUN_LOCK_TIMEOUT_SECONDS = saved
        check("isolation: concurrent same-scope run is refused busy", busy)
        # 4: a refresh the run writes must survive.
        check("refresh: run uid can write a token", _write_as(uid_a, os.path.join(vol, "refreshed"), "NEW"))

    # 3: restored to canonical; the lane uid can no longer read.
    check("restore: canonical owner restored", os.stat(vol).st_uid == CANON)
    check("restore: lane uid can no longer read", not _can_read_as(uid_a, token))
    check("refresh: refreshed token is owner-readable", _can_read_as(CANON, os.path.join(vol, "refreshed")))

    # 5: recovery — simulate a crash that left the tree owned by a lane uid.
    credentials._chown_tree(vol, uid_b, uid_b)
    with deliver(uid_a):
        check("recovery: lend succeeds after a crash-left owner", _can_read_as(uid_a, token))
    check("recovery: canonical restored afterwards", os.stat(vol).st_uid == CANON)

    # 6: persistence — the credential file was never deleted.
    check("persistence: credentials preserved", os.path.exists(token))

    with contextlib.suppress(Exception):
        credentials._chown_tree(vol, os.geteuid(), os.getegid())
        subprocess.run(["rm", "-rf", tmp], check=False)
    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(prog="neev_agent_credentials_acceptance")
    parser.add_argument("--uid-a", type=int, default=61000)
    parser.add_argument("--uid-b", type=int, default=61001)
    parser.add_argument(
        "--allow-skip",
        action="store_true",
        help="exit 0 (not an acceptance) when Linux/root/setpriv are absent; without"
        " this, an unmet prerequisite is a FAILURE so a skip can never look like a pass",
    )
    args = parser.parse_args(argv)
    # This is an acceptance run, not a portable unit test: an unmet prerequisite is
    # reported as UNMET and exits NONZERO so it can never masquerade as ACCEPTED.
    unmet = None
    if platform.system() != "Linux":
        unmet = "Linux required"
    elif os.geteuid() != 0:
        unmet = "must run as root (chown + setpriv to spare uids; grant DAC_OVERRIDE"
        unmet += " to match helper caps)"
    elif not os.path.exists(SETPRIV):
        unmet = "setpriv (util-linux) required"
    if unmet is not None:
        print("UNMET:", unmet)
        return 0 if args.allow_skip else 2
    failures = run(args.uid_a, args.uid_b)
    print("RESULT:", "ACCEPTED" if not failures else "FAILED %d" % len(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
