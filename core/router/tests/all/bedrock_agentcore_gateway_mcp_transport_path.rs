// SPDX-License-Identifier: CC0-1.0

//! The address a gateway's published MCP endpoint answers on, across the services that own
//! the two halves: AgentCore Control publishes the endpoint and holds the gateway's
//! targets, Lambda owns the function a target's tool is backed by.
//!
//! RULE: the address a gateway publishes in `gatewayUrl` is an address this emulator SERVES
//! that gateway's protocol on, and joining the protocol's transport path onto it is
//! IDEMPOTENT — the published URL already ends in the transport path, so a client that
//! appends it again (the usual `url.rstrip('/') + '/mcp'` join), or appends a separator,
//! reaches the same endpoint. Every other path under that address publishes nothing and is
//! refused.
//!
//! The rows span the two dimensions the rule generalises over:
//!
//! * WHO asked — a client that resolves AWS's own hostnames, which is published the
//!   gateway's own endpoint host, and a client on the machine this emulator runs on, which
//!   resolves nothing but the endpoint it is already talking to and is published that
//!   endpoint with the gateway named in the path;
//! * HOW the transport path is joined — the published path as handed back, with a trailing
//!   separator, with repeated trailing separators, with the transport path appended once and
//!   twice, and — on the refusing side
//!   — the address without its transport path, a segment that merely BEGINS with the
//!   transport path's text, a sub-resource beneath it, and the transport path beneath
//!   another segment.
//!
//! Each served address is shown to be the SAME endpoint, by the tool listing it answers
//! with, and to apply the gateway's `AWS_IAM` authorizer to an unsigned request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the Region a gateway's own endpoint
/// host carries.
const REGION: &str = "us-west-2";

/// A role ARN inside `RoleArn`'s pattern, for both services.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/mcp-exec";

/// A deployment package as `Code.ZipFile` carries it: base64 in a `restJson1` JSON body.
const ZIP_FILE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// The address a client on this machine reaches the emulator at — a loopback authority,
/// which resolves no service and is therefore the emulator's own endpoint.
const EMULATOR_AUTHORITY: &str = "127.0.0.1:9297";

/// The transport path an MCP gateway's endpoint is served on, which its published
/// `gatewayUrl` ends in.
const TRANSPORT_PATH: &str = "/mcp";

/// How long a clock-owed transition is waited OUT for, polling for the status rather than
/// sleeping a span: generous, because nothing here is timing the transition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often the status is looked at again while waiting for it.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The status a gateway and a target each reach once their creation has settled
/// (`GatewayStatus` / `TargetStatus`, `smithy_query operation=GetGateway,GetGatewayTarget`).
const READY: &str = "READY";

/// The gateway's own target and tool, and the qualified name the gateway aggregates them
/// under (`<targetName>___<toolName>`).
const TARGET_NAME: &str = "writer";
/// See [`TARGET_NAME`].
const TOOL_NAME: &str = "simparityPutObject";

type Control =
    service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService<
        engine::InMemoryEngine,
    >;
type Data = service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore::EmulatedService<
    engine::InMemoryEngine,
>;
type Lambda =
    service_com_amazonaws_lambda_awsgirapiservice::EmulatedService<engine::InMemoryEngine>;

/// The three services over ONE shared engine, as `router/src/lib.rs` mounts them: the two
/// that share the AgentCore signing name, and the Lambda a gateway target resolves against.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(Control::new(Arc::clone(&engine)));
    router.register_service(Data::new(Arc::clone(&engine)));
    router.register_service(Lambda::new(engine));
    router
}

/// Who is calling: the two kinds of client a published address is composed for.
#[derive(Clone, Copy, Debug)]
enum Caller {
    /// A client that resolves AWS's own hostnames and addresses each service's endpoint.
    AwsEndpoint,
    /// A client on the machine this emulator runs on, which reaches the emulator's own
    /// endpoint and resolves nothing else.
    EmulatorEndpoint,
}

impl Caller {
    /// The host this caller sends a control-plane call to the service signing as
    /// `signing_name` with.
    fn host(self, signing_name: &str) -> String {
        match self {
            Self::AwsEndpoint => format!("{signing_name}.{REGION}.amazonaws.com"),
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The
/// signature is never checked: the router here runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One call to the service whose signing name is `signing_name`, at `uri`.
async fn call(
    router: &MultiServiceRouter,
    caller: Caller,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", caller.host(signing_name))
        .header("content-type", "application/json")
        .header("authorization", authorization(signing_name))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, body_json(response.body()))
}

/// The `(host, path)` a published URL addresses.
fn split_url(url: &str) -> (String, String) {
    let rest = url
        .strip_prefix("https://")
        .expect("the published gateway URL is an https URL");
    match rest.find('/') {
        Some(path_start) => (
            rest[..path_start].to_string(),
            rest[path_start..].to_string(),
        ),
        None => (rest.to_string(), "/".to_string()),
    }
}

/// One JSON-RPC call to `path` on a published gateway address, driven the way an MCP client
/// does — and, when `signed` is false, the way an unauthenticated one does, carrying no
/// Amazon Web Services credential at all.
async fn mcp(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    message: &Value,
    signed: bool,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-03-26");
    if signed {
        request = request.header("authorization", authorization("bedrock-agentcore"));
    }
    let request = request
        .body(Bytes::from(message.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, body_json(response.body()))
}

/// The JSON-RPC `tools/list` message an MCP client opens a tool listing with.
fn tools_list() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
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

/// Create a Lambda function and return the ARN Lambda minted for it.
async fn create_function(router: &MultiServiceRouter, caller: Caller, name: &str) -> String {
    let (status, created) = call(
        router,
        caller,
        "lambda",
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": ROLE_ARN,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": ZIP_FILE},
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected HTTP 201 from Lambda CreateFunction, got {status} ({created})"
    );
    text(&created, "FunctionArn")
}

/// Create a gateway, wait its clock-owed transition out, and return the `gatewayUrl` it
/// published together with its `gatewayId`.
async fn ready_gateway(
    router: &MultiServiceRouter,
    caller: Caller,
    name: &str,
) -> (String, String) {
    let (status, created) = call(
        router,
        caller,
        "bedrock-agentcore",
        "POST",
        "/gateways/",
        json!({
            "name": name,
            "roleArn": ROLE_ARN,
            "protocolType": "MCP",
            "authorizerType": "AWS_IAM",
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {name:?}: expected HTTP 202 from CreateGateway, got {status} ({created})"
    );
    let gateway_id = text(&created, "gatewayId");
    let url = text(&created, "gatewayUrl");
    poll_until_ready(
        router,
        caller,
        &format!("GetGateway {gateway_id}"),
        &format!("/gateways/{gateway_id}/"),
    )
    .await;
    (gateway_id, url)
}

/// Create an MCP Lambda target on `gateway_id` declaring one tool, and wait its clock-owed
/// transition out.
async fn ready_target(
    router: &MultiServiceRouter,
    caller: Caller,
    gateway_id: &str,
    lambda_arn: &str,
) {
    let (status, created) = call(
        router,
        caller,
        "bedrock-agentcore",
        "POST",
        &format!("/gateways/{gateway_id}/targets/"),
        json!({
            "name": TARGET_NAME,
            "targetConfiguration": {"mcp": {"lambda": {
                "lambdaArn": lambda_arn,
                "toolSchema": {"inlinePayload": [{
                    "name": TOOL_NAME,
                    "description": "write an object to a bucket",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"key": {"type": "string"}, "body": {"type": "string"}},
                        "required": ["key", "body"]
                    }
                }]}
            }}},
            "credentialProviderConfigurations": [{"credentialProviderType": "GATEWAY_IAM_ROLE"}],
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {TARGET_NAME:?}: expected HTTP 202 from CreateGatewayTarget, got {status} \
         ({created})"
    );
    let target_id = text(&created, "targetId");
    poll_until_ready(
        router,
        caller,
        &format!("GetGatewayTarget {target_id}"),
        &format!("/gateways/{gateway_id}/targets/{target_id}/"),
    )
    .await;
}

/// Read `uri` until its `status` is [`READY`], or fail once [`SETTLE_DEADLINE`] has
/// passed: the condition is waited FOR, never a span slept.
async fn poll_until_ready(
    router: &MultiServiceRouter,
    caller: Caller,
    case: &str,
    uri: &str,
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, read) = call(
            router,
            caller,
            "bedrock-agentcore",
            "GET",
            uri,
            Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading {uri}, got {status} ({read})"
        );
        let reported = text(&read, "status");
        if reported == READY {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {READY} from {uri}, last reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The tool names a `tools/list` answer reports.
fn tool_names(answer: &Value) -> Vec<String> {
    answer["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tool| text(tool, "name"))
        .collect()
}

/// One gateway with one `READY` target, created by `caller`, as `(host, address base,
/// published path)`: the base is the published path without its trailing transport path,
/// which every row below is composed from.
async fn published_endpoint(
    router: &MultiServiceRouter,
    caller: Caller,
    name: &str,
) -> (String, String, String) {
    let lambda_arn = create_function(router, caller, &format!("{name}-fn")).await;
    let (gateway_id, url) = ready_gateway(router, caller, name).await;
    ready_target(router, caller, &gateway_id, &lambda_arn).await;

    let (host, path) = split_url(&url);
    // The address a gateway publishes is one this emulator answers on for the client that
    // asked, and it ends in the transport path of the protocol the gateway speaks.
    match caller {
        Caller::AwsEndpoint => assert_eq!(
            host,
            format!("{gateway_id}.gateway.bedrock-agentcore.{REGION}.amazonaws.com"),
            "case {:?}: expected the gateway's own endpoint host, got {host:?}",
            caller.case()
        ),
        Caller::EmulatorEndpoint => assert_eq!(
            host, EMULATOR_AUTHORITY,
            "case {:?}: expected the emulator's own endpoint, got {host:?}",
            caller.case()
        ),
    }
    let base = path
        .strip_suffix(TRANSPORT_PATH)
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the published path must end in the transport path \
                 {TRANSPORT_PATH}, got {path:?}",
                caller.case()
            )
        })
        .to_string();
    (host, base, path)
}

/// RULE: every address formed by joining the transport path onto the published gateway URL
/// is the one MCP endpoint, for both kinds of client; every other path under that address
/// is refused.
#[tokio::test]
async fn joining_the_transport_path_onto_the_published_url_reaches_the_same_endpoint() {
    let qualified = format!("{TARGET_NAME}___{TOOL_NAME}");
    for (caller, name) in [
        (Caller::AwsEndpoint, "awspathgateway"),
        (Caller::EmulatorEndpoint, "ownpathgateway"),
    ] {
        let router = router();
        let (host, base, published) = published_endpoint(&router, caller, name).await;

        // (case, path, served)
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
            let (status, answer) = mcp(&router, &host, &path, &tools_list(), true).await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?} (POST {path}): the published MCP endpoint must be served, \
                     got {status} ({answer})"
                );
                assert_eq!(
                    tool_names(&answer),
                    vec![qualified.clone()],
                    "case {case:?} (POST {path}): expected the gateway's one aggregated tool, \
                     got {}",
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
    }
}

/// RULE, on the authorizer: the gateway's `AWS_IAM` authorizer applies to every address its
/// endpoint answers on, so an unsigned request is refused at a joined path exactly as at
/// the published one.
#[tokio::test]
async fn an_unsigned_request_is_refused_at_every_address_of_the_endpoint() {
    for (caller, name) in [
        (Caller::AwsEndpoint, "awsunsignedgw"),
        (Caller::EmulatorEndpoint, "ownunsignedgw"),
    ] {
        let router = router();
        let (host, _, published) = published_endpoint(&router, caller, name).await;

        let rows: [(&str, String); 3] = [
            (
                "the published URL as it was handed back",
                published.clone(),
            ),
            (
                "the transport path joined onto the published URL",
                format!("{published}{TRANSPORT_PATH}"),
            ),
            (
                "the joined path with a trailing separator",
                format!("{published}{TRANSPORT_PATH}/"),
            ),
        ];

        for (row, path) in rows {
            let case = format!("{} / {row}", caller.case());
            let (status, answer) = mcp(&router, &host, &path, &tools_list(), false).await;
            assert_eq!(
                status, 403,
                "case {case:?} (unsigned POST {path}): an AWS_IAM gateway refuses a request \
                 carrying no credential, got {status} ({answer})"
            );
            assert_eq!(
                text(&answer, "__type"),
                "AccessDeniedException",
                "case {case:?} (unsigned POST {path}): expected the access-denied envelope, \
                 got {answer}"
            );
        }
    }
}
