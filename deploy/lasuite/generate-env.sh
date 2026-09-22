#!/usr/bin/env bash
# Generate the document engine's env.d files and install the stack on a host.
#
# Every secret is created here, on the host. Nothing secret is committed, and
# nothing secret is printed: this script reports file names and modes only.
#
#   ./generate-env.sh                         production, /opt/neev-document-engine
#   ./generate-env.sh --canary [TARGET]       loopback canary (INSECURE cookies)
#   ./generate-env.sh [TARGET] [ORIGIN]
#
# It refuses to overwrite an existing env.d, so a running stack cannot be
# silently re-keyed — re-keying would invalidate every live session and every
# bridge call at once.

set -euo pipefail

# Everything is created private by default; the few files a container must read
# are then opened up explicitly and individually.
umask 077

mode="production"
if [ "${1:-}" = "--canary" ]; then
  mode="canary"
  shift
fi

if [ "$mode" = "canary" ]; then
  target="${1:-/opt/neev-document-engine-canary}"
  origin="${2:-http://127.0.0.1:3100}"
  configuration="NeevCanary"
  # Comma-separated, NOT JSON. django-configurations' ListValue splits on ",",
  # so a JSON array is stored verbatim as broken strings — verified against the
  # real image, where '["a","b"]' became ['["a"', '"b"]'] and every CSRF origin
  # check failed. The container tests assert the parsed form.
  allowed_hosts="127.0.0.1,localhost,backend"
else
  target="${1:-/opt/neev-document-engine}"
  origin="${2:-https://app.neev.so}"
  configuration="NeevProduction"
  # Comma-separated, NOT JSON — see the canary branch above for why.
  allowed_hosts="$(printf '%s' "$origin" | sed -e 's|^https\?://||' -e 's|/.*$||'),backend"
fi

here="$(cd "$(dirname "$0")" && pwd)"

if [ -d "$target/env.d" ]; then
  echo "refusing to overwrite $target/env.d — remove it first if you mean to re-key"
  exit 1
fi

# No pipeline: `tr -dc ... </dev/urandom | head -c N` dies on SIGPIPE under
# `set -o pipefail`. Draw a finite amount and slice it in the shell instead.
secret() {
  local want="${1:-32}" raw=""
  if command -v python3 >/dev/null 2>&1; then
    raw="$(python3 -c 'import secrets; print(secrets.token_hex(64))')"
  elif command -v openssl >/dev/null 2>&1; then
    raw="$(openssl rand -hex 64)"
  else
    echo "need python3 or openssl to generate secrets" >&2
    exit 1
  fi
  if [ "${#raw}" -lt "$want" ]; then
    echo "generated material too short — refusing to emit a weak secret" >&2
    exit 1
  fi
  printf '%s' "${raw:0:want}"
}

mkdir -p "$target/env.d"

db_password="$(secret 32)"
minio_password="$(secret 32)"
collab_secret="$(secret 32)"
yprovider_key="$(secret 32)"
django_secret="$(secret 50)"
# The bridge credential. Refused by the endpoints below 32 characters; 64 here.
bridge_key="$(secret 64)"

cat >"$target/env.d/common" <<EOF
S3_HOST=minio:9000
BACKEND_HOST=backend
YPROVIDER_HOST=y-provider
BUCKET_NAME=docs-media-storage
EOF

cat >"$target/env.d/backend" <<EOF
# The settings seam: upstream's Production, subclassed. See
# backend/neev_bridge/settings.py for what changes and what deliberately does not.
DJANGO_CONFIGURATION=$configuration
DJANGO_SETTINGS_MODULE=neev_bridge.settings
DJANGO_SECRET_KEY=$django_secret
DJANGO_ALLOWED_HOSTS=$allowed_hosts
DJANGO_CSRF_TRUSTED_ORIGINS=$origin
DJANGO_EMAIL_BACKEND=django.core.mail.backends.console.EmailBackend

# Shared credential for the internal /_neev/ endpoints. Reachable only on the
# private compose network; the proxy strips this header from client requests.
NEEV_BRIDGE_KEY=$bridge_key

REDIS_URL=redis://redis:6379/1
AWS_S3_ENDPOINT_URL=http://minio:9000
AWS_S3_ACCESS_KEY_ID=engine
AWS_S3_SECRET_ACCESS_KEY=$minio_password
AWS_STORAGE_BUCKET_NAME=docs-media-storage

# Deliberately NOT set, and checked at startup:
#   MEDIA_BASE_URL      the frontend's backendUrl() already carries
#                       /document-engine; setting it would double the prefix
#   COLLABORATION_WS_URL  the frontend derives the socket URL same-origin
EOF

if [ "$mode" = "canary" ]; then
  cat >>"$target/env.d/backend" <<'EOF'

# Canary only. NeevCanary refuses to start without this, and it turns OFF
# Secure cookies because the canary is reached over plain HTTP on loopback.
# It must never appear in a production env file.
NEEV_CANARY=1
EOF
fi

cat >"$target/env.d/postgresql" <<EOF
POSTGRES_DB=docs
POSTGRES_USER=docs
POSTGRES_PASSWORD=$db_password
DB_HOST=postgresql
DB_NAME=docs
DB_USER=docs
DB_PASSWORD=$db_password
DB_PORT=5432
EOF

cat >"$target/env.d/minio" <<EOF
MINIO_ROOT_USER=engine
MINIO_ROOT_PASSWORD=$minio_password
BUCKET_NAME=docs-media-storage
EOF

cat >"$target/env.d/yprovider" <<EOF
COLLABORATION_BACKEND_BASE_URL=http://backend:8000
COLLABORATION_SERVER_ORIGIN=$origin
COLLABORATION_SERVER_SECRET=$collab_secret
COLLABORATION_API_URL=http://y-provider:4444/api/
Y_PROVIDER_API_KEY=$yprovider_key
PORT=4444
EOF

# The name of the existing network the Neev container is on. compose resolves it
# at parse time, so it lives in .env rather than the environment of whoever runs
# `docker compose`.
if [ "$mode" = "canary" ]; then
  neev_network="${NEEV_NETWORK:-neev-docs-native-test}"
else
  neev_network="${NEEV_NETWORK:-neev}"
fi
cat >"$target/.env" <<EOF
NEEV_NETWORK=$neev_network
EOF

# The compose file and the settings package a container must read.
install -m 644 "$here/compose.yaml" "$target/compose.yaml"
install -d -m 755 "$target/backend" "$target/backend/neev_bridge"
install -m 644 "$here"/backend/neev_bridge/*.py "$target/backend/neev_bridge/"

# env.d is read by the docker daemon as root, never by a container, so it stays
# closed.
chmod 644 "$target/.env"
chmod 700 "$target/env.d"
chmod 600 "$target"/env.d/*

echo "written to $target  (mode: $mode, origin: $origin)"
echo "  env.d/{common,backend,postgresql,minio,yprovider}"
echo "    directory 700, files 600 — root only, never read by a container"
echo "  compose.yaml (644), backend/neev_bridge/*.py (644 in a 755 directory)"
echo
echo "next — dependencies first, then migrate, then the rest:"
echo "  cd $target"
echo "  docker compose up -d --wait --wait-timeout 120 postgresql redis minio"
echo "  docker compose run --rm backend python manage.py migrate"
echo "  docker compose up -d --wait --wait-timeout 180"
echo
echo "the backend's healthcheck is 'manage.py check', which needs a migrated"
echo "schema, so bringing the whole stack up first can time out as unhealthy."
echo
echo "backup (see README.md for restore, and test a restore before you need one):"
echo "  docker compose exec -T postgresql pg_dump -U docs -Fc docs > docs-db.dump"
echo "  docker compose run --rm -v \"\$PWD:/backup\" createbuckets sh -c \\"
echo "    'mc alias set engine http://minio:9000 \$MINIO_ROOT_USER \$MINIO_ROOT_PASSWORD &&"
echo "     mc mirror --overwrite engine/docs-media-storage /backup/docs-media'"
