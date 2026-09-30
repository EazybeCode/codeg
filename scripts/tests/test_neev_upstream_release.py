"""Tests for the upstream updater release hooks.

Git is real (a throwaway repository); Docker, HTTP and Postgres are a small
in-memory fake that models images, the running container, compose and the
builder, so the gate, fail-closed, deploy/health/rollback transaction and
candidate path protections are exercised through the real CLI entry point.
The trusted uid is relaxed to the invoking user; modes and symlink refusal are not.
"""

import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[1] / "operations"))

import neev_upstream_release as rel  # noqa: E402

GIT = os.path.realpath(shutil.which("git"))
GIT_ENV = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t",
           "GIT_COMMITTER_EMAIL": "t@t", "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
OLD, RUN = "neev-native-agent:shared-library-20260928", "20260930T020000Z-42"
CORE = "neev-native-agent:core-20260930"
BUILDER = "neev-upstream-frontend:20260930"
BACKEND, WEB = "/usr/local/bin/neev-server", "/app/web"


def h(data):
    return hashlib.sha256(data).hexdigest()


def git(repo, *args):
    return subprocess.run([GIT, "-C", repo, *args], check=True, capture_output=True, env=GIT_ENV).stdout.decode().strip()


def tar_of(entries):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tf:
        for name, data in entries:
            info = tarfile.TarInfo(name)
            if data not in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE):
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))
            else:
                info.type = data
                info.linkname = "/etc/passwd"
                tf.addfile(info)
    return buf.getvalue()


class World:
    """Fake Docker daemon + live site + database."""

    def __init__(self, compose_path):
        self.compose_path = compose_path
        self.images, self.meta = {}, {}
        self.running = None
        self.db = b"18|0|0\n"
        self.health = 200
        self.builders, self.node_runs, self.removed_images, self.calls = {}, [], [], []
        self.fail_node = False
        self.cfg = None
        self.free = 50 * 1024 ** 3
        self.git_argv = []

    def add_image(self, ref, files, layers, html=b"", labels=(), user="65532:65532", volumes=None):
        iid = "sha256:" + h(ref.encode())
        self.images[ref] = self.images[iid] = iid
        self.meta[iid] = {"files": files, "layers": layers, "html": html, "labels": set(labels),
                          "user": user, "volumes": volumes}
        return iid

    def http(self, url, timeout=10):
        if url.endswith("/healthz"):
            return self.health, b"ok"
        return 200, self.meta[self.running]["html"] if self.running else b""

    def run(self, argv, input=None, timeout=None, env=None, log=None):
        if argv[0] == GIT:
            self.git_argv.append(argv)
            p = subprocess.run(argv, input=input, capture_output=True, env=env)
            return p.returncode, p.stdout, p.stderr
        assert argv[0] == self.cfg["docker"], argv
        a = argv[1:]
        self.calls.append(a)
        ok = lambda out=b"": (0, out if isinstance(out, bytes) else out.encode(), b"")  # noqa: E731
        miss = (1, b"", b"No such object")
        if a[:2] == ["image", "inspect"]:
            if len(a) == 3:
                return ok() if a[2] in self.images else miss
            ref = a[4]
            if ref not in self.images:
                return miss
            meta = self.meta[self.images[ref]]
            return ok({"{{.Id}}": self.images[ref], rel.IMAGE_CONFIG_FMT: "%s|cfg" % json.dumps(meta["user"]),
                       "{{json .Config.User}} {{json .Config.Volumes}}":
                           "%s %s" % (json.dumps(meta["user"]), json.dumps(meta["volumes"])),
                       "{{json .RootFS.Layers}}": json.dumps(meta["layers"])}[a[3]] + "\n")
        if a[:2] == ["image", "ls"]:
            return ok("\n".join(r for r, i in self.images.items() if not r.startswith("sha256:")
                                and rel.OWNER_LABEL in self.meta[i]["labels"]))
        if a[:2] == ["image", "rm"]:
            self.removed_images.append(a[2])
            return ok()
        if a[0] == "info":
            return ok("/var/lib/docker\n")
        if a[0] == "inspect":
            if a[2].startswith("{{.State"):
                return ok("true %s" % self.running) if self.running else miss
            return ok(json.dumps(["PATH=/bin", "NEEV_DATABASE_URL=postgres://db/neev"]))
        if a[0] == "run" and "-i" in a:
            assert env["NEEV_URL"] == "postgres://db/neev" and input == rel.GATE_SQL.encode()
            return (1, b"", b"db down") if self.db is None else ok(self.db)
        if a[0] == "run":
            files = self.meta[self.images[a[6]]]["files"]
            if any(p not in files for p in a[7:]):
                return miss
            return ok("".join("%s  %s\n" % (files[p], p) for p in a[7:]))
        if a[0] == "compose":
            ref = re.search(r"image: (\S+)", Path(self.compose_path).read_text()).group(1)
            self.running = self.images[ref]
            return ok()
        if a[0] == "ps":
            return ok("\n".join(n for n, b in self.builders.items() if not b["removed"]))
        if a[0] == "create":
            name = a[a.index("--name") + 1]
            self.builders[name] = {"argv": a, "removed": False, "files": {}, "image": self.images[a[-1]]
                                   if "--entrypoint" not in a else self.images[a[-2]]}
            return ok()
        if a[0] == "commit":
            stage = self.builders[a[-2]]
            base = self.meta[stage["image"]]
            html = stage["web"]["web/workspace.html"]
            files = dict(base["files"], **{WEB + "/workspace.html": h(html)})
            self.committed = a
            self.add_image(a[-1], files, base["layers"] + ["web-" + a[-1]], html, [rel.OWNER_LABEL], base["user"])
            return ok()
        if a[0] == "start":
            return ok()
        if a[0] == "rm":
            self.builders[a[2]]["removed"] = True
            return ok()
        if a[0] == "cp" and a[1] == "-":
            with tarfile.open(fileobj=io.BytesIO(input)) as tf:
                regs = [m for m in tf.getmembers() if m.isreg()]
                assert all(m.uid == 0 and m.gid == 0 for m in tf.getmembers())
                b = self.builders[a[2].split(":")[0]]
                b["files"] = {"/app/" + m.name: h(tf.extractfile(m).read()) for m in regs}
                b["web"] = {m.name: tf.extractfile(m).read() for m in regs}
            return ok()
        if a[0] == "cp":
            return ok(tar_of([("out/index.html", b"<html>index</html>"),
                              ("out/workspace.html", b"<html>new workspace</html>")]))
        if a[0] == "exec" and a[1] == self.cfg["container"]:
            return ok("%s  %s\n" % (self.meta[self.running]["files"][a[3]], a[3]))
        if a[0] == "exec" and "node" in a:
            self.node_runs.append(a[a.index("node"):])
            return (1, b"", b"") if self.fail_node else ok()
        if a[0] == "exec" and a[2] == "sha256sum":
            files = self.builders[a[1]]["files"]
            return ok("".join("%s  %s\n" % (files[p], p) for p in a[3:]))
        if a[0] == "exec":
            return ok()
        raise AssertionError("unexpected docker call %r" % a)


class Harness(unittest.TestCase):
    def setUp(self):
        self._uid = rel.TRUSTED_UID
        rel.TRUSTED_UID = os.getuid()
        self.root = os.path.realpath(tempfile.mkdtemp())
        os.chmod(self.root, 0o700)
        for d in ("etc", "bin", "compose"):
            os.mkdir(os.path.join(self.root, d), 0o700)
        self.repo = os.path.join(self.root, "repo")
        self._make_repo()
        self.compose = os.path.join(self.root, "compose", "compose.yml")
        Path(self.compose).write_text("services:\n  neev:\n    image: %s\n    restart: always\n" % OLD)
        os.chmod(self.compose, 0o600)
        docker = os.path.join(self.root, "bin", "docker")
        Path(docker).write_text("#!/bin/sh\nexit 1\n")
        os.chmod(docker, 0o755)
        self.world = World(self.compose)
        self.old_html = b"<html>old workspace</html>"
        self.core_id = self.world.add_image(CORE, {BACKEND: h(b"server")}, ["base"], b"<html>core</html>")
        self.old_id = self.world.add_image(OLD, {BACKEND: h(b"server")}, ["base", "ui-old"], self.old_html)
        self.world.running = self.old_id
        lock = {"/app/package.json": h(b'{"name":"t"}\n'), "/app/pnpm-lock.yaml": h(b"lock: 1\n")}
        builder_id = self.world.add_image(BUILDER, lock, ["builder"], user="1000:1000")
        self.cfg = {
            "schemaVersion": 1, "runtimeDir": os.path.join(self.root, "runtime"),
            "repoPath": os.path.join(self.repo, ".git"), "docker": docker, "git": GIT,
            "composeFile": self.compose, "composeService": "neev", "container": "neev-native-https-20260913-neev-1",
            "imageRepository": "neev-native-agent", "builderImage": BUILDER, "minFreeGiB": 8,
            "builderImageId": builder_id, "healthUrl": "https://app.neev.so/healthz",
            "workspaceUrl": "https://app.neev.so/workspace", "workspaceHtmlPath": "workspace.html",
            "backendBinary": BACKEND, "webRoot": WEB, "expectedDbSchema": 18,
            "dbCertPath": "/etc/neev-db/server.crt", "psqlImage": "postgres:16-alpine",
            "vitestTargets": ["src/components/neev/native-agent-chats.test.tsx",
                              "src/components/neev/native-chat.test.tsx",
                              "src/components/neev/native-workspace-move.test.tsx"],
        }
        self.world.cfg = self.cfg
        self.config_path = os.path.join(self.root, "etc", "release.json")
        self.write_config(self.cfg)
        self.assertEqual(self.cli("record-current", "--source-sha", self.base, "--image", OLD,
                                  "--base-image", CORE), 0, self.err)

    def tearDown(self):
        rel.TRUSTED_UID = self._uid
        shutil.rmtree(self.root, ignore_errors=True)

    def _make_repo(self):
        os.mkdir(self.repo)
        git(self.repo, "init", "-q")
        files = {"package.json": '{"name":"t"}\n', "pnpm-lock.yaml": "lock: 1\n",
                 "src/components/ui/button.tsx": "export const B = 1\n", "src/lib/core.ts": "export {}\n",
                 "src/components/neev/native-chat.test.tsx": "test\n", "docs/url-scheme.md": "# URL\n"}
        for name, body in files.items():
            Path(self.repo, name).parent.mkdir(parents=True, exist_ok=True)
            Path(self.repo, name).write_text(body)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "base")
        self.base = git(self.repo, "rev-parse", "HEAD")

    def candidate(self, path, body=None, link=None):
        git(self.repo, "checkout", "-q", "--detach", self.base)
        target = Path(self.repo, path)
        target.parent.mkdir(parents=True, exist_ok=True)
        if link:
            target.unlink() if target.exists() else None
            os.symlink(link, target)
        else:
            target.write_text(body)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "candidate")
        return git(self.repo, "rev-parse", "HEAD")

    def write_config(self, cfg, mode=0o600):
        Path(self.config_path).write_text(json.dumps(cfg))
        os.chmod(self.config_path, mode)

    def cli(self, *args, env=None):
        out, err = io.StringIO(), io.StringIO()
        def make(cfg, e):
            ctx = rel.Ctx(cfg, e, runner=self.world.run, http_get=self.world.http,
                          sleep=lambda s: None, out=out, err=err)
            ctx.disk_free = lambda path: self.world.free
            self.ctx_env = e
            return ctx
        real_err, sys.stderr = sys.stderr, err
        try:
            code = rel.main(["--config", self.config_path, *args], env=env or {}, make_ctx=make)
        finally:
            sys.stderr = real_err
        self.out, self.err = out.getvalue(), err.getvalue()
        return code

    def state(self):
        return json.loads(Path(self.cfg["runtimeDir"], "state.json").read_text())

    def hook_env(self, cand):
        return {"NEEV_UPSTREAM_RUN_ID": RUN, "NEEV_UPSTREAM_CANDIDATE_SHA": cand, "NEEV_UPSTREAM_BASE_SHA": self.base,
                "NEEV_UPSTREAM_PREVIOUS_SHA": self.base, "NEEV_UPSTREAM_CANDIDATE_DIR": os.path.join(self.root, "wt")}

    def build_candidate(self):
        cand = self.candidate("src/components/ui/button.tsx", "export const B = 2\n")
        env = self.hook_env(cand)
        self.assertEqual(self.cli("validate", env=env), 0, self.err)
        self.assertEqual(self.cli("build", env=env), 0, self.err)
        return cand, env


class ConfigTests(Harness):
    def test_record_current_mapping_and_private_runtime(self):
        cur = self.state()["current"]
        self.assertEqual((cur["sourceSha"], cur["image"], cur["imageId"]), (self.base, OLD, self.old_id))
        self.assertEqual(cur["workspaceHtmlSha256"], h(self.old_html))
        self.assertEqual(self.state()["base"]["imageId"], self.core_id)
        self.assertEqual(os.stat(self.cfg["runtimeDir"]).st_mode & 0o777, 0o700)

    def test_group_writable_config_rejected(self):
        self.write_config(self.cfg, mode=0o620)
        self.assertEqual(self.cli("current-release"), 1)
        self.assertIn("unsafe mode", self.err)

    def test_symlinked_config_rejected(self):
        link = os.path.join(self.root, "etc", "link.json")
        os.symlink(self.config_path, link)
        self.config_path = link
        self.assertEqual(self.cli("current-release"), 1)
        self.assertIn("symlink", self.err)

    def test_runtime_dir_must_be_0700(self):
        os.chmod(self.cfg["runtimeDir"], 0o750)
        self.assertEqual(self.cli("current-release"), 1)
        self.assertIn("unsafe mode", self.err)

    def test_config_inside_candidate_dir_rejected(self):
        env = {"NEEV_UPSTREAM_CANDIDATE_DIR": os.path.join(self.root, "etc")}
        self.assertEqual(self.cli("current-release", env=env), 1)
        self.assertIn("must not live inside", self.err)

    def test_unknown_config_key_rejected(self):
        self.write_config(dict(self.cfg, extra=1))
        self.assertEqual(self.cli("current-release"), 1)


class CurrentReleaseTests(Harness):
    def test_prints_only_the_sha(self):
        self.assertEqual(self.cli("current-release"), 0, self.err)
        self.assertEqual(self.out.strip(), self.base)

    def test_running_image_mismatch_fails_closed(self):
        self.world.running = self.world.add_image("other:tag", {BACKEND: h(b"server")}, ["x"])
        self.assertEqual(self.cli("current-release"), 1)
        self.assertEqual(self.out, "")

    def test_moved_tag_fails_closed(self):
        self.world.images[OLD] = "sha256:" + "0" * 64
        self.assertEqual(self.cli("current-release"), 1)


class CandidateTests(Harness):
    def run_validate(self, cand):
        return self.cli("validate", env=self.hook_env(cand))

    def test_protected_path_rejected_before_any_container(self):
        self.assertEqual(self.run_validate(self.candidate("src/lib/core.ts", "evil\n")), 1)
        self.assertIn("outside the NON-CORE bound", self.err)
        self.assertFalse(self.world.builders)

    def test_symlink_in_allowlisted_path_rejected(self):
        self.assertEqual(self.run_validate(self.candidate("src/components/ui/evil.tsx", link="/etc/passwd")), 1)
        self.assertIn("non-regular", self.err)
        self.assertFalse(self.world.builders)

    def test_dependency_file_change_rejected(self):
        self.assertEqual(self.run_validate(self.candidate("pnpm-lock.yaml", "lock: 2\n")), 1)
        self.assertFalse(self.world.builders)

    def test_dependency_digest_mismatch_with_baseline_rejected(self):
        st = self.state()
        st["current"]["depsSha256"] = "1" * 64
        Path(self.cfg["runtimeDir"], "state.json").write_text(json.dumps(st))
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# new\n")), 1)
        self.assertIn("dependency", self.err)
        self.assertFalse(self.world.builders)

    def test_builder_lockfile_mismatch_rejected(self):
        self.world.meta[self.cfg["builderImageId"]]["files"]["/app/pnpm-lock.yaml"] = "2" * 64
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# new\n")), 1)
        self.assertIn("node_modules", self.err)
        self.assertFalse(self.world.builders)

    def test_base_mismatch_rejected(self):
        env = dict(self.hook_env(self.candidate("docs/url-scheme.md", "# x\n")), NEEV_UPSTREAM_BASE_SHA="a" * 40)
        self.assertEqual(self.cli("validate", env=env), 1)

    def test_existing_symlink_anywhere_in_source_rejected(self):
        git(self.repo, "checkout", "-q", "--detach", self.base)
        os.symlink("/etc/passwd", Path(self.repo, "public-link"))
        os.makedirs(Path(self.repo, "public"))
        os.rename(Path(self.repo, "public-link"), Path(self.repo, "public", "x.png"))
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "symlink")
        ctx = rel.Ctx(self.cfg, {}, runner=self.world.run)
        with self.assertRaisesRegex(rel.ReleaseError, "symlink"):
            rel.source_tar(ctx, git(self.repo, "rev-parse", "HEAD"))

    def test_validate_runs_fixed_commands_in_disposable_offline_builder(self):
        self.assertEqual(self.run_validate(self.candidate("src/components/ui/button.tsx", "export const B = 3\n")),
                         0, self.err)
        (name, b), = self.world.builders.items()
        self.assertTrue(name.startswith(rel.NAME_PREFIX) and b["removed"])
        self.assertIn("none", b["argv"][b["argv"].index("--network") + 1])
        self.assertEqual(self.world.node_runs, [rel.TSC, rel.VITEST + self.cfg["vitestTargets"]])

    def test_builder_is_capless_offline_and_unmounted(self):
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# x\n")), 0, self.err)
        argv = next(iter(self.world.builders.values()))["argv"]
        self.assertEqual(argv[argv.index("--cap-drop") + 1], "ALL")
        self.assertEqual(argv[argv.index("--network") + 1], "none")
        self.assertFalse({"-v", "--volume", "--mount", "--privileged", "--volumes-from"} & set(argv))

    def test_low_disk_fails_clearly_before_any_container(self):
        self.world.free = 2 * 1024 ** 3
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# x\n")), 1)
        self.assertIn("insufficient disk: 2.0 GiB free at /var/lib/docker, need 8 GiB", self.err)
        self.assertFalse(self.world.builders)

    def test_root_or_volume_builder_rejected(self):
        meta = self.world.meta[self.cfg["builderImageId"]]
        for user, volumes in (("", None), ("0:0", None), ("1000", {"/data": {}})):
            meta["user"], meta["volumes"] = user, volumes
            self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# %s\n" % user)), 1)
            self.assertFalse(self.world.builders)

    def test_git_uses_exact_safe_directory_and_no_archive(self):
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# x\n")), 0, self.err)
        for argv in self.world.git_argv:
            self.assertIn("safe.directory=" + self.cfg["repoPath"], argv)
            self.assertNotIn("safe.directory=*", argv)
            self.assertFalse({"archive", "checkout", "--textconv", "--filters"} & set(argv))

    def test_tampered_blob_rejected(self):
        oid = "a" * 40

        def runner(argv, **kw):
            return 0, ("%s blob 3\nbad\n" % oid).encode(), b""
        with self.assertRaisesRegex(rel.ReleaseError, "does not match"):
            rel.read_blobs(rel.Ctx(self.cfg, {}, runner=runner), [oid])

    def test_symlinked_repo_rejected(self):
        link = os.path.join(self.root, "repo-link.git")
        os.symlink(self.cfg["repoPath"], link)
        self.write_config(dict(self.cfg, repoPath=link))
        self.assertEqual(self.cli("current-release"), 1)
        self.assertIn("symlink", self.err)

    def test_only_allowed_env_reaches_hooks(self):
        cand = self.candidate("docs/url-scheme.md", "# x\n")
        env = dict(self.hook_env(cand), GIT_DIR="/evil", PYTHONPATH="/evil", LD_PRELOAD="/evil")
        self.assertEqual(self.cli("validate", env=env), 0, self.err)
        self.assertEqual(set(self.ctx_env) - set(rel.ALLOWED_ENV), set())

    def test_failing_tests_fail_and_remove_builder(self):
        self.world.fail_node = True
        self.assertEqual(self.run_validate(self.candidate("docs/url-scheme.md", "# x\n")), 1)
        self.assertTrue(all(b["removed"] for b in self.world.builders.values()))


class TarTests(unittest.TestCase):
    def check(self, data, tops=("out",)):
        with tarfile.open(fileobj=io.BytesIO(data)) as tf:
            return rel.validate_tar(tf, set(tops))

    def test_rejects_traversal_absolute_and_links(self):
        for entries in ([("out/../etc/x", b"x")], [("/out/x", b"x")], [("other/x", b"x")],
                        [("out/l", tarfile.SYMTYPE)], [("out/l", tarfile.LNKTYPE)], [("out/d", tarfile.CHRTYPE)]):
            with self.subTest(entries=entries[0][0]), self.assertRaises(rel.ReleaseError):
                self.check(tar_of(entries))

    def test_safe_extract_normalizes_modes(self):
        data = tar_of([("out/a/b.html", b"hi")])
        dest = os.path.join(tempfile.mkdtemp(), "x")
        with tarfile.open(fileobj=io.BytesIO(data)) as tf:
            rel.safe_extract(tf, rel.validate_tar(tf, {"out"}), dest)
        self.assertEqual(Path(dest, "out/a/b.html").read_bytes(), b"hi")
        shutil.rmtree(os.path.dirname(dest))


class GateTests(Harness):
    def test_clear(self):
        self.assertEqual(self.cli("active-runs"), 0, self.err)

    def test_active_sign_in_or_turn_blocks(self):
        for row in (b"18|1|0", b"18|0|2"):
            self.world.db = row
            self.assertEqual(self.cli("active-runs"), 1)

    def test_fail_closed_on_schema_garbage_and_errors(self):
        for row in (b"19|0|0", b"18|0", b"x|0|0", b"", None):
            self.world.db = row
            self.assertEqual(self.cli("active-runs"), 1, row)


class DeployTests(Harness):
    def test_build_is_pinned_base_plus_one_latest_web_layer(self):
        cand, _ = self.build_candidate()
        build = json.loads(Path(self.cfg["runtimeDir"], "runs", RUN, "build.json").read_text())
        rec = build["release"]
        self.assertEqual((rec["sourceSha"], rec["image"]), (cand, "neev-native-agent:upd-" + RUN.lower()))
        self.assertEqual(build["runtimeBaseImageId"], self.core_id)
        # FROM the core base, not the current image: old UI layers are not inherited.
        self.assertEqual(self.world.meta[rec["imageId"]]["layers"], ["base", "web-" + rec["image"]])
        self.assertEqual(self.world.builders["neev-upd-stage-" + RUN.lower()]["image"], self.core_id)
        self.assertTrue(all(b["removed"] for b in self.world.builders.values()))
        self.assertFalse(any(c[0] == "build" for c in self.world.calls))

    def test_daily_builds_do_not_accumulate_layers(self):
        self.build_candidate()
        first = json.loads(Path(self.cfg["runtimeDir"], "runs", RUN, "build.json").read_text())["release"]
        self.assertEqual(len(self.world.meta[first["imageId"]]["layers"]), 2)

    def test_run_artifacts_retain_three_prior_runs(self):
        runs = Path(self.cfg["runtimeDir"], "runs")
        for i in range(6):
            (runs / ("20260901T00000%dZ-1" % i)).mkdir(mode=0o700)
        self.build_candidate()
        self.assertEqual(sorted(p.name for p in runs.iterdir()),
                         ["20260901T000003Z-1", "20260901T000004Z-1", "20260901T000005Z-1", RUN])

    def test_updater_image_cannot_be_recorded_as_base(self):
        self.assertEqual(self.cli("record-current", "--source-sha", self.base, "--image", OLD,
                                  "--base-image", "neev-native-agent:upd-x"), 1)

    def test_moved_base_tag_blocks_build(self):
        cand = self.candidate("docs/url-scheme.md", "# x\n")
        self.world.images[CORE] = self.old_id
        self.assertEqual(self.cli("build", env=self.hook_env(cand)), 1)
        self.assertIn("runtime base", self.err)
        self.assertNotIn("neev-upd-build-" + RUN.lower(), self.world.builders)

    def test_base_backend_mismatch_blocks_build(self):
        cand = self.candidate("docs/url-scheme.md", "# x\n")
        self.world.meta[self.core_id]["files"][BACKEND] = h(b"other server")
        self.assertEqual(self.cli("build", env=self.hook_env(cand)), 1)
        self.assertIn("backend", self.err)

    def test_prune_keeps_current_previous_and_removes_own_stale(self):
        self.world.add_image("neev-native-agent:upd-stale", {}, ["base", "x"], labels=[rel.OWNER_LABEL])
        _, env = self.build_candidate()
        self.assertEqual(self.cli("deploy", env=env), 0, self.err)
        self.assertEqual(self.cli("health", env=env), 0, self.err)
        self.assertIn("neev-native-agent:upd-stale", self.world.removed_images)
        self.assertNotIn(OLD, self.world.removed_images)
        self.assertNotIn(CORE, self.world.removed_images)
        self.assertNotIn("neev-native-agent:upd-" + RUN.lower(), self.world.removed_images)

    def test_deploy_rechecks_gate_and_touches_nothing_when_busy(self):
        _, env = self.build_candidate()
        before = Path(self.compose).read_text()
        self.world.db = b"18|0|1"
        self.assertEqual(self.cli("deploy", env=env), 1)
        self.assertEqual(Path(self.compose).read_text(), before)
        self.assertIsNone(self.state().get("pending"))
        self.assertEqual(self.world.running, self.old_id)

    def test_deploy_refuses_ambiguous_compose(self):
        _, env = self.build_candidate()
        Path(self.compose).write_text(Path(self.compose).read_text() + "  other:\n    image: %s\n" % OLD)
        self.assertEqual(self.cli("deploy", env=env), 1)
        self.assertIsNone(self.state().get("pending"))

    def test_deploy_health_commit_keeps_previous(self):
        cand, env = self.build_candidate()
        self.assertEqual(self.cli("deploy", env=env), 0, self.err)
        new = "neev-native-agent:upd-" + RUN.lower()
        self.assertEqual(Path(self.compose).read_text().count(new), 1)
        self.assertNotIn(OLD, Path(self.compose).read_text())
        self.assertEqual(self.state()["pending"]["phase"], "started")
        self.assertEqual(self.cli("health", env=env), 0, self.err)
        st = self.state()
        self.assertIsNone(st["pending"])
        self.assertEqual((st["current"]["sourceSha"], st["previous"]["image"]), (cand, OLD))
        self.assertNotIn(OLD, self.world.removed_images)
        self.assertEqual(self.cli("current-release"), 0, self.err)
        self.assertEqual(self.out.strip(), cand)

    def test_health_failure_then_rollback_restores_image_only(self):
        _, env = self.build_candidate()
        self.assertEqual(self.cli("deploy", env=env), 0, self.err)
        self.world.health = 503
        self.assertEqual(self.cli("health", env=env), 1)
        self.assertIsNotNone(self.state()["pending"])
        self.world.health = 200
        self.assertEqual(self.cli("rollback", env=env), 0, self.err)
        st = self.state()
        self.assertIsNone(st["pending"])
        self.assertEqual(st["current"]["image"], OLD)
        self.assertEqual(st["history"][-1]["event"], "rolled_back")
        self.assertEqual(self.world.running, self.old_id)
        self.assertEqual(Path(self.compose).read_text().count(OLD), 1)
        self.assertFalse(any("psql" in " ".join(c) and "UPDATE" in " ".join(c) for c in self.world.calls))
        self.assertEqual(self.cli("current-release"), 0, self.err)
        self.assertEqual(self.out.strip(), self.base)

    def test_wrong_workspace_hash_is_unhealthy(self):
        _, env = self.build_candidate()
        self.assertEqual(self.cli("deploy", env=env), 0, self.err)
        new_id = self.world.running
        self.world.meta[new_id]["html"] = b"<html>stale cdn</html>"
        self.assertEqual(self.cli("health", env=env), 1)
        self.assertIn("workspace HTML", self.err)

    def test_failed_rollback_keeps_transaction_open_and_blocks_next_run(self):
        _, env = self.build_candidate()
        self.assertEqual(self.cli("deploy", env=env), 0, self.err)
        self.world.images[OLD] = "sha256:" + "0" * 64
        self.assertEqual(self.cli("rollback", env=env), 1)
        self.assertIsNotNone(self.state()["pending"])
        self.assertEqual(self.cli("current-release"), 1)
        self.assertIn("unresolved", self.err)


if __name__ == "__main__":
    unittest.main()
