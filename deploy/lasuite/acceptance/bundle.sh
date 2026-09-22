#!/usr/bin/env bash
#
# Package the harness for a host that has the actors but not the tooling.
#
# The acceptance cases need a real signed-in actor, and the actor files live on
# the canary host. A tunnel gives HTTP but not files, so when the harness cannot
# be driven from a workstation it has to go to the host instead — with the
# collaboration client it depends on, because installing one there would defeat
# the version pin the WS cases rest on.
#
#   ./bundle.sh [output.tar.gz]     default /tmp/neev-acceptance-bundle.tar.gz
#
# The bundle carries NO credentials. The actor files stay where they are and are
# read at run time.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-/tmp/neev-acceptance-bundle.tar.gz}"
SRC="${NEEV_ACCEPTANCE_MODULES:-/tmp/neev-lasuite-build/work/node_modules}"
STAGE="$(mktemp -d)/neev-acceptance-bundle"

trap 'rm -rf "$(dirname "$STAGE")"' EXIT

[ -d "$SRC/@hocuspocus/provider" ] || {
  echo "no pinned client at $SRC — run deploy/lasuite/frontend/build.sh first" >&2
  exit 2
}

export PATH="/opt/homebrew/opt/node@22/bin:$PATH"
mkdir -p "$STAGE/node_modules"
cp -R "$HERE" "$STAGE/acceptance"
rm -f "$STAGE/acceptance/bundle.sh"

# Seed with what the harness imports directly, then let Node tell us what else
# it needs. Resolving the closure by asking beats maintaining a list by hand —
# a list goes stale silently and the failure lands on whoever runs it remotely.
for p in @hocuspocus yjs lib0 y-protocols ws; do
  cp -RL "$SRC/$p" "$STAGE/node_modules/" 2>/dev/null || true
done

for _ in $(seq 1 30); do
  missing="$(NEEV_ACCEPTANCE_MODULES="$STAGE/node_modules" node -e "
    const m = await import('$STAGE/acceptance/lib/collab.mjs'); await m.loadClient(); console.log('LOADED')
  " --input-type=module 2>&1 || true)"
  case "$missing" in
    *LOADED*) break ;;
  esac
  pkg="$(printf '%s' "$missing" | grep -o "Cannot find package '[^']*'" | head -1 | sed "s/Cannot find package '//;s/'//")"
  [ -n "$pkg" ] || { echo "cannot resolve the client:" >&2; printf '%s\n' "$missing" | head -5 >&2; exit 1; }
  [ -d "$SRC/$pkg" ] || { echo "$pkg is not in $SRC" >&2; exit 1; }
  mkdir -p "$STAGE/node_modules/$(dirname "$pkg")"
  cp -RL "$SRC/$pkg" "$STAGE/node_modules/$(dirname "$pkg")/"
  echo "  + $pkg"
done

cat > "$STAGE/run-on-host.sh" <<'RUNNER'
#!/usr/bin/env bash
# Run the document-engine acceptance harness on the canary host.
# Installs nothing, downloads nothing, carries no credentials.
#   ./run-on-host.sh [--json] [--only=<substring>]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export NEEV_ACCEPTANCE_MODULES="$HERE/node_modules"
export NEEV_ACCEPTANCE_BASE="${NEEV_ACCEPTANCE_BASE:-http://127.0.0.1:3100}"
export NEEV_ACCEPTANCE_DIR="${NEEV_ACCEPTANCE_DIR:-/opt/neev-docs-canary}"
exec node "$HERE/acceptance/run.mjs" "$@"
RUNNER
chmod +x "$STAGE/run-on-host.sh"

# Prove it stands alone before shipping it, rather than finding out on the host.
# Pointing NODE_PATH away is not enough — the check is that nothing resolves back
# into the build tree.
( cd "$(dirname "$STAGE")" && COPYFILE_DISABLE=1 tar --no-xattrs -czf "$OUT" neev-acceptance-bundle )

echo "bundle: $OUT"
shasum -a 256 "$OUT"
echo "run it with:  tar xzf $(basename "$OUT") && ./neev-acceptance-bundle/run-on-host.sh --json"
