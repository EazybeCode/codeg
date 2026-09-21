#![allow(dead_code)]

use std::{fmt, path::PathBuf};

#[cfg(unix)]
use std::fs::File;

use sha2::{Digest, Sha256};

const EPOCH_MAGIC: &[u8; 8] = b"NEEVEPC1";
const EPOCH_RECORD_SIZE: usize = 48;
#[cfg(unix)]
const LOCK_NAME: &[u8] = b"coordinator.lock";
#[cfg(unix)]
const EPOCH_NAME: &[u8] = b"coordinator.epoch";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone)]
pub(crate) struct CoordinatorFileSpec {
    pub parent: PathBuf,
    pub parent_identity: FileIdentity,
    pub parent_gid: u32,
    pub parent_mode: u32,
    pub service_uid: u32,
    pub service_gid: u32,
    pub lock_identity: FileIdentity,
    pub epoch_identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoordinatorError {
    Invalid,
    Busy,
    Exhausted,
    Storage,
    Poisoned,
    Unsupported,
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid",
            Self::Busy => "busy",
            Self::Exhausted => "exhausted",
            Self::Storage => "storage",
            Self::Poisoned => "poisoned",
            Self::Unsupported => "unsupported",
        })
    }
}

impl std::error::Error for CoordinatorError {}

pub(crate) struct CoordinatorLease {
    #[cfg(unix)]
    parent: File,
    #[cfg(unix)]
    epoch_file: File,
    minted_record: [u8; EPOCH_RECORD_SIZE],
    epoch: i64,
    poisoned: bool,
    spec: CoordinatorFileSpec,
    #[cfg(unix)]
    admission: Admission,
    // Keep the lock last so it is the last retained filesystem resource dropped.
    #[cfg(unix)]
    lock: File,
}

impl CoordinatorLease {
    pub(crate) fn try_acquire(spec: CoordinatorFileSpec) -> Result<Self, CoordinatorError> {
        #[cfg(target_os = "linux")]
        {
            return acquire(spec, Admission::Production, TestFault::None);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = spec;
            Err(CoordinatorError::Unsupported)
        }
    }

    pub(crate) fn checked_epoch(&mut self) -> Result<i64, CoordinatorError> {
        if self.poisoned {
            return Err(CoordinatorError::Poisoned);
        }
        #[cfg(unix)]
        let result = checked_epoch_inner(self);
        #[cfg(not(unix))]
        let result = Err(CoordinatorError::Unsupported);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    #[cfg(all(test, unix))]
    fn try_acquire_fixture(spec: CoordinatorFileSpec) -> Result<Self, CoordinatorError> {
        acquire(spec, Admission::Fixture, TestFault::None)
    }

    #[cfg(all(test, unix))]
    fn try_acquire_fixture_with_fault(
        spec: CoordinatorFileSpec,
        fault: TestFault,
    ) -> Result<Self, CoordinatorError> {
        acquire(spec, Admission::Fixture, fault)
    }
}

fn encode_epoch(epoch: i64) -> Result<[u8; EPOCH_RECORD_SIZE], CoordinatorError> {
    if epoch <= 0 {
        return Err(CoordinatorError::Invalid);
    }
    let mut record = [0_u8; EPOCH_RECORD_SIZE];
    record[..8].copy_from_slice(EPOCH_MAGIC);
    record[8..16].copy_from_slice(&(epoch as u64).to_be_bytes());
    let digest = Sha256::digest(&record[..16]);
    record[16..].copy_from_slice(&digest);
    Ok(record)
}

fn decode_epoch(record: &[u8]) -> Result<i64, CoordinatorError> {
    if record.len() != EPOCH_RECORD_SIZE || &record[..8] != EPOCH_MAGIC {
        return Err(CoordinatorError::Invalid);
    }
    let digest = Sha256::digest(&record[..16]);
    if record[16..] != digest[..] {
        return Err(CoordinatorError::Invalid);
    }
    let raw = u64::from_be_bytes(
        record[8..16]
            .try_into()
            .map_err(|_| CoordinatorError::Invalid)?,
    );
    if raw == 0 || raw > i64::MAX as u64 {
        return Err(CoordinatorError::Invalid);
    }
    Ok(raw as i64)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct MetadataSnapshot {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    size: i64,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Admission {
    Production,
    #[cfg(test)]
    Fixture,
}

#[derive(Clone, Copy)]
enum TestFault {
    None,
    ReplaceLockAfterOpen,
    PartialWrite(usize),
    WriteError,
    SyncError,
    PostSyncReadback,
    PostSyncIdentity,
}

#[cfg(unix)]
fn validate_spec(spec: &CoordinatorFileSpec) -> Result<(), CoordinatorError> {
    use std::os::unix::ffi::OsStrExt;

    let path = spec.parent.as_os_str().as_bytes();
    if path.len() < 2
        || path.len() > 4096
        || path[0] != b'/'
        || path[path.len() - 1] == b'/'
        || path.contains(&0)
        || path.windows(2).any(|pair| pair == b"//")
    {
        return Err(CoordinatorError::Invalid);
    }
    let mut count = 0_usize;
    for component in path[1..].split(|byte| *byte == b'/') {
        count += 1;
        if component.is_empty() || component == b"." || component == b".." {
            return Err(CoordinatorError::Invalid);
        }
    }
    if count > 32
        || spec.parent_mode > 0o777
        || spec.parent_mode & 0o022 != 0
        || spec.service_uid == 0
        || spec.lock_identity == spec.epoch_identity
        || spec.lock_identity.device != spec.epoch_identity.device
    {
        return Err(CoordinatorError::Invalid);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_ancestor_metadata(metadata: &MetadataSnapshot) -> Result<(), CoordinatorError> {
    if file_type(metadata.mode) != libc::S_IFDIR as u32
        || metadata.uid != 0
        || metadata.mode & 0o022 != 0
    {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_parent_metadata(
    metadata: &MetadataSnapshot,
    spec: &CoordinatorFileSpec,
) -> Result<(), CoordinatorError> {
    if file_type(metadata.mode) != libc::S_IFDIR as u32
        || metadata.uid != 0
        || metadata.gid != spec.parent_gid
        || permission_mode(metadata.mode) != spec.parent_mode
        || identity_of(metadata) != spec.parent_identity
    {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(all(test, unix))]
fn validate_fixture_parent_metadata(
    metadata: &MetadataSnapshot,
    spec: &CoordinatorFileSpec,
) -> Result<(), CoordinatorError> {
    if file_type(metadata.mode) != libc::S_IFDIR as u32
        || metadata.uid != spec.service_uid
        || metadata.gid != spec.parent_gid
        || permission_mode(metadata.mode) != spec.parent_mode
        || identity_of(metadata) != spec.parent_identity
    {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_coordinator_file_metadata(
    metadata: &MetadataSnapshot,
    expected: FileIdentity,
    service_uid: u32,
    service_gid: u32,
    size: i64,
) -> Result<(), CoordinatorError> {
    if file_type(metadata.mode) != libc::S_IFREG as u32
        || permission_mode(metadata.mode) != 0o600
        || metadata.links != 1
        || metadata.uid != service_uid
        || metadata.gid != service_gid
        || metadata.size != size
        || identity_of(metadata) != expected
    {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn file_type(mode: u32) -> u32 {
    mode & libc::S_IFMT as u32
}

#[cfg(unix)]
fn permission_mode(mode: u32) -> u32 {
    mode & 0o7777
}

#[cfg(unix)]
fn identity_of(metadata: &MetadataSnapshot) -> FileIdentity {
    FileIdentity {
        device: metadata.device,
        inode: metadata.inode,
    }
}

#[cfg(unix)]
fn validate_parent_for_admission(
    metadata: &MetadataSnapshot,
    spec: &CoordinatorFileSpec,
    admission: Admission,
) -> Result<(), CoordinatorError> {
    match admission {
        Admission::Production => validate_parent_metadata(metadata, spec),
        #[cfg(test)]
        Admission::Fixture => validate_fixture_parent_metadata(metadata, spec),
    }
}

#[cfg(unix)]
fn acquire(
    spec: CoordinatorFileSpec,
    admission: Admission,
    fault: TestFault,
) -> Result<CoordinatorLease, CoordinatorError> {
    use std::fs::TryLockError;

    validate_spec(&spec)?;
    if matches!(admission, Admission::Production)
        && (unsafe { libc::geteuid() } != spec.service_uid
            || unsafe { libc::getegid() } != spec.service_gid)
    {
        return Err(CoordinatorError::Storage);
    }

    let parent = open_parent(&spec, admission)?;
    let lock = open_coordinator_file(&parent, LOCK_NAME)?;
    revalidate_file(
        &parent,
        &lock,
        LOCK_NAME,
        spec.lock_identity,
        spec.service_uid,
        spec.service_gid,
        0,
    )?;
    inject_lock_replacement(&spec, fault)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(CoordinatorError::Busy),
        Err(TryLockError::Error(_)) => return Err(CoordinatorError::Storage),
    }
    revalidate_parent(&parent, &spec, admission)?;
    revalidate_file(
        &parent,
        &lock,
        LOCK_NAME,
        spec.lock_identity,
        spec.service_uid,
        spec.service_gid,
        0,
    )?;

    let epoch_file = open_coordinator_file(&parent, EPOCH_NAME)?;
    revalidate_file(
        &parent,
        &epoch_file,
        EPOCH_NAME,
        spec.epoch_identity,
        spec.service_uid,
        spec.service_gid,
        EPOCH_RECORD_SIZE as i64,
    )?;
    let original_record = read_record(&epoch_file)?;
    let current = decode_epoch(&original_record)?;
    let next = current
        .checked_add(1)
        .ok_or(CoordinatorError::Exhausted)?;

    revalidate_parent(&parent, &spec, admission)?;
    revalidate_file(
        &parent,
        &lock,
        LOCK_NAME,
        spec.lock_identity,
        spec.service_uid,
        spec.service_gid,
        0,
    )?;
    revalidate_file(
        &parent,
        &epoch_file,
        EPOCH_NAME,
        spec.epoch_identity,
        spec.service_uid,
        spec.service_gid,
        EPOCH_RECORD_SIZE as i64,
    )?;
    if read_record(&epoch_file)? != original_record {
        return Err(CoordinatorError::Storage);
    }

    let minted_record = encode_epoch(next)?;
    write_record(&epoch_file, &minted_record, fault)?;
    sync_record(&epoch_file, fault)?;
    if matches!(fault, TestFault::PostSyncIdentity) {
        return Err(CoordinatorError::Storage);
    }
    revalidate_parent(&parent, &spec, admission)?;
    revalidate_file(
        &parent,
        &lock,
        LOCK_NAME,
        spec.lock_identity,
        spec.service_uid,
        spec.service_gid,
        0,
    )?;
    revalidate_file(
        &parent,
        &epoch_file,
        EPOCH_NAME,
        spec.epoch_identity,
        spec.service_uid,
        spec.service_gid,
        EPOCH_RECORD_SIZE as i64,
    )?;
    if matches!(fault, TestFault::PostSyncReadback) {
        return Err(CoordinatorError::Storage);
    }
    if read_record(&epoch_file)? != minted_record {
        return Err(CoordinatorError::Storage);
    }

    Ok(CoordinatorLease {
        parent,
        epoch_file,
        minted_record,
        epoch: next,
        poisoned: false,
        spec,
        admission,
        lock,
    })
}

#[cfg(unix)]
fn checked_epoch_inner(lease: &CoordinatorLease) -> Result<i64, CoordinatorError> {
    revalidate_parent(&lease.parent, &lease.spec, lease.admission)?;
    revalidate_file(
        &lease.parent,
        &lease.lock,
        LOCK_NAME,
        lease.spec.lock_identity,
        lease.spec.service_uid,
        lease.spec.service_gid,
        0,
    )?;
    revalidate_file(
        &lease.parent,
        &lease.epoch_file,
        EPOCH_NAME,
        lease.spec.epoch_identity,
        lease.spec.service_uid,
        lease.spec.service_gid,
        EPOCH_RECORD_SIZE as i64,
    )?;
    let record = read_record(&lease.epoch_file)?;
    if record != lease.minted_record || decode_epoch(&record)? != lease.epoch {
        return Err(CoordinatorError::Storage);
    }
    Ok(lease.epoch)
}

#[cfg(unix)]
fn open_parent(spec: &CoordinatorFileSpec, admission: Admission) -> Result<File, CoordinatorError> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let path = spec.parent.as_os_str().as_bytes();
    let components = path[1..].split(|byte| *byte == b'/').collect::<Vec<_>>();
    let mut current = open_directory_at(libc::AT_FDCWD, b"/")?;
    let root = file_metadata(&current)?;
    if matches!(admission, Admission::Production) {
        validate_ancestor_metadata(&root)?;
    }
    for (index, component) in components.iter().enumerate() {
        let opened = open_directory_at(current.as_raw_fd(), component)?;
        let opened_metadata = file_metadata(&opened)?;
        let published_metadata = entry_metadata(current.as_raw_fd(), component)?;
        if opened_metadata != published_metadata {
            return Err(CoordinatorError::Storage);
        }
        if index + 1 == components.len() {
            validate_parent_for_admission(&opened_metadata, spec, admission)?;
        } else if matches!(admission, Admission::Production) {
            validate_ancestor_metadata(&opened_metadata)?;
        }
        current = opened;
    }
    Ok(current)
}

#[cfg(unix)]
fn revalidate_parent(
    retained: &File,
    spec: &CoordinatorFileSpec,
    admission: Admission,
) -> Result<(), CoordinatorError> {
    let retained_metadata = file_metadata(retained)?;
    validate_parent_for_admission(&retained_metadata, spec, admission)?;
    let published = open_parent(spec, admission)?;
    if file_metadata(&published)? != retained_metadata {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn open_directory_at(parent: std::os::fd::RawFd, name: &[u8]) -> Result<File, CoordinatorError> {
    use std::ffi::CString;

    let name = CString::new(name).map_err(|_| CoordinatorError::Invalid)?;
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    owned_file(fd)
}

#[cfg(unix)]
fn open_coordinator_file(parent: &File, name: &[u8]) -> Result<File, CoordinatorError> {
    use std::{ffi::CString, os::fd::AsRawFd};

    let name = CString::new(name).map_err(|_| CoordinatorError::Invalid)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    owned_file(fd)
}

#[cfg(unix)]
fn owned_file(fd: std::os::fd::RawFd) -> Result<File, CoordinatorError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    if fd < 0 {
        return Err(CoordinatorError::Storage);
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 || flags & libc::FD_CLOEXEC == 0 {
        return Err(CoordinatorError::Storage);
    }
    Ok(file)
}

#[cfg(unix)]
fn file_metadata(file: &File) -> Result<MetadataSnapshot, CoordinatorError> {
    use std::{mem::MaybeUninit, os::fd::AsRawFd};

    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) } != 0 {
        return Err(CoordinatorError::Storage);
    }
    Ok(snapshot_from_stat(unsafe { metadata.assume_init() }))
}

#[cfg(unix)]
fn entry_metadata(
    parent: std::os::fd::RawFd,
    name: &[u8],
) -> Result<MetadataSnapshot, CoordinatorError> {
    use std::{ffi::CString, mem::MaybeUninit};

    let name = CString::new(name).map_err(|_| CoordinatorError::Invalid)?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(CoordinatorError::Storage);
    }
    Ok(snapshot_from_stat(unsafe { metadata.assume_init() }))
}

#[cfg(unix)]
fn snapshot_from_stat(metadata: libc::stat) -> MetadataSnapshot {
    MetadataSnapshot {
        device: metadata.st_dev as u64,
        inode: metadata.st_ino as u64,
        uid: metadata.st_uid,
        gid: metadata.st_gid,
        mode: metadata.st_mode as u32,
        links: metadata.st_nlink as u64,
        size: metadata.st_size,
    }
}

#[cfg(unix)]
fn revalidate_file(
    parent: &File,
    file: &File,
    name: &[u8],
    expected: FileIdentity,
    uid: u32,
    gid: u32,
    size: i64,
) -> Result<(), CoordinatorError> {
    use std::os::fd::AsRawFd;

    let opened = file_metadata(file)?;
    let published = entry_metadata(parent.as_raw_fd(), name)?;
    validate_coordinator_file_metadata(&opened, expected, uid, gid, size)?;
    validate_coordinator_file_metadata(&published, expected, uid, gid, size)?;
    if opened != published {
        return Err(CoordinatorError::Storage);
    }
    Ok(())
}

#[cfg(unix)]
fn read_record(file: &File) -> Result<[u8; EPOCH_RECORD_SIZE], CoordinatorError> {
    use std::{io::ErrorKind, os::unix::fs::FileExt};

    let mut record = [0_u8; EPOCH_RECORD_SIZE];
    let mut offset = 0_usize;
    while offset < record.len() {
        match file.read_at(&mut record[offset..], offset as u64) {
            Ok(0) => return Err(CoordinatorError::Storage),
            Ok(read) => offset += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(CoordinatorError::Storage),
        }
    }
    let mut trailing = [0_u8; 1];
    loop {
        match file.read_at(&mut trailing, EPOCH_RECORD_SIZE as u64) {
            Ok(0) => break,
            Ok(_) => return Err(CoordinatorError::Storage),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(CoordinatorError::Storage),
        }
    }
    Ok(record)
}

#[cfg(unix)]
fn write_record(
    file: &File,
    record: &[u8; EPOCH_RECORD_SIZE],
    fault: TestFault,
) -> Result<(), CoordinatorError> {
    use std::{io::ErrorKind, os::unix::fs::FileExt};

    if matches!(fault, TestFault::WriteError) {
        return Err(CoordinatorError::Storage);
    }
    if let TestFault::PartialWrite(limit) = fault {
        let limit = limit.min(record.len());
        let written = file
            .write_at(&record[..limit], 0)
            .map_err(|_| CoordinatorError::Storage)?;
        if written == 0 {
            return Err(CoordinatorError::Storage);
        }
        return Err(CoordinatorError::Storage);
    }

    let mut offset = 0_usize;
    while offset < record.len() {
        match file.write_at(&record[offset..], offset as u64) {
            Ok(0) => return Err(CoordinatorError::Storage),
            Ok(written) => offset += written,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(CoordinatorError::Storage),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sync_record(file: &File, fault: TestFault) -> Result<(), CoordinatorError> {
    if matches!(fault, TestFault::SyncError) {
        return Err(CoordinatorError::Storage);
    }
    file.sync_all().map_err(|_| CoordinatorError::Storage)
}

#[cfg(unix)]
fn inject_lock_replacement(
    spec: &CoordinatorFileSpec,
    fault: TestFault,
) -> Result<(), CoordinatorError> {
    #[cfg(test)]
    if matches!(fault, TestFault::ReplaceLockAfterOpen) {
        use std::{fs, os::unix::fs::PermissionsExt};

        let lock = spec.parent.join("coordinator.lock");
        fs::rename(&lock, spec.parent.join("replaced-lock"))
            .map_err(|_| CoordinatorError::Storage)?;
        fs::write(&lock, []).map_err(|_| CoordinatorError::Storage)?;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600))
            .map_err(|_| CoordinatorError::Storage)?;
    }
    #[cfg(not(test))]
    let _ = (spec, fault);
    Ok(())
}

#[cfg(test)]
#[path = "control_coordinator_tests.rs"]
mod tests;
