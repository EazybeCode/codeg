//! Independent schema. Never invokes the legacy desktop/team migrator.
use sea_orm::sea_query::{
    Alias, ColumnDef, Expr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement, Table,
    TableCreateStatement,
};

pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_schema_version",
    "neev_accounts",
    "neev_identities",
    "neev_verified_emails",
    "neev_organizations",
    "neev_memberships",
    "neev_sessions",
    "neev_oauth_attempts",
    "neev_magic_attempts",
    "neev_invitations",
    "neev_quota_buckets",
    "neev_mail_leases",
    "neev_requests",
    "neev_nodes",
];
pub(super) const SCHEMA_VERSION: i64 = 2;

// Column descriptors keep identical SQL types/constraints across supported DBs.
// i=integer, l=bigint, t=text, b=boolean; ? suffix means nullable.
fn table(
    name: &str,
    columns: &[(&str, &str)],
    auto_id: bool,
    primary: &[&str],
    checks: &[&str],
    unique: &[&[&str]],
    foreign: &[(&str, &str, &str)],
) -> TableCreateStatement {
    let mut t = Table::create();
    t.table(Alias::new(name));
    if auto_id {
        t.col(
            ColumnDef::new(Alias::new("id"))
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        );
    }
    for (name, kind) in columns {
        let mut c = ColumnDef::new(Alias::new(*name));
        match kind.as_bytes()[0] {
            b'i' => {
                c.integer();
            }
            b'l' => {
                c.big_integer();
            }
            b'b' => {
                c.boolean();
            }
            _ => {
                c.text();
            }
        }
        if *kind == "l1" {
            c.default(1);
        }
        if !kind.ends_with('?') {
            c.not_null();
        }
        t.col(&mut c);
    }
    if !primary.is_empty() {
        let mut key = Index::create();
        for c in primary {
            key.col(Alias::new(*c));
        }
        t.primary_key(&mut key);
    }
    for check in checks {
        t.check(Expr::cust(*check));
    }
    for columns in unique {
        let mut key = Index::create();
        key.unique();
        for c in *columns {
            key.col(Alias::new(*c));
        }
        t.index(&mut key);
    }
    for (col, other, other_col) in foreign {
        t.foreign_key(
            ForeignKey::create()
                .from(Alias::new(name), Alias::new(*col))
                .to(Alias::new(*other), Alias::new(*other_col))
                .on_delete(ForeignKeyAction::Restrict),
        );
    }
    t
}

pub(super) fn tables() -> Vec<TableCreateStatement> {
    let mut result = vec![
        table("neev_schema_version", &[("id","i"),("version","l")], false, &["id"], &["id=1", "version>0"], &[], &[]),
        table("neev_accounts", &[("display_name","t"),("avatar_url","t?"),("auth_revision","l"),("disabled","b"),("created_at","l")], true, &[], &["id>0", "auth_revision>=1", "length(display_name) BETWEEN 1 AND 200"], &[], &[]),
        table("neev_identities", &[("user_id","i"),("provider","t"),("issuer","t"),("subject","t"),("created_at","l")], true, &[], &["provider IN ('google','github','email')", "length(subject) BETWEEN 1 AND 255"], &[&["provider","issuer","subject"]], &[("user_id","neev_accounts","id")]),
        table("neev_verified_emails", &[("user_id","i"),("canonical_email","t"),("verified_at","l")], true, &[], &["length(canonical_email) BETWEEN 3 AND 254"], &[&["canonical_email"]], &[("user_id","neev_accounts","id")]),
        table("neev_organizations", &[("owner_user_id","i"),("name","t"),("logo_data","t?"),("revision","l"),("branding_revision","l1"),("disabled","b"),("created_at","l")], true, &[], &["id>0", "revision>=1", "branding_revision>=1", "length(name) BETWEEN 1 AND 100"], &[], &[("owner_user_id","neev_accounts","id")]),
        table("neev_memberships", &[("org_id","i"),("user_id","i"),("role","t"),("revision","l"),("active","b")], true, &[], &["id>0", "revision>=1", "role IN ('admin','member')"], &[&["org_id","user_id"], &["id","org_id","user_id"]], &[("org_id","neev_organizations","id"),("user_id","neev_accounts","id")]),
        table("neev_sessions", &[("id","t"),("token_hash","t"),("csrf_hash","t"),("user_id","i"),("account_revision","l"),("org_id","i?"),("member_id","i?"),("member_revision","l?"),("org_revision","l?"),("authenticated_at","l"),("expires_at","l"),("revoked_at","l?")], false, &["id"], &[
            "account_revision>=1", "expires_at>authenticated_at", "length(token_hash)=64", "length(csrf_hash)=64",
            "(org_id IS NULL AND member_id IS NULL AND member_revision IS NULL AND org_revision IS NULL) OR (org_id IS NOT NULL AND member_id IS NOT NULL AND member_revision IS NOT NULL AND org_revision IS NOT NULL AND member_revision>=1 AND org_revision>=1)"
        ], &[&["token_hash"]], &[("user_id","neev_accounts","id"),("org_id","neev_organizations","id"),("member_id","neev_memberships","id")]),
        table("neev_oauth_attempts", &[("state_hash","t"),("browser_hash","t"),("provider","t"),("intent","t"),("link_user_id","i?"),("link_session_id","t?"),("link_revision","l?"),("exchange_enc","t"),("expires_at","l"),("consumed_at","l?")], false, &["state_hash"], &[
            "provider IN ('google','github')", "length(state_hash)=64", "length(browser_hash)=64", LINK_CHECK,
        ], &[], &[("link_user_id","neev_accounts","id"),("link_session_id","neev_sessions","id")]),
        table("neev_magic_attempts", &[("token_hash","t"),("browser_hash","t"),("canonical_email","t"),("intent","t"),("link_user_id","i?"),("link_session_id","t?"),("link_revision","l?"),("expires_at","l"),("consumed_at","l?")], false, &["token_hash"], &[
            "length(token_hash)=64", "length(browser_hash)=64", LINK_CHECK,
        ], &[], &[("link_user_id","neev_accounts","id"),("link_session_id","neev_sessions","id")]),
        table("neev_invitations", &[("id","t"),("org_id","i"),("inviter_id","i"),("recipient_email","t"),("role","t"),("token_hash","t"),("expires_at","l"),("revoked_at","l?"),("accepted_by","i?"),("accepted_at","l?")], false, &["id"], &[
            "role IN ('admin','member')", "length(token_hash)=64", "(accepted_by IS NULL AND accepted_at IS NULL) OR (accepted_by IS NOT NULL AND accepted_at IS NOT NULL)",
        ], &[&["token_hash"]], &[("org_id","neev_organizations","id"),("inviter_id","neev_accounts","id"),("accepted_by","neev_accounts","id")]),
        table("neev_quota_buckets", &[("kind","t"),("key_hash","t"),("window_start","l"),("used","l"),("expires_at","l")], false, &["kind","key_hash","window_start"], &["used>=0"], &[], &[]),
        table("neev_mail_leases", &[("id","t"),("expires_at","l")], false, &["id"], &[], &[], &[]),
        table("neev_requests", &[("user_id","i"),("action","t"),("request_id","t"),("body_hash","t"),("resource_id","t"),("expires_at","l")], false, &["user_id","action","request_id"], &["length(body_hash)=64", "length(resource_id) BETWEEN 1 AND 64"], &[], &[("user_id","neev_accounts","id")]),
        table("neev_nodes", &[("org_id","i"),("parent_id","i?"),("kind","t"),("name","t"),("revision","l"),("created_by","i")], true, &[], &[
            "id>0", "revision>=1", "kind IN ('project','folder','working_directory')", "length(name) BETWEEN 1 AND 100", "parent_id IS NULL OR parent_id<>id", "(kind='project' AND parent_id IS NULL) OR (kind<>'project' AND parent_id IS NOT NULL)",
        ], &[&["id","org_id"]], &[("org_id","neev_organizations","id"),("created_by","neev_accounts","id")]),
    ];
    // Cross-column foreign keys prevent valid IDs being combined across tenants.
    result[6].foreign_key(
        ForeignKey::create()
            .from_tbl(Alias::new("neev_sessions"))
            .from_col(Alias::new("member_id"))
            .from_col(Alias::new("org_id"))
            .from_col(Alias::new("user_id"))
            .to_tbl(Alias::new("neev_memberships"))
            .to_col(Alias::new("id"))
            .to_col(Alias::new("org_id"))
            .to_col(Alias::new("user_id")),
    );
    result[13].foreign_key(
        ForeignKey::create()
            .from_tbl(Alias::new("neev_nodes"))
            .from_col(Alias::new("parent_id"))
            .from_col(Alias::new("org_id"))
            .to_tbl(Alias::new("neev_nodes"))
            .to_col(Alias::new("id"))
            .to_col(Alias::new("org_id")),
    );
    result
}

const LINK_CHECK: &str = "(intent='login' AND link_user_id IS NULL AND link_session_id IS NULL AND link_revision IS NULL) OR (intent='link' AND link_user_id IS NOT NULL AND link_session_id IS NOT NULL AND link_revision IS NOT NULL AND link_revision>=1)";

pub(super) fn indexes() -> Vec<IndexCreateStatement> {
    [
        ("neev_identities_user", "neev_identities", "user_id"),
        (
            "neev_verified_emails_user",
            "neev_verified_emails",
            "user_id",
        ),
        ("neev_memberships_user", "neev_memberships", "user_id"),
        ("neev_sessions_expiry", "neev_sessions", "expires_at"),
        ("neev_sessions_user", "neev_sessions", "user_id"),
        ("neev_oauth_expiry", "neev_oauth_attempts", "expires_at"),
        ("neev_magic_expiry", "neev_magic_attempts", "expires_at"),
        ("neev_invites_org", "neev_invitations", "org_id"),
        ("neev_invites_expiry", "neev_invitations", "expires_at"),
        ("neev_quota_expiry", "neev_quota_buckets", "expires_at"),
        ("neev_lease_expiry", "neev_mail_leases", "expires_at"),
        ("neev_request_expiry", "neev_requests", "expires_at"),
        ("neev_nodes_org", "neev_nodes", "org_id"),
    ]
    .into_iter()
    .map(|(name, table, column)| {
        Index::create()
            .name(name)
            .table(Alias::new(table))
            .col(Alias::new(column))
            .to_owned()
    })
    .collect()
}
