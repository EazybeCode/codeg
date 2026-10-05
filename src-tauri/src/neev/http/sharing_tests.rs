//! The sharing endpoints' wire contract (`.context/sharing-api-contract.md`):
//! shapes, methods, status codes and coded refusals.

use axum::http::{header, Method, StatusCode};
use serde_json::{json, Value};

use crate::neev::{
    auth::session,
    catalog::nodes,
    content::{
        resources,
        types::{ResourceInput, ResourceKind},
    },
    test_support as h,
    types::{NodeInput, NodeKind, OrgContext},
};

use super::cookies::CookieNames;

struct Client {
    server: axum_test::TestServer,
    cookies: String,
    csrf: String,
}

impl Client {
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> axum_test::TestResponse {
        let request = self
            .server
            .method(method, path)
            .add_header(header::ORIGIN, "http://127.0.0.1:3100")
            .add_header(header::COOKIE, &self.cookies)
            .add_header("x-neev-csrf", &self.csrf);
        match body {
            Some(body) => request.json(&body).await,
            None => request.await,
        }
    }
    async fn get(&self, path: &str) -> axum_test::TestResponse {
        self.send(Method::GET, path, None).await
    }
}

async fn client(org: &h::OrgHarness, actor: &OrgContext) -> Client {
    let identity = session::issue_identity(&org.database, actor.identity.user_id, 1_700_000_000)
        .await
        .unwrap();
    let identity = session::identity(&org.database, &identity.session, 1_700_000_000)
        .await
        .unwrap();
    let scoped = session::select_organization(&org.database, &identity, actor.org_id, 1_700_000_000)
        .await
        .unwrap();
    let names = CookieNames::for_preview(true);
    Client {
        server: axum_test::TestServer::new(super::router(org.state.clone())).unwrap(),
        cookies: format!("{}={}; {}={}", names.session, scoped.session, names.csrf, scoped.csrf),
        csrf: scoped.csrf,
    }
}

#[tokio::test]
async fn directory_and_teams_speak_the_contract() {
    let org = h::OrgHarness::new().await;
    let owner = client(&org, org.owner_context()).await;
    let member = client(&org, org.member_context()).await;

    let directory = member.get("/api/neev/organization/directory?limit=1").await;
    directory.assert_status_ok();
    assert_eq!(directory.header(header::CACHE_CONTROL), "no-store");
    let body: Value = directory.json();
    let row = body["members"][0].as_object().unwrap();
    let mut keys: Vec<_> = row.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["avatarUrl", "displayName", "role", "userId"], "never an email");
    let after = body["nextAfter"].as_str().unwrap().to_owned();
    let next: Value = member.get(&format!("/api/neev/organization/directory?limit=1&after={after}")).await.json();
    assert_ne!(next["members"][0]["userId"], row["userId"]);
    member.get("/api/neev/organization/directory?limit=0").await.assert_status(StatusCode::BAD_REQUEST);
    member.get("/api/neev/organization/directory?email=x").await.assert_status(StatusCode::BAD_REQUEST);

    let request = uuid::Uuid::new_v4().to_string();
    member
        .send(Method::POST, "/api/neev/teams", Some(json!({"name": "Design", "requestId": request})))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let created = owner.send(Method::POST, "/api/neev/teams", Some(json!({"name": "Design", "requestId": request}))).await;
    created.assert_status(StatusCode::CREATED);
    let team: Value = created.json();
    assert_eq!(team, json!({"id": team["id"], "name": "Design", "memberCount": 0, "revision": "1", "canManage": true}));
    let id = team["id"].as_str().unwrap().to_owned();
    let taken = owner
        .send(Method::POST, "/api/neev/teams", Some(json!({"name": "design", "requestId": uuid::Uuid::new_v4().to_string()})))
        .await;
    taken.assert_status(StatusCode::CONFLICT);
    assert_eq!(taken.json::<Value>(), json!({"code": "team_name_taken"}));
    let listed: Value = member.get("/api/neev/teams").await.json();
    assert_eq!(listed["teams"][0]["canManage"], false);

    let user = org.member_context().identity.user_id;
    let added = owner
        .send(Method::POST, &format!("/api/neev/teams/{id}/members"), Some(json!({"userId": user, "expectedRevision": "1"})))
        .await;
    added.assert_status_ok();
    let added: Value = added.json();
    assert_eq!(added["revision"], "2");
    assert_eq!(added["members"][0]["userId"], user);
    let stale = owner.send(Method::PATCH, &format!("/api/neev/teams/{id}"), Some(json!({"name": "X", "expectedRevision": "1"}))).await;
    stale.assert_status(StatusCode::CONFLICT);
    let removed: Value = owner
        .send(Method::DELETE, &format!("/api/neev/teams/{id}/members/{user}"), Some(json!({"expectedRevision": "2"})))
        .await
        .json();
    assert_eq!(removed["members"], json!([]));
    let deleted = owner.send(Method::DELETE, &format!("/api/neev/teams/{id}"), Some(json!({"expectedRevision": "3"}))).await;
    deleted.assert_status_ok();
    assert_eq!(deleted.json::<Value>(), json!({"ok": true}));
    member.get(&format!("/api/neev/teams/{id}/members")).await.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resource_node_and_session_sharing_speak_the_contract() {
    let org = h::OrgHarness::new().await;
    let creator = org.owner_context().clone();
    let reader = org.member_context().clone();
    let owner = client(&org, &creator).await;
    let member = client(&org, &reader).await;
    let other = client(&org, org.other_org_context()).await;
    let node = nodes::create(&org.state, &creator, NodeInput { parent_id: None, kind: NodeKind::Project, name: "P".into() })
        .await
        .unwrap()
        .id;
    // Created shared over the wire.
    let created = owner
        .send(
            Method::POST,
            &format!("/api/neev/nodes/{node}/chats"),
            Some(json!({"title": "Plan", "requestId": uuid::Uuid::new_v4().to_string(), "visibility": "shared"})),
        )
        .await;
    created.assert_status(StatusCode::CREATED);
    let chat: Value = created.json();
    assert_eq!(chat["visibility"], "shared");
    let id = chat["id"].as_str().unwrap().to_owned();
    member.get(&format!("/api/neev/resources/{id}")).await.assert_status(StatusCode::NOT_FOUND);
    member.get(&format!("/api/neev/resources/{id}/access")).await.assert_status(StatusCode::NOT_FOUND);

    let grant = json!({
        "expectedAuthRevision": "1",
        "principal": {"kind": "user", "userId": reader.identity.user_id},
        "level": "view",
    });
    member.send(Method::PUT, &format!("/api/neev/resources/{id}/grants"), Some(grant.clone())).await.assert_status(StatusCode::NOT_FOUND);
    let granted = owner.send(Method::PUT, &format!("/api/neev/resources/{id}/grants"), Some(grant)).await;
    granted.assert_status_ok();
    let access: Value = granted.json();
    assert_eq!(access["authRevision"], "2");
    assert_eq!(access["level"], "owner");
    assert_eq!(access["direct"][0]["principal"]["kind"], "user");
    assert_eq!(access["direct"][0]["level"], "view");
    let seen: Value = member.get(&format!("/api/neev/resources/{id}/access")).await.json();
    assert_eq!((seen["level"].as_str(), seen["canManage"].as_bool()), (Some("view"), Some(false)));
    member.get(&format!("/api/neev/resources/{id}")).await.assert_status_ok();
    // A viewer posting is refused as a non-reader would be.
    member
        .send(Method::POST, &format!("/api/neev/chats/{id}/messages"), Some(json!({"requestId": uuid::Uuid::new_v4().to_string(), "expectedAuthRevision": "2", "body": "hi"})))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    other.get(&format!("/api/neev/resources/{id}/access")).await.assert_status(StatusCode::NOT_FOUND);
    // Bad bodies.
    for body in [
        json!({"expectedAuthRevision": "2", "principal": {"kind": "user", "userId": 0}, "level": "view"}),
        json!({"expectedAuthRevision": "2", "principal": {"kind": "team", "teamId": "x"}, "level": "view"}),
        json!({"expectedAuthRevision": "2", "principal": {"kind": "user", "userId": 1}, "level": "owner"}),
        json!({"expectedAuthRevision": "2", "principal": {"kind": "user", "userId": 1, "email": "x"}, "level": "view"}),
    ] {
        owner.send(Method::PUT, &format!("/api/neev/resources/{id}/grants"), Some(body)).await.assert_status(StatusCode::BAD_REQUEST);
    }
    let revoked: Value = owner
        .send(Method::DELETE, &format!("/api/neev/resources/{id}/grants"), Some(json!({"expectedAuthRevision": "2", "principal": {"kind": "user", "userId": reader.identity.user_id}})))
        .await
        .json();
    assert_eq!(revoked["direct"], json!([]));
    member.get(&format!("/api/neev/resources/{id}")).await.assert_status(StatusCode::NOT_FOUND);

    // A private item refuses grants with its own code.
    let private = resources::create(
        &org.state,
        &creator,
        ResourceInput {
            node_id: node,
            kind: ResourceKind::Chat,
            title: "Private".into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    let refused = owner
        .send(Method::PUT, &format!("/api/neev/resources/{}/grants", private.id), Some(json!({"expectedAuthRevision": "1", "principal": {"kind": "user", "userId": reader.identity.user_id}, "level": "edit"})))
        .await;
    refused.assert_status(StatusCode::CONFLICT);
    assert_eq!(refused.json::<Value>(), json!({"code": "resource_not_shared"}));

    // Folder sharing.
    let node_access: Value = member.get(&format!("/api/neev/nodes/{node}/access")).await.json();
    assert_eq!(node_access["canManage"], false);
    let revision = node_access["revision"].as_str().unwrap().to_owned();
    member
        .send(Method::PUT, &format!("/api/neev/nodes/{node}/grants"), Some(json!({"expectedRevision": revision, "principal": {"kind": "user", "userId": reader.identity.user_id}, "level": "edit"})))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let node_granted: Value = owner
        .send(Method::PUT, &format!("/api/neev/nodes/{node}/grants"), Some(json!({"expectedRevision": revision, "principal": {"kind": "user", "userId": reader.identity.user_id}, "level": "edit"})))
        .await
        .json();
    assert_eq!(node_granted["direct"][0]["level"], "edit");
    // Folder access now reaches the shared chat, but not the private one.
    member.get(&format!("/api/neev/resources/{id}")).await.assert_status_ok();
    member.get(&format!("/api/neev/resources/{}", private.id)).await.assert_status(StatusCode::NOT_FOUND);
    let default: Value = owner
        .send(Method::PATCH, &format!("/api/neev/nodes/{node}/sharing"), Some(json!({"expectedRevision": node_granted["revision"], "defaultVisibility": "shared"})))
        .await
        .json();
    assert_eq!((default["defaultVisibility"].as_str(), default["effectiveVisibility"].as_str()), (Some("shared"), Some("shared")));

    // Agent Chat sharing: view only.
    let session = uuid::Uuid::new_v4().to_string();
    crate::neev::db::execute(&org.database, "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,effort,fast,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Research','claude','default',NULL,false,'private','chat',NULL,NULL,NULL,false,1,1,1)", vec![creator.org_id.into(), session.clone().into(), creator.identity.user_id.into()]).await.unwrap();
    let principal = json!({"kind": "user", "userId": reader.identity.user_id});
    owner
        .send(Method::PUT, &format!("/api/neev/agent/sessions/{session}/grants"), Some(json!({"expectedRevision": "1", "principal": principal, "level": "edit"})))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    member.get(&format!("/api/neev/agent/sessions/{session}")).await.assert_status(StatusCode::NOT_FOUND);
    let shared: Value = owner
        .send(Method::PUT, &format!("/api/neev/agent/sessions/{session}/grants"), Some(json!({"expectedRevision": "1", "principal": principal, "level": "view"})))
        .await
        .json();
    assert_eq!((shared["revision"].as_str(), shared["level"].as_str()), (Some("2"), Some("owner")));
    member.get(&format!("/api/neev/agent/sessions/{session}")).await.assert_status_ok();
    member.get(&format!("/api/neev/agent/sessions/{session}/turns")).await.assert_status_ok();
    let list: Value = member.get("/api/neev/agent/shared-sessions").await.json();
    assert_eq!(list["sessions"][0]["session"]["id"], session);
    assert_eq!(list["sessions"][0]["owner"]["userId"], creator.identity.user_id);
    let seen: Value = member.get(&format!("/api/neev/agent/sessions/{session}/access")).await.json();
    assert_eq!((seen["level"].as_str(), seen["canManage"].as_bool()), (Some("view"), Some(false)));
    // Owner-only paths stay closed to a viewer.
    member
        .send(Method::POST, &format!("/api/neev/agent/sessions/{session}/turns"), Some(json!({"prompt": "go"})))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let gone = owner
        .send(Method::DELETE, &format!("/api/neev/agent/sessions/{session}/grants"), Some(json!({"expectedRevision": "2", "principal": principal})))
        .await;
    gone.assert_status_ok();
    member.get(&format!("/api/neev/agent/sessions/{session}")).await.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restricted_folders_speak_the_contract() {
    let org = h::OrgHarness::new().await;
    let creator = org.owner_context().clone();
    let owner = client(&org, &creator).await;
    let member = client(&org, org.member_context()).await;
    let project = nodes::create(&org.state, &creator, NodeInput { parent_id: None, kind: NodeKind::Project, name: "Secret plans".into() })
        .await
        .unwrap();
    let chat = resources::create(
        &org.state,
        &creator,
        ResourceInput { node_id: project.id, kind: ResourceKind::Chat, title: "Inside".into(), origin_chat_id: None, request_id: uuid::Uuid::new_v4().to_string() },
    )
    .await
    .unwrap();
    member
        .send(Method::PUT, &format!("/api/neev/nodes/{}/restriction", project.id), Some(json!({"expectedRevision": "1", "restricted": true})))
        .await
        .assert_status(StatusCode::FORBIDDEN);
    let restricted = owner
        .send(Method::PUT, &format!("/api/neev/nodes/{}/restriction", project.id), Some(json!({"expectedRevision": "1", "restricted": true})))
        .await;
    restricted.assert_status_ok();
    let body: Value = restricted.json();
    assert_eq!((body["restricted"].as_bool(), body["level"].as_str(), body["revision"].as_str()), (Some(true), Some("owner"), Some("2")));
    assert_eq!((body["canManage"].as_bool(), body["canRestrict"].as_bool()), (Some(true), Some(true)));
    // Hidden from the member: not in the tree, and every lookup is missing.
    let tree: Value = member.get("/api/neev/nodes").await.json();
    let listed = tree.to_string();
    assert!(!listed.contains("Secret plans"), "a hidden name leaked: {listed}");
    member.get(&format!("/api/neev/nodes/{}/access", project.id)).await.assert_status(StatusCode::NOT_FOUND);
    member.get(&format!("/api/neev/resources/{}", chat.id)).await.assert_status(StatusCode::NOT_FOUND);
    // Shared directly once made "shared": listed under the hidden-folder view.
    let current = resources::get(&org.state, &creator, &chat.id).await.unwrap();
    let mut patch = crate::neev::content::types::ResourcePatch {
        expected_auth_revision: current.auth_revision,
        title: current.title.clone(),
        node_id: current.node_id,
        visibility: crate::neev::content::types::Visibility::Shared,
        archived: false,
    };
    let current = resources::update(&org.state, &creator, &chat.id, patch.clone()).await.unwrap();
    patch.expected_auth_revision = current.auth_revision;
    owner
        .send(Method::PUT, &format!("/api/neev/resources/{}/grants", chat.id), Some(json!({"expectedAuthRevision": current.auth_revision.to_string(), "principal": {"kind": "user", "userId": org.member_context().identity.user_id}, "level": "view"})))
        .await
        .assert_status_ok();
    let shared: Value = member.get("/api/neev/shared/resources").await.json();
    assert_eq!(shared["resources"][0]["id"], chat.id.as_str());
    assert!(!shared.to_string().contains("Secret plans"));
    member.get(&format!("/api/neev/resources/{}", chat.id)).await.assert_status_ok();
    let access: Value = member.get(&format!("/api/neev/resources/{}/access", chat.id)).await.json();
    assert_eq!(access["inherited"], json!([]));
}
