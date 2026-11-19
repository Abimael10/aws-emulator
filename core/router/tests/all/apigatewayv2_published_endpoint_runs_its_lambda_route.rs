// SPDX-License-Identifier: CC0-1.0

//! What a client that HONOURS the `ApiEndpoint` an HTTP api publishes is served, when that
//! api's route is RUN by an AWS Lambda function.
//!
//! RULE: **an api's `ApiEndpoint` is an address THIS emulator serves that api's data plane
//! at, settled by the operation that mints the api and reported unchanged by every later
//! reporter — the endpoint the minting request itself reached, with the api behind the
//! reserved data-plane path, for a client that resolves nothing but the emulator's own
//! endpoint, and the api's own
//! `{api-id}.execute-api.{region}.amazonaws.com` host for a client that resolves AWS's own
//! hostnames — so the invoke URL a client builds from the published value (that value with
//! the stage name appended, the reserved stage that serves the api's root being reached with
//! no stage segment at all) is resolved all the way to the route's own backend: an
//! `AWS_PROXY` route's function RUNS and the client is answered the object it returned, and
//! where the function's resource policy admits the gateway nothing, the SAME published
//! address is answered the gateway's own `500` `{"message":"Internal Server Error"}`.**
//!
//! `Api$ApiEndpoint` is "the URI of the API, of the form
//! `{api-id}.execute-api.{region}.amazonaws.com`. The stage name is typically appended to
//! this URI to form a complete path to a deployed API stage", and
//! `Integration$IntegrationType` gives `AWS_PROXY` as "for integrating the route or method
//! request with the Lambda function-invoking action".
//!
//! Four crates have to agree for that to hold — CloudFormation archives the inline handler
//! into a deployment package this emulator can run, IAM holds the execution role the template
//! names, Lambda holds the function and its resource policy and RUNS the handler, and API
//! Gateway v2 mints the api, publishes the address and resolves a request on it to the route
//! and its integration — so the suite lives here rather than in any one crate. The API
//! Gateway service that owns REST apis is mounted beside them, because the `execute-api`
//! endpoint space is SHARED by the two data planes: an address that resolves at all is one
//! exactly one of them claimed.
//!
//! Nothing here reads whether the function ran off an emulator internal: the answer a served
//! row asserts is the object the handler itself returned — its own status, its own header and
//! its own body — which the gateway has no way to produce without invoking it.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane client was
//! configured with (an IPv4 loopback literal with a port, a loopback NAME with a port, a
//! bracketed IPv6 literal with a port, and one of AWS's own service hosts), the stage the api
//! is deployed to (the reserved stage that serves the api's root, and named ones), the form of
//! the invoke URL built from the published value, the reporter the value is read from
//! (`CreateApi`, `GetApi`, `GetApis`), the endpoint the LATER reporter was reached at, and
//! whether the route's function grants the gateway the invocation at all. The recorded case —
//! an HTTP api created and read back by a client pointed at the emulator's loopback endpoint,
//! whose `$default` stage was then invoked at the published value — is the first row.
//!
//! What the owning crates' own suites pin and this one does not restate: which policy shapes
//! admit a gateway's invocation
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/http_api_lambda_invoke_permission.rs`),
//! and the authority half of the published address against an `HTTP_PROXY` upstream
//! (`.../tests/all/http_api_published_invoke_address.rs`). What is added here is the LAMBDA
//! leg of the published address, on an emulator where the REST plane claims the same space.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the CloudFormation, IAM and Lambda
/// callers above address, and so the Region the apis of these rows live in.
const REGION: &str = "us-east-1";

/// The execution role the templates name, provisioned in IAM before the stack so the
/// declaration names a role that service really holds.
const ROLE_NAME: &str = "apigwv2-published-endpoint-exec";

/// The reserved stage "that serves an api's root" (`$default`), reached with no stage segment
/// in the path at all.
const ROOT_STAGE: &str = "$default";

/// The route every row deploys, and the path within the stage that matches it.
const ROUTE_KEY: &str = "GET /items";
const ROUTE_PATH: &str = "/items";

/// The status, the header and the body member the handler answers with when it RUNS, so a
/// served row is told from a refused one by the function's own answer and not by the
/// gateway's.
const SERVED: u16 = 201;
const RAN_HEADER: &str = "x-ran";
const RAN_MARKER: &str = "the-function";

/// The status and sentence an HTTP api answers for an integration that produced no response,
/// which is the class an invocation the function's policy does not admit falls into.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The front door's error code for a request NO service claimed. The defect these rows pin is
/// a published address nothing serves, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The service principal a gateway invokes a route's function as, the action that invocation
/// performs, and the statement id the grant is written under.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const STATEMENT_ID: &str = "apigateway-may-invoke";

/// How long a function is given to report itself invocable before the suite gives up on it.
/// The span is AWS Lambda's own and the wait is on that CONDITION; this is only the deadline
/// past which the suite stops looking, so it is generous enough for a loaded machine.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The handler every row fronts, inline in the template exactly as a synthesised app carries
/// one: it answers a status, a header and a body of its OWN, so an answer carrying them is one
/// the function produced.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 201,
        "headers": {"x-ran": "the-function", "Content-Type": "application/json"},
        "body": json.dumps({"ran": True, "path": event.get("rawPath")}),
    }
"#;

/// CloudFormation, API Gateway v2, the API Gateway service that owns REST apis, AWS Lambda and
/// IAM over ONE engine, as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published address
    // carries none, as a client of a published HTTP api endpoint sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One API Gateway v2 control-plane call, addressed at `host` — the endpoint the CLIENT
/// reached, which is what decides the address a minted api announces — as
/// `(status, parsed body)`.
async fn control(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/apigateway\
                 /aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// What a dialled address answered: its status, the front door's error code, the header the
/// handler sets and the body verbatim.
struct Answered {
    status: u16,
    error_type: Option<String>,
    ran_header: Option<String>,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Enough of the answer to identify it in an assertion message.
    fn summary(&self) -> String {
        format!(
            "{} {:?} (x-amzn-errortype: {:?})",
            self.status,
            self.text(),
            self.error_type
        )
    }
}

/// Send one request to `authority` at `path`, as a client of a published HTTP api endpoint
/// makes it: unsigned, carrying no credential of any kind.
///
/// The request is handed straight to the router IN PROCESS, so no socket is opened and nothing
/// leaves this test: the authority is only the address the request carries.
async fn invoke(router: &MultiServiceRouter, authority: &str, path: &str) -> Answered {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
    };
    let error_type = header("x-amzn-errortype");
    let ran_header = header(RAN_HEADER);
    Answered {
        status: response.status().as_u16(),
        error_type,
        ran_header,
        body: response.into_body(),
    }
}

/// The member `member` of `body`, as text.
fn as_str(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {body}"))
        .to_string()
}

/// The template one row's function is deployed as: the role IAM holds, the inline handler, and
/// a name the suite can poll the function's state by.
fn function_stack(function: &str) -> String {
    json!({
        "Resources": {
            "Handler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Handler", "Arn"] } },
        },
    })
    .to_string()
}

/// Create the execution role the templates name, where IAM holds it: trusted by Lambda, which
/// is what lets that service be handed the role at all.
async fn execution_role(router: &MultiServiceRouter) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `function` `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`). The span is that
/// service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, function: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let (_status, read) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if read.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} invocable, last read {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Deploy `function` as a one-resource stack and answer its ARN, once Lambda reports it
/// invocable. A template is what archives inline source into a deployment package this
/// emulator can run, which is how a handler written out here comes to be a function that RUNS.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
) -> String {
    cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &function_stack(function)),
        ],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the function's stack to deploy, got {status} with {described}"
    );
    let arn = outputs_of(&described)
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish a FunctionArn, got {described}")
        });
    wait_until_active(router, case, function).await;
    arn
}

/// Grant API Gateway the invocation of `function` with no narrowing — the `AddPermission` a
/// client makes beside the api it fronts a route with.
async fn gateway_may_invoke(router: &MultiServiceRouter, case: &str, function: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": STATEMENT_ID,
            "Action": INVOKE_ACTION,
            "Principal": GATEWAY_PRINCIPAL,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission answered {status} {granted}"
    );
}

/// One row: the endpoint the control-plane client was configured with, and the stage the api is
/// deployed to.
struct Row {
    case: &'static str,
    /// The `Host` every control-plane call of this row carries.
    host: &'static str,
    stage: &'static str,
    /// Whether that client resolves nothing but the endpoint it reached, which is the address
    /// the api is then announced on.
    reaches_only_the_emulator: bool,
}

/// The addressings these rows span: the three forms of this emulator's own endpoint a client on
/// the machine it runs on is configured with, and one of AWS's own service hosts.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint, deployed to the stage that serves the api's root",
            host: "127.0.0.1:45923",
            stage: ROOT_STAGE,
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by its loopback NAME, deployed to a named stage",
            host: "localhost:9297",
            stage: "prod",
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal, deployed to the stage \
                   that serves the api's root",
            host: "[::1]:9297",
            stage: ROOT_STAGE,
            reaches_only_the_emulator: true,
        },
        Row {
            case: "a client that resolves AWS's own hosts: this service's own endpoint host, \
                   deployed to a named stage",
            host: "apigateway.us-east-1.amazonaws.com",
            stage: "live",
            reaches_only_the_emulator: false,
        },
    ]
}

/// The announced endpoint split into the authority a client dials and the path it carries ahead
/// of the stage — empty for an endpoint that is a bare host.
///
/// `pub(crate)` because every suite that DIALS an announced address has to split it exactly this
/// way, and a second copy of the split is a second reading of what was announced: the sibling
/// that holds a CloudFormation-deployed api's published address to the same rule
/// ([`super::cloudformation_published_http_api_endpoint`]) reads it from here.
pub(crate) fn announced_parts(announced: &str) -> (String, String) {
    let without_scheme = announced
        .split_once("://")
        .map_or(announced, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), String::new()),
    }
}

/// The authority and base path the api of `api_id` must be ANNOUNCED on for the row's client: a
/// client that resolves nothing but the emulator's own endpoint is announced that endpoint with
/// the api behind the reserved data-plane path, and a client that resolves AWS's own hostnames
/// is announced the api's own documented `execute-api` host.
fn expected_announced_parts(row: &Row, api_id: &str) -> (String, String) {
    if row.reaches_only_the_emulator {
        (row.host.to_string(), format!("/_aws/execute-api/{api_id}"))
    } else {
        (
            format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
            String::new(),
        )
    }
}

/// Deploy the row's api at its own control-plane endpoint: an `AWS_PROXY` integration over
/// `function_arn`, the route that fronts it and an auto-deploying stage. Answers
/// `(ApiId, the ApiEndpoint CreateApi published)`.
async fn given_api(
    router: &MultiServiceRouter,
    row: &Row,
    name: &str,
    function_arn: &str,
) -> (String, String) {
    let case = row.case;
    let (status, api) = control(
        router,
        row.host,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {status} {api}"
    );
    let api_id = as_str(case, &api, "apiId");
    let announced = as_str(case, &api, "apiEndpoint");

    let (status, integration) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "payloadFormatVersion": "2.0",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateIntegration must answer 201, got {status} {integration}"
    );
    let integration_id = as_str(case, &integration, "integrationId");

    let (status, route) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}"),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {status} {route}"
    );

    let (status, stage) = control(
        router,
        row.host,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": row.stage, "autoDeploy": true })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {status} {stage}"
    );
    assert!(
        stage.get("deploymentId").and_then(Value::as_str).is_some(),
        "case {case:?}: an auto-deploying stage must carry a deployment, got {stage}"
    );

    (api_id, announced)
}

/// Assert `announced` is the address this emulator serves the row's api at, and answer the
/// authority and base path a client builds its invoke URL from.
fn assert_announced(row: &Row, reporter: &str, api_id: &str, announced: &str) -> (String, String) {
    let case = row.case;
    assert!(
        announced.starts_with("https://"),
        "case {case:?}: {reporter} must announce an https:// endpoint for an HTTP api, got \
         {announced:?}"
    );
    let (authority, base_path) = announced_parts(announced);
    let (expected_authority, expected_base_path) = expected_announced_parts(row, api_id);
    assert_eq!(
        authority, expected_authority,
        "case {case:?}: {reporter} must announce the api on {expected_authority}, got \
         {announced:?}"
    );
    assert_eq!(
        base_path, expected_base_path,
        "case {case:?}: {reporter} must announce the base path {expected_base_path:?}, got \
         {announced:?}"
    );
    (authority, base_path)
}

/// Every invoke URL a client builds from the row's published endpoint: the stage name appended,
/// and — for the reserved stage that serves the api's root — the endpoint on its own, which is
/// the "typically" of `Api$ApiEndpoint`.
fn invoke_forms(row: &Row, base_path: &str) -> Vec<(&'static str, String)> {
    let mut forms = vec![(
        "the published endpoint with the stage name appended",
        format!("{base_path}/{}{ROUTE_PATH}", row.stage),
    )];
    if row.stage == ROOT_STAGE {
        forms.push((
            "the published endpoint on its own, for the stage that serves the api's root",
            format!("{base_path}{ROUTE_PATH}"),
        ));
    }
    forms
}

/// RULE (the served half): the invoke URL a client builds from the published `ApiEndpoint` is
/// resolved all the way to the route's `AWS_PROXY` function, which RUNS and answers the client
/// with the object it returned — whichever endpoint the control-plane client was configured
/// with, and whichever reporter published the value.
#[tokio::test]
async fn the_published_api_endpoint_is_served_by_the_routes_own_lambda_function() {
    let router = emulator();
    execution_role(&router).await;
    let function = "published-endpoint-served";
    let arn = given_function(
        &router,
        "setup",
        "apigwv2-published-endpoint-served",
        function,
    )
    .await;
    gateway_may_invoke(&router, "setup", function).await;

    // Every row's api is minted on the SAME emulator, so the listing at the end reports them
    // all and no row can pass against an endpoint another row's api announced.
    let mut minted: Vec<(&'static str, String, String)> = Vec::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (api_id, announced) =
            given_api(&router, &row, &format!("published-lambda-{index}"), &arn).await;
        let (authority, base_path) = assert_announced(&row, "CreateApi", &api_id, &announced);

        // The value is settled by the operation that MINTED the api, so a later reporter
        // reports it unchanged — the one the row's own client reaches, and one reached at
        // another endpoint altogether.
        for (reporter, reached_at) in [
            ("GetApi at the endpoint the api was created through", row.host),
            ("GetApi at another endpoint", "apigateway.eu-west-1.amazonaws.com"),
        ] {
            let (status, fetched) =
                control(&router, reached_at, "GET", &format!("/v2/apis/{api_id}"), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {reporter} must answer 200, got {status} {fetched}"
            );
            assert_eq!(
                as_str(case, &fetched, "apiEndpoint"),
                announced,
                "case {case:?}: {reporter} must report the announced ApiEndpoint {announced:?}, \
                 got {fetched}"
            );
        }

        for (form, path) in invoke_forms(&row, &base_path) {
            let answered = invoke(&router, &authority, &path).await;
            assert_ne!(
                answered.error_type.as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {case:?}: the published address must be one this emulator serves, but \
                 {form} ({authority}{path}) is claimed by no service: {}",
                answered.summary()
            );
            assert_eq!(
                answered.status,
                SERVED,
                "case {case:?}: {form} ({authority}{path}) must be answered by the route's own \
                 function with {SERVED}, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.ran_header.as_deref(),
                Some(RAN_MARKER),
                "case {case:?}: {form} must be answered the header the function itself set, got \
                 {}",
                answered.summary()
            );
            let body = answered.json();
            assert_eq!(
                body.get("ran"),
                Some(&json!(true)),
                "case {case:?}: {form} must be answered the body the function itself returned, \
                 got {}",
                answered.summary()
            );
            let reached = body
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                reached.ends_with(ROUTE_PATH),
                "case {case:?}: {form} must reach the function on the route's own path \
                 {ROUTE_PATH:?}, got {reached:?} in {}",
                answered.summary()
            );
        }

        minted.push((case, api_id, announced));
    }

    // The third reporter: the listing publishes each api on the address that api was minted
    // with, so no reporter of one api drifts from another.
    let (status, listed) = control(
        &router,
        "apigateway.us-east-1.amazonaws.com",
        "GET",
        "/v2/apis",
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "GetApis must answer 200, got {status} {listed}"
    );
    let items = listed["items"]
        .as_array()
        .unwrap_or_else(|| panic!("GetApis served no items, got {listed}"))
        .clone();
    for (case, api_id, announced) in minted {
        let held = items
            .iter()
            .find(|item| item.get("apiId").and_then(Value::as_str) == Some(api_id.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: GetApis did not list {api_id}, got {listed}"));
        assert_eq!(
            as_str(case, held, "apiEndpoint"),
            announced,
            "case {case:?}: GetApis must list the announced ApiEndpoint {announced:?}, got {held}"
        );
    }
}

/// RULE (the refused half): the SAME published `ApiEndpoint` is answered the gateway's own
/// `500` `{"message":"Internal Server Error"}` where the route's function grants the gateway
/// nothing — an answer of the api's, not of a front door that claims the address for nobody.
#[tokio::test]
async fn the_published_api_endpoint_carries_the_gateways_refusal_for_an_ungranted_function() {
    let router = emulator();
    execution_role(&router).await;
    let function = "published-endpoint-ungranted";
    // No AddPermission follows: the function exists and is invocable, and its resource policy
    // admits nobody.
    let arn = given_function(
        &router,
        "setup",
        "apigwv2-published-endpoint-ungranted",
        function,
    )
    .await;

    // The two addressings that publish the two different forms of invoke address; the refusal
    // is the function's policy's and so is the same at either.
    let refused: Vec<Row> = rows()
        .into_iter()
        .filter(|row| row.stage == ROOT_STAGE || !row.reaches_only_the_emulator)
        .collect();
    for (index, row) in refused.into_iter().enumerate() {
        let case = row.case;
        let (api_id, announced) =
            given_api(&router, &row, &format!("published-ungranted-{index}"), &arn).await;
        let (authority, base_path) = assert_announced(&row, "CreateApi", &api_id, &announced);

        for (form, path) in invoke_forms(&row, &base_path) {
            let answered = invoke(&router, &authority, &path).await;
            assert_ne!(
                answered.error_type.as_deref(),
                Some(UNKNOWN_SERVICE),
                "case {case:?}: the published address must be one this emulator serves, but \
                 {form} ({authority}{path}) is claimed by no service: {}",
                answered.summary()
            );
            assert_eq!(
                answered.status,
                INTEGRATION_FAULT,
                "case {case:?}: {form} ({authority}{path}) must be answered the gateway's own \
                 {INTEGRATION_FAULT}, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.json(),
                json!({ "message": INTEGRATION_FAULT_MESSAGE }),
                "case {case:?}: {form} must carry the gateway's own sentence, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.ran_header, None,
                "case {case:?}: {form} must not be answered the function's own header, got {}",
                answered.summary()
            );
        }
    }
}
