use super::*;

const CHAT: &str = "11111111-1111-4111-8111-111111111111";
const OTHER: &str = "22222222-2222-4222-8222-222222222222";

fn hub() -> Arc<MessageHub> {
    Arc::new(MessageHub::default())
}

#[tokio::test]
async fn every_subscriber_of_a_conversation_is_notified_with_the_sequence_only() {
    let hub = hub();
    let mut first = hub.subscribe(3, CHAT).unwrap();
    let mut second = hub.subscribe(3, CHAT).unwrap();
    hub.publish(3, CHAT, 7);
    assert_eq!(first.recv().await, Some(Notice::Sequence(7)));
    assert_eq!(second.recv().await, Some(Notice::Sequence(7)));
    // One topic, two slots: fan-out never duplicates per-conversation state.
    assert_eq!((hub.topic_count(), hub.subscriber_count()), (1, 2));
}

#[tokio::test]
async fn notices_never_cross_an_organization_or_a_conversation() {
    let hub = hub();
    let mut mine = hub.subscribe(3, CHAT).unwrap();
    let mut foreign_org = hub.subscribe(4, CHAT).unwrap();
    let mut other_chat = hub.subscribe(3, OTHER).unwrap();
    hub.publish(3, CHAT, 1);
    assert_eq!(mine.recv().await, Some(Notice::Sequence(1)));
    // A same-id chat in another tenant, and a sibling chat, learn nothing —
    // not even that an event happened.
    assert!(matches!(
        foreign_org.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        other_chat.receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn publishing_without_subscribers_allocates_nothing() {
    let hub = hub();
    hub.publish(3, CHAT, 1);
    assert_eq!(hub.topic_count(), 0);
}

#[tokio::test]
async fn dropping_a_subscription_releases_its_slot_and_removes_the_empty_topic() {
    let hub = hub();
    let first = hub.subscribe(3, CHAT).unwrap();
    let second = hub.subscribe(3, CHAT).unwrap();
    drop(first);
    assert_eq!((hub.topic_count(), hub.subscriber_count()), (1, 1));
    drop(second);
    assert_eq!((hub.topic_count(), hub.subscriber_count()), (0, 0));
    // A cancelled stream must not keep the conversation resident forever.
    hub.publish(3, CHAT, 2);
    assert_eq!(hub.topic_count(), 0);
}

#[tokio::test]
async fn subscriptions_are_bounded_per_conversation_and_in_total() {
    let hub = hub();
    let mut held: Vec<Subscription> = (0..MAX_TOPIC_SUBSCRIBERS)
        .map(|_| hub.subscribe(3, CHAT).unwrap())
        .collect();
    assert!(hub.subscribe(3, CHAT).is_none(), "per-chat cap not enforced");
    // A different conversation is unaffected by one busy chat's cap.
    held.push(hub.subscribe(3, OTHER).unwrap());
    // Spread over fresh conversations until the process-wide cap is reached.
    let mut topic = 0;
    while hub.subscriber_count() < MAX_TOTAL_SUBSCRIBERS {
        match hub.subscribe(3, &format!("00000000-0000-4000-8000-{topic:012}")) {
            Some(subscription) => held.push(subscription),
            None => topic += 1,
        }
    }
    assert!(
        hub.subscribe(3, &format!("00000000-0000-4000-8000-{:012}", topic + 1))
            .is_none(),
        "global cap not enforced"
    );
    assert!(
        hub.subscribe(9, "33333333-3333-4333-8333-333333333333")
            .is_none(),
        "global cap must hold across organizations"
    );
    held.clear();
    assert_eq!((hub.topic_count(), hub.subscriber_count()), (0, 0));
    assert!(hub.subscribe(3, CHAT).is_some(), "slots are reclaimed");
}

#[tokio::test]
async fn a_subscriber_that_falls_behind_the_bounded_buffer_is_told_to_resync() {
    let hub = hub();
    let mut slow = hub.subscribe(3, CHAT).unwrap();
    for sequence in 1..=(TOPIC_CAPACITY as i64 + 10) {
        hub.publish(3, CHAT, sequence);
    }
    // Backpressure drops old notices instead of growing without bound; the
    // client re-reads from its own cursor, so nothing is actually lost.
    assert_eq!(slow.recv().await, Some(Notice::Lagged));
}

#[tokio::test]
async fn draining_collapses_a_burst_and_keeps_a_lag_sticky() {
    let hub = hub();
    let mut subscriber = hub.subscribe(3, CHAT).unwrap();
    hub.publish(3, CHAT, 1);
    hub.publish(3, CHAT, 2);
    hub.publish(3, CHAT, 3);
    let first = subscriber.recv().await.unwrap();
    assert_eq!(subscriber.drain(first), Notice::Sequence(3));
    assert_eq!(subscriber.drain(Notice::Lagged), Notice::Lagged);
    hub.publish(3, CHAT, 4);
    // A pending resync is never downgraded by a later sequence notice.
    assert_eq!(subscriber.drain(Notice::Lagged), Notice::Lagged);
}

#[test]
fn the_default_policy_bounds_both_the_recheck_cadence_and_the_connection() {
    let policy = StreamPolicy::default();
    // A revoked reader must not keep a stale conversation on screen much longer
    // than the 5s polling path it replaces.
    assert!(policy.heartbeat <= Duration::from_secs(15));
    assert!(policy.coalesce < policy.heartbeat);
    assert!(policy.lifetime >= policy.heartbeat && policy.lifetime <= Duration::from_secs(900));
}
