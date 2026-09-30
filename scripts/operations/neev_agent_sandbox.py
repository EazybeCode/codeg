"""Per-tenant Linux isolation launcher for personal-agent execution.

Runs one tenant's agent process isolated from every other tenant, composing the
existing login-isolation model (fixed network slots in `neev_login_network_profile`,
per-scope encrypted volumes in `neev_credential_volumes`) with a process sandbox:
separate mount/pid/net/uts/ipc namespaces with a private /proc, a dedicated real
per-tenant uid/gid owning a 0700 workdir, POSIX
resource limits, a private working directory, and deterministic teardown of the
whole process group. A fresh network namespace with no configured veth denies all
egress by default; an allowlisted egress path is wired per slot as a follow-on.

Linux is the production target. Every real launch primitive returns `unsupported`
before any filesystem/namespace access on non-Linux, so this module is import- and
unit-safe on macOS/CI while its real acceptance runs on Linux
(`neev_agent_sandbox_acceptance.py`). The pure spec/argv/distinctness logic below
is unit tested on every platform; a passing unit suite does NOT prove kernel
isolation — that is the Linux acceptance gate.
"""

import os
import platform
import signal
import subprocess
from dataclasses import dataclass

UNSHARE = "/usr/bin/unshare"
SETPRIV = "/usr/bin/setpriv"
PRLIMIT = "/usr/bin/prlimit"
IP = "/usr/sbin/ip"
# Fixed interpreter + in-namespace bind prelude (same interpreter the caplaunch
# helper uses). The prelude lives beside this module so a relocated install
# (/opt/neev/scripts/operations) resolves it without configuration.
PYTHON = "/usr/bin/python3"
NS_PRELUDE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "neev_agent_ns_prelude.py")
# The in-sandbox HOME leaf under the run workdir (see `home_path`).
_HOME_LEAF = "home"

# A dedicated agent uid must be an unprivileged, non-login, non-service id.
_MIN_RUN_UID = 60000
_MAX_ID = 65533
_MAX_PATH_COMPONENTS = 32
_VALID_SLOTS = frozenset(("a", "b"))


def _valid_name(name, prefix):
    """A safe netns/link identifier: prefixed, bounded, and shell-metachar-free."""
    return (
        isinstance(name, str)
        and name.startswith(prefix)
        and 0 < len(name) <= 15  # Linux IFNAMSIZ / netns practical bound
        and all(c.isalnum() or c in "-_" for c in name)
    )


class SandboxError(Exception):
    _CATEGORIES = frozenset(("invalid", "unsupported", "launch", "teardown"))

    def __init__(self, category):
        if category not in self._CATEGORIES:
            category = "invalid"
        super().__init__(category)
        self.category = category

    def __repr__(self):
        return f"SandboxError({self.category})"


@dataclass(frozen=True)
class ResourceLimits:
    cpu_seconds: int
    address_space_bytes: int
    open_files: int
    processes: int


@dataclass(frozen=True)
class EgressPolicy:
    """Default-deny egress. `deny_all` gives an isolated net namespace with no
    route out; `provider_allowlist` permits ONLY the listed provider IPv4s (wired
    on Linux through the existing slot proxy, `neev_codex_login_egress`), denying
    every other destination and all cross-tenant traffic.
    """

    mode: str  # "deny_all" | "provider_allowlist"
    provider_ipv4: tuple = ()

    @staticmethod
    def deny_all():
        return EgressPolicy(mode="deny_all", provider_ipv4=())

    @staticmethod
    def provider_allowlist(ipv4):
        # Reuse the accepted IPv4 validator from the login egress proxy so the
        # allowlist shape is identical to the deployed slot-proxy policy.
        from neev_codex_login_egress import validated_ipv4

        return EgressPolicy(mode="provider_allowlist", provider_ipv4=validated_ipv4(list(ipv4)))


def egress_decision(policy, destination_ipv4):
    """Default-deny: permit a destination only under an explicit provider
    allowlist. `deny_all` and any unlisted destination are denied."""
    if not isinstance(policy, EgressPolicy):
        raise SandboxError("invalid")
    if policy.mode == "deny_all":
        return False
    if policy.mode == "provider_allowlist":
        return destination_ipv4 in policy.provider_ipv4
    raise SandboxError("invalid")


@dataclass(frozen=True)
class SandboxSpec:
    tenant: str
    slot: str
    workdir: str
    run_uid: int
    run_gid: int
    provider_argv: tuple
    limits: ResourceLimits
    egress: EgressPolicy = EgressPolicy(mode="deny_all", provider_ipv4=())


def _validate_workdir(path):
    if not isinstance(path, str) or not path.startswith("/") or "\x00" in path:
        raise SandboxError("invalid")
    parts = [c for c in path.split("/") if c != ""]
    if not parts or len(parts) > _MAX_PATH_COMPONENTS:
        raise SandboxError("invalid")
    if any(part in (".", "..") for part in parts):
        raise SandboxError("invalid")
    if "/" + "/".join(parts) != path.rstrip("/") or path.endswith("/"):
        raise SandboxError("invalid")


def validate_spec(spec):
    """Reject any spec that would weaken isolation; pure and platform-agnostic."""
    if not isinstance(spec, SandboxSpec):
        raise SandboxError("invalid")
    if not isinstance(spec.tenant, str) or not spec.tenant.strip():
        raise SandboxError("invalid")
    if spec.slot not in _VALID_SLOTS:
        raise SandboxError("invalid")
    _validate_workdir(spec.workdir)
    for identity in (spec.run_uid, spec.run_gid):
        if not isinstance(identity, int) or identity < _MIN_RUN_UID or identity > _MAX_ID:
            raise SandboxError("invalid")
    if (
        not isinstance(spec.provider_argv, tuple)
        or not spec.provider_argv
        or not all(isinstance(a, str) and "\x00" not in a for a in spec.provider_argv)
    ):
        raise SandboxError("invalid")
    limits = spec.limits
    if not isinstance(limits, ResourceLimits):
        raise SandboxError("invalid")
    for bound in (
        limits.cpu_seconds,
        limits.address_space_bytes,
        limits.open_files,
        limits.processes,
    ):
        if not isinstance(bound, int) or bound <= 0:
            raise SandboxError("invalid")
    egress = spec.egress
    if not isinstance(egress, EgressPolicy) or egress.mode not in (
        "deny_all",
        "provider_allowlist",
    ):
        raise SandboxError("invalid")
    if egress.mode == "provider_allowlist" and not egress.provider_ipv4:
        raise SandboxError("invalid")
    if egress.mode == "deny_all" and egress.provider_ipv4:
        raise SandboxError("invalid")


def home_path(workdir):
    """The in-sandbox HOME mountpoint: a `home` dir directly under the run workdir.

    The dropped lane uid can traverse this (the workdir is 0700 lane-owned with
    world-traversable ancestry under the trusted agents root), unlike the 0700
    root-owned secret credential store where the volume is physically mounted. The
    sandbox bind-mounts the credential volume here inside its private mount
    namespace (see `neev_agent_ns_prelude`)."""
    _validate_workdir(workdir)
    return os.path.join(workdir, _HOME_LEAF)


def build_launch_argv(spec, join_netns=None, bind_source=None):
    """Construct the isolated launch argv (pure; no execution).

    `bind_source`, when given, is the physical path of the delivered credential
    volume mount; the argv inserts the fixed in-namespace prelude (running as root
    inside the fresh mount namespace, before setpriv) to bind it privately onto
    `home_path(workdir)` and then exec the drop chain. Without it the provider gets
    no HOME bind (deny-all/no-credential runs).

    The launcher runs as root and `unshare` creates fresh mount/uts/ipc/pid
    namespaces (`--fork` so the agent is PID 1 of its own tree) plus a private
    `/proc` (`--mount-proc`) so only the sandbox's own processes are visible;
    `setpriv` then drops to a real, dedicated, unprivileged host uid/gid, clears
    supplementary groups, sets `no_new_privs` and empties the capability
    bounding + ambient/inheritable sets so a dropped agent can NEVER regain
    privilege by exec'ing a setuid-root binary; `prlimit` caps
    cpu/address-space/open-files/processes. The provider argv runs last.

    Network isolation has two mutually exclusive modes, selected by `join_netns`:
    * `join_netns=None` (deny-all): add `--net` so unshare makes a fresh
      anonymous net namespace with no veth — every egress is denied.
    * `join_netns="neev-agent-<slot>"` (provider allowlist): the caller has
      already wired a named netns via `neev_agent_egress` (veth to the slot
      bridge + default-deny nft with a single provider-proxy accept). We prefix
      `ip netns exec <netns>` and OMIT `--net` so the agent JOINS that wired
      namespace instead of getting thrown into a fresh empty one — otherwise the
      allowlist would be silently discarded and behave as deny-all.

    A single-uid user namespace is deliberately NOT used here: `unshare --user`
    with a single mapping forces `setgroups=deny` (breaking `--clear-groups`) and
    maps the inside uid back to host root, which weakens filesystem isolation.
    Dropping to a real host uid via `setpriv` gives genuine per-tenant credential
    separation (see `launch`, which also 0700-owns the workdir). A user namespace
    with proper subuid/subgid ranges can be layered on later as defence in depth.
    """
    validate_spec(spec)
    limits = spec.limits
    prefix = []
    unshare_argv = [UNSHARE, "--mount", "--uts", "--ipc", "--pid"]
    if join_netns is None:
        unshare_argv.append("--net")  # fresh empty netns => deny-all egress
    else:
        if not _valid_name(join_netns, "neev-agent-"):
            raise SandboxError("invalid")
        prefix = [IP, "netns", "exec", join_netns]  # join the pre-wired netns
    unshare_argv += ["--fork", "--mount-proc"]
    prelude = []
    if bind_source is not None:
        _validate_workdir(bind_source)  # abs, bounded, no '..'/NUL/trailing slash
        prelude = [
            PYTHON, "-I", NS_PRELUDE,
            "--src", bind_source,
            "--dst", home_path(spec.workdir),
            "--",
        ]
    return [
        *prefix,
        *unshare_argv,
        *prelude,
        PRLIMIT,
        "--cpu=%d" % limits.cpu_seconds,
        "--as=%d" % limits.address_space_bytes,
        "--nofile=%d" % limits.open_files,
        "--nproc=%d" % limits.processes,
        "--",
        SETPRIV,
        "--reuid=%d" % spec.run_uid,
        "--regid=%d" % spec.run_gid,
        "--clear-groups",
        "--no-new-privs",
        "--bounding-set=-all",
        "--inh-caps=-all",
        "--",
        *spec.provider_argv,
    ]


def distinct_tenants(first, second):
    """Two concurrent tenants must not share a uid, gid, slot or workdir."""
    validate_spec(first)
    validate_spec(second)
    return (
        first.run_uid != second.run_uid
        and first.run_gid != second.run_gid
        and first.slot != second.slot
        and first.workdir != second.workdir
    )


def _secure_own_workdir(workdir, run_uid, run_gid):  # pragma: no cover - Linux path
    """chown+chmod the workdir to the tenant uid at 0700 WITHOUT a TOCTOU or a
    symlink-follow: open the final component with O_NOFOLLOW|O_DIRECTORY and act
    on the fd, so a tenant cannot swap the path for a symlink between check and
    use and trick root into chowning an arbitrary target. Callers must also
    ensure no parent component is tenant-writable."""
    try:
        fd = os.open(workdir, os.O_RDONLY | os.O_NOFOLLOW | os.O_DIRECTORY)
    except OSError as error:
        raise SandboxError("invalid") from error
    try:
        # The run workdir is created by the UNPRIVILEGED web server (uid 65532, via
        # execution.rs) and THIS launcher runs with euid 0 but only the 9 ambient
        # caps the cap-helper grants — which deliberately do NOT include CAP_FOWNER.
        # fchmod on a file whose owner != euid needs CAP_FOWNER (CAP_DAC_OVERRIDE
        # does NOT cover chmod's owner check), so chmod'ing a web-owned dir EPERMs.
        # (The login/probe path passed only because it is driven by the supervisor,
        # which runs as real root with the full cap set.) We DO hold CAP_CHOWN, so:
        # take ownership to our own euid/egid (0,0) FIRST, THEN fchmod (now owner ==
        # euid, no CAP_FOWNER needed), THEN hand ownership to the tenant lane — chown
        # preserves the mode bits. All ops act on the O_NOFOLLOW fd (no TOCTOU /
        # symlink-follow); the caller guarantees no parent component is tenant-writable.
        os.fchown(fd, 0, 0)  # take ownership via CAP_CHOWN so the chmod is self-owned
        os.fchmod(fd, 0o700)  # owner == euid now -> no CAP_FOWNER required
        os.fchown(fd, run_uid, run_gid)  # hand to the lane uid/gid (CAP_CHOWN)
    except OSError as error:
        raise SandboxError("launch") from error
    finally:
        os.close(fd)


# Width of the login terminal. Wide enough that no CLI decides to wrap a long
# authorize URL for display (the kernel never inserts line breaks; a TUI might).
PTY_COLUMNS = 1000
PTY_ROWS = 50


def open_login_pty(columns=PTY_COLUMNS, rows=PTY_ROWS):
    """A pseudo-terminal pair for a login CLI that requires an interactive TTY.

    Gemini CLI's documented NO_BROWSER sign-in only runs when stdin is a TTY
    (`isInteractive()`; over a pipe it refuses with "Manual authorization is
    required but the current session is non-interactive"). Returns
    `(master_fd, slave_fd)`, both close-on-exec except that the caller hands the
    slave to exactly one child as stdin/stdout. Echo is disabled so a pasted
    authorization code is never reflected back into the output we parse."""
    import fcntl
    import pty
    import struct
    import termios

    master, slave = pty.openpty()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        attrs = termios.tcgetattr(slave)
        attrs[3] &= ~(termios.ECHO | termios.ECHONL)
        termios.tcsetattr(slave, termios.TCSANOW, attrs)
        os.set_inheritable(master, False)
    except Exception:
        os.close(master)
        os.close(slave)
        raise
    return master, slave


class SandboxHandle:
    def __init__(self, process, egress_plan=None, egress_runner=None, tty=None):
        self._process = process
        # (reader, writer) over the PTY master when launched with pty_stdio=True.
        self._tty = tty
        # When the tenant runs under a provider-allowlist, launch() wired a named
        # egress netns; teardown must reverse it so the wiring is scoped to the run.
        self._egress_plan = egress_plan
        self._egress_runner = egress_runner

    @property
    def pid(self):
        return self._process.pid

    @property
    def stdout(self):
        """The agent's piped stdout (provider stream-json), for forwarding; the PTY
        master when launched with pty_stdio=True."""
        if self._tty is not None:
            return self._tty[0]
        return self._process.stdout

    @property
    def stdin(self):
        """The agent's piped stdin, when launched with stdin_pipe=True (login flows
        that drive the provider's stdio) or pty_stdio=True; None otherwise."""
        if self._tty is not None:
            return self._tty[1]
        return self._process.stdin

    @property
    def stderr(self):
        """The agent's piped stderr, when launched with stderr_pipe=True (login flows
        that classify a failure); None otherwise."""
        return self._process.stderr

    def wait(self, timeout=None):
        return self._process.wait(timeout=timeout)

    def teardown(self):
        """Kill the entire sandbox process group, reap it, and tear down any
        provider-egress wiring. Idempotent."""
        try:
            os.killpg(os.getpgid(self._process.pid), signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError as error:
            raise SandboxError("teardown") from error
        finally:
            # Reap so repeated launch/teardown never leaks zombies, and always
            # attempt egress teardown even if the process was already gone.
            try:
                self._process.wait(timeout=5)
            except Exception:
                pass
            if self._tty is not None:
                for stream in self._tty:
                    try:
                        stream.close()
                    except OSError:
                        pass
                self._tty = None
            if self._egress_plan is not None and self._egress_runner is not None:
                import neev_agent_egress as _egress

                _egress.teardown(self._egress_plan, self._egress_runner)


class _CommandRunner:  # pragma: no cover - Linux path
    """Minimal runner for the egress command plan (checked, no shell)."""

    def run(self, argv):
        subprocess.run(argv, check=True, capture_output=True, timeout=15)


def _create_bind_mountpoint(workdir, run_uid, run_gid):  # pragma: no cover - Linux path
    """Create `home_path(workdir)` (in the parent, so the child mount namespace sees
    it) owned by the lane uid at 0700. Its own perms are masked once the volume is
    bound over it, but a sane owner keeps it safe if the bind ever fails."""
    target = home_path(workdir)
    try:
        os.mkdir(target, 0o700)
    except FileExistsError:
        pass
    except OSError as error:
        raise SandboxError("launch") from error
    fd = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_DIRECTORY)
    try:
        os.fchmod(fd, 0o700)
        os.fchown(fd, run_uid, run_gid)
    except OSError as error:
        raise SandboxError("launch") from error
    finally:
        os.close(fd)
    return target


def launch(spec, env=None, bind_source=None, stdin_pipe=False, stderr_pipe=False,
           pty_stdio=False):  # pragma: no cover - Linux production path
    """Launch the agent inside the sandbox. Linux only.

    `stdin_pipe=True` opens a pipe to the provider's stdin (login flows that drive the
    provider's stdio, e.g. Codex app-server / Claude paste-code); default DEVNULL.

    `stderr_pipe=True` captures the provider's stderr on a pipe so a login flow can read
    and CLASSIFY a failure (sanitized) before teardown; default DEVNULL (production runs
    never capture provider stderr).

    For a `provider_allowlist` egress policy this wires (and later tears down) a
    named per-tenant egress netns via `neev_agent_egress` and JOINS the agent to
    it; for `deny_all` the agent gets a fresh empty netns (no egress at all).

    `env`, when given, is the COMPLETE environment for the provider process — the
    caller (neev_agent_launch) builds a minimal allowlist (HOME + provider config +
    PATH/locale) so the server's own environment (DB URL, keys) is never inherited
    by the sandboxed provider. `None` keeps the inherited environment (dev/tests).

    `bind_source`, when given, is the physical credential-volume mount to bind onto
    the lane-traversable `home_path(workdir)` inside the sandbox's private mount
    namespace (so the 0700 secret store is never exposed).

    `pty_stdio=True` gives the provider a pseudo-terminal as stdin AND stdout (for a
    login CLI that refuses to run its official flow without a TTY — see
    `open_login_pty`). Mutually exclusive with `stdin_pipe`."""
    if pty_stdio and stdin_pipe:
        raise SandboxError("invalid")
    if platform.system() != "Linux":
        raise SandboxError("unsupported")
    if env is not None and not (
        isinstance(env, dict)
        and all(
            isinstance(k, str) and isinstance(v, str) and "\x00" not in k and "\x00" not in v
            for k, v in env.items()
        )
    ):
        raise SandboxError("invalid")
    validate_spec(spec)
    if not os.path.isdir(spec.workdir):
        raise SandboxError("invalid")
    # Enforce per-tenant credential isolation (TOCTOU/symlink-safe): the dropped
    # uid must own its own workdir at mode 0700 so no other tenant uid can
    # traverse or read it. A mount namespace alone does NOT hide a sibling
    # tenant's directory on the shared filesystem.
    _secure_own_workdir(spec.workdir, spec.run_uid, spec.run_gid)
    if bind_source is not None:
        _validate_workdir(bind_source)
        _create_bind_mountpoint(spec.workdir, spec.run_uid, spec.run_gid)

    egress_plan = None
    egress_runner = None
    join_netns = None
    if spec.egress.mode == "provider_allowlist":
        import neev_agent_egress as egress

        egress_runner = _CommandRunner()
        egress_plan = egress.plan_provider_network(
            spec.tenant, spec.slot, spec.egress.provider_ipv4
        )
        egress.apply(egress_plan, egress_runner)  # rolls back on any failure
        join_netns = egress_plan.netns

    argv = build_launch_argv(spec, join_netns=join_netns, bind_source=bind_source)
    master = slave = None
    try:
        if pty_stdio:
            master, slave = open_login_pty()
        process = subprocess.Popen(
            argv,
            cwd=spec.workdir,
            stdin=slave if pty_stdio else (subprocess.PIPE if stdin_pipe else subprocess.DEVNULL),
            stdout=slave if pty_stdio else subprocess.PIPE,
            stderr=subprocess.PIPE if stderr_pipe else subprocess.DEVNULL,
            start_new_session=True,
            close_fds=True,
            env=env,
        )
    except OSError as error:
        for fd in (master, slave):
            if fd is not None:
                os.close(fd)
        # Don't leak the egress wiring if the process failed to spawn.
        if egress_plan is not None:
            egress.teardown(egress_plan, egress_runner)
        raise SandboxError("launch") from error
    tty = None
    if pty_stdio:
        os.close(slave)  # only the child holds the terminal side now
        tty = (os.fdopen(master, "rb", buffering=0),
               os.fdopen(os.dup(master), "wb", buffering=0))
    return SandboxHandle(process, egress_plan=egress_plan, egress_runner=egress_runner,
                         tty=tty)
