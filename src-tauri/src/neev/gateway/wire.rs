#![allow(dead_code)]

use super::wire_types::*;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;

const MAX_BYTES: usize = 16_384;
const MAX_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    InvalidMessage,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid_gateway_message")
    }
}

impl std::error::Error for WireError {}

pub(crate) fn decode_request(op: Operation, raw: &[u8]) -> Result<Request, WireError> {
    match op {
        Operation::Begin => decode::<BeginRequest>(raw).map(Request::Begin),
        Operation::Issue => decode::<IssueRequest>(raw).map(Request::Issue),
        Operation::Complete => decode::<CompleteRequest>(raw).map(Request::Complete),
        Operation::Session => decode::<SessionRequest>(raw).map(Request::Session),
        Operation::Redeem => decode::<RedeemRequest>(raw).map(Request::Redeem),
        Operation::Validate => decode::<ValidateRequest>(raw).map(Request::Validate),
        Operation::Logout => decode::<LogoutRequest>(raw).map(Request::Logout),
        Operation::Revoke => decode::<RevokeRequest>(raw).map(Request::Revoke),
        Operation::Revocations => decode::<RevocationsRequest>(raw).map(Request::Revocations),
        Operation::Ack => decode::<AckRequest>(raw).map(Request::Ack),
    }
}

pub(crate) fn decode_response(
    op: Operation,
    raw: &[u8],
    expected_request_id: &str,
) -> Result<Response, WireError> {
    let response = match op {
        Operation::Begin => decode::<BeginResponse>(raw).map(Response::Begin),
        Operation::Issue => decode::<IssueResponse>(raw).map(Response::Issue),
        Operation::Complete => decode::<CompleteResponse>(raw).map(Response::Complete),
        Operation::Session => decode::<SessionResponse>(raw).map(Response::Session),
        Operation::Redeem => decode::<RedeemResponse>(raw).map(Response::Redeem),
        Operation::Validate => decode::<ValidateResponse>(raw).map(Response::Validate),
        Operation::Logout => decode::<LogoutResponse>(raw).map(Response::Logout),
        Operation::Revoke => decode::<RevokeResponse>(raw).map(Response::Revoke),
        Operation::Revocations => decode::<RevocationsResponse>(raw).map(Response::Revocations),
        Operation::Ack => decode::<AckResponse>(raw).map(Response::Ack),
    }?;
    if response.request_id() != expected_request_id {
        return Err(WireError::InvalidMessage);
    }
    Ok(response)
}

pub(crate) fn encode_request(op: Operation, request: &Request) -> Result<Vec<u8>, WireError> {
    if request.operation() != op || !request.valid() {
        return Err(WireError::InvalidMessage);
    }
    match request {
        Request::Begin(value) => encode(value),
        Request::Issue(value) => encode(value),
        Request::Complete(value) => encode(value),
        Request::Session(value) => encode(value),
        Request::Redeem(value) => encode(value),
        Request::Validate(value) => encode(value),
        Request::Logout(value) => encode(value),
        Request::Revoke(value) => encode(value),
        Request::Revocations(value) => encode(value),
        Request::Ack(value) => encode(value),
    }
}

pub(crate) fn encode_response(op: Operation, response: &Response) -> Result<Vec<u8>, WireError> {
    if response.operation() != op || !response.valid() {
        return Err(WireError::InvalidMessage);
    }
    match response {
        Response::Begin(value) => encode(value),
        Response::Issue(value) => encode(value),
        Response::Complete(value) => encode(value),
        Response::Session(value) => encode(value),
        Response::Redeem(value) => encode(value),
        Response::Validate(value) => encode(value),
        Response::Logout(value) => encode(value),
        Response::Revoke(value) => encode(value),
        Response::Revocations(value) => encode(value),
        Response::Ack(value) => encode(value),
    }
}

fn decode<T>(raw: &[u8]) -> Result<T, WireError>
where
    T: DeserializeOwned + Validated,
{
    preflight(raw)?;
    let value: T = serde_json::from_slice(raw).map_err(|_| WireError::InvalidMessage)?;
    if !value.valid() {
        return Err(WireError::InvalidMessage);
    }
    Ok(value)
}

fn encode<T>(value: &T) -> Result<Vec<u8>, WireError>
where
    T: Serialize + Validated,
{
    if !value.valid() {
        return Err(WireError::InvalidMessage);
    }
    let raw = serde_json::to_vec(value).map_err(|_| WireError::InvalidMessage)?;
    preflight(&raw)?;
    Ok(raw)
}

pub(super) fn preflight(raw: &[u8]) -> Result<(), WireError> {
    if raw.len() > MAX_BYTES {
        return Err(WireError::InvalidMessage);
    }
    let source = std::str::from_utf8(raw).map_err(|_| WireError::InvalidMessage)?;
    if source.starts_with('\u{feff}') {
        return Err(WireError::InvalidMessage);
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in source.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.checked_add(1).ok_or(WireError::InvalidMessage)?;
                if depth > MAX_DEPTH {
                    return Err(WireError::InvalidMessage);
                }
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or(WireError::InvalidMessage)?;
            }
            _ => {}
        }
    }
    Ok(())
}
