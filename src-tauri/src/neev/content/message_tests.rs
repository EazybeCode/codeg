use super::{
    messages, resources,
    test_support::{patch, request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};

async fn conversations(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let id = request();
    let first = messages::append(&h.state, &h.creator, &chat.id, &id, 2, "Hello team")
        .await
        .unwrap();
    assert_eq!(
        (first.sequence, first.author_id, first.created_at),
        (1, h.creator.identity.user_id, 1700000000)
    );
    assert_eq!(
        messages::append(&h.state, &h.creator, &chat.id, &id, 2, "Hello team")
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        messages::append(&h.state, &h.creator, &chat.id, &id, 2, "Changed").await,
        Err(ContentError::Conflict)
    );
    let second_request = request();
    let second = messages::append(&h.state, &h.admin, &chat.id, &second_request, 2, "Reply")
        .await
        .unwrap();
    assert_eq!(
        (second.sequence, second.author_id),
        (2, h.admin.identity.user_id)
    );
    db::execute(
        &h.state.database,
        "UPDATE neev_accounts SET display_name='Current author' WHERE id=$1",
        vec![h.creator.identity.user_id.into()],
    )
    .await
    .unwrap();
    let page = messages::list(&h.state, &h.admin, &chat.id, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].author_name, "Current author");
    assert_eq!(
        messages::list(&h.state, &h.admin, &chat.id, Some(1))
            .await
            .unwrap()
            .items,
        vec![second]
    );
    assert!(matches!(
        messages::list(&h.state, &h.foreign, &chat.id, None).await,
        Err(ContentError::NotFound)
    ));
    assert_eq!(
        messages::append(&h.state, &h.foreign, &chat.id, &request(), 2, "Hidden").await,
        Err(ContentError::NotFound)
    );
    for body in ["".to_string(), "x".repeat(32769), "é".repeat(16385)] {
        assert_eq!(
            messages::append(&h.state, &h.creator, &chat.id, &request(), 2, &body).await,
            Err(ContentError::Invalid)
        );
    }
    assert_eq!(
        messages::append(&h.state, &h.creator, &chat.id, &request(), 1, "Stale").await,
        Err(ContentError::Conflict)
    );
    assert_eq!(
        (h.usage("messages").await, h.usage("charged_bytes").await),
        (2, 15)
    );
    assert_eq!(
        resources::get(&h.state, &h.creator, &chat.id)
            .await
            .unwrap()
            .content_revision,
        2
    );
    let mut p = patch(&chat);
    p.archived = true;
    let archived = resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert!(
        resources::get(&h.state, &h.admin, &chat.id)
            .await
            .unwrap()
            .archived
    );
    assert_eq!(
        messages::list(&h.state, &h.admin, &chat.id, None)
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    assert_eq!(
        messages::append(
            &h.state,
            &h.creator,
            &chat.id,
            &request(),
            archived.auth_revision,
            "No"
        )
        .await,
        Err(ContentError::Archived)
    );
    assert_eq!(
        messages::append(&h.state, &h.creator, &chat.id, &id, 2, "Hello team")
            .await
            .unwrap()
            .id,
        first.id
    );
    assert_eq!(h.usage("messages").await, 2);
    let mut p = patch(&archived);
    p.archived = false;
    p.visibility = Visibility::Private;
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert!(matches!(
        messages::list(&h.state, &h.admin, &chat.id, None).await,
        Err(ContentError::NotFound)
    ));
    assert_eq!(
        messages::append(&h.state, &h.admin, &chat.id, &second_request, 2, "Reply").await,
        Err(ContentError::NotFound)
    );
    crate::neev::org::remove(&h.state, &h.admin, h.creator.member_id)
        .await
        .unwrap();
    assert!(matches!(
        messages::append(&h.state, &h.creator, &chat.id, &id, 2, "Hello team").await,
        Err(ContentError::Auth(_))
    ));
}
async fn quotas(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    for (bytes, messages) in [(1073741824_i64, 0_i64), (0, 100000)] {
        db::execute(
            &h.state.database,
            "UPDATE neev_content_usage SET charged_bytes=$1,messages=$2 WHERE org_id=$3",
            vec![bytes.into(), messages.into(), h.creator.org_id.into()],
        )
        .await
        .unwrap();
        assert_eq!(
            messages::append(&h.state, &h.creator, &chat.id, &request(), 2, "x").await,
            Err(ContentError::Quota)
        );
    }
    db::execute(
        &h.state.database,
        "UPDATE neev_content_usage SET charged_bytes=1073741823,messages=99999 WHERE org_id=$1",
        vec![h.creator.org_id.into()],
    )
    .await
    .unwrap();
    let req_a = request();
    let req_b = request();
    let (a, b) = tokio::join!(
        messages::append(&h.state, &h.creator, &chat.id, &req_a, 2, "a"),
        messages::append(&h.state, &h.admin, &chat.id, &req_b, 2, "b")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(a == Err(ContentError::Quota) || b == Err(ContentError::Quota));
    assert_eq!(
        (h.usage("messages").await, h.usage("charged_bytes").await),
        (100000, 1073741824)
    );
    assert_eq!(
        messages::list(&h.state, &h.creator, &chat.id, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}
async fn pages_and_boundaries(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let max = "é".repeat(16384);
    messages::append(&h.state, &h.creator, &chat.id, &request(), 2, &max)
        .await
        .unwrap();
    for _ in 1..101 {
        messages::append(&h.state, &h.admin, &chat.id, &request(), 2, "a")
            .await
            .unwrap();
    }
    let page = messages::list(&h.state, &h.creator, &chat.id, None)
        .await
        .unwrap();
    assert_eq!(
        (page.items.len(), page.next_after.as_deref()),
        (100, Some("100"))
    );
    assert_eq!(page.items[0].body, max);
    let last = messages::list(&h.state, &h.creator, &chat.id, Some(100))
        .await
        .unwrap();
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.items[0].sequence, 101);
    assert!(last.next_after.is_none());
    for invalid in [0, -1, 102] {
        assert!(matches!(
            messages::list(&h.state, &h.creator, &chat.id, Some(invalid)).await,
            Err(ContentError::NotFound)
        ));
    }
    let private = h.chat().await;
    assert_eq!(
        messages::append(&h.state, &h.admin, &private.id, &request(), 1, "hidden").await,
        Err(ContentError::NotFound)
    );
    let doc = resources::create(&h.state, &h.creator, h.input(ResourceKind::Document, "Doc"))
        .await
        .unwrap();
    assert_eq!(
        messages::append(&h.state, &h.creator, &doc.id, &request(), 1, "wrong kind").await,
        Err(ContentError::NotFound)
    );
    let original_request = request();
    messages::append(
        &h.state,
        &h.creator,
        &private.id,
        &original_request,
        1,
        "bound",
    )
    .await
    .unwrap();
    assert_eq!(
        messages::append(
            &h.state,
            &h.creator,
            &chat.id,
            &original_request,
            2,
            "bound"
        )
        .await,
        Err(ContentError::Conflict)
    );
    assert_eq!(h.usage("charged_bytes").await, 32873);
    db::execute(
        &h.state.database,
        "UPDATE neev_sessions SET authenticated_at=1699999999,expires_at=1700000000 WHERE id=$1",
        vec![h.creator.identity.session_id.clone().into()],
    )
    .await
    .unwrap();
    assert!(matches!(
        messages::list(&h.state, &h.creator, &chat.id, None).await,
        Err(ContentError::Auth(
            crate::neev::error::AuthError::Unauthorized
        ))
    ));
}
#[tokio::test]
async fn sqlite_message_pagination_and_boundaries() {
    pages_and_boundaries(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_message_pagination_and_boundaries() {
    let (db, admin, schema) = h::postgres_database().await;
    pages_and_boundaries(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_removed_member_cannot_finish_waiting_append() {
    let (database, admin, schema) = h::postgres_database().await;
    let fixture = Harness::new(database.clone()).await;
    let chat = fixture.shared_chat().await;
    let guard = db::begin(&database).await.unwrap();
    db::lock_admission_guard(&guard).await.unwrap();
    let state = fixture.state.clone();
    let actor = fixture.creator.clone();
    let id = chat.id.clone();
    let pending = tokio::spawn(async move {
        messages::append(&state, &actor, &id, &request(), 2, "Too late").await
    });
    h::wait_for_postgres_lock(&admin, "%neev_schema_version%FOR UPDATE%").await;
    crate::neev::org::remove(&fixture.state, &fixture.admin, fixture.creator.member_id)
        .await
        .unwrap();
    guard.commit().await.unwrap();
    assert!(matches!(pending.await.unwrap(), Err(ContentError::Auth(_))));
    assert_eq!(fixture.usage("messages").await, 0);
    assert_eq!(
        resources::get(&fixture.state, &fixture.admin, &chat.id)
            .await
            .unwrap()
            .content_revision,
        0
    );
    drop(fixture);
    h::cleanup_postgres(database, admin, schema).await;
}
#[tokio::test]
async fn sqlite_human_conversations() {
    conversations(h::database().await).await;
}
#[tokio::test]
async fn sqlite_message_quota_race() {
    quotas(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_human_conversations() {
    let (db, admin, schema) = h::postgres_database().await;
    conversations(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    quotas(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
