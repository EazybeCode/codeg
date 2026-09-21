#!/usr/bin/env python3
"""In-namespace mount prelude: bind the delivered credential volume onto the run's
private HOME, then exec the (still-privileged-but-about-to-drop) rest of the launch
chain.

The agent sandbox unshares a fresh MOUNT namespace and runs THIS as root, BEFORE
`setpriv` drops to the unprivileged lane uid. The credential volume is mounted by
the launcher under the 0700 root-owned secret credential store, whose ancestry the
lane uid cannot traverse. Here we bind that mount onto `<workdir>/home` — a path
whose ancestry the lane uid *can* traverse — INSIDE the private mount namespace, so:

  * the secret store is never chmod'd world-traversable, and
  * the bind is invisible to every other process/namespace (we make `/` rprivate
    first, so it cannot propagate out).

We then `execv` the rest of the argv (`prlimit ... setpriv ... provider`), which
drops to the lane uid with HOME pointing at the bind. Fixed and argv-driven: it
validates every path before any mount and performs no other effect. Linux only
(the caller only invokes it inside a Linux mount namespace); the mount logic is
injected for portable unit tests.
"""

from __future__ import annotations

import os
import stat
import sys

# linux/fs.h mount flags.
MS_BIND = 0x1000
MS_REC = 0x4000
MS_PRIVATE = 0x40000

_MAX_PATH_COMPONENTS = 33  # a run workdir (<=32) plus the fixed "home" leaf


class PreludeError(Exception):
    """A fixed, non-sensitive prelude failure (never carries a path)."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


def _valid_path(path: object) -> bool:
    if not isinstance(path, str) or not path.startswith("/") or "\x00" in path:
        return False
    parts = [c for c in path.split("/") if c != ""]
    if not parts or len(parts) > _MAX_PATH_COMPONENTS:
        return False
    if any(part in (".", "..") for part in parts):
        return False
    return "/" + "/".join(parts) == path.rstrip("/") and not path.endswith("/")


def _parse(argv):
    """argv := --src <SRC> --dst <DST> -- <REST...> (REST non-empty)."""
    if len(argv) < 6 or argv[0] != "--src" or argv[2] != "--dst" or argv[4] != "--":
        raise PreludeError("usage")
    src, dst, rest = argv[1], argv[3], argv[5:]
    if not rest:
        raise PreludeError("usage")
    if not _valid_path(src) or not _valid_path(dst):
        raise PreludeError("path")
    return src, dst, rest


def _require_real_dir(path, lstat_fn):
    try:
        info = lstat_fn(path)
    except OSError as error:
        raise PreludeError("stat") from error
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        raise PreludeError("shape")


def run(argv, *, mount_fn, exec_fn, lstat_fn=os.lstat):
    """Validate paths, make `/` rprivate, bind SRC→DST, then exec REST. Every failure
    is fatal and non-sensitive; nothing is bound if any check fails."""
    src, dst, rest = _parse(argv)
    # Both endpoints must be real, non-symlink directories (defence in depth: the
    # launcher already computes trusted values, but never bind through a swapped link).
    _require_real_dir(src, lstat_fn)
    _require_real_dir(dst, lstat_fn)
    # Make the whole tree private FIRST so the bind cannot propagate to any peer
    # namespace, then bind (recursively, to carry any submounts) read-write so the
    # provider can persist refreshed credentials.
    mount_fn(None, "/", None, MS_REC | MS_PRIVATE, None)
    mount_fn(src, dst, None, MS_REC | MS_BIND, None)
    exec_fn(rest[0], rest)


def _real_mount(source, target, fstype, flags, data):  # pragma: no cover - Linux only
    import ctypes

    libc = ctypes.CDLL(None, use_errno=True)

    def _encode(value):
        return None if value is None else os.fsencode(value)

    result = libc.mount(_encode(source), _encode(target), _encode(fstype), flags,
                        _encode(data))
    if result != 0:
        raise PreludeError("mount")


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    try:
        run(argv, mount_fn=_real_mount, exec_fn=os.execv)
    except PreludeError as error:
        sys.stderr.write("neev_agent_ns_prelude: %s\n" % error.code)
        return 2
    # execv only returns on failure.
    return 3


if __name__ == "__main__":
    raise SystemExit(main())
