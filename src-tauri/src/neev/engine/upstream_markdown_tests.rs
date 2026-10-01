use super::test_support::{fake_bridge, upstream_for};
use super::upstream::*;

#[tokio::test]
async fn maps_bridge_markdown_statuses_and_sends_the_contract_body() {
    let (base, seen) = fake_bridge(vec![(
        200,
        r##"{"id":"x","content_sha256":"ab","unchanged":false,"markdown":"# A","updated_at":"t"}"##.into(),
    )])
    .await;
    assert_eq!(
        upstream_for(&base).markdown("x", 1, None).await.unwrap().unwrap(),
        EngineMarkdown::Content {
            content_sha256: "ab".into(),
            markdown: "# A".into()
        }
    );
    let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert_eq!(body, serde_json::json!({"org_id": 1, "if_none_match": null}));

    let (base, seen) = fake_bridge(vec![(
        200,
        r#"{"id":"x","content_sha256":"ab","unchanged":true}"#.into(),
    )])
    .await;
    assert_eq!(
        upstream_for(&base).markdown("x", 7, Some("ab")).await.unwrap().unwrap(),
        EngineMarkdown::Unchanged {
            content_sha256: "ab".into()
        }
    );
    let body: serde_json::Value = serde_json::from_str(&seen.lock().unwrap()[0]).unwrap();
    assert_eq!(body, serde_json::json!({"org_id": 7, "if_none_match": "ab"}));

    for (status, body, expected) in [
        (413, r#"{"error":"too_large"}"#, MarkdownRefusal::TooLarge),
        (409, r#"{"error":"not_an_engine_document"}"#, MarkdownRefusal::NotEngine),
        (409, r#"{"error":"organization_mismatch"}"#, MarkdownRefusal::OrgMismatch),
        (404, r#"{"error":"unknown_document"}"#, MarkdownRefusal::Unknown),
    ] {
        let (base, _) = fake_bridge(vec![(status, body.into())]).await;
        assert_eq!(
            upstream_for(&base).markdown("x", 1, None).await.unwrap(),
            Err(expected),
            "{status} {body}"
        );
    }
    let (base, _) = fake_bridge(vec![(503, r#"{"error":"converter_unavailable"}"#.into())]).await;
    assert!(matches!(
        upstream_for(&base).markdown("x", 1, None).await,
        Err(EngineError::Unavailable)
    ));
    // A bridge-key failure is a refusal, not a capture state.
    let (base, _) = fake_bridge(vec![(403, r#"{"error":"forbidden"}"#.into())]).await;
    assert!(matches!(
        upstream_for(&base).markdown("x", 1, None).await,
        Err(EngineError::Refused)
    ));
    // A 200 without a digest is malformed, never an empty success.
    let (base, _) = fake_bridge(vec![(200, r#"{"id":"x","unchanged":false,"markdown":""}"#.into())]).await;
    assert!(matches!(
        upstream_for(&base).markdown("x", 1, None).await,
        Err(EngineError::Invalid)
    ));
}
