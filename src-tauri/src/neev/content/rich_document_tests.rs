use super::{
    documents, resources, rich_tests,
    test_support::{request, Harness},
    types::*,
};
use crate::neev::test_support as h;
use std::sync::Arc;
use tokio::sync::Notify;

tokio::task_local! { static PAUSE: (&'static str, Arc<Notify>, Arc<Notify>); }
pub(super) async fn boundary(phase: &str) {
    if let Some((entered, release)) = PAUSE
        .try_with(|(p, a, b)| (*p == phase).then(|| (a.clone(), b.clone())))
        .ok()
        .flatten()
    {
        entered.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(10), release.notified())
            .await
            .expect("bounded rich I/O pause");
    }
}

#[tokio::test]
async fn sqlite_rich_save_persists_canonical_bytes() {
    let h = Harness::new(h::database().await).await;
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Notes"),
    )
    .await
    .unwrap();
    let saved = documents::save_rich(
        &h.state,
        &h.creator,
        &doc.id,
        RichSaveInput {
            request_id: request(),
            expected_content_revision: 0,
            expected_auth_revision: 1,
            document: rich_tests::document(),
        },
    )
    .await
    .expect("rich save publishes a revision");
    assert_eq!(saved.revision, 1);
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .1,
        rich_tests::SOURCE
    );
}

async fn mixed_history(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Mixed"),
    )
    .await
    .unwrap();
    let old = "{\"looks\":\"like JSON\"}\r\n";
    let markdown_input = SaveInput {
        request_id: request(),
        expected_content_revision: 0,
        expected_auth_revision: 1,
        markdown: old.into(),
    };
    let first = documents::save(&h.state, &h.creator, &doc.id, markdown_input.clone())
        .await
        .unwrap();
    assert_eq!(
        (first.format, first.format_version),
        (DocumentFormat::Markdown, 1)
    );
    // Seed the historical receipt recipe independently, then migrate the exact
    // populated schema5 inventory. This catches digest changes and double charge.
    let historical_hash = crate::neev::org::digest_parts(&[
        "content.document.save",
        &h.creator.org_id.to_string(),
        &doc.id,
        "0",
        "1",
        old,
        "",
    ]);
    crate::neev::db::execute(&h.state.database,"UPDATE neev_requests SET body_hash=$1 WHERE user_id=$2 AND action='content.document.save' AND request_id=$3",vec![historical_hash.into(),h.creator.identity.user_id.into(),markdown_input.request_id.clone().into()]).await.unwrap();
    let historical_charge = h.usage("charged_bytes").await;
    for table in [
        "neev_image_blobs",
        "neev_org_join_codes",
        "neev_gateway_revoke_intents",
        "neev_gateway_redemptions",
        "neev_gateway_relying_sessions",
        "neev_gateway_exchanges",
        "neev_gateway_relying_bindings",
        "neev_gateway_revocations",
        "neev_gateway_audience",
        "neev_agent_status_checks",
        "neev_agent_status_owner",
    ] {
        crate::neev::db::execute(&h.state.database, &format!("DROP TABLE {table}"), vec![])
            .await
            .unwrap();
    }
    crate::neev::db::execute(
        &h.state.database,
        "DROP TABLE neev_document_formats",
        vec![],
    )
    .await
    .unwrap();
    crate::neev::db::execute(
        &h.state.database,
        "UPDATE neev_schema_version SET version=5",
        vec![],
    )
    .await
    .unwrap();
    crate::neev::db::initialize(&h.state.database)
        .await
        .unwrap();
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, markdown_input)
            .await
            .unwrap(),
        first
    );
    assert_eq!(h.usage("charged_bytes").await, historical_charge);
    let input = RichSaveInput {
        request_id: request(),
        expected_content_revision: 1,
        expected_auth_revision: 1,
        document: rich_tests::document(),
    };
    let second = documents::save_rich(&h.state, &h.creator, &doc.id, input.clone())
        .await
        .unwrap();
    assert_eq!(
        (second.format, second.format_version),
        (DocumentFormat::Blocks, 1)
    );
    let charge = h.usage("charged_bytes").await;
    assert_eq!(
        documents::save_rich(&h.state, &h.creator, &doc.id, input.clone())
            .await
            .unwrap(),
        second
    );
    assert_eq!(h.usage("charged_bytes").await, charge);
    let mut changed = input.clone();
    changed.document.blocks.clear();
    assert_eq!(
        documents::save_rich(&h.state, &h.creator, &doc.id, changed).await,
        Err(ContentError::Invalid)
    );
    let mut changed = input.clone();
    if let super::rich::RichBlock::Paragraph { content, .. } = &mut changed.document.blocks[0] {
        content[0].text = "Changed".into();
    }
    assert_eq!(
        documents::save_rich(&h.state, &h.creator, &doc.id, changed).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(
        documents::save(
            &h.state,
            &h.creator,
            &doc.id,
            SaveInput {
                request_id: input.request_id.clone(),
                expected_content_revision: 1,
                expected_auth_revision: 1,
                markdown: String::from_utf8(rich_tests::SOURCE.to_vec()).unwrap()
            }
        )
        .await,
        Err(ContentError::Conflict)
    );
    for (head, auth) in [(1, 1), (2, 2)] {
        let mut stale = input.clone();
        stale.request_id = request();
        stale.expected_content_revision = head;
        stale.expected_auth_revision = auth;
        assert_eq!(
            documents::save_rich(&h.state, &h.creator, &doc.id, stale).await,
            Err(ContentError::Conflict)
        );
    }
    let third = documents::restore(&h.state, &h.creator, &doc.id, 1, &request(), 2, 1)
        .await
        .unwrap();
    let fourth = documents::restore(&h.state, &h.creator, &doc.id, 2, &request(), 3, 1)
        .await
        .unwrap();
    for (revision, format, body) in [
        (1, DocumentFormat::Markdown, old.as_bytes()),
        (2, DocumentFormat::Blocks, rich_tests::SOURCE),
        (3, DocumentFormat::Markdown, old.as_bytes()),
        (4, DocumentFormat::Blocks, rich_tests::SOURCE),
    ] {
        let (meta, bytes) = documents::read(&h.state, &h.creator, &doc.id, Some(revision))
            .await
            .unwrap();
        assert_eq!(
            (meta.revision, meta.format, meta.format_version),
            (revision, format, 1)
        );
        assert_eq!(bytes, body);
    }
    assert_eq!(
        (third.sha256, third.parent_revision, third.restored_from),
        (first.sha256, Some(2), Some(1))
    );
    assert_eq!(
        (fourth.sha256, fourth.parent_revision, fourth.restored_from),
        (second.sha256, Some(3), Some(2))
    );
    for actor in [&h.admin, &h.foreign] {
        assert_eq!(
            documents::read(&h.state, actor, &doc.id, None).await,
            Err(ContentError::NotFound)
        );
        assert_eq!(
            documents::save_rich(&h.state, actor, &doc.id, input.clone()).await,
            Err(ContentError::NotFound)
        );
    }
}
#[tokio::test]
async fn sqlite_rich_mixed_history() {
    mixed_history(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_rich_mixed_history() {
    let (db, admin, schema) = h::postgres_database().await;
    eprintln!("rich history owned schema: {schema}");
    mixed_history(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

async fn source_authority_during_io(database: sea_orm::DatabaseConnection, phase: &'static str) {
    let mut h = Harness::new(database).await;
    let clock = Arc::new(h::TestClock::new(1_700_000_000));
    Arc::get_mut(&mut h.state).unwrap().clock = clock.clone();
    let chat = h.shared_chat().await;
    let mut create = h.input(ResourceKind::Document, "Attached");
    create.origin_chat_id = Some(chat.id.clone());
    let doc = resources::create(&h.state, &h.admin, create).await.unwrap();
    let input = RichSaveInput {
        request_id: request(),
        expected_content_revision: 0,
        expected_auth_revision: 1,
        document: rich_tests::document(),
    };
    documents::save_rich(&h.state, &h.admin, &doc.id, input.clone())
        .await
        .unwrap();
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .1,
        rich_tests::SOURCE
    );
    assert_eq!(
        documents::read(&h.state, &h.foreign, &doc.id, None).await,
        Err(ContentError::NotFound)
    );
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let state = h.state.clone();
    let actor = h.admin.clone();
    let id = doc.id.clone();
    let worker = tokio::spawn(
        PAUSE.scope((phase, entered.clone(), release.clone()), async move {
            if phase == "written" {
                let mut next = input;
                next.request_id = request();
                next.expected_content_revision = 1;
                documents::save_rich(&state, &actor, &id, next)
                    .await
                    .map(|_| ())
            } else {
                documents::read(&state, &actor, &id, None).await.map(|_| ())
            }
        }),
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .expect("rich operation reached I/O");
    let mut patch = super::test_support::patch(&chat);
    patch.visibility = Visibility::Private;
    if phase == "written" {
        assert_eq!(
            resources::update(&h.state, &h.creator, &chat.id, patch.clone()).await,
            Err(ContentError::Conflict)
        );
        clock.0.fetch_add(60, std::sync::atomic::Ordering::SeqCst);
    }
    resources::update(&h.state, &h.creator, &chat.id, patch)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(worker.await.unwrap(), Err(ContentError::NotFound));
    let (revision, bytes) = documents::read(&h.state, &h.creator, &doc.id, None)
        .await
        .unwrap();
    assert_eq!(
        (revision.revision, revision.format),
        (1, DocumentFormat::Blocks)
    );
    assert_eq!(bytes, rich_tests::SOURCE);
    assert_eq!(
        documents::read(&h.state, &h.admin, &doc.id, None).await,
        Err(ContentError::NotFound)
    );
    let next = documents::save_rich(
        &h.state,
        &h.creator,
        &doc.id,
        RichSaveInput {
            request_id: request(),
            expected_content_revision: 1,
            expected_auth_revision: 2,
            document: rich_tests::document(),
        },
    )
    .await;
    assert_eq!(next.unwrap().revision, 2);
}
#[tokio::test]
async fn sqlite_rich_source_authority_during_io() {
    for phase in ["written", "read"] {
        source_authority_during_io(h::database().await, phase).await;
    }
}
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_rich_source_authority_during_io() {
    for phase in ["written", "read"] {
        let (db, admin, schema) = h::postgres_database().await;
        eprintln!("rich source I/O owned schema: {schema}");
        source_authority_during_io(db.clone(), phase).await;
        h::cleanup_postgres(db, admin, schema).await;
    }
}
