#!/usr/bin/env bash
# Deterministic tests for the extension half: protocol shapes, the scope
# guard, the run controller, and the page agent against a hand-built DOM.
#
# No browser, no network, no installed extension, no provider. Node's built-in
# test runner only — there is no dependency to install.
set -euo pipefail
cd "$(dirname "$0")/.."

for file in extension/*.js extension/lib/*.js extension/content/*.js; do
  node --check "$file"
done

exec node --test extension-tests/
