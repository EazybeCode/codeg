use super::{resources, types::*};
use crate::neev::{
    catalog::nodes,
    db,
    startup::ServerState,
    test_support as h,
    types::{NodeInput, NodeKind, OrgContext},
};
use sea_orm::DatabaseConnection;
use std::sync::Arc;

pub(super) struct Harness {
    pub state: Arc<ServerState>,
    pub creator: OrgContext,
    pub admin: OrgContext,
    pub foreign: OrgContext,
    pub node: i32,
    pub second_node: i32,
    pub foreign_node: i32,
}
impl Harness {
    pub async fn new(database: DatabaseConnection) -> Self {
        let clock = Arc::new(h::TestClock::new(1_700_000_000));
        let mail =
            crate::neev::mail::Service::new(database.clone(), Arc::new(h::MailCapture::new()));
        let state = Arc::new(
            ServerState::with_mail(database.clone(), h::preview_config(), clock, mail).unwrap(),
        );
        let admin_id = h::native_account(&database, "content-admin", "admin@example.test").await;
        let creator_id =
            h::native_account(&database, "content-creator", "creator@example.test").await;
        let foreign_id =
            h::native_account(&database, "content-foreign", "foreign@example.test").await;
        let (org, _) = h::organization(&database, admin_id, "admin").await;
        h::add_membership(&database, org, creator_id, "member").await;
        let (other, _) = h::organization(&database, foreign_id, "admin").await;
        let admin = h::scoped_context(&database, admin_id, org, 1_700_000_000).await;
        let creator = h::scoped_context(&database, creator_id, org, 1_700_000_000).await;
        let foreign = h::scoped_context(&database, foreign_id, other, 1_700_000_000).await;
        let mut ids = Vec::new();
        for actor in [&creator, &creator, &foreign] {
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
            admin,
            foreign,
            node: ids[0],
            second_node: ids[1],
            foreign_node: ids[2],
        }
    }
    pub fn input(&self, kind: ResourceKind, title: &str) -> ResourceInput {
        ResourceInput {
            node_id: self.node,
            kind,
            title: title.into(),
            origin_chat_id: None,
            request_id: uuid::Uuid::new_v4().to_string(),
        }
    }
    pub async fn chat(&self) -> Resource {
        resources::create(
            &self.state,
            &self.creator,
            self.input(ResourceKind::Chat, "Private title"),
        )
        .await
        .unwrap()
    }
    pub async fn shared_chat(&self) -> Resource {
        let chat = self.chat().await;
        let mut p = patch(&chat);
        p.visibility = Visibility::Organization;
        resources::update(&self.state, &self.creator, &chat.id, p)
            .await
            .unwrap()
    }
    pub async fn usage(&self, column: &str) -> i64 {
        db::one(
            &self.state.database,
            &format!("SELECT {column} AS n FROM neev_content_usage WHERE org_id=$1"),
            vec![self.creator.org_id.into()],
        )
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
    }
}
pub(super) fn patch(r: &Resource) -> ResourcePatch {
    ResourcePatch {
        expected_auth_revision: r.auth_revision,
        title: r.title.clone(),
        node_id: r.node_id,
        visibility: r.visibility,
        archived: r.archived,
    }
}
pub(super) fn request() -> String {
    uuid::Uuid::new_v4().to_string()
}
