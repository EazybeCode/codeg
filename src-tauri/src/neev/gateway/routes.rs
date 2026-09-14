//! Gateway request handlers: decode a bounded wire envelope, drive the matching
//! `issuer` operation, and encode the exact wire response (success or a
//! fixed-code error envelope). Kept inside `gateway` so it can construct wire
//! types whose envelope version field is module-private.
//!
//! These functions are transport-agnostic: the browser `issue` handler is
//! mounted on the public router (native session + Origin + CSRF, integration-
//! gated); the private `redeem/validate/revoke/revocations/ack` handlers are
//! consumed by the private mTLS listener at deploy time. `IssuerError` maps to
//! the exact wire code, preserving `authority_changed` distinct from `conflict`.
#![allow(dead_code)]

use super::issuer::{self, IssuerError};
use super::wire::{decode_request, encode_response};
use super::wire_types::*;
use crate::neev::clock::Clock;
use crate::neev::types::{OrgContext, Role};
use sea_orm::DatabaseConnection;

/// HTTP status + encoded wire body for the transport layer to return verbatim.
pub(crate) struct Encoded {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

fn error_code(error: IssuerError) -> (u16, ErrorCode) {
    match error {
        IssuerError::Invalid => (400, ErrorCode::InvalidRequest),
        IssuerError::Unauthorized => (401, ErrorCode::Unauthorized),
        IssuerError::Forbidden => (403, ErrorCode::Forbidden),
        IssuerError::AuthorityChanged => (409, ErrorCode::AuthorityChanged),
        IssuerError::Conflict => (409, ErrorCode::Conflict),
        IssuerError::Limited => (429, ErrorCode::RateLimited),
        IssuerError::Unavailable => (503, ErrorCode::Unavailable),
    }
}

fn encoded(op: Operation, response: &Response, status: u16) -> Encoded {
    match encode_response(op, response) {
        Ok(body) => Encoded { status, body },
        // A well-formed operation result must always encode; treat the
        // impossible case as unavailable rather than emit a partial body.
        Err(_) => Encoded {
            status: 503,
            body: Vec::new(),
        },
    }
}

fn error_encoded(op: Operation, request_id: UuidV4, error: IssuerError) -> Encoded {
    let (status, code) = error_code(error);
    let envelope = ErrorEnvelope {
        v: Version,
        request_id,
        error: ErrorBody { code },
    };
    let response = match op {
        Operation::Issue => Response::Issue(ResponseEnvelope::Error(envelope)),
        Operation::Redeem => Response::Redeem(ResponseEnvelope::Error(envelope)),
        Operation::Validate => Response::Validate(ResponseEnvelope::Error(envelope)),
        Operation::Revoke => Response::Revoke(ResponseEnvelope::Error(envelope)),
        Operation::Revocations => Response::Revocations(ResponseEnvelope::Error(envelope)),
        Operation::Ack => Response::Ack(ResponseEnvelope::Error(envelope)),
        _ => {
            return Encoded {
                status: 400,
                body: Vec::new(),
            }
        }
    };
    encoded(op, &response, status)
}

fn wire_subject(subject: &issuer::Subject) -> Result<Subject, IssuerError> {
    let fail = || IssuerError::Unavailable;
    Ok(Subject {
        user_id: NativeId::new(subject.user_id.to_string()).map_err(|()| fail())?,
        org_id: NativeId::new(subject.org_id.to_string()).map_err(|()| fail())?,
        source_session_id: UuidV4::new(subject.source_session_id.clone()).map_err(|()| fail())?,
        account_revision: Counter::new(subject.account_revision.to_string()).map_err(|()| fail())?,
        org_revision: Counter::new(subject.org_revision.to_string()).map_err(|()| fail())?,
        member_revision: Counter::new(subject.member_revision.to_string()).map_err(|()| fail())?,
        role: match subject.role {
            Role::Admin => super::wire_types::Role::Admin,
            Role::Member => super::wire_types::Role::Member,
        },
        display_name: Name::new(subject.display_name.clone()).map_err(|()| fail())?,
        org_name: Name::new(subject.org_name.clone()).map_err(|()| fail())?,
        verified_email: Email::new(subject.verified_email.clone()).map_err(|()| fail())?,
    })
}

fn cursor(value: i64) -> Result<Cursor, IssuerError> {
    Cursor::new(value.to_string()).map_err(|()| IssuerError::Unavailable)
}
fn counter(value: i64) -> Result<Counter, IssuerError> {
    Counter::new(value.to_string()).map_err(|()| IssuerError::Unavailable)
}
fn unix(value: i64) -> Result<UnixTime, IssuerError> {
    UnixTime::new(u64::try_from(value).map_err(|_| IssuerError::Unavailable)?)
        .map_err(|()| IssuerError::Unavailable)
}

/// Browser-facing exchange-issue (native org session already verified by the
/// transport). `{attempt_id, challenge}` -> `{code, expires_at}`.
pub(crate) async fn issue(
    database: &DatabaseConnection,
    context: &OrgContext,
    clock: &dyn Clock,
    raw: &[u8],
) -> Encoded {
    let Ok(Request::Issue(request)) = decode_request(Operation::Issue, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let issued = issuer::issue(
            database,
            context,
            request.data.attempt_id.as_str(),
            request.data.challenge.as_str(),
            clock,
        )
        .await?;
        let data = IssueSuccessData {
            code: Token::new(issued.code).map_err(|()| IssuerError::Unavailable)?,
            expires_at: unix(issued.expires_at)?,
        };
        Ok::<_, IssuerError>(Response::Issue(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Issue, &response, 200),
        Err(error) => error_encoded(Operation::Issue, request_id, error),
    }
}

/// Private redemption over the authenticated boundary.
pub(crate) async fn redeem(database: &DatabaseConnection, clock: &dyn Clock, raw: &[u8]) -> Encoded {
    let Ok(Request::Redeem(request)) = decode_request(Operation::Redeem, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let receipt = issuer::redeem(
            database,
            request.data.attempt_id.as_str(),
            request.data.code.as_str(),
            request.data.verifier.as_str(),
            clock,
        )
        .await?;
        let data = RedeemSuccessData {
            receipt_id: UuidV4::new(receipt.receipt_id).map_err(|()| IssuerError::Unavailable)?,
            issuer: Issuer::NeevNativeV1,
            audience: Audience::NeevCollaborationV1,
            relying_ref: UuidV4::new(receipt.relying_ref).map_err(|()| IssuerError::Unavailable)?,
            subject: wire_subject(&receipt.subject)?,
            expires_at: unix(receipt.expires_at)?,
            issued_after_cursor: cursor(receipt.issued_after_cursor)?,
            revocation_cursor: cursor(receipt.revocation_cursor)?,
        };
        Ok::<_, IssuerError>(Response::Redeem(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Redeem, &response, 200),
        Err(error) => error_encoded(Operation::Redeem, request_id, error),
    }
}

/// Private per-request validation of a relying reference.
pub(crate) async fn validate(
    database: &DatabaseConnection,
    clock: &dyn Clock,
    raw: &[u8],
) -> Encoded {
    let Ok(Request::Validate(request)) = decode_request(Operation::Validate, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let validated = issuer::validate(database, request.data.relying_ref.as_str(), clock).await?;
        let data = ValidateSuccessData {
            subject: wire_subject(&validated.subject)?,
            expires_at: unix(validated.expires_at)?,
            issued_after_cursor: cursor(validated.issued_after_cursor)?,
            revocation_cursor: cursor(validated.revocation_cursor)?,
        };
        Ok::<_, IssuerError>(Response::Validate(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Validate, &response, 200),
        Err(error) => error_encoded(Operation::Validate, request_id, error),
    }
}

/// Private session revoke (idempotent by intent).
pub(crate) async fn revoke(database: &DatabaseConnection, clock: &dyn Clock, raw: &[u8]) -> Encoded {
    let Ok(Request::Revoke(request)) = decode_request(Operation::Revoke, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let outcome = issuer::revoke(
            database,
            request.data.relying_ref.as_str(),
            request.data.intent_id.as_str(),
            clock,
        )
        .await?;
        let data = RevokeSuccessData {
            event_id: UuidV4::new(outcome.event_id).map_err(|()| IssuerError::Unavailable)?,
            cursor: counter(outcome.cursor)?,
            state: RevokeState::Revoked,
        };
        Ok::<_, IssuerError>(Response::Revoke(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Revoke, &response, 200),
        Err(error) => error_encoded(Operation::Revoke, request_id, error),
    }
}

/// Private contiguous revocation page read.
pub(crate) async fn revocations(database: &DatabaseConnection, raw: &[u8]) -> Encoded {
    let Ok(Request::Revocations(request)) = decode_request(Operation::Revocations, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let after = request
            .data
            .after_cursor
            .as_str()
            .parse::<i64>()
            .map_err(|_| IssuerError::Invalid)?;
        let limit = i64::from(request.data.limit.get());
        let page = issuer::revocations(database, after, limit).await?;
        let mut events = Vec::with_capacity(page.events.len());
        for event in &page.events {
            events.push(RevocationEvent {
                event_id: UuidV4::new(event.event_id.clone()).map_err(|()| IssuerError::Unavailable)?,
                cursor: counter(event.cursor)?,
                scope: match event.scope.as_str() {
                    "session" => EventScope::Session,
                    "account" => EventScope::Account,
                    "org" => EventScope::Org,
                    "membership" => EventScope::Membership,
                    _ => return Err(IssuerError::Unavailable),
                },
                user_id: RequiredNullable::new(match event.user_id {
                    Some(value) => Some(NativeId::new(value.to_string()).map_err(|()| IssuerError::Unavailable)?),
                    None => None,
                }),
                org_id: RequiredNullable::new(match event.org_id {
                    Some(value) => Some(NativeId::new(value.to_string()).map_err(|()| IssuerError::Unavailable)?),
                    None => None,
                }),
                source_session_id: RequiredNullable::new(match &event.source_session_id {
                    Some(value) => Some(UuidV4::new(value.clone()).map_err(|()| IssuerError::Unavailable)?),
                    None => None,
                }),
            });
        }
        let data = RevocationsSuccessData {
            events,
            next_cursor: cursor(page.next_cursor)?,
            high_watermark: cursor(page.high_watermark)?,
        };
        Ok::<_, IssuerError>(Response::Revocations(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Revocations, &response, 200),
        Err(error) => error_encoded(Operation::Revocations, request_id, error),
    }
}

/// Private monotonic acknowledgement.
pub(crate) async fn ack(database: &DatabaseConnection, raw: &[u8]) -> Encoded {
    let Ok(Request::Ack(request)) = decode_request(Operation::Ack, raw) else {
        return Encoded {
            status: 400,
            body: Vec::new(),
        };
    };
    let request_id = request.request_id.clone();
    let build = || async {
        let through = request
            .data
            .through_cursor
            .as_str()
            .parse::<i64>()
            .map_err(|_| IssuerError::Invalid)?;
        let epoch = request
            .data
            .writer_epoch
            .as_str()
            .parse::<i64>()
            .map_err(|_| IssuerError::Invalid)?;
        let acknowledged = issuer::ack(database, through, epoch).await?;
        let data = AckSuccessData {
            acknowledged_cursor: cursor(acknowledged)?,
        };
        Ok::<_, IssuerError>(Response::Ack(ResponseEnvelope::Success(Envelope {
            v: Version,
            request_id: request.request_id.clone(),
            data,
        })))
    };
    match build().await {
        Ok(response) => encoded(Operation::Ack, &response, 200),
        Err(error) => error_encoded(Operation::Ack, request_id, error),
    }
}
