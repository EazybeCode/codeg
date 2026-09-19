use super::control_wire::{
    decode_client_hello, decode_request, decode_response, decode_server_hello, encode_client_hello,
    encode_request, encode_response, encode_server_hello, WireContext, WireError,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

const REQUEST_LIMIT: usize = 8192;
const RESPONSE_LIMIT: usize = 16384;

#[derive(Deserialize)]
struct Fixture {
    key_hex: String,
    client_nonce_hex: String,
    supervisor_boot_id_hex: String,
    server_challenge_hex: String,
    request_payload_hex: String,
    response_payload_hex: String,
    client_hello_hex: String,
    server_hello_hex: String,
    request_frame_hex: String,
    response_frame_hex: String,
}

struct Inputs {
    key: [u8; 32],
    client_nonce: [u8; 32],
    boot_id: [u8; 16],
    challenge: [u8; 32],
    request_payload: Vec<u8>,
    response_payload: Vec<u8>,
    client_hello: Vec<u8>,
    server_hello: Vec<u8>,
    request_frame: Vec<u8>,
    response_frame: Vec<u8>,
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len() % 2 == 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(digits, 16).unwrap()
        })
        .collect()
}

fn array<const N: usize>(value: &str) -> [u8; N] {
    decode_hex(value).try_into().ok().unwrap()
}

fn inputs() -> Inputs {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../scripts/tests/fixtures/neev-control-wire-v1.json"
    ))
    .unwrap();
    Inputs {
        key: array(&fixture.key_hex),
        client_nonce: array(&fixture.client_nonce_hex),
        boot_id: array(&fixture.supervisor_boot_id_hex),
        challenge: array(&fixture.server_challenge_hex),
        request_payload: decode_hex(&fixture.request_payload_hex),
        response_payload: decode_hex(&fixture.response_payload_hex),
        client_hello: decode_hex(&fixture.client_hello_hex),
        server_hello: decode_hex(&fixture.server_hello_hex),
        request_frame: decode_hex(&fixture.request_frame_hex),
        response_frame: decode_hex(&fixture.response_frame_hex),
    }
}

fn context(input: &Inputs) -> WireContext {
    WireContext::new(input.client_nonce, input.challenge, input.boot_id)
}

fn assert_invalid<T>(result: Result<T, WireError>) {
    assert!(matches!(result, Err(WireError::Invalid)));
}

fn assert_auth<T>(result: Result<T, WireError>) {
    assert!(matches!(result, Err(WireError::Auth)));
}

#[test]
fn literal_fixture_round_trips_non_utf8_bytes_exactly() {
    let input = inputs();
    let original_context = context(&input);
    assert!(encode_client_hello(&input.client_nonce).unwrap() == input.client_hello);
    assert!(decode_client_hello(&input.client_hello).unwrap() == input.client_nonce);
    assert!(encode_server_hello(&input.key, &original_context).unwrap() == input.server_hello);
    let decoded_context =
        decode_server_hello(&input.key, &input.client_nonce, &input.server_hello).unwrap();
    assert!(
        encode_request(&input.key, &decoded_context, &input.request_payload).unwrap()
            == input.request_frame
    );
    assert!(
        decode_request(&input.key, &decoded_context, &input.request_frame).unwrap()
            == input.request_payload
    );
    assert!(
        encode_response(
            &input.key,
            &decoded_context,
            &input.request_payload,
            &input.response_payload,
        )
        .unwrap()
            == input.response_frame
    );
    assert!(
        decode_response(
            &input.key,
            &decoded_context,
            &input.request_payload,
            &input.response_frame,
        )
        .unwrap()
            == input.response_payload
    );
    assert!(std::str::from_utf8(&input.request_payload).is_err());
}

#[test]
fn wire_errors_have_only_safe_fixed_debug_and_display_values() {
    assert!(format!("{}", WireError::Invalid) == "invalid");
    assert!(format!("{:?}", WireError::Invalid) == "Invalid");
    assert!(format!("{}", WireError::Auth) == "auth");
    assert!(format!("{:?}", WireError::Auth) == "Auth");
}

#[test]
fn client_hello_rejects_wrong_extent_and_magic() {
    let input = inputs();
    let mut short_nonce = input.client_nonce.to_vec();
    short_nonce.pop();
    assert_invalid(encode_client_hello(&short_nonce));
    let mut long_nonce = input.client_nonce.to_vec();
    long_nonce.push(0);
    assert_invalid(encode_client_hello(&long_nonce));

    assert_invalid(decode_client_hello(&input.client_hello[..39]));
    let mut trailing = input.client_hello.clone();
    trailing.push(0);
    assert_invalid(decode_client_hello(&trailing));
    let mut bad_magic = input.client_hello.clone();
    bad_magic[0] ^= 1;
    assert_invalid(decode_client_hello(&bad_magic));
}

#[test]
fn server_hello_validates_extent_magic_mac_and_echoed_nonce() {
    let input = inputs();
    assert_invalid(decode_server_hello(
        &input.key,
        &input.client_nonce,
        &input.server_hello[..input.server_hello.len() - 1],
    ));
    let mut trailing = input.server_hello.clone();
    trailing.push(0);
    assert_invalid(decode_server_hello(
        &input.key,
        &input.client_nonce,
        &trailing,
    ));
    let mut bad_magic = input.server_hello.clone();
    bad_magic[0] ^= 1;
    assert_invalid(decode_server_hello(
        &input.key,
        &input.client_nonce,
        &bad_magic,
    ));

    for index in [8, 24, 56, input.server_hello.len() - 1] {
        let mut forged = input.server_hello.clone();
        forged[index] ^= 1;
        assert_auth(decode_server_hello(
            &input.key,
            &input.client_nonce,
            &forged,
        ));
    }

    let mut other_nonce = input.client_nonce;
    other_nonce.reverse();
    let other_context = WireContext::new(other_nonce, input.challenge, input.boot_id);
    let valid_for_other = encode_server_hello(&input.key, &other_context).unwrap();
    assert_auth(decode_server_hello(
        &input.key,
        &input.client_nonce,
        &valid_for_other,
    ));
}

#[test]
fn server_hello_accepts_an_exact_expected_nonce_slice() {
    let input = inputs();
    let expected_nonce = input.client_nonce.to_vec();
    let expected_nonce_slice: &[u8] = &expected_nonce;
    assert!(decode_server_hello(&input.key, expected_nonce_slice, &input.server_hello).is_ok());
}

#[test]
fn server_hello_rejects_a_short_expected_nonce_slice_as_invalid() {
    let input = inputs();
    assert_invalid(decode_server_hello(
        &input.key,
        &input.client_nonce[..31],
        &input.server_hello,
    ));
}

#[test]
fn server_hello_rejects_a_long_expected_nonce_slice_as_invalid() {
    let input = inputs();
    let mut expected_nonce = input.client_nonce.to_vec();
    expected_nonce.push(0);
    assert_invalid(decode_server_hello(
        &input.key,
        &expected_nonce,
        &input.server_hello,
    ));
}

#[test]
fn server_hello_rejects_a_mac_from_another_domain() {
    let input = inputs();
    let prefix = &input.server_hello[..input.server_hello.len() - 32];
    let mut reflected = prefix.to_vec();
    let mut mac = Hmac::<Sha256>::new_from_slice(&input.key).unwrap();
    mac.update(b"neev-control-v1/request\0");
    mac.update(prefix);
    reflected.extend_from_slice(&mac.finalize().into_bytes());
    assert_auth(decode_server_hello(
        &input.key,
        &input.client_nonce,
        &reflected,
    ));
}

#[test]
fn request_payload_limits_accept_lower_and_exact_bounds() {
    let input = inputs();
    let context = context(&input);
    for size in [1, REQUEST_LIMIT] {
        let payload = vec![b'r'; size];
        let frame = encode_request(&input.key, &context, &payload).unwrap();
        assert!(decode_request(&input.key, &context, &frame).unwrap() == payload);
    }
    assert_invalid(encode_request(&input.key, &context, &[]));
    assert_invalid(encode_request(
        &input.key,
        &context,
        &vec![b'r'; REQUEST_LIMIT + 1],
    ));
}

#[test]
fn response_payload_limits_accept_lower_and_exact_bounds() {
    let input = inputs();
    let context = context(&input);
    for size in [1, RESPONSE_LIMIT] {
        let payload = vec![b's'; size];
        let frame =
            encode_response(&input.key, &context, &input.request_payload, &payload).unwrap();
        assert!(
            decode_response(&input.key, &context, &input.request_payload, &frame,).unwrap()
                == payload
        );
    }
    assert_invalid(encode_response(
        &input.key,
        &context,
        &input.request_payload,
        &[],
    ));
    assert_invalid(encode_response(
        &input.key,
        &context,
        &input.request_payload,
        &vec![b's'; RESPONSE_LIMIT + 1],
    ));
}

#[test]
fn response_request_digest_input_obeys_request_limits() {
    let input = inputs();
    let context = context(&input);
    for request_payload in [&[][..], &vec![b'r'; REQUEST_LIMIT + 1][..]] {
        assert_invalid(encode_response(
            &input.key,
            &context,
            request_payload,
            &input.response_payload,
        ));
        assert_invalid(decode_response(
            &input.key,
            &context,
            request_payload,
            &input.response_frame,
        ));
    }
}

#[test]
fn request_decoder_rejects_zero_overflow_truncation_and_trailing_bytes() {
    let input = inputs();
    let context = context(&input);
    for raw in [
        [&[0, 0, 0, 0][..], &[b'x'; 32][..]].concat(),
        [&[0xff; 4][..], &[b'x'; 32][..]].concat(),
        input.request_frame[..input.request_frame.len() - 1].to_vec(),
        [&input.request_frame[..], &[b'x'][..]].concat(),
        vec![b'x'; 65_537],
    ] {
        assert_invalid(decode_request(&input.key, &context, &raw));
    }
}

#[test]
fn response_decoder_rejects_zero_overflow_truncation_and_trailing_bytes() {
    let input = inputs();
    let context = context(&input);
    for raw in [
        [&[0, 0, 0, 0][..], &[b'x'; 32][..]].concat(),
        [&[0xff; 4][..], &[b'x'; 32][..]].concat(),
        input.response_frame[..input.response_frame.len() - 1].to_vec(),
        [&input.response_frame[..], &[b'x'][..]].concat(),
        vec![b'x'; 65_537],
    ] {
        assert_invalid(decode_response(
            &input.key,
            &context,
            &input.request_payload,
            &raw,
        ));
    }
}

#[test]
fn request_authentication_binds_domain_context_payload_and_mac() {
    let input = inputs();
    let context = context(&input);
    for index in [4, input.request_frame.len() - 1] {
        let mut changed = input.request_frame.clone();
        changed[index] ^= 1;
        assert_auth(decode_request(&input.key, &context, &changed));
    }

    let body = &input.request_frame[..input.request_frame.len() - 32];
    let mut wrong_domain = body.to_vec();
    let mut mac = Hmac::<Sha256>::new_from_slice(&input.key).unwrap();
    mac.update(b"neev-control-v1/response\0");
    mac.update(&input.client_nonce);
    mac.update(&input.challenge);
    mac.update(&input.boot_id);
    mac.update(body);
    wrong_domain.extend_from_slice(&mac.finalize().into_bytes());
    assert_auth(decode_request(&input.key, &context, &wrong_domain));

    for other_context in other_contexts(&input) {
        assert_auth(decode_request(
            &input.key,
            &other_context,
            &input.request_frame,
        ));
    }
}

#[test]
fn response_authentication_binds_context_request_digest_payload_and_mac() {
    let input = inputs();
    let context = context(&input);
    for index in [4, input.response_frame.len() - 1] {
        let mut changed = input.response_frame.clone();
        changed[index] ^= 1;
        assert_auth(decode_response(
            &input.key,
            &context,
            &input.request_payload,
            &changed,
        ));
    }
    let mut changed_request = input.request_payload.clone();
    changed_request.push(0);
    assert_auth(decode_response(
        &input.key,
        &context,
        &changed_request,
        &input.response_frame,
    ));
    for other_context in other_contexts(&input) {
        assert_auth(decode_response(
            &input.key,
            &other_context,
            &input.request_payload,
            &input.response_frame,
        ));
    }
}

fn other_contexts(input: &Inputs) -> [WireContext; 3] {
    let mut client_nonce = input.client_nonce;
    client_nonce.reverse();
    let mut challenge = input.challenge;
    challenge.reverse();
    let mut boot_id = input.boot_id;
    boot_id.reverse();
    [
        WireContext::new(client_nonce, input.challenge, input.boot_id),
        WireContext::new(input.client_nonce, challenge, input.boot_id),
        WireContext::new(input.client_nonce, input.challenge, boot_id),
    ]
}

#[test]
fn supervisor_boot_id_getter_returns_raw_context_boot() {
    let input = inputs();
    // Raw accessor returns exactly the context's 16 boot bytes.
    assert_eq!(context(&input).supervisor_boot_id(), input.boot_id);
    // The same bytes come back from a context produced by a verified server
    // hello; authenticity lives in the decode path, not this plain getter, so a
    // synthetic context returns its bytes too.
    let verified =
        decode_server_hello(&input.key, &input.client_nonce, &input.server_hello).unwrap();
    assert_eq!(verified.supervisor_boot_id(), input.boot_id);
    let synthetic = WireContext::new([7u8; 32], [9u8; 32], [3u8; 16]);
    assert_eq!(synthetic.supervisor_boot_id(), [3u8; 16]);
}
