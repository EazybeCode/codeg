#!/usr/bin/env bash
#
# Neev document engine — production rollout.
#
# WRITTEN TO BE READ BEFORE IT IS RUN. Nothing runs on import, every mutating
# step needs an explicit subcommand plus --yes, and each one prints its plan
# first.
#
#   ./release.sh preflight --image REF        # read-only, changes nothing
#   ./release.sh backup --yes
#   ./release.sh rollout --image REF --yes
#   ./release.sh enable-creation --yes        # the separate, deliberate step
#   ./release.sh disable-creation --yes       # rollback 1: same image
#   ./release.sh detach --yes                 # rollback 2: same image
#   ./release.sh status
#
# `rollout` deliberately stops with the engine wired and **creation off**: Neev
# runs the engine-aware image, can open documents already on the engine, and
# creates nothing new there until someone runs `enable-creation`. Splitting them
# is the point — the risky half is one command, reversed by its opposite, with
# no image change in either direction.
#
# Never done here: touching the `https` service, changing an existing env value,
# removing a volume, altering a capability or device, or going near the preview
# and canary stacks. Three fields of the `neev` service change and the script
# proves that before it installs anything.
#
# Output carries no secrets. The compose file may inline credentials in
# `environment:`, so changes are reported as a summary of what was allowed to
# change, never as a diff of the file.

set -euo pipefail

NATIVE_DIR=/opt/neev-native-https-20260913
NATIVE_COMPOSE="${NATIVE_DIR}/compose.yml"
NATIVE_SERVICE=neev
NATIVE_CONTAINER=neev-native-https-20260913-neev-1
NATIVE_IP=10.0.2.4          # fixed address of the native service on its network
NATIVE_PORT=3080            # NEEV_BIND inside the container

DOCS_DIR=/opt/neev-document-engine-prod
APP_ENV=/etc/neev-native/app.env
ENGINE_ENV=/etc/neev-native/document-engine.env
ENGINE_ENV_SOURCE="${DOCS_DIR}/native-env-additions.private"

ARTIFACT_HOST=/var/lib/neev-native/document-engine/artifact
ARTIFACT_TARGET=/data/document-engine/artifact

BACKUP_DIR=/var/backups/neev-docs-release
MIN_FREE_MB=2048
HEALTH_TIMEOUT=120

# Capabilities the agent runtime needs. Asserted before and after, because a
# rollout that quietly dropped one would look fine until an agent tried to work.
REQUIRED_CAPS="SYS_ADMIN NET_ADMIN MKNOD"

# The launch helper's own file capabilities are part of what makes an agent able
# to start. Named here so the check reads as a requirement rather than a path.
NEEV_CAP_HELPER_NAME=neev-agent-caplaunch

# Same digest the docs stack pins, already on this host. Used only if the host's
# own client tools are missing or older than the server.
PG_IMAGE=postgres:16@sha256:a3b7f434b2dc57ce85a67e171163eb8ab1a1ebcb39d27484661f26b1dfbe30d6
DB_CERT=/etc/neev-db/server.crt

YES=0
DRY_RUN=0
IMAGE=""

log()  { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
info() { printf '    %s\n' "$*"; }
fail() { printf '\n\033[1mFAILED: %s\033[0m\n' "$*" >&2; exit 1; }

confirm() {
  [ "$DRY_RUN" -eq 1 ] && { info "dry run: would $*"; return 1; }
  [ "$YES" -eq 1 ] || fail "$* — re-run with --yes when you have read the plan above"
  return 0
}

require_root() { [ "$(id -u)" -eq 0 ] || fail "must run as root"; }

db_url() { grep '^NEEV_DATABASE_URL=' "$APP_ENV" | cut -d= -f2-; }

# --------------------------------------------------------- database tooling --
#
# The server is PostgreSQL 16 on a separate host reached over the private NIC.
# A client older than the server cannot dump it, so the version is checked
# rather than assumed; if the host's tools are missing or too old, the pinned
# postgres:16 image already present on this host is used instead, with the CA
# certificate mounted read-only and host networking so the private address
# resolves the same way it does for the native service.

PG_MODE=""   # "host" or "container", decided by detect_pg_tooling

server_version_major() {
  local url; url=$(db_url)
  if [ "$PG_MODE" = "container" ]; then
    docker run --rm --network host -v "${DB_CERT}:${DB_CERT}:ro" -e PGCONNECT_TIMEOUT=10 \
      "$PG_IMAGE" psql "$url" -tAc 'show server_version' 2>/dev/null | cut -d. -f1
  else
    PGCONNECT_TIMEOUT=10 psql "$url" -tAc 'show server_version' 2>/dev/null | cut -d. -f1
  fi
}

detect_pg_tooling() {
  local host_major="" server_major=""
  if command -v pg_dump >/dev/null && command -v psql >/dev/null; then
    host_major=$(pg_dump --version | awk '{print $3}' | cut -d. -f1)
    PG_MODE=host
    server_major=$(server_version_major || true)
    if [ -z "$server_major" ]; then
      info "host psql cannot reach the server; falling back to the container client"
      PG_MODE=container
    elif [ "$host_major" -lt "$server_major" ]; then
      info "host pg_dump is ${host_major}, server is ${server_major}: too old, using the container client"
      PG_MODE=container
    else
      info "database client: host pg_dump ${host_major} against server ${server_major}"
      return 0
    fi
  else
    info "host pg_dump/psql not installed; using the pinned container client"
    PG_MODE=container
  fi

  docker image inspect "$PG_IMAGE" >/dev/null 2>&1 ||
    fail "neither host client tools nor the pinned postgres image ${PG_IMAGE%%@*} are available"
  [ -r "$DB_CERT" ] || fail "$DB_CERT is not readable; the container client needs it for sslmode=verify-full"
  server_major=$(server_version_major || true)
  [ -n "$server_major" ] || fail "cannot reach the native database (checked without printing the URL)"
  info "database client: pinned postgres container against server ${server_major}"
}

run_pg_dump() {
  local url="$1" target="$2"
  if [ "$PG_MODE" = "container" ]; then
    docker run --rm --network host \
      -v "${DB_CERT}:${DB_CERT}:ro" -v "${BACKUP_DIR}:${BACKUP_DIR}" \
      "$PG_IMAGE" pg_dump "$url" --format=custom --no-owner --no-privileges --file="$target"
  else
    pg_dump "$url" --format=custom --no-owner --no-privileges --file="$target"
  fi
}

run_pg_restore_list() {
  if [ "$PG_MODE" = "container" ]; then
    docker run --rm -v "${BACKUP_DIR}:${BACKUP_DIR}" "$PG_IMAGE" pg_restore --list "$1"
  else
    pg_restore --list "$1"
  fi
}

run_psql_scalar() {
  local url; url=$(db_url)
  if [ "$PG_MODE" = "container" ]; then
    docker run --rm --network host -v "${DB_CERT}:${DB_CERT}:ro" -e PGCONNECT_TIMEOUT=10 \
      "$PG_IMAGE" psql "$url" -tAc "$1" 2>/dev/null
  else
    PGCONNECT_TIMEOUT=10 psql "$url" -tAc "$1" 2>/dev/null
  fi
}

# ---------------------------------------------------------------- preflight --

check_candidate_image() {
  [ -n "$IMAGE" ] || fail "--image REF is required (the candidate build)"
  # `pull_policy: never` means a missing image is not a pull failure — it is a
  # start failure, after the old container is gone.
  docker image inspect "$IMAGE" >/dev/null 2>&1 ||
    fail "image $IMAGE is not present locally, and pull_policy is 'never'"

  local current; current=$(current_native_image)
  info "candidate: $IMAGE"
  info "  id:      $(docker image inspect -f '{{.Id}}' "$IMAGE" | cut -c1-19)"
  info "  created: $(docker image inspect -f '{{.Created}}' "$IMAGE")"

  # The entrypoint is set in the compose file, not the image, so a candidate that
  # does not carry that script starts and dies. Check it is there.
  local entrypoint
  entrypoint=$(python3 - "$NATIVE_COMPOSE" "$NATIVE_SERVICE" <<'PY'
import sys, yaml
service = yaml.safe_load(open(sys.argv[1]))["services"][sys.argv[2]]
print((service.get("entrypoint") or [""])[-1])
PY
)
  # Fingerprint both images and compare. The container is disposable, has no
  # network, no mounts and a read-only root: it reads its own filesystem and
  # exits. Nothing about the live deployment is touched.
  local candidate_fp current_fp
  candidate_fp=$(image_fingerprint "$IMAGE" "$entrypoint") ||
    fail "could not fingerprint the candidate image"
  current_fp=$(image_fingerprint "$current" "$entrypoint" 2>/dev/null || true)

  echo "$candidate_fp" | grep -q "^entrypoint ok" ||
    fail "candidate image does not contain the compose entrypoint: $entrypoint"

  # The launch helper carries file capabilities. A rebuild that drops them
  # produces an image that starts, passes every health check, and then cannot
  # launch an agent — so it is checked here rather than discovered later.
  local helper_caps
  helper_caps=$(echo "$candidate_fp" | awk '$1 == "cap_helper" {print $3}')
  case "$helper_caps" in
    "") fail "candidate image has no ${NEEV_CAP_HELPER_NAME}; the agent launcher would be missing" ;;
    none) fail "candidate's launch helper carries NO file capabilities (the current image has them)" ;;
    *) ;;
  esac

  info "  binaries (caps, sha256 head) — candidate vs currently deployed:"
  local line label path caps sha current_line
  while read -r label path caps sha; do
    [ "$label" = "entrypoint" ] && continue
    current_line=$(echo "$current_fp" | awk -v l="$label" '$1 == l {print $3" "$4}')
    if [ -n "$current_line" ] && [ "${caps} ${sha}" != "$current_line" ]; then
      info "    ${path}: ${caps} ${sha}   (was ${current_line})"
    else
      info "    ${path}: ${caps} ${sha}"
    fi
  done <<<"$(echo "$candidate_fp" | grep -v '^entrypoint ')"

  # Workspace layout must match what the running deployment expects; a candidate
  # built against a different root would start and then not find its agents.
  local live_root cand_root
  live_root=$(grep '^NEEV_AGENT_WORKSPACE_ROOT=' /etc/neev-native/agent.env | cut -d= -f2- || true)
  cand_root=$(docker image inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$IMAGE" |
              grep '^NEEV_AGENT_WORKSPACE_ROOT=' | cut -d= -f2- || true)
  if [ -n "$cand_root" ] && [ -n "$live_root" ] && [ "$cand_root" != "$live_root" ]; then
    fail "candidate expects workspace root ${cand_root}, deployment uses ${live_root}"
  fi
  info "  workspace root: ${live_root:-<set only in env file>} (candidate agrees)"

  local revision
  revision=$(docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$IMAGE" 2>/dev/null || true)
  [ -n "$revision" ] && [ "$revision" != "<no value>" ] && info "  source revision: $revision"

  [ "$IMAGE" = "$current" ] &&
    info "  NOTE: candidate is the image already configured ($current)" || true
}

# One disposable container per image: no network, no mounts, read-only root.
# Prints "<label> <path> <caps|none> <sha256-head>" per interesting file, and
# "entrypoint ok" when the compose entrypoint exists.
image_fingerprint() {
  local image="$1" entrypoint="$2"
  docker run --rm --network none --read-only --entrypoint /bin/sh "$image" -c '
    entrypoint="$1"
    [ -n "$entrypoint" ] && [ -f "$entrypoint" ] && echo "entrypoint ok $entrypoint"
    describe() {
      label="$1"; path="$2"
      [ -n "$path" ] && [ -f "$path" ] || return 0
      caps=$(getcap "$path" 2>/dev/null | sed "s|^.*= *||")
      [ -n "$caps" ] || caps=none
      printf "%s %s %s %s\n" "$label" "$path" "$caps" "$(sha256sum "$path" | cut -c1-16)"
    }
    describe entry_script "$entrypoint"
    describe launcher    "$NEEV_AGENT_LAUNCHER"
    describe cap_helper  "$NEEV_AGENT_CAP_HELPER"
    describe agent       "$NEEV_AGENT_BINARY"
    describe codex       "$NEEV_AGENT_CODEX_BINARY"
  ' _ "$entrypoint" 2>/dev/null
}

current_native_image() {
  python3 - "$NATIVE_COMPOSE" "$NATIVE_SERVICE" <<'PY'
import sys, yaml
print(yaml.safe_load(open(sys.argv[1]))["services"][sys.argv[2]]["image"])
PY
}

check_capabilities_configured() {
  python3 - "$NATIVE_COMPOSE" "$NATIVE_SERVICE" "$REQUIRED_CAPS" <<'PY'
import sys, yaml
service = yaml.safe_load(open(sys.argv[1]))["services"][sys.argv[2]]
granted = set(service.get("cap_add") or [])
missing = [cap for cap in sys.argv[3].split() if cap not in granted]
if missing:
    raise SystemExit(f"REFUSING: compose no longer grants {', '.join(missing)}")
print(f"    capabilities granted: {' '.join(sorted(granted))}")
print(f"    devices: {len(service.get('devices') or [])}, "
      f"cgroup rules: {len(service.get('device_cgroup_rules') or [])}")
PY
}

check_docs_stack() {
  [ -d "$DOCS_DIR" ] || fail "$DOCS_DIR does not exist"
  ( cd "$DOCS_DIR" && docker compose config -q ) || fail "docs compose config is invalid"
  local published
  published=$(cd "$DOCS_DIR" && docker compose config --format json |
    python3 -c 'import json,sys; c=json.load(sys.stdin); print(sum(len(s.get("ports") or []) for s in c["services"].values()))')
  [ "$published" = "0" ] ||
    fail "the docs stack publishes $published port(s); Neev must be the only way in"
  grep -q 'neev-document-engine-prod-backend' "${DOCS_DIR}/env.d/backend" ||
    fail "DJANGO_ALLOWED_HOSTS lacks the backend alias; Django would answer 400 to the proxy"
  info "docs stack: config valid, no published ports, alias allowed"
}

check_artifact() {
  [ -d "$ARTIFACT_HOST" ] || fail "artifact directory $ARTIFACT_HOST is missing"
  [ -f "${ARTIFACT_HOST}/docs/[id]/index.html" ] ||
    fail "artifact has no docs/[id]/index.html — the editor route would 404"
  ! find "$ARTIFACT_HOST" -name service-worker.js -print -quit | grep -q . ||
    fail "artifact contains a service worker; it must not be served"
  info "artifact: $(find "$ARTIFACT_HOST" -type f | wc -l) files, editor route present, no service worker"
}

check_engine_env() {
  [ -f "$ENGINE_ENV_SOURCE" ] || fail "$ENGINE_ENV_SOURCE is missing"
  [ "$(stat -c '%a' "$ENGINE_ENV_SOURCE")" = "600" ] ||
    fail "$ENGINE_ENV_SOURCE must be mode 600 — it holds the bridge credential"
  local missing=0 var
  for var in NEEV_DOCS_CREATE_ENABLED NEEV_DOCS_BACKEND_URL NEEV_DOCS_COLLABORATION_URL \
             NEEV_DOCS_ARTIFACT_ROOT NEEV_DOCS_BRIDGE_KEY; do
    grep -q "^${var}=" "$ENGINE_ENV_SOURCE" || { info "missing: $var"; missing=1; }
  done
  [ "$missing" -eq 0 ] || fail "engine env additions are incomplete"
  local key_len
  key_len=$(grep '^NEEV_DOCS_BRIDGE_KEY=' "$ENGINE_ENV_SOURCE" | cut -d= -f2- | tr -d '\n' | wc -c)
  [ "$key_len" -ge 32 ] || fail "bridge key is ${key_len} chars; the endpoint refuses under 32"
  grep -q '^NEEV_DOCS_CREATE_ENABLED=false' "$ENGINE_ENV_SOURCE" ||
    fail "the staged env must start with creation OFF; enabling it is a separate step"
  info "engine env: five vars present, key length ${key_len}, creation off"
}

check_disk() {
  local free_mb; free_mb=$(df -Pm / | awk 'NR==2 {print $4}')
  [ "$free_mb" -ge "$MIN_FREE_MB" ] ||
    fail "only ${free_mb} MB free on /; want at least ${MIN_FREE_MB} MB"
  info "disk: ${free_mb} MB free"
}

check_compose_anchors() {
  python3 - "$NATIVE_COMPOSE" "$NATIVE_SERVICE" <<'PY'
import sys, yaml
compose = yaml.safe_load(open(sys.argv[1]))
service = compose["services"].get(sys.argv[2])
if service is None:
    raise SystemExit(f"service {sys.argv[2]} not found")
for key in ("image", "env_file", "volumes"):
    if key not in service:
        raise SystemExit(f"service {sys.argv[2]} has no {key}: — this script's edits assume it")
print(f"    native service: {len(service['env_file'])} env_file entries, "
      f"{len(service['volumes'])} volumes; other services: "
      f"{', '.join(k for k in compose['services'] if k != sys.argv[2])}")
PY
}

cmd_preflight() {
  require_root
  log "Preflight — reads only, changes nothing"
  info "current native image: $(current_native_image)"
  if [ -n "$IMAGE" ]; then check_candidate_image; else info "candidate image: (pass --image to check it)"; fi
  check_compose_anchors
  check_capabilities_configured
  check_docs_stack
  check_artifact
  check_engine_env
  detect_pg_tooling
  check_disk
  log "Preflight passed"
}

# ------------------------------------------------------------------- backup --

cmd_backup() {
  require_root
  detect_pg_tooling
  check_disk
  mkdir -p "$BACKUP_DIR"; chmod 700 "$BACKUP_DIR"

  local url stamp dump meta rows
  url=$(db_url)
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  dump="${BACKUP_DIR}/neev-${stamp}.dump"
  meta="${dump}.meta"

  log "Backing up the native database to ${dump}"
  info "every backup is kept; nothing here deletes one"
  confirm "take a fresh backup" || return 0

  # Read from the live database before the dump, so verification compares
  # against the source rather than against the dump describing itself.
  rows=$(run_psql_scalar 'select count(*) from neev_resources' || echo unknown)
  run_pg_dump "$url" "$dump"
  chmod 600 "$dump"

  local size objects
  size=$(stat -c '%s' "$dump")
  [ "$size" -gt 4096 ] || fail "backup is only ${size} bytes — refusing to call that a backup"
  objects=$(run_pg_restore_list "$dump" | grep -cv '^;' || true)
  [ "$objects" -ge 10 ] || fail "backup lists only ${objects} objects — it captured no schema"
  run_pg_restore_list "$dump" | grep -q 'TABLE DATA public neev_resources' ||
    fail "backup does not contain neev_resources table data"

  { echo "taken_at=${stamp}"; echo "dump=${dump}"; echo "bytes=${size}"
    echo "objects=${objects}"; echo "neev_resources_rows=${rows}"
    echo "native_image=$(current_native_image)"; echo "client_mode=${PG_MODE}"
  } >"$meta"
  chmod 600 "$meta"

  info "size: $(numfmt --to=iec "$size"), objects: ${objects}, neev_resources rows: ${rows}"
  info "restore (overwrites live data — never run casually):"
  info "  pg_restore --clean --if-exists --no-owner --dbname \"\$NEEV_DATABASE_URL\" ${dump}"
  log "Backup verified"
}

latest_backup() { ls -1t "${BACKUP_DIR}"/neev-*.dump 2>/dev/null | head -1; }

require_recent_backup() {
  local dump age_min
  dump=$(latest_backup) || true
  [ -n "${dump:-}" ] || fail "no backup found; run 'backup --yes' first"
  age_min=$(( ( $(date +%s) - $(stat -c '%Y' "$dump") ) / 60 ))
  [ "$age_min" -le 120 ] ||
    fail "newest backup is ${age_min} min old; take a fresh one before changing the image"
  info "using backup $(basename "$dump") (${age_min} min old)"
}

# -------------------------------------------------------------------- stage --

stage_engine_env() {
  log "Installing the engine env file at ${ENGINE_ENV} (root only)"
  confirm "install the engine env file" || return 0
  install -m 600 -o root -g root "$ENGINE_ENV_SOURCE" "$ENGINE_ENV"
  info "installed, mode $(stat -c '%a' "$ENGINE_ENV"), $(grep -c '^NEEV_DOCS_' "$ENGINE_ENV") variables"
}

# Build the edited compose beside the live one, validate everything about it,
# and only then put it in place. The live file is never the thing being tested.
stage_compose() {
  local staged backup_copy
  staged="${NATIVE_COMPOSE}.staged.$$"
  backup_copy="${NATIVE_COMPOSE}.before-document-engine-$(date -u +%Y%m%d-%H%M%S)"

  log "Staging three changes to the ${NATIVE_SERVICE} service"
  info "  image     -> ${IMAGE}"
  info "  env_file  += ${ENGINE_ENV}"
  info "  volumes   += ${ARTIFACT_HOST} -> ${ARTIFACT_TARGET} (read-only)"
  confirm "stage and validate the edit" || return 0

  trap 'rm -f "$staged"' RETURN

  python3 - "$NATIVE_COMPOSE" "$staged" "$NATIVE_SERVICE" "$IMAGE" "$ENGINE_ENV" \
              "$ARTIFACT_HOST" "$ARTIFACT_TARGET" <<'PY'
import sys, yaml
src, dst, service_name, image, engine_env, artifact_host, artifact_target = sys.argv[1:8]
compose = yaml.safe_load(open(src))
service = compose["services"][service_name]

service["image"] = image

# Appended, never replacing. Later entries win in compose, so the engine file
# cannot be shadowed by the ones before it. Idempotent: re-running adds nothing.
if engine_env not in service["env_file"]:
    service["env_file"].append(engine_env)

if not any(v.get("target") == artifact_target for v in service["volumes"]):
    service["volumes"].append({
        "type": "bind", "source": artifact_host, "target": artifact_target,
        "read_only": True,
        "bind": {"create_host_path": False, "propagation": "rprivate"},
    })

with open(dst, "w") as handle:
    yaml.safe_dump(compose, handle, default_flow_style=False, sort_keys=False, width=100)
PY

  # Everything that must be identical, checked on the staged file while the live
  # one is still untouched. Generic: any key of any service, and every top-level
  # key, must match except the three fields this rollout is allowed to change.
  python3 - "$NATIVE_COMPOSE" "$staged" "$NATIVE_SERVICE" "$ENGINE_ENV" \
              "$ARTIFACT_TARGET" "$IMAGE" "$REQUIRED_CAPS" <<'PY'
import sys, yaml
src, staged, name, engine_env, artifact_target, image, required_caps = sys.argv[1:8]
before, after = yaml.safe_load(open(src)), yaml.safe_load(open(staged))

if set(before) != set(after):
    raise SystemExit("REFUSING: top-level keys changed")
for key in before:
    if key != "services" and before[key] != after[key]:
        raise SystemExit(f"REFUSING: top-level '{key}' changed")
if set(before["services"]) != set(after["services"]):
    raise SystemExit("REFUSING: the set of services changed")
for other in before["services"]:
    if other != name and before["services"][other] != after["services"][other]:
        raise SystemExit(f"REFUSING: service '{other}' changed; this rollout touches only '{name}'")

b, a = before["services"][name], after["services"][name]
if set(b) != set(a):
    raise SystemExit("REFUSING: the set of keys on the native service changed")
for key in b:
    if key in ("image", "env_file", "volumes"):
        continue
    if b[key] != a[key]:
        raise SystemExit(f"REFUSING: '{key}' changed, which this rollout must never do")

if a["image"] != image:
    raise SystemExit("REFUSING: image is not the candidate")

expected_env = list(b["env_file"]) + ([engine_env] if engine_env not in b["env_file"] else [])
if a["env_file"] != expected_env:
    raise SystemExit("REFUSING: env_file changed by more than appending the engine file")

# Idempotent: if the mount is already there the count must stay the same.
already = [v for v in b["volumes"] if v.get("target") == artifact_target]
if len(a["volumes"]) != len(b["volumes"]) + (0 if already else 1):
    raise SystemExit("REFUSING: volume count changed by something other than the artifact mount")
for volume in b["volumes"]:
    if volume not in a["volumes"]:
        raise SystemExit(f"REFUSING: an existing volume was altered: {volume.get('target')}")
mount = [v for v in a["volumes"] if v.get("target") == artifact_target][0]
if not mount.get("read_only"):
    raise SystemExit("REFUSING: the artifact mount is not read-only")

granted = set(a.get("cap_add") or [])
missing = [cap for cap in required_caps.split() if cap not in granted]
if missing:
    raise SystemExit(f"REFUSING: staged config drops {', '.join(missing)}")

# A summary, never a diff: `environment:` can inline credentials.
print("    validated, nothing else differs:")
print(f"      image     {b['image']}  ->  {a['image']}")
print(f"      env_file  {len(b['env_file'])} -> {len(a['env_file'])} entries (engine file "
      f"{'already present' if engine_env in b['env_file'] else 'appended'})")
print(f"      volumes   {len(b['volumes'])} -> {len(a['volumes'])} "
      f"({'artifact mount already present' if already else 'artifact mount appended, read-only'})")
print(f"      preserved {len([k for k in b if k not in ('image','env_file','volumes')])} other keys "
      f"on '{name}', every other service, and all top-level keys")
print(f"      capabilities intact: {' '.join(sorted(granted))}")
PY

  ( cd "$NATIVE_DIR" && docker compose -f "$staged" config -q ) ||
    fail "staged compose is not valid; the live file was never touched"
  info "compose validated the staged file"

  cp -p "$NATIVE_COMPOSE" "$backup_copy"
  info "copy of the live file: $(basename "$backup_copy")"
  cat "$staged" >"$NATIVE_COMPOSE"     # preserves the live file's mode and owner
  info "staged file installed"
}

# ------------------------------------------------------------------- health --

native_http_probe() {
  # Run from inside the docs backend container, which shares a network with
  # Neev's fixed address: a real HTTP answer, and nothing new pulled to get it.
  ( cd "$DOCS_DIR" && docker compose exec -T backend python - "$NATIVE_IP" "$NATIVE_PORT" <<'PY'
import sys, urllib.error, urllib.request
try:
    with urllib.request.urlopen(f"http://{sys.argv[1]}:{sys.argv[2]}/", timeout=5) as response:
        print(response.status)
except urllib.error.HTTPError as err:
    print(err.code)
except Exception as err:
    print(f"unreachable: {err}")
    raise SystemExit(1)
PY
  ) 2>/dev/null
}

wait_for_native() {
  log "Health: waiting for Neev to answer HTTP (up to ${HEALTH_TIMEOUT}s)"
  local deadline=$(( $(date +%s) + HEALTH_TIMEOUT )) status
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if docker ps --format '{{.Names}}' | grep -q "^${NATIVE_CONTAINER}$"; then
      status=$(native_http_probe || true)
      case "$status" in 2*|3*|4*) info "Neev answered HTTP ${status}"; return 0 ;; esac
    fi
    sleep 3
  done
  fail "Neev did not answer HTTP within ${HEALTH_TIMEOUT}s — consider 'detach --yes'"
}

verify_runtime_invariants() {
  log "Health: the running container kept its privileges and devices"
  python3 - "$NATIVE_CONTAINER" "$REQUIRED_CAPS" "$ARTIFACT_TARGET" <<'PY'
import json, subprocess, sys
container, required, artifact_target = sys.argv[1:4]
spec = json.loads(subprocess.check_output(["docker", "inspect", container]))[0]
host = spec["HostConfig"]
granted = set(host.get("CapAdd") or [])
missing = [cap for cap in required.split() if cap not in granted]
if missing:
    raise SystemExit(f"REFUSING: running container lacks {', '.join(missing)}")
print(f"    capabilities: {' '.join(sorted(granted))}")
print(f"    devices: {len(host.get('Devices') or [])}, "
      f"cgroup rules: {len(host.get('DeviceCgroupRules') or [])}, "
      f"read_only rootfs: {host.get('ReadonlyRootfs')}")
mounts = [m for m in spec["Mounts"] if m["Destination"] == artifact_target]
if not mounts:
    raise SystemExit("REFUSING: the artifact is not mounted in the running container")
if mounts[0]["RW"]:
    raise SystemExit("REFUSING: the artifact is mounted writable")
print(f"    artifact mounted read-only at {artifact_target}")
PY
}

wait_for_docs() {
  log "Health: docs stack"
  ( cd "$DOCS_DIR" && docker compose ps --format '{{.Service}} {{.Status}}' ) | sed 's/^/    /'
  ( cd "$DOCS_DIR" && docker compose exec -T backend python manage.py check >/dev/null 2>&1 ) ||
    fail "the docs backend failed its own django check"
  info "docs backend: django check clean"
}

report_engine_state() {
  log "Engine state as the running container sees it"
  docker exec "$NATIVE_CONTAINER" printenv NEEV_DOCS_CREATE_ENABLED 2>/dev/null |
    sed 's/^/    NEEV_DOCS_CREATE_ENABLED=/' || info "NEEV_DOCS_CREATE_ENABLED is not set"
}

# ------------------------------------------------------------------ rollout --

cmd_rollout() {
  require_root
  [ -n "$IMAGE" ] || fail "--image REF is required"
  cmd_preflight

  log "Plan"
  info "1. require a backup taken in the last two hours"
  info "2. start the private docs stack (no published ports)"
  info "3. install the engine env (creation OFF); stage, validate, then install the compose edit"
  info "4. recreate ONLY the ${NATIVE_SERVICE} service on ${IMAGE}"
  info "5. health-check Neev, its privileges and devices, and the docs stack"
  info "creation stays OFF. 'enable-creation --yes' is a separate decision."
  confirm "perform the rollout" || return 0

  require_recent_backup

  log "Starting the docs stack"
  ( cd "$DOCS_DIR" && docker compose up -d --wait --wait-timeout 240 )
  wait_for_docs

  stage_engine_env
  stage_compose

  log "Recreating the ${NATIVE_SERVICE} service"
  # Only this service: the https/Caddy container keeps serving throughout.
  ( cd "$NATIVE_DIR" && docker compose -f "$NATIVE_COMPOSE" up -d --no-deps "$NATIVE_SERVICE" )
  wait_for_native
  verify_runtime_invariants
  report_engine_state

  log "Rolled out with creation OFF"
  info "Neev runs the engine-aware image and can open documents already on the engine."
  info "Nothing new is created there until: $0 enable-creation --yes"
}

# ----------------------------------------------------------- the flag itself --

set_creation_flag() {
  local value="$1"
  [ -f "$ENGINE_ENV" ] || fail "$ENGINE_ENV is not installed; nothing to flip"
  sed -i "s/^NEEV_DOCS_CREATE_ENABLED=.*/NEEV_DOCS_CREATE_ENABLED=${value}/" "$ENGINE_ENV"
  grep -q "^NEEV_DOCS_CREATE_ENABLED=${value}$" "$ENGINE_ENV" || fail "failed to set the flag"
  # `restart` keeps the environment the container was created with: compose only
  # re-reads env_file when it recreates. This cost a cycle on the canary.
  ( cd "$NATIVE_DIR" && docker compose -f "$NATIVE_COMPOSE" up -d --no-deps --force-recreate "$NATIVE_SERVICE" )
  wait_for_native
  verify_runtime_invariants
  report_engine_state
}

cmd_enable_creation() {
  require_root
  [ -f "$ENGINE_ENV" ] || fail "run 'rollout' first"
  log "Enabling document creation on the engine"
  info "New documents will be created on La Suite Docs once Neev restarts."
  info "Reversible with: $0 disable-creation --yes  (same image, existing documents keep working)"
  confirm "enable creation" || return 0
  set_creation_flag true
  log "Creation is ON"
}

cmd_disable_creation() {
  require_root
  log "Rollback 1 of 2 — creation off, engine still wired"
  info "Documents already on the engine keep opening; only new creation stops."
  info "The image does NOT change: reverting to the pre-engine image would remove"
  info "the proxy that serves the documents already created."
  confirm "disable creation" || return 0
  set_creation_flag false
  log "Creation is OFF; documents already on the engine still open"
}

cmd_detach() {
  require_root
  log "Rollback 2 of 2 — remove the engine wiring entirely"
  info "Neev keeps the same engine-aware image and stops being configured for the"
  info "engine. Documents already created there become unreachable from Neev — no"
  info "data is destroyed, and 'disable-creation' is what avoids ever needing this"
  info "with a population to strand."
  confirm "detach the engine" || return 0

  local staged backup_copy
  staged="${NATIVE_COMPOSE}.staged.$$"
  backup_copy="${NATIVE_COMPOSE}.before-detach-$(date -u +%Y%m%d-%H%M%S)"
  trap 'rm -f "$staged"' RETURN

  python3 - "$NATIVE_COMPOSE" "$staged" "$NATIVE_SERVICE" "$ENGINE_ENV" "$ARTIFACT_TARGET" <<'PY'
import sys, yaml
src, dst, name, engine_env, artifact_target = sys.argv[1:6]
compose = yaml.safe_load(open(src))
service = compose["services"][name]
service["env_file"] = [entry for entry in service["env_file"] if entry != engine_env]
service["volumes"] = [v for v in service["volumes"] if v.get("target") != artifact_target]
with open(dst, "w") as handle:
    yaml.safe_dump(compose, handle, default_flow_style=False, sort_keys=False, width=100)
PY

  python3 - "$NATIVE_COMPOSE" "$staged" "$NATIVE_SERVICE" "$REQUIRED_CAPS" <<'PY'
import sys, yaml
src, staged, name, required = sys.argv[1:5]
before, after = yaml.safe_load(open(src)), yaml.safe_load(open(staged))
for other in before["services"]:
    if other != name and before["services"][other] != after["services"][other]:
        raise SystemExit(f"REFUSING: service '{other}' changed")
b, a = before["services"][name], after["services"][name]
for key in b:
    if key in ("env_file", "volumes"):
        continue
    if b[key] != a[key]:
        raise SystemExit(f"REFUSING: '{key}' changed during detach")
if a["image"] != b["image"]:
    raise SystemExit("REFUSING: detach must not change the image")
granted = set(a.get("cap_add") or [])
missing = [cap for cap in required.split() if cap not in granted]
if missing:
    raise SystemExit(f"REFUSING: detach would drop {', '.join(missing)}")
print(f"    env_file {len(b['env_file'])} -> {len(a['env_file'])}, "
      f"volumes {len(b['volumes'])} -> {len(a['volumes'])}, image unchanged")
PY

  ( cd "$NATIVE_DIR" && docker compose -f "$staged" config -q ) ||
    fail "staged compose is not valid; the live file was never touched"
  cp -p "$NATIVE_COMPOSE" "$backup_copy"
  cat "$staged" >"$NATIVE_COMPOSE"
  info "copy of the live file: $(basename "$backup_copy")"

  ( cd "$NATIVE_DIR" && docker compose -f "$NATIVE_COMPOSE" up -d --no-deps --force-recreate "$NATIVE_SERVICE" )
  wait_for_native
  verify_runtime_invariants
  log "Engine detached. The docs stack still holds its data;"
  info "stop it with: cd ${DOCS_DIR} && docker compose stop   (never 'down -v')"
}

# ------------------------------------------------------------------- status --

cmd_status() {
  log "Native"
  info "compose image: $(current_native_image)"
  info "running:       $(docker inspect -f '{{.Config.Image}} ({{.State.Status}})' "$NATIVE_CONTAINER" 2>/dev/null || echo 'not running')"
  if [ -f "$ENGINE_ENV" ]; then
    info "engine env:    installed, $(grep '^NEEV_DOCS_CREATE_ENABLED=' "$ENGINE_ENV")"
  else
    info "engine env:    not installed"
  fi
  log "Docs stack"
  ( cd "$DOCS_DIR" && docker compose ps --format '{{.Service}} {{.Status}}' 2>/dev/null ) | sed 's/^/    /' ||
    info "not running"
  log "Backups (all retained)"
  local count; count=$(ls -1 "${BACKUP_DIR}"/neev-*.dump 2>/dev/null | wc -l)
  if [ "$count" -gt 0 ]; then
    local dump; dump=$(latest_backup)
    info "${count} kept; newest $(basename "$dump")  $(numfmt --to=iec "$(stat -c '%s' "$dump")")  $(date -d "@$(stat -c '%Y' "$dump")" -u +%FT%TZ)"
  else
    info "none in ${BACKUP_DIR}"
  fi
}

# --------------------------------------------------------------------- main --

command="${1:-}"; shift || true
while [ $# -gt 0 ]; do
  case "$1" in
    --yes) YES=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --image) IMAGE="${2:-}"; shift ;;
    *) fail "unknown option: $1" ;;
  esac
  shift
done

case "$command" in
  preflight)         cmd_preflight ;;
  backup)            cmd_backup ;;
  rollout)           cmd_rollout ;;
  enable-creation)   cmd_enable_creation ;;
  disable-creation)  cmd_disable_creation ;;
  detach)            cmd_detach ;;
  status)            cmd_status ;;
  *) sed -n '2,32p' "$0"; exit 1 ;;
esac
