use super::routes;
use super::wire::{decode_response, encode_request};
use super::wire_types::*;
use crate::neev::test_support as h;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

const REQ: &str = "11111111-1111-4111-8111-111111111111";

fn tok(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn clock(now: i64) -> h::TestClock {
    h::TestClock::new(now)
}
fn req_id() -> UuidV4 {
    UuidV4::new(REQ.to_owned()).unwrap()
}
fn token(value: &str) -> Token {
    Token::new(value.to_owned()).unwrap()
}
fn uuid(value: &str) -> UuidV4 {
    UuidV4::new(value.to_owned()).unwrap()
}

fn issue_bytes(attempt: &str, challenge: &str) -> Vec<u8> {
    encode_request(
        Operation::Issue,
        &Request::Issue(Envelope {
            v: Version,
            request_id: req_id(),
            data: IssueRequestData {
                attempt_id: token(attempt),
                challenge: token(challenge),
            },
        }),
    )
    .unwrap()
}
fn redeem_bytes(attempt: &str, code: &str, verifier: &str) -> Vec<u8> {
    encode_request(
        Operation::Redeem,
        &Request::Redeem(Envelope {
            v: Version,
            request_id: req_id(),
            data: RedeemRequestData {
                attempt_id: token(attempt),
                code: token(code),
                verifier: token(verifier),
            },
        }),
    )
    .unwrap()
}
fn validate_bytes(relying_ref: &str) -> Vec<u8> {
    encode_request(
        Operation::Validate,
        &Request::Validate(Envelope {
            v: Version,
            request_id: req_id(),
            data: ValidateRequestData {
                relying_ref: uuid(relying_ref),
            },
        }),
    )
    .unwrap()
}

fn issued_code(body: &[u8]) -> String {
    match decode_response(Operation::Issue, body, REQ).unwrap() {
        Response::Issue(ResponseEnvelope::Success(env)) => env.data.code.as_str().to_owned(),
        _ => panic!("expected issue success"),
    }
}
fn redeemed(body: &[u8]) -> (String, String, String) {
    match decode_response(Operation::Redeem, body, REQ).unwrap() {
        Response::Redeem(ResponseEnvelope::Success(env)) => (
            env.data.relying_ref.as_str().to_owned(),
            env.data.subject.user_id.as_str().to_owned(),
            env.data.subject.verified_email.as_str().to_owned(),
        ),
        _ => panic!("expected redeem success"),
    }
}

#[tokio::test]
async fn sqlite_gateway_routes_full_wire_pipeline() {
    let db = h::database().await;
    let user = h::native_account(&db, "routes-sub", "routes@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let ctx = h::scoped_context(&db, user, org, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);

    // issue
    let out = routes::issue(&db, &ctx, &clock(2000), &issue_bytes(&tok(1), &challenge)).await;
    assert_eq!(out.status, 200);
    let code = issued_code(&out.body);

    // redeem
    let out = routes::redeem(&db, &clock(2001), &redeem_bytes(&tok(1), &code, &verifier)).await;
    assert_eq!(out.status, 200);
    let (relying_ref, subject_user, email) = redeemed(&out.body);
    assert_eq!(subject_user, user.to_string());
    assert_eq!(email, "routes@example.test");

    // validate
    let out = routes::validate(&db, &clock(2002), &validate_bytes(&relying_ref)).await;
    assert_eq!(out.status, 200);
    match decode_response(Operation::Validate, &out.body, REQ).unwrap() {
        Response::Validate(ResponseEnvelope::Success(env)) => {
            assert_eq!(env.data.subject.user_id.as_str(), user.to_string());
        }
        _ => panic!("expected validate success"),
    }

    // revoke
    let revoke = encode_request(
        Operation::Revoke,
        &Request::Revoke(Envelope {
            v: Version,
            request_id: req_id(),
            data: RevokeRequestData {
                relying_ref: uuid(&relying_ref),
                intent_id: uuid("55555555-5555-4555-8555-555555555555"),
            },
        }),
    )
    .unwrap();
    let out = routes::revoke(&db, &clock(2100), &revoke).await;
    assert_eq!(out.status, 200);
    let cursor = match decode_response(Operation::Revoke, &out.body, REQ).unwrap() {
        Response::Revoke(ResponseEnvelope::Success(env)) => env.data.cursor.as_str().to_owned(),
        _ => panic!("expected revoke success"),
    };
    assert_eq!(cursor, "1");

    // revocations
    let page = encode_request(
        Operation::Revocations,
        &Request::Revocations(Envelope {
            v: Version,
            request_id: req_id(),
            data: RevocationsRequestData {
                after_cursor: Cursor::new("0".to_owned()).unwrap(),
                limit: Limit::new(50).unwrap(),
            },
        }),
    )
    .unwrap();
    let out = routes::revocations(&db, &page).await;
    assert_eq!(out.status, 200);
    match decode_response(Operation::Revocations, &out.body, REQ).unwrap() {
        Response::Revocations(ResponseEnvelope::Success(env)) => {
            assert_eq!(env.data.next_cursor.as_str(), "1");
            assert_eq!(env.data.high_watermark.as_str(), "1");
        }
        _ => panic!("expected revocations success"),
    }

    // ack
    let ack = encode_request(
        Operation::Ack,
        &Request::Ack(Envelope {
            v: Version,
            request_id: req_id(),
            data: AckRequestData {
                through_cursor: Cursor::new("1".to_owned()).unwrap(),
                writer_epoch: Counter::new("1".to_owned()).unwrap(),
            },
        }),
    )
    .unwrap();
    let out = routes::ack(&db, &ack).await;
    assert_eq!(out.status, 200);
    match decode_response(Operation::Ack, &out.body, REQ).unwrap() {
        Response::Ack(ResponseEnvelope::Success(env)) => {
            assert_eq!(env.data.acknowledged_cursor.as_str(), "1");
        }
        _ => panic!("expected ack success"),
    }
}

#[tokio::test]
async fn sqlite_gateway_routes_map_error_codes() {
    let db = h::database().await;
    let user = h::native_account(&db, "routes-sub", "routes@example.test").await;
    let (org, _) = h::organization(&db, user, "admin").await;
    let ctx = h::scoped_context(&db, user, org, 1000).await;
    let verifier = tok(9);
    let challenge = challenge_for(&verifier);
    let code = issued_code(
        &routes::issue(&db, &ctx, &clock(2000), &issue_bytes(&tok(1), &challenge)).await.body,
    );

    // Wrong verifier -> 401 unauthorized.
    let out = routes::redeem(&db, &clock(2001), &redeem_bytes(&tok(1), &code, &tok(8))).await;
    assert_eq!(out.status, 401);
    assert!(matches!(
        decode_response(Operation::Redeem, &out.body, REQ).unwrap().error_code(),
        Some(ErrorCode::Unauthorized)
    ));

    // Redeem, then bump membership revision -> validate is authority_changed (409).
    let (relying_ref, _, _) = redeemed(
        &routes::redeem(&db, &clock(2001), &redeem_bytes(&tok(1), &code, &verifier)).await.body,
    );
    h::execute(
        &db,
        "UPDATE neev_memberships SET revision=revision+1 WHERE org_id=$1 AND user_id=$2",
        vec![org.into(), user.into()],
    )
    .await;
    let out = routes::validate(&db, &clock(2002), &validate_bytes(&relying_ref)).await;
    assert_eq!(out.status, 409);
    assert!(matches!(
        decode_response(Operation::Validate, &out.body, REQ).unwrap().error_code(),
        Some(ErrorCode::AuthorityChanged)
    ));

    // Malformed body -> 400, empty body.
    let out = routes::redeem(&db, &clock(2003), b"{not valid").await;
    assert_eq!(out.status, 400);
    assert!(out.body.is_empty());
}
