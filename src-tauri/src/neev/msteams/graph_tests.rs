//! Graph authority: response parsing and the real HTTP transport against a
//! loopback server (token request, bearer use, query shape, failure mapping).

use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TENANT: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const OTHER_TENANT: &str = "99999999-bbbb-cccc-dddd-eeeeeeeeeeee";
const GROUP: &str = "12345678-1234-1234-1234-123456789abc";
const ASHA: &str = "0f0f0f0f-1111-2222-3333-444444444444";

fn member(user: &str, roles: Value, tenant: &str) -> Value {
    json!({"@odata.type": "#microsoft.graph.aadUserConversationMember", "userId": user, "roles": roles, "tenantId": tenant})
}

#[test]
fn membership_distinguishes_owner_member_guest_external_and_absent() {
    let list = |m: Value| json!({"value": [m]});
    assert_eq!(membership_from(&list(member(ASHA, json!(["owner"]), TENANT)), TENANT, ASHA), Ok(Membership::Owner));
    assert_eq!(membership_from(&list(member(ASHA, json!([]), TENANT)), TENANT, ASHA), Ok(Membership::Member));
    assert_eq!(membership_from(&list(member(ASHA, json!(["guest"]), TENANT)), TENANT, ASHA), Ok(Membership::Guest));
    // A guest who is ALSO marked owner is still a guest.
    assert_eq!(membership_from(&list(member(ASHA, json!(["owner", "guest"]), TENANT)), TENANT, ASHA), Ok(Membership::Guest));
    assert_eq!(membership_from(&list(member(ASHA, json!([]), OTHER_TENANT)), TENANT, ASHA), Ok(Membership::External));
    assert_eq!(membership_from(&json!({"value": []}), TENANT, ASHA), Ok(Membership::Absent));
    // A different user in the answer is not this user.
    assert_eq!(membership_from(&list(member("0e0e0e0e-1111-2222-3333-444444444444", json!(["owner"]), TENANT)), TENANT, ASHA), Ok(Membership::Absent));
}

#[test]
fn unknown_tenancy_is_never_an_internal_member_or_owner() {
    for roles in [json!([]), json!(["owner"])] {
        let missing = json!({"value": [{"userId": ASHA, "roles": roles.clone()}]});
        assert_eq!(membership_from(&missing, TENANT, ASHA), Ok(Membership::External), "missing tenantId {roles}");
        for tenant in [Value::Null, json!(42), json!(""), json!("not-a-guid"), json!(OTHER_TENANT)] {
            let value = json!({"value": [{"userId": ASHA, "roles": roles.clone(), "tenantId": tenant.clone()}]});
            assert_eq!(membership_from(&value, TENANT, ASHA), Ok(Membership::External), "tenant {tenant} roles {roles}");
        }
    }
    // Case differences in the same GUID are the same tenant.
    assert_eq!(membership_from(&json!({"value": [member(ASHA, json!([]), &TENANT.to_uppercase())]}), TENANT, ASHA), Ok(Membership::Member));
}

#[test]
fn ambiguous_membership_answers_fail_closed() {
    // Unknown role, missing roles, paging without the person, contradictory rows.
    assert_eq!(membership_from(&json!({"value": [member(ASHA, json!(["superuser"]), TENANT)]}), TENANT, ASHA), Err(GraphError::Unavailable));
    assert_eq!(membership_from(&json!({"value": [{"userId": ASHA}]}), TENANT, ASHA), Err(GraphError::Unavailable));
    assert_eq!(membership_from(&json!({"value": [], "@odata.nextLink": "https://graph/next"}), TENANT, ASHA), Err(GraphError::Unavailable));
    // A positive match on an incomplete (paged) answer is refused too.
    assert_eq!(
        membership_from(&json!({"value": [member(ASHA, json!(["owner"]), TENANT)], "@odata.nextLink": "https://graph/next"}), TENANT, ASHA),
        Err(GraphError::Unavailable)
    );
    // Unknown or non-string roles are refused even next to owner.
    assert_eq!(membership_from(&json!({"value": [member(ASHA, json!(["owner", "unknownFutureRole"]), TENANT)]}), TENANT, ASHA), Err(GraphError::Unavailable));
    assert_eq!(membership_from(&json!({"value": [member(ASHA, json!(["owner", 123]), TENANT)]}), TENANT, ASHA), Err(GraphError::Unavailable));
    assert_eq!(membership_from(&json!({"value": [member(ASHA, json!([null]), TENANT)]}), TENANT, ASHA), Err(GraphError::Unavailable));
    assert_eq!(
        membership_from(&json!({"value": [member(ASHA, json!([]), TENANT), member(ASHA, json!(["guest"]), TENANT)]}), TENANT, ASHA),
        Err(GraphError::Unavailable)
    );
    assert_eq!(membership_from(&json!({"nope": true}), TENANT, ASHA), Err(GraphError::Unavailable));
}

#[test]
fn only_a_live_standard_channel_without_guests_or_externals_is_pilot_eligible() {
    let channel = |kind: &str, archived: bool| json!({"id": "19:c@thread.tacv2", "membershipType": kind, "isArchived": archived});
    let summary = |guests: u64, external: bool| json!({"summary": {"guestsCount": guests, "hasMembersFromOtherTenants": external}});
    let eligible = |c: Value, s: Value| audience_from(&c, "19:c@thread.tacv2", &s).map(|a| a.pilot_eligible());
    assert_eq!(eligible(channel("standard", false), summary(0, false)), Ok(true));
    assert_eq!(eligible(channel("private", false), summary(0, false)), Ok(false));
    assert_eq!(eligible(channel("shared", false), summary(0, false)), Ok(false));
    assert_eq!(eligible(channel("unknownFutureValue", false), summary(0, false)), Ok(false));
    assert_eq!(eligible(channel("standard", true), summary(0, false)), Ok(false));
    assert_eq!(eligible(channel("standard", false), summary(1, false)), Ok(false));
    assert_eq!(eligible(channel("standard", false), summary(0, true)), Ok(false));
    // No summary = unknown audience = not eligible.
    assert_eq!(eligible(channel("standard", false), json!({})), Ok(false));
    // Missing archive flag is treated as archived.
    assert_eq!(eligible(json!({"id": "19:c@thread.tacv2", "membershipType": "standard"}), summary(0, false)), Ok(false));
    // A summary answer naming another channel is refused.
    assert_eq!(
        audience_from(&channel("standard", false), "19:c@thread.tacv2", &json!({"id": "19:other@thread.tacv2", "summary": {"guestsCount": 0, "hasMembersFromOtherTenants": false}})),
        Err(GraphError::Unavailable)
    );
    // A different channel id in the answer is refused outright.
    assert_eq!(audience_from(&channel("standard", false), "19:other@thread.tacv2", &summary(0, false)), Err(GraphError::Unavailable));
}

/// A loopback HTTP server answering each request with the next canned
/// response, recording the request heads it saw.
async fn server(responses: Vec<(u16, String)>) -> (String, Arc<StdMutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        for (status, body) in responses {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut data = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                data.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&data);
                if read == 0 {
                    break;
                }
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let length = text[..head_end]
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if data.len() >= head_end + 4 + length {
                        break;
                    }
                }
            }
            log.lock().unwrap().push(String::from_utf8_lossy(&data).into_owned());
            let reply = format!("HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (format!("http://{address}"), seen)
}

fn authority(base: &str) -> GraphAuthority {
    let mut graph = GraphAuthority::new(&GraphConfig {
        client_id: "11111111-2222-3333-4444-555555555555".to_owned(),
        client_secret: "graph-secret".to_owned(),
        files: false,
    })
    .unwrap();
    graph.login_base = base.to_owned();
    graph.graph_base = base.to_owned();
    graph
}

fn token_ok() -> (u16, String) {
    (200, json!({"access_token": "graph-token", "expires_in": 3600}).to_string())
}

#[tokio::test]
async fn membership_uses_its_own_graph_token_in_the_installing_tenant() {
    let (base, seen) = server(vec![
        token_ok(),
        (200, json!({"value": [member(ASHA, json!(["owner"]), TENANT)]}).to_string()),
        (200, json!({"value": []}).to_string()),
    ])
    .await;
    let graph = authority(&base);
    assert_eq!(graph.membership(TENANT, GROUP, ASHA).await, Ok(Membership::Owner));
    // The token is cached: the second lookup makes no token request.
    assert_eq!(graph.membership(TENANT, GROUP, ASHA).await, Ok(Membership::Absent));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].starts_with(&format!("POST /{TENANT}/oauth2/v2.0/token ")), "{}", seen[0]);
    assert!(seen[0].contains("scope=https%3A%2F%2Fgraph.microsoft.com%2F.default"));
    assert!(seen[0].contains("grant_type=client_credentials"));
    assert!(seen[1].starts_with(&format!("GET /teams/{GROUP}/members?%24filter=")), "{}", seen[1]);
    assert!(seen[1].contains(ASHA));
    assert!(seen[1].to_ascii_lowercase().contains("authorization: bearer graph-token"));
}

#[tokio::test]
async fn channel_audience_reads_type_then_summary_with_encoded_channel_id() {
    let (base, seen) = server(vec![
        token_ok(),
        (200, json!({"id": "19:c@thread.tacv2", "membershipType": "standard", "isArchived": false}).to_string()),
        (200, json!({"summary": {"guestsCount": 0, "hasMembersFromOtherTenants": false}}).to_string()),
    ])
    .await;
    let audience = authority(&base).channel_audience(TENANT, GROUP, "19:c@thread.tacv2").await.unwrap();
    assert!(audience.pilot_eligible());
    let seen = seen.lock().unwrap().clone();
    assert!(seen[1].starts_with(&format!("GET /teams/{GROUP}/channels/19%3Ac%40thread.tacv2?%24select=id%2CmembershipType%2CisArchived ")), "{}", seen[1]);
    assert!(seen[2].contains("%24select=id%2Csummary"));
}

#[tokio::test]
async fn graph_refusals_and_outages_fail_closed_without_leaking_bodies() {
    for (status, expected) in [(403, GraphError::Denied), (404, GraphError::NotFound), (429, GraphError::Unavailable), (503, GraphError::Unavailable)] {
        let (base, _) = server(vec![token_ok(), (status, "{\"error\":{\"message\":\"secret detail\"}}".to_owned())]).await;
        let result = authority(&base).membership(TENANT, GROUP, ASHA).await;
        assert_eq!(result, Err(expected), "status {status}");
        assert!(!format!("{result:?}").contains("secret"));
    }
    // A refused token request is a denial; nothing is sent to Graph.
    let (base, seen) = server(vec![(400, "{\"error\":\"invalid_client\"}".to_owned())]).await;
    assert_eq!(authority(&base).membership(TENANT, GROUP, ASHA).await, Err(GraphError::Denied));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_ids_never_reach_the_network() {
    let graph = authority("http://127.0.0.1:9");
    assert_eq!(graph.membership(TENANT, "not-a-group", ASHA).await, Err(GraphError::NotFound));
    assert_eq!(graph.membership(TENANT, GROUP, "x' or 1 eq 1 or 'a").await, Err(GraphError::NotFound));
    assert_eq!(graph.membership("not-a-tenant", GROUP, ASHA).await, Err(GraphError::Denied));
}
