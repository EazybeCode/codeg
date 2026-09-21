#!/usr/bin/env python3
"""PID 1 for the explicitly enabled agent container; never the default image CMD."""

import os
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path

DAEMON = "/opt/neev/scripts/operations/neev_agent_supervisor_main.py"
WEB = "/usr/local/bin/neev-server"
WEB_ID = 65532


def web_command():
    # Retain the bounding set for the fixed file-capability launch helper, while
    # the HTTP process itself receives no effective, ambient or inheritable caps.
    return [
        "/usr/bin/setpriv", "--reuid=65532", "--regid=65532", "--clear-groups",
        "--inh-caps=-all", "--ambient-caps=-all", WEB,
    ]


def wait_for_socket(process, path, stopping, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if stopping():
            raise RuntimeError("startup interrupted")
        if process.poll() is not None:
            raise RuntimeError("supervisor exited before readiness")
        try:
            info = path.lstat()
        except FileNotFoundError:
            time.sleep(0.05)
            continue
        if not stat.S_ISSOCK(info.st_mode) or info.st_uid != 0:
            raise RuntimeError("invalid control socket identity")
        if info.st_gid == WEB_ID and stat.S_IMODE(info.st_mode) == 0o660:
            return
        # bind() precedes the daemon's chown/chmod. Do not release the web process
        # during that short interval, or treat the interval as a startup failure.
        time.sleep(0.05)
    raise RuntimeError("supervisor readiness timed out")


def stop_children(children, grace=10):
    alive = [child for child in children if child.poll() is None]
    for child in alive:
        try:
            os.killpg(child.pid, signal.SIGINT)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + grace
    while any(child.poll() is None for child in alive) and time.monotonic() < deadline:
        time.sleep(0.05)
    for child in alive:
        if child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        child.wait()


def main():
    if sys.platform != "linux" or os.getuid() != 0 or os.getgid() != 0:
        raise RuntimeError("agent init requires Linux root 0:0")
    if os.environ.get("NEEV_RUNTIME_ENABLED") != "true":
        raise RuntimeError("agent init requires explicit runtime enablement")
    raw = os.environ.get("NEEV_SUPERVISOR_SOCKET", "")
    path = Path(raw)
    if not raw or not path.is_absolute():
        raise RuntimeError("missing absolute supervisor socket path")
    # The control directory is an ephemeral container mount. Refuse an existing
    # endpoint rather than mistake another/stale daemon's socket for readiness.
    if path.exists() or path.is_symlink():
        raise RuntimeError("control socket already exists")
    epoch_dir = os.environ.get("NEEV_SUPERVISOR_EPOCH_DIR", "")
    if not epoch_dir:
        raise RuntimeError("missing coordinator epoch directory")
    from neev_supervisor_deployment import provision_coordinator_epoch

    # These durable files belong to the unprivileged coordinator, separately
    # from the supervisor's private fence. Existing epochs are never reset.
    provision_coordinator_epoch(epoch_dir)
    stopping = False

    def stop(_signum, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    children = []
    try:
        daemon = subprocess.Popen(["/usr/bin/python3", DAEMON], start_new_session=True)
        children.append(daemon)
        wait_for_socket(daemon, path, lambda: stopping)
        if stopping:
            return 0
        web = subprocess.Popen(web_command(), start_new_session=True)
        children.append(web)
        while not stopping:
            for child in children:
                code = child.poll()
                if code is not None:
                    # An unexpected clean daemon exit still means the complete
                    # service is unavailable; let the container restart policy act.
                    return code if code > 0 else 1
            # As PID 1, collect exited descendants adopted when a provider's
            # parent exits. Poll the two managed children above first; if either
            # exits in this interval, stop the service rather than lose its status.
            while True:
                try:
                    reaped, _status = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if reaped == 0:
                    break
                if any(reaped == child.pid for child in children):
                    return 1
            time.sleep(0.1)
        return 0
    finally:
        stop_children(children)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError) as error:
        # Do not dump configuration, provider output or credentials on failure.
        print("neev agent container startup failed: " + type(error).__name__, file=sys.stderr)
        sys.exit(1)
