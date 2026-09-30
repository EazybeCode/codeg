#!/usr/bin/env python3
"""Linux acceptance for direct-sign-in loopback delivery, with a STUB listener.

Run as root on Linux (e.g. a throwaway container with CAP_SYS_ADMIN +
CAP_NET_ADMIN). No provider, no account, no network. It creates a private
network namespace, starts a Claude-like stub listener INSIDE it as an
unprivileged uid (bound to 127.0.0.1, exits on a wrong state like the measured
claude 2.1.267), and checks the production delivery path
(`neev_login_callback.serve_argv` -> `ip netns exec` + `setpriv` -> `--serve`):

  1. the host namespace cannot reach the listener (isolation is real);
  2. a bound callback is delivered once, byte-exact, with Host localhost:<port>;
  3. a forged callback is refused by the gate and never reaches the stub
     (which is still alive afterwards);
  4. a hung listener costs at most the short delivery timeout, and reports 0
     (unavailable) without consuming the gate.

Prints one JSON report; exit 0 only when every check passed. Cleans up the
namespace and the stub in all cases.
"""

import json
import os
import secrets
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import neev_login_callback as cb  # noqa: E402

UID = GID = 61999
STUB = r'''
import http.server, json, sys, os
state, mode = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path, _, query = self.path.partition("?")
        sys.stdout.write(json.dumps({"path": path, "query": query,
                                     "host": self.headers.get("Host")}) + "\n")
        sys.stdout.flush()
        if mode == "hang":
            import time; time.sleep(30)
        params = dict(p.split("=", 1) for p in query.split("&") if "=" in p)
        if params.get("state") != state or not params.get("code"):
            self.send_response(400); self.end_headers(); os._exit(1)
        self.send_response(302); self.send_header("Location", "https://example.invalid/ok")
        self.end_headers()
    def log_message(self, *a): pass
server = http.server.HTTPServer(("127.0.0.1", 0), H)
sys.stdout.write(json.dumps({"port": server.server_address[1]}) + "\n"); sys.stdout.flush()
server.serve_forever()
'''


def run(argv, **kw):
    return subprocess.run(argv, check=True, capture_output=True, timeout=20, **kw)


def start_stub(netns, state, mode):
    proc = subprocess.Popen(
        [cb.IP, "netns", "exec", netns, cb.SETPRIV, f"--reuid={UID}", f"--regid={GID}",
         "--clear-groups", "--no-new-privs", sys.executable, "-I", "-c", STUB, state, mode],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    port = json.loads(proc.stdout.readline())["port"]
    return proc, port


def main() -> int:
    if sys.platform != "linux" or os.geteuid() != 0:
        print(json.dumps({"ok": False, "error": "needs root on Linux"}))
        return 2
    netns = "neev-cbtest-" + secrets.token_hex(3)
    state = secrets.token_urlsafe(32)
    checks = {}
    stubs = []
    try:
        run([cb.IP, "netns", "add", netns])
        run([cb.IP, "-n", netns, "link", "set", "lo", "up"])
        stub, port = start_stub(netns, state, "normal")
        stubs.append(stub)
        target = cb.CallbackTarget("claude", port, "/callback", state)

        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            checks["host_namespace_isolated"] = False
        except OSError:
            checks["host_namespace_isolated"] = True

        gate = cb.CallbackGate(target)
        try:
            gate.check(f"code=forged&state={'A' * len(state)}")
            checks["forged_refused_before_cli"] = False
        except cb.CallbackRejected:
            checks["forged_refused_before_cli"] = True
        checks["stub_alive_after_forged"] = stub.poll() is None

        argv = cb.serve_argv(netns, UID, GID)
        query = gate.check(f"http://localhost:{port}/callback?code=probe-code&state={state}")
        status = cb.deliver(target, query, argv)
        if status:
            gate.commit()
        seen = json.loads(stub.stdout.readline())
        checks["bound_delivered_302"] = status == 302 and cb.outcome(status) == "delivered"
        checks["delivered_byte_exact"] = (seen == {"path": "/callback", "query": query,
                                                   "host": f"localhost:{port}"})
        checks["gate_spent_after_answer"] = gate.used

        hung, hung_port = start_stub(netns, state, "hang")
        stubs.append(hung)
        hung_target = cb.CallbackTarget("claude", hung_port, "/callback", state)
        hung_gate = cb.CallbackGate(hung_target)
        started = time.monotonic()
        hung_status = cb.deliver(hung_target, hung_gate.check(f"code=x&state={state}"), argv)
        elapsed = time.monotonic() - started
        checks["hung_listener_bounded"] = (hung_status == 0
                                           and elapsed < cb.DELIVER_TIMEOUT_SECONDS + 3)
        checks["hung_gate_not_spent"] = not hung_gate.used
        report = {"ok": all(checks.values()), "checks": checks,
                  "hung_seconds": round(elapsed, 2), "netns": netns}
    except Exception as error:  # noqa: BLE001 - reported, then cleaned up
        report = {"ok": False, "checks": checks, "error": type(error).__name__}
    finally:
        for proc in stubs:
            proc.kill()
            proc.wait(timeout=5)
        subprocess.run([cb.IP, "netns", "del", netns], capture_output=True, timeout=20)
    print(json.dumps(report))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
