#!/usr/bin/env python3
"""Production agent launch entrypoint — run ONE provider agent through the
per-tenant sandbox + provider-egress, streaming its stdout.

The native Rust runtime (`SandboxedProviderCliRuntime`) spawns THIS, never the
provider binary directly, so every agent process is isolated
(mount/pid/net/uts/ipc namespaces, private /proc, a dropped unprivileged uid that
owns a 0700 workdir, no-new-privs + empty capability bounding set, rlimits) and
its egress is default-deny except to its own provider proxy. We forward the
provider's stdout (stream-json) to our stdout unchanged; on our termination (the
runtime cancels a run by killing us) we tear the sandbox + egress down.

Linux only — `neev_agent_sandbox.launch` refuses off Linux. Usage:

    neev_agent_launch.py --tenant org:3/user:7 --slot a \
        --workdir /var/lib/neev-native/agents/org3-user7 --uid 61000 --gid 61000 \
        [--provider-ipv4 A.B.C.D]... -- claude -p "<prompt>" --output-format stream-json ...

With no --provider-ipv4 the egress policy is deny-all (a fresh empty netns); with
one or more it is a provider allowlist wired through the slot proxy.
"""

import argparse
import os
import re
import signal
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import neev_agent_sandbox as sandbox
import neev_agent_credentials as credentials


def _parse(argv):
    parser = argparse.ArgumentParser(prog="neev_agent_launch", add_help=True)
    parser.add_argument("--tenant", required=True)
    parser.add_argument("--slot", required=True, choices=sorted(sandbox._VALID_SLOTS))
    parser.add_argument("--workdir", required=True)
    parser.add_argument("--uid", type=int, required=True)
    parser.add_argument("--gid", type=int, required=True)
    # The runtime (execution.rs) always passes these: which per-user credential
    # volume to open (scope = --tenant + --provider) and the opaque handle naming
    # it. The launcher opens the caller's OWN volume and delivers it as HOME.
    parser.add_argument("--provider", required=True, choices=sorted(credentials.PROVIDERS))
    parser.add_argument("--credential-handle", required=True, dest="credential_handle")
    parser.add_argument("--provider-ipv4", action="append", default=[], dest="provider_ipv4")
    parser.add_argument("--cpu-seconds", type=int, default=120)
    parser.add_argument("--address-space-bytes", type=int, default=2 * 1024 * 1024 * 1024)
    parser.add_argument("--open-files", type=int, default=256)
    parser.add_argument("--processes", type=int, default=64)
    # Everything after `--` is the provider argv.
    parser.add_argument("provider_argv", nargs=argparse.REMAINDER)
    return parser.parse_args(argv)


def build_spec(args):
    """Pure: turn parsed args into a validated SandboxSpec (raises SandboxError)."""
    provider_argv = list(args.provider_argv)
    if provider_argv and provider_argv[0] == "--":
        provider_argv = provider_argv[1:]
    if not provider_argv:
        raise sandbox.SandboxError("invalid")
    egress = (
        sandbox.EgressPolicy.provider_allowlist(args.provider_ipv4)
        if args.provider_ipv4
        else sandbox.EgressPolicy.deny_all()
    )
    spec = sandbox.SandboxSpec(
        tenant=args.tenant,
        slot=args.slot,
        workdir=args.workdir,
        run_uid=args.uid,
        run_gid=args.gid,
        provider_argv=tuple(provider_argv),
        limits=sandbox.ResourceLimits(
            cpu_seconds=args.cpu_seconds,
            address_space_bytes=args.address_space_bytes,
            open_files=args.open_files,
            processes=args.processes,
        ),
        egress=egress,
    )
    sandbox.validate_spec(spec)
    return spec


_RUN_DIR = re.compile(r"run-[A-Za-z0-9._-]{1,64}\Z")


def _validate_workdir_under_trusted_root(workdir):
    """The caller (potentially an attacker who can exec the privileged helper)
    controls --workdir, but NOT the environment: the helper clears it and sets a
    trusted NEEV_AGENT_WORKSPACE_ROOT. Require --workdir to be a safe run-dir
    whose PARENT resolves (realpath, following any symlinks) to that trusted root,
    so a symlinked-parent cannot trick the privileged chown into an arbitrary
    directory. The final component is opened O_NOFOLLOW in the sandbox."""
    root_env = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
    if not root_env or not os.path.isabs(root_env):
        raise sandbox.SandboxError("invalid")
    root = os.path.realpath(root_env)
    if not os.path.isabs(workdir):
        raise sandbox.SandboxError("invalid")
    if os.path.realpath(os.path.dirname(workdir)) != root:
        raise sandbox.SandboxError("invalid")
    if _RUN_DIR.fullmatch(os.path.basename(workdir)) is None:
        raise sandbox.SandboxError("invalid")


# The provider inherits ONLY this allowlist plus the credential env — never the
# server's environment (DB URL, encryption key, ...). HOME/CLAUDE_CONFIG_DIR/
# CODEX_HOME come from the delivery and point strictly inside the sandbox volume.
_ENV_ALLOWLIST = ("PATH", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ")
_DEFAULT_PATH = "/usr/local/bin:/usr/bin:/bin"


def child_env(delivery, proxy_env=None):
    """Minimal, secret-free environment for the sandboxed provider (pure).

    When the run has provider egress, `proxy_env` is the authoritative fixed
    slot-proxy environment (`neev_login_network_profile.proxy_env`): the guest's
    netns only routes to the slot proxy IP, so without HTTP(S)_PROXY set the
    provider's direct TLS would have no path out. The proxy vars come from the
    server-owned profile, never inherited from the ambient environment."""
    env = {"PATH": _DEFAULT_PATH}
    for key in _ENV_ALLOWLIST:
        value = os.environ.get(key)
        if value is not None and "\x00" not in value:
            env[key] = value
    if proxy_env:
        env.update(proxy_env)  # HTTP(S)_PROXY + empty NO_PROXY (fixed, authoritative)
    env.update(delivery.env)  # HOME + provider config dir (authoritative)
    return env


def _become_credential_owner():  # pragma: no cover - requires the cap helper (euid 0)
    """Elevate to euid 0 for the credential/mount phase.

    The caplaunch helper execs us with euid 65532 (the web-server uid) plus ambient
    caps (incl. CAP_SETUID). But the credential store, master key and volume images
    are ROOT-owned, and their validators require owner == geteuid(); mount/cryptsetup
    also want euid 0. So the trusted, fixed launcher raises euid to 0 here — AFTER its
    args/workdir are validated, BEFORE any credential effect. The provider child is
    still dropped to its DISTINCT unprivileged lane uid with an empty capability set
    by the sandbox's setpriv, so nothing untrusted ever runs as root. Fail closed if
    elevation is impossible (e.g. the helper profile lacks CAP_SETUID)."""
    import platform

    if platform.system() != "Linux":
        raise sandbox.SandboxError("unsupported")
    if os.geteuid() == 0:
        return
    try:
        os.setresuid(0, 0, 0)
    except OSError as error:
        raise sandbox.SandboxError("launch") from error
    if os.geteuid() != 0:
        raise sandbox.SandboxError("launch")


def _production_volumes():  # pragma: no cover - requires the real store
    import neev_credential_store as store_module
    import neev_credential_volumes as volumes_module

    store = store_module.CredentialStore(
        volumes_module._STORE_ROOT, volumes_module._KEY_PATH
    )
    return volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())


def main(argv=None, volumes=None):
    args = _parse(sys.argv[1:] if argv is None else argv)
    try:
        _validate_workdir_under_trusted_root(args.workdir)
        spec = build_spec(args)
    except sandbox.SandboxError as error:
        print(f"neev_agent_launch: invalid spec: {error.category}", file=sys.stderr)
        return 2
    # Open the caller's OWN per-user/per-provider credential volume and lend it to
    # this run's lane uid for the CHILD'S ENTIRE LIFETIME (a per-scope run lock is
    # held across the whole `with`, incl. cancel/teardown), then restored to the
    # canonical owner on exit. A handle for another user/provider cannot unlock this
    # scope; the lock means no other lane can access the volume concurrently.
    if volumes is None:  # pragma: no cover - production wiring
        try:
            _become_credential_owner()
        except sandbox.SandboxError as error:
            print(
                f"neev_agent_launch: cannot assume credential owner: {error.category}",
                file=sys.stderr,
            )
            return 2
        volumes = _production_volumes()
    # A run with provider egress must route the provider's TLS through its slot
    # CONNECT proxy — the netns has no other path out. deny_all runs have no proxy
    # (and no network), so no proxy env is injected.
    proxy_env = None
    if spec.egress.mode == "provider_allowlist":
        import neev_login_network_profile as network_profile

        proxy_env = network_profile.proxy_env(args.slot)
    try:
        with credentials.deliver(
            volumes,
            args.tenant,
            args.provider,
            args.credential_handle,
            args.uid,
            args.gid,
        ) as delivery:
            return _run_child(spec, child_env(delivery, proxy_env))
    except credentials.CredentialError as error:
        # Honest, non-sensitive reason ("busy" = another run for this user+provider
        # holds the volume). Rust surfaces waiting/busy; nothing runs unauthorized.
        print(
            f"neev_agent_launch: credential delivery refused: {error.category}",
            file=sys.stderr,
        )
        return 5


def _run_child(spec, env):
    """Launch the provider in the sandbox, forward its stream, and always tear the
    sandbox down. Runs while the credential volume is lent to this run's uid."""
    try:
        handle = sandbox.launch(spec, env=env)
    except sandbox.SandboxError as error:
        print(f"neev_agent_launch: launch failed: {error.category}", file=sys.stderr)
        return 3

    # Keep the signal handler async-signal-safe: do ONLY a fast process-group
    # kill (single syscall) to unblock the stdout read; the heavy teardown
    # (reap + egress `ip`/`nft` commands) runs in the `finally`, and the credential
    # ownership is restored + the run lock released by the enclosing `with` after.
    def _terminate(*_unused):
        try:
            os.killpg(os.getpgid(handle.pid), signal.SIGKILL)
        except OSError:
            pass

    signal.signal(signal.SIGTERM, _terminate)
    signal.signal(signal.SIGINT, _terminate)

    rc = 4
    try:
        out = handle.stdout
        if out is not None:
            for line in out:  # forward provider stream-json line by line
                sys.stdout.buffer.write(line)
                sys.stdout.buffer.flush()
        rc = handle.wait()
    finally:
        handle.teardown()  # kill process group + reap + egress teardown (idempotent)
    return 0 if rc == 0 else 4


if __name__ == "__main__":
    raise SystemExit(main())
