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
import threading

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
    # Which Codex transport to run. `exec` is the deployment's existing
    # behaviour and stays the default, so an older runtime that passes nothing
    # gets exactly what it got before.
    parser.add_argument(
        "--codex-stream",
        choices=("exec", "app-server"),
        default="exec",
        dest="codex_stream",
    )
    parser.add_argument("--cpu-seconds", type=int, default=120)
    # `None` means "unset", so an operator's explicit value is never silently
    # replaced by the per-provider default below.
    parser.add_argument("--address-space-bytes", type=int, default=None)
    parser.add_argument("--open-files", type=int, default=256)
    parser.add_argument("--processes", type=int, default=64)
    # Everything after `--` is the provider argv.
    parser.add_argument("provider_argv", nargs=argparse.REMAINDER)
    return parser.parse_args(argv)


# Codex's own flag for this exact situation. Its help: "Skip all confirmation
# prompts and execute commands without sandboxing. EXTREMELY DANGEROUS. Intended
# solely for running in environments that are externally sandboxed."
#
# This IS that environment, and only here. Codex ships bubblewrap and tries to
# build a sandbox of its own before running any shell command; inside ours that
# is not merely redundant, it is impossible. Measured in the real sandbox: user
# namespaces are available but `CapBnd` is empty, so `CAP_SYS_ADMIN` can never be
# held even in a namespace the process creates itself, and bwrap dies at
# `Failed to make / slave: Permission denied`. Every command the model tried to
# run failed there, which reached the reader as "the shell tool failed".
#
# What it turns off is CODEX's sandbox. Ours is untouched and is the actual
# boundary: fresh mount/uts/ipc/pid namespaces, egress confined to the CONNECT
# allowlist (or an empty netns), uid 61000, an emptied capability bounding set,
# `no_new_privs`, and a per-run workdir removed on teardown.
#
# It lives here, NOT in the shared Rust `provider_argv`, because reaching this
# function means `sandbox.launch` is what will run the child. The unsandboxed
# dev runtime builds the same argv without passing through here and therefore
# keeps Codex's own protection, which is the only thing standing between it and
# the developer's machine.
_EXTERNALLY_SANDBOXED = "--dangerously-bypass-approvals-and-sandbox"

# Address space is RESERVED, not used, and V8 reserves on a scale no general
# limit anticipates. Codex's code-mode host — which every current model requires,
# since they all report `tool_mode: code_mode_only` — creates a V8 isolate whose
# sandbox cage measured 1.35 TiB of virtual address space on the pinned binary.
# Under the 2 GiB default it died with "Fatal process out of memory: Failed to
# reserve the virtual address space for the V8 sandbox", its stdout closed, and
# every command the model tried to run came back as a tool error. 8 GiB failed
# identically: this is not a number to tune upward a little.
#
# So Codex gets a finite ceiling above what V8 asks for, and PHYSICAL memory
# stays bounded where it already was — the container's own cgroup (`memory.max`
# 2 GiB, swap capped to match, pids 512). RLIMIT_AS does impose an upper bound;
# it just cannot distinguish reserved from committed memory, and the cage is
# reserved `PROT_NONE` and never committed. Claude keeps the 2 GiB default, and
# an operator's explicit `--address-space-bytes` still wins over both.
_ADDRESS_SPACE_BYTES = 2 * 1024**3
_CODEX_ADDRESS_SPACE_BYTES = 2 * 1024**4


def address_space_bytes(provider, requested):
    """Pure: the address-space ceiling for one run. An explicit value wins."""
    if requested is not None:
        return requested
    return _CODEX_ADDRESS_SPACE_BYTES if provider == "codex" else _ADDRESS_SPACE_BYTES


def externally_sandboxed_argv(provider, provider_argv):
    """Pure: add the bypass to a `codex exec` run, and put the prompt behind `--`.

    Narrow on purpose. `codex app-server` (the metadata probe) and any login
    flow run no model-generated commands, so they have no nested sandbox to
    disable, and `claude` is a different CLI entirely. Approvals go with the
    sandbox because `exec` is non-interactive: a prompt nobody can answer is a
    turn that hangs rather than a turn that is safe.

    The prompt moves because a bare positional is read as an option when it
    starts with a dash. A prompt of `--help` printed the help and ran no turn;
    one shaped like `-c notify=[…]` would have been read as a config override.
    After `--` both are the prompt and a turn starts. Nothing is parsed,
    escaped or interpolated: the text is moved, never inspected, and every
    other argument keeps its position and its pairing, `--image FILE` included.

    Recognising the canonical shape is what makes this safe. If the argv is not
    exactly what `provider_argv` builds — `exec`, `--json`, the prompt, then
    options — it is handed back untouched rather than reordered on a guess.
    """
    argv = list(provider_argv)
    if provider != "codex" or len(argv) < 4:
        return argv
    if argv[1] != "exec" or argv[2] != "--json":
        return argv
    # Already normalised: what we produce always ENDS `-- <prompt>`. Testing the
    # separator's position rather than its presence matters, because a prompt
    # may itself be the two characters `--`.
    if argv[-2] == "--":
        return argv
    prompt, options = argv[3], argv[4:]
    head = [argv[0], "exec"]
    # Checked against the OPTIONS, never the whole argv: a prompt that happens
    # to read `--dangerously-bypass-approvals-and-sandbox` is a user's words,
    # not a flag already present, and must not suppress the real one.
    if _EXTERNALLY_SANDBOXED not in options:
        head.append(_EXTERNALLY_SANDBOXED)
    return head + ["--json"] + options + ["--", prompt]


# The in-sandbox streaming adapter, beside this launcher in the same root-owned
# 0555 directory. `-I` matches the ns prelude: no user site-packages, no
# inherited PYTHONPATH.
ADAPTER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "neev_codex_stream_adapter.py")


# The `-c key=value` overrides `provider_argv` builds for `codex exec`, and what
# each becomes for the app-server. Effort and tier have parameters of their own
# in the pinned protocol, so they are sent as parameters and the thread reports
# back what it took; fast mode has none and stays a config override. An override
# outside this table is refused rather than dropped — it is a choice somebody
# made, and a turn that ignores it looks like a turn that honoured it.
_EFFORT_OVERRIDE = re.compile(r'\Amodel_reasoning_effort="([A-Za-z-]{1,32})"\Z')
_TIER_OVERRIDE = re.compile(r'\Aservice_tier="([A-Za-z-]{1,32})"\Z')
_FAST_MODE_OVERRIDE = re.compile(r"\Afeatures\.fast_mode=(?:true|false)\Z")


def _config_argument(value):
    """Pure: one `-c` override -> the adapter's arguments for it."""
    effort = _EFFORT_OVERRIDE.match(value)
    if effort:
        return ["--effort", effort.group(1)]
    tier = _TIER_OVERRIDE.match(value)
    if tier:
        return ["--service-tier", tier.group(1)]
    if _FAST_MODE_OVERRIDE.match(value):
        return ["--config", value]
    raise sandbox.SandboxError("invalid")


def app_server_argv(provider, provider_argv, adapter=ADAPTER, python=sandbox.PYTHON):
    """Pure: rewrite a `codex exec --json` launch into the streaming adapter's.

    `exec` emits nothing until an item is finished, so a reply appears only once
    the turn is over. The adapter drives `codex app-server`, which streams the
    message as it is written, and it runs HERE — as the sandboxed provider
    process — so it owns the app-server's stdin inside the isolation boundary
    instead of asking for a pipe through it. The launcher's own stdin stays
    DEVNULL for every paid turn, exactly as before.

    Only the canonical shape `provider_argv` builds is accepted. Anything else
    raises rather than being rewritten on a guess or quietly run as `exec`: an
    argument this function does not know is a selection the reader made and
    would otherwise lose. Codex's sandbox bypass is NOT added — the adapter
    expresses the same thing as thread parameters.
    """
    argv = list(provider_argv)
    if provider != "codex":
        return argv
    if tuple(argv[1:]) == _METADATA_ARGV:  # noqa: F821 - defined below, read at call time
        # The catalogue probe already IS an app-server launch, driven by the
        # runtime over the bounded stdin relay. It is not a turn and has no
        # prompt to stream.
        return argv
    if len(argv) < 4 or argv[1] != "exec" or argv[2] != "--json":
        raise sandbox.SandboxError("invalid")
    binary, prompt, options = argv[0], argv[3], argv[4:]
    mapped = ["--codex", binary]
    index = 0
    while index < len(options):
        option = options[index]
        if option in ("-c", "-m", "--image"):
            if index + 1 >= len(options):
                raise sandbox.SandboxError("invalid")
            value = options[index + 1]
            if option == "-c":
                mapped += _config_argument(value)
            else:
                mapped += [{"-m": "--model", "--image": "--image"}[option], value]
            index += 2
            continue
        if option == "--skip-git-repo-check":
            # An `exec` precondition with no app-server equivalent: a thread is
            # started on a cwd, not on a checkout.
            index += 1
            continue
        raise sandbox.SandboxError("invalid")
    return [python, "-I", adapter] + mapped + ["--", prompt]


def build_spec(args):
    """Pure: turn parsed args into a validated SandboxSpec (raises SandboxError)."""
    provider_argv = list(args.provider_argv)
    if provider_argv and provider_argv[0] == "--":
        provider_argv = provider_argv[1:]
    if not provider_argv:
        raise sandbox.SandboxError("invalid")
    if args.codex_stream == "app-server":
        provider_argv = app_server_argv(args.provider, provider_argv)
    else:
        provider_argv = externally_sandboxed_argv(args.provider, provider_argv)
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
            address_space_bytes=address_space_bytes(
                args.provider, args.address_space_bytes
            ),
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
    """Elevate to euid 0 AND egid 0 for the credential/mount phase, WITHOUT becoming
    unsignalable by the web-server parent that launched us.

    The caplaunch helper execs us with ruid/euid/egid 65532 (the web-server uid/gid)
    plus ambient caps (incl. CAP_SETUID/CAP_SETGID). The credential store, master key
    and volume images are ROOT-owned (uid 0, gid 0) and their validators check BOTH
    owner == geteuid() and group == getegid(); mount/cryptsetup also want euid 0. So the
    trusted, fixed launcher raises BOTH euid and egid to 0 here — AFTER its args/workdir
    are validated, BEFORE any credential effect.

    Crucially we keep the CALLER'S uid as the SAVED set-uid instead of `setresuid(0,0,0)`:
    kill(2) permits a sender whose real/effective uid equals the target's real OR saved
    set-uid, so preserving suid == the web uid lets that parent (which runs with zero
    effective caps) still cancel us — deliver the SIGTERM whose handler tears the sandbox
    down and cleans the workdir. `setresuid(0,0,0)` would drop the saved-uid to 0 and
    make cancel from the parent impossible; adding CAP_KILL to the helper only lets US
    signal the inner provider, never the parent signal US. ruid/euid are 0, so cryptsetup
    and the root-owned store's owner==geteuid check still pass, and the provider child is
    still dropped to its DISTINCT unprivileged lane uid/gid with an empty capability set
    by the sandbox's setpriv. Fail closed if elevation is impossible."""
    import platform

    if platform.system() != "Linux":
        raise sandbox.SandboxError("unsupported")
    if os.geteuid() == 0 and os.getegid() == 0:
        return
    caller_uid = os.getuid()  # the web-server parent's ruid (e.g. 65532)
    try:
        os.setresgid(0, 0, 0)  # CAP_SETGID
        os.setresuid(0, 0, caller_uid)  # ruid/euid 0; saved-uid stays the caller's
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
                spec,
                child_env(delivery, proxy_env),
                bind_source=delivery.mount,
                relay_stdin=is_metadata_probe(args.provider, spec),
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


# The metadata probe is the ONE shape that needs a writable stdin, and it is
# recognised exactly: provider `codex`, invoked as nothing but `app-server`.
# `codex exec` runs — every turn a user pays for — keep DEVNULL, so no prompt or
# provider stream can ever be fed from outside the sandbox.
_METADATA_ARGV = ("app-server",)
# Enough for the handshake the reader sends (initialize, initialized, model/list)
# with room to spare, and far too little to smuggle a payload through.
_STDIN_RELAY_MAX_BYTES = 64 * 1024


def is_metadata_probe(provider, spec):
    """Pure: does this launch drive the catalogue probe, and nothing else?"""
    return provider == "codex" and tuple(spec.provider_argv[1:]) == _METADATA_ARGV


def _relay_stdin(destination, source, limit=_STDIN_RELAY_MAX_BYTES):
    """Copy at most `limit` bytes from `source` to `destination`, then close it.

    Bounded and one-way. The reader keeps its end open until the answer arrives,
    so this returns on EOF, on the cap, or when the child has gone; closing the
    child's stdin is what lets a well-behaved app-server finish.
    """
    written = 0
    try:
        while written < limit:
            chunk = source.read1(min(4096, limit - written)) if hasattr(
                source, "read1"
            ) else source.read(min(4096, limit - written))
            if not chunk:
                break
            destination.write(chunk)
            destination.flush()
            written += len(chunk)
    except (BrokenPipeError, ValueError, OSError):
        pass  # the child exited first; its rc is the outcome, not this
    finally:
        try:
            destination.close()
        except (BrokenPipeError, OSError):
            pass
    return written


def _run_child(spec, env, bind_source=None, relay_stdin=False):
    """Launch the provider in the sandbox, forward its stream, and always tear the
    sandbox down. Runs while the credential volume is lent to this run's uid."""
    try:
        handle = sandbox.launch(
            spec, env=env, bind_source=bind_source, stdin_pipe=relay_stdin
        )
    except sandbox.SandboxError as error:
        print(f"neev_agent_launch: launch failed: {error.category}", file=sys.stderr)
        return 3

    # The probe is a conversation: the app-server answers `initialize` before it
    # will accept a method, so its stdin must stay open until the caller has what
    # it asked for. Without this the child read EOF immediately and exited, and
    # the catalogue was never read.
    if relay_stdin and handle.stdin is not None:
        threading.Thread(
            target=_relay_stdin,
            args=(handle.stdin, sys.stdin.buffer),
            daemon=True,
        ).start()

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
