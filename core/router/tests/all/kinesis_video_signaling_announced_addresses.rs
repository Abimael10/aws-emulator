// SPDX-License-Identifier: CC0-1.0

//! Every ADDRESS the Kinesis Video signaling surface hands a client to DIAL, over the whole
//! path a WebRTC viewer walks: the control plane's `CreateSignalingChannel` and
//! `GetSignalingChannelEndpoint`, then the signaling data plane's `GetIceServerConfig` at the
//! endpoint that answer named.
//!
//! The rule these cases assert, one level above any single call:
//!
//! > An address this emulator announces for a client to DIAL names a host THIS process
//! > ANSWERS on — the emulator's own endpoint the request reached, when this process answers
//! > on that host, else the address this process is SERVED at — never a host AWS publishes an
//! > endpoint at and never an authority the request merely carried that nothing here answers
//! > on.
//!
//! Three members of this surface carry such an address, and all three are asserted here for
//! every row: the `HTTPS` (and `WEBRTC`) `ResourceEndpoint` of
//! `GetSignalingChannelEndpoint`, its `WSS` `ResourceEndpoint` — the socket the signaling
//! service serves a channel's peers on — and the `Uris` of the TURN credential set
//! `GetIceServerConfig` issues, which a WebRTC client takes verbatim as the relay it dials.
//!
//! The rows differ along the ADDRESSING a request carries, which is what the rule
//! generalises over: the served address itself, two other names of the same interface (the
//! loopback name, a bracketed IPv6 literal), one of AWS's own regional hosts (what a client
//! that resolves them sends), a front end's name this process is not reached on, and a
//! routable address it is not bound on. The channel's name and region differ with the rows,
//! because neither is part of any announced address.
//!
//! The announced `HTTPS` endpoint is then DIALLED — the ICE call of each row is addressed at
//! it, as a client that just read it does — so each row proves the announced address is one
//! this emulator answers the next request on, not merely one that is shaped right.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

/// The base URL this process is SERVED at (`MultiServiceRouter::set_url_base`, which is what
/// `OperationContext::endpoint` reports): the address every announcement falls back to.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// That base's `host:port`, which is the authority an announced address carries.
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// The route `GetIceServerConfig` is served at.
const ICE_ROUTE: &str = "/v1/get-ice-server-config";

/// The transports the issued credential set offers the relay over, each as the URI scheme
/// carrying it and its `transport` parameter — the `turn:`/`turns:` URI form `IceServer$Uris`
/// documents (`smithy_query(query="shape", shape="IceServer")`).
const RELAY_TRANSPORTS: [(&str, &str); 3] = [("turn", "udp"), ("turns", "udp"), ("turns", "tcp")];

/// How many times a case looks for a channel to reach `ACTIVE`, [`ACTIVE_POLL_INTERVAL`]
/// apart: a budget far above the span the control plane owes that transition, and bounded so
/// a case that never activates fails instead of hanging.
const ACTIVE_POLL_ATTEMPTS: usize = 1_200;

/// How long a case waits between looks; see [`ACTIVE_POLL_ATTEMPTS`].
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The control plane and the signaling data plane on ONE engine, as `core/router` mounts
/// them, served at [`SERVED_BASE`].
fn harness() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise the announced addresses, not SigV4: every request carries a
    // credential scope so the router can name the service, but no signature.
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(SERVED_BASE).unwrap_or_else(|| panic!("{SERVED_BASE} must parse")),
    );
    router.register_service(
        service_com_amazonaws_kinesisvideo_kinesisvideo_20170930::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// POST one restJson1 call to `path`, from a client that addressed `authority` and signed for
/// `region`.
async fn call(
    router: &MultiServiceRouter,
    authority: &str,
    region: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{region}/kinesisvideo/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
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

/// Poll `DescribeSignalingChannel` until the channel reports `ACTIVE`, as a client waiting on
/// the asynchronous create does. Waits for the CONDITION, never for a fixed time.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, region: &str, arn: &str) {
    for _ in 0..ACTIVE_POLL_ATTEMPTS {
        let (status, body) = call(
            router,
            SERVED_AUTHORITY,
            region,
            "/describeSignalingChannel",
            serde_json::json!({ "ChannelARN": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSignalingChannel must answer 200, got {status} with {body}"
        );
        if body["ChannelInfo"]["ChannelStatus"] == "ACTIVE" {
            return;
        }
        tokio::time::sleep(ACTIVE_POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the channel must reach ACTIVE within {ACTIVE_POLL_ATTEMPTS} looks");
}

/// The `ResourceEndpoint` the answer carries for `protocol`.
fn announced_endpoint(case: &str, answer: &serde_json::Value, protocol: &str) -> String {
    answer["ResourceEndpointList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ResourceEndpointList in {answer}"))
        .iter()
        .find(|item| item["Protocol"] == protocol)
        .and_then(|item| item["ResourceEndpoint"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: no {protocol} endpoint in {answer}"))
        .to_string()
}

/// The `host[:port]` of a `scheme://host[:port]` URL.
fn authority_of(case: &str, url: &str) -> String {
    url.split_once("://")
        .unwrap_or_else(|| panic!("case {case:?}: expected a scheme://authority URL, got {url:?}"))
        .1
        .trim_end_matches('/')
        .to_string()
}

/// One row: the addressing a client carries, and the authority the announced `HTTPS`
/// endpoint must then name.
struct AddressingCase {
    case: &'static str,
    /// The `host[:port]` the client addressed.
    addressed: &'static str,
    /// The authority the announced `HTTPS`/`WEBRTC` endpoint must carry: the carried one when
    /// this process answers on it, else the address this process is served at.
    expected_authority: &'static str,
    /// The channel this row creates, and the region it signs for — part of no address.
    channel_name: &'static str,
    region: &'static str,
}

/// The rows: see the module header for what they span.
const ROWS: &[AddressingCase] = &[
    AddressingCase {
        case: "the served address itself",
        addressed: SERVED_AUTHORITY,
        expected_authority: SERVED_AUTHORITY,
        channel_name: "sim2real-probe-kvsig-served",
        region: "us-west-2",
    },
    AddressingCase {
        case: "the loopback name of the same interface",
        addressed: "localhost:9297",
        expected_authority: "localhost:9297",
        channel_name: "channel.named_host-1",
        region: "eu-central-1",
    },
    AddressingCase {
        case: "a bracketed IPv6 loopback literal",
        addressed: "[::1]:9297",
        expected_authority: "[::1]:9297",
        channel_name: "ipv6-addressed",
        region: "ap-southeast-1",
    },
    AddressingCase {
        case: "one of AWS's own regional hosts, which this process does not answer on",
        addressed: "kinesisvideo.us-east-1.amazonaws.com",
        expected_authority: SERVED_AUTHORITY,
        channel_name: "aws-addressed",
        region: "us-east-1",
    },
    AddressingCase {
        case: "a front end between the client and this process",
        addressed: "signaling-front-end.example:8443",
        expected_authority: SERVED_AUTHORITY,
        channel_name: "front-end-addressed",
        region: "sa-east-1",
    },
    AddressingCase {
        case: "a routable address this process is not bound on",
        addressed: "10.0.0.7:8443",
        expected_authority: SERVED_AUTHORITY,
        channel_name: "routable-addressed",
        region: "ca-central-1",
    },
];

#[tokio::test]
async fn every_announced_signaling_address_is_one_this_emulator_answers_on() {
    let router = harness();

    for row in ROWS {
        let case = row.case;
        let (status, created) = call(
            &router,
            row.addressed,
            row.region,
            "/createSignalingChannel",
            serde_json::json!({
                "ChannelName": row.channel_name,
                "ChannelType": "SINGLE_MASTER",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateSignalingChannel must answer 200, got {status} with {created}"
        );
        let arn = created["ChannelARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ChannelARN in {created}"))
            .to_string();
        wait_until_active(&router, case, row.region, &arn).await;

        // The endpoints a client resolves before it dials anything.
        let (status, resolved) = call(
            &router,
            row.addressed,
            row.region,
            "/getSignalingChannelEndpoint",
            serde_json::json!({
                "ChannelARN": arn.clone(),
                "SingleMasterChannelEndpointConfiguration": {
                    "Protocols": ["HTTPS", "WSS"],
                    "Role": "VIEWER",
                }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSignalingChannelEndpoint must answer 200, got {status} with \
             {resolved}"
        );

        let https_endpoint = announced_endpoint(case, &resolved, "HTTPS");
        assert_eq!(
            https_endpoint,
            format!("https://{}", row.expected_authority),
            "case {case:?}: expected the HTTPS endpoint at {:?}, got {https_endpoint:?}",
            row.expected_authority
        );

        // The WSS endpoint is the socket the signaling service serves a channel's peers on:
        // a loopback address of this process with the port it landed on, never the carried
        // addressing and never a host AWS publishes.
        let wss_endpoint = announced_endpoint(case, &resolved, "WSS");
        let wss_authority = authority_of(case, &wss_endpoint);
        assert!(
            wss_endpoint.starts_with("wss://"),
            "case {case:?}: expected a wss:// endpoint, got {wss_endpoint:?}"
        );
        let (wss_host, wss_port) = wss_authority.rsplit_once(':').unwrap_or_else(|| {
            panic!("case {case:?}: expected a host:port, got {wss_authority:?}")
        });
        assert_eq!(
            wss_host, "127.0.0.1",
            "case {case:?}: the websocket endpoint must name an interface this emulator binds, \
             got {wss_endpoint:?}"
        );
        assert!(
            wss_port.parse::<u16>().is_ok_and(|port| port > 0),
            "case {case:?}: the websocket endpoint must name the port it is served on, got \
             {wss_endpoint:?}"
        );

        // The announced HTTPS endpoint is DIALLED: the ICE call goes to the authority the
        // answer above named, as a client that just read it does.
        let (status, issued) = call(
            &router,
            row.expected_authority,
            row.region,
            ICE_ROUTE,
            serde_json::json!({ "ChannelARN": arn.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetIceServerConfig at the announced endpoint must answer 200, got \
             {status} with {issued}"
        );

        let servers = issued["IceServerList"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no IceServerList in {issued}"));
        assert_eq!(
            servers.len(),
            1,
            "case {case:?}: expected one ICE server, got {servers:?}"
        );
        let uris: Vec<String> = servers[0]["Uris"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no Uris in {}", servers[0]))
            .iter()
            .map(|uri| {
                uri.as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a Uri is not a string"))
                    .to_string()
            })
            .collect();

        // The relay a viewer dials is the address this PROCESS answers on, whichever of this
        // emulator's addresses the asking client used, in the documented TURN URI form
        // (`scheme ":" host [":" port] ["?transport=" transport]`, with no `//` marker).
        let expected_uris: Vec<String> = RELAY_TRANSPORTS
            .iter()
            .map(|(scheme, transport)| {
                format!("{scheme}:{SERVED_AUTHORITY}?transport={transport}")
            })
            .collect();
        assert_eq!(
            uris, expected_uris,
            "case {case:?}: expected the relay URIs {expected_uris:?}, got {uris:?}"
        );

        // No announced value names a host AWS publishes an endpoint at, the carried
        // addressing this process does not answer on, or the channel the call named.
        let announced = [https_endpoint.clone(), wss_endpoint.clone()]
            .into_iter()
            .chain(uris.iter().cloned())
            .collect::<Vec<String>>();
        for value in &announced {
            assert!(
                !value.contains("amazonaws.com"),
                "case {case:?}: an announced address must name no AWS endpoint, got {value:?}"
            );
            assert!(
                !value.contains(row.channel_name),
                "case {case:?}: an announced address must not name the channel, got {value:?}"
            );
            if row.expected_authority != row.addressed {
                assert!(
                    !value.contains(row.addressed),
                    "case {case:?}: an announced address must not be the carried authority \
                     {:?}, got {value:?}",
                    row.addressed
                );
            }
        }
    }
}
