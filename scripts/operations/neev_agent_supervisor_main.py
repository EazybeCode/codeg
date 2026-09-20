#!/usr/bin/env python3
"""Executable Neev supervisor daemon.

Wires the settled pieces into a runnable service: it opens the durable coordinator
fence, adopts the pre-bound control socket, constructs the seven-op dispatcher with
CONCRETE per-provider login drivers + account probes, RECOVERS from the durable
registry (fail-closed), and serves the control protocol.

Identity model (per deployment): the daemon runs as root 0:0 (the credential store,
key and volumes are root-owned and their validators check owner AND group), while
the IPC socket/key are group `control_gid` (65532) so the unprivileged web-server
coordinator can reach them — peer identity is checked separately by the transport.
The provider login process is still dropped to a distinct unprivileged lane uid by
the sandbox.

Config is read from the environment (fail-closed if a required value is missing or
malformed); the pure config + factory wiring is unit-tested, while the fence/socket/
serve glue is Linux-only.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import neev_agent_supervisor as sup
import neev_agent_login_drivers as drivers
import neev_supervisor_dto as dto


class ConfigError(Exception):
    """A missing/invalid daemon configuration value (fail-closed)."""


@dataclass(frozen=True)
class Config:
    socket_path: str          # pre-bound AF_UNIX control socket
    key_path: str             # 32-byte control MAC key
    store_root: str           # credential store root (root-owned)
    key_master_path: str      # credential master key
    workspace_root: str       # run workdirs live here (0711)
    registry_path: str        # durable connection registry
    control_gid: int          # IPC socket/key group (unprivileged web server)
    peer_uid: int             # expected connecting coordinator uid
    peer_gid: int             # expected connecting coordinator gid
    lane_uid: int             # unprivileged login lane uid
    lane_gid: int             # unprivileged login lane gid
    slot: str                 # network slot ("a"/"b")
    codex_login_argv: tuple   # argv to launch the Codex app-server login
    claude_login_argv: tuple  # argv to launch the Claude browser+paste login
    codex_probe_argv: tuple   # argv to launch a Codex app-server account probe
    claude_probe_argv: tuple  # argv for `claude auth status --json` (native probe)
    egress_ipv4: tuple        # validated provider egress IPv4s for the slot proxy


def _require(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise ConfigError(name)
    return value


def _require_int(name: str) -> int:
    raw = _require(name)
    try:
        return int(raw)
    except ValueError:
        raise ConfigError(name) from None


def config_from_env(environ: Optional[dict] = None) -> Config:
    env = os.environ if environ is None else environ
    get = lambda name: env.get(name)

    def need(name):
        value = get(name)
        if not value:
            raise ConfigError(name)
        return value

    def need_int(name):
        try:
            return int(need(name))
        except ValueError:
            raise ConfigError(name) from None

    def argv(name):
        # A NUL-separated argv (so a login command with spaces is unambiguous).
        raw = need(name)
        parts = tuple(part for part in raw.split("\x00") if part)
        if not parts:
            raise ConfigError(name)
        return parts

    slot = need("NEEV_SUPERVISOR_SLOT")
    if slot not in ("a", "b"):
        raise ConfigError("NEEV_SUPERVISOR_SLOT")
    ipv4 = tuple(part for part in need("NEEV_SUPERVISOR_EGRESS_IPV4").split(",") if part)
    if not ipv4:
        raise ConfigError("NEEV_SUPERVISOR_EGRESS_IPV4")
    return Config(
        socket_path=need("NEEV_SUPERVISOR_SOCKET"),
        key_path=need("NEEV_SUPERVISOR_KEY"),
        store_root=need("NEEV_SUPERVISOR_STORE_ROOT"),
        key_master_path=need("NEEV_SUPERVISOR_MASTER_KEY"),
        workspace_root=need("NEEV_AGENT_WORKSPACE_ROOT"),
        registry_path=need("NEEV_SUPERVISOR_REGISTRY"),
        control_gid=need_int("NEEV_SUPERVISOR_CONTROL_GID"),
        peer_uid=need_int("NEEV_SUPERVISOR_PEER_UID"),
        peer_gid=need_int("NEEV_SUPERVISOR_PEER_GID"),
        lane_uid=need_int("NEEV_SUPERVISOR_LANE_UID"),
        lane_gid=need_int("NEEV_SUPERVISOR_LANE_GID"),
        slot=slot,
        codex_login_argv=argv("NEEV_SUPERVISOR_CODEX_LOGIN_ARGV"),
        claude_login_argv=argv("NEEV_SUPERVISOR_CLAUDE_LOGIN_ARGV"),
        codex_probe_argv=argv("NEEV_SUPERVISOR_CODEX_PROBE_ARGV"),
        claude_probe_argv=argv("NEEV_SUPERVISOR_CLAUDE_PROBE_ARGV"),
        egress_ipv4=ipv4,
    )


# --------------------------------------------------------------------------- #
# Driver / probe factories (pure wiring; the sandbox spawn is deferred to start())
# --------------------------------------------------------------------------- #


def _run_dir(config: Config) -> str:  # pragma: no cover - creates a real dir
    import tempfile

    return tempfile.mkdtemp(prefix="run-", dir=config.workspace_root)


def _login_spec(config: Config, tenant: str, argv_tuple):
    import neev_agent_sandbox as sandbox

    return sandbox.SandboxSpec(
        tenant=tenant,
        slot=config.slot,
        workdir=_run_dir(config),
        run_uid=config.lane_uid,
        run_gid=config.lane_gid,
        provider_argv=tuple(argv_tuple),
        limits=sandbox.ResourceLimits(
            cpu_seconds=300, address_space_bytes=2 * 1024 * 1024 * 1024,
            open_files=256, processes=64,
        ),
        egress=sandbox.EgressPolicy.provider_allowlist(config.egress_ipv4),
    )


def _login_env(config: Config, provider: dto.Provider, home: str):
    import neev_agent_credentials as credentials
    import neev_login_network_profile as profile

    env = {"PATH": "/usr/local/bin:/usr/bin:/bin"}
    env.update(profile.proxy_env(config.slot))  # route login TLS via the slot proxy
    env.update(credentials.provider_env(home, provider.value))
    return env


def make_factories(config: Config, volumes, *, channel_factory=None):
    """Build (driver_for, probe_for) for the Supervisor. `channel_factory(spec, mount,
    env)` builds a Channel (defaults to the production SandboxChannel); tests inject a
    fake so the wiring is exercised without spawning."""
    import neev_agent_sandbox as sandbox

    build_channel = channel_factory or (
        lambda spec, mount, env: drivers.SandboxChannel(spec, mount, env)
    )

    def _channel(provider, store_scope, handle, argv_tuple):
        mount = os.fspath(volumes.open(store_scope, handle))
        tenant = "org:%d/user:%d" % (store_scope.org_id, store_scope.user_id)
        spec = _login_spec(config, tenant, argv_tuple)
        env = _login_env(config, provider, sandbox.home_path(spec.workdir))
        return build_channel(spec, mount, env), mount

    def _credential_present(mount, provider):
        # A completed login leaves the provider's config dir in the volume HOME.
        sub = ".codex" if provider is dto.Provider.CODEX else ".claude"
        return lambda: os.path.isdir(os.path.join(mount, sub))

    def driver_for(provider, store_scope, handle):
        argv_tuple = (
            config.codex_login_argv if provider is dto.Provider.CODEX
            else config.claude_login_argv
        )
        channel, mount = _channel(provider, store_scope, handle, argv_tuple)
        if provider is dto.Provider.CODEX:
            return drivers.CodexLoginDriver(channel)
        return drivers.ClaudeLoginDriver(channel, _credential_present(mount, provider))

    def probe_for(provider, store_scope, handle):
        probe_argv = (
            config.codex_probe_argv if provider is dto.Provider.CODEX
            else config.claude_probe_argv
        )
        channel, _mount = _channel(provider, store_scope, handle, probe_argv)
        if provider is dto.Provider.CODEX:
            return drivers.CodexAccountProbe(channel)
        # Native `claude auth status --json` against the volume (not file presence).
        return drivers.ClaudeAccountProbe(channel)

    return driver_for, probe_for


def build_supervisor(config: Config, volumes, boot_id: dto.Id, *, channel_factory=None,
                     now=None, mint_id=None) -> sup.Supervisor:
    import secrets

    driver_for, probe_for = make_factories(config, volumes, channel_factory=channel_factory)
    return sup.Supervisor(
        boot_id, volumes, driver_for,
        now=now or (lambda: int(_monotonic_unixsec())),
        mint_id=mint_id or (lambda: secrets.token_hex(16)),
        probe_for=probe_for,
        durable=sup.FileRegistry(config.registry_path),
    )


def _monotonic_unixsec():  # pragma: no cover - wall clock
    import time

    return int(time.time())


def main(argv=None):  # pragma: no cover - Linux daemon glue
    import asyncio
    import platform
    import uuid

    if platform.system() != "Linux":
        raise SystemExit("neev supervisor: Linux only")
    if os.geteuid() != 0 or os.getegid() != 0:
        raise SystemExit("neev supervisor: must run as root 0:0")
    config = config_from_env()

    import neev_agent_credentials as _creds  # noqa: F401 - ensures deps import
    import neev_credential_store as store_module
    import neev_credential_volumes as volumes_module
    import neev_supervisor_fence as fence_mod
    import neev_supervisor_transport as transport

    store = store_module.CredentialStore(Path(config.store_root), Path(config.key_master_path))
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    boot_id = dto.Id(uuid.uuid4().hex)
    supervisor = build_supervisor(config, volumes, boot_id)
    try:
        supervisor.recover()  # fail-closed: RegistryError aborts startup
    except sup.RegistryError as error:
        raise SystemExit("neev supervisor: durable recovery failed: %s" % error.reason)

    fence = fence_mod.FenceOwner.open(_fence_deployment(config))
    listener = transport.SupervisorListener.adopt(
        _adopt_socket(config), _control_deployment(config)
    )
    try:
        asyncio.run(sup.serve(listener, fence, supervisor))
    finally:
        listener.close()
        fence.close()
    return 0


def _adopt_socket(config):  # pragma: no cover - Linux socket
    import socket

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(config.socket_path)
    os.chown(config.socket_path, 0, config.control_gid)  # root:control_gid
    os.chmod(config.socket_path, 0o660)
    sock.listen(32)
    return sock


def _fence_deployment(config):  # pragma: no cover - Linux filesystem inspection
    raise SystemExit(
        "neev supervisor: fence deployment construction is provided by the deploy "
        "wiring (protected-path stat); see NEEV handoff"
    )


def _control_deployment(config):  # pragma: no cover - Linux filesystem inspection
    raise SystemExit(
        "neev supervisor: control deployment construction is provided by the deploy "
        "wiring (peer_uid/gid, control_gid, key identity); see NEEV handoff"
    )


if __name__ == "__main__":
    raise SystemExit(main())
