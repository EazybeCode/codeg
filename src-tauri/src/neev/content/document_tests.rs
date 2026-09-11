use super::{
    documents, resources,
    test_support::{patch, request, Harness},
    types::*,
    BlobStore,
};
use crate::neev::{db, test_support as h};
use std::sync::Arc;
use tokio::sync::Notify;

// Task-scoped and bounded: unrelated tests never inherit a pause/failure control.
tokio::task_local! { static PAUSE: Pause; }
struct Pause {
    phase: &'static str,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
pub(super) async fn boundary(phase: &str) {
    let signals = PAUSE
        .try_with(|p| (p.phase == phase).then(|| (p.entered.clone(), p.release.clone())))
        .ok()
        .flatten();
    if let Some((entered, release)) = signals {
        entered.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(10), release.notified())
            .await
            .expect("bounded document test pause");
    }
}
fn pause(phase: &'static str) -> (Pause, Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    (
        Pause {
            phase,
            entered: entered.clone(),
            release: release.clone(),
        },
        entered,
        release,
    )
}
async fn reached(signal: &Notify) {
    tokio::time::timeout(std::time::Duration::from_secs(10), signal.notified())
        .await
        .expect("document reached filesystem boundary");
}
fn clock(h: &mut Harness) -> Arc<h::TestClock> {
    let clock = Arc::new(h::TestClock::new(1_700_000_000));
    Arc::get_mut(&mut h.state).unwrap().clock = clock.clone();
    clock
}
fn advance(clock: &h::TestClock) {
    clock.0.fetch_add(60, std::sync::atomic::Ordering::SeqCst);
}
async fn document(h: &Harness) -> Resource {
    resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Notes"),
    )
    .await
    .unwrap()
}
async fn shared_document(h: &Harness) -> Resource {
    let doc = document(h).await;
    let mut p = patch(&doc);
    p.visibility = Visibility::Organization;
    resources::update(&h.state, &h.creator, &doc.id, p)
        .await
        .unwrap()
}

fn input(head: i64, auth: i64, text: &str) -> SaveInput {
    SaveInput {
        request_id: request(),
        expected_content_revision: head,
        expected_auth_revision: auth,
        markdown: text.into(),
    }
}

// Catches overwritten history, stale-writer publication, duplicate charging and restore-in-place.
async fn immutable_history(database: sea_orm::DatabaseConnection) {
    let mut h = Harness::new(database).await;
    let doc = resources::create(
        &h.state,
        &h.creator,
        h.input(ResourceKind::Document, "Notes"),
    )
    .await
    .unwrap();
    assert_eq!(doc.content_revision, 0);
    let first_input = input(0, 1, "# First é");
    let first = documents::save(&h.state, &h.creator, &doc.id, first_input.clone())
        .await
        .unwrap();
    assert_eq!(
        (
            first.revision,
            first.parent_revision,
            first.restored_from,
            first.byte_length
        ),
        (1, None, None, 10)
    );
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, first_input.clone())
            .await
            .unwrap(),
        first
    );
    let mut different = first_input;
    different.markdown = "Different".into();
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, different).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, input(0, 1, "Stale")).await,
        Err(ContentError::Conflict)
    );
    let second = documents::save(&h.state, &h.creator, &doc.id, input(1, 1, "Second"))
        .await
        .unwrap();
    let restore_key = request();
    let third = documents::restore(&h.state, &h.creator, &doc.id, 1, &restore_key, 2, 1)
        .await
        .unwrap();
    assert_eq!(
        (third.revision, third.parent_revision, third.restored_from),
        (3, Some(2), Some(1))
    );
    assert_eq!(
        documents::restore(&h.state, &h.creator, &doc.id, 1, &restore_key, 2, 1)
            .await
            .unwrap(),
        third
    );
    assert_eq!(
        documents::restore(&h.state, &h.creator, &doc.id, 2, &restore_key, 2, 1).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(h.usage("charged_bytes").await, 26);
    Arc::get_mut(&mut h.state).unwrap().blobs =
        Arc::new(BlobStore::open(&h.state.config.document_root).unwrap());
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, Some(1))
            .await
            .unwrap(),
        (first.clone(), "# First é".as_bytes().to_vec())
    );
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, Some(2))
            .await
            .unwrap(),
        (second.clone(), b"Second".to_vec())
    );
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap(),
        (third.clone(), "# First é".as_bytes().to_vec())
    );
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .items,
        vec![first, second, third]
    );
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, Some(3))
            .await
            .unwrap()
            .items
            .len(),
        0
    );
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, Some(4)).await,
        Err(ContentError::NotFound)
    );
    let keys = db::one(
        &h.state.database,
        "SELECT COUNT(DISTINCT blob_key) AS n FROM neev_document_revisions",
        vec![],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "n")
    .unwrap();
    assert_eq!(keys, 3);
}

#[tokio::test]
async fn sqlite_document_immutable_history() {
    immutable_history(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_immutable_history() {
    let (db, admin, schema) = h::postgres_database().await;
    immutable_history(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

async fn denied(h: &Harness, actor: &crate::neev::types::OrgContext, id: &str) {
    assert_eq!(
        documents::history(&h.state, actor, id, None).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        documents::history(&h.state, actor, id, Some(1)).await,
        Err(ContentError::NotFound)
    );
    for rev in [None, Some(1), Some(1000)] {
        assert_eq!(
            documents::read(&h.state, actor, id, rev).await,
            Err(ContentError::NotFound)
        );
    }
    assert_eq!(
        documents::save(&h.state, actor, id, input(1, 1, "Hidden")).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        documents::restore(&h.state, actor, id, 1, &request(), 1, 1).await,
        Err(ContentError::NotFound)
    );
}
// Catches administrator private override and a second audience gate on bound docs.
async fn audiences(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let doc = document(&h).await;
    documents::save(&h.state, &h.creator, &doc.id, input(0, 1, "Private"))
        .await
        .unwrap();
    denied(&h, &h.admin, &doc.id).await;
    denied(&h, &h.foreign, &doc.id).await;
    let chat = h.shared_chat().await;
    let mut i = h.input(ResourceKind::Document, "Attached");
    i.origin_chat_id = Some(chat.id.clone());
    let bound = resources::create(&h.state, &h.admin, i).await.unwrap();
    documents::save(&h.state, &h.admin, &bound.id, input(0, 1, "Bound"))
        .await
        .unwrap();
    let mut p = patch(&chat);
    p.visibility = Visibility::Private;
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    denied(&h, &h.admin, &bound.id).await;
    assert_eq!(
        documents::read(&h.state, &h.creator, &bound.id, None)
            .await
            .unwrap()
            .1,
        b"Bound"
    );
    documents::save(&h.state, &h.creator, &bound.id, input(1, 2, "Owner"))
        .await
        .unwrap();
    let current = resources::get(&h.state, &h.creator, &bound.id)
        .await
        .unwrap();
    let mut p = patch(&current);
    p.archived = true;
    // Bound creator lost access; source owner can still read/save, but cannot independently archive another creator's doc.
    assert_eq!(
        resources::update(&h.state, &h.creator, &bound.id, p).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        documents::read(&h.state, &h.creator, &chat.id, None).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, Some(-1)).await,
        Err(ContentError::NotFound)
    );
}

// Catches non-atomic head checks and quota admission across two documents.
async fn concurrent_boundaries(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let doc = shared_document(&h).await;
    let (a, b) = tokio::join!(
        documents::save(&h.state, &h.creator, &doc.id, input(0, 2, "Alice")),
        documents::save(&h.state, &h.admin, &doc.id, input(0, 2, "Bob"))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (winner, loser, body) = if a.is_ok() {
        (a, b, b"Alice".to_vec())
    } else {
        (b, a, b"Bob".to_vec())
    };
    assert_eq!(loser, Err(ContentError::Conflict));
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap(),
        (winner.unwrap(), body)
    );
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    let other = document(&h).await;
    let exact = "é".repeat(524288);
    assert_eq!(
        documents::save(
            &h.state,
            &h.creator,
            &other.id,
            input(0, 1, &format!("{exact}x"))
        )
        .await,
        Err(ContentError::Invalid)
    );
    assert_eq!(
        documents::save(&h.state, &h.creator, &other.id, input(0, 1, &exact))
            .await
            .unwrap()
            .byte_length,
        1048576
    );
    assert_eq!(
        documents::read(&h.state, &h.creator, &other.id, None)
            .await
            .unwrap()
            .1,
        exact.as_bytes()
    );
    db::execute(
        &h.state.database,
        "UPDATE neev_content_usage SET charged_bytes=1073741823 WHERE org_id=$1",
        vec![h.creator.org_id.into()],
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        documents::save(&h.state, &h.creator, &doc.id, input(1, 2, "x")),
        documents::save(&h.state, &h.creator, &other.id, input(1, 1, "y"))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(if a.is_err() { a } else { b }, Err(ContentError::Quota));
    assert_eq!(h.usage("charged_bytes").await, 1073741824);
}

#[tokio::test]
async fn sqlite_document_audiences() {
    audiences(h::database().await).await;
}
#[tokio::test]
async fn sqlite_document_concurrent_boundaries() {
    concurrent_boundaries(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_audiences_and_boundaries() {
    let (db, admin, schema) = h::postgres_database().await;
    audiences(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    concurrent_boundaries(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

// Catches off-by-one revision cap and pagination that omits/repeats boundary rows.
async fn revision_limit(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let doc = shared_document(&h).await;
    documents::save(&h.state, &h.creator, &doc.id, input(0, 2, ""))
        .await
        .unwrap();
    let tx = db::begin(&h.state.database).await.unwrap();
    for revision in 2_i64..=999 {
        db::execute(&tx,"INSERT INTO neev_document_revisions(org_id,document_id,resource_kind,revision,blob_key,sha256,byte_length,author_id,created_at,parent_revision,restored_from) VALUES($1,$2,'document',$3,$4,$5,0,$6,1700000000,$7,NULL)",vec![h.creator.org_id.into(),doc.id.clone().into(),revision.into(),request().into(),"0".repeat(64).into(),h.creator.identity.user_id.into(),(revision-1).into()]).await.unwrap();
    }
    db::execute(
        &tx,
        "UPDATE neev_resources SET content_revision=999 WHERE id=$1",
        vec![doc.id.clone().into()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (a, b) = tokio::join!(
        documents::save(&h.state, &h.creator, &doc.id, input(999, 2, "a")),
        documents::save(&h.state, &h.admin, &doc.id, input(999, 2, "b"))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        if a.is_ok() {
            a.unwrap().revision
        } else {
            b.unwrap().revision
        },
        1000
    );
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, input(1000, 2, "overflow")).await,
        Err(ContentError::Quota)
    );
    let mut cursor = None;
    let mut revisions = Vec::new();
    loop {
        let page = documents::history(&h.state, &h.creator, &doc.id, cursor)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 100);
        revisions.extend(page.items.iter().map(|r| r.revision));
        cursor = page.next_after.map(|v| v.parse().unwrap());
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(revisions, (1..=1000).collect::<Vec<_>>());
    assert_eq!(h.usage("charged_bytes").await, 1);
}
#[tokio::test]
async fn sqlite_document_revision_limit() {
    revision_limit(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_revision_limit() {
    let (db, admin, schema) = h::postgres_database().await;
    revision_limit(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

#[cfg(unix)]
#[tokio::test]
async fn sqlite_document_every_blob_failure_stays_charged() {
    use super::blob::FailurePoint::*;
    for point in [
        BeforeWrite,
        AfterWrite,
        BeforeFileSync,
        AfterFileSync,
        BeforeFinalize,
        AfterFinalize,
        BeforeDirectorySync,
        AfterDirectorySync,
    ] {
        let mut h = Harness::new(h::database().await).await;
        let clock = clock(&mut h);
        let doc = document(&h).await;
        let root = h.state.config.document_root.clone();
        Arc::get_mut(&mut h.state).unwrap().blobs =
            Arc::new(BlobStore::open_with_failure(&root, point).unwrap());
        let i = input(0, 1, "orphan");
        assert_eq!(
            documents::save(&h.state, &h.creator, &doc.id, i.clone()).await,
            Err(ContentError::Storage),
            "{point:?}"
        );
        assert_eq!(h.usage("charged_bytes").await, 6);
        assert!(documents::history(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .items
            .is_empty());
        assert_eq!(
            documents::read(&h.state, &h.creator, &doc.id, None).await,
            Err(ContentError::NotFound)
        );
        assert_eq!(
            documents::save(&h.state, &h.creator, &doc.id, i.clone()).await,
            Err(ContentError::Conflict)
        );
        Arc::get_mut(&mut h.state).unwrap().blobs = Arc::new(BlobStore::open(&root).unwrap());
        advance(&clock);
        assert_eq!(
            documents::save(&h.state, &h.creator, &doc.id, i)
                .await
                .unwrap()
                .revision,
            1
        );
        assert_eq!(h.usage("charged_bytes").await, 12);
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_expiry_after_blocking_resource_lock() {
    let (database, admin, schema) = h::postgres_database().await;
    let mut h = Harness::new(database.clone()).await;
    let clock = clock(&mut h);
    let doc = document(&h).await;
    let (p, entered, release) = pause("written");
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let pending = tokio::spawn(PAUSE.scope(p, async move {
        documents::save(&state, &actor, &id, input(0, 1, "expired")).await
    }));
    reached(&entered).await;
    let guard = db::begin(&database).await.unwrap();
    db::one(
        &guard,
        "SELECT id FROM neev_resources WHERE id=$1 FOR UPDATE",
        vec![doc.id.clone().into()],
    )
    .await
    .unwrap();
    release.notify_one();
    h::wait_for_postgres_lock(&admin, "%neev_resources%FOR UPDATE%").await;
    advance(&clock);
    guard.commit().await.unwrap();
    assert_eq!(pending.await.unwrap(), Err(ContentError::Conflict));
    assert_eq!(h.usage("charged_bytes").await, 7);
    assert!(documents::history(&h.state, &h.creator, &doc.id, None)
        .await
        .unwrap()
        .items
        .is_empty());
    drop(h);
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_expiry_during_publication_rolls_back() {
    expiry_during_publication("neev_requests", "INSERT INTO neev_requests%").await;
    expiry_during_publication("neev_content_leases", "DELETE FROM neev_content_leases%").await;
}

async fn expiry_during_publication(table: &str, waiting_query: &str) {
    let (database, admin, schema) = h::postgres_database().await;
    let mut h = Harness::new(database.clone()).await;
    let clock = clock(&mut h);
    let doc = document(&h).await;
    let (p, entered, release) = pause("written");
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let pending = tokio::spawn(PAUSE.scope(p, async move {
        documents::save(&state, &actor, &id, input(0, 1, "expired")).await
    }));
    reached(&entered).await;
    let guard = db::begin(&database).await.unwrap();
    db::execute(&guard, &format!("LOCK TABLE {table} IN SHARE MODE"), vec![])
        .await
        .unwrap();
    release.notify_one();
    h::wait_for_postgres_lock(&admin, waiting_query).await;
    advance(&clock);
    guard.commit().await.unwrap();
    let result = pending.await.unwrap();
    let charged = h.usage("charged_bytes").await;
    let head = resources::get(&h.state, &h.creator, &doc.id)
        .await
        .unwrap()
        .content_revision;
    let history = documents::history(&h.state, &h.creator, &doc.id, None)
        .await
        .unwrap();
    drop(h);
    h::cleanup_postgres(database, admin, schema).await;
    assert_eq!(result, Err(ContentError::Conflict));
    assert_eq!(charged, 7);
    assert_eq!(head, 0);
    assert!(history.items.is_empty());
}

#[tokio::test]
async fn sqlite_document_expired_takeover_cannot_delete_successor_lease() {
    let mut h = Harness::new(h::database().await).await;
    let clock = clock(&mut h);
    let doc = document(&h).await;
    let i = input(0, 1, "retry");
    let (p, entered, release) = pause("written");
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let old_input = i.clone();
    let old = tokio::spawn(PAUSE.scope(p, async move {
        documents::save(&state, &actor, &id, old_input).await
    }));
    reached(&entered).await;
    assert_eq!(
        documents::save(&h.state, &h.creator, &doc.id, i.clone()).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(h.usage("charged_bytes").await, 5);
    advance(&clock);
    let (p, new_entered, new_release) = pause("reserved");
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let new = tokio::spawn(PAUSE.scope(
        p,
        async move { documents::save(&state, &actor, &id, i).await },
    ));
    reached(&new_entered).await;
    release.notify_one();
    assert_eq!(old.await.unwrap(), Err(ContentError::Conflict));
    assert_eq!(h.usage("charged_bytes").await, 10);
    new_release.notify_one();
    assert_eq!(new.await.unwrap().unwrap().revision, 1);
    assert_eq!(
        documents::read(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .1,
        b"retry"
    );
}

async fn removed_at_filesystem_boundary(
    database: sea_orm::DatabaseConnection,
    phase: &'static str,
) {
    let h = Harness::new(database).await;
    let doc = shared_document(&h).await;
    let (p, entered, release) = pause(phase);
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let pending = tokio::spawn(PAUSE.scope(p, async move {
        documents::save(&state, &actor, &id, input(0, 2, "removed")).await
    }));
    reached(&entered).await;
    // This real membership mutation must finish while the file operation is paused.
    crate::neev::org::remove(&h.state, &h.admin, h.creator.member_id)
        .await
        .unwrap();
    release.notify_one();
    assert!(matches!(pending.await.unwrap(), Err(ContentError::Auth(_))));
    assert_eq!(h.usage("charged_bytes").await, 7);
    assert!(documents::history(&h.state, &h.admin, &doc.id, None)
        .await
        .unwrap()
        .items
        .is_empty());
    assert_eq!(
        resources::get(&h.state, &h.admin, &doc.id)
            .await
            .unwrap()
            .content_revision,
        0
    );
}

async fn source_bound(h: &Harness) -> (Resource, Resource) {
    let chat = h.shared_chat().await;
    let mut i = h.input(ResourceKind::Document, "Attached");
    i.origin_chat_id = Some(chat.id.clone());
    let doc = resources::create(&h.state, &h.admin, i).await.unwrap();
    documents::save(&h.state, &h.admin, &doc.id, input(0, 1, "Source"))
        .await
        .unwrap();
    (chat, doc)
}

// Covers all read-based body/download paths and restore's source read, on both sides of I/O.
async fn source_narrowed_during_read(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    for phase in ["before_read", "read"] {
        for operation in ["head", "revision", "restore"] {
            let (chat, doc) = source_bound(&h).await;
            let (p, entered, release) = pause(phase);
            let state = h.state.clone();
            let actor = h.admin.clone();
            let id = doc.id.clone();
            let pending = tokio::spawn(PAUSE.scope(p, async move {
                match operation {
                    "restore" => documents::restore(&state, &actor, &id, 1, &request(), 1, 1)
                        .await
                        .map(|_| ()),
                    _ => documents::read(
                        &state,
                        &actor,
                        &id,
                        if operation == "head" { None } else { Some(1) },
                    )
                    .await
                    .map(|_| ()),
                }
            }));
            reached(&entered).await;
            let mut p = patch(&chat);
            p.visibility = Visibility::Private;
            resources::update(&h.state, &h.creator, &chat.id, p)
                .await
                .unwrap();
            release.notify_one();
            assert_eq!(
                pending.await.unwrap(),
                Err(ContentError::NotFound),
                "{phase}/{operation}"
            );
            assert_eq!(
                documents::history(&h.state, &h.creator, &doc.id, None)
                    .await
                    .unwrap()
                    .items
                    .len(),
                1
            );
        }
    }
    assert_eq!(h.usage("charged_bytes").await, 36);
    // Retaining access is insufficient if the source's authorization revision changed.
    let (chat, doc) = source_bound(&h).await;
    let (p, entered, release) = pause("read");
    let state = h.state.clone();
    let actor = h.creator.clone();
    let id = doc.id.clone();
    let pending = tokio::spawn(PAUSE.scope(p, async move {
        documents::read(&state, &actor, &id, None).await
    }));
    reached(&entered).await;
    let mut p = patch(&chat);
    p.title = "Changed during read".into();
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(ContentError::Conflict));
}

async fn source_narrowed_during_restore_write(database: sea_orm::DatabaseConnection) {
    let mut h = Harness::new(database).await;
    let clock = clock(&mut h);
    let (chat, doc) = source_bound(&h).await;
    let (p, entered, release) = pause("written");
    let state = h.state.clone();
    let actor = h.admin.clone();
    let id = doc.id.clone();
    let pending = tokio::spawn(PAUSE.scope(p, async move {
        documents::restore(&state, &actor, &id, 1, &request(), 1, 1).await
    }));
    reached(&entered).await;
    let mut p = patch(&chat);
    p.visibility = Visibility::Private;
    assert_eq!(
        resources::update(&h.state, &h.creator, &chat.id, p.clone()).await,
        Err(ContentError::Conflict)
    );
    let mut archived = patch(&chat);
    archived.archived = true;
    assert_eq!(
        resources::update(&h.state, &h.creator, &chat.id, archived).await,
        Err(ContentError::Conflict)
    );
    advance(&clock);
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap(), Err(ContentError::NotFound));
    assert_eq!(h.usage("charged_bytes").await, 12);
    assert_eq!(
        documents::history(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn sqlite_document_authority_races() {
    for phase in ["reserved", "written"] {
        removed_at_filesystem_boundary(h::database().await, phase).await;
    }
    source_narrowed_during_read(h::database().await).await;
    source_narrowed_during_restore_write(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_authority_races() {
    for phase in ["reserved", "written"] {
        let (db, admin, schema) = h::postgres_database().await;
        removed_at_filesystem_boundary(db.clone(), phase).await;
        h::cleanup_postgres(db, admin, schema).await;
    }
    let (db, admin, schema) = h::postgres_database().await;
    source_narrowed_during_read(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
    let (db, admin, schema) = h::postgres_database().await;
    source_narrowed_during_restore_write(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}

// Real database errors after different publication steps must roll back the entire
// publication, preserve the reservation charge, and leave retry possible after expiry.
async fn publication_failures(database: sea_orm::DatabaseConnection) {
    use sea_orm::ConnectionTrait;
    let mut h = Harness::new(database).await;
    let clock = clock(&mut h);
    let postgres = h.state.database.get_database_backend() == sea_orm::DbBackend::Postgres;
    if postgres {
        db::execute(&h.state.database,"CREATE FUNCTION document_failure() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''injected document publication failure''; END'",vec![]).await.unwrap();
    }
    for (table, event) in [
        ("neev_document_revisions", "INSERT"),
        ("neev_requests", "INSERT"),
        ("neev_content_leases", "DELETE"),
    ] {
        let doc = document(&h).await;
        let charged_before = h.usage("charged_bytes").await;
        let i = input(0, 1, "failed");
        let (p, entered, release) = pause("written");
        let state = h.state.clone();
        let actor = h.creator.clone();
        let id = doc.id.clone();
        let saved = i.clone();
        let pending = tokio::spawn(PAUSE.scope(p, async move {
            documents::save(&state, &actor, &id, saved).await
        }));
        reached(&entered).await;
        let sql = if postgres {
            format!("CREATE TRIGGER document_failure BEFORE {event} ON {table} FOR EACH ROW EXECUTE FUNCTION document_failure()")
        } else {
            format!("CREATE TRIGGER document_failure BEFORE {event} ON {table} BEGIN SELECT RAISE(ABORT,'injected document publication failure'); END")
        };
        db::execute(&h.state.database, &sql, vec![]).await.unwrap();
        release.notify_one();
        assert!(pending.await.unwrap().is_err());
        assert_eq!(h.usage("charged_bytes").await, charged_before + 6);
        let sql = if postgres {
            format!("DROP TRIGGER document_failure ON {table}")
        } else {
            "DROP TRIGGER document_failure".into()
        };
        db::execute(&h.state.database, &sql, vec![]).await.unwrap();
        assert_eq!(
            resources::get(&h.state, &h.creator, &doc.id)
                .await
                .unwrap()
                .content_revision,
            0
        );
        assert!(documents::history(&h.state, &h.creator, &doc.id, None)
            .await
            .unwrap()
            .items
            .is_empty());
        assert_eq!(
            documents::read(&h.state, &h.creator, &doc.id, None).await,
            Err(ContentError::NotFound)
        );
        assert_eq!(
            documents::save(&h.state, &h.creator, &doc.id, i.clone()).await,
            Err(ContentError::Conflict)
        );
        advance(&clock);
        assert_eq!(
            documents::save(&h.state, &h.creator, &doc.id, i)
                .await
                .unwrap()
                .revision,
            1
        );
    }
    assert_eq!(h.usage("charged_bytes").await, 36);
}
#[tokio::test]
async fn sqlite_document_publication_failures() {
    publication_failures(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_document_publication_failures() {
    let (db, admin, schema) = h::postgres_database().await;
    publication_failures(db.clone()).await;
    h::cleanup_postgres(db, admin, schema).await;
}
