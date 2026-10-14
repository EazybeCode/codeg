//! Unit tests for the Teams layer: JWT verification, configuration,
//! activity normalization, thread identity and context, refusal codes and
//! cards. Database-backed binding/thread behaviour is in `db_tests`.
//!
//! JWTs are signed with a fixed TEST-ONLY RSA-2048 key (`testdata/`); it is
//! not, and must never become, a key any deployment trusts.

use std::collections::HashMap;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::{json, Value};

use super::activity::{self, Activity, Inbound, Scope};
use super::bot;
use super::cards;
use super::codes::Refusal;
use super::config::{self, TeamsConfigError};
use super::jwt::{self, JwtError};
use super::keys::{KeySource, StaticKeys};
use super::threads;

const APP_ID: &str = "11111111-2222-3333-4444-555555555555";
const BOT_TENANT: &str = "99999999-8888-7777-6666-555555555555";
const TENANT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OID: &str = "0f0f0f0f-1111-2222-3333-444444444444";
const OTHER_OID: &str = "0e0e0e0e-1111-2222-3333-444444444444";
const SERVICE_URL: &str = "https://smba.trafficmanager.net/amer/";
const CHANNEL: &str = "19:abc@thread.tacv2";
const TEAM: &str = "19:team@thread.tacv2";
const NOW: i64 = 1_800_000_000;

// ---------------------------------------------------------------- helpers

fn modulus() -> String {
    include_str!("testdata/rsa2048.n.b64u").trim().to_owned()
}

fn sign_with(header: Value, claims: Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{claims}");
    let der = STANDARD
        .decode(include_str!("testdata/rsa2048.pk8.b64").trim())
        .unwrap();
    let pair = ring::signature::RsaKeyPair::from_pkcs8(&der).unwrap();
    let mut signature = vec![0u8; 256];
    pair.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn sign(claims: Value) -> String {
    sign_with(json!({"alg": "RS256", "kid": "k1", "typ": "JWT"}), claims)
}

fn jwk(endorsements: &[&str]) -> jwt::Jwk {
    let document = json!({"keys": [{
        "kty": "RSA", "kid": "k1", "use": "sig", "n": modulus(), "e": "AQAB",
        "endorsements": endorsements,
    }]});
    jwt::parse_jwks(document.to_string().as_bytes())
        .unwrap()
        .remove(0)
}

fn connector_claims() -> Value {
    json!({
        "iss": jwt::CONNECTOR_ISSUER,
        "aud": APP_ID,
        "exp": NOW + 3600,
        "nbf": NOW - 60,
        "serviceurl": SERVICE_URL,
    })
}

fn verify_connector(token: &str, key: &jwt::Jwk, now: i64) -> Result<(), JwtError> {
    jwt::verify_connector(
        jwt::decode(token)?,
        key,
        APP_ID,
        SERVICE_URL,
        "msteams",
        now,
    )
}

fn entra_claims() -> Value {
    json!({
        "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
        "aud": APP_ID,
        "exp": NOW + 3600,
        "tid": TENANT,
        "oid": OID,
        "scp": "User.Read access_as_user",
        "name": "Asha Rao",
    })
}

fn verify_entra(claims: Value) -> Result<jwt::EntraUser, JwtError> {
    let uri = format!("api://neev.example.com/{APP_ID}");
    jwt::verify_entra(
        jwt::decode(&sign(claims))?,
        &jwk(&[]),
        &[uri.as_str(), APP_ID],
        super::TAB_SCOPE,
        NOW,
    )
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn enabled() -> HashMap<String, String> {
    env(&[
        ("NEEV_TEAMS_ENABLED", "true"),
        ("NEEV_TEAMS_BOT_APP_ID", &APP_ID.to_uppercase()),
        ("NEEV_TEAMS_BOT_TENANT_ID", BOT_TENANT),
        ("NEEV_TEAMS_BOT_CLIENT_SECRET", "s3cr3t~value.from-entra"),
    ])
}

fn personal_activity(text: &str) -> Value {
    json!({
        "type": "message",
        "id": "1700000000001",
        "channelId": "msteams",
        "serviceUrl": SERVICE_URL,
        "from": {"id": "29:user", "name": "Asha Rao", "aadObjectId": OID.to_uppercase()},
        "recipient": {"id": format!("28:{APP_ID}"), "name": "Neev"},
        "conversation": {"id": "a:personal-conv", "conversationType": "personal", "tenantId": TENANT},
        "channelData": {"tenant": {"id": TENANT}},
        "text": text,
    })
}

fn channel_activity(mention_bot: bool) -> Value {
    let mentioned = if mention_bot {
        format!("28:{APP_ID}")
    } else {
        "29:someone-else".to_owned()
    };
    json!({
        "type": "message",
        "id": "1700000000002",
        "channelId": "msteams",
        "serviceUrl": SERVICE_URL,
        "from": {"id": "29:user", "name": "Asha Rao", "aadObjectId": OID},
        "recipient": {"id": format!("28:{APP_ID}"), "name": "Neev"},
        "conversation": {
            "id": format!("{CHANNEL};messageid=1700000000000"),
            "conversationType": "channel",
            "tenantId": TENANT,
        },
        "channelData": {
            "tenant": {"id": TENANT},
            "channel": {"id": CHANNEL},
            "team": {"id": TEAM},
        },
        "entities": [{"type": "mention", "mentioned": {"id": mentioned, "name": "Neev"}, "text": "<at>Neev</at>"}],
        "text": "<at>Neev</at> summarise the release notes &amp; risks",
    })
}

fn parse(value: Value) -> Activity {
    serde_json::from_value(value).unwrap()
}

fn normalized(value: Value) -> Inbound {
    activity::normalize(&parse(value)).unwrap()
}

fn envelope_of(value: Value) -> activity::Envelope {
    match normalized(value) {
        Inbound::Message(message) => message.envelope,
        Inbound::Action(action) => action.envelope,
        Inbound::Installed(envelope) => envelope,
        Inbound::Lifecycle(_) => panic!("lifecycle"),
        Inbound::Ignored => panic!("ignored"),
    }
}

// ---------------------------------------------------------------- JWT

#[test]
fn connector_token_verifies() {
    assert_eq!(
        verify_connector(&sign(connector_claims()), &jwk(&["msteams"]), NOW),
        Ok(())
    );
}

#[test]
fn connector_service_url_compare_ignores_trailing_slash_and_case() {
    let mut claims = connector_claims();
    claims["serviceurl"] = json!("HTTPS://smba.trafficmanager.net/amer");
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Ok(())
    );
}

#[test]
fn connector_refuses_other_service_url() {
    let mut claims = connector_claims();
    claims["serviceurl"] = json!("https://evil.example.com/amer/");
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Err(JwtError::ServiceUrl)
    );
    let mut claims = connector_claims();
    claims.as_object_mut().unwrap().remove("serviceurl");
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Err(JwtError::ServiceUrl)
    );
}

#[test]
fn connector_refuses_wrong_issuer_and_audience() {
    let mut claims = connector_claims();
    claims["iss"] = json!("https://sts.windows.net/x/");
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Err(JwtError::Issuer)
    );
    let mut claims = connector_claims();
    claims["aud"] = json!("22222222-2222-2222-2222-222222222222");
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Err(JwtError::Audience)
    );
    let mut claims = connector_claims();
    claims["aud"] = json!([APP_ID, "someone-else"]);
    assert_eq!(
        verify_connector(&sign(claims), &jwk(&["msteams"]), NOW),
        Err(JwtError::Audience)
    );
}

#[test]
fn connector_enforces_lifetime_with_skew() {
    let token = sign(connector_claims());
    let key = jwk(&["msteams"]);
    let exp = NOW + 3600;
    assert_eq!(verify_connector(&token, &key, exp + jwt::SKEW_SECS), Ok(()));
    assert_eq!(
        verify_connector(&token, &key, exp + jwt::SKEW_SECS + 1),
        Err(JwtError::Lifetime)
    );
    assert_eq!(
        verify_connector(&token, &key, NOW - 60 - jwt::SKEW_SECS - 1),
        Err(JwtError::Lifetime)
    );
    let mut claims = connector_claims();
    claims.as_object_mut().unwrap().remove("exp");
    assert_eq!(
        verify_connector(&sign(claims), &key, NOW),
        Err(JwtError::Lifetime)
    );
}

#[test]
fn connector_requires_channel_endorsement() {
    assert_eq!(
        verify_connector(&sign(connector_claims()), &jwk(&["webchat"]), NOW),
        Err(JwtError::Endorsement)
    );
}

#[test]
fn tampered_payload_fails_signature() {
    let token = sign(connector_claims());
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut claims = connector_claims();
    claims["aud"] = json!(APP_ID);
    claims["exp"] = json!(NOW + 999_999);
    let forged = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    parts[1] = &forged;
    assert_eq!(
        verify_connector(&parts.join("."), &jwk(&["msteams"]), NOW),
        Err(JwtError::Signature)
    );
}

#[test]
fn only_rs256_compact_tokens_decode() {
    for alg in ["none", "HS256", "RS512", "PS256"] {
        let token = sign_with(json!({"alg": alg, "kid": "k1"}), connector_claims());
        assert_eq!(
            jwt::decode(&token).err(),
            Some(JwtError::Algorithm),
            "{alg}"
        );
    }
    let crit = sign_with(
        json!({"alg": "RS256", "kid": "k1", "crit": ["b64"]}),
        connector_claims(),
    );
    assert_eq!(jwt::decode(&crit).err(), Some(JwtError::Malformed));
    let no_kid = sign_with(json!({"alg": "RS256"}), connector_claims());
    assert_eq!(jwt::decode(&no_kid).err(), Some(JwtError::Malformed));
    let token = sign(connector_claims());
    assert_eq!(
        jwt::decode(&format!("{token}.x")).err(),
        Some(JwtError::Malformed)
    );
    assert_eq!(jwt::decode("a.b").err(), Some(JwtError::Malformed));
    assert_eq!(
        jwt::decode(&"a".repeat(17 * 1024)).err(),
        Some(JwtError::Malformed)
    );
}

#[test]
fn verification_requires_matching_kid() {
    let token = sign_with(json!({"alg": "RS256", "kid": "k2"}), connector_claims());
    assert_eq!(
        verify_connector(&token, &jwk(&["msteams"]), NOW),
        Err(JwtError::UnknownKey)
    );
}

#[test]
fn jwks_keeps_only_rsa_signature_keys() {
    let document = json!({"keys": [
        {"kty": "RSA", "kid": "sig", "use": "sig", "n": modulus(), "e": "AQAB"},
        {"kty": "RSA", "kid": "enc", "use": "enc", "n": modulus(), "e": "AQAB"},
        {"kty": "EC", "kid": "ec", "crv": "P-256", "x": "AA", "y": "AA"},
        {"kty": "RSA", "kid": "", "n": modulus(), "e": "AQAB"},
        {"kty": "RSA", "kid": "bad", "n": "!!!", "e": "AQAB"},
    ]});
    let keys = jwt::parse_jwks(document.to_string().as_bytes()).unwrap();
    assert_eq!(
        keys.iter().map(|key| key.kid.as_str()).collect::<Vec<_>>(),
        ["sig"]
    );
    assert_eq!(keys[0].e, vec![1, 0, 1]);
    assert_eq!(jwt::parse_jwks(b"not json"), Err(JwtError::KeysUnavailable));
}

#[tokio::test]
async fn static_key_source_finds_by_kid() {
    let source = StaticKeys(vec![jwk(&["msteams"])]);
    assert_eq!(source.key("k1").await.unwrap().kid, "k1");
    assert_eq!(source.key("nope").await, Err(JwtError::UnknownKey));
}

#[test]
fn key_refresh_is_floored_even_when_stale() {
    use super::keys::should_fetch;
    // Fresh set, known kid: never refetch.
    assert!(!should_fetch(false, true, true));
    // Unknown kid or stale set: refetch once the floor allows ...
    assert!(should_fetch(false, false, true));
    assert!(should_fetch(true, true, true));
    // ... and never inside it, stale or not, so an issuer outage cannot make
    // every request wait on a fetch.
    assert!(!should_fetch(true, true, false));
    assert!(!should_fetch(true, false, false));
    assert!(!should_fetch(false, false, false));
}

#[test]
fn entra_token_verifies_v2_and_v1_issuers() {
    let user = verify_entra(entra_claims()).unwrap();
    assert_eq!(user.tenant_id, TENANT);
    assert_eq!(user.object_id, OID);
    assert_eq!(user.name, "Asha Rao");
    let mut claims = entra_claims();
    claims["iss"] = json!(format!("https://sts.windows.net/{TENANT}/"));
    claims["aud"] = json!(format!("api://neev.example.com/{APP_ID}"));
    assert!(verify_entra(claims).is_ok());
}

#[test]
fn entra_issuer_must_be_the_tokens_own_tenant() {
    let mut claims = entra_claims();
    claims["iss"] = json!(format!(
        "https://login.microsoftonline.com/{BOT_TENANT}/v2.0"
    ));
    assert_eq!(verify_entra(claims), Err(JwtError::Issuer));
}

#[test]
fn entra_requires_scope_audience_and_identity() {
    let mut claims = entra_claims();
    claims["scp"] = json!("User.Read access_as_user_extra");
    assert_eq!(verify_entra(claims), Err(JwtError::Scope));
    let mut claims = entra_claims();
    claims["aud"] = json!("00000003-0000-0000-c000-000000000000");
    assert_eq!(verify_entra(claims), Err(JwtError::Audience));
    let mut claims = entra_claims();
    claims["oid"] = json!("not-a-guid");
    assert_eq!(verify_entra(claims), Err(JwtError::Identity));
    let mut claims = entra_claims();
    claims.as_object_mut().unwrap().remove("tid");
    assert_eq!(verify_entra(claims), Err(JwtError::Identity));
}

#[test]
fn entra_marks_guest_and_external_accounts_by_their_identity_provider() {
    assert!(!verify_entra(entra_claims()).unwrap().external);
    let mut claims = entra_claims();
    claims["idp"] = json!(format!("https://sts.windows.net/{TENANT}/"));
    assert!(!verify_entra(claims).unwrap().external, "own tenant issuer is not external");
    let mut claims = entra_claims();
    claims["idp"] = json!("https://sts.windows.net/99999999-bbbb-cccc-dddd-eeeeeeeeeeee/");
    assert!(verify_entra(claims).unwrap().external, "B2B guest from another tenant");
    let mut claims = entra_claims();
    claims["idp"] = json!("live.com");
    assert!(verify_entra(claims).unwrap().external, "consumer account guest");
    let mut claims = entra_claims();
    claims["idp"] = json!(42);
    assert!(verify_entra(claims).unwrap().external, "malformed idp fails closed");
}

// ---------------------------------------------------------------- config

#[test]
fn teams_is_off_unless_exactly_true() {
    assert!(config::teams(&env(&[])).unwrap().is_none());
    assert!(config::teams(&env(&[("NEEV_TEAMS_ENABLED", "false")]))
        .unwrap()
        .is_none());
    for flag in ["TRUE", "1", "yes", ""] {
        assert_eq!(
            config::teams(&env(&[("NEEV_TEAMS_ENABLED", flag)])).err(),
            Some(TeamsConfigError::Flag),
            "{flag:?}"
        );
    }
}

#[test]
fn complete_config_loads_and_normalizes() {
    let teams = config::teams(&enabled()).unwrap().unwrap();
    assert_eq!(teams.bot_app_id, APP_ID);
    assert_eq!(teams.bot_tenant_id, BOT_TENANT);
    assert_eq!(teams.service_url_hosts, ["smba.trafficmanager.net"]);
    assert_eq!(teams.tab_app_id_uri, None);
    let debug = format!("{teams:?}");
    assert!(
        !debug.contains("s3cr3t"),
        "secret leaked into Debug: {debug}"
    );
}

#[test]
fn graph_authority_is_off_unless_explicitly_and_completely_configured() {
    // Bot credentials alone never imply Graph access.
    assert_eq!(config::teams(&enabled()).unwrap().unwrap().graph, None);
    let mut values = enabled();
    values.insert("NEEV_TEAMS_GRAPH_ENABLED".to_owned(), "true".to_owned());
    assert_eq!(config::teams(&values).err(), Some(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_CLIENT_ID")));
    values.insert("NEEV_TEAMS_GRAPH_CLIENT_ID".to_owned(), "not-a-guid".to_owned());
    assert_eq!(config::teams(&values).err(), Some(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_CLIENT_ID")));
    values.insert("NEEV_TEAMS_GRAPH_CLIENT_ID".to_owned(), APP_ID.to_uppercase());
    assert_eq!(config::teams(&values).err(), Some(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_CLIENT_SECRET")));
    values.insert("NEEV_TEAMS_GRAPH_CLIENT_SECRET".to_owned(), "graph~s3cr3t".to_owned());
    let teams = config::teams(&values).unwrap().unwrap();
    let graph = teams.graph.clone().unwrap();
    assert_eq!(graph.client_id, APP_ID);
    assert!(!format!("{teams:?}").contains("graph~s3cr3t"), "graph secret leaked into Debug");
    values.insert("NEEV_TEAMS_GRAPH_ENABLED".to_owned(), "yes".to_owned());
    assert_eq!(config::teams(&values).err(), Some(TeamsConfigError::Key("NEEV_TEAMS_GRAPH_ENABLED")));
}

#[test]
fn enabled_config_requires_every_key() {
    for missing in [
        "NEEV_TEAMS_BOT_APP_ID",
        "NEEV_TEAMS_BOT_TENANT_ID",
        "NEEV_TEAMS_BOT_CLIENT_SECRET",
    ] {
        let mut values = enabled();
        values.remove(missing);
        assert_eq!(
            config::teams(&values).err(),
            Some(TeamsConfigError::Key(missing))
        );
    }
}

#[test]
fn config_rejects_bad_values() {
    let cases: [(&str, &str, &'static str); 6] = [
        (
            "NEEV_TEAMS_BOT_APP_ID",
            "not-a-guid",
            "NEEV_TEAMS_BOT_APP_ID",
        ),
        (
            "NEEV_TEAMS_BOT_TENANT_ID",
            "{99999999-8888-7777-6666-555555555555}",
            "NEEV_TEAMS_BOT_TENANT_ID",
        ),
        (
            "NEEV_TEAMS_BOT_CLIENT_SECRET",
            "has space",
            "NEEV_TEAMS_BOT_CLIENT_SECRET",
        ),
        (
            "NEEV_TEAMS_SERVICE_URL_HOSTS",
            "SMBA.trafficmanager.net",
            "NEEV_TEAMS_SERVICE_URL_HOSTS",
        ),
        (
            "NEEV_TEAMS_SERVICE_URL_HOSTS",
            " , ",
            "NEEV_TEAMS_SERVICE_URL_HOSTS",
        ),
        (
            "NEEV_TEAMS_TAB_APP_ID_URI",
            "api://neev.example.com/someone-else",
            "NEEV_TEAMS_TAB_APP_ID_URI",
        ),
    ];
    for (key, value, expected) in cases {
        let mut values = enabled();
        values.insert(key.to_owned(), value.to_owned());
        assert_eq!(
            config::teams(&values).err(),
            Some(TeamsConfigError::Key(expected)),
            "{key}={value}"
        );
    }
}

#[test]
fn config_accepts_custom_hosts_and_both_tab_uri_shapes() {
    let mut values = enabled();
    values.insert(
        "NEEV_TEAMS_SERVICE_URL_HOSTS".to_owned(),
        "smba.trafficmanager.net, smba.infra.gcc.teams.microsoft.com".to_owned(),
    );
    for uri in [
        format!("api://neev.example.com/{APP_ID}"),
        format!("api://neev.example.com/botid-{APP_ID}"),
    ] {
        values.insert("NEEV_TEAMS_TAB_APP_ID_URI".to_owned(), uri.clone());
        let teams = config::teams(&values).unwrap().unwrap();
        assert_eq!(teams.tab_app_id_uri.as_deref(), Some(uri.as_str()));
        assert_eq!(teams.service_url_hosts.len(), 2);
    }
}

#[test]
fn the_retired_mapping_file_is_refused_not_ignored() {
    // Bindings live in the database now; a leftover file must not look like it
    // still grants (or narrows) anything.
    for path in ["/etc/neev/teams-mapping.json", ""] {
        let mut values = enabled();
        values.insert("NEEV_TEAMS_MAPPING_FILE".to_owned(), path.to_owned());
        assert_eq!(
            config::teams(&values).err(),
            Some(TeamsConfigError::MappingRetired)
        );
    }
}

// ---------------------------------------------------------------- activity

#[test]
fn personal_message_normalizes() {
    let Inbound::Message(message) = normalized(personal_activity("  hello &amp; welcome ")) else {
        panic!("expected a message");
    };
    assert_eq!(message.text, "hello & welcome");
    let envelope = message.envelope;
    assert_eq!(envelope.tenant_id, TENANT);
    assert_eq!(envelope.from_aad.as_deref(), Some(OID));
    assert_eq!(envelope.scope, Scope::Personal);
    assert_eq!(envelope.activity_id, "1700000000001");
    assert_eq!(envelope.bot_id, format!("28:{APP_ID}"));
}

#[test]
fn channel_traffic_is_mention_only() {
    assert_eq!(normalized(channel_activity(false)), Inbound::Ignored);
    let Inbound::Message(message) = normalized(channel_activity(true)) else {
        panic!("expected a message");
    };
    assert_eq!(message.text, "summarise the release notes & risks");
    assert_eq!(
        message.envelope.scope,
        Scope::Channel {
            team_id: Some(TEAM.to_owned()),
            channel_id: CHANNEL.to_owned(),
        }
    );
}

#[test]
fn channel_id_falls_back_to_thread_conversation() {
    let mut value = channel_activity(true);
    value["channelData"]
        .as_object_mut()
        .unwrap()
        .remove("channel");
    let envelope = envelope_of(value);
    let Scope::Channel { channel_id, .. } = envelope.scope else {
        panic!("expected a channel");
    };
    assert_eq!(channel_id, CHANNEL);
}

#[test]
fn tenant_must_be_present_and_consistent() {
    let mut value = personal_activity("hi");
    value["channelData"]["tenant"]["id"] = json!(BOT_TENANT);
    assert_eq!(
        activity::normalize(&parse(value)),
        Err(activity::ActivityError::Tenant)
    );
    let mut value = personal_activity("hi");
    value.as_object_mut().unwrap().remove("channelData");
    value["conversation"]
        .as_object_mut()
        .unwrap()
        .remove("tenantId");
    assert_eq!(
        activity::normalize(&parse(value)),
        Err(activity::ActivityError::Tenant)
    );
}

#[test]
fn required_fields_are_enforced() {
    let mut value = personal_activity("hi");
    value.as_object_mut().unwrap().remove("id");
    assert_eq!(
        activity::normalize(&parse(value)),
        Err(activity::ActivityError::Malformed)
    );
    let mut value = personal_activity("hi");
    value["serviceUrl"] = json!("");
    assert_eq!(
        activity::normalize(&parse(value)),
        Err(activity::ActivityError::Malformed)
    );
}

#[test]
fn other_channels_and_empty_text_are_ignored() {
    let mut value = personal_activity("hi");
    value["channelId"] = json!("webchat");
    assert_eq!(normalized(value), Inbound::Ignored);
    assert_eq!(
        normalized(personal_activity("<at>Neev</at>  ")),
        Inbound::Ignored
    );
    let mut typing = personal_activity("hi");
    typing["type"] = json!("typing");
    assert_eq!(normalized(typing), Inbound::Ignored);
}

#[test]
fn guest_sender_has_no_object_id() {
    let mut value = personal_activity("hi");
    value["from"]["aadObjectId"] = json!("guest@example.com");
    assert_eq!(envelope_of(value).from_aad, None);
}

#[test]
fn card_actions_normalize_from_invoke_and_submit() {
    let mut invoke = personal_activity("");
    invoke["type"] = json!("invoke");
    invoke["name"] = json!("adaptiveCard/action");
    invoke["value"] = json!({"action": {"type": "Action.Execute", "verb": cards::STATUS_VERB, "data": {"runId": "ab"}}});
    let Inbound::Action(action) = normalized(invoke) else {
        panic!("expected an action");
    };
    assert!(action.invoke);
    assert_eq!(action.verb, cards::STATUS_VERB);
    assert_eq!(action.data["runId"], "ab");

    let mut submit = personal_activity("");
    submit["value"] = json!({"neev": cards::STATUS_VERB, "runId": "cd"});
    let Inbound::Action(action) = normalized(submit) else {
        panic!("expected an action");
    };
    assert!(!action.invoke);
    assert_eq!(action.data["runId"], "cd");

    let mut bare = personal_activity("");
    bare["type"] = json!("invoke");
    bare["name"] = json!("adaptiveCard/action");
    assert_eq!(
        activity::normalize(&parse(bare)),
        Err(activity::ActivityError::Malformed)
    );
}

#[test]
fn installs_are_recognised() {
    let mut install = personal_activity("");
    install["type"] = json!("installationUpdate");
    install["action"] = json!("add");
    assert!(matches!(normalized(install), Inbound::Installed(_)));
    let mut added = channel_activity(true);
    added["type"] = json!("conversationUpdate");
    added["membersAdded"] = json!([{"id": format!("28:{APP_ID}")}]);
    assert!(matches!(normalized(added), Inbound::Installed(_)));
    let mut someone_else = channel_activity(true);
    someone_else["type"] = json!("conversationUpdate");
    someone_else["membersAdded"] = json!([{"id": "29:new-person"}]);
    assert_eq!(normalized(someone_else), Inbound::Ignored);
}

#[test]
fn clean_text_strips_mentions_and_entities() {
    assert_eq!(
        activity::clean_text("<at>Neev</at> hi <at>Bob</at>there"),
        "hi there"
    );
    assert_eq!(
        activity::clean_text("a&nbsp;&lt;b&gt; &quot;c&#39; &amp;amp;"),
        "a <b> \"c' &amp;"
    );
    assert_eq!(activity::clean_text("<at>unterminated"), "");
    assert_eq!(
        activity::clean_text(&"x".repeat(activity::MAX_TEXT_CHARS + 10)).len(),
        activity::MAX_TEXT_CHARS
    );
}

#[test]
fn idempotency_is_stable_per_activity() {
    let first = envelope_of(personal_activity("hi"));
    let retry = envelope_of(personal_activity("hi again"));
    assert_eq!(
        activity::idempotency_key(&first),
        activity::idempotency_key(&retry)
    );
    let mut other = personal_activity("hi");
    other["id"] = json!("1700000000009");
    assert_ne!(
        activity::idempotency_key(&first),
        activity::idempotency_key(&envelope_of(other))
    );
    // Length-prefixed parts: shifting a boundary changes the digest.
    assert_ne!(
        activity::digest(&["ab", "c"]),
        activity::digest(&["a", "bc"])
    );
    let id = activity::request_uuid(&activity::idempotency_key(&first));
    let parsed = uuid::Uuid::parse_str(&id).unwrap();
    assert_eq!(parsed.get_version_num(), 8);
    assert_eq!(id, parsed.to_string());
}

// ---------------------------------------------------------------- codes & cards

#[test]
fn refusal_codes_are_unique_stable_and_plain() {
    let mut seen = std::collections::BTreeSet::new();
    for reason in Refusal::ALL {
        assert!(reason.code().starts_with("teams_"), "{reason:?}");
        assert!(seen.insert(reason.code()), "duplicate {}", reason.code());
        let message = reason.message();
        assert!(!message.is_empty() && message.len() < 300, "{reason:?}");
        // Nothing internal reaches a Teams user.
        for leak in ["orgId", "org id", "database", "token", "uuid", "{", "}"] {
            assert!(
                !message.contains(leak),
                "{reason:?} leaks {leak:?}: {message}"
            );
        }
    }
    assert_eq!(Refusal::UnmappedUser.code(), "teams_unmapped_user");
    assert_eq!(
        Refusal::RuntimeUnavailable.code(),
        "teams_runtime_unavailable"
    );
}

#[test]
fn started_card_round_trips_through_both_action_paths() {
    let run_id = "0123456789abcdef0123456789abcdef";
    let card = cards::started("Teams · chat", run_id);
    let action = &card["actions"][0];
    assert_eq!(action["type"], "Action.Execute");
    assert_eq!(action["verb"], cards::STATUS_VERB);
    assert_eq!(action["fallback"]["type"], "Action.Submit");
    // What a pre-Universal-Actions client posts back is the fallback's data.
    let mut submit = personal_activity("");
    submit["value"] = action["fallback"]["data"].clone();
    let Inbound::Action(parsed) = normalized(submit) else {
        panic!("expected an action");
    };
    assert_eq!(parsed.verb, cards::STATUS_VERB);
    assert_eq!(parsed.data["runId"], run_id);
}

#[test]
fn cards_report_true_turn_state() {
    let refusal = cards::refusal(Refusal::UnmappedUser);
    assert!(refusal.to_string().contains("teams_unmapped_user"));
    let running = cards::thread_started("Asha", "r");
    assert_eq!(running["actions"][0]["verb"], cards::STATUS_VERB);
    assert_eq!(running["actions"][1]["verb"], cards::STOP_VERB);
    let done = cards::final_answer("completed", "**Answer**", None, None);
    assert_eq!(done["body"][0]["text"], "**Answer**");
    assert!(done.get("actions").is_none());
    let attributed = cards::final_answer("completed", "x", None, Some("Asha"));
    assert!(attributed.to_string().contains("For Asha"));
    let failed = cards::final_answer("failed", "", Some("runtime_disabled"), None);
    assert!(failed.to_string().contains("runtime_disabled"));
    assert!(cards::final_answer("canceled", "partial", None, None).to_string().contains("stopped"));
    let message = cards::message(done, Some("123"));
    assert_eq!(message["replyToId"], "123");
    assert_eq!(
        message["attachments"][0]["contentType"],
        cards::CARD_CONTENT_TYPE
    );
    let reply = cards::invoke_message("hi");
    assert_eq!(reply["statusCode"], 200);
    assert_eq!(reply["type"], "application/vnd.microsoft.activity.message");
    let clipped = cards::clip(&"é".repeat(cards::MAX_CARD_TEXT_CHARS + 1));
    assert!(clipped.ends_with(")"));
    assert!(clipped.starts_with(&"é".repeat(cards::MAX_CARD_TEXT_CHARS)));
}

// ---------------------------------------------------------------- threads

fn thread_activity(conversation: &str, activity_id: &str) -> Value {
    let mut value = channel_activity(true);
    value["conversation"]["id"] = json!(conversation);
    value["id"] = json!(activity_id);
    value
}

#[test]
fn a_thread_is_its_channel_and_root_never_its_sender() {
    let root = envelope_of(thread_activity(&format!("{CHANNEL};messageid=1700000000001"), "1700000000001"));
    let reply = envelope_of(thread_activity(&format!("{CHANNEL};messageid=1700000000001"), "1700000000077"));
    let mut colleague = thread_activity(&format!("{CHANNEL};messageid=1700000000001"), "1700000000078");
    colleague["from"]["aadObjectId"] = json!(OTHER_OID);
    let colleague = envelope_of(colleague);
    let key = threads::thread_key(&root).unwrap();
    assert_eq!(key.tenant_id, TENANT);
    assert_eq!(key.channel_id, CHANNEL);
    assert_eq!(key.root_id, "1700000000001");
    assert_eq!(threads::thread_key(&reply), Some(key.clone()));
    assert_eq!(threads::thread_key(&colleague), Some(key.clone()));
    // Another root in the same channel is another thread.
    let other = envelope_of(thread_activity(&format!("{CHANNEL};messageid=1700000000002"), "1700000000002"));
    assert_ne!(threads::thread_key(&other), Some(key));
}

#[test]
fn unidentifiable_threads_are_not_guessed() {
    for conversation in [
        CHANNEL.to_owned(),
        format!("{CHANNEL};messageid="),
        format!("{CHANNEL};messageid=12;34"),
        format!("19:other@thread.tacv2;messageid=1"),
    ] {
        let envelope = envelope_of(thread_activity(&conversation, "1"));
        assert_eq!(threads::thread_key(&envelope), None, "{conversation}");
    }
    // Personal chats are never threads.
    assert_eq!(threads::thread_key(&envelope_of(personal_activity("hi"))), None);
}

#[test]
fn thread_context_is_attributed_bounded_and_whole() {
    let entries = vec![
        threads::Entry { sequence: 1, author: Some("Asha".into()), body: "draft a proposal".into() },
        threads::Entry { sequence: 2, author: None, body: "Here is a draft.".into() },
        threads::Entry { sequence: 3, author: Some("Bob".into()), body: "use annual pricing".into() },
    ];
    let prompt = threads::compose(&entries, "Bob", "tighten section 2");
    assert!(prompt.contains("[Asha]: draft a proposal\n[Neev]: Here is a draft.\n[Bob]: use annual pricing"));
    assert!(prompt.ends_with("New request from Bob:\ntighten section 2"));
    // No history: just the attributed request.
    assert_eq!(
        threads::compose(&[], "", "hi"),
        "Request from A teammate in a Microsoft Teams channel thread:\nhi"
    );
    // Oldest entries drop first, never half an entry.
    let long: Vec<threads::Entry> = (1..=40)
        .map(|n| threads::Entry { sequence: n, author: Some(format!("P{n}")), body: "x".repeat(1_000) })
        .collect();
    let prompt = threads::compose(&long, "Bob", "go");
    assert!(prompt.contains("[P40]: "));
    assert!(!prompt.contains("[P1]: "));
    assert!(prompt.chars().count() < 20_000);
}

#[test]
fn recipient_must_be_this_bot() {
    let envelope = envelope_of(personal_activity("hi"));
    assert_eq!(bot::check_recipient(&envelope, APP_ID), Ok(()));
    assert_eq!(
        bot::check_recipient(&envelope, "22222222-2222-2222-2222-222222222222"),
        Err(Refusal::WrongBot)
    );
}

#[test]
fn service_urls_are_allowlisted_https_hosts() {
    let hosts = vec!["smba.trafficmanager.net".to_owned()];
    assert!(bot::service_url_allowed(SERVICE_URL, &hosts));
    for bad in [
        "http://smba.trafficmanager.net/amer/",
        "https://smba.trafficmanager.net.evil.com/amer/",
        "https://user:pw@smba.trafficmanager.net/amer/",
        "https://smba.trafficmanager.net/amer/?x=1",
        "not a url",
    ] {
        assert!(!bot::service_url_allowed(bad, &hosts), "{bad}");
    }
    assert_eq!(
        bot::reply_url(SERVICE_URL, "19:abc@thread.tacv2;messageid=1", "1/2"),
        "https://smba.trafficmanager.net/amer/v3/conversations/19%3Aabc%40thread.tacv2%3Bmessageid%3D1/activities/1%2F2"
    );
}
