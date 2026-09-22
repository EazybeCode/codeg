//! Storage-level behaviour of the engine binding, exercised against a real
//! database rather than a stub, because every property here is a property of
//! the receipt table's key and constraints.

use super::binding;
use crate::neev::{
    catalog::nodes,
    content::{
        resources,
        types::{ContentError, ResourceInput, ResourceKind},
    },
    db,
    mail,
    startup::ServerState,
    test_support as h,
    types::{NodeInput, NodeKind, OrgContext},
};
use sea_orm::DatabaseConnection;
use std::sync::Arc;

struct Harness {
    state: Arc<ServerState>,
    creator: OrgContext,
    other: OrgContext,
    node: i32,
    other_node: i32,
}

impl Harness {
    async fn new() -> Self {
        let database = h::database().await;
        let clock = Arc::new(h::TestClock::new(1_700_000_000));
        let mail = mail::Service::new(database.clone(), Arc::new(h::MailCapture::new()));
        let state = Arc::new(
            ServerState::with_mail(database.clone(), h::preview_config(), clock, mail).unwrap(),
        );
        let creator_id = h::native_account(&database, "engine-creator", "creator@example.test").await;
        let other_id = h::native_account(&database, "engine-other", "other@example.test").await;
        let (org, _) = h::organization(&database, creator_id, "admin").await;
        let (other_org, _) = h::organization(&database, other_id, "admin").await;
        let creator = h::scoped_context(&database, creator_id, org, 1_700_000_000).await;
        let other = h::scoped_context(&database, other_id, other_org, 1_700_000_000).await;
        let mut ids = Vec::new();
        for actor in [&creator, &other] {
            ids.push(
                nodes::create(
                    &state,
                    actor,
                    NodeInput {
                        parent_id: None,
                        kind: NodeKind::Project,
                        name: "Project".into(),
                    },
                )
                .await
                .unwrap()
                .id,
            );
        }
        Self {
            state,
            creator,
            other,
            node: ids[0],
            other_node: ids[1],
        }
    }

    /// Request ids are canonical UUIDv4s, because `normalize_request_id`
    /// requires it — a readable label is rejected as invalid input.
    fn request(label: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{label:02x}")
    }

    fn input(&self, node_id: i32, request_id: &str) -> ResourceInput {
        ResourceInput {
            node_id,
            kind: ResourceKind::Document,
            title: "Document".into(),
            origin_chat_id: None,
            request_id: request_id.into(),
        }
    }
}

async fn is_bound(database: &DatabaseConnection, resource: &crate::neev::content::types::Resource) -> bool {
    let tx = db::begin(database).await.unwrap();
    let bound = binding::bound(&tx, resource).await.unwrap();
    tx.rollback().await.unwrap();
    bound
}

#[tokio::test]
async fn an_engine_document_is_bound_and_a_native_one_is_not() {
    let harness = Harness::new().await;
    let database = harness.state.database.clone();

    let engine = resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(1)),
    )
    .await
    .unwrap();
    let native = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(2)),
    )
    .await
    .unwrap();

    assert!(is_bound(&database, &engine).await);
    assert!(!is_bound(&database, &native).await);
    assert_eq!(binding::name(true), "lasuite");
    assert_eq!(binding::name(false), "native");
}

#[tokio::test]
async fn retrying_a_creation_returns_the_same_document_and_binds_it_once() {
    // The requirement is that a retry produces no duplicate and no orphan. This
    // reuses the existing receipt rather than new retry machinery, so the test
    // is really checking that the binding did not break that guarantee — a
    // second binding row for the same key would violate the receipt table's
    // primary key and surface as an error rather than as a silent duplicate.
    let harness = Harness::new().await;
    let database = harness.state.database.clone();

    let first = resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(3)),
    )
    .await
    .unwrap();
    let second = resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(3)),
    )
    .await
    .unwrap();

    assert_eq!(first.id, second.id);
    assert!(is_bound(&database, &first).await);
    let rows = h::count(&database, "neev_resources").await;
    assert_eq!(rows, 1, "a retry created a second resource");
}

#[tokio::test]
async fn a_request_id_cannot_switch_engines_between_attempts() {
    // Retrying with the same request id but the other engine is a conflict, not
    // a silent hand-back of the wrong kind of document. Without the engine in
    // the receipt digest this would return the first document and the caller
    // would believe it got what it asked for.
    let harness = Harness::new().await;

    resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(4)),
    )
    .await
    .unwrap();
    let native = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(4)),
    )
    .await;

    assert!(
        matches!(native, Err(ContentError::Conflict)),
        "expected a conflict, got {native:?}"
    );
}

#[tokio::test]
async fn a_binding_is_not_readable_as_another_resources_binding() {
    // The receipt key is not organization-scoped, so the digest carries the
    // organization and the resource id and is recomputed on read. A row must
    // therefore mean nothing for any resource other than the one it was written
    // for.
    let harness = Harness::new().await;
    let database = harness.state.database.clone();

    let bound = resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(5)),
    )
    .await
    .unwrap();
    let unbound = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(6)),
    )
    .await
    .unwrap();

    // Same creator, same organization, different resource: still unbound.
    assert!(is_bound(&database, &bound).await);
    assert!(!is_bound(&database, &unbound).await);

    // A resource carrying the bound id but another organization does not read as
    // bound, because the digest would not match.
    let mut forged = bound.clone();
    forged.org_id = harness.other.org_id;
    assert!(!is_bound(&database, &forged).await);

    // Nor does one carrying the bound id but a different creator, since the
    // receipt key includes the creator.
    let mut wrong_creator = bound.clone();
    wrong_creator.creator_id = harness.other.identity.user_id;
    assert!(!is_bound(&database, &wrong_creator).await);
}

#[tokio::test]
async fn legacy_content_operations_are_refused_on_an_engine_document() {
    // An engine-backed resource has no native revisions. A legacy write would
    // fork it into two stores and a legacy read would render it blank, so the
    // legacy path must refuse it outright rather than treat it as empty.
    let harness = Harness::new().await;
    let database = harness.state.database.clone();

    let engine = resources::create_engine_document(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(7)),
    )
    .await
    .unwrap();
    let native = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(harness.node, &Harness::request(8)),
    )
    .await
    .unwrap();

    let tx = db::begin(&database).await.unwrap();
    assert!(matches!(
        binding::refuse_if_bound(&tx, &engine).await,
        Err(ContentError::NotFound)
    ));
    // Legacy documents are untouched by any of this and must keep working.
    assert!(binding::refuse_if_bound(&tx, &native).await.is_ok());
    tx.rollback().await.unwrap();

    let _ = harness.other_node;
}
