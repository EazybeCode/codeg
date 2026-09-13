use super::control_dto as dto;
use serde::Deserialize;

const I64_MAX: i64 = 9_223_372_036_854_775_807;

#[derive(Deserialize)]
struct RequestFixture {
    name: String,
    payload_hex: String,
    expected_op: String,
}

#[derive(Deserialize)]
struct ResponseFixture {
    name: String,
    request_name: String,
    payload_hex: String,
    expected_outcome: String,
    expected_tag: String,
}

#[derive(Deserialize)]
struct InvalidFixture {
    direction: String,
    payload_hex: String,
    expected_category: String,
}

#[derive(Deserialize)]
struct Fixture {
    format: String,
    synthetic: bool,
    requests: Vec<RequestFixture>,
    responses: Vec<ResponseFixture>,
    invalid: Vec<InvalidFixture>,
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

fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "../../../../scripts/tests/fixtures/neev-control-dto-v1.json"
    ))
    .unwrap()
}

fn request_bytes(name: &str) -> Vec<u8> {
    let fixture = fixture();
    decode_hex(
        &fixture
            .requests
            .iter()
            .find(|item| item.name == name)
            .unwrap()
            .payload_hex,
    )
}

fn response_bytes(name: &str) -> Vec<u8> {
    let fixture = fixture();
    decode_hex(
        &fixture
            .responses
            .iter()
            .find(|item| item.name == name)
            .unwrap()
            .payload_hex,
    )
}

fn first_request_bytes() -> Vec<u8> {
    request_bytes("reconcile_first")
}

fn first_response_bytes() -> Vec<u8> {
    response_bytes("reconcile_first_page")
}

fn reconcile_entry(
    entry_number: u128,
    phase: dto::RegistryPhase,
    handle: bool,
    receipt: bool,
    cleanup: bool,
) -> dto::ReconcileEntry {
    let request = dto::decode_request(&request_bytes("check")).unwrap();
    let binding = match request.body {
        dto::RequestBody::CheckConnection(body) => body.ticket.binding,
        _ => panic!("check fixture decoded to another closed request variant"),
    };
    dto::ReconcileEntry {
        entry_id: dto::Id::new(format!("{entry_number:032x}")).unwrap(),
        binding,
        attempt_id: dto::Id::new("5".repeat(32)).unwrap(),
        operation_id: dto::Id::new("6".repeat(32)).unwrap(),
        phase,
        storage_handle: handle.then(|| dto::Id::new("9".repeat(32)).unwrap()),
        receipt_id: receipt.then(|| dto::Id::new("8".repeat(32)).unwrap()),
        cleanup_operation_id: cleanup.then(|| dto::Id::new("7".repeat(32)).unwrap()),
    }
}

fn reconcile_response(
    revision: &str,
    entries: Vec<dto::ReconcileEntry>,
    next: Option<dto::Id>,
) -> dto::Response {
    let mut response = dto::decode_response(&first_response_bytes()).unwrap();
    response.body = Some(dto::ResultBody::ReconcilePage(dto::ReconcilePage {
        snapshot_revision: dto::Counter::new(revision.into()).unwrap(),
        entries,
        next_after_entry_id: next,
    }));
    response
}

fn assert_error<T>(result: Result<T, dto::DtoError>, expected: dto::DtoError) {
    assert!(matches!(result, Err(error) if error == expected));
}

#[test]
fn first_page_roundtrip() {
    let request = dto::decode_request(&first_request_bytes()).unwrap();
    let encoded = dto::encode_request(&request).unwrap();
    assert!(dto::decode_request(&encoded).unwrap() == request);
    let response = dto::decode_response(&first_response_bytes()).unwrap();
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    assert!(dto::validate_response(&request, &response, &boot).is_ok());
}

#[test]
fn literal_fixture_covers_every_request_response_and_error() {
    let fixture = fixture();
    assert!(fixture.format == "neev-control-dto-v1");
    assert!(fixture.synthetic);
    assert!(fixture.requests.len() == 6);
    assert!(fixture.responses.len() == 40);
    let requests: Vec<(String, dto::Request)> = fixture
        .requests
        .iter()
        .map(|item| {
            let decoded = dto::decode_request(&decode_hex(&item.payload_hex)).unwrap();
            assert!(decoded.op().as_str() == item.expected_op);
            let encoded = dto::encode_request(&decoded).unwrap();
            assert!(dto::decode_request(&encoded).unwrap() == decoded);
            (item.name.clone(), decoded)
        })
        .collect();
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    for item in &fixture.responses {
        let decoded = dto::decode_response(&decode_hex(&item.payload_hex)).unwrap();
        assert!(decoded.outcome().as_str() == item.expected_outcome);
        assert!(decoded.tag() == item.expected_tag);
        let encoded = dto::encode_response(&decoded).unwrap();
        assert!(dto::decode_response(&encoded).unwrap() == decoded);
        let request = &requests
            .iter()
            .find(|(name, _)| name == &item.request_name)
            .unwrap()
            .1;
        assert!(dto::validate_response(request, &decoded, &boot).is_ok());
    }
}

#[test]
fn settlement_witness_fixed_kind_survives_decode_encode() {
    let raw = request_bytes("settle");
    let decoded = dto::decode_request(&raw).unwrap();
    match &decoded.body {
        dto::RequestBody::SettleLogin(body) => {
            assert!(matches!(
                body.witness.kind,
                dto::CompletedConnectKind::CompletedConnect
            ));
        }
        _ => panic!("settlement fixture decoded to another closed request variant"),
    }
    let encoded = dto::encode_request(&decoded).unwrap();
    assert!(encoded == raw);
    assert!(encoded
        .windows(b"\"kind\":\"completed_connect\"".len())
        .any(|part| part == b"\"kind\":\"completed_connect\""));
}

#[test]
fn scalar_boundaries_are_exact() {
    assert!(dto::Id::new("a".repeat(32)).is_ok());
    assert_error(dto::Id::new("A".repeat(32)), dto::DtoError::Invalid);
    assert_error(dto::Id::new("a".repeat(31)), dto::DtoError::Invalid);
    assert!(dto::SessionId::new("123e4567-e89b-42d3-a456-426614174000".into()).is_ok());
    assert_error(
        dto::SessionId::new("123e4567-e89b-52d3-a456-426614174000".into()),
        dto::DtoError::Invalid,
    );
    assert!(dto::Counter::new("1".into()).unwrap().get() == 1);
    assert!(dto::Counter::new(I64_MAX.to_string()).unwrap().get() == I64_MAX);
    for value in ["0", "01", "9223372036854775808"] {
        assert_error(dto::Counter::new(value.into()), dto::DtoError::Invalid);
    }
    assert!(dto::OwnerId::new(1).is_ok());
    assert!(dto::OwnerId::new(2_147_483_647).is_ok());
    assert_error(dto::OwnerId::new(0), dto::DtoError::Invalid);
    assert!(dto::UnixSec::new(0).is_ok());
    assert!(dto::UnixSec::new(253_402_300_799).is_ok());
    assert_error(dto::UnixSec::new(253_402_300_800), dto::DtoError::Invalid);
    assert!(dto::RetryMs::new(250).is_ok());
    assert!(dto::RetryMs::new(30_000).is_ok());
    assert_error(dto::RetryMs::new(249), dto::DtoError::Invalid);
    assert!(dto::Label::new("é".repeat(160)).is_ok());
    assert_error(dto::Label::new("x".repeat(161)), dto::DtoError::Invalid);
    assert_error(dto::Label::new("a\u{202e}b".into()), dto::DtoError::Invalid);
    assert!(dto::UserCode::new("A-9".into()).is_ok());
    assert_error(dto::UserCode::new("lower".into()), dto::DtoError::Invalid);
}

#[test]
fn strict_json_rejects_duplicates_numbers_depth_and_bad_extent() {
    let request = first_request_bytes();
    let mutations = [
        request
            .windows(b"\"limit\":1".len())
            .position(|part| part == b"\"limit\":1")
            .map(|index| {
                let mut value = request.clone();
                value.splice(
                    index..index + b"\"limit\":1".len(),
                    b"\"limit\":1,\"\\u006cimit\":1".iter().copied(),
                );
                value
            })
            .unwrap(),
        [b"\xef\xbb\xbf".as_slice(), request.as_slice()].concat(),
        [request.as_slice(), b" x"].concat(),
        String::from_utf8(request.clone())
            .unwrap()
            .replace("\"limit\":1", "\"limit\":1.0")
            .into_bytes(),
        String::from_utf8(request.clone())
            .unwrap()
            .replace("\"limit\":1", "\"limit\":1e0")
            .into_bytes(),
        String::from_utf8(request.clone())
            .unwrap()
            .replace("\"limit\":1", "\"limit\":-0")
            .into_bytes(),
        vec![0xff],
    ];
    for value in mutations {
        assert_error(dto::decode_request(&value), dto::DtoError::Invalid);
    }
    let mut padded = request.clone();
    padded.resize(8192, b' ');
    assert!(dto::decode_request(&padded).is_ok());
    padded.push(b' ');
    assert_error(dto::decode_request(&padded), dto::DtoError::Invalid);
    let depth_17 = format!("{{\"v\":1,\"a\":{}0{}}}", "[".repeat(16), "]".repeat(16));
    assert_error(
        dto::decode_request(depth_17.as_bytes()),
        dto::DtoError::Invalid,
    );
}

#[test]
fn bounded_json_accepts_and_rejects_exact_member_key_string_array_and_depth_limits() {
    let members_32 = format!(
        "{{{}}}",
        (0..32)
            .map(|index| format!("\"k{index:02}\":null"))
            .collect::<Vec<_>>()
            .join(",")
    )
    .into_bytes();
    let mut members_33 = members_32[..members_32.len() - 1].to_vec();
    members_33.extend_from_slice(b",\"overflow\":null}");
    let key_64 = format!("{{\"{}\":null}}", "k".repeat(64)).into_bytes();
    let key_65 = format!("{{\"{}\":null}}", "k".repeat(65)).into_bytes();
    let string_1024 = format!("{{\"x\":\"{}\"}}", "s".repeat(1024)).into_bytes();
    let string_1025 = format!("{{\"x\":\"{}\"}}", "s".repeat(1025)).into_bytes();
    let array_32 = format!("{{\"x\":[{}]}}", vec!["0"; 32].join(",")).into_bytes();
    let array_33 = format!("{{\"x\":[{}]}}", vec!["0"; 33].join(",")).into_bytes();
    let depth_16 = format!("{{\"x\":{}0{}}}", "[".repeat(15), "]".repeat(15)).into_bytes();
    let depth_17 = format!("{{\"x\":{}0{}}}", "[".repeat(16), "]".repeat(16)).into_bytes();

    for raw in [members_32, key_64, string_1024, array_32, depth_16] {
        assert!(dto::validate_bounded_json_for_test(&raw, 8192).is_ok());
    }
    for raw in [members_33, key_65, string_1025, array_33, depth_17] {
        assert_error(
            dto::validate_bounded_json_for_test(&raw, 8192),
            dto::DtoError::Invalid,
        );
    }
}

#[test]
fn every_invalid_literal_is_rejected_with_fixed_error() {
    for item in fixture().invalid {
        assert!(item.expected_category == "invalid");
        let raw = decode_hex(&item.payload_hex);
        let result = if item.direction == "request" {
            dto::decode_request(&raw).map(|_| ())
        } else {
            dto::decode_response(&raw).map(|_| ())
        };
        assert_error(result, dto::DtoError::Invalid);
    }
    assert!(format!("{}", dto::DtoError::Invalid) == "invalid");
    assert!(format!("{:?}", dto::DtoError::Correlation) == "Correlation");
}

#[test]
fn semantic_keys_and_channels_enforce_replay_boundaries() {
    let login = dto::decode_request(&request_bytes("start")).unwrap();
    let cleanup = dto::decode_request(&request_bytes("cleanup")).unwrap();
    let login_ticket = login.login_ticket().unwrap();
    assert!(dto::login_semantic_key(login_ticket) == dto::login_semantic_key(login_ticket));
    let mut changed_login = login_ticket.clone();
    changed_login.expires_at = dto::UnixSec::new(2_000_000_001).unwrap();
    assert!(dto::login_semantic_key(login_ticket) != dto::login_semantic_key(&changed_login));
    let cleanup_ticket = cleanup.cleanup_ticket().unwrap();
    let mut renewed = cleanup_ticket.clone();
    renewed.lease_owner = dto::Id::new("e".repeat(32)).unwrap();
    renewed.lease_expires_at = dto::UnixSec::new(2_000_000_001).unwrap();
    assert!(dto::cleanup_semantic_key(cleanup_ticket) == dto::cleanup_semantic_key(&renewed));
    let mut changed_target = cleanup_ticket.clone();
    changed_target.target = dto::CleanupTarget::Disconnect;
    assert!(
        dto::cleanup_semantic_key(cleanup_ticket) != dto::cleanup_semantic_key(&changed_target)
    );

    let coordinator = dto::Coordinator::new(
        dto::Counter::new("1".into()).unwrap(),
        dto::Id::new("1".repeat(32)).unwrap(),
    );
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    let request = dto::decode_request(&first_request_bytes()).unwrap();
    let response = dto::decode_response(&first_response_bytes()).unwrap();
    let mut client = dto::ClientChannel::new(coordinator.clone(), boot.clone());
    assert!(client.begin(&request).is_ok());
    assert!(client.finish(&response).is_ok());
    assert_error(client.finish(&response), dto::DtoError::State);
    assert_error(client.begin(&request), dto::DtoError::State);

    let mut server = dto::ServerChannel::new(coordinator, boot);
    let mut gap = request.clone();
    gap.sequence = dto::Counter::new("2".into()).unwrap();
    assert_error(server.accept(&gap), dto::DtoError::Sequence);
    assert_error(server.accept(&request), dto::DtoError::State);
    server.poison();
    server.poison();

    let mut server = dto::ServerChannel::new(
        request.coordinator.clone(),
        dto::Id::new("3".repeat(32)).unwrap(),
    );
    assert!(server.accept(&request).is_ok());
    assert!(server.finish(&response).is_ok());

    let mut poisoned = dto::ClientChannel::new(
        request.coordinator.clone(),
        dto::Id::new("3".repeat(32)).unwrap(),
    );
    poisoned.poison();
    assert_error(poisoned.begin(&request), dto::DtoError::State);
}

#[test]
fn application_error_consumes_sequence_and_maximum_closes() {
    let coordinator = dto::Coordinator::new(
        dto::Counter::new("1".into()).unwrap(),
        dto::Id::new("1".repeat(32)).unwrap(),
    );
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    let mut request = dto::decode_request(&first_request_bytes()).unwrap();
    let mut error = dto::decode_response(&response_bytes("error_busy")).unwrap();
    let mut channel = dto::ClientChannel::new(coordinator.clone(), boot.clone());
    assert!(channel.begin(&request).is_ok());
    assert!(channel.finish(&error).is_ok());
    request.sequence = dto::Counter::new("2".into()).unwrap();
    request.request_id = dto::Id::new("d".repeat(32)).unwrap();
    error.sequence = dto::Counter::new("2".into()).unwrap();
    error.request_id = dto::Id::new("d".repeat(32)).unwrap();
    assert!(channel.begin(&request).is_ok());
    assert!(channel.finish(&error).is_ok());

    let mut final_request = dto::decode_request(&first_request_bytes()).unwrap();
    final_request.sequence = dto::Counter::new(I64_MAX.to_string()).unwrap();
    let mut final_response = dto::decode_response(&first_response_bytes()).unwrap();
    final_response.sequence = dto::Counter::new(I64_MAX.to_string()).unwrap();
    let mut final_channel =
        dto::ClientChannel::new_at_sequence_for_test(coordinator, boot, I64_MAX);
    assert!(final_channel.begin(&final_request).is_ok());
    assert!(final_channel.finish(&final_response).is_ok());
    assert_error(final_channel.begin(&final_request), dto::DtoError::State);
}

#[test]
fn supplied_time_login_snapshot_rejects_late_challenge() {
    let observe = dto::decode_request(&request_bytes("observe")).unwrap();
    let response = dto::decode_response(&response_bytes("observe_awaiting_user")).unwrap();
    let binding = observe.login_binding().unwrap();
    let snapshot = response.login_snapshot().unwrap();
    let expected = dto::UnixSec::new(2_000_000_000).unwrap();
    assert!(dto::validate_login_snapshot(
        binding,
        snapshot,
        &expected,
        &dto::UnixSec::new(1_999_999_998).unwrap(),
    )
    .is_ok());
    assert_error(
        dto::validate_login_snapshot(
            binding,
            snapshot,
            &expected,
            &dto::UnixSec::new(1_999_999_999).unwrap(),
        ),
        dto::DtoError::Expired,
    );
}

#[test]
fn response_bounds_correlation_and_reconcile_cursor_are_strict() {
    let raw = first_response_bytes();
    let mut padded = raw.clone();
    padded.resize(16_384, b' ');
    assert!(dto::decode_response(&padded).is_ok());
    padded.push(b' ');
    assert_error(dto::decode_response(&padded), dto::DtoError::Invalid);

    let request = dto::decode_request(&first_request_bytes()).unwrap();
    let response = dto::decode_response(&first_response_bytes()).unwrap();
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    let mut changed = response.clone();
    changed.request_id = dto::Id::new("f".repeat(32)).unwrap();
    assert_error(
        dto::validate_response(&request, &changed, &boot),
        dto::DtoError::Correlation,
    );
    let mut changed = response.clone();
    changed.sequence = dto::Counter::new("2".into()).unwrap();
    assert_error(
        dto::validate_response(&request, &changed, &boot),
        dto::DtoError::Correlation,
    );
    assert_error(
        dto::validate_response(&request, &response, &dto::Id::new("f".repeat(32)).unwrap()),
        dto::DtoError::Correlation,
    );

    let entry = concat!(
        "{\"entry_id\":\"cccccccccccccccccccccccccccccccc\",",
        "\"binding\":{\"scope\":{\"org_id\":1,\"user_id\":2,\"provider\":\"codex\"},",
        "\"connection_id\":\"44444444444444444444444444444444\",\"generation\":\"1\"},",
        "\"attempt_id\":\"55555555555555555555555555555555\",",
        "\"operation_id\":\"66666666666666666666666666666666\",",
        "\"phase\":\"prepared\",\"storage_handle\":\"99999999999999999999999999999999\",",
        "\"receipt_id\":\"88888888888888888888888888888888\",\"cleanup_operation_id\":null}"
    );
    let with_entry = String::from_utf8(raw.clone()).unwrap().replace(
        "\"entries\":[],\"next_after_entry_id\":null",
        &format!(
            "\"entries\":[{entry}],\"next_after_entry_id\":\"cccccccccccccccccccccccccccccccc\""
        ),
    );
    let page = dto::decode_response(with_entry.as_bytes()).unwrap();
    assert!(dto::validate_response(&request, &page, &boot).is_ok());
    let bad_cursor = with_entry.replace(
        "\"next_after_entry_id\":\"cccccccccccccccccccccccccccccccc\"",
        "\"next_after_entry_id\":\"dddddddddddddddddddddddddddddddd\"",
    );
    let bad_page = dto::decode_response(bad_cursor.as_bytes()).unwrap();
    assert_error(
        dto::validate_response(&request, &bad_page, &boot),
        dto::DtoError::Correlation,
    );
}

#[test]
fn reconcile_phase_nullability_matrix_is_complete() {
    let phases = [
        dto::RegistryPhase::AllocationPending,
        dto::RegistryPhase::PendingLogin,
        dto::RegistryPhase::Prepared,
        dto::RegistryPhase::Committed,
        dto::RegistryPhase::CleanupPending,
        dto::RegistryPhase::Quarantined,
        dto::RegistryPhase::Disposed,
    ];
    for phase in phases {
        for handle in [false, true] {
            for receipt in [false, true] {
                for cleanup in [false, true] {
                    let valid = match phase {
                        dto::RegistryPhase::AllocationPending => !handle && !receipt && !cleanup,
                        dto::RegistryPhase::PendingLogin => handle && !receipt && !cleanup,
                        dto::RegistryPhase::Prepared | dto::RegistryPhase::Committed => {
                            handle && receipt && !cleanup
                        }
                        dto::RegistryPhase::CleanupPending => cleanup && (!receipt || handle),
                        dto::RegistryPhase::Quarantined | dto::RegistryPhase::Disposed => {
                            !receipt || handle
                        }
                    };
                    let response = reconcile_response(
                        "1",
                        vec![reconcile_entry(1, phase, handle, receipt, cleanup)],
                        None,
                    );
                    assert!(dto::encode_response(&response).is_ok() == valid);
                }
            }
        }
    }
}

#[test]
fn reconcile_entry_count_order_revision_and_encoded_size_are_bounded() {
    let boot = dto::Id::new("3".repeat(32)).unwrap();
    let mut request = dto::decode_request(&first_request_bytes()).unwrap();
    if let dto::RequestBody::ReconcilePage(body) = &mut request.body {
        body.limit = 32;
    } else {
        panic!("reconcile fixture decoded to another closed request variant");
    }
    let entries_32 = (1..=32)
        .map(|index| {
            reconcile_entry(
                index,
                dto::RegistryPhase::AllocationPending,
                false,
                false,
                false,
            )
        })
        .collect::<Vec<_>>();
    let response_32 = reconcile_response("1", entries_32.clone(), None);
    let encoded_32 = dto::encode_response(&response_32).unwrap();
    assert!(match dto::decode_response(&encoded_32).unwrap().body {
        Some(dto::ResultBody::ReconcilePage(page)) => page.entries.len() == 32,
        _ => false,
    });
    assert!(dto::validate_response(&request, &response_32, &boot).is_ok());

    let mut entries_33 = entries_32;
    entries_33.push(reconcile_entry(
        33,
        dto::RegistryPhase::AllocationPending,
        false,
        false,
        false,
    ));
    assert_error(
        dto::encode_response(&reconcile_response("1", entries_33, None)),
        dto::DtoError::Invalid,
    );

    let mut continuation = request.clone();
    if let dto::RequestBody::ReconcilePage(body) = &mut continuation.body {
        body.snapshot_revision = Some(dto::Counter::new("1".into()).unwrap());
        body.after_entry_id = Some(dto::Id::new("0".repeat(32)).unwrap());
    }
    let one_entry = reconcile_entry(
        1,
        dto::RegistryPhase::AllocationPending,
        false,
        false,
        false,
    );
    let short_page = reconcile_response(
        "1",
        vec![one_entry.clone()],
        Some(one_entry.entry_id.clone()),
    );
    assert!(dto::validate_response(&continuation, &short_page, &boot).is_ok());
    assert_error(
        dto::validate_response(
            &continuation,
            &reconcile_response(
                "2",
                vec![one_entry.clone()],
                Some(one_entry.entry_id.clone()),
            ),
            &boot,
        ),
        dto::DtoError::Correlation,
    );

    assert_error(
        dto::validate_response(
            &request,
            &reconcile_response("1", vec![one_entry.clone(), one_entry.clone()], None),
            &boot,
        ),
        dto::DtoError::Correlation,
    );
    let higher = reconcile_entry(
        2,
        dto::RegistryPhase::AllocationPending,
        false,
        false,
        false,
    );
    assert_error(
        dto::validate_response(
            &request,
            &reconcile_response("1", vec![higher, one_entry], None),
            &boot,
        ),
        dto::DtoError::Correlation,
    );

    let oversized_entries = (1..=32)
        .map(|index| {
            let mut entry =
                reconcile_entry(index, dto::RegistryPhase::Quarantined, true, true, true);
            entry.entry_id = dto::invalid_id_for_test("f".repeat(1024));
            entry
        })
        .collect();
    assert_error(
        dto::encode_response(&reconcile_response("1", oversized_entries, None)),
        dto::DtoError::Invalid,
    );
}

#[test]
fn authenticated_wire_bytes_still_require_dto_validation() {
    use super::control_wire as wire;

    let key: [u8; 32] = std::array::from_fn(|index| index as u8);
    let context = wire::WireContext::new(
        std::array::from_fn(|index| index as u8),
        std::array::from_fn(|index| (31 - index) as u8),
        *b"3333333333333333",
    );
    let payload = first_request_bytes();
    let frame = wire::encode_request(&key, &context, &payload).unwrap();
    let authenticated = wire::decode_request(&key, &context, &frame).unwrap();
    assert!(dto::decode_request(&authenticated).is_ok());

    let invalid = b"{\"v\":1,\"v\":1}";
    let frame = wire::encode_request(&key, &context, invalid).unwrap();
    let authenticated = wire::decode_request(&key, &context, &frame).unwrap();
    assert_error(dto::decode_request(&authenticated), dto::DtoError::Invalid);
}

#[test]
fn typed_values_are_revalidated_at_channel_boundaries() {
    let mut cleanup = dto::decode_request(&request_bytes("cleanup")).unwrap();
    if let dto::RequestBody::CleanupConnection(body) = &mut cleanup.body {
        body.ticket.result_generation = dto::Counter::new("3".into()).unwrap();
    } else {
        panic!("cleanup fixture decoded to another closed request variant");
    }
    assert_error(dto::encode_request(&cleanup), dto::DtoError::Invalid);
    let mut server = dto::ServerChannel::new(
        cleanup.coordinator.clone(),
        dto::Id::new("3".repeat(32)).unwrap(),
    );
    assert_error(server.accept(&cleanup), dto::DtoError::Invalid);
    assert_error(server.accept(&cleanup), dto::DtoError::State);

    let request = dto::decode_request(&first_request_bytes()).unwrap();
    let mut response = dto::decode_response(&response_bytes("error_busy")).unwrap();
    response.error.as_mut().unwrap().retry_after_ms = None;
    assert_error(
        dto::validate_response(&request, &response, &dto::Id::new("3".repeat(32)).unwrap()),
        dto::DtoError::Invalid,
    );

    let observe = dto::decode_request(&request_bytes("observe")).unwrap();
    let mut invalid_snapshot =
        dto::decode_response(&response_bytes("observe_awaiting_user")).unwrap();
    match invalid_snapshot.body.as_mut() {
        Some(dto::ResultBody::ObserveLogin(dto::LoginSnapshot::AwaitingUser {
            challenge, ..
        })) => challenge.verification_url = "SYNTHETIC-INVALID-URL".into(),
        _ => panic!("awaiting-user fixture decoded to another closed result variant"),
    }
    assert_error(
        dto::encode_response(&invalid_snapshot),
        dto::DtoError::Invalid,
    );
    assert_error(
        dto::validate_response(
            &observe,
            &invalid_snapshot,
            &dto::Id::new("3".repeat(32)).unwrap(),
        ),
        dto::DtoError::Invalid,
    );
    let mut client = dto::ClientChannel::new(
        observe.coordinator.clone(),
        dto::Id::new("3".repeat(32)).unwrap(),
    );
    assert!(client.begin(&observe).is_ok());
    assert_error(client.finish(&invalid_snapshot), dto::DtoError::Invalid);
    assert_error(client.begin(&observe), dto::DtoError::State);
}
