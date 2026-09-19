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
        socket_parent: parent.clone(),
        key_parent: parent,
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
