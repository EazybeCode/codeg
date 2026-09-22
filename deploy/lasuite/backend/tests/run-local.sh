#!/usr/bin/env bash
#
# Local half of the bridge tests: identity parsing, session behaviour and
# settings wiring, against real Django 5.2.17 (the version pinned by upstream's
# uv.lock at v5.7.0) and a User model mirroring `core.models.User`.
#
# The other half — provisioning, ACL projection and the real settings module —
# needs `core` and therefore the image: see tests-container/run.sh.
#
#   ./tests/run-local.sh              # uses/creates .venv-tests
#   VENV=/path/to/venv ./tests/run-local.sh
#
set -euo pipefail

cd "$(dirname "$0")/.."

VENV="${VENV:-.venv-tests}"
PYTHON="${PYTHON:-python3.11}"

if [[ ! -x "${VENV}/bin/python" ]]; then
  echo "creating ${VENV} with django==5.2.17 (upstream's pin)"
  "${PYTHON}" -m venv "${VENV}"
  "${VENV}/bin/pip" --quiet install "django==5.2.17"
fi

exec "${VENV}/bin/python" -m django test tests \
  --settings=tests.settings_local \
  --verbosity "${VERBOSITY:-2}" \
  "$@"
