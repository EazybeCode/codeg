#!/usr/bin/env bash
#
# Integration acceptance for the Neev document engine.
#
#   ./run.sh [--json] [--only=<substring>]
#
# Runs against a canary over loopback. It installs nothing: the collaboration
# client is the one the app ships, borrowed from the pinned upstream build tree,
# and the run aborts if the versions are not exactly right.
#
# Where it expects things:
#   NEEV_ACCEPTANCE_BASE     default http://127.0.0.1:3100
#   NEEV_ACCEPTANCE_DIR      default /opt/neev-docs-canary   (actors.json, fixtures.json)
#   NEEV_ACCEPTANCE_MODULES  default /tmp/neev-lasuite-build/work/node_modules
#
# Running it from a workstation instead of the canary host is fine — point
# NEEV_ACCEPTANCE_BASE at an SSH tunnel to 3100. The actor files stay on the
# host and stay private either way; nothing here copies or prints them.
#
#   ssh -L 3100:127.0.0.1:3100 <host>
#
# One flag worth knowing: NEEV_ACCEPTANCE_ENGINE_STOPPED=1 tells the harness the
# engine is deliberately down, which enables the unavailability case. Without it
# that case skips rather than inventing a result.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODULES="${NEEV_ACCEPTANCE_MODULES:-/tmp/neev-lasuite-build/work/node_modules}"

if [ ! -d "$MODULES/@hocuspocus/provider" ]; then
  cat >&2 <<EOF
The pinned collaboration client is not at:
  $MODULES

It comes from the upstream build tree, which deploy/lasuite/frontend/build.sh
creates. Point NEEV_ACCEPTANCE_MODULES at an existing one rather than installing
a second copy — a provider at a different version would make every convergence
result a statement about a client nobody runs.
EOF
  exit 2
fi

export PATH="/opt/homebrew/opt/node@22/bin:$PATH"
export NEEV_ACCEPTANCE_MODULES="$MODULES"

node --version >/dev/null 2>&1 || { echo "node 22 not on PATH" >&2; exit 2; }

exec node "$HERE/run.mjs" "$@"
