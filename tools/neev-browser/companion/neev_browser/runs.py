"""Per-run directories. Everything a run produces is kept, outside /tmp.

The directory is created fresh and private: a collision is an error rather
than a reuse, and every file the run writes is opened with O_NOFOLLOW so a
pre-placed symlink cannot redirect a log, the MCP config, or the socket.
"""

import errno
import json
import os
import re
import stat

DEFAULT_BASE_DIR = os.path.join(os.path.expanduser("~"), ".neev", "browser-runs")
_RUN_ID_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")


class RunDirError(Exception):
    pass


class RunPaths(object):
    def __init__(self, root):
        self.root = root
        self.run_json = os.path.join(root, "run.json")
        self.transcript = os.path.join(root, "transcript.jsonl")
        self.stdout_log = os.path.join(root, "agent.stdout.log")
        self.stderr_log = os.path.join(root, "agent.stderr.log")
        self.mcp_config = os.path.join(root, "mcp-config.json")
        self.files = os.path.join(root, "files")
        self.socket = os.path.join(root, "ipc.sock")


def _ensure_private_dir(path):
    """Create `path` 0700 if absent; if present, insist it is ours and safe."""
    try:
        os.mkdir(path, 0o700)
        return
    except OSError as exc:
        if exc.errno != errno.EEXIST:
            raise
    info = os.lstat(path)
    if stat.S_ISLNK(info.st_mode):
        raise RunDirError("%s is a symlink; refusing to use it" % path)
    if not stat.S_ISDIR(info.st_mode):
        raise RunDirError("%s exists and is not a directory" % path)
    if info.st_uid != os.getuid():
        raise RunDirError("%s is not owned by this user" % path)
    if info.st_mode & 0o077:
        raise RunDirError("%s is group- or world-accessible" % path)


def create_run(run_id, base_dir=None):
    if not _RUN_ID_RE.match(run_id or ""):
        raise ValueError("unusable run id %r" % (run_id,))
    base = base_dir or DEFAULT_BASE_DIR
    parent = os.path.dirname(base)
    if parent and not os.path.isdir(parent):
        os.makedirs(parent, mode=0o700)
    _ensure_private_dir(base)
    root = os.path.join(base, run_id)
    try:
        os.mkdir(root, 0o700)
    except OSError as exc:
        if exc.errno == errno.EEXIST:
            raise RunDirError("run directory %s already exists" % root)
        raise
    paths = RunPaths(root)
    os.mkdir(paths.files, 0o700)
    return paths


def open_private(path, append=False):
    """Open a run file for writing without ever following a symlink."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW
    flags |= os.O_APPEND if append else os.O_TRUNC
    return os.fdopen(os.open(path, flags, 0o600), "a" if append else "w")


def open_private_binary_append(path):
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW
    return os.fdopen(os.open(path, flags, 0o600), "ab", 0)


def write_run_json(paths, payload):
    with open_private(paths.run_json) as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def append_transcript(paths, entry):
    with open_private(paths.transcript, append=True) as handle:
        handle.write(json.dumps(entry, sort_keys=True) + "\n")
