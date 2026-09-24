#!/usr/bin/env bash
# The whole companion suite. Offline, no provider installed, no network.
set -euo pipefail
cd "$(dirname "$0")"
exec python3 -m unittest discover -s tests -t . "$@"
