#!/usr/bin/env python3
"""Resolve and deliver ONE run's per-user, per-provider credential volume into its
sandbox HOME — safely, without ever mutating the persistent volume.

The native runtime spawns `neev_agent_launch.py` with a server-authoritative
`--tenant org:<org>/user:<user>`, a `--provider`, and a `--credential-handle`. This
module turns those into a `neev_credential_store.Scope` and opens the caller's own
LUKS-encrypted credential volume via `neev_credential_volumes.CredentialVolumes`.

Isolation + concurrency invariants (why this cannot race a chown or corrupt refresh
state — the two failures called out in review):

* **Scope is server-authoritative, never the request.** The `(org_id, user_id)`
  come from `--tenant` (stamped by the runtime from the authenticated actor) and the
  provider from `--provider`; the volume key is derived from that scope + handle
  (`CredentialStore.volume_key`). A handle that belongs to another user/provider
  therefore cannot unlock this scope's volume — `open` fails. One tenant can never
  reach another's volume.
* **This module never writes to the volume — no chown, no unmount, no delete.** It
  `open`s (idempotent + serialized by the store's own exclusive `.lock`) and then
  only *reads* the mount root's identity to verify delivery is safe. Because it
  mutates nothing, two concurrent runs for the SAME (user, provider) — even on
  different isolation-lane uids — cannot race a chown or leave the credentials in a
  half-owned/corrupt state. Refresh tokens the provider writes go straight to the
  one live volume; the volume's own lifecycle (create/refresh/revoke/close) stays
  owned by the supervisor + `neev_credential_volumes`, not by a per-run launcher.
* **Ownership is required, not imposed.** The live volume is delivered as the run's
  HOME so refresh writes persist. For the dropped, unprivileged provider process to
  read AND write it, the volume's mount root must already be owned by the run's uid
  at mode 0700. We *verify* that and refuse otherwise — we do not chown a shared
  volume to a per-run lane uid (that is precisely the race). See the report:
  credential-bearing runs must execute as the volume-owner uid.

Both providers resolve their config strictly within the sandbox HOME: Claude reads
`$HOME/.claude.json` + `$CLAUDE_CONFIG_DIR`; Codex reads `$CODEX_HOME` — all set
under HOME here, so neither can escape to a host login.
"""

from __future__ import annotations

import os
import stat
from dataclasses import dataclass
from typing import Callable, Dict, Tuple

from neev_credential_store import Scope, StoreError

PROVIDERS: Tuple[str, ...] = ("claude", "codex")


class CredentialError(Exception):
    """Delivery refused. `category` is a stable, non-sensitive reason code."""

    def __init__(self, category: str):
        super().__init__(category)
        self.category = category


def parse_tenant(tenant: str) -> Tuple[int, int]:
    """Parse the runtime's `org:<int>/user:<int>` tenant string. Rejects anything
    else — no negatives, no leading zeros, no extra segments."""
    if not isinstance(tenant, str):
        raise CredentialError("tenant")
    parts = tenant.split("/")
    if len(parts) != 2 or not parts[0].startswith("org:") or not parts[1].startswith("user:"):
        raise CredentialError("tenant")
    org_text = parts[0][len("org:") :]
    user_text = parts[1][len("user:") :]

    def _positive(text: str) -> int:
        # ASCII digits only; no sign, no whitespace, no leading zero (canonical).
        if not text.isascii() or not text.isdigit():
            raise CredentialError("tenant")
        if len(text) > 1 and text[0] == "0":
            raise CredentialError("tenant")
        value = int(text)
        if value <= 0 or value > 2_147_483_647:
            raise CredentialError("tenant")
        return value

    return _positive(org_text), _positive(user_text)


def scope_of(tenant: str, provider: str) -> Scope:
    org_id, user_id = parse_tenant(tenant)
    if provider not in PROVIDERS:
        raise CredentialError("provider")
    return Scope(org_id, user_id, provider)


def provider_env(home: str, provider: str) -> Dict[str, str]:
    """The environment that binds the provider's config to the sandbox HOME.

    Every path is under HOME, so a provider can only ever read/write the mounted
    per-user volume — never a host login or another tenant's config."""
    if provider not in PROVIDERS:
        raise CredentialError("provider")
    env = {"HOME": home}
    if provider == "claude":
        # Claude reads $HOME/.claude.json and $CLAUDE_CONFIG_DIR (its state dir).
        env["CLAUDE_CONFIG_DIR"] = os.path.join(home, ".claude")
    else:
        # Codex reads $CODEX_HOME (default $HOME/.codex).
        env["CODEX_HOME"] = os.path.join(home, ".codex")
    return env


@dataclass(frozen=True)
class Delivery:
    """A resolved, ready-to-run credential delivery for one run."""

    home: str
    env: Dict[str, str]


def resolve_delivery(
    volumes,
    tenant: str,
    provider: str,
    handle: str,
    run_uid: int,
    *,
    stat_fn: Callable[[str], os.stat_result] = os.stat,
) -> Delivery:
    """Open the caller's own credential volume and return the HOME + env to run its
    provider with. Raises `CredentialError` (never leaks a path or secret) on any
    unsafe or unauthorized delivery. Mutates nothing.

    `volumes` is a `neev_credential_volumes.CredentialVolumes` (or a test double
    exposing `open(scope, handle) -> path`). `stat_fn` is injectable for tests."""
    if not isinstance(handle, str) or not handle:
        raise CredentialError("handle")
    if not isinstance(run_uid, int) or run_uid <= 0:
        raise CredentialError("uid")
    scope = scope_of(tenant, provider)
    try:
        mountpoint = volumes.open(scope, handle)
    except StoreError as error:
        # Unknown handle, scope/handle mismatch (another user's handle), a volume
        # that is not ready, or the store is busy — all map to a single opaque
        # refusal so probing cannot distinguish them.
        raise CredentialError("credentials") from error
    home = os.fspath(mountpoint)
    # No chown, no mkdir, no write. Verify the LIVE volume is already owned by the
    # run's uid at 0700 so the dropped provider can read + refresh it. Refusing here
    # (rather than chowning a shared volume to a transient lane uid) is what makes
    # concurrent same-user runs unable to race.
    try:
        info = stat_fn(home)
    except OSError as error:
        raise CredentialError("credentials") from error
    if not stat.S_ISDIR(info.st_mode):
        raise CredentialError("shape")
    if stat.S_IMODE(info.st_mode) != 0o700:
        raise CredentialError("mode")
    if info.st_uid != run_uid:
        # The volume is owned by a different uid than the run will drop to; running
        # anyway would leave the provider unable to read its own credentials, and
        # chowning a shared per-user volume to a per-run lane uid is the race we
        # refuse to introduce. See the report's uid-ownership contract.
        raise CredentialError("owner")
    return Delivery(home=home, env=provider_env(home, provider))
