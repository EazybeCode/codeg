use std::sync::Arc;
use std::time::Duration;

use axum::http::header;
use serde_json::Value;

use crate::neev::auth::session;
use crate::neev::catalog::nodes;
use crate::neev::content::realtime::StreamPolicy;
use crate::neev::content::{
    messages, resources,
    types::{ResourceInput, ResourceKind, ResourcePatch, Visibility},
};
use crate::neev::startup::ServerState;
use crate::neev::test_support as h;
use crate::neev::types::{NodeInput, NodeKind, OrgContext};

use super::cookies::CookieNames;

/// Short enough that a collected SSE body finishes inside a test, long enough
/// that the opening `sync` and one pushed notice both land.
fn test_policy() -> StreamPolicy {
    StreamPolicy {
        heartbeat: Duration::from_millis(150),
        coalesce: Duration::from_millis(10),
        lifetime: Duration::from_millis(700),
    }
}

const BODY: &str = "Ship the realtime patch";

struct Harness {
    server: axum_test::TestServer,
    state: Arc<ServerState>,
    owner: OrgContext,
    member: OrgContext,
    member_member_id: i32,
    owner_cookies: String,
    member_cookies: String,
    foreign_cookies: String,
    /// Org-visible chat created by the owner.
    chat: String,
    /// The owner's private chat: invisible to the member.
    private_chat: String,
    document: String,
}

async fn cookies(
    database: &sea_orm::DatabaseConnection,
    user: i32,
    org: i32,
    names: &CookieNames,
) -> String {
    let issued = session::issue_identity(database, user, 1_700_000_000)
        .await
        .unwrap();
    let identity = session::identity(database, &issued.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped = session::select_organization(database, &identity, org, 1_700_000_000)
        .await
        .unwrap();
    format!(
        "{}={}; {}={}",
        names.session, scoped.session, names.csrf, scoped.csrf
    )
}

impl Harness {
    async fn build(realtime_enabled: bool) -> Self {
        let database = h::database().await;
        let clock = Arc::new(h::TestClock::new(1_700_000_000));
        let mut config = h::preview_config();
        config.realtime_enabled = realtime_enabled;
        let mut state = ServerState::new(database.clone(), config, clock).unwrap();
        state.set_realtime_policy(test_policy());
        let state = Arc::new(state);
        let owner_user = h::native_account(&database, "rt-owner", "rt-owner@example.test").await;
        let member_user = h::native_account(&database, "rt-member", "rt-member@example.test").await;
        let foreign_user =
            h::native_account(&database, "rt-foreign", "rt-foreign@example.test").await;
        let (org, _) = h::organization(&database, owner_user, "admin").await;
        let member_member_id = h::add_membership(&database, org, member_user, "member").await;
        let (other_org, _) = h::organization(&database, foreign_user, "admin").await;
        let owner = h::scoped_context(&database, owner_user, org, 1_700_000_000).await;
        let member = h::scoped_context(&database, member_user, org, 1_700_000_000).await;
        let node = nodes::create(
            &state,
            &owner,
            NodeInput {
                parent_id: None,
                kind: NodeKind::Project,
                name: "Realtime".into(),
            },
        )
        .await
        .unwrap()
        .id;
        let make = |kind: ResourceKind, title: &str| ResourceInput {
            node_id: node,
            kind,
            title: title.into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        };
        let chat = resources::create(&state, &owner, make(ResourceKind::Chat, "Team"))
            .await
            .unwrap();
        let shared = resources::update(
            &state,
            &owner,
            &chat.id,
            ResourcePatch {
                expected_auth_revision: chat.auth_revision,
                title: chat.title.clone(),
                node_id: chat.node_id,
                visibility: Visibility::Organization,
                archived: false,
            },
        )
        .await
        .unwrap();
        let private_chat = resources::create(&state, &owner, make(ResourceKind::Chat, "Solo"))
            .await
            .unwrap()
            .id;
        let document = resources::create(&state, &owner, make(ResourceKind::Document, "Notes"))
            .await
            .unwrap()
            .id;
        let names = CookieNames::for_preview(true);
        Self {
            server: axum_test::TestServer::new(super::super::router(state.clone())).unwrap(),
            owner_cookies: cookies(&database, owner_user, org, &names).await,
            member_cookies: cookies(&database, member_user, org, &names).await,
            foreign_cookies: cookies(&database, foreign_user, other_org, &names).await,
            state,
            owner,
            member,
            member_member_id,
            chat: shared.id,
            private_chat,
            document,
        }
    }

    async fn new() -> Self {
        Self::build(true).await
    }

    fn path(&self, id: &str) -> String {
        format!("/api/neev/chats/{id}/stream")
    }

    /// Collect the member's stream until the server closes it.
    async fn member_stream(&self, id: &str) -> axum_test::TestResponse {
        self.server
            .get(&self.path(id))
            .add_header(header::COOKIE, &self.member_cookies)
            .await
    }

    async fn append(&self, body: &str) {
        messages::append(
            &self.state,
            &self.owner,
            &self.chat,
            &uuid::Uuid::new_v4().to_string(),
            2,
            body,
        )
        .await
        .unwrap();
    }
}

fn frames(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|json| serde_json::from_str(json.trim()).expect("each frame is JSON"))
        .collect()
}

fn kinds(frames: &[Value]) -> Vec<&str> {
    frames
        .iter()
        .map(|frame| frame["type"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn stream_requires_an_organization_scoped_session() {
    let h = Harness::new().await;
    let path = h.path(&h.chat);
    h.server.get(&path).await.assert_status_unauthorized();
    // A slot is never reserved for a caller that failed authentication.
    assert_eq!(h.state.realtime.subscriber_count(), 0);
}

#[tokio::test]
async fn stream_never_admits_a_foreign_tenant_or_leaks_that_the_chat_exists() {
    let h = Harness::new().await;
    let response = h
        .server
        .get(&h.path(&h.chat))
        .add_header(header::COOKIE, &h.foreign_cookies)
        .await;
    response.assert_status_not_found();
    // Same answer as an id that does not exist at all: no existence signal.
    let unknown = h
        .server
        .get(&h.path("33333333-3333-4333-8333-333333333333"))
        .add_header(header::COOKIE, &h.foreign_cookies)
        .await;
    unknown.assert_status_not_found();
    assert_eq!(response.text(), unknown.text());
    assert_eq!(
        (h.state.realtime.topic_count(), h.state.realtime.subscriber_count()),
        (0, 0),
        "a denied caller must not allocate fan-out state"
    );
}

#[tokio::test]
async fn stream_refuses_a_private_conversation_and_a_document_id() {
    let h = Harness::new().await;
    // Private chats stay creator-only, exactly as `messages::list` enforces.
    h.server
        .get(&h.path(&h.private_chat))
        .add_header(header::COOKIE, &h.member_cookies)
        .await
        .assert_status_not_found();
    // A document id is not a conversation, even for its own creator.
    h.server
        .get(&h.path(&h.document))
        .add_header(header::COOKIE, &h.owner_cookies)
        .await
        .assert_status_not_found();
    assert_eq!(h.state.realtime.topic_count(), 0);
}

#[tokio::test]
async fn an_authorized_member_is_pushed_a_sequence_only_notice() {
    let h = Harness::new().await;
    // Let the subscription land before the append, then post as another author.
    let (response, ()) = tokio::join!(h.member_stream(&h.chat), async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        h.append(BODY).await;
    });
    response.assert_status_ok();
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
    let body = response.text();
    let frames = frames(&body);
    let kinds = kinds(&frames);
    assert_eq!(kinds.first(), Some(&"sync"), "opening gap-closer missing: {body}");
    let pushed = frames
        .iter()
        .find(|frame| frame["type"] == "message")
        .unwrap_or_else(|| panic!("no pushed notice: {body}"));
    assert_eq!(pushed["sequence"], "1");
    // The wire carries a cursor, never the conversation itself.
    assert!(!body.contains(BODY), "message content leaked: {body}");
    assert!(!body.contains("rt-owner"), "author leaked: {body}");
    assert!(
        frames
            .iter()
            .all(|frame| frame.as_object().unwrap().len() <= 2),
        "unexpected fields on the wire: {body}"
    );
}

#[tokio::test]
async fn losing_membership_stops_delivery_instead_of_continuing_it() {
    let h = Harness::new().await;
    let (response, ()) = tokio::join!(h.member_stream(&h.chat), async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        crate::neev::org::remove(&h.state, &h.owner, h.member_member_id)
            .await
            .unwrap();
        // A message posted after the removal must never reach the removed member.
        h.append(BODY).await;
    });
    let body = response.text();
    let parsed = frames(&body);
    let kinds = kinds(&parsed);
    assert_eq!(kinds.last(), Some(&"revoked"), "stream did not stop: {body}");
    assert!(
        !kinds.contains(&"message"),
        "delivered after revocation: {body}"
    );
    assert!(!body.contains(BODY), "content leaked after revocation: {body}");
}

#[tokio::test]
async fn making_a_conversation_private_revokes_a_live_reader() {
    let h = Harness::new().await;
    let (response, ()) = tokio::join!(h.member_stream(&h.chat), async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let current = resources::get(&h.state, &h.owner, &h.chat).await.unwrap();
        resources::update(
            &h.state,
            &h.owner,
            &h.chat,
            ResourcePatch {
                expected_auth_revision: current.auth_revision,
                title: current.title.clone(),
                node_id: current.node_id,
                visibility: Visibility::Private,
                archived: false,
            },
        )
        .await
        .unwrap();
    });
    let body = response.text();
    assert_eq!(
        kinds(&frames(&body)).last(),
        Some(&"revoked"),
        "audience change did not stop the stream: {body}"
    );
}

#[tokio::test]
async fn a_finished_stream_releases_its_slot_and_its_topic() {
    let h = Harness::new().await;
    let (response, live) = tokio::join!(h.member_stream(&h.chat), async {
        tokio::time::sleep(Duration::from_millis(60)).await;
        h.state.realtime.subscriber_count()
    });
    assert_eq!(live, 1, "an open stream must hold exactly one slot");
    response.assert_status_ok();
    // The pump task drops its subscription as it unwinds.
    for _ in 0..50 {
        if h.state.realtime.subscriber_count() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        (h.state.realtime.topic_count(), h.state.realtime.subscriber_count()),
        (0, 0),
        "a closed connection kept fan-out state alive"
    );
}

#[tokio::test]
async fn rolling_realtime_back_removes_the_route_and_reports_the_capability_honestly() {
    let h = Harness::build(false).await;
    let capabilities: Value = h.server.get("/api/workos/capabilities").await.json();
    assert_eq!(capabilities["realtime"], false);
    let response = h
        .server
        .get(&h.path(&h.chat))
        .add_header(header::COOKIE, &h.member_cookies)
        .await;
    assert!(
        !response
            .maybe_header(header::CONTENT_TYPE)
            .is_some_and(|value| value == "text/event-stream"),
        "the stream route is still mounted after rollback"
    );
    // Appending still works and still publishes into a hub nobody listens to.
    h.append(BODY).await;
    assert_eq!(
        messages::list(&h.state, &h.member, &h.chat, None)
            .await
            .unwrap()
            .items
            .len(),
        1,
        "polling delivery must be unaffected by the rollback"
    );
}

#[tokio::test]
async fn realtime_is_advertised_when_it_is_mounted() {
    let h = Harness::new().await;
    let capabilities: Value = h.server.get("/api/workos/capabilities").await.json();
    assert_eq!(capabilities["realtime"], true);
}

/// Member/team sharing: removing the reader's grant ends an established stream
/// on its next heartbeat recheck, with no content notice in between; a folder
/// grant removal does the same.
#[tokio::test]
async fn removing_a_grant_revokes_a_live_reader_without_any_notice() {
    use crate::neev::content::grants::{self, Level, Principal};
    for through_folder in [false, true] {
        let h = Harness::new().await;
        let current = resources::get(&h.state, &h.owner, &h.chat).await.unwrap();
        let shared = resources::update(
            &h.state,
            &h.owner,
            &h.chat,
            ResourcePatch {
                expected_auth_revision: current.auth_revision,
                title: current.title.clone(),
                node_id: current.node_id,
                visibility: Visibility::Shared,
                archived: false,
            },
        )
        .await
        .unwrap();
        let reader = Principal::User(h.member.identity.user_id);
        if through_folder {
            let node = grants::node_access(&h.state, &h.owner, shared.node_id).await.unwrap();
            grants::set_node_grant(&h.state, &h.owner, shared.node_id, node.revision, &reader, Some(Level::View))
                .await
                .unwrap();
        } else {
            grants::set_resource_grant(&h.state, &h.owner, &h.chat, shared.auth_revision, &reader, Some(Level::View))
                .await
                .unwrap();
        }
        let (response, ()) = tokio::join!(h.member_stream(&h.chat), async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            if through_folder {
                let node = grants::node_access(&h.state, &h.owner, shared.node_id).await.unwrap();
                grants::set_node_grant(&h.state, &h.owner, shared.node_id, node.revision, &reader, None)
                    .await
                    .unwrap();
            } else {
                let now = resources::get(&h.state, &h.owner, &h.chat).await.unwrap();
                grants::set_resource_grant(&h.state, &h.owner, &h.chat, now.auth_revision, &reader, None)
                    .await
                    .unwrap();
            }
        });
        let body = response.text();
        let parsed = frames(&body);
        let seen = kinds(&parsed);
        assert_eq!(seen.first(), Some(&"sync"), "the reader was live first: {body}");
        assert_eq!(seen.last(), Some(&"revoked"), "grant removal did not stop the stream: {body}");
    }
}
