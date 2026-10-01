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
pub(crate) mod knowledge;
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
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    std::process::ExitCode::from(run_schema_downgrade_with(
        &args,
        std::env::var("DATABASE_URL").ok(),
    ))
}

/// The operator step the arguments name, exactly; anything else is a usage error.
fn downgrade_step(args: &[&str]) -> Option<i64> {
    match args {
        ["--from", "22", "--to", "21", "--confirm"] => Some(22),
        ["--from", "21", "--to", "20", "--confirm"] => Some(21),
        ["--from", "20", "--to", "19", "--confirm"] => Some(20),
        _ => None,
    }
}

/// The whole CLI minus process plumbing, so tests can drive it. Returns the
/// exit code: 0 done, 2 usage, 3 wrong marker, 4 user data present, 1 error.
fn run_schema_downgrade_with(args: &[&str], url: Option<String>) -> u8 {
    let Some(step) = downgrade_step(args) else {
        eprintln!(
            "usage: neev-schema-downgrade --from 22 --to 21 --confirm\n       neev-schema-downgrade --from 21 --to 20 --confirm\n       neev-schema-downgrade --from 20 --to 19 --confirm"
        );
        return 2;
    };
    let Some(url) = url else {
        eprintln!("neev-schema-downgrade: DATABASE_URL is not set");
        return 2;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return 1;
    };
    runtime.block_on(async move {
        let mut options = sea_orm::ConnectOptions::new(url);
        options.max_connections(1).sqlx_logging(false);
        let Ok(database) = sea_orm::Database::connect(options).await else {
            eprintln!("neev-schema-downgrade: cannot connect");
            return 1;
        };
        if step == 22 {
            return knowledge_downgrade_exit(db::downgrade_knowledge(&database).await);
        }
        if step == 21 {
            return sharing_downgrade_exit(db::downgrade_node_sharing(&database).await);
        }
        use db::provider_enum_migration::Downgrade;
        match db::downgrade_provider_enum(&database).await {
            Ok(Downgrade::Done) => {
                println!("downgraded schema 20 -> 19; every row preserved");
                0
            }
            Ok(Downgrade::NotAtTwenty(version)) => {
                println!("refused: schema is {version}, not 20; nothing changed");
                3
            }
            Ok(Downgrade::AcpRowsExist { table, rows }) => {
                println!(
                    "refused: {rows} row(s) in {table} name an ACP provider; \
                     nothing changed — keep schema 20 (never delete or restore over them)"
                );
                4
            }
            Err(_) => {
                eprintln!("neev-schema-downgrade: database error; rolled back");
                1
            }
        }
    })
}

/// 22 -> 21 (org knowledge). Same codes as the other steps: 0 done, 3 wrong
/// marker, 4 knowledge rows exist (refused; nothing changed — keep 22 and
/// repair forward), 1 database error or unknown shape (rolled back).
fn knowledge_downgrade_exit(
    result: Result<db::knowledge_migration::Downgrade, error::AuthError>,
) -> u8 {
    use db::knowledge_migration::Downgrade;
    match result {
        Ok(Downgrade::Downgraded) => {
            println!("downgraded schema 22 -> 21; no knowledge data existed, every row preserved");
            0
        }
        Ok(Downgrade::NotAtTwentyTwo(version)) => {
            println!("refused: schema is {version}, not 22; nothing changed");
            3
        }
        Ok(Downgrade::Refused) => {
            println!(
                "refused: org knowledge data exists; nothing changed — keep schema 22 and \
                 repair forward (never delete it or restore over it)"
            );
            4
        }
        Err(_) => {
            eprintln!("neev-schema-downgrade: database error or unknown schema shape; rolled back");
            1
        }
    }
}

/// Exit codes mirror the 20 -> 19 step: 0 done, 3 wrong marker, 4 user data
/// present (refused; nothing changed), 1 database error (rolled back).
fn sharing_downgrade_exit(
    result: Result<db::node_sharing_migration::Downgrade, error::AuthError>,
) -> u8 {
    use db::node_sharing_migration::Downgrade;
    match result {
        Ok(Downgrade::Done) => {
            println!("downgraded schema 21 -> 20; no folder default existed, every row preserved");
            0
        }
        Ok(Downgrade::NotAtTwentyOne(version)) => {
            println!("refused: schema is {version}, not 21; nothing changed");
            3
        }
        Ok(Downgrade::DefaultsExist { rows }) => {
            println!(
                "refused: {rows} folder sharing default(s) are configured; nothing changed — \
                 keep schema 21 and repair forward (never delete them or restore over them)"
            );
            4
        }
        Err(_) => {
            eprintln!("neev-schema-downgrade: database error; rolled back");
            1
        }
    }
}

#[cfg(test)]
mod downgrade_cli_tests {
    use super::{db, downgrade_step, run_schema_downgrade_with, test_support as h};

    #[test]
    fn only_the_exact_operator_steps_are_accepted() {
        assert_eq!(downgrade_step(&["--from", "22", "--to", "21", "--confirm"]), Some(22));
        assert_eq!(downgrade_step(&["--from", "21", "--to", "20", "--confirm"]), Some(21));
        assert_eq!(downgrade_step(&["--from", "20", "--to", "19", "--confirm"]), Some(20));
        for bad in [
            &["--from", "22", "--to", "21"][..],
            &["--from", "22", "--to", "20", "--confirm"][..],
            &["--to", "21", "--from", "22", "--confirm"][..],
            &[][..],
        ] {
            assert_eq!(downgrade_step(bad), None, "{bad:?}");
            assert_eq!(run_schema_downgrade_with(bad, Some("sqlite::memory:".into())), 2);
        }
        assert_eq!(run_schema_downgrade_with(&["--from", "22", "--to", "21", "--confirm"], None), 2);
    }

    /// The real CLI path against a real (file) database: 0, then 3 on a rerun;
    /// 4 while a knowledge row exists, changing nothing.
    #[test]
    fn twenty_two_to_twenty_one_runs_through_the_cli() {
        let step = ["--from", "22", "--to", "21", "--confirm"];
        let dir = tempfile::tempdir().unwrap();
        let open = |name: &str| {
            let url = format!("sqlite://{}?mode=rwc", dir.path().join(name).display());
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let database = runtime.block_on(async {
                let mut options = sea_orm::ConnectOptions::new(url.clone());
                options.max_connections(1).sqlx_logging(false);
                let database = sea_orm::Database::connect(options).await.unwrap();
                db::initialize(&database).await.unwrap();
                database
            });
            (url, runtime, database)
        };
        let version = |runtime: &tokio::runtime::Runtime, database: &sea_orm::DatabaseConnection| -> i64 {
            runtime.block_on(async {
                db::one(database, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
                    .await
                    .unwrap()
                    .unwrap()
                    .try_get("", "version")
                    .unwrap()
            })
        };

        let (url, runtime, database) = open("empty.db");
        runtime.block_on(database.clone().close()).unwrap();
        assert_eq!(run_schema_downgrade_with(&step, Some(url.clone())), 0);
        assert_eq!(run_schema_downgrade_with(&step, Some(url.clone())), 3);
        let (_, runtime, database) = open("empty.db");
        assert_eq!(version(&runtime, &database), 22, "initialize re-migrates the downgraded file");

        let (url, runtime, database) = open("data.db");
        runtime.block_on(async {
            let user = h::native_account(&database, "cli-owner", "cli@example.test").await;
            let (org, _) = h::organization(&database, user, "admin").await;
            db::execute(&database, "INSERT INTO neev_knowledge_settings(org_id,capture_enabled,capture_blocked,revision,updated_by,updated_at) VALUES($1,false,NULL,1,$2,1)", vec![org.into(), user.into()]).await.unwrap();
            database.clone().close().await.unwrap();
        });
        assert_eq!(run_schema_downgrade_with(&step, Some(url.clone())), 4);
        let (_, runtime, database) = open("data.db");
        assert_eq!(version(&runtime, &database), 22);
        let rows: i64 = runtime.block_on(async {
            db::one(&database, "SELECT COUNT(*) AS n FROM neev_knowledge_settings", vec![]).await.unwrap().unwrap().try_get("", "n").unwrap()
        });
        assert_eq!(rows, 1);
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
