"""Behavioral tests for the persistent Neev credential-key custody store."""

from pathlib import Path
import hashlib
import hmac
import importlib.util
import json
import os
import stat
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "operations" / "neev_credential_store.py"


def load_store_module():
    spec = importlib.util.spec_from_file_location("neev_credential_store", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load credential store module")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class CredentialStoreTests(unittest.TestCase):
    def setUp(self):
        self.module = load_store_module()
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.root = self.base / "credentials"
        self.key_path = self.base / "keys" / "credential-master.key"
        self.store = self.module.CredentialStore(self.root, self.key_path)
        self.scope = self.module.Scope(11, 22, "claude")

    def initialize_and_allocate(self):
        self.store.initialize()
        with self.store.locked():
            return self.store.allocate(self.scope)

    def manifest_path(self, handle):
        return self.root / handle / "manifest.json"

    def replace_key(self, data):
        replacement = self.key_path.with_name("replacement")
        replacement.write_bytes(data)
        replacement.chmod(0o600)
        replacement.replace(self.key_path)

    def rewrite_manifest(self, handle, data):
        path = self.manifest_path(handle)
        path.write_bytes(data)
        path.chmod(0o600)

    def assert_store_error(self, expected_code, operation):
        with self.assertRaises(self.module.StoreError) as caught:
            operation()
        self.assertEqual(caught.exception.code, expected_code)
        self.assertEqual(str(caught.exception), expected_code)
        self.assertEqual(
            repr(caught.exception), f"StoreError('{expected_code}')"
        )
        self.assertIsNone(caught.exception.__cause__)

    def test_reopened_store_derives_same_volume_key_and_rejects_wrong_scope(self):
        handle = self.initialize_and_allocate()
        with self.store.locked():
            before = self.store.volume_key(self.scope, handle)

        reopened = self.module.CredentialStore(self.root, self.key_path)
        with reopened.locked():
            after = reopened.volume_key(self.scope, handle)
            self.assertTrue(hmac.compare_digest(after, before))
            self.assertEqual(len(after), 32)
            self.assert_store_error(
                "invalid",
                lambda: reopened.read_manifest(
                    self.module.Scope(12, 22, "claude"), handle
                ),
            )

    def test_initialize_replay_retains_key_and_existing_entry(self):
        handle = self.initialize_and_allocate()
        before_digest = hashlib.sha256(self.key_path.read_bytes()).digest()
        self.store.initialize()
        after_digest = hashlib.sha256(self.key_path.read_bytes()).digest()
        self.assertTrue(hmac.compare_digest(after_digest, before_digest))
        with self.store.locked():
            self.assertEqual(self.store.read_manifest(self.scope, handle)["handle"], handle)

    def test_created_state_has_exact_private_modes_and_no_stored_volume_key(self):
        self.store.initialize()
        with self.store.locked():
            first = self.store.allocate(self.scope)
            second = self.store.allocate(self.scope)
            first_key = self.store.volume_key(self.scope, first)
            second_key = self.store.volume_key(self.scope, second)
        self.assertEqual(stat.S_IMODE(self.root.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.key_path.parent.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.key_path.stat().st_mode), 0o600)
        self.assertEqual(self.key_path.stat().st_nlink, 1)
        self.assertEqual(stat.S_IMODE((self.root / first).stat().st_mode), 0o700)
        manifest_path = self.manifest_path(first)
        self.assertEqual(stat.S_IMODE(manifest_path.stat().st_mode), 0o600)
        self.assertEqual(manifest_path.stat().st_nlink, 1)
        self.assertEqual({path.name for path in (self.root / first).iterdir()}, {"manifest.json"})
        self.assertFalse(hmac.compare_digest(first_key, second_key))

    def test_only_valid_initialization_lock_is_not_credential_state(self):
        self.root.mkdir(mode=0o700)
        self.key_path.parent.mkdir(mode=0o700)
        lock_path = self.root / ".lock"
        lock_path.write_bytes(b"")
        lock_path.chmod(0o600)
        self.store.initialize()
        self.assertEqual(len(self.key_path.read_bytes()), 32)

    def test_malformed_initialization_lock_is_not_exempt_or_repaired(self):
        self.root.mkdir(mode=0o700)
        self.key_path.parent.mkdir(mode=0o700)
        lock_path = self.root / ".lock"
        lock_path.write_bytes(b"partial")
        lock_path.chmod(0o600)
        self.assert_store_error("unavailable", self.store.initialize)
        self.assertEqual(lock_path.read_bytes(), b"partial")
        self.assertFalse(self.key_path.exists())

    def test_missing_key_with_existing_entry_refuses_without_modifying_state(self):
        handle = self.initialize_and_allocate()
        self.key_path.unlink()
        manifest_before = self.manifest_path(handle).read_bytes()
        self.assert_store_error("conflict", self.store.initialize)
        self.assertFalse(self.key_path.exists())
        self.assertEqual(self.manifest_path(handle).read_bytes(), manifest_before)

    def test_unknown_partial_entry_counts_as_state_when_key_is_missing(self):
        self.root.mkdir(mode=0o700)
        self.key_path.parent.mkdir(mode=0o700)
        partial = self.root / "partial"
        partial.mkdir(mode=0o700)
        self.assert_store_error("conflict", self.store.initialize)
        self.assertTrue(partial.is_dir())
        self.assertFalse(self.key_path.exists())

    def test_missing_key_fails_normal_operations_without_reinitializing(self):
        handle = self.initialize_and_allocate()
        self.key_path.unlink()
        with self.store.locked():
            self.assert_store_error(
                "unavailable", lambda: self.store.volume_key(self.scope, handle)
            )
        self.assertFalse(self.key_path.exists())

    def test_truncated_key_is_rejected_and_not_replaced(self):
        self.store.initialize()
        self.key_path.write_bytes(b"short")
        self.key_path.chmod(0o600)
        self.assert_store_error("unavailable", self.store.initialize)
        self.assertEqual(self.key_path.read_bytes(), b"short")

    def test_changed_key_invalidates_existing_manifest(self):
        handle = self.initialize_and_allocate()
        self.replace_key(os.urandom(32))
        with self.store.locked():
            self.assert_store_error(
                "invalid", lambda: self.store.volume_key(self.scope, handle)
            )

    def test_key_must_be_nonempty_regular_private_single_link_file(self):
        mutations = (
            "empty",
            "overlong",
            "directory",
            "fifo",
            "world-readable",
            "hardlink",
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with tempfile.TemporaryDirectory() as raw_base:
                    base = Path(raw_base).resolve()
                    root = base / "credentials"
                    key_path = base / "keys" / "master.key"
                    store = self.module.CredentialStore(root, key_path)
                    store.initialize()
                    key_path.unlink()
                    if mutation == "empty":
                        key_path.write_bytes(b"")
                        key_path.chmod(0o600)
                    elif mutation == "overlong":
                        key_path.write_bytes(b"x" * 33)
                        key_path.chmod(0o600)
                    elif mutation == "directory":
                        key_path.mkdir(mode=0o700)
                    elif mutation == "fifo":
                        os.mkfifo(key_path, 0o600)
                    elif mutation == "world-readable":
                        key_path.write_bytes(os.urandom(32))
                        key_path.chmod(0o604)
                    else:
                        source = base / "linked-key"
                        source.write_bytes(os.urandom(32))
                        source.chmod(0o600)
                        os.link(source, key_path)
                    self.assert_store_error("unavailable", store.initialize)

    def test_key_and_path_symlinks_are_rejected(self):
        self.store.initialize()
        target = self.base / "target-key"
        target.write_bytes(os.urandom(32))
        target.chmod(0o600)
        self.key_path.unlink()
        self.key_path.symlink_to(target)
        self.assert_store_error("unavailable", self.store.initialize)

        other = self.base / "other"
        other.mkdir(mode=0o700)
        symlink_root = self.base / "root-link"
        symlink_root.symlink_to(other, target_is_directory=True)
        symlink_store = self.module.CredentialStore(
            symlink_root, self.base / "other-keys" / "master.key"
        )
        self.assert_store_error("unavailable", symlink_store.initialize)

    def test_existing_private_directories_are_not_mode_repaired(self):
        self.root.mkdir(mode=0o755)
        self.root.chmod(0o755)
        self.key_path.parent.mkdir(mode=0o700)
        self.assert_store_error("unavailable", self.store.initialize)
        self.assertEqual(stat.S_IMODE(self.root.stat().st_mode), 0o755)

    def test_untrusted_writable_ancestor_is_rejected_but_sticky_temp_is_allowed(self):
        unsafe = self.base / "unsafe"
        unsafe.mkdir(mode=0o777)
        unsafe.chmod(0o777)
        unsafe_store = self.module.CredentialStore(
            unsafe / "credentials", unsafe / "keys" / "master.key"
        )
        self.assert_store_error("unavailable", unsafe_store.initialize)
        self.assertEqual(stat.S_IMODE(unsafe.stat().st_mode), 0o777)

        unsafe.chmod(0o1777)
        unsafe_store.initialize()
        self.assertEqual(len((unsafe / "keys" / "master.key").read_bytes()), 32)

    def test_scope_validation_rejects_bool_range_provider_and_wrong_types(self):
        self.store.initialize()
        invalid_scopes = (
            self.module.Scope(True, 22, "claude"),
            self.module.Scope(0, 22, "claude"),
            self.module.Scope(2147483648, 22, "claude"),
            self.module.Scope(11, False, "claude"),
            self.module.Scope(11, 22, "openai"),
            self.module.Scope("11", 22, "claude"),
        )
        with self.store.locked():
            for scope in invalid_scopes:
                with self.subTest(scope=scope):
                    self.assert_store_error(
                        "invalid", lambda scope=scope: self.store.allocate(scope)
                    )

    def test_handle_validation_rejects_traversal_uppercase_and_wrong_types(self):
        self.store.initialize()
        invalid_handles = (
            "../" + "a" * 29,
            "A" * 32,
            "a" * 31,
            "g" * 32,
            123,
        )
        with self.store.locked():
            for handle in invalid_handles:
                with self.subTest(handle=handle):
                    self.assert_store_error(
                        "invalid", lambda handle=handle: self.store.read_manifest(self.scope, handle)
                    )

    def test_wrong_org_user_and_provider_are_rejected(self):
        handle = self.initialize_and_allocate()
        wrong_scopes = (
            self.module.Scope(12, 22, "claude"),
            self.module.Scope(11, 23, "claude"),
            self.module.Scope(11, 22, "codex"),
        )
        with self.store.locked():
            for scope in wrong_scopes:
                with self.subTest(scope=scope):
                    self.assert_store_error(
                        "invalid", lambda scope=scope: self.store.read_manifest(scope, handle)
                    )

    def test_manifest_schema_phase_and_uuid_are_strict(self):
        handle = self.initialize_and_allocate()
        invalid_updates = (
            ("unknown", None),
            ("provisioning", "12345678-1234-1234-1234-123456789abc"),
            ("ready", None),
            ("ready", "12345678123412341234123456789abc"),
            ("ready", "12345678-1234-1234-1234-123456789ABC"),
            ("ready", 123),
        )
        with self.store.locked():
            for phase, luks_uuid in invalid_updates:
                with self.subTest(phase=phase, luks_uuid=luks_uuid):
                    self.assert_store_error(
                        "invalid",
                        lambda phase=phase, luks_uuid=luks_uuid: self.store.write_manifest(
                            self.scope, handle, phase, luks_uuid
                        ),
                    )
            self.store.write_manifest(
                self.scope,
                handle,
                "ready",
                "12345678-1234-1234-1234-123456789abc",
            )
            self.assertEqual(self.store.read_manifest(self.scope, handle)["phase"], "ready")

    def test_corrupt_duplicate_unknown_and_oversize_manifests_fail_closed(self):
        corruptions = {
            "empty": b"",
            "malformed": b"{",
            "duplicate": b'{"version":1,"version":1}',
            "unknown-field": b'{"unexpected":true}',
            "oversize": b"x" * 4097,
        }
        for label, data in corruptions.items():
            with self.subTest(label=label):
                handle = self.initialize_and_allocate()
                self.rewrite_manifest(handle, data)
                with self.store.locked():
                    self.assert_store_error(
                        "invalid", lambda: self.store.read_manifest(self.scope, handle)
                    )

    def test_manifest_tampering_and_noncanonical_field_types_fail_closed(self):
        field_values = {
            "version": True,
            "org_id": True,
            "user_id": "22",
            "provider": 1,
            "handle": "A" * 32,
            "phase": "done",
            "luks_uuid": "not-a-uuid",
            "mac": "A" * 64,
        }
        for field, value in field_values.items():
            with self.subTest(field=field):
                handle = self.initialize_and_allocate()
                path = self.manifest_path(handle)
                manifest = json.loads(path.read_text())
                manifest[field] = value
                self.rewrite_manifest(
                    handle,
                    json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode(),
                )
                with self.store.locked():
                    self.assert_store_error(
                        "invalid", lambda: self.store.read_manifest(self.scope, handle)
                    )

    def test_manifest_must_be_regular_private_single_link_file(self):
        mutations = ("symlink", "hardlink", "directory", "fifo", "world-readable")
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                handle = self.initialize_and_allocate()
                path = self.manifest_path(handle)
                original = path.read_bytes()
                path.unlink()
                if mutation == "symlink":
                    target = self.base / ("manifest-target-" + handle)
                    target.write_bytes(original)
                    target.chmod(0o600)
                    path.symlink_to(target)
                elif mutation == "hardlink":
                    target = self.base / ("manifest-target-" + handle)
                    target.write_bytes(original)
                    target.chmod(0o600)
                    os.link(target, path)
                elif mutation == "directory":
                    path.mkdir(mode=0o700)
                elif mutation == "fifo":
                    os.mkfifo(path, 0o600)
                else:
                    path.write_bytes(original)
                    path.chmod(0o604)
                with self.store.locked():
                    self.assert_store_error(
                        "invalid", lambda: self.store.read_manifest(self.scope, handle)
                    )

    def test_entry_directory_symlink_is_rejected(self):
        handle = self.initialize_and_allocate()
        external = self.base / "external-entry"
        (self.root / handle).replace(external)
        (self.root / handle).symlink_to(external, target_is_directory=True)
        with self.store.locked():
            self.assert_store_error(
                "invalid", lambda: self.store.read_manifest(self.scope, handle)
            )

    def test_unverified_existing_manifest_is_never_overwritten(self):
        handle = self.initialize_and_allocate()
        path = self.manifest_path(handle)
        corrupted = b'{"corrupted":true}'
        self.rewrite_manifest(handle, corrupted)
        with self.store.locked():
            self.assert_store_error(
                "invalid",
                lambda: self.store.write_manifest(
                    self.scope,
                    handle,
                    "ready",
                    "12345678-1234-1234-1234-123456789abc",
                ),
            )
        self.assertEqual(path.read_bytes(), corrupted)

    def test_allocation_enforces_quota_and_counts_unknown_entries(self):
        self.store.initialize()
        for number in range(64):
            (self.root / f"unknown-{number}").mkdir(mode=0o700)
        with self.store.locked():
            self.assert_store_error("quota", lambda: self.store.allocate(self.scope))
        self.assertEqual(len([p for p in self.root.iterdir() if p.name != ".lock"]), 64)

    def test_second_store_fails_busy_instead_of_waiting(self):
        self.store.initialize()
        second = self.module.CredentialStore(self.root, self.key_path)
        with self.store.locked():
            self.assert_store_error("busy", lambda: second.locked().__enter__())

    def test_metadata_operations_require_the_exclusive_lock(self):
        self.store.initialize()
        self.assert_store_error("busy", lambda: self.store.allocate(self.scope))

    def test_initialize_fsync_failure_preserves_created_key_for_retry(self):
        self.root.mkdir(mode=0o700)
        self.key_path.parent.mkdir(mode=0o700)
        lock_path = self.root / ".lock"
        lock_path.write_bytes(b"")
        lock_path.chmod(0o600)
        original_fsync = os.fsync
        calls = 0

        def fail_directory_flush(fd):
            nonlocal calls
            calls += 1
            if calls == 2:
                raise OSError("injected fsync failure")
            return original_fsync(fd)

        with mock.patch.object(self.module.os, "fsync", side_effect=fail_directory_flush):
            self.assert_store_error("unavailable", self.store.initialize)
        before_digest = hashlib.sha256(self.key_path.read_bytes()).digest()
        self.store.initialize()
        after_digest = hashlib.sha256(self.key_path.read_bytes()).digest()
        self.assertTrue(hmac.compare_digest(after_digest, before_digest))

    def test_manifest_write_failure_preserves_old_authenticated_manifest(self):
        handle = self.initialize_and_allocate()
        before = self.manifest_path(handle).read_bytes()
        with self.store.locked():
            with mock.patch.object(
                self.module.os, "write", side_effect=OSError("injected write failure")
            ):
                self.assert_store_error(
                    "unavailable",
                    lambda: self.store.write_manifest(
                        self.scope,
                        handle,
                        "ready",
                        "12345678-1234-1234-1234-123456789abc",
                    ),
                )
            self.assertEqual(self.store.read_manifest(self.scope, handle)["phase"], "provisioning")
        self.assertEqual(self.manifest_path(handle).read_bytes(), before)
        self.assertGreaterEqual(len(list((self.root / handle).iterdir())), 2)

    def test_allocation_fsync_failure_preserves_partial_entry(self):
        self.store.initialize()
        with self.store.locked():
            with mock.patch.object(
                self.module.os, "fsync", side_effect=OSError("injected fsync failure")
            ):
                self.assert_store_error("unavailable", lambda: self.store.allocate(self.scope))
        entries = [path for path in self.root.iterdir() if path.name != ".lock"]
        self.assertEqual(len(entries), 1)
        self.assertTrue(entries[0].is_dir())

    def test_store_error_only_accepts_fixed_safe_codes(self):
        for code in ("invalid", "unavailable", "conflict", "busy", "quota"):
            self.assertEqual(self.module.StoreError(code).code, code)
        with self.assertRaises(ValueError):
            self.module.StoreError("secret detail")


if __name__ == "__main__":
    unittest.main()
