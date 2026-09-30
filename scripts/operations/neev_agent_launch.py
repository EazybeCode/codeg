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
import hashlib
import json
import os
import re
import signal
import sys
import threading
import uuid

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
    # Chromium needs more than 64 threads even without the provider process.
    # Keep a finite shared budget for the provider and one local HTML preview.
    parser.add_argument("--processes", type=int, default=256)
    # Enable mid-run steering: relay JSON {requestId, text} from launcher stdin
    # to the adapter, which sends turn/steer to the provider. Only effective
    # with --codex-stream=app-server; exec mode ignores this.
    parser.add_argument(
        "--enable-steering", action="store_true", default=False, dest="enable_steering"
    )
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
# reserved `PROT_NONE` and never committed. Claude's local browser helper also
# needs V8 reservation space: 2 GiB fails before preview, 128 GiB was verified.
# An operator's explicit `--address-space-bytes` still wins over both.
_ADDRESS_SPACE_BYTES = 128 * 1024**3
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


# The in-sandbox streaming adapters, beside this launcher in the same root-owned
# 0555 directory. `-I` matches the ns prelude: no user site-packages, no
# inherited PYTHONPATH.
ADAPTER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "neev_codex_stream_adapter.py")
CLAUDE_ADAPTER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "neev_claude_stream_adapter.py")
ACP_ADAPTER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "neev_acp_stream_adapter.py")

# The ONLY executables an ACP run may start, per provider: the image-pinned paths
# (see the pinned-image requirements in the providers report). Fixed here, never
# taken from the runtime's argv or the environment — the runtime's binary must be
# EXACTLY one of these, and the adapter re-checks the realpath against them.
ACP_AGENT_PINS = {
    "cursor": ("/usr/local/bin/agent",),
    "gemini": ("/usr/local/bin/gemini",),
    "opencode": ("/usr/local/bin/opencode",),
}
# The canonical ACP turn the Rust runtime builds: `<binary> --neev-acp -- <prompt>`.
# `--neev-acp` is a marker for THIS launcher, never passed to a provider: an older
# launcher (or a misrouted argv) refuses the provider at argparse instead of
# running the binary with a prompt it would misread.
ACP_MARKER = "--neev-acp"


# The `-c key=value` overrides `provider_argv` builds for `codex exec`, and what
# each becomes for the app-server. Effort and tier have parameters of their own
# in the pinned protocol, so they are sent as parameters and the thread reports
# back what it took; fast mode has none and stays a config override. An override
# outside this table is refused rather than dropped — it is a choice somebody
# made, and a turn that ignores it looks like a turn that honoured it.
_EFFORT_OVERRIDE = re.compile(r'\Amodel_reasoning_effort="([A-Za-z-]{1,32})"\Z')
_TIER_OVERRIDE = re.compile(r'\Aservice_tier="([A-Za-z-]{1,32})"\Z')
_FAST_MODE_OVERRIDE = re.compile(r"\Afeatures\.fast_mode=(?:true|false)\Z")
# The response-style policy on the `exec` transport: Codex's documented
# developer-instruction config key, carried as a JSON string so newlines, quotes
# and backslashes survive the override verbatim. DOTALL because the value is
# multi-line by construction.
_DEVELOPER_INSTRUCTIONS_OVERRIDE = re.compile(
    r'\Adeveloper_instructions=(".*")\Z', re.DOTALL
)


# Only the configuration fields emitted by the shared-library runtime. This
# extends the existing exact override allowlist, not sandbox/network policy.
_MCP_OVERRIDE = re.compile(
    r"\Amcp_servers\.[A-Za-z0-9_-]{1,160}\.(command|args|url)=(.+)\Z", re.DOTALL
)


def _shared_mcp_argument(value):
    match = _MCP_OVERRIDE.fullmatch(value)
    if not match or len(value) > 65536:
        return None
    field, encoded = match.groups()
    try:
        decoded = json.loads(encoded)
    except ValueError as error:
        raise sandbox.SandboxError("invalid") from error
    if field == "args":
        valid = (isinstance(decoded, list) and len(decoded) <= 50
                 and all(isinstance(arg, str) and len(arg) <= 1000
                         and "\x00" not in arg for arg in decoded))
    else:
        valid = isinstance(decoded, str) and bool(decoded.strip()) and "\x00" not in decoded
        if valid and field == "command":
            valid = len(decoded) <= 500
        elif valid:
            from urllib.parse import urlsplit
            try:
                url = urlsplit(decoded)
                valid = (url.scheme == "https" and bool(url.hostname)
                         and not url.username and not url.password
                         and not url.query and not url.fragment)
            except ValueError:
                valid = False
    if not valid:
        raise sandbox.SandboxError("invalid")
    return ["--config", value]


def _config_argument(value):
    """Pure: one `-c` override -> the adapter's arguments for it."""
    shared = _shared_mcp_argument(value)
    if shared is not None:
        return shared
    effort = _EFFORT_OVERRIDE.match(value)
    if effort:
        return ["--effort", effort.group(1)]
    tier = _TIER_OVERRIDE.match(value)
    if tier:
        return ["--service-tier", tier.group(1)]
    if _FAST_MODE_OVERRIDE.match(value):
        return ["--config", value]
    instructions = _DEVELOPER_INSTRUCTIONS_OVERRIDE.match(value)
    if instructions:
        # The app-server has a parameter for this (`thread/start`
        # `developerInstructions`), so the override becomes one rather than
        # being passed through as config. Decoded here and re-checked: an
        # override that is not a JSON string is refused, never guessed at.
        try:
            text = json.loads(instructions.group(1))
        except ValueError as error:
            raise sandbox.SandboxError("invalid") from error
        if not isinstance(text, str) or not text:
            raise sandbox.SandboxError("invalid")
        return ["--developer-instructions", text]
    raise sandbox.SandboxError("invalid")


def app_server_argv(provider, provider_argv, adapter=ADAPTER, python=sandbox.PYTHON,
                    enable_steering=False):
    """Pure: rewrite a `codex exec --json` launch into the streaming adapter's.

    `exec` emits nothing until an item is finished, so a reply appears only once
    the turn is over. The adapter drives `codex app-server`, which streams the
    message as it is written, and it runs HERE — as the sandboxed provider
    process — so it owns the app-server's stdin inside the isolation boundary
    instead of asking for a pipe through it.

    When `enable_steering` is True, the adapter reads JSON {requestId, text}
    from stdin and relays them as `turn/steer` requests to add follow-up input
    to the active turn. The launcher must then pass stdin to the sandbox rather
    than DEVNULL.

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
    if enable_steering:
        mapped.append("--enable-steering")
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


def claude_stream_argv(provider, provider_argv, adapter=CLAUDE_ADAPTER, python=sandbox.PYTHON,
                       enable_steering=False):
    """Pure: rewrite a `claude -p` launch into the streaming adapter's.

    Unlike Codex (app-server JSON-RPC), Claude uses stream-json with queued user
    messages and interrupt-and-continue for steering. The adapter handles:
    - `--input-format stream-json` for follow-up messages
    - `--replay-user-messages` for delivery acknowledgment
    - Interrupt control protocol for mid-run steering

    Only the canonical shape `claude -p <prompt> ...` is accepted.
    All launcher flags are preserved and passed to the adapter.
    """
    argv = list(provider_argv)
    if provider != "claude":
        return argv
    # Canonical shape: claude -p <prompt> [options...]
    if len(argv) < 3 or argv[1] != "-p":
        return argv
    binary, prompt, options = argv[0], argv[2], argv[3:]
    mapped = ["--claude", binary]
    if enable_steering:
        mapped.append("--enable-steering")
    # Walk options to extract model, effort, system prompt, settings, and flags
    walked = _walk_options(options, _CLAUDE_VALUE_OPTIONS, _CLAUDE_FLAG_OPTIONS)
    if walked is None:
        # Unknown option, fall back to direct execution
        return argv
    for option, at in walked:
        if at is None:
            # Flag option - preserve it (--verbose, --include-partial-messages, etc.)
            if option == "--verbose":
                mapped.append("--verbose")
            elif option == "--include-partial-messages":
                mapped.append("--include-partial-messages")
            # --output-format and --json are replaced by adapter's stream-json
            continue
        value = options[at]
        if option == "--model":
            mapped += ["--model", value]
        elif option == "--effort":
            mapped += ["--effort", value]
        elif option == _CLAUDE_SYSTEM_PROMPT:
            mapped += ["--append-system-prompt", value]
        elif option == "--settings":
            mapped += ["--settings", value]
        # --output-format is replaced by adapter's stream-json
    return [python, "-I", adapter] + mapped + ["--", prompt]


def acp_stream_argv(provider, provider_argv, style="", adapter=ACP_ADAPTER,
                    python=sandbox.PYTHON, pins=None):
    """Pure: rewrite the canonical ACP turn into the in-sandbox adapter's argv.

    Only `[<pinned binary>, "--neev-acp", "--", <prompt>]` is accepted; anything
    else raises (a provider binary must never be run with an argv it would parse
    on its own). The adapter is given the pinned paths as its allowlist, the
    default fail-closed permission policy (no product grant exists for ACP runs),
    and never `--enable-steering` (ACP v1 has no mid-turn input). The response
    style travels as `--context`, because ACP has no system-prompt parameter.
    """
    pins = ACP_AGENT_PINS if pins is None else pins
    allowed = pins.get(provider)
    argv = list(provider_argv)
    if not allowed:
        raise sandbox.SandboxError("invalid")
    if len(argv) != 4 or argv[1] != ACP_MARKER or argv[2] != "--":
        raise sandbox.SandboxError("invalid")
    binary, prompt = argv[0], argv[3]
    if binary not in allowed or not prompt:
        raise sandbox.SandboxError("invalid")
    mapped = [python, "-I", adapter, "--provider", provider, "--agent", binary]
    for path in allowed:
        mapped += ["--allowed-agent", path]
    if style:
        mapped += ["--context", style]
    return mapped + ["--", prompt]


def acp_probe_argv(provider, adapter=ACP_ADAPTER, python=sandbox.PYTHON, pins=None):
    """Pure: the fixed account-probe argv for one ACP provider (supervisor lane)."""
    pins = ACP_AGENT_PINS if pins is None else pins
    allowed = pins.get(provider)
    if not allowed:
        raise sandbox.SandboxError("invalid")
    argv = [python, "-I", adapter, "--provider", provider, "--agent", allowed[0]]
    for path in allowed:
        argv += ["--allowed-agent", path]
    return argv + ["--probe"]


# The shared response-style policy, beside this launcher in the same root-owned
# 0555 directory and nowhere else — never from the run workdir, which the model
# itself can write. It is our text, not the reader's and not the workspace's.
RESPONSE_STYLE = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "neev_response_style.md"
)
# Generous for a ~300-word policy and far too small to be a channel for anything
# else. A file above this is refused rather than truncated: half a policy is a
# instruction set nobody wrote.
_MAX_RESPONSE_STYLE_BYTES = 8 * 1024


def load_response_style(path=RESPONSE_STYLE, limit=_MAX_RESPONSE_STYLE_BYTES):
    """The packaged policy text, or "" when there is none to apply.

    Bounded, UTF-8, read from the fixed package path. Missing, oversized or
    undecodable all mean the same thing and are handled the same way: the turn
    runs exactly as it did before this existed. A style that cannot be read is
    a formatting default the reply loses, not a reason to refuse work somebody
    is paying for.
    """
    try:
        with open(path, "rb") as handle:
            raw = handle.read(limit + 1)
    except OSError:
        return ""
    if len(raw) > limit:
        return ""
    try:
        return raw.decode("utf-8").strip()
    except UnicodeDecodeError:
        return ""


# Claude's paid-turn options as `provider_argv` spells them, split by whether
# they take a value. The table exists so the argv is WALKED rather than
# searched: a prompt is the user's words and may be exactly
# `--append-system-prompt`, so `in argv` would find the prompt and append our
# policy to it.
_CLAUDE_SYSTEM_PROMPT = "--append-system-prompt"
_CLAUDE_VALUE_OPTIONS = frozenset(
    {_CLAUDE_SYSTEM_PROMPT, "--effort", "--model", "--output-format", "--settings"}
)
_CLAUDE_FLAG_OPTIONS = frozenset({"--include-partial-messages", "--verbose"})
# Codex `exec` options, same split, same reason (its prompt is index 3).
_CODEX_VALUE_OPTIONS = frozenset({"-c", "-m", "--image"})
_CODEX_FLAG_OPTIONS = frozenset({"--skip-git-repo-check", _EXTERNALLY_SANDBOXED})


def _walk_options(options, value_options, flag_options):
    """Pure: the index of each option's VALUE, or None if the shape is unknown.

    Returns a list of `(option, value_index_or_None)` in order. An option this
    table does not know ends the walk: we then leave the argv alone instead of
    guessing which of its arguments is a value.
    """
    walked = []
    index = 0
    while index < len(options):
        option = options[index]
        if option in flag_options:
            walked.append((option, None))
            index += 1
            continue
        if option in value_options:
            if index + 1 >= len(options):
                return None
            walked.append((option, index + 1))
            index += 2
            continue
        return None
    return walked


def _already_carries(instructions, style):
    """Pure: is `style` there as OUR suffix — not merely somewhere in the text?

    `style in instructions` would be a hole: the briefing is workspace data a
    user can write, so a copy of the policy pasted into a workspace description
    would read as "already applied" and the turn would run with no trusted
    instruction at all. Only text we appended ourselves (or an argv we already
    rewrote) counts.
    """
    return instructions == style or instructions.endswith("\n\n" + style)


def response_style_argv(provider, provider_argv, style):
    """Pure: carry the shared response-style policy into ONE canonical paid turn.

    Claude takes real system instructions, so the policy is APPENDED to the
    `--append-system-prompt` value the runtime already built — the workspace
    briefing stays first and intact — or the flag is added when there is none.
    Codex `exec` has no such switch; it gets the documented
    `developer_instructions` config key as a JSON string, which
    `app_server_argv` then maps to the app-server's `thread/start`
    `developerInstructions` parameter. `baseInstructions` is never set: the
    provider's own instructions stay, ours are added to them.

    Only the canonical shapes are touched — `claude -p <prompt> …` and
    `codex exec --json <prompt> …` — and the prompt (index 2 / index 3) is
    never read, parsed or matched against anything. Metadata probes and login
    flows are neither of those shapes and pass through unchanged. Anything
    else, including an option this launcher does not know, is handed back as it
    arrived: a missing style costs formatting, a mangled argv costs the turn.
    """
    argv = list(provider_argv)
    if not style:
        return argv
    if provider == "codex":
        if len(argv) < 4 or argv[1] != "exec" or argv[2] != "--json":
            return argv
        options = argv[4:]
        walked = _walk_options(options, _CODEX_VALUE_OPTIONS, _CODEX_FLAG_OPTIONS)
        if walked is None:
            return argv
        for option, at in walked:
            if option != "-c":
                continue
            existing = _DEVELOPER_INSTRUCTIONS_OVERRIDE.match(options[at])
            if existing is None:
                continue
            # An override that is already there is somebody's instruction and
            # is KEPT; the policy is appended to it once. Dropping ours would
            # silently exempt the turn, replacing theirs would silently lose
            # it.
            try:
                text = json.loads(existing.group(1))
            except ValueError:
                return argv
            if not isinstance(text, str):
                return argv
            if _already_carries(text, style):
                return argv
            merged = list(argv)
            merged[4 + at] = "developer_instructions=" + json.dumps(
                f"{text}\n\n{style}" if text else style
            )
            return merged
        return argv + ["-c", f"developer_instructions={json.dumps(style)}"]
    if len(argv) < 3 or argv[1] != "-p":
        return argv
    options = argv[3:]
    walked = _walk_options(options, _CLAUDE_VALUE_OPTIONS, _CLAUDE_FLAG_OPTIONS)
    if walked is None:
        return argv
    at = next(
        (value for option, value in walked if option == _CLAUDE_SYSTEM_PROMPT), None
    )
    if at is None:
        return argv + [_CLAUDE_SYSTEM_PROMPT, style]
    briefing = options[at]
    if _already_carries(briefing, style):
        return argv
    merged = list(options)
    merged[at] = f"{briefing}\n\n{style}" if briefing else style
    return argv[:3] + merged


def build_spec(args, style=None):
    """Pure: turn parsed args into a validated SandboxSpec (raises SandboxError).

    `style` defaults to the packaged response-style policy; pass `""` for a run
    with none.
    """
    provider_argv = list(args.provider_argv)
    if provider_argv and provider_argv[0] == "--":
        provider_argv = provider_argv[1:]
    if not provider_argv:
        raise sandbox.SandboxError("invalid")
    style = load_response_style() if style is None else style
    enable_steering = getattr(args, "enable_steering", False)
    if args.provider not in credentials.ACP_PROVIDERS:
        provider_argv = response_style_argv(args.provider, provider_argv, style)
    if args.provider in credentials.ACP_PROVIDERS:
        # ACP runs have no steering; a runtime asking for it is misconfigured.
        if enable_steering:
            raise sandbox.SandboxError("invalid")
        provider_argv = acp_stream_argv(args.provider, provider_argv, style)
    elif args.provider == "claude" and enable_steering:
        # Claude with steering uses stream-json + interrupt-and-continue
        provider_argv = claude_stream_argv(args.provider, provider_argv,
                                           enable_steering=True)
    elif args.codex_stream == "app-server":
        # Codex with app-server mode (optionally with steering via turn/steer)
        provider_argv = app_server_argv(args.provider, provider_argv,
                                        enable_steering=enable_steering)
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
            # Determine stdin mode: metadata probe uses bounded relay,
            # steering uses unbounded relay, otherwise DEVNULL.
            is_probe = is_metadata_probe(args.provider, spec)
            enable_steering = (
                getattr(args, "enable_steering", False)
                and args.codex_stream == "app-server"
                and not is_probe
            )
            rc = _run_child(
                spec,
                child_env(delivery, proxy_env),
                bind_source=delivery.mount,
                relay_stdin=is_probe,
                relay_steering=enable_steering,
            )
    except credentials.CredentialError as error:
        # Honest, non-sensitive reason ("busy" = another run for this user+provider
        # holds the volume). Rust surfaces waiting/busy; nothing runs unauthorized.
        print(
            f"neev_agent_launch: credential delivery refused: {error.category}",
            file=sys.stderr,
        )
        return 5

    # Export generated outputs to the server-readable spool BEFORE cleanup.
    # The runtime derives the spool path from its own workdir, not from stdout.
    run_id = os.path.basename(args.workdir)
    export_error = _export_generated_outputs(args.workdir, run_id)
    if export_error:
        # Export was attempted but failed - this is an error, not silent skip
        print(f"neev_agent_launch: output export failed: {export_error}", file=sys.stderr)
        # Continue to cleanup - don't leave orphan spool dirs

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
    return 8 if export_error else rc


_MAX_RUN_ENTRIES = 8192

# --- Generated output export ---
# Agent saves deliverables to outputs/ (canonical) or neev-output/.
# After run completes but BEFORE cleanup, export to server-readable spool.
# Server validates content types; exporter treats files as opaque bytes.

_OUTPUT_DIRS = ("outputs", "neev-output")
_SPOOL_ROOT = "spool"
_MAX_OUTPUT_FILES = 100
# Per-run total matches org storage ceiling (256 MiB) - server enforces quota
_MAX_OUTPUT_TOTAL_BYTES = 256 * 1024 * 1024
_SERVER_UID = 65532
_SERVER_GID = 65532
# Reject names that look like auth/config files
_REJECTED_NAMES = frozenset((
    ".env", ".envrc", ".npmrc", ".pypirc", ".netrc", ".gitconfig",
    "credentials", "credentials.json", "config.json", "secrets.json",
))


def _safe_output_name(name):
    """Validate output filename. Returns None if invalid or sensitive."""
    if not name or not isinstance(name, str):
        return None
    name = name.strip()
    if not name or len(name) > 200:
        return None
    if name in (".", "..") or name.startswith("."):
        return None
    if any(c in name for c in ("/", "\\", "\x00", ":")):
        return None
    if any(ord(c) < 32 for c in name):
        return None
    if name.lower() in _REJECTED_NAMES:
        return None
    return name


def _writeall(fd, data):
    """Write all bytes to fd, handling partial writes."""
    total = 0
    mv = memoryview(data)
    while total < len(data):
        written = os.write(fd, mv[total:])
        if written == 0:
            raise OSError("write returned 0")
        total += written
    return total


def _export_generated_outputs(workdir, run_id):
    """Copy declared outputs via anchored descriptors; publish a manifest last."""
    import stat
    from contextlib import ExitStack

    root = os.environ.get("NEEV_AGENT_WORKSPACE_ROOT")
    if not root:
        return "workspace_root_unavailable"
    if (_RUN_DIR.fullmatch(run_id) is None or os.path.basename(workdir) != run_id
            or os.path.realpath(os.path.dirname(workdir)) != os.path.realpath(root)):
        return "invalid_run_directory"
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    made_run = False
    committed = False
    spool_fd = run_fd = files_fd = None
    created = []
    with ExitStack() as stack:
        def keep(fd):
            stack.callback(os.close, fd)
            return fd
        try:
            root_fd = keep(os.open(root, directory_flags))
            work_fd = keep(os.open(run_id, directory_flags, dir_fd=root_fd))
            sources = []
            names = set()
            declared_bytes = 0
            for dirname in _OUTPUT_DIRS:
                try:
                    output_fd = keep(os.open(dirname, directory_flags, dir_fd=work_fd))
                except FileNotFoundError:
                    continue
                for name in sorted(os.listdir(output_fd)):
                    safe_name = _safe_output_name(name)
                    if not safe_name or safe_name in names:
                        raise ValueError("invalid_or_duplicate_output_name")
                    if len(sources) >= _MAX_OUTPUT_FILES:
                        raise ValueError("output_count_exceeded")
                    fd = keep(os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=output_fd))
                    info = os.fstat(fd)
                    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                        raise ValueError("output_not_single_regular_file")
                    declared_bytes += info.st_size
                    if info.st_size <= 0 or declared_bytes > _MAX_OUTPUT_TOTAL_BYTES:
                        raise ValueError("output_storage_bound_exceeded")
                    sources.append((safe_name, fd, info.st_size))
                    names.add(safe_name)
            if not sources:
                return None
            try:
                os.mkdir(_SPOOL_ROOT, 0o700, dir_fd=root_fd)
            except FileExistsError:
                pass
            spool_fd = keep(os.open(_SPOOL_ROOT, directory_flags, dir_fd=root_fd))
            info = os.fstat(spool_fd)
            if info.st_uid not in (0, _SERVER_UID) or info.st_mode & 0o022:
                raise ValueError("unsafe_spool_owner_or_mode")
            # The privileged helper has CAP_CHOWN but deliberately no CAP_FOWNER.
            # chmod while self-owned, then hand the protected spool to the server.
            os.fchown(spool_fd, os.geteuid(), os.getegid())
            os.fchmod(spool_fd, 0o700)
            os.fchown(spool_fd, _SERVER_UID, _SERVER_GID)
            os.mkdir(run_id, 0o700, dir_fd=spool_fd)
            made_run = True
            run_fd = keep(os.open(run_id, directory_flags, dir_fd=spool_fd))
            os.mkdir("files", 0o700, dir_fd=run_fd)
            files_fd = keep(os.open("files", directory_flags, dir_fd=run_fd))
            manifest = {"run_id": run_id, "files": [], "errors": []}
            total_bytes = 0
            for name, source_fd, expected_bytes in sources:
                key = str(uuid.uuid4())
                leaf = f"{key}--{name}"
                dest_fd = keep(os.open(leaf, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                       0o600, dir_fd=files_fd))
                created.append(leaf)
                digest = hashlib.sha256()
                length = 0
                while True:
                    chunk = os.read(source_fd, 65536)
                    if not chunk:
                        break
                    length += len(chunk)
                    total_bytes += len(chunk)
                    if total_bytes > _MAX_OUTPUT_TOTAL_BYTES:
                        raise ValueError("output_storage_bound_exceeded")
                    _writeall(dest_fd, chunk)
                    digest.update(chunk)
                if length != expected_bytes:
                    raise ValueError("output_changed_during_export")
                os.fsync(dest_fd)
                os.fchown(dest_fd, _SERVER_UID, _SERVER_GID)
                manifest["files"].append({"key": key, "name": name,
                                          "byte_length": length, "sha256": digest.hexdigest()})
            manifest_fd = keep(os.open("manifest.tmp", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                      0o600, dir_fd=run_fd))
            _writeall(manifest_fd, json.dumps(manifest).encode("utf-8"))
            os.fsync(manifest_fd)
            os.fchown(manifest_fd, _SERVER_UID, _SERVER_GID)
            os.rename("manifest.tmp", "manifest.json", src_dir_fd=run_fd, dst_dir_fd=run_fd)
            os.fsync(files_fd)
            os.fsync(run_fd)
            os.fchown(files_fd, _SERVER_UID, _SERVER_GID)
            os.fchown(run_fd, _SERVER_UID, _SERVER_GID)
            committed = True
            return None
        except (OSError, ValueError) as error:
            return f"output_export_refused: {type(error).__name__}"
        finally:
            if made_run and not committed:
                # Only unlink entries created here, relative to still-open fds.
                for leaf in created:
                    try:
                        os.unlink(leaf, dir_fd=files_fd)
                    except OSError:
                        pass
                if run_fd is not None:
                    for name in ("manifest.tmp", "manifest.json"):
                        try:
                            os.unlink(name, dir_fd=run_fd)
                        except OSError:
                            pass
                    try:
                        os.rmdir("files", dir_fd=run_fd)
                    except OSError:
                        pass
                try:
                    os.rmdir(run_id, dir_fd=spool_fd)
                except OSError:
                    pass


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


class _FdReader:
    """The launcher's stdin, read with plain `os.read` on a PRIVATE duplicate fd.

    Never `sys.stdin` / `sys.stdin.buffer` from a relay thread. Those are the
    interpreter's own buffered objects: a daemon thread blocked in their
    `readline` holds the buffer lock, and when the main thread then exits (the
    runtime still holding our stdin open, as it does for the whole of a
    steerable run) interpreter shutdown tries to take that lock and ABORTS —
    `Fatal Python error: _enter_buffered_busy`, SIGABRT. The reply had already
    streamed and the turn had completed, but the launcher's exit status said
    failure, so every steerable Codex turn that outlived its first read was
    recorded as "the provider stopped without a reply" (live QA, 2026-09-30).
    A raw fd has no such lock; shutdown has nothing to contend for.

    Offers exactly what the relays use: `read1(n)` and a bounded `readline(n)`.
    """

    def __init__(self, fd=0):
        self._fd = os.dup(fd)
        self._pending = bytearray()
        self._eof = False

    def _fill(self):
        if self._eof:
            return
        chunk = os.read(self._fd, 65536)
        if chunk:
            self._pending += chunk
        else:
            self._eof = True

    def _take(self, count):
        out = bytes(self._pending[:count])
        del self._pending[:count]
        return out

    def read1(self, size):
        if not self._pending:
            self._fill()
        return self._take(size)

    def readline(self, limit=-1):
        while True:
            newline = self._pending.find(b"\n")
            if newline >= 0 and (limit < 0 or newline < limit):
                return self._take(newline + 1)
            if 0 <= limit <= len(self._pending):
                return self._take(limit)
            if self._eof:
                return self._take(len(self._pending))
            self._fill()


# How the relays open the launcher's stdin. A seam for tests only.
_stdin_reader = _FdReader


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


# Bounded line relay for steering: each JSON line must fit, and there's a total cap.
_STEER_MAX_LINE_BYTES = 64 * 1024
_STEER_MAX_TOTAL_BYTES = 1024 * 1024  # 1 MiB total steering input per run


def _relay_steering(destination, source, max_line=_STEER_MAX_LINE_BYTES,
                    max_total=_STEER_MAX_TOTAL_BYTES):
    """Relay steering JSON lines from `source` to `destination` until EOF or child exits.

    Unlike `_relay_stdin` (bounded byte count then close), this:
    - Reads line-by-line (steering protocol is JSON lines)
    - Stays open for the run duration (steering may come at any time)
    - Bounds per-line and total bytes for safety
    - Closes destination only when source EOF or total exceeded

    BYTES in, bytes out. `destination` is the sandbox's binary `Popen` pipe, so
    `source` must be binary too (an `_FdReader`, never the text `sys.stdin`):
    writing a `str` there raised `TypeError`, which killed this thread on the
    first steer and — through the `finally` — closed the adapter's stdin, so
    every follow-up after that went unanswered. Lines are read with a bound, so
    one runaway line cannot grow the launcher either.
    """
    total = 0
    try:
        while True:
            line = source.readline(max_line + 1)
            if not line:
                break
            if not isinstance(line, bytes):
                raise TypeError("steering relay source must be binary")
            if len(line) > max_line:
                # Oversized line is dropped, not fatal — including the rest of
                # it that the bounded read left behind.
                while line and not line.endswith(b"\n"):
                    line = source.readline(max_line + 1)
                continue
            total += len(line)
            if total > max_total:
                break
            destination.write(line)
            destination.flush()
    except (BrokenPipeError, ValueError, OSError):
        pass  # child exited; the main loop handles the outcome
    finally:
        try:
            destination.close()
        except (BrokenPipeError, OSError):
            pass


def _run_child(spec, env, bind_source=None, relay_stdin=False, relay_steering=False):
    """Launch the provider in the sandbox, forward its stream, and always tear the
    sandbox down. Runs while the credential volume is lent to this run's uid.

    `relay_stdin`: bounded relay for metadata probes (64KB cap then close).
    `relay_steering`: line-based relay for steering (stays open for the run).
    """
    # Open stdin pipe if either relay mode is requested
    needs_stdin = relay_stdin or relay_steering
    try:
        handle = sandbox.launch(
            spec, env=env, bind_source=bind_source, stdin_pipe=needs_stdin
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
            args=(handle.stdin, _stdin_reader()),
            daemon=True,
        ).start()
    elif relay_steering and handle.stdin is not None:
        # Steering relay: read JSON lines from launcher stdin, relay to adapter.
        # Binary (the sandbox pipe is binary, see `_relay_steering`) and read
        # through a raw fd, NEVER `sys.stdin.buffer` (see `_FdReader`).
        threading.Thread(
            target=_relay_steering,
            args=(handle.stdin, _stdin_reader()),
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
