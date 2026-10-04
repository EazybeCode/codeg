//! The trash routes over HTTP: session + CSRF, the stable 409 refusal bodies
//! (`trash_blocked` with its reason, `parent_trashed`, `conflict`), private
//! headers on every answer, and another tenant's 404.

use axum::http::{header, StatusCode};
use serde_json::{json, Value};

use crate::neev::{
    auth::session,
    catalog::nodes,
    content::{
        resources,
        types::{ResourceInput, ResourceKind, Visibility},
    },
    db,
    test_support::OrgHarness,
    types::{NodeInput, NodeKind, OrgContext},
};

use super::super::cookies::CookieNames;

const NOW: i64 = 1_700_000_000;

struct Client {
    cookies: String,
    csrf: String,
}

struct Harness {
    org: OrgHarness,
    server: axum_test::TestServer,
    owner: Client,
    foreign: Client,
}

async fn client(org: &OrgHarness, actor: &OrgContext) -> Client {
    let issued = session::issue_identity(&org.database, actor.identity.user_id, NOW)
        .await
        .unwrap();
    let identity = session::identity(&org.database, &issued.session, NOW)
        .await
        .unwrap();
    let scoped = session::select_organization(&org.database, &identity, actor.org_id, NOW)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    Client {
        cookies: format!(
            "{}={}; {}={}",
            names.session, scoped.session, names.csrf, scoped.csrf
        ),
        csrf: scoped.csrf,
    }
}

impl Harness {
    async fn new() -> Self {
        let org = OrgHarness::new().await;
        let owner = client(&org, org.owner_context()).await;
        let foreign = client(&org, org.other_org_context()).await;
        let server = axum_test::TestServer::new(super::super::router(org.state.clone())).unwrap();
        Self {
            org,
            server,
            owner,
            foreign,
        }
    }

    async fn node(&self, parent: Option<i32>, name: &str) -> (i32, String) {
        let node = nodes::create(
            &self.org.state,
            self.org.owner_context(),
            NodeInput {
                parent_id: parent,
                kind: if parent.is_some() {
                    NodeKind::Folder
                } else {
                    NodeKind::Project
                },
                name: name.into(),
            },
        )
        .await
        .unwrap();
        (node.id, node.revision.to_string())
    }

    async fn get(&self, who: &Client, path: &str) -> axum_test::TestResponse {
        self.server
            .get(path)
            .add_header(header::COOKIE, &who.cookies)
            .await
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> axum_test::TestResponse {
        self.server
            .post(path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &who.cookies)
            .add_header("x-neev-csrf", &who.csrf)
            .json(&body)
            .await
    }

    async fn trash(&self, who: &Client, id: i32, revision: &str) -> axum_test::TestResponse {
        self.post(
            who,
            &format!("/api/neev/nodes/{id}/trash"),
            json!({"expectedRevision": revision}),
        )
        .await
    }

    async fn revision(&self, id: i32) -> String {
        let row = db::one(
            &self.org.database,
            "SELECT revision FROM neev_nodes WHERE id=$1",
            vec![id.into()],
        )
        .await
        .unwrap()
        .unwrap();
        row.try_get::<i64>("", "revision").unwrap().to_string()
    }
}

fn private(response: &axum_test::TestResponse) {
    assert_eq!(response.header(header::CACHE_CONTROL), "no-store");
}

#[tokio::test]
async fn preview_trash_list_and_restore_round_trip() {
    let h = Harness::new().await;
    let (project, _) = h.node(None, "Project").await;
    let (folder, revision) = h.node(Some(project), "Folder").await;

    let preview = h
        .get(&h.owner, &format!("/api/neev/nodes/{folder}/trash-preview"))
        .await;
    preview.assert_status_ok();
    private(&preview);
    preview.assert_json(&json!({
        "folders": 0, "documents": 0, "chats": 0, "agentChats": 0,
        "blocked": {"agentActive": false, "inUse": false, "privateOthers": false}
    }));

    let trashed = h.trash(&h.owner, folder, &revision).await;
    trashed.assert_status_ok();
    private(&trashed);
    let body: Value = trashed.json();
    assert_eq!(
        (body["nodes"].as_i64(), body["resources"].as_i64()),
        (Some(1), Some(0))
    );

    let list = h.get(&h.owner, "/api/neev/trash/nodes").await;
    list.assert_status_ok();
    let items: Value = list.json();
    assert_eq!(items["items"][0]["nodeId"], json!(folder));
    assert_eq!(items["items"][0]["parentName"], json!("Project"));
    assert_eq!(items["items"][0]["parentTrashed"], json!(false));

    let restored = h
        .post(
            &h.owner,
            &format!("/api/neev/nodes/{folder}/restore"),
            json!({}),
        )
        .await;
    restored.assert_status_ok();
    private(&restored);
    let nodes: Value = h.get(&h.owner, "/api/neev/nodes").await.json();
    assert!(nodes["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == json!(folder)));
}

#[tokio::test]
async fn a_running_agent_is_a_409_trash_blocked_with_its_reason() {
    let h = Harness::new().await;
    let (project, revision) = h.node(None, "Project").await;
    let actor = h.org.owner_context();
    let session = uuid::Uuid::new_v4().to_string();
    db::execute(
        &h.org.database,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'S','claude','default','private','chat',NULL,NULL,$4,false,1,0,0)",
        vec![actor.org_id.into(), session.clone().into(), actor.identity.user_id.into(), project.into()],
    )
    .await
    .unwrap();
    db::execute(
        &h.org.database,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,1,$3,$4,NULL,'p','','running',NULL,'claude','default',0,0,NULL)",
        vec![
            actor.org_id.into(),
            session.into(),
            uuid::Uuid::new_v4().to_string().into(),
            uuid::Uuid::new_v4().to_string().into(),
        ],
    )
    .await
    .unwrap();
    let refused = h.trash(&h.owner, project, &revision).await;
    refused.assert_status(StatusCode::CONFLICT);
    private(&refused);
    refused.assert_json(&json!({"code": "trash_blocked", "reason": "agent_active"}));
    assert_eq!(
        h.revision(project).await,
        revision,
        "a refusal writes nothing"
    );
}

#[tokio::test]
async fn another_members_private_item_is_a_409_with_only_a_count() {
    let h = Harness::new().await;
    let (project, revision) = h.node(None, "Project").await;
    resources::create_with_visibility(
        &h.org.state,
        h.org.member_context(),
        ResourceInput {
            node_id: project,
            kind: ResourceKind::Document,
            title: "Secret plan".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
        Some(Visibility::Private),
    )
    .await
    .unwrap();
    let refused = h.trash(&h.owner, project, &revision).await;
    refused.assert_status(StatusCode::CONFLICT);
    refused.assert_json(&json!({"code": "trash_blocked", "reason": "private_content"}));
    // The preview says only that something private is there, never how much.
    let preview: Value = h
        .get(
            &h.owner,
            &format!("/api/neev/nodes/{project}/trash-preview"),
        )
        .await
        .json();
    assert_eq!(preview["blocked"]["privateOthers"], json!(true));
    assert_eq!(preview["documents"], json!(0));
    assert!(!refused.text().contains("Secret plan"));
}

#[tokio::test]
async fn parent_trashed_and_stale_revision_are_distinct_409s() {
    let h = Harness::new().await;
    let (project, _) = h.node(None, "Project").await;
    let (folder, folder_revision) = h.node(Some(project), "Folder").await;
    h.trash(&h.owner, folder, &folder_revision)
        .await
        .assert_status_ok();
    let project_revision = h.revision(project).await;
    h.trash(&h.owner, project, &project_revision)
        .await
        .assert_status_ok();

    let refused = h
        .post(
            &h.owner,
            &format!("/api/neev/nodes/{folder}/restore"),
            json!({}),
        )
        .await;
    refused.assert_status(StatusCode::CONFLICT);
    private(&refused);
    refused.assert_json(&json!({"code": "parent_trashed"}));

    h.post(
        &h.owner,
        &format!("/api/neev/nodes/{project}/restore"),
        json!({}),
    )
    .await
    .assert_status_ok();
    let (other, revision) = h.node(None, "Other").await;
    let next = (revision.parse::<i64>().unwrap() + 1).to_string();
    let stale = h.trash(&h.owner, other, &next).await;
    stale.assert_status(StatusCode::CONFLICT);
    stale.assert_json(&json!({"code": "conflict"}));
}

#[tokio::test]
async fn another_tenant_gets_404_and_an_empty_trash() {
    let h = Harness::new().await;
    let (project, revision) = h.node(None, "Project").await;
    let preview = h
        .get(
            &h.foreign,
            &format!("/api/neev/nodes/{project}/trash-preview"),
        )
        .await;
    preview.assert_status_not_found();
    private(&preview);
    h.trash(&h.foreign, project, &revision)
        .await
        .assert_status_not_found();
    h.trash(&h.owner, project, &revision)
        .await
        .assert_status_ok();
    h.post(
        &h.foreign,
        &format!("/api/neev/nodes/{project}/restore"),
        json!({}),
    )
    .await
    .assert_status_not_found();
    h.get(&h.foreign, "/api/neev/trash/nodes")
        .await
        .assert_json(&json!({"items": []}));
}

#[tokio::test]
async fn mutations_need_a_session_and_csrf() {
    let h = Harness::new().await;
    let (project, revision) = h.node(None, "Project").await;
    let anonymous = h
        .server
        .get(&format!("/api/neev/nodes/{project}/trash-preview"))
        .expect_failure()
        .await;
    assert_eq!(anonymous.status_code(), StatusCode::UNAUTHORIZED);
    let no_csrf = h
        .server
        .post(&format!("/api/neev/nodes/{project}/trash"))
        .add_header(header::ORIGIN, "http://127.0.0.1:3100")
        .add_header(header::COOKIE, &h.owner.cookies)
        .json(&json!({"expectedRevision": revision}))
        .expect_failure()
        .await;
    assert!(matches!(
        no_csrf.status_code(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ));
    // Nothing was trashed.
    assert!(h
        .get(&h.owner, "/api/neev/trash/nodes")
        .await
        .json::<Value>()["items"]
        .as_array()
        .unwrap()
        .is_empty());
}
