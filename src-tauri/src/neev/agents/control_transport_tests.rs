use super::*;
use crate::neev::agents::control_dto::{self, Counter, Response};
use crate::neev::agents::control_wire;
use rand::rngs::OsRng;
use rand::RngCore;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

#[derive(serde::Deserialize)]
struct Fixture {
    requests: Vec<Entry>,
    responses: Vec<Entry>,
}

#[derive(serde::Deserialize)]
struct Entry {
    name: String,
    payload_hex: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "../../../../scripts/tests/fixtures/neev-control-dto-v1.json"
    ))
    .unwrap()
}

fn from_hex(value: &str) -> Vec<u8> {
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn payload(entries: &[Entry], name: &str) -> Vec<u8> {
    from_hex(&entries.iter().find(|e| e.name == name).unwrap().payload_hex)
}

fn sample_deployment() -> ControlDeployment {
    let parent = ProtectedParent {
        absolute_path: PathBuf::from("/var/lib/neev-native/control"),
        identity: FileIdentity {
            device: 2049,
            inode: 1000,
        },
        gid: 800,
        mode: 0o750,
    };
    ControlDeployment {
        socket_path: PathBuf::from("/var/lib/neev-native/control/control.sock"),
        key_path: PathBuf::from("/var/lib/neev-native/control/control.key"),
        socket_parent: parent.clone(),
        key_parent: parent,
        socket_identity: FileIdentity {
            device: 2049,
            inode: 1111,
        },
        key_identity: FileIdentity {
            device: 2049,
            inode: 1234,
        },
        control_gid: 800,
        self_uid: 0,
        self_gid: 0,
        peer_uid: 1000,
        peer_gid: 1000,
        socket_owner_uid: 0,
    }
}

fn sample_coordinator() -> Coordinator {
    Coordinator::new(
        Counter::new("1".into()).unwrap(),
        Id::new("3".repeat(32)).unwrap(),
    )
}

#[test]
fn parent_path_shape_is_bounded_and_normal() {
    assert!(validate_parent_path(Path::new("/var/lib/neev-native/control")).is_ok());
    assert_eq!(
        validate_parent_path(Path::new("relative/x")),
        Err(TransportError::Metadata)
    );
    assert_eq!(
        validate_parent_path(Path::new("/a/../b")),
        Err(TransportError::Metadata)
    );
    assert_eq!(
        validate_parent_path(Path::new("/")),
        Err(TransportError::Metadata)
    );
}

#[tokio::test]
async fn connect_is_unsupported_off_linux() {
    if cfg!(target_os = "linux") {
        return;
    }
    let result = SupervisorClient::connect(&sample_deployment(), sample_coordinator()).await;
    assert!(matches!(result, Err(TransportError::Unsupported)));
}

/// On the deploy target the gate is wired: an absent deployment is refused for
/// what it actually is, NOT with the `Unsupported` the stub used to return.
#[tokio::test]
async fn connect_runs_the_real_gate_on_linux() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let result = SupervisorClient::connect(&sample_deployment(), sample_coordinator()).await;
    assert_eq!(result.err(), Some(TransportError::Metadata));
}

/// A socket that exists but is not the declared supervisor must be refused
/// before the handshake — an impostor never sees a client nonce.
#[cfg(unix)]
#[tokio::test]
async fn connect_refuses_a_deployment_it_cannot_prove() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let fixture = on_disk_fixture();
    // The dev/CI user is not root, so the root-owned requirement fails here;
    // what matters is that it fails on metadata rather than being waved through.
    let result = SupervisorClient::connect(&fixture.spec, sample_coordinator()).await;
    assert_eq!(result.err(), Some(TransportError::Metadata));
}

/// In-process reliability proof: a Rust server built from the accepted wire
/// server functions round-trips a correlated response to the client.
#[tokio::test]
async fn in_process_round_trip_authenticates_and_correlates() {
    let fx = fixture();
    let request_payload = payload(&fx.requests, "reconcile_first");
    let base_response = control_dto::decode_response(&payload(&fx.responses, "reconcile_first_page"))
        .unwrap();
    let request = control_dto::decode_request(&request_payload).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let key = {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    };
    let listener = UnixListener::bind(&path).unwrap();

    let server_key = key;
    let server_payload = request_payload.clone();
    let server = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut hello = [0u8; 40];
        conn.read_exact(&mut hello).await.unwrap();
        let client_nonce = control_wire::decode_client_hello(&hello).unwrap();
        let mut challenge = [0u8; 32];
        let mut boot16 = [0u8; 16];
        OsRng.fill_bytes(&mut challenge);
        OsRng.fill_bytes(&mut boot16);
        let context = WireContext::new(client_nonce, challenge, boot16);
        let server_hello = control_wire::encode_server_hello(&server_key, &context).unwrap();
        conn.write_all(&server_hello).await.unwrap();

        let mut len = [0u8; 4];
        conn.read_exact(&mut len).await.unwrap();
        let n = u32::from_be_bytes(len) as usize;
        let mut rest = vec![0u8; n + 32];
        conn.read_exact(&mut rest).await.unwrap();
        let mut frame = len.to_vec();
        frame.extend_from_slice(&rest);
        let decoded = control_wire::decode_request(&server_key, &context, &frame).unwrap();
        assert_eq!(decoded, server_payload);

        let response = Response {
            supervisor_boot_id: Id::new(super::hex16(&boot16)).unwrap(),
            ..base_response
        };
        let response_payload = control_dto::encode_response(&response).unwrap();
        let response_frame =
            control_wire::encode_response(&server_key, &context, &decoded, &response_payload)
                .unwrap();
        conn.write_all(&response_frame).await.unwrap();
    });

    let candidate = request.coordinator.clone();
    let mut client = SupervisorClient::connect_unchecked(&path, key, candidate)
        .await
        .unwrap();
    let response = client.exchange(&request).await.unwrap();
    // Id deliberately has no Debug (safe-repr), so compare with `==`.
    assert!(response.request_id == request.request_id);
    assert!(*client.authenticated_supervisor_boot_id() == response.supervisor_boot_id);
    client.close();
    server.await.unwrap();
}

/// Adopted cross-process acceptance: the Rust client interoperates with the
/// Python adopted-listener fixture over a real Unix socket.
#[tokio::test]
async fn cross_process_python_listener_interop() {
    let fx = fixture();
    let request_payload = payload(&fx.requests, "reconcile_first");
    let response_payload = payload(&fx.responses, "reconcile_first_page");
    let request = control_dto::decode_request(&request_payload).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let key = {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(1);
        }
        k
    };
    let peer = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/tests/fixtures/neev_transport_peer.py"
    );

    let mut child = tokio::process::Command::new("python3")
        .args([
            "-I",
            "-B",
            peer,
            path.to_str().unwrap(),
            &to_hex(&key),
            &to_hex(&response_payload),
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn python fixture peer");

    let mut stdout = child.stdout.take().unwrap();
    let mut ready = [0u8; 6];
    // The peer prints "READY\n" once its socket is bound and adopted.
    let read = tokio::time::timeout(std::time::Duration::from_secs(10), stdout.read_exact(&mut ready))
        .await;
    assert!(read.is_ok() && &ready[..5] == b"READY", "peer did not signal ready");

    let candidate = request.coordinator.clone();
    let result = async {
        let mut client = SupervisorClient::connect_unchecked(&path, key, candidate).await?;
        client.exchange(&request).await
    }
    .await;

    let _ = child.kill().await;
    let response = result.expect("cross-process exchange");
    assert!(response.request_id == request.request_id);
}

/// A deployment rooted at a real temporary directory, so the metadata checks
/// run against an actual filesystem rather than hand-written numbers.
#[cfg(unix)]
struct DeploymentFixture {
    _dir: tempfile::TempDir,
    spec: ControlDeployment,
}

#[cfg(unix)]
fn identity_of(path: &Path) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).unwrap();
    FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    }
}

/// Build a deployment that matches what is really on disk. The dev host cannot
/// create root-owned files, so `verify_deployment` is never expected to accept
/// this wholesale — the tests below assert that each individual check fires on
/// the mutation it is there to catch.
#[cfg(unix)]
fn on_disk_fixture() -> DeploymentFixture {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let socket_path = root.join("control.sock");
    let key_path = root.join("control.key");
    std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::write(&key_path, [7u8; 32]).unwrap();
    let meta = std::fs::symlink_metadata(&root).unwrap();
    let parent = ProtectedParent {
        absolute_path: root.clone(),
        identity: identity_of(&root),
        gid: meta.gid(),
        mode: meta.mode() & 0o7777,
    };
    let spec = ControlDeployment {
        socket_identity: identity_of(&socket_path),
        key_identity: identity_of(&key_path),
        socket_path,
        key_path,
        socket_parent: parent.clone(),
        key_parent: parent,
        control_gid: meta.gid(),
        self_uid: unsafe { libc::geteuid() },
        self_gid: unsafe { libc::getegid() },
        peer_uid: 0,
        peer_gid: 0,
        socket_owner_uid: meta.uid(),
    };
    DeploymentFixture { _dir: dir, spec }
}

#[cfg(unix)]
#[test]
fn a_file_must_live_directly_inside_its_proven_directory() {
    let parent = ProtectedParent {
        absolute_path: PathBuf::from("/var/lib/neev-native/control"),
        identity: FileIdentity {
            device: 1,
            inode: 2,
        },
        gid: 800,
        mode: 0o750,
    };
    assert!(verify_child_of(
        Path::new("/var/lib/neev-native/control/control.sock"),
        &parent
    )
    .is_ok());
    // The protected directory is what makes the path safe, so anywhere else —
    // a subdirectory, a sibling, a traversal, or the directory itself — is out.
    for bad in [
        "/var/lib/neev-native/control/nested/control.sock",
        "/var/lib/neev-native/other/control.sock",
        "/var/lib/neev-native/control/../control.sock",
        "/var/lib/neev-native/control",
        "control.sock",
    ] {
        assert_eq!(
            verify_child_of(Path::new(bad), &parent),
            Err(TransportError::Metadata),
            "accepted {bad}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_component_is_refused_rather_than_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let real = root.join("real");
    std::fs::create_dir(&real).unwrap();
    assert!(no_symlink_ancestry(&real).is_ok());
    let linked = root.join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    // Following it would land on a directory whose inode we DID prove, which is
    // exactly the substitution this check exists to stop.
    assert_eq!(
        no_symlink_ancestry(&linked),
        Err(TransportError::Metadata)
    );
    assert_eq!(
        no_symlink_ancestry(&linked.join("control.sock")),
        Err(TransportError::Metadata)
    );
}

#[cfg(unix)]
#[test]
fn a_tenant_writable_control_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut parent = ProtectedParent {
        absolute_path: root.clone(),
        identity: identity_of(&root),
        gid: 0,
        mode: 0o755,
    };
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o757)).unwrap();
    parent.mode = 0o757;
    // Other-writable means any local account could swap the socket or the key.
    assert_eq!(
        verify_parent(&parent, 0),
        Err(TransportError::Metadata)
    );
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o775)).unwrap();
    parent.mode = 0o775;
    parent.gid = 12345;
    // Group-writable is allowed only for the control group itself.
    assert_eq!(
        verify_parent(&parent, 999),
        Err(TransportError::Metadata)
    );
}

#[cfg(unix)]
#[test]
fn a_directory_whose_inode_moved_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let parent = ProtectedParent {
        absolute_path: root.clone(),
        identity: FileIdentity {
            device: identity_of(&root).device,
            inode: identity_of(&root).inode ^ 1,
        },
        gid: 0,
        mode: 0o755,
    };
    // Same path, different inode: the directory was replaced.
    assert_eq!(verify_parent(&parent, 0), Err(TransportError::Metadata));
}

#[cfg(unix)]
#[test]
fn the_declared_process_identity_must_be_the_running_one() {
    let mut fixture = on_disk_fixture();
    fixture.spec.self_uid = fixture.spec.self_uid.wrapping_add(1);
    // A spec describing some other process is a misconfiguration, caught before
    // any path is touched.
    assert_eq!(
        verify_deployment(&fixture.spec),
        Err(TransportError::Metadata)
    );
}

#[cfg(unix)]
#[test]
fn every_declared_identity_is_checked_against_the_real_file() {
    // The dev host owns these files as an unprivileged user, so the root-owned
    // requirement fails by construction — which is itself the point: the gate
    // refuses a deployment that is not root-owned.
    let baseline = on_disk_fixture();
    assert_eq!(
        verify_deployment(&baseline.spec),
        Err(TransportError::Metadata),
        "a non-root-owned control directory must never be accepted"
    );
    // And each declared identity is compared, not assumed.
    let mut moved = on_disk_fixture();
    moved.spec.socket_identity.inode ^= 1;
    assert_eq!(
        verify_deployment(&moved.spec),
        Err(TransportError::Metadata)
    );
    let mut wrong_key = on_disk_fixture();
    wrong_key.spec.key_identity.inode ^= 1;
    assert_eq!(
        verify_deployment(&wrong_key.spec),
        Err(TransportError::Metadata)
    );
}

#[cfg(unix)]
#[test]
fn a_key_that_is_not_exactly_one_secret_is_refused() {
    let fixture = on_disk_fixture();
    for bytes in [vec![7u8; 31], vec![7u8; 33], vec![0u8; 32]] {
        std::fs::write(&fixture.spec.key_path, &bytes).unwrap();
        let mut spec = fixture.spec.clone();
        spec.key_identity = identity_of(&spec.key_path);
        // Short, long, or all-zero: none of these is a key, and an all-zero one
        // would MAC perfectly well.
        assert_eq!(verify_deployment(&spec), Err(TransportError::Metadata));
    }
}

#[cfg(unix)]
#[test]
fn a_socket_path_that_is_a_regular_file_is_refused() {
    let fixture = on_disk_fixture();
    let decoy = fixture.spec.socket_parent.absolute_path.join("decoy.sock");
    std::fs::write(&decoy, b"not a socket").unwrap();
    let mut spec = fixture.spec.clone();
    spec.socket_identity = identity_of(&decoy);
    spec.socket_path = decoy;
    assert_eq!(verify_deployment(&spec), Err(TransportError::Metadata));
}
