//! Knowledge test fixtures. Everything goes through the real content, capture
//! and compile paths; raw SQL is used only for states the product cannot
//! produce on demand (e.g. a page already in the wiki).

use super::{capture, jobs, settings, store, types::*};
use crate::neev::{
    agents::execution::{AgentRuntime, FakeRuntime},
    content::{
        documents, messages, resources,
        types::{Resource, ResourceInput, ResourceKind, ResourcePatch, SaveInput, Visibility},
    },
    db,
    startup::ServerState,
    test_support as h,
    types::{NodeInput, NodeKind, OrgContext},
};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use std::sync::Arc;

pub(crate) const NOW: i64 = 1_700_000_000;

pub(crate) struct K {
    pub state: Arc<ServerState>,
    clock: Arc<h::TestClock>,
    /// Org owner, an admin.
    pub admin: OrgContext,
    /// A plain member who creates the sources.
    pub member: OrgContext,
    /// Another org's admin.
    pub foreign: OrgContext,
    pub node: i32,
    pub foreign_node: i32,
}

/// SQLite by default. With `NEEV_KNOWLEDGE_TEST_POSTGRES=1` (and the guarded
/// `NEEV_TEST_DATABASE_URL`), every knowledge test runs unchanged on an
/// isolated PostgreSQL schema instead.
pub(crate) async fn database() -> DatabaseConnection {
    if std::env::var("NEEV_KNOWLEDGE_TEST_POSTGRES").is_ok() {
        let (database, admin, _schema) = h::postgres_database().await;
        // The admin connection only created the schema; the disposable test
        // cluster is dropped wholesale afterwards.
        drop(admin);
        return database;
    }
    h::database().await
}

impl K {
    pub(crate) async fn new(database: DatabaseConnection) -> Self {
        Self::build(database, None, true).await
    }

    pub(crate) async fn with_runtime(database: DatabaseConnection, runtime: FakeRuntime) -> Self {
        Self::build(database, Some(runtime), true).await
    }

    pub(crate) async fn unconfigured(database: DatabaseConnection) -> Self {
        Self::build(database, None, false).await
    }

    /// A fixture on caller-chosen roots (end-to-end backup/restore seeding).
    pub(crate) async fn with_config(database: DatabaseConnection, config: crate::neev::config::Config) -> Self {
        Self::build_on(database, None, config).await
    }

    async fn build(database: DatabaseConnection, runtime: Option<FakeRuntime>, knowledge: bool) -> Self {
        let config = if knowledge { h::knowledge_config() } else { h::preview_config() };
        Self::build_on(database, runtime, config).await
    }

    async fn build_on(database: DatabaseConnection, runtime: Option<FakeRuntime>, config: crate::neev::config::Config) -> Self {
        let clock = Arc::new(h::TestClock::new(NOW));
        let mail = crate::neev::mail::Service::new(database.clone(), Arc::new(h::MailCapture::new()));
        let mut state = ServerState::with_mail(database.clone(), config, clock.clone(), mail).unwrap();
        if let Some(runtime) = runtime {
            state.set_agent_runtime(AgentRuntime::Fake(runtime));
        }
        let state = Arc::new(state);
        let admin_id = h::native_account(&database, "k-admin", "admin@k.test").await;
        let member_id = h::native_account(&database, "k-member", "member@k.test").await;
        let foreign_id = h::native_account(&database, "k-foreign", "foreign@k.test").await;
        let (org, _) = h::organization(&database, admin_id, "admin").await;
        h::add_membership(&database, org, member_id, "member").await;
        let (other, _) = h::organization(&database, foreign_id, "admin").await;
        let admin = h::scoped_context(&database, admin_id, org, NOW).await;
        let member = h::scoped_context(&database, member_id, org, NOW).await;
        let foreign = h::scoped_context(&database, foreign_id, other, NOW).await;
        let node = crate::neev::catalog::nodes::create(
            &state,
            &member,
            NodeInput { parent_id: None, kind: NodeKind::Project, name: "Project".into() },
        )
        .await
        .unwrap()
        .id;
        let foreign_node = crate::neev::catalog::nodes::create(
            &state,
            &foreign,
            NodeInput { parent_id: None, kind: NodeKind::Project, name: "Theirs".into() },
        )
        .await
        .unwrap()
        .id;
        Self { state, clock, admin, member, foreign, node, foreign_node }
    }

    pub(crate) fn now(&self) -> i64 {
        self.clock.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn advance(&self, seconds: i64) -> i64 {
        self.clock.0.fetch_add(seconds, std::sync::atomic::Ordering::SeqCst) + seconds
    }

    pub(crate) async fn resource(&self, kind: ResourceKind, title: &str, visibility: Visibility) -> Resource {
        resources::create_with_visibility(
            &self.state,
            &self.member,
            ResourceInput {
                node_id: self.node,
                kind,
                title: title.into(),
                origin_chat_id: None,
                request_id: uuid::Uuid::new_v4().to_string(),
            },
            Some(visibility),
        )
        .await
        .unwrap()
    }

    pub(crate) async fn shared_chat(&self, title: &str) -> Resource {
        self.resource(ResourceKind::Chat, title, Visibility::Organization).await
    }

    pub(crate) async fn shared_doc(&self, title: &str) -> Resource {
        self.resource(ResourceKind::Document, title, Visibility::Organization).await
    }

    pub(crate) async fn bound_document(&self, chat_id: &str) -> Resource {
        resources::create(
            &self.state,
            &self.member,
            ResourceInput {
                node_id: self.node,
                kind: ResourceKind::Document,
                title: "Generated".into(),
                origin_chat_id: Some(chat_id.to_owned()),
                request_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap()
    }

    pub(crate) async fn current(&self, id: &str) -> Resource {
        resources::get(&self.state, &self.member, id).await.unwrap()
    }

    pub(crate) async fn post(&self, chat_id: &str, text: &str) {
        let chat = self.current(chat_id).await;
        messages::append(&self.state, &self.member, chat_id, &uuid::Uuid::new_v4().to_string(), chat.auth_revision, text)
            .await
            .unwrap();
    }

    pub(crate) async fn save_markdown(&self, doc_id: &str, text: &str) {
        let doc = self.current(doc_id).await;
        documents::save(
            &self.state,
            &self.member,
            doc_id,
            SaveInput {
                request_id: uuid::Uuid::new_v4().to_string(),
                expected_content_revision: doc.content_revision,
                expected_auth_revision: doc.auth_revision,
                markdown: text.into(),
            },
        )
        .await
        .unwrap();
    }

    async fn patch(&self, id: &str, edit: impl FnOnce(&mut ResourcePatch)) {
        let current = self.current(id).await;
        let mut patch = ResourcePatch {
            expected_auth_revision: current.auth_revision,
            title: current.title.clone(),
            node_id: current.node_id,
            visibility: current.visibility,
            archived: current.archived,
        };
        edit(&mut patch);
        resources::update(&self.state, &self.member, id, patch).await.unwrap();
    }

    pub(crate) async fn set_visibility(&self, id: &str, visibility: Visibility) {
        self.patch(id, |p| p.visibility = visibility).await;
    }

    pub(crate) async fn archive(&self, id: &str, archived: bool) {
        self.patch(id, |p| p.archived = archived).await;
    }

    pub(crate) async fn enable_capture(&self) {
        let current = settings::get(&self.state.database, self.admin.org_id).await.unwrap();
        settings::set(&self.state, &self.admin, true, current.revision).await.unwrap();
    }

    /// One capture tick at the current clock (no bridge).
    pub(crate) async fn tick(&self) -> usize {
        capture::run_tick_with(&self.state, None, self.now()).await.unwrap()
    }

    /// Two ticks (debounce) so every stable change is captured.
    pub(crate) async fn capture(&self) -> usize {
        self.tick().await;
        self.advance(1);
        self.tick().await
    }

    pub(crate) async fn connect(&self, actor: &OrgContext, provider: &str) {
        db::execute(
            &self.state.database,
            "INSERT INTO neev_agent_connections(id,org_id,user_id,provider,generation,state,account_label,checked_at,error_code,storage_handle,created_at,updated_at) VALUES($1,$2,$3,$4,1,'connected','acct',1700000000,NULL,$5,1700000000,1700000000)",
            vec![
                uuid::Uuid::new_v4().simple().to_string().into(),
                actor.org_id.into(),
                actor.identity.user_id.into(),
                provider.into(),
                uuid::Uuid::new_v4().simple().to_string().into(),
            ],
        )
        .await
        .unwrap();
    }

    pub(crate) async fn start_job(&self, mode: JobMode) -> jobs::Job {
        jobs::start(&self.state, &self.admin, mode, "claude", "default").await.unwrap()
    }

    pub(crate) async fn scalar_i64(&self, sql: &str, values: Vec<sea_orm::Value>) -> i64 {
        db::one(&self.state.database, sql, values).await.unwrap().unwrap().try_get("", "n").unwrap()
    }

    pub(crate) async fn scalar_text(&self, sql: &str, values: Vec<sea_orm::Value>) -> Option<String> {
        db::one(&self.state.database, sql, values)
            .await
            .unwrap()
            .and_then(|row| row.try_get::<Option<String>>("", "v").unwrap())
    }

    pub(crate) async fn current_snapshots(&self, source_id: &str) -> Vec<Snapshot> {
        let rows = self
            .state
            .database
            .query_all(db::statement(
                &self.state.database,
                &format!("SELECT {} FROM neev_knowledge_snapshots s WHERE s.org_id=$1 AND s.source_id=$2 AND s.state='current' ORDER BY s.segment", super::compile::SNAPSHOT_COLUMNS),
                vec![self.admin.org_id.into(), source_id.to_owned().into()],
            ))
            .await
            .unwrap();
        rows.iter().map(|row| super::compile::snapshot_row(row).unwrap()).collect()
    }

    pub(crate) async fn snapshot_states(&self, source_id: &str) -> (i64, i64) {
        let current = self.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_snapshots WHERE org_id=$1 AND source_id=$2 AND state='current'", vec![self.admin.org_id.into(), source_id.to_owned().into()]).await;
        let superseded = self.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_snapshots WHERE org_id=$1 AND source_id=$2 AND state='superseded'", vec![self.admin.org_id.into(), source_id.to_owned().into()]).await;
        (current, superseded)
    }

    pub(crate) async fn captured_sources(&self) -> Vec<String> {
        let rows = self
            .state
            .database
            .query_all(db::statement(
                &self.state.database,
                "SELECT DISTINCT source_id FROM neev_knowledge_snapshots WHERE org_id=$1 AND state='current' ORDER BY source_id",
                vec![self.admin.org_id.into()],
            ))
            .await
            .unwrap();
        rows.iter().map(|row| row.try_get::<String>("", "source_id").unwrap()).collect()
    }

    /// Every snapshot and revision blob's text, concatenated.
    pub(crate) async fn all_blob_text(&self) -> String {
        let blobs = store::store(&self.state).unwrap();
        let rows = self
            .state
            .database
            .query_all(db::statement(
                &self.state.database,
                "SELECT org_id,blob_key,sha256,byte_length FROM neev_knowledge_snapshots UNION ALL SELECT org_id,blob_key,sha256,byte_length FROM neev_knowledge_revisions",
                vec![],
            ))
            .await
            .unwrap();
        let mut out = String::new();
        for row in &rows {
            let bytes = blobs
                .read(
                    row.try_get("", "org_id").unwrap(),
                    &row.try_get::<String>("", "blob_key").unwrap(),
                    row.try_get("", "byte_length").unwrap(),
                    &row.try_get::<String>("", "sha256").unwrap(),
                )
                .await
                .unwrap();
            out.push_str(&String::from_utf8(bytes).unwrap());
            out.push('\n');
        }
        out
    }

    pub(crate) async fn blob_files(&self) -> usize {
        let root = self.state.config.knowledge_root.clone().unwrap();
        let mut count = 0;
        if let Ok(orgs) = std::fs::read_dir(root) {
            for org in orgs.flatten() {
                if let Ok(files) = std::fs::read_dir(org.path()) {
                    count += files.flatten().filter(|f| f.file_name().to_string_lossy().ends_with(".md")).count();
                }
            }
        }
        count
    }

    pub(crate) async fn source_state(&self, id: &str) -> String {
        self.scalar_text("SELECT state AS v FROM neev_knowledge_sources WHERE org_id=$1 AND source_id=$2", vec![self.admin.org_id.into(), id.to_owned().into()]).await.unwrap()
    }

    pub(crate) async fn coverage(&self, snapshot_id: &str) -> (String, i64) {
        let row = db::one(&self.state.database, "SELECT state,attempts FROM neev_knowledge_coverage WHERE org_id=$1 AND snapshot_id=$2", vec![self.admin.org_id.into(), snapshot_id.to_owned().into()]).await.unwrap().unwrap();
        (row.try_get("", "state").unwrap(), row.try_get::<i32>("", "attempts").unwrap() as i64)
    }

    pub(crate) async fn job(&self, id: &str) -> jobs::Job {
        jobs::load(&self.state.database, self.admin.org_id, id).await.unwrap().unwrap()
    }

    pub(crate) async fn batches(&self, job_id: &str) -> Vec<(String, String, Option<String>)> {
        let rows = self
            .state
            .database
            .query_all(db::statement(
                &self.state.database,
                "SELECT id,state,reason FROM neev_knowledge_batches WHERE org_id=$1 AND job_id=$2 ORDER BY seq",
                vec![self.admin.org_id.into(), job_id.to_owned().into()],
            ))
            .await
            .unwrap();
        rows.iter()
            .map(|row| (row.try_get("", "id").unwrap(), row.try_get("", "state").unwrap(), row.try_get("", "reason").unwrap()))
            .collect()
    }

    /// Wait until no batch of the job is running (the driver settled it).
    pub(crate) async fn settle(&self, job_id: &str) {
        for _ in 0..500 {
            let running = self.scalar_i64("SELECT COUNT(*) AS n FROM neev_knowledge_batches WHERE org_id=$1 AND job_id=$2 AND state='running'", vec![self.admin.org_id.into(), job_id.to_owned().into()]).await;
            if running == 0 && self.state.agent_runs.lock().map(|r| r.is_empty()).unwrap_or(false) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("knowledge batch never settled");
    }

    /// The page row for a slug: `(id, head_revision, status)`.
    pub(crate) async fn page(&self, slug: &str) -> Option<(String, i32, String)> {
        db::one(&self.state.database, "SELECT id,head_revision,status FROM neev_knowledge_pages WHERE org_id=$1 AND slug=$2", vec![self.admin.org_id.into(), slug.into()])
            .await
            .unwrap()
            .map(|row| (row.try_get("", "id").unwrap(), row.try_get("", "head_revision").unwrap(), row.try_get("", "status").unwrap()))
    }

    pub(crate) async fn page_body(&self, slug: &str) -> String {
        let (id, head, _) = self.page(slug).await.unwrap();
        let row = db::one(&self.state.database, "SELECT blob_key,sha256,byte_length FROM neev_knowledge_revisions WHERE org_id=$1 AND page_id=$2 AND revision=$3", vec![self.admin.org_id.into(), id.into(), head.into()]).await.unwrap().unwrap();
        let bytes = store::store(&self.state)
            .unwrap()
            .read(self.admin.org_id, &row.try_get::<String>("", "blob_key").unwrap(), row.try_get("", "byte_length").unwrap(), &row.try_get::<String>("", "sha256").unwrap())
            .await
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    pub(crate) async fn deps(&self, slug: &str, revision: i32) -> std::collections::BTreeSet<String> {
        let (id, _, _) = self.page(slug).await.unwrap();
        let rows = self
            .state
            .database
            .query_all(db::statement(&self.state.database, "SELECT snapshot_id FROM neev_knowledge_revision_deps WHERE org_id=$1 AND page_id=$2 AND revision=$3", vec![self.admin.org_id.into(), id.into(), revision.into()]))
            .await
            .unwrap();
        rows.iter().map(|row| row.try_get::<String>("", "snapshot_id").unwrap()).collect()
    }

    /// A page already in the wiki (as if written by an earlier accepted batch
    /// of a finished job), depending on the CURRENT snapshots of `sources`.
    pub(crate) async fn seed_page(&self, slug: &str, title: &str, body: &str, sources: &[&str]) -> String {
        let org = self.admin.org_id;
        let now = self.now();
        let job = uuid::Uuid::new_v4().to_string();
        let batch = uuid::Uuid::new_v4().to_string();
        let page = uuid::Uuid::new_v4().to_string();
        let database = &self.state.database;
        db::execute(database, "INSERT INTO neev_knowledge_jobs(org_id,id,mode,state,started_by,provider,model,pause_reason,session_id,wait_reason,created_at,updated_at) VALUES($1,$2,'incremental','completed',$3,'claude','default',NULL,NULL,NULL,$4,$4)", vec![org.into(), job.clone().into(), self.admin.identity.user_id.into(), now.into()]).await.unwrap();
        db::execute(database, "INSERT INTO neev_knowledge_batches(org_id,id,job_id,seq,state,turn_id,reason,provider,model,instructions_sha256,created_at,started_at,settled_at) VALUES($1,$2,$3,1,'accepted',NULL,NULL,'claude','default','seed',$4,$4,$4)", vec![org.into(), batch.clone().into(), job.into(), now.into()]).await.unwrap();
        db::execute(database, "INSERT INTO neev_knowledge_pages(org_id,id,slug,title,head_revision,status,created_at,updated_at) VALUES($1,$2,$3,$4,NULL,'active',$5,$5)", vec![org.into(), page.clone().into(), slug.into(), title.into(), now.into()]).await.unwrap();
        let key = store::new_key();
        let info = store::store(&self.state).unwrap().put(org, &key, body.as_bytes()).await.unwrap();
        db::execute(database, "INSERT INTO neev_knowledge_revisions(org_id,page_id,revision,blob_key,sha256,byte_length,parent_revision,batch_id,created_at) VALUES($1,$2,1,$3,$4,$5,NULL,$6,$7)", vec![org.into(), page.clone().into(), key.into(), info.sha256.into(), info.byte_length.into(), batch.into(), now.into()]).await.unwrap();
        for source in sources {
            for snapshot in self.current_snapshots(source).await {
                db::execute(database, "INSERT INTO neev_knowledge_revision_deps(org_id,page_id,revision,snapshot_id) VALUES($1,$2,1,$3)", vec![org.into(), page.clone().into(), snapshot.id.into()]).await.unwrap();
            }
        }
        db::execute(database, "UPDATE neev_knowledge_pages SET head_revision=1 WHERE org_id=$1 AND id=$2", vec![org.into(), page.clone().into()]).await.unwrap();
        page
    }
}

/// Labels named by the staged unit files of a request (`k-u01` → `u01`).
pub(crate) fn staged_labels(request: &crate::neev::agents::execution::RunRequest) -> Vec<String> {
    request
        .context_files
        .iter()
        .filter_map(|file| file.key.strip_prefix("k-u").map(|n| format!("u{n}")))
        .collect()
}

/// The text of a staged context file by key.
pub(crate) fn staged_text(request: &crate::neev::agents::execution::RunRequest, key: &str) -> Option<String> {
    request
        .context_files
        .iter()
        .find(|file| file.key == key)
        .map(|file| String::from_utf8_lossy(&file.bytes).into_owned())
}
