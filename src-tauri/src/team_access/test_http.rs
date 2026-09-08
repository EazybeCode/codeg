use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::Response,
    routing::any,
    Router,
};
use futures_util::stream;
use serde_json::Value;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RecordedRequest {
    pub(super) method: Method,
    pub(super) path: String,
    pub(super) query: Option<String>,
    pub(super) authorization: Option<String>,
    pub(super) user_agent: Option<String>,
    pub(super) accept: Option<String>,
    pub(super) api_version: Option<String>,
    pub(super) content_type: Option<String>,
    pub(super) body: Vec<u8>,
}

pub(super) enum FixtureBody {
    Bytes(Vec<u8>),
    Chunks(Vec<Vec<u8>>),
    Delayed(Duration, Vec<u8>),
}

pub(super) struct FixtureResponse {
    pub(super) status: StatusCode,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: FixtureBody,
}

impl FixtureResponse {
    pub(super) fn json(status: StatusCode, value: Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: FixtureBody::Bytes(serde_json::to_vec(&value).unwrap()),
        }
    }

    pub(super) fn raw(status: StatusCode, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![],
            body: FixtureBody::Bytes(bytes.into()),
        }
    }

    pub(super) fn with_header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct FixtureState {
    responses: Mutex<VecDeque<FixtureResponse>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

pub(super) struct FixtureServer {
    origin: String,
    state: Arc<FixtureState>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl FixtureServer {
    pub(super) async fn start(responses: Vec<FixtureResponse>) -> Self {
        let state = Arc::new(FixtureState {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .fallback(any(fixture_handler))
            .with_state(Arc::clone(&state));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
                .unwrap();
        });
        Self {
            origin: format!("http://{address}"),
            state,
            shutdown: Some(shutdown),
            task,
        }
    }

    pub(super) fn origin(&self) -> &str {
        &self.origin
    }

    pub(super) fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().unwrap().clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn fixture_handler(
    State(state): State<Arc<FixtureState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    fn header(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    state.requests.lock().unwrap().push(RecordedRequest {
        method,
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        authorization: header(&headers, "authorization"),
        user_agent: header(&headers, "user-agent"),
        accept: header(&headers, "accept"),
        api_version: header(&headers, "x-github-api-version"),
        content_type: header(&headers, "content-type"),
        body: body.to_vec(),
    });
    let scripted = state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .expect("fixture received an unexpected request");
    let mut builder = Response::builder().status(scripted.status);
    for (name, value) in scripted.headers {
        builder = builder.header(name, value);
    }
    let body = match scripted.body {
        FixtureBody::Bytes(bytes) => Body::from(bytes),
        FixtureBody::Chunks(chunks) => Body::from_stream(stream::iter(
            chunks
                .into_iter()
                .map(Ok::<Vec<u8>, std::io::Error>),
        )),
        FixtureBody::Delayed(delay, bytes) => {
            tokio::time::sleep(delay).await;
            Body::from(bytes)
        }
    };
    builder.body(body).unwrap()
}
