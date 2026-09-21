#!/usr/bin/env python3
"""One-time per-container network bootstrap for the Neev agent runtime (LINUX).

The host/canary container does NOT ship the login-isolation bridges, so
`provider_allowlist` egress (agent netns --veth--> slot bridge --> slot proxy -->
provider) has nowhere to attach. This provisions, INSIDE the owned container's own
network namespace (never the host):

  * the three fixed reservations from `neev_login_network_profile`:
      nv-login-a (10.203.0.0/29, internal), nv-login-b (10.203.0.8/29, internal),
      nv-login-out (10.203.0.16/29, egress);
  * a dedicated proxy namespace (`neev-login-proxy`) with one veth onto each bridge
    holding the fixed proxy addresses (10.203.0.2 / 10.203.0.10 on a/b, 10.203.0.18
    on out) and a default route via the out gateway; the CONNECT proxies bind these;
  * default-DENY IPv4 forwarding whose ONLY accept is the proxy namespace egressing
    via the out bridge to the container uplink (masqueraded). The internal a/b
    bridges never appear in a forward accept, so a guest can reach ONLY its slot
    proxy — exactly the invariant `neev_agent_egress`'s per-guest nft also enforces.

Security invariants (validated before apply): forward policy is `drop`; the only
masquerade is `ip saddr 10.203.0.16/29 oif <uplink>`; no unqualified accept; the
uplink is a strictly-validated interface name; and NAT/forwarding live in the
container netns only — no host-network or broad host-firewall change.

The uplink is a parameter (the canary container's is `eth0`, NOT a host interface).
The command PLAN + fail-closed validation are pure and unit-tested on every
platform; `ip`/`nft`/`sysctl` execution is Linux-only.
"""

from __future__ import annotations

import contextlib
import errno
import fcntl
import os
import platform
import re
from dataclasses import dataclass

import neev_login_network_profile as profile

IP = "/usr/sbin/ip"
NFT = "/usr/sbin/nft"
SYSCTL = "/usr/sbin/sysctl"

PROXY_NETNS = "neev-login-proxy"
_TABLE = "neev_login_bootstrap"
_OUT_SUBNET = "10.203.0.16/29"
_OUT_GATEWAY = "10.203.0.17"
# Proxy-namespace addresses (one per bridge). Documented to match the fixed profile;
# a test cross-checks the a/b addresses against `slot_profile`.
_PROXY_ENDPOINTS = (
    ("nv-login-a", "10.203.0.2", "nvpa-h", "nvpa-g"),
    ("nv-login-b", "10.203.0.10", "nvpb-h", "nvpb-g"),
    ("nv-login-out", "10.203.0.18", "nvpo-h", "nvpo-g"),
)

# Strict interface name: lowercase alnum, 1..15 bytes (IFNAMSIZ), no metacharacters.
_UPLINK_RE = re.compile(r"[a-z0-9]{1,15}")

_PROXY_LOCK_DIR = "/var/lib/neev-supervisor/proxy-locks"
_VALID_SLOTS = frozenset(("a", "b"))


class CommandRunner:  # pragma: no cover - Linux subprocess
    """Minimal checked runner for the bootstrap command plan (no shell)."""

    def run(self, argv):
        import subprocess

        subprocess.run(argv, check=True, capture_output=True, timeout=30)


class BootstrapError(Exception):
    _CATEGORIES = frozenset(("invalid", "unsupported", "apply", "teardown", "busy"))

    def __init__(self, category):
        if category not in self._CATEGORIES:
            category = "invalid"
        super().__init__(category)
        self.category = category

    def __repr__(self):
        return f"BootstrapError({self.category})"


@dataclass(frozen=True)
class BootstrapPlan:
    uplink: str


def _validate_uplink(uplink):
    if not isinstance(uplink, str) or _UPLINK_RE.fullmatch(uplink) is None:
        raise BootstrapError("invalid")


def plan(uplink):
    """Build a validated bootstrap plan for the given container uplink interface."""
    _validate_uplink(uplink)
    return BootstrapPlan(uplink=uplink)


def _nft_ruleset(uplink):
    return [
        [NFT, "add", "table", "inet", _TABLE],
        # NAT: masquerade ONLY the proxy-out subnet leaving via the uplink.
        [NFT, "add", "chain", "inet", _TABLE, "postrouting",
         "{ type nat hook postrouting priority 100 ; }"],
        [NFT, "add", "rule", "inet", _TABLE, "postrouting",
         "ip", "saddr", _OUT_SUBNET, "oif", uplink, "masquerade"],
        # Forwarding: default DROP; only the proxy-out subnet may egress via uplink.
        [NFT, "add", "chain", "inet", _TABLE, "forward",
         "{ type filter hook forward priority 0 ; policy drop ; }"],
        [NFT, "add", "rule", "inet", _TABLE, "forward",
         "ct", "state", "established,related", "accept"],
        [NFT, "add", "rule", "inet", _TABLE, "forward",
         "iif", "nv-login-out", "oif", uplink, "ip", "saddr", _OUT_SUBNET, "accept"],
        # IPv6 has no address/route here; deny wholesale as defence in depth.
        [NFT, "add", "rule", "inet", _TABLE, "forward", "meta", "nfproto", "ipv6", "drop"],
    ]


def setup_commands(plan_obj):
    """Ordered argv plan (pure; execution gated to Linux in `apply`)."""
    if not isinstance(plan_obj, BootstrapPlan):
        raise BootstrapError("invalid")
    _validate_uplink(plan_obj.uplink)
    commands = []
    for reservation in profile.network_profiles():
        subnet_prefix = reservation.subnet.split("/")[1]
        commands += [
            [IP, "link", "add", reservation.bridge, "type", "bridge"],
            [IP, "addr", "add", "%s/%s" % (reservation.gateway, subnet_prefix),
             "dev", reservation.bridge],
            [IP, "link", "set", reservation.bridge, "up"],
        ]
    commands.append([IP, "netns", "add", PROXY_NETNS])
    for bridge, proxy_ip, veth_host, veth_ns in _PROXY_ENDPOINTS:
        commands += [
            [IP, "link", "add", veth_host, "type", "veth", "peer", "name", veth_ns],
            [IP, "link", "set", veth_host, "master", bridge],
            [IP, "link", "set", veth_host, "up"],
            [IP, "link", "set", veth_ns, "netns", PROXY_NETNS],
            [IP, "-n", PROXY_NETNS, "addr", "add", proxy_ip + "/29", "dev", veth_ns],
            [IP, "-n", PROXY_NETNS, "link", "set", veth_ns, "up"],
        ]
    commands += [
        [IP, "-n", PROXY_NETNS, "link", "set", "lo", "up"],
        [IP, "-n", PROXY_NETNS, "route", "add", "default", "via", _OUT_GATEWAY],
    ]
    # NOTE: we do NOT write net.ipv4.ip_forward here. The scoped AppArmor profile
    # denies /proc/sys writes and Docker may mount /proc/sys read-only; the deployment
    # provisions the namespaced sysctl via `docker --sysctl net.ipv4.ip_forward=1`
    # and `apply()` VERIFIES it is already 1 (fail-closed) instead of setting it.
    commands += _nft_ruleset(plan_obj.uplink)
    return commands


_IP_FORWARD_PATH = "/proc/sys/net/ipv4/ip_forward"


def _forwarding_enabled():  # pragma: no cover - reads the live procfs
    try:
        with open(_IP_FORWARD_PATH, "r", encoding="ascii") as handle:
            return handle.read().strip() == "1"
    except OSError:
        return False


def teardown_commands(plan_obj):
    if not isinstance(plan_obj, BootstrapPlan):
        raise BootstrapError("invalid")
    commands = [[NFT, "delete", "table", "inet", _TABLE]]
    for _bridge, _ip, veth_host, _ns in _PROXY_ENDPOINTS:
        commands.append([IP, "link", "del", veth_host])
    commands.append([IP, "netns", "del", PROXY_NETNS])
    for reservation in profile.network_profiles():
        commands.append([IP, "link", "del", reservation.bridge])
    return commands


def assert_fail_closed(commands):
    """Structural bypass check: forwarding defaults to drop, the only masquerade is
    the proxy-out subnet via the uplink, and no internal bridge is ever forwarded."""
    flat = [" ".join(part) for part in commands]
    if not any("policy drop" in line for line in flat):
        raise BootstrapError("invalid")
    if not any("nfproto ipv6 drop" in line for line in flat):
        raise BootstrapError("invalid")
    for line in flat:
        stripped = line.rstrip()
        if stripped.endswith(" masquerade") and (
            "ip saddr %s" % _OUT_SUBNET not in line or " oif " not in line
        ):
            raise BootstrapError("invalid")
        if stripped.endswith(" accept") and not any(
            qualifier in line for qualifier in ("ct state", "iif nv-login-out")
        ):
            raise BootstrapError("invalid")
        # A guest bridge must never be granted forwarding.
        if stripped.endswith(" accept") and (
            "nv-login-a" in line or "nv-login-b" in line
        ):
            raise BootstrapError("invalid")
    return True


def apply(plan_obj, runner):  # pragma: no cover - Linux execution path
    if platform.system() != "Linux":
        raise BootstrapError("unsupported")
    # IP forwarding must be provisioned by the deployment (docker --sysctl
    # net.ipv4.ip_forward=1); we verify rather than write it (the scoped profile
    # denies /proc/sys writes and /proc/sys may be read-only). Fail closed if absent.
    if not _forwarding_enabled():
        raise BootstrapError("apply")
    commands = setup_commands(plan_obj)
    assert_fail_closed(commands)
    done = []
    try:
        for command in commands:
            runner.run(command)
            done.append(command)
    except Exception:
        _rollback(plan_obj, runner)
        raise BootstrapError("apply")
    return plan_obj


def teardown(plan_obj, runner):  # pragma: no cover - Linux execution path
    if platform.system() != "Linux":
        raise BootstrapError("unsupported")
    _rollback(plan_obj, runner)


def _rollback(plan_obj, runner):  # pragma: no cover - Linux execution path
    for command in teardown_commands(plan_obj):
        try:
            runner.run(command)
        except Exception:
            continue


@contextlib.contextmanager
def slot_proxy_lock(slot, *, lock_dir=None):
    """Hold the EXCLUSIVE per-slot proxy endpoint lock.

    The slot proxy IP:port is a single endpoint, but the dispatcher (login proxy)
    and the runtime launcher (runtime proxy) both want to bind it. This
    non-blocking flock makes them mutually exclusive per slot: whoever holds it owns
    the endpoint, the other gets `busy` (fail fast, never race two binds). Released
    on exit (including cancellation), and SIGKILL drops the flock too."""
    if slot not in _VALID_SLOTS:
        raise BootstrapError("invalid")
    directory = lock_dir if lock_dir is not None else _PROXY_LOCK_DIR
    try:
        os.makedirs(directory, mode=0o700, exist_ok=True)
    except OSError as error:
        raise BootstrapError("invalid") from error
    path = os.path.join(directory, "slot-%s.lock" % slot)
    fd = None
    try:
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            if error.errno in (errno.EACCES, errno.EAGAIN, errno.EWOULDBLOCK):
                raise BootstrapError("busy") from None
            raise BootstrapError("invalid") from error
        yield
    finally:
        if fd is not None:
            try:
                fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError:
                pass
            os.close(fd)
