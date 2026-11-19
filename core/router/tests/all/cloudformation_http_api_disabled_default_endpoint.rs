// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed HTTP api's DEFAULT `execute-api` endpoint serves while that
//! api's `DisableExecuteApiEndpoint` is set, and what it serves once a deployment clears it.
//!
//! The rule under test: **"To require that clients use a custom domain name to invoke your
//! API, disable the default endpoint" (`Api$DisableExecuteApiEndpoint`) — so while an api
//! carries that member set, its default `execute-api` endpoint serves NOTHING of the api, and
//! a request to it is answered exactly as that endpoint answers a request for an api it serves
//! none of, whatever stage, route or integration the address would otherwise have resolved: an
//! HTTP api is refused `404` with the gateway's own `{"message": "Not Found"}` body. The
//! member is a property of the ENDPOINT rather than of the configuration a stage's deployment
//! froze, so clearing it makes the same address serve again with no redeployment, and an api
//! of the same stack whose member is clear keeps serving throughout.** CloudFormation
//! provisions the apis and applies the changed member, and API Gateway v2's `execute-api` data
//! plane answers the requests — two crates, so the suite lives here rather than in either of
//! them.
//!
//! The rows differ along what the rule generalises over: how the api was declared (quick
//! created from a `Target`, and with its integration, route, named stage and deployment
//! declared one by one), which stage serves the request (the one that serves the api's root,
//! reached with no stage segment, and a named stage reached by its own first segment), the
//! addressing (the host `Api$ApiEndpoint` documents, and this emulator's own rewriting of it
//! with the api id in the path), where the member's value came from (the template the stack
//! was created from, and the one it was updated to) and both directions of the transition. The
//! disabled api's answer is also compared, status and body alike, against the same endpoint
//! addressed for an api id nothing ever created.
//!
//! This suite OWNS the reading of that answer, and the wire it drives the two services over:
//! the sibling that holds a stack's PUBLISHED `ApiEndpoint` to the address this emulator serves
//! ([`super::cloudformation_published_http_api_endpoint`]) reads the mounting, the two
//! control-plane callers, the invoke and both assertions from here rather than wording a second
//! copy of any of them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request below is signed for, and the one every endpoint host names.
pub(crate) const REGION: &str = "us-east-1";

/// The terminal statuses the two stack operations are waited for.
pub(crate) const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The refusal an HTTP api's `execute-api` endpoint gives a request it serves nothing for,
/// which is what an api that turned its default endpoint off is on that endpoint.
const NOT_FOUND: u16 = 404;
const NOT_FOUND_MESSAGE: &str = "Not Found";

/// What a SERVED request is answered with here: each api's route fronts an `HTTP_PROXY`
/// upstream, which names "an endpoint over the network" this emulator opens no connection to,
/// so the caller is sent there with the redirect that preserves its method and body.
const TEMPORARY_REDIRECT: u16 = 307;

/// The header that redirect carries the upstream in.
const LOCATION_HEADER: &str = "location";

/// The endpoint a client on this machine resolves, which is where the emulator's own rewriting
/// of an `execute-api` endpoint is addressed.
const LOCAL_ENDPOINT: &str = "localhost:9297";

/// The marker that rewriting carries the endpoint behind.
pub(crate) const LOCAL_ENDPOINT_PREFIX: &str = "/_aws/execute-api";

/// An api id of the shape API Gateway mints that nothing in this suite ever creates, so its
/// `execute-api` endpoint serves nothing — the answer a disabled default endpoint has to be
/// indistinguishable from.
const UNHELD_API_ID: &str = "aaaaaaaaaa";

/// How long to wait between polls of a clock-owed transition, and how many polls to make.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 400;

/// One answered request: its status, its headers and its body verbatim.
pub(crate) struct Answered {
    pub(crate) status: u16,
    headers: BTreeMap<String, String>,
    body: Bytes,
}

impl Answered {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the stack's apis are held by the same
/// router the requests below reach.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and a request to a published endpoint
    // carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
pub(crate) fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
pub(crate) async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect(),
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
pub(crate) async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One API Gateway v2 control-plane call over its `restJson1` wire, at that service's own
/// endpoint host.
pub(crate) async fn apigatewayv2(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    apigatewayv2_at(
        router,
        &format!("apigateway.{REGION}.amazonaws.com"),
        method,
        path,
    )
    .await
}

/// The same call addressed at `host`, for a reader that reaches the control plane at an
/// endpoint OTHER than that service's own — the emulator's own endpoint, say. One spelling of
/// the wire for both, so a suite that varies the endpoint varies nothing else.
pub(crate) async fn apigatewayv2_at(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
) -> Answered {
    dispatch(
        router,
        method,
        host,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::new(),
    )
    .await
}

/// One api the stack declares: how it is declared, the upstream its route fronts, and the path
/// within the api's endpoint that reaches that route.
struct Declared {
    case: &'static str,
    /// The `Outputs` entry its `ApiId` is published under.
    output: &'static str,
    /// The `IntegrationUri` its route's integration names, which is what a served request is
    /// redirected to — one per api, so a served answer names WHICH api served it.
    upstream: &'static str,
    /// The path a client addresses the api's own endpoint at to reach its route: a named stage
    /// is reached with its name as the first segment, and the stage that serves the api's root
    /// with no stage segment at all.
    invoke_path: &'static str,
}

/// The upstream each api's route fronts.
const QUICK_UPSTREAM: &str = "https://upstream.example.test/quick";
const TWIN_UPSTREAM: &str = "https://upstream.example.test/twin";
const STAGED_UPSTREAM: &str = "https://upstream.example.test/staged";

/// The named stage the third api declares, which is reached by its own first path segment.
const NAMED_STAGE: &str = "prod";

/// The route path that stage's route matches.
const STAGED_ROUTE_PATH: &str = "/orders";

const DECLARED: &[Declared] = &[
    Declared {
        case: "an api quick-created from a Target, its $default stage reached with no stage \
               segment",
        output: "QuickApiId",
        upstream: QUICK_UPSTREAM,
        invoke_path: "/",
    },
    Declared {
        case: "its twin in the same stack, quick-created the same way",
        output: "TwinApiId",
        upstream: TWIN_UPSTREAM,
        invoke_path: "/",
    },
    Declared {
        case: "an api whose integration, route, named stage and deployment the template \
               declares one by one",
        output: "StagedApiId",
        upstream: STAGED_UPSTREAM,
        invoke_path: "/prod/orders",
    },
];

/// Whether each api of [`DECLARED`] turns its default endpoint off, in the template the stack
/// is CREATED from and in the one it is then UPDATED to — so every api is driven through both
/// states, and each state is held by an api of the same stack at the same instant.
const CREATED_WITH: [bool; 3] = [true, false, true];
const UPDATED_TO: [bool; 3] = [false, true, false];

/// The template the stack deploys, with each api's `DisableExecuteApiEndpoint` as `disabled`
/// states it — the ONE declaration that changes between the create and the update.
fn template(disabled: [bool; 3]) -> String {
    json!({
        "Resources": {
            "QuickApi": {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": "quick-api",
                    "ProtocolType": "HTTP",
                    "Target": QUICK_UPSTREAM,
                    "RouteKey": "ANY /",
                    "DisableExecuteApiEndpoint": disabled[0]
                }
            },
            "TwinApi": {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": "twin-api",
                    "ProtocolType": "HTTP",
                    "Target": TWIN_UPSTREAM,
                    "RouteKey": "ANY /",
                    "DisableExecuteApiEndpoint": disabled[1]
                }
            },
            "StagedApi": {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": "staged-api",
                    "ProtocolType": "HTTP",
                    "DisableExecuteApiEndpoint": disabled[2]
                }
            },
            "StagedIntegration": {
                "Type": "AWS::ApiGatewayV2::Integration",
                "Properties": {
                    "ApiId": { "Ref": "StagedApi" },
                    "IntegrationType": "HTTP_PROXY",
                    "IntegrationMethod": "GET",
                    "IntegrationUri": STAGED_UPSTREAM,
                    "PayloadFormatVersion": "1.0"
                }
            },
            "StagedRoute": {
                "Type": "AWS::ApiGatewayV2::Route",
                "Properties": {
                    "ApiId": { "Ref": "StagedApi" },
                    "RouteKey": format!("GET {STAGED_ROUTE_PATH}"),
                    "Target": {
                        "Fn::Join": ["/", ["integrations", { "Ref": "StagedIntegration" }]]
                    }
                }
            },
            "StagedStage": {
                "Type": "AWS::ApiGatewayV2::Stage",
                "Properties": {
                    "ApiId": { "Ref": "StagedApi" },
                    "StageName": NAMED_STAGE,
                    "AutoDeploy": true
                }
            },
            "StagedDeployment": {
                "Type": "AWS::ApiGatewayV2::Deployment",
                "DependsOn": ["StagedRoute", "StagedStage"],
                "Properties": {
                    "ApiId": { "Ref": "StagedApi" },
                    "StageName": { "Ref": "StagedStage" }
                }
            }
        },
        "Outputs": {
            "QuickApiId": { "Value": { "Ref": "QuickApi" } },
            "TwinApiId": { "Value": { "Ref": "TwinApi" } },
            "StagedApiId": { "Value": { "Ref": "StagedApi" } }
        }
    })
    .to_string()
}

/// The two ways a client addresses an api's DEFAULT `execute-api` endpoint: the host
/// `Api$ApiEndpoint` documents (`{api-id}.execute-api.{region}.amazonaws.com`), and this
/// emulator's own rewriting of it, which carries the endpoint in the path for a client that
/// resolves no AWS host.
fn addressings(api_id: &str, invoke_path: &str) -> Vec<(&'static str, String, String)> {
    vec![
        (
            "the api's own execute-api host",
            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
            invoke_path.to_string(),
        ),
        (
            "the endpoint this emulator announces, the api id in the path",
            LOCAL_ENDPOINT.to_string(),
            format!("{LOCAL_ENDPOINT_PREFIX}/{api_id}{invoke_path}"),
        ),
    ]
}

/// Request one address of an api's default endpoint, as a client of a published endpoint does:
/// no credential, no body.
pub(crate) async fn request(router: &MultiServiceRouter, host: &str, path: &str) -> Answered {
    dispatch(router, "GET", host, path, &[], Bytes::new()).await
}

/// Assert this answer is the one an endpoint that serves NOTHING gives: `404` with the
/// gateway's own `{"message": "Not Found"}` body, and nothing of the route it would otherwise
/// have resolved — no redirect to any upstream.
pub(crate) fn assert_serves_nothing(case: &str, answered: &Answered) {
    assert_eq!(
        answered.status, NOT_FOUND,
        "case {case:?}: expected HTTP {NOT_FOUND} on a disabled default endpoint, got {} with \
         {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.json(),
        json!({ "message": NOT_FOUND_MESSAGE }),
        "case {case:?}: expected the gateway's own {{\"message\": {NOT_FOUND_MESSAGE:?}}} body, \
         got {:?}",
        answered.text()
    );
    assert_eq!(
        answered.header(LOCATION_HEADER),
        None,
        "case {case:?}: a disabled default endpoint reaches no integration, got a \
         {LOCATION_HEADER} of {:?}",
        answered.header(LOCATION_HEADER)
    );
}

/// Assert this answer is the route's own: the redirect to the upstream that api's integration
/// names, which is also what tells one api's answer from another's.
pub(crate) fn assert_serves_its_route(case: &str, upstream: &str, answered: &Answered) {
    assert_eq!(
        answered.status, TEMPORARY_REDIRECT,
        "case {case:?}: expected HTTP {TEMPORARY_REDIRECT} from the route's upstream, got {} \
         with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header(LOCATION_HEADER),
        Some(upstream),
        "case {case:?}: expected the upstream {upstream:?} in {LOCATION_HEADER}, got {:?}",
        answered.header(LOCATION_HEADER)
    );
}

/// Read the `Outputs` of the stack once it has settled, with the answer they were read from.
async fn stack_outputs(
    router: &MultiServiceRouter,
    phase: &str,
    stack: &str,
    wanted: &str,
) -> BTreeMap<String, String> {
    let (reached, described) = wait_for_stack_status(router, stack, wanted).await;
    assert_eq!(
        reached, wanted,
        "{phase}: expected the stack to reach {wanted}, got {described}"
    );
    outputs_of(&described)
}

/// The api ids the stack publishes, in the order of [`DECLARED`].
fn published_ids(phase: &str, outputs: &BTreeMap<String, String>) -> Vec<String> {
    DECLARED
        .iter()
        .map(|declared| {
            outputs
                .get(declared.output)
                .filter(|id| !id.is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "{phase}: expected the stack to publish a {} output, got {outputs:?}",
                        declared.output
                    )
                })
        })
        .collect()
}

/// Poll `GetApi` until API Gateway v2 reports `wanted` for the api's own
/// `disableExecuteApiEndpoint`, and answer whether it ever did with the last record it read.
///
/// A stack operation is complete once the services that own its resources hold what it
/// declared, which only a read asks them about — so the wait is on that CONDITION rather than
/// on a span.
async fn wait_until_member_is(
    router: &MultiServiceRouter,
    api_id: &str,
    wanted: bool,
) -> (bool, String) {
    let mut last = String::new();
    for _ in 0..POLLS {
        let read = apigatewayv2(router, "GET", &format!("/v2/apis/{api_id}")).await;
        last = read.text();
        if read.status == 200
            && read
                .json()
                .get("disableExecuteApiEndpoint")
                .and_then(Value::as_bool)
                == Some(wanted)
        {
            return (true, last);
        }
        tokio::time::sleep(POLL).await;
    }
    (false, last)
}

/// Every api of the stack, at every addressing of its default endpoint, answered as the member
/// the api now carries says it must be.
async fn assert_every_endpoint(
    router: &MultiServiceRouter,
    phase: &str,
    api_ids: &[String],
    disabled: [bool; 3],
) {
    for (index, declared) in DECLARED.iter().enumerate() {
        let api_id = &api_ids[index];
        let turned_off = disabled[index];

        // The control leg: the api really carries the member the template declared, read back
        // through API Gateway v2's own `GetApi`.
        let (reported, record) = wait_until_member_is(router, api_id, turned_off).await;
        assert!(
            reported,
            "{phase}, case {:?}: expected GetApi to report disableExecuteApiEndpoint \
             {turned_off}, got {record}",
            declared.case
        );

        for (addressing, host, path) in addressings(api_id, declared.invoke_path) {
            let case = format!(
                "{phase}, {} [{addressing}, DisableExecuteApiEndpoint {turned_off}]",
                declared.case
            );
            let answered = request(router, &host, &path).await;
            if turned_off {
                assert_serves_nothing(&case, &answered);
            } else {
                assert_serves_its_route(&case, declared.upstream, &answered);
            }
        }
    }
}

/// The whole point of the member: a client probing the default endpoint of a disabled api
/// cannot tell it from an api id the `execute-api` endpoint space serves nothing for, so the
/// two answers are compared status and body alike, at both addressings.
async fn assert_indistinguishable_from_an_api_nothing_holds(
    router: &MultiServiceRouter,
    phase: &str,
    api_ids: &[String],
    index: usize,
) {
    let api_id = &api_ids[index];
    let invoke_path = DECLARED[index].invoke_path;
    // The premise of the comparison: the id the twin is compared against is one the stack
    // never published, so the endpoint addressed for it really serves no api of this emulator.
    assert!(
        !api_ids.iter().any(|published| published == UNHELD_API_ID),
        "{phase}: {UNHELD_API_ID:?} must be an api id nothing created, but the stack published \
         it; got {api_ids:?}"
    );
    let disabled = addressings(api_id, invoke_path);
    let unheld = addressings(UNHELD_API_ID, invoke_path);
    for ((addressing, host, path), (_, unheld_host, unheld_path)) in
        disabled.into_iter().zip(unheld)
    {
        let case = format!("{phase}, an api id nothing ever created [{addressing}]");
        let refused = request(router, &host, &path).await;
        let unserved = request(router, &unheld_host, &unheld_path).await;
        assert_serves_nothing(&case, &unserved);
        assert_eq!(
            (refused.status, refused.text()),
            (unserved.status, unserved.text()),
            "case {case:?}: a disabled default endpoint must answer exactly as an endpoint \
             that serves no api, got {} {:?} against {} {:?}",
            refused.status,
            refused.text(),
            unserved.status,
            unserved.text()
        );
    }
}

// The emulator boots a multi-thread runtime, and the data plane performs a route's integration
// as a task of the runtime the request is served on, so the suite drives the router on that
// same flavour.
#[tokio::test(flavor = "multi_thread")]
async fn a_stacks_http_api_serves_nothing_on_its_default_endpoint_while_the_member_is_set() {
    let router = emulator();
    let stack = "http-api-disabled-default-endpoint";
    // The third api's address is its named stage and its route's own path, so the constants the
    // template declares them with and the path the rows address are held to each other.
    assert_eq!(
        DECLARED[2].invoke_path,
        format!("/{NAMED_STAGE}{STAGED_ROUTE_PATH}"),
        "setup: the staged api's invoke path must be its declared stage and route path"
    );

    // 1. The stack as CREATED: two quick-created apis differing only in the member, and a
    // third whose integration, route, named stage and deployment the template declares.
    let created_body = template(CREATED_WITH);
    let created = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &created_body)],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let phase = "the stack as created";
    let outputs = stack_outputs(&router, phase, stack, CREATE_COMPLETE).await;
    let api_ids = published_ids(phase, &outputs);

    assert_every_endpoint(&router, phase, &api_ids, CREATED_WITH).await;
    // The disabled api of this phase, at the addressings an api id travels in.
    assert_indistinguishable_from_an_api_nothing_holds(&router, phase, &api_ids, 0).await;

    // 2. The same stack UPDATED, with every api's member stated the other way round and
    // nothing else changed: the member is no part of the deployment a stage froze, so what
    // each endpoint serves follows it with no stage redeployed and no api replaced.
    let updated_body = template(UPDATED_TO);
    let updated = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &updated_body)],
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "setup: expected HTTP 200 from UpdateStack, got {} with {:?}",
        updated.status,
        updated.text()
    );
    let phase = "the stack as updated";
    let outputs = stack_outputs(&router, phase, stack, UPDATE_COMPLETE).await;
    let updated_ids = published_ids(phase, &outputs);
    assert_eq!(
        updated_ids, api_ids,
        "{phase}: the member is no create-only property, so the stack must keep the apis it \
         had; got {updated_ids:?} against {api_ids:?}"
    );

    assert_every_endpoint(&router, phase, &api_ids, UPDATED_TO).await;
    // The api this phase turned off is the twin that served throughout the first one.
    assert_indistinguishable_from_an_api_nothing_holds(&router, phase, &api_ids, 1).await;
}
