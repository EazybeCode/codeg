//! Schema 25 — Microsoft Teams bindings and shared channel-thread ledgers.
//!
//! Nine additive tables, and NO change to any existing table or row:
//! - `neev_msteams_installations`: one Microsoft 365 tenant -> exactly one org,
//!   with the agent a new Teams thread starts on.
//! - `neev_msteams_project_links`: a Team (bot `team.id`) -> a Neev project node.
//! - `neev_msteams_channel_links`: a channel of a linked Team that Neev answers in.
//! - `neev_msteams_principals`: an Entra object id -> a Neev member, created only
//!   by an explicit link that proved both identities.
//! - `neev_msteams_threads`: one channel root message = one shared thread, with
//!   at most one live run across all of its participants (`live_sequence`
//!   claims it BEFORE the run exists, so two members cannot both start one).
//! - `neev_msteams_thread_messages`: the thread ledger (shared context). Its key
//!   is the Bot Connector's `(tenant, conversation, activity)`, so a retried
//!   activity can never be recorded twice.
//! - `neev_msteams_inbox`: every accepted message, committed before the HTTP
//!   acknowledgement and carried to exactly one run by the worker.
//! - `neev_msteams_outbox`: every message Neev will post, retried with backoff
//!   and re-authorized at send time.
//! - `neev_msteams_documents`: generated files of channel runs and their
//!   publication to the channel's SharePoint folder (Release 4).
//!
//! Nothing here grants access: every request still runs as the linked member
//! through an ordinary re-checked org session. A binary that predates this schema,
//! or a downgrade that drops these tables, simply has no Teams bindings.

use super::{database_error, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend};

pub(crate) const SCHEMA_VERSION: i64 = 25;
pub(super) const TABLE_NAMES: &[&str] = &[
    "neev_msteams_installations",
    "neev_msteams_project_links",
    "neev_msteams_channel_links",
    "neev_msteams_principals",
    "neev_msteams_threads",
    "neev_msteams_thread_messages",
    "neev_msteams_inbox",
    "neev_msteams_outbox",
    "neev_msteams_documents",
];

/// Children before parents: the order a fixture that rewinds to an older
/// inventory drops these tables in.
#[cfg(test)]
pub(crate) const DROP_ORDER: &[&str] = &[
    "neev_msteams_documents",
    "neev_msteams_outbox",
    "neev_msteams_inbox",
    "neev_msteams_thread_messages",
    "neev_msteams_threads",
    "neev_msteams_principals",
    "neev_msteams_channel_links",
    "neev_msteams_project_links",
    "neev_msteams_installations",
];

struct Table {
    name: &'static str,
    ddl: &'static str,
    /// (column, PostgreSQL data_type, nullable).
    columns: &'static [(&'static str, &'static str, bool)],
    /// (constraint name, contype, fragments of `pg_get_constraintdef`).
    constraints: &'static [(&'static str, &'static str, &'static [&'static str])],
}

const TABLES: [Table; 9] = [
    Table {
        name: "neev_msteams_installations",
        ddl: "CREATE TABLE neev_msteams_installations (tenant_id text NOT NULL, org_id integer NOT NULL, enabled boolean NOT NULL, provider text NOT NULL, model text NOT NULL, service_url text, revision bigint NOT NULL, created_by integer NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_installations_pk PRIMARY KEY (tenant_id), CONSTRAINT neev_msteams_installations_org_key UNIQUE (tenant_id, org_id), CONSTRAINT neev_msteams_installations_tenant CHECK (length(tenant_id)=36), CONSTRAINT neev_msteams_installations_agent CHECK (length(provider) BETWEEN 1 AND 32 AND length(model) BETWEEN 1 AND 128), CONSTRAINT neev_msteams_installations_service CHECK (service_url IS NULL OR length(service_url) BETWEEN 9 AND 512), CONSTRAINT neev_msteams_installations_revision CHECK (revision>0), CONSTRAINT neev_msteams_installations_org FOREIGN KEY (org_id) REFERENCES neev_organizations (id) ON DELETE CASCADE, CONSTRAINT neev_msteams_installations_by FOREIGN KEY (created_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("created_at", "bigint", false),
            ("created_by", "integer", false),
            ("enabled", "boolean", false),
            ("model", "text", false),
            ("org_id", "integer", false),
            ("provider", "text", false),
            ("revision", "bigint", false),
            ("service_url", "text", true),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
        ],
        constraints: &[
            ("neev_msteams_installations_pk", "p", &["PRIMARY KEY (tenant_id)"]),
            ("neev_msteams_installations_org_key", "u", &["UNIQUE (tenant_id, org_id)"]),
            ("neev_msteams_installations_tenant", "c", &["length(tenant_id)", "36"]),
            ("neev_msteams_installations_agent", "c", &["length(provider)", "length(model)"]),
            ("neev_msteams_installations_service", "c", &["service_url IS NULL", "512"]),
            ("neev_msteams_installations_revision", "c", &["revision > 0"]),
            ("neev_msteams_installations_org", "f", &["REFERENCES neev_organizations(id) ON DELETE CASCADE"]),
            ("neev_msteams_installations_by", "f", &["(created_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    // `team_id` is the bot's `team.id`; `graph_group_id` is the Team's Entra
    // group id, read from the Bot Connector (never typed by a person), and
    // `verified_owner` is the Entra object id Graph confirmed as a Team owner
    // when an admin linked it.
    Table {
        name: "neev_msteams_project_links",
        ddl: "CREATE TABLE neev_msteams_project_links (org_id integer NOT NULL, tenant_id text NOT NULL, team_id text NOT NULL, graph_group_id text NOT NULL, node_id integer NOT NULL, enabled boolean NOT NULL, verified_owner text NOT NULL, verified_at bigint NOT NULL, revision bigint NOT NULL, created_by integer NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_project_links_pk PRIMARY KEY (tenant_id, team_id), CONSTRAINT neev_msteams_project_links_org_key UNIQUE (tenant_id, team_id, org_id), CONSTRAINT neev_msteams_project_links_group UNIQUE (tenant_id, graph_group_id), CONSTRAINT neev_msteams_project_links_team CHECK (length(team_id) BETWEEN 1 AND 256 AND length(graph_group_id)=36 AND length(verified_owner)=36), CONSTRAINT neev_msteams_project_links_revision CHECK (revision>0), CONSTRAINT neev_msteams_project_links_install FOREIGN KEY (tenant_id, org_id) REFERENCES neev_msteams_installations (tenant_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_project_links_node FOREIGN KEY (node_id, org_id) REFERENCES neev_nodes (id, org_id), CONSTRAINT neev_msteams_project_links_by FOREIGN KEY (created_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("created_at", "bigint", false),
            ("created_by", "integer", false),
            ("enabled", "boolean", false),
            ("graph_group_id", "text", false),
            ("node_id", "integer", false),
            ("org_id", "integer", false),
            ("revision", "bigint", false),
            ("team_id", "text", false),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
            ("verified_at", "bigint", false),
            ("verified_owner", "text", false),
        ],
        constraints: &[
            ("neev_msteams_project_links_pk", "p", &["PRIMARY KEY (tenant_id, team_id)"]),
            ("neev_msteams_project_links_org_key", "u", &["UNIQUE (tenant_id, team_id, org_id)"]),
            ("neev_msteams_project_links_group", "u", &["UNIQUE (tenant_id, graph_group_id)"]),
            ("neev_msteams_project_links_team", "c", &["length(team_id)", "graph_group_id", "verified_owner"]),
            ("neev_msteams_project_links_revision", "c", &["revision > 0"]),
            ("neev_msteams_project_links_install", "f", &["(tenant_id, org_id) REFERENCES neev_msteams_installations(tenant_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_project_links_node", "f", &["(node_id, org_id) REFERENCES neev_nodes(id, org_id)"]),
            ("neev_msteams_project_links_by", "f", &["(created_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    // A channel is enabled only after Graph showed it is a live standard channel
    // with no guests or members from other tenants (`verified_at`).
    Table {
        name: "neev_msteams_channel_links",
        ddl: "CREATE TABLE neev_msteams_channel_links (org_id integer NOT NULL, tenant_id text NOT NULL, team_id text NOT NULL, channel_id text NOT NULL, enabled boolean NOT NULL, verified_at bigint NOT NULL, created_by integer NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_channel_links_pk PRIMARY KEY (tenant_id, channel_id), CONSTRAINT neev_msteams_channel_links_org_key UNIQUE (tenant_id, channel_id, org_id), CONSTRAINT neev_msteams_channel_links_channel CHECK (length(channel_id) BETWEEN 1 AND 256), CONSTRAINT neev_msteams_channel_links_project FOREIGN KEY (tenant_id, team_id, org_id) REFERENCES neev_msteams_project_links (tenant_id, team_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_channel_links_by FOREIGN KEY (created_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("channel_id", "text", false),
            ("created_at", "bigint", false),
            ("created_by", "integer", false),
            ("enabled", "boolean", false),
            ("org_id", "integer", false),
            ("team_id", "text", false),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
            ("verified_at", "bigint", false),
        ],
        constraints: &[
            ("neev_msteams_channel_links_pk", "p", &["PRIMARY KEY (tenant_id, channel_id)"]),
            ("neev_msteams_channel_links_org_key", "u", &["UNIQUE (tenant_id, channel_id, org_id)"]),
            ("neev_msteams_channel_links_channel", "c", &["length(channel_id)", "256"]),
            ("neev_msteams_channel_links_project", "f", &["(tenant_id, team_id, org_id) REFERENCES neev_msteams_project_links(tenant_id, team_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_channel_links_by", "f", &["(created_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    Table {
        name: "neev_msteams_principals",
        ddl: "CREATE TABLE neev_msteams_principals (org_id integer NOT NULL, tenant_id text NOT NULL, aad_object_id text NOT NULL, user_id integer NOT NULL, created_by integer NOT NULL, created_at bigint NOT NULL, CONSTRAINT neev_msteams_principals_pk PRIMARY KEY (tenant_id, aad_object_id), CONSTRAINT neev_msteams_principals_user UNIQUE (tenant_id, user_id), CONSTRAINT neev_msteams_principals_object CHECK (length(aad_object_id)=36), CONSTRAINT neev_msteams_principals_install FOREIGN KEY (tenant_id, org_id) REFERENCES neev_msteams_installations (tenant_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_principals_member FOREIGN KEY (org_id, user_id) REFERENCES neev_memberships (org_id, user_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_principals_by FOREIGN KEY (created_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("aad_object_id", "text", false),
            ("created_at", "bigint", false),
            ("created_by", "integer", false),
            ("org_id", "integer", false),
            ("tenant_id", "text", false),
            ("user_id", "integer", false),
        ],
        constraints: &[
            ("neev_msteams_principals_pk", "p", &["PRIMARY KEY (tenant_id, aad_object_id)"]),
            ("neev_msteams_principals_user", "u", &["UNIQUE (tenant_id, user_id)"]),
            ("neev_msteams_principals_object", "c", &["length(aad_object_id)", "36"]),
            ("neev_msteams_principals_install", "f", &["(tenant_id, org_id) REFERENCES neev_msteams_installations(tenant_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_principals_member", "f", &["(org_id, user_id) REFERENCES neev_memberships(org_id, user_id) ON DELETE CASCADE"]),
            ("neev_msteams_principals_by", "f", &["(created_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    // A thread survives its channel link being DISABLED (links are suspended,
    // never deleted): suspending stops new runs, never erases the audit trail.
    // Every child row is bound to the org through a composite key, so a
    // query that forgot its org filter still cannot write a cross-org row.
    Table {
        name: "neev_msteams_threads",
        ddl: "CREATE TABLE neev_msteams_threads (org_id integer NOT NULL, tenant_id text NOT NULL, team_id text NOT NULL, channel_id text NOT NULL, root_id text NOT NULL, service_url text NOT NULL, started_by integer NOT NULL, live_sequence bigint, next_sequence bigint NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_threads_pk PRIMARY KEY (tenant_id, channel_id, root_id), CONSTRAINT neev_msteams_threads_org_key UNIQUE (tenant_id, channel_id, root_id, org_id), CONSTRAINT neev_msteams_threads_root CHECK (length(root_id) BETWEEN 1 AND 256 AND length(team_id) BETWEEN 1 AND 256 AND length(service_url) BETWEEN 9 AND 512), CONSTRAINT neev_msteams_threads_sequence CHECK (next_sequence>0), CONSTRAINT neev_msteams_threads_live CHECK (live_sequence IS NULL OR (live_sequence>0 AND live_sequence<next_sequence)), CONSTRAINT neev_msteams_threads_channel FOREIGN KEY (tenant_id, channel_id, org_id) REFERENCES neev_msteams_channel_links (tenant_id, channel_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_threads_by FOREIGN KEY (started_by) REFERENCES neev_accounts (id))",
        columns: &[
            ("channel_id", "text", false),
            ("created_at", "bigint", false),
            ("live_sequence", "bigint", true),
            ("next_sequence", "bigint", false),
            ("org_id", "integer", false),
            ("root_id", "text", false),
            ("service_url", "text", false),
            ("started_by", "integer", false),
            ("team_id", "text", false),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
        ],
        constraints: &[
            ("neev_msteams_threads_pk", "p", &["PRIMARY KEY (tenant_id, channel_id, root_id)"]),
            ("neev_msteams_threads_org_key", "u", &["UNIQUE (tenant_id, channel_id, root_id, org_id)"]),
            ("neev_msteams_threads_root", "c", &["length(root_id)", "team_id", "service_url"]),
            ("neev_msteams_threads_sequence", "c", &["next_sequence > 0"]),
            ("neev_msteams_threads_live", "c", &["live_sequence IS NULL", "next_sequence"]),
            ("neev_msteams_threads_channel", "f", &["(tenant_id, channel_id, org_id) REFERENCES neev_msteams_channel_links(tenant_id, channel_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_threads_by", "f", &["(started_by) REFERENCES neev_accounts(id)"]),
        ],
    },
    // The shared CONTEXT of a thread: what people said and the answers Neev
    // actually posted. `author_user_id` is who SAID it; `executed_as` is whose
    // Neev session and connection ran it.
    Table {
        name: "neev_msteams_thread_messages",
        ddl: "CREATE TABLE neev_msteams_thread_messages (org_id integer NOT NULL, tenant_id text NOT NULL, conversation_id text NOT NULL, activity_id text NOT NULL, channel_id text NOT NULL, root_id text NOT NULL, sequence bigint NOT NULL, kind text NOT NULL, author_user_id integer, author_name text NOT NULL, body text NOT NULL, outcome text NOT NULL, executed_as integer, run_id text, created_at bigint NOT NULL, CONSTRAINT neev_msteams_thread_messages_pk PRIMARY KEY (tenant_id, conversation_id, activity_id), CONSTRAINT neev_msteams_thread_messages_order UNIQUE (tenant_id, channel_id, root_id, sequence), CONSTRAINT neev_msteams_thread_messages_kind CHECK (kind IN ('input','answer')), CONSTRAINT neev_msteams_thread_messages_outcome CHECK (outcome IN ('started','queued','refused','answered')), CONSTRAINT neev_msteams_thread_messages_shape CHECK ((kind='input' AND author_user_id IS NOT NULL) OR (kind='answer' AND author_user_id IS NULL AND outcome='answered' AND run_id IS NOT NULL)), CONSTRAINT neev_msteams_thread_messages_run CHECK (run_id IS NULL OR length(run_id)=32), CONSTRAINT neev_msteams_thread_messages_size CHECK (length(body)<=40000 AND length(author_name)<=128 AND length(activity_id) BETWEEN 1 AND 300 AND length(conversation_id) BETWEEN 1 AND 512), CONSTRAINT neev_msteams_thread_messages_sequence CHECK (sequence>0), CONSTRAINT neev_msteams_thread_messages_thread FOREIGN KEY (tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads (tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_thread_messages_author FOREIGN KEY (author_user_id) REFERENCES neev_accounts (id), CONSTRAINT neev_msteams_thread_messages_executor FOREIGN KEY (executed_as) REFERENCES neev_accounts (id))",
        columns: &[
            ("activity_id", "text", false),
            ("author_name", "text", false),
            ("author_user_id", "integer", true),
            ("body", "text", false),
            ("channel_id", "text", false),
            ("conversation_id", "text", false),
            ("created_at", "bigint", false),
            ("executed_as", "integer", true),
            ("kind", "text", false),
            ("org_id", "integer", false),
            ("outcome", "text", false),
            ("root_id", "text", false),
            ("run_id", "text", true),
            ("sequence", "bigint", false),
            ("tenant_id", "text", false),
        ],
        constraints: &[
            ("neev_msteams_thread_messages_pk", "p", &["PRIMARY KEY (tenant_id, conversation_id, activity_id)"]),
            ("neev_msteams_thread_messages_order", "u", &["UNIQUE (tenant_id, channel_id, root_id, sequence)"]),
            ("neev_msteams_thread_messages_kind", "c", &["'input'", "'answer'"]),
            ("neev_msteams_thread_messages_outcome", "c", &["'started'", "'queued'", "'refused'", "'answered'"]),
            ("neev_msteams_thread_messages_shape", "c", &["author_user_id IS NOT NULL", "author_user_id IS NULL"]),
            ("neev_msteams_thread_messages_run", "c", &["run_id IS NULL", "32"]),
            ("neev_msteams_thread_messages_size", "c", &["40000", "128", "300", "512"]),
            ("neev_msteams_thread_messages_sequence", "c", &["sequence > 0"]),
            ("neev_msteams_thread_messages_thread", "f", &["(tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads(tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_thread_messages_author", "f", &["(author_user_id) REFERENCES neev_accounts(id)"]),
            ("neev_msteams_thread_messages_executor", "f", &["(executed_as) REFERENCES neev_accounts(id)"]),
        ],
    },
    // The durable INBOX: every addressed message that passed authorization is
    // committed here BEFORE the HTTP acknowledgement, keyed by the Bot
    // Connector's activity, and carried to a run by the worker (`msteams::worker`)
    // even across a restart. `queued` waits for the thread's live run;
    // `pending` is due to launch; `launched`/`settled` name the run; `refused`
    // records the stable refusal code.
    Table {
        name: "neev_msteams_inbox",
        ddl: "CREATE TABLE neev_msteams_inbox (org_id integer NOT NULL, tenant_id text NOT NULL, conversation_id text NOT NULL, activity_id text NOT NULL, scope text NOT NULL, service_url text NOT NULL, user_id integer NOT NULL, aad_object_id text NOT NULL, author_name text NOT NULL, body text NOT NULL, team_id text, channel_id text, root_id text, sequence bigint, state text NOT NULL, run_id text, refusal text, attempts integer NOT NULL, next_attempt_at bigint NOT NULL, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_inbox_pk PRIMARY KEY (tenant_id, conversation_id, activity_id), CONSTRAINT neev_msteams_inbox_run UNIQUE (org_id, run_id), CONSTRAINT neev_msteams_inbox_scope CHECK ((scope='personal' AND team_id IS NULL AND channel_id IS NULL AND root_id IS NULL AND sequence IS NULL) OR (scope='channel' AND team_id IS NOT NULL AND channel_id IS NOT NULL AND root_id IS NOT NULL AND sequence IS NOT NULL)), CONSTRAINT neev_msteams_inbox_state CHECK ((state IN ('queued','pending') AND run_id IS NULL AND refusal IS NULL) OR (state IN ('launched','settled') AND run_id IS NOT NULL AND refusal IS NULL) OR (state='refused' AND run_id IS NULL AND refusal IS NOT NULL)), CONSTRAINT neev_msteams_inbox_size CHECK (length(body)<=8000 AND length(author_name)<=128 AND length(aad_object_id)=36 AND length(activity_id) BETWEEN 1 AND 300 AND length(conversation_id) BETWEEN 1 AND 512 AND length(service_url) BETWEEN 9 AND 512 AND (run_id IS NULL OR length(run_id)=32) AND (refusal IS NULL OR length(refusal)<=64) AND attempts>=0), CONSTRAINT neev_msteams_inbox_install FOREIGN KEY (tenant_id, org_id) REFERENCES neev_msteams_installations (tenant_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_inbox_thread FOREIGN KEY (tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads (tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_inbox_member FOREIGN KEY (org_id, user_id) REFERENCES neev_memberships (org_id, user_id) ON DELETE CASCADE)",
        columns: &[
            ("aad_object_id", "text", false),
            ("activity_id", "text", false),
            ("attempts", "integer", false),
            ("author_name", "text", false),
            ("body", "text", false),
            ("channel_id", "text", true),
            ("conversation_id", "text", false),
            ("created_at", "bigint", false),
            ("next_attempt_at", "bigint", false),
            ("org_id", "integer", false),
            ("refusal", "text", true),
            ("root_id", "text", true),
            ("run_id", "text", true),
            ("scope", "text", false),
            ("sequence", "bigint", true),
            ("service_url", "text", false),
            ("state", "text", false),
            ("team_id", "text", true),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
            ("user_id", "integer", false),
        ],
        constraints: &[
            ("neev_msteams_inbox_pk", "p", &["PRIMARY KEY (tenant_id, conversation_id, activity_id)"]),
            ("neev_msteams_inbox_run", "u", &["UNIQUE (org_id, run_id)"]),
            ("neev_msteams_inbox_scope", "c", &["'personal'", "'channel'"]),
            ("neev_msteams_inbox_state", "c", &["'queued'", "'launched'", "'refused'"]),
            ("neev_msteams_inbox_size", "c", &["8000", "300", "512", "attempts >= 0"]),
            ("neev_msteams_inbox_install", "f", &["(tenant_id, org_id) REFERENCES neev_msteams_installations(tenant_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_inbox_thread", "f", &["(tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads(tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_inbox_member", "f", &["(org_id, user_id) REFERENCES neev_memberships(org_id, user_id) ON DELETE CASCADE"]),
        ],
    },
    // The durable OUTBOX: one row per message Neev will post, keyed by a digest
    // of (run or activity, kind) so a retried enqueue is a no-op. A `final`
    // row stores NO answer text: the answer is read and its destination
    // re-authorized at send time. `message_id` is the id Teams returned.
    Table {
        name: "neev_msteams_outbox",
        ddl: "CREATE TABLE neev_msteams_outbox (id text NOT NULL, org_id integer NOT NULL, tenant_id text NOT NULL, service_url text NOT NULL, conversation_id text NOT NULL, reply_to_id text NOT NULL, scope text NOT NULL, channel_id text, root_id text, kind text NOT NULL, run_id text, user_id integer NOT NULL, card text, state text NOT NULL, attempts integer NOT NULL, next_attempt_at bigint NOT NULL, message_id text, error_code text, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_outbox_pk PRIMARY KEY (id), CONSTRAINT neev_msteams_outbox_kind CHECK ((kind='notice' AND card IS NOT NULL) OR (kind='final' AND run_id IS NOT NULL AND card IS NULL)), CONSTRAINT neev_msteams_outbox_scope CHECK ((scope='personal' AND channel_id IS NULL AND root_id IS NULL) OR (scope='channel' AND channel_id IS NOT NULL AND root_id IS NOT NULL)), CONSTRAINT neev_msteams_outbox_state CHECK (state IN ('pending','sent','withheld','failed')), CONSTRAINT neev_msteams_outbox_size CHECK (length(id)=64 AND length(service_url) BETWEEN 9 AND 512 AND length(conversation_id) BETWEEN 1 AND 512 AND length(reply_to_id) BETWEEN 1 AND 300 AND (card IS NULL OR length(card)<=60000) AND (run_id IS NULL OR length(run_id)=32) AND (message_id IS NULL OR length(message_id)<=256) AND (error_code IS NULL OR length(error_code)<=64) AND attempts>=0), CONSTRAINT neev_msteams_outbox_install FOREIGN KEY (tenant_id, org_id) REFERENCES neev_msteams_installations (tenant_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_outbox_member FOREIGN KEY (org_id, user_id) REFERENCES neev_memberships (org_id, user_id) ON DELETE CASCADE)",
        columns: &[
            ("attempts", "integer", false),
            ("card", "text", true),
            ("channel_id", "text", true),
            ("conversation_id", "text", false),
            ("created_at", "bigint", false),
            ("error_code", "text", true),
            ("id", "text", false),
            ("kind", "text", false),
            ("message_id", "text", true),
            ("next_attempt_at", "bigint", false),
            ("org_id", "integer", false),
            ("reply_to_id", "text", false),
            ("root_id", "text", true),
            ("run_id", "text", true),
            ("scope", "text", false),
            ("service_url", "text", false),
            ("state", "text", false),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
            ("user_id", "integer", false),
        ],
        constraints: &[
            ("neev_msteams_outbox_pk", "p", &["PRIMARY KEY (id)"]),
            ("neev_msteams_outbox_kind", "c", &["'notice'", "'final'"]),
            ("neev_msteams_outbox_scope", "c", &["'personal'", "'channel'"]),
            ("neev_msteams_outbox_state", "c", &["'pending'", "'withheld'", "'failed'"]),
            ("neev_msteams_outbox_size", "c", &["60000", "256", "attempts >= 0"]),
            ("neev_msteams_outbox_install", "f", &["(tenant_id, org_id) REFERENCES neev_msteams_installations(tenant_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_outbox_member", "f", &["(org_id, user_id) REFERENCES neev_memberships(org_id, user_id) ON DELETE CASCADE"]),
        ],
    },
    // Release 4: one row per generated file of a channel run, published to the
    // channel's SharePoint folder by the worker. `sealed_session` is the Graph
    // upload capability, AES-GCM sealed (`msteams::documents`), kept only
    // while an upload is in flight so a restart RESUMES it instead of
    // creating a second file.
    Table {
        name: "neev_msteams_documents",
        ddl: "CREATE TABLE neev_msteams_documents (id text NOT NULL, org_id integer NOT NULL, tenant_id text NOT NULL, run_id text NOT NULL, attachment_key text NOT NULL, file_name text NOT NULL, byte_length bigint NOT NULL, sha256 text NOT NULL, user_id integer NOT NULL, service_url text NOT NULL, conversation_id text NOT NULL, reply_to_id text NOT NULL, channel_id text NOT NULL, root_id text NOT NULL, state text NOT NULL, sealed_session text, drive_id text, item_id text, web_url text, attempts integer NOT NULL, next_attempt_at bigint NOT NULL, error_code text, created_at bigint NOT NULL, updated_at bigint NOT NULL, CONSTRAINT neev_msteams_documents_pk PRIMARY KEY (id), CONSTRAINT neev_msteams_documents_artifact UNIQUE (org_id, run_id, attachment_key), CONSTRAINT neev_msteams_documents_state CHECK ((state IN ('pending','withheld','failed') AND item_id IS NULL) OR (state='uploading' AND sealed_session IS NOT NULL AND item_id IS NULL) OR (state='published' AND drive_id IS NOT NULL AND item_id IS NOT NULL AND web_url IS NOT NULL AND sealed_session IS NULL)), CONSTRAINT neev_msteams_documents_size CHECK (length(id)=64 AND length(run_id)=32 AND length(attachment_key)=36 AND length(file_name) BETWEEN 1 AND 255 AND byte_length>0 AND length(sha256)=64 AND length(service_url) BETWEEN 9 AND 512 AND length(conversation_id) BETWEEN 1 AND 512 AND length(reply_to_id) BETWEEN 1 AND 300 AND (sealed_session IS NULL OR length(sealed_session)<=8192) AND (web_url IS NULL OR length(web_url)<=2048) AND (error_code IS NULL OR length(error_code)<=64) AND attempts>=0), CONSTRAINT neev_msteams_documents_install FOREIGN KEY (tenant_id, org_id) REFERENCES neev_msteams_installations (tenant_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_documents_thread FOREIGN KEY (tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads (tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE, CONSTRAINT neev_msteams_documents_member FOREIGN KEY (org_id, user_id) REFERENCES neev_memberships (org_id, user_id) ON DELETE CASCADE)",
        columns: &[
            ("attachment_key", "text", false),
            ("attempts", "integer", false),
            ("byte_length", "bigint", false),
            ("channel_id", "text", false),
            ("conversation_id", "text", false),
            ("created_at", "bigint", false),
            ("drive_id", "text", true),
            ("error_code", "text", true),
            ("file_name", "text", false),
            ("id", "text", false),
            ("item_id", "text", true),
            ("next_attempt_at", "bigint", false),
            ("org_id", "integer", false),
            ("reply_to_id", "text", false),
            ("root_id", "text", false),
            ("run_id", "text", false),
            ("sealed_session", "text", true),
            ("service_url", "text", false),
            ("sha256", "text", false),
            ("state", "text", false),
            ("tenant_id", "text", false),
            ("updated_at", "bigint", false),
            ("user_id", "integer", false),
            ("web_url", "text", true),
        ],
        constraints: &[
            ("neev_msteams_documents_pk", "p", &["PRIMARY KEY (id)"]),
            ("neev_msteams_documents_artifact", "u", &["UNIQUE (org_id, run_id, attachment_key)"]),
            ("neev_msteams_documents_state", "c", &["'uploading'", "'published'", "sealed_session"]),
            ("neev_msteams_documents_size", "c", &["byte_length > 0", "8192", "2048"]),
            ("neev_msteams_documents_install", "f", &["(tenant_id, org_id) REFERENCES neev_msteams_installations(tenant_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_documents_thread", "f", &["(tenant_id, channel_id, root_id, org_id) REFERENCES neev_msteams_threads(tenant_id, channel_id, root_id, org_id) ON DELETE CASCADE"]),
            ("neev_msteams_documents_member", "f", &["(org_id, user_id) REFERENCES neev_memberships(org_id, user_id) ON DELETE CASCADE"]),
        ],
    },
];

async fn exists<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, AuthError> {
    let sql = match db.get_database_backend() {
        DbBackend::Postgres => "SELECT 1 AS present FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=current_schema() AND c.relname=$1",
        DbBackend::Sqlite => "SELECT 1 AS present FROM sqlite_master WHERE type='table' AND name=$1",
        _ => return Err(AuthError::Unavailable),
    };
    Ok(super::one(db, sql, vec![table.into()]).await?.is_some())
}

/// Create each table when absent, in dependency order. Idempotent.
pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    for table in &TABLES {
        if !exists(tx, table.name).await? {
            super::execute(tx, table.ddl, vec![]).await?;
        }
    }
    validate(tx).await
}

async fn validate_table<C: ConnectionTrait>(db: &C, table: &Table) -> Result<(), AuthError> {
    match db.get_database_backend() {
        DbBackend::Sqlite => {
            let ddl: String = super::one(
                db,
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=$1",
                vec![table.name.into()],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "sql")
            .map_err(database_error)?;
            if ddl != table.ddl {
                return Err(AuthError::Unavailable);
            }
            let extra = super::one(
                db,
                "SELECT 1 AS extra FROM sqlite_master WHERE tbl_name=$1 AND type<>'table' AND name NOT LIKE 'sqlite_autoindex_%' LIMIT 1",
                vec![table.name.into()],
            )
            .await?;
            if extra.is_some() {
                return Err(AuthError::Unavailable);
            }
        }
        DbBackend::Postgres => {
            let rows = db
                .query_all(super::statement(
                    db,
                    "SELECT column_name::text AS name,data_type::text AS kind,is_nullable::text AS nullable FROM information_schema.columns WHERE table_schema=current_schema() AND table_name=$1 ORDER BY column_name",
                    vec![table.name.into()],
                ))
                .await
                .map_err(database_error)?;
            if rows.len() != table.columns.len() {
                return Err(AuthError::Unavailable);
            }
            for (row, (name, kind, nullable)) in rows.iter().zip(table.columns) {
                let actual: (String, String, String) = (
                    row.try_get("", "name").map_err(database_error)?,
                    row.try_get("", "kind").map_err(database_error)?,
                    row.try_get("", "nullable").map_err(database_error)?,
                );
                let expected = (
                    (*name).to_owned(),
                    (*kind).to_owned(),
                    if *nullable { "YES" } else { "NO" }.to_owned(),
                );
                if actual != expected {
                    return Err(AuthError::Unavailable);
                }
            }
            let relid = format!(
                "(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE c.relname='{}' AND n.nspname=current_schema())",
                table.name
            );
            let count: i64 = super::one(
                db,
                &format!("SELECT COUNT(*) AS n FROM pg_constraint WHERE conrelid={relid}"),
                vec![],
            )
            .await?
            .ok_or(AuthError::Unavailable)?
            .try_get("", "n")
            .map_err(database_error)?;
            if count != table.constraints.len() as i64 {
                return Err(AuthError::Unavailable);
            }
            for (name, kind, fragments) in table.constraints {
                let definition: String = super::one(
                    db,
                    &format!("SELECT pg_get_constraintdef(oid) AS def FROM pg_constraint WHERE conrelid={relid} AND conname=$1 AND contype=$2"),
                    vec![(*name).into(), (*kind).into()],
                )
                .await?
                .ok_or(AuthError::Unavailable)?
                .try_get("", "def")
                .map_err(database_error)?;
                if !fragments.iter().all(|fragment| definition.contains(fragment)) {
                    return Err(AuthError::Unavailable);
                }
            }
            let extra = super::one(
                db,
                &format!("SELECT 1 AS extra FROM pg_index i WHERE i.indrelid={relid} AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid=i.indexrelid) LIMIT 1"),
                vec![],
            )
            .await?;
            if extra.is_some() {
                return Err(AuthError::Unavailable);
            }
        }
        _ => return Err(AuthError::Unavailable),
    }
    Ok(())
}

/// Prove the exact schema-25 shape, and that every stored row is one the
/// service could have written. Safe on a transaction or a pooled connection.
pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    for table in &TABLES {
        validate_table(db, table).await?;
    }
    // A linked project is a project-kind node; a thread and its ledger share
    // the org of their channel's installation; a thread's live claim names one
    // of its own inputs.
    let invalid = super::one(
        db,
        "SELECT 1 AS invalid FROM neev_msteams_project_links l WHERE NOT EXISTS (SELECT 1 FROM neev_nodes n WHERE n.org_id=l.org_id AND n.id=l.node_id AND n.kind='project') \
         UNION ALL SELECT 1 FROM neev_msteams_threads t WHERE NOT EXISTS (SELECT 1 FROM neev_msteams_installations i WHERE i.tenant_id=t.tenant_id AND i.org_id=t.org_id) \
         UNION ALL SELECT 1 FROM neev_msteams_thread_messages m WHERE NOT EXISTS (SELECT 1 FROM neev_msteams_threads t WHERE t.tenant_id=m.tenant_id AND t.channel_id=m.channel_id AND t.root_id=m.root_id AND t.org_id=m.org_id) \
         UNION ALL SELECT 1 FROM neev_msteams_threads t WHERE t.live_sequence IS NOT NULL AND NOT EXISTS (SELECT 1 FROM neev_msteams_thread_messages m WHERE m.tenant_id=t.tenant_id AND m.channel_id=t.channel_id AND m.root_id=t.root_id AND m.sequence=t.live_sequence AND m.kind='input' AND m.outcome='started') \
         LIMIT 1",
        vec![],
    )
    .await?;
    if invalid.is_some() {
        return Err(AuthError::Unavailable);
    }
    Ok(())
}

/// What a 25 -> 24 downgrade did, or why it refused (changing nothing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Downgrade {
    /// The nine tables are gone and the marker is 24; nothing else changed.
    Done,
    /// The marker is not 25.
    NotAtTwentyFive(i64),
    /// Teams bindings or thread history exist. Dropping them would silently
    /// disconnect Teams and erase its audit trail, so the operator must remove
    /// them deliberately first.
    TeamsDataExists { rows: i64 },
}

/// Take a schema-25 database back to 24. GUARDED: refuses unless the marker is
/// exactly 25 and every Teams table is empty. Runs in the caller's transaction;
/// the caller commits only on `Done`.
pub(crate) async fn downgrade(tx: &DatabaseTransaction) -> Result<Downgrade, AuthError> {
    let version: i64 = super::one(tx, "SELECT version FROM neev_schema_version WHERE id=1", vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "version")
        .map_err(database_error)?;
    if version != SCHEMA_VERSION {
        return Ok(Downgrade::NotAtTwentyFive(version));
    }
    validate(tx).await?;
    let sum = TABLE_NAMES
        .iter()
        .map(|name| format!("(SELECT COUNT(*) FROM {name})"))
        .collect::<Vec<_>>()
        .join("+");
    let rows: i64 = super::one(tx, &format!("SELECT {sum} AS n"), vec![])
        .await?
        .ok_or(AuthError::Unavailable)?
        .try_get("", "n")
        .map_err(database_error)?;
    if rows > 0 {
        return Ok(Downgrade::TeamsDataExists { rows });
    }
    for table in TABLES.iter().rev() {
        super::execute(tx, &format!("DROP TABLE {}", table.name), vec![]).await?;
    }
    super::execute(
        tx,
        "UPDATE neev_schema_version SET version=$1 WHERE id=1",
        vec![super::member_sharing_migration::SCHEMA_VERSION.into()],
    )
    .await?;
    for table in TABLES.iter() {
        if exists(tx, table.name).await? {
            return Err(AuthError::Unavailable);
        }
    }
    super::member_sharing_migration::validate(tx).await?;
    Ok(Downgrade::Done)
}
