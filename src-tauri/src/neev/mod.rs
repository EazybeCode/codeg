//! Native Neev identity and tenancy. No legacy router or database fallback.
pub(crate) mod agents;
pub(crate) mod auth;
#[cfg(test)]
mod browser_fixture;
pub(crate) mod catalog;
pub(crate) mod clock;
pub(crate) mod config;
pub(crate) mod content;
pub(crate) mod db;
pub(crate) mod error;
pub(crate) mod http;
pub(crate) mod mail;
pub(crate) mod org;
mod startup;
#[cfg(test)]
mod test_support;
pub(crate) mod types;

pub fn run_server() -> std::process::ExitCode {
    startup::run_server()
}

pub fn run_content_verifier() -> std::process::ExitCode {
    content::backup::run_content_verifier()
}

#[cfg(test)]
mod contract_tests {
    use super::{
        clock::{Clock, SystemClock},
        db::entities::Account,
        types::{
            IdentityContext, MemberId, Node, NodeId, NodeInput, NodeKind, OrgContext, OrgId,
            Organization, Role, SessionSecrets, UserId,
        },
    };

    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now_unix(&self) -> i64 {
            self.0
        }
    }

    #[test]
    fn shared_native_contracts_have_the_required_shape() {
        let user_id: UserId = 1;
        let org_id: OrgId = 2;
        let member_id: MemberId = 3;
        let node_id: NodeId = 4;
        let identity = IdentityContext {
            user_id,
            session_id: "session".into(),
            auth_revision: 5,
            authenticated_at: 6,
        };
        let org = OrgContext {
            identity,
            org_id,
            member_id,
            member_revision: 7,
            org_revision: 8,
            role: Role::Admin,
        };
        let secrets = SessionSecrets {
            session: "session".into(),
            csrf: "csrf".into(),
            expires_at: 9,
        };
        let organization = Organization {
            id: org_id,
            name: "Neev".into(),
            owner_user_id: user_id,
            revision: 1,
        };
        let input = NodeInput {
            parent_id: None,
            kind: NodeKind::Project,
            name: "Project".into(),
        };
        let node = Node {
            id: node_id,
            org_id,
            parent_id: input.parent_id,
            kind: input.kind,
            name: input.name,
            revision: 1,
        };
        let account = Account {
            id: user_id,
            auth_revision: 1,
            disabled: false,
        };

        assert_eq!(FixedClock(10).now_unix(), 10);
        assert!(SystemClock.now_unix() > 0);
        assert_eq!(org.identity.user_id, user_id);
        assert_eq!(org.identity.session_id, "session");
        assert_eq!(org.identity.auth_revision, 5);
        assert_eq!(org.identity.authenticated_at, 6);
        assert_eq!(org.org_id, org_id);
        assert_eq!(org.member_id, member_id);
        assert_eq!(org.member_revision, 7);
        assert_eq!(org.org_revision, 8);
        assert!(matches!(org.role, Role::Admin));
        assert_eq!(secrets.session, "session");
        assert_eq!(secrets.csrf, "csrf");
        assert_eq!(secrets.expires_at, 9);
        assert_eq!(organization.id, org_id);
        assert_eq!(organization.name, "Neev");
        assert_eq!(organization.owner_user_id, user_id);
        assert_eq!(organization.revision, 1);
        assert_eq!(node.id, node_id);
        assert_eq!(node.org_id, org_id);
        assert_eq!(node.parent_id, None);
        assert!(matches!(node.kind, NodeKind::Project));
        assert_eq!(node.name, "Project");
        assert_eq!(node.revision, 1);
        assert_eq!(account.id, user_id);
        assert_eq!(account.auth_revision, 1);
        assert!(!account.disabled);

        let _ = Role::Member;
        let _ = NodeKind::Folder;
        let _ = NodeKind::WorkingDirectory;
    }
}

#[cfg(test)]
mod task2_boundary_tests {
    use super::test_support;

    #[tokio::test]
    async fn legacy_cookie_cannot_authenticate_native_routes() {
        let server = test_support::http_server().await;
        server
            .get("/api/neev/session")
            .add_header("cookie", "workos_team_session=old; workos_session=old")
            .await
            .assert_status_unauthorized();
        server.get("/api/settings").await.assert_status_not_found();
        server
            .get("/api/team/session")
            .await
            .assert_status_not_found();
    }
}
