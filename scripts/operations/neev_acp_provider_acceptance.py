#!/usr/bin/env python3
"""Credential-free acceptance for the ACP providers on the pinned image (Linux).

Run INSIDE the image built from Dockerfile.native.agent.acp, as any user:

    python3 -I /opt/neev/scripts/operations/neev_acp_provider_acceptance.py \
        [--provider cursor|gemini|opencode]... [--skip-network] [--json]

It proves what can be proven WITHOUT anybody's account, and says so per check:

  pin       the pinned path exists, resolves under /opt/neev, is not writable by
            the runner, and `--version` names the pinned version.
  describe  the adapter's static capabilities (no provider process).
  acp       the adapter's `--probe` against a fresh EMPTY HOME: the provider must
            start, speak ACP v1, and answer "disconnected" (no sign-in exists).
            Anything else — "supported" with no credentials above all — FAILS:
            that would be exactly the fake success the probe exists to prevent.
  login     the provider's OFFICIAL sign-in command, started in an empty HOME,
            must print a challenge the Neev parser accepts (Gemini on a PTY, as
            in production); the process is then killed. Nobody signs in; the
            challenge is reported only as its origin, never the code or query.
  egress    a TCP+TLS handshake to every allowlisted host (direct, or through
            the CONNECT proxy in HTTPS_PROXY when set). Reachable is evidence for
            flipping `confirmed` in neev_provider_endpoints — after review.

This does NOT prove a sign-in completes, a turn runs, or cancel works: those
need a human account (providers-report.md, blocker 1). Exit 0 only when every
selected check passed.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import select
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from typing import Callable, Dict, List, Optional
from urllib.parse import urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import neev_acp_login_protocol as login_proto  # noqa: E402
import neev_provider_endpoints as endpoints  # noqa: E402

ADAPTER = os.path.join(HERE, "neev_acp_stream_adapter.py")
PYTHON = sys.executable or "/usr/bin/python3"
INSTALL_ROOT = "/opt/neev/"

# What the image pins (Dockerfile.native.agent.acp) and how each sign-in starts.
PINS = {
    "cursor": {"path": "/usr/local/bin/agent", "version": "2026.09.28-64d2043",
               "login": ("login",), "pty": False},
    "gemini": {"path": "/usr/local/bin/gemini", "version": "0.62.0",
               "login": (), "pty": True},
    "opencode": {"path": "/usr/local/bin/opencode", "version": "1.18.33",
                 "login": ("auth", "login", "--provider", "openai",
                           "--method", "ChatGPT Pro/Plus (headless)"), "pty": False},
}
PROBE_TIMEOUT = 90.0
LOGIN_TIMEOUT = 60.0
VERSION_TIMEOUT = 30.0
TLS_TIMEOUT = 10.0
MAX_OUTPUT = 256 * 1024


@dataclass
class Check:
    provider: str
    name: str
    ok: bool
    detail: str
    evidence: Dict[str, object] = field(default_factory=dict)


# --------------------------------------------------------------------------- #
# Pure helpers (unit-tested off Linux)
# --------------------------------------------------------------------------- #


def redact_url(url: str) -> str:
    """Origin + path only: a challenge URL's query carries state/codes."""
    parts = urlsplit(url)
    return f"{parts.scheme}://{parts.hostname or ''}{parts.path}"


def version_matches(output: str, expected: str) -> bool:
    """The pinned version appears as a whole token in `--version` output."""
    return re.search(r"(?<![0-9A-Za-z.])" + re.escape(expected) + r"(?![0-9A-Za-z])",
                     output) is not None


def headless_env(home: str, provider: str, extra: Optional[Dict[str, str]] = None,
                 environ: Optional[Dict[str, str]] = None) -> Dict[str, str]:
    """The same fixed, secret-free env production gives the provider."""
    import neev_agent_credentials as credentials

    source = os.environ if environ is None else environ
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8"}
    for key in ("HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "https_proxy", "http_proxy",
                "no_proxy"):
        if source.get(key):
            env[key] = source[key]
    env.update(credentials.provider_env(home, provider))
    if extra:
        env.update(extra)
    return env


def probe_verdict(stdout: bytes) -> Optional[str]:
    for line in stdout.splitlines():
        try:
            record = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            continue
        if isinstance(record, dict) and record.get("type") == "neev.account":
            status = record.get("status")
            return status if isinstance(status, str) else None
    return None


def parser_for(provider: str):
    return {
        "cursor": login_proto.CursorLoginProtocol,
        "gemini": login_proto.GeminiLoginProtocol,
        "opencode": login_proto.OpenCodeLoginProtocol,
    }[provider]()


def challenge_from(provider: str, chunks: List[bytes]) -> Optional[dict]:
    """Feed output chunks to the production parser; the accepted challenge
    (redacted), or None. A ProtocolError propagates: it is a real failure."""
    parser = parser_for(provider)
    challenge = None
    reading = provider != "gemini"
    for chunk in chunks:
        for event in parser.receive(chunk):
            if event.kind == "challenge" and challenge is None:
                challenge = {"origin": redact_url(event.verification_url),
                             "user_code": bool(event.user_code)}
            elif event.kind == "awaiting_code":
                reading = True
    return challenge if challenge is not None and reading else None


def summarize(checks: List[Check]) -> dict:
    return {
        "ok": all(check.ok for check in checks) and bool(checks),
        "checks": [asdict(check) for check in checks],
        "not_proven": [
            "a completed sign-in", "a model turn", "streaming", "cancel",
        ],
    }


# --------------------------------------------------------------------------- #
# Process helpers
# --------------------------------------------------------------------------- #


def _read_until(process, fd, done: Callable[[List[bytes]], bool], timeout: float):
    chunks: List[bytes] = []
    total = 0
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                chunk = b""
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_OUTPUT or done(chunks):
                break
        elif process.poll() is not None:
            break
    return chunks


def _stop(process) -> None:
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            process.kill()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass


# --------------------------------------------------------------------------- #
# Checks
# --------------------------------------------------------------------------- #


def check_pin(provider: str, run=subprocess.run) -> Check:
    pin = PINS[provider]
    path = pin["path"]
    real = os.path.realpath(path)
    evidence = {"path": path, "realpath": real}
    if not os.path.isfile(real) or not os.access(real, os.X_OK):
        return Check(provider, "pin", False, "pinned binary missing", evidence)
    if not real.startswith(INSTALL_ROOT):
        return Check(provider, "pin", False, "binary not under /opt/neev", evidence)
    if os.access(real, os.W_OK) and os.geteuid() != 0:
        return Check(provider, "pin", False, "binary writable by the runner", evidence)
    with tempfile.TemporaryDirectory() as home:
        try:
            result = run([path, "--version"], capture_output=True, timeout=VERSION_TIMEOUT,
                         env=headless_env(home, provider), cwd=home)
        except (OSError, subprocess.TimeoutExpired):
            return Check(provider, "pin", False, "--version did not answer", evidence)
    output = (result.stdout + result.stderr).decode("utf-8", "replace")[:400]
    evidence["version_output"] = output.strip()
    ok = result.returncode == 0 and version_matches(output, pin["version"])
    return Check(provider, "pin", ok,
                 "pinned version" if ok else "version mismatch", evidence)


def check_launcher(provider: str) -> Check:
    """The shipped launcher maps the Rust runtime's canonical ACP argv to the
    adapter with the image pins, no steering and no permission grant (pure; the
    sandbox itself needs the cap helper and is not started here)."""
    import neev_agent_launch as launch

    pin = PINS[provider]["path"]
    try:
        spec = launch.build_spec(launch._parse([
            "--tenant", "org:1/user:1", "--slot", "a",
            "--workdir", "/data/agents/run-acceptance", "--uid", "61000", "--gid", "61000",
            "--provider", provider, "--credential-handle", "0" * 32,
            "--", pin, "--neev-acp", "--", "hello",
        ]), style="")
    except Exception as error:  # noqa: BLE001 - reported, not raised
        return Check(provider, "launcher", False, f"launcher refused: {type(error).__name__}")
    argv = list(spec.provider_argv)
    ok = (argv[1:3] == ["-I", launch.ACP_ADAPTER] and argv[-2:] == ["--", "hello"]
          and argv[argv.index("--agent") + 1] == pin
          and "--enable-steering" not in argv and "--permission-policy" not in argv
          and os.path.isfile(launch.ACP_ADAPTER))
    return Check(provider, "launcher", ok,
                 "canonical argv -> adapter with image pins" if ok else "unexpected argv",
                 {"argv_head": argv[:7]})


def check_describe(provider: str, run=subprocess.run) -> Check:
    result = run([PYTHON, "-I", ADAPTER, "--provider", provider, "--describe"],
                 capture_output=True, timeout=30)
    try:
        caps = json.loads(result.stdout)
    except ValueError:
        return Check(provider, "describe", False, "no capabilities JSON")
    ok = (caps.get("protocol") == "acp" and caps.get("followUp") is False
          and caps.get("modelSelection") is False and caps.get("probe") is True)
    return Check(provider, "describe", ok, "static capabilities", {"capabilities": caps})


def check_acp_probe(provider: str) -> Check:
    """Empty HOME -> the adapter's real probe must say "disconnected"."""
    path = PINS[provider]["path"]
    with tempfile.TemporaryDirectory() as home:
        import neev_agent_credentials as credentials

        credentials._ensure_provider_home(home, provider)
        argv = [PYTHON, "-I", ADAPTER, "--provider", provider, "--agent", path,
                "--allowed-agent", path, "--cwd", home, "--probe"]
        started = time.monotonic()
        try:
            result = subprocess.run(argv, capture_output=True, timeout=PROBE_TIMEOUT,
                                    env=headless_env(home, provider), cwd=home)
        except subprocess.TimeoutExpired:
            return Check(provider, "acp", False, "probe timed out")
    status = probe_verdict(result.stdout)
    evidence = {"status": status, "exit": result.returncode,
                "seconds": round(time.monotonic() - started, 1)}
    if status == "disconnected":
        return Check(provider, "acp", True,
                     "ACP v1 handshake; no-credential session refused", evidence)
    if status == "supported":
        return Check(provider, "acp", False,
                     "FAKE SUCCESS: supported with no credentials", evidence)
    return Check(provider, "acp", False, "provider did not answer the probe", evidence)


def check_login_challenge(provider: str) -> Check:
    """Start the official sign-in in an empty HOME; the challenge must parse.
    Killed as soon as it does (nobody signs in)."""
    import neev_agent_credentials as credentials
    import neev_agent_sandbox as sandbox

    pin = PINS[provider]
    argv = [pin["path"], *pin["login"]]
    with tempfile.TemporaryDirectory() as home:
        credentials._ensure_provider_home(home, provider)
        env = headless_env(home, provider,
                           {"TERM": "xterm-256color"} if pin["pty"] else None)
        master = slave = None
        if pin["pty"]:
            master, slave = sandbox.open_login_pty()
            process = subprocess.Popen(argv, stdin=slave, stdout=slave,
                                       stderr=subprocess.DEVNULL, env=env, cwd=home,
                                       start_new_session=True, close_fds=True)
            os.close(slave)
            fd = master
        else:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL,
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                       env=env, cwd=home, start_new_session=True)
            fd = process.stdout.fileno()
        try:
            def done(chunks):
                try:
                    return challenge_from(provider, chunks) is not None
                except login_proto.ProtocolError:
                    return True
            chunks = _read_until(process, fd, done, LOGIN_TIMEOUT)
        finally:
            _stop(process)
            if master is not None:
                os.close(master)
    try:
        challenge = challenge_from(provider, chunks)
    except login_proto.ProtocolError:
        return Check(provider, "login", False, "challenge refused by the parser")
    if challenge is None:
        return Check(provider, "login", False, "no challenge printed",
                     {"bytes": sum(len(c) for c in chunks)})
    return Check(provider, "login", True, "official sign-in printed a parsable challenge",
                 challenge)


def summarize_handshake(init_result, session_reply) -> dict:
    """Pure: the capability facts worth recording, and nothing else — no error
    messages (they can carry paths), no session ids, no model display names."""
    result = init_result if isinstance(init_result, dict) else {}
    caps = result.get("agentCapabilities") if isinstance(result.get("agentCapabilities"), dict) else {}
    facts = {
        "protocolVersion": result.get("protocolVersion"),
        "agentCapabilities": caps,
        "authMethods": sorted(m.get("id") for m in result.get("authMethods") or []
                              if isinstance(m, dict) and isinstance(m.get("id"), str)),
        "agentInfo": {k: v for k, v in (result.get("agentInfo") or {}).items()
                      if k in ("name", "version")},
    }
    reply = session_reply if isinstance(session_reply, dict) else {}
    if isinstance(reply.get("error"), dict):
        facts["sessionNew"] = {"error": reply["error"].get("code")}
    elif isinstance(reply.get("result"), dict):
        options = reply["result"].get("configOptions") or []
        facts["sessionNew"] = {
            "ok": True,
            "configOptions": [
                {"id": o.get("id"), "category": o.get("category"),
                 "currentValuePrefix": str(o.get("currentValue", "")).split("/")[0]}
                for o in options if isinstance(o, dict)],
            "modes": bool(reply["result"].get("modes")),
        }
    else:
        facts["sessionNew"] = {"noAnswer": True}
    return facts


def check_protocol(provider: str, timeout: float = 60.0) -> Check:
    """Raw ACP v1 initialize + session/new against the pinned binary in an EMPTY
    HOME, recording what the real agent advertises (no prompt, no sign-in)."""
    import neev_acp_stream_adapter as adapter
    import neev_agent_credentials as credentials

    path = PINS[provider]["path"]
    with tempfile.TemporaryDirectory() as home:
        if provider == "gemini":
            # UNSEEDED on purpose. With Neev's Google sign-in seed and no
            # credential, the pinned Gemini prints its sign-in prompt BEFORE it
            # answers `initialize` (observed): capabilities are only readable
            # without the seed. The seeded behaviour is what the `acp` check covers.
            os.makedirs(os.path.join(home, ".gemini"), mode=0o700)
        else:
            credentials._ensure_provider_home(home, provider)
        process = subprocess.Popen(
            adapter.agent_argv(provider, path), stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=headless_env(home, provider), cwd=home, start_new_session=True)
        replies = {}
        try:
            def send(payload):
                process.stdin.write((json.dumps(payload) + "\n").encode())
                process.stdin.flush()
            send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": adapter.initialize_params()})
            buffer = b""
            deadline = time.monotonic() + timeout
            fd = process.stdout.fileno()
            while time.monotonic() < deadline and 2 not in replies:
                ready, _, _ = select.select([fd], [], [], 0.2)
                if not ready:
                    if process.poll() is not None:
                        break
                    continue
                chunk = os.read(fd, 65536)
                if not chunk:
                    break
                buffer += chunk
                while b"\n" in buffer:
                    line, buffer = buffer.split(b"\n", 1)
                    try:
                        message = json.loads(line)
                    except ValueError:
                        # A provider asking for an interactive sign-in instead.
                        if adapter.is_signin_prompt(provider, line.decode("utf-8", "replace")):
                            replies[2] = {"signin_prompt": True}
                        continue
                    if isinstance(message, dict) and message.get("id") in (1, 2):
                        replies[message["id"]] = message
                        if message["id"] == 1:
                            send({"jsonrpc": "2.0", "id": 2, "method": "session/new",
                                  "params": adapter.session_new_params(home)})
        finally:
            _stop(process)
    init = replies.get(1, {}).get("result")
    facts = summarize_handshake(init, replies.get(2))
    if replies.get(2, {}).get("signin_prompt"):
        facts["sessionNew"] = {"signinPromptOnStdout": True}
    ok = facts["protocolVersion"] == 1
    return Check(provider, "protocol", ok,
                 "real ACP v1 capabilities recorded" if ok else "no ACP v1 initialize", facts)


def _proxy(environ=None):
    value = (environ or os.environ).get("HTTPS_PROXY") or (environ or os.environ).get(
        "https_proxy")
    if not value:
        return None
    parts = urlsplit(value)
    return (parts.hostname, parts.port or 3128) if parts.hostname else None


def tls_reachable(host: str, port: int, proxy=None, timeout: float = TLS_TIMEOUT) -> bool:
    try:
        if proxy:
            sock = socket.create_connection(proxy, timeout=timeout)
            request = (f"CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n")
            sock.sendall(request.encode("ascii"))
            response = sock.recv(4096)
            if not re.match(rb"HTTP/1\.[01] 200", response):
                sock.close()
                return False
        else:
            sock = socket.create_connection((host, port), timeout=timeout)
        context = ssl.create_default_context()
        with context.wrap_socket(sock, server_hostname=host) as tls:
            return tls.version() is not None
    except (OSError, ssl.SSLError):
        return False


def check_egress(provider: str, reach=tls_reachable) -> Check:
    results = {}
    for purpose in endpoints.PURPOSES:
        for endpoint in endpoints.endpoints(provider, purpose):
            key = f"{endpoint.host}:{endpoint.port}"
            if key not in results:
                results[key] = reach(endpoint.host, endpoint.port, _proxy())
    ok = all(results.values())
    return Check(provider, "egress", ok,
                 "every allowlisted host completed a TLS handshake" if ok
                 else "unreachable allowlisted host(s)", {"hosts": results})


def run_checks(providers, network: bool) -> List[Check]:
    checks: List[Check] = []
    for provider in providers:
        pin = check_pin(provider)
        checks.append(pin)
        checks.append(check_describe(provider))
        checks.append(check_launcher(provider))
        if not pin.ok:
            continue
        if network:
            checks.append(check_protocol(provider))
            checks.append(check_egress(provider))
            checks.append(check_acp_probe(provider))
            checks.append(check_login_challenge(provider))
    return checks


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="neev_acp_provider_acceptance")
    parser.add_argument("--provider", action="append", choices=sorted(PINS))
    parser.add_argument("--skip-network", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if sys.platform != "linux":
        print("neev_acp_provider_acceptance: run inside the Linux image", file=sys.stderr)
        return 2
    providers = args.provider or sorted(PINS)
    report = summarize(run_checks(providers, network=not args.skip_network))
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for check in report["checks"]:
            mark = "PASS" if check["ok"] else "FAIL"
            print(f"{mark} {check['provider']:9} {check['name']:9} {check['detail']}")
        print("NOT PROVEN (needs a human account): " + ", ".join(report["not_proven"]))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
