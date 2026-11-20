// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS `GetIceServerConfig` hands a viewer for the TURN relay.
//!
//! The rule these cases assert, one level above the recorded case (one channel, one call,
//! one address):
//!
//! > Every address this plane announces for a client to DIAL names the host and port THIS
//! > emulator ANSWERS on — the endpoint this process is served at, else the one it is
//! > configured to serve on — never a host AWS publishes an endpoint at and never an
//! > address a request merely carried, in the `turn:`/`turns:` URI form `IceServer$Uris`
//! > documents.
//!
//! The rows span what the rule generalises over: the port this process is served on, and
//! the addressing a request carries (the served endpoint itself, another of this emulator's
//! own names, a bracketed IPv6 literal, a port this process does not serve, a name carrying
//! no port, one of AWS's own hosts) — none of which moves the answer. The channel's region
//! and name differ with the rows, because neither is part of the answer either.

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
async fn every_announced_relay_uri_names_an_address_this_emulator_is_served_at() {
    let cases = [
        AddressCase {
            case: "the loopback endpoint the client addressed, which is the served one",
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
            case: "another of this emulator's own names does not move the answer",
            arn: "arn:aws:kinesisvideo:ap-southeast-1:999999999999:channel/named-host/42",
            addressed: "localhost:9297",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "127.0.0.1:9297",
        },
        AddressCase {
            case: "a bracketed IPv6 addressing does not move the answer either",
            arn: "arn:aws:kinesisvideo:sa-east-1:123456789012:channel/ipv6-host/7",
            addressed: "[::1]:9297",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "127.0.0.1:9297",
        },
        AddressCase {
            case: "a served IPv6 endpoint keeps its brackets",
            arn: "arn:aws:kinesisvideo:ca-central-1:123456789012:channel/ipv6-served/8",
            addressed: "[::1]:9443",
            served_base: Some("https://[::1]:9443"),
            expected_authority: "[::1]:9443",
        },
        AddressCase {
            case: "a carried port this process does not serve is not announced",
            arn: "arn:aws:kinesisvideo:us-west-1:123456789012:channel/foreign-port/1700000001",
            addressed: "127.0.0.1:43577",
            served_base: Some("https://127.0.0.1:35713"),
            expected_authority: "127.0.0.1:35713",
        },
        AddressCase {
            case: "a loopback name carrying no port is not announced either",
            arn: "arn:aws:kinesisvideo:eu-central-1:123456789012:channel/no-port/1700000002",
            addressed: "localhost",
            served_base: Some("https://127.0.0.1:9297"),
            expected_authority: "127.0.0.1:9297",
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

/// RULE: a credential set minted under this emulator's address is never replayed under
/// another — the issuance keeps its credentials for its whole lifetime, and every call is
/// answered with the relay address this process is served at, whichever of this emulator's
/// addresses the asking client used.
#[tokio::test]
async fn a_stored_credential_set_keeps_the_address_this_process_is_served_at() {
    const ARN: &str = "arn:aws:kinesisvideo:us-west-2:123456789012:channel/re-addressed/1700000000";
    const SERVED_AUTHORITY: &str = "127.0.0.1:9297";
    // The emulator is served at one address and addressed at several: the rows are the
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
            .map(|(scheme, transport)| {
                format!("{scheme}:{SERVED_AUTHORITY}?transport={transport}")
            })
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

/// RULE, stated as the invariant it is: whatever a request CARRIES as its address, every
/// announced relay URI names the authority this process is SERVED at — so no client is ever
/// sent to an address nothing here answers on.
///
/// The rows differ along the addressing a request can carry (an address of a front end
/// between the client and this process, a routable host, a host naming a service, a name
/// with no port, the served endpoint itself under each of this emulator's own names) and
/// along the port this process is served on, which is the one every answer has to carry.
#[tokio::test]
async fn every_announced_relay_uri_names_the_authority_this_process_is_served_at() {
    const SERVED_PORTS: [u16; 3] = [9297, 35713, 18443];
    // One carried addressing per row, with the channel it asks about: the channel's name
    // and region are part of no answer, so they differ with the rows.
    let carried: [(&str, &str); 6] = [
        (
            "a front end between the client and this process",
            "arn:aws:kinesisvideo:us-west-2:123456789012:channel/front-end/1700000000",
        ),
        (
            "a routable host this process is not bound on",
            "arn:aws:kinesisvideo:eu-west-1:000000000001:channel/routable-host/1700000001",
        ),
        (
            "a host naming one of AWS's own endpoints",
            "arn:aws:kinesisvideo:ap-northeast-1:999999999999:channel/aws-host/1700000002",
        ),
        (
            "a loopback name carrying no port at all",
            "arn:aws:kinesisvideo:sa-east-1:123456789012:channel/no-port/1700000003",
        ),
        (
            "the loopback literal on the served port",
            "arn:aws:kinesisvideo:us-east-1:123456789012:channel/own-literal/1700000004",
        ),
        (
            "the loopback name on the served port",
            "arn:aws:kinesisvideo:us-east-2:123456789012:channel/own-name/1700000005",
        ),
    ];

    for served_port in SERVED_PORTS {
        let served_authority = format!("127.0.0.1:{served_port}");
        let (engine, router) = emulator_served_at(Some(&format!("https://{served_authority}")));
        for (index, (addressing, arn)) in carried.iter().enumerate() {
            // The addressings are built from the served port so the rows that carry a
            // DIFFERENT port stay different at every served port.
            let addressed = match index {
                0 => format!("127.0.0.1:{}", served_port + 1),
                1 => "10.0.0.7:8443".to_string(),
                2 => "kinesisvideo.us-west-2.amazonaws.com".to_string(),
                3 => "localhost".to_string(),
                4 => served_authority.clone(),
                _ => format!("localhost:{served_port}"),
            };
            let case = format!("{addressing} at the served port {served_port}");
            seed_channel(&engine, arn);

            let (status, answer) = get_ice_server_config(&router, &addressed, arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {answer}"
            );

            let uris = served_uris(&case, &answer);
            let expected: Vec<String> = RELAY_TRANSPORTS
                .iter()
                .map(|(scheme, transport)| {
                    format!("{scheme}:{served_authority}?transport={transport}")
                })
                .collect();
            assert_eq!(
                uris, expected,
                "case {case:?}: expected the relay URIs {expected:?}, got {uris:?}"
            );
        }
    }
}
