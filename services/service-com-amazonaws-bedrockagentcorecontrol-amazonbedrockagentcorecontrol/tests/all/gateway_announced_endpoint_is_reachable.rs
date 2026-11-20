// SPDX-License-Identifier: CC0-1.0

//! The address a gateway ANNOUNCES in `gatewayUrl`, for every address this emulator is
//! reached and served at.
//!
//! RULE: the endpoint a service announces for a resource's data plane is an address this
//! emulator ANSWERS on for the client that asked — the emulator's own endpoint the request
//! reached, and otherwise the address this emulator is served at, with the resource named in
//! the path and the protocol's transport path kept — so a host AWS publishes its own
//! endpoints at is announced only where this emulator has no address of its own to announce
//! in its place.
//!
//! The rows span what the rule generalises over: the address the client reached this emulator
//! at (a loopback IP with a port, the loopback NAME, a bracketed IPv6 literal, and one of
//! AWS's own endpoint hosts), whether this emulator is served on an address of its own and
//! whether that address is an IP or a name the operator bound, and whether the gateway named
//! its `protocolType` or left it to the modelled default. Each row drives the announced URL
//! VERBATIM — its own host, its own path, appending nothing — and the gateway's MCP endpoint
//! has to answer there, so an address the emulator does not serve cannot pass.
//!
//! The second test spans the other dimension of the same rule: WHICH client is asking for
//! which report. A gateway created by one client is read and updated by another, and every
//! one of those reports announces the address its own caller reached — the create's answer is
//! not a value later callers inherit.
//!
//! Lambda is not mounted here, so the gateway has no target and its tool listing is empty;
//! what matters in this suite is WHICH address answers, not what the tools are.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::{
    state, EmulatedService,
};

// The `(host, path)` splitter the MCP endpoint suite already states for a URL this service
// published; it reports the root path for a URL that carries none.
use crate::gateway_mcp_endpoint::split_url;

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// The Region every call in this suite is signed for.
const REGION: &str = "us-west-2";

/// The MCP protocol revision an MCP client declares in `MCP-Protocol-Version`.
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// What this emulator must be ANNOUNCED as, once a row has created its gateway.
#[derive(Clone, Copy, Debug)]
enum Announced {
    /// An address of this emulator's own: that authority, with the gateway named in the
    /// path and the transport path last.
    OwnAddress(&'static str),
    /// No address of this emulator's own exists, so the gateway's own AWS endpoint host is
    /// announced instead — the one case where a host AWS publishes endpoints at is the
    /// address to hand back, and a host this service's data plane claims as well.
    GatewayEndpointHost,
}

/// One case: who asked, how this emulator is served, and the address that client has to be
/// announced.
#[derive(Clone, Copy, Debug)]
struct Row {
    case: &'static str,
    /// The `Host` the control-plane call carries — the address this client reached the
    /// emulator at.
    reached: &'static str,
    /// The base URL this emulator is served at, or `None` for one served on no port at all.
    served_at: Option<&'static str>,
    /// The `protocolType` the create names, or `None` to leave it to the modelled default.
    protocol_type: Option<&'static str>,
    /// The gateway name this row creates, unique for diagnostics.
    name: &'static str,
    announced: Announced,
}

/// The cases the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "a client on the emulator's loopback address",
        reached: "127.0.0.1:9297",
        served_at: None,
        protocol_type: None,
        name: "announcedloopbackip",
        announced: Announced::OwnAddress("127.0.0.1:9297"),
    },
    Row {
        case: "a client on the loopback name",
        reached: "localhost:9297",
        served_at: None,
        protocol_type: Some("MCP"),
        name: "announcedloopbackname",
        announced: Announced::OwnAddress("localhost:9297"),
    },
    Row {
        case: "a client on a bracketed IPv6 loopback literal",
        reached: "[::1]:9297",
        served_at: None,
        protocol_type: None,
        name: "announcedipv6",
        announced: Announced::OwnAddress("[::1]:9297"),
    },
    Row {
        case: "a client on the emulator's own endpoint, on an emulator served elsewhere too",
        reached: "127.0.0.1:9297",
        served_at: Some("https://127.0.0.1:9555"),
        protocol_type: Some("MCP"),
        name: "announcedreachedwins",
        announced: Announced::OwnAddress("127.0.0.1:9297"),
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on an emulator served on a port",
        reached: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        served_at: Some("https://127.0.0.1:9555"),
        protocol_type: Some("MCP"),
        name: "announcedservedport",
        announced: Announced::OwnAddress("127.0.0.1:9555"),
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on an emulator served at a bound \
               name",
        reached: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        served_at: Some("https://emulator.test:8443"),
        protocol_type: None,
        name: "announcedservedname",
        announced: Announced::OwnAddress("emulator.test:8443"),
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on an emulator served on no port \
               at all",
        reached: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        served_at: None,
        protocol_type: None,
        name: "announcednoownaddress",
        announced: Announced::GatewayEndpointHost,
    },
];

/// A router whose `OperationContext::endpoint` is `served_at`, which is the address this
/// emulator answers on when the request addressed one of AWS's own hosts; `None` leaves the
/// router's unserved base, which names no port at all.
fn router(served_at: Option<&str>) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    if let Some(base) = served_at {
        router.set_url_base(
            smithy_server_runtime::EndpointUrl::parse(base).expect("a base URL the router takes"),
        );
    }
    (router, engine)
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One control-plane call, as a client that reached this emulator at `reached` makes it.
async fn control(
    router: &MultiServiceRouter,
    reached: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", reached);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, body_json(response.body()))
}

/// Drive `url` the way an MCP client holding it does: its own host in the `Host` header, its
/// own path as the request target, and the headers an MCP client sends.
async fn tools_list(router: &MultiServiceRouter, case: &str, url: &str) -> (u16, Value) {
    let (host, path) = split_url(case, url);
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
        .body(Bytes::from(message.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, body_json(response.body()))
}

fn body_json(bytes: &Bytes) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Make every clock-owed `CREATING` -> `READY` transition fall due, so the next request
/// materialises `READY` without waiting the span out on the wall clock. Writes only the due
/// instant the emulator itself stores.
fn settle_now(engine: &Arc<engine::InMemoryEngine>) {
    let store = engine
        .state::<state::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState>();
    let mut store = store.lock();
    let past = SystemTime::now() - Duration::from_millis(1);
    for gateway in store.gateways.values_mut() {
        gateway.settles_at = past;
    }
}

/// Assert that `url` announces what `announced` requires for the gateway `gateway_id`.
fn assert_announced(case: &str, url: &str, announced: Announced, gateway_id: &str) {
    assert!(
        url.starts_with("https://"),
        "case {case:?}: the announced endpoint must carry the scheme its clients dial, got \
         {url:?}"
    );
    let (host, path) = split_url(case, url);
    match announced {
        Announced::OwnAddress(authority) => {
            assert_eq!(
                host, authority,
                "case {case:?}: expected the address this emulator answers on, got {host:?} \
                 (announced {url:?})"
            );
            assert!(
                !service_common::data_plane_addressing::addresses_an_aws_endpoint(&host),
                "case {case:?}: an address of this emulator's own exists, so no host AWS \
                 publishes its own endpoints at may be announced, got {host:?}"
            );
            let expected = format!(
                "/{namespace}/{plane}/{gateway_id}{transport}",
                namespace = service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
                plane = state::GATEWAY_PLANE_SEGMENT,
                transport = state::GATEWAY_MCP_PATH,
            );
            assert_eq!(
                path, expected,
                "case {case:?}: expected the gateway named in the path with the transport path \
                 last, got {path:?} (announced {url:?})"
            );
        }
        Announced::GatewayEndpointHost => {
            assert_eq!(
                host,
                state::gateway_endpoint_host(REGION, gateway_id),
                "case {case:?}: with no address of this emulator's own to announce, expected \
                 the gateway's own endpoint host, got {host:?} (announced {url:?})"
            );
            assert_eq!(
                path,
                state::GATEWAY_MCP_PATH,
                "case {case:?}: expected the transport path alone under the gateway's own \
                 host, got {path:?} (announced {url:?})"
            );
        }
    }
}

/// `CreateGateway` as a client that reached this emulator at `reached`, reporting
/// `(gatewayId, announced gatewayUrl)`.
async fn create_gateway(
    router: &MultiServiceRouter,
    case: &str,
    reached: &str,
    name: &str,
    protocol_type: Option<&str>,
) -> (String, String) {
    let mut request = json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "authorizerType": "AWS_IAM",
    });
    if let Some(protocol_type) = protocol_type {
        request["protocolType"] = json!(protocol_type);
    }
    let (status, created) = control(router, reached, "POST", "/gateways/", Some(&request)).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateGateway, got {status} ({created})"
    );
    (text(&created, "gatewayId"), text(&created, "gatewayUrl"))
}

/// Drive the announced URL the way the client holding it does, and expect the gateway's own
/// MCP endpoint to answer there.
async fn assert_served(router: &MultiServiceRouter, case: &str, url: &str) {
    let (status, answer) = tools_list(router, case, url).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the announced endpoint {url:?} must be served, got {status} ({answer})"
    );
    assert_eq!(
        answer["result"]["tools"],
        json!([]),
        "case {case:?}: a gateway with no target lists no tool at {url:?}, got {}",
        answer["result"]
    );
}

/// RULE: the announced endpoint is an address this emulator answers on for the client that
/// asked, and that announced URL — driven verbatim — reaches the gateway's MCP endpoint.
#[tokio::test]
async fn a_gateway_announces_an_address_this_emulator_answers_on() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router(row.served_at);
        let (gateway_id, url) =
            create_gateway(&router, case, row.reached, row.name, row.protocol_type).await;
        assert_announced(case, &url, row.announced, &gateway_id);

        // The read reports the identical value the create announced: one gateway, one
        // address for the client that asked.
        settle_now(&engine);
        let (status, fetched) = control(
            &router,
            row.reached,
            "GET",
            &format!("/gateways/{gateway_id}/"),
            None,
        )
        .await;
        assert_eq!(
            (status, text(&fetched, "gatewayUrl")),
            (200, url.clone()),
            "case {case:?}: GetGateway must report the announced endpoint {url:?}, got \
             {status} ({fetched})"
        );

        // And the announced URL is served: the address the gateway handed back is the
        // address its MCP endpoint answers on.
        assert_served(&router, case, &url).await;
    }
}

/// One case of the rule ACROSS clients: a gateway created by one client, reported to
/// another.
#[derive(Clone, Copy, Debug)]
struct CrossRow {
    case: &'static str,
    /// The address the client that CREATED the gateway reached this emulator at.
    created_by: &'static str,
    /// The address the client that READS and UPDATES it reached this emulator at.
    read_by: &'static str,
    served_at: Option<&'static str>,
    name: &'static str,
    /// What the READER has to be announced — the address IT can reach, not the creator's.
    announced: Announced,
}

/// The cases that span who created the gateway against who reads it.
const CROSS_ROWS: &[CrossRow] = &[
    CrossRow {
        case: "created by a client that resolves AWS's own hostnames, read on the emulator's \
               own endpoint",
        created_by: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        read_by: "127.0.0.1:9297",
        served_at: None,
        name: "crossownreader",
        announced: Announced::OwnAddress("127.0.0.1:9297"),
    },
    CrossRow {
        case: "created on the emulator's own endpoint, read by a client that resolves AWS's \
               own hostnames",
        created_by: "127.0.0.1:9297",
        read_by: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        served_at: None,
        name: "crossawsreader",
        announced: Announced::GatewayEndpointHost,
    },
    CrossRow {
        case: "created on one address of the emulator's own, read on another",
        created_by: "127.0.0.1:9297",
        read_by: "[::1]:9297",
        served_at: None,
        name: "crossotherownaddress",
        announced: Announced::OwnAddress("[::1]:9297"),
    },
    CrossRow {
        case: "created by a client that resolves AWS's own hostnames, read by one too, on an \
               emulator served on a port",
        created_by: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        read_by: "bedrock-agentcore-control.us-west-2.amazonaws.com",
        served_at: Some("https://127.0.0.1:9555"),
        name: "crossservedport",
        announced: Announced::OwnAddress("127.0.0.1:9555"),
    },
];

/// RULE, across clients: each report of a gateway announces the address ITS OWN reader
/// reached this emulator at — a read and an update as much as the create — so no client is
/// handed the address another client was given.
#[tokio::test]
async fn every_report_announces_the_address_its_own_reader_reached() {
    for row in CROSS_ROWS {
        let case = row.case;
        let (router, engine) = router(row.served_at);
        let (gateway_id, _) = create_gateway(&router, case, row.created_by, row.name, None).await;
        settle_now(&engine);

        let (status, fetched) = control(
            &router,
            row.read_by,
            "GET",
            &format!("/gateways/{gateway_id}/"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetGateway, got {status} ({fetched})"
        );
        let read_url = text(&fetched, "gatewayUrl");
        assert_announced(case, &read_url, row.announced, &gateway_id);
        assert_served(&router, case, &read_url).await;

        // `UpdateGateway` announces the endpoint too, and announces it for the client that
        // made the update.
        let (status, updated) = control(
            &router,
            row.read_by,
            "PUT",
            &format!("/gateways/{gateway_id}/"),
            Some(&json!({
                "name": row.name,
                "roleArn": ROLE_ARN,
                "authorizerType": "AWS_IAM",
                "description": "the same gateway, reconfigured",
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from UpdateGateway, got {status} ({updated})"
        );
        assert_eq!(
            text(&updated, "gatewayUrl"),
            read_url,
            "case {case:?}: UpdateGateway must announce the same endpoint the read did, got \
             {updated}"
        );

        settle_now(&engine);
        assert_served(&router, case, &read_url).await;
    }
}
