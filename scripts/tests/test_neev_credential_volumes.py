"""Behavioral tests for Neev's bounded encrypted credential volumes."""

from pathlib import Path
import errno
import hashlib
import importlib.util
import json
import os
import stat
import sys
import tempfile
import unittest
from unittest import mock


OPERATIONS = Path(__file__).parents[1] / "operations"
STORE_PATH = OPERATIONS / "neev_credential_store.py"
VOLUMES_PATH = OPERATIONS / "neev_credential_volumes.py"


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class FixtureRunner:
    """Stateful fake for only the privileged command boundary."""

    def __init__(self, module, mapper_root):
        self.module = module
        self.mapper_root = mapper_root
        self.calls = []
        self.mapping = False
        self.mounted = False
        self.image = None
        self.mountpoint = None
        self.uuid = "12345678-1234-1234-1234-123456789abc"
        self.foreign_loop = False
        self.foreign_mount = False
        self.fail_program = None
        self.replace_mapping_on_failure = False
        self.replace_mapping_after_uuid = False
        self.busy_umount = False
        self.umount_error = None

    def run(self, argv, input_bytes=None):
        self.calls.append((list(argv), input_bytes))
        program = argv[0]
        if program == self.fail_program:
            if self.replace_mapping_on_failure:
                self.foreign_loop = True
            raise self.module.CommandError("failed")
        if argv[1:2] == ["luksFormat"]:
            self.image = Path(argv[-1])
            return b""
        if argv[1:2] == ["open"]:
            self.image = Path(argv[-2])
            self.mapping = True
            (self.mapper_root / argv[-1]).touch(mode=0o600)
            return b""
        if argv[1:2] == ["luksUUID"]:
            if self.replace_mapping_after_uuid:
                self.foreign_loop = True
            return (self.uuid + "\n").encode("ascii")
        if argv[1:2] == ["status"]:
            if not self.mapping:
                raise self.module.CommandError("failed", returncode=4)
            return (
                "/dev/mapper/" + argv[-1] + " is active.\n"
                "  type:    LUKS2\n"
                "  cipher:  aes-xts-plain64\n"
                "  keysize: 512 bits\n"
                "  device:  /dev/loop7\n"
                "  offset:  32768 sectors\n"
                "  size:    131072 sectors\n"
                "  mode:    read/write\n"
            ).encode("ascii")
        if program == self.module.LOSETUP:
            image_stat = self.image.stat()
            back_inode = image_stat.st_ino + (1 if self.foreign_loop else 0)
            payload = {
                "loopdevices": [{
                    "name": "/dev/loop7",
                    "back-file": str(self.image),
                    "back-ino": back_inode,
                    "back-maj:min": "%d:%d" % (
                        os.major(image_stat.st_dev), os.minor(image_stat.st_dev)
                    ),
                    "maj:min": "7:7",
                }]
            }
            return json.dumps(payload).encode("ascii")
        if program == self.module.FINDMNT:
            self.mountpoint = Path(argv[-1])
            filesystems = []
            if self.mounted:
                filesystems.append({
                    "target": str(self.mountpoint),
                    "source": (
                        "/dev/mapper/foreign" if self.foreign_mount
                        else str(self.mapper_root / (
                            "neev-credential-" + self.mountpoint.parent.name
                        ))
                    ),
                    "fstype": "ext4",
                    "options": "rw,nosuid,nodev,noexec,relatime",
                    "maj:min": "253:8" if self.foreign_mount else "253:7",
                })
            return json.dumps({"filesystems": filesystems}).encode("ascii")
        if program == self.module.MOUNT:
            self.mounted = True
            self.mountpoint = Path(argv[-1])
            return b""
        if program == self.module.UMOUNT:
            if self.umount_error is not None:
                raise self.umount_error
            if self.busy_umount:
                raise self.module.CommandError("failed", returncode=32)
            self.mounted = False
            return b""
        if argv[1:2] == ["close"]:
            self.mapping = False
            mapping_path = self.mapper_root / argv[-1]
            if mapping_path.exists():
                mapping_path.unlink()
            return b""
        if program == self.module.MKFS:
            return b""
        raise AssertionError("unexpected command: %r" % (argv,))


class CredentialVolumeTests(unittest.TestCase):
    def setUp(self):
        self.store_module = load_module("neev_credential_store", STORE_PATH)
        self.module = load_module("neev_credential_volumes", VOLUMES_PATH)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.root = self.base / "credentials"
        self.key_path = self.base / "keys" / "credential-master.key"
        self.mapper_root = self.base / "mapper"
        self.mapper_root.mkdir(mode=0o700)
        self.store = self.store_module.CredentialStore(self.root, self.key_path)
        self.store.initialize()
        self.scope = self.store_module.Scope(11, 22, "claude")
        self.runner = FixtureRunner(self.module, self.mapper_root)
        self.mapping_patch = mock.patch.object(self.module, "MAPPER_ROOT", self.mapper_root)
        self.mapping_patch.start()
        self.addCleanup(self.mapping_patch.stop)
        self.device_patch = mock.patch.object(
            self.module, "_mapping_major_minor", return_value="253:7"
        )
        self.device_patch.start()
        self.addCleanup(self.device_patch.stop)
        self.allocate_patch = mock.patch.object(
            self.module,
            "_preallocate",
            side_effect=lambda fd, offset, length: os.ftruncate(fd, offset + length),
        )
        self.allocate_patch.start()
        self.addCleanup(self.allocate_patch.stop)
        self.owner_patch = mock.patch.object(self.module, "_secure_filesystem_root")
        self.owner_patch.start()
        self.addCleanup(self.owner_patch.stop)
        self.mount_identity_patch = mock.patch.object(
            self.module,
            "_mountpoint_identity",
            side_effect=lambda unused_path: (
                (65532, 65532) if self.runner.mounted
                else (os.geteuid(), os.getegid())
            ),
            create=True,
        )
        self.mount_identity_patch.start()
        self.addCleanup(self.mount_identity_patch.stop)
        self.volumes = self.module.CredentialVolumes(self.store, self.runner)

    def create(self):
        return self.volumes.create(self.scope)

    def manifest(self, handle):
        with self.store.locked():
            return self.store.read_manifest(self.scope, handle)

    def expected_format_calls(self, handle):
        image = str(self.root / handle / "volume.luks")
        mapping = "neev-credential-" + handle
        return [
            [
                self.module.CRYPTSETUP, "luksFormat", "--type", "luks2",
                "--batch-mode", "--cipher", "aes-xts-plain64", "--key-size", "512",
                "--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "100000",
                "--key-file", "-", image,
            ],
            [
                self.module.CRYPTSETUP, "open", "--type", "luks2", "--key-file", "-",
                image, mapping,
            ],
            [
                self.module.MKFS, "-q", "-b", "4096", "-m", "0",
                str(self.mapper_root / mapping), "16384",
            ],
            [self.module.CRYPTSETUP, "luksUUID", image],
            [self.module.CRYPTSETUP, "status", mapping],
            [
                self.module.LOSETUP, "--json", "--list", "--output",
                "NAME,BACK-FILE,BACK-INO,BACK-MAJ:MIN,MAJ:MIN", "/dev/loop7",
            ],
            [self.module.CRYPTSETUP, "close", mapping],
        ]

    def test_create_uses_exact_commands_private_image_and_ready_uuid(self):
        handle = self.create()
        image = self.root / handle / "volume.luks"
        self.assertEqual(image.stat().st_size, 96 * 1024 * 1024)
        self.assertEqual(stat.S_IMODE(image.stat().st_mode), 0o600)
        self.assertEqual(image.stat().st_nlink, 1)
        self.assertEqual(self.manifest(handle)["phase"], "ready")
        self.assertEqual(self.manifest(handle)["luks_uuid"], self.runner.uuid)
        self.assertEqual([call[0] for call in self.runner.calls], self.expected_format_calls(handle))
        key_inputs = [value for argv, value in self.runner.calls if "--key-file" in argv]
        self.assertEqual(len(key_inputs), 2)
        self.assertEqual(len(key_inputs[0]), 32)
        self.assertEqual(key_inputs[0], key_inputs[1])
        for argv, unused_input in self.runner.calls:
            self.assertNotIn(key_inputs[0].hex(), " ".join(argv))

    def test_repeated_create_allocates_distinct_images_without_reuse(self):
        first = self.create()
        first_image = self.root / first / "volume.luks"
        marker = b"preserved"
        with first_image.open("r+b") as stream:
            stream.write(marker)
        second = self.create()
        self.assertNotEqual(first, second)
        self.assertEqual(first_image.read_bytes()[:len(marker)], marker)
        self.assertTrue((self.root / second / "volume.luks").is_file())

    def test_wrong_scope_refuses_before_any_tool_call(self):
        handle = self.create()
        self.runner.calls.clear()
        wrong = self.store_module.Scope(12, 22, "claude")
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.open(wrong, handle)
        self.assertEqual(caught.exception.code, "invalid")
        self.assertEqual(self.runner.calls, [])

    def test_open_reuses_ready_image_and_never_formats(self):
        handle = self.create()
        self.runner.calls.clear()
        reopened_store = self.store_module.CredentialStore(self.root, self.key_path)
        reopened = self.module.CredentialVolumes(reopened_store, self.runner)
        mountpoint = reopened.open(self.scope, handle)
        self.assertEqual(mountpoint, self.root / handle / "mount")
        commands = [argv for argv, unused in self.runner.calls]
        self.assertFalse(any(argv[0] == self.module.MKFS for argv in commands))
        self.assertFalse(any(argv[1:2] == ["luksFormat"] for argv in commands))
        self.assertIn([
            self.module.MOUNT, "-t", "ext4", "-o", "nodev,nosuid,noexec",
            str(self.mapper_root / ("neev-credential-" + handle)), str(mountpoint),
        ], commands)
        self.assertEqual(reopened.status(self.scope, handle), "open")

    def test_open_reuses_only_an_owned_existing_unmounted_mapping(self):
        handle = self.create()
        self.runner.mapping = True
        (self.mapper_root / ("neev-credential-" + handle)).touch(mode=0o600)
        self.runner.calls.clear()
        self.volumes.open(self.scope, handle)
        commands = [argv for argv, unused in self.runner.calls]
        self.assertFalse(any(argv[1:2] == ["open"] for argv in commands))
        self.assertEqual(self.volumes.status(self.scope, handle), "open")

    def test_tool_failure_preserves_provisioning_image_for_inspection(self):
        self.runner.fail_program = self.module.MKFS
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.create()
        self.assertEqual(caught.exception.code, "unavailable")
        entries = [path for path in self.root.iterdir() if path.name != ".lock"]
        self.assertEqual(len(entries), 1)
        handle = entries[0].name
        self.assertTrue((entries[0] / "volume.luks").is_file())
        self.assertEqual(self.manifest(handle)["phase"], "provisioning")
        self.assertFalse(self.runner.mapping)

    def test_failed_create_does_not_close_a_replaced_mapping(self):
        self.runner.fail_program = self.module.MKFS
        self.runner.replace_mapping_on_failure = True
        with self.assertRaises(self.store_module.StoreError):
            self.create()
        self.assertTrue(self.runner.mapping)
        self.assertFalse(any(
            argv[1:2] == ["close"] for argv, unused in self.runner.calls
        ))

    def test_success_path_revalidates_mapping_before_close(self):
        self.runner.replace_mapping_after_uuid = True
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.create()
        self.assertEqual(caught.exception.code, "conflict")
        self.assertTrue(self.runner.mapping)
        self.assertFalse(any(
            argv[1:2] == ["close"] for argv, unused in self.runner.calls
        ))

    def test_create_refuses_when_free_space_floor_would_be_crossed(self):
        fake_space = type("Space", (), {"f_bavail": 607, "f_frsize": 1024 * 1024})()
        with mock.patch.object(self.module.os, "statvfs", return_value=fake_space):
            with self.assertRaises(self.store_module.StoreError) as caught:
                self.create()
        self.assertEqual(caught.exception.code, "quota")
        self.assertEqual(self.runner.calls, [])
        self.assertEqual([p.name for p in self.root.iterdir()], [".lock"])

    def test_open_refuses_mismatched_luks_uuid_without_opening(self):
        handle = self.create()
        self.runner.calls.clear()
        self.runner.uuid = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.open(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.assertFalse(any(argv[1:2] == ["open"] for argv, unused in self.runner.calls))

    def test_foreign_existing_mapping_and_mount_fail_closed(self):
        handle = self.create()
        self.runner.mapping = True
        (self.mapper_root / ("neev-credential-" + handle)).touch(mode=0o600)
        self.runner.foreign_loop = True
        self.runner.calls.clear()
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.open(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.assertFalse(any(argv[0] == self.module.MOUNT for argv, unused in self.runner.calls))

        self.runner.foreign_loop = False
        self.runner.mounted = True
        self.runner.foreign_mount = True
        self.runner.calls.clear()
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.close(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.assertFalse(any(argv[0] == self.module.UMOUNT for argv, unused in self.runner.calls))

    def test_close_busy_mount_leaves_owned_volume_open(self):
        handle = self.create()
        self.volumes.open(self.scope, handle)
        self.runner.busy_umount = True
        self.runner.calls.clear()
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.close(self.scope, handle)
        self.assertEqual(caught.exception.code, "busy")
        self.assertTrue(self.runner.mounted)
        self.assertTrue(self.runner.mapping)
        self.assertFalse(any(argv[1:2] == ["close"] for argv, unused in self.runner.calls))

    def test_close_timeout_is_unavailable_not_misreported_as_busy(self):
        handle = self.create()
        self.volumes.open(self.scope, handle)
        self.runner.umount_error = self.module.CommandError("timeout")
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.close(self.scope, handle)
        self.assertEqual(caught.exception.code, "unavailable")
        self.assertTrue(self.runner.mounted)
        self.assertTrue(self.runner.mapping)

    def test_close_owned_volume_and_idempotent_locked_status(self):
        handle = self.create()
        self.assertEqual(self.volumes.status(self.scope, handle), "locked")
        self.volumes.open(self.scope, handle)
        self.volumes.close(self.scope, handle)
        self.assertFalse(self.runner.mounted)
        self.assertFalse(self.runner.mapping)
        self.assertEqual(self.volumes.status(self.scope, handle), "locked")
        self.volumes.close(self.scope, handle)

    def test_mounted_filesystem_root_requires_exact_service_uid_and_gid(self):
        handle = self.create()
        self.volumes.open(self.scope, handle)
        with mock.patch.object(
            self.module, "_mountpoint_identity", return_value=(0, 65532), create=True
        ):
            with self.assertRaises(self.store_module.StoreError) as caught:
                self.volumes.status(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.assertTrue(self.runner.mounted)
        self.assertTrue(self.runner.mapping)
        with mock.patch.object(
            self.module, "_mountpoint_identity", return_value=(65532, 0), create=True
        ):
            with self.assertRaises(self.store_module.StoreError) as caught:
                self.volumes.status(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.volumes.close(self.scope, handle)

    def test_nested_mount_is_refused_without_tool_mutation(self):
        handle = self.create()
        self.runner.calls.clear()
        mountpoint = self.root / handle / "mount"
        payload = {
            "filesystems": [{
                "target": str(mountpoint / "nested"), "source": "/dev/x",
                "fstype": "ext4", "options": "rw", "maj:min": "8:1",
            }]
        }
        original_run = self.runner.run

        def nested(argv, input_bytes=None):
            if argv[0] == self.module.FINDMNT:
                self.runner.calls.append((list(argv), input_bytes))
                return json.dumps(payload).encode("ascii")
            return original_run(argv, input_bytes)

        self.runner.run = nested
        with self.assertRaises(self.store_module.StoreError) as caught:
            self.volumes.open(self.scope, handle)
        self.assertEqual(caught.exception.code, "conflict")
        self.assertFalse(any(argv[1:2] == ["open"] for argv, unused in self.runner.calls))

    def test_status_parsers_reject_malformed_duplicate_and_ambiguous_output(self):
        invalid_status = (
            b"garbage\n",
            b"/dev/mapper/x is active.\n type: LUKS2\n device: /dev/loop1\n device: /dev/loop2\n mode: read/write\n",
            b"/dev/mapper/x is active.\n type: LUKS2\n device: ../../dev/loop1\n mode: read/write\n",
        )
        for raw in invalid_status:
            with self.subTest(raw=raw):
                with self.assertRaises(self.store_module.StoreError):
                    self.module._parse_crypt_status(raw, "x")
        invalid_json = (
            b"{",
            b'{"filesystems":[],"filesystems":[]}',
            b'{"filesystems":[{"target":"/x"}]}',
            b'{"loopdevices":[],"extra":true}',
        )
        for raw in invalid_json:
            with self.subTest(raw=raw):
                with self.assertRaises(self.store_module.StoreError):
                    self.module._parse_system_json(raw)


class BoundedRunnerTests(unittest.TestCase):
    def setUp(self):
        load_module("neev_credential_store", STORE_PATH)
        self.module = load_module("neev_credential_volumes", VOLUMES_PATH)
        self.runner = self.module.FixedToolRunner()

    def assert_command_error(self, code, operation):
        with self.assertRaises(self.module.CommandError) as caught:
            operation()
        self.assertEqual(caught.exception.kind, code)
        self.assertEqual(str(caught.exception), code)

    def test_internal_fixture_seam_caps_combined_output_and_kills_child_group(self):
        self.assert_command_error(
            "overflow",
            lambda: self.runner._run_for_test(
                [sys.executable, "-c", "import sys; sys.stdout.write('x'*70000)"]
            ),
        )

    def test_internal_fixture_seam_times_out_and_reaps_child(self):
        self.assert_command_error(
            "timeout",
            lambda: self.runner._run_for_test(
                [sys.executable, "-c", "import time; time.sleep(2)"], timeout=0.05
            ),
        )

    def test_post_spawn_transport_error_kills_group_after_leader_exits(self):
        real_selector_factory = self.module.selectors.DefaultSelector
        real_popen = self.module.subprocess.Popen
        captured = []
        descendant_pid = None

        class FailingSelector:
            def __init__(self):
                self.delegate = real_selector_factory()

            def register(self, *args, **kwargs):
                return self.delegate.register(*args, **kwargs)

            def unregister(self, *args, **kwargs):
                return self.delegate.unregister(*args, **kwargs)

            def get_map(self):
                return self.delegate.get_map()

            def select(self, timeout=None):
                events = self.delegate.select(timeout)
                if events:
                    raise OSError(errno.EIO, "injected transport failure")
                return events

            def close(self):
                self.delegate.close()

        def capture_popen(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            captured.append(process)
            return process

        with tempfile.TemporaryDirectory() as raw_temporary:
            pid_path = Path(raw_temporary).resolve() / "descendant.pid"
            script = (
                "import os,sys,time\n"
                "leader=os.getpid()\n"
                "child=os.fork()\n"
                "if child == 0:\n"
                " while os.getppid() == leader: time.sleep(0.01)\n"
                " os.write(1,b'x'); time.sleep(30); os._exit(0)\n"
                "with open(sys.argv[1],'w') as stream:\n"
                " stream.write(str(child)); stream.flush(); os.fsync(stream.fileno())\n"
                "os._exit(0)\n"
            )
            try:
                with mock.patch.object(
                    self.module.selectors, "DefaultSelector", FailingSelector
                ), mock.patch.object(
                    self.module.subprocess, "Popen", side_effect=capture_popen
                ):
                    try:
                        self.runner._run_for_test(
                            [sys.executable, "-c", script, str(pid_path)], timeout=2
                        )
                    except self.module.CommandError as error:
                        self.assertEqual(error.kind, "failed")
                    except OSError:
                        self.fail("post-spawn transport error was not made safe")
                    else:
                        self.fail("injected transport error was ignored")
                self.assertTrue(captured)
                self.assertEqual(captured[0].returncode, 0)
                descendant_pid = int(pid_path.read_text())
                deadline = self.module.time.monotonic() + 2
                while self.module.time.monotonic() < deadline:
                    try:
                        os.kill(descendant_pid, 0)
                    except ProcessLookupError:
                        break
                    self.module.time.sleep(0.01)
                else:
                    self.fail("descendant in the owned process group survived cleanup")
            finally:
                if captured:
                    try:
                        os.killpg(captured[0].pid, self.module.signal.SIGKILL)
                    except (OSError, ProcessLookupError):
                        pass
                    try:
                        captured[0].wait(timeout=1)
                    except Exception:
                        pass
                if descendant_pid is not None:
                    try:
                        os.kill(descendant_pid, self.module.signal.SIGKILL)
                    except (OSError, ProcessLookupError):
                        pass

    def test_internal_fixture_seam_maps_missing_executable_without_raw_error(self):
        self.assert_command_error(
            "missing",
            lambda: self.runner._run_for_test([str(Path(tempfile.gettempdir()) / "absent-neev-tool")]),
        )

    def test_internal_fixture_seam_drops_hostile_inherited_environment(self):
        with mock.patch.dict(os.environ, {"NEEV_HOSTILE_SECRET": "do-not-inherit"}):
            raw = self.runner._run_for_test([
                sys.executable, "-c",
                "import json,os; print(json.dumps(dict(os.environ),sort_keys=True))",
            ])
        child_env = json.loads(raw)
        self.assertNotIn("NEEV_HOSTILE_SECRET", child_env)
        self.assertEqual(child_env["PATH"], "/usr/sbin:/usr/bin")
        self.assertEqual(child_env["LC_ALL"], "C")

    def test_findmnt_no_match_is_a_valid_empty_fixed_schema_result(self):
        with mock.patch.object(self.runner, "_verify_tool"), mock.patch.object(
            self.runner,
            "_execute",
            side_effect=self.module.CommandError("failed", returncode=1),
        ):
            try:
                raw = self.runner.run([
                    self.module.FINDMNT, "--json", "--list", "--output",
                    "TARGET,SOURCE,FSTYPE,OPTIONS,MAJ:MIN", "--submounts", "/private/mount",
                ])
            except self.module.CommandError:
                self.fail("findmnt exit 1 must mean a valid empty result")
        self.assertEqual(json.loads(raw), {"filesystems": []})


class CredentialVolumeCliTests(unittest.TestCase):
    def setUp(self):
        self.store_module = load_module("neev_credential_store", STORE_PATH)
        self.module = load_module("neev_credential_volumes", VOLUMES_PATH)

    def test_malformed_arguments_return_only_bounded_safe_json(self):
        for argv in ([], ["open"], ["create", "--org-id", "secret"]):
            with self.subTest(argv=argv):
                output, exit_code = self.module.run_cli(argv, euid=0)
                payload = json.loads(output)
                self.assertEqual(exit_code, 2)
                self.assertEqual(set(payload), {"error"})
                self.assertIn(payload["error"], {"invalid", "unavailable", "conflict", "busy", "quota"})
                self.assertLessEqual(len(output), 128)
                self.assertNotIn("usage", output.lower())
                self.assertNotIn("secret", output)

    def test_non_root_is_refused_before_store_or_tool_use(self):
        output, exit_code = self.module.run_cli(["init"], euid=501)
        self.assertEqual((json.loads(output), exit_code), ({"error": "unavailable"}, 1))


@unittest.skipUnless(
    os.environ.get("NEEV_CREDENTIAL_VOLUME_ACCEPTANCE") == "1",
    "requires explicit isolated root Linux acceptance",
)
class LinuxCredentialVolumeAcceptance(unittest.TestCase):
    """Opt-in destructive fixture restricted to a newly invented private root."""

    def test_real_encrypted_reopen_capacity_and_key_refusal(self):
        if sys.platform != "linux" or os.geteuid() != 0:
            self.fail("acceptance requires Linux root")
        store_module = load_module("neev_credential_store", STORE_PATH)
        module = load_module("neev_credential_volumes", VOLUMES_PATH)
        fixture = Path(tempfile.mkdtemp(prefix="neev-volume-acceptance.", dir="/var/tmp")).resolve()
        fixture.chmod(0o700)
        root = fixture / "credentials"
        key_path = fixture / "keys" / "credential-master.key"
        scope = store_module.Scope(17001, 27001, "claude")
        wrong_scope = store_module.Scope(17002, 27001, "claude")
        marker = b"NEEV-INVENTED-VOLUME-ACCEPTANCE-9d5a2c7f"
        store = store_module.CredentialStore(root, key_path)
        volumes = module.CredentialVolumes(store, module.FixedToolRunner())
        handle = None
        cleanup_error = None
        print("retained acceptance fixture: %s" % fixture)
        try:
            store.initialize()
            handle = volumes.create(scope)
            image = root / handle / "volume.luks"
            with self.assertRaises(store_module.StoreError) as caught:
                volumes.open(wrong_scope, handle)
            self.assertEqual(caught.exception.code, "invalid")
            self.assertNotIn(marker, image.read_bytes())

            mountpoint = volumes.open(scope, handle)
            metadata = mountpoint.stat()
            self.assertEqual((metadata.st_uid, metadata.st_gid), (65532, 65532))
            self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o700)
            self.assertEqual(volumes.status(scope, handle), "open")
            marker_path = mountpoint / "invented-marker.bin"
            with marker_path.open("wb") as stream:
                stream.write(marker)
                stream.flush()
                os.fsync(stream.fileno())
            filler_path = mountpoint / "acceptance-filler.bin"
            filler_fd = os.open(str(filler_path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            filled = 0
            saw_enospc = False
            try:
                chunk = b"f" * (1024 * 1024)
                while filled <= 64 * 1024 * 1024:
                    try:
                        written = os.write(filler_fd, chunk)
                        if written <= 0:
                            self.fail("filler made no progress")
                        filled += written
                    except OSError as error:
                        if error.errno != errno.ENOSPC:
                            raise
                        saw_enospc = True
                        break
            finally:
                os.close(filler_fd)
            self.assertTrue(saw_enospc)
            self.assertLessEqual(filled, 64 * 1024 * 1024)
            filler_path.unlink()
            directory_fd = os.open(str(mountpoint), os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
            probe = mountpoint / "post-enospc-probe"
            probe.write_bytes(b"writable")
            volumes.close(scope, handle)
            self.assertNotIn(marker, image.read_bytes())

            reopened_store = store_module.CredentialStore(root, key_path)
            reopened = module.CredentialVolumes(reopened_store, module.FixedToolRunner())
            reopened_mountpoint = reopened.open(scope, handle)
            self.assertEqual((reopened_mountpoint / marker_path.name).read_bytes(), marker)
            reopened.close(scope, handle)

            key = key_path.read_bytes()
            image_digest = hashlib.sha256(image.read_bytes()).digest()
            manifest_path = root / handle / "manifest.json"
            manifest_bytes = manifest_path.read_bytes()
            saved_key = key_path.with_name("credential-master.key.acceptance-saved")
            key_path.rename(saved_key)
            try:
                with self.assertRaises(store_module.StoreError):
                    reopened.open(scope, handle)
                self.assertEqual(hashlib.sha256(image.read_bytes()).digest(), image_digest)
            finally:
                if saved_key.exists():
                    saved_key.rename(key_path)
            try:
                key_fd = os.open(str(key_path), os.O_WRONLY | os.O_TRUNC)
                try:
                    os.write(key_fd, b"x" * 32)
                    os.fsync(key_fd)
                finally:
                    os.close(key_fd)
                with self.assertRaises(store_module.StoreError):
                    reopened.open(scope, handle)
                self.assertEqual(hashlib.sha256(image.read_bytes()).digest(), image_digest)
                self.assertEqual(manifest_path.read_bytes(), manifest_bytes)
            finally:
                key_fd = os.open(str(key_path), os.O_WRONLY | os.O_TRUNC)
                try:
                    os.write(key_fd, key)
                    os.fsync(key_fd)
                finally:
                    os.close(key_fd)
        finally:
            if handle is not None:
                try:
                    cleanup_store = store_module.CredentialStore(root, key_path)
                    module.CredentialVolumes(
                        cleanup_store, module.FixedToolRunner()
                    ).close(scope, handle)
                except Exception:
                    cleanup_error = "mapping neev-credential-%s under %s" % (handle, fixture)
            if cleanup_error is not None:
                self.fail("cleanup failed; retained safe resource: %s" % cleanup_error)


if __name__ == "__main__":
    unittest.main()
