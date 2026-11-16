// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS `GetIceServerConfig` hands a viewer for the TURN relay.
//!
//! The rule these cases assert, one level above the recorded case (one channel, one call,
//! one address):
//!
//! > Every address this plane announces for a client to DIAL names a host and port THIS
//! > emulator is reached at — the endpoint the request addressed when the caller resolves
//! > nothing else, otherwise the address this process is served at — and never a host AWS
//! > publishes an endpoint at, in the `turn:`/`turns:` URI form `IceServer$Uris` documents.
//!
//! The rows span what the rule generalises over: how a request carries its address (an
//! IPv4 literal with a port, a loopback name without one, a bracketed IPv6 literal), a
//! caller that addressed one of AWS's own hosts instead, and whether this process is served
//! on a port at all. The channel's region and name differ with the rows, because neither is
//! part of the answer.

use std::sync::Arc;

use bytes::Bytes;
use engine::InMemoryEngine;
use service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice as signaling;
use signaling::state::{
    ComAmazonawsKinesisvideosignalingAwsacuitysignalingserviceState as SignalingState,
    SignalingChannel,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

/// The route `GetIceServerConfig` is served at.
const GET_ICE_SERVER_CONFIG: &str = "/v1/get-ice-server-config";

/// The transports the answer offers the relay over, each as the URI scheme carrying it and
/// its `transport` parameter — the URI form `IceServer$Uris` documents
/// (`smithy_query(query="shape", shape="IceServer")`).
const RELAY_TRANSPORTS: [(&str, &str); 3] = [("turn", "udp"), ("turns", "udp"), ("turns", "tcp")];

/// An emulator carrying only this service, served at `base` when this process is served on
/// a port at all (`MultiServiceRouter::set_url_base`, which is what
/// `OperationContext::endpoint` reports).
fn emulator_served_at(base: Option<&str>) -> (Arc<InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise the announced address, not SigV4: each request carries a
    // credential scope so the router can name the service, but no signature.
    router.set_request_authentication(false);
    router.register_service(signaling::EmulatedService::new(Arc::clone(&engine)));
    if let Some(base) = base {
        router.set_url_base(
            EndpointUrl::parse(base).unwrap_or_else(|| panic!("the base URL {base} must parse")),
        );
    }
    (engine, router)
}

/// Put a channel in the registry the way the Kinesis Video control plane's
/// `CreateSignalingChannel` does.
fn seed_channel(engine: &InMemoryEngine, arn: &str) {
    let channel = SignalingChannel::from_arn(arn)
        .unwrap_or_else(|| panic!("{arn} must parse as a signaling channel ARN"));
    let registered = engine
        .state::<SignalingState>()
        .lock()
        .register_channel(channel);
    assert!(
        registered,
        "case {arn:?}: the registry must accept the seeded channel"
    );
}

/// `GetIceServerConfig` for `arn`, from a client that addressed `authority`.
async fn get_ice_server_config(
    router: &MultiServiceRouter,
    authority: &str,
    arn: &str,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{authority}{GET_ICE_SERVER_CONFIG}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/kinesisvideo/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .body(Bytes::from(
            serde_json::json!({ "ChannelARN": arn }).to_string(),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "the response body is not JSON ({error}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The `Uris` of the one ICE server a 200 carries.
fn served_uris(case: &str, answer: &serde_json::Value) -> Vec<String> {
    let servers = answer
        .get("IceServerList")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no IceServerList in {answer}"));
    assert_eq!(
        servers.len(),
        1,
        "case {case:?}: expected one ICE server, got {servers:?}"
    );
    servers[0]
        .get("Uris")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no Uris in {}", servers[0]))
        .iter()
        .map(|uri| {
            uri.as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a Uri is not a string"))
                .to_string()
        })
        .collect()
}

/// One row: the address a client addressed, the address this process is served at, and the
/// relay authority the answer must name.
struct AddressCase {
    case: &'static str,
    /// The channel the call asks about: its region and name differ along the rows and are
    /// part of no answer.
    arn: &'static str,
    /// The `host[:port]` the client addressed.
    addressed: &'static str,
    /// The base URL this process is served at, or `None` for an emulator served on no port.
    served_base: Option<&'static str>,
    /// The `host:port` every announced relay URI must name.
    expected_authority: &'static str,
}

#[tokio::test]
async fn every_announced_relay_uri_names_an_address_this_emulator_is_reached_at() {
    let cases = [
        AddressCase {
            case: "the loopback endpoint the client addressed, with its port",
            arn: "arn:aws:kinesisvideo:us-west-2:123456789012:channel/probe-channel/1700000000",
            addressed: "127.0.0.1:9297",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "127.0.0.1:9297",
        },
        AddressCase {
            case: "another port of the same emulator",
            arn: "arn:aws:kinesisvideo:eu-west-2:000000000001:channel/second.channel_2/1",
            addressed: "127.0.0.1:18443",
            served_base: Some("https://127.0.0.1:18443"),
            expected_authority: "127.0.0.1:18443",
        },
        AddressCase {
            case: "a loopback name without a port, which https dials on 443",
            arn: "arn:aws:kinesisvideo:ap-southeast-1:999999999999:channel/named-host/42",
            addressed: "localhost",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "localhost:443",
        },
        AddressCase {
            case: "a bracketed IPv6 loopback literal keeps its brackets",
            arn: "arn:aws:kinesisvideo:sa-east-1:123456789012:channel/ipv6-host/7",
            addressed: "[::1]:9297",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "[::1]:9297",
        },
        AddressCase {
            case: "a client that addressed AWS's own host is told where this process is served",
            arn: "arn:aws:kinesisvideo:us-east-1:123456789012:channel/aws-addressed/1700000003",
            addressed: "kinesisvideo.us-east-1.amazonaws.com",
            served_base: Some("https://127.0.0.1:19297"),
            expected_authority: "127.0.0.1:19297",
        },
    ];

    for case in &cases {
        let (engine, router) = emulator_served_at(case.served_base);
        seed_channel(&engine, case.arn);

        let (status, answer) = get_ice_server_config(&router, case.addressed, case.arn).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} with {answer}",
            case.case
        );

        let uris = served_uris(case.case, &answer);
        let expected: Vec<String> = RELAY_TRANSPORTS
            .iter()
            .map(|(scheme, transport)| {
                format!(
                    "{scheme}:{}?transport={transport}",
                    case.expected_authority
                )
            })
            .collect();
        assert_eq!(
            uris, expected,
            "case {:?}: expected the relay URIs {expected:?}, got {uris:?}",
            case.case
        );

        // The channel's own identity names no part of the address, and no announced URI
        // names a host AWS publishes an endpoint at.
        let channel_name = case.arn.split('/').nth(1).expect("the ARN names a channel");
        let region = case
            .arn
            .split(':')
            .nth(3)
            .expect("the ARN names a region")
            .to_string();
        for uri in &uris {
            assert!(
                !uri.contains("amazonaws.com") && !uri.contains(&region),
                "case {:?}: an announced URI must name no AWS endpoint, got {uri:?}",
                case.case
            );
            assert!(
                !uri.contains(channel_name),
                "case {:?}: an announced URI must not name the channel ({channel_name}), got \
                 {uri:?}",
                case.case
            );
            // The documented TURN URI form carries no `//` authority marker: a client
            // parses `scheme:host:port?transport=…` by that grammar.
            assert!(
                !uri.contains("//"),
                "case {:?}: an announced URI must keep the documented TURN URI form, got \
                 {uri:?}",
                case.case
            );
        }

        // The record the call stored carries the SAME addresses it answered with, so a
        // later read of that record announces nothing else.
        let recorded = {
            let state = engine.state::<SignalingState>();
            let store = state.lock();
            let channel = store
                .resolve_channel(case.arn)
                .unwrap_or_else(|error| panic!("case {:?}: {error:?}", case.case));
            channel
                .ice_credential
                .as_ref()
                .unwrap_or_else(|| panic!("case {:?}: the call recorded no issuance", case.case))
                .uris
                .clone()
        };
        assert_eq!(
            recorded, uris,
            "case {:?}: the recorded issuance must carry the announced relay URIs",
            case.case
        );
    }
}

/// RULE: a credential set minted under one of this emulator's addresses is never replayed
/// under another — the issuance keeps its credentials for its whole lifetime, and the relay
/// address it is served with is the one the ASKING call reached.
#[tokio::test]
async fn a_stored_credential_set_is_re_addressed_onto_the_endpoint_the_asking_call_reached() {
    const ARN: &str = "arn:aws:kinesisvideo:us-west-2:123456789012:channel/re-addressed/1700000000";
    // The emulator is served at one address and reached at several: the rows are the
    // endpoints one client after another addresses it on, in order.
    let addressings = ["127.0.0.1:9297", "localhost:9297", "[::1]:9297"];

    let (engine, router) = emulator_served_at(Some("https://127.0.0.1:9297"));
    seed_channel(&engine, ARN);

    let mut credentials: Option<(String, String)> = None;
    for addressed in addressings {
        let (status, answer) = get_ice_server_config(&router, addressed, ARN).await;
        assert_eq!(
            status, 200,
            "case {addressed:?}: expected HTTP 200, got {status} with {answer}"
        );

        let uris = served_uris(addressed, &answer);
        let expected: Vec<String> = RELAY_TRANSPORTS
            .iter()
            .map(|(scheme, transport)| format!("{scheme}:{addressed}?transport={transport}"))
            .collect();
        assert_eq!(
            uris, expected,
            "case {addressed:?}: expected the relay URIs {expected:?}, got {uris:?}"
        );

        let server = &answer["IceServerList"][0];
        let issued = (
            server["Username"]
                .as_str()
                .unwrap_or_else(|| panic!("case {addressed:?}: no Username in {server}"))
                .to_string(),
            server["Password"]
                .as_str()
                .unwrap_or_else(|| panic!("case {addressed:?}: no Password in {server}"))
                .to_string(),
        );
        match &credentials {
            // The credentials are the channel's and outlive the addressing: only the
            // address a client is sent to follows the call.
            Some(first) => assert_eq!(
                &issued, first,
                "case {addressed:?}: a repeat call must answer with the recorded credentials"
            ),
            None => credentials = Some(issued),
        }
    }
}
