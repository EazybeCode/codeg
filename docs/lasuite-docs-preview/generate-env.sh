#!/usr/bin/env bash
# Generate the preview's env.d files and Keycloak realm.
#
# Every secret is created here, on the host. Nothing secret is committed, and
# nothing secret is printed: the script reports file names and modes only.
#
# Run from this directory. It refuses to overwrite an existing env.d, so a
# running preview cannot be silently re-keyed.

set -euo pipefail

# Everything below is created private by default; the few files a container has
# to read are then opened up explicitly and individually.
umask 077

target="${1:-/opt/neev-lasuite-preview}"
origin="http://localhost:8100"

if [ -d "$target/env.d" ]; then
  echo "refusing to overwrite $target/env.d — remove it first if you mean to re-key"
  exit 1
fi

# No pipeline. `tr -dc ... </dev/urandom | head -c N` looks idiomatic but head
# closes the pipe as soon as it has N bytes, tr dies on SIGPIPE, and under
# `set -o pipefail` that is exit 141 — the script would abort on its very first
# secret. Draw a finite amount and slice it in the shell instead.
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

mkdir -p "$target/env.d" "$target/keycloak"

client_secret="$(secret 40)"
user_one_password="$(secret 24)"
user_two_password="$(secret 24)"
db_password="$(secret 32)"
minio_password="$(secret 32)"
kc_admin_password="$(secret 24)"
collab_secret="$(secret 32)"
yprovider_key="$(secret 32)"
django_secret="$(secret 50)"
superuser_password="$(secret 24)"

cat >"$target/env.d/common" <<EOF
S3_HOST=minio:9000
BACKEND_HOST=backend
FRONTEND_HOST=frontend
YPROVIDER_HOST=y-provider
BUCKET_NAME=docs-media-storage
REALM_NAME=docs
EOF

cat >"$target/env.d/backend" <<EOF
DJANGO_CONFIGURATION=LoopbackPreview
DJANGO_SETTINGS_MODULE=preview_settings
DJANGO_SECRET_KEY=$django_secret
DJANGO_ALLOWED_HOSTS=["localhost","127.0.0.1","backend","frontend"]
DJANGO_CSRF_TRUSTED_ORIGINS=["$origin","http://127.0.0.1:8100"]
DJANGO_EMAIL_BACKEND=django.core.mail.backends.console.EmailBackend
DJANGO_SUPERUSER_PASSWORD=$superuser_password
REDIS_URL=redis://redis:6379/1
AWS_S3_ENDPOINT_URL=http://minio:9000
AWS_S3_ACCESS_KEY_ID=preview
AWS_S3_SECRET_ACCESS_KEY=$minio_password
AWS_STORAGE_BUCKET_NAME=docs-media-storage
OIDC_RP_CLIENT_ID=docs
OIDC_RP_CLIENT_SECRET=$client_secret
OIDC_RP_SIGN_ALGO=RS256
OIDC_RP_SCOPES=openid email profile
# Front-channel endpoints are where the BROWSER is sent, so they must use the
# address the browser can reach.
OIDC_OP_AUTHORIZATION_ENDPOINT=http://localhost:8180/realms/docs/protocol/openid-connect/auth
OIDC_OP_LOGOUT_ENDPOINT=http://localhost:8180/realms/docs/protocol/openid-connect/logout
# Back-channel endpoints are called by the BACKEND. localhost there is the
# backend container itself, so they must address the provider over the compose
# network. Getting JWKS wrong here is a 500 on the callback, not a login error.
OIDC_OP_TOKEN_ENDPOINT=http://keycloak:8080/realms/docs/protocol/openid-connect/token
OIDC_OP_USER_ENDPOINT=http://keycloak:8080/realms/docs/protocol/openid-connect/userinfo
OIDC_OP_JWKS_ENDPOINT=http://keycloak:8080/realms/docs/protocol/openid-connect/certs
LOGIN_REDIRECT_URL=$origin
LOGIN_REDIRECT_URL_FAILURE=$origin
LOGOUT_REDIRECT_URL=$origin
OIDC_REDIRECT_ALLOWED_HOSTS=$origin
OIDC_AUTH_REQUEST_EXTRA_PARAMS={}
EOF

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
MINIO_ROOT_USER=preview
MINIO_ROOT_PASSWORD=$minio_password
EOF

cat >"$target/env.d/keycloak" <<EOF
KC_BOOTSTRAP_ADMIN_USERNAME=admin
KC_BOOTSTRAP_ADMIN_PASSWORD=$kc_admin_password
KC_HTTP_ENABLED=true
KC_HOSTNAME_STRICT=false
EOF

cat >"$target/env.d/yprovider" <<EOF
COLLABORATION_BACKEND_BASE_URL=http://backend:8000
COLLABORATION_SERVER_ORIGIN=$origin
COLLABORATION_SERVER_SECRET=$collab_secret
COLLABORATION_API_URL=http://y-provider:4444/api/
Y_PROVIDER_API_KEY=$yprovider_key
PORT=4444
EOF

cat >"$target/env.d/test-users" <<EOF
# Two SEPARATE preview accounts. Not real identities; this instance holds no
# live data and is reachable only over a loopback tunnel.
USER_ONE=alice
USER_ONE_PASSWORD=$user_one_password
USER_TWO=bob
USER_TWO_PASSWORD=$user_two_password
KEYCLOAK_ADMIN=admin
KEYCLOAK_ADMIN_PASSWORD=$kc_admin_password
OIDC_RP_CLIENT_SECRET=$client_secret
EOF

sed -e "s|__CLIENT_SECRET__|$client_secret|" \
    -e "s|__USER_ONE_PASSWORD__|$user_one_password|" \
    -e "s|__USER_TWO_PASSWORD__|$user_two_password|" \
    keycloak/realm.template.json >"$target/keycloak/realm.json"

# Config a container must read. The frontend runs as uid 101 and the backend as
# uid 1000, so a mode inherited from umask 077 would leave nginx exiting with
# "Permission denied" and the backend unable to import its settings.
install -m 644 compose.yaml         "$target/compose.yaml"
install -m 644 preview_settings.py  "$target/preview_settings.py"
install -m 644 default.conf.template "$target/default.conf.template"

# env.d is read by the docker daemon as root, never by a container, so it stays
# closed. The realm holds secrets but is bind-mounted into Keycloak and must be
# readable by that container's user: the file itself is 644, and the directory
# above it is 700 so nothing but root can reach it from the host. A bind mount
# is resolved by the daemon as root, so the restrictive parent does not block
# the container.
chmod 700 "$target/env.d" "$target/keycloak"
chmod 600 "$target"/env.d/*
chmod 644 "$target/keycloak/realm.json"

echo "written to $target:"
echo "  env.d/{common,backend,postgresql,minio,keycloak,yprovider,test-users}"
echo "    directory 700, files 600 — root only, never read by a container"
echo "  keycloak/realm.json"
echo "    file 644 inside a 700 directory — Keycloak reads it via the bind mount"
echo "  compose.yaml, preview_settings.py, default.conf.template  (644)"
echo
echo "next — dependencies first, then migrate, then the rest:"
echo "  cd $target"
echo "  docker compose up -d --wait --wait-timeout 120 postgresql redis minio keycloak"
echo "  docker compose run --rm backend python manage.py migrate"
echo "  docker compose up -d --wait --wait-timeout 180"
echo
echo "the backend's healthcheck runs before migrations exist, so bringing the"
echo "whole stack up with --wait first can time out on an unhealthy backend."
