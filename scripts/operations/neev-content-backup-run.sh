#!/usr/bin/env bash
# Complete Neev content backup: a PostgreSQL snapshot plus every original it
# references, verified by hash. Originals evicted from the live root are
# streamed from the originals broker into the backup (never back into the live
# cache). Runs as root on the app host; the backup itself runs as the blob-store
# uid. The database URL is read from the running app container at run time and
# passed only through the environment (never argv, never stored).
#
# Output: $NEEV_BACKUP_OUTPUT_PARENT/neev-content-<UTC>/ with complete.json on
# success. A directory without complete.json is incomplete and must not be used.
set -euo pipefail

APP_CONTAINER=${NEEV_BACKUP_APP_CONTAINER:?set NEEV_BACKUP_APP_CONTAINER}
SOURCE=${NEEV_BACKUP_SOURCE:-/var/lib/neev-native/documents}
OUTPUT_PARENT=${NEEV_BACKUP_OUTPUT_PARENT:-/var/lib/neev-content-backups}
SCHEMA=${NEEV_BACKUP_SCHEMA:-public}
NODE=${NEEV_BACKUP_NODE:-/opt/neev-content-backup/node/bin/node}
SCRIPT=${NEEV_BACKUP_SCRIPT:-/opt/neev-content-backup/neev-content-backup.mjs}
BROKER=${NEEV_ORIGINALS_BROKER_SOCKET:-/run/neev-originals/broker.sock}
APP_UID=${NEEV_BACKUP_UID:-65532}

[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
[ -x "$NODE" ] && [ -f "$SCRIPT" ] || { echo "node or backup script missing" >&2; exit 1; }
url=$(docker exec "$APP_CONTAINER" printenv NEEV_DATABASE_URL) || { echo "cannot read app database URL" >&2; exit 1; }
[ -n "$url" ] || { echo "empty app database URL" >&2; exit 1; }
broker=""
[ -S "$BROKER" ] && broker=$BROKER
output="$OUTPUT_PARENT/neev-content-$(date -u +%Y%m%dT%H%M%SZ)"

cd /
(
  # Clean environment: only what the backup needs. The URL never appears in argv.
  for name in $(compgen -e); do unset "$name" 2>/dev/null || true; done
  export PATH=/usr/bin:/bin HOME=/nonexistent LANG=C.UTF-8 NEEV_BACKUP_DATABASE_URL="$url"
  [ -n "$broker" ] && export NEEV_ORIGINALS_BROKER_SOCKET="$broker"
  exec setpriv --reuid="$APP_UID" --regid="$APP_UID" --clear-groups \
    "$NODE" "$SCRIPT" --source "$SOURCE" --output "$output" --schema "$SCHEMA"
)
test -f "$output/complete.json"
echo "complete: $output"
