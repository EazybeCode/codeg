use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn endpoint(status: u16, body: &'static str) -> (GraphFiles, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            if let Ok((s, _)) = listener.accept() {
                break s;
            }
            if std::time::Instant::now() >= deadline {
                return String::new();
            }
            thread::sleep(Duration::from_millis(5));
        };
        // macOS may inherit nonblocking mode from the listening socket.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut data = Vec::new();
        loop {
            let mut b = [0; 1024];
            let n = stream.read(&mut b).unwrap();
            data.extend_from_slice(&b[..n]);
            if n == 0 || data.windows(4).any(|x| x == b"\r\n\r\n") {
                break;
            }
        }
        write!(stream, "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        String::from_utf8(data).unwrap()
    });
    let mut graph = GraphFiles::new().unwrap();
    graph.base = format!("http://{address}");
    (graph, handle)
}
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
#[test]
fn discovers_actual_drive_and_encodes_channel_id() {
    let (graph, server) = endpoint(
        200,
        r#"{"id":"folder1","parentReference":{"driveId":"drive1"},"folder":{},"webUrl":"https://tenant.sharepoint.com/sites/sales/Files","eTag":"\"1\""}"#,
    );
    let result = run(graph.channel_folder("test-token", "team1", "19:a@thread.tacv2"));
    let request = server.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(result.unwrap().drive_id, "drive1");
    assert!(
        request.starts_with("GET /teams/team1/channels/19%3Aa%40thread.tacv2/filesFolder HTTP/1.1")
    );
}
#[test]
fn access_denied_is_not_retried_or_reclassified_as_missing() {
    let (graph, server) = endpoint(
        403,
        r#"{"error":{"message":"sensitive response must not appear in error"}}"#,
    );
    assert!(matches!(
        run(graph.channel_folder("test-token", "team", "channel")),
        Err(GraphError::Forbidden)
    ));
    server.join().unwrap();
}
#[test]
fn malformed_discovery_never_guesses_drive_id() {
    let (graph, server) = endpoint(
        200,
        r#"{"id":"folder1","folder":{},"webUrl":"https://tenant.sharepoint.com"}"#,
    );
    assert!(matches!(
        run(graph.channel_folder("test-token", "team", "channel")),
        Err(GraphError::InvalidResponse)
    ));
    server.join().unwrap();
}
fn parent() -> Item {
    Item {
        drive_id: "drive1".into(),
        item_id: "folder1".into(),
        web_url: "https://tenant.sharepoint.com/folder".into(),
        etag: "\"1\"".into(),
    }
}
#[test]
fn creates_child_folder_without_permission_mutations() {
    let (graph, server) = endpoint(
        201,
        r#"{"id":"child","parentReference":{"driveId":"drive1"},"folder":{},"webUrl":"https://tenant.sharepoint.com/folder/Neevz"}"#,
    );
    let item = run(graph.folder("test-token", &parent(), "Neevz")).unwrap();
    let request = server.join().unwrap();
    assert_eq!(item.item_id, "child");
    assert!(request.starts_with("POST /drives/drive1/items/folder1/children HTTP/1.1"));
    assert!(request.contains(r#""@microsoft.graph.conflictBehavior":"fail""#));
}
#[test]
fn new_upload_is_create_only_and_uses_no_guessed_share_url() {
    let (graph, server) = endpoint(
        200,
        r#"{"uploadUrl":"https://tenant.sharepoint.com/upload/secret","expirationDateTime":"2026-10-14T00:00:00Z"}"#,
    );
    let session = run(graph.begin("test-token", &parent(), "Proposal.docx")).unwrap();
    let request = server.join().unwrap();
    assert_eq!(session.expires_at, "2026-10-14T00:00:00Z");
    assert!(request.starts_with(
        "POST /drives/drive1/items/folder1:/Proposal.docx:/createUploadSession HTTP/1.1"
    ));
    assert!(request.contains(r#""@microsoft.graph.conflictBehavior":"fail""#));
}
#[test]
fn revision_fetches_latest_version_without_overwriting() {
    let (graph, server) = endpoint(
        200,
        r#"{"id":"folder1","parentReference":{"driveId":"drive1"},"webUrl":"https://tenant.sharepoint.com/file.docx","eTag":"\"2\""}"#,
    );
    let item = run(graph.current("test-token", &parent())).unwrap();
    assert_eq!(item.etag, "\"2\"");
    assert_eq!(item.web_url, "https://tenant.sharepoint.com/file.docx");
    assert!(server
        .join()
        .unwrap()
        .starts_with("GET /drives/drive1/items/folder1 HTTP/1.1"));
}
#[test]
fn upload_transfers_bytes_without_forwarding_graph_token() {
    let (graph, server) = endpoint(
        201,
        r#"{"id":"doc1","parentReference":{"driveId":"drive1"},"webUrl":"https://tenant.sharepoint.com/file.docx","eTag":"\"1\""}"#,
    );
    // Direct construction is test-only: production sessions validate HTTPS hosts.
    let session = UploadSession {
        upload_url: graph.base.clone(),
        expires_at: "later".into(),
        drive_id: "drive1".into(),
    };
    let item = run(graph.complete_small(&session, b"abc".to_vec())).unwrap();
    let request = server.join().unwrap().to_lowercase();
    assert_eq!(item.item_id, "doc1");
    assert!(request.contains("content-range: bytes 0-2/3"));
    assert!(!request.contains("authorization:"));
}
#[test]
fn session_url_cannot_redirect_upload_to_arbitrary_host() {
    for url in [
        "http://tenant.sharepoint.com/x",
        "https://evil.example/x",
        "https://tenant.sharepoint.com.evil.example/x",
        "https://user@tenant.sharepoint.com/x",
        "https://127.0.0.1/x",
    ] {
        assert!(UploadSession::from_response(
            &json!({"uploadUrl":url,"expirationDateTime":"later"}),
            "drive".into()
        )
        .is_err());
    }
}
#[test]
fn interrupted_upload_can_query_status_without_authorization() {
    let (graph, server) = endpoint(200, r#"{"nextExpectedRanges":["0-"]}"#);
    let session = UploadSession {
        upload_url: graph.base.clone(),
        expires_at: "later".into(),
        drive_id: "drive1".into(),
    };
    let result = run(graph.upload_status(&session)).unwrap();
    assert_eq!(result["nextExpectedRanges"][0], "0-");
    assert!(!server
        .join()
        .unwrap()
        .to_lowercase()
        .contains("authorization:"));
}

fn sequence(replies: Vec<(u16, String)>) -> (GraphFiles, UploadSession, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut data = Vec::new();
            let header_end;
            loop {
                let mut b=[0;8192]; let n=stream.read(&mut b).unwrap(); assert!(n>0);
                data.extend_from_slice(&b[..n]);
                if let Some(i)=data.windows(4).position(|w|w==b"\r\n\r\n") {header_end=i+4;break;}
            }
            let headers=String::from_utf8(data[..header_end].to_vec()).unwrap();
            let length=headers.lines().find_map(|l|l.to_lowercase().strip_prefix("content-length:").map(|v|v.trim().parse::<usize>().unwrap())).unwrap_or(0);
            while data.len()<header_end+length {let mut b=[0;8192];let n=stream.read(&mut b).unwrap();assert!(n>0);data.extend_from_slice(&b[..n]);}
            requests.push(headers);
            if status==0 {continue;}
            // 299 is a fixture sentinel: final success headers followed by a
            // truncated body, simulating loss after the server committed bytes.
            if status==299 {
                write!(stream,"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()+20).unwrap();
                continue;
            }
            write!(stream,"HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    let graph=GraphFiles::new().unwrap();
    let session=UploadSession{upload_url:format!("http://{address}/secret"), expires_at:"later".into(), drive_id:"drive1".into()};
    (graph,session,handle)
}
fn completed() -> String {r#"{"id":"doc1","parentReference":{"driveId":"drive1"},"webUrl":"https://tenant.sharepoint.com/file.docx"}"#.into()}
#[test]
fn multipart_accepts_more_than_ten_mib_and_aligned_short_final_chunk() {
    let size=11*1024*1024+7;
    let mut replies=Vec::new();
    let chunk=5*1024*1024;
    for next in [chunk,chunk*2] {replies.push((202,json!({"nextExpectedRanges":[format!("{next}-")]}).to_string()));}
    replies.push((201,completed()));
    let (graph,session,server)=sequence(replies);
    let result=run(graph.complete_small(&session,vec![7;size]));
    assert!(result.is_ok(),"{result:?}");
    let requests=server.join().unwrap();
    for (i,request) in requests.iter().enumerate() {
        let start=i*chunk;let end=(start+chunk).min(size)-1;
        assert!(request.contains(&format!("content-range: bytes {start}-{end}/{size}")));
        assert!(!request.to_lowercase().contains("authorization:"));
    }
}
#[test]
fn resumes_at_server_confirmed_offset() {
    let n=UPLOAD_CHUNK_BYTES;
    let (graph,session,server)=sequence(vec![(200,json!({"nextExpectedRanges":[format!("{n}-")]}).to_string()),(201,completed())]);
    assert!(run(graph.resume(&session,&vec![1;n+3])).is_ok());
    let requests=server.join().unwrap();
    assert!(requests[0].starts_with("GET "));
    assert!(requests[1].contains(&format!("content-range: bytes {n}-{}/{total}",n+2,total=n+3)));
}
#[test]
fn lost_fragment_ack_queries_status_before_sending_next_fragment() {
    let n=UPLOAD_CHUNK_BYTES;
    let (graph,session,server)=sequence(vec![(0,String::new()),(200,json!({"nextExpectedRanges":[format!("{n}-")]}).to_string()),(201,completed())]);
    assert!(run(graph.complete(&session,&vec![1;n+3])).is_ok());
    let requests=server.join().unwrap();
    assert!(requests[1].starts_with("GET "));
    assert!(requests[2].contains(&format!("content-range: bytes {n}-")));
}
#[test]
fn lost_final_ack_and_missing_session_remains_uncertain() {
    let (graph,session,server)=sequence(vec![(0,String::new()),(404,"{}".into())]);
    assert!(matches!(run(graph.complete(&session,b"abc")),Err(GraphError::UncertainTransport)));
    let requests=server.join().unwrap();
    assert_eq!(requests.len(),2);
    assert!(requests[1].starts_with("GET "));
}
#[test]
fn conflict_never_overwrites_or_creates_another_session() {
    let (graph,session,server)=sequence(vec![(409,"{}".into())]);
    assert!(matches!(run(graph.complete(&session,b"abc")),Err(GraphError::Conflict)));
    assert_eq!(server.join().unwrap().len(),1);
}
#[test]
fn retryable_response_returns_to_durable_scheduler_without_hot_loop() {
    let (graph,session,server)=sequence(vec![(429,"{}".into())]);
    assert!(matches!(run(graph.complete(&session,b"abc")),Err(GraphError::Retryable{status:429,..})));
    assert_eq!(server.join().unwrap().len(),1);
}
#[test]
fn unaligned_resume_range_is_rejected_before_sending_bytes() {
    let (graph,session,server)=sequence(vec![(200,r#"{"nextExpectedRanges":["3-"]}"#.into())]);
    assert!(matches!(run(graph.resume(&session,b"abcdef")),Err(GraphError::InvalidResponse)));
    assert_eq!(server.join().unwrap().len(),1);
}
#[test]
fn upload_redirect_is_rejected_without_forwarding_capability() {
    let (graph,session,server)=sequence(vec![(302,"{}".into())]);
    assert!(matches!(run(graph.complete(&session,b"abc")),Err(GraphError::Http(302))));
    assert_eq!(server.join().unwrap().len(),1);
}

#[test]
fn truncated_final_success_body_reconciles_before_any_more_writes() {
    let (graph,session,server)=sequence(vec![(299,"{\"id\":".into()),(404,"{}".into())]);
    let result=run(graph.complete(&session,b"abc"));
    assert!(matches!(result,Err(GraphError::UncertainTransport)),"{result:?}");
    let requests=server.join().unwrap();
    assert_eq!(requests.len(),2);
    assert!(requests[0].starts_with("PUT "));
    assert!(requests[1].starts_with("GET "));
}
#[test]
fn resume_missing_session_remains_uncertain_without_writing() {
    let (graph,session,server)=sequence(vec![(404,"{}".into())]);
    let result=run(graph.resume(&session,b"abc"));
    assert!(matches!(result,Err(GraphError::UncertainTransport)),"{result:?}");
    let requests=server.join().unwrap();
    assert_eq!(requests.len(),1);
    assert!(requests[0].starts_with("GET "));
}

