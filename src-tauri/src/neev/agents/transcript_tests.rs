//! What a run is allowed to remember, and what it must never reach for.
//!
//! The first three cases are the reported failure, written as tests: a long
//! message survives whole into the transcript, a file sent on an earlier turn is
//! readable on the next one without being sent again, and a conversation the
//! actor has no right to refuses outright.

use super::*;
use crate::neev::agents::test_support::Harness;
use crate::neev::{content::attachment::Owner, db, test_support as h, types::OrgContext};
use sea_orm::DatabaseConnection;

const CHAT: &str = "55555555-5555-4555-8555-555555555555";
const OTHER_CHAT: &str = "66666666-6666-4666-8666-666666666666";
const SESSION: &str = "77777777-7777-4777-8777-777777777777";
const OTHER_SESSION: &str = "88888888-8888-4888-8888-888888888888";

async fn node(database: &DatabaseConnection, org: i32, owner: i32) -> i32 {
    db::one(
        database,
        "INSERT INTO neev_nodes(org_id,parent_id,kind,name,revision,created_by) VALUES($1,NULL,'project','Project',1,$2) RETURNING id",
        vec![org.into(), owner.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "id")
    .unwrap()
}

async fn chat(
    database: &DatabaseConnection,
    org: i32,
    id: &str,
    node_id: i32,
    creator: i32,
    visibility: &str,
) {
    db::execute(
        database,
        "INSERT INTO neev_resources(org_id,id,kind,node_id,creator_id,title,visibility,origin_chat_id,origin_kind,auth_revision,content_revision,archived,created_at,updated_at) VALUES($1,$2,'chat',$3,$4,'Thread',$5,NULL,NULL,1,1,false,1000,1000)",
        vec![
            org.into(),
            id.into(),
            node_id.into(),
            creator.into(),
            visibility.into(),
        ],
    )
    .await
    .unwrap();
}

async fn message(
    database: &DatabaseConnection,
    org: i32,
    chat_id: &str,
    author: i32,
    sequence: i64,
    id: &str,
    body: &str,
) {
    db::execute(
        database,
        "INSERT INTO neev_messages(org_id,chat_id,resource_kind,id,sequence,author_id,body,byte_length,created_at) VALUES($1,$2,'chat',$3,$4,$5,$6,$7,1000)",
        vec![
            org.into(),
            chat_id.into(),
            id.into(),
            sequence.into(),
            author.into(),
            body.into(),
            (body.len() as i64).into(),
        ],
    )
    .await
    .unwrap();
}

/// A saved Agent Chat, private to its owner — the only visibility the schema
/// permits, and the reason a session's history is the owner's alone.
async fn session(database: &DatabaseConnection, org: i32, id: &str, owner: i32) {
    db::execute(
        database,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,'Agent chat','claude','default','private','chat',NULL,NULL,false,1,1000,1000)",
        vec![org.into(), id.into(), owner.into()],
    )
    .await
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn turn(
    database: &DatabaseConnection,
    org: i32,
    session_id: &str,
    sequence: i64,
    id: &str,
    prompt: &str,
    response: &str,
    status: &str,
    error_code: Option<&str>,
) {
    db::execute(
        database,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,NULL,$6,$7,$8,$9,'claude','default',1000,1000,1000)",
        vec![
            org.into(),
            session_id.into(),
            sequence.into(),
            id.into(),
            format!("run-{sequence}-{session_id}").into(),
            prompt.into(),
            response.into(),
            status.into(),
            error_code.map(str::to_owned).into(),
            ],
    )
    .await
    .unwrap();
}

/// Upload a file into a conversation the way the HTTP surface does, so the
/// reference rows and the blob both exist exactly as in production.
async fn upload(
    harness: &Harness,
    actor: &OrgContext,
    owner: Owner<'_>,
    name: &str,
    body: &str,
) -> String {
    crate::neev::content::attachment::store(&harness.state, actor, owner, name, body.as_bytes())
        .await
        .expect("upload")
        .key
}

async fn link_turn(harness: &Harness, actor: &OrgContext, session_id: &str, seq: i64, key: &str) {
    let tx = db::begin(&harness.state.database).await.unwrap();
    crate::neev::content::attachment::link_to_turn(
        &tx,
        actor,
        session_id,
        seq,
        std::slice::from_ref(&key.to_owned()),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn transcript_of(bundle: &Bundle) -> String {
    String::from_utf8(
        bundle
            .files
            .iter()
            .find(|file| file.file_name == TRANSCRIPT_NAME)
            .expect("the transcript is always staged")
            .bytes
            .clone(),
    )
    .unwrap()
}

/// The reported bug. A 1343-character message whose tenth point sits past the
/// briefing's 600-character per-message budget reached the provider ending at
/// point 5, and the model reported the rest as unavailable. The transcript is
/// what makes that impossible: the message is there in full, with its last point
/// present, and the file says nothing was shortened.
#[tokio::test]
async fn a_long_message_reaches_the_run_whole_including_its_last_point() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    let project = node(&database, harness.org_a, harness.alice_id).await;
    chat(
        &database,
        harness.org_a,
        CHAT,
        project,
        harness.alice_id,
        "organization",
    )
    .await;
    // Shaped like the real one: ten numbered points, the tenth well past the
    // per-message budget the briefing applies.
    let mut body = String::new();
    for point in 1..=10 {
        body.push_str(&format!(
            "point {point}: {}\n",
            "a considered sentence about the matter at hand. ".repeat(3)
        ));
    }
    assert!(
        body.chars().count() > 1_000,
        "the fixture must exceed the briefing's per-message budget to be a test of anything"
    );
    message(
        &database,
        harness.org_a,
        CHAT,
        harness.alice_id,
        1,
        "aaaaaaaa-1111-4111-8111-111111111111",
        &body,
    )
    .await;
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Chat { chat_id: CHAT },
        &[],
    )
    .await
    .expect("a readable chat always has a transcript");
    let text = transcript_of(&bundle);
    assert!(
        text.contains("point 10:"),
        "the tenth point is present — this is the whole bug"
    );
    assert!(
        text.contains("point 5:") && text.contains("point 1:"),
        "and so is everything before it"
    );
    assert!(
        !text.contains('…'),
        "nothing inside an entry is shortened, so no ellipsis appears"
    );
    assert_eq!(bundle.manifest.omitted, 0);
    assert!(
        text.contains("Nothing has been omitted"),
        "and the file says so, in terms the model can act on"
    );
}

/// The second half of the report: a file sent on one turn had to be sent again
/// to be readable on the next, because a run resolved only the CURRENT request's
/// keys. It is now staged from the conversation itself.
#[tokio::test]
async fn a_file_sent_on_an_earlier_turn_is_readable_on_the_next_one() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    let key = upload(
        &harness,
        &harness.alice_a,
        Owner::Session(SESSION),
        "report.html",
        "<h1>the earlier artifact</h1>",
    )
    .await;
    // The TURN first, then the reference to it. A reference may not exist
    // without the turn it belongs to — production commits both in one
    // transaction, with the turn row written before `link_to_turn` — and a
    // fixture that inverts the order is testing a state the schema forbids.
    turn(
        &database,
        harness.org_a,
        SESSION,
        1,
        "bbbbbbbb-1111-4111-8111-111111111111",
        "here is the page",
        "noted",
        "completed",
        None,
    )
    .await;
    link_turn(&harness, &harness.alice_a, SESSION, 1, &key).await;
    // The NEXT turn asks about it and attaches nothing.
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        &[],
    )
    .await
    .expect("an owned session always has a transcript");
    let carried = bundle
        .files
        .iter()
        .find(|file| file.file_name == "report.html")
        .expect("the earlier upload is staged for this turn");
    assert_eq!(carried.bytes, b"<h1>the earlier artifact</h1>");
    assert!(
        !carried.is_image,
        "carried files are read, never re-sent as native inputs"
    );
    assert_eq!(bundle.manifest.carried_total, 1);
    assert_eq!(bundle.manifest.carried_omitted, 0);
    assert!(!bundle.manifest.carried_unavailable);
    assert_eq!(bundle.manifest.carried, vec!["report.html".to_owned()]);
}

/// A file attached to THIS turn is staged by the attachment path, under
/// `<attached-files>`. Carrying it a second time would spend the budget to say
/// the same thing twice and would blur which files just arrived.
#[tokio::test]
async fn this_turns_own_upload_is_not_carried_a_second_time() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    let key = upload(
        &harness,
        &harness.alice_a,
        Owner::Session(SESSION),
        "fresh.txt",
        "just sent",
    )
    .await;
    // The turn this file arrived on, which its reference requires.
    turn(
        &database,
        harness.org_a,
        SESSION,
        1,
        "bbbbbbbb-3333-4333-8333-333333333333",
        "look at this",
        "looking",
        "completed",
        None,
    )
    .await;
    link_turn(&harness, &harness.alice_a, SESSION, 1, &key).await;
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        std::slice::from_ref(&key),
    )
    .await
    .expect("transcript");
    assert!(
        bundle
            .files
            .iter()
            .all(|file| file.file_name != "fresh.txt"),
        "excluded because it is already staged as this turn's own upload"
    );
    assert_eq!(bundle.manifest.carried_total, 0);
    assert_eq!(
        bundle.manifest.carried_omitted, 0,
        "excluded is not omitted: nothing is missing from this run"
    );
}

/// Another member's session is not readable, and the transcript proves that for
/// itself rather than trusting that a caller checked.
#[tokio::test]
async fn another_members_session_has_no_transcript() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, OTHER_SESSION, harness.alice_id).await;
    turn(
        &database,
        harness.org_a,
        OTHER_SESSION,
        1,
        "cccccccc-1111-4111-8111-111111111111",
        "alice's private question",
        "alice's private answer",
        "completed",
        None,
    )
    .await;
    assert!(
        build(
            &harness.state,
            &harness.bob_a,
            Source::Session {
                session_id: OTHER_SESSION,
            },
            &[],
        )
        .await
        .is_none(),
        "bob gets no transcript of alice's agent chat"
    );
}

/// A private chat is not readable by another member, however much of the org
/// they are in — the same predicate the content routes apply.
#[tokio::test]
async fn another_members_private_chat_has_no_transcript() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    let project = node(&database, harness.org_a, harness.alice_id).await;
    chat(
        &database,
        harness.org_a,
        OTHER_CHAT,
        project,
        harness.alice_id,
        "private",
    )
    .await;
    message(
        &database,
        harness.org_a,
        OTHER_CHAT,
        harness.alice_id,
        1,
        "dddddddd-1111-4111-8111-111111111111",
        "a private matter",
    )
    .await;
    assert!(
        build(
            &harness.state,
            &harness.bob_a,
            Source::Chat {
                chat_id: OTHER_CHAT,
            },
            &[],
        )
        .await
        .is_none(),
        "a private chat briefs nobody but its creator"
    );
}

/// A conversation in another organization is not reachable even by the same
/// person, because the scope is the org context, not the human.
#[tokio::test]
async fn a_conversation_in_another_org_has_no_transcript() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    assert!(
        build(
            &harness.state,
            // Alice, in her OTHER organization.
            &harness.alice_b,
            Source::Session {
                session_id: SESSION,
            },
            &[],
        )
        .await
        .is_none(),
        "same person, different tenant, no history"
    );
}

/// A file belonging to a DIFFERENT conversation is never carried into this one,
/// even when the same actor may read both.
#[tokio::test]
async fn a_file_from_another_conversation_is_never_carried_in() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    session(&database, harness.org_a, OTHER_SESSION, harness.alice_id).await;
    let elsewhere = upload(
        &harness,
        &harness.alice_a,
        Owner::Session(OTHER_SESSION),
        "elsewhere.txt",
        "another conversation's file",
    )
    .await;
    // The other session's turn, so its reference has the parent the schema
    // requires — and so this is a real conversation being kept out, not an
    // orphan reference that would never have existed.
    turn(
        &database,
        harness.org_a,
        OTHER_SESSION,
        1,
        "bbbbbbbb-4444-4444-8444-444444444444",
        "a question over there",
        "an answer over there",
        "completed",
        None,
    )
    .await;
    link_turn(&harness, &harness.alice_a, OTHER_SESSION, 1, &elsewhere).await;
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        &[],
    )
    .await
    .expect("transcript");
    assert_eq!(
        bundle.manifest.carried_total, 0,
        "the other session's file stays in the other session"
    );
    assert!(bundle
        .files
        .iter()
        .all(|file| file.file_name != "elsewhere.txt"));
}

/// An unsent upload is not part of the conversation. It is a draft, and a draft
/// the reader abandoned must not arrive in an answer.
#[tokio::test]
async fn an_unsent_upload_is_not_carried() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    upload(
        &harness,
        &harness.alice_a,
        Owner::Session(SESSION),
        "draft.txt",
        "never sent",
    )
    .await;
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        &[],
    )
    .await
    .expect("transcript");
    assert_eq!(bundle.manifest.carried_total, 0);
    assert!(bundle
        .files
        .iter()
        .all(|file| file.file_name != "draft.txt"));
}

/// A turn that produced no trustworthy answer is RECORDED — the reader can see
/// it happened — but its partial text is not reproduced, which is the same rule
/// the prompt composer applies.
#[tokio::test]
async fn an_unfinished_turn_is_recorded_without_reproducing_its_partial_text() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    turn(
        &database,
        harness.org_a,
        SESSION,
        1,
        "eeeeeeee-1111-4111-8111-111111111111",
        "the question that was interrupted",
        "half an answ",
        "canceled",
        Some("canceled"),
    )
    .await;
    let text = transcript_of(
        &build(
            &harness.state,
            &harness.alice_a,
            Source::Session {
                session_id: SESSION,
            },
            &[],
        )
        .await
        .expect("transcript"),
    );
    assert!(
        text.contains("the question that was interrupted"),
        "the turn happened and the transcript says so"
    );
    assert!(
        text.contains("canceled"),
        "with the reason no reply came"
    );
    assert!(
        !text.contains("half an answ"),
        "but never the partial text, which would invent context"
    );
}

/// The transcript is a file, not a row: nothing about it is stored, and it must
/// never appear as if someone had uploaded it.
#[tokio::test]
async fn staging_a_transcript_stores_nothing() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    turn(
        &database,
        harness.org_a,
        SESSION,
        1,
        "ffffffff-1111-4111-8111-111111111111",
        "a question",
        "an answer",
        "completed",
        None,
    )
    .await;
    build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        &[],
    )
    .await
    .expect("transcript");
    let rows: i64 = db::one(
        &database,
        "SELECT COUNT(*) AS c FROM neev_attachments WHERE org_id=$1",
        vec![harness.org_a.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "c")
    .unwrap();
    assert_eq!(rows, 0, "no attachment row, no quota spent, no upload");
}

/// The byte ceiling drops whole OLD entries and says how many. What it must
/// never do is shorten an entry it keeps — that is the failure being fixed.
#[test]
fn a_transcript_over_its_ceiling_drops_old_entries_and_says_so() {
    let big = "x".repeat(MAX_TRANSCRIPT_BYTES / 3);
    let entries: Vec<Entry> = (1..=6)
        .map(|index| entry(&format!("## Message {index}\n\n{big}\n")))
        .collect();
    let (text, written) = render(&entries, entries.len(), Source::Chat { chat_id: CHAT });
    assert!(
        written < entries.len(),
        "the ceiling must actually bite for this to test anything"
    );
    assert!(text.len() <= MAX_TRANSCRIPT_BYTES, "and must be respected");
    assert!(
        text.contains(&format!("{} oldest are omitted", entries.len() - written)),
        "the file states the omission with a real number"
    );
    assert!(
        text.contains("reproduced in full"),
        "and says that what remains is not shortened"
    );
    // Entries arrive newest-first from the database and must read oldest-first
    // in the file. The fixture is in that arrival order, so index 1 is the
    // newest: the kept set is 1..=written, and the file must open with the
    // LAST of those — the oldest one kept.
    let first = text.find("## Message").expect("a heading");
    assert!(
        text[first..].starts_with(&format!("## Message {written}")),
        "the oldest kept entry comes first, so the file reads like a conversation"
    );
}

/// The v63 artifact, at its real size. A 22 MiB HTML page is an ordinary thing
/// for this workspace to hold — the tenant's own quota admits it — and an
/// earlier draft of this module capped carried bytes at 16 MiB, which would have
/// silently dropped exactly the file the reader was asking about. The bound that
/// applies is the tenant's storage bound and nothing smaller.
///
/// Deliberately heavy: it is the only way to test the size that broke.
#[tokio::test]
async fn a_file_larger_than_sixteen_mebibytes_is_still_carried() {
    let database = h::database().await;
    let harness = Harness::new(database.clone()).await;
    session(&database, harness.org_a, SESSION, harness.alice_id).await;
    // 22 MiB of HTML, with a recognisable tail so a truncated carry is visible.
    let chunk = "<p>generated section of the report</p>\n";
    let mut body = String::with_capacity(22 * 1024 * 1024 + 64);
    while body.len() < 22 * 1024 * 1024 {
        body.push_str(chunk);
    }
    body.push_str("<!--end-of-artifact-->");
    let expected = body.len();
    assert!(
        expected > 16 * 1024 * 1024,
        "the fixture must exceed the cap that used to exist"
    );
    let key = upload(
        &harness,
        &harness.alice_a,
        Owner::Session(SESSION),
        "artifact.html",
        &body,
    )
    .await;
    turn(
        &database,
        harness.org_a,
        SESSION,
        1,
        "aaaaaaaa-2222-4222-8222-222222222222",
        "here is the page",
        "noted",
        "completed",
        None,
    )
    .await;
    link_turn(&harness, &harness.alice_a, SESSION, 1, &key).await;
    let bundle = build(
        &harness.state,
        &harness.alice_a,
        Source::Session {
            session_id: SESSION,
        },
        &[],
    )
    .await
    .expect("transcript");
    let carried = bundle
        .files
        .iter()
        .find(|file| file.file_name == "artifact.html")
        .expect("a 22 MiB artifact is carried, not quietly dropped");
    // Length and tail rather than a full comparison: a 22 MiB assert_eq failure
    // prints 22 MiB.
    assert_eq!(carried.bytes.len(), expected, "carried whole");
    assert!(
        carried.bytes.ends_with(b"<!--end-of-artifact-->"),
        "including its end"
    );
    assert_eq!(bundle.manifest.carried_total, 1);
    assert_eq!(
        bundle.manifest.carried_omitted, 0,
        "nothing was omitted, so nothing must be reported as omitted"
    );
}
