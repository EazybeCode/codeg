use super::{
    attachment::{self, Owner},
    resources,
    test_support::{patch, Harness},
    types::*,
};
use crate::neev::test_support as h;

/// The harness clock, so a test can age a row without leaving the stored
/// domain: `created_at` carries a `>=0` CHECK, and a negative value is refused
/// by the database rather than read as "long ago".
const HARNESS_NOW: i64 = 1_700_000_000;

fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0u8; 24]);
    bytes
}

fn pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend_from_slice(b"1 0 obj\n<<>>\nendobj\n");
    bytes
}

async fn chat(harness: &Harness) -> Resource {
    resources::create(
        &harness.state,
        &harness.creator,
        harness.input(ResourceKind::Chat, "Private chat"),
    )
    .await
    .unwrap()
}

/// Classification is by content, never by what the client said it was uploading.
#[test]
fn sniffing_classifies_by_content_and_refuses_anything_else() {
    assert_eq!(attachment::sniff_attachment(&png()), Some("image/png"));
    assert_eq!(attachment::sniff_attachment(&pdf()), Some("application/pdf"));
    assert_eq!(
        attachment::sniff_attachment(b"fn main() {}\n"),
        Some("text/plain")
    );
    // A PNG header followed by anything is still a PNG, never text.
    let mut polyglot = png();
    polyglot.extend_from_slice(b"<script>alert(1)</script>");
    assert_eq!(attachment::sniff_attachment(&polyglot), Some("image/png"));
    // Binary that matches nothing is refused rather than stored as text.
    assert_eq!(attachment::sniff_attachment(&[0x00, 0x01, 0xFF, 0xFE]), None);
    assert_eq!(attachment::sniff_attachment(&[]), None);
}

/// The name reaches a filesystem inside the sandbox, so anything that could
/// climb out of the staging directory is refused rather than sanitised into
/// something plausible.
#[test]
fn names_that_could_escape_the_staging_directory_are_refused() {
    let cases = [
        "../../etc/passwd",
        "a/b.png",
        "a\\b.png",
        ".env",
        ".",
        "..",
        "with\nnewline.txt",
        "",
        "   ",
    ];
    for name in cases {
        assert!(
            attachment::canonical_name_for_test(name).is_none(),
            "expected {name:?} to be refused"
        );
    }
    // Ordinary names, including ones that look like agent configuration, are
    // kept verbatim — they are staged into their own directory, where they are
    // inert, rather than being written over the run's workspace.
    for name in ["AGENTS.md", "notes.txt", "a b,c.png", "Screenshot 2026.png"] {
        assert_eq!(
            attachment::canonical_name_for_test(name).as_deref(),
            Some(name)
        );
    }
}

#[tokio::test]
async fn attachment_serve_enforces_the_bound_chats_private_acl() {
    private_attachment_contract(h::database().await).await;
}

#[tokio::test]
#[ignore = "requires guarded PostgreSQL"]
async fn postgres_attachment_serve_enforces_the_bound_chats_private_acl() {
    use futures_util::FutureExt;
    let (db, admin, schema) = h::postgres_database().await;
    let outcome = std::panic::AssertUnwindSafe(private_attachment_contract(db.clone()))
        .catch_unwind()
        .await;
    h::cleanup_postgres(db, admin, schema).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}

async fn private_attachment_contract(db: sea_orm::DatabaseConnection) {
    let harness = Harness::new(db).await;
    let chat = chat(&harness).await;
    let stored = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "shot.png",
        &png(),
    )
    .await
    .expect("creator may attach to their own chat");
    assert_eq!(stored.content_type, "image/png");

    // The creator reads it back, bytes intact.
    let (bytes, content_type, name) = attachment::load(&harness.state, &harness.creator, &stored.key)
        .await
        .expect("creator may read");
    assert_eq!(bytes, png());
    assert_eq!(content_type, "image/png");
    assert_eq!(name, "shot.png");

    // A same-org member who is not authorized for this PRIVATE chat cannot read
    // it, even holding the exact key.
    let refused = attachment::load(&harness.state, &harness.admin, &stored.key).await;
    assert!(
        matches!(refused, Err(ContentError::NotFound)),
        "a same-org member must not read a private chat's attachment"
    );

    // And cannot attach to it either.
    let denied = attachment::store(
        &harness.state,
        &harness.admin,
        Owner::Chat(&chat.id),
        "theirs.png",
        &png(),
    )
    .await;
    assert!(matches!(denied, Err(ContentError::NotFound)));
}

#[tokio::test]
async fn a_run_only_carries_attachments_from_its_own_chat() {
    let harness = Harness::new(h::database().await).await;
    let first = chat(&harness).await;
    let second = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(ResourceKind::Chat, "Another chat"),
    )
    .await
    .unwrap();
    let elsewhere = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&second.id),
        "other.png",
        &png(),
    )
    .await
    .unwrap();

    // Authorized for the caller, but not part of THIS conversation: refused, so
    // a key cannot be smuggled from one chat's audience into another's.
    let refused = attachment::resolve_for_run(
        &harness.state,
        &harness.creator,
        Owner::Chat(&first.id),
        &[elsewhere.key.clone()],
    )
    .await;
    assert!(matches!(refused, Err(ContentError::Invalid)));

    // Its own chat resolves, and carries real bytes rather than a name.
    let mine = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&first.id),
        "mine.png",
        &png(),
    )
    .await
    .unwrap();
    let resolved = attachment::resolve_for_run(
        &harness.state,
        &harness.creator,
        Owner::Chat(&first.id),
        &[mine.key.clone()],
    )
    .await
    .expect("its own chat's attachment resolves");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].bytes, png());
    assert!(resolved[0].is_image());

    // The same key twice is refused rather than staged twice.
    let duplicated = attachment::resolve_for_run(
        &harness.state,
        &harness.creator,
        Owner::Chat(&first.id),
        &[mine.key.clone(), mine.key.clone()],
    )
    .await;
    assert!(matches!(duplicated, Err(ContentError::Invalid)));
}

/// A turn cannot exceed the per-run total even when each file is individually
/// within its own cap.
#[tokio::test]
async fn a_run_refuses_more_bytes_than_one_turn_may_carry() {
    let harness = Harness::new(h::database().await).await;
    let chat = chat(&harness).await;
    let mut keys = Vec::new();
    // Six megabytes each: individually fine, and over the 25 MiB run total by
    // the fifth.
    let big = {
        let mut bytes = png();
        bytes.resize(6 * 1024 * 1024, 0);
        bytes
    };
    for index in 0..5 {
        let stored = attachment::store(
            &harness.state,
            &harness.creator,
            Owner::Chat(&chat.id),
            &format!("big{index}.png"),
            &big,
        )
        .await
        .unwrap();
        keys.push(stored.key);
    }
    let refused =
        attachment::resolve_for_run(&harness.state, &harness.creator, Owner::Chat(&chat.id), &keys)
            .await;
    assert!(matches!(refused, Err(ContentError::Quota)));
    let _ = patch;
}

/// Insert an Agent Chat session directly.
///
/// `sessions::create` validates against the caller's provider catalogue, which
/// is about connected accounts rather than about storage. These tests are about
/// who may read an attachment, so the row is written directly and the columns
/// are exactly the ones `session_migration` declares.
async fn session(harness: &Harness, owner: &crate::neev::types::OrgContext) -> String {
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    crate::neev::db::execute(
        &harness.state.database,
        "INSERT INTO neev_agent_sessions(org_id,id,owner_id,title,provider,model,visibility,kind,bound_chat_id,bound_kind,archived,revision,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,NULL,NULL,$9,$10,$11,$12)",
        vec![
            owner.org_id.into(),
            id.clone().into(),
            owner.identity.user_id.into(),
            "Agent chat".into(),
            "claude".into(),
            "default".into(),
            "private".into(),
            "chat".into(),
            false.into(),
            1i64.into(),
            0i64.into(),
            0i64.into(),
        ],
    )
    .await
    .unwrap();
    id
}

/// An Agent Chat is private to its owner. There is no sharing to widen it, so
/// ownership IS the read check — and a teammate holding the key gets the same
/// uniform refusal as a stranger.
#[tokio::test]
async fn a_session_attachment_is_readable_only_by_that_sessions_owner() {
    let harness = Harness::new(h::database().await).await;
    let mine = session(&harness, &harness.creator).await;
    let stored = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Session(&mine),
        "trace.txt",
        b"private notes",
    )
    .await
    .expect("the owner may attach to their own session");
    assert_eq!(stored.content_type, "text/plain");

    let (bytes, _, name) = attachment::load(&harness.state, &harness.creator, &stored.key)
        .await
        .expect("the owner may read it back");
    assert_eq!(bytes, b"private notes");
    assert_eq!(name, "trace.txt");

    // Same organization, not the owner.
    assert!(matches!(
        attachment::load(&harness.state, &harness.admin, &stored.key).await,
        Err(ContentError::NotFound)
    ));
    // And cannot put anything into someone else's session either.
    assert!(matches!(
        attachment::store(
            &harness.state,
            &harness.admin,
            Owner::Session(&mine),
            "theirs.txt",
            b"x",
        )
        .await,
        Err(ContentError::NotFound)
    ));
}

/// A key is not portable between conversations, in either direction: not from
/// one session to another, and not between a session and a chat.
#[tokio::test]
async fn a_session_run_refuses_a_key_from_anywhere_else() {
    let harness = Harness::new(h::database().await).await;
    let first = session(&harness, &harness.creator).await;
    let second = session(&harness, &harness.creator).await;
    let chat = chat(&harness).await;

    let in_second = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Session(&second),
        "other.txt",
        b"other",
    )
    .await
    .unwrap();
    let in_chat = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "chat.txt",
        b"chat",
    )
    .await
    .unwrap();
    let in_first = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Session(&first),
        "mine.txt",
        b"mine",
    )
    .await
    .unwrap();

    // Both belong to the caller. Neither belongs to THIS session.
    for key in [&in_second.key, &in_chat.key] {
        assert!(matches!(
            attachment::resolve_for_run(
                &harness.state,
                &harness.creator,
                Owner::Session(&first),
                &[key.clone()],
            )
            .await,
            Err(ContentError::Invalid)
        ));
    }
    // A session key is equally refused by a chat run.
    assert!(matches!(
        attachment::resolve_for_run(
            &harness.state,
            &harness.creator,
            Owner::Chat(&chat.id),
            &[in_first.key.clone()],
        )
        .await,
        Err(ContentError::Invalid)
    ));
    // Its own resolves, with real bytes.
    let resolved = attachment::resolve_for_run(
        &harness.state,
        &harness.creator,
        Owner::Session(&first),
        &[in_first.key.clone()],
    )
    .await
    .unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].bytes, b"mine");
    assert!(!resolved[0].is_image());
}

/// The sweeper reclaims abandoned drafts and nothing else. An attachment a
/// message carries is part of that conversation's history however old it is;
/// one that was never sent is bytes nobody asked to keep.
#[tokio::test]
async fn the_sweeper_reclaims_aged_orphans_and_keeps_what_a_message_carries() {
    let harness = Harness::new(h::database().await).await;
    let chat = chat(&harness).await;
    let orphan = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "abandoned.txt",
        b"never sent",
    )
    .await
    .unwrap();
    let fresh = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "still-composing.txt",
        b"being written",
    )
    .await
    .unwrap();

    // Nothing is old enough yet: a draft somebody may still be composing is not
    // reclaimed out from under them.
    assert_eq!(
        attachment::sweep_orphans(&harness.state).await.unwrap(),
        0,
        "a recent upload must not be reclaimed"
    );

    // Age the abandoned one past the retention window.
    crate::neev::db::execute(
        &harness.state.database,
        "UPDATE neev_attachments SET created_at=$1 WHERE org_id=$2 AND key=$3",
        vec![
            (HARNESS_NOW - attachment::ORPHAN_RETENTION_SECONDS - 1).into(),
            harness.creator.org_id.into(),
            orphan.key.clone().into(),
        ],
    )
    .await
    .unwrap();

    assert_eq!(
        attachment::sweep_orphans(&harness.state).await.unwrap(),
        1,
        "the aged orphan is reclaimed"
    );
    assert!(matches!(
        attachment::load(&harness.state, &harness.creator, &orphan.key).await,
        Err(ContentError::NotFound)
    ));
    // The one still being composed is untouched.
    assert!(attachment::load(&harness.state, &harness.creator, &fresh.key)
        .await
        .is_ok());
}

/// The per-organization cap is the one bound a reader can actually reach, so it
/// is reported as a quota they can act on rather than a generic failure.
#[tokio::test]
async fn the_per_file_cap_refuses_before_anything_is_stored() {
    let harness = Harness::new(h::database().await).await;
    let chat = chat(&harness).await;
    // One byte over the per-file cap.
    let oversized = vec![b'a'; 10 * 1024 * 1024 + 1];
    assert!(matches!(
        attachment::store(
            &harness.state,
            &harness.creator,
            Owner::Chat(&chat.id),
            "huge.txt",
            &oversized,
        )
        .await,
        Err(ContentError::Invalid)
    ));
    // Empty is refused too: there is nothing to give a provider.
    assert!(matches!(
        attachment::store(
            &harness.state,
            &harness.creator,
            Owner::Chat(&chat.id),
            "empty.txt",
            b"",
        )
        .await,
        Err(ContentError::Invalid)
    ));
}

/// Exercise the ref helpers against the real tables.
///
/// This exists because a compiler cannot read SQL. `link_to_message` shipped
/// writing to `neev_message_attachments` — a table that was renamed out of
/// existence before release — and every type checked. Only running the
/// statement finds that, which is why both link helpers and both readers are
/// driven here rather than asserted about.
#[tokio::test]
async fn the_ref_helpers_write_and_read_the_tables_that_exist() {
    let harness = Harness::new(h::database().await).await;
    // The shared chat, because this test is about the ref tables and the
    // message append path expects the same shape `message_tests` uses.
    let chat = harness.shared_chat().await;
    let first = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "one.txt",
        b"one",
    )
    .await
    .unwrap();
    let second = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Chat(&chat.id),
        "two.txt",
        b"two",
    )
    .await
    .unwrap();

    let request = uuid::Uuid::new_v4().hyphenated().to_string();
    let message = crate::neev::content::messages::append(
        &harness.state,
        &harness.creator,
        &chat.id,
        &request,
        2,
        "with attachments",
    )
    .await
    .expect("a message to hang them on");

    let tx = crate::neev::db::begin(&harness.state.database)
        .await
        .unwrap();
    attachment::link_to_message(
        &tx,
        &harness.creator,
        &message.id,
        &[first.key.clone(), second.key.clone()],
    )
    .await
    .expect("the insert must name a table that exists");
    tx.commit().await.unwrap();

    // Read back in the order they were sent, for the page the thread renders.
    let found = attachment::for_messages(&harness.state, &harness.creator, &[message.id.clone()])
        .await
        .unwrap();
    let items = found.get(&message.id).expect("the message carries them");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].file_name, "one.txt");
    assert_eq!(items[1].file_name, "two.txt");
    assert_eq!(items[0].content_type, "text/plain");

    // A referenced attachment is never reclaimed, however old it gets.
    crate::neev::db::execute(
        &harness.state.database,
        "UPDATE neev_attachments SET created_at=$1 WHERE org_id=$2",
        vec![
            (HARNESS_NOW - attachment::ORPHAN_RETENTION_SECONDS - 1).into(),
            harness.creator.org_id.into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        attachment::sweep_orphans(&harness.state).await.unwrap(),
        0,
        "an attachment a message carries is part of its history"
    );
    assert!(attachment::load(&harness.state, &harness.creator, &first.key)
        .await
        .is_ok());
}

/// The turn side of the same thing, for an Agent Chat.
#[tokio::test]
async fn the_turn_ref_helpers_write_and_read_the_tables_that_exist() {
    let harness = Harness::new(h::database().await).await;
    let id = session(&harness, &harness.creator).await;
    let stored = attachment::store(
        &harness.state,
        &harness.creator,
        Owner::Session(&id),
        "trace.txt",
        b"trace",
    )
    .await
    .unwrap();
    // A turn to hang it on. Written directly for the same reason the session is.
    crate::neev::db::execute(
        &harness.state.database,
        "INSERT INTO neev_agent_turns(org_id,session_id,sequence,id,run_id,request_id,prompt,response,status,error_code,provider,model,created_at,updated_at,completed_at) VALUES($1,$2,$3,$4,$5,NULL,$6,$7,$8,NULL,$9,$10,$11,$12,NULL)",
        vec![
            harness.creator.org_id.into(),
            id.clone().into(),
            1i64.into(),
            uuid::Uuid::new_v4().hyphenated().to_string().into(),
            uuid::Uuid::new_v4().hyphenated().to_string().into(),
            "look".into(),
            "".into(),
            "running".into(),
            "claude".into(),
            "default".into(),
            0i64.into(),
            0i64.into(),
        ],
    )
    .await
    .unwrap();

    let tx = crate::neev::db::begin(&harness.state.database)
        .await
        .unwrap();
    attachment::link_to_turn(&tx, &harness.creator, &id, 1, &[stored.key.clone()])
        .await
        .expect("the turn insert must name a table that exists");
    tx.commit().await.unwrap();

    let found = attachment::for_turns(&harness.state, &harness.creator, &id, &[1])
        .await
        .unwrap();
    let items = found.get(&1).expect("the turn carries it");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].file_name, "trace.txt");
}
