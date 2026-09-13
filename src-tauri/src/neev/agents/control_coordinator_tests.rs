use sha2::{Digest, Sha256};

use super::{decode_epoch, encode_epoch, CoordinatorError};

const EPOCH_ONE: [u8; 48] = [
    0x4e, 0x45, 0x45, 0x56, 0x45, 0x50, 0x43, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0xc0, 0xb5, 0x1c, 0x34, 0x8c, 0x69, 0x44, 0x57, 0xc4, 0x54, 0xb3, 0xdf, 0xfb, 0x24, 0x46, 0xc5,
    0x3e, 0xc2, 0x74, 0xe1, 0x61, 0x29, 0xb5, 0x07, 0xfd, 0x1b, 0x67, 0xf6, 0xe9, 0x29, 0x98, 0x20,
];

fn record_for_raw_epoch(epoch: u64) -> [u8; 48] {
    let mut record = [0_u8; 48];
    record[..8].copy_from_slice(b"NEEVEPC1");
    record[8..16].copy_from_slice(&epoch.to_be_bytes());
    let digest = Sha256::digest(&record[..16]);
    record[16..].copy_from_slice(&digest);
    record
}

#[test]
fn epoch_codec_matches_literal_vector_and_positive_boundaries() {
    assert_eq!(decode_epoch(&EPOCH_ONE), Ok(1));
    assert_eq!(encode_epoch(1), Ok(EPOCH_ONE));

    let maximum = encode_epoch(i64::MAX).unwrap();
    assert_eq!(decode_epoch(&maximum), Ok(i64::MAX));
    assert_eq!(&maximum[8..16], &(i64::MAX as u64).to_be_bytes());
    assert_eq!(&maximum[16..], &Sha256::digest(&maximum[..16])[..]);
}

#[test]
fn epoch_codec_rejects_every_short_and_bounded_long_record() {
    for length in 0..48 {
        assert_eq!(
            decode_epoch(&EPOCH_ONE[..length]),
            Err(CoordinatorError::Invalid),
            "accepted short length {length}"
        );
    }

    let mut long = EPOCH_ONE.to_vec();
    long.extend([0_u8; 48]);
    for length in 49..=long.len() {
        assert_eq!(
            decode_epoch(&long[..length]),
            Err(CoordinatorError::Invalid),
            "accepted long length {length}"
        );
    }
}

#[test]
fn epoch_codec_rejects_each_magic_and_digest_byte_corruption() {
    for index in 0..8 {
        let mut damaged = EPOCH_ONE;
        damaged[index] ^= 0x80;
        assert_eq!(decode_epoch(&damaged), Err(CoordinatorError::Invalid));
    }

    for index in 16..48 {
        let mut damaged = EPOCH_ONE;
        damaged[index] ^= 0x01;
        assert_eq!(decode_epoch(&damaged), Err(CoordinatorError::Invalid));
    }

    let mut damaged_epoch = EPOCH_ONE;
    damaged_epoch[15] ^= 0x01;
    assert_eq!(decode_epoch(&damaged_epoch), Err(CoordinatorError::Invalid));
}

#[test]
fn epoch_codec_rejects_zero_and_values_above_signed_range_with_valid_digests() {
    assert_eq!(
        decode_epoch(&record_for_raw_epoch(0)),
        Err(CoordinatorError::Invalid)
    );
    assert_eq!(
        decode_epoch(&record_for_raw_epoch(i64::MAX as u64 + 1)),
        Err(CoordinatorError::Invalid)
    );
    assert_eq!(encode_epoch(0), Err(CoordinatorError::Invalid));
    assert_eq!(encode_epoch(-1), Err(CoordinatorError::Invalid));
}

#[test]
fn errors_expose_only_fixed_categories() {
    use std::error::Error;

    let cases = [
        (CoordinatorError::Invalid, "invalid", "Invalid"),
        (CoordinatorError::Busy, "busy", "Busy"),
        (CoordinatorError::Exhausted, "exhausted", "Exhausted"),
        (CoordinatorError::Storage, "storage", "Storage"),
        (CoordinatorError::Poisoned, "poisoned", "Poisoned"),
        (CoordinatorError::Unsupported, "unsupported", "Unsupported"),
    ];
    for (error, display, debug) in cases {
        assert_eq!(error.to_string(), display);
        assert_eq!(format!("{error:?}"), debug);
        assert!(error.source().is_none());
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn public_acquisition_is_unsupported_without_touching_the_path() {
    use std::path::PathBuf;

    use super::{CoordinatorFileSpec, CoordinatorLease, FileIdentity};

    let missing = PathBuf::from("/definitely/not/a/coordinator/fixture");
    let spec = CoordinatorFileSpec {
        parent: missing.clone(),
        parent_identity: FileIdentity {
            device: 1,
            inode: 2,
        },
        parent_gid: 3,
        parent_mode: 0o700,
        service_uid: 4,
        service_gid: 5,
        lock_identity: FileIdentity {
            device: 6,
            inode: 7,
        },
        epoch_identity: FileIdentity {
            device: 6,
            inode: 8,
        },
    };
    assert!(matches!(
        CoordinatorLease::try_acquire(spec),
        Err(CoordinatorError::Unsupported)
    ));
    assert!(!missing.exists());
}

#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        fs::File,
        os::{
            fd::AsRawFd,
            unix::{
                ffi::OsStringExt,
                fs::{symlink, MetadataExt, PermissionsExt},
            },
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    use super::super::{
        validate_ancestor_metadata, validate_coordinator_file_metadata, validate_parent_metadata,
        validate_spec, CoordinatorFileSpec, CoordinatorLease, FileIdentity, MetadataSnapshot,
        TestFault,
    };
    use super::{encode_epoch, CoordinatorError};

    const CHILD_TEST: &str = "neev::agents::control_coordinator::tests::unix::synthetic_child";

    struct Fixture {
        _temp: TempDir,
        parent: PathBuf,
        spec: CoordinatorFileSpec,
    }

    impl Fixture {
        fn new(epoch: i64) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let parent = temp.path().canonicalize().unwrap().join("coordinator");
            fs::create_dir(&parent).unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
            let lock = parent.join("coordinator.lock");
            let epoch_path = parent.join("coordinator.epoch");
            fs::write(&lock, []).unwrap();
            fs::write(&epoch_path, encode_epoch(epoch).unwrap()).unwrap();
            fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
            fs::set_permissions(&epoch_path, fs::Permissions::from_mode(0o600)).unwrap();

            let parent_metadata = fs::metadata(&parent).unwrap();
            let lock_metadata = fs::metadata(&lock).unwrap();
            let epoch_metadata = fs::metadata(&epoch_path).unwrap();
            let spec = CoordinatorFileSpec {
                parent: parent.clone(),
                parent_identity: identity(&parent_metadata),
                parent_gid: parent_metadata.gid(),
                parent_mode: 0o700,
                service_uid: lock_metadata.uid(),
                service_gid: lock_metadata.gid(),
                lock_identity: identity(&lock_metadata),
                epoch_identity: identity(&epoch_metadata),
            };
            Self {
                _temp: temp,
                parent,
                spec,
            }
        }

        fn lock(&self) -> PathBuf {
            self.parent.join("coordinator.lock")
        }

        fn epoch(&self) -> PathBuf {
            self.parent.join("coordinator.epoch")
        }

        fn record(&self) -> Vec<u8> {
            fs::read(self.epoch()).unwrap()
        }

        fn acquire(&self) -> Result<CoordinatorLease, CoordinatorError> {
            CoordinatorLease::try_acquire_fixture(self.spec.clone())
        }

        fn acquire_with_fault(
            &self,
            fault: TestFault,
        ) -> Result<CoordinatorLease, CoordinatorError> {
            CoordinatorLease::try_acquire_fixture_with_fault(self.spec.clone(), fault)
        }
    }

    fn identity(metadata: &fs::Metadata) -> FileIdentity {
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn snapshot(mode: u32) -> MetadataSnapshot {
        MetadataSnapshot {
            device: 11,
            inode: 12,
            uid: 0,
            gid: 13,
            mode,
            links: 1,
            size: 0,
        }
    }

    #[test]
    fn specification_rejects_noncanonical_paths_and_impossible_identity_policy() {
        let fixture = Fixture::new(1);
        let mut invalid = fixture.spec.clone();
        let paths = [
            PathBuf::from("relative"),
            PathBuf::from("/"),
            PathBuf::from("/a/./b"),
            PathBuf::from("/a/../b"),
            PathBuf::from("/a//b"),
            PathBuf::from("/a/b/"),
            PathBuf::from(format!("/{}", vec!["x"; 33].join("/"))),
            PathBuf::from(format!("/{}", "x".repeat(4096))),
            PathBuf::from(std::ffi::OsString::from_vec(b"/a/\0b".to_vec())),
        ];
        for path in paths {
            invalid.parent = path;
            assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
        }

        invalid = fixture.spec.clone();
        invalid.parent_mode = 0o1000;
        assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
        invalid.parent_mode = 0o722;
        assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
        invalid = fixture.spec.clone();
        invalid.service_uid = 0;
        assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
        invalid = fixture.spec.clone();
        invalid.epoch_identity = invalid.lock_identity;
        assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
        invalid = fixture.spec.clone();
        invalid.epoch_identity.device = invalid.lock_identity.device + 1;
        assert_eq!(validate_spec(&invalid), Err(CoordinatorError::Invalid));
    }

    #[test]
    fn production_metadata_policy_rejects_unsafe_ancestors_parent_and_files() {
        let mut ancestor = snapshot(libc::S_IFDIR as u32 | 0o755);
        assert_eq!(validate_ancestor_metadata(&ancestor), Ok(()));
        ancestor.uid = 1;
        assert_eq!(
            validate_ancestor_metadata(&ancestor),
            Err(CoordinatorError::Storage)
        );
        ancestor.uid = 0;
        ancestor.mode = libc::S_IFDIR as u32 | 0o775;
        assert_eq!(
            validate_ancestor_metadata(&ancestor),
            Err(CoordinatorError::Storage)
        );
        ancestor.mode = libc::S_IFREG as u32 | 0o755;
        assert_eq!(
            validate_ancestor_metadata(&ancestor),
            Err(CoordinatorError::Storage)
        );

        let fixture = Fixture::new(1);
        let mut parent = snapshot(libc::S_IFDIR as u32 | fixture.spec.parent_mode);
        parent.device = fixture.spec.parent_identity.device;
        parent.inode = fixture.spec.parent_identity.inode;
        parent.gid = fixture.spec.parent_gid;
        assert_eq!(validate_parent_metadata(&parent, &fixture.spec), Ok(()));
        for change in 0..5 {
            let mut wrong = parent;
            match change {
                0 => wrong.uid = 1,
                1 => wrong.gid += 1,
                2 => wrong.mode = libc::S_IFDIR as u32 | 0o755,
                3 => wrong.device += 1,
                _ => wrong.inode += 1,
            }
            assert_eq!(
                validate_parent_metadata(&wrong, &fixture.spec),
                Err(CoordinatorError::Storage)
            );
        }

        let expected = FileIdentity {
            device: 21,
            inode: 22,
        };
        let mut file = MetadataSnapshot {
            device: 21,
            inode: 22,
            uid: 23,
            gid: 24,
            mode: libc::S_IFREG as u32 | 0o600,
            links: 1,
            size: 48,
        };
        assert_eq!(
            validate_coordinator_file_metadata(&file, expected, 23, 24, 48),
            Ok(())
        );
        for change in 0..7 {
            let mut wrong = file;
            match change {
                0 => wrong.mode = libc::S_IFDIR as u32 | 0o600,
                1 => wrong.mode = libc::S_IFREG as u32 | 0o640,
                2 => wrong.links = 2,
                3 => wrong.uid += 1,
                4 => wrong.gid += 1,
                5 => wrong.inode += 1,
                _ => wrong.size += 1,
            }
            assert_eq!(
                validate_coordinator_file_metadata(&wrong, expected, 23, 24, 48),
                Err(CoordinatorError::Storage)
            );
        }
        file.device += 1;
        assert_eq!(
            validate_coordinator_file_metadata(&file, expected, 23, 24, 48),
            Err(CoordinatorError::Storage)
        );
    }

    fn assert_fixture_refused_unchanged(fixture: &Fixture) {
        let epoch = fixture.record();
        let mut names = fs::read_dir(&fixture.parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        names.sort();
        assert!(matches!(fixture.acquire(), Err(CoordinatorError::Storage)));
        assert_eq!(fixture.record(), epoch);
        let mut after = fs::read_dir(&fixture.parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        after.sort();
        assert_eq!(after, names);
    }

    #[test]
    fn fixed_pair_refuses_symlinks_nonfiles_links_modes_sizes_and_wrong_identities() {
        let outside = Fixture::new(1);
        let original_outside = outside.record();
        let fixture = Fixture::new(1);
        fs::remove_file(fixture.lock()).unwrap();
        symlink(outside.lock(), fixture.lock()).unwrap();
        assert_fixture_refused_unchanged(&fixture);
        assert_eq!(outside.record(), original_outside);

        let fixture = Fixture::new(1);
        fs::remove_file(fixture.lock()).unwrap();
        fs::create_dir(fixture.lock()).unwrap();
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        fs::remove_file(fixture.lock()).unwrap();
        let fifo = std::ffi::CString::new(fixture.lock().as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        fs::hard_link(fixture.lock(), fixture.parent.join("extra-link")).unwrap();
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        fs::set_permissions(fixture.epoch(), fs::Permissions::from_mode(0o640)).unwrap();
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        fs::write(fixture.epoch(), [0_u8; 47]).unwrap();
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        let mut trailing = encode_epoch(1).unwrap().to_vec();
        trailing.push(0);
        fs::write(fixture.epoch(), trailing).unwrap();
        assert_fixture_refused_unchanged(&fixture);

        let fixture = Fixture::new(1);
        let mut wrong = fixture.spec.clone();
        wrong.lock_identity.inode = wrong.lock_identity.inode.wrapping_add(101);
        if wrong.lock_identity == wrong.epoch_identity {
            wrong.lock_identity.inode = wrong.lock_identity.inode.wrapping_add(1);
        }
        let epoch = fixture.record();
        assert!(matches!(
            CoordinatorLease::try_acquire_fixture(wrong),
            Err(CoordinatorError::Storage)
        ));
        assert_eq!(fixture.record(), epoch);
    }

    #[test]
    fn no_follow_traversal_refuses_symlinked_parent_and_post_open_replacement() {
        let fixture = Fixture::new(1);
        let alias = fixture.parent.with_file_name("coordinator-alias");
        symlink(&fixture.parent, &alias).unwrap();
        let mut spec = fixture.spec.clone();
        spec.parent = alias;
        assert!(matches!(
            CoordinatorLease::try_acquire_fixture(spec),
            Err(CoordinatorError::Storage)
        ));
        assert_eq!(fixture.record(), encode_epoch(1).unwrap());

        let fixture = Fixture::new(1);
        assert!(matches!(
            fixture.acquire_with_fault(TestFault::ReplaceLockAfterOpen),
            Err(CoordinatorError::Storage)
        ));
        assert_eq!(fixture.record(), encode_epoch(1).unwrap());
    }

    #[test]
    fn lease_increments_once_moves_without_unlocking_and_drop_allows_next_epoch() {
        let fixture = Fixture::new(1);
        let mut lease = fixture.acquire().unwrap();
        assert_eq!(lease.checked_epoch(), Ok(2));
        assert_eq!(lease.checked_epoch(), Ok(2));
        assert_eq!(fixture.record(), encode_epoch(2).unwrap());
        assert!(matches!(fixture.acquire(), Err(CoordinatorError::Busy)));

        let moved = lease;
        assert!(matches!(fixture.acquire(), Err(CoordinatorError::Busy)));
        drop(moved);

        let mut next = fixture.acquire().unwrap();
        assert_eq!(next.checked_epoch(), Ok(3));
        assert_eq!(fixture.record(), encode_epoch(3).unwrap());
    }

    fn assert_lock_available(path: &Path) {
        let file = File::options().read(true).write(true).open(path).unwrap();
        assert!(file.try_lock().is_ok());
    }

    #[test]
    fn lock_descriptor_is_close_on_exec_and_exec_child_cannot_retain_it() {
        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        let flags = unsafe { libc::fcntl(lease.lock.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);

        let ready = fixture.parent.join("child-ready");
        let stop = fixture.parent.join("child-stop");
        let mut child = spawn_child("wait", &fixture, &ready, &stop);
        wait_for(&ready);
        drop(lease);
        let replacement = fixture.acquire().unwrap();
        assert!(child.try_wait().unwrap().is_none());
        drop(replacement);
        fs::write(&stop, []).unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn independent_process_contends_and_kill_reap_releases_its_lock() {
        let fixture = Fixture::new(1);
        let ready = fixture.parent.join("holder-ready");
        let stop = fixture.parent.join("holder-stop");
        let mut child = spawn_child("hold", &fixture, &ready, &stop);
        wait_for(&ready);
        let held_record = fixture.record();
        assert_eq!(held_record, encode_epoch(2).unwrap());
        assert!(matches!(fixture.acquire(), Err(CoordinatorError::Busy)));
        assert_eq!(fixture.record(), held_record);

        child.kill().unwrap();
        let _ = child.wait().unwrap();
        let mut lease = fixture.acquire().unwrap();
        assert_eq!(lease.checked_epoch(), Ok(3));
    }

    #[test]
    fn acquisition_faults_never_return_a_lease_and_never_repair_or_rollback() {
        let fixture = Fixture::new(1);
        assert!(matches!(
            fixture.acquire_with_fault(TestFault::WriteError),
            Err(CoordinatorError::Storage)
        ));
        assert_eq!(fixture.record(), encode_epoch(1).unwrap());
        assert_lock_available(&fixture.lock());

        let fixture = Fixture::new(1);
        assert!(matches!(
            fixture.acquire_with_fault(TestFault::PartialWrite(16)),
            Err(CoordinatorError::Storage)
        ));
        let partial = fixture.record();
        assert_ne!(partial, encode_epoch(1).unwrap());
        assert_ne!(partial, encode_epoch(2).unwrap());
        assert!(matches!(fixture.acquire(), Err(CoordinatorError::Invalid)));
        assert_eq!(fixture.record(), partial);
        assert_lock_available(&fixture.lock());

        for fault in [
            TestFault::SyncError,
            TestFault::PostSyncReadback,
            TestFault::PostSyncIdentity,
        ] {
            let fixture = Fixture::new(1);
            assert!(matches!(
                fixture.acquire_with_fault(fault),
                Err(CoordinatorError::Storage)
            ));
            assert_eq!(fixture.record(), encode_epoch(2).unwrap());
            assert_lock_available(&fixture.lock());
        }

        let fixture = Fixture::new(i64::MAX);
        assert!(matches!(
            fixture.acquire(),
            Err(CoordinatorError::Exhausted)
        ));
        assert_eq!(fixture.record(), encode_epoch(i64::MAX).unwrap());
        assert_lock_available(&fixture.lock());
    }

    fn assert_poisoned(mut lease: CoordinatorLease, first: CoordinatorError) {
        assert_eq!(lease.checked_epoch(), Err(first));
        assert_eq!(lease.checked_epoch(), Err(CoordinatorError::Poisoned));
    }

    #[test]
    fn checked_epoch_poison_is_permanent_and_retains_lock_until_drop() {
        let fixture = Fixture::new(1);
        let mut lease = fixture.acquire().unwrap();
        fs::write(fixture.epoch(), encode_epoch(3).unwrap()).unwrap();
        assert_eq!(lease.checked_epoch(), Err(CoordinatorError::Storage));
        assert_eq!(lease.checked_epoch(), Err(CoordinatorError::Poisoned));
        let contender = File::options()
            .read(true)
            .write(true)
            .open(fixture.lock())
            .unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(lease);
        assert!(contender.try_lock().is_ok());

        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        fs::set_permissions(fixture.epoch(), fs::Permissions::from_mode(0o640)).unwrap();
        assert_poisoned(lease, CoordinatorError::Storage);

        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        fs::write(fixture.epoch(), [0_u8; 47]).unwrap();
        assert_poisoned(lease, CoordinatorError::Storage);

        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        let mut corrupt = encode_epoch(2).unwrap();
        corrupt[47] ^= 1;
        fs::write(fixture.epoch(), corrupt).unwrap();
        assert_poisoned(lease, CoordinatorError::Storage);

        let fixture = Fixture::new(1);
        let mut lease = fixture.acquire().unwrap();
        lease.spec.service_uid = lease.spec.service_uid.saturating_add(1);
        assert_poisoned(lease, CoordinatorError::Storage);
    }

    #[test]
    fn checked_epoch_poison_detects_replaced_lock_epoch_and_parent() {
        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        let saved_lock = fixture.parent.join("saved-lock");
        fs::rename(fixture.lock(), &saved_lock).unwrap();
        fs::write(fixture.lock(), []).unwrap();
        fs::set_permissions(fixture.lock(), fs::Permissions::from_mode(0o600)).unwrap();
        let original = File::options()
            .read(true)
            .write(true)
            .open(&saved_lock)
            .unwrap();
        assert!(matches!(
            original.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert_poisoned(lease, CoordinatorError::Storage);
        assert!(original.try_lock().is_ok());

        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        fs::rename(fixture.epoch(), fixture.parent.join("saved-epoch")).unwrap();
        fs::write(fixture.epoch(), encode_epoch(2).unwrap()).unwrap();
        fs::set_permissions(fixture.epoch(), fs::Permissions::from_mode(0o600)).unwrap();
        assert_poisoned(lease, CoordinatorError::Storage);

        let fixture = Fixture::new(1);
        let lease = fixture.acquire().unwrap();
        let moved_parent = fixture.parent.with_file_name("moved-parent");
        fs::rename(&fixture.parent, &moved_parent).unwrap();
        fs::create_dir(&fixture.parent).unwrap();
        fs::set_permissions(&fixture.parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert_poisoned(lease, CoordinatorError::Storage);
    }

    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() {
            assert!(Instant::now() < deadline, "child handshake timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn spawn_child(role: &str, fixture: &Fixture, ready: &Path, stop: &Path) -> ChildGuard {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .arg("--ignored")
            .arg("--exact")
            .arg(CHILD_TEST)
            .arg("--test-threads=1")
            .env("NEEV_COORD_TEST_ROLE", role)
            .env("NEEV_COORD_TEST_PARENT", &fixture.spec.parent)
            .env(
                "NEEV_COORD_TEST_PARENT_DEV",
                fixture.spec.parent_identity.device.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_PARENT_INO",
                fixture.spec.parent_identity.inode.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_PARENT_GID",
                fixture.spec.parent_gid.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_PARENT_MODE",
                fixture.spec.parent_mode.to_string(),
            )
            .env("NEEV_COORD_TEST_UID", fixture.spec.service_uid.to_string())
            .env("NEEV_COORD_TEST_GID", fixture.spec.service_gid.to_string())
            .env(
                "NEEV_COORD_TEST_LOCK_DEV",
                fixture.spec.lock_identity.device.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_LOCK_INO",
                fixture.spec.lock_identity.inode.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_EPOCH_DEV",
                fixture.spec.epoch_identity.device.to_string(),
            )
            .env(
                "NEEV_COORD_TEST_EPOCH_INO",
                fixture.spec.epoch_identity.inode.to_string(),
            )
            .env("NEEV_COORD_TEST_READY", ready)
            .env("NEEV_COORD_TEST_STOP", stop)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ChildGuard(Some(command.spawn().unwrap()))
    }

    struct ChildGuard(Option<Child>);

    impl std::ops::Deref for ChildGuard {
        type Target = Child;

        fn deref(&self) -> &Self::Target {
            self.0.as_ref().unwrap()
        }
    }

    impl std::ops::DerefMut for ChildGuard {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.0.as_mut().unwrap()
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
        }
    }

    fn env_u64(name: &str) -> u64 {
        env::var(name).unwrap().parse().unwrap()
    }

    fn env_u32(name: &str) -> u32 {
        env::var(name).unwrap().parse().unwrap()
    }

    #[test]
    #[ignore = "invoked as an owned exec child by coordinator tests"]
    fn synthetic_child() {
        let role = env::var("NEEV_COORD_TEST_ROLE").unwrap();
        let parent = PathBuf::from(env::var_os("NEEV_COORD_TEST_PARENT").unwrap());
        let spec = CoordinatorFileSpec {
            parent,
            parent_identity: FileIdentity {
                device: env_u64("NEEV_COORD_TEST_PARENT_DEV"),
                inode: env_u64("NEEV_COORD_TEST_PARENT_INO"),
            },
            parent_gid: env_u32("NEEV_COORD_TEST_PARENT_GID"),
            parent_mode: env_u32("NEEV_COORD_TEST_PARENT_MODE"),
            service_uid: env_u32("NEEV_COORD_TEST_UID"),
            service_gid: env_u32("NEEV_COORD_TEST_GID"),
            lock_identity: FileIdentity {
                device: env_u64("NEEV_COORD_TEST_LOCK_DEV"),
                inode: env_u64("NEEV_COORD_TEST_LOCK_INO"),
            },
            epoch_identity: FileIdentity {
                device: env_u64("NEEV_COORD_TEST_EPOCH_DEV"),
                inode: env_u64("NEEV_COORD_TEST_EPOCH_INO"),
            },
        };
        let _lease = if role == "hold" {
            Some(CoordinatorLease::try_acquire_fixture(spec).unwrap())
        } else {
            None
        };
        let ready = PathBuf::from(env::var_os("NEEV_COORD_TEST_READY").unwrap());
        let stop = PathBuf::from(env::var_os("NEEV_COORD_TEST_STOP").unwrap());
        fs::write(ready, []).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stop.exists() {
            assert!(Instant::now() < deadline, "parent did not release child");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
