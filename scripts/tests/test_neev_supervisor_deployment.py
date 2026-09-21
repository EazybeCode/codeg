"""Tests for the supervisor's protected deployment construction.

Production runs as root 0:0 with a separate control group; these run as whoever
invokes them, so every test uses `DeploymentPolicy().for_current_process()` and
the module's ids stay parameters rather than constants. What is NOT relaxed is
the shape: the same modes, the same create-only behaviour, the same symlink
refusal, and the resulting deployments are handed to the fence's and the
transport's OWN validators rather than to assertions written here.

The Linux-only gate (`FenceOwner.open`, `SupervisorListener.adopt`) is proved on
the host; what is portable is everything up to it.
"""

import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_supervisor_deployment as deploy  # noqa: E402
import neev_supervisor_fence as fence_mod  # noqa: E402
import neev_supervisor_transport as transport  # noqa: E402


def policy():
    return deploy.DeploymentPolicy().for_current_process()


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        root = os.path.realpath(self._dir.name)
        self.paths = deploy.DeploymentPaths(
            fence_directory=os.path.join(root, "fence"),
            ipc_directory=os.path.join(root, "ipc"),
        )
        self.root = root
        self.addCleanup(self._dir.cleanup)

    def read(self, directory, name):
        with open(os.path.join(directory, name), "rb") as handle:
            return handle.read()

    def stat_of(self, directory, name):
        return os.lstat(os.path.join(directory, name))

    def fake_stat(self, *, mode, uid, gid, size=96, inode=1, nlink=1, device=7):
        """A stat result the transport's pure checkers can read.

        Production metadata is root-owned; these tests run unprivileged, so the
        POLICY is exercised over invented-but-realistic facts rather than over
        files this process could not possibly own.
        """
        return os.stat_result(
            (mode, inode, device, nlink, uid, gid, size, 0, 0, 0)
        )

    def control_like(self, parent_info, key_info):
        """A ControlDeployment describing the given metadata, production ids."""
        parent = transport.ProtectedParent(
            absolute_path=self.paths.ipc_directory,
            identity=transport.FileIdentity(parent_info.st_dev, parent_info.st_ino),
            gid=parent_info.st_gid,
            mode=parent_info.st_mode,
        )
        return transport.ControlDeployment(
            socket_parent=parent,
            key_parent=parent,
            key_identity=transport.FileIdentity(key_info.st_dev, key_info.st_ino),
            control_gid=deploy.DEFAULT_CONTROL_GID,
            self_uid=0,
            self_gid=0,
            peer_uid=deploy.DEFAULT_PEER_UID,
            peer_gid=deploy.DEFAULT_PEER_GID,
            socket_owner_uid=0,
        )

    # -- provisioning ---------------------------------------------------- #

    def test_provisions_both_directories_with_their_declared_shape(self):
        deploy.provision(self.paths, policy())
        fence_info = os.lstat(self.paths.fence_directory)
        ipc_info = os.lstat(self.paths.ipc_directory)
        # The fence is supervisor-private; the IPC directory is group-traversable.
        self.assertEqual(stat.S_IMODE(fence_info.st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(ipc_info.st_mode), 0o750)
        for info in (fence_info, ipc_info):
            self.assertFalse(info.st_mode & 0o022, "must not be group/other writable")
        lock = self.stat_of(self.paths.fence_directory, deploy.FENCE_LOCK_BASENAME)
        record = self.stat_of(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME)
        key = self.stat_of(self.paths.ipc_directory, deploy.KEY_BASENAME)
        self.assertEqual(stat.S_IMODE(lock.st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(record.st_mode), 0o600)
        # 0440: the control group reads the MAC key and nobody writes it.
        self.assertEqual(stat.S_IMODE(key.st_mode), 0o440)
        self.assertEqual(record.st_size, fence_mod.RECORD_SIZE)
        self.assertEqual(lock.st_size, 0)
        self.assertEqual(key.st_size, deploy.KEY_SIZE)

    def test_does_not_create_the_socket(self):
        deploy.provision(self.paths, policy())
        # The daemon binds and publishes it; provisioning must not leave a file
        # in its place for bind() to trip over.
        self.assertFalse(os.path.exists(deploy.socket_path(self.paths)))
        self.assertEqual(
            deploy.socket_path(self.paths),
            os.path.join(self.paths.ipc_directory, "control.sock"),
        )

    def test_the_fresh_record_is_the_never_registered_state(self):
        deploy.provision(self.paths, policy())
        raw = self.read(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME)
        # Parsed by the fence's own codec: epoch 0 with the zero boot id is the
        # only "no coordinator has ever registered" record it will accept.
        epoch, boot16 = fence_mod._parse_record(raw)
        self.assertEqual(epoch, 0)
        self.assertEqual(boot16, b"\x00" * 16)

    def test_reprovisioning_never_rewrites_a_key_or_an_epoch(self):
        deploy.provision(self.paths, policy())
        key = self.read(self.paths.ipc_directory, deploy.KEY_BASENAME)
        record_stat = self.stat_of(
            self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME
        )
        # Pretend a coordinator has since registered epoch 7.
        body = fence_mod.MARKER + (7).to_bytes(8, "big") + b"\x01" * 16
        import hashlib

        advanced = body + hashlib.sha256(body).digest()
        with open(
            os.path.join(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME),
            "r+b",
        ) as handle:
            handle.write(advanced)

        deploy.provision(self.paths, policy())

        # A restart must not roll the epoch back or mint a new MAC key out from
        # under a peer that is already using the old one.
        self.assertEqual(self.read(self.paths.ipc_directory, deploy.KEY_BASENAME), key)
        self.assertEqual(
            self.read(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME),
            advanced,
        )
        after = self.stat_of(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME)
        self.assertEqual((after.st_dev, after.st_ino), (record_stat.st_dev, record_stat.st_ino))

    def test_two_provisions_mint_two_different_keys_in_two_deployments(self):
        other = deploy.DeploymentPaths(
            fence_directory=os.path.join(self.root, "fence-b"),
            ipc_directory=os.path.join(self.root, "ipc-b"),
        )
        deploy.provision(self.paths, policy())
        deploy.provision(other, policy())
        self.assertNotEqual(
            self.read(self.paths.ipc_directory, deploy.KEY_BASENAME),
            self.read(other.ipc_directory, deploy.KEY_BASENAME),
        )

    # -- symlink refusal -------------------------------------------------- #

    def test_a_symlinked_directory_is_refused_rather_than_followed(self):
        real = os.path.join(self.root, "real")
        os.mkdir(real, 0o700)
        linked = os.path.join(self.root, "linked")
        os.symlink(real, linked)
        paths = deploy.DeploymentPaths(
            fence_directory=linked,
            ipc_directory=os.path.join(self.root, "ipc"),
        )
        # Following it would provision into a directory we never proved.
        with self.assertRaises(deploy.DeploymentError):
            deploy.provision(paths, policy())

    def test_a_symlinked_ancestor_is_refused(self):
        real = os.path.join(self.root, "real")
        os.makedirs(os.path.join(real, "inner"), 0o700)
        linked = os.path.join(self.root, "linked")
        os.symlink(real, linked)
        paths = deploy.DeploymentPaths(
            fence_directory=os.path.join(linked, "inner"),
            ipc_directory=os.path.join(self.root, "ipc"),
        )
        with self.assertRaises(deploy.DeploymentError):
            deploy.provision(paths, policy())

    def test_a_relative_or_odd_path_is_refused_before_any_syscall(self):
        for bad in ("relative/fence", "/", "/a//b", "/a/", "/a/../b", "/a/\x00b"):
            paths = deploy.DeploymentPaths(
                fence_directory=bad, ipc_directory=os.path.join(self.root, "ipc")
            )
            with self.assertRaises(deploy.DeploymentError):
                deploy.provision(paths, policy())

    # -- deployment construction ------------------------------------------ #

    def test_fence_deployment_describes_what_is_actually_on_disk(self):
        deploy.provision(self.paths, policy())
        built = deploy.fence_deployment(self.paths, policy())
        parent = os.lstat(self.paths.fence_directory)
        lock = self.stat_of(self.paths.fence_directory, deploy.FENCE_LOCK_BASENAME)
        record = self.stat_of(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME)
        self.assertEqual(built.parent_absolute_path, self.paths.fence_directory)
        self.assertEqual(
            built.parent_identity, fence_mod.Identity(parent.st_dev, parent.st_ino)
        )
        self.assertEqual(
            built.lock_identity, fence_mod.Identity(lock.st_dev, lock.st_ino)
        )
        self.assertEqual(
            built.record_identity, fence_mod.Identity(record.st_dev, record.st_ino)
        )
        self.assertNotEqual(built.lock_identity, built.record_identity)
        self.assertEqual(built.parent_mode, 0o700)
        # The fence's own validator is the real acceptance test for the shape.
        fence_mod._validate_deployment(built)

    def test_control_deployment_requires_a_root_owned_deployment(self):
        deploy.provision(self.paths, policy())
        if os.geteuid() == 0:
            built = deploy.control_deployment(self.paths, policy())
            self.assertEqual(built.socket_parent, built.key_parent)
            return
        # The transport hardcodes uid 0 for both the IPC directory and the key,
        # so an unprivileged deployment is refused here rather than at adopt
        # time. That refusal IS the contract, not a test limitation.
        with self.assertRaises(deploy.DeploymentError):
            deploy.control_deployment(self.paths, policy())

    def test_control_deployment_describes_the_real_key_and_one_shared_parent(self):
        deploy.provision(self.paths, policy())
        key = self.stat_of(self.paths.ipc_directory, deploy.KEY_BASENAME)
        parent = os.lstat(self.paths.ipc_directory)
        built = self.control_like(parent, key)
        self.assertEqual(
            built.key_identity, transport.FileIdentity(key.st_dev, key.st_ino)
        )
        # Socket and key live in the same protected directory.
        self.assertIs(built.socket_parent, built.key_parent)
        self.assertEqual(built.peer_uid, deploy.DEFAULT_PEER_UID)
        self.assertEqual(built.socket_owner_uid, 0)

    def test_the_transport_accepts_a_correctly_owned_deployment(self):
        # Portable proof of the happy path: the real checkers over root-owned
        # metadata, which is what production presents.
        parent = self.fake_stat(mode=0o040750, uid=0, gid=deploy.DEFAULT_CONTROL_GID)
        key = self.fake_stat(
            mode=0o100440, uid=0, gid=deploy.DEFAULT_CONTROL_GID, size=32, inode=2
        )
        deploy.verify_control(self.control_like(parent, key), parent, key)

    def test_the_transport_refuses_each_way_the_deployment_can_be_wrong(self):
        good_parent = self.fake_stat(
            mode=0o040750, uid=0, gid=deploy.DEFAULT_CONTROL_GID
        )
        good_key = self.fake_stat(
            mode=0o100440, uid=0, gid=deploy.DEFAULT_CONTROL_GID, size=32, inode=2
        )
        cases = {
            # Anyone could swap the key or the socket underneath us.
            "world-writable directory": (
                self.fake_stat(mode=0o040757, uid=0, gid=deploy.DEFAULT_CONTROL_GID),
                good_key,
            ),
            "group-writable directory": (
                self.fake_stat(mode=0o040770, uid=0, gid=deploy.DEFAULT_CONTROL_GID),
                good_key,
            ),
            "directory not owned by root": (
                self.fake_stat(mode=0o040750, uid=1, gid=deploy.DEFAULT_CONTROL_GID),
                good_key,
            ),
            "key readable by everyone": (
                good_parent,
                self.fake_stat(
                    mode=0o100444, uid=0, gid=deploy.DEFAULT_CONTROL_GID, size=32, inode=2
                ),
            ),
            "key writable": (
                good_parent,
                self.fake_stat(
                    mode=0o100640, uid=0, gid=deploy.DEFAULT_CONTROL_GID, size=32, inode=2
                ),
            ),
            "key not owned by root": (
                good_parent,
                self.fake_stat(
                    mode=0o100440, uid=1, gid=deploy.DEFAULT_CONTROL_GID, size=32, inode=2
                ),
            ),
            "key outside the control group": (
                good_parent,
                self.fake_stat(mode=0o100440, uid=0, gid=1, size=32, inode=2),
            ),
            "short key": (
                good_parent,
                self.fake_stat(
                    mode=0o100440, uid=0, gid=deploy.DEFAULT_CONTROL_GID, size=31, inode=2
                ),
            ),
            "hardlinked key": (
                good_parent,
                self.fake_stat(
                    mode=0o100440,
                    uid=0,
                    gid=deploy.DEFAULT_CONTROL_GID,
                    size=32,
                    inode=2,
                    nlink=2,
                ),
            ),
        }
        for name, (parent, key) in cases.items():
            with self.subTest(name):
                with self.assertRaises(deploy.DeploymentError):
                    deploy.verify_control(self.control_like(parent, key), parent, key)

    def test_a_deployment_is_refused_when_the_key_is_missing(self):
        deploy.provision(self.paths, policy())
        os.unlink(os.path.join(self.paths.ipc_directory, deploy.KEY_BASENAME))
        with self.assertRaises(deploy.DeploymentError):
            deploy.control_deployment(self.paths, policy())

    def test_a_deployment_is_refused_when_the_declared_owner_is_not_the_real_one(self):
        deploy.provision(self.paths, policy())
        # A policy claiming an owner the files do not have must not be described
        # as valid: the fence and the transport would refuse it at open time.
        from dataclasses import replace

        wrong = replace(policy(), supervisor_uid=os.geteuid() + 1)
        with self.assertRaises(deploy.DeploymentError):
            deploy.fence_deployment(self.paths, wrong)

    def test_a_deployment_is_refused_when_a_fence_file_is_missing(self):
        deploy.provision(self.paths, policy())
        os.unlink(os.path.join(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME))
        with self.assertRaises(deploy.DeploymentError):
            deploy.fence_deployment(self.paths, policy())

    def test_a_deployment_is_refused_when_the_record_is_the_wrong_length(self):
        deploy.provision(self.paths, policy())
        path = os.path.join(self.paths.fence_directory, deploy.FENCE_RECORD_BASENAME)
        with open(path, "r+b") as handle:
            handle.truncate(fence_mod.RECORD_SIZE - 1)
        # The fence reads a fixed-size record; a short one is corruption.
        with self.assertRaises(deploy.DeploymentError):
            deploy.fence_deployment(self.paths, policy())

    def test_a_hardlinked_key_is_refused_on_disk_too(self):
        deploy.provision(self.paths, policy())
        path = os.path.join(self.paths.ipc_directory, deploy.KEY_BASENAME)
        os.link(path, os.path.join(self.paths.ipc_directory, "second.key"))
        # A second name for the key is a second way to reach it.
        with self.assertRaises(deploy.DeploymentError):
            deploy.control_deployment(self.paths, policy())


class FromConfigTests(unittest.TestCase):
    """The adapter the daemon's wiring calls."""

    class Config:
        def __init__(self, **kwargs):
            self.socket_path = "/run/neev/ipc/control.sock"
            self.key_path = "/run/neev/ipc/control.key"
            self.fence_dir = "/var/lib/neev/fence"
            self.control_gid = 65532
            self.peer_uid = 65532
            self.peer_gid = 65532
            for key, value in kwargs.items():
                setattr(self, key, value)

    def test_derives_the_two_directories_and_the_production_ids(self):
        paths, pol = deploy.from_config(self.Config())
        self.assertEqual(paths.ipc_directory, "/run/neev/ipc")
        self.assertEqual(paths.fence_directory, "/var/lib/neev/fence")
        self.assertEqual(pol.control_gid, 65532)
        self.assertEqual((pol.peer_uid, pol.peer_gid), (65532, 65532))
        # The supervisor itself is root; that is not a config knob.
        self.assertEqual((pol.supervisor_uid, pol.supervisor_gid), (0, 0))
        self.assertEqual(deploy.socket_path(paths), "/run/neev/ipc/control.sock")

    def test_refuses_a_config_the_transport_could_not_honour(self):
        cases = {
            # One ProtectedParent describes both, so they must share a directory.
            "split directories": {"socket_path": "/run/neev/other/control.sock"},
            # `_load_verified_key` opens this name literally.
            "renamed key": {"key_path": "/run/neev/ipc/mac.key"},
            "renamed socket": {"socket_path": "/run/neev/ipc/ctl.sock"},
            # The control group must not be able to read the epoch record.
            "fence shared with ipc": {"fence_dir": "/run/neev/ipc"},
            "relative fence": {"fence_dir": "var/lib/neev/fence"},
            "traversal": {"key_path": "/run/neev/../ipc/control.key"},
        }
        for name, override in cases.items():
            with self.subTest(name):
                with self.assertRaises(deploy.DeploymentError):
                    deploy.from_config(self.Config(**override))


if __name__ == "__main__":
    unittest.main()
