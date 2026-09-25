use super::{
    mentions::{self, MentionInput, StoredMention},
    messages, resources,
    test_support::{patch, request, Harness},
    types::*,
};
use crate::neev::{db, test_support as h};

async fn display_name(harness: &Harness, user_id: i32) -> String {
    db::one(
        &harness.state.database,
        "SELECT display_name FROM neev_accounts WHERE id=$1",
        vec![user_id.into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "display_name")
    .unwrap()
}

/// "hi @<name> there", and the span that says which characters are the person.
fn said(name: &str) -> (String, MentionInput, i32) {
    let body = format!("hi @{name} there");
    let mention = MentionInput {
        user_id: 0,
        start: 3,
        length: 1 + i64::try_from(name.chars().count()).unwrap(),
    };
    (body, mention, 0)
}

async fn stored_rows(harness: &Harness, message_id: &str) -> Vec<StoredMention> {
    mentions::for_messages(
        &harness.state,
        &harness.creator,
        std::slice::from_ref(&message_id.to_owned()),
    )
    .await
    .unwrap()
    .remove(message_id)
    .unwrap_or_default()
}

/// The whole round trip: named, stored as an identity, read back, noticed once
/// by the person named and by nobody else.
async fn names_a_coworker(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let name = display_name(&h, h.admin.identity.user_id).await;
    let (body, span, _) = said(&name);
    let mention = MentionInput {
        user_id: h.admin.identity.user_id,
        ..span
    };
    let id = request();
    let message = messages::append_with_attachments(
        &h.state,
        &h.creator,
        &chat.id,
        &id,
        2,
        &body,
        &[],
        &[mention],
    )
    .await
    .unwrap();
    // The body is the plain sentence — what a provider is asked and what any
    // older client renders. The identity lives beside it.
    assert_eq!(message.body, body);
    assert_eq!(
        stored_rows(&h, &message.id).await,
        vec![StoredMention {
            user_id: h.admin.identity.user_id,
            start: mention.start,
            length: mention.length,
        }]
    );

    let (notices, unread) = mentions::inbox(&h.state, &h.admin).await.unwrap();
    assert_eq!((notices.len(), unread), (1, 1));
    assert_eq!(notices[0].message_id, message.id);
    assert_eq!(notices[0].chat_id, chat.id);
    assert!(!notices[0].read);
    assert_eq!(notices[0].excerpt, body);
    // The author is not notified of their own sentence, and neither is anyone
    // who was not named.
    assert_eq!(
        mentions::inbox(&h.state, &h.creator).await.unwrap(),
        (Vec::new(), 0)
    );

    // Reading is idempotent: following the same link twice is one notice seen.
    mentions::mark_read(&h.state, &h.admin, &[message.id.clone()])
        .await
        .unwrap();
    let (notices, unread) = mentions::inbox(&h.state, &h.admin).await.unwrap();
    assert_eq!((notices.len(), unread, notices[0].read), (1, 0, true));
    mentions::mark_read(&h.state, &h.admin, &[message.id.clone()])
        .await
        .unwrap();
    assert_eq!(mentions::inbox(&h.state, &h.admin).await.unwrap().1, 0);
}

/// Everybody a message may not name, and every span it may not claim.
async fn refuses_what_it_must(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let name = display_name(&h, h.admin.identity.user_id).await;
    let (body, span, _) = said(&name);
    let admin = MentionInput {
        user_id: h.admin.identity.user_id,
        ..span
    };
    // The chat's OWN revision, per case: a mention refused for what it is has to
    // be refused by the mention rules, not by a stale revision one step earlier.
    let send = async |actor: &crate::neev::types::OrgContext,
                      chat: &Resource,
                      body: &str,
                      mentions: &[MentionInput]| {
        messages::append_with_attachments(
            &h.state,
            actor,
            &chat.id,
            &request(),
            chat.auth_revision,
            body,
            &[],
            mentions,
        )
        .await
    };

    // A private conversation has no audience to notify, so it carries none.
    let private = h.chat().await;
    assert_eq!(
        send(&h.creator, &private, &body, &[admin]).await,
        Err(ContentError::Invalid)
    );
    // Somebody else's organization is not a directory this message can reach.
    assert_eq!(
        send(
            &h.creator,
            &chat,
            &body,
            &[MentionInput {
                user_id: h.foreign.identity.user_id,
                ..span
            }]
        )
        .await,
        Err(ContentError::Invalid)
    );
    // A span has to cover the name it claims, and nothing else.
    for wrong in [
        MentionInput { start: 0, ..admin },
        MentionInput {
            length: admin.length + 1,
            ..admin
        },
        MentionInput {
            start: 10_000,
            ..admin
        },
    ] {
        assert_eq!(
            send(&h.creator, &chat, &body, &[wrong]).await,
            Err(ContentError::Invalid)
        );
    }
    // Two spans cannot claim the same characters.
    assert_eq!(
        send(&h.creator, &chat, &body, &[admin, admin]).await,
        Err(ContentError::Invalid)
    );
    // Nothing was written by any of the refusals.
    assert_eq!(
        messages::list(&h.state, &h.creator, &chat.id, None)
            .await
            .unwrap()
            .items
            .len(),
        0
    );

    // A member who has since been removed is not someone to notify.
    db::execute(
        &h.state.database,
        "UPDATE neev_memberships SET active=false WHERE org_id=$1 AND user_id=$2",
        vec![h.creator.org_id.into(), h.admin.identity.user_id.into()],
    )
    .await
    .unwrap();
    assert_eq!(
        send(&h.creator, &chat, &body, &[admin]).await,
        Err(ContentError::Invalid)
    );
    db::execute(
        &h.state.database,
        "UPDATE neev_memberships SET active=true WHERE org_id=$1 AND user_id=$2",
        vec![h.creator.org_id.into(), h.admin.identity.user_id.into()],
    )
    .await
    .unwrap();
    // And a deactivated account is not one either.
    db::execute(
        &h.state.database,
        "UPDATE neev_accounts SET disabled=true WHERE id=$1",
        vec![h.admin.identity.user_id.into()],
    )
    .await
    .unwrap();
    assert_eq!(
        send(&h.creator, &chat, &body, &[admin]).await,
        Err(ContentError::Invalid)
    );
}

/// A retry is the same send, so it is the same notice — and an agent's words
/// are never a notice at all.
async fn retries_and_agents_notify_nobody_twice(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let name = display_name(&h, h.admin.identity.user_id).await;
    let (body, span, _) = said(&name);
    let mention = MentionInput {
        user_id: h.admin.identity.user_id,
        ..span
    };
    let id = request();
    let first = messages::append_with_attachments(
        &h.state,
        &h.creator,
        &chat.id,
        &id,
        2,
        &body,
        &[],
        &[mention],
    )
    .await
    .unwrap();
    let replay = messages::append_with_attachments(
        &h.state,
        &h.creator,
        &chat.id,
        &id,
        2,
        &body,
        &[],
        &[mention],
    )
    .await
    .unwrap();
    assert_eq!(replay, first);
    assert_eq!(stored_rows(&h, &first.id).await.len(), 1);
    assert_eq!(mentions::inbox(&h.state, &h.admin).await.unwrap().1, 1);
    // The same request id meaning something else is a conflict, not a second
    // quietly different send.
    assert_eq!(
        messages::append_with_attachments(
            &h.state,
            &h.creator,
            &chat.id,
            &id,
            2,
            &body,
            &[],
            &[MentionInput {
                start: mention.start,
                length: mention.length,
                user_id: h.creator.identity.user_id,
            }]
        )
        .await,
        Err(ContentError::Conflict)
    );

    // An agent reply carrying the same characters names nobody: the publish
    // path writes no spans, so there is nothing for a reader to draw and
    // nothing for an inbox to carry.
    let reply = messages::publish_agent_reply(
        &h.state,
        &h.creator,
        &chat.id,
        &format!("done, @{name}"),
        "claude",
        &request(),
    )
    .await
    .unwrap();
    assert!(stored_rows(&h, &reply.id).await.is_empty());
    assert_eq!(mentions::inbox(&h.state, &h.admin).await.unwrap().1, 1);
}

/// Authority is re-proved on every read and on every link followed.
async fn a_closed_conversation_carries_no_notice(database: sea_orm::DatabaseConnection) {
    let h = Harness::new(database).await;
    let chat = h.shared_chat().await;
    let name = display_name(&h, h.admin.identity.user_id).await;
    let (body, span, _) = said(&name);
    let message = messages::append_with_attachments(
        &h.state,
        &h.creator,
        &chat.id,
        &request(),
        2,
        &body,
        &[],
        &[MentionInput {
            user_id: h.admin.identity.user_id,
            ..span
        }],
    )
    .await
    .unwrap();
    assert_eq!(mentions::inbox(&h.state, &h.admin).await.unwrap().1, 1);

    // The chat becomes the creator's own again. The row is still there, and it
    // is no longer anything the reader may be shown.
    let latest = resources::get(&h.state, &h.creator, &chat.id).await.unwrap();
    let mut p = patch(&latest);
    p.visibility = Visibility::Private;
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert_eq!(
        mentions::inbox(&h.state, &h.admin).await.unwrap(),
        (Vec::new(), 0)
    );
    // A stale link cannot even mark it read, which is what stops an inaccessible
    // notice being quietly changed by somebody who can no longer see it.
    mentions::mark_read(&h.state, &h.admin, &[message.id.clone()])
        .await
        .unwrap();
    let read_at: Option<i64> = db::one(
        &h.state.database,
        "SELECT read_at FROM neev_message_mentions WHERE message_id=$1",
        vec![message.id.clone().into()],
    )
    .await
    .unwrap()
    .unwrap()
    .try_get("", "read_at")
    .unwrap();
    assert_eq!(read_at, None);

    // Shared again, and the notice is exactly where it was: unread.
    let latest = resources::get(&h.state, &h.creator, &chat.id).await.unwrap();
    let mut p = patch(&latest);
    p.visibility = Visibility::Organization;
    resources::update(&h.state, &h.creator, &chat.id, p)
        .await
        .unwrap();
    assert_eq!(mentions::inbox(&h.state, &h.admin).await.unwrap().1, 1);
}

#[tokio::test]
async fn sqlite_names_a_coworker() {
    names_a_coworker(h::database().await).await;
}
#[tokio::test]
async fn sqlite_refuses_what_it_must() {
    refuses_what_it_must(h::database().await).await;
}
#[tokio::test]
async fn sqlite_retries_and_agents_notify_nobody_twice() {
    retries_and_agents_notify_nobody_twice(h::database().await).await;
}
#[tokio::test]
async fn sqlite_a_closed_conversation_carries_no_notice() {
    a_closed_conversation_carries_no_notice(h::database().await).await;
}
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn postgres_mentions() {
    for case in [
        "names",
        "refuses",
        "retries",
        "closed",
    ] {
        let (db, admin, schema) = h::postgres_database().await;
        match case {
            "names" => names_a_coworker(db.clone()).await,
            "refuses" => refuses_what_it_must(db.clone()).await,
            "retries" => retries_and_agents_notify_nobody_twice(db.clone()).await,
            _ => a_closed_conversation_carries_no_notice(db.clone()).await,
        }
        h::cleanup_postgres(db, admin, schema).await;
    }
}
