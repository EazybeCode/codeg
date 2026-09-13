use super::wire::preflight;
use super::wire_types::{ErrorCode, ResponseEnvelope, Role, UnixTime};
use super::{
    decode_request, decode_response, encode_request, encode_response, Operation, Response,
    WireError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::error::Error;

const REQUEST_ID: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_ID: &str = "22222222-2222-4222-8222-222222222222";
const MAX_COUNTER: &str = "9223372036854775807";

#[derive(Deserialize)]
struct Fixture {
    operation: String,
    request: Value,
    success: Value,
    error: Value,
}

fn fixtures() -> Vec<Fixture> {
    serde_json::from_str(include_str!("fixtures/identity-v1.json")).unwrap()
}

fn operation(value: &str) -> Operation {
    match value {
        "begin" => Operation::Begin,
        "issue" => Operation::Issue,
        "complete" => Operation::Complete,
        "session" => Operation::Session,
        "redeem" => Operation::Redeem,
        "validate" => Operation::Validate,
        "logout" => Operation::Logout,
        "revoke" => Operation::Revoke,
        "revocations" => Operation::Revocations,
        "ack" => Operation::Ack,
        _ => panic!("fixture contains an unreviewed operation"),
    }
}

fn fixture(name: &str) -> Fixture {
    fixtures()
        .into_iter()
        .find(|fixture| fixture.operation == name)
        .unwrap()
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn rejects_request(op: Operation, value: &Value) {
    assert_eq!(
        decode_request(op, &bytes(value)),
        Err(WireError::InvalidMessage)
    );
}

fn rejects_response(op: Operation, value: &Value) {
    assert_eq!(
        decode_response(op, &bytes(value), REQUEST_ID),
        Err(WireError::InvalidMessage)
    );
}

#[test]
fn all_reviewed_literal_variants_decode_and_round_trip() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 10);
    for fixture in fixtures {
        let op = operation(&fixture.operation);
        let request = decode_request(op, &bytes(&fixture.request)).unwrap();
        assert_eq!(request.operation(), op);
        assert_eq!(request.request_id(), REQUEST_ID);
        assert_eq!(
            serde_json::from_slice::<Value>(&encode_request(op, &request).unwrap()).unwrap(),
            fixture.request
        );

        let success = decode_response(op, &bytes(&fixture.success), REQUEST_ID).unwrap();
        assert_eq!(success.operation(), op);
        assert_eq!(success.request_id(), REQUEST_ID);
        assert!(!success.is_error());
        assert_eq!(
            serde_json::from_slice::<Value>(&encode_response(op, &success).unwrap()).unwrap(),
            fixture.success
        );

        let error = decode_response(op, &bytes(&fixture.error), REQUEST_ID).unwrap();
        assert_eq!(error.operation(), op);
        assert_eq!(error.request_id(), REQUEST_ID);
        assert_eq!(error.error_code(), Some(ErrorCode::Unavailable));
        assert_eq!(
            serde_json::from_slice::<Value>(&encode_response(op, &error).unwrap()).unwrap(),
            fixture.error
        );
    }
}

#[test]
fn typed_authority_getters_preserve_role_ids_and_maximum_counter_string() {
    let fixture = fixture("redeem");
    let response =
        decode_response(Operation::Redeem, &bytes(&fixture.success), REQUEST_ID).unwrap();
    let subject = response.subject().unwrap();
    assert_eq!(subject.role(), Role::Admin);
    assert_eq!(subject.user_id(), "2147483647");
    assert_eq!(subject.org_id(), "2");
    assert_eq!(response.revocation_cursor(), Some(MAX_COUNTER));
}

#[test]
fn all_static_error_codes_decode_without_dynamic_messages() {
    for code in [
        "invalid_request",
        "unauthorized",
        "forbidden",
        "authority_changed",
        "login_retry_required",
        "conflict",
        "rate_limited",
        "unavailable",
    ] {
        let raw = json!({"v":1,"request_id":REQUEST_ID,"error":{"code":code}});
        let response = decode_response(Operation::Begin, &bytes(&raw), REQUEST_ID).unwrap();
        assert!(response.is_error());
    }
    let unknown = json!({"v":1,"request_id":REQUEST_ID,"error":{"code":"other"}});
    rejects_response(Operation::Begin, &unknown);

    let error = WireError::InvalidMessage;
    assert_eq!(error.to_string(), "invalid_gateway_message");
    assert!(error.source().is_none());
}

#[test]
fn begin_rejects_ambiguous_version() {
    let raw = br#"{"v":1,"v":1,"request_id":"11111111-1111-4111-8111-111111111111","data":{}}"#;
    assert!(decode_request(Operation::Begin, raw).is_err());
}

#[test]
fn escaped_duplicate_nested_field_is_rejected() {
    let raw = br#"{"v":1,"request_id":"11111111-1111-4111-8111-111111111111","data":{"relying_ref":"11111111-1111-4111-8111-111111111111","relying_\u0072ef":"11111111-1111-4111-8111-111111111111"}}"#;
    assert!(decode_request(Operation::Validate, raw).is_err());
}

#[test]
fn invalid_utf8_bom_trailing_json_and_oversize_are_rejected() {
    for raw in [
        vec![b'"', 0xc0, 0xaf, b'"'],
        b"\xef\xbb\xbf{}".to_vec(),
        b"{}{}".to_vec(),
        vec![b' '; 16_385],
    ] {
        assert_eq!(
            decode_request(Operation::Begin, &raw),
            Err(WireError::InvalidMessage)
        );
    }

    let mut exact_limit =
        br#"{"v":1,"request_id":"11111111-1111-4111-8111-111111111111","data":{}}"#.to_vec();
    exact_limit.resize(16_384, b' ');
    assert!(decode_request(Operation::Begin, &exact_limit).is_ok());
    exact_limit.push(b' ');
    assert!(decode_request(Operation::Begin, &exact_limit).is_err());
}

#[test]
fn preflight_counts_only_containers_and_enforces_depth_eight() {
    assert!(preflight(format!("{}0{}", "[".repeat(8), "]".repeat(8)).as_bytes()).is_ok());
    assert!(preflight(format!("{}0{}", "[".repeat(9), "]".repeat(9)).as_bytes()).is_err());
    assert!(preflight(br#"{"text":"[[[[[[[[["}"#).is_ok());
}

#[test]
fn numeric_fields_accept_integer_valued_json_lexemes() {
    for version in ["1.0", "1e0"] {
        let begin = format!(r#"{{"v":{version},"request_id":"{REQUEST_ID}","data":{{}}}}"#);
        assert!(decode_request(Operation::Begin, begin.as_bytes()).is_ok());
    }

    let issue = fixture("issue");
    let raw = String::from_utf8(bytes(&issue.success))
        .unwrap()
        .replace("2000000000", "2000000000.0");
    assert!(decode_response(Operation::Issue, raw.as_bytes(), REQUEST_ID).is_ok());

    let revocations = fixture("revocations");
    let raw = String::from_utf8(bytes(&revocations.request))
        .unwrap()
        .replace("\"limit\":50", "\"limit\":5e1");
    assert!(decode_request(Operation::Revocations, raw.as_bytes()).is_ok());
}

#[test]
fn numeric_fields_reject_fractional_nonfinite_and_out_of_range_values() {
    for version in ["1.5", "2", "-1", "1e400"] {
        let raw = format!(r#"{{"v":{version},"request_id":"{REQUEST_ID}","data":{{}}}}"#);
        assert!(decode_request(Operation::Begin, raw.as_bytes()).is_err());
    }
    let issue = fixture("issue");
    for expires_at in [json!(-1), json!(0.5), json!(253402300800u64)] {
        let mut value = issue.success.clone();
        value["data"]["expires_at"] = expires_at;
        rejects_response(Operation::Issue, &value);
    }
    let revocations = fixture("revocations");
    for limit in [json!(0), json!(50.5), json!(51)] {
        let mut value = revocations.request.clone();
        value["data"]["limit"] = limit;
        rejects_request(Operation::Revocations, &value);
    }
}

#[test]
fn strict_objects_reject_unknown_fields_and_success_error_ambiguity() {
    let begin = fixture("begin");
    let mut request = begin.request.clone();
    request["data"]["extra"] = json!(true);
    rejects_request(Operation::Begin, &request);

    let validate = fixture("validate");
    let mut success = validate.success.clone();
    success["data"]["subject"]["extra"] = json!(true);
    rejects_response(Operation::Validate, &success);

    let mut error = begin.error.clone();
    error["error"]["message"] = json!("SYNTHETIC_CANARY");
    rejects_response(Operation::Begin, &error);

    let mut both = begin.success.clone();
    both["error"] = json!({"code":"unavailable"});
    rejects_response(Operation::Begin, &both);
}

#[test]
fn trusted_operation_selects_the_only_valid_shape() {
    let begin = fixture("begin");
    assert!(decode_request(Operation::Session, &bytes(&begin.request)).is_ok());
    let issue = fixture("issue");
    rejects_request(Operation::Complete, &issue.request);

    let decoded = decode_request(Operation::Issue, &bytes(&issue.request)).unwrap();
    assert_eq!(
        encode_request(Operation::Complete, &decoded),
        Err(WireError::InvalidMessage)
    );
}

#[test]
fn response_request_id_must_match_expected_id() {
    let fixture = fixture("validate");
    assert_eq!(
        decode_response(Operation::Validate, &bytes(&fixture.success), OTHER_ID),
        Err(WireError::InvalidMessage)
    );
}

#[test]
fn canonical_decimal_ids_counters_and_cursors_enforce_ranges() {
    let ack = fixture("ack");
    for writer_epoch in ["0", "01", "+1", "1.0", "9223372036854775808"] {
        let mut request = ack.request.clone();
        request["data"]["writer_epoch"] = json!(writer_epoch);
        rejects_request(Operation::Ack, &request);
    }
    for through_cursor in ["00", "-1", "9223372036854775808"] {
        let mut request = ack.request.clone();
        request["data"]["through_cursor"] = json!(through_cursor);
        rejects_request(Operation::Ack, &request);
    }

    let validate = fixture("validate");
    for user_id in ["0", "01", "2147483648"] {
        let mut response = validate.success.clone();
        response["data"]["subject"]["user_id"] = json!(user_id);
        rejects_response(Operation::Validate, &response);
    }
}

#[test]
fn token_uuid_text_and_fixed_vocabulary_validation_rejects_wrong_branches() {
    let issue = fixture("issue");
    for token in [
        "A".repeat(42),
        format!("{}=", "A".repeat(43)),
        "+".repeat(43),
        format!("{}B", "A".repeat(42)),
    ] {
        let mut request = issue.request.clone();
        request["data"]["challenge"] = json!(token);
        rejects_request(Operation::Issue, &request);
    }

    let redeem = fixture("redeem");
    for (field, value) in [
        ("request_id", "11111111-1111-1111-8111-111111111111"),
        ("request_id", "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
    ] {
        let mut response = redeem.success.clone();
        response[field] = json!(value);
        rejects_response(Operation::Redeem, &response);
    }
    for (field, value) in [("issuer", "other"), ("audience", "other")] {
        let mut response = redeem.success.clone();
        response["data"][field] = json!(value);
        rejects_response(Operation::Redeem, &response);
    }
    let mut response = redeem.success.clone();
    response["data"]["subject"]["role"] = json!("owner");
    rejects_response(Operation::Redeem, &response);

    for name in [
        String::new(),
        "é".repeat(129),
        "hello\n".into(),
        "hello\u{85}".into(),
    ] {
        let mut response = redeem.success.clone();
        response["data"]["subject"]["display_name"] = json!(name);
        rejects_response(Operation::Redeem, &response);
    }
    let mut valid = redeem.success.clone();
    valid["data"]["subject"]["display_name"] = json!("é".repeat(128));
    assert!(decode_response(Operation::Redeem, &bytes(&valid), REQUEST_ID).is_ok());
}

#[test]
fn verified_email_matches_installed_zod_email_predicate_examples() {
    let validate = fixture("validate");
    for email in [
        "invented@example.test",
        "a+b@example.com",
        "o'reilly@example.com",
        "A@EXAMPLE.COM",
        "a_b@example.com",
        "a@b-.com",
    ] {
        let mut response = validate.success.clone();
        response["data"]["subject"]["verified_email"] = json!(email);
        assert!(decode_response(Operation::Validate, &bytes(&response), REQUEST_ID).is_ok());
    }
    for email in [
        "a..b@example.com",
        "x@y",
        "a@b.c",
        "a b@example.com",
        "a@-b.com",
        "a.@b.com",
        ".a@b.com",
        "a%25@example.com",
        "a@bücher.de",
    ] {
        let mut response = validate.success.clone();
        response["data"]["subject"]["verified_email"] = json!(email);
        rejects_response(Operation::Validate, &response);
    }

    let mut boundary = validate.success.clone();
    boundary["data"]["subject"]["verified_email"] = json!(format!("{}@b.co", "a".repeat(249)));
    assert!(decode_response(Operation::Validate, &bytes(&boundary), REQUEST_ID).is_ok());
    boundary["data"]["subject"]["verified_email"] = json!(format!("{}@b.co", "a".repeat(250)));
    rejects_response(Operation::Validate, &boundary);
}

#[test]
fn csrf_expiry_and_immutable_cutoff_ordering_are_enforced() {
    for name in ["complete", "session"] {
        let fixture = fixture(name);
        let mut response = fixture.success.clone();
        response["data"]["csrf_expires_at"] = json!(2000001001);
        rejects_response(operation(name), &response);
    }
    for name in ["redeem", "validate"] {
        let fixture = fixture(name);
        let mut response = fixture.success.clone();
        response["data"]["issued_after_cursor"] = json!("2");
        response["data"]["revocation_cursor"] = json!("1");
        rejects_response(operation(name), &response);
    }
    let redeem = fixture("redeem");
    let mut response = redeem.success.clone();
    response["data"]["relying_ref"] = response["data"]["subject"]["source_session_id"].clone();
    rejects_response(Operation::Redeem, &response);
}

#[test]
fn encoding_rechecks_semantic_relationships_and_bounds() {
    let session = fixture("session");
    let mut response =
        decode_response(Operation::Session, &bytes(&session.success), REQUEST_ID).unwrap();
    if let Response::Session(ResponseEnvelope::Success(envelope)) = &mut response {
        envelope.data.csrf_expires_at = UnixTime::new(2_000_001_001).unwrap();
    } else {
        panic!("decoded the wrong response branch");
    }
    assert_eq!(
        encode_response(Operation::Session, &response),
        Err(WireError::InvalidMessage)
    );

    let revocations = fixture("revocations");
    let mut response = decode_response(
        Operation::Revocations,
        &bytes(&revocations.success),
        REQUEST_ID,
    )
    .unwrap();
    if let Response::Revocations(ResponseEnvelope::Success(envelope)) = &mut response {
        let event = envelope.data.events[0].clone();
        envelope.data.events.resize(51, event);
    } else {
        panic!("decoded the wrong response branch");
    }
    assert_eq!(
        encode_response(Operation::Revocations, &response),
        Err(WireError::InvalidMessage)
    );
}

#[test]
fn every_event_scope_requires_exact_nullable_identity_fields() {
    let revocations = fixture("revocations");
    for (scope, user, org, session) in [
        ("session", json!("1"), json!("2"), json!(OTHER_ID)),
        ("account", json!("1"), Value::Null, Value::Null),
        ("org", Value::Null, json!("2"), Value::Null),
        ("membership", json!("1"), json!("2"), Value::Null),
    ] {
        let mut response = revocations.success.clone();
        response["data"]["events"][0]["scope"] = json!(scope);
        response["data"]["events"][0]["user_id"] = user;
        response["data"]["events"][0]["org_id"] = org;
        response["data"]["events"][0]["source_session_id"] = session;
        assert!(decode_response(Operation::Revocations, &bytes(&response), REQUEST_ID).is_ok());

        response["data"]["events"][0]["source_session_id"] = if scope == "session" {
            Value::Null
        } else {
            json!(OTHER_ID)
        };
        rejects_response(Operation::Revocations, &response);
    }
}

#[test]
fn revocation_pages_require_unique_contiguous_ordered_events_and_valid_watermarks() {
    let revocations = fixture("revocations");
    for cursors in [["1", "3"], ["2", "1"]] {
        let mut response = revocations.success.clone();
        let base = response["data"]["events"][0].clone();
        response["data"]["events"] = json!([
            base,
            {
                "event_id": OTHER_ID,
                "cursor": cursors[1],
                "scope": "membership",
                "user_id": "1",
                "org_id": "2",
                "source_session_id": null
            }
        ]);
        response["data"]["events"][0]["cursor"] = json!(cursors[0]);
        response["data"]["next_cursor"] = json!(cursors[1]);
        rejects_response(Operation::Revocations, &response);
    }

    let mut duplicate = revocations.success.clone();
    let event = duplicate["data"]["events"][0].clone();
    duplicate["data"]["events"] = json!([event.clone(), event]);
    rejects_response(Operation::Revocations, &duplicate);

    let mut wrong_next = revocations.success.clone();
    wrong_next["data"]["next_cursor"] = json!("0");
    rejects_response(Operation::Revocations, &wrong_next);

    let mut beyond_watermark = revocations.success.clone();
    beyond_watermark["data"]["events"] = json!([]);
    beyond_watermark["data"]["next_cursor"] = json!("2");
    beyond_watermark["data"]["high_watermark"] = json!("1");
    rejects_response(Operation::Revocations, &beyond_watermark);

    let mut overflow = revocations.success.clone();
    let first = overflow["data"]["events"][0].clone();
    let mut second = first.clone();
    second["event_id"] = json!(OTHER_ID);
    overflow["data"]["events"] = json!([first, second]);
    rejects_response(Operation::Revocations, &overflow);
}

#[test]
fn revocation_page_accepts_fifty_events_but_rejects_fifty_one() {
    let revocations = fixture("revocations");
    let mut response = revocations.success.clone();
    let base = response["data"]["events"][0].clone();
    response["data"]["events"] = Value::Array(
        (0..50)
            .map(|index| {
                let mut event = base.clone();
                event["event_id"] = json!(format!("11111111-1111-4111-8111-{index:012}"));
                event["cursor"] = json!((index + 1).to_string());
                event
            })
            .collect(),
    );
    response["data"]["next_cursor"] = json!("50");
    response["data"]["high_watermark"] = json!("50");
    let decoded = decode_response(Operation::Revocations, &bytes(&response), REQUEST_ID).unwrap();
    assert!(
        encode_response(Operation::Revocations, &decoded)
            .unwrap()
            .len()
            <= 16_384
    );

    response["data"]["events"]
        .as_array_mut()
        .unwrap()
        .push(base);
    rejects_response(Operation::Revocations, &response);
}
