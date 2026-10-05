use std::collections::BTreeMap;

use sea_orm::ConnectionTrait;

use super::{
    attachment,
    fork::{self, CopiedFile, FileSnapshot, ForkError},
    messages, resources,
    test_support::{request, Harness},
    types::*,
};
use crate::neev::{agents::transcript, db, test_support as h};

async fn agent_reply(h: &Harness, chat: &str, body: &str, keys: &[String]) -> Message {
    messages::publish_agent_reply_with_attachments(
        &h.state,
        &h.creator,
        chat,
        body,
        "claude",
        &request(),
        keys,
    )
    .await
    .unwrap()
}

async fn count(h: &Harness, sql: &str, values: Vec<sea_orm::Value>) -> i64 {
    db::one(&h.state.database, sql, values)
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}

async fn fork_rows(h: &Harness, chat: &str) -> Vec<(i64, i32, String, Option<String>, String, i64)> {
    let rows = h
        .state
        .database
        .query_all(db::statement(
            &h.state.database,
            "SELECT sequence,author_id,author_kind,agent_label,body,created_at FROM neev_messages WHERE org_id=$1 AND chat_id=$2 ORDER BY sequence",
            vec![h.creator.org_id.into(), chat.into()],
        ))
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            (
                row.try_get("", "sequence").unwrap(),
                row.try_get("", "author_id").unwrap(),
                row.try_get("", "author_kind").unwrap(),
                row.try_get("", "agent_label").unwrap(),
                row.try_get("", "body").unwrap(),
                row.try_get("", "created_at").unwrap(),
            )
        })
        .collect()
}

/// A shared chat with: a person's ask, the agent's reply, a member's follow-up,
/// and a second agent reply — so a fork through the FIRST reply must stop there.
async fn conversation(h: &Harness) -> (Resource, Message, Message, Message) {
    let chat = h.shared_chat().await;
    let ask = messages::append(&h.state, &h.creator, &chat.id, &request(), 2, "What changed?")
        .await
        .unwrap();
    let first = agent_reply(h, &chat.id, "Three things changed.", &[]).await;
    messages::append(&h.state, &h.admin, &chat.id, &request(), 2, "Thanks, more?")
        .await
        .unwrap();
    let later = agent_reply(h, &chat.id, "Also a fourth.", &[]).await;
    (chat, ask, first, later)
}

async fn chat_fork(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let (chat, ask, first, _later) = conversation(&h).await;
    let before = fork_rows(&h, &chat.id).await;
    let chats_before = h.usage("chats").await;

    // Another member who can READ the shared chat forks it: not only its creator.
    let done = fork::fork_chat(&h.state, &h.admin, &chat.id, &first.id, &request())
        .await
        .unwrap();
    assert_eq!((done.messages, done.attachments, done.replayed), (2, 0, false));
    assert_eq!(done.source_chat_id, chat.id);

    // Private to the forker, in the source's folder, marked as a fork.
    let forked = resources::get(&h.state, &h.admin, &done.chat_id).await.unwrap();
    assert_eq!(forked.visibility, Visibility::Private);
    assert_eq!(forked.creator_id, h.admin.identity.user_id);
    assert_eq!(forked.node_id, chat.node_id);
    assert_eq!(forked.title, "Private title (fork)");
    assert_eq!(forked.kind, ResourceKind::Chat);
    // The source's creator cannot see it; nor can another org.
    assert_eq!(
        resources::get(&h.state, &h.creator, &done.chat_id).await,
        Err(ContentError::NotFound)
    );
    assert_eq!(
        resources::get(&h.state, &h.foreign, &done.chat_id).await,
        Err(ContentError::NotFound)
    );

    // Copied faithfully up through the selected reply, and no further: the
    // agent reply is still an agent reply, authors and times are the originals.
    let copied = fork_rows(&h, &done.chat_id).await;
    assert_eq!(copied, before[..2].to_vec());
    assert_eq!(copied[0].1, ask.author_id);
    assert_eq!((copied[1].2.as_str(), copied[1].3.as_deref()), ("agent", Some("claude")));

    // The original is untouched.
    assert_eq!(fork_rows(&h, &chat.id).await, before);
    assert_eq!(
        resources::get(&h.state, &h.creator, &chat.id).await.unwrap().content_revision,
        4
    );
    assert_eq!(h.usage("chats").await, chats_before + 1);

    // It continues: the fork's own transcript is what its next run is given.
    let bundle = transcript::build(
        &h.state,
        &h.admin,
        transcript::Source::Chat { chat_id: &done.chat_id },
        &[],
    )
    .await
    .unwrap();
    let text = String::from_utf8(bundle.files[0].bytes.clone()).unwrap();
    assert!(text.contains("What changed?") && text.contains("Three things changed."));
    assert!(!text.contains("Also a fourth."));

    // Provenance, for the forker.
    assert_eq!(
        fork::chat_fork_source(&h.state, &h.admin, &done.chat_id).await.unwrap(),
        Some(chat.id.clone())
    );
}

#[tokio::test]
async fn a_chat_fork_is_a_private_faithful_copy_up_through_the_reply() {
    chat_fork(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_a_chat_fork_is_a_private_faithful_copy_up_through_the_reply() {
    let (database, admin, schema) = h::postgres_database().await;
    chat_fork(database.clone()).await;
    h::cleanup_postgres(database, admin, schema).await;
}

#[tokio::test]
async fn files_are_copied_to_fresh_keys_the_fork_owns_and_links_follow_them() {
    let h = Harness::new(h::database().await).await;
    let chat = h.shared_chat().await;
    let upload = attachment::store(
        &h.state,
        &h.creator,
        attachment::Owner::Chat(&chat.id),
        "notes.txt",
        b"the original bytes\n",
    )
    .await
    .unwrap();
    messages::append_with_attachments(
        &h.state,
        &h.creator,
        &chat.id,
        &request(),
        2,
        "Read this",
        std::slice::from_ref(&upload.key),
        &[],
    )
    .await
    .unwrap();
    let generated = attachment::store(
        &h.state,
        &h.creator,
        attachment::Owner::Chat(&chat.id),
        "report.txt",
        b"generated output\n",
    )
    .await
    .unwrap();
    let hidden = attachment::store(
        &h.state,
        &h.creator,
        attachment::Owner::Chat(&chat.id),
        "deleted.txt",
        b"soft-deleted\n",
    )
    .await
    .unwrap();
    let reply = agent_reply(
        &h,
        &chat.id,
        &format!("See [report](/api/neev/attachments/{})", generated.key),
        &[generated.key.clone(), hidden.key.clone()],
    )
    .await;
    attachment::set_archived(&h.state, &h.creator, &hidden.key, true)
        .await
        .unwrap();

    let done = fork::fork_chat(&h.state, &h.admin, &chat.id, &reply.id, &request())
        .await
        .unwrap();
    assert_eq!((done.attachments, done.archived_attachments), (2, 1));

    let rows = h
        .state
        .database
        .query_all(db::statement(
            &h.state.database,
            "SELECT key,file_name,chat_id FROM neev_attachments WHERE org_id=$1 AND chat_id=$2 ORDER BY file_name",
            vec![h.admin.org_id.into(), done.chat_id.clone().into()],
        ))
        .await
        .unwrap();
    let names: Vec<String> = rows.iter().map(|row| row.try_get("", "file_name").unwrap()).collect();
    assert_eq!(names, vec!["notes.txt", "report.txt"]);
    let mut fresh = BTreeMap::new();
    for row in &rows {
        let key: String = row.try_get("", "key").unwrap();
        assert!(key != upload.key && key != generated.key, "a fresh key, never shared");
        let name: String = row.try_get("", "file_name").unwrap();
        fresh.insert(name, key);
    }
    // Byte-for-byte, readable by the forker through the fork's own ACL.
    let (bytes, _, _) = attachment::load(&h.state, &h.admin, &fresh["notes.txt"]).await.unwrap();
    assert_eq!(bytes, b"the original bytes\n");
    // The originals are untouched and still served from the source.
    let (bytes, _, _) = attachment::load(&h.state, &h.admin, &upload.key).await.unwrap();
    assert_eq!(bytes, b"the original bytes\n");
    // The copied reply links to the COPY, not to the original.
    let copied = fork_rows(&h, &done.chat_id).await;
    assert_eq!(copied[1].4, format!("See [report](/api/neev/attachments/{})", fresh["report.txt"]));
    // And its next run carries them.
    let bundle = transcript::build(
        &h.state,
        &h.admin,
        transcript::Source::Chat { chat_id: &done.chat_id },
        &[],
    )
    .await
    .unwrap();
    assert!(bundle.manifest.carried.iter().any(|name| name == "notes.txt"));
    // The fork's files never reach the source's creator.
    assert_eq!(
        attachment::load(&h.state, &h.creator, &fresh["notes.txt"]).await.err(),
        Some(ContentError::NotFound)
    );
}

#[tokio::test]
async fn a_fork_is_private_even_in_a_folder_whose_default_is_organization() {
    let h = Harness::new(h::database().await).await;
    let node = db::one(
        &h.state.database,
        "SELECT revision FROM neev_nodes WHERE org_id=$1 AND id=$2",
        vec![h.creator.org_id.into(), h.node.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "revision")
    .unwrap();
    super::sharing::set(
        &h.state,
        &h.creator,
        h.node,
        node,
        super::sharing::DefaultVisibility::Organization,
    )
    .await
    .unwrap();
    let (chat, _, first, _) = conversation(&h).await;
    let done = fork::fork_chat(&h.state, &h.creator, &chat.id, &first.id, &request())
        .await
        .unwrap();
    let forked = resources::get(&h.state, &h.creator, &done.chat_id).await.unwrap();
    assert_eq!(forked.visibility, Visibility::Private);
    assert_eq!(
        resources::get(&h.state, &h.admin, &done.chat_id).await,
        Err(ContentError::NotFound)
    );
}

#[tokio::test]
async fn only_a_completed_reply_the_caller_can_read_is_forkable() {
    let h = Harness::new(h::database().await).await;
    let (chat, ask, first, _) = conversation(&h).await;
    // A person's message is not a reply.
    assert_eq!(
        fork::fork_chat(&h.state, &h.creator, &chat.id, &ask.id, &request()).await,
        Err(ForkError::NotComplete)
    );
    // Unknown message, another org, malformed ids.
    assert_eq!(
        fork::fork_chat(&h.state, &h.creator, &chat.id, &request(), &request()).await,
        Err(ForkError::Content(ContentError::NotFound))
    );
    assert_eq!(
        fork::fork_chat(&h.state, &h.foreign, &chat.id, &first.id, &request()).await,
        Err(ForkError::Content(ContentError::NotFound))
    );
    assert_eq!(
        fork::fork_chat(&h.state, &h.creator, "not-a-uuid", &first.id, &request()).await,
        Err(ForkError::Content(ContentError::NotFound))
    );
    // A private chat is forkable only by the one person who can read it.
    let private = h.chat().await;
    messages::append(&h.state, &h.creator, &private.id, &request(), 1, "Mine")
        .await
        .unwrap();
    let mine = agent_reply(&h, &private.id, "Yours.", &[]).await;
    assert_eq!(
        fork::fork_chat(&h.state, &h.admin, &private.id, &mine.id, &request()).await,
        Err(ForkError::Content(ContentError::NotFound))
    );
    assert!(fork::fork_chat(&h.state, &h.creator, &private.id, &mine.id, &request())
        .await
        .is_ok());
}

#[tokio::test]
async fn a_retried_fork_is_the_same_fork_and_a_changed_one_is_a_conflict() {
    let h = Harness::new(h::database().await).await;
    let (chat, _, first, later) = conversation(&h).await;
    let id = request();
    let done = fork::fork_chat(&h.state, &h.admin, &chat.id, &first.id, &id).await.unwrap();
    let chats = h.usage("chats").await;
    let again = fork::fork_chat(&h.state, &h.admin, &chat.id, &first.id, &id).await.unwrap();
    assert_eq!((again.chat_id.as_str(), again.replayed, again.messages), (done.chat_id.as_str(), true, 2));
    assert_eq!(h.usage("chats").await, chats, "a replay charges nothing");
    assert_eq!(
        fork::fork_chat(&h.state, &h.admin, &chat.id, &later.id, &id).await,
        Err(ForkError::Content(ContentError::Conflict))
    );
}

#[tokio::test]
async fn a_chat_in_a_trashed_folder_is_not_forked() {
    let h = Harness::new(h::database().await).await;
    let (chat, _, first, _) = conversation(&h).await;
    let revision: i64 = db::one(
        &h.state.database,
        "SELECT revision FROM neev_nodes WHERE org_id=$1 AND id=$2",
        vec![h.creator.org_id.into(), h.node.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "revision")
    .unwrap();
    crate::neev::catalog::trash::trash(&h.state, &h.creator, h.node, revision)
        .await
        .unwrap();
    assert_eq!(
        fork::fork_chat(&h.state, &h.creator, &chat.id, &first.id, &request()).await,
        Err(ForkError::SourceTrashed)
    );
    assert_eq!(
        count(&h, "SELECT COUNT(*) AS n FROM neev_resources WHERE org_id=$1 AND title LIKE '%(fork)'", vec![h.creator.org_id.into()]).await,
        0
    );
}

#[test]
fn file_bounds_refuse_rather_than_drop() {
    let file = |key: &str, length: i64| FileSnapshot {
        key: key.into(),
        file_name: "a.txt".into(),
        content_type: "text/plain".into(),
        byte_length: length,
        sha256: "0".repeat(64),
        created_by: 1,
        created_at: 0,
    };
    let mut files = BTreeMap::new();
    files.insert("a".to_string(), file("a", fork::MAX_FORK_ATTACHMENT_BYTES));
    assert_eq!(fork::check_file_bounds(&files), Ok(()));
    files.insert("b".to_string(), file("b", 1));
    assert_eq!(fork::check_file_bounds(&files), Err(ForkError::AttachmentsTooLarge));
    let many: BTreeMap<_, _> = (0..=fork::MAX_FORK_ATTACHMENTS)
        .map(|i| (i.to_string(), file(&i.to_string(), 1)))
        .collect();
    assert_eq!(fork::check_file_bounds(&many), Err(ForkError::AttachmentsTooLarge));
}

#[test]
fn links_follow_only_the_copied_files_and_titles_stay_bounded() {
    let copied = vec![CopiedFile {
        old_key: "11111111-1111-4111-8111-111111111111".into(),
        new_key: "22222222-2222-4222-8222-222222222222".into(),
        file: FileSnapshot {
            key: "11111111-1111-4111-8111-111111111111".into(),
            file_name: "a".into(),
            content_type: "text/plain".into(),
            byte_length: 1,
            sha256: "0".repeat(64),
            created_by: 1,
            created_at: 0,
        },
    }];
    let text = "[a](/api/neev/attachments/11111111-1111-4111-8111-111111111111) and /api/neev/attachments/33333333-3333-4333-8333-333333333333 and 11111111-1111-4111-8111-111111111111";
    assert_eq!(
        fork::rewrite_links(text, &copied),
        "[a](/api/neev/attachments/22222222-2222-4222-8222-222222222222) and /api/neev/attachments/33333333-3333-4333-8333-333333333333 and 11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(fork::fork_title("Plan"), "Plan (fork)");
    let long = fork::fork_title(&"x".repeat(100));
    assert_eq!(long.chars().count(), 100);
    assert!(long.ends_with(" (fork)"));
}
