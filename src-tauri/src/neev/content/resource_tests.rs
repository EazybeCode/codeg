use super::{
    resources,
    test_support::{patch, request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

// Every operation waits on SQL after admission; expiry must prevent both disclosure
// and publication, including successful idempotent replays.
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_resource_and_message_post_admission_expiry() {
    use super::messages;
    use std::sync::{atomic::Ordering, Arc};
    for operation in [
        "create",
        "get",
        "list",
        "update",
        "resource_receipt",
        "send",
        "message_list",
        "message_receipt",
    ] {
        let (database, admin, schema) = h::postgres_database().await;
        let mut harness = Harness::new(database.clone()).await;
        let clock = Arc::new(h::TestClock::new(1_700_000_000));
        Arc::get_mut(&mut harness.state).unwrap().clock = clock.clone();
        let input = harness.input(ResourceKind::Chat, "Private title");
        let chat = resources::create(&harness.state, &harness.creator, input.clone())
            .await
            .unwrap();
        let message_request = request();
        messages::append(
            &harness.state,
            &harness.creator,
            &chat.id,
            &message_request,
            1,
            "private body",
        )
        .await
        .unwrap();
        db::execute(
            &database,
            "UPDATE neev_sessions SET expires_at=1700000001 WHERE id=$1",
            vec![harness.creator.identity.session_id.clone().into()],
        )
        .await
        .unwrap();
        let guard = db::begin(&database).await.unwrap();
        let (table, waiting) = match operation {
            "create" => ("neev_requests", "INSERT INTO neev_requests%"),
            "resource_receipt" => ("neev_requests", "SELECT body_hash%"),
            "message_list" => ("neev_messages", "SELECT m.id%"),
            "message_receipt" => ("neev_requests", "SELECT body_hash%"),
            _ => ("neev_resources", "%neev_resources%"),
        };
        let mode = if operation == "create" {
            "SHARE"
        } else {
            "ACCESS EXCLUSIVE"
        };
        db::execute(
            &guard,
            &format!("LOCK TABLE {table} IN {mode} MODE"),
            vec![],
        )
        .await
        .unwrap();
        let state = harness.state.clone();
        let actor = harness.creator.clone();
        let node = harness.node;
        let pending = tokio::spawn(async move {
            match operation {
                "create" => {
                    let mut input = input;
                    input.request_id = request();
                    resources::create(&state, &actor, input).await.map(|_| ())
                }
                "get" => resources::get(&state, &actor, &chat.id).await.map(|_| ()),
                "list" => resources::list(&state, &actor, node, ResourceKind::Chat, false, None)
                    .await
                    .map(|_| ()),
                "update" => {
                    let mut patch = patch(&chat);
                    patch.title = "Changed".into();
                    resources::update(&state, &actor, &chat.id, patch)
                        .await
                        .map(|_| ())
                }
                "resource_receipt" => resources::create(&state, &actor, input).await.map(|_| ()),
                "send" => messages::append(&state, &actor, &chat.id, &request(), 1, "new body")
                    .await
                    .map(|_| ()),
                "message_list" => messages::list(&state, &actor, &chat.id, None)
                    .await
                    .map(|_| ()),
                _ => messages::append(
                    &state,
                    &actor,
                    &chat.id,
                    &message_request,
                    1,
                    "private body",
                )
                .await
                .map(|_| ()),
            }
        });
        h::wait_for_postgres_lock(&admin, waiting).await;
        clock.0.store(1_700_000_001, Ordering::SeqCst);
        guard.commit().await.unwrap();
        let result = pending.await.unwrap();
        let row = db::one(&database, "SELECT COUNT(*) AS n,MIN(title) AS title,MAX(content_revision) AS head FROM neev_resources", vec![]).await.unwrap().unwrap();
        let counts = (
            row.try_get::<i64>("", "n").unwrap(),
            row.try_get::<String>("", "title").unwrap(),
            row.try_get::<i64>("", "head").unwrap(),
        );
        let usage = (
            harness.usage("chats").await,
            harness.usage("messages").await,
        );
        drop(harness);
        h::cleanup_postgres(database, admin, schema).await;
        assert_eq!(
            result,
            Err(ContentError::Auth(
                crate::neev::error::AuthError::Unauthorized
            )),
            "{operation}"
        );
        assert_eq!(counts, (1, "Private title".into(), 1), "{operation}");
        assert_eq!(usage, (1, 1), "{operation}");
    }
}

async fn authority(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let input = h.input(ResourceKind::Chat, "Private title");
    let chat = resources::create(&h.state, &h.creator, input.clone())
        .await
        .unwrap();
    assert_eq!(chat.visibility, Visibility::Private);
    assert_eq!(
        uuid::Uuid::parse_str(&chat.id).unwrap().to_string(),
        chat.id
    );
    assert_eq!(
        resources::create(&h.state, &h.creator, input.clone())
            .await
            .unwrap()
            .id,
        chat.id
    );
    let mut changed = input.clone();
    changed.title = "Different".into();
    assert_eq!(
        resources::create(&h.state, &h.creator, changed).await,
        Err(ContentError::Conflict)
    );
    let mut changed = input.clone();
    changed.node_id = h.second_node;
    assert_eq!(
        resources::create(&h.state, &h.creator, changed).await,
        Err(ContentError::Conflict)
    );
    let mut changed = input.clone();
    changed.kind = ResourceKind::Document;
    assert_eq!(
        resources::create(&h.state, &h.creator, changed).await,
        Err(ContentError::Conflict)
    );
    let mut foreign_input = h.input(ResourceKind::Chat, "Foreign placement");
    foreign_input.node_id = h.foreign_node;
    assert_eq!(
        resources::create(&h.state, &h.creator, foreign_input).await,
        Err(ContentError::NotFound)
    );
    for actor in [&h.admin, &h.foreign] {
        assert_eq!(
            resources::get(&h.state, actor, &chat.id).await,
            Err(ContentError::NotFound)
        );
        assert_eq!(
            resources::update(&h.state, actor, &chat.id, patch(&chat)).await,
            Err(ContentError::NotFound)
        );
    }
    for id in ["invalid", "10000000-0000-4000-8000-000000000099"] {
        assert_eq!(
            resources::get(&h.state, &h.creator, id).await,
            Err(ContentError::NotFound)
        );
    }
    assert!(
        resources::list(&h.state, &h.admin, h.node, ResourceKind::Chat, false, None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let hidden = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&(h.creator.org_id, h.node, "chat", false, &chat.id)).unwrap());
    assert!(matches!(
        resources::list(
            &h.state,
            &h.admin,
            h.node,
            ResourceKind::Chat,
            false,
            Some(&hidden)
        )
        .await,
        Err(ContentError::NotFound)
    ));
    let mut p = patch(&chat);
    p.visibility = Visibility::Organization;
    let shared = resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert_eq!(shared.auth_revision, 2);
    assert_eq!(
        resources::get(&h.state, &h.admin, &chat.id)
            .await
            .unwrap()
            .title,
        "Private title"
    );
    let mut p = patch(&shared);
    p.archived = true;
    assert_eq!(
        resources::update(&h.state, &h.admin, &chat.id, p).await,
        Err(ContentError::NotFound)
    );
    let mut p = patch(&shared);
    p.node_id = h.foreign_node;
    assert_eq!(
        resources::update(&h.state, &h.creator, &chat.id, p).await,
        Err(ContentError::NotFound)
    );
    let mut p = patch(&shared);
    p.node_id = h.second_node;
    p.title = "Renamed".into();
    assert_eq!(
        resources::update(&h.state, &h.admin, &chat.id, p.clone()).await,
        Err(ContentError::NotFound)
    );
    let moved = resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert_eq!((moved.node_id, moved.auth_revision), (h.second_node, 3));
    assert_eq!(
        resources::update(&h.state, &h.creator, &chat.id, patch(&shared)).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(h.usage("chats").await, 1);
    crate::neev::org::remove(&h.state, &h.admin, h.creator.member_id)
        .await
        .unwrap();
    assert!(matches!(
        resources::create(&h.state, &h.creator, input).await,
        Err(ContentError::Auth(_))
    ));
}

async fn attachments(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let mut input = h.input(ResourceKind::Document, "Attached");
    input.origin_chat_id = Some(chat.id.clone());
    let doc = resources::create(&h.state, &h.admin, input.clone())
        .await
        .unwrap();
    let mut detached_request = input.clone();
    detached_request.origin_chat_id = None;
    assert_eq!(
        resources::create(&h.state, &h.admin, detached_request).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(
        resources::get(&h.state, &h.creator, &doc.id)
            .await
            .unwrap()
            .visibility,
        Visibility::Organization
    );
    let mut p = patch(&doc);
    p.visibility = Visibility::Private;
    assert_eq!(
        resources::update(&h.state, &h.admin, &doc.id, p).await,
        Err(ContentError::Invalid)
    );
    let mut p = patch(&doc);
    p.node_id = h.second_node;
    assert_eq!(
        resources::update(&h.state, &h.creator, &doc.id, p).await,
        Err(ContentError::NotFound)
    );
    db::execute(&h.state.database,"INSERT INTO neev_content_leases(org_id,resource_id,id,actor_id,request_id,body_hash,blob_key,reserved_bytes,expected_content_revision,expected_auth_revision,expires_at) VALUES($1,$2,$3,$4,$5,'hash','key',0,0,1,1700000060)",vec![h.creator.org_id.into(),doc.id.clone().into(),request().into(),h.admin.identity.user_id.into(),request().into()]).await.unwrap();
    let mut p = patch(&chat);
    p.visibility = Visibility::Private;
    assert_eq!(
        resources::update(&h.state, &h.creator, &chat.id, p.clone()).await,
        Err(ContentError::Conflict)
    );
    db::execute(
        &h.state.database,
        "UPDATE neev_content_leases SET expires_at=1700000000",
        vec![],
    )
    .await
    .unwrap();
    let private = resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert_eq!(
        resources::create(&h.state, &h.admin, input).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        resources::get(&h.state, &h.admin, &doc.id).await,
        Err(ContentError::NotFound)
    );
    let attached = resources::get(&h.state, &h.creator, &doc.id).await.unwrap();
    assert_eq!(
        (attached.auth_revision, attached.visibility),
        (2, Visibility::Private)
    );
    assert!(resources::list(
        &h.state,
        &h.admin,
        h.node,
        ResourceKind::Document,
        false,
        None
    )
    .await
    .unwrap()
    .items
    .is_empty());
    let mut p = patch(&private);
    p.archived = true;
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    let tx = db::begin(&h.state.database).await.unwrap();
    db::lock_admission_guard(&tx).await.unwrap();
    crate::neev::auth::session::recheck(&tx, &h.creator, 1700000000)
        .await
        .unwrap();
    assert_eq!(
        super::access::authorized(&tx, &h.creator, &doc.id, true).await,
        Err(ContentError::Archived)
    );
    tx.rollback().await.unwrap();
}

async fn pagination_and_validation(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    for title in ["".to_string(), "x".repeat(101), "bad\nname".into()] {
        assert_eq!(
            resources::create(&h.state, &h.creator, h.input(ResourceKind::Chat, &title)).await,
            Err(ContentError::Invalid)
        );
    }
    let unicode = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Chat, &"é".repeat(100)),
    )
    .await
    .unwrap();
    assert_eq!(unicode.title.chars().count(), 100);
    for n in 0..103 {
        let mut input = h.input(ResourceKind::Chat, &format!("Visible {n}"));
        input.request_id = request();
        let r = resources::create(&h.state, &h.admin, input).await.unwrap();
        if n < 101 {
            let mut p = patch(&r);
            p.visibility = Visibility::Organization;
            resources::update(&h.state, &h.admin, &r.id, p)
                .await
                .unwrap();
        }
    }
    let page = resources::list(
        &h.state,
        &h.creator,
        h.node,
        ResourceKind::Chat,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 100);
    let cursor = page.next_after.unwrap();
    let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(&cursor).unwrap()).unwrap();
    assert!(!decoded.contains("Visible"));
    let end = resources::list(
        &h.state,
        &h.creator,
        h.node,
        ResourceKind::Chat,
        false,
        Some(&cursor),
    )
    .await
    .unwrap();
    assert_eq!(end.items.len(), 2);
    assert!(end.next_after.is_none());
    for (actor, node, kind, archived, c) in [
        (
            &h.foreign,
            h.foreign_node,
            ResourceKind::Chat,
            false,
            cursor.as_str(),
        ),
        (
            &h.creator,
            h.second_node,
            ResourceKind::Chat,
            false,
            cursor.as_str(),
        ),
        (
            &h.creator,
            h.node,
            ResourceKind::Document,
            false,
            cursor.as_str(),
        ),
        (
            &h.creator,
            h.node,
            ResourceKind::Chat,
            true,
            cursor.as_str(),
        ),
        (&h.creator, h.node, ResourceKind::Chat, false, "bad"),
    ] {
        assert!(matches!(
            resources::list(&h.state, actor, node, kind, archived, Some(c)).await,
            Err(ContentError::NotFound)
        ));
    }
    db::execute(
        &h.state.database,
        "UPDATE neev_content_usage SET chats=9999,documents=10000 WHERE org_id=$1",
        vec![h.creator.org_id.into()],
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        resources::create(&h.state, &h.creator, h.input(ResourceKind::Chat, "Last A")),
        resources::create(&h.state, &h.admin, h.input(ResourceKind::Chat, "Last B"))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(a == Err(ContentError::Quota) || b == Err(ContentError::Quota));
    assert_eq!(
        resources::create(
            &h.state,
            &h.creator,
            h.input(ResourceKind::Document, "Full")
        )
        .await,
        Err(ContentError::Quota)
    );
}

#[tokio::test]
async fn sqlite_resource_authority() {
    authority(h::database().await).await;
}
#[tokio::test]
async fn sqlite_resource_attachments() {
    attachments(h::database().await).await;
}
#[tokio::test]
async fn sqlite_resource_pagination_and_quotas() {
    pagination_and_validation(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_resource_contracts() {
    let (db, admin, schema) = h::postgres_database().await;
    authority(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    attachments(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    pagination_and_validation(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
