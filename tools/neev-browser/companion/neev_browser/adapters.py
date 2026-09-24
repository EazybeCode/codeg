"""Launching the user's own Claude Code / Codex CLI, scoped to one run.

Nothing global is written: no `~/.claude.json`, no `~/.codex/config.toml`, no
`claude mcp add`. Each run gets its own MCP config and its own launcher script,
and the IPC token lives in a 0600 file that is deleted when the run ends — so
it never appears in `ps` output or in a preserved artifact.
"""

import json
import os
import signal
import subprocess
import sys
import time

from .runs import open_private
from .scope import ALLOWED_TOOLS

MCP_SERVER_NAME = "neev_browser"
PACKAGE_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

BANNED_FLAGS = ("--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions",
                "bypassPermissions",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                "--full-auto",
                "danger-full-access")

# Tried in order; the first one that answers `--version` wins. The PATH entry
# is not assumed to be the working one.
BINARY_CANDIDATES = {
    "claude": ("$NEEV_BROWSER_CLAUDE_BIN",
               "~/.local/bin/claude",
               "claude",
               "/usr/local/bin/claude"),
    "codex": ("$NEEV_BROWSER_CODEX_BIN",
              "~/.local/bin/codex",
              "codex",
              "/Applications/ChatGPT.app/Contents/Resources/codex"),
}


class ProviderError(Exception):
    pass


def qualified_tool_names():
    return ["mcp__%s__%s" % (MCP_SERVER_NAME, name) for name in ALLOWED_TOOLS]


def _candidate_paths(provider, environ):
    for candidate in BINARY_CANDIDATES[provider]:
        if candidate.startswith("$"):
            value = environ.get(candidate[1:])
            if value:
                yield value
        elif candidate.startswith("~"):
            yield os.path.expanduser(candidate)
        elif os.sep in candidate:
            yield candidate
        else:
            from shutil import which
            found = which(candidate, path=environ.get("PATH"))
            if found:
                yield found


def resolve_binary(provider, environ=None, probe=None):
    """Return (path, version) for the first candidate that actually runs."""
    if provider not in BINARY_CANDIDATES:
        raise ProviderError("unknown provider %r" % (provider,))
    environ = os.environ if environ is None else environ
    probe = probe or _probe_version
    tried = []
    for path in _candidate_paths(provider, environ):
        if path in tried:
            continue
        tried.append(path)
        version = probe(path)
        if version is not None:
            return path, version
    raise ProviderError(
        "no working %s CLI found. Tried: %s. Install it and sign in, or set "
        "NEEV_BROWSER_%s_BIN to its full path."
        % (provider, ", ".join(tried) or "(nothing)", provider.upper()))


def _probe_version(path):
    try:
        result = subprocess.run([path, "--version"], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return result.stdout.decode("utf-8", "replace").strip()[:200]


def write_launcher(paths, token, python_exe=None):
    """Write the per-run MCP launcher and the 0600 token file it reads."""
    token_file = os.path.join(paths.root, "agent-token")
    with open_private(token_file) as handle:
        handle.write(token)
    launcher = os.path.join(paths.root, "mcp-launch.sh")
    with open_private(launcher) as handle:
        handle.write(
            "#!/bin/sh\n"
            "# Generated per run by Neev Browser. Safe to delete with the run.\n"
            "set -eu\n"
            'NEEV_BROWSER_SOCKET=%s; export NEEV_BROWSER_SOCKET\n'
            'NEEV_BROWSER_TOKEN=$(cat %s); export NEEV_BROWSER_TOKEN\n'
            'PYTHONPATH=%s; export PYTHONPATH\n'
            'exec %s -m neev_browser.mcp_server\n'
            % (_sh_quote(paths.socket), _sh_quote(token_file),
               _sh_quote(PACKAGE_ROOT), _sh_quote(python_exe or sys.executable)))
    os.chmod(launcher, 0o700)
    return launcher


def discard_token(paths):
    """Delete the run's IPC credential. The rest of the run is preserved."""
    for name in ("agent-token",):
        path = os.path.join(paths.root, name)
        if os.path.exists(path):
            try:
                os.unlink(path)
            except OSError:
                pass


def _sh_quote(value):
    return "'" + str(value).replace("'", "'\\''") + "'"


def build_mcp_config(launcher):
    return {"mcpServers": {MCP_SERVER_NAME: {"command": launcher, "args": []}}}


def write_mcp_config(paths, launcher):
    with open_private(paths.mcp_config) as handle:
        json.dump(build_mcp_config(launcher), handle, indent=2, sort_keys=True)
        handle.write("\n")


def claude_argv(binary, paths, prompt, model=None):
    argv = [binary, "--print",
            "--output-format", "stream-json", "--verbose",
            "--mcp-config", paths.mcp_config,
            "--strict-mcp-config",
            "--tools", "",
            "--allowedTools", ",".join(qualified_tool_names()),
            "--permission-mode", "default",
            "--no-session-persistence"]
    if model:
        argv += ["--model", model]
    return argv + ["--", prompt]


def codex_argv(binary, paths, prompt, launcher, model=None):
    argv = [binary, "exec", "--json",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ephemeral",
            "--sandbox", "read-only",
            "--color", "never",
            "-C", paths.files,
            "-c", "mcp_servers.%s.command=%s" % (MCP_SERVER_NAME, _toml(launcher))]
    if model:
        argv += ["--model", model]
    return argv + ["--", prompt]


def _toml(value):
    return '"%s"' % str(value).replace("\\", "\\\\").replace('"', '\\"')


class AgentProcess(object):
    """A CLI running as the leader of its own process group.

    The group id is captured at launch, not looked up at kill time: once the
    leader has exited and been reaped, `os.getpgid` fails, and an MCP server
    grandchild would be left running with a live socket. The group outlives the
    leader, so the recorded id stays the right thing to signal.
    """

    def __init__(self, popen, pgid=None):
        self._popen = popen
        self.pid = popen.pid
        if pgid is not None:
            self.pgid = pgid
        else:
            try:
                self.pgid = os.getpgid(popen.pid)
            except OSError:
                self.pgid = None
        self.stdout = popen.stdout
        self.stderr = popen.stderr

    def poll(self):
        return self._popen.poll()

    def wait(self, timeout=None):
        return self._popen.wait(timeout=timeout)

    def terminate_tree(self, grace=3.0):
        """SIGTERM the whole process group, then SIGKILL what is left."""
        group = self.pgid
        if group is None:
            return
        self._signal_group(group, signal.SIGTERM)
        deadline = time.time() + grace
        while time.time() < deadline:
            if self._popen.poll() is not None:
                break
            time.sleep(0.05)
        self._signal_group(group, signal.SIGKILL)
        try:
            self._popen.wait(timeout=grace)
        except Exception:
            pass

    def close_streams(self):
        for stream in (self.stdout, self.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError):
                    pass

    @staticmethod
    def _signal_group(group, sig):
        try:
            os.killpg(group, sig)
        except OSError:
            pass


def build_argv(provider, binary, paths, prompt, launcher, model=None):
    if provider == "claude":
        return claude_argv(binary, paths, prompt, model)
    if provider == "codex":
        return codex_argv(binary, paths, prompt, launcher, model)
    raise ProviderError("unknown provider %r" % (provider,))


def launch(provider, paths, prompt, token, model=None, environ=None,
           python_exe=None):
    """Resolve the CLI, write per-run config, and start it in its own group."""
    environ = dict(os.environ if environ is None else environ)
    binary, version = resolve_binary(provider, environ)
    launcher = write_launcher(paths, token, python_exe)
    write_mcp_config(paths, launcher)
    argv = build_argv(provider, binary, paths, prompt, launcher, model)
    # Output is piped rather than redirected to a file: the host relays it to
    # the side panel and writes the logs itself through the O_NOFOLLOW helper,
    # so no parent-held file handle can be redirected by a pre-placed symlink.
    popen = subprocess.Popen(argv, cwd=paths.files, env=environ,
                             stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             start_new_session=True)
    # start_new_session makes the child the leader of a new group, so the group
    # id is its pid. Record it now, while the process is certainly alive.
    return (AgentProcess(popen, pgid=popen.pid),
            {"binary": binary, "version": version, "argv": argv})
