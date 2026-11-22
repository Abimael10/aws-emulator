// SPDX-License-Identifier: CC0-1.0

//! The grant that admits a deployed HTTP api's `AWS_PROXY` route is whatever the LATEST
//! deployment of the stack left on the function, in both directions — end to end over the
//! assembled emulator, from the template to the handler's own work product.
//!
//! The rule pinned here: **an `AWS::Lambda::Permission` is one statement of the named
//! function's resource-based policy, so a deployment that declares it adds that statement and
//! one that stops declaring it removes it; a gateway invokes an `AWS_PROXY` route's function as
//! its own service principal (`apigateway.amazonaws.com`), so every request after a deployment
//! is admitted or refused by the statements that deployment left — an invocation no statement
//! admits is never made, the deployment package does not run and the route is answered `500`
//! `{"message":"Internal Server Error"}` (the status an HTTP api maps an integration that
//! produced no response to), while an admitted one is answered the function's own response —
//! with nothing else about the api, the stage, the route or the function redeployed in either
//! direction.**
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the queue, the
//! function, the api with its integration, route and auto-deployed stage and the statement;
//! API Gateway v2 resolves the stage and the route and decides the invocation against the
//! function's policy; Lambda holds that policy and RUNS the handler; Amazon SQS holds what the
//! handler wrote; IAM holds the execution role the handler's own write is judged against — so
//! the suite lives here rather than in any one crate. Whether the function RAN is read where a
//! client reads it: off the queue the handler writes to, never off an emulator internal, so a
//! refused phase asserts that the package never ran rather than merely that the route answered
//! `500` — and is read against a positive control, AWS Lambda's own `Invoke` of the same
//! package with the same event, so a refusal is the grant's and not a function that cannot run.
//!
//! The two tests differ along exactly what the rule generalises over: the DIRECTION the one
//! deployment moves the grant in — a stack deployed without the statement whose next
//! deployment declares it, and a stack deployed with the statement whose next deployment stops
//! declaring it. Every expected value is computed from the api id the stack published.
//!
//! Which policy shapes admit the invocation at all, and the same boundary over stacks that
//! declare the statement differently from the start, are
//! [`crate::cloudformation_http_api_lambda_invoke_permission`]; only the stack update is
//! varied here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, stayed_quiet_about, trust_policy, wait_for, ACCOUNT,
    CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for and every ARN below names — the one
/// `wait_for_stack_status` polls in.
const REGION: &str = "us-east-1";

/// The `StackStatus` a deployment of an existing stack reaches.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The service principal an api invokes a route's function as, which is what the declared
/// statement names, and the action such an invocation performs.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The execution role the declared function is created with, provisioned in IAM before the
/// stack so the template names a role that service really holds.
const ROLE_NAME: &str = "httpapi-permission-repair-exec";

/// How long the function is given to report itself ready before the suite gives up on it. The
/// span is AWS Lambda's own and the wait is on that CONDITION; this is only the deadline past
/// which the suite stops looking, so it is generous enough for a loaded machine.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The method and path every request below is made with: the api's one route is `$default`,
/// which catches everything, and the `$default` stage serves the api's root.
const REQUEST_METHOD: &str = "GET";
const REQUEST_PATH: &str = "/";

/// The status the handler answers when it runs, and the status and sentence an HTTP api
/// answers for an integration that produced no response — the class an invocation the
/// function's policy does not admit falls into.
const SERVED: u16 = 200;
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// How many consecutive requests a refused phase makes, so the refusal is asserted to be
/// stable rather than a single answer.
const REFUSED_ATTEMPTS: usize = 3;

/// The header AWS Lambda reports an unhandled error of a run in: "X-Amz-Function-Error …
/// indicates whether an error occurred while executing the Lambda function" (`Invoke`).
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The emulator as it is ASSEMBLED, on one engine: the stack's queue, function and api are
/// held by the same router the invoke below reaches.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published stage
    // carries none, as a client of one sends none.
    router.set_request_authentication(false);
    router
}

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One request on the deployed stage, as a client of a published HTTP api endpoint makes it:
/// unsigned, on the api's own host, with no credential of any kind.
async fn request(router: &MultiServiceRouter, host: &str, marker: &str) -> Answered {
    let uri = format!("https://{host}{REQUEST_PATH}?marker={marker}");
    let built = http::Request::builder()
        .method(REQUEST_METHOD)
        .uri(uri)
        .header("host", host)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(built).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes a
/// control-plane call. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One API Gateway v2 control-plane read over its `restJson1` wire, as its status and parsed
/// body.
async fn apigatewayv2(router: &MultiServiceRouter, path: &str) -> (u16, Value) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(built).await;
    let status = response.status().as_u16();
    (
        status,
        serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    )
}

/// The identity policy the execution role carries, so the write the handler makes is served:
/// a handler calls another service with temporary credentials for its execution role, so
/// resolving its sink queue's URL by name and the write itself are both judged against the
/// policies in force on that role.
fn forwarding_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, and
/// granting the write its handler makes.
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
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-the-sink-queue"),
            ("PolicyDocument", &forwarding_policy()),
        ],
    )
    .await;
}

/// The handler the stack declares inline, as a synthesised app carries one: it records the
/// invocation on its sink queue — the marker the request carried, the path it arrived on and
/// the method it used — and answers that same record. The queue is addressed by NAME through
/// the service's own `GetQueueUrl`, so the source carries nothing the deployment had to
/// compute.
const RECORDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    params = event.get("queryStringParameters") or {}
    record = {
        "marker": params.get("marker"),
        "path": event.get("rawPath"),
        "method": event["requestContext"]["http"]["method"],
    }
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(record))
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(record),
    }
"#;

/// Whether a deployment of the template declares the statement that admits the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    Declared,
    NotDeclared,
}

/// The template one deployment carries: the sink queue, the function fronted by an `AWS_PROXY`
/// integration on the api's `$default` route, the auto-deployed `$default` stage, and — only
/// for [`Grant::Declared`] — the statement granting this service the invocation, narrowed to
/// every stage, method and path of the api the same template declares.
fn template(stack: &str, grant: Grant) -> String {
    let sink = format!("{stack}-sink");
    let mut resources = json!({
        "Sink": {
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": sink },
        },
        "Fn": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": format!("{stack}-fn"),
                "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Timeout": 10,
                "Code": { "ZipFile": RECORDING_SOURCE.replace("SINK_QUEUE_NAME", &sink) },
            },
        },
        "Api": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": format!("{stack}-api"),
                "ProtocolType": "HTTP",
            },
        },
        "Integration": {
            "Type": "AWS::ApiGatewayV2::Integration",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationType": "AWS_PROXY",
                "IntegrationUri": { "Fn::GetAtt": ["Fn", "Arn"] },
                "PayloadFormatVersion": "2.0",
            },
        },
        "Route": {
            "Type": "AWS::ApiGatewayV2::Route",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "RouteKey": "$default",
                "Target": { "Fn::Join": ["/", ["integrations", { "Ref": "Integration" }]] },
            },
        },
        "Stage": {
            "Type": "AWS::ApiGatewayV2::Stage",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "StageName": "$default",
                "AutoDeploy": true,
            },
        },
    });
    if grant == Grant::Declared {
        resources["InvokePermission"] = json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": ["Fn", "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": GATEWAY_PRINCIPAL,
                "SourceArn": {
                    "Fn::Sub":
                        "arn:aws:execute-api:${AWS::Region}:${AWS::AccountId}:${Api}/*/*",
                },
            },
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "FunctionName": { "Value": { "Ref": "Fn" } },
            "SinkUrl": { "Value": { "Ref": "Sink" } },
        },
    })
    .to_string()
}

/// What one deployed stack publishes, which is all the phases below address it by.
struct Deployed {
    api_id: String,
    function: String,
    sink_url: String,
    host: String,
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active` — "the
/// function is ready to be invoked" (`FunctionConfiguration$State`). The span is that
/// service's own, so nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        last = reported;
        if last.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: Lambda never reported {name} Active, last read {last}");
}

/// Deploy the stack for the first time and read back what it published, with the api's one
/// route and one integration asserted to be the shape the rule is stated about.
async fn given_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    grant: Grant,
) -> Deployed {
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template(stack, grant))],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let published = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish {name}, got {described}")
        })
    };
    let deployed = Deployed {
        api_id: published("ApiId"),
        function: published("FunctionName"),
        sink_url: published("SinkUrl"),
        host: format!(
            "{api}.execute-api.{REGION}.amazonaws.com",
            api = published("ApiId")
        ),
    };

    // The api the stack deployed is the one the rule is about: exactly one route, `$default`,
    // and exactly one integration, of the proxy type that invokes a function.
    let (code, routes) = apigatewayv2(router, &format!("/v2/apis/{}/routes", deployed.api_id)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetRoutes to answer, got {code} with {routes}"
    );
    let route_keys = items_of(&routes, "routeKey");
    assert_eq!(
        route_keys,
        vec!["$default".to_string()],
        "case {case:?}: expected the stack to deploy exactly the $default route, got \
         {route_keys:?}"
    );
    let (code, integrations) =
        apigatewayv2(router, &format!("/v2/apis/{}/integrations", deployed.api_id)).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetIntegrations to answer, got {code} with {integrations}"
    );
    let types = items_of(&integrations, "integrationType");
    assert_eq!(
        types,
        vec!["AWS_PROXY".to_string()],
        "case {case:?}: expected the stack to deploy exactly one AWS_PROXY integration, got \
         {types:?}"
    );

    // "During this time, you can't invoke or modify the function" (`CreateFunction`), so an
    // invocation withheld before that would be the lifecycle's refusal and not the policy's.
    wait_until_active(router, case, &deployed.function).await;
    deployed
}

/// One string member of every item of a paged listing, in the order it lists them.
fn items_of(listing: &Value, member: &str) -> Vec<String> {
    listing
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get(member)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Deploy a REVISION of the stack and wait for the deployment to conclude.
async fn deploy_revision(router: &MultiServiceRouter, case: &str, stack: &str, grant: Grant) {
    cloudformation(
        router,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &template(stack, grant))],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, stack, UPDATE_COMPLETE).await;
    assert_eq!(
        status, UPDATE_COMPLETE,
        "case {case:?}: expected the revision to deploy, got {status} with {described}"
    );
}

/// Assert that AWS Lambda holds EXACTLY the statement the deployment declared: the gateway's
/// own service principal, the invoke action, and the condition narrowing it to the api the
/// stack created.
async fn assert_the_grant_is_held(router: &MultiServiceRouter, case: &str, deployed: &Deployed) {
    let (code, answered) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{}/policy", deployed.function),
        None,
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected AWS Lambda to report the function's policy, got {code} with \
         {answered}"
    );
    let document: Value = answered
        .get("Policy")
        .and_then(Value::as_str)
        .and_then(|policy| serde_json::from_str(policy).ok())
        .unwrap_or(Value::Null);
    let statements = document
        .get("Statement")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        statements.len(),
        1,
        "case {case:?}: expected exactly the declared statement, got {document}"
    );
    let statement = &statements[0];
    assert_eq!(
        statement.get("Principal"),
        Some(&json!({ "Service": GATEWAY_PRINCIPAL })),
        "case {case:?}: expected the statement to admit the gateway, got {statement}"
    );
    assert_eq!(
        statement.get("Action").and_then(Value::as_str),
        Some(INVOKE_ACTION),
        "case {case:?}: expected the statement to allow the invocation, got {statement}"
    );
    let wanted = format!(
        "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*",
        api = deployed.api_id
    );
    assert_eq!(
        statement
            .pointer("/Condition/ArnLike/AWS:SourceArn")
            .and_then(Value::as_str),
        Some(wanted.as_str()),
        "case {case:?}: expected the statement to be narrowed to {wanted:?}, got {statement}"
    );
}

/// Assert that AWS Lambda reports the function as carrying NO resource policy at all, which is
/// what a function no deployment declared a statement for carries.
async fn assert_no_policy_is_held(router: &MultiServiceRouter, case: &str, deployed: &Deployed) {
    let (code, answered) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{}/policy", deployed.function),
        None,
    )
    .await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected AWS Lambda to report no policy for the function, got {code} \
         with {answered}"
    );
    let reported = answered
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        reported, "ResourceNotFoundException",
        "case {case:?}: expected the modelled not-found refusal, got {answered}"
    );
}

/// Assert that the route is REFUSED, [`REFUSED_ATTEMPTS`] times in a row, and that the
/// deployment package never ran for any of them.
async fn assert_the_route_is_refused(
    router: &MultiServiceRouter,
    case: &str,
    deployed: &Deployed,
    marker: &str,
) {
    for attempt in 1..=REFUSED_ATTEMPTS {
        let answered = request(router, &deployed.host, marker).await;
        assert_eq!(
            answered.status,
            INTEGRATION_FAULT,
            "case {case:?}: attempt {attempt}: expected {INTEGRATION_FAULT} for an invocation \
             the function's policy does not admit, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.json(),
            json!({ "message": INTEGRATION_FAULT_MESSAGE }),
            "case {case:?}: attempt {attempt}: expected the gateway's own sentence for an \
             integration that produced no response, got {:?}",
            answered.text()
        );
        assert!(
            !answered.text().contains(marker),
            "case {case:?}: attempt {attempt}: expected nothing the handler would have \
             produced in the answer, got {:?}",
            answered.text()
        );
    }
    let recorded = stayed_quiet_about(router, &deployed.sink_url, Some(marker)).await;
    assert!(
        recorded.is_empty(),
        "case {case:?}: expected the function never to be invoked, so it records nothing of \
         {marker:?}, got {recorded:?}"
    );
}

/// Assert that the deployment package ITSELF is healthy on the same path: AWS Lambda's own
/// `Invoke` of it, with the proxy event the route would hand it, signals no function error and
/// answers the handler's own object. That is the positive control a refusal is read against —
/// the route's `500` is attributable to the grant alone and not to a function that cannot run.
async fn assert_the_function_is_healthy(
    router: &MultiServiceRouter,
    case: &str,
    deployed: &Deployed,
    marker: &str,
) {
    let event = json!({
        "rawPath": REQUEST_PATH,
        "queryStringParameters": { "marker": marker },
        "requestContext": { "http": { "method": REQUEST_METHOD } },
    });
    let host = format!("lambda.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://{host}/2015-03-31/functions/{}/invocations",
            deployed.function
        ))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(event.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(built).await;
    let (parts, body) = response.into_parts();
    let answered = Answered {
        status: parts.status.as_u16(),
        body,
    };
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected AWS Lambda to run the function, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let signalled = parts.headers.get(FUNCTION_ERROR_HEADER);
    assert!(
        signalled.is_none(),
        "case {case:?}: expected the run to signal no function error, got {signalled:?} with \
         {:?}",
        answered.text()
    );
    // The payload is the proxy response the handler returned, whose `body` is its own JSON
    // object.
    let payload = answered.json();
    let returned: Value = payload
        .get("body")
        .and_then(Value::as_str)
        .and_then(|body| serde_json::from_str(body).ok())
        .unwrap_or(Value::Null);
    let expected = json!({
        "marker": marker,
        "path": REQUEST_PATH,
        "method": REQUEST_METHOD,
    });
    assert_eq!(
        returned, expected,
        "case {case:?}: expected the handler's own object {expected}, got {payload}"
    );
}

/// Assert that the route is answered by the FUNCTION — its own response — and that the
/// deployment package really ran, which is read off the queue it wrote to.
async fn assert_the_route_is_served(
    router: &MultiServiceRouter,
    case: &str,
    deployed: &Deployed,
    marker: &str,
) {
    let answered = request(router, &deployed.host, marker).await;
    assert_eq!(
        answered.status,
        SERVED,
        "case {case:?}: expected the route to be answered by its own function with {SERVED}, \
         got {} with {:?}",
        answered.status,
        answered.text()
    );
    let expected = json!({
        "marker": marker,
        "path": REQUEST_PATH,
        "method": REQUEST_METHOD,
    });
    assert_eq!(
        answered.json(),
        expected,
        "case {case:?}: expected the handler's own answer {expected}, got {:?}",
        answered.text()
    );
    let recorded = wait_for(router, &deployed.sink_url, marker).await;
    assert!(
        recorded.iter().any(|body| body.contains(marker)),
        "case {case:?}: expected the function to have run and recorded {marker:?}, got \
         {recorded:?}"
    );
}

/// The recorded shape: the statement is missing from the deployed stack, the route is refused
/// and nothing runs for it, and the ONE deployment that declares it again makes the same
/// request serve.
#[tokio::test(flavor = "multi_thread")]
async fn a_permission_declared_by_a_stack_update_makes_the_refused_route_serve() {
    let case = "the statement is added by the next deployment of the stack";
    let stack = "httpapi-permission-repaired";
    let router = emulator();
    execution_role(&router).await;

    let deployed = given_stack(&router, case, stack, Grant::NotDeclared).await;
    assert_no_policy_is_held(&router, case, &deployed).await;
    assert_the_route_is_refused(&router, case, &deployed, "before-the-repair").await;
    // The function is healthy on that same path, so the refusal above is the grant's.
    assert_the_function_is_healthy(&router, case, &deployed, "run-by-its-own-service").await;

    deploy_revision(&router, case, stack, Grant::Declared).await;
    assert_the_grant_is_held(&router, case, &deployed).await;
    assert_the_route_is_served(&router, case, &deployed, "after-the-repair").await;
}

/// The other direction of the same rule: a deployment that stops declaring the statement
/// removes it, and the next request on the serving route is refused with nothing run for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_permission_withdrawn_by_a_stack_update_stops_the_route_serving() {
    let case = "the statement is removed by the next deployment of the stack";
    let stack = "httpapi-permission-withdrawn";
    let router = emulator();
    execution_role(&router).await;

    let deployed = given_stack(&router, case, stack, Grant::Declared).await;
    assert_the_grant_is_held(&router, case, &deployed).await;
    assert_the_route_is_served(&router, case, &deployed, "before-the-withdrawal").await;

    deploy_revision(&router, case, stack, Grant::NotDeclared).await;
    assert_no_policy_is_held(&router, case, &deployed).await;
    assert_the_route_is_refused(&router, case, &deployed, "after-the-withdrawal").await;
}
