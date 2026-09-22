#!/usr/bin/env bash
# Run the in-image bridge tests against the pinned Docs backend.
#
# Must be run from a stack directory produced by generate-env.sh (it needs
# env.d/ and a running postgresql + redis). Django's test runner creates and
# destroys its own `test_docs` database, so the stack's own data is untouched.
#
#   cd /opt/neev-document-engine-canary
#   /path/to/backend/tests-container/run.sh
#
# The test package is mounted read-only beside the bridge package; nothing is
# added to the image.

set -euo pipefail

tests_dir="$(cd "$(dirname "$0")" && pwd)"

exec docker compose run --rm --no-deps \
  -v "${tests_dir}:/app/neev_selftest:ro" \
  -e DJANGO_SETTINGS_MODULE=neev_bridge.settings \
  backend python manage.py test neev_selftest --verbosity "${VERBOSITY:-2}" "$@"
