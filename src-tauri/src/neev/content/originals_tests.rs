//! Read-through of evicted originals: the protocol against an in-test broker,
//! the real root-side broker process end to end, and the API path's ACL order.

use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::{
    attachment::{self, Owner},
    originals::{BlobKind, OriginalsBroker},
    resources,
    test_support::Harness,
    types::*,
    BlobStore,
};
use crate::neev::test_support as h;

const KEY: &str = "01234567-89ab-4def-8123-456789abcdef";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A 0700 document root plus a short socket path (AF_UNIX paths are short).
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    socket: PathBuf,
    base: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("nvo-")
            .tempdir_in("/tmp")
            .unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("documents");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            socket: base.join("b.sock"),
            root,
            base,
            _temp: temp,
        }
    }

    fn store(&self) -> BlobStore {
        BlobStore::open(&self.root).unwrap().with_originals(Some(
            OriginalsBroker::new(self.socket.clone(), Duration::from_secs(20)).unwrap(),
        ))
    }
}

fn publish(root: &Path, org: i32, name: &str, bytes: &[u8]) {
    let org_dir = root.join(org.to_string());
    if !org_dir.exists() {
        fs::create_dir(&org_dir).unwrap();
        fs::set_permissions(&org_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(org_dir.join(name))
        .unwrap();
    file.write_all(bytes).unwrap();
}

#[derive(Clone)]
enum Behaviour {
    /// Publish these bytes at the requested path, then report success.
    Publish(Vec<u8>),
    /// Report success without publishing anything.
    ClaimOnly,
    Refuse(&'static str),
}

/// A protocol-faithful broker: it publishes into the real temp root like the
/// root-side broker, and records every request it receives.
fn spawn_broker(fixture: &Fixture, behaviour: Behaviour) -> Arc<Mutex<Vec<serde_json::Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(&fixture.socket).unwrap();
    let root = fixture.root.clone();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
            log.lock().unwrap().push(request.clone());
            let reply = match &behaviour {
                _ if request["op"] == "forget" => r#"{"ok":true,"result":"forgotten"}"#.to_owned(),
                Behaviour::Publish(bytes) => {
                    let name = format!(
                        "{}.{}",
                        request["key"].as_str().unwrap(),
                        request["kind"].as_str().unwrap()
                    );
                    publish(
                        &root,
                        request["org_id"].as_i64().unwrap() as i32,
                        &name,
                        bytes,
                    );
                    r#"{"ok":true,"result":"fetched"}"#.to_owned()
                }
                Behaviour::ClaimOnly => r#"{"ok":true,"result":"fetched"}"#.to_owned(),
                Behaviour::Refuse(code) => format!(r#"{{"ok":false,"error":"{code}"}}"#),
            };
            let mut stream = reader.into_inner();
            stream
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    seen
}

#[tokio::test]
async fn an_evicted_original_is_restored_through_the_broker_and_verified() {
    let fixture = Fixture::new();
    let bytes = b"attachment bytes restored from the storage box".to_vec();
    let seen = spawn_broker(&fixture, Behaviour::Publish(bytes.clone()));
    let store = fixture.store();
    let read = store
        .read_attachment(7, KEY, bytes.len() as i64, &hash(&bytes))
        .await
        .unwrap();
    assert_eq!(read, bytes);
    // The request carries only identity and the row's expectations: no path.
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [serde_json::json!({
            "v": 1, "op": "hydrate", "org_id": 7, "key": KEY, "kind": "att",
            "sha256": hash(&bytes), "byte_length": bytes.len(),
        })]
    );
    // Now local: a second read does not contact the broker.
    store
        .read_attachment(7, KEY, bytes.len() as i64, &hash(&bytes))
        .await
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn every_blob_kind_reads_through() {
    let fixture = Fixture::new();
    let bytes = b"# a document revision".to_vec();
    let seen = spawn_broker(&fixture, Behaviour::Publish(bytes.clone()));
    let store = fixture.store();
    let length = bytes.len() as i64;
    assert_eq!(
        store.read(3, KEY, length, &hash(&bytes)).await.unwrap(),
        bytes
    );
    let image_key = "11111111-89ab-4def-8123-456789abcdef";
    assert_eq!(
        store
            .read_image(3, image_key, length, &hash(&bytes))
            .await
            .unwrap(),
        bytes
    );
    let kinds: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["md", "img"]);
    assert_eq!(BlobKind::Attachment.extension(), "att");
}

#[tokio::test]
async fn outages_and_refusals_are_retryable_storage_errors_never_not_found() {
    let bytes = b"bytes".to_vec();
    let expected = (bytes.len() as i64, hash(&bytes));

    // No broker listening at all.
    let fixture = Fixture::new();
    let error = fixture
        .store()
        .read_attachment(7, KEY, expected.0, &expected.1)
        .await
        .unwrap_err();
    assert!(matches!(error, ContentError::Storage), "{error:?}");

    for behaviour in [
        Behaviour::Refuse("unavailable"),
        Behaviour::Refuse("not_verified"),
        Behaviour::ClaimOnly,
        // Wrong bytes published under the right name: the read-side hash check
        // refuses them even though the broker claimed success.
        Behaviour::Publish(b"other".to_vec()),
    ] {
        let fixture = Fixture::new();
        spawn_broker(&fixture, behaviour);
        let error = fixture
            .store()
            .read_attachment(7, KEY, expected.0, &expected.1)
            .await
            .unwrap_err();
        assert!(matches!(error, ContentError::Storage), "{error:?}");
    }
}

#[tokio::test]
async fn without_a_broker_a_miss_stays_not_found_and_bad_expectations_are_refused() {
    let fixture = Fixture::new();
    let plain = BlobStore::open(&fixture.root).unwrap();
    assert!(matches!(
        plain.read_attachment(7, KEY, 5, &hash(b"bytes")).await,
        Err(ContentError::NotFound)
    ));
    let seen = spawn_broker(&fixture, Behaviour::Publish(b"bytes".to_vec()));
    let store = fixture.store();
    for (size, digest) in [(0, hash(b"")), (5, "A".repeat(64)), (5, "abc".to_owned())] {
        assert!(matches!(
            store.read_attachment(7, KEY, size, &digest).await,
            Err(ContentError::Invalid)
        ));
    }
    assert!(matches!(
        store
            .read_image(7, KEY, 5 * 1024 * 1024 + 1, &hash(b"x"))
            .await,
        Err(ContentError::Invalid)
    ));
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_local_blob_that_disagrees_with_its_row_is_not_served() {
    let fixture = Fixture::new();
    publish(&fixture.root, 7, &format!("{KEY}.att"), b"tampered");
    let seen = spawn_broker(&fixture, Behaviour::Publish(b"original".to_vec()));
    let error = fixture
        .store()
        .read_attachment(7, KEY, 8, &hash(b"original"))
        .await
        .unwrap_err();
    assert!(matches!(error, ContentError::Storage));
    assert!(
        seen.lock().unwrap().is_empty(),
        "present files are never refetched"
    );
}

#[tokio::test]
async fn a_read_marks_an_idle_blob_used_for_the_evictor() {
    let fixture = Fixture::new();
    let bytes = b"idle".to_vec();
    publish(&fixture.root, 7, &format!("{KEY}.att"), &bytes);
    let path = fixture.root.join("7").join(format!("{KEY}.att"));
    let month_ago = SystemTime::now() - Duration::from_secs(30 * 86_400);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_accessed(month_ago)
                .set_modified(month_ago),
        )
        .unwrap();
    fixture
        .store()
        .read_attachment(7, KEY, 4, &hash(&bytes))
        .await
        .unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert!(metadata.accessed().unwrap() > SystemTime::now() - Duration::from_secs(60));
    // Only access time changes; content age is untouched.
    let modified = metadata.modified().unwrap();
    assert!(modified < SystemTime::now() - Duration::from_secs(29 * 86_400));
}

/// The real root-side broker process, fed by a fake `ssh` on PATH that serves a
/// local directory: exercises the wire protocol, peer-uid authentication, the
/// ledger check, hash verification and atomic publication together.
#[tokio::test]
async fn the_real_broker_restores_verified_originals_end_to_end() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("python3 unavailable; skipping real broker test");
        return;
    }
    let fixture = Fixture::new();
    let base = &fixture.base;
    let remote = base.join("remote");
    let bytes = b"%PDF-1.7\nreal broker round trip\n".to_vec();
    let unverified = "22222222-89ab-4def-8123-456789abcdef";
    for (key, data) in [(KEY, &bytes), (unverified, &bytes)] {
        let dir = remote.join("neev-originals").join("7");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{key}.att")), data).unwrap();
    }
    let state = base.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        state.join("verified.json"),
        serde_json::json!({"version": 1, "verified": {
            format!("7/{KEY}.att"): {"sha256": hash(&bytes), "size": bytes.len(), "verified_at": "t", "evicted_at": 1}
        }})
        .to_string(),
    )
    .unwrap();
    let private = |name: &str, body: &str| {
        let path = base.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    };
    let identity = private("id", "private");
    let known = private("known_hosts", "");
    let bin = base.join("bin");
    fs::create_dir(&bin).unwrap();
    let ssh = bin.join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nfor last; do :; done\nexec cat \"$NEEV_TEST_REMOTE/$last\"\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let config = private(
        "config.json",
        &serde_json::json!({
            "host": "u123456.your-storagebox.de", "user": "u123456", "allow_main_account": true,
            "identity_file": identity, "known_hosts_file": known,
            "remote_root": "neev-originals", "local_root": fixture.root, "state_dir": state,
            "broker": {"socket": fixture.socket, "client_uid": uid, "socket_gid": gid,
                       "fetch_timeout_seconds": 30},
        })
        .to_string(),
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/operations/neev_originals_broker.py");
    let mut child = tokio::process::Command::new("python3")
        .arg(script)
        .args(["--config", config.to_str().unwrap(), "serve"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("NEEV_TEST_REMOTE", &remote)
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if fixture.socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let store = fixture.store();
    let read = store
        .read_attachment(7, KEY, bytes.len() as i64, &hash(&bytes))
        .await
        .unwrap();
    assert_eq!(read, bytes);
    let published =
        fs::symlink_metadata(fixture.root.join("7").join(format!("{KEY}.att"))).unwrap();
    assert_eq!(published.permissions().mode() & 0o7777, 0o600);
    // Present on the remote but never verified by replication: refused.
    assert!(matches!(
        store
            .read_attachment(7, unverified, bytes.len() as i64, &hash(&bytes))
            .await,
        Err(ContentError::Storage)
    ));
    assert!(!fixture
        .root
        .join("7")
        .join(format!("{unverified}.att"))
        .exists());
    let leftovers: Vec<_> = fs::read_dir(fixture.root.join("7"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
    child.kill().await.unwrap();
}

/// Authorization still happens at the API boundary before any byte is read: a
/// member without access to the chat never triggers a Storage Box fetch.
#[tokio::test]
async fn the_api_path_authorizes_before_reading_through() {
    let mut harness = Harness::new(h::database().await).await;
    let chat = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(ResourceKind::Chat, "Private chat"),
    )
    .await
    .unwrap();
    let body = b"fn evicted() {}\n".to_vec();
    let stored = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "code.rs",
        &body,
    )
    .await
    .unwrap();
    let root = harness.state.config.document_root.clone();
    // Evict it the way the evictor does: unlink the local file only.
    let org = harness.creator.org_id;
    fs::remove_file(
        root.join(org.to_string())
            .join(format!("{}.att", stored.key)),
    )
    .unwrap();

    let socket_dir = tempfile::Builder::new()
        .prefix("nvo-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = socket_dir.path().join("b.sock");
    let fixture_root = root.clone();
    let seen = Arc::new(Mutex::new(0usize));
    let counter = seen.clone();
    let listener = UnixListener::bind(&socket).unwrap();
    let published = body.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mut reader = BufReader::new(stream);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
            *counter.lock().unwrap() += 1;
            publish(
                &fixture_root,
                request["org_id"].as_i64().unwrap() as i32,
                &format!("{}.att", request["key"].as_str().unwrap()),
                &published,
            );
            let mut stream = reader.into_inner();
            stream
                .write_all(b"{\"ok\":true,\"result\":\"fetched\"}\n")
                .await
                .unwrap();
        }
    });
    Arc::get_mut(&mut harness.state).unwrap().blobs =
        Arc::new(BlobStore::open(&root).unwrap().with_originals(Some(
            OriginalsBroker::new(socket, Duration::from_secs(20)).unwrap(),
        )));

    let refused = attachment::load(&harness.state, &harness.admin, &stored.key).await;
    assert!(matches!(refused, Err(ContentError::NotFound)));
    assert_eq!(
        *seen.lock().unwrap(),
        0,
        "no fetch for an unauthorized reader"
    );

    let (bytes, content_type, name) =
        attachment::load(&harness.state, &harness.creator, &stored.key)
            .await
            .unwrap();
    assert_eq!(
        (bytes, content_type, name.as_str()),
        (body, "text/plain", "code.rs")
    );
    assert_eq!(*seen.lock().unwrap(), 1);
}

/// Deleting an attachment tombstones it at the broker before the local unlink,
/// so a racing read-through cannot resurrect it; a broker outage does not
/// block reclaiming the local file.
#[tokio::test]
async fn removing_an_attachment_tombstones_it_before_unlinking() {
    let fixture = Fixture::new();
    publish(&fixture.root, 7, &format!("{KEY}.att"), b"orphan");
    let seen = spawn_broker(&fixture, Behaviour::Refuse("unused"));
    let store = fixture.store();
    store.remove_attachment(7, KEY).await.unwrap();
    assert!(!fixture.root.join("7").join(format!("{KEY}.att")).exists());
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [serde_json::json!({"v": 1, "op": "forget", "org_id": 7, "key": KEY, "kind": "att"})]
    );

    let unreachable = Fixture::new();
    publish(&unreachable.root, 7, &format!("{KEY}.att"), b"orphan");
    unreachable.store().remove_attachment(7, KEY).await.unwrap();
    assert!(!unreachable
        .root
        .join("7")
        .join(format!("{KEY}.att"))
        .exists());
}
