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
pub(crate) mod engine;
pub(crate) mod error;
mod gateway;
pub(crate) mod http;
pub(crate) mod library;
#[cfg(feature = "dev-agent")]
pub mod dev_agent;
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

/// `neev-schema-downgrade --from 20 --to 19 --confirm`: the guarded rollback of
/// the provider-enum widening (see `db::provider_enum_migration::downgrade`).
/// Reads `DATABASE_URL` (never printed). Exit codes: 0 downgraded; 2 usage;
/// 3 not at schema 20 (nothing changed); 4 rows name an ACP provider (nothing
/// changed — keep schema 20); 1 database error (rolled back).
pub fn run_provider_schema_downgrade() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let step = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--from", "21", "--to", "20", "--confirm"] => 21,
        ["--from", "20", "--to", "19", "--confirm"] => 20,
        _ => {
            eprintln!(
                "usage: neev-schema-downgrade --from 21 --to 20 --confirm\n       neev-schema-downgrade --from 20 --to 19 --confirm"
            );
            return std::process::ExitCode::from(2);
        }
    };
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("neev-schema-downgrade: DATABASE_URL is not set");
        return std::process::ExitCode::from(2);
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return std::process::ExitCode::from(1);
    };
    runtime.block_on(async move {
        let mut options = sea_orm::ConnectOptions::new(url);
        options.max_connections(1).sqlx_logging(false);
        let Ok(database) = sea_orm::Database::connect(options).await else {
            eprintln!("neev-schema-downgrade: cannot connect");
            return std::process::ExitCode::from(1);
        };
        if step == 21 {
            return sharing_downgrade_exit(db::downgrade_node_sharing(&database).await);
        }
        use db::provider_enum_migration::Downgrade;
        match db::downgrade_provider_enum(&database).await {
            Ok(Downgrade::Done) => {
                println!("downgraded schema 20 -> 19; every row preserved");
                std::process::ExitCode::SUCCESS
            }
            Ok(Downgrade::NotAtTwenty(version)) => {
                println!("refused: schema is {version}, not 20; nothing changed");
                std::process::ExitCode::from(3)
            }
            Ok(Downgrade::AcpRowsExist { table, rows }) => {
                println!(
                    "refused: {rows} row(s) in {table} name an ACP provider; \
                     nothing changed — keep schema 20 (never delete or restore over them)"
                );
                std::process::ExitCode::from(4)
            }
            Err(_) => {
                eprintln!("neev-schema-downgrade: database error; rolled back");
                std::process::ExitCode::from(1)
            }
        }
    })
}

/// Exit codes mirror the 20 -> 19 step: 0 done, 3 wrong marker, 4 user data
/// present (refused; nothing changed), 1 database error (rolled back).
fn sharing_downgrade_exit(
    result: Result<db::node_sharing_migration::Downgrade, error::AuthError>,
) -> std::process::ExitCode {
    use db::node_sharing_migration::Downgrade;
    match result {
        Ok(Downgrade::Done) => {
            println!("downgraded schema 21 -> 20; no folder default existed, every row preserved");
            std::process::ExitCode::SUCCESS
        }
        Ok(Downgrade::NotAtTwentyOne(version)) => {
            println!("refused: schema is {version}, not 21; nothing changed");
            std::process::ExitCode::from(3)
        }
        Ok(Downgrade::DefaultsExist { rows }) => {
            println!(
                "refused: {rows} folder sharing default(s) are configured; nothing changed — \
                 keep schema 21 and repair forward (never delete them or restore over them)"
            );
            std::process::ExitCode::from(4)
        }
        Err(_) => {
            eprintln!("neev-schema-downgrade: database error; rolled back");
            std::process::ExitCode::from(1)
        }
    }
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
