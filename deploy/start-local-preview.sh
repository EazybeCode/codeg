#!/bin/bash
set -euo pipefail

# Launch only; never stop another server, copy a database, or run migrations
# against an implicit/default database. Credentials are never printed.
: "${WORKOS_OPERATOR_ENV:?Set the existing operator environment file}"
: "${WORKOS_PREVIEW_DATABASE_URL:?Set an isolated preview database URL}"
: "${WORKOS_PREVIEW_DATA_DIR:?Set an absolute durable preview directory}"
: "${WORKOS_PREVIEW_TOKEN:?Set a localhost installation token}"
case "$WORKOS_PREVIEW_DATA_DIR" in /*) ;; *) echo "Preview data directory must be absolute" >&2; exit 1;; esac
case "$WORKOS_PREVIEW_DATABASE_URL" in *workos_local*) echo "Refusing the original database" >&2; exit 1;; esac
preview_source="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
preview_database="$WORKOS_PREVIEW_DATABASE_URL"
preview_directory="$WORKOS_PREVIEW_DATA_DIR"
preview_token="$WORKOS_PREVIEW_TOKEN"
preview_port="${WORKOS_PREVIEW_PORT:-3095}"
set -a
source "$WORKOS_OPERATOR_ENV"
set +a
export DATABASE_URL="$preview_database"
export CODEG_DATA_DIR="$preview_directory" CODEG_HOME="$preview_directory"
export CODEG_STATIC_DIR="$preview_source/out"
export CODEG_HOST=127.0.0.1 CODEG_PORT="$preview_port" CODEG_TOKEN="$preview_token"
export WORKOS_APP_ORIGIN="http://localhost:$preview_port"
export GITHUB_CALLBACK_URL="$WORKOS_APP_ORIGIN/auth/github/callback"
export WORKOS_ENABLE_CATALOG_RUNTIME=true
test -x "$preview_source/src-tauri/target/release/codeg-server"
test -f "$CODEG_STATIC_DIR/workspace.html"
cd "$preview_source"
exec src-tauri/target/release/codeg-server
