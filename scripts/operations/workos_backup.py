#!/usr/bin/env python3
"""Root-only off-server transport for completed Coolify archives, not live volumes."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


def select_sources(config, now):
    sources = config["sources"]
    if len(sources) != 4 or {s["label"] for s in sources} != {"postgres", "data", "claude", "codex"}:
        raise ValueError("Exactly one source for each required backup category is required")
    selected = []
    directories = set()
    identities = set()
    for source in sources:
        directory = Path(source["directory"])
        if not directory.is_absolute() or directory.resolve() != directory:
            raise ValueError("Source directories must be absolute and must not traverse symlinks")
        if directory in directories:
            raise ValueError("Backup categories must use distinct directories")
        directories.add(directory)
        pattern = source["pattern"]
        if "/" in pattern or pattern in ("", ".", ".."):
            raise ValueError("Only direct archive filenames may be selected")
        candidates = list(directory.glob(pattern))
        if not candidates or any(p.is_symlink() or not p.is_file() for p in candidates):
            raise ValueError(f"Missing or unsafe archive: {source['label']}")
        latest = max(candidates, key=lambda p: p.stat().st_mtime)
        stat = latest.stat()
        identity = (stat.st_dev, stat.st_ino)
        if identity in identities:
            raise ValueError("Backup categories must use distinct archive files")
        identities.add(identity)
        age = now - stat.st_mtime
        # At 04:30 UTC all native jobs should have finished this day's window.
        if stat.st_size == 0 or age < 60 or age > 8 * 3600:
            raise ValueError(f"Empty, unsettled, or stale archive: {source['label']}")
        selected.append(latest)
    return selected


def fingerprints(paths):
    result = []
    for path in paths:
        if path.is_symlink() or not path.is_file():
            raise ValueError("Archive changed type while preparing backup")
        stat = path.stat()
        result.append((stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns))
    return result


def validate_sources(config, sources, run):
    before = fingerprints(sources)
    for source, path in zip(config["sources"], sources):
        if source["label"] == "postgres":
            commands = [["/usr/bin/pg_restore", "--file=/dev/null", str(path)]]
        else:
            commands = [["/usr/bin/gzip", "-t", "--", str(path)], ["/usr/bin/tar", "-tzf", str(path)]]
        for command in commands:
            # Read the complete archive, but never emit its SQL/file contents.
            run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    if fingerprints(sources) != before:
        raise ValueError("Archive changed during validation; retry after native backup completes")
    return before


def restic_command(config, args):
    return [
        "/usr/bin/restic", "--repo", config["repository"],
        "--password-file", config["password_file"],
        "--cache-dir", config["cache_dir"], "--limit-upload", "10240",
        "-o", "sftp.command=ssh -F " + config["ssh_config"] + " workos-storage -s sftp",
        *args,
    ]


def run_backup(config, now, dry_run=False, run=subprocess.run):
    sources = select_sources(config, now)
    before = validate_sources(config, sources, run)
    options = ["--host", config["host"], "--tag", "workos-daily", "--group-by", "host,tags"]
    args = ["backup", "--host", config["host"], "--tag", "workos-pending", "--group-by", "host,tags", "--json"]
    if dry_run:
        args.append("--dry-run")
    result = run(restic_command(config, [*args, "--", *map(str, sources)]), check=True, stdout=subprocess.PIPE, text=True)
    if fingerprints(sources) != before:
        raise ValueError("Archive changed during upload; snapshot is unverified and retention skipped")
    if dry_run:
        print("Archive validation and Restic dry run passed")
        return
    summaries = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    ids = [row.get("snapshot_id") for row in summaries if row.get("message_type") == "summary"]
    if len(ids) != 1 or not isinstance(ids[0], str) or not re.fullmatch(r"[0-9a-f]{64}", ids[0]):
        raise ValueError("Missing exact snapshot ID; pending backup must not be promoted")
    run(restic_command(config, ["check"]), check=True)
    # Only this stable, checked snapshot may satisfy a daily retention slot.
    # Failed uploads stay quarantined for operator review, not automatic pruning.
    run(restic_command(config, ["tag", "--remove", "workos-pending", "--add", "workos-daily", ids[0]]), check=True)
    state = Path(config["state_dir"])
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    verified = state / "restore-verified"
    if verified.is_file() and verified.read_text().strip() == config["repository"]:
        run(restic_command(config, ["forget", *options, "--keep-daily", "7", "--keep-weekly", "4", "--keep-monthly", "3", "--prune"]), check=True)
    status = {"completed_at_unix": time.time(), "sources": [str(p) for p in sources]}
    pending = state / "last-success.json.tmp"
    pending.write_text(json.dumps(status) + "\n")
    pending.replace(state / "last-success.json")
    print("Verified WorkOS backup completed")


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default="/etc/workos-backup/config.json")
    parser.add_argument("action", choices=["sources", "dry-run", "backup", "restic"])
    parser.add_argument("args", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    config = json.loads(Path(options.config).read_text())
    if options.action == "sources":
        for path in select_sources(config, time.time()):
            print(path)
    elif options.action == "restic":
        subprocess.run(restic_command(config, options.args), check=True)
    else:
        run_backup(config, time.time(), dry_run=options.action == "dry-run")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"WorkOS backup failed: {error}", file=sys.stderr)
        sys.exit(1)
