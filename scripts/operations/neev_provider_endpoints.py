"""Explicit per-provider, per-purpose CONNECT-proxy destination allowlists.

The slot egress proxy is fixed-destination: the sandboxed provider can reach ONLY
the exact TLS authorities named here — never a wildcard or an all-Internet fallback.
Each entry is a `(provider, purpose)` -> tuple of `Endpoint(host, port, confirmed)`,
where `purpose` separates the interactive LOGIN flow (OAuth/device) from the RUNTIME
chat flow, because they hit different hosts.

Provenance (why each host is on the list):

  * ("codex", "login")  auth.openai.com:443
        The Codex app-server device-login verification URL is
        https://auth.openai.com/codex/device (see neev_codex_login_protocol) and the
        existing login proxy already pins this host. CONFIRMED.
  * ("claude", "login") claude.com:443, console.anthropic.com:443
        Root probed pinned claude 2.1.267 (query stripped, no secrets logged): the
        default --claudeai flow opens https://claude.com/cai/oauth/authorize.
        console.anthropic.com is the documented --console flow host.
        claude.com CONFIRMED (probed); console.anthropic.com doc-derived.
  * ("claude", "run")   api.anthropic.com:443
        The Anthropic Messages API host the Claude CLI calls at runtime. Doc-derived;
        a runtime probe should confirm before claiming runtime egress operational.
  * ("codex", "run")    chatgpt.com:443, api.openai.com:443, auth.openai.com:443
        ChatGPT-auth Codex runs hit the ChatGPT backend (chatgpt.com); API-key runs
        hit api.openai.com; both may refresh tokens via auth.openai.com. Doc-derived
        pending a runtime probe.

`confirmed=False` entries are DELIBERATELY included so egress can be wired and
end-to-end tested, but callers/reports must not claim runtime egress is proven until
an isolated probe on the pinned image confirms each one (`unconfirmed()` lists them).
"""

from __future__ import annotations

from dataclasses import dataclass
import re


PROVIDERS = ("claude", "codex")
PURPOSES = ("login", "run")

# A public DNS FQDN: lowercase, dot-separated labels, at least two labels, <=253
# bytes, no wildcard/underscore/trailing dot.
_HOST_RE = re.compile(
    r"(?=.{1,253}\Z)[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?"
    r"(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+\Z"
)


class EndpointError(Exception):
    """A fixed, non-sensitive endpoint-config failure."""

    def __init__(self, code: str = "invalid"):
        self.code = code
        super().__init__(code)


@dataclass(frozen=True)
class Endpoint:
    host: str
    port: int
    confirmed: bool

    def __post_init__(self):
        if type(self.host) is not str or _HOST_RE.fullmatch(self.host) is None:
            raise EndpointError("host")
        if type(self.port) is not int or isinstance(self.port, bool) or not (
            1 <= self.port <= 65535
        ):
            raise EndpointError("port")
        if type(self.confirmed) is not bool:
            raise EndpointError("invalid")

    def authority(self) -> bytes:
        """The `host:port` authority bytes a CONNECT request must name exactly."""
        return ("%s:%d" % (self.host, self.port)).encode("ascii")


# Anthropic's official network-access docs (code.claude.com/docs/en/network-config):
# platform.claude.com performs the OAuth token EXCHANGE/REFRESH/REVOKE for BOTH the
# claude.ai and Console flows, so it is required for login AND run (api.anthropic.com
# alone cannot refresh). claude.ai authentication is also officially required
# (claude.com redirects to it). These are doc-confirmed.
_ENDPOINTS = {
    ("codex", "login"): (Endpoint("auth.openai.com", 443, True),),
    ("claude", "login"): (
        Endpoint("claude.com", 443, True),            # observed authorize origin
        Endpoint("claude.ai", 443, True),             # official auth (redirect target)
        Endpoint("console.anthropic.com", 443, True),  # official --console flow
        Endpoint("platform.claude.com", 443, True),   # official OAuth token exchange
        # The pinned CLI completes login by fetching its role/profile from
        # api.anthropic.com/api/oauth/claude_cli/roles AFTER the code exchange; without
        # this host that call is denied, the CLI exits nonzero, and login never lands
        # credentials. Precise host, login purpose only — no wildcard.
        Endpoint("api.anthropic.com", 443, True),
    ),
    ("claude", "run"): (
        Endpoint("api.anthropic.com", 443, True),     # official Messages API
        Endpoint("platform.claude.com", 443, True),   # official OAuth token refresh
    ),
    ("codex", "run"): (
        Endpoint("chatgpt.com", 443, False),
        Endpoint("api.openai.com", 443, False),
        Endpoint("auth.openai.com", 443, False),
    ),
}


def _key(provider: str, purpose: str):
    if provider not in PROVIDERS or purpose not in PURPOSES:
        raise EndpointError("scope")
    return (provider, purpose)


def endpoints(provider: str, purpose: str) -> tuple:
    """The exact allowlisted endpoints for one provider + purpose (never empty)."""
    return _ENDPOINTS[_key(provider, purpose)]


def hosts(provider: str, purpose: str) -> frozenset:
    return frozenset(endpoint.host for endpoint in endpoints(provider, purpose))


def authorities(provider: str, purpose: str) -> frozenset:
    """The set of `host:port` authority byte-strings a CONNECT may target."""
    return frozenset(endpoint.authority() for endpoint in endpoints(provider, purpose))


def port_for(provider: str, purpose: str, host: str) -> int:
    for endpoint in endpoints(provider, purpose):
        if endpoint.host == host:
            return endpoint.port
    raise EndpointError("host")


def unconfirmed(provider: str, purpose: str) -> tuple:
    """Endpoints not yet proven by an isolated probe on the pinned image."""
    return tuple(e for e in endpoints(provider, purpose) if not e.confirmed)
