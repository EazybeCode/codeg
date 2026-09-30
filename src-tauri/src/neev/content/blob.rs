#[cfg(not(unix))]
use std::path::Path;

use super::types::ContentError;

const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Uploaded images are larger than text documents but still bounded well below a
/// photo-library scale; the editor embeds diagrams/screenshots, not galleries.
pub(crate) const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// **Not a per-file limit** — a chat attachment has none.
///
/// This is the org's total attachment storage quota
/// (`attachment::MAX_ATTACHMENT_BYTES_PER_ORG` is defined as this), reused as the
/// allocation guard wherever a whole attachment must be held in memory. One file
/// cannot exceed the entire tenant budget, so anything over it was never
/// admissible; raise the quota and every buffer bound follows.
pub(crate) const ATTACHMENT_STORAGE_CEILING: usize = 256 * 1024 * 1024;

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

    use super::{
        BlobInfo, ContentError, ATTACHMENT_STORAGE_CEILING, MAX_DOCUMENT_BYTES, MAX_IMAGE_BYTES,
    };
    use crate::neev::content::originals::{BlobKind, OriginalsBroker};

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
        /// Read-through for evicted originals; `None` keeps a miss `NotFound`.
        originals: Option<OriginalsBroker>,
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
                originals: None,
                #[cfg(test)]
                failure: None,
                #[cfg(test)]
                root_sync_probe: None,
            })
        }

        pub(crate) fn with_originals(mut self, originals: Option<OriginalsBroker>) -> Self {
            self.originals = originals;
            self
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
            self.read_original(
                org_id,
                key,
                BlobKind::Document,
                expected_size,
                expected_sha256,
            )
            .await
        }

        /// Open a final blob, asking the broker to restore it on a local miss.
        ///
        /// Callers pass the length and digest from the database row that proved
        /// the blob exists, so after a successful hydrate anything but a readable
        /// final is a storage fault (503), never `NotFound`.
        async fn open_original(
            &self,
            org_id: i32,
            key: &str,
            kind: BlobKind,
            expected_size: i64,
            expected_sha256: &str,
        ) -> Result<File, ContentError> {
            match self.open_final(org_id, key, kind) {
                Err(ContentError::NotFound) => {}
                other => return other,
            }
            let Some(originals) = &self.originals else {
                return Err(ContentError::NotFound);
            };
            originals
                .hydrate(org_id, key, kind, expected_size, expected_sha256)
                .await?;
            self.open_final(org_id, key, kind)
                .map_err(|_| ContentError::Storage)
        }

        fn open_final(&self, org_id: i32, key: &str, kind: BlobKind) -> Result<File, ContentError> {
            let directory = self.open_org_directory(org_id, false)?;
            let final_name = c_string(&format!("{key}.{}", kind.extension()))?;
            open_file_at(directory.as_raw_fd(), &final_name)
        }

        /// Every read is checked against the row: owner, 0600, one link, exact
        /// length, then SHA-256. A successful read marks the blob as used so the
        /// evictor keeps recently read files local.
        async fn read_original(
            &self,
            org_id: i32,
            key: &str,
            kind: BlobKind,
            expected_size: i64,
            expected_sha256: &str,
        ) -> Result<Vec<u8>, ContentError> {
            let mut file = self
                .open_original(org_id, key, kind, expected_size, expected_sha256)
                .await?;
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
            mark_used(&file);
            Ok(bytes)
        }

        /// Store an uploaded image blob under `{key}.img` (distinct namespace from
        /// document `.md` blobs), mode 0600, in the tenant's 0700 directory. Same
        /// atomic temp→fsync→publish→dir-sync sequence as `put`; no create-replace
        /// so a fresh key never clobbers an existing blob.
        pub(crate) async fn put_image(
            &self,
            org_id: i32,
            key: &str,
            bytes: &[u8],
        ) -> Result<BlobInfo, ContentError> {
            validate_location(org_id, key)?;
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
                return Err(ContentError::Invalid);
            }
            let directory = self.open_org_directory(org_id, true)?;
            let temporary_name = c_string(&format!("{key}.img.tmp"))?;
            let final_name = c_string(&format!("{key}.img"))?;
            let mut temporary = create_file_at(directory.as_raw_fd(), &temporary_name)?;
            let mut cleanup = TemporaryEntry::new(directory.as_raw_fd(), temporary_name);
            temporary
                .write_all(bytes)
                .map_err(|_| ContentError::Storage)?;
            temporary.sync_all().map_err(|_| ContentError::Storage)?;
            verify_file(&temporary, bytes.len() as i64)?;
            publish_without_replace(
                directory.as_raw_fd(),
                cleanup.name(),
                directory.as_raw_fd(),
                &final_name,
            )?;
            cleanup.remove()?;
            directory.sync_all().map_err(|_| ContentError::Storage)?;
            Ok(BlobInfo {
                key: key.to_owned(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
                byte_length: bytes.len() as i64,
            })
        }

        /// Store a chat attachment. Byte-for-byte the image path with its own
        /// suffix, so an attachment can never be served through the image route
        /// and vice versa — but with NO per-file size rule of its own. Empty is
        /// still refused; the ceiling is the tenant quota `attachment::store`
        /// already enforced, repeated here as defence in depth.
        pub(crate) async fn put_attachment(
            &self,
            org_id: i32,
            key: &str,
            bytes: &[u8],
        ) -> Result<BlobInfo, ContentError> {
            validate_location(org_id, key)?;
            if bytes.is_empty() || bytes.len() > ATTACHMENT_STORAGE_CEILING {
                return Err(ContentError::Invalid);
            }
            let directory = self.open_org_directory(org_id, true)?;
            let temporary_name = c_string(&format!("{key}.att.tmp"))?;
            let final_name = c_string(&format!("{key}.att"))?;
            let mut temporary = create_file_at(directory.as_raw_fd(), &temporary_name)?;
            let mut cleanup = TemporaryEntry::new(directory.as_raw_fd(), temporary_name);
            temporary
                .write_all(bytes)
                .map_err(|_| ContentError::Storage)?;
            temporary.sync_all().map_err(|_| ContentError::Storage)?;
            verify_file(&temporary, bytes.len() as i64)?;
            publish_without_replace(
                directory.as_raw_fd(),
                cleanup.name(),
                directory.as_raw_fd(),
                &final_name,
            )?;
            cleanup.remove()?;
            directory.sync_all().map_err(|_| ContentError::Storage)?;
            Ok(BlobInfo {
                key: key.to_owned(),
                sha256: format!("{:x}", Sha256::digest(bytes)),
                byte_length: bytes.len() as i64,
            })
        }

        /// Read a stored attachment, checked against its row's length and digest.
        /// A missing blob is `NotFound` unless read-through restores it.
        ///
        /// Bounded by `ATTACHMENT_STORAGE_CEILING`, the tenant budget rather than a
        /// size rule: a row over it was never admissible.
        pub(crate) async fn read_attachment(
            &self,
            org_id: i32,
            key: &str,
            expected_size: i64,
            expected_sha256: &str,
        ) -> Result<Vec<u8>, ContentError> {
            validate_location(org_id, key)?;
            if !(1..=ATTACHMENT_STORAGE_CEILING as i64).contains(&expected_size)
                || !valid_sha256(expected_sha256)
            {
                return Err(ContentError::Invalid);
            }
            self.read_original(
                org_id,
                key,
                BlobKind::Attachment,
                expected_size,
                expected_sha256,
            )
            .await
        }

        /// Delete a stored attachment. A blob that is already gone is not an
        /// error: the caller is reclaiming, and reclaiming twice is harmless.
        pub(crate) async fn remove_attachment(
            &self,
            org_id: i32,
            key: &str,
        ) -> Result<(), ContentError> {
            validate_location(org_id, key)?;
            // Tombstone first, so a read-through racing this deletion cannot
            // republish the blob after its row is gone. A broker outage must not
            // block reclaiming the local copy; hydration is down with it.
            if let Some(originals) = &self.originals {
                let _ = originals.forget(org_id, key, BlobKind::Attachment).await;
            }
            let directory = match self.open_org_directory(org_id, false) {
                Ok(directory) => directory,
                Err(_) => return Ok(()),
            };
            let final_name = c_string(&format!("{key}.att"))?;
            if unsafe { libc::unlinkat(directory.as_raw_fd(), final_name.as_ptr(), 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT)
            {
                return Err(ContentError::Storage);
            }
            Ok(())
        }

        /// Read a stored image blob, checked against its row's length and digest.
        /// A missing blob is `NotFound` unless read-through restores it.
        pub(crate) async fn read_image(
            &self,
            org_id: i32,
            key: &str,
            expected_size: i64,
            expected_sha256: &str,
        ) -> Result<Vec<u8>, ContentError> {
            validate_location(org_id, key)?;
            if !(1..=MAX_IMAGE_BYTES as i64).contains(&expected_size)
                || !valid_sha256(expected_sha256)
            {
                return Err(ContentError::Invalid);
            }
            self.read_original(org_id, key, BlobKind::Image, expected_size, expected_sha256)
                .await
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

    /// Record a read as the file's access time, at most hourly, so the evictor's
    /// idle test does not depend on the mount's atime policy. Best effort.
    fn mark_used(file: &File) {
        const REFRESH_SECONDS: libc::time_t = 60 * 60;
        let Ok(metadata) = metadata(file) else {
            return;
        };
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        if metadata.st_atime > now - REFRESH_SECONDS {
            return;
        }
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW,
            },
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
        ];
        unsafe {
            libc::futimens(file.as_raw_fd(), times.as_ptr());
        }
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

    pub(crate) async fn put_image(
        &self,
        _org_id: i32,
        _key: &str,
        _bytes: &[u8],
    ) -> Result<BlobInfo, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) fn with_originals(
        self,
        _originals: Option<crate::neev::content::originals::OriginalsBroker>,
    ) -> Self {
        self
    }

    pub(crate) async fn read_image(
        &self,
        _org_id: i32,
        _key: &str,
        _expected_size: i64,
        _expected_sha256: &str,
    ) -> Result<Vec<u8>, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) async fn put_attachment(
        &self,
        _org_id: i32,
        _key: &str,
        _bytes: &[u8],
    ) -> Result<BlobInfo, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) async fn read_attachment(
        &self,
        _org_id: i32,
        _key: &str,
        _expected_size: i64,
        _expected_sha256: &str,
    ) -> Result<Vec<u8>, ContentError> {
        Err(ContentError::Storage)
    }

    pub(crate) async fn remove_attachment(
        &self,
        _org_id: i32,
        _key: &str,
    ) -> Result<(), ContentError> {
        Err(ContentError::Storage)
    }
}
