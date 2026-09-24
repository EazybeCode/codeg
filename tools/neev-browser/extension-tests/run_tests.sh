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

# Name the test files rather than the directory. Node 22 treats a positional
# path as a module to load and dies with MODULE_NOT_FOUND on a directory,
# where Node 18 searched it. The glob also keeps `helpers/` out, which is
# fixtures, not tests.
shopt -s nullglob
tests=(extension-tests/*.test.mjs)
if [ ${#tests[@]} -eq 0 ]; then
  echo "no test files matched extension-tests/*.test.mjs" >&2
  exit 1
fi

exec node --test "${tests[@]}"
