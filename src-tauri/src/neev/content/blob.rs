#[cfg(not(unix))]
use std::path::Path;

use super::types::ContentError;

const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BlobInfo {
    pub(crate) key: String,
    pub(crate) sha256: String,
    pub(crate) byte_length: i64,
}

#[cfg(unix)]
mod unix {
    #[cfg(test)]
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::{
        ffi::{CStr, CString},
        fs::File,
        io::{Read, Write},
        mem::MaybeUninit,
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::ffi::OsStrExt,
        },
        path::{Component, Path},
    };

    use sha2::{Digest, Sha256};

    use super::{BlobInfo, ContentError, MAX_DOCUMENT_BYTES};

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum FailurePoint {
        BeforeWrite,
        AfterWrite,
        BeforeFileSync,
        AfterFileSync,
        BeforeFinalize,
        AfterFinalize,
        BeforeDirectorySync,
        AfterDirectorySync,
    }

    pub(crate) struct BlobStore {
        root: File,
        #[cfg(test)]
        failure: Option<FailurePoint>,
        #[cfg(test)]
        root_sync_probe: Option<Arc<RootSyncProbe>>,
    }

    #[cfg(test)]
    struct RootSyncProbe {
        fail_next: AtomicBool,
        attempts: AtomicUsize,
    }

    impl BlobStore {
        pub(crate) fn open(root: &Path) -> Result<Self, ContentError> {
            if !root.is_absolute() {
                return Err(ContentError::Invalid);
            }
            let mut current = open_absolute_root()?;
            for component in root.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(component) => {
                        current = open_directory_at(current.as_raw_fd(), component.as_bytes())?;
                    }
                    _ => return Err(ContentError::Invalid),
                }
            }
            verify_directory(&current)?;
            Ok(Self {
                root: current,
                #[cfg(test)]
                failure: None,
                #[cfg(test)]
                root_sync_probe: None,
            })
        }

        #[cfg(test)]
        pub(crate) fn open_with_failure(
            root: &Path,
            failure: FailurePoint,
        ) -> Result<Self, ContentError> {
            let mut store = Self::open(root)?;
            store.failure = Some(failure);
            Ok(store)
        }

        #[cfg(test)]
        pub(crate) fn open_with_root_sync_failure(root: &Path) -> Result<Self, ContentError> {
            let mut store = Self::open(root)?;
            store.root_sync_probe = Some(Arc::new(RootSyncProbe {
                fail_next: AtomicBool::new(true),
                attempts: AtomicUsize::new(0),
            }));
            Ok(store)
        }

        #[cfg(test)]
        pub(crate) fn root_sync_attempts(&self) -> usize {
            self.root_sync_probe
                .as_ref()
                .map(|probe| probe.attempts.load(Ordering::SeqCst))
                .unwrap_or(0)
        }

        pub(crate) async fn put(
            &self,
            org_id: i32,
            key: &str,
            bytes: &[u8],
        ) -> Result<BlobInfo, ContentError> {
            validate_location(org_id, key)?;
            if bytes.len() > MAX_DOCUMENT_BYTES {
                return Err(ContentError::Invalid);
            }
            let directory = self.open_org_directory(org_id, true)?;
            let temporary_name = c_string(&format!("{key}.tmp"))?;
            let final_name = c_string(&format!("{key}.md"))?;
            let mut temporary = create_file_at(directory.as_raw_fd(), &temporary_name)?;
            let mut cleanup = TemporaryEntry::new(directory.as_raw_fd(), temporary_name);

            #[cfg(test)]
            self.fail(FailurePoint::BeforeWrite)?;
            temporary
                .write_all(bytes)
                .map_err(|_| ContentError::Storage)?;
            #[cfg(test)]
            self.fail(FailurePoint::AfterWrite)?;
            #[cfg(test)]
            self.fail(FailurePoint::BeforeFileSync)?;
            temporary.sync_all().map_err(|_| ContentError::Storage)?;
            #[cfg(test)]
            self.fail(FailurePoint::AfterFileSync)?;
            verify_file(&temporary, bytes.len() as i64)?;
            #[cfg(test)]
            self.fail(FailurePoint::BeforeFinalize)?;
            publish_without_replace(
                directory.as_raw_fd(),
                cleanup.name(),
                directory.as_raw_fd(),
                &final_name,
            )?;
            cleanup.remove()?;
            #[cfg(test)]
            self.fail(FailurePoint::AfterFinalize)?;
            #[cfg(test)]
            self.fail(FailurePoint::BeforeDirectorySync)?;
            directory.sync_all().map_err(|_| ContentError::Storage)?;
            #[cfg(test)]
            self.fail(FailurePoint::AfterDirectorySync)?;

            Ok(BlobInfo {
                key: key.to_owned(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
                byte_length: bytes.len() as i64,
            })
        }

        pub(crate) async fn read(
            &self,
            org_id: i32,
            key: &str,
            expected_size: i64,
            expected_sha256: &str,
        ) -> Result<Vec<u8>, ContentError> {
            validate_location(org_id, key)?;
            if !(0..=MAX_DOCUMENT_BYTES as i64).contains(&expected_size)
                || !valid_sha256(expected_sha256)
            {
                return Err(ContentError::Invalid);
            }
            let directory = self.open_org_directory(org_id, false)?;
            let final_name = c_string(&format!("{key}.md"))?;
            let mut file = open_file_at(directory.as_raw_fd(), &final_name)?;
            verify_file(&file, expected_size)?;
            let mut bytes = Vec::with_capacity(expected_size as usize);
            (&mut file)
                .take(expected_size as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ContentError::Storage)?;
            if bytes.len() as i64 != expected_size
                || format!("{:x}", Sha256::digest(&bytes)) != expected_sha256
            {
                return Err(ContentError::Storage);
            }
            Ok(bytes)
        }

        #[cfg(test)]
        fn fail(&self, point: FailurePoint) -> Result<(), ContentError> {
            if self.failure == Some(point) {
                Err(ContentError::Storage)
            } else {
                Ok(())
            }
        }

        fn open_org_directory(&self, org_id: i32, create: bool) -> Result<File, ContentError> {
            let name = c_string(&org_id.to_string())?;
            if create {
                let result = unsafe { libc::mkdirat(self.root.as_raw_fd(), name.as_ptr(), 0o700) };
                if result != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
                {
                    return Err(ContentError::Storage);
                }
                self.sync_root()?;
            }
            let directory =
                open_directory_at(self.root.as_raw_fd(), name.as_bytes()).map_err(|error| {
                    if !create
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
                    {
                        ContentError::NotFound
                    } else {
                        error
                    }
                })?;
            verify_directory(&directory)?;
            Ok(directory)
        }

        fn sync_root(&self) -> Result<(), ContentError> {
            #[cfg(test)]
            if let Some(probe) = &self.root_sync_probe {
                probe.attempts.fetch_add(1, Ordering::SeqCst);
                if probe.fail_next.swap(false, Ordering::SeqCst) {
                    return Err(ContentError::Storage);
                }
            }
            self.root.sync_all().map_err(|_| ContentError::Storage)
        }
    }

    fn validate_location(org_id: i32, key: &str) -> Result<(), ContentError> {
        if org_id <= 0 {
            return Err(ContentError::Invalid);
        }
        let parsed = uuid::Uuid::parse_str(key).map_err(|_| ContentError::Invalid)?;
        if parsed.hyphenated().to_string() != key {
            return Err(ContentError::Invalid);
        }
        Ok(())
    }

    fn valid_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn c_string(value: &str) -> Result<CString, ContentError> {
        CString::new(value).map_err(|_| ContentError::Invalid)
    }

    fn open_absolute_root() -> Result<File, ContentError> {
        open_directory_at(libc::AT_FDCWD, b"/")
    }

    fn open_directory_at(parent: RawFd, name: &[u8]) -> Result<File, ContentError> {
        let name = CString::new(name).map_err(|_| ContentError::Invalid)?;
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        owned_file(fd)
    }

    fn create_file_at(parent: RawFd, name: &CStr) -> Result<File, ContentError> {
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        owned_file(fd)
    }

    fn open_file_at(parent: RawFd, name: &CStr) -> Result<File, ContentError> {
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            return Err(ContentError::NotFound);
        }
        owned_file(fd)
    }

    fn owned_file(fd: RawFd) -> Result<File, ContentError> {
        if fd < 0 {
            return Err(ContentError::Storage);
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn metadata(file: &File) -> Result<libc::stat, ContentError> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) } != 0 {
            return Err(ContentError::Storage);
        }
        Ok(unsafe { metadata.assume_init() })
    }

    fn verify_directory(directory: &File) -> Result<(), ContentError> {
        let metadata = metadata(directory)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
            || metadata.st_uid != unsafe { libc::geteuid() }
            || metadata.st_mode & 0o7777 != 0o700
        {
            return Err(ContentError::Storage);
        }
        Ok(())
    }

    fn verify_file(file: &File, expected_size: i64) -> Result<(), ContentError> {
        let metadata = metadata(file)?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
            || metadata.st_uid != unsafe { libc::geteuid() }
            || metadata.st_mode & 0o7777 != 0o600
            || metadata.st_nlink != 1
            || metadata.st_size != expected_size
        {
            return Err(ContentError::Storage);
        }
        Ok(())
    }

    fn publish_without_replace(
        old_parent: RawFd,
        old_name: &CStr,
        new_parent: RawFd,
        new_name: &CStr,
    ) -> Result<(), ContentError> {
        let result = unsafe {
            libc::linkat(
                old_parent,
                old_name.as_ptr(),
                new_parent,
                new_name.as_ptr(),
                0,
            )
        };
        if result != 0 {
            return Err(
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                    ContentError::Conflict
                } else {
                    ContentError::Storage
                },
            );
        }
        Ok(())
    }

    struct TemporaryEntry {
        directory: RawFd,
        name: CString,
        present: bool,
    }

    impl TemporaryEntry {
        fn new(directory: RawFd, name: CString) -> Self {
            Self {
                directory,
                name,
                present: true,
            }
        }

        fn name(&self) -> &CStr {
            &self.name
        }

        fn remove(&mut self) -> Result<(), ContentError> {
            if unsafe { libc::unlinkat(self.directory, self.name.as_ptr(), 0) } != 0 {
                return Err(ContentError::Storage);
            }
            self.present = false;
            Ok(())
        }
    }

    impl Drop for TemporaryEntry {
        fn drop(&mut self) {
            if self.present {
                unsafe {
                    libc::unlinkat(self.directory, self.name.as_ptr(), 0);
                }
            }
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::BlobStore;
#[cfg(all(test, unix))]
pub(super) use unix::FailurePoint;

#[cfg(not(unix))]
pub(crate) struct BlobStore;

#[cfg(not(unix))]
impl BlobStore {
    pub(crate) fn open(_root: &Path) -> Result<Self, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) async fn put(
        &self,
        _org_id: i32,
        _key: &str,
        _bytes: &[u8],
    ) -> Result<BlobInfo, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) async fn read(
        &self,
        _org_id: i32,
        _key: &str,
        _expected_size: i64,
        _expected_sha256: &str,
    ) -> Result<Vec<u8>, ContentError> {
        Err(ContentError::Storage)
    }
}
