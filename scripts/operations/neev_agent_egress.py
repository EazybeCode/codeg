"""Per-tenant provider-egress network wiring for the agent sandbox (LINUX).

Composes the existing login-isolation slot model (`neev_login_network_profile`
slots + `neev_codex_login_egress` allowlisting CONNECT proxy) into a network
namespace for one running agent, so the tenant can reach ONLY its provider and
nothing else:

  agent netns --veth--> slot bridge --> slot proxy (allowlist to provider IPv4s)

Security invariants, enforced by the generated command plan and validated before
apply:
  * Default-DENY egress on BOTH IPv4 and IPv6 (nft chain policy `drop`).
  * The ONLY permitted egress is to the slot proxy address:port; the proxy then
    permits only the validated provider IPv4s (`validated_ipv4`). No `accept`-all
    rule and no direct route to the internet exist (bypass prevention).
  * IPv6 is denied wholesale (no address/route + ip6 policy drop) so it cannot be
    used to bypass the IPv4 allowlist.
  * Scoped, reversible lifecycle: `teardown_commands` deletes the nft table, the
    guest link and the namespace; apply rolls back on any failure.

The command PLAN and its fail-closed validation are pure and unit-tested on every
platform. Actually running `ip`/`nft` and the two-tenant acceptance is Linux-only
(`neev_agent_sandbox_acceptance.py`); non-Linux callers get `unsupported` before
any command runs.
"""

import platform
from dataclasses import dataclass, field

IP = "/usr/sbin/ip"
NFT = "/usr/sbin/nft"


class EgressWiringError(Exception):
    _CATEGORIES = frozenset(("invalid", "unsupported", "apply", "teardown"))

    def __init__(self, category):
        if category not in self._CATEGORIES:
            category = "invalid"
        super().__init__(category)
        self.category = category


@dataclass(frozen=True)
class NetworkPlan:
    tenant: str
    netns: str
    veth_host: str
    veth_guest: str
    bridge: str
    guest_ip: str
    guest_prefix: int
    proxy_ip: str
    proxy_port: int
    provider_ipv4: tuple = field(default_factory=tuple)


def _valid_name(name, prefix):
    return (
        isinstance(name, str)
        and name.startswith(prefix)
        and 0 < len(name) <= 15  # Linux IFNAMSIZ/netns practical bound
        and all(c.isalnum() or c in "-_" for c in name)
    )


def plan_provider_network(tenant, slot, provider_ipv4, proxy_port=3128):
    """Build a validated per-tenant NetworkPlan from a fixed slot profile."""
    from neev_codex_login_egress import validated_ipv4
    from neev_login_network_profile import network_profiles, slot_profile

    profile = slot_profile(slot)  # raises on an unknown slot
    # The veth master must be the bridge DEVICE (e.g. "nv-login-a"), not the
    # docker network NAME (`profile.network`, e.g. "neev-login-a"). Resolve it
    # from the fixed network profile whose name matches this slot's network.
    bridge_device = next(
        (n.bridge for n in network_profiles() if n.name == profile.network), None
    )
    if bridge_device is None:
        raise EgressWiringError("invalid")
    try:
        allow = validated_ipv4(list(provider_ipv4))
    except Exception as error:
        # Normalize the login-proxy validator's error to our fail-closed type.
        raise EgressWiringError("invalid") from error
    if not allow:
        raise EgressWiringError("invalid")
    if not isinstance(tenant, str) or not tenant.strip():
        raise EgressWiringError("invalid")
    if not isinstance(proxy_port, int) or not (1 <= proxy_port <= 65535):
        raise EgressWiringError("invalid")
    suffix = slot
    plan = NetworkPlan(
        tenant=tenant,
        netns=f"neev-agent-{suffix}",
        veth_host=f"nvah-{suffix}",
        veth_guest=f"nvag-{suffix}",
        bridge=bridge_device,
        guest_ip=profile.guest_ip,
        guest_prefix=29,
        proxy_ip=profile.proxy_ip,
        proxy_port=proxy_port,
        provider_ipv4=allow,
    )
    validate_plan(plan)
    return plan


def validate_plan(plan):
    if not isinstance(plan, NetworkPlan):
        raise EgressWiringError("invalid")
    if not _valid_name(plan.netns, "neev-agent-"):
        raise EgressWiringError("invalid")
    if not _valid_name(plan.veth_host, "nvah-") or not _valid_name(plan.veth_guest, "nvag-"):
        raise EgressWiringError("invalid")
    if not plan.provider_ipv4:
        raise EgressWiringError("invalid")
    if not (1 <= plan.proxy_port <= 65535):
        raise EgressWiringError("invalid")


def _nft_ruleset(plan):
    """Default-deny v4+v6 ruleset; the only egress accept is to the slot proxy."""
    table = f"neev_agent_{plan.tenant.replace(':', '_').replace('/', '_')}"
    return table, [
        # inet table covers both ip and ip6.
        [NFT, "add", "table", "inet", table],
        # Output chain, DEFAULT-DENY (policy drop) — bypass-proof baseline.
        [NFT, "add", "chain", "inet", table, "output",
         "{ type filter hook output priority 0 ; policy drop ; }"],
        # Loopback stays usable inside the namespace.
        [NFT, "add", "rule", "inet", table, "output", "oif", "lo", "accept"],
        # Allow replies to connections we initiated.
        [NFT, "add", "rule", "inet", table, "output", "ct", "state",
         "established,related", "accept"],
        # The ONLY new egress permitted: to the slot proxy over IPv4/TCP.
        [NFT, "add", "rule", "inet", table, "output", "ip", "daddr", plan.proxy_ip,
         "tcp", "dport", str(plan.proxy_port), "accept"],
        # Deny ALL IPv6 explicitly (defence in depth beyond having no v6 address).
        [NFT, "add", "rule", "inet", table, "output", "meta", "nfproto", "ipv6", "drop"],
    ]


def setup_commands(plan):
    """Ordered command plan to create the isolated, provider-only network.
    Pure (builds argv only); execution is gated to Linux in `apply`."""
    validate_plan(plan)
    table, nft = _nft_ruleset(plan)
    commands = [
        [IP, "netns", "add", plan.netns],
        [IP, "link", "add", plan.veth_host, "type", "veth", "peer", "name", plan.veth_guest],
        [IP, "link", "set", plan.veth_guest, "netns", plan.netns],
        [IP, "link", "set", plan.veth_host, "master", plan.bridge],
        [IP, "link", "set", plan.veth_host, "up"],
        [IP, "-n", plan.netns, "addr", "add", f"{plan.guest_ip}/{plan.guest_prefix}",
         "dev", plan.veth_guest],
        [IP, "-n", plan.netns, "link", "set", plan.veth_guest, "up"],
        [IP, "-n", plan.netns, "link", "set", "lo", "up"],
        # No default route to the internet: reach only the on-link proxy. No
        # IPv6 address or route is ever added, so IPv6 has no path out.
        [IP, "-n", plan.netns, "route", "add", f"{plan.proxy_ip}/32", "dev", plan.veth_guest],
    ]
    # nft rules run inside the namespace so the policy binds the tenant only.
    commands.extend([[IP, "netns", "exec", plan.netns] + rule for rule in nft])
    return commands


def teardown_commands(plan):
    """Reversible teardown: delete nft table, guest link and the namespace."""
    validate_plan(plan)
    table, _ = _nft_ruleset(plan)
    return [
        [IP, "netns", "exec", plan.netns, NFT, "delete", "table", "inet", table],
        [IP, "-n", plan.netns, "link", "del", plan.veth_guest],
        [IP, "link", "del", plan.veth_host],
        [IP, "netns", "del", plan.netns],
    ]


def assert_fail_closed(commands):
    """Structural bypass check: the output chain must default to drop and contain
    no unconditional accept. Raises if the plan could leak egress."""
    flat = [" ".join(part) for part in commands]
    if not any("policy drop" in line for line in flat):
        raise EgressWiringError("invalid")
    if not any("nfproto ipv6 drop" in line for line in flat):
        raise EgressWiringError("invalid")
    for line in flat:
        stripped = line.rstrip()
        # An accept must be qualified (oif lo, ct state, or a specific daddr).
        if stripped.endswith(" accept") and not any(
            q in line for q in ("oif", "ct state", "ip daddr")
        ):
            raise EgressWiringError("invalid")
    return True


def apply(plan, runner):  # pragma: no cover - Linux execution path
    """Run the setup plan on Linux, rolling back on any failure."""
    if platform.system() != "Linux":
        raise EgressWiringError("unsupported")
    commands = setup_commands(plan)
    assert_fail_closed(commands)
    done = []
    try:
        for command in commands:
            runner.run(command)
            done.append(command)
    except Exception:
        _rollback(plan, runner)
        raise EgressWiringError("apply")
    return plan


def teardown(plan, runner):  # pragma: no cover - Linux execution path
    if platform.system() != "Linux":
        raise EgressWiringError("unsupported")
    _rollback(plan, runner)


def _rollback(plan, runner):  # pragma: no cover - Linux execution path
    for command in teardown_commands(plan):
        try:
            runner.run(command)
        except Exception:
            continue
