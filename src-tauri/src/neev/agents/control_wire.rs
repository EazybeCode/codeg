use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt;

const MAGIC: &[u8; 8] = b"NEEVCTL1";
const HELLO_DOMAIN: &[u8] = b"neev-control-v1/hello\0";
const REQUEST_DOMAIN: &[u8] = b"neev-control-v1/request\0";
const RESPONSE_DOMAIN: &[u8] = b"neev-control-v1/response\0";
const MAC_SIZE: usize = 32;
const REQUEST_PAYLOAD_MAX: usize = 8192;
const RESPONSE_PAYLOAD_MAX: usize = 16384;
const FRAME_MAX: usize = 65536;
const CLIENT_HELLO_SIZE: usize = MAGIC.len() + 32;
const SERVER_HELLO_PREFIX_SIZE: usize = MAGIC.len() + 16 + 32 + 32;
const SERVER_HELLO_SIZE: usize = SERVER_HELLO_PREFIX_SIZE + MAC_SIZE;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WireContext {
    client_nonce: [u8; 32],
    server_challenge: [u8; 32],
    supervisor_boot_id: [u8; 16],
}

impl WireContext {
    pub(crate) fn new(
        client_nonce: [u8; 32],
        server_challenge: [u8; 32],
        supervisor_boot_id: [u8; 16],
    ) -> Self {
        Self {
            client_nonce,
            server_challenge,
            supervisor_boot_id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    Invalid,
    Auth,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid",
            Self::Auth => "auth",
        })
    }
}

impl std::error::Error for WireError {}

pub(crate) fn encode_client_hello(client_nonce: &[u8]) -> Result<Vec<u8>, WireError> {
    let client_nonce: &[u8; 32] = client_nonce.try_into().map_err(|_| WireError::Invalid)?;
    let mut hello = Vec::with_capacity(CLIENT_HELLO_SIZE);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(client_nonce);
    Ok(hello)
}

pub(crate) fn decode_client_hello(raw: &[u8]) -> Result<[u8; 32], WireError> {
    if raw.len() != CLIENT_HELLO_SIZE || &raw[..MAGIC.len()] != MAGIC {
        return Err(WireError::Invalid);
    }
    raw[MAGIC.len()..]
        .try_into()
        .map_err(|_| WireError::Invalid)
}

pub(crate) fn encode_server_hello(
    key: &[u8; 32],
    context: &WireContext,
) -> Result<Vec<u8>, WireError> {
    let mut hello = Vec::with_capacity(SERVER_HELLO_SIZE);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(&context.supervisor_boot_id);
    hello.extend_from_slice(&context.client_nonce);
    hello.extend_from_slice(&context.server_challenge);
    let tag = calculate_mac(key, &[HELLO_DOMAIN, &hello])?;
    hello.extend_from_slice(&tag);
    Ok(hello)
}

pub(crate) fn decode_server_hello(
    key: &[u8; 32],
    expected_client_nonce: &[u8],
    raw: &[u8],
) -> Result<WireContext, WireError> {
    let expected_client_nonce: &[u8; 32] = expected_client_nonce
        .try_into()
        .map_err(|_| WireError::Invalid)?;
    if raw.len() != SERVER_HELLO_SIZE || &raw[..MAGIC.len()] != MAGIC {
        return Err(WireError::Invalid);
    }

    let prefix = &raw[..SERVER_HELLO_PREFIX_SIZE];
    verify_mac(
        key,
        &[HELLO_DOMAIN, prefix],
        &raw[SERVER_HELLO_PREFIX_SIZE..],
    )?;
    let boot_start = MAGIC.len();
    let nonce_start = boot_start + 16;
    let challenge_start = nonce_start + 32;
    let echoed_nonce: [u8; 32] = raw[nonce_start..challenge_start]
        .try_into()
        .map_err(|_| WireError::Invalid)?;
    if expected_client_nonce != &echoed_nonce {
        return Err(WireError::Auth);
    }
    Ok(WireContext::new(
        echoed_nonce,
        raw[challenge_start..SERVER_HELLO_PREFIX_SIZE]
            .try_into()
            .map_err(|_| WireError::Invalid)?,
        raw[boot_start..nonce_start]
            .try_into()
            .map_err(|_| WireError::Invalid)?,
    ))
}

pub(crate) fn encode_request(
    key: &[u8; 32],
    context: &WireContext,
    payload: &[u8],
) -> Result<Vec<u8>, WireError> {
    encode_frame(
        key,
        context,
        REQUEST_DOMAIN,
        None,
        payload,
        REQUEST_PAYLOAD_MAX,
    )
}

pub(crate) fn decode_request(
    key: &[u8; 32],
    context: &WireContext,
    frame: &[u8],
) -> Result<Vec<u8>, WireError> {
    decode_frame(
        key,
        context,
        REQUEST_DOMAIN,
        None,
        frame,
        REQUEST_PAYLOAD_MAX,
    )
}

pub(crate) fn encode_response(
    key: &[u8; 32],
    context: &WireContext,
    request_payload: &[u8],
    response_payload: &[u8],
) -> Result<Vec<u8>, WireError> {
    validate_payload(request_payload, REQUEST_PAYLOAD_MAX)?;
    let request_digest = Sha256::digest(request_payload);
    encode_frame(
        key,
        context,
        RESPONSE_DOMAIN,
        Some(&request_digest),
        response_payload,
        RESPONSE_PAYLOAD_MAX,
    )
}

pub(crate) fn decode_response(
    key: &[u8; 32],
    context: &WireContext,
    request_payload: &[u8],
    frame: &[u8],
) -> Result<Vec<u8>, WireError> {
    validate_payload(request_payload, REQUEST_PAYLOAD_MAX)?;
    let request_digest = Sha256::digest(request_payload);
    decode_frame(
        key,
        context,
        RESPONSE_DOMAIN,
        Some(&request_digest),
        frame,
        RESPONSE_PAYLOAD_MAX,
    )
}

fn validate_payload(payload: &[u8], maximum: usize) -> Result<(), WireError> {
    if payload.is_empty() || payload.len() > maximum {
        return Err(WireError::Invalid);
    }
    Ok(())
}

fn encode_frame(
    key: &[u8; 32],
    context: &WireContext,
    domain: &[u8],
    request_digest: Option<&[u8]>,
    payload: &[u8],
    payload_maximum: usize,
) -> Result<Vec<u8>, WireError> {
    validate_payload(payload, payload_maximum)?;
    let length = u32::try_from(payload.len())
        .map_err(|_| WireError::Invalid)?
        .to_be_bytes();
    let mut frame = Vec::with_capacity(4 + payload.len() + MAC_SIZE);
    frame.extend_from_slice(&length);
    frame.extend_from_slice(payload);

    let mut parts = vec![
        domain,
        &context.client_nonce,
        &context.server_challenge,
        &context.supervisor_boot_id,
    ];
    if let Some(request_digest) = request_digest {
        parts.push(request_digest);
    }
    parts.push(&frame);
    let tag = calculate_mac(key, &parts)?;
    let mut output = frame;
    output.extend_from_slice(&tag);
    if output.len() > FRAME_MAX {
        return Err(WireError::Invalid);
    }
    Ok(output)
}

fn decode_frame(
    key: &[u8; 32],
    context: &WireContext,
    domain: &[u8],
    request_digest: Option<&[u8]>,
    frame: &[u8],
    payload_maximum: usize,
) -> Result<Vec<u8>, WireError> {
    if frame.len() < 4 + MAC_SIZE || frame.len() > FRAME_MAX {
        return Err(WireError::Invalid);
    }
    let declared_length =
        u32::from_be_bytes(frame[..4].try_into().map_err(|_| WireError::Invalid)?) as usize;
    if declared_length == 0 || declared_length > payload_maximum {
        return Err(WireError::Invalid);
    }
    let body_size = 4usize
        .checked_add(declared_length)
        .ok_or(WireError::Invalid)?;
    let expected_size = body_size.checked_add(MAC_SIZE).ok_or(WireError::Invalid)?;
    if frame.len() != expected_size {
        return Err(WireError::Invalid);
    }

    let body = &frame[..body_size];
    let mut parts = vec![
        domain,
        &context.client_nonce,
        &context.server_challenge,
        &context.supervisor_boot_id,
    ];
    if let Some(request_digest) = request_digest {
        parts.push(request_digest);
    }
    parts.push(body);
    verify_mac(key, &parts, &frame[body_size..])?;
    Ok(frame[4..body_size].to_vec())
}

fn calculate_mac(key: &[u8; 32], parts: &[&[u8]]) -> Result<[u8; 32], WireError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| WireError::Invalid)?;
    for part in parts {
        mac.update(part);
    }
    Ok(mac.finalize().into_bytes().into())
}

fn verify_mac(key: &[u8; 32], parts: &[&[u8]], actual: &[u8]) -> Result<(), WireError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| WireError::Invalid)?;
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(actual).map_err(|_| WireError::Auth)
}
