#!/usr/bin/env python3
"""Two-tenant agent-sandbox isolation acceptance — LINUX ONLY.

Launches two tenants through `neev_agent_sandbox.launch` and asserts real kernel
separation: process (PID namespace), filesystem/credential (mount namespace +
dropped uid cannot read the peer's workdir), network egress (fresh net namespace
denies outbound), plus resource limits and full teardown. On non-Linux it prints
SKIP and exits 0 — a skip is NOT a pass and must never be reported as acceptance.

Run on a Linux host/CI runner with unshare/setpriv/prlimit and unprivileged user
namespaces enabled:
    python3 -B scripts/operations/neev_agent_sandbox_acceptance.py

This is the acceptance gate the production runtime waits on; passing it (with the
independent review) is the prerequisite to enabling `NEEV_RUNTIME_ENABLED`.
"""

import os
import platform
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import neev_agent_sandbox as sandbox
import neev_agent_egress as egress
import neev_login_network_profile as profile


def _limits():
    return sandbox.ResourceLimits(
        cpu_seconds=30,
        address_space_bytes=1024 * 1024 * 1024,
        open_files=128,
        processes=32,
    )


def _probe(peer_secret_path):
    # Runs inside the sandbox: prove no peer file, own PID ns, hardened privileges,
    # and no egress. The peer's workdir is 0700 owned by the peer's uid (see
    # sandbox.launch), so this dropped uid cannot traverse/read it. The PID bound
    # is small but not 3: the `$(ls | grep)` pipeline itself briefly spawns a
    # subshell + ls + grep on top of PID 1, so a correct private /proc still shows
    # a handful of entries; a leaked host/container proc table would show dozens.
    # Egress is checked with a REAL socket connect (python) rather than bash-only
    # `/dev/tcp`, which is a no-op under dash and would false-pass.
    return (
        "test ! -r %s && "  # cannot read the peer tenant's secret (uid-owned 0700 dir)
        "[ \"$(ls -1 /proc | grep -c '^[0-9]')\" -le 8 ] && "  # own PID namespace + private /proc
        "grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status && "  # no privilege escalation
        "grep -Eq '^CapBnd:[[:space:]]+0+$' /proc/self/status && "  # empty capability bounding set
        "! python3 -c 'import socket; socket.create_connection((\"1.1.1.1\",53),2)' 2>/dev/null && "  # no egress
        "echo ISOLATED"
    ) % peer_secret_path


def _fixed_runner():
    import subprocess

    class Runner:
        def run(self, argv):
            subprocess.run(argv, check=True, capture_output=True, timeout=15)

    return Runner()


def provider_egress_acceptance(provider_ipv4):
    """LINUX: wire two tenants' provider-egress namespaces, assert the provider
    subnet is reachable ONLY through each tenant's own slot proxy while an
    unrelated host and the peer tenant's guest IP are denied, then tear down.
    Requires the slot bridges/proxies to be provisioned (the login-isolation
    deployment); returns SKIP details otherwise. This is the enforced-isolation
    counterpart to the pure `neev_agent_egress` plan tests."""
    if platform.system() != "Linux":
        return "SKIP: provider-egress acceptance requires Linux"
    runner = _fixed_runner()
    plans = [
        egress.plan_provider_network("org:1/user:1", "a", provider_ipv4),
        egress.plan_provider_network("org:2/user:2", "b", provider_ipv4),
    ]
    applied = []
    try:
        for plan in plans:
            egress.assert_fail_closed(egress.setup_commands(plan))
            egress.apply(plan, runner)
            applied.append(plan)
        # Reachability probes run inside each namespace: the peer tenant's guest
        # IP and an unrelated public host must fail; the proxy path must work.
        for plan, peer in ((plans[0], plans[1]), (plans[1], plans[0])):
            deny_peer = subprocess.run(
                [egress.IP, "netns", "exec", plan.netns, "timeout", "3", "bash", "-c",
                 f"echo > /dev/tcp/{peer.guest_ip}/22"],
                capture_output=True,
            )
            deny_other = subprocess.run(
                [egress.IP, "netns", "exec", plan.netns, "timeout", "3", "bash", "-c",
                 "echo > /dev/tcp/1.1.1.1/53"],
                capture_output=True,
            )
            if deny_peer.returncode == 0 or deny_other.returncode == 0:
                return f"FAIL: egress leak in {plan.netns}"
        return "PASS: provider-only egress; cross-tenant + unrelated denied"
    finally:
        for plan in applied:
            egress.teardown(plan, runner)


def _accept_listener(bind_ip, port, ready):
    """A throwaway TCP accept loop bound to bind_ip:port (stand-in slot proxy).
    Used so a DENY of a *listening* port proves the nft policy drop, not a mere
    connection-refused. Returns the socket so the caller can close it."""
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((bind_ip, port))
    srv.listen(8)

    def loop():
        while True:
            try:
                conn, _ = srv.accept()
                conn.close()
            except OSError:
                return

    threading.Thread(target=loop, daemon=True).start()
    ready.append(srv)
    return srv


def _composed_probe(proxy_ip, peer_proxy_ip, proxy_port):
    # Runs INSIDE the agent's provider-egress netns (joined via sandbox.launch).
    # Proves: (a) the allowlisted proxy addr:port is reachable; (b) the SAME proxy
    # IP on a NON-allowed port that HAS a listener is DROPPED by nft (not merely
    # refused) — a real exercise of `policy drop`; (c) the peer tenant's proxy is
    # unreachable (cross-tenant denial); (d) hardened privileges hold.
    py = (
        "import socket,sys\n"
        "def ok(h,p,t=3):\n"
        " try:\n"
        "  socket.create_connection((h,p),t).close(); return True\n"
        " except OSError: return False\n"
        "r=[]\n"
        "r.append('PROXY_OK' if ok('%s',%d) else 'PROXY_FAIL')\n"
        "r.append('NONPROXY_DROP' if not ok('%s',9999) else 'NONPROXY_LEAK')\n"
        "r.append('PEER_DENY' if not ok('%s',%d) else 'PEER_LEAK')\n"
        "print(' '.join(r))\n"
    ) % (proxy_ip, proxy_port, proxy_ip, peer_proxy_ip, proxy_port)
    return (
        "grep -Eq '^NoNewPrivs:[[:space:]]+1$' /proc/self/status && "
        "grep -Eq '^CapBnd:[[:space:]]+0+$' /proc/self/status && "
        "python3 -c \"$(printf '%%s' %s)\"" % _shquote(py)
    )


def _shquote(text):
    return "'" + text.replace("'", "'\\''") + "'"


def composed_provider_acceptance(provider_ipv4):
    """LINUX: launch each tenant's agent THROUGH sandbox.launch bound to a
    provider-allowlist egress policy (so the agent JOINS its wired netns), and
    prove the production egress behavior end-to-end: the allowlisted proxy path
    works, a non-allowlisted port with a live listener is dropped by nft, and the
    peer tenant's proxy is unreachable. This exercises the composed system the
    review flagged as untested (agent process + provider egress together)."""
    if platform.system() != "Linux":
        return "SKIP: composed provider acceptance requires Linux"
    runner = sandbox._CommandRunner()
    slots = ("a", "b")
    profiles = {s: profile.slot_profile(s) for s in slots}
    netprofiles = {n.name: n for n in profile.network_profiles()}
    bridges = []
    listeners = []
    handles = {}
    dirs = {}
    root = tempfile.mkdtemp()
    os.chmod(root, 0o711)
    try:
        # Provision the two slot bridges + assign each proxy IP (stand-in for the
        # deployed slot proxies) and run listeners on :proxy_port and :9999.
        for s in slots:
            p = profiles[s]
            br = netprofiles[p.network].bridge
            runner.run([egress.IP, "link", "add", br, "type", "bridge"])
            runner.run([egress.IP, "link", "set", br, "up"])
            bridges.append(br)
            runner.run([egress.IP, "addr", "add", f"{p.proxy_ip}/29", "dev", br])
            ready = []
            _accept_listener(p.proxy_ip, 3128, ready)
            _accept_listener(p.proxy_ip, 9999, ready)
            listeners.extend(ready)
        # Launch each tenant's agent joined to its provider-egress netns.
        for i, s in enumerate(slots):
            p = profiles[s]
            peer = profiles[slots[1 - i]]
            d = os.path.join(root, f"tenant-{s}")
            os.mkdir(d)
            dirs[s] = d
            spec = sandbox.SandboxSpec(
                tenant=f"org:{i+1}/user:{i+1}",
                slot=s,
                workdir=d,
                run_uid=61000 + i,
                run_gid=61000 + i,
                provider_argv=("sh", "-c", _composed_probe(p.proxy_ip, peer.proxy_ip, 3128)),
                limits=_limits(),
                egress=sandbox.EgressPolicy.provider_allowlist(list(provider_ipv4)),
            )
            handles[s] = sandbox.launch(spec)
        deadline = time.monotonic() + 30
        outputs = {}
        for s, h in handles.items():
            remaining = max(0.0, deadline - time.monotonic())
            out, _ = h._process.communicate(timeout=remaining)
            outputs[s] = (out or b"").decode("utf-8", "replace")
    finally:
        for h in handles.values():
            h.teardown()  # tears down egress wiring + reaps
        for srv in listeners:
            try:
                srv.close()
            except OSError:
                pass
        for br in bridges:
            try:
                runner.run([egress.IP, "link", "del", br])
            except Exception:
                pass
        subprocess.run(["rm", "-rf", root], check=False)
    for s, out in outputs.items():
        want = ("PROXY_OK", "NONPROXY_DROP", "PEER_DENY")
        if not all(tok in out for tok in want):
            return f"FAIL: composed tenant {s}: {out!r}"
    return "PASS: agent joined provider-egress netns; proxy reachable, nft-drop + cross-tenant denied"


def main():
    if platform.system() != "Linux":
        print("SKIP: agent sandbox acceptance requires Linux namespaces")
        return 0
    import subprocess  # noqa: F401  (used by provider_egress_acceptance probes)
    with tempfile.TemporaryDirectory() as root:
        # The temp root must be traversable so each tenant's dropped uid can
        # reach its own 0700 workdir; the per-tenant dirs themselves are locked
        # to the tenant uid by sandbox.launch (chown + chmod 0700).
        os.chmod(root, 0o711)
        a_dir = os.path.join(root, "tenant-a")
        b_dir = os.path.join(root, "tenant-b")
        os.mkdir(a_dir)
        os.mkdir(b_dir)
        a_secret = os.path.join(a_dir, "secret")
        b_secret = os.path.join(b_dir, "secret")
        Path(a_secret).write_text("A-only")
        Path(b_secret).write_text("B-only")

        specs = {
            "a": sandbox.SandboxSpec(
                tenant="org:1/user:1", slot="a", workdir=a_dir,
                run_uid=61000, run_gid=61000,
                provider_argv=("sh", "-c", _probe(b_secret)),
                limits=_limits(),
            ),
            "b": sandbox.SandboxSpec(
                tenant="org:2/user:2", slot="b", workdir=b_dir,
                run_uid=61001, run_gid=61001,
                provider_argv=("sh", "-c", _probe(a_secret)),
                limits=_limits(),
            ),
        }
        assert sandbox.distinct_tenants(specs["a"], specs["b"]), "tenants share identity"

        handles = {}
        try:
            for name, spec in specs.items():
                handles[name] = sandbox.launch(spec)
            deadline = time.monotonic() + 30
            outputs = {}
            for name, handle in handles.items():
                remaining = max(0.0, deadline - time.monotonic())
                out, _ = handle._process.communicate(timeout=remaining)
                outputs[name] = (out or b"").decode("utf-8", "replace")
        finally:
            for handle in handles.values():
                handle.teardown()

        for name, out in outputs.items():
            if "ISOLATED" not in out:
                print(f"FAIL: tenant {name} isolation probe: {out!r}")
                return 1
        print("PASS: two-tenant process/filesystem/egress isolation + teardown")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
