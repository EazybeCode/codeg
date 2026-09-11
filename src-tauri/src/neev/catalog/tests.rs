use std::sync::Arc;

use sea_orm::{ConnectionTrait, Statement};

use crate::neev::{
    db,
    error::AuthError,
    org,
    startup::ServerState,
    test_support::{self, OrgHarness},
    types::{NodeInput, NodeKind},
};

use super::nodes;

fn input(parent_id: Option<i32>, kind: NodeKind, name: &str) -> NodeInput {
    NodeInput {
        parent_id,
        kind,
        name: name.into(),
    }
}

#[tokio::test]
async fn folders_can_nest_without_any_github_identity() {
    let h = OrgHarness::new().await;
    assert_eq!(
        count_matching(
            &h.database,
            "SELECT COUNT(*) AS n FROM neev_identities WHERE provider='github'",
        )
        .await,
        0
    );
    let root = nodes::create(
        h.state(),
        h.owner_context(),
        input(None, NodeKind::Project, "Marketing"),
    )
    .await
    .unwrap();
    let folder = nodes::create(
        h.state(),
        h.owner_context(),
        input(Some(root.id), NodeKind::Folder, "Campaigns"),
    )
    .await
    .unwrap();
    let nested = nodes::create(
        h.state(),
        h.member_context(),
        input(Some(folder.id), NodeKind::Folder, "Launch"),
    )
    .await
    .unwrap();
    assert_eq!(nested.parent_id, Some(folder.id));
    assert_eq!(nested.org_id, h.org_id());
    assert_eq!(nested.revision, 1);
    assert_eq!(
        nodes::create(
            h.state(),
            h.other_org_context(),
            input(Some(folder.id), NodeKind::Folder, "Escape"),
        )
        .await
        .err(),
        Some(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn node_shapes_names_and_native_capability_boundary_fail_closed() {
    let h = OrgHarness::new().await;
    let root = nodes::create(
        h.state(),
        h.owner_context(),
        input(None, NodeKind::Project, "  Product  "),
    )
    .await
    .unwrap();
    assert_eq!(root.name, "Product");

    let too_long = "x".repeat(101);
    for invalid in ["", " \t ", "line\nbreak", too_long.as_str()] {
        assert_eq!(
            nodes::create(
                h.state(),
                h.owner_context(),
                input(None, NodeKind::Project, invalid),
            )
            .await
            .err(),
            Some(AuthError::Invalid)
        );
    }
    assert_eq!(
        nodes::create(
            h.state(),
            h.owner_context(),
            input(None, NodeKind::Folder, "Orphan"),
        )
        .await
        .err(),
        Some(AuthError::Invalid)
    );
    assert_eq!(
        nodes::create(
            h.state(),
            h.owner_context(),
            input(Some(root.id), NodeKind::Project, "Nested project"),
        )
        .await
        .err(),
        Some(AuthError::Invalid)
    );
    assert_eq!(
        nodes::create(
            h.state(),
            h.owner_context(),
            input(Some(root.id), NodeKind::WorkingDirectory, "Repository"),
        )
        .await
        .err(),
        Some(AuthError::Unavailable)
    );
    let preserved = db::one(
        &h.database,
        "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'working_directory','Preserved',1,$3) RETURNING id",
        vec![
            h.org_id().into(),
            root.id.into(),
            h.owner_context().identity.user_id.into(),
        ],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i32>("", "id")
    .unwrap();
    assert_eq!(
        nodes::update(
            h.state(),
            h.owner_context(),
            preserved,
            1,
            input(Some(root.id), NodeKind::Folder, "Conversion"),
        )
        .await
        .err(),
        Some(AuthError::Unavailable)
    );
}

#[tokio::test]
async fn moves_recheck_revision_tenant_and_ancestors() {
    let h = OrgHarness::new().await;
    let first = nodes::create(
        h.state(),
        h.owner_context(),
        input(None, NodeKind::Project, "First"),
    )
    .await
    .unwrap();
    let second = nodes::create(
        h.state(),
        h.owner_context(),
        input(None, NodeKind::Project, "Second"),
    )
    .await
    .unwrap();
    let folder = nodes::create(
        h.state(),
        h.owner_context(),
        input(Some(first.id), NodeKind::Folder, "Folder"),
    )
    .await
    .unwrap();
    let child = nodes::create(
        h.state(),
        h.owner_context(),
        input(Some(folder.id), NodeKind::Folder, "Child"),
    )
    .await
    .unwrap();

    assert_eq!(
        nodes::update(
            h.state(),
            h.owner_context(),
            folder.id,
            folder.revision,
            input(Some(child.id), NodeKind::Folder, "Cycle"),
        )
        .await
        .err(),
        Some(AuthError::Invalid)
    );
    let moved = nodes::update(
        h.state(),
        h.member_context(),
        child.id,
        child.revision,
        input(Some(second.id), NodeKind::Folder, "Moved"),
    )
    .await
    .unwrap();
    assert_eq!(moved.parent_id, Some(second.id));
    assert_eq!(moved.name, "Moved");
    assert_eq!(moved.revision, 2);
    assert_eq!(
        nodes::update(
            h.state(),
            h.owner_context(),
            child.id,
            child.revision,
            input(Some(first.id), NodeKind::Folder, "Stale"),
        )
        .await
        .err(),
        Some(AuthError::Conflict)
    );
    assert_eq!(
        nodes::update(
            h.state(),
            h.owner_context(),
            folder.id,
            folder.revision,
            input(Some(folder.id), NodeKind::Folder, "Self"),
        )
        .await
        .err(),
        Some(AuthError::Invalid)
    );
    assert_eq!(
        nodes::update(
            h.state(),
            h.other_org_context(),
            folder.id,
            folder.revision,
            input(Some(second.id), NodeKind::Folder, "Foreign"),
        )
        .await
        .err(),
        Some(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn cursor_pages_are_bounded_complete_and_tenant_scoped() {
    let h = OrgHarness::new().await;
    seed_projects(&h, 200).await;
    let (complete, next) = nodes::list_page(h.state(), h.owner_context(), None)
        .await
        .unwrap();
    assert_eq!(complete.len(), 200);
    assert_eq!(next, None, "an exact final page must not invent a cursor");

    let extra = nodes::create(
        h.state(),
        h.owner_context(),
        input(None, NodeKind::Project, "Page two"),
    )
    .await
    .unwrap();
    let (first, next) = nodes::list_page(h.state(), h.owner_context(), None)
        .await
        .unwrap();
    assert_eq!(first.len(), 200);
    let next = next.expect("201 rows require another page");
    assert_eq!(next, first.last().map(|node| node.id).unwrap());
    let (last, done) = nodes::list_page(h.state(), h.owner_context(), Some(next))
        .await
        .unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].id, extra.id);
    assert_eq!(done, None);
    let (empty, done) = nodes::list_page(h.state(), h.owner_context(), Some(extra.id))
        .await
        .unwrap();
    assert!(empty.is_empty());
    assert_eq!(done, None);

    let foreign = nodes::create(
        h.state(),
        h.other_org_context(),
        input(None, NodeKind::Project, "Foreign cursor"),
    )
    .await
    .unwrap();
    assert_eq!(
        nodes::list(h.state(), h.owner_context(), Some(foreign.id))
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
    assert_eq!(
        nodes::list(h.state(), h.owner_context(), Some(i32::MAX))
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
}

#[tokio::test]
async fn organization_node_cap_is_exact_and_serialized_without_admission_quota() {
    let h = OrgHarness::new().await;
    seed_projects(&h, 1000).await;
    assert_eq!(
        nodes::create(
            h.state(),
            h.owner_context(),
            input(None, NodeKind::Project, "One too many"),
        )
        .await
        .err(),
        Some(AuthError::Limited)
    );
    assert_eq!(
        count_matching(
            &h.database,
            &format!(
                "SELECT COUNT(*) AS n FROM neev_nodes WHERE org_id={}",
                h.org_id()
            ),
        )
        .await,
        1000
    );
    assert_eq!(
        test_support::count(&h.database, "neev_quota_buckets").await,
        0
    );
}

#[tokio::test]
async fn removed_member_cannot_complete_a_later_hierarchy_write() {
    let h = OrgHarness::new().await;
    let member = h.member_context().clone();
    org::remove(h.state(), h.owner_context(), member.member_id)
        .await
        .unwrap();
    assert_eq!(
        nodes::create(h.state(), &member, input(None, NodeKind::Project, "Denied"),)
            .await
            .err(),
        Some(AuthError::Forbidden)
    );
    assert_eq!(test_support::count(&h.database, "neev_nodes").await, 0);
}

#[tokio::test]
#[ignore = "requires explicitly isolated NEEV_TEST_DATABASE_URL"]
async fn postgres_member_removal_serializes_before_catalog_final_write() {
    let (database, admin, schema) = test_support::postgres_database().await;
    let clock = Arc::new(test_support::TestClock::new(1_700_000_000));
    let state = Arc::new(
        ServerState::new(database.clone(), test_support::preview_config(), clock).unwrap(),
    );
    let owner_user =
        test_support::native_account(&database, "node-race-owner", "owner@race.test").await;
    let member_user =
        test_support::native_account(&database, "node-race-member", "member@race.test").await;
    let (org_id, _) = test_support::organization(&database, owner_user, "admin").await;
    let member_id = test_support::add_membership(&database, org_id, member_user, "member").await;
    let owner = test_support::scoped_context(&database, owner_user, org_id, 1_700_000_000).await;
    let member = test_support::scoped_context(&database, member_user, org_id, 1_700_000_000).await;

    let org_blocker = lock_postgres_organization(&database, org_id).await;
    let remove_state = state.clone();
    let removal = tokio::spawn(async move { org::remove(&remove_state, &owner, member_id).await });
    test_support::wait_for_postgres_lock(&admin, "%neev_organizations%FOR UPDATE%").await;
    let create_state = state.clone();
    let creation = tokio::spawn(async move {
        nodes::create(
            &create_state,
            &member,
            input(None, NodeKind::Project, "Cannot commit"),
        )
        .await
    });
    test_support::wait_for_postgres_lock(&admin, "%neev_accounts%FOR UPDATE%").await;
    org_blocker.commit().await.unwrap();

    assert_eq!(removal.await.unwrap(), Ok(()));
    assert_eq!(creation.await.unwrap().err(), Some(AuthError::Forbidden));
    let count = database
        .query_one(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT COUNT(*) AS n FROM neev_nodes WHERE org_id=$1",
            vec![org_id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(count, 0);
    test_support::cleanup_postgres(database, admin, schema).await;
}

async fn seed_projects(h: &OrgHarness, count: i32) {
    for index in 0..count {
        test_support::execute(
            &h.database,
            "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project',$2,1,$3)",
            vec![
                h.org_id().into(),
                format!("Seed {index}").into(),
                h.owner_context().identity.user_id.into(),
            ],
        )
        .await;
    }
}

async fn count_matching(database: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    database
        .query_one(Statement::from_string(
            database.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}

async fn lock_postgres_organization(
    database: &sea_orm::DatabaseConnection,
    org_id: i32,
) -> sea_orm::DatabaseTransaction {
    let tx = crate::neev::db::begin(database).await.unwrap();
    tx.query_one(Statement::from_sql_and_values(
        tx.get_database_backend(),
        "SELECT id FROM neev_organizations WHERE id=$1 FOR UPDATE",
        vec![org_id.into()],
    ))
    .await
    .unwrap()
    .unwrap();
    tx
}
