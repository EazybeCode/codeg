"""Immutable server-owned profile for Neev Codex login network isolation."""

from __future__ import annotations

from dataclasses import dataclass
import re
from types import MappingProxyType


CODEX_IMAGE = (
    "sha256:735d67c77f139634f9903ed731c4d4a0ac03409a3f520b12c588191b3e9406bc"
)
GUEST_RUNTIME = "workos-runsc"
GUEST_USER = "65532:65532"
GUEST_READ_ONLY = True
GUEST_CAP_DROP = ("ALL",)
GUEST_SECURITY_OPT = ("no-new-privileges",)
GUEST_CPU_COUNT = 1
GUEST_MEMORY_BYTES = 1073741824
GUEST_MEMORY_SWAP_BYTES = 1073741824
GUEST_PIDS_LIMIT = 128
GUEST_CORE_LIMIT = 0
GUEST_LOG_DRIVER = "none"
GUEST_RESTART_POLICY = "no"
GUEST_SCRATCH_BYTES = 67108864
GUEST_DNS = ("127.0.0.1",)
GUEST_DNS_SEARCH = ()
GUEST_PUBLISHED_PORTS = ()
GUEST_NO_PROXY = ""
PROXY_ENV_KEYS = ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy")
PROXY_URLS = MappingProxyType(
    {
        "a": "http://10.203.0.2:3128",
        "b": "http://10.203.0.10:3128",
    }
)


class ProfileError(Exception):
    """A fixed failure that never reveals rejected profile input."""

    def __init__(self, unused_detail: object = None):
        del unused_detail
        super().__init__("invalid")


@dataclass(frozen=True)
class NetworkProfile:
    name: str
    bridge: str
    subnet: str
    gateway: str
    internal: bool


@dataclass(frozen=True)
class SlotProfile:
    slot: str
    network: str
    proxy_ip: str
    guest_ip: str


@dataclass(frozen=True)
class Endpoint:
    role: str
    bridge: str
    ipv4: str
    mac: str
    interface: str


_NETWORK_PROFILES = (
    NetworkProfile(
        "neev-login-a", "nv-login-a", "10.203.0.0/29", "10.203.0.1", True
    ),
    NetworkProfile(
        "neev-login-b", "nv-login-b", "10.203.0.8/29", "10.203.0.9", True
    ),
    NetworkProfile(
        "neev-login-out",
        "nv-login-out",
        "10.203.0.16/29",
        "10.203.0.17",
        False,
    ),
)
_SLOT_PROFILES = MappingProxyType(
    {
        "a": SlotProfile("a", "neev-login-a", "10.203.0.2", "10.203.0.3"),
        "b": SlotProfile("b", "neev-login-b", "10.203.0.10", "10.203.0.11"),
    }
)
_ENDPOINT_ADDRESSES = MappingProxyType(
    {
        "proxy_a": ("nv-login-a", "10.203.0.2"),
        "proxy_b": ("nv-login-b", "10.203.0.10"),
        "proxy_out": ("nv-login-out", "10.203.0.18"),
        "guest_a": ("nv-login-a", "10.203.0.3"),
        "guest_b": ("nv-login-b", "10.203.0.11"),
    }
)
_REQUIRED_PROXY_ROLES = frozenset(("proxy_a", "proxy_b", "proxy_out"))
_MAC_PATTERN = re.compile(r"[0-9a-f]{2}(?::[0-9a-f]{2}){5}")
_INTERFACE_PATTERN = re.compile(r"veth[0-9a-f]{1,11}", re.ASCII)


def network_profiles() -> tuple[NetworkProfile, ...]:
    """Return the three fixed network reservations in creation order."""
    return _NETWORK_PROFILES


def slot_profile(slot: str) -> SlotProfile:
    """Return one of the two fixed guest-slot address assignments."""
    if type(slot) is not str or slot not in _SLOT_PROFILES:
        raise ProfileError("invalid")
    return _SLOT_PROFILES[slot]


def validate_endpoints(endpoints: tuple[Endpoint, ...]) -> tuple[Endpoint, ...]:
    """Validate inspected endpoint identity and return stable role order."""
    if type(endpoints) is not tuple or not 3 <= len(endpoints) <= 5:
        raise ProfileError("invalid")

    roles = set()
    macs = set()
    interfaces = set()
    for endpoint in endpoints:
        if type(endpoint) is not Endpoint:
            raise ProfileError("invalid")
        if endpoint.role not in _ENDPOINT_ADDRESSES or endpoint.role in roles:
            raise ProfileError("invalid")
        if (endpoint.bridge, endpoint.ipv4) != _ENDPOINT_ADDRESSES[endpoint.role]:
            raise ProfileError("invalid")
        if type(endpoint.mac) is not str or _MAC_PATTERN.fullmatch(endpoint.mac) is None:
            raise ProfileError("invalid")
        mac_bytes = bytes.fromhex(endpoint.mac.replace(":", ""))
        if mac_bytes == b"\x00" * 6 or mac_bytes[0] & 1:
            raise ProfileError("invalid")
        if (
            type(endpoint.interface) is not str
            or _INTERFACE_PATTERN.fullmatch(endpoint.interface) is None
        ):
            raise ProfileError("invalid")
        if endpoint.mac in macs or endpoint.interface in interfaces:
            raise ProfileError("invalid")
        roles.add(endpoint.role)
        macs.add(endpoint.mac)
        interfaces.add(endpoint.interface)

    if not _REQUIRED_PROXY_ROLES.issubset(roles):
        raise ProfileError("invalid")
    return tuple(sorted(endpoints, key=lambda endpoint: endpoint.role))
