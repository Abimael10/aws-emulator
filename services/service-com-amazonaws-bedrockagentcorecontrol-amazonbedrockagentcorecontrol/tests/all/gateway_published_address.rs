// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a gateway publishes in `gatewayUrl`, and that it is the address this
//! service serves the gateway's protocol on.
//!
//! RULE: the endpoint a gateway publishes is an address this emulator ANSWERS on for the
//! client that asked — the emulator's own endpoint, with the gateway named in the path,
//! for a client that reaches this emulator directly, and the gateway's own AWS endpoint
//! host for a client that resolves AWS's own hostnames and has no own address to be given
//! instead — and joining the protocol's transport path onto that URL is IDEMPOTENT: the
//! published value already ends in the transport path, so a client that appends it again
//! (the usual `url.rstrip('/') + '/mcp'` join), or appends a separator, reaches the same
//! endpoint. Nothing else under that address is served.
//!
//! The rows span both dimensions the rule generalises over: WHO asked (the two kinds of
//! client, which decide the form of the published address) and HOW the transport path was
//! joined onto what they were handed (as published, with a trailing separator, with repeated
//! trailing separators, with the transport path appended once and twice, and the refusing side
//! — the address without its
//! transport path, a segment that merely BEGINS with the transport path's text, a
//! sub-resource beneath it, and the transport path beneath another segment).
//!
//! Lambda is not mounted here, so the gateway has no target and its tool listing is empty;
//! the tools a target contributes are pinned in
//! `core/router/tests/all/bedrock_agentcore_gateway_mcp_transport_path.rs`, where both
//! services can be mounted together. What matters here is WHICH addresses answer.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::{
    state, EmulatedService,
};

use crate::gateway_mcp_endpoint::split_url;

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The address a client on this machine reaches the emulator at: a loopback authority,
/// which names no service and is therefore the emulator's own endpoint.
const EMULATOR_AUTHORITY: &str = "127.0.0.1:9297";

/// The transport path an MCP gateway's endpoint is served on, which its published
/// `gatewayUrl` ends in.
const TRANSPORT_PATH: &str = "/mcp";

/// The MCP protocol revision an MCP client declares in `MCP-Protocol-Version`.
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Who is calling: the two kinds of client a published address is composed for.
#[derive(Clone, Copy, Debug)]
enum Caller {
    /// A client that resolves AWS's own hostnames and addresses this service's endpoint.
    AwsEndpoint,
    /// A client on the machine this emulator runs on, which reaches the emulator's own
    /// endpoint and resolves nothing else.
    EmulatorEndpoint,
}

impl Caller {
    /// The host this caller sends a control-plane call with.
    fn host(self) -> String {
        match self {
            Self::AwsEndpoint => format!("bedrock-agentcore-control.{REGION}.amazonaws.com"),
            Self::EmulatorEndpoint => EMULATOR_AUTHORITY.to_string(),
        }
    }

    fn case(self) -> &'static str {
        match self {
            Self::AwsEndpoint => "a client that resolves AWS's own hostnames",
            Self::EmulatorEndpoint => "a client on the emulator's own endpoint",
        }
    }
}

/// A SigV4 `Authorization` header for this service.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One control-plane call, as `caller` addresses this emulator.
async fn control(
    router: &MultiServiceRouter,
    caller: Caller,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", caller.host());
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, body_json(response.body()))
}

/// One JSON-RPC call to `path` on `host`, driven the way an MCP client does — and, when
/// `signed` is false, the way an unauthenticated one does.
async fn mcp(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    signed: bool,
) -> (u16, Value) {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}});
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION);
    if signed {
        request = request.header("authorization", authorization());
    }
    let request = request
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
/// materialises `READY` without waiting the span out on the wall clock. Writes only the
/// due instant the emulator itself stores.
fn settle_now(engine: &Arc<engine::InMemoryEngine>) {
    let store = engine
        .state::<state::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState>();
    let mut store = store.lock();
    let past = SystemTime::now() - Duration::from_millis(1);
    for gateway in store.gateways.values_mut() {
        gateway.settles_at = past;
    }
}

/// Create a `READY` gateway as `caller`, and report `(host, base, published path)` of the
/// endpoint it published: the base is that path without its trailing transport path.
async fn published_endpoint(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    caller: Caller,
    name: &str,
) -> (String, String, String) {
    let case = caller.case();
    let (status, created) = control(
        router,
        caller,
        "POST",
        "/gateways/",
        Some(&json!({
            "name": name,
            "roleArn": ROLE_ARN,
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateGateway, got {status} ({created})"
    );
    let gateway_id = text(&created, "gatewayId");
    let url = text(&created, "gatewayUrl");

    // The read reports the same published endpoint the create did: one gateway, one
    // address.
    settle_now(engine);
    let (status, fetched) = control(
        router,
        caller,
        "GET",
        &format!("/gateways/{gateway_id}/"),
        None,
    )
    .await;
    assert_eq!(
        (status, text(&fetched, "gatewayUrl")),
        (200, url.clone()),
        "case {case:?}: GetGateway must report the published endpoint {url:?}, got {status} \
         ({fetched})"
    );

    let (host, path) = split_url(case, &url);
    match caller {
        Caller::AwsEndpoint => assert_eq!(
            host,
            format!("{gateway_id}.gateway.bedrock-agentcore.{REGION}.amazonaws.com"),
            "case {case:?}: expected the gateway's own endpoint host, got {host:?}"
        ),
        Caller::EmulatorEndpoint => assert_eq!(
            host, EMULATOR_AUTHORITY,
            "case {case:?}: expected the emulator's own endpoint, got {host:?}"
        ),
    }
    let base = path
        .strip_suffix(TRANSPORT_PATH)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the published path must end in the transport path \
                 {TRANSPORT_PATH}, got {path:?}"
            )
        })
        .to_string();
    (host, base, path)
}

/// RULE, through the MUTATION of a gateway: every call that reports `gatewayUrl` reports
/// the one address this emulator answers that gateway on, so an `UpdateGateway` publishes
/// what the create published and that address is still served afterwards.
///
/// The rows are the two kinds of client, which decide the form of the published address;
/// an update renames nothing (the model requires the name the gateway was created with),
/// so what varies between the calls is which caller asked.
#[tokio::test]
async fn an_update_publishes_the_address_the_emulator_answers_the_gateway_on() {
    for (caller, name) in [
        (Caller::AwsEndpoint, "awsupdatedgw"),
        (Caller::EmulatorEndpoint, "ownupdatedgw"),
    ] {
        let (router, engine) = router();
        let case = caller.case();
        let (host, _, path) = published_endpoint(&router, &engine, caller, name).await;
        let published = format!("https://{host}{path}");

        let (status, updated) = control(
            &router,
            caller,
            "PUT",
            &format!("/gateways/{}/", gateway_id_of(&host, &path)),
            Some(&json!({
                "name": name,
                "roleArn": ROLE_ARN,
                "authorizerType": "AWS_IAM",
                "description": "the configuration this update replaces it with",
            })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from UpdateGateway, got {status} ({updated})"
        );
        assert_eq!(
            text(&updated, "gatewayUrl"),
            published,
            "case {case:?}: an update publishes the address the gateway is answered on \
             ({published:?}), got {:?}",
            text(&updated, "gatewayUrl")
        );

        // The address the update published is still the one that answers: an update
        // re-announces an endpoint, it does not move it.
        settle_now(&engine);
        let (status, answer) = mcp(&router, &host, &path, true).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the address the update published must be served, got {status} \
             ({answer})"
        );
    }
}

/// The `gatewayId` a published endpoint names: the last path segment before the transport
/// path for a client on the emulator's own endpoint, and the first host label for a client
/// on AWS's own hostnames — the two forms [`published_endpoint`] asserts.
fn gateway_id_of(host: &str, path: &str) -> String {
    let base = path.strip_suffix(TRANSPORT_PATH).unwrap_or(path);
    match base.rsplit('/').next().unwrap_or_default() {
        "" => host.split('.').next().unwrap_or_default().to_string(),
        last => last.to_string(),
    }
}

/// RULE: the published address is served, the transport-path join is idempotent, and
/// nothing else under that address answers — for both kinds of client.
#[tokio::test]
async fn the_published_address_is_served_and_the_transport_path_join_is_idempotent() {
    for (caller, name) in [
        (Caller::AwsEndpoint, "awsaddressgw"),
        (Caller::EmulatorEndpoint, "ownaddressgw"),
    ] {
        let (router, engine) = router();
        let (host, base, published) = published_endpoint(&router, &engine, caller, name).await;

        // (row, path, served)
        let rows: [(&str, String, bool); 10] = [
            (
                "the published URL as it was handed back",
                published.clone(),
                true,
            ),
            (
                "the published URL with a trailing separator",
                format!("{published}/"),
                true,
            ),
            (
                "the transport path joined onto the published URL",
                format!("{published}{TRANSPORT_PATH}"),
                true,
            ),
            (
                "the transport path joined on twice",
                format!("{published}{TRANSPORT_PATH}{TRANSPORT_PATH}"),
                true,
            ),
            (
                "the joined path with a trailing separator",
                format!("{published}{TRANSPORT_PATH}/"),
                true,
            ),
            (
                "the published URL with repeated trailing separators",
                format!("{published}//"),
                true,
            ),
            (
                "the address without its transport path",
                format!("{base}/"),
                false,
            ),
            (
                "a segment that only begins with the transport path",
                format!("{published}-tools"),
                false,
            ),
            (
                "a sub-resource beneath the transport path",
                format!("{published}/tools"),
                false,
            ),
            (
                "the transport path beneath another segment",
                format!("{base}/tools{TRANSPORT_PATH}"),
                false,
            ),
        ];

        for (row, path, served) in rows {
            let case = format!("{} / {row}", caller.case());
            let (status, answer) = mcp(&router, &host, &path, true).await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?} (POST {path}): the published MCP endpoint must be served, \
                     got {status} ({answer})"
                );
                assert_eq!(
                    answer["result"]["tools"],
                    json!([]),
                    "case {case:?} (POST {path}): a gateway with no target lists no tool, got {}",
                    answer["result"]
                );
            } else {
                assert_eq!(
                    status, 404,
                    "case {case:?} (POST {path}): this path publishes nothing, got {status} \
                     ({answer})"
                );
                assert_eq!(
                    text(&answer, "__type"),
                    "ResourceNotFoundException",
                    "case {case:?} (POST {path}): expected the not-found envelope, got {answer}"
                );
            }
        }

        // The gateway's AWS_IAM authorizer applies to every address its endpoint answers
        // on, so an unsigned request is refused at the joined path as at the published one.
        for row in [published.clone(), format!("{published}{TRANSPORT_PATH}")] {
            let case = format!("{} / unsigned {row}", caller.case());
            let (status, answer) = mcp(&router, &host, &row, false).await;
            assert_eq!(
                status, 403,
                "case {case:?}: an AWS_IAM gateway refuses a request carrying no credential, \
                 got {status} ({answer})"
            );
            assert_eq!(
                text(&answer, "__type"),
                "AccessDeniedException",
                "case {case:?}: expected the access-denied envelope, got {answer}"
            );
        }
    }
}
