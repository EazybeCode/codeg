//! Test-only helpers for the bridge client.

use super::upstream::Upstream;
use std::sync::{Arc, Mutex};

/// The real constructor, pointed at a local fake bridge, with a 32-character
/// dummy key.
pub(crate) fn upstream_for(base: &str) -> Upstream {
    Upstream::new(
        url::Url::parse(base).unwrap(),
        "https://app.neev.test".into(),
        "k".repeat(32),
    )
    .unwrap()
}

/// A fake bridge answering successive connections with `responses` in order
/// (the last one repeats). Returns the base URL and the recorded request
/// bodies, so a test can assert what Neev asked for (e.g. `if_none_match`).
pub(crate) async fn fake_bridge(
    responses: Vec<(u16, String)>,
) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = Vec::new();
            let mut chunk = vec![0u8; 16 * 1024];
            // Read headers, then the declared body.
            loop {
                let Ok(read) = socket.read(&mut chunk).await else { break };
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|line| {
                            let lower = line.to_ascii_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buffer.len() >= split + 4 + length {
                        recorded
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&buffer[split + 4..split + 4 + length]).to_string());
                        break;
                    }
                }
            }
            let (status, body) = responses[index.min(responses.len() - 1)].clone();
            index += 1;
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{addr}/"), seen)
}
