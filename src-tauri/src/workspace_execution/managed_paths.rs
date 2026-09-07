use std::{fs::{self, File, OpenOptions}, path::{Component, Path, PathBuf}};
use super::ExecutionError;

/// The configured data root is trusted; every managed descendant is checked
/// component-by-component so an existing symlink never redirects a write.
pub(crate) fn checked(root: &Path, relative: &Path) -> Result<PathBuf, ExecutionError> {
    let mut path = root.canonicalize().map_err(|_| ExecutionError::AllocationMismatch)?;
    for component in relative.components() {
        let Component::Normal(component) = component else { return Err(ExecutionError::AllocationMismatch); };
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(ExecutionError::AllocationMismatch),
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => return Err(ExecutionError::AllocationMismatch),
        }
    }
    Ok(path)
}

/// Ownership ends with the allocator, not with an unrelated inherited file
/// descriptor. This guard intentionally exposes neither cloning nor its File.
pub(crate) struct CacheLock {
    file: File,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            // Closing remains the conservative fallback; never report that a
            // subsequent allocator has acquired ownership here.
            tracing::warn!(%error, "failed to explicitly release allocation cache lock");
        }
    }
}

pub(crate) fn cache_lock(root: &Path, parent: &Path) -> Result<CacheLock, ExecutionError> {
    let parent_path = checked(root, parent)?;
    fs::create_dir_all(&parent_path).map_err(|_| ExecutionError::AllocationMismatch)?;
    let path = checked(root, &parent.join("allocation.lock"))?;
    if path.exists() && !path.is_file() { return Err(ExecutionError::AllocationMismatch); }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    let file = options.open(path).map_err(|_| ExecutionError::AllocationMismatch)?;
    file.try_lock().map_err(|_| ExecutionError::Busy)?;
    Ok(CacheLock { file })
}

pub(crate) fn reject_metadata_symlinks(path: &Path) -> Result<(), ExecutionError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ExecutionError::AllocationMismatch)?;
    if metadata.file_type().is_symlink() { return Err(ExecutionError::AllocationMismatch); }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|_| ExecutionError::AllocationMismatch)? {
            reject_metadata_symlinks(&entry.map_err(|_| ExecutionError::AllocationMismatch)?.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_execution_cache_lock_release_ignores_unrelated_inherited_handles() {
        let root = tempfile::tempdir().unwrap();
        let parent = Path::new("orgs/1/repositories/123");
        let owner = cache_lock(root.path(), parent).unwrap();
        // dup and fork share the same open file description. CLOEXEC closes an
        // inherited fd only at exec, so unrelated concurrent subprocess setup
        // can keep it alive briefly after this allocator returns.
        let inherited = owner.file.try_clone().unwrap();
        assert!(matches!(cache_lock(root.path(), parent), Err(ExecutionError::Busy)));
        drop(owner);
        let next = cache_lock(root.path(), parent);
        assert!(next.is_ok(), "finished allocator must release ownership even while an unrelated duplicate remains open");
        drop(inherited);
    }
}
