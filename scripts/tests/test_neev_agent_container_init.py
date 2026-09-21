import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "operations"))
import neev_agent_container_init as init


def test_failed_daemon_prevents_web_start(tmp_path):
    process = subprocess.Popen([sys.executable, "-c", "raise SystemExit(3)"])
    process.wait()
    with pytest.raises(RuntimeError, match="exited before readiness"):
        init.wait_for_socket(process, tmp_path / "control.sock", lambda: False)


def test_regular_file_cannot_impersonate_ready_socket(tmp_path):
    path = tmp_path / "control.sock"
    path.write_text("not a socket")
    process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                               start_new_session=True)
    try:
        with pytest.raises(RuntimeError, match="invalid control socket"):
            init.wait_for_socket(process, path, lambda: False)
    finally:
        init.stop_children([process], grace=0.2)


def test_shutdown_kills_and_reaps_child_ignoring_interrupt(tmp_path):
    ready = tmp_path / "ready"
    process = subprocess.Popen(
        [sys.executable, "-c",
         "import signal,time,pathlib,sys; signal.signal(signal.SIGINT,signal.SIG_IGN); "
         "pathlib.Path(sys.argv[1]).touch(); time.sleep(30)", str(ready)],
        start_new_session=True,
    )
    try:
        import time
        deadline = time.monotonic() + 5
        while not ready.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert ready.exists()
        init.stop_children([process], grace=0.05)
        assert process.returncode == -signal.SIGKILL
        with pytest.raises(ProcessLookupError):
            os.kill(process.pid, 0)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
