#!/usr/bin/env python3
"""Encrypted off-server backup of native Neev (database snapshot + originals).

Daily pipeline, each step gating the next:
  lock -> free-disk preflight -> complete content backup (neev-content-backup-run.sh)
  -> local re-verification (every blob hash, including org-knowledge blobs when
     `knowledge_source` is configured, full dump read)
  -> restic backup to a SEPARATE repository, tagged neev-pending
  -> restic check -> restore canary (dump + one blob, and one knowledge blob
     when any exist, restored from the snapshot, hashed against the manifest,
     dump fully read)
  -> promote exactly that snapshot to neev-daily
  -> bounded local staging cleanup (only outputs whose snapshot is verified)
  -> retention: forget --host neev-native --tag neev-daily 7/4/3 --prune
A failure at any step stops the run: nothing is promoted, pruned or cleaned up,
and an unverified snapshot stays tagged neev-pending for the operator.

It never touches the WorkOS repository: the config refuses that repository, and
retention is scoped to this host and tag inside the separate repository.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import time

OUTPUT = re.compile(r"^neev-content-\d{8}T\d{6}Z$")
SNAPSHOT = re.compile(r"^[0-9a-f]{64}$")
UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
FORMATS = ("neev-content-copy-v2", "neev-content-copy-v3")
# Authoritative schema-22 CHECK for both knowledge tables (Task 1 DDL).
KNOWLEDGE_MAX_BYTES = 65536
DAY = 86400
REQUIRED = {"repository", "password_file", "ssh_config", "ssh_host", "cache_dir", "state_dir",
            "staging_dir", "runner", "node", "backup_script", "app_container", "source", "host",
            "min_free_bytes"}
OPTIONAL = {"broker_socket", "keep_local_verified", "keep", "forbidden_repositories", "restic",
            "incomplete_max_age_seconds", "knowledge_source"}


class BackupError(Exception):
    pass


def load_config(path):
    info = os.lstat(path)
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise BackupError("config must be private to the running user")
    config = json.loads(Path(path).read_text())
    if not REQUIRED <= config.keys() <= REQUIRED | OPTIONAL:
        raise BackupError(f"config keys must be {sorted(REQUIRED)} plus optional {sorted(OPTIONAL)}")
    if not config["repository"].startswith("sftp:") and not config["repository"].startswith("/"):
        raise BackupError("repository must be sftp: or a local absolute path (tests)")
    for other in config.get("forbidden_repositories", []):
        if config["repository"].rstrip("/") == other.rstrip("/"):
            raise BackupError("refusing to use another job's repository")
    config.setdefault("keep", {"daily": 7, "weekly": 4, "monthly": 3})
    config.setdefault("keep_local_verified", 1)
    config.setdefault("incomplete_max_age_seconds", DAY)
    config.setdefault("restic", "/usr/bin/restic")
    if config["keep_local_verified"] < 1:
        raise BackupError("keep_local_verified must be >= 1")
    knowledge = config.get("knowledge_source")
    if knowledge is not None and (not isinstance(knowledge, str) or not os.path.isabs(knowledge)
                                  or os.path.normpath(knowledge) != knowledge):
        raise BackupError("knowledge_source must be an absolute normalised path")
    return config


def restic(config, args):
    command = [config["restic"], "--repo", config["repository"], "--password-file", config["password_file"],
               "--cache-dir", config["cache_dir"], "--limit-upload", "10240"]
    if config["repository"].startswith("sftp:"):
        command += ["-o", f"sftp.command=ssh -F {config['ssh_config']} {config['ssh_host']} -s sftp"]
    return [*command, *args]


def retention_args(config):
    keep = config["keep"]
    # Scoped: only this host's promoted snapshots. Pending (unverified) and any
    # other tag or host are never candidates.
    return ["forget", "--host", config["host"], "--tag", "neev-daily", "--group-by", "host,tags",
            "--keep-daily", str(keep["daily"]), "--keep-weekly", str(keep["weekly"]),
            "--keep-monthly", str(keep["monthly"]), "--prune"]


def _call(run, command, **kwargs):
    try:
        return run(command, check=True, **kwargs)
    except subprocess.CalledProcessError as error:
        raise BackupError(f"{Path(command[0]).name} {command_label(command)} failed (exit {error.returncode})") from None
    except OSError as error:
        raise BackupError(f"{Path(command[0]).name} could not be started: {error.strerror}") from None


def command_label(command):
    for word in command[1:]:
        if word in ("backup", "check", "restore", "tag", "forget", "snapshots", "init", "cat"):
            return word
    return ""


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_bytes(path):
    total = 0
    for directory, _, files in os.walk(path):
        for name in files:
            total += os.lstat(os.path.join(directory, name)).st_size
    return total


def preflight_disk(config):
    """Refuse to start unless the staging filesystem can hold a new backup,
    its restore canary and the configured free-space floor."""
    estimate = tree_bytes(config["source"])
    if config.get("knowledge_source"):
        estimate += tree_bytes(config["knowledge_source"])
    latest = newest_complete(config)
    if latest:
        estimate += os.path.getsize(os.path.join(latest, "metadata.dump"))
    else:
        estimate += 256 << 20
    stat = os.statvfs(config["staging_dir"])
    free = stat.f_bavail * stat.f_frsize
    needed = config["min_free_bytes"] + 2 * estimate
    if free < needed:
        raise BackupError(f"insufficient free disk: {free} bytes free, {needed} needed")
    return {"free_bytes": free, "needed_bytes": needed, "estimate_bytes": estimate}


def outputs(config):
    staging = config["staging_dir"]
    found = []
    for name in os.listdir(staging):
        path = os.path.join(staging, name)
        if OUTPUT.match(name) and os.path.isdir(path) and not os.path.islink(path):
            found.append(path)
    return sorted(found)


def newest_complete(config):
    complete = [p for p in outputs(config) if os.path.isfile(os.path.join(p, "complete.json"))]
    return complete[-1] if complete else None


def run_content_backup(config, run):
    env = {"PATH": "/usr/bin:/bin", "NEEV_BACKUP_APP_CONTAINER": config["app_container"],
           "NEEV_BACKUP_SOURCE": config["source"], "NEEV_BACKUP_OUTPUT_PARENT": config["staging_dir"],
           "NEEV_BACKUP_NODE": config["node"], "NEEV_BACKUP_SCRIPT": config["backup_script"],
           "NEEV_ORIGINALS_BROKER_SOCKET": config.get("broker_socket", "/nonexistent")}
    if config.get("knowledge_source"):
        # Environment only, like every other runner input.
        env["NEEV_BACKUP_KNOWLEDGE_SOURCE"] = config["knowledge_source"]
    result = _call(run, ["/usr/bin/bash", config["runner"]], env=env, stdout=subprocess.PIPE, text=True)
    lines = [line for line in result.stdout.splitlines() if line.startswith("complete: ")]
    if len(lines) != 1:
        raise BackupError("content backup did not report exactly one complete output")
    output = lines[0][len("complete: "):].strip()
    if os.path.dirname(output) != config["staging_dir"] or not OUTPUT.match(os.path.basename(output)):
        raise BackupError("content backup output is outside the staging directory")
    return output


def knowledge_blobs(manifest):
    """The v3 knowledge entries, validated; [] for v2 or "not configured"."""
    if manifest.get("format") != "neev-content-copy-v3":
        return []
    section = manifest.get("knowledge")
    if section == "not configured":
        return []
    if not isinstance(section, dict) or not isinstance(section.get("tables"), list) \
            or not isinstance(section.get("blobs"), list):
        raise BackupError("unexpected knowledge section in the manifest")
    seen = set()
    for blob in section["blobs"]:
        if not isinstance(blob, dict) or type(blob.get("org_id")) is not int or blob["org_id"] < 1 \
                or not isinstance(blob.get("key"), str) or not UUID.match(blob["key"]) \
                or not isinstance(blob.get("sha256"), str) or not SNAPSHOT.match(blob["sha256"]) \
                or type(blob.get("byte_length")) is not int \
                or not 0 <= blob["byte_length"] <= KNOWLEDGE_MAX_BYTES:
            raise BackupError("unexpected knowledge blob entry in the manifest")
        identity = (blob["org_id"], blob["key"])
        if identity in seen:
            raise BackupError("duplicate knowledge blob entry in the manifest")
        seen.add(identity)
    return section["blobs"]


def knowledge_path(blob):
    return os.path.join("blobs-knowledge", str(blob["org_id"]), f"{blob['key']}.md")


def verify_local(output, run, knowledge_expected=False):
    manifest = json.loads(Path(output, "complete.json").read_text())
    if manifest.get("format") not in FORMATS or not manifest.get("blobs"):
        raise BackupError("unexpected manifest")
    knowledge = knowledge_blobs(manifest)
    if knowledge_expected and not isinstance(manifest.get("knowledge"), dict):
        # The dump may index knowledge rows; an output without the root would
        # not restore them.
        raise BackupError("content backup output lacks the configured knowledge root")
    dump = os.path.join(output, "metadata.dump")
    if sha256_file(dump) != manifest["dump"]["sha256"] or os.path.getsize(dump) != manifest["dump"]["byte_length"]:
        raise BackupError("dump does not match manifest")
    for blob in manifest["blobs"]:
        path = os.path.join(output, "blobs", str(blob["org_id"]), f"{blob['key']}.{blob['kind']}")
        if sha256_file(path) != blob["sha256"] or os.path.getsize(path) != blob["byte_length"]:
            raise BackupError("a copied blob does not match the manifest")
    for blob in knowledge:
        path = os.path.join(output, knowledge_path(blob))
        if sha256_file(path) != blob["sha256"] or os.path.getsize(path) != blob["byte_length"]:
            raise BackupError("a copied knowledge blob does not match the manifest")
    # Read the complete archive without a database and without printing it.
    _call(run, ["/usr/bin/pg_restore", "--file=/dev/null", dump], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return manifest


def upload(config, output, run):
    result = _call(run, restic(config, ["backup", "--host", config["host"], "--tag", "neev-pending",
                                        "--group-by", "host,tags", "--json", "--", output]),
                   stdout=subprocess.PIPE, text=True)
    ids = []
    for line in result.stdout.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("message_type") == "summary":
            ids.append(row.get("snapshot_id"))
    if len(ids) != 1 or not isinstance(ids[0], str) or not SNAPSHOT.match(ids[0]):
        raise BackupError("no exact snapshot id; the upload stays unpromoted")
    return ids[0]


def restore_canary(config, snapshot, output, manifest, run, blob=None, knowledge_blob=None):
    """Restore the dump, one blob and (when any exist) one knowledge blob from
    the snapshot and compare them with the manifest; fully read the restored
    dump. Removes its scratch copy."""
    blob = blob or secrets.SystemRandom().choice(manifest["blobs"])
    relative_blob = os.path.join("blobs", str(blob["org_id"]), f"{blob['key']}.{blob['kind']}")
    knowledge = knowledge_blobs(manifest)
    if knowledge and knowledge_blob is None:
        knowledge_blob = secrets.SystemRandom().choice(knowledge)
    relative_knowledge = knowledge_path(knowledge_blob) if knowledge_blob else None
    scratch = os.path.join(config["staging_dir"], f".canary-{snapshot[:12]}-{secrets.token_hex(4)}")
    os.mkdir(scratch, 0o700)
    try:
        includes = []
        for relative in ("complete.json", "metadata.dump", relative_blob, relative_knowledge):
            if relative:
                includes += ["--include", os.path.join(output, relative)]
        _call(run, restic(config, ["restore", snapshot, "--target", scratch, *includes, "--verify"]),
              stdout=subprocess.DEVNULL)
        restored = scratch + output
        try:
            same = json.loads(Path(restored, "complete.json").read_text()) == manifest
        except (OSError, ValueError):
            same = False
        if not same:
            raise BackupError("restored manifest differs")
        if sha256_file(os.path.join(restored, "metadata.dump")) != manifest["dump"]["sha256"]:
            raise BackupError("restored dump hash differs")
        if sha256_file(os.path.join(restored, relative_blob)) != blob["sha256"]:
            raise BackupError("restored blob hash differs")
        if relative_knowledge:
            restored_knowledge = os.path.join(restored, relative_knowledge)
            if os.path.getsize(restored_knowledge) != knowledge_blob["byte_length"]:
                raise BackupError("restored knowledge blob size differs")
            if sha256_file(restored_knowledge) != knowledge_blob["sha256"]:
                raise BackupError("restored knowledge blob hash differs")
        _call(run, ["/usr/bin/pg_restore", "--file=/dev/null", os.path.join(restored, "metadata.dump")],
              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        result = {"blob_kind": blob["kind"], "blob_bytes": blob["byte_length"],
                  "dump_bytes": manifest["dump"]["byte_length"]}
        if knowledge_blob:
            result["knowledge_bytes"] = knowledge_blob["byte_length"]
        return result
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def promoted_id(config, uploaded, run):
    result = _call(run, restic(config, ["snapshots", "--json", "--host", config["host"], "--tag", "neev-daily"]),
                   stdout=subprocess.PIPE, text=True)
    try:
        snapshots = json.loads(result.stdout or "[]")
    except ValueError:
        raise BackupError("unreadable snapshot list after promotion") from None
    matches = [s["id"] for s in snapshots if uploaded in (s.get("id"), s.get("original"))]
    if len(matches) != 1 or not SNAPSHOT.match(matches[0]):
        raise BackupError("promotion not confirmed in the repository")
    return matches[0]


def load_state(config):
    path = os.path.join(config["state_dir"], "state.json")
    return json.loads(Path(path).read_text()) if os.path.exists(path) else {"verified_outputs": {}}


def save_json(path, payload):
    temporary = f"{path}.tmp-{secrets.token_hex(4)}"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=1, sort_keys=True)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def cleanup(config, state, now):
    """Delete only runner outputs in the staging directory: complete outputs
    whose snapshot was verified offsite, beyond the newest keep_local_verified;
    and incomplete (failed) outputs older than incomplete_max_age_seconds.
    Complete outputs without a verified snapshot are never deleted."""
    verified = state["verified_outputs"]
    removed = []
    complete_verified = [p for p in outputs(config)
                         if os.path.isfile(os.path.join(p, "complete.json")) and os.path.basename(p) in verified]
    for path in complete_verified[:-config["keep_local_verified"]]:
        shutil.rmtree(path)
        removed.append(os.path.basename(path))
    for path in outputs(config):
        if not os.path.isfile(os.path.join(path, "complete.json")) and \
                now - os.lstat(path).st_mtime > config["incomplete_max_age_seconds"]:
            shutil.rmtree(path)
            removed.append(os.path.basename(path))
    return removed


def backup(config, run=subprocess.run, now=None, log=print):
    now = time.time() if now is None else now
    state_dir = config["state_dir"]
    record = {"started_at": int(now), "ok": False}

    def stage(name):
        # One line per stage so `journalctl -u neev-backup` shows progress.
        record["stage"] = name
        log(f"stage: {name}")

    try:
        stage("preflight")
        record["disk"] = preflight_disk(config)
        stage("content-backup")
        output = run_content_backup(config, run)
        record["output"] = os.path.basename(output)
        stage("verify-local")
        manifest = verify_local(output, run, knowledge_expected=bool(config.get("knowledge_source")))
        knowledge = knowledge_blobs(manifest)
        record.update(blobs=len(manifest["blobs"]), blob_bytes=sum(b["byte_length"] for b in manifest["blobs"]),
                      dump_bytes=manifest["dump"]["byte_length"], knowledge_blobs=len(knowledge),
                      knowledge_bytes=sum(b["byte_length"] for b in knowledge))
        stage("upload")
        snapshot = upload(config, output, run)
        record["snapshot"] = snapshot
        stage("check")
        _call(run, restic(config, ["check"]), stdout=subprocess.DEVNULL)
        stage("restore-canary")
        record["canary"] = restore_canary(config, snapshot, output, manifest, run)
        stage("promote")
        _call(run, restic(config, ["tag", "--remove", "neev-pending", "--add", "neev-daily", snapshot]),
              stdout=subprocess.DEVNULL)
        # `restic tag` rewrites the snapshot under a new id (keeping the old as
        # "original"): confirm promotion by finding it, and record the new id.
        record["promoted_snapshot"] = promoted_id(config, snapshot, run)
        record["promoted"] = True
        state = load_state(config)
        state["verified_outputs"][os.path.basename(output)] = {"snapshot": record["promoted_snapshot"],
                                                               "uploaded_snapshot": snapshot, "verified_at": int(now)}
        stage("cleanup")
        record["removed_local"] = cleanup(config, state, now)
        state["verified_outputs"] = {name: meta for name, meta in state["verified_outputs"].items()
                                     if os.path.isdir(os.path.join(config["staging_dir"], name))}
        save_json(os.path.join(state_dir, "state.json"), state)
        stage("retention-prune")
        _call(run, restic(config, retention_args(config)), stdout=subprocess.DEVNULL)
        record["retention"] = "applied"
        stage("done")
        record["ok"] = True
        save_json(os.path.join(state_dir, "last-success.json"), record)
        log(json.dumps(record))
        return record
    except BaseException as error:
        record["failure"] = str(error) if isinstance(error, BackupError) else type(error).__name__
        save_json(os.path.join(state_dir, "last-run.json"), record)
        raise
    finally:
        if record["ok"]:
            save_json(os.path.join(state_dir, "last-run.json"), record)


def _log(message):
    print(message, flush=True)


def main(argv=None, run=subprocess.run):
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", default="/etc/neev-backup/config.json")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("backup")
    commands.add_parser("init")
    commands.add_parser("status")
    ret = commands.add_parser("retention")
    ret.add_argument("--dry-run", action="store_true")
    rs = commands.add_parser("restic")
    rs.add_argument("args", nargs=argparse.REMAINDER)
    options = parser.parse_args(argv)
    try:
        config = load_config(options.config)
        for directory in (config["state_dir"], config["cache_dir"], config["staging_dir"]):
            if not os.path.isdir(directory):
                raise BackupError(f"missing directory {directory}")
        if options.command == "restic":
            return run(restic(config, options.args)).returncode
        with open(os.path.join(config["state_dir"], "lock"), "a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise BackupError("another neev_backup run holds the lock") from None
            if options.command == "backup":
                backup(config, run=run, log=_log)
            elif options.command == "init":
                # Explicit only: an existing repository is never re-initialised.
                exists = run(restic(config, ["cat", "config"]), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if exists.returncode == 0:
                    raise BackupError("repository already initialised")
                _call(run, restic(config, ["init"]), stdout=subprocess.DEVNULL)
                print("repository initialised")
            elif options.command == "retention":
                args = retention_args(config) + (["--dry-run"] if options.dry_run else [])
                _call(run, restic(config, args))
            else:
                for name in ("last-run.json", "last-success.json", "state.json"):
                    path = os.path.join(config["state_dir"], name)
                    print(name, Path(path).read_text().strip() if os.path.exists(path) else "absent")
        return 0
    except (BackupError, OSError, ValueError) as error:
        print(f"neev_backup: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
