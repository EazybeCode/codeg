#!/usr/bin/env python3
"""Production-path acceptance for the ACP providers (Cursor, Gemini CLI,
OpenCode), in a disposable QA scope, WITHOUT any account.

Everything runs through the code production runs, never a friendlier harness:

  * SIGN-IN  — `neev_agent_supervisor_main.make_factories(...).driver_for`
    with the real `LoginChannel`: slot login proxy (login allowlist), a fresh
    per-user LUKS credential volume lent to the lane uid, the sandbox, the
    pinned CLI. Driven until the provider's own challenge is shown, then
    CANCELLED. The challenge is never opened and no consent is ever given; only
    its host is recorded (never the URL or a user code).
  * PROBE    — the production `probe_for` (daemon-side record check), and the
    same probe argv through a real `LoginChannel` so the adapter itself answers
    from inside the sandbox: both must say `disconnected`, never `supported`.
  * RUN      — the REAL caplaunch helper, invoked as the web uid 65532, with the
    run egress (`--provider-ipv4`, i.e. the slot proxy on the RUN allowlist), the
    canonical ACP argv the server builds (a staged image with no extension, a
    local stdio library tool, an HTTP library tool): the HTTP tool must be named
    as skipped, and the turn must end with the honest sign-in failure.
  * CANCEL   — SIGTERM from the web uid mid-start; the whole tree must exit
    promptly and the run workdir must be gone.
  * ISOLATION— tenant B presenting tenant A's volume handle is refused.

After each step: no run workdir is left, the slot is not quarantined, and no
process of the lane uid survives.

Run as ROOT inside the ACP image with the production container profile (caps,
loop/dm devices, AppArmor `neev-agent`, read-only root) and the supervisor env,
pointing every store/workspace path at DISPOSABLE staging directories:

    NEEV_ACP_SANDBOX_ACCEPTANCE=1 python3 neev_acp_sandbox_acceptance.py \\
        [--provider cursor --provider gemini --provider opencode] [--json out.json]

Exit 0 = every check held; 1 = a check failed; 2 = a prerequisite is unmet."""

import argparse
import json
import os
import platform
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path
from urllib.parse import urlsplit

OPERATIONS = "/opt/neev/scripts/operations"
sys.path.insert(0, OPERATIONS)
sys.path.insert(1, os.path.dirname(os.path.abspath(__file__)))

SETPRIV = "/usr/bin/setpriv"
WEB_UID = 65532
# A disposable QA org and two users no real account can have (ids far above any
# issued one); their volumes are revoked at the end.
QA_ORG = 990_000_001
QA_USER_A = 990_000_101
QA_USER_B = 990_000_102
PROVIDERS = ("cursor", "gemini", "opencode")
# Where each provider's own sign-in page lives (host only).
CHALLENGE_HOSTS = {
    "cursor": ("cursor.com",),
    "gemini": ("accounts.google.com",),
    "opencode": ("auth.openai.com",),
}
PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\rIHDR" + b"\x00" * 13
LOGIN_TIMEOUT = 90.0
RUN_TIMEOUT = 150


class Result:
    def __init__(self):
        self.checks = []

    def check(self, provider, name, ok, detail="", evidence=None):
        self.checks.append({"provider": provider, "name": name, "ok": bool(ok),
                            "detail": detail, "evidence": evidence or {}})
        print(("PASS" if ok else "FAIL"), provider, name, ("- " + detail) if detail else "",
              flush=True)

    @property
    def failures(self):
        return [c for c in self.checks if not c["ok"]]


def as_web(argv):
    """Exactly how the server starts the helper: uid 65532, bounding caps kept,
    no-new-privs off, so the helper's file capabilities are the only privilege."""
    return [SETPRIV, "--reuid=%d" % WEB_UID, "--regid=%d" % WEB_UID, "--clear-groups", *argv]


def lane_processes(uid):
    """PIDs whose real uid is `uid` (from /proc: the image ships no `ps`)."""
    found = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open("/proc/%s/status" % entry) as handle:
                for line in handle:
                    if line.startswith("Uid:"):
                        if int(line.split()[1]) == uid:
                            found.append(entry)
                        break
        except (OSError, ValueError, IndexError):
            continue
    return found


def leftovers(config):
    return sorted(name for name in os.listdir(config.workspace_root) if name.startswith("run-"))


def settled(result, provider, step, config, slot_proxy):
    """The invariants every step must leave behind."""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and (lane_processes(config.lane_uid) or leftovers(config)):
        time.sleep(0.25)
    procs, dirs = lane_processes(config.lane_uid), leftovers(config)
    quarantined = slot_proxy.is_quarantined(config.slot)
    result.check(provider, step + ": teardown clean",
                 not procs and not dirs and not quarantined,
                 "" if not (procs or dirs or quarantined) else
                 "lane processes=%d run dirs=%d quarantined=%s" % (len(procs), len(dirs), quarantined))


def check_login(result, provider, driver_for, scope, handle, config, slot_proxy, dto):
    started = time.monotonic()
    driver = driver_for(dto.Provider(provider), scope, handle)
    state = None
    try:
        driver.start()
        while time.monotonic() - started < LOGIN_TIMEOUT:
            state = driver.poll()
            if state.phase in ("awaiting_user", "failed", "prepared"):
                break
            time.sleep(0.25)
    finally:
        # Cancelled BEFORE anyone could open the page: nothing is consented to.
        driver.cancel()
    seconds = round(time.monotonic() - started, 1)
    phase = state.phase if state else None
    host = None
    if state is not None and state.challenge is not None:
        try:
            host = urlsplit(state.challenge.verification_url).hostname
        except ValueError:
            host = None
    expected = CHALLENGE_HOSTS[provider]
    evidence = {"phase": phase, "challenge_host": host, "seconds": seconds,
                "user_code_shown": bool(state and state.challenge and state.challenge.user_code),
                "failure": state.failure if state else None}
    result.check(provider, "login: official challenge through the real login sandbox",
                 phase == "awaiting_user" and host is not None
                 and any(host == h or host.endswith("." + h) for h in expected),
                 "" if phase == "awaiting_user" else "phase=%s failure=%s" % (
                     phase, evidence["failure"]), evidence)
    settled(result, provider, "login", config, slot_proxy)


def check_probes(result, provider, probe_for, scope, handle, config, volumes, main_mod,
                 drivers, launch, dto, slot_proxy):
    verdict = probe_for(dto.Provider(provider), scope, handle).probe()
    result.check(provider, "probe: production probe says disconnected", verdict == "disconnected",
                 "" if verdict == "disconnected" else repr(verdict))
    # The adapter itself, inside the sandbox, on an empty volume.
    channel = main_mod.LoginChannel(config, volumes, dto.Provider(provider), scope, handle,
                                    tuple(launch.acp_probe_argv(provider)))
    started = time.monotonic()
    try:
        answer = drivers.AcpAccountProbe(channel, "qa").probe()
    except Exception as error:  # an undecided probe must raise, never guess
        answer = "raised:%s" % type(error).__name__
    result.check(provider, "probe: in-sandbox ACP probe is honest (no fake success)",
                 answer == "disconnected", "" if answer == "disconnected" else repr(answer),
                 {"seconds": round(time.monotonic() - started, 1)})
    settled(result, provider, "probe", config, slot_proxy)


def fresh_workdir(config):
    path = os.path.join(config.workspace_root, "run-acp-qa-%s" % uuid.uuid4().hex[:12])
    os.mkdir(path)
    return path


def stage_image(workdir):
    """As `agents::execution::stage_attachments` does: a keyed leaf under
    neev-attachments, 0755 dirs / 0644 file. No file extension on purpose."""
    root = os.path.join(workdir, "neev-attachments")
    os.mkdir(root)
    leaf = os.path.join(root, uuid.uuid4().hex)
    os.mkdir(leaf)
    path = os.path.join(leaf, "11111111-1111-4111-8111-111111111111--chart")
    with open(path, "wb") as handle:
        handle.write(PNG)
    for directory in (root, leaf):
        os.chmod(directory, 0o755)
    os.chmod(path, 0o644)
    return path


def run_argv(config, provider, tenant, handle, workdir, binary, options, prompt):
    return as_web([
        os.environ["NEEV_AGENT_CAP_HELPER"], "--tenant", tenant, "--slot", config.slot, "--workdir", workdir,
        "--uid", str(config.lane_uid), "--gid", str(config.lane_gid),
        "--provider", provider, "--credential-handle", handle, "--codex-stream", "exec",
        *[part for ip in config.egress_ipv4 for part in ("--provider-ipv4", ip)],
        "--", binary, "--neev-acp", *options, "--", prompt,
    ])


def parse_lines(raw):
    lines = []
    for line in raw.decode("utf-8", "replace").splitlines():
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if isinstance(record, dict):
            lines.append(record)
    return lines


def check_run(result, provider, tenant, handle, config, launch, slot_proxy):
    binary = launch.ACP_AGENT_PINS[provider][0]
    workdir = fresh_workdir(config)
    image = stage_image(workdir)
    tag = uuid.uuid4().hex[:8]
    local = json.dumps({"name": "qa-local-%s" % tag, "command": "/bin/true", "args": []})
    remote = json.dumps({"name": "qa-http-%s" % tag, "type": "http",
                         "url": "https://mcp.example.invalid/mcp"})
    argv = run_argv(config, provider, tenant, handle, workdir, binary,
                    ["--image", image, "--mcp", local, "--mcp", remote],
                    "Reply with exactly: NEEV-QA")
    started = time.monotonic()
    proc = subprocess.run(argv, capture_output=True, timeout=RUN_TIMEOUT)
    seconds = round(time.monotonic() - started, 1)
    lines = parse_lines(proc.stdout)
    notices = [l.get("name") for l in lines if l.get("type") == "neev.notice"]
    last = lines[-1] if lines else {}
    # The provider's own complaint, bounded; the QA volume holds no credential,
    # and egress diagnostics are reduced to their category and host.
    tail = [line for line in proc.stderr.decode("utf-8", "replace").splitlines()
            if line.strip() and "NEEV-EGRESS-DIAG" not in line][-12:]
    evidence = {"rc": proc.returncode, "seconds": seconds, "last": last, "notices": notices,
                "stderr_tail": [line[:200] for line in tail],
                "kinds": [l.get("type") for l in lines][:20],
                "workdir_removed": not os.path.exists(workdir)}
    if provider == "gemini" and not notices:
        # Without an account Gemini CLI prints its sign-in prompt BEFORE it
        # answers initialize, so no tool is negotiated at all: the turn ends at
        # sign-in (checked next) and the tool question never arises.
        result.check(provider, "run: tools never negotiated before sign-in (no notice due)",
                     last.get("reason") == "provider sign-in required", "", evidence)
    else:
        result.check(provider, "run: unreachable HTTP library tool skipped by name",
                     notices == ["qa-http-%s" % tag], "" if notices else "no notice", evidence)
    result.check(provider, "run: honest sign-in failure through caplaunch+volume+run proxy",
                 last == {"type": "turn.failed", "reason": "provider sign-in required"},
                 "" if last.get("reason") == "provider sign-in required" else
                 "last=%r stderr_tail_bytes=%d" % (last, len(proc.stderr)), evidence)
    result.check(provider, "run: workdir removed by the launcher", evidence["workdir_removed"])
    settled(result, provider, "run", config, slot_proxy)


def check_cancel(result, provider, tenant, handle, config, launch, slot_proxy, after=1.5):
    binary = launch.ACP_AGENT_PINS[provider][0]
    workdir = fresh_workdir(config)
    argv = run_argv(config, provider, tenant, handle, workdir, binary, [],
                    "Count slowly from 1 to 500.")
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    time.sleep(after)
    sent = time.monotonic()
    # From the web uid, as the server cancels.
    # The image ships no kill(1); the signal is sent by a web-uid process.
    signalled = subprocess.run(as_web([sys.executable, "-I", "-c",
                                       "import os,signal,sys;os.kill(int(sys.argv[1]),signal.SIGTERM)",
                                       str(proc.pid)]), check=False).returncode == 0
    try:
        out, _ = proc.communicate(timeout=30)
        exited = round(time.monotonic() - sent, 1)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, _ = proc.communicate()
        exited = None
    lines = parse_lines(out)
    evidence = {"rc": proc.returncode, "signalled": signalled,
                "exit_seconds_after_sigterm": exited,
                "last": lines[-1] if lines else None,
                "workdir_removed": not os.path.exists(workdir)}
    result.check(provider, "cancel: SIGTERM from the web uid ends the whole tree",
                 signalled and exited is not None and exited <= 15
                 and evidence["workdir_removed"],
                 "" if exited is not None else "did not exit in 30s", evidence)
    settled(result, provider, "cancel", config, slot_proxy)


def check_isolation(result, provider, handle_a, config, launch, slot_proxy):
    tenant_b = "org:%d/user:%d" % (QA_ORG, QA_USER_B)
    workdir = fresh_workdir(config)
    argv = run_argv(config, provider, tenant_b, handle_a, workdir,
                    launch.ACP_AGENT_PINS[provider][0], [], "x")
    proc = subprocess.run(argv, capture_output=True, timeout=60)
    lines = parse_lines(proc.stdout)
    result.check(provider, "isolation: tenant B cannot open tenant A's volume",
                 proc.returncode != 0 and not any(l.get("type") == "turn.completed" for l in lines),
                 "rc=%d" % proc.returncode, {"rc": proc.returncode})
    if os.path.exists(workdir):
        import neev_agent_launch as launch_mod
        launch_mod._cleanup_run_workdir(workdir)
    settled(result, provider, "isolation", config, slot_proxy)


def staging_refusal(environ):
    """Pure-ish: why this environment is NOT a disposable staging store, or None.

    The QA volumes are revoked at the end, but a mistaken run must not touch a
    production store at all: any existing credential entry (a store holds only
    `.lock` plus one directory per volume) or a supervisor connection registry
    means real users, and the run refuses before creating anything."""
    store = environ.get("NEEV_SUPERVISOR_STORE_ROOT", "")
    registry = environ.get("NEEV_SUPERVISOR_REGISTRY", "")
    if not store or not os.path.isabs(store) or not registry or not os.path.isabs(registry):
        return "store/registry paths not configured"
    try:
        entries = [name for name in os.listdir(store) if name != ".lock"]
    except FileNotFoundError:
        entries = []
    if entries:
        return "credential store is not empty: not a disposable staging store"
    if os.path.lexists(registry):
        return "a supervisor registry exists: not a disposable staging store"
    return None


def run(args):
    import neev_agent_launch as launch
    import neev_agent_login_drivers as drivers
    import neev_agent_network_bootstrap as bootstrap
    import neev_agent_supervisor_main as main_mod
    import neev_credential_store as store_module
    import neev_credential_volumes as volumes_module
    import neev_slot_proxy as slot_proxy
    import neev_supervisor_dto as dto

    config = main_mod.config_from_env()
    os.environ["NEEV_AGENT_WORKSPACE_ROOT"] = config.workspace_root
    result = Result()
    # The supervisor's own start-up, minus the control socket.
    main_mod.purge_abandoned_runs(config.workspace_root)
    slot_proxy.clear_quarantine(config.slot)
    bootstrap.apply(bootstrap.plan(config.uplink), bootstrap.CommandRunner())
    store = store_module.CredentialStore(Path(config.store_root), Path(config.master_key_path))
    store.initialize()
    volumes = volumes_module.CredentialVolumes(store, volumes_module.FixedToolRunner())
    driver_for, probe_for = main_mod.make_factories(config, volumes)
    tenant_a = "org:%d/user:%d" % (QA_ORG, QA_USER_A)

    for provider in args.providers:
        scope_a = store_module.Scope(QA_ORG, QA_USER_A, provider)
        handle = volumes.create(scope_a)
        try:
            check_login(result, provider, driver_for, scope_a, handle, config, slot_proxy, dto)
            check_probes(result, provider, probe_for, scope_a, handle, config, volumes,
                         main_mod, drivers, launch, dto, slot_proxy)
            check_run(result, provider, tenant_a, handle, config, launch, slot_proxy)
            check_cancel(result, provider, tenant_a, handle, config, launch, slot_proxy)
            check_isolation(result, provider, handle, config, launch, slot_proxy)
        finally:
            try:
                volumes.revoke(scope_a, handle)
            except Exception as error:
                result.check(provider, "cleanup: QA volume revoked", False, type(error).__name__)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(prog="neev_acp_sandbox_acceptance")
    parser.add_argument("--provider", action="append", choices=PROVIDERS, dest="providers")
    parser.add_argument("--json", default=None)
    args = parser.parse_args(argv)
    args.providers = args.providers or list(PROVIDERS)
    unmet = None
    if os.environ.get("NEEV_ACP_SANDBOX_ACCEPTANCE") != "1":
        unmet = "set NEEV_ACP_SANDBOX_ACCEPTANCE=1"
    elif platform.system() != "Linux":
        unmet = "Linux required"
    elif os.geteuid() != 0:
        unmet = "must run as root"
    elif not os.path.exists(os.environ.get("NEEV_AGENT_CAP_HELPER", "/nonexistent")):
        unmet = "caphelper missing"
    else:
        unmet = staging_refusal(os.environ)
    if unmet:
        print("UNMET:", unmet)
        return 2
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    result = run(args)
    ok = not result.failures
    if args.json:
        with open(args.json, "w") as handle:
            json.dump({"ok": ok, "checks": result.checks,
                       "not_proven": [
                           "a completed sign-in, model turn, stream or cancel with a real "
                           "Cursor, Google or ChatGPT account (needs a human)",
                           "that the run allowlists are sufficient for a signed-in turn",
                       ]}, handle, indent=1)
    print("RESULT:", "ACCEPTED" if ok else "FAILED %d" % len(result.failures))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
