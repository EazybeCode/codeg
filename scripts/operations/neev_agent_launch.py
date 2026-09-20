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


def child_env(delivery):
    """Minimal, secret-free environment for the sandboxed provider (pure)."""
    env = {"PATH": _DEFAULT_PATH}
    for key in _ENV_ALLOWLIST:
        value = os.environ.get(key)
        if value is not None and "\x00" not in value:
            env[key] = value
    env.update(delivery.env)  # HOME + provider config dir (authoritative)
    return env


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
    # Open the caller's OWN per-user/per-provider credential volume and deliver it
    # as the run's HOME. A handle for another user/provider cannot unlock this
    # scope's volume; a volume not owned by the run's uid is refused (never
    # chowned). Nothing agent-runs without its own valid credentials.
    if volumes is None:  # pragma: no cover - production wiring
        volumes = _production_volumes()
    try:
        delivery = credentials.resolve_delivery(
            volumes, args.tenant, args.provider, args.credential_handle, args.uid
        )
    except credentials.CredentialError as error:
        print(
            f"neev_agent_launch: credential delivery refused: {error.category}",
            file=sys.stderr,
        )
        return 5
    try:
        handle = sandbox.launch(spec, env=child_env(delivery))
    except sandbox.SandboxError as error:
        print(f"neev_agent_launch: launch failed: {error.category}", file=sys.stderr)
        return 3

    # Keep the signal handler async-signal-safe: do ONLY a fast process-group
    # kill (single syscall) to unblock the stdout read; the heavy teardown
    # (reap + egress `ip`/`nft` commands) runs in the `finally` on the main flow.
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
