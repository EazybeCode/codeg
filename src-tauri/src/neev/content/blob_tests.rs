use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::blob::FailurePoint;
use super::{types::ContentError, BlobStore};

const KEY: &str = "01234567-89ab-4def-8123-456789abcdef";

struct RootFixture {
    _temp: TempDir,
    path: PathBuf,
}

impl RootFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self { _temp: temp, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[tokio::test]
async fn writes_reads_and_never_overwrites_an_immutable_blob() {
    let root = RootFixture::new();
    let store = BlobStore::open(root.path()).unwrap();
    let original = b"# First revision";
    let expected_hash = hash(original);

    let info = store.put(1, KEY, original).await.unwrap();
    assert_eq!(
        info,
        super::BlobInfo {
            key: KEY.into(),
            sha256: expected_hash.clone(),
            byte_length: 16,
        }
    );
    assert!(store.put(1, KEY, b"replacement").await.is_err());
    assert_eq!(
        store.read(1, KEY, 16, &expected_hash).await.unwrap(),
        original
    );
}

#[tokio::test]
async fn accepts_zero_byte_markdown_and_rejects_noncanonical_locations() {
    let root = RootFixture::new();
    let store = BlobStore::open(root.path()).unwrap();
    let empty_hash = hash(b"");
    store.put(1, KEY, b"").await.unwrap();
    assert_eq!(store.read(1, KEY, 0, &empty_hash).await.unwrap(), b"");

    for (org_id, key) in [
        (1, "../outside"),
        (1, "0123456789ab4def8123456789abcdef"),
        (1, "01234567-89AB-4DEF-8123-456789ABCDEF"),
        (1, "01234567-89ab-4def-8123-456789abcdef.md"),
        (0, "11234567-89ab-4def-8123-456789abcdef"),
        (-1, "21234567-89ab-4def-8123-456789abcdef"),
    ] {
        assert_eq!(
            store.put(org_id, key, b"x").await.err(),
            Some(ContentError::Invalid),
            "accepted org {org_id} key {key:?}"
        );
    }
    assert!(!root.path().join("outside").exists());
}

#[test]
fn refuses_symlinked_root_or_parent_and_permissive_root() {
    let base = RootFixture::new();
    let real_root = base.path().join("real-root");
    fs::create_dir(&real_root).unwrap();
    fs::set_permissions(&real_root, fs::Permissions::from_mode(0o700)).unwrap();
    let linked_root = base.path().join("linked-root");
    symlink(&real_root, &linked_root).unwrap();
    assert!(BlobStore::open(&linked_root).is_err());

    let real_parent = base.path().join("real-parent");
    let child = real_parent.join("documents");
    fs::create_dir(&real_parent).unwrap();
    fs::create_dir(&child).unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    let linked_parent = base.path().join("linked-parent");
    symlink(&real_parent, &linked_parent).unwrap();
    assert!(BlobStore::open(&linked_parent.join("documents")).is_err());

    let permissive = base.path().join("permissive");
    fs::create_dir(&permissive).unwrap();
    fs::set_permissions(&permissive, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(BlobStore::open(&permissive).is_err());

    let special_mode = base.path().join("special-mode");
    fs::create_dir(&special_mode).unwrap();
    fs::set_permissions(&special_mode, fs::Permissions::from_mode(0o2700)).unwrap();
    assert_eq!(
        fs::metadata(&special_mode).unwrap().permissions().mode() & 0o7777,
        0o2700
    );
    assert!(BlobStore::open(&special_mode).is_err());

    let root_file = base.path().join("root-file");
    fs::write(&root_file, b"not a directory").unwrap();
    fs::set_permissions(&root_file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(BlobStore::open(&root_file).is_err());
}

#[tokio::test]
async fn refuses_symlinked_or_permissive_org_directories_and_files() {
    let root = RootFixture::new();
    let outside = RootFixture::new();
    let linked_org = root.path().join("1");
    symlink(outside.path(), &linked_org).unwrap();
    let store = BlobStore::open(root.path()).unwrap();
    assert!(store.put(1, KEY, b"private").await.is_err());
    assert!(!outside.path().join(format!("{KEY}.md")).exists());
    fs::remove_file(&linked_org).unwrap();

    let org = root.path().join("1");
    fs::create_dir(&org).unwrap();
    fs::set_permissions(&org, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(store.put(1, KEY, b"private").await.is_err());
    fs::set_permissions(&org, fs::Permissions::from_mode(0o700)).unwrap();

    let outside_file = outside.path().join("outside.md");
    fs::write(&outside_file, b"outside").unwrap();
    fs::set_permissions(&outside_file, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside_file, org.join(format!("{KEY}.md"))).unwrap();
    assert!(store.read(1, KEY, 7, &hash(b"outside")).await.is_err());
}

#[tokio::test]
async fn refuses_permissive_files_and_detects_size_or_hash_mismatch() {
    let root = RootFixture::new();
    let store = BlobStore::open(root.path()).unwrap();
    let bytes = b"# verified";
    let expected_hash = hash(bytes);
    store.put(1, KEY, bytes).await.unwrap();

    assert!(store.read(1, KEY, 9, &expected_hash).await.is_err());
    assert!(store.read(1, KEY, 10, &hash(b"different")).await.is_err());

    let file = root.path().join("1").join(format!("{KEY}.md"));
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.read(1, KEY, 10, &expected_hash).await.is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o4600)).unwrap();
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o4600
    );
    assert!(store.read(1, KEY, 10, &expected_hash).await.is_err());
}

#[tokio::test]
async fn enforces_the_one_mebibyte_document_bound() {
    let root = RootFixture::new();
    let store = BlobStore::open(root.path()).unwrap();
    let allowed = vec![b'x'; 1024 * 1024];
    let info = store.put(1, KEY, &allowed).await.unwrap();
    assert_eq!(info.byte_length, 1024 * 1024);

    let second_key = "11234567-89ab-4def-8123-456789abcdef";
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    assert_eq!(
        store.put(1, second_key, &oversized).await.err(),
        Some(ContentError::Invalid)
    );
    assert_eq!(
        store
            .read(1, KEY, 1024 * 1024 + 1, &hash(&allowed))
            .await
            .err(),
        Some(ContentError::Invalid)
    );
}

#[tokio::test]
async fn injected_stage_failures_never_replace_published_bytes() {
    let bytes = b"# durable";
    let expected_hash = hash(bytes);
    let before_publication = [
        FailurePoint::BeforeWrite,
        FailurePoint::AfterWrite,
        FailurePoint::BeforeFileSync,
        FailurePoint::AfterFileSync,
        FailurePoint::BeforeFinalize,
    ];
    for point in before_publication {
        let root = RootFixture::new();
        let failing = BlobStore::open_with_failure(root.path(), point).unwrap();
        assert_eq!(
            failing.put(1, KEY, bytes).await.err(),
            Some(ContentError::Storage)
        );
        assert!(!root.path().join("1").join(format!("{KEY}.md")).exists());
        assert!(!root.path().join("1").join(format!("{KEY}.tmp")).exists());
    }

    for point in [
        FailurePoint::AfterFinalize,
        FailurePoint::BeforeDirectorySync,
        FailurePoint::AfterDirectorySync,
    ] {
        let root = RootFixture::new();
        let failing = BlobStore::open_with_failure(root.path(), point).unwrap();
        assert_eq!(
            failing.put(1, KEY, bytes).await.err(),
            Some(ContentError::Storage)
        );
        let healthy = BlobStore::open(root.path()).unwrap();
        assert_eq!(
            healthy
                .read(1, KEY, bytes.len() as i64, &expected_hash)
                .await
                .unwrap(),
            bytes
        );
        assert!(healthy.put(1, KEY, b"replacement").await.is_err());
        assert_eq!(
            healthy
                .read(1, KEY, bytes.len() as i64, &expected_hash)
                .await
                .unwrap(),
            bytes
        );
    }
}
