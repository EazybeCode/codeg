use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseConnection};

use super::{
    session_fork::{self, SessionFork},
    sessions, transcript,
};
use crate::neev::{
    content::{attachment, fork::ForkError, resources, types::ContentError},
    db,
    startup::ServerState,
    test_support as h,
    types::OrgContext,
};

struct Fixture {
    state: Arc<ServerState>,
    owner: OrgContext,
    member: OrgContext,
    foreign: OrgContext,
}

async fn fixture(database: DatabaseConnection) -> Fixture {
    let clock = Arc::new(h::TestClock::new(1_700_000_000));
    let mail = crate::neev::mail::Service::new(database.clone(), Arc::new(h::MailCapture::new()));
    let state =
        Arc::new(ServerState::with_mail(database.clone(), h::preview_config(), clock, mail).unwrap());
    let owner = h::native_account(&database, "fork-owner", "owner@example.test").await;
    let member = h::native_account(&database, "fork-member", "member@example.test").await;
    let foreign = h::native_account(&database, "fork-foreign", "foreign@example.test").await;
    let (org, _) = h::organization(&database, owner, "admin").await;
    h::add_membership(&database, org, member, "member").await;
    let (other, _) = h::organization(&database, foreign, "admin").await;
    Fixture {
        owner: h::scoped_context(&database, owner, org, 1_700_000_000).await,
        member: h::scoped_context(&database, member, org, 1_700_000_000).await,
        foreign: h::scoped_context(&database, foreign, other, 1_700_000_000).await,
        state,
    }
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn exec(f: &Fixture, sql: &str, values: Vec<sea_orm::Value>) {
    db::execute(&f.state.database, sql, values).await.unwrap();
}

async fn session(f: &Fixture) -> String {
    let id = uuid();
    exec(
        f,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,effort,fast,visibility,kind,bound_chat_id,bound_kind,node_id,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Research','claude','default',NULL,false,'private','chat',NULL,NULL,NULL,false,1,1,1)",
        vec![f.owner.org_id.into(), id.clone().into(), f.owner.identity.user_id.into()],
    )
    .await;
    id
}

/// A turn row as the run driver leaves it.
async fn turn(f: &Fixture, session: &str, sequence: i64, prompt: &str, response: &str) -> String {
    let id = uuid();
    exec(
        f,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,activity,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,NULL,$6,$7,NULL,'completed',NULL,'claude','default',$8,$8,$8)",
        vec![
            f.owner.org_id.into(),
            session.into(),
            sequence.into(),
            id.clone().into(),
            uuid().into(),
            prompt.into(),
            response.into(),
            (100 + sequence).into(),
        ],
    )
    .await;
    id
}

async fn link(f: &Fixture, session: &str, sequence: i64, key: &str) {
    let tx = resources::begin(&f.state, &f.owner).await.unwrap();
    attachment::link_to_turn(&tx, &f.owner, session, sequence, &[key.to_owned()])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Turn 1 completed with a file, turn 2 failed, turn 3 completed with a
/// generated output linked from its response and an accepted follow-up, turn 4
/// completed after the selection.
async fn history(f: &Fixture) -> (String, Vec<String>, String) {
    let source = session(f).await;
    let upload = attachment::store(
        &f.state,
        &f.owner,
        attachment::Owner::Session(&source),
        "data.csv",
        b"a,b\n1,2\n",
    )
    .await
    .unwrap();
    let output = attachment::store(
        &f.state,
        &f.owner,
        attachment::Owner::Session(&source),
        "chart.txt",
        b"chart\n",
    )
    .await
    .unwrap();
    let t1 = turn(f, &source, 1, "Summarise data.csv", "It has two columns.").await;
    link(f, &source, 1, &upload.key).await;
    let t2 = turn(f, &source, 2, "Try again", "").await;
    exec(f, "UPDATE neev_agent_turns SET status='failed',error_code='unavailable' WHERE org_id=$1 AND id=$2", vec![f.owner.org_id.into(), t2.clone().into()]).await;
    let t3 = turn(
        f,
        &source,
        3,
        "Chart it",
        &format!("Done: [chart](/api/neev/attachments/{})", output.key),
    )
    .await;
    link(f, &source, 3, &output.key).await;
    exec(
        f,
        "INSERT INTO neev_steer_inputs(org_id,session_id,turn_sequence,request_id,text,accepted,response_offset,created_at) VALUES($1,$2,3,$3,'Use bars',true,4,150)",
        vec![f.owner.org_id.into(), source.clone().into(), uuid().into()],
    )
    .await;
    let t4 = turn(f, &source, 4, "And now?", "Later answer.").await;
    (source, vec![t1, t2, t3, t4], output.key)
}

async fn rows(f: &Fixture, session: &str) -> Vec<(i64, String, String, String)> {
    f.state
        .database
        .query_all(db::statement(
            &f.state.database,
            "SELECT sequence,prompt,response,status FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2 ORDER BY sequence",
            vec![f.owner.org_id.into(), session.into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.try_get("", "sequence").unwrap(),
                row.try_get("", "prompt").unwrap(),
                row.try_get("", "response").unwrap(),
                row.try_get("", "status").unwrap(),
            )
        })
        .collect()
}

async fn session_fork(database: DatabaseConnection) {
    let f = fixture(database).await;
    let (source, turns, output) = history(&f).await;
    let before = rows(&f, &source).await;
    let done: SessionFork =
        session_fork::fork_session(&f.state, &f.owner, &source, &turns[2], &uuid())
            .await
            .unwrap();
    assert_eq!(
        (done.turns, done.attachments, done.skipped_turns, done.replayed),
        (2, 2, 1, false)
    );

    // Completed turns up through the selection, renumbered in order; the
    // failed one and the one after the selection are not there.
    let copied = rows(&f, &done.session_id).await;
    assert_eq!(copied.len(), 2);
    assert_eq!((copied[0].0, copied[0].1.as_str()), (1, "Summarise data.csv"));
    assert_eq!((copied[1].0, copied[1].1.as_str()), (2, "Chart it"));
    assert!(copied.iter().all(|row| row.3 == "completed"));
    // The response links to the fork's COPY of the generated file.
    let fresh: String = db::one(
        &f.state.database,
        "SELECT key FROM neev_attachments WHERE org_id=$1 AND session_id=$2 AND file_name='chart.txt'",
        vec![f.owner.org_id.into(), done.session_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "key")
    .unwrap();
    assert_ne!(fresh, output);
    assert_eq!(copied[1].2, format!("Done: [chart](/api/neev/attachments/{fresh})"));
    let (bytes, _, _) = attachment::load(&f.state, &f.owner, &fresh).await.unwrap();
    assert_eq!(bytes, b"chart\n");
    // The accepted follow-up came along, on the renumbered turn.
    let steer: String = db::one(
        &f.state.database,
        "SELECT text FROM neev_steer_inputs WHERE org_id=$1 AND session_id=$2 AND turn_sequence=2",
        vec![f.owner.org_id.into(), done.session_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "text")
    .unwrap();
    assert_eq!(steer, "Use bars");

    // Untouched original.
    assert_eq!(rows(&f, &source).await, before);

    // Private: the owner's alone.
    for actor in [&f.member, &f.foreign] {
        assert_eq!(
            sessions::recent_turns(&f.state, actor, &done.session_id).await.err(),
            Some(ContentError::NotFound)
        );
        assert_eq!(
            attachment::load(&f.state, actor, &fresh).await.err(),
            Some(ContentError::NotFound)
        );
    }

    // It continues: what the next run is given IS the copied history.
    let context = sessions::recent_turns(&f.state, &f.owner, &done.session_id).await.unwrap();
    let prompts: Vec<&str> = context.iter().map(|turn| turn.prompt.as_str()).collect();
    assert!(prompts.contains(&"Summarise data.csv") && prompts.contains(&"Chart it"));
    assert!(!prompts.contains(&"And now?"));
    let bundle = transcript::build(
        &f.state,
        &f.owner,
        transcript::Source::Session { session_id: &done.session_id },
        &[],
    )
    .await
    .unwrap();
    assert!(bundle.manifest.carried.iter().any(|name| name == "data.csv"));

    assert_eq!(
        session_fork::session_fork_source(&f.state, &f.owner, &done.session_id).await.unwrap(),
        Some(source.clone())
    );
}

#[tokio::test]
async fn an_agent_chat_fork_copies_completed_turns_files_and_follow_ups() {
    session_fork(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_an_agent_chat_fork_copies_completed_turns_files_and_follow_ups() {
    let (database, admin, schema) = h::postgres_database().await;
    session_fork(database.clone()).await;
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
async fn only_the_owner_may_fork_and_only_at_a_completed_turn() {
    let f = fixture(h::database().await).await;
    let (source, turns, _) = history(&f).await;
    assert_eq!(
        session_fork::fork_session(&f.state, &f.owner, &source, &turns[1], &uuid()).await,
        Err(ForkError::NotComplete),
        "a failed turn is not a reply"
    );
    let running = turn(&f, &source, 5, "Still going", "").await;
    exec(&f, "UPDATE neev_agent_turns SET status='running',completed_at=NULL WHERE org_id=$1 AND id=$2", vec![f.owner.org_id.into(), running.clone().into()]).await;
    assert_eq!(
        session_fork::fork_session(&f.state, &f.owner, &source, &running, &uuid()).await,
        Err(ForkError::NotComplete)
    );
    for actor in [&f.member, &f.foreign] {
        assert_eq!(
            session_fork::fork_session(&f.state, actor, &source, &turns[0], &uuid()).await,
            Err(ForkError::Content(ContentError::NotFound))
        );
    }
    assert_eq!(
        session_fork::fork_session(&f.state, &f.owner, &source, &uuid(), &uuid()).await,
        Err(ForkError::Content(ContentError::NotFound))
    );
}

#[tokio::test]
async fn a_retried_session_fork_is_the_same_fork() {
    let f = fixture(h::database().await).await;
    let (source, turns, _) = history(&f).await;
    let id = uuid();
    let done = session_fork::fork_session(&f.state, &f.owner, &source, &turns[0], &id)
        .await
        .unwrap();
    let again = session_fork::fork_session(&f.state, &f.owner, &source, &turns[0], &id)
        .await
        .unwrap();
    assert_eq!((again.session_id.as_str(), again.replayed, again.turns), (done.session_id.as_str(), true, 1));
    let forks: i64 = db::one(
        &f.state.database,
        "SELECT COUNT(*) AS n FROM neev_agent_sessions WHERE org_id=$1 AND title='Research (fork)'",
        vec![f.owner.org_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap();
    assert_eq!(forks, 1);
    assert_eq!(
        session_fork::fork_session(&f.state, &f.owner, &source, &turns[2], &id).await,
        Err(ForkError::Content(ContentError::Conflict))
    );
}

#[tokio::test]
async fn a_full_agent_chat_is_refused_so_every_fork_can_take_its_next_turn() {
    let f = fixture(h::database().await).await;
    let source = session(&f).await;
    let mut last = String::new();
    for sequence in 1..=500 {
        last = turn(&f, &source, sequence, "Ask", "Answer").await;
    }
    // 500 completed turns: the fork would refuse its own next turn.
    assert_eq!(
        session_fork::fork_session(&f.state, &f.owner, &source, &last, &uuid()).await,
        Err(ForkError::TooLarge)
    );
    // One failed turn among them: 499 are copied and the fork can continue.
    exec(
        &f,
        "UPDATE neev_agent_turns SET status='failed',error_code='unavailable' WHERE org_id=$1 AND session_id=$2 AND sequence=7",
        vec![f.owner.org_id.into(), source.clone().into()],
    )
    .await;
    let done = session_fork::fork_session(&f.state, &f.owner, &source, &last, &uuid())
        .await
        .unwrap();
    assert_eq!((done.turns, done.skipped_turns), (499, 1));
    let highest: i64 = db::one(
        &f.state.database,
        "SELECT MAX(sequence) AS n FROM neev_agent_turns WHERE org_id=$1 AND session_id=$2",
        vec![f.owner.org_id.into(), done.session_id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "n")
    .unwrap();
    assert!(highest < 500, "room for the next turn");
}
