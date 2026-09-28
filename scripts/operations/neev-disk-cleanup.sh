#!/usr/bin/env bash
# Reclaim rebuildable cache only. Never prune volumes, containers or tagged images.
set -euo pipefail
exec 9>/run/lock/neev-disk-cleanup.lock
flock -n 9 || exit 0
printf '\n[%s] cleanup started\n' "$(date -u +%FT%TZ)"
free_kb=$(df -Pk / | awk 'NR==2 {print $4}')
cache_age=(--filter until=24h)
if (( free_kb < 5 * 1024 * 1024 )); then
  cache_age=()
  echo 'Low disk space: reclaiming unused build cache regardless of age'
fi
for builder in default neevrel neev-release-20260910; do
  if docker buildx inspect "$builder" >/dev/null 2>&1; then
    timeout 600 docker buildx prune --builder "$builder" --all --force \
      --reserved-space 1GB --max-used-space 4GB --min-free-space 8GB \
      "${cache_age[@]}" || echo "Cache cleanup unavailable for $builder"
  fi
done
# Only untagged, unused images older than a day; retain tagged rollback releases.
timeout 300 docker image prune --force --filter until=24h
# Bound system diagnostic logs, not application/user files.
journalctl --vacuum-time=14d --vacuum-size=200M
df -h /
printf '[%s] cleanup finished\n' "$(date -u +%FT%TZ)"
