use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    Set, Statement,
};
use sea_orm_migration::MigratorTrait;
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

use super::{
    core::TeamClock,
    github::{
        GithubEvidenceError, GithubIdentity, GithubMembershipEvidence,
        GithubMembershipSource, GithubOrganizationChoice,
    },
    oauth::{GithubOAuthSource, GithubTokenSet, OAuthError},
    secrets::SecretVault,
};
use crate::db::{
    entities::{org, organization_membership, team_session, user},
    migration::Migrator,
};

pub(super) const ALICE_USER_ID: i32 = 301;
pub(super) const ALICE_GITHUB_ID: i64 = 20_301;
pub(super) const POSTGRES_ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
pub(super) const POSTGRES_DATABASE_PREFIX: &str = "workos_team_auth_test_";

pub(super) fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 10, 0, 30)
        .single()
        .expect("valid controlled time")
}

pub(super) fn tokens(
    access: &str,
    refresh: Option<&str>,
    access_expires_at: Option<DateTime<Utc>>,
    refresh_expires_at: Option<DateTime<Utc>>,
) -> GithubTokenSet {
    GithubTokenSet {
        access_token: access.into(),
        refresh_token: refresh.map(str::to_owned),
        access_expires_at,
        refresh_expires_at,
    }
}

pub(super) struct TestClock(Mutex<DateTime<Utc>>);

impl TestClock {
    pub(super) fn new(now: DateTime<Utc>) -> Self {
        Self(Mutex::new(now))
    }

    pub(super) fn advance(&self, duration: Duration) {
        let mut now = self.0.lock().expect("test clock lock");
        *now = now.checked_add_signed(duration).expect("test clock range");
    }
}

impl TeamClock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("test clock lock")
    }
}

pub(super) struct RefreshGate {
    pub(super) started: Semaphore,
    pub(super) release: Notify,
}

pub(super) struct FakeOAuth {
    replies: Mutex<VecDeque<Result<GithubTokenSet, OAuthError>>>,
    calls: Mutex<Vec<String>>,
    exchange_replies: Mutex<VecDeque<Result<GithubTokenSet, OAuthError>>>,
    exchange_calls: Mutex<Vec<(String, String)>>,
    gate: Mutex<Option<Arc<RefreshGate>>>,
}

impl FakeOAuth {
    pub(super) fn new(replies: Vec<Result<GithubTokenSet, OAuthError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
            exchange_replies: Mutex::new(VecDeque::new()),
            exchange_calls: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        }
    }

    pub(super) fn with_exchange(
        replies: Vec<Result<GithubTokenSet, OAuthError>>,
    ) -> Self {
        let source = Self::new(Vec::new());
        *source
            .exchange_replies
            .lock()
            .expect("fake exchange replies lock") = replies.into();
        source
    }

    pub(super) fn exchange_calls(&self) -> Vec<(String, String)> {
        self.exchange_calls
            .lock()
            .expect("fake exchange calls lock")
            .clone()
    }

    pub(super) fn gated(
        replies: Vec<Result<GithubTokenSet, OAuthError>>,
    ) -> (Self, Arc<RefreshGate>) {
        let gate = Arc::new(RefreshGate {
            started: Semaphore::new(0),
            release: Notify::new(),
        });
        let source = Self::new(replies);
        *source.gate.lock().expect("fake gate lock") = Some(gate.clone());
        (source, gate)
    }

    pub(super) fn refresh_calls(&self) -> usize {
        self.calls.lock().expect("fake calls lock").len()
    }

    pub(super) fn refresh_inputs(&self) -> Vec<String> {
        self.calls.lock().expect("fake calls lock").clone()
    }
}

#[async_trait::async_trait]
impl GithubOAuthSource for FakeOAuth {
    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        _now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError> {
        self.exchange_calls
            .lock()
            .expect("fake exchange calls lock")
            .push((code.to_owned(), verifier.to_owned()));
        self.exchange_replies
            .lock()
            .expect("fake exchange replies lock")
            .pop_front()
            .unwrap_or(Err(OAuthError::Unavailable))
    }

    async fn refresh(
        &self,
        refresh_token: &str,
        _now: DateTime<Utc>,
    ) -> Result<GithubTokenSet, OAuthError> {
        self.calls
            .lock()
            .expect("fake calls lock")
            .push(refresh_token.to_owned());
        let gate = self.gate.lock().expect("fake gate lock").clone();
        if let Some(gate) = gate {
            gate.started.add_permits(1);
            gate.release.notified().await;
        }
        self.replies
            .lock()
            .expect("fake replies lock")
            .pop_front()
            .unwrap_or(Err(OAuthError::Unavailable))
    }
}

pub(super) struct ProviderGate {
    pub(super) started: Semaphore,
    pub(super) release: Notify,
}

pub(super) struct FakeGithub {
    identities: Mutex<VecDeque<Result<GithubIdentity, GithubEvidenceError>>>,
    organizations: Mutex<VecDeque<Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>>>,
    memberships: Mutex<VecDeque<Result<GithubMembershipEvidence, GithubEvidenceError>>>,
    organization_calls: Mutex<Vec<(String, i64)>>,
    membership_calls: Mutex<Vec<(String, i64, i64, String)>>,
    membership_gate: Mutex<Option<Arc<ProviderGate>>>,
}

impl FakeGithub {
    pub(super) fn new(
        identities: Vec<Result<GithubIdentity, GithubEvidenceError>>,
        organizations: Vec<Result<Vec<GithubOrganizationChoice>, GithubEvidenceError>>,
        memberships: Vec<Result<GithubMembershipEvidence, GithubEvidenceError>>,
    ) -> Self {
        Self {
            identities: Mutex::new(identities.into()),
            organizations: Mutex::new(organizations.into()),
            memberships: Mutex::new(memberships.into()),
            organization_calls: Mutex::new(Vec::new()),
            membership_calls: Mutex::new(Vec::new()),
            membership_gate: Mutex::new(None),
        }
    }

    pub(super) fn gate_membership(&self) -> Arc<ProviderGate> {
        let gate = Arc::new(ProviderGate {
            started: Semaphore::new(0),
            release: Notify::new(),
        });
        *self.membership_gate.lock().expect("membership gate lock") = Some(gate.clone());
        gate
    }

    pub(super) fn clear_membership_gate(&self) {
        *self.membership_gate.lock().expect("membership gate lock") = None;
    }

    pub(super) fn organization_calls(&self) -> Vec<(String, i64)> {
        self.organization_calls
            .lock()
            .expect("organization calls lock")
            .clone()
    }

    pub(super) fn membership_calls(&self) -> Vec<(String, i64, i64, String)> {
        self.membership_calls
            .lock()
            .expect("membership calls lock")
            .clone()
    }
}

#[async_trait::async_trait]
impl GithubMembershipSource for FakeGithub {
    async fn identity(
        &self,
        _access_token: &str,
    ) -> Result<GithubIdentity, GithubEvidenceError> {
        self.identities
            .lock()
            .expect("fake identities lock")
            .pop_front()
            .unwrap_or(Err(GithubEvidenceError::Unavailable))
    }

    async fn active_organizations(
        &self,
        access_token: &str,
        expected_user_id: i64,
    ) -> Result<Vec<GithubOrganizationChoice>, GithubEvidenceError> {
        self.organization_calls
            .lock()
            .expect("organization calls lock")
            .push((access_token.to_owned(), expected_user_id));
        self.organizations
            .lock()
            .expect("fake organizations lock")
            .pop_front()
            .unwrap_or(Err(GithubEvidenceError::Unavailable))
    }

    async fn verify_membership(
        &self,
        access_token: &str,
        expected_user_id: i64,
        expected_org_id: i64,
        organization_login: &str,
    ) -> Result<GithubMembershipEvidence, GithubEvidenceError> {
        self.membership_calls
            .lock()
            .expect("membership calls lock")
            .push((
                access_token.to_owned(),
                expected_user_id,
                expected_org_id,
                organization_login.to_owned(),
            ));
        let gate = self
            .membership_gate
            .lock()
            .expect("membership gate lock")
            .clone();
        if let Some(gate) = gate {
            gate.started.add_permits(1);
            gate.release.notified().await;
        }
        self.memberships
            .lock()
            .expect("fake memberships lock")
            .pop_front()
            .unwrap_or(Err(GithubEvidenceError::Unavailable))
    }
}

pub(super) struct Fixture {
    pub(super) db: DatabaseConnection,
    pub(super) vault: Arc<SecretVault>,
    pub(super) oauth: Arc<FakeOAuth>,
    pub(super) clock: Arc<TestClock>,
}

pub(super) async fn sqlite_fixture(replies: Vec<Result<GithubTokenSet, OAuthError>>) -> Fixture {
    let db = crate::db::test_helpers::fresh_in_memory_db().await.conn;
    seed_accounts(&db).await.expect("seed SQLite auth fixture");
    Fixture {
        db,
        vault: Arc::new(
            SecretVault::from_hex_key(
                "1111111111111111111111111111111111111111111111111111111111111111",
            )
            .expect("test vault"),
        ),
        oauth: Arc::new(FakeOAuth::new(replies)),
        clock: Arc::new(TestClock::new(fixed_now())),
    }
}

pub(super) async fn seed_accounts(db: &DatabaseConnection) -> Result<(), String> {
    let now = fixed_now();
    for model in [
        org::ActiveModel {
            id: Set(201),
            github_org_id: Set(10_201),
            login: Set("alice-a".into()),
            name: Set(None),
            avatar_url: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
        org::ActiveModel {
            id: Set(202),
            github_org_id: Set(10_202),
            login: Set("alice-b".into()),
            name: Set(None),
            avatar_url: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
        org::ActiveModel {
            id: Set(203),
            github_org_id: Set(10_203),
            login: Set("bob".into()),
            name: Set(None),
            avatar_url: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
        org::ActiveModel {
            id: Set(204),
            github_org_id: Set(10_204),
            login: Set("alice-unverified".into()),
            name: Set(None),
            avatar_url: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    ] {
        model
            .insert(db)
            .await
            .map_err(|error| format!("seed organization: {error}"))?;
    }
    for model in [
        user::ActiveModel {
            id: Set(ALICE_USER_ID),
            github_user_id: Set(ALICE_GITHUB_ID),
            login: Set("alice-renamable".into()),
            name: Set(None),
            email: Set(None),
            avatar_url: Set(None),
            org_id: Set(203),
            role: Set("owner".into()),
            gh_token_enc: Set(Some("legacy-must-not-be-read".into())),
            gh_refresh_token_enc: Set(Some("legacy-must-not-be-read".into())),
            gh_token_expires_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
        user::ActiveModel {
            id: Set(302),
            github_user_id: Set(20_302),
            login: Set("bob".into()),
            name: Set(None),
            email: Set(None),
            avatar_url: Set(None),
            org_id: Set(203),
            role: Set("member".into()),
            gh_token_enc: Set(None),
            gh_refresh_token_enc: Set(None),
            gh_token_expires_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    ] {
        model
            .insert(db)
            .await
            .map_err(|error| format!("seed user: {error}"))?;
    }
    for (id, org_id, user_id, state, generation, verified_at) in [
        (401, 201, ALICE_USER_ID, "active", 11, Some(now)),
        (402, 202, ALICE_USER_ID, "blocked", 11, Some(now)),
        (403, 203, ALICE_USER_ID, "active", 5, Some(now)),
        (404, 203, 302, "active", 11, Some(now)),
        (405, 204, ALICE_USER_ID, "active", 0, Some(now)),
    ] {
        organization_membership::ActiveModel {
            id: Set(id),
            org_id: Set(org_id),
            user_id: Set(user_id),
            role: Set(if id == 401 { "admin" } else { "member" }.into()),
            state: Set(state.into()),
            authorization_revision: Set(7),
            credential_generation: Set(generation),
            verified_at: Set(verified_at),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(|error| format!("seed membership: {error}"))?;
    }
    for (hash, user_id, org_id, membership_id, generation) in [
        ("session-a", ALICE_USER_ID, 201, 401, 11),
        ("session-blocked", ALICE_USER_ID, 202, 402, 11),
        ("session-mismatch", ALICE_USER_ID, 203, 403, 5),
        ("session-bob", 302, 203, 404, 11),
        ("session-zero", ALICE_USER_ID, 204, 405, 0),
    ] {
        team_session::ActiveModel {
            token_hash: Set(hash.into()),
            user_id: Set(user_id),
            org_id: Set(org_id),
            membership_id: Set(membership_id),
            authorization_revision: Set(7),
            credential_generation: Set(generation),
            csrf_hash: Set(format!("csrf-{hash}")),
            created_at: Set(now),
            expires_at: Set(now + Duration::hours(1)),
        }
        .insert(db)
        .await
        .map_err(|error| format!("seed session: {error}"))?;
    }
    Ok(())
}

pub(super) async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, String> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(options)
        .await
        .map_err(|error| format!("connect to {url}: {error}"))
}

pub(super) async fn create_postgres_fixture(
) -> Result<(String, DatabaseConnection, DatabaseConnection), String> {
    let name = format!("{POSTGRES_DATABASE_PREFIX}{}", Uuid::new_v4().simple());
    validate_database_target(&name, &name)?;
    let admin = connect(POSTGRES_ADMIN_URL, 1).await?;
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE \"{name}\""),
        ))
        .await
        .map_err(|error| format!("create disposable database: {error}"))?;
    let url = format!("postgres://air1@localhost:5432/{name}");
    let db = match connect(&url, 4).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.map_err(|close| close.to_string())?;
            return Err(format!("{error}; preserved database {name}"));
        }
    };
    if let Err(error) = Migrator::up(&db, None).await {
        db.close().await.map_err(|close| close.to_string())?;
        admin.close().await.map_err(|close| close.to_string())?;
        return Err(format!(
            "migrate disposable database: {error}; preserved database {name}"
        ));
    }
    if let Err(error) = seed_accounts(&db).await {
        db.close().await.map_err(|close| close.to_string())?;
        admin.close().await.map_err(|close| close.to_string())?;
        return Err(format!("{error}; preserved database {name}"));
    }
    Ok((name, admin, db))
}

pub(super) async fn drop_postgres_fixture(
    expected: &str,
    admin: DatabaseConnection,
    db: DatabaseConnection,
) -> Result<(), String> {
    let actual = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS database_name".to_owned(),
        ))
        .await
        .map_err(|error| format!("read disposable database identity: {error}"))?
        .ok_or_else(|| "current_database returned no row".to_owned())?
        .try_get::<String>("", "database_name")
        .map_err(|error| format!("decode disposable database identity: {error}"))?;
    validate_database_target(expected, &actual)?;
    db.close().await.map_err(|error| error.to_string())?;
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("DROP DATABASE \"{actual}\""),
        ))
        .await
        .map_err(|error| format!("drop disposable database: {error}"))?;
    admin.close().await.map_err(|error| error.to_string())?;
    eprintln!("verified and removed disposable database {actual}");
    Ok(())
}

pub(super) async fn wait_for_postgres_lock(
    admin: &DatabaseConnection,
    database_name: &str,
    query_fragment: &str,
) -> Result<(), String> {
    let pattern = format!("%{query_fragment}%");
    tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            let waiting = admin
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT EXISTS (\
                        SELECT 1 FROM pg_stat_activity \
                        WHERE datname = $1 \
                          AND wait_event_type = 'Lock' \
                          AND query LIKE $2\
                    ) AS waiting"
                        .to_owned(),
                    [database_name.into(), pattern.clone().into()],
                ))
                .await
                .map_err(|error| format!("observe PostgreSQL lock wait: {error}"))?
                .ok_or_else(|| "PostgreSQL lock observer returned no row".to_owned())?
                .try_get::<bool>("", "waiting")
                .map_err(|error| format!("decode PostgreSQL lock wait: {error}"))?;
            if waiting {
                return Ok(());
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| format!("timed out observing blocked PostgreSQL query containing {query_fragment}"))?
}

pub(super) fn validate_database_target(expected: &str, candidate: &str) -> Result<(), String> {
    let suffix = candidate
        .strip_prefix(POSTGRES_DATABASE_PREFIX)
        .unwrap_or_default();
    (candidate == expected
        && suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(())
    .ok_or_else(|| format!("refusing database target {candidate}; expected {expected}"))
}
