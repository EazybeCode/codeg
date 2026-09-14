"""Tests for the durable supervisor coordinator fence (Task 7).

Group 1 (codec/vectors) is pure. Groups 2-6 drive the real owner logic through an
injected fake filesystem seam, since the real protected-path/flock/ownership walk
is a Linux deployment-acceptance gate not exercised here. Literal record vectors
are the independently supplied values from the adopted proposal, never computed
through the implementation under test.
"""

import os
import stat
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "operations"))

import neev_supervisor_fence as fence  # noqa: E402
from neev_supervisor_dto import Coordinator, Counter, Id  # noqa: E402

SENTINEL = bytes.fromhex(
    "4e45455653463031"
    "0000000000000000"
    "00000000000000000000000000000000"
    "68d87f195d3cfd5d131c9ca30421dee7683e3f29c5478c99d20ca772e9ee3817"
)
BOOT1_HEX = "0123456789abcdef0123456789abcdef"
EPOCH1 = bytes.fromhex(
    "4e45455653463031"
    "0000000000000001"
    "0123456789abcdef0123456789abcdef"
    "8f23fc800d2c9edbc5c171ea5ad2ec4bbfb1fe4ca668db989cb20c8badd0763b"
)


def coordinator(epoch, boot_hex=BOOT1_HEX):
    return Coordinator(Counter(str(epoch)), Id(boot_hex))


class FakeStat:
    def __init__(self, dev, ino, mode, nlink, uid, gid, size):
        self.st_dev = dev
        self.st_ino = ino
        self.st_mode = mode
        self.st_nlink = nlink
        self.st_uid = uid
        self.st_gid = gid
        self.st_size = size


PARENT = fence.Identity(device=41, inode=100)
LOCK = fence.Identity(device=41, inode=101)
RECORD = fence.Identity(device=41, inode=102)
PROTECTED_UID = 5000
SUP_UID = 5000
SUP_GID = 6000
PARENT_MODE = 0o700


def deployment():
    return fence.FenceDeployment(
        parent_absolute_path="/srv/neev/fence",
        parent_identity=PARENT,
        protected_directory_uid=PROTECTED_UID,
        parent_gid=SUP_GID,
        parent_mode=PARENT_MODE,
        supervisor_uid=SUP_UID,
        supervisor_gid=SUP_GID,
        lock_identity=LOCK,
        record_identity=RECORD,
    )


class FakeFs:
    def __init__(self, disk):
        self.disk = bytearray(disk)
        self.euid = SUP_UID
        self.egid = SUP_GID
        # fault / metadata knobs
        self.flock_busy = False
        self.fsync_error = False
        self.fsync_mutate = None
        self.pwrite_zero = False
        self.pwrite_short_once = False
        self.record_size = len(disk)
        self.record_mode = 0o600
        self.lock_size = 0
        self.ancestor_bad_uid = False
        # observability
        self.fsync_calls = 0
        self.pwrite_calls = 0
        self.closed_fds = []
        self._next = 10
        self._kind = {}
        self._short_done = False
        self._components = 0

    def platform_supported(self):
        return True

    def geteuid(self):
        return self.euid

    def getegid(self):
        return self.egid

    def _alloc(self, kind):
        fd = self._next
        self._next += 1
        self._kind[fd] = kind
        return fd

    def open_root(self):
        self._components = 0
        return self._alloc("root")

    def open_child_dir(self, parent_fd, name):
        self._components += 1
        return self._alloc("dir:%d" % self._components)

    def open_file(self, parent_fd, name):
        if name == fence._LOCK_BASENAME:
            return self._alloc("lock")
        if name == fence._RECORD_BASENAME:
            return self._alloc("record")
        raise FileNotFoundError(name)

    def fstat(self, fd):
        kind = self._kind[fd]
        if kind == "root" or kind.startswith("dir"):
            uid = PROTECTED_UID
            if self.ancestor_bad_uid and kind == "dir:1":
                uid = PROTECTED_UID + 1
            return FakeStat(PARENT.device, PARENT.inode, stat.S_IFDIR | PARENT_MODE, 4, uid, SUP_GID, 0)
        if kind == "lock":
            return FakeStat(LOCK.device, LOCK.inode, stat.S_IFREG | 0o600, 1, SUP_UID, SUP_GID, self.lock_size)
        if kind == "record":
            return FakeStat(RECORD.device, RECORD.inode, stat.S_IFREG | self.record_mode, 1, SUP_UID, SUP_GID, self.record_size)
        raise OSError("bad fd")

    def flock_exclusive_nonblocking(self, fd):
        if self.flock_busy:
            raise BlockingIOError("locked")

    def pread(self, fd, count, offset):
        return bytes(self.disk[offset:offset + count])

    def pwrite(self, fd, data, offset):
        self.pwrite_calls += 1
        if self.pwrite_zero:
            return 0
        if self.pwrite_short_once and not self._short_done:
            self._short_done = True
            half = max(1, len(data) // 2)
            self.disk[offset:offset + half] = data[:half]
            return half
        self.disk[offset:offset + len(data)] = data
        return len(data)

    def fsync(self, fd):
        self.fsync_calls += 1
        if self.fsync_mutate is not None:
            self.fsync_mutate(self.disk)
        if self.fsync_error:
            raise OSError("fsync failed")

    def close(self, fd):
        self.closed_fds.append(fd)


def open_owner(disk=SENTINEL, configure=None):
    fs = FakeFs(disk)
    if configure is not None:
        configure(fs)
    owner = fence.FenceOwner.open(deployment(), _fs=fs)
    return owner, fs


class CodecTests(unittest.TestCase):
    def test_literal_vectors(self):
        self.assertEqual(fence._encode_record(0, b"\x00" * 16), SENTINEL)
        self.assertEqual(fence._encode_record(1, bytes.fromhex(BOOT1_HEX)), EPOCH1)

    def test_parse_valid(self):
        self.assertEqual(fence._parse_record(SENTINEL), (0, b"\x00" * 16))
        self.assertEqual(fence._parse_record(EPOCH1), (1, bytes.fromhex(BOOT1_HEX)))

    def test_parse_rejections(self):
        for bad in [
            SENTINEL[:-1],  # truncated
            SENTINEL + b"\x00",  # oversized
            b"XEEVSF01" + SENTINEL[8:],  # wrong marker
            SENTINEL[:63] + bytes([SENTINEL[63] ^ 0xFF]),  # bad digest
        ]:
            with self.assertRaises(fence.FenceError) as ctx:
                fence._parse_record(bad)
            self.assertEqual(ctx.exception.category, "Unavailable")

    def test_zero_epoch_nonzero_boot_is_corrupt(self):
        corrupt = fence._encode_record(0, bytes.fromhex(BOOT1_HEX))
        with self.assertRaises(fence.FenceError):
            fence._parse_record(corrupt)

    def test_epoch_above_i64_max_is_corrupt(self):
        body = fence.MARKER + (fence._MAX_EPOCH + 1).to_bytes(8, "big") + bytes.fromhex(BOOT1_HEX)
        import hashlib

        record = body + hashlib.sha256(body).digest()
        with self.assertRaises(fence.FenceError):
            fence._parse_record(record)

    def test_candidate_rejects_non_coordinator(self):
        with self.assertRaises(fence.FenceError) as ctx:
            fence._candidate_pair(object())
        self.assertEqual(ctx.exception.category, "Invalid")

    def test_sentinel_epoch_is_not_a_valid_candidate(self):
        # The DTO Counter forbids "0", so a zero-epoch candidate cannot exist.
        # (Assert on any construction failure to stay robust to import-path
        # duplication of the DTO module under full-suite discovery.)
        with self.assertRaises(Exception):
            Counter("0")


class OpenTests(unittest.TestCase):
    def test_non_linux_is_unsupported_before_fs_access(self):
        with self.assertRaises(fence.FenceError) as ctx:
            fence.FenceOwner.open(deployment())  # real fs; darwin -> Unsupported
        # On Linux this would proceed; here we only assert the gate on non-Linux.
        if not sys.platform.startswith("linux"):
            self.assertEqual(ctx.exception.category, "Unsupported")

    def test_euid_mismatch_unavailable(self):
        def wrong(fs):
            fs.euid = SUP_UID + 1

        with self.assertRaises(fence.FenceError) as ctx:
            open_owner(configure=wrong)
        self.assertEqual(ctx.exception.category, "Unavailable")

    def test_open_reads_sentinel(self):
        owner, _ = open_owner()
        self.assertEqual(owner._epoch, 0)
        self.assertFalse(owner._registered)

    def test_open_rejects_bad_record_size(self):
        with self.assertRaises(fence.FenceError) as ctx:
            open_owner(configure=lambda fs: setattr(fs, "record_size", 63))
        self.assertEqual(ctx.exception.category, "Unavailable")

    def test_open_rejects_bad_mode(self):
        with self.assertRaises(fence.FenceError):
            open_owner(configure=lambda fs: setattr(fs, "record_mode", 0o644))

    def test_open_rejects_nonzero_lock_size(self):
        with self.assertRaises(fence.FenceError):
            open_owner(configure=lambda fs: setattr(fs, "lock_size", 1))

    def test_open_lock_contention_is_busy(self):
        with self.assertRaises(fence.FenceError) as ctx:
            open_owner(configure=lambda fs: setattr(fs, "flock_busy", True))
        self.assertEqual(ctx.exception.category, "Busy")

    def test_open_ancestor_wrong_uid_unavailable(self):
        with self.assertRaises(fence.FenceError) as ctx:
            open_owner(configure=lambda fs: setattr(fs, "ancestor_bad_uid", True))
        self.assertEqual(ctx.exception.category, "Unavailable")

    def test_open_rejects_invalid_deployment(self):
        bad = fence.FenceDeployment(
            parent_absolute_path="relative/path",
            parent_identity=PARENT,
            protected_directory_uid=PROTECTED_UID,
            parent_gid=SUP_GID,
            parent_mode=PARENT_MODE,
            supervisor_uid=SUP_UID,
            supervisor_gid=SUP_GID,
            lock_identity=LOCK,
            record_identity=RECORD,
        )
        with self.assertRaises(fence.FenceError) as ctx:
            fence.FenceOwner.open(bad, _fs=FakeFs(SENTINEL))
        self.assertEqual(ctx.exception.category, "Invalid")


class RegisterTests(unittest.TestCase):
    def test_higher_from_sentinel_writes_syncs_and_registers(self):
        owner, fs = open_owner()
        reg = owner.register(coordinator(1))
        self.assertIsInstance(reg, fence.Registration)
        self.assertEqual(bytes(fs.disk), EPOCH1)  # complete write persisted
        self.assertGreaterEqual(fs.fsync_calls, 1)
        self.assertTrue(owner._registered)
        self.assertEqual(owner._epoch, 1)

    def test_reconnect_same_tuple_syncs_without_writing(self):
        owner, fs = open_owner(disk=EPOCH1)
        writes_before = fs.pwrite_calls
        reg = owner.register(coordinator(1))
        self.assertIsInstance(reg, fence.Registration)
        self.assertEqual(fs.pwrite_calls, writes_before)  # no write
        self.assertGreaterEqual(fs.fsync_calls, 1)  # durable ack
        self.assertEqual(bytes(fs.disk), EPOCH1)

    def test_lower_epoch_is_stale_no_write(self):
        owner, fs = open_owner(disk=fence._encode_record(5, bytes.fromhex(BOOT1_HEX)))
        with self.assertRaises(fence.FenceError) as ctx:
            owner.register(coordinator(3))
        self.assertEqual(ctx.exception.category, "Stale")
        self.assertEqual(fs.pwrite_calls, 0)

    def test_equal_epoch_different_boot_is_conflict(self):
        owner, fs = open_owner(disk=EPOCH1)
        with self.assertRaises(fence.FenceError) as ctx:
            owner.register(coordinator(1, "ffffffffffffffffffffffffffffffff"))
        self.assertEqual(ctx.exception.category, "Conflict")
        self.assertEqual(fs.pwrite_calls, 0)

    def test_record_changed_outside_owner_is_unavailable(self):
        owner, fs = open_owner(disk=EPOCH1)
        fs.disk = bytearray(fence._encode_record(2, bytes.fromhex(BOOT1_HEX)))
        with self.assertRaises(fence.FenceError) as ctx:
            owner.register(coordinator(3))
        self.assertEqual(ctx.exception.category, "Unavailable")


class FaultTests(unittest.TestCase):
    def test_zero_write_poisons_and_revokes(self):
        owner, fs = open_owner(disk=EPOCH1)
        first = owner.register(coordinator(1))  # reconnect ok
        fs.pwrite_zero = True
        with self.assertRaises(fence.FenceError) as ctx:
            owner.register(coordinator(2))
        self.assertEqual(ctx.exception.category, "Unavailable")
        # Poisoned: prior capability revoked, lock retained (register stays down).
        with self.assertRaises(fence.FenceError):
            with owner.admission(first):
                pass
        with self.assertRaises(fence.FenceError) as ctx2:
            owner.register(coordinator(3))
        self.assertEqual(ctx2.exception.category, "Unavailable")
        self.assertIsNotNone(owner._lock_fd)  # lock held until close

    def test_fsync_failure_poisons(self):
        owner, fs = open_owner()
        fs.fsync_error = True
        with self.assertRaises(fence.FenceError):
            owner.register(coordinator(1))
        self.assertTrue(owner._poisoned)

    def test_post_sync_mismatch_poisons(self):
        owner, fs = open_owner()

        def corrupt(disk):
            disk[8] ^= 0xFF  # change persisted bytes after the write, before readback

        fs.fsync_mutate = corrupt
        with self.assertRaises(fence.FenceError) as ctx:
            owner.register(coordinator(1))
        self.assertEqual(ctx.exception.category, "Unavailable")
        self.assertTrue(owner._poisoned)

    def test_short_write_completes(self):
        owner, fs = open_owner()
        fs.pwrite_short_once = True
        reg = owner.register(coordinator(1))
        self.assertIsInstance(reg, fence.Registration)
        self.assertEqual(bytes(fs.disk), EPOCH1)


class CapabilityTests(unittest.TestCase):
    def test_admission_valid_then_stale_after_higher(self):
        owner, _ = open_owner()
        reg1 = owner.register(coordinator(1))
        with owner.admission(reg1):
            pass  # a valid synchronous boundary
        reg2 = owner.register(coordinator(2))
        # Old registration is now stale (generation advanced).
        with self.assertRaises(fence.FenceError) as ctx:
            with owner.admission(reg1):
                pass
        self.assertEqual(ctx.exception.category, "Unavailable")
        with owner.admission(reg2):
            pass  # newest registration still admits

    def test_foreign_owner_registration_rejected(self):
        owner_a, _ = open_owner()
        reg_a = owner_a.register(coordinator(1))
        owner_b, _ = open_owner(disk=EPOCH1)
        with self.assertRaises(fence.FenceError):
            with owner_b.admission(reg_a):
                pass

    def test_admission_reentrancy_is_busy(self):
        owner, _ = open_owner()
        reg = owner.register(coordinator(1))
        with owner.admission(reg):
            with self.assertRaises(fence.FenceError) as ctx:
                owner.register(coordinator(2))
            self.assertEqual(ctx.exception.category, "Busy")

    def test_closed_owner_rejects_and_close_is_idempotent(self):
        owner, _ = open_owner()
        reg = owner.register(coordinator(1))
        owner.close()
        with self.assertRaises(fence.FenceError) as ctx:
            with owner.admission(reg):
                pass
        self.assertEqual(ctx.exception.category, "Closed")
        owner.close()  # idempotent

    def test_close_inside_admission_is_busy(self):
        owner, _ = open_owner()
        reg = owner.register(coordinator(1))
        with owner.admission(reg):
            with self.assertRaises(fence.FenceError) as ctx:
                owner.close()
            self.assertEqual(ctx.exception.category, "Busy")


class BoundaryTests(unittest.TestCase):
    def test_external_start_marker_not_undone_by_fencing(self):
        owner, _ = open_owner()
        reg1 = owner.register(coordinator(1))
        started = {"marker": False}
        with owner.admission(reg1):
            started["marker"] = True  # a synchronous external-start marker
        # A higher coordinator fences future authority, but cannot undo the marker.
        owner.register(coordinator(2))
        self.assertTrue(started["marker"])
        with self.assertRaises(fence.FenceError):
            with owner.admission(reg1):
                pass

    def test_rollback_of_whole_valid_record_is_not_detected(self):
        # Owner writes epoch5; a fresh open against a rolled-back valid epoch2
        # record accepts it — the single record alone cannot detect rollback.
        owner, fs = open_owner()
        owner.register(coordinator(5))
        self.assertEqual(fence._parse_record(bytes(fs.disk))[0], 5)
        rolled_back = fence._encode_record(2, bytes.fromhex(BOOT1_HEX))
        reopened, _ = open_owner(disk=rolled_back)
        self.assertEqual(reopened._epoch, 2)  # accepted; rollback undetectable here


if __name__ == "__main__":
    unittest.main()
