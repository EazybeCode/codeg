#!/usr/bin/env python3
"""Copy immutable Neev originals to a Hetzner Storage Box and verify them.

Commands: replicate (copy + remote sha256 -> ledger), verify (re-hash remote
copies, recorded as the health gate for eviction), evict (pressure-bounded
removal of idle, freshly re-verified local copies; the app reads them back
through neev_originals_broker.py), purge (remove remote copies of blobs the app
deleted and the broker tombstoned), status, preflight. Nothing overwrites an
original; the only deletions are evict (local, verified remote copy) and purge
(remote, tombstoned by the app after its row was deleted). Design and deploy
order: docs/operations/neev-storage-box.md.

Local layout (BlobStore): ``{root}/{org_id}/{uuid}.{md|img|att}``, org dirs 0700,
final files 0600 with one link. ``*.tmp`` files are unpublished and skipped.
Remote layout mirrors it: ``{remote_root}/{org_id}/{uuid}.{ext}``.

Exit codes: 0 ok, 1 configuration, 2 local integrity, 3 remote unavailable,
4 remote verification mismatch.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys
import tempfile

# Published at https://docs.hetzner.com/storage/storage-box/general/ (checked
# 2026-09-30). DSA is omitted: current OpenSSH no longer accepts it. A rotation
# by Hetzner must be re-verified from that page and reviewed here.
HETZNER_STORAGE_BOX_FINGERPRINTS = {
    "SHA256:XqONwb1S0zuj5A1CDxpOSuD2hnAArV1A3wKY7Z3sdgM": "ED25519",
    "SHA256:EMlfI8GsRIfpVkoW1H2u0zYVpFGKkIMKHFZIRkf2ioI": "RSA",
    "SHA256:oDHZqKXnoMtgvPBjjC57pcuFez28roaEuFcfwyg8O5c": "ECDSA",
}
PORT = 23  # the only port with the restricted shell (sha256sum) plus rsync
HOST = re.compile(r"^u[0-9]{1,10}(-sub[0-9]{1,3})?\.your-storagebox\.de$")
USER = re.compile(r"^u[0-9]{1,10}(-sub[0-9]{1,3})?$")
SEGMENT = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$")
ORG = re.compile(r"^[1-9][0-9]{0,9}$")
BLOB = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.(md|img|att)$"
)
SHA_LINE = re.compile(r"^([0-9a-f]{64})\s+\*?(\S+)$")
SAFE_PATH = re.compile(r"^/[A-Za-z0-9._/-]+$")  # rsync -e splits on whitespace
VERIFY_CHUNK = 50
# Reading for a hash must not look like use: without this, hourly replication
# would refresh atime (relatime) and no blob would ever become idle.
READ_FLAGS = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_NOATIME", 0)


class ConfigError(Exception):
    code = 1


class LocalIntegrityError(Exception):
    code = 2


class RemoteUnavailable(Exception):
    code = 3


class RemoteMismatch(Exception):
    code = 4


def _private_file(path, label, allow_world_read=False):
    if not SAFE_PATH.match(path) or os.path.realpath(path) != path:
        raise ConfigError(f"{label} must be an absolute, symlink-free path without spaces")
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        raise ConfigError(f"{label} does not exist") from None
    forbidden = 0o022 if allow_world_read else 0o077
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & forbidden:
        raise ConfigError(f"{label} must be a regular file owned by this user with private mode")


def load_config(path):
    _private_file(path, "config")
    with open(path, encoding="utf-8") as handle:
        raw = json.load(handle)
    required = {"host", "user", "identity_file", "known_hosts_file", "remote_root", "local_root", "state_dir"}
    allowed = required | {"port", "allow_main_account", "broker", "eviction"}
    if not isinstance(raw, dict) or not required <= raw.keys() or not raw.keys() <= allowed:
        raise ConfigError(f"config keys must be {sorted(required)} (+ optional port, allow_main_account)")
    if raw.get("port", PORT) != PORT:
        raise ConfigError("port must be 23 (rsync + restricted shell for sha256sum)")
    if not HOST.match(str(raw["host"])) or not USER.match(str(raw["user"])):
        raise ConfigError("host/user must be a Hetzner Storage Box account (uNNN[-subN])")
    if "-sub" not in raw["user"] and raw.get("allow_main_account") is not True:
        raise ConfigError("use a dedicated sub-account; set allow_main_account only by explicit decision")
    if not SEGMENT.match(str(raw["remote_root"])):
        raise ConfigError("remote_root must be one relative directory name, e.g. neev-originals")
    for key in ("local_root", "state_dir"):
        value = raw[key]
        if not isinstance(value, str) or not SAFE_PATH.match(value) or os.path.realpath(value) != value:
            raise ConfigError(f"{key} must be an absolute, symlink-free path without spaces")
    _private_file(raw["identity_file"], "identity_file")
    _private_file(raw["known_hosts_file"], "known_hosts_file", allow_world_read=True)
    return raw


def ssh_options(config):
    """Options shared by ssh and rsync. Nothing is inherited from ~/.ssh/config."""
    return [
        "-F", "/dev/null",
        "-p", str(PORT),
        "-i", config["identity_file"],
        "-o", "IdentitiesOnly=yes",
        "-o", "IdentityAgent=none",
        "-o", "StrictHostKeyChecking=yes",
        "-o", "UserKnownHostsFile=" + config["known_hosts_file"],
        "-o", "GlobalKnownHostsFile=/dev/null",
        "-o", "UpdateHostKeys=no",
        "-o", "BatchMode=yes",
        "-o", "ForwardAgent=no",
        "-o", "ClearAllForwardings=yes",
        "-o", "ConnectTimeout=20",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=3",
    ]


def remote_command(config, words):
    """One restricted-shell command. Every word is validated by the caller."""
    return ["ssh", *ssh_options(config), f"{config['user']}@{config['host']}", "--", *words]


def check_known_hosts(config, run=subprocess.run):
    """The pinned host key must already exist and match Hetzner's published list.

    This never fetches a key (no ssh-keyscan, no accept-new). The operator adds the
    entry from a trusted channel; this check refuses anything unpublished.
    """
    target = f"[{config['host']}]:{PORT}"
    found = run(["ssh-keygen", "-F", target, "-f", config["known_hosts_file"]],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    entries = [line for line in found.stdout.splitlines() if line and not line.startswith("#")]
    if found.returncode != 0 or not entries:
        raise ConfigError(f"known_hosts has no pinned entry for {target}")
    with tempfile.NamedTemporaryFile("w", suffix=".known_hosts") as handle:
        handle.write("\n".join(entries) + "\n")
        handle.flush()
        listed = run(["ssh-keygen", "-l", "-E", "sha256", "-f", handle.name],
                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    fingerprints = [line.split()[1] for line in listed.stdout.splitlines() if len(line.split()) > 1]
    if listed.returncode != 0 or not fingerprints:
        raise ConfigError("could not read pinned host key fingerprints")
    unknown = [fp for fp in fingerprints if fp not in HETZNER_STORAGE_BOX_FINGERPRINTS]
    if unknown:
        raise ConfigError(f"pinned key(s) not in Hetzner's published fingerprints: {unknown}")
    return fingerprints


def _sha256_file(fd):
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = os.read(fd, 1 << 20)
        if not chunk:
            return digest.hexdigest(), size
        digest.update(chunk)
        size += len(chunk)


def _private_dir(path, owner):
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != owner or info.st_mode & 0o7777 != 0o700:
        raise LocalIntegrityError(f"{path} must be a 0700 directory owned by the blob-store user")


def inventory(local_root):
    """Hash every finalized blob. Unexpected entries fail closed.

    Blobs are write-once (published without replace), so copying a live root is
    safe; a blob removed by the orphan sweep mid-scan is simply absent.
    """
    owner = os.lstat(local_root).st_uid
    _private_dir(local_root, owner)
    blobs = []
    in_flight = 0
    for org in sorted(os.listdir(local_root), key=lambda name: (len(name), name)):
        if not ORG.match(org) or int(org) > 2147483647:
            raise LocalIntegrityError(f"unexpected entry in document root: {org!r}")
        org_path = os.path.join(local_root, org)
        _private_dir(org_path, owner)
        names = sorted(os.listdir(org_path))
        temporaries = [entry for entry in names if entry.endswith(".tmp")]
        for name in names:
            if name.endswith(".tmp"):
                continue
            if not BLOB.match(name):
                raise LocalIntegrityError(f"unexpected entry {org}/{name!r}")
            try:
                fd = os.open(os.path.join(org_path, name), READ_FLAGS)
            except FileNotFoundError:
                continue
            except OSError:
                raise LocalIntegrityError(f"{org}/{name} is not a readable regular file") from None
            try:
                info = os.fstat(fd)
                if info.st_nlink == 2 and _publishing(name, temporaries):
                    in_flight += 1  # link-then-unlink publish in progress; next run
                    continue
                if (not stat.S_ISREG(info.st_mode) or info.st_uid != owner
                        or info.st_nlink != 1 or info.st_mode & 0o7777 != 0o600):
                    raise LocalIntegrityError(f"{org}/{name} violates the blob file contract")
                sha, size = _sha256_file(fd)
                if size != info.st_size:
                    raise LocalIntegrityError(f"{org}/{name} changed while hashing")
            finally:
                os.close(fd)
            blobs.append({"path": f"{org}/{name}", "sha256": sha, "size": size,
                          "identity": (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_atime_ns, info.st_size),
                          "atime": info.st_atime, "mtime": info.st_mtime})
    return blobs


def _publishing(name, temporaries):
    """The app publishes `{key}.tmp` (documents) or `{key}.{kind}.tmp` by link
    then unlink; for that instant the final has two links."""
    key = name.split(".", 1)[0]
    return any(entry in (f"{key}.tmp", f"{name}.tmp") or entry.startswith(f"{name}.fetch-")
               for entry in temporaries)


def save_json(path, payload):
    """Write a state file atomically: temp, fsync, rename, directory fsync."""
    directory = os.path.dirname(path)
    fd, temporary = tempfile.mkstemp(prefix="." + os.path.basename(path) + ".", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, sort_keys=True, indent=1)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise
    dir_fd = os.open(directory, os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


def tombstone_path(config):
    """Written only by the broker (`forget`); read by everything else."""
    return os.path.join(config["state_dir"], "tombstones.json")


def load_tombstones(config):
    try:
        with open(tombstone_path(config), encoding="utf-8") as handle:
            return json.load(handle)["forgotten"]
    except FileNotFoundError:
        return {}


def verify_record_path(config):
    return os.path.join(config["state_dir"], "last-verify.json")


class Ledger:
    """Local record of remote copies that passed a hash check. Atomic rewrites.

    Written only under the state-directory lock (replicate, evict, purge).
    Entries may carry `evicted_at`: the broker hydrates only those.
    """

    def __init__(self, state_dir):
        self.path = os.path.join(state_dir, "verified.json")
        self.entries = {}
        if os.path.exists(self.path):
            with open(self.path, encoding="utf-8") as handle:
                self.entries = json.load(handle)["verified"]

    def save(self):
        save_json(self.path, {"version": 1, "verified": self.entries})


def _call(run, args, tolerated=(0,)):
    try:
        result = run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    except OSError as error:
        raise RemoteUnavailable(f"{args[0]} could not be started; nothing local was changed") from error
    if result.returncode not in tolerated:
        raise RemoteUnavailable(f"{args[0]} exited {result.returncode}; nothing local was changed")
    return result


def remote_hashes(config, relative_paths, run=subprocess.run):
    """sha256 of remote files via the Storage Box restricted shell. Missing -> absent."""
    found = {}
    root = config["remote_root"]
    for start in range(0, len(relative_paths), VERIFY_CHUNK):
        chunk = relative_paths[start:start + VERIFY_CHUNK]
        args = remote_command(config, ["sha256sum", *[f"{root}/{path}" for path in chunk]])
        try:
            result = run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        except OSError as error:
            raise RemoteUnavailable("ssh could not be started") from error
        # sha256sum exits 1 when some file is missing; 255 is an ssh failure.
        if result.returncode not in (0, 1):
            raise RemoteUnavailable(f"ssh exited {result.returncode}")
        for line in result.stdout.splitlines():
            match = SHA_LINE.match(line.strip())
            if match and match.group(2).startswith(root + "/"):
                found[match.group(2)[len(root) + 1:]] = match.group(1)
    return found


def upload(config, relative_paths, run=subprocess.run):
    """rsync writes a hidden temporary and renames it: no partial final names.

    --ignore-existing: an original is immutable, so an existing remote file is
    never replaced. A conflicting one is caught by the hash check afterwards.
    """
    with tempfile.NamedTemporaryFile("w", suffix=".files") as listing:
        listing.write("\n".join(relative_paths) + "\n")
        listing.flush()
        # 24 = a source vanished (orphan sweep); the hash check decides per file.
        _call(run, [
            "rsync", "--recursive", "--times", "--perms",
            "--chmod=Du=rwx,Dgo=,Fu=rw,Fgo=", "--ignore-existing",
            "--files-from=" + listing.name,
            "-e", " ".join(["ssh", *ssh_options(config)]),
            config["local_root"] + "/",
            f"{config['user']}@{config['host']}:{config['remote_root']}/",
        ], tolerated=(0, 24))


def replicate(config, run=subprocess.run, limit=None, dry_run=False, log=print):
    ledger = Ledger(config["state_dir"])
    local = inventory(config["local_root"])
    forgotten = load_tombstones(config)
    # A tombstoned blob was deleted by the app; never copy it again.
    pending = [blob for blob in local
               if blob["path"] not in forgotten
               and ledger.entries.get(blob["path"], {}).get("sha256") != blob["sha256"]]
    if limit is not None:
        pending = pending[:limit]
    log(f"local originals: {len(local)}; pending copy: {len(pending)} "
        f"({sum(blob['size'] for blob in pending)} bytes)")
    if dry_run or not pending:
        return {"local": len(local), "pending": len(pending), "verified": 0, "vanished": 0}
    upload(config, [blob["path"] for blob in pending], run=run)
    remote = remote_hashes(config, [blob["path"] for blob in pending], run=run)
    now = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    bad, verified, vanished = [], 0, 0
    for blob in pending:
        if remote.get(blob["path"]) == blob["sha256"]:
            entry = ledger.entries.get(blob["path"], {})
            entry.update({"sha256": blob["sha256"], "size": blob["size"], "verified_at": now})
            ledger.entries[blob["path"]] = entry
            verified += 1
        elif os.path.lexists(os.path.join(config["local_root"], blob["path"])):
            bad.append(blob["path"])
        else:
            vanished += 1  # swept as an orphan during the run; nothing to protect
    ledger.save()  # keep what did verify, even when some did not
    if bad:
        raise RemoteMismatch(f"{len(bad)} remote copies missing or different (first: {bad[:5]}); "
                             "nothing was overwritten or deleted")
    log(f"verified remote copies: {verified}; vanished during run: {vanished}")
    return {"local": len(local), "pending": len(pending), "verified": verified, "vanished": vanished}


def verify(config, run=subprocess.run, sample=None, log=print, now=None):
    """Re-hash ledger entries on the Storage Box. Read-only on both sides.

    Evicted originals exist only remotely, so this is the scheduled check that
    finds silent remote loss. Every outcome is recorded; eviction requires a
    recent successful record.
    """
    import time

    ledger = Ledger(config["state_dir"])
    forgotten = load_tombstones(config)
    paths = sorted(path for path in ledger.entries if path not in forgotten)
    full = sample is None or sample >= len(paths)
    if not full:
        paths = sorted(secrets.SystemRandom().sample(paths, sample))
    record = {"at": int(time.time() if now is None else now), "checked": len(paths), "full": full,
              "ok": False, "evicted_checked": sum(1 for p in paths if ledger.entries[p].get("evicted_at"))}
    try:
        remote = remote_hashes(config, paths, run=run)
    except RemoteUnavailable:
        save_json(verify_record_path(config), {**record, "failure": "unavailable"})
        raise
    bad = [path for path in paths if remote.get(path) != ledger.entries[path]["sha256"]]
    if bad:
        save_json(verify_record_path(config), {**record, "failure": "mismatch", "bad": len(bad)})
        raise RemoteMismatch(f"{len(bad)} of {len(paths)} remote copies missing or different: {bad[:5]}")
    save_json(verify_record_path(config), {**record, "ok": True})
    log(f"remote copies verified: {len(paths)}")
    return len(paths)


def status(config, log=print):
    ledger = Ledger(config["state_dir"])
    local = inventory(config["local_root"])
    covered = [b for b in local if ledger.entries.get(b["path"], {}).get("sha256") == b["sha256"]]
    remote_only = set(ledger.entries) - {b["path"] for b in local}
    try:
        with open(verify_record_path(config), encoding="utf-8") as handle:
            last_verify = json.load(handle)
    except FileNotFoundError:
        last_verify = None
    summary = {
        "local_originals": len(local),
        "local_bytes": sum(b["size"] for b in local),
        "verified_remote": len(covered),
        "pending": len(local) - len(covered),
        "remote_only_verified": len(remote_only),
        "evicted": sum(1 for path in remote_only if ledger.entries[path].get("evicted_at")),
        "forgotten": len(load_tombstones(config)),
        "last_verify": last_verify,
        "eviction": "configured" if config.get("eviction") else "not configured",
    }
    log(json.dumps(summary, indent=1))
    return summary


EVICTABLE = ("md", "img", "att")


def eviction_settings(config):
    settings = config.get("eviction")
    if not isinstance(settings, dict):
        raise ConfigError("eviction is not configured")
    allowed = {"kinds", "min_idle_seconds", "max_local_bytes", "min_free_bytes", "max_evictions_per_run",
               "max_verify_age_seconds"}
    if not settings.keys() <= allowed or not {"kinds", "min_idle_seconds"} <= settings.keys():
        raise ConfigError(f"eviction keys must be within {sorted(allowed)}")
    kinds = settings["kinds"]
    if not isinstance(kinds, list) or not kinds or not set(kinds) <= set(EVICTABLE):
        raise ConfigError("eviction.kinds must be a non-empty subset of md/img/att")
    # A day is the floor: recent uploads and outputs always stay local.
    for key, low in (("min_idle_seconds", 86400), ("max_local_bytes", 0),
                     ("min_free_bytes", 0), ("max_evictions_per_run", 1), ("max_verify_age_seconds", 3600)):
        if key in settings and (type(settings[key]) is not int or settings[key] < low):
            raise ConfigError(f"eviction.{key} must be an integer >= {low}")
    if "max_local_bytes" not in settings and "min_free_bytes" not in settings:
        raise ConfigError("eviction needs max_local_bytes and/or min_free_bytes (pressure bound)")
    return settings


def evict(config, run=subprocess.run, now=None, broker_ping=None, dry_run=False, log=print):
    """Free local space by unlinking idle blobs that the Storage Box holds.

    Every candidate must be: an allowed kind; in the ledger with the same hash and
    size as the local bytes; unread and unmodified for min_idle_seconds; re-hashed
    on the Storage Box during this run; and unchanged (same inode, size, mtime,
    atime) immediately before unlink. Nothing happens unless the cache is over
    max_local_bytes or free space is under min_free_bytes, and nothing happens
    when the broker (the read-through path) or the Storage Box is unreachable.
    """
    import time

    settings = eviction_settings(config)
    now = time.time() if now is None else now
    if broker_ping is None:
        raise ConfigError("broker health check is required before eviction")
    if not broker_ping():
        raise RemoteUnavailable("originals broker is not answering; nothing evicted")
    # Remote copies become the only local-disk-independent copy: require that
    # the scheduled verify ran recently and found every checked copy intact.
    try:
        with open(verify_record_path(config), encoding="utf-8") as handle:
            last = json.load(handle)
    except FileNotFoundError:
        last = {}
    if not last.get("ok") or last.get("at", 0) < now - settings.get("max_verify_age_seconds", 26 * 3600):
        raise RemoteMismatch("no recent successful verify; run `verify` (scheduled) before eviction")
    forgotten = load_tombstones(config)
    ledger = Ledger(config["state_dir"])
    local = inventory(config["local_root"])
    local_bytes = sum(blob["size"] for blob in local)
    fs = os.statvfs(config["local_root"])
    free = fs.f_bavail * fs.f_frsize
    need = max(0,
               local_bytes - settings.get("max_local_bytes", local_bytes),
               settings.get("min_free_bytes", 0) - free)
    summary = {"local_bytes": local_bytes, "free_bytes": free, "need_bytes": need,
               "evicted": 0, "evicted_bytes": 0, "skipped_changed": 0, "remote_unverified": 0}
    if need == 0:
        log(json.dumps(summary))
        return summary
    cutoff = now - settings["min_idle_seconds"]
    candidates = sorted(
        (blob for blob in local
         if blob["path"].rsplit(".", 1)[1] in settings["kinds"]
         and blob["path"] not in forgotten
         and ledger.entries.get(blob["path"], {}).get("sha256") == blob["sha256"]
         and ledger.entries[blob["path"]].get("size") == blob["size"]
         and blob["atime"] <= cutoff and blob["mtime"] <= cutoff),
        key=lambda blob: (blob["atime"], blob["path"]))
    chosen, planned = [], 0
    for blob in candidates:
        if planned >= need or len(chosen) >= settings.get("max_evictions_per_run", 500):
            break
        chosen.append(blob)
        planned += blob["size"]
    if not chosen:
        log(json.dumps(summary))
        return summary
    remote = remote_hashes(config, [blob["path"] for blob in chosen], run=run)
    confirmed = [blob for blob in chosen if remote.get(blob["path"]) == blob["sha256"]]
    summary["remote_unverified"] = len(chosen) - len(confirmed)
    if confirmed and not dry_run:
        # Record intent before unlinking: the broker hydrates only entries marked
        # evicted, so a crash mid-run can never leave an unrestorable blob.
        stamp = int(now)
        for blob in confirmed:
            ledger.entries[blob["path"]]["evicted_at"] = stamp
        ledger.save()
    for blob in confirmed:
        if dry_run:
            summary["evicted"] += 1
            summary["evicted_bytes"] += blob["size"]
            continue
        org, name = blob["path"].split("/")
        org_fd = os.open(os.path.join(config["local_root"], org), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            try:
                info = os.stat(name, dir_fd=org_fd, follow_symlinks=False)
            except FileNotFoundError:
                continue
            if ((info.st_dev, info.st_ino, info.st_mtime_ns, info.st_atime_ns, info.st_size) != blob["identity"]
                    or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1):
                summary["skipped_changed"] += 1  # read or replaced since selection
                continue
            # A reader holding it open keeps its bytes; a later open hydrates.
            os.unlink(name, dir_fd=org_fd)
            os.fsync(org_fd)
        finally:
            os.close(org_fd)
        summary["evicted"] += 1
        summary["evicted_bytes"] += blob["size"]
    log(json.dumps(summary))
    if summary["remote_unverified"]:
        raise RemoteMismatch(f"{summary['remote_unverified']} candidates failed the fresh remote check and were kept")
    return summary


def purge(config, run=subprocess.run, older_than_seconds=30 * 86400, now=None, dry_run=False, log=print):
    """Remove remote copies of blobs the app deleted.

    Only paths the broker tombstoned (`forget`, sent by the app after the row was
    deleted) and older than the grace period are touched, one fixed remote path
    each. The tombstone stays forever, so the blob can never be hydrated,
    streamed or replicated again; the ledger entry is dropped once the remote
    file is confirmed gone.
    """
    import time

    now = time.time() if now is None else now
    ledger = Ledger(config["state_dir"])
    forgotten = load_tombstones(config)
    due = sorted(path for path, mark in forgotten.items()
                 if path in ledger.entries and mark.get("at", now) <= now - older_than_seconds
                 and BLOB.match(path.split("/")[-1]) and ORG.match(path.split("/")[0]))
    summary = {"due": len(due), "purged": 0}
    if dry_run or not due:
        log(json.dumps(summary))
        return summary
    root = config["remote_root"]
    for path in due:
        # rm fails harmlessly for an already-missing file; absence is re-checked.
        _call(run, remote_command(config, ["rm", f"{root}/{path}"]), tolerated=(0, 1))
    still_there = remote_hashes(config, due, run=run)
    for path in due:
        if path not in still_there:
            del ledger.entries[path]
            summary["purged"] += 1
    ledger.save()
    log(json.dumps(summary))
    if still_there:
        raise RemoteMismatch(f"{len(still_there)} tombstoned remote copies could not be removed")
    return summary


def preflight(config, run=subprocess.run, connect=False, write_probe=False, log=print):
    for tool in ("ssh", "ssh-keygen", "rsync"):
        if not any(os.access(os.path.join(d, tool), os.X_OK) for d in os.environ.get("PATH", "").split(":")):
            raise ConfigError(f"{tool} is not installed")
    fingerprints = check_known_hosts(config, run=run)
    log(f"pinned host key matches Hetzner publication: {', '.join(fingerprints)}")
    owner = os.lstat(config["local_root"]).st_uid
    _private_dir(config["local_root"], owner)
    _private_dir(config["state_dir"], os.getuid())
    log("local document root and state directory: ok")
    if not connect:
        log("offline checks passed; connectivity NOT tested (use --connect)")
        return
    _call(run, remote_command(config, ["ls", "-la"]))
    log("ssh with strict host key checking: ok")
    if write_probe:
        probe_name = f".preflight-{secrets.token_hex(8)}"
        payload = secrets.token_bytes(64)
        with tempfile.TemporaryDirectory() as scratch:
            os.chmod(scratch, 0o700)
            Path(scratch, probe_name).write_bytes(payload)
            _call(run, ["rsync", "--times", "-e", " ".join(["ssh", *ssh_options(config)]),
                        os.path.join(scratch, probe_name),
                        f"{config['user']}@{config['host']}:{config['remote_root']}/"])
        found = remote_hashes(config, [probe_name], run=run)
        # Remove only the probe this run created.
        _call(run, remote_command(config, ["rm", f"{config['remote_root']}/{probe_name}"]))
        if found.get(probe_name) != hashlib.sha256(payload).hexdigest():
            raise RemoteMismatch("write probe hash mismatch")
        log("write + sha256 verification probe: ok")


def _broker_ping(config):
    import socket

    path = (config.get("broker") or {}).get("socket")
    if not path:
        return False
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(10)
            client.connect(path)
            client.sendall(b'{"v":1,"op":"ping"}\n')
            reply = client.recv(256)
        return json.loads(reply) == {"ok": True, "result": "pong"}
    except (OSError, ValueError):
        return False


def _acquire(lock, wait_seconds):
    import time

    deadline = time.monotonic() + wait_seconds
    while True:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return
        except BlockingIOError:
            if time.monotonic() >= deadline:
                raise ConfigError("another neev_storage_box run holds the lock") from None
            time.sleep(min(5, max(0.05, deadline - time.monotonic())))


def main(argv=None, run=subprocess.run, lock_wait_seconds=1800):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", default="/etc/neev-storage-box/config.json")
    commands = parser.add_subparsers(dest="command", required=True)
    pre = commands.add_parser("preflight")
    pre.add_argument("--connect", action="store_true")
    pre.add_argument("--write-probe", action="store_true")
    rep = commands.add_parser("replicate")
    rep.add_argument("--limit", type=int)
    rep.add_argument("--dry-run", action="store_true")
    ver = commands.add_parser("verify")
    ver.add_argument("--sample", type=int)
    commands.add_parser("status")
    ev = commands.add_parser("evict")
    ev.add_argument("--dry-run", action="store_true")
    pu = commands.add_parser("purge")
    pu.add_argument("--older-than-days", type=int, default=30)
    pu.add_argument("--dry-run", action="store_true")
    options = parser.parse_args(argv)
    try:
        config = load_config(options.config)
        if options.command == "preflight":
            preflight(config, run=run, connect=options.connect or options.write_probe,
                      write_probe=options.write_probe)
            return 0
        _private_dir(config["state_dir"], os.getuid())
        with open(os.path.join(config["state_dir"], "lock"), "a") as lock:
            # Hourly replicate skips a busy window; scheduled verify/evict/purge
            # wait (bounded) for a running replicate instead of failing.
            _acquire(lock, 0 if options.command in ("replicate", "status") else lock_wait_seconds)
            if options.command == "replicate":
                replicate(config, run=run, limit=options.limit, dry_run=options.dry_run)
            elif options.command == "verify":
                verify(config, run=run, sample=options.sample)
            elif options.command == "evict":
                evict(config, run=run, broker_ping=lambda: _broker_ping(config), dry_run=options.dry_run)
            elif options.command == "purge":
                purge(config, run=run, older_than_seconds=max(1, options.older_than_days) * 86400,
                      dry_run=options.dry_run)
            else:
                status(config)
        return 0
    except (ConfigError, LocalIntegrityError, RemoteUnavailable, RemoteMismatch) as error:
        print(f"neev_storage_box: {error}", file=sys.stderr)
        return error.code
    except (OSError, ValueError) as error:
        print(f"neev_storage_box: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
