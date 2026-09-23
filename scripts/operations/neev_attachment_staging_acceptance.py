#!/usr/bin/env python3
"""Prove a staged attachment is readable by the DROPPED run uid, and by nobody else.

The staging layout is decided in Rust (`agents::execution::stage_attachments`)
and its safety rests entirely on a claim about uids that a host-side test cannot
check: the files are written by the SERVER uid, and the process that has to read
them runs as a different, unprivileged run uid after the launcher drops
privileges. A test that reads them back as the writing user proves nothing.

So this forks, drops to a real uid, and reads. It must run as root on Linux —
anywhere else it skips loudly rather than passing vacuously.

    sudo python3 neev_attachment_staging_acceptance.py

Layout under test, mirroring `stage_attachments`:

    <run root>/                      0700, owned by the RUN uid   <- the boundary
    <run root>/neev-attachments/            0755, owned by the SERVER uid
    <run root>/neev-attachments/<call>/     0755, owned by the SERVER uid
    <run root>/neev-attachments/<call>/<key>--<name>   0644, SERVER uid

The child directory is deliberately 0755. A 0700 directory owned by the server
uid would block the run uid's traversal outright, and the 0644 files inside
would be unreachable however open their own mode was. Protection comes from the
0700 run root above it, which is what this asserts.
"""

import os
import shutil
import sys
import tempfile

ATTACHMENT_DIR = "neev-attachments"
# Two uids that are not root and not each other. `nobody` exists everywhere;
# the second is a stand-in for a different tenant's lane.
RUN_UID = 65534
OTHER_UID = 65533


def _skip(reason):
    print(f"SKIP: {reason}")
    return 0


def _read_as(uid, path):
    """Read `path` in a child that has dropped to `uid`. Returns (ok, detail)."""
    readable, writable = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(readable)
        try:
            os.setgroups([])
            os.setgid(uid)
            os.setuid(uid)
            if os.getuid() != uid:
                os._exit(3)
            with open(path, "rb") as handle:
                handle.read()
            os.write(writable, b"ok")
            os._exit(0)
        except PermissionError:
            os._exit(1)
        except OSError:
            os._exit(2)
        finally:
            os.close(writable)
    os.close(writable)
    payload = os.read(readable, 16)
    os.close(readable)
    _, status = os.waitpid(pid, 0)
    return os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0 and payload == b"ok", status


def main():
    if sys.platform != "linux":
        return _skip(f"needs Linux uid semantics, not {sys.platform}")
    if os.geteuid() != 0:
        return _skip("needs root to drop to a run uid")

    root = tempfile.mkdtemp(prefix="neev-staging-")
    try:
        # Stable root, then a per-call child: the real layout stages each run
        # into its own directory so two runs sharing a workspace cannot collide.
        staging_root = os.path.join(root, ATTACHMENT_DIR)
        os.mkdir(staging_root)
        directory = os.path.join(staging_root, "b3d1f0c2a9e44f7d8c1a2b3c4d5e6f70")
        os.mkdir(directory)
        name = "11111111-1111-4111-8111-111111111111--notes.txt"
        path = os.path.join(directory, name)
        # Written under a hostile umask, exactly as a server at 077 would: the
        # creating mode is masked, so only an explicit chmod decides the result.
        previous = os.umask(0o077)
        try:
            with open(path, "wb") as handle:
                handle.write(b"attached bytes")
        finally:
            os.umask(previous)

        # Exactly what the Rust does: child 0755, files 0644, both still owned
        # by the writing (server) uid. Only the run root changes hands.
        os.chmod(staging_root, 0o755)
        os.chmod(directory, 0o755)
        os.chmod(path, 0o644)
        os.chmod(root, 0o700)
        os.chown(root, RUN_UID, RUN_UID)

        ok, status = _read_as(RUN_UID, path)
        if not ok:
            print(f"FAIL: the dropped run uid {RUN_UID} could not read its own staged file (status {status})")
            return 1
        print(f"PASS: run uid {RUN_UID} read the staged file through a 0700 root it owns")

        # And the boundary actually bounds: another unprivileged uid cannot
        # traverse the run root, so the 0644 file is unreachable to it.
        leaked, _ = _read_as(OTHER_UID, path)
        if leaked:
            print(f"FAIL: uid {OTHER_UID} read another run's staged file")
            return 1
        print(f"PASS: uid {OTHER_UID} was refused by the 0700 run root")

        # The staged leaf is key-prefixed, so an upload cannot land under a name
        # an agent reads as configuration.
        if os.path.exists(os.path.join(root, "AGENTS.md")) or os.path.exists(
            os.path.join(staging_root, "AGENTS.md")
        ):
            print("FAIL: an attachment landed under a configuration name")
            return 1
        print("PASS: nothing is staged under a bare configuration name")
        return 0
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
