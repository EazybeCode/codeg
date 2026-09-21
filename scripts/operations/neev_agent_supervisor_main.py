#!/usr/bin/env python3
"""Executable Neev supervisor daemon.

Opens the durable coordinator fence, adopts the pre-bound control socket, constructs
the seven-op dispatcher with CONCRETE per-provider login drivers + account probes,
purges abandoned run workdirs (crash recovery), RECOVERS from the durable registry
(fail-closed), and serves the control protocol.

Identity: the daemon runs root 0:0 (the credential store/key/volumes are root-owned
and validators check owner AND group); the IPC socket/key are group `control_gid`
(65532) so the unprivileged web-server coordinator can reach them; the connecting
peer's identity is checked separately (peer_uid/peer_gid). The provider login process
is still dropped to a distinct unprivileged lane uid by the sandbox, and its
credential volume is delivered with the SAME lend/restore + per-scope run lock +
workdir cleanup as a runtime agent run.

Provider commands are FIXED (not attacker/env influenced); config env carries only
paths, ids and the egress allowlist (env values cannot contain NUL, so no packed
argv). The pure config, deployment construction, purge and factory wiring are unit
tested; the fence open / socket adopt / serve loop are Linux-only.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import neev_agent_supervisor as sup
import neev_agent_login_drivers as drivers
import neev_supervisor_dto as dto

# FIXED provider commands (verified where possible against the pinned image). Never
# taken from the environment. Codex uses its app-server stdio protocol; Claude uses
# the interactive login and the native status probe.
_CODEX_LOGIN_ARGV = ("codex", "app-server")
_CODEX_PROBE_ARGV = ("codex", "app-server")
_CLAUDE_LOGIN_ARGV = ("claude", "auth", "login")
_CLAUDE_PROBE_ARGV = ("claude", "auth", "status", "--json")

_RUN_DIR_RE = re.compile(r"run-[A-Za-z0-9._-]{1,64}\Z")
_KEY_BASENAME = "control.key"  # the transport loads the key by this fixed name


class ConfigError(Exception):
    """A missing/invalid daemon configuration value (fail-closed)."""


@dataclass(frozen=True)
class Config:
    socket_path: str          # AF_UNIX control socket (basename under socket parent)
    key_path: str             # 32-byte control MAC key (basename control.key, 0440)
    fence_dir: str            # protected dir holding the coordinator fence lock+record
    store_root: str           # credential store root (root-owned 0700)
    master_key_path: str      # credential master key
    workspace_root: str       # run workdirs live here (0711)
    registry_path: str        # durable connection registry
    control_gid: int          # IPC socket/key group (unprivileged web server)
    peer_uid: int             # expected connecting coordinator uid
    peer_gid: int             # expected connecting coordinator gid
    lane_uid: int             # unprivileged login lane uid
    lane_gid: int             # unprivileged login lane gid
    slot: str                 # network slot ("a"/"b")
    uplink: str               # container uplink interface for the egress bootstrap NAT
    egress_ipv4: tuple        # validated provider egress IPv4s for the slot proxy


def config_from_env(environ: Optional[dict] = None) -> Config:
    env = os.environ if environ is None else environ

    def need(name):
        value = env.get(name)
        if not value:
            raise ConfigError(name)
        return value

    def need_int(name):
        try:
            return int(need(name))
        except ValueError:
            raise ConfigError(name) from None

    slot = need("NEEV_SUPERVISOR_SLOT")
    if slot not in ("a", "b"):
        raise ConfigError("NEEV_SUPERVISOR_SLOT")
    ipv4 = tuple(part for part in need("NEEV_SUPERVISOR_EGRESS_IPV4").split(",") if part)
    if not ipv4:
        raise ConfigError("NEEV_SUPERVISOR_EGRESS_IPV4")
    key_path = need("NEEV_SUPERVISOR_KEY")
    if os.path.basename(key_path) != _KEY_BASENAME:
        raise ConfigError("NEEV_SUPERVISOR_KEY")
    return Config(
        socket_path=need("NEEV_SUPERVISOR_SOCKET"),
        key_path=key_path,
        fence_dir=need("NEEV_SUPERVISOR_FENCE_DIR"),
        store_root=need("NEEV_SUPERVISOR_STORE_ROOT"),
        master_key_path=need("NEEV_SUPERVISOR_MASTER_KEY"),
        workspace_root=need("NEEV_AGENT_WORKSPACE_ROOT"),
        registry_path=need("NEEV_SUPERVISOR_REGISTRY"),
        control_gid=need_int("NEEV_SUPERVISOR_CONTROL_GID"),
        peer_uid=need_int("NEEV_SUPERVISOR_PEER_UID"),
        peer_gid=need_int("NEEV_SUPERVISOR_PEER_GID"),
        lane_uid=need_int("NEEV_SUPERVISOR_LANE_UID"),
        lane_gid=need_int("NEEV_SUPERVISOR_LANE_GID"),
        slot=slot,
        uplink=need("NEEV_SUPERVISOR_UPLINK"),
        egress_ipv4=ipv4,
    )


# --------------------------------------------------------------------------- #
# Fail-closed cross-tenant reuse guard
# --------------------------------------------------------------------------- #


class ChannelError(Exception):
    """A login channel that could not be brought up OR torn down cleanly. Raised so a
    teardown failure surfaces (a live child / half-removed workdir) instead of being
    swallowed, and so a quarantined slot refuses to build a new channel.

    The durable slot quarantine itself lives in `neev_slot_proxy` (a file marker that
    survives the process and is shared with the runtime launcher), because the login
    lane uid is FIXED per slot: a slot whose sandbox child or run workdir did not tear
    down cleanly must not be handed to the next login/probe, and the marker is only
    cleared by the privileged restart purge or a deliberate operator action."""


# --------------------------------------------------------------------------- #
# Login channel — full credential-delivery lifecycle (matches a runtime run)
# --------------------------------------------------------------------------- #


class LoginChannel(drivers.Channel):  # pragma: no cover - Linux lifecycle path
    """A Channel whose lifetime IS the login process's: on start() it takes the
    per-slot proxy lock (fail busy), lends the credential volume to the lane uid under
    the per-scope run lock (canonical 65532 at rest), binds it into the sandbox HOME
    and launches the provider with stdin; on terminate() it tears the sandbox down,
    restores the canonical owner + releases the run lock, removes the run workdir, and
    releases the slot lock — the same owner-handoff + locking + cleanup a runtime
    agent run uses."""

    def __init__(self, config, volumes, provider, store_scope, handle, argv, *,
                 cleanup=None, quarantine_dir=None):
        self._config = config
        self._volumes = volumes
        self._provider = provider
        self._store_scope = store_scope
        self._handle = handle
        self._argv = argv
        self._cleanup = cleanup  # injectable for tests; default is the launcher's
        self._quarantine_dir = quarantine_dir  # None -> slot_proxy default
        self._workdir = None
        self._proxy_cm = None
        self._delivery_cm = None
        self._sandbox = None

    def start(self):
        import neev_agent_credentials as credentials
        import neev_agent_sandbox as sandbox
        import neev_slot_proxy as slot_proxy

        # Own the slot's LOGIN proxy for the whole login: this takes the exclusive slot
        # lock (busy propagates -> driver start fails), refuses a quarantined or already
        # occupied slot, spawns the namespaced CONNECT proxy bound to the login
        # allowlist, and yields the authoritative HTTP(S)_PROXY env. The provider's TLS
        # has no other path out of the netns.
        self._proxy_cm = slot_proxy.slot_proxy(
            self._provider.value, "login", self._config.slot,
            quarantine_dir=self._quarantine_dir,
        )
        proxy_env = self._proxy_cm.__enter__()
        try:
            self._workdir = _make_run_dir(self._config)
            home = sandbox.home_path(self._workdir)
            tenant = _tenant(self._store_scope)
            self._delivery_cm = credentials.deliver(
                self._volumes, tenant, self._provider.value, self._handle,
                self._config.lane_uid, self._config.lane_gid, home_as=home,
            )
            delivery = self._delivery_cm.__enter__()
            spec = _login_spec(self._config, tenant, self._argv, self._workdir)
            env = _login_env(self._config, self._provider, home, proxy_env)
            self._sandbox = sandbox.launch(
                spec, env=env, bind_source=delivery.mount, stdin_pipe=True
            )
            os.set_blocking(self._sandbox.stdout.fileno(), False)
        except BaseException:
            self._unwind()
            raise

    def read(self):
        if self._sandbox is None or self._sandbox.stdout is None:
            return b""
        try:
            return os.read(self._sandbox.stdout.fileno(), 65536) or b""
        except (BlockingIOError, OSError):
            return b""

    def write(self, data):
        if self._sandbox is None or self._sandbox.stdin is None:
            raise OSError("no stdin")
        self._sandbox.stdin.write(data)
        self._sandbox.stdin.flush()

    def returncode(self):
        if self._sandbox is None:
            return None
        try:
            return self._sandbox.wait(timeout=0)
        except Exception:
            return None

    def terminate(self):
        self._unwind()

    def _unwind(self):
        import neev_slot_proxy as slot_proxy

        cleanup = self._cleanup
        if cleanup is None:
            import neev_agent_launch as launch

            cleanup = launch._cleanup_run_workdir
        clean = True
        if self._sandbox is not None:
            try:
                self._sandbox.teardown()  # SIGKILL the lane child + reap (needs CAP_KILL)
            except Exception:
                clean = False  # teardown failed -> a live lane-uid child may survive
            self._sandbox = None
        if self._delivery_cm is not None:
            try:
                self._delivery_cm.__exit__(None, None, None)  # restore canonical + release run lock
            except Exception:
                clean = False
            self._delivery_cm = None
        if self._workdir is not None:
            if not cleanup(self._workdir):  # symlink-safe bounded delete; False on failure
                clean = False  # a run workdir left behind would leak into the next tenant
            self._workdir = None
        # Stop + reap the login proxy child and release the slot lock. slot_proxy's own
        # __exit__ writes the durable quarantine marker if the proxy child cannot be
        # confirmed gone (SlotProxyError), so a live proxy also blocks reuse.
        if self._proxy_cm is not None:
            try:
                self._proxy_cm.__exit__(None, None, None)
            except Exception:
                clean = False
            self._proxy_cm = None
        if clean:
            return
        # The sandbox child or the run workdir did not tear down cleanly. The login lane
        # uid is FIXED, so the next login/probe MUST NOT reuse this slot: write the same
        # durable slot-proxy quarantine marker (cleared only by the privileged restart
        # purge or an operator) and raise so the failure is surfaced, never swallowed.
        slot_proxy.quarantine(self._config.slot, directory=self._quarantine_dir)
        raise ChannelError("teardown_failed")


def _tenant(store_scope) -> str:
    return "org:%d/user:%d" % (store_scope.org_id, store_scope.user_id)


def _make_run_dir(config) -> str:  # pragma: no cover - creates a real dir
    import tempfile

    return tempfile.mkdtemp(prefix="run-", dir=config.workspace_root)


def _login_spec(config, tenant, argv_tuple, workdir):
    import neev_agent_sandbox as sandbox

    return sandbox.SandboxSpec(
        tenant=tenant, slot=config.slot, workdir=workdir,
        run_uid=config.lane_uid, run_gid=config.lane_gid,
        provider_argv=tuple(argv_tuple),
        limits=sandbox.ResourceLimits(
            cpu_seconds=300, address_space_bytes=2 * 1024 * 1024 * 1024,
            open_files=256, processes=64,
        ),
        egress=sandbox.EgressPolicy.provider_allowlist(config.egress_ipv4),
    )


def _login_env(config, provider, home, proxy_env):
    import neev_agent_credentials as credentials

    env = {"PATH": "/usr/local/bin:/usr/bin:/bin"}
    env.update(proxy_env)  # authoritative slot-proxy env from the live slot_proxy CM
    env.update(credentials.provider_env(home, provider.value))
    return env


_MAX_CREDS_BYTES = 1 << 20


def _credential_present(config, handle, provider):
    """True only when a REAL Claude OAuth credential record is stored.

    Used ONLY for Claude (the Codex driver proves completion via account/read). Neither
    the config dir's existence nor a file COUNT proves a login succeeded: the dir is
    pre-created by credential delivery (Codex needs $CODEX_HOME), and a FAILED login
    still drops settings/debug files. The credential RECORD is what matters — the pinned
    CLI writes it to `.claude/.credentials.json` as a `claudeAiOauth` object — so require
    that file to parse (bounded) to a claudeAiOauth object with non-empty accessToken AND
    refreshToken. Reads only at the euid-0 daemon's store mount path; checks presence and
    shape, NEVER token values. Secondary to the driver's rc-0 gate."""
    creds_path = os.path.join(
        config.store_root, handle, "mount", ".claude", ".credentials.json"
    )

    def present():
        try:
            with open(creds_path, "rb") as fp:
                raw = fp.read(_MAX_CREDS_BYTES + 1)
        except OSError:
            return False
        if not raw or len(raw) > _MAX_CREDS_BYTES:
            return False
        try:
            data = json.loads(raw)
        except (ValueError, UnicodeDecodeError):
            return False
        oauth = data.get("claudeAiOauth") if isinstance(data, dict) else None
        if not isinstance(oauth, dict):
            return False
        access = oauth.get("accessToken")
        refresh = oauth.get("refreshToken")
        return (
            isinstance(access, str) and bool(access)
            and isinstance(refresh, str) and bool(refresh)
        )

    return present


# --------------------------------------------------------------------------- #
# Driver / probe factories
# --------------------------------------------------------------------------- #


def make_factories(config: Config, volumes, *, channel_factory=None, quarantine_dir=None):
    """Build (driver_for, probe_for). `channel_factory(provider, store_scope, handle,
    argv)` builds a Channel (default: the full-lifecycle LoginChannel); tests inject a
    fake so the wiring is exercised without a real sandbox.

    Before building ANY channel — login or probe — the slot is checked against the
    durable `neev_slot_proxy` quarantine: a slot whose previous teardown could not be
    confirmed clean refuses here, so `driver_for`/`probe_for` raise and the supervisor
    reports UNAVAILABLE rather than reusing a slot that may still hold a live lane-uid
    child. The marker is cleared only by the privileged restart purge or an operator."""

    def build(provider, store_scope, handle, argv):
        import neev_slot_proxy as slot_proxy

        if slot_proxy.is_quarantined(config.slot, directory=quarantine_dir):
            raise ChannelError("slot_quarantined")
        if channel_factory is not None:
            return channel_factory(provider, store_scope, handle, argv)
        return LoginChannel(
            config, volumes, provider, store_scope, handle, argv,
            quarantine_dir=quarantine_dir,
        )

    def driver_for(provider, store_scope, handle):
        if provider is dto.Provider.CODEX:
            channel = build(provider, store_scope, handle, _CODEX_LOGIN_ARGV)
            return drivers.CodexLoginDriver(channel)
        channel = build(provider, store_scope, handle, _CLAUDE_LOGIN_ARGV)
        return drivers.ClaudeLoginDriver(
            channel, _credential_present(config, handle, provider)
        )

    def probe_for(provider, store_scope, handle):
        if provider is dto.Provider.CODEX:
            channel = build(provider, store_scope, handle, _CODEX_PROBE_ARGV)
            return drivers.CodexAccountProbe(channel)
        channel = build(provider, store_scope, handle, _CLAUDE_PROBE_ARGV)
        return drivers.ClaudeAccountProbe(channel)

    return driver_for, probe_for


def build_supervisor(config: Config, volumes, boot_id: dto.Id, *, channel_factory=None,
                     now=None, mint_id=None) -> sup.Supervisor:
    import secrets
    import time

    driver_for, probe_for = make_factories(config, volumes, channel_factory=channel_factory)
    return sup.Supervisor(
        boot_id, volumes, driver_for,
        now=now or (lambda: int(time.time())),
        mint_id=mint_id or (lambda: secrets.token_hex(16)),
        probe_for=probe_for,
        durable=sup.FileRegistry(config.registry_path),
    )


# --------------------------------------------------------------------------- #
# Startup: purge abandoned run workdirs (crash recovery)
# --------------------------------------------------------------------------- #


def purge_abandoned_runs(workspace_root: str, cleanup=None) -> int:
    """Before serving (a fresh daemon has NO live children), privileged-purge every
    validated abandoned `run-*` workdir under the trusted workspace root, so a crash
    that left lane-owned run dirs cannot leak into a later same-uid run and does not
    need operator intervention. Symlink-safe + bounded (reuses the launcher cleanup);
    only run-* names under THIS root are touched — never other containers/runs.
    Returns the count removed."""
    import neev_agent_launch as launch

    remove = cleanup or launch._cleanup_run_workdir
    removed = 0
    try:
        names = os.listdir(workspace_root)
    except OSError:
        return 0
    for name in names:
        if _RUN_DIR_RE.fullmatch(name) is None:
            continue
        path = os.path.join(workspace_root, name)
        if os.path.islink(path) or not os.path.isdir(path):
            continue
        if remove(path):
            removed += 1
    return removed


def _abandoned_runs_remain(workspace_root: str) -> bool:
    """True if any validated `run-*` dir is STILL present after a purge — i.e. the
    privileged cleanup could not remove a leftover. The daemon refuses to serve in that
    state rather than reuse a lane whose previous tenant's files survive."""
    try:
        names = os.listdir(workspace_root)
    except OSError:
        return False
    for name in names:
        if _RUN_DIR_RE.fullmatch(name) is None:
            continue
        path = os.path.join(workspace_root, name)
        if not os.path.islink(path) and os.path.isdir(path):
            return True
    return False


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #
#
# The protected-path FenceDeployment/ControlDeployment constructors live in
# `neev_supervisor_deployment` (owned by the deployment worker); main() imports them.
# Contract (reported to the handoff): given this Config, build the deployments by
# stat'ing the root-owned fence dir (+ coordinator-fence.lock/.record), the control
# socket parent, the control key parent and the key file, pinning their device/inode;
# identities are self=0:0, peer=(peer_uid,peer_gid), control_gid, socket_owner_uid=0.


def main(argv=None):  # pragma: no cover - Linux daemon glue
    import asyncio
    import platform
    import socket

    import neev_agent_network_bootstrap as bootstrap
    import neev_credential_store as store_module
    import neev_credential_volumes as volumes_module
    import neev_slot_proxy as slot_proxy
    import neev_supervisor_deployment as deployment
    import neev_supervisor_fence as fence_mod
    import neev_supervisor_transport as transport

    if platform.system() != "Linux":
        raise SystemExit("neev supervisor: Linux only")
    if os.geteuid() != 0 or os.getegid() != 0:
        raise SystemExit("neev supervisor: must run as root 0:0")
    config = config_from_env()
    # _cleanup_run_workdir + the login workdir validation read this.
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    paths, policy = deployment.from_config(config)

    # Prepare protected state (fence dir+lock+record, IPC dir + control key) via the
    # deployment module BEFORE anything binds; then purge abandoned runs and bring up
    # the egress bridges/proxy netns. All of this happens before the control socket is
    # bind-ready, which is what the container-init waits on.
    deployment.provision(paths, policy)
    purge_abandoned_runs(config.workspace_root)
    if _abandoned_runs_remain(config.workspace_root):
        raise SystemExit(
            "neev supervisor: abandoned run workdirs could not be purged; refusing to serve"
        )
    # The privileged purge verified this workspace is clean, so a durable slot
    # quarantine left by a prior crash/teardown can be lifted — restart IS the recovery
    # path (retry only after verified privileged cleanup, never by reusing in place).
    slot_proxy.clear_quarantine(config.slot)
    bootstrap.apply(bootstrap.plan(config.uplink), bootstrap.CommandRunner())

    store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
    # Create the pristine store roots + lock + master key BEFORE anything reads it.
    # Create-only: an already-provisioned store keeps its existing key untouched, and a
    # populated store missing its key is refused rather than re-keyed. Without this a
    # fresh deployment binds fine but the first start_login's volumes.create fails.
    store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())

    # Open the fence, bind + adopt the control socket, THEN build the supervisor from the
    # listener's boot id. The listener mints the supervisor boot id used in BOTH the
    # handshake and per-response correlation, so the supervisor MUST stamp its responses
    # with the SAME id: building it with an independent uuid made ServerChannel.finish
    # reject every response as a correlation mismatch (the daemon appeared to serve but
    # every op failed). One boot id, sourced from the listener, is the invariant.
    fence = fence_mod.FenceOwner.open(deployment.fence_deployment(paths, policy))
    bind_path = deployment.socket_path(paths)
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(bind_path)
    os.chown(bind_path, policy.supervisor_uid, policy.control_gid)  # root:control_gid
    os.chmod(bind_path, 0o660)
    sock.listen(32)
    listener = transport.SupervisorListener.adopt(
        sock, deployment.control_deployment(paths, policy)
    )
    supervisor = build_supervisor(config, volumes, listener.supervisor_boot_id)
    try:
        supervisor.recover()  # fail-closed, BEFORE any request is served
    except sup.RegistryError as error:
        listener.close()
        fence.close()
        raise SystemExit("neev supervisor: durable recovery failed: %s" % error.reason)

    try:
        asyncio.run(sup.serve(listener, fence, supervisor))
    finally:
        listener.close()
        fence.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
