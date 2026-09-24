"""Deterministic stand-ins for the agent CLI and the extension.

No provider is ever invoked by the test suite: `FakeLauncher` replaces
`adapters.launch`, and the extension side is a plain function that answers
`tool.call` frames from a script.
"""

import io
import threading


class FakeStream(object):
    """A byte stream that yields queued lines and then ends."""

    def __init__(self, lines=()):
        self._lines = list(lines)
        self._closed = False

    def readline(self):
        if self._lines:
            return self._lines.pop(0)
        return b""

    def close(self):
        self._closed = True


class FakeProcess(object):
    def __init__(self, stdout_lines=()):
        self.terminated = False
        self.terminate_calls = 0
        self.stdout = FakeStream(stdout_lines)
        self.stderr = FakeStream()
        self.pid = -1
        self.pgid = -1
        self._exited = threading.Event()

    def poll(self):
        return 0 if self._exited.is_set() else None

    def wait(self, timeout=None):
        self._exited.wait(timeout)
        return 0

    def terminate_tree(self, grace=3.0):
        self.terminated = True
        self.terminate_calls += 1
        self._exited.set()

    def finish(self):
        """Simulate the CLI exiting on its own."""
        self._exited.set()


class FakeLauncher(object):
    """Drop-in for `adapters.launch` that never starts a real process."""

    def __init__(self, stdout_lines=(), fail_with=None):
        self.last = None
        self.calls = []
        self._stdout_lines = stdout_lines
        self._fail_with = fail_with

    def __call__(self, provider, paths, prompt, token, model=None):
        self.calls.append({"provider": provider, "prompt": prompt,
                           "token": token, "model": model, "paths": paths})
        if self._fail_with is not None:
            raise self._fail_with
        self.last = FakeProcess(self._stdout_lines)
        return self.last, {"binary": "/fake/%s" % provider,
                           "version": "fake 0.0.0", "argv": [provider]}


class FakeIpcServer(object):
    """Captures the handler instead of binding a socket."""

    instances = []

    def __init__(self, socket_path, token, handler):
        self.socket_path = socket_path
        self.token = token
        self.handler = handler
        self.started = False
        self.closed = False
        FakeIpcServer.instances.append(self)

    def start(self):
        self.started = True

    def close(self):
        self.closed = True


class RecordingOut(io.BytesIO):
    """A native-messaging output stream tests can read back."""

    def messages(self):
        from neev_browser.framing import read_message
        cursor = io.BytesIO(self.getvalue())
        out = []
        while True:
            message = read_message(cursor)
            if message is None:
                return out
            out.append(message)


def messages_of(out, kind):
    return [m for m in out.messages() if m.get("type") == kind]
