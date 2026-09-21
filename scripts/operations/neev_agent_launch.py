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
    """Elevate to euid 0 AND egid 0 for the credential/mount phase.

    The caplaunch helper execs us with euid/egid 65532 (the web-server uid/gid) plus
    ambient caps (incl. CAP_SETUID/CAP_SETGID). The credential store, master key and
    volume images are ROOT-owned (uid 0, gid 0) and their validators check BOTH
    owner == geteuid() and group == getegid(); mount/cryptsetup also want euid 0. So
    the trusted, fixed launcher raises BOTH euid and egid to 0 here — AFTER its
    args/workdir are validated, BEFORE any credential effect. (Raising only the uid
    left egid at 65532, so a root-owned mountpoint failed the gid check.) The provider
    child is still dropped to its DISTINCT unprivileged lane uid/gid with an empty
    capability set by the sandbox's setpriv, so nothing untrusted runs as root. Fail
    closed if elevation is impossible (e.g. the profile lacks CAP_SETUID/CAP_SETGID)."""
    import platform

    if platform.system() != "Linux":
        raise sandbox.SandboxError("unsupported")
    if os.geteuid() == 0 and os.getegid() == 0:
        return
    try:
        os.setresgid(0, 0, 0)  # CAP_SETGID
        os.setresuid(0, 0, 0)  # CAP_SETUID (after gid: still privileged here)
    except OSError as error:
        raise sandbox.SandboxError("launch") from error
    if os.geteuid() != 0 or os.getegid() != 0:
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
    # The provider sees its credentials at a lane-traversable HOME under the run
    # workdir; the sandbox bind-mounts the physical volume there in its private
    # mount namespace (the 0700 secret store is never made traversable).
    home_as = sandbox.home_path(spec.workdir)
    # A run with provider egress must route the provider's TLS through its slot CONNECT
    # proxy — the netns has no other path out. The slot_proxy context manager owns the
    # slot for the child's WHOLE lifetime: it takes the exclusive slot lock (busy ->
    # another run holds the endpoint), refuses a quarantined/occupied slot, spawns the
    # namespaced proxy bound to the RUN allowlist, yields the authoritative proxy env,
    # and stops+reaps the proxy before releasing the lock. deny_all runs have no proxy
    # (and no network), so nothing is spawned.
    if spec.egress.mode == "provider_allowlist":
        import neev_slot_proxy as slot_proxy

        try:
            with slot_proxy.slot_proxy(args.provider, "run", args.slot) as proxy_env:
                rc = _deliver_run_cleanup(volumes, args, spec, proxy_env, home_as)
                if rc == 6:
                    # Workdir cleanup failed: the FIXED lane must not be reused. Write
                    # the durable slot quarantine too so the next run's slot_proxy also
                    # refuses (belt-and-braces with the Rust LaneGuard's rc-6 handling).
                    slot_proxy.quarantine(args.slot)
                return rc
        except slot_proxy.SlotProxyError as error:
            # busy/occupied/quarantined/timeout/unavailable — never a host or policy
            # detail. A proxy that could not be confirmed dead has already quarantined
            # the slot inside the context manager.
            print(
                f"neev_agent_launch: slot proxy unavailable: {error.category}",
                file=sys.stderr,
            )
            return 7
    return _deliver_run_cleanup(volumes, args, spec, None, home_as)


def _deliver_run_cleanup(volumes, args, spec, proxy_env, home_as):
    """Open the caller's OWN volume, lend it to the lane uid for the child's whole
    lifetime, run the sandboxed provider, then privileged-remove the run workdir.

    Returns the child's rc, or 5 (credential delivery refused) / 6 (workdir cleanup
    failed -> lane must be quarantined, not reused)."""
    try:
        with credentials.deliver(
            volumes,
            args.tenant,
            args.provider,
            args.credential_handle,
            args.uid,
            args.gid,
            home_as=home_as,
        ) as delivery:
            rc = _run_child(
                spec, child_env(delivery, proxy_env), bind_source=delivery.mount
            )
    except credentials.CredentialError as error:
        # Honest, non-sensitive reason ("busy" = another run for this user+provider
        # holds the volume). Rust surfaces waiting/busy; nothing runs unauthorized.
        print(
            f"neev_agent_launch: credential delivery refused: {error.category}",
            file=sys.stderr,
        )
        return 5
    # The child has terminated and the sandbox is torn down. Remove THIS run's workdir
    # tree ourselves (we are the privileged, euid-0 launcher) BEFORE returning: the
    # provider ran as the lane uid and may have left files in its CWD owned by that
    # uid, which a later run REUSING the same lane uid could otherwise read. The Rust
    # LaneGuard runs as 65532 and cannot traverse a 0700 lane-owned run dir, so it
    # cannot do this. If cleanup fails we return a distinct code so the lane is
    # QUARANTINED (not reused) rather than leaking leftovers.
    if not _cleanup_run_workdir(args.workdir):
        print(
            "neev_agent_launch: run workdir cleanup failed (lane must be quarantined)",
            file=sys.stderr,
        )
        return 6
    return rc


_MAX_RUN_ENTRIES = 8192


def _cleanup_run_workdir(workdir):
    """Remove the run workdir tree as the euid-0 launcher. Symlink-safe, bounded, and
    ONLY the validated run dir under the trusted root — so a later run reusing the
    lane uid cannot read leftover CWD files. Returns True iff fully removed; refuses
    (False) to delete through a mount or an unexpected/oversized tree (fail closed)."""
    try:
        _validate_workdir_under_trusted_root(workdir)
    except sandbox.SandboxError:
        return False
    try:
        if os.path.islink(workdir) or not os.path.isdir(workdir):
            return False
        seen = 0
        for current, dirs, files in os.walk(workdir, topdown=False, followlinks=False):
            for name in files:
                seen += 1
                if seen > _MAX_RUN_ENTRIES:
                    return False
                os.unlink(os.path.join(current, name))
            for name in dirs:
                seen += 1
                if seen > _MAX_RUN_ENTRIES:
                    return False
                path = os.path.join(current, name)
                if os.path.islink(path):
                    os.unlink(path)
                elif os.path.ismount(path):
                    return False  # never delete through a mount
                else:
                    os.rmdir(path)
        os.rmdir(workdir)
        return True
    except OSError:
        return False


def _run_child(spec, env, bind_source=None):
    """Launch the provider in the sandbox, forward its stream, and always tear the
    sandbox down. Runs while the credential volume is lent to this run's uid."""
    try:
        handle = sandbox.launch(spec, env=env, bind_source=bind_source)
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
