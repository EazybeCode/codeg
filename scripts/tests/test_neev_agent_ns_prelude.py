"""Portable tests for the in-namespace bind prelude.

The real mount(2) calls need a Linux mount namespace + root (the sandbox
acceptance drives that); here the mount and exec are injected so the pure argv
parsing, path validation, and ordering (make-rprivate -> bind -> exec) are
verified on every platform."""

import stat
import sys
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_agent_ns_prelude as prelude  # noqa: E402


class _FakeDirStat:
    st_mode = stat.S_IFDIR | 0o700


def _dir_lstat(_path):
    return _FakeDirStat()


class PreludeTests(unittest.TestCase):
    def setUp(self):
        self.mounts = []
        self.execs = []

    def _run(self, argv, lstat_fn=_dir_lstat):
        prelude.run(
            argv,
            mount_fn=lambda *a: self.mounts.append(a),
            exec_fn=lambda path, args: self.execs.append((path, args)),
            lstat_fn=lstat_fn,
        )

    def test_makes_root_private_then_binds_then_execs_rest(self):
        src = "/var/lib/neev-supervisor/credentials/" + "a" * 32 + "/mount"
        dst = "/data/agents/run-abc/home"
        rest = ["/usr/bin/prlimit", "--cpu=1", "--", "/usr/bin/setpriv", "--", "codex"]
        self._run(["--src", src, "--dst", dst, "--"] + rest)
        # Order: MS_REC|MS_PRIVATE on "/" first, then MS_REC|MS_BIND src->dst.
        self.assertEqual(self.mounts[0], (None, "/", None, prelude.MS_REC | prelude.MS_PRIVATE, None))
        self.assertEqual(self.mounts[1], (src, dst, None, prelude.MS_REC | prelude.MS_BIND, None))
        # Then exec the rest with argv[0] preserved.
        self.assertEqual(self.execs, [(rest[0], rest)])

    def test_rejects_malformed_argv(self):
        rest = ["/usr/bin/prlimit"]
        for argv in (
            [],
            ["--src", "/a", "--dst", "/b"],  # no `--`
            ["--src", "/a", "--dst", "/b", "--"],  # empty rest
            ["--bad", "/a", "--dst", "/b", "--"] + rest,
            ["--src", "/a", "--wrong", "/b", "--"] + rest,
        ):
            with self.subTest(argv=argv):
                with self.assertRaises(prelude.PreludeError):
                    self._run(argv)
        self.assertEqual(self.mounts, [])

    def test_rejects_unsafe_paths(self):
        rest = ["/usr/bin/prlimit"]
        for src, dst in (
            ("relative/x", "/data/agents/run/home"),
            ("/a/../b", "/data/agents/run/home"),
            ("/ok", "/data/agents/run/home/"),  # trailing slash
            ("/ok\x00", "/data/agents/run/home"),
        ):
            with self.subTest(src=src, dst=dst):
                with self.assertRaises(prelude.PreludeError):
                    self._run(["--src", src, "--dst", dst, "--"] + rest)
        self.assertEqual(self.mounts, [])

    def test_rejects_symlinked_or_nondir_endpoints(self):
        rest = ["/usr/bin/prlimit"]

        class _LinkStat:
            st_mode = stat.S_IFLNK | 0o777

        with self.assertRaises(prelude.PreludeError):
            self._run(
                ["--src", "/src", "--dst", "/dst", "--"] + rest,
                lstat_fn=lambda _p: _LinkStat(),
            )
        self.assertEqual(self.mounts, [])

    def test_bind_never_runs_if_src_stat_fails(self):
        rest = ["/usr/bin/prlimit"]

        def _boom(_p):
            raise OSError("gone")

        with self.assertRaises(prelude.PreludeError):
            self._run(["--src", "/src", "--dst", "/dst", "--"] + rest, lstat_fn=_boom)
        self.assertEqual(self.mounts, [])
        self.assertEqual(self.execs, [])


if __name__ == "__main__":
    unittest.main()
