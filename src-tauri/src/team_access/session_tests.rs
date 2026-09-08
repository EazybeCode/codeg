use axum::http::{header, HeaderMap, HeaderValue};
use chrono::{DateTime, Duration, TimeZone, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, IntoActiveModel, Set, Statement,
};
use sea_orm_migration::MigratorTrait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::session::{
    membership_freshness, resolve_local_session, revoke_local_session, LocalPrincipal,
    LocalSessionError, MembershipFreshness, TeamRole,
};
use crate::db::entities::{organization_membership, org, team_session, user};
use crate::db::migration::Migrator;

const ALICE_A_TOKEN: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ALICE_B_TOKEN: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BOB_TOKEN: &str =
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const CAROL_TOKEN: &str =
    "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const SHORT_TOKEN: &str =
    "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const UPPERCASE_TOKEN: &str =
    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
const NON_HEX_TOKEN: &str =
    "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg";
const POSTGRES_ADMIN_URL: &str = "postgres://air1@localhost:5432/postgres";
const POSTGRES_DATABASE_PREFIX: &str = "workos_team_session_test_";

struct Fixture {
    db: DatabaseConnection,
    now: DateTime<Utc>,
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 8, 10, 0, 30)
        .single()
        .expect("valid controlled clock")
}

fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn cookie_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("workos_team_session={token}"))
            .expect("valid cookie fixture"),
    );
    headers
}

fn organization(id: i32, login: &str, at: DateTime<Utc>) -> org::ActiveModel {
    org::ActiveModel {
        id: Set(id),
        github_org_id: Set(i64::from(id) + 10_000),
        login: Set(login.into()),
        name: Set(Some(format!("{login} name"))),
        avatar_url: Set(None),
        created_at: Set(at),
        updated_at: Set(at),
    }
}

fn account(
    id: i32,
    login: &str,
    legacy_org_id: i32,
    legacy_role: &str,
    at: DateTime<Utc>,
) -> user::ActiveModel {
    user::ActiveModel {
        id: Set(id),
        github_user_id: Set(i64::from(id) + 20_000),
        login: Set(login.into()),
        name: Set(Some(format!("{login} name"))),
        email: Set(Some(format!("{login}@example.invalid"))),
        avatar_url: Set(None),
        org_id: Set(legacy_org_id),
        role: Set(legacy_role.into()),
        gh_token_enc: Set(None),
        gh_refresh_token_enc: Set(None),
        gh_token_expires_at: Set(None),
        created_at: Set(at),
        updated_at: Set(at),
    }
}

fn membership(
    id: i32,
    org_id: i32,
    user_id: i32,
    role: &str,
    verified_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) -> organization_membership::ActiveModel {
    organization_membership::ActiveModel {
        id: Set(id),
        org_id: Set(org_id),
        user_id: Set(user_id),
        role: Set(role.into()),
        state: Set("active".into()),
        authorization_revision: Set(7),
        credential_generation: Set(11),
        verified_at: Set(verified_at),
        created_at: Set(at),
        updated_at: Set(at),
    }
}

fn session(
    token: &str,
    user_id: i32,
    org_id: i32,
    membership_id: i32,
    at: DateTime<Utc>,
) -> team_session::ActiveModel {
    team_session::ActiveModel {
        token_hash: Set(token_hash(token)),
        user_id: Set(user_id),
        org_id: Set(org_id),
        membership_id: Set(membership_id),
        authorization_revision: Set(7),
        credential_generation: Set(11),
        csrf_hash: Set(format!("csrf-{membership_id}")),
        created_at: Set(at - Duration::seconds(120)),
        expires_at: Set(at + Duration::hours(1)),
    }
}

async fn seed_fixture(db: DatabaseConnection) -> Fixture {
    let at = now();
    for model in [
        organization(201, "alice-a", at),
        organization(202, "alice-b", at),
        organization(203, "bob-org", at),
        organization(204, "carol-org", at),
    ] {
        model.insert(&db).await.expect("seed organization");
    }
    for model in [
        account(301, "alice", 201, "admin", at),
        account(302, "bob", 203, "member", at),
        account(303, "carol", 204, "admin", at),
    ] {
        model.insert(&db).await.expect("seed user");
    }
    for model in [
        membership(401, 201, 301, "admin", Some(at - Duration::seconds(10)), at),
        membership(402, 202, 301, "member", Some(at - Duration::seconds(59)), at),
        membership(403, 203, 302, "member", Some(at), at),
        membership(404, 204, 303, "admin", Some(at), at),
    ] {
        model.insert(&db).await.expect("seed membership");
    }
    for model in [
        session(ALICE_A_TOKEN, 301, 201, 401, at),
        session(ALICE_B_TOKEN, 301, 202, 402, at),
        session(BOB_TOKEN, 302, 203, 403, at),
        session(CAROL_TOKEN, 303, 204, 404, at),
    ] {
        model.insert(&db).await.expect("seed team session");
    }
    Fixture { db, now: at }
}

async fn sqlite_fixture() -> Fixture {
    seed_fixture(crate::db::test_helpers::fresh_in_memory_db().await.conn).await
}

async fn alice_b(fixture: &Fixture) -> Result<LocalPrincipal, LocalSessionError> {
    resolve_local_session(&fixture.db, &cookie_headers(ALICE_B_TOKEN), fixture.now).await
}

async fn replace_session(fixture: &Fixture, model: team_session::ActiveModel) {
    model.update(&fixture.db).await.expect("update session fixture");
}

async fn replace_membership(
    fixture: &Fixture,
    model: organization_membership::ActiveModel,
) {
    model
        .update(&fixture.db)
        .await
        .expect("update membership fixture");
}

#[tokio::test]
async fn organization_bound_cookie_resolves_membership_instead_of_legacy_user_scope() {
    let fixture = sqlite_fixture().await;
    let principal = alice_b(&fixture).await.unwrap();

    assert_eq!(principal.session_id, token_hash(ALICE_B_TOKEN));
    assert_eq!(principal.org_id, 202);
    assert_eq!(principal.role, TeamRole::Member);
    assert_eq!(principal.user_id, 301);
    assert_eq!(principal.membership_id, 402);
    assert_eq!(principal.authorization_revision, 7);
    assert_eq!(principal.credential_generation, 11);
    assert_eq!(
        membership_freshness(&principal, fixture.now),
        MembershipFreshness::Fresh
    );
}

#[tokio::test]
async fn session_cookie_is_required_unique_well_formed_and_not_replaceable_by_operator_auth() {
    let fixture = sqlite_fixture().await;
    for malformed_token in [SHORT_TOKEN, UPPERCASE_TOKEN, NON_HEX_TOKEN] {
        session(malformed_token, 301, 202, 402, fixture.now)
            .insert(&fixture.db)
            .await
            .expect("seed otherwise valid session for malformed-token digest");
    }
    let mut bearer_only = HeaderMap::new();
    bearer_only.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer operator-token"));
    let malformed = [
        HeaderMap::new(),
        bearer_only,
        cookie_headers(SHORT_TOKEN),
        cookie_headers(UPPERCASE_TOKEN),
        cookie_headers(NON_HEX_TOKEN),
    ];
    for headers in malformed {
        assert_eq!(
            resolve_local_session(&fixture.db, &headers, fixture.now).await,
            Err(LocalSessionError::Unauthorized)
        );
    }

    for value in ["workos_team_session", "workos_team_session="] {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static(value));
        assert_eq!(
            resolve_local_session(&fixture.db, &headers, fixture.now).await,
            Err(LocalSessionError::Unauthorized)
        );
    }

    let mut duplicate_one_header = HeaderMap::new();
    duplicate_one_header.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "workos_team_session={ALICE_B_TOKEN}; workos_team_session={ALICE_A_TOKEN}"
        ))
        .unwrap(),
    );
    assert_eq!(
        resolve_local_session(&fixture.db, &duplicate_one_header, fixture.now).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mut duplicate_headers = HeaderMap::new();
    duplicate_headers.append(
        header::COOKIE,
        HeaderValue::from_str(&format!("workos_team_session={ALICE_B_TOKEN}")).unwrap(),
    );
    duplicate_headers.append(
        header::COOKIE,
        HeaderValue::from_str(&format!("other=1; workos_team_session={ALICE_A_TOKEN}")).unwrap(),
    );
    assert_eq!(
        resolve_local_session(&fixture.db, &duplicate_headers, fixture.now).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mut unrelated = HeaderMap::new();
    unrelated.insert(header::COOKIE, HeaderValue::from_static("theme=dark; locale=en"));
    assert_eq!(
        resolve_local_session(&fixture.db, &unrelated, fixture.now).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mut legacy = HeaderMap::new();
    legacy.insert(
        header::COOKIE,
        HeaderValue::from_static(
            "workos_session=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    );
    assert_eq!(
        resolve_local_session(&fixture.db, &legacy, fixture.now).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mut unrelated_and_valid = cookie_headers(ALICE_B_TOKEN);
    unrelated_and_valid.insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("theme=dark; workos_team_session={ALICE_B_TOKEN}"))
            .unwrap(),
    );
    assert_eq!(alice_b(&fixture).await.unwrap().org_id, 202);
    assert_eq!(
        resolve_local_session(&fixture.db, &unrelated_and_valid, fixture.now)
            .await
            .unwrap()
            .org_id,
        202
    );

    let mut non_utf8 = HeaderMap::new();
    non_utf8.insert(
        header::COOKIE,
        HeaderValue::from_bytes(b"workos_team_session=\xff").unwrap(),
    );
    assert_eq!(
        resolve_local_session(&fixture.db, &non_utf8, fixture.now).await,
        Err(LocalSessionError::Unauthorized)
    );
}

#[tokio::test]
async fn expired_and_future_created_sessions_are_rejected() {
    let expired = sqlite_fixture().await;
    let mut expired_session = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
        .one(&expired.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    expired_session.expires_at = Set(expired.now);
    replace_session(&expired, expired_session).await;
    assert_eq!(alice_b(&expired).await, Err(LocalSessionError::Unauthorized));

    let future = sqlite_fixture().await;
    let mut future_session = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
        .one(&future.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    future_session.created_at = Set(future.now + Duration::seconds(1));
    replace_session(&future, future_session).await;
    assert_eq!(alice_b(&future).await, Err(LocalSessionError::Unauthorized));
}

#[tokio::test]
async fn every_referenced_identity_row_must_exist() {
    let missing_membership = sqlite_fixture().await;
    organization_membership::Entity::delete_by_id(402)
        .exec(&missing_membership.db)
        .await
        .unwrap();
    assert_eq!(
        alice_b(&missing_membership).await,
        Err(LocalSessionError::Unauthorized)
    );

    let missing_org = sqlite_fixture().await;
    org::Entity::delete_by_id(202)
        .exec(&missing_org.db)
        .await
        .unwrap();
    assert_eq!(alice_b(&missing_org).await, Err(LocalSessionError::Unauthorized));

    let missing_user = sqlite_fixture().await;
    user::Entity::delete_by_id(301)
        .exec(&missing_user.db)
        .await
        .unwrap();
    assert_eq!(alice_b(&missing_user).await, Err(LocalSessionError::Unauthorized));
}

#[tokio::test]
async fn session_and_membership_composite_identity_must_match() {
    let mismatched_org = sqlite_fixture().await;
    let mut model = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
        .one(&mismatched_org.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    model.org_id = Set(201);
    replace_session(&mismatched_org, model).await;
    assert_eq!(
        alice_b(&mismatched_org).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mismatched_user = sqlite_fixture().await;
    let mut model = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
        .one(&mismatched_user.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    model.user_id = Set(302);
    replace_session(&mismatched_user, model).await;
    assert_eq!(
        alice_b(&mismatched_user).await,
        Err(LocalSessionError::Unauthorized)
    );

    let mismatched_membership = sqlite_fixture().await;
    let mut model = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
        .one(&mismatched_membership.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    model.membership_id = Set(401);
    replace_session(&mismatched_membership, model).await;
    assert_eq!(
        alice_b(&mismatched_membership).await,
        Err(LocalSessionError::Unauthorized)
    );
}

#[tokio::test]
async fn inactive_membership_states_are_rejected() {
    for state in ["revoked", "blocked"] {
        let fixture = sqlite_fixture().await;
        let mut model = organization_membership::Entity::find_by_id(402)
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap()
            .into_active_model();
        model.state = Set(state.into());
        replace_membership(&fixture, model).await;
        assert_eq!(alice_b(&fixture).await, Err(LocalSessionError::Unauthorized));
    }
}

#[tokio::test]
async fn session_and_membership_revisions_must_match() {
    let stale_authorization = sqlite_fixture().await;
    let mut model = organization_membership::Entity::find_by_id(402)
        .one(&stale_authorization.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    model.authorization_revision = Set(8);
    replace_membership(&stale_authorization, model).await;
    assert_eq!(
        alice_b(&stale_authorization).await,
        Err(LocalSessionError::Unauthorized)
    );

    let stale_credentials = sqlite_fixture().await;
    let mut model = organization_membership::Entity::find_by_id(402)
        .one(&stale_credentials.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    model.credential_generation = Set(12);
    replace_membership(&stale_credentials, model).await;
    assert_eq!(
        alice_b(&stale_credentials).await,
        Err(LocalSessionError::Unauthorized)
    );
}

async fn inject_corruption(fixture: &Fixture, sql: &str) {
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA ignore_check_constraints=ON".to_owned(),
        ))
        .await
        .expect("temporarily disable SQLite checks");
    fixture
        .db
        .execute(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .expect("inject corrupt row");
    fixture
        .db
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA ignore_check_constraints=OFF".to_owned(),
        ))
        .await
        .expect("immediately restore SQLite checks");
}

#[tokio::test]
async fn corrupt_roles_and_negative_revisions_are_rejected() {
    let unknown_role = sqlite_fixture().await;
    inject_corruption(
        &unknown_role,
        "UPDATE organization_membership SET role = 'owner' WHERE id = 402",
    )
    .await;
    assert_eq!(alice_b(&unknown_role).await, Err(LocalSessionError::Unauthorized));

    let negative_authorization = sqlite_fixture().await;
    inject_corruption(
        &negative_authorization,
        "UPDATE organization_membership SET authorization_revision = -1 WHERE id = 402; \
         UPDATE team_session SET authorization_revision = -1 WHERE membership_id = 402",
    )
    .await;
    assert_eq!(
        alice_b(&negative_authorization).await,
        Err(LocalSessionError::Unauthorized)
    );

    let negative_credentials = sqlite_fixture().await;
    inject_corruption(
        &negative_credentials,
        "UPDATE organization_membership SET credential_generation = -1 WHERE id = 402; \
         UPDATE team_session SET credential_generation = -1 WHERE membership_id = 402",
    )
    .await;
    assert_eq!(
        alice_b(&negative_credentials).await,
        Err(LocalSessionError::Unauthorized)
    );
}

#[tokio::test]
async fn storage_failures_are_unavailable_without_database_details() {
    assert_eq!(LocalSessionError::Unauthorized.to_string(), "unauthorized");
    assert_eq!(LocalSessionError::Unavailable.to_string(), "session storage unavailable");

    let fixture = sqlite_fixture().await;
    let closed = fixture.db.clone();
    fixture.db.close().await.expect("close SQLite pool");
    assert_eq!(
        resolve_local_session(&closed, &cookie_headers(ALICE_B_TOKEN), fixture.now).await,
        Err(LocalSessionError::Unavailable)
    );
}

#[tokio::test]
async fn membership_proof_is_fresh_for_less_than_sixty_seconds_only() {
    let fixture = sqlite_fixture().await;
    let mut principal = alice_b(&fixture).await.unwrap();
    for (age, expected) in [
        (0, MembershipFreshness::Fresh),
        (59, MembershipFreshness::Fresh),
        (60, MembershipFreshness::ReverificationRequired),
        (61, MembershipFreshness::ReverificationRequired),
    ] {
        principal.verified_at = Some(fixture.now - Duration::seconds(age));
        assert_eq!(membership_freshness(&principal, fixture.now), expected);
    }
    principal.verified_at = None;
    assert_eq!(
        membership_freshness(&principal, fixture.now),
        MembershipFreshness::ReverificationRequired
    );
    principal.verified_at = Some(fixture.now + Duration::seconds(1));
    assert_eq!(
        membership_freshness(&principal, fixture.now),
        MembershipFreshness::ReverificationRequired
    );
}

#[tokio::test]
async fn revocation_is_idempotent_and_scoped_to_the_checked_principal() {
    let fixture = sqlite_fixture().await;
    let principal = alice_b(&fixture).await.unwrap();

    revoke_local_session(&fixture.db, &principal).await.unwrap();
    revoke_local_session(&fixture.db, &principal).await.unwrap();

    assert_eq!(alice_b(&fixture).await, Err(LocalSessionError::Unauthorized));
    assert!(team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        resolve_local_session(&fixture.db, &cookie_headers(ALICE_A_TOKEN), fixture.now)
            .await
            .unwrap()
            .role,
        TeamRole::Admin
    );
    assert!(organization_membership::Entity::find_by_id(402)
        .one(&fixture.db)
        .await
        .unwrap()
        .is_some());
}

async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, String> {
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(max_connections)
        .min_connections(1)
        .sqlx_logging(false);
    Database::connect(options)
        .await
        .map_err(|error| format!("connect to {url}: {error}"))
}

fn validate_database_target(expected: &str, candidate: &str) -> Result<(), String> {
    let suffix = candidate
        .strip_prefix(POSTGRES_DATABASE_PREFIX)
        .unwrap_or_default();
    (candidate == expected
        && suffix.len() == 32
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
    .then_some(())
    .ok_or_else(|| {
        format!("refusing database target {candidate}; expected exact generated name {expected}")
    })
}

/// Exercises the production joined resolver against PostgreSQL's timestamp decoding and
/// placeholder syntax. Run explicitly with:
/// `cargo test --no-default-features --lib team_session_postgres_round_trip -- --ignored --test-threads=1 --nocapture`
#[tokio::test]
#[ignore = "requires local Postgres role air1 with CREATEDB on localhost:5432"]
async fn team_session_postgres_round_trip() {
    let generated_database_name = format!(
        "{POSTGRES_DATABASE_PREFIX}{}",
        Uuid::new_v4().simple()
    );
    validate_database_target(&generated_database_name, &generated_database_name)
        .expect("generated database name must be safe");
    let admin = connect(POSTGRES_ADMIN_URL, 1)
        .await
        .expect("connect explicit local Postgres admin database");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("CREATE DATABASE \"{generated_database_name}\""),
        ))
        .await
        .expect("create generated team-session test database");
    let database_url = format!("postgres://air1@localhost:5432/{generated_database_name}");
    let db = match connect(&database_url, 2).await {
        Ok(db) => db,
        Err(error) => {
            admin.close().await.expect("close admin pool");
            panic!("{error}; preserved database {generated_database_name}");
        }
    };

    let outcome: Result<(), String> = async {
        Migrator::up(&db, None)
            .await
            .map_err(|error| format!("run migrations: {error}"))?;
        let fixture = seed_fixture(db.clone()).await;
        let principal = alice_b(&fixture)
            .await
            .map_err(|error| format!("resolve PostgreSQL B principal: {error}"))?;
        if principal.org_id != 202 || principal.user_id != 301 || principal.role != TeamRole::Member
        {
            return Err(format!("resolved wrong PostgreSQL principal: {principal:?}"));
        }

        let original = team_session::Entity::find_by_id(token_hash(ALICE_B_TOKEN))
            .one(&db)
            .await
            .map_err(|error| format!("load PostgreSQL B session: {error}"))?
            .ok_or_else(|| "PostgreSQL B session missing".to_owned())?;
        let mut mismatched = original.clone().into_active_model();
        mismatched.org_id = Set(201);
        mismatched
            .update(&db)
            .await
            .map_err(|error| format!("write PostgreSQL mismatched binding: {error}"))?;
        if resolve_local_session(&db, &cookie_headers(ALICE_B_TOKEN), fixture.now).await
            != Err(LocalSessionError::Unauthorized)
        {
            return Err("PostgreSQL resolver accepted mismatched binding".into());
        }
        let mut restored = original.into_active_model();
        restored.org_id = Set(202);
        restored
            .update(&db)
            .await
            .map_err(|error| format!("restore PostgreSQL B binding: {error}"))?;

        let principal = resolve_local_session(&db, &cookie_headers(ALICE_B_TOKEN), fixture.now)
            .await
            .map_err(|error| format!("re-resolve PostgreSQL B principal: {error}"))?;
        revoke_local_session(&db, &principal)
            .await
            .map_err(|error| format!("revoke PostgreSQL B session: {error}"))?;
        if team_session::Entity::find_by_id(token_hash(ALICE_A_TOKEN))
            .one(&db)
            .await
            .map_err(|error| format!("check PostgreSQL A session: {error}"))?
            .is_none()
        {
            return Err("PostgreSQL scoped revocation deleted A session".into());
        }
        if resolve_local_session(&db, &cookie_headers(ALICE_B_TOKEN), fixture.now).await
            != Err(LocalSessionError::Unauthorized)
        {
            return Err("revoked PostgreSQL B cookie still resolved".into());
        }
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        db.close().await.expect("close disposable database pool");
        admin
            .close()
            .await
            .expect("close admin pool after failure");
        panic!("{error}; preserved database {generated_database_name}");
    }

    let connected_database_name = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS database_name".to_owned(),
        ))
        .await
        .expect("read current disposable database identity")
        .expect("current_database must return one row")
        .try_get::<String>("", "database_name")
        .expect("decode current disposable database identity");
    validate_database_target(&generated_database_name, &connected_database_name)
        .expect("cleanup target must equal connected generated database");
    db.close().await.expect("close disposable database pool");
    admin
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!("DROP DATABASE \"{connected_database_name}\""),
        ))
        .await
        .expect("drop successful team-session test database");
    admin.close().await.expect("close admin pool");
    eprintln!("verified and removed disposable database {connected_database_name}");
}
