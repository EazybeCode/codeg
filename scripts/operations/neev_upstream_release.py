#!/usr/bin/env python3
"""Trusted release hooks for the unattended NON-CORE upstream updater.

scripts/upstream/updater.mjs invokes one fixed argv per hook:

    /usr/bin/python3 /opt/neev-upstream/neev_upstream_release.py \
        --config /etc/neev-upstream/release.json <command>

    current-release  print the source SHA of the verified running release
    validate         candidate policy, dependency pins, tsc + targeted vitest
    build            Next static export; image = pinned runtime base + latest out/
    active-runs      exit 0 only when no sign-in or agent turn is active
    deploy           recheck gate, swap the compose image once, compose up
    health           verify health, workspace HTML hash, backend hash; commit
    rollback         restore the previous image (no schema change); commit

Operator only: record-current --source-sha SHA --image REPO:TAG
--base-image REPO:TAG seeds the release mapping from what is verifiably running
and pins the immutable core runtime image every NON-CORE image is built FROM.

Privilege split: the updater runs as an unprivileged user that owns the bare
repository; this root-owned script runs as root through a narrow sudo rule
(`python3 -I`, env limited to ALLOWED_ENV). As root it only reads the
repository through git plumbing that never runs repository-configured programs
(no archive/checkout/filters/textconv/hooks), re-hashes every blob it reads,
and never touches the candidate worktree. Candidate code runs only inside a
disposable builder container: non-root image user, --cap-drop ALL,
--network none, no mounts or volumes, fixed node entry points, no package
scripts or installs. A NON-CORE candidate differs from the release only in
allowlisted presentation/doc files, so the backend is never rebuilt; every
image is the pinned base plus one layer holding the latest out/. Everything
fails closed; current and previous images are always retained.
"""

import argparse
import contextlib
import fcntl
import hashlib
import io
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import time
import urllib.error
import urllib.request

TRUSTED_UID = 0
ALLOWED_ENV = (
    "NEEV_UPSTREAM_RUN_ID", "NEEV_UPSTREAM_CANDIDATE_SHA", "NEEV_UPSTREAM_BASE_SHA",
    "NEEV_UPSTREAM_PREVIOUS_SHA", "NEEV_UPSTREAM_CANDIDATE_DIR", "NEEV_UPSTREAM_REPORT_DIR",
)
OWNER = "neev-upstream-release"
OWNER_LABEL = "neev.upstream.owner=" + OWNER
NAME_PREFIX = "neev-upd-"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
IMAGE_ID_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
RUN_ID_RE = re.compile(r"^\d{8}T\d{6}Z-\d{1,10}$")
REPO_RE = re.compile(r"^[a-z0-9][a-z0-9._/-]{0,100}$")
IMAGE_REF_RE = re.compile(
    r"^[a-z0-9][a-z0-9._/-]{0,100}:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$"
)
SAFE_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
USER_RE = re.compile(r"^([0-9]{1,10}|[a-z_][a-z0-9_-]{0,31})(:([0-9]{1,10}|[a-z_][a-z0-9_-]{0,31}))?$")

# Independent second check: the updater's policy is stricter, this is the
# outer bound a candidate may ever differ from the release in.
_SEG = r"[A-Za-z0-9_-][A-Za-z0-9._-]*"
CANDIDATE_PATH_RE = re.compile(
    r"^(src/components/ui/" + _SEG + r"\.tsx"
    r"|docs/readme/README\.[A-Za-z0-9._-]+\.md"
    r"|docs/chat-channels/" + _SEG + r"\.md"
    r"|docs/url-scheme\.md"
    r"|docs/images/" + _SEG + r"\.(png|jpg|gif))$"
)
# Anything that changes what node_modules or the build toolchain must be.
DEP_FILES = (
    "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc",
    "next.config.ts", "tsconfig.json", "vitest.config.ts",
    "postcss.config.mjs", "components.json", "src/test-setup.ts",
)
# The builder's cached node_modules must have been installed from these.
NODE_DEP_FILES = ("package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc")
SOURCE_PATHS = (
    "src", "public", "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml",
    ".npmrc", "next.config.ts", "tsconfig.json", "vitest.config.ts",
    "postcss.config.mjs", "components.json",
)
TSC = ["node", "node_modules/typescript/bin/tsc", "--noEmit", "-p", "tsconfig.json"]
VITEST = ["node", "node_modules/vitest/vitest.mjs", "run", "--maxWorkers=1", "--no-color"]
NEXT_BUILD = ["node", "node_modules/next/dist/bin/next", "build"]
GATE_SQL = (
    "SELECT (SELECT version FROM neev_schema_version WHERE id=1),"
    "(SELECT count(*) FROM neev_agent_attempts WHERE state='pending' "
    "AND expires_at>extract(epoch from now())::bigint),"
    "(SELECT count(*) FROM neev_agent_turns WHERE status='running');"
)
CONFIG_KEYS = {
    "schemaVersion", "runtimeDir", "repoPath", "docker", "git", "composeFile",
    "composeService", "container", "imageRepository", "builderImage",
    "builderImageId", "healthUrl", "workspaceUrl", "workspaceHtmlPath",
    "backendBinary", "webRoot", "expectedDbSchema", "dbCertPath", "psqlImage",
    "vitestTargets", "minFreeGiB",
}
RECORD_KEYS = {"sourceSha": SHA_RE, "image": IMAGE_REF_RE, "imageId": IMAGE_ID_RE,
               "workspaceHtmlSha256": HEX64_RE, "backendSha256": HEX64_RE,
               "depsSha256": HEX64_RE}
MAX_TAR_BYTES = 2 * 1024 ** 3
MAX_TAR_MEMBERS = 200_000
HEALTH_ATTEMPTS = 60
KEEP_RUN_DIRS = 3
ROOT_USERS = {"", "0", "root", "0:0", "root:root", "0:root", "root:0"}


class ReleaseError(Exception):
    pass


def fail(message):
    raise ReleaseError(message)


# ------------------------------------------------------------------ trust


def _is_norm_abs(path):
    return isinstance(path, str) and os.path.isabs(path) and os.path.normpath(path) == path


def _inside(child, parent):
    return child == parent or child.startswith(parent.rstrip("/") + "/")


def check_trusted(path, *, is_dir, private=False):
    """Regular file/dir owned by the trusted uid, not writable by others, with
    no symlinked or foreign-writable ancestor (sticky dirs such as /tmp are
    tolerated as ancestors only)."""
    if not _is_norm_abs(path):
        fail("%s must be an absolute normalized path" % path)
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        fail("%s does not exist" % path)
    if stat.S_ISLNK(st.st_mode):
        fail("%s must not be a symlink" % path)
    if is_dir != stat.S_ISDIR(st.st_mode) or not (is_dir or stat.S_ISREG(st.st_mode)):
        fail("%s has the wrong file type" % path)
    if st.st_uid not in (0, TRUSTED_UID):
        fail("%s must be owned by root or uid %d" % (path, TRUSTED_UID))
    mode = stat.S_IMODE(st.st_mode)
    if mode & (0o077 if private else 0o022):
        fail("%s has unsafe mode %o" % (path, mode))
    parent = os.path.dirname(path)
    while True:
        pst = os.lstat(parent)
        if stat.S_ISLNK(pst.st_mode) or not stat.S_ISDIR(pst.st_mode):
            fail("ancestor %s is not a real directory" % parent)
        if pst.st_uid not in (0, TRUSTED_UID):
            fail("ancestor %s has an untrusted owner" % parent)
        if pst.st_mode & 0o022 and not pst.st_mode & stat.S_ISVTX:
            fail("ancestor %s is writable by others" % parent)
        if parent == "/":
            return
        parent = os.path.dirname(parent)


def validate_config(raw):
    if not isinstance(raw, dict) or set(raw) != CONFIG_KEYS:
        fail("config keys must be exactly: %s" % ", ".join(sorted(CONFIG_KEYS)))
    if raw["schemaVersion"] != 1:
        fail("config schemaVersion must be 1")
    for key in ("runtimeDir", "repoPath", "docker", "git", "composeFile",
                "backendBinary", "webRoot", "dbCertPath"):
        if not _is_norm_abs(raw[key]):
            fail("%s must be an absolute normalized path" % key)
    for a in ("runtimeDir", "repoPath"):
        for b in ("runtimeDir", "repoPath", "composeFile"):
            if a != b and (_inside(raw[a], raw[b]) or _inside(raw[b], raw[a])):
                fail("%s and %s must not nest" % (a, b))
    checks = {
        "composeService": SAFE_NAME_RE, "container": SAFE_NAME_RE,
        "imageRepository": REPO_RE, "builderImage": IMAGE_REF_RE,
        "builderImageId": IMAGE_ID_RE, "psqlImage": IMAGE_REF_RE,
    }
    for key, regex in checks.items():
        if not isinstance(raw[key], str) or not regex.match(raw[key]):
            fail("%s is malformed" % key)
    for key in ("healthUrl", "workspaceUrl"):
        if not isinstance(raw[key], str) or not re.match(r"^https://[A-Za-z0-9.-]+(:\d+)?/[A-Za-z0-9._/-]*$", raw[key]):
            fail("%s must be a plain https URL" % key)
    if not re.match(r"^[A-Za-z0-9_-]+(/[A-Za-z0-9_-]+)*\.html$", str(raw["workspaceHtmlPath"])):
        fail("workspaceHtmlPath is malformed")
    if not isinstance(raw["expectedDbSchema"], int) or not 0 < raw["expectedDbSchema"] < 100000:
        fail("expectedDbSchema must be a positive integer")
    if not isinstance(raw["minFreeGiB"], int) or not 1 <= raw["minFreeGiB"] <= 1000:
        fail("minFreeGiB must be an integer between 1 and 1000")
    targets = raw["vitestTargets"]
    if (not isinstance(targets, list) or not 0 < len(targets) <= 50
            or not all(isinstance(t, str) and re.match(r"^src/[A-Za-z0-9_/-]+(\.[A-Za-z0-9_-]+)*\.test\.tsx?$", t)
                       and ".." not in t for t in targets)):
        fail("vitestTargets must list src/**/*.test.ts(x) files")
    return dict(raw)


def load_config(path, env):
    check_trusted(os.path.dirname(path) if _is_norm_abs(path) else "", is_dir=True, private=True)
    check_trusted(path, is_dir=False)
    with open(path, "rb") as fh:
        raw = json.loads(fh.read(1 << 20).decode("utf-8"))
    cfg = validate_config(raw)
    for key in ("docker", "git"):
        check_trusted(cfg[key], is_dir=False)
    check_trusted(cfg["composeFile"], is_dir=False)
    for key in ("NEEV_UPSTREAM_CANDIDATE_DIR", "NEEV_UPSTREAM_REPORT_DIR"):
        untrusted = env.get(key)
        if untrusted:
            for trusted in (path, cfg["runtimeDir"], cfg["docker"], cfg["git"], cfg["composeFile"]):
                if _inside(os.path.realpath(trusted), os.path.realpath(untrusted)):
                    fail("%s must not live inside %s" % (trusted, key))
    if _inside(path, cfg["repoPath"]):
        fail("config must not live inside repoPath")
    check_repo(cfg["repoPath"])
    return cfg


def check_repo(repo):
    """The bare repository is owned by the unprivileged updater user, so only
    its shape is checked: a real directory reached without symlinks."""
    cur = repo
    while cur != "/":
        st = os.lstat(cur)
        if stat.S_ISLNK(st.st_mode) or not stat.S_ISDIR(st.st_mode):
            fail("repoPath component %s must be a real directory, not a symlink" % cur)
        cur = os.path.dirname(cur)
    if stat.S_IMODE(os.lstat(repo).st_mode) & 0o002:
        fail("repoPath must not be world-writable")


def ensure_runtime(cfg):
    root = cfg["runtimeDir"]
    if not os.path.lexists(root):
        check_trusted(os.path.dirname(root), is_dir=True)
        os.mkdir(root, 0o700)
    check_trusted(root, is_dir=True, private=True)
    for sub in ("runs", "home", "compose-backups"):
        p = os.path.join(root, sub)
        if not os.path.lexists(p):
            os.mkdir(p, 0o700)
        check_trusted(p, is_dir=True, private=True)


@contextlib.contextmanager
def runtime_lock(cfg):
    fd = os.open(os.path.join(cfg["runtimeDir"], "lock"), os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail("another release hook holds the lock")
        yield
    finally:
        os.close(fd)


# ------------------------------------------------------------------ state


def _write_atomic(path, data, mode=0o600, owner=None):
    tmp = "%s.tmp-%d" % (path, os.getpid())
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    try:
        if owner is not None:
            os.fchown(fd, *owner)
        os.fchmod(fd, mode)
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)
    dfd = os.open(os.path.dirname(path), os.O_RDONLY)
    try:
        os.fsync(dfd)
    finally:
        os.close(dfd)


def check_record(rec, what="release record"):
    if not isinstance(rec, dict):
        fail("%s missing" % what)
    for key, regex in RECORD_KEYS.items():
        if not isinstance(rec.get(key), str) or not regex.match(rec[key]):
            fail("%s has invalid %s" % (what, key))
    return rec


def load_state(cfg):
    path = os.path.join(cfg["runtimeDir"], "state.json")
    if not os.path.lexists(path):
        fail("no release state; run record-current first")
    check_trusted(path, is_dir=False, private=True)
    with open(path, "rb") as fh:
        st = json.loads(fh.read(4 << 20).decode("utf-8"))
    if st.get("schemaVersion") != 1:
        fail("unsupported release state schema")
    check_record(st.get("current"), "current release")
    base = st.get("base")
    if (not isinstance(base, dict) or not IMAGE_REF_RE.match(str(base.get("image")))
            or not IMAGE_ID_RE.match(str(base.get("imageId"))) or not HEX64_RE.match(str(base.get("backendSha256")))):
        fail("state has no valid pinned runtime base image")
    return st


def save_state(cfg, st):
    st["history"] = st.get("history", [])[-100:]
    _write_atomic(os.path.join(cfg["runtimeDir"], "state.json"),
                  (json.dumps(st, indent=2, sort_keys=True) + "\n").encode())


# ------------------------------------------------------------------ process


def default_runner(argv, *, input=None, timeout=600, env=None, log=None):
    out = open(log, "ab") if log else subprocess.PIPE
    try:
        proc = subprocess.run(argv, input=input, stdout=out, stderr=subprocess.STDOUT if log else subprocess.PIPE,
                              timeout=timeout, env=env, check=False)
    except subprocess.TimeoutExpired:
        return 124, b"", b"timed out"
    finally:
        if log:
            out.close()
    return proc.returncode, proc.stdout or b"", proc.stderr or b""


def default_http_get(url, timeout=10):
    req = urllib.request.Request(url, headers={"Cache-Control": "no-cache", "User-Agent": OWNER})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, resp.read(32 << 20)
    except urllib.error.HTTPError as err:
        return err.code, b""


class Ctx:
    def __init__(self, cfg, env, runner=default_runner, http_get=default_http_get,
                 sleep=time.sleep, out=None, err=None):
        self.cfg, self.env, self.runner, self.http_get, self.sleep = cfg, env, runner, http_get, sleep
        self.out = out or sys.stdout
        self.err = err or sys.stderr

    def log(self, message):
        print(message, file=self.err, flush=True)

    def try_cmd(self, argv, *, input=None, timeout=600, env_extra=None, log=None):
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
               "HOME": os.path.join(self.cfg["runtimeDir"], "home")}
        env.update(env_extra or {})
        return self.runner(list(argv), input=input, timeout=timeout, env=env, log=log)

    def cmd(self, argv, **kw):
        rc, out, err = self.try_cmd(argv, **kw)
        if rc != 0:
            tail = err.decode("utf-8", "replace")[-400:].strip()
            fail("%s %s exited %s: %s" % (os.path.basename(argv[0]), argv[1] if len(argv) > 1 else "", rc, tail))
        return out

    def docker(self, *args, **kw):
        return self.cmd([self.cfg["docker"], *args], **kw)

    def git(self, *args, **kw):
        return self.cmd(self._git_argv(args), env_extra=GIT_ENV, **kw)

    def git_ok(self, *args):
        return self.try_cmd(self._git_argv(args), env_extra=GIT_ENV)[0] == 0

    def _git_argv(self, args):
        # Command-line scope is protected config, so this exact safe.directory
        # is honoured; no wildcard, and only the configured repository.
        return [self.cfg["git"], "--no-pager", "-c", "safe.directory=" + self.cfg["repoPath"],
                "--git-dir", self.cfg["repoPath"], "-c", "core.hooksPath=/dev/null",
                "-c", "core.fsmonitor=false", "-c", "protocol.allow=never", *args]

    def disk_free(self, path):
        return shutil.disk_usage(path).free


GIT_ENV = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_TERMINAL_PROMPT": "0",
           "GIT_PAGER": "cat", "GIT_EXTERNAL_DIFF": "", "GIT_OPTIONAL_LOCKS": "0"}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


# ------------------------------------------------------------------ docker facts


def image_id(ctx, ref):
    value = ctx.docker("image", "inspect", "--format", "{{.Id}}", ref).decode().strip()
    if not IMAGE_ID_RE.match(value):
        fail("cannot resolve image %s" % ref)
    return value


def running_image_id(ctx):
    value = ctx.docker("inspect", "--format", "{{.State.Running}} {{.Image}}", ctx.cfg["container"]).decode().split()
    if len(value) != 2 or value[0] != "true" or not IMAGE_ID_RE.match(value[1]):
        fail("production container is not running")
    return value[1]


def _parse_sha256sum(out):
    fields = out.decode().split()
    if not fields or not HEX64_RE.match(fields[0]):
        fail("unexpected sha256sum output")
    return fields[0]


def image_file_sha(ctx, image, path):
    return _parse_sha256sum(ctx.docker("run", "--rm", "--network", "none", "--entrypoint", "sha256sum", image, path))


def live_file_sha(ctx, path):
    return _parse_sha256sum(ctx.docker("exec", ctx.cfg["container"], "sha256sum", path))


def _ref_pattern(ref):
    return re.compile(r"(?<![A-Za-z0-9._/:@-])" + re.escape(ref) + r"(?![A-Za-z0-9._/:@-])")


def compose_count(ctx, ref):
    with open(ctx.cfg["composeFile"], "r", encoding="utf-8") as fh:
        return len(_ref_pattern(ref).findall(fh.read()))


def verify_live(ctx, rec, *, http):
    if image_id(ctx, rec["image"]) != rec["imageId"]:
        fail("image tag %s no longer points at the recorded image" % rec["image"])
    if running_image_id(ctx) != rec["imageId"]:
        fail("running container image does not match the recorded release")
    if compose_count(ctx, rec["image"]) != 1:
        fail("compose file does not reference the release image exactly once")
    if live_file_sha(ctx, ctx.cfg["backendBinary"]) != rec["backendSha256"]:
        fail("running backend binary does not match the recorded release")
    if http:
        verify_http(ctx, rec)


def verify_http(ctx, rec, attempts=HEALTH_ATTEMPTS):
    for attempt in range(attempts):
        try:
            health, _ = ctx.http_get(ctx.cfg["healthUrl"])
            status, body = ctx.http_get(ctx.cfg["workspaceUrl"])
            if health == 200 and status == 200 and sha256(body) == rec["workspaceHtmlSha256"]:
                return
        except Exception:  # noqa: BLE001 - any transport error is "not yet healthy"
            pass
        if attempt + 1 < attempts:
            ctx.sleep(2)
    fail("health or workspace HTML hash did not verify")


def db_state(ctx):
    env_list = json.loads(ctx.docker("inspect", "--format", "{{json .Config.Env}}", ctx.cfg["container"]))
    urls = [v.split("=", 1)[1] for v in env_list if isinstance(v, str) and v.startswith("NEEV_DATABASE_URL=")]
    if len(urls) != 1 or not urls[0]:
        fail("production container has no database URL")
    cert = ctx.cfg["dbCertPath"]
    out = ctx.docker("run", "--rm", "-i", "--network", "host", "-v", "%s:%s:ro" % (cert, cert),
                     "-e", "NEEV_URL", ctx.cfg["psqlImage"], "sh", "-c",
                     'exec psql "$NEEV_URL" -At -v ON_ERROR_STOP=1',
                     input=GATE_SQL.encode(), timeout=120, env_extra={"NEEV_URL": urls[0]})
    parts = out.decode().strip().split("|")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        fail("unexpected gate query output")
    return tuple(int(p) for p in parts)


def gate_clear(ctx):
    version, pending, running = db_state(ctx)
    ctx.log("schema=%d active_sign_ins=%d running_turns=%d" % (version, pending, running))
    if version != ctx.cfg["expectedDbSchema"]:
        fail("database schema %d is not the expected %d" % (version, ctx.cfg["expectedDbSchema"]))
    return pending == 0 and running == 0


def verify_schema(ctx):
    if db_state(ctx)[0] != ctx.cfg["expectedDbSchema"]:
        fail("database schema changed")


# ------------------------------------------------------------------ candidate


def hook_env(ctx, *keys):
    values = {}
    for key in keys:
        value = ctx.env.get(key, "")
        regex = RUN_ID_RE if key == "NEEV_UPSTREAM_RUN_ID" else SHA_RE
        if not regex.match(value):
            fail("%s is missing or malformed" % key)
        values[key] = value
    return values


def check_candidate(ctx, base, cand):
    for sha in (base, cand):
        if not SHA_RE.match(sha) or not ctx.git_ok("cat-file", "-e", sha + "^{commit}"):
            fail("unknown commit %s" % sha)
    if not ctx.git_ok("merge-base", "--is-ancestor", base, cand):
        fail("candidate does not descend from the release")
    raw = ctx.git("diff-tree", "-r", "-z", "--raw", "--no-renames", "--no-ext-diff", "--no-textconv",
                  "--no-commit-id", base, cand).decode("utf-8", "strict")
    tokens = raw.split("\0")
    paths = []
    for i in range(0, len(tokens) - 1, 2):
        meta, path = tokens[i].split(), tokens[i + 1]
        if len(meta) != 5 or meta[4] not in ("M", "A") or meta[1] != "100644":
            fail("candidate has a non-regular or non-additive change: %r" % path[:200])
        if not CANDIDATE_PATH_RE.match(path):
            fail("candidate touches a path outside the NON-CORE bound: %r" % path[:200])
        paths.append(path)
    if not paths:
        fail("candidate has no changes")
    return paths


def deps_digest(ctx, sha):
    files = {}
    for name in DEP_FILES:
        spec = "%s:%s" % (sha, name)
        files[name] = sha256(read_blobs(ctx, [ctx.git("rev-parse", "--verify", spec).decode().strip()])[0]) \
            if ctx.git_ok("cat-file", "-e", spec) else None
    return sha256(json.dumps(files, sort_keys=True).encode()), files


def _member_parts(name):
    parts = name.rstrip("/").split("/")
    if (not name or name.startswith("/") or "\\" in name or any(ord(c) < 32 for c in name)
            or any(p in ("", ".", "..") for p in parts)):
        fail("unsafe archive path %r" % name[:200])
    return parts


def validate_tar(tf, tops):
    members, total = [], 0
    for m in tf.getmembers():
        parts = _member_parts(m.name)
        if parts[0] not in tops:
            fail("archive entry outside allowed roots: %r" % m.name[:200])
        if not (m.isreg() or m.isdir()):
            fail("archive entry is not a regular file or directory: %r" % m.name[:200])
        total += m.size
        members.append(m)
        if len(members) > MAX_TAR_MEMBERS or total > MAX_TAR_BYTES:
            fail("archive too large")
    return members


def safe_extract(tf, members, dest):
    os.mkdir(dest, 0o700)
    for m in members:
        target = os.path.join(dest, *_member_parts(m.name))
        if m.isdir():
            os.makedirs(target, 0o755, exist_ok=True)
            continue
        os.makedirs(os.path.dirname(target), 0o755, exist_ok=True)
        mode = 0o755 if m.mode & 0o111 else 0o644
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
        with os.fdopen(fd, "wb") as out:
            shutil.copyfileobj(tf.extractfile(m), out)


def read_blobs(ctx, oids):
    """Read blobs with `cat-file --batch` (never runs filters or textconv) and
    re-hash each one: the repository is writable by the unprivileged user."""
    if not oids:
        return []
    data = ctx.git("cat-file", "--batch", input=("\n".join(oids) + "\n").encode(), timeout=600)
    blobs, pos = [], 0
    for oid in oids:
        end = data.index(b"\n", pos)
        header = data[pos:end].decode().split()
        if len(header) != 3 or header[0] != oid or header[1] != "blob":
            fail("unexpected object %s" % oid)
        size = int(header[2])
        body = data[end + 1:end + 1 + size]
        if len(body) != size or hashlib.sha1(b"blob %d\0" % size + body).hexdigest() != oid:
            fail("object %s does not match its id" % oid)
        blobs.append(body)
        pos = end + 1 + size + 1
    return blobs


def _tar_add(tf, name, data=None, mode=0o755):
    info = tarfile.TarInfo(name)
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    info.mtime = 0
    if data is None:
        info.type, info.mode = tarfile.DIRTYPE, 0o755
        tf.addfile(info)
    else:
        info.size, info.mode = len(data), mode
        tf.addfile(info, io.BytesIO(data))


def source_tar(ctx, sha):
    listing = ctx.git("ls-tree", "-r", "-z", "--full-tree", sha, "--", *SOURCE_PATHS).decode("utf-8", "strict")
    entries = []
    for entry in filter(None, listing.split("\0")):
        meta, path = entry.split("\t", 1)
        parts, fields = _member_parts(path), meta.split()
        if fields[0] not in ("100644", "100755") or fields[1] != "blob" or not SHA_RE.match(fields[2]):
            fail("candidate source contains a symlink or submodule: %r" % path[:200])
        if parts[0] not in SOURCE_PATHS:
            fail("unexpected source path %r" % path[:200])
        entries.append((path, fields[0], fields[2]))
    if not {"src", "package.json", "pnpm-lock.yaml"} <= {p.split("/", 1)[0] for p, _, _ in entries}:
        fail("candidate source is incomplete")
    blobs = read_blobs(ctx, [oid for _, _, oid in entries])
    buf, dirs = io.BytesIO(), set()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.PAX_FORMAT) as tf:
        for (path, mode, _), body in zip(entries, blobs):
            parts = path.split("/")
            for i in range(1, len(parts)):
                d = "/".join(parts[:i])
                if d not in dirs:
                    dirs.add(d)
                    _tar_add(tf, d)
            _tar_add(tf, path, body, 0o755 if mode == "100755" else 0o644)
    data = buf.getvalue()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as tf:
        validate_tar(tf, set(SOURCE_PATHS))
    return data


def run_dir(ctx, run_id):
    path = os.path.join(ctx.cfg["runtimeDir"], "runs", run_id)
    if not os.path.lexists(path):
        os.mkdir(path, 0o700)
    check_trusted(path, is_dir=True, private=True)
    return path


def prepare(ctx):
    """Shared validate/build prechecks. Everything before any container runs."""
    env = hook_env(ctx, "NEEV_UPSTREAM_RUN_ID", "NEEV_UPSTREAM_CANDIDATE_SHA", "NEEV_UPSTREAM_BASE_SHA")
    run_id, cand, base = env["NEEV_UPSTREAM_RUN_ID"], env["NEEV_UPSTREAM_CANDIDATE_SHA"], env["NEEV_UPSTREAM_BASE_SHA"]
    st = load_state(ctx.cfg)
    if st.get("pending"):
        fail("a deploy or rollback is unresolved")
    if st["current"]["sourceSha"] != base:
        fail("updater base is not the recorded current release")
    check_candidate(ctx, base, cand)
    digest, files = deps_digest(ctx, cand)
    if digest != st["current"]["depsSha256"]:
        fail("dependency or toolchain files differ from the released baseline")
    check_disk(ctx)
    tar = source_tar(ctx, cand)
    if image_id(ctx, ctx.cfg["builderImage"]) != ctx.cfg["builderImageId"]:
        fail("builder image tag does not match the pinned builder image id")
    fmt = "{{json .Config.User}} {{json .Config.Volumes}}"
    user, volumes = ctx.docker("image", "inspect", "--format", fmt, ctx.cfg["builderImageId"]).decode().split(None, 1)
    if json.loads(user) in ROOT_USERS or not USER_RE.match(json.loads(user)):
        fail("builder image must declare a non-root USER")
    if json.loads(volumes):
        fail("builder image must not declare VOLUMEs")
    wanted = ["/app/" + n for n in NODE_DEP_FILES if files[n] is not None]
    out = ctx.docker("run", "--rm", "--network", "none", "--entrypoint", "sha256sum", ctx.cfg["builderImageId"], *wanted)
    have = {line.split()[1][len("/app/"):]: line.split()[0] for line in out.decode().splitlines() if line.strip()}
    for name in NODE_DEP_FILES:
        if files[name] is not None and have.get(name) != files[name]:
            fail("builder node_modules were not installed from the candidate's %s" % name)
    return st, run_id, cand, tar, files


def check_disk(ctx):
    need = ctx.cfg["minFreeGiB"] * 1024 ** 3
    root = ctx.docker("info", "--format", "{{.DockerRootDir}}").decode().strip()
    if not _is_norm_abs(root):
        fail("cannot determine the Docker root directory")
    for path in (root, ctx.cfg["runtimeDir"]):
        free = ctx.disk_free(path)
        if free < need:
            fail("insufficient disk: %.1f GiB free at %s, need %d GiB; nothing built"
                 % (free / 1024 ** 3, path, ctx.cfg["minFreeGiB"]))


def check_base(ctx, st):
    """The pinned core runtime image must still be exactly what was recorded
    and carry the same backend as the running release."""
    base, cur = st["base"], st["current"]
    if image_id(ctx, base["image"]) != base["imageId"]:
        fail("runtime base tag %s no longer points at the pinned image" % base["image"])
    if base["backendSha256"] != cur["backendSha256"] or \
            image_file_sha(ctx, base["imageId"], ctx.cfg["backendBinary"]) != cur["backendSha256"]:
        fail("runtime base backend differs from the running release")
    return base


def cleanup_containers(ctx, keep=()):
    out = ctx.docker("ps", "-a", "--filter", "label=" + OWNER_LABEL, "--format", "{{.Names}}").decode()
    for name in out.split():
        if name.startswith(NAME_PREFIX) and name not in keep:
            ctx.try_cmd([ctx.cfg["docker"], "rm", "-f", name])


@contextlib.contextmanager
def builder(ctx, run_id, phase, tar, files):
    name = "%s%s-%s" % (NAME_PREFIX, phase, run_id.lower())
    cleanup_containers(ctx)
    ctx.docker("create", "--name", name, "--label", OWNER_LABEL, "--label", "neev.upstream.run=" + run_id,
               "--network", "none", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
               "--memory", "6g", "--memory-swap", "8g", "--cpus", "2", "--pids-limit", "4096",
               "--entrypoint", "sleep", ctx.cfg["builderImageId"], "infinity")
    try:
        ctx.docker("start", name)
        ctx.docker("exec", name, "rm", "-rf", "--", "/app/src", "/app/out", "/app/.next", "/app/next-env.d.ts")
        ctx.docker("cp", "-", name + ":/app", input=tar, timeout=600)
        out = ctx.docker("exec", name, "sha256sum", "/app/package.json", "/app/pnpm-lock.yaml").decode().split()
        if out[0::2] != [files["package.json"], files["pnpm-lock.yaml"]]:
            fail("candidate source did not land in the builder")
        yield name
    finally:
        ctx.try_cmd([ctx.cfg["docker"], "rm", "-f", name])


def node_exec(ctx, name, argv, log, timeout, production=False):
    envs = ["-e", "CI=1", "-e", "NEXT_TELEMETRY_DISABLED=1", "-e", "HOME=/tmp",
            "-e", "NODE_ENV=" + ("production" if production else "test")]
    ctx.log("running %s (log %s)" % (" ".join(argv[:3]), log))
    ctx.docker("exec", "-w", "/app", *envs, name, *argv, timeout=timeout, log=log)


def cmd_validate(ctx):
    st, run_id, cand, tar, files = prepare(ctx)
    rdir = run_dir(ctx, run_id)
    with builder(ctx, run_id, "validate", tar, files) as name:
        node_exec(ctx, name, TSC, os.path.join(rdir, "tsc.log"), 1800)
        node_exec(ctx, name, VITEST + list(ctx.cfg["vitestTargets"]), os.path.join(rdir, "vitest.log"), 1800)
    ctx.log("validated %s" % cand)


def web_tar(out_dir, web_root):
    """Tar of the validated out/ tree, rooted at basename(webRoot), root-owned."""
    top = os.path.basename(web_root)
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.PAX_FORMAT) as tf:
        _tar_add(tf, top)
        for dirpath, dirnames, filenames in os.walk(out_dir):
            dirnames.sort()
            rel = os.path.relpath(dirpath, out_dir)
            prefix = top if rel == "." else top + "/" + rel.replace(os.sep, "/")
            for d in dirnames:
                _tar_add(tf, prefix + "/" + d)
            for f in sorted(filenames):
                full = os.path.join(dirpath, f)
                st = os.lstat(full)
                if not stat.S_ISREG(st.st_mode):
                    fail("export contains a non-regular file")
                with open(full, "rb") as fh:
                    _tar_add(tf, prefix + "/" + f, fh.read(), 0o755 if st.st_mode & 0o111 else 0o644)
    return buf.getvalue()


IMAGE_CONFIG_FMT = "{{json .Config.User}}|{{json .Config.Entrypoint}}|{{json .Config.Cmd}}|{{json .Config.Env}}"


def cmd_build(ctx):
    st, run_id, cand, tar, files = prepare(ctx)
    cur = st["current"]
    base = check_base(ctx, st)
    prune_images(ctx, st)
    rdir = run_dir(ctx, run_id)
    tree = os.path.join(rdir, "export")
    if os.path.lexists(tree):
        shutil.rmtree(tree)
    with builder(ctx, run_id, "build", tar, files) as name:
        node_exec(ctx, name, NEXT_BUILD, os.path.join(rdir, "next-build.log"), 3600, production=True)
        data = ctx.docker("cp", name + ":/app/out", "-", timeout=600)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as tf:
        safe_extract(tf, validate_tar(tf, {"out"}), tree)
    out_dir = os.path.join(tree, "out")
    html_path = os.path.join(out_dir, *ctx.cfg["workspaceHtmlPath"].split("/"))
    if not os.path.isfile(html_path) or os.path.islink(html_path):
        fail("build did not produce %s" % ctx.cfg["workspaceHtmlPath"])
    with open(html_path, "rb") as fh:
        html_sha = sha256(fh.read())
    web = web_tar(out_dir, ctx.cfg["webRoot"])
    shutil.rmtree(tree)

    # Base + exactly one layer holding the latest out/: create (never started),
    # copy, commit. No Dockerfile, no build cache, no RUN.
    tag = "%s:upd-%s" % (ctx.cfg["imageRepository"], run_id.lower())
    if ctx.try_cmd([ctx.cfg["docker"], "image", "inspect", tag])[0] == 0:
        fail("image %s already exists" % tag)
    stage = "%sstage-%s" % (NAME_PREFIX, run_id.lower())
    ctx.docker("create", "--name", stage, "--label", OWNER_LABEL, "--network", "none", base["imageId"])
    try:
        ctx.docker("cp", "-", stage + ":" + os.path.dirname(ctx.cfg["webRoot"]), input=web, timeout=600)
        ctx.docker("commit", "--change", "LABEL " + OWNER_LABEL, "--change", "LABEL neev.upstream.source=" + cand,
                   stage, tag, timeout=900)
    finally:
        ctx.try_cmd([ctx.cfg["docker"], "rm", "-f", stage])
    new_id = image_id(ctx, tag)

    def inspect(ref, fmt):
        return ctx.docker("image", "inspect", "--format", fmt, ref).decode().strip()

    base_layers = json.loads(inspect(base["imageId"], "{{json .RootFS.Layers}}"))
    new_layers = json.loads(inspect(new_id, "{{json .RootFS.Layers}}"))
    if new_layers[:-1] != base_layers or len(new_layers) != len(base_layers) + 1:
        fail("built image is not the runtime base plus exactly one web layer")
    if inspect(new_id, IMAGE_CONFIG_FMT) != inspect(base["imageId"], IMAGE_CONFIG_FMT):
        fail("built image changed the runtime user, entrypoint, command or environment")
    if image_file_sha(ctx, new_id, ctx.cfg["backendBinary"]) != cur["backendSha256"]:
        fail("built image backend differs from the live backend")
    if image_file_sha(ctx, new_id, ctx.cfg["webRoot"] + "/" + ctx.cfg["workspaceHtmlPath"]) != html_sha:
        fail("built image does not serve the exported workspace HTML")
    record = {"sourceSha": cand, "image": tag, "imageId": new_id, "workspaceHtmlSha256": html_sha,
              "backendSha256": cur["backendSha256"], "depsSha256": cur["depsSha256"]}
    build = {"runId": run_id, "baseSourceSha": cur["sourceSha"], "currentImageId": cur["imageId"],
             "runtimeBaseImageId": base["imageId"], "release": record}
    _write_atomic(os.path.join(rdir, "build.json"), json.dumps(build, indent=2).encode())
    ctx.log("built %s %s" % (tag, new_id))
    prune_runs(ctx, keep=run_id)


def prune_runs(ctx, keep):
    root = os.path.join(ctx.cfg["runtimeDir"], "runs")
    runs = sorted(n for n in os.listdir(root) if RUN_ID_RE.match(n) and n != keep)
    for n in runs[:max(0, len(runs) - KEEP_RUN_DIRS)]:
        shutil.rmtree(os.path.join(root, n))


def prune_images(ctx, st):
    keep = {r["image"] for r in (st.get("current"), st.get("previous")) if r}
    pending = st.get("pending") or {}
    keep |= {r["image"] for r in (pending.get("from"), pending.get("to")) if r}
    prefix = ctx.cfg["imageRepository"] + ":upd-"
    out = ctx.docker("image", "ls", "--filter", "label=" + OWNER_LABEL, "--format", "{{.Repository}}:{{.Tag}}").decode()
    for ref in out.split():
        if ref.startswith(prefix) and ref not in keep:
            ctx.try_cmd([ctx.cfg["docker"], "image", "rm", ref])  # never forced


# ------------------------------------------------------------------ deploy


def rewrite_compose(ctx, old_ref, new_ref):
    path = ctx.cfg["composeFile"]
    check_trusted(path, is_dir=False)
    st = os.lstat(path)
    with open(path, "r", encoding="utf-8") as fh:
        text = fh.read()
    if len(_ref_pattern(old_ref).findall(text)) != 1 or _ref_pattern(new_ref).findall(text):
        fail("compose file must reference %s exactly once and not %s" % (old_ref, new_ref))
    new = _ref_pattern(old_ref).sub(new_ref, text, count=1)
    _write_atomic(path, new.encode("utf-8"), stat.S_IMODE(st.st_mode), (st.st_uid, st.st_gid))
    return text


def compose_up(ctx, image_id_wanted):
    ctx.docker("compose", "-f", ctx.cfg["composeFile"], "up", "-d", "--no-deps", ctx.cfg["composeService"], timeout=600)
    for _ in range(HEALTH_ATTEMPTS):
        try:
            if running_image_id(ctx) == image_id_wanted:
                return
        except ReleaseError:
            pass
        ctx.sleep(2)
    fail("container did not start on the expected image")


def cmd_deploy(ctx):
    env = hook_env(ctx, "NEEV_UPSTREAM_RUN_ID", "NEEV_UPSTREAM_CANDIDATE_SHA", "NEEV_UPSTREAM_PREVIOUS_SHA")
    run_id = env["NEEV_UPSTREAM_RUN_ID"]
    st = load_state(ctx.cfg)
    if st.get("pending"):
        fail("a deploy or rollback is unresolved")
    cur = st["current"]
    with open(os.path.join(run_dir(ctx, run_id), "build.json"), "rb") as fh:
        build = json.load(fh)
    new = check_record(build.get("release"), "build record")
    if (build.get("runId") != run_id or new["sourceSha"] != env["NEEV_UPSTREAM_CANDIDATE_SHA"]
            or build.get("baseSourceSha") != cur["sourceSha"] or cur["sourceSha"] != env["NEEV_UPSTREAM_PREVIOUS_SHA"]
            or build.get("currentImageId") != cur["imageId"]
            or build.get("runtimeBaseImageId") != st["base"]["imageId"]):
        fail("build record does not match this deployment")
    if image_id(ctx, new["image"]) != new["imageId"]:
        fail("candidate image tag moved")
    verify_live(ctx, cur, http=False)
    if not gate_clear(ctx):
        fail("active sign-ins or agent turns appeared; not deploying")
    backup = os.path.join(ctx.cfg["runtimeDir"], "compose-backups", "compose.yml." + run_id)
    with open(ctx.cfg["composeFile"], "rb") as fh:
        original = fh.read()
    _write_atomic(backup, original)
    st["pending"] = {"runId": run_id, "phase": "deploying", "from": cur, "to": new,
                     "composeBackup": backup, "composeSha256": sha256(original), "at": _now()}
    save_state(ctx.cfg, st)
    rewrite_compose(ctx, cur["image"], new["image"])
    compose_up(ctx, new["imageId"])
    st["pending"]["phase"] = "started"
    save_state(ctx.cfg, st)
    ctx.log("started %s" % new["image"])


def cmd_health(ctx):
    run_id = hook_env(ctx, "NEEV_UPSTREAM_RUN_ID")["NEEV_UPSTREAM_RUN_ID"]
    st = load_state(ctx.cfg)
    pending = st.get("pending")
    if pending and pending.get("runId") != run_id:
        fail("pending deployment belongs to another run")
    rec = check_record(pending["to"]) if pending else st["current"]
    verify_live(ctx, rec, http=True)
    verify_schema(ctx)
    if pending:
        st["history"] = st.get("history", []) + [{"runId": run_id, "event": "deployed", "from": pending["from"]["image"],
                                                  "to": rec["image"], "at": _now()}]
        st["previous"], st["current"], st["pending"] = pending["from"], dict(rec, runId=run_id), None
        save_state(ctx.cfg, st)
        prune_images(ctx, st)
    ctx.log("healthy %s" % rec["image"])


def cmd_rollback(ctx):
    env = hook_env(ctx, "NEEV_UPSTREAM_RUN_ID", "NEEV_UPSTREAM_PREVIOUS_SHA")
    run_id = env["NEEV_UPSTREAM_RUN_ID"]
    st = load_state(ctx.cfg)
    pending = st.get("pending")
    if pending and pending.get("runId") == run_id:
        target, failed = check_record(pending["from"]), check_record(pending["to"])
    elif not pending and st["current"].get("runId") == run_id and st.get("previous"):
        target, failed = check_record(st["previous"]), st["current"]
    else:
        fail("nothing from this run to roll back")
    if target["sourceSha"] != env["NEEV_UPSTREAM_PREVIOUS_SHA"]:
        fail("rollback target is not the updater's previous release")
    if image_id(ctx, target["image"]) != target["imageId"]:
        fail("previous image tag moved; manual intervention required")
    st["pending"] = dict(pending or {}, runId=run_id, phase="rolling_back", **{"from": target, "to": failed})
    save_state(ctx.cfg, st)
    if compose_count(ctx, failed["image"]) == 1:
        rewrite_compose(ctx, failed["image"], target["image"])
    elif compose_count(ctx, target["image"]) != 1:
        fail("compose file references neither release image exactly once")
    compose_up(ctx, target["imageId"])
    verify_live(ctx, target, http=True)
    st["history"] = st.get("history", []) + [{"runId": run_id, "event": "rolled_back", "from": failed["image"],
                                              "to": target["image"], "at": _now()}]
    if st["current"].get("runId") == run_id:
        st["current"], st["previous"] = target, None
    st["pending"] = None
    save_state(ctx.cfg, st)
    ctx.log("rolled back to %s" % target["image"])


def cmd_current_release(ctx):
    st = load_state(ctx.cfg)
    if st.get("pending"):
        fail("a deploy or rollback is unresolved")
    verify_live(ctx, st["current"], http=False)
    print(st["current"]["sourceSha"], file=ctx.out)


def cmd_active_runs(ctx):
    if not gate_clear(ctx):
        fail("active sign-ins or agent turns")


def cmd_record_current(ctx, source_sha, image, base_image):
    if not SHA_RE.match(source_sha) or not IMAGE_REF_RE.match(image) or not IMAGE_REF_RE.match(base_image):
        fail("record-current needs --source-sha SHA --image REPO:TAG --base-image REPO:TAG")
    if base_image.startswith(ctx.cfg["imageRepository"] + ":upd-"):
        fail("the runtime base must be a reviewed core image, not an updater image")
    path = os.path.join(ctx.cfg["runtimeDir"], "state.json")
    st = load_state(ctx.cfg) if os.path.lexists(path) else {"schemaVersion": 1, "history": []}
    if st.get("pending"):
        fail("a deploy or rollback is unresolved")
    if not ctx.git_ok("cat-file", "-e", source_sha + "^{commit}"):
        fail("source SHA is not in repoPath")
    health, _ = ctx.http_get(ctx.cfg["healthUrl"])
    status, body = ctx.http_get(ctx.cfg["workspaceUrl"])
    if health != 200 or status != 200:
        fail("live site is not healthy")
    rec = {"sourceSha": source_sha, "image": image, "imageId": image_id(ctx, image),
           "workspaceHtmlSha256": sha256(body), "backendSha256": live_file_sha(ctx, ctx.cfg["backendBinary"]),
           "depsSha256": deps_digest(ctx, source_sha)[0]}
    verify_live(ctx, rec, http=False)
    verify_schema(ctx)
    base = {"image": base_image, "imageId": image_id(ctx, base_image), "backendSha256": rec["backendSha256"]}
    if image_file_sha(ctx, base["imageId"], ctx.cfg["backendBinary"]) != rec["backendSha256"]:
        fail("base image backend differs from the running backend")
    st["base"] = base
    if st.get("current") and st["current"]["imageId"] != rec["imageId"]:
        st["previous"] = st["current"]
    st["current"] = rec
    st.setdefault("previous", None)
    st["history"] = st.get("history", []) + [{"event": "recorded", "to": image, "at": _now()}]
    save_state(ctx.cfg, st)
    print(json.dumps(rec, indent=2), file=ctx.out)


def _now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


COMMANDS = {
    "current-release": (cmd_current_release, False), "validate": (cmd_validate, True),
    "build": (cmd_build, True), "active-runs": (cmd_active_runs, False),
    "deploy": (cmd_deploy, True), "health": (cmd_health, True), "rollback": (cmd_rollback, True),
}


def main(argv=None, env=None, make_ctx=Ctx):
    os.umask(0o077)
    parser = argparse.ArgumentParser(prog="neev_upstream_release.py")
    parser.add_argument("--config", required=True)
    parser.add_argument("command", choices=sorted(COMMANDS) + ["record-current"])
    parser.add_argument("--source-sha")
    parser.add_argument("--image")
    parser.add_argument("--base-image")
    args = parser.parse_args(argv)
    source = os.environ if env is None else env
    env = {k: source[k] for k in ALLOWED_ENV if k in source}
    try:
        if os.geteuid() == 0 and not sys.flags.isolated:
            fail("run as root only via `python3 -I` (isolated mode)")
        cfg = load_config(args.config, env)
        ensure_runtime(cfg)
        ctx = make_ctx(cfg, env)
        if args.command == "record-current":
            with runtime_lock(cfg):
                cmd_record_current(ctx, args.source_sha or "", args.image or "", args.base_image or "")
            return 0
        func, locked = COMMANDS[args.command]
        with runtime_lock(cfg) if locked else contextlib.nullcontext():
            func(ctx)
        return 0
    except ReleaseError as error:
        print("neev-release %s: %s" % (args.command, error), file=sys.stderr)
    except Exception as error:  # noqa: BLE001 - fail closed on anything unexpected
        print("neev-release %s: unexpected %s: %s" % (args.command, type(error).__name__, str(error)[:300]),
              file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
