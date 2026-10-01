//! Folder sharing defaults: new resources inherit the nearest explicit default,
//! explicit private stays private, defaults never rewrite existing items, and
//! only a node's creator or an org admin may change one.

use super::{
    access, resources,
    sharing::{self, DefaultVisibility, NodeSharing},
    test_support::{patch, request, Harness},
    types::*,
};
use crate::neev::{
    catalog::nodes,
    db,
    error::AuthError,
    org,
    types::{NodeInput, NodeKind, OrgContext},
};

async fn folder(h: &Harness, actor: &OrgContext, parent: i32, name: &str) -> i32 {
    nodes::create(
        &h.state,
        actor,
        NodeInput {
            parent_id: Some(parent),
            kind: NodeKind::Folder,
            name: name.into(),
        },
    )
    .await
    .unwrap()
    .id
}

async fn set(h: &Harness, actor: &OrgContext, node: i32, value: DefaultVisibility) -> NodeSharing {
    let current = sharing::get(&h.state, actor, node).await.unwrap();
    sharing::set(&h.state, actor, node, current.revision, value)
        .await
        .unwrap()
}

fn input(node: i32, kind: ResourceKind, title: &str) -> ResourceInput {
    ResourceInput {
        node_id: node,
        kind,
        title: title.into(),
        origin_chat_id: None,
        request_id: request(),
    }
}

async fn create(h: &Harness, node: i32, kind: ResourceKind, explicit: Option<Visibility>) -> Resource {
    resources::create_with_visibility(&h.state, &h.creator, input(node, kind, "Item"), explicit)
        .await
        .unwrap()
}

/// Every resource row's audience-bearing columns, for "nothing was rewritten".
async fn audience_rows(h: &Harness) -> Vec<(String, String, i64, i64)> {
    let rows = sea_orm::ConnectionTrait::query_all(
        &h.state.database,
        db::statement(
            &h.state.database,
            "SELECT id,visibility,auth_revision,updated_at FROM neev_resources ORDER BY id",
            vec![],
        ),
    )
    .await
    .unwrap();
    rows.iter()
        .map(|row| {
            (
                row.try_get("", "id").unwrap(),
                row.try_get("", "visibility").unwrap(),
                row.try_get("", "auth_revision").unwrap(),
                row.try_get("", "updated_at").unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn without_any_default_new_items_stay_private_as_before() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let child = folder(&h, &h.creator, h.node, "Child").await;
    let view = sharing::get(&h.state, &h.admin, child).await.unwrap();
    assert_eq!(view.default_visibility, DefaultVisibility::Inherit);
    assert_eq!(view.effective_visibility, Visibility::Private);
    assert_eq!(view.inherited_from, None);
    for kind in [ResourceKind::Chat, ResourceKind::Document] {
        let made = resources::create(&h.state, &h.creator, input(child, kind, "Plain"))
            .await
            .unwrap();
        assert_eq!(made.visibility, Visibility::Private);
        // Invisible to another member, exactly the schema-20 behaviour.
        assert_eq!(
            resources::get(&h.state, &h.admin, &made.id).await,
            Err(ContentError::NotFound)
        );
    }
}

#[tokio::test]
async fn new_items_inherit_the_nearest_explicit_default_through_subfolders() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let child = folder(&h, &h.creator, h.node, "Child").await;
    let grandchild = folder(&h, &h.creator, child, "Grandchild").await;
    let shared = set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    assert_eq!(shared.default_visibility, DefaultVisibility::Organization);
    assert_eq!(shared.effective_visibility, Visibility::Organization);
    assert_eq!(shared.inherited_from, None);

    let view = sharing::get(&h.state, &h.admin, grandchild).await.unwrap();
    assert_eq!(view.default_visibility, DefaultVisibility::Inherit);
    assert_eq!(view.effective_visibility, Visibility::Organization);
    assert_eq!(view.inherited_from, Some(h.node));

    for (node, kind) in [
        (h.node, ResourceKind::Document),
        (child, ResourceKind::Chat),
        (grandchild, ResourceKind::Document),
        (grandchild, ResourceKind::Chat),
    ] {
        let made = create(&h, node, kind, None).await;
        assert_eq!(made.visibility, Visibility::Organization, "{node} {kind:?}");
        // Materialized: another member reads it through the ordinary access path.
        assert_eq!(
            resources::get(&h.state, &h.admin, &made.id).await.unwrap().visibility,
            Visibility::Organization
        );
    }
    // A different project is untouched by this one's default.
    assert_eq!(
        create(&h, h.second_node, ResourceKind::Chat, None).await.visibility,
        Visibility::Private
    );
}

#[tokio::test]
async fn a_nearer_explicit_default_overrides_its_ancestors() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let child = folder(&h, &h.creator, h.node, "Private child").await;
    let grandchild = folder(&h, &h.creator, child, "Below").await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    set(&h, &h.creator, child, DefaultVisibility::Private).await;
    let view = sharing::get(&h.state, &h.creator, grandchild).await.unwrap();
    assert_eq!(view.effective_visibility, Visibility::Private);
    assert_eq!(view.inherited_from, Some(child));
    assert_eq!(create(&h, grandchild, ResourceKind::Chat, None).await.visibility, Visibility::Private);
    assert_eq!(create(&h, child, ResourceKind::Document, None).await.visibility, Visibility::Private);
    assert_eq!(create(&h, h.node, ResourceKind::Document, None).await.visibility, Visibility::Organization);
    // Back to inherit: the child follows the project again.
    let child_view = set(&h, &h.creator, child, DefaultVisibility::Inherit).await;
    assert_eq!(child_view.default_visibility, DefaultVisibility::Inherit);
    assert_eq!(child_view.effective_visibility, Visibility::Organization);
    assert_eq!(child_view.inherited_from, Some(h.node));
    assert_eq!(create(&h, grandchild, ResourceKind::Chat, None).await.visibility, Visibility::Organization);
}

#[tokio::test]
async fn explicit_private_is_an_exception_that_survives_later_default_changes() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    let secret = create(&h, h.node, ResourceKind::Document, Some(Visibility::Private)).await;
    let inherited = create(&h, h.node, ResourceKind::Chat, None).await;
    assert_eq!(secret.visibility, Visibility::Private);
    assert_eq!(inherited.visibility, Visibility::Organization);
    // An admin cannot see another member's private item, default or not.
    assert_eq!(resources::get(&h.state, &h.admin, &secret.id).await, Err(ContentError::NotFound));
    // Explicit organization inside a private folder is the opposite exception.
    set(&h, &h.creator, h.second_node, DefaultVisibility::Private).await;
    let opened = create(&h, h.second_node, ResourceKind::Chat, Some(Visibility::Organization)).await;
    assert_eq!(opened.visibility, Visibility::Organization);

    let before = audience_rows(&h).await;
    for value in [
        DefaultVisibility::Private,
        DefaultVisibility::Organization,
        DefaultVisibility::Inherit,
        DefaultVisibility::Organization,
    ] {
        set(&h, &h.creator, h.node, value).await;
        set(&h, &h.admin, h.second_node, value).await;
    }
    // No resource row changed: not the exception, not the inherited item.
    assert_eq!(audience_rows(&h).await, before);
    assert_eq!(resources::get(&h.state, &h.admin, &secret.id).await, Err(ContentError::NotFound));
    assert_eq!(
        resources::get(&h.state, &h.creator, &secret.id).await.unwrap().visibility,
        Visibility::Private
    );
    // The per-item control is still the way to change an existing item.
    let mut p = patch(&inherited);
    p.visibility = Visibility::Private;
    assert_eq!(
        resources::update(&h.state, &h.creator, &inherited.id, p).await.unwrap().visibility,
        Visibility::Private
    );
}

#[tokio::test]
async fn moving_an_existing_item_into_a_shared_folder_keeps_its_audience() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let private = create(&h, h.second_node, ResourceKind::Document, None).await;
    assert_eq!(private.visibility, Visibility::Private);
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    let mut p = patch(&private);
    p.node_id = h.node;
    let moved = resources::update(&h.state, &h.creator, &private.id, p).await.unwrap();
    assert_eq!((moved.node_id, moved.visibility), (h.node, Visibility::Private));
    assert_eq!(resources::get(&h.state, &h.admin, &private.id).await, Err(ContentError::NotFound));
}

#[tokio::test]
async fn attached_documents_follow_their_chat_not_the_folder() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    let private_chat = create(&h, h.node, ResourceKind::Chat, Some(Visibility::Private)).await;
    let attached = resources::create(
        &h.state,
        &h.creator,
        ResourceInput {
            origin_chat_id: Some(private_chat.id.clone()),
            ..input(h.node, ResourceKind::Document, "Generated")
        },
    )
    .await
    .unwrap();
    assert_eq!(attached.visibility, Visibility::Private);
    assert_eq!(resources::get(&h.state, &h.admin, &attached.id).await, Err(ContentError::NotFound));

    // The other way round: a shared chat in a private folder shares its output.
    set(&h, &h.creator, h.second_node, DefaultVisibility::Private).await;
    let shared_chat = create(&h, h.second_node, ResourceKind::Chat, Some(Visibility::Organization)).await;
    let output = resources::create(
        &h.state,
        &h.creator,
        ResourceInput {
            origin_chat_id: Some(shared_chat.id.clone()),
            ..input(h.second_node, ResourceKind::Document, "Output")
        },
    )
    .await
    .unwrap();
    assert_eq!(output.visibility, Visibility::Organization);

    // An explicit audience on a bound document is refused, not silently ignored.
    assert_eq!(
        resources::create_with_visibility(
            &h.state,
            &h.creator,
            ResourceInput {
                origin_chat_id: Some(shared_chat.id.clone()),
                ..input(h.second_node, ResourceKind::Document, "Override")
            },
            Some(Visibility::Private),
        )
        .await,
        Err(ContentError::Invalid)
    );
}

#[tokio::test]
async fn retries_replay_the_original_resource_after_the_default_changes() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let original_input = input(h.node, ResourceKind::Chat, "Retry me");
    let first = resources::create(&h.state, &h.creator, original_input.clone()).await.unwrap();
    assert_eq!(first.visibility, Visibility::Private);
    let chats = h.usage("chats").await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    let replay = resources::create(&h.state, &h.creator, original_input.clone()).await.unwrap();
    assert_eq!(replay, first, "a retry must not change the audience or make a second chat");
    assert_eq!(h.usage("chats").await, chats);

    // Omitted intent keeps the schema-20 request hash, so receipts written by
    // the previous release still replay after the upgrade.
    let stored: String = db::one(
        &h.state.database,
        "SELECT body_hash FROM neev_requests WHERE resource_id=$1",
        vec![first.id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "body_hash")
    .unwrap();
    assert_eq!(
        stored,
        org::digest_parts(&[
            "content.resource.create",
            &h.creator.org_id.to_string(),
            &h.node.to_string(),
            access::kind_name(ResourceKind::Chat),
            "Retry me",
            "",
            "native",
        ])
    );

    // An explicit audience is part of the request identity.
    let explicit_input = input(h.node, ResourceKind::Document, "Explicit");
    let explicit = resources::create_with_visibility(
        &h.state,
        &h.creator,
        explicit_input.clone(),
        Some(Visibility::Private),
    )
    .await
    .unwrap();
    assert_eq!(
        resources::create_with_visibility(&h.state, &h.creator, explicit_input.clone(), Some(Visibility::Private))
            .await
            .unwrap(),
        explicit
    );
    for other in [Some(Visibility::Organization), None] {
        assert_eq!(
            resources::create_with_visibility(&h.state, &h.creator, explicit_input.clone(), other).await,
            Err(ContentError::Conflict)
        );
    }
    assert_eq!(
        resources::create_with_visibility(&h.state, &h.creator, original_input, Some(Visibility::Private)).await,
        Err(ContentError::Conflict)
    );
}

#[tokio::test]
async fn only_the_creator_or_an_admin_may_change_a_default() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    // The admin creates a project; the creator is a plain member there.
    let admins = nodes::create(
        &h.state,
        &h.admin,
        NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Admin project".into(),
        },
    )
    .await
    .unwrap();
    let member_view = sharing::get(&h.state, &h.creator, admins.id).await.unwrap();
    assert!(!member_view.can_manage);
    assert_eq!(
        sharing::set(&h.state, &h.creator, admins.id, member_view.revision, DefaultVisibility::Organization).await,
        Err(ContentError::Auth(AuthError::Forbidden))
    );
    // Nothing changed, not even the revision.
    assert_eq!(sharing::get(&h.state, &h.creator, admins.id).await.unwrap(), member_view);
    // A member's own subfolder inside the admin's project is theirs to manage.
    let own = folder(&h, &h.creator, admins.id, "Mine").await;
    assert!(sharing::get(&h.state, &h.creator, own).await.unwrap().can_manage);
    set(&h, &h.creator, own, DefaultVisibility::Organization).await;
    // An admin manages every node in the org, including a member's.
    assert!(sharing::get(&h.state, &h.admin, h.node).await.unwrap().can_manage);
    assert_eq!(
        set(&h, &h.admin, h.node, DefaultVisibility::Organization).await.effective_visibility,
        Visibility::Organization
    );
    // The creator of the node and the admin both see the admin-set default.
    assert_eq!(
        sharing::get(&h.state, &h.creator, h.node).await.unwrap().default_visibility,
        DefaultVisibility::Organization
    );
}

#[tokio::test]
async fn other_tenants_can_neither_read_nor_change_a_default() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    assert_eq!(sharing::get(&h.state, &h.foreign, h.node).await, Err(ContentError::NotFound));
    assert_eq!(
        sharing::set(&h.state, &h.foreign, h.node, 2, DefaultVisibility::Private).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(sharing::get(&h.state, &h.creator, h.foreign_node).await, Err(ContentError::NotFound));
    assert_eq!(sharing::get(&h.state, &h.creator, 0).await, Err(ContentError::NotFound));
    assert_eq!(sharing::get(&h.state, &h.creator, 999_999).await, Err(ContentError::NotFound));
    // A foreign creation cannot resolve our default either: the node is not theirs.
    assert_eq!(
        resources::create(&h.state, &h.foreign, input(h.node, ResourceKind::Chat, "x")).await,
        Err(ContentError::NotFound)
    );
    // Their own default does not leak into ours or vice versa.
    let theirs = sharing::get(&h.state, &h.foreign, h.foreign_node).await.unwrap();
    assert_eq!(theirs.effective_visibility, Visibility::Private);
    assert!(theirs.can_manage);
}

#[tokio::test]
async fn revisions_are_optimistic_and_shared_with_rename() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let before = sharing::get(&h.state, &h.creator, h.node).await.unwrap();
    // Stale revision: conflict, nothing written.
    assert_eq!(
        sharing::set(&h.state, &h.creator, h.node, before.revision + 1, DefaultVisibility::Organization).await,
        Err(ContentError::Conflict)
    );
    assert_eq!(sharing::set(&h.state, &h.creator, h.node, 0, DefaultVisibility::Organization).await, Err(ContentError::Invalid));
    assert_eq!(sharing::get(&h.state, &h.creator, h.node).await.unwrap(), before);
    let changed = sharing::set(&h.state, &h.creator, h.node, before.revision, DefaultVisibility::Organization)
        .await
        .unwrap();
    assert_eq!(changed.revision, before.revision + 1);
    // Re-sending the same value is a no-op: no bump.
    let same = sharing::set(&h.state, &h.creator, h.node, changed.revision, DefaultVisibility::Organization)
        .await
        .unwrap();
    assert_eq!(same, changed);
    // The bump is the node revision: a rename with the pre-change revision conflicts.
    let rename = |revision| {
        nodes::update(
            &h.state,
            &h.creator,
            h.node,
            revision,
            NodeInput {
                parent_id: None,
                kind: NodeKind::Project,
                name: "Renamed".into(),
            },
        )
    };
    assert!(matches!(rename(before.revision).await, Err(AuthError::Conflict)));
    let renamed = rename(changed.revision).await.unwrap();
    // And a rename invalidates a sharing write based on the old revision.
    assert_eq!(
        sharing::set(&h.state, &h.creator, h.node, changed.revision, DefaultVisibility::Private).await,
        Err(ContentError::Conflict)
    );
    let after = sharing::get(&h.state, &h.creator, h.node).await.unwrap();
    assert_eq!(after.revision, renamed.revision);
    assert_eq!(after.default_visibility, DefaultVisibility::Organization);
    // The audit columns record who set it.
    let row = db::one(
        &h.state.database,
        "SELECT set_by,default_visibility FROM neev_node_sharing_defaults WHERE org_id=$1 AND node_id=$2",
        vec![h.creator.org_id.into(), h.node.into()],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(row.try_get::<i32>("", "set_by").unwrap(), h.creator.identity.user_id);
    assert_eq!(row.try_get::<String>("", "default_visibility").unwrap(), "organization");
}

#[tokio::test]
async fn a_removed_member_can_no_longer_read_or_change_defaults() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    let view = sharing::get(&h.state, &h.creator, h.node).await.unwrap();
    crate::neev::test_support::execute(
        &h.state.database,
        "UPDATE neev_memberships SET active=false,revision=revision+1 WHERE org_id=$1 AND user_id=$2",
        vec![h.creator.org_id.into(), h.creator.identity.user_id.into()],
    )
    .await;
    assert!(matches!(
        sharing::get(&h.state, &h.creator, h.node).await,
        Err(ContentError::Auth(_))
    ));
    assert!(matches!(
        sharing::set(&h.state, &h.creator, h.node, view.revision, DefaultVisibility::Organization).await,
        Err(ContentError::Auth(_))
    ));
}

/// The same contract on PostgreSQL, where `set` row-locks the node (FOR UPDATE
/// beside a scalar subquery) and the upsert/delete run under real concurrency
/// control.
#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_defaults_inherit_materialize_and_guard_authority() {
    let (database, admin_db, schema) = crate::neev::test_support::postgres_database().await;
    {
        let h = Harness::new(database.clone()).await;
        let child = folder(&h, &h.creator, h.node, "Child").await;
        let shared = set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
        assert_eq!(
            sharing::set(&h.state, &h.creator, h.node, shared.revision - 1, DefaultVisibility::Private).await,
            Err(ContentError::Conflict)
        );
        let view = sharing::get(&h.state, &h.admin, child).await.unwrap();
        assert_eq!((view.effective_visibility, view.inherited_from), (Visibility::Organization, Some(h.node)));
        let retry = input(child, ResourceKind::Chat, "Inherited");
        let inherited = resources::create(&h.state, &h.creator, retry.clone()).await.unwrap();
        assert_eq!(inherited.visibility, Visibility::Organization);
        let secret = create(&h, child, ResourceKind::Document, Some(Visibility::Private)).await;
        assert_eq!(secret.visibility, Visibility::Private);
        let before = audience_rows(&h).await;
        set(&h, &h.admin, h.node, DefaultVisibility::Private).await;
        set(&h, &h.creator, child, DefaultVisibility::Organization).await;
        set(&h, &h.creator, child, DefaultVisibility::Inherit).await;
        assert_eq!(audience_rows(&h).await, before);
        assert_eq!(resources::create(&h.state, &h.creator, retry).await.unwrap(), inherited);
        assert_eq!(create(&h, child, ResourceKind::Chat, None).await.visibility, Visibility::Private);
        let admins = nodes::create(
            &h.state,
            &h.admin,
            NodeInput { parent_id: None, kind: NodeKind::Project, name: "Admin".into() },
        )
        .await
        .unwrap();
        assert_eq!(
            sharing::set(&h.state, &h.creator, admins.id, admins.revision, DefaultVisibility::Organization).await,
            Err(ContentError::Auth(AuthError::Forbidden))
        );
        assert_eq!(sharing::get(&h.state, &h.foreign, h.node).await, Err(ContentError::NotFound));
    }
    crate::neev::test_support::cleanup_postgres(database, admin_db, schema).await;
}

#[tokio::test]
async fn a_legacy_working_directory_ancestor_stops_inheritance_safely() {
    let h = Harness::new(crate::neev::test_support::database().await).await;
    set(&h, &h.creator, h.node, DefaultVisibility::Organization).await;
    // Raw rows: the API no longer creates either shape.
    let directory: i32 = db::one(&h.state.database, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'working_directory','wd',1,$3) RETURNING id", vec![h.creator.org_id.into(), h.node.into(), h.creator.identity.user_id.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    let below: i32 = db::one(&h.state.database, "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,$2,'folder','legacy',1,$3) RETURNING id", vec![h.creator.org_id.into(), directory.into(), h.creator.identity.user_id.into()]).await.unwrap().unwrap().try_get("", "id").unwrap();
    let view = sharing::get(&h.state, &h.creator, below).await.unwrap();
    assert_eq!((view.effective_visibility, view.inherited_from), (Visibility::Private, None));
    assert_eq!(create(&h, below, ResourceKind::Chat, None).await.visibility, Visibility::Private);
    // The working directory itself is not a sharing target.
    assert_eq!(sharing::get(&h.state, &h.creator, directory).await, Err(ContentError::NotFound));
}
