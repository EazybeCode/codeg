#!/usr/bin/env python3
"""Resolve and deliver ONE run's per-user, per-provider credential volume into its
sandbox HOME so the provider — running as its isolation-lane uid, which is NOT the
canonical volume owner — can read its credentials AND persist refreshed tokens,
without ever racing a concurrent run for the same volume.

The native runtime spawns `neev_agent_launch.py` with a server-authoritative
`--tenant org:<org>/user:<user>`, a `--provider`, and a `--credential-handle`. This
module turns those into a `neev_credential_store.Scope` and opens the caller's own
LUKS-encrypted credential volume via `neev_credential_volumes.CredentialVolumes`.

Ownership model (why owner-uid vs lane-uid works end-to-end):

* The volume is created + kept by the supervisor and owned at rest by the canonical
  service uid (`_secure_filesystem_root` → 65532). A run's provider drops to a
  distinct lane uid, so it cannot read a 0700 65532-owned tree as-is.
* `deliver()` therefore, **while holding a per-scope run lock**, chowns the volume
  tree to the run's lane uid for the duration of the run, then chowns it BACK to the
  canonical owner on exit. Because the lock is per (org, user, provider), two
  concurrent runs for the SAME volume are serialized — they can never race the
  chown or interleave refresh writes — while different users/providers (different
  locks, different volumes) run concurrently and never see each other's volume.
* The run lock is a dedicated lockfile, NOT the credential store's global metadata
  lock (which is non-blocking and must stay free for create/open/close), so holding
  it for a run's lifetime does not stall unrelated credential operations.

Isolation invariants preserved: the scope is server-authoritative (from `--tenant`
+ `--provider`); the volume key is derived from that scope + handle, so a handle for
another user/provider cannot unlock this scope. Every provider resolves config only
under HOME (Claude `$HOME/.claude`, Codex `$HOME/.codex`, Cursor `$HOME/.cursor` and
`$XDG_CONFIG_HOME/cursor`, Gemini `$HOME/.gemini`, OpenCode `$XDG_DATA_HOME/opencode`
+ `$XDG_CONFIG_HOME/opencode`, all XDG roots pinned inside HOME), so none can escape
to a host login, and the provider env is a strict secret-free allowlist (see
`neev_agent_launch.child_env`).
"""

from __future__ import annotations

import contextlib
import errno
import fcntl
import os
import stat
import sys
import time
from dataclasses import dataclass
from typing import Callable, Dict, Iterator, Tuple

from neev_credential_store import Scope, StoreError

PROVIDERS: Tuple[str, ...] = ("claude", "codex", "cursor", "gemini", "opencode")
# The ACP providers (run through neev_acp_stream_adapter). Separate so callers can
# tell a Claude/Codex-native provider from one driven over ACP.
ACP_PROVIDERS: Tuple[str, ...] = ("cursor", "gemini", "opencode")
# Canonical at-rest owner of every credential volume (matches
# neev_credential_volumes._secure_filesystem_root). A run temporarily borrows
# ownership under the run lock and restores this on exit.
CANONICAL_OWNER_UID = 65532
CANONICAL_OWNER_GID = 65532
# Bound on a volume's entry count so a corrupt/hostile tree cannot make the chown
# walk unbounded. A credential volume holds a handful of small files.
MAX_TREE_ENTRIES = 4096
# Where per-scope run locks live. Root provisions this 0700 root-owned directory
# (see the report); it is NOT the credential store's metadata lock.
RUN_LOCK_DIR = "/var/lib/neev-supervisor/run-locks"
# Bounded wait for the per-scope run lock before giving up (another run for the same
# user+provider is holding it). Keeps a stuck peer from wedging a slot forever.
RUN_LOCK_TIMEOUT_SECONDS = 120.0


class CredentialError(Exception):
    """Delivery refused. `category` is a stable, non-sensitive reason code."""

    def __init__(self, category: str):
        super().__init__(category)
        self.category = category


def parse_tenant(tenant: str) -> Tuple[int, int]:
    """Parse the runtime's `org:<int>/user:<int>` tenant string (canonical decimals,
    no sign, no leading zero, no extra segments)."""
    if not isinstance(tenant, str):
        raise CredentialError("tenant")
    parts = tenant.split("/")
    if len(parts) != 2 or not parts[0].startswith("org:") or not parts[1].startswith("user:"):
        raise CredentialError("tenant")

    def _positive(text: str) -> int:
        if not text.isascii() or not text.isdigit():
            raise CredentialError("tenant")
        if len(text) > 1 and text[0] == "0":
            raise CredentialError("tenant")
        value = int(text)
        if value <= 0 or value > 2_147_483_647:
            raise CredentialError("tenant")
        return value

    return _positive(parts[0][len("org:") :]), _positive(parts[1][len("user:") :])


def scope_of(tenant: str, provider: str) -> Scope:
    org_id, user_id = parse_tenant(tenant)
    if provider not in PROVIDERS:
        raise CredentialError("provider")
    return Scope(org_id, user_id, provider)


# Directories each provider's config/credentials live in, relative to HOME. Created
# (0700) before the lend so the provider never has to create them itself.
_PROVIDER_CONFIG_SUBDIRS = {
    "claude": (".claude",),
    "codex": (".codex",),
    # `~/.cursor/cli-config.json`, or `$XDG_CONFIG_HOME/cursor/` (cursor.com/docs/
    # cli/reference/configuration). XDG_CONFIG_HOME is pinned to HOME/.config below.
    "cursor": (".cursor", ".config", ".config/cursor"),
    # `~/.gemini/settings.json`, `~/.gemini/oauth_creds.json`.
    "gemini": (".gemini",),
    # `~/.local/share/opencode/auth.json` (opencode.ai/docs/cli), config under
    # `$XDG_CONFIG_HOME/opencode`.
    "opencode": (".local", ".local/share", ".local/share/opencode", ".config",
                 ".config/opencode", ".local/state", ".cache"),
}


def provider_env(home: str, provider: str) -> Dict[str, str]:
    """The environment binding the provider's config strictly to the sandbox HOME.

    Values are FIXED per provider; nothing is taken from the caller's environment.
    Browser suppression is the official headless switch for each CLI (Cursor
    `NO_OPEN_BROWSER`, Gemini `NO_BROWSER`); without it the CLI tries to launch a
    browser that does not exist in the sandbox. `GEMINI_FORCE_FILE_STORAGE` keeps
    Gemini's OAuth record in a file under HOME (keychainService.ts) rather than an OS
    keyring that would live outside the per-user volume."""
    if provider not in PROVIDERS:
        raise CredentialError("provider")
    env = {"HOME": home}
    if provider == "claude":
        env["CLAUDE_CONFIG_DIR"] = os.path.join(home, ".claude")
    elif provider == "codex":
        env["CODEX_HOME"] = os.path.join(home, ".codex")
    elif provider == "cursor":
        env["XDG_CONFIG_HOME"] = os.path.join(home, ".config")
        env["NO_OPEN_BROWSER"] = "1"
        # The pinned CLI (2026.09.28-64d2043, src/utils/credential-store.ts)
        # reads AGENT_CLI_CREDENTIAL_STORE: "file" keeps the sign-in in
        # $XDG_CONFIG_HOME/cursor/auth.json (0600) inside this per-user volume,
        # never an OS keyring outside it.
        env["AGENT_CLI_CREDENTIAL_STORE"] = "file"
    elif provider == "gemini":
        env["NO_BROWSER"] = "true"
        env["GEMINI_FORCE_FILE_STORAGE"] = "true"
    else:  # opencode
        env["XDG_CONFIG_HOME"] = os.path.join(home, ".config")
        env["XDG_DATA_HOME"] = os.path.join(home, ".local", "share")
        env["XDG_STATE_HOME"] = os.path.join(home, ".local", "state")
        env["XDG_CACHE_HOME"] = os.path.join(home, ".cache")
    return env


# Gemini picks its auth method from `security.auth.selectedType` in settings.json;
# with none set, ACP `session/new` falls back to the API-key method and fails
# (observed on pinned 0.62.0: -32000 "Gemini API key is missing"). Neev offers ONLY
# the Google sign-in, so a fresh volume is seeded with it — never an API key.
# Usage statistics are off: that traffic has no allowlisted egress anyway.
GEMINI_SEED_SETTINGS = (
    b'{"security":{"auth":{"selectedType":"oauth-personal"}},'
    b'"privacy":{"usageStatisticsEnabled":false}}\n'
)


def _seed_file(path: str, content: bytes) -> None:
    """Create `path` with `content` only if it does not exist. Never follows a
    symlink and never overwrites: an existing file is the user's own setting."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                     | os.O_CLOEXEC, 0o600)
    except FileExistsError:
        return
    except OSError as error:
        raise CredentialError("credentials") from error
    try:
        view = memoryview(content)
        while view:
            written = os.write(fd, view)
            view = view[written:]
    except OSError as error:
        raise CredentialError("credentials") from error
    finally:
        os.close(fd)


def _ensure_provider_home(home: str, provider: str) -> None:
    """Create the provider's config dir inside the delivered HOME if it is absent.

    The Codex app-server REFUSES to start when $CODEX_HOME does not exist — it errors
    ("CODEX_HOME points to ... but that path does not exist") and exits rather than
    creating it — so a FRESH login volume (empty HOME) can never reach the device-code
    challenge without this. Claude tolerates a missing dir, but we create both for
    consistency. Made under the held run lock as the (root) owner; the subsequent lend
    chown makes it writable by the provider's lane uid."""
    for subdir in _PROVIDER_CONFIG_SUBDIRS.get(provider, ()):
        path = os.path.join(home, subdir)
        try:
            os.mkdir(path, 0o700)
        except FileExistsError:
            # Must be a REAL directory: a symlink planted in the volume would
            # otherwise redirect the provider's config (and the seed below).
            if os.path.islink(path) or not os.path.isdir(path):
                raise CredentialError("shape") from None
        except OSError as error:
            raise CredentialError("credentials") from error
    if provider == "gemini":
        _seed_file(os.path.join(home, ".gemini", "settings.json"), GEMINI_SEED_SETTINGS)


# The ONLY codes a release diagnostic may name — every one is a fixed literal from
# `neev_credential_store._SAFE_ERROR_CODES`. Anything else degrades to "unknown", so
# no path, handle, scope or provider message can reach the daemon log through here.
_RELEASE_DIAG_CODES = frozenset(("invalid", "unavailable", "conflict", "busy", "quota"))


def _release_volume(volumes, scope: Scope, handle: str) -> None:
    """Close the volume this lend opened, so it is unmapped once the run is over.

    `open`/`close` are a PAIR. `open` attaches the image through an autoclear loop
    device; closing the mapping is what detaches it. Skipping the close leaves one
    loop device pinned per volume for the life of the container, and a container has
    only the loop device nodes it was given. Once they are all pinned, the very next
    `CredentialVolumes.create` fails at `cryptsetup open` ("Attaching loopback device
    failed"), `Supervisor._start_login` turns that into a bare UNAVAILABLE, and the
    server cancels the attempt it just recorded — every sign-in, for every provider,
    dying one second in as "This service is temporarily unavailable". Capacity must
    depend on how many runs are in flight, not on how many users have ever signed in.

    Closing is NOT the destructive half of teardown: `close` only unmounts and
    unmaps a mapping it has proven belongs to this exact image, while `revoke` is
    what deletes credentials — and a lend must still never revoke.

    A close that cannot be completed is deliberately not fatal. A busy unmount is
    documented to leave the volume mounted for an administrator to inspect, the
    store's metadata lock is non-blocking so a concurrent create/open can make this
    transiently unavailable, and `open` revalidates and reuses a surviving mapping,
    so the next lend converges. Failing a finished sign-in over a teardown blip would
    be strictly worse than the leak. It is reported, never silent.

    `volumes` may be a caller-supplied double that only implements `open` (the
    documented minimum), so the close is looked up rather than assumed.
    """
    close = getattr(volumes, "close", None)
    if close is None:
        return
    try:
        close(scope, handle)
    except Exception as error:  # noqa: BLE001 - deliberately non-fatal, see above
        code = getattr(error, "code", None)
        if type(code) is not str or code not in _RELEASE_DIAG_CODES:
            code = "unknown"
        with contextlib.suppress(Exception):
            sys.stderr.write("NEEV-CRED-DIAG stage=release code=%s\n" % code)
            sys.stderr.flush()


@dataclass(frozen=True)
class Delivery:
    # `home` is the path the PROVIDER sees (its HOME); `mount` is the PHYSICAL mount
    # the credentials live on (under the 0700 secret store). They differ when the
    # sandbox bind-mounts `mount` onto a lane-traversable `home` in its private mount
    # namespace: ownership transfer operates on `mount` (same inodes), the provider
    # env points at `home`.
    home: str
    env: Dict[str, str]
    mount: str = None


def _chown_tree(root: str, uid: int, gid: int) -> None:
    """Recursively chown a trusted credential volume to (uid, gid), symlink-safe and
    bounded. Never follows a symlink (chowns the link itself), never descends into a
    symlinked directory, and refuses a tree larger than MAX_TREE_ENTRIES."""
    seen = 0
    os.chown(root, uid, gid, follow_symlinks=False)
    for current, dirs, files in os.walk(root, topdown=True, followlinks=False):
        for name in dirs + files:
            seen += 1
            if seen > MAX_TREE_ENTRIES:
                raise CredentialError("tree")
            os.chown(os.path.join(current, name), uid, gid, follow_symlinks=False)


@contextlib.contextmanager
def _scope_run_lock(scope: Scope, lock_dir: str) -> Iterator[None]:
    """Hold a blocking exclusive lock for exactly this (org, user, provider) so
    concurrent runs for the SAME volume serialize (no chown/refresh race). A stale
    peer cannot wedge us forever: we bound the wait."""
    try:
        os.makedirs(lock_dir, mode=0o700, exist_ok=True)
    except OSError as error:
        raise CredentialError("lockdir") from error
    path = os.path.join(
        lock_dir, f"{scope.org_id}-{scope.user_id}-{scope.provider}.lock"
    )
    fd = None
    try:
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        deadline = time.monotonic() + RUN_LOCK_TIMEOUT_SECONDS
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EAGAIN, errno.EWOULDBLOCK):
                    raise CredentialError("lock") from error
                if time.monotonic() >= deadline:
                    raise CredentialError("busy") from None
                time.sleep(0.1)
        yield
    except CredentialError:
        raise
    except OSError as error:
        raise CredentialError("lock") from error
    finally:
        if fd is not None:
            try:
                fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError:
                pass
            os.close(fd)


@contextlib.contextmanager
def deliver(
    volumes,
    tenant: str,
    provider: str,
    handle: str,
    run_uid: int,
    run_gid: int,
    *,
    lock_dir: str = None,
    home_as: str = None,
    chown_tree: Callable[[str, int, int], None] = _chown_tree,
    stat_fn: Callable[..., os.stat_result] = os.stat,
    ensure_provider_home: Callable[[str, str], None] = _ensure_provider_home,
) -> Iterator[Delivery]:
    """Open the caller's own credential volume, lend it to the run's lane uid under a
    per-scope lock, yield the HOME + env to run with, and restore the canonical owner
    on exit. Raises `CredentialError` (never leaks a path or secret) on any unsafe or
    unauthorized delivery.

    `volumes` is a `neev_credential_volumes.CredentialVolumes` (or a test double with
    `open(scope, handle) -> path`). Ownership transfer always operates on the physical
    mount. `home_as`, when given, is the path the PROVIDER will see its HOME at (the
    sandbox bind-mounts the physical mount there in its private namespace, so the
    0700 secret store need not be made traversable); the yielded env points at it
    while `mount` carries the physical path for the bind. `chown_tree`/`stat_fn`/
    `lock_dir`/`home_as` are injectable for tests (chown needs root in production)."""
    if not isinstance(handle, str) or not handle:
        raise CredentialError("handle")
    for identity in (run_uid, run_gid):
        if not isinstance(identity, int) or identity <= 0:
            raise CredentialError("uid")
    if home_as is not None and (
        not isinstance(home_as, str) or not home_as.startswith("/") or "\x00" in home_as
    ):
        raise CredentialError("home")
    scope = scope_of(tenant, provider)
    # Resolve the lock dir at call time so a test (or a future config) can override
    # RUN_LOCK_DIR; production passes nothing and uses the provisioned default.
    with _scope_run_lock(scope, lock_dir if lock_dir is not None else RUN_LOCK_DIR):
        try:
            mountpoint = volumes.open(scope, handle)
        except StoreError as error:
            raise CredentialError("credentials") from error
        home = os.fspath(mountpoint)
        # Everything past a SUCCESSFUL open belongs to this outer try: validation, the
        # ownership handoff and the provider-home creation can all fail, and each one
        # of those exits used to abandon a mapped volume. The mapping is the scarce
        # resource (one autoclear loop device each), so the release is owed from the
        # moment `open` returns, not merely from the moment the run starts. Both this
        # and the run's own release happen under the per-scope run lock taken above,
        # so no other lend for this volume can be reading it as it is unmapped.
        try:
            # The mount root must be a real directory (not a symlink) at 0700 before we
            # touch it — refuse to chown through a swapped-in link.
            try:
                info = stat_fn(home, follow_symlinks=False)
            except OSError as error:
                raise CredentialError("credentials") from error
            if not stat.S_ISDIR(info.st_mode):
                raise CredentialError("shape")
            if stat.S_IMODE(info.st_mode) != 0o700:
                raise CredentialError("mode")
            # Recover first, under the lock: if a prior run crashed mid-lend the tree may
            # still be owned by a lane uid. Restoring the canonical owner here is safe —
            # the lock guarantees no other run for this volume — and idempotent when it
            # is already canonical. Then lend to THIS run's lane uid; the lock means no
            # other lane can ever hold or read the volume concurrently, so there is no
            # chown race and refresh writes cannot interleave.
            chown_tree(home, CANONICAL_OWNER_UID, CANONICAL_OWNER_GID)
            # The provider's config dir must exist before it launches (Codex refuses to
            # start otherwise). Create it under the lock, before the lend, so the
            # following chown makes it owned by (writable to) this run's lane uid.
            ensure_provider_home(home, provider)
            chown_tree(home, run_uid, run_gid)
            seen_home = home if home_as is None else home_as
            try:
                yield Delivery(
                    home=seen_home, env=provider_env(seen_home, provider), mount=home
                )
            finally:
                # Restore the canonical owner so refresh tokens written during the run
                # (now this lane uid's) become owner-readable again and the store
                # invariant holds at rest. Best-effort: a failure does not mask the
                # run's result; the next lend (or the supervisor's reconcile)
                # re-recovers it. A refusal BEFORE the lend never chowned, and must not
                # start now — a tree of the wrong shape or mode is not ours to touch.
                with contextlib.suppress(Exception):
                    chown_tree(home, CANONICAL_OWNER_UID, CANONICAL_OWNER_GID)
        finally:
            # Ownership first, then give the mapping back: at rest a volume is
            # canonically owned AND unmapped.
            _release_volume(volumes, scope, handle)
