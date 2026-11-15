// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed HTTP API's `AWS_PROXY` route INVOKES its Lambda function only
//! while the stack also grants API Gateway the invocation — end to end over the real
//! `MultiServiceRouter` with CloudFormation, API Gateway v2, Lambda, Amazon SQS and IAM
//! mounted on one engine.
//!
//! The rule pinned here: **a gateway invokes an `AWS_PROXY` route's function as its OWN
//! service principal (`apigateway.amazonaws.com`) rather than as the client that called the
//! route, so the invocation is made only where the FUNCTION's resource-based policy allows
//! that principal `lambda:InvokeFunction` for the `execute-api` ARN of the route being served
//! (`arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{method}/{path}`, which is what
//! such a grant's `aws:SourceArn` may narrow it to). An invocation the policy does not admit
//! is NEVER MADE — the deployment package does not run — and the route is answered `500`
//! `{"message":"Internal Server Error"}`, the status an HTTP api maps an integration that
//! produced no response to.** None of that depends on how the api, the function or the grant
//! came to exist: a stack's `AWS::ApiGatewayV2::*` resources and its `AWS::Lambda::Permission`
//! are the same api, integration, route and policy statement a client's own
//! `CreateApi`/`CreateIntegration`/`CreateRoute`/`AddPermission` leave behind.
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the queues, the
//! functions, the api with its integrations, routes and auto-deployed stage, and the statement;
//! API Gateway v2 resolves the stage and the route and decides the invocation against the
//! function's policy; Lambda holds that policy and RUNS the handler; Amazon SQS holds what the
//! handler wrote; IAM holds the execution role the handler's own write is judged against — so
//! the suite lives here rather than in any one crate. Whether the function RAN is read where a
//! client reads it: off the queue the handler writes to, never off an emulator internal, so a
//! refused row asserts that the package never ran rather than merely that the route answered
//! `500`.
//!
//! The rows differ along exactly what the rule generalises over: whether the stack declares the
//! statement at all, and — when it does — what it narrows the grant to (nothing, every stage,
//! method and path of this api, this one route exactly, another api altogether) and which
//! service principal it names. Each row deploys the same TWO-route shape — two functions, each
//! behind its own `AWS_PROXY` integration and route — where the second function always carries
//! an unnarrowed grant, so every row asserts the probed route against a positive control served
//! by the same stack, the same api and the same stage: a refusal is then the grant's and not the
//! deployment's.
//!
//! Which policy shapes admit the invocation at all, driven through this service's own control
//! plane, is
//! `service-com-amazonaws-apigatewayv2-apigatewayv2`'s `http_api_lambda_invoke_permission`;
//! this one is the suite for the whole deployed stack, from the template to the handler's own
//! work product.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, stayed_quiet, trust_policy, wait_for, ACCOUNT, CREATE_COMPLETE,
    LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one `wait_for_stack_status` polls in.
const REGION: &str = "us-east-1";

/// The service principal an api invokes a route's function as, which is what a grant for this
/// service has to name, and one that is not it.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM action an invocation of a function performs, and one that is not it.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const OTHER_ACTION: &str = "lambda:GetFunction";

/// The execution role the declared functions are created with, provisioned in IAM before the
/// stack so the template names a role that service really holds.
const ROLE_NAME: &str = "httpapi-invoke-boundary-exec";

/// How long a function is given to report itself ready before the suite gives up on it. The
/// span is AWS Lambda's own and the wait is on that CONDITION; this is only the deadline past
/// which the suite stops looking, so it is generous enough for a loaded machine.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// The two routes every row deploys: the one whose grant the row varies, and the positive
/// control whose grant it never does.
const PROBE_METHOD: &str = "GET";
const PROBE_PATH: &str = "/probe";
const CONTROL_PATH: &str = "/control";

/// The status the handler answers when it runs, so a row tells a served route from a refused
/// one by the answer as well as by the record.
const SERVED: u16 = 201;

/// The status and the sentence an HTTP api answers for an integration that produced no
/// response, which is the class an invocation the function's policy does not admit falls into.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// An api id of the published shape that nothing in this suite ever created, which is what a
/// grant narrowed to ANOTHER api names.
const ANOTHER_API_ID: &str = "aaaaaaaaaa";

/// CloudFormation, API Gateway v2, Lambda, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published stage
    // carries none, as a client of one sends none.
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
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Lambda `restJson1` GET, as its parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function
/// is ready to be invoked" (`FunctionConfiguration$State`). The span is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = lambda_get(
            router,
            &format!("/2015-03-31/functions/{name}/configuration"),
        )
        .await;
        if last.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: Lambda never reported {name} Active, last read {last}");
}

/// One answered request on the api's published endpoint: its status and its body verbatim.
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
async fn invoke(router: &MultiServiceRouter, host: &str, path: &str, marker: &str) -> Answered {
    let request = http::Request::builder()
        .method(PROBE_METHOD)
        .uri(format!("https://{host}{path}?marker={marker}"))
        .header("host", host)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// The identity policy the execution role carries, so the write each handler makes is served:
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
/// granting the write its handlers make.
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

/// The handler both declared functions carry inline, exactly as a synthesised app carries one:
/// it records the invocation on its OWN sink queue — the path it was reached on and the marker
/// the request carried — and answers [`SERVED`]. The queue is addressed by NAME through the
/// service's own `GetQueueUrl`, so the source carries nothing the deployment had to compute.
const RECORDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    params = event.get("queryStringParameters") or {}
    record = {"path": event.get("rawPath"), "marker": params.get("marker")}
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(record))
    return {
        "statusCode": 201,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(record),
    }
"#;

/// What the stack's `AWS::Lambda::Permission` for the PROBED function declares, which is the
/// one thing the rows differ in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No `AWS::Lambda::Permission` resource at all, so the function carries no policy — the
    /// recorded case.
    Absent,
    /// The grant declared with no `SourceArn`, so it is narrowed to no invoking resource.
    Unnarrowed,
    /// The grant narrowed to every stage, method and path of THIS api, the form a synthesised
    /// app declares.
    ThisApiWildcard,
    /// The grant narrowed to this ONE route of this api exactly.
    ThisRoute,
    /// The grant narrowed to another api altogether.
    AnotherApi,
    /// The unnarrowed grant, made to another service principal.
    AnotherPrincipal,
    /// The unnarrowed grant to this service, of another action.
    AnotherAction,
}

impl Grant {
    /// The `Principal` the declaration names.
    fn principal(self) -> &'static str {
        match self {
            Grant::AnotherPrincipal => OTHER_PRINCIPAL,
            _ => GATEWAY_PRINCIPAL,
        }
    }

    /// The `Action` the declaration names.
    fn action(self) -> &'static str {
        match self {
            Grant::AnotherAction => OTHER_ACTION,
            _ => INVOKE_ACTION,
        }
    }

    /// The `SourceArn` the declaration names, as the template states it — `None` for a
    /// declaration that names none at all. The ARNs of THIS api are built around a reference
    /// to the api the same template declares, which is how a stack names a resource whose id
    /// the deployment mints.
    fn source_arn(self) -> Option<Value> {
        let prefix = "arn:aws:execute-api:${AWS::Region}:${AWS::AccountId}";
        match self {
            Grant::ThisApiWildcard => Some(json!({ "Fn::Sub": format!("{prefix}:${{Api}}/*/*/*") })),
            Grant::ThisRoute => Some(json!({
                "Fn::Sub": format!(
                    "{prefix}:${{Api}}/$default/{PROBE_METHOD}{PROBE_PATH}",
                ),
            })),
            Grant::AnotherApi => Some(json!(format!(
                "arn:aws:execute-api:{REGION}:{ACCOUNT}:{ANOTHER_API_ID}/*/*/*"
            ))),
            _ => None,
        }
    }
}

/// One row: what the stack grants on the probed route's function, and whether the gateway may
/// therefore invoke it.
struct Row {
    case: &'static str,
    stack: &'static str,
    grant: Grant,
    /// Whether the probed function is invoked, so its handler's record reaches its sink queue
    /// and the route is answered the handler's own response.
    invoked: bool,
}

/// The table this suite spans, one named row per statement the stack can declare on the probed
/// route's function. Each row is driven by its OWN test at the end of this file rather than by a
/// loop over the table: a row deploys a stack and runs two deployment packages, so one row per
/// test process keeps every row's deployment inside its own budget however loaded the host is.
const NO_PERMISSION: Row = Row {
    // The recorded case: the permission a synthesised app provisions is not in the stack.
    case: "the stack declares no AWS::Lambda::Permission for the route's function",
    stack: "httpapi-invoke-boundary-none",
    grant: Grant::Absent,
    invoked: false,
};
const UNNARROWED: Row = Row {
    case: "the permission grants this service the invocation, narrowed to no source at all",
    stack: "httpapi-invoke-boundary-unnarrowed",
    grant: Grant::Unnarrowed,
    invoked: true,
};
const THIS_API_WILDCARD: Row = Row {
    case: "the permission is narrowed to every stage, method and path of this api",
    stack: "httpapi-invoke-boundary-wildcard",
    grant: Grant::ThisApiWildcard,
    invoked: true,
};
const THIS_ROUTE: Row = Row {
    case: "the permission is narrowed to this one route of this api exactly",
    stack: "httpapi-invoke-boundary-route",
    grant: Grant::ThisRoute,
    invoked: true,
};
const ANOTHER_API: Row = Row {
    case: "the permission is narrowed to another api altogether",
    stack: "httpapi-invoke-boundary-other-api",
    grant: Grant::AnotherApi,
    invoked: false,
};
const ANOTHER_PRINCIPAL: Row = Row {
    case: "the permission grants the invocation to another service principal",
    stack: "httpapi-invoke-boundary-other-principal",
    grant: Grant::AnotherPrincipal,
    invoked: false,
};
const ANOTHER_ACTION: Row = Row {
    case: "the permission grants this service another action",
    stack: "httpapi-invoke-boundary-other-action",
    grant: Grant::AnotherAction,
    invoked: false,
};

/// One `AWS::Lambda::Function` declaration: the role IAM holds, the inline handler recording on
/// `sink`, and a name the suite can poll the function's state by.
fn function(name: &str, sink: &str) -> Value {
    json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": name,
            "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
            "Runtime": "python3.12",
            "Handler": "index.lambda_handler",
            "Timeout": 10,
            "Code": { "ZipFile": RECORDING_SOURCE.replace("SINK_QUEUE_NAME", sink) },
        },
    })
}

/// One `AWS::ApiGatewayV2::Integration` fronting the function declared under `logical_id`:
/// "AWS_PROXY: for integrating the route or method request with the Lambda function-invoking
/// action" (`Integration$IntegrationType`).
fn integration(logical_id: &str) -> Value {
    json!({
        "Type": "AWS::ApiGatewayV2::Integration",
        "Properties": {
            "ApiId": { "Ref": "Api" },
            "IntegrationType": "AWS_PROXY",
            "IntegrationUri": { "Fn::GetAtt": [logical_id, "Arn"] },
            "PayloadFormatVersion": "2.0",
        },
    })
}

/// One `AWS::ApiGatewayV2::Route` wired to the integration declared under `logical_id`, in the
/// `integrations/<IntegrationId>` form that service accepts.
fn route(path: &str, logical_id: &str) -> Value {
    json!({
        "Type": "AWS::ApiGatewayV2::Route",
        "Properties": {
            "ApiId": { "Ref": "Api" },
            "RouteKey": format!("{PROBE_METHOD} {path}"),
            "Target": { "Fn::Join": ["/", ["integrations", { "Ref": logical_id }]] },
        },
    })
}

/// The template one row deploys: a sink queue per function, the api with its auto-deployed
/// `$default` stage, two functions each behind its own `AWS_PROXY` integration and route, the
/// unnarrowed grant on the CONTROL function, and the row's own declaration — if any — on the
/// probed one.
fn template(stack: &str, grant: Grant) -> String {
    let probe_sink = format!("{stack}-probe-sink");
    let control_sink = format!("{stack}-control-sink");
    let mut resources = json!({
        "ProbeSink": {
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": probe_sink },
        },
        "ControlSink": {
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": control_sink },
        },
        "Api": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": format!("{stack}-api"),
                "ProtocolType": "HTTP",
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
        "Probe": function(&format!("{stack}-probe"), &probe_sink),
        "Control": function(&format!("{stack}-control"), &control_sink),
        "ProbeIntegration": integration("Probe"),
        "ControlIntegration": integration("Control"),
        "ProbeRoute": route(PROBE_PATH, "ProbeIntegration"),
        "ControlRoute": route(CONTROL_PATH, "ControlIntegration"),
        "ControlPermission": {
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": ["Control", "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": GATEWAY_PRINCIPAL,
            },
        },
    });
    if grant != Grant::Absent {
        let mut properties = json!({
            "FunctionName": { "Fn::GetAtt": ["Probe", "Arn"] },
            "Action": grant.action(),
            "Principal": grant.principal(),
        });
        if let Some(source_arn) = grant.source_arn() {
            properties["SourceArn"] = source_arn;
        }
        resources["ProbePermission"] = json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": properties,
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "ProbeSinkUrl": { "Value": { "Ref": "ProbeSink" } },
            "ControlSinkUrl": { "Value": { "Ref": "ControlSink" } },
        },
    })
    .to_string()
}

/// ONE row, end to end: the statement the stack declares on the probed route's function decides
/// whether that route's request runs the function at all, asserted against a positive control
/// route of the same api whose function always carries the grant.
async fn assert_the_row(row: &Row) {
    let case = row.case;
    let router = build_router();
    execution_role(&router).await;

    cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template(row.stack, row.grant)),
        ],
    )
    .await;
    let (status, described) = wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
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
    let api_id = published("ApiId");
    let probe_sink_url = published("ProbeSinkUrl");
    let control_sink_url = published("ControlSinkUrl");

    // Both functions have to be invocable before either route is called: "During this
    // time, you can't invoke or modify the function" (`CreateFunction`), so an invocation
    // withheld here would be the lifecycle's refusal and not the policy's.
    wait_until_active(&router, case, &format!("{}-probe", row.stack)).await;
    wait_until_active(&router, case, &format!("{}-control", row.stack)).await;

    // The probed route, on the api's own endpoint, as a client of a published stage calls it.
    let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
    let probe_marker = format!("{}-probe-call", row.stack);
    let probed = invoke(&router, &host, PROBE_PATH, &probe_marker).await;

    // The control route of the SAME api and stage, whose function carries the grant: it is
    // served by the function itself in every row, so a refusal above is the statement's and
    // not the deployment's.
    let control_marker = format!("{}-control-call", row.stack);
    let controlled = invoke(&router, &host, CONTROL_PATH, &control_marker).await;
    assert_eq!(
        controlled.status,
        SERVED,
        "case {case:?}: expected the control route's own function to answer {SERVED}, got {} \
         with {:?}",
        controlled.status,
        controlled.text()
    );
    let control_record = json!({ "path": CONTROL_PATH, "marker": control_marker });
    assert_eq!(
        controlled.json(),
        control_record,
        "case {case:?}: expected the control route to be answered the handler's own response \
         {control_record}, got {:?}",
        controlled.text()
    );
    let on_control_sink = wait_for(&router, &control_sink_url, &control_marker).await;
    assert!(
        on_control_sink
            .iter()
            .any(|body| body.contains(&control_marker)),
        "case {case:?}: expected the control route's function to have run and recorded \
         {control_marker:?}, got {on_control_sink:?}"
    );

    if row.invoked {
        assert_eq!(
            probed.status,
            SERVED,
            "case {case:?}: expected the probed route to be answered by its own function with \
             {SERVED}, got {} with {:?}",
            probed.status,
            probed.text()
        );
        let probe_record = json!({ "path": PROBE_PATH, "marker": probe_marker });
        assert_eq!(
            probed.json(),
            probe_record,
            "case {case:?}: expected the probed route to be answered the handler's own \
             response {probe_record}, got {:?}",
            probed.text()
        );
        let on_probe_sink = wait_for(&router, &probe_sink_url, &probe_marker).await;
        assert!(
            on_probe_sink.iter().any(|body| body.contains(&probe_marker)),
            "case {case:?}: expected the probed route's function to have run and recorded \
             {probe_marker:?}, got {on_probe_sink:?}"
        );
        return;
    }

    assert_eq!(
        probed.status,
        INTEGRATION_FAULT,
        "case {case:?}: expected {INTEGRATION_FAULT} for an invocation the function's policy \
         does not admit, got {} with {:?}",
        probed.status,
        probed.text()
    );
    assert_eq!(
        probed.json(),
        json!({ "message": INTEGRATION_FAULT_MESSAGE }),
        "case {case:?}: expected the gateway's own sentence for an integration that produced \
         no response, got {:?}",
        probed.text()
    );
    // The invocation is synchronous — the route's answer would come after the function
    // returned — so nothing on this queue says the deployment package never ran at all.
    let quiet = stayed_quiet(&router, &probe_sink_url).await;
    assert!(
        quiet.is_empty(),
        "case {case:?}: expected the probed route's function never to be invoked, so it \
         records nothing, got {quiet:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_the_stack_declares_no_permission_for_is_not_invoked() {
    assert_the_row(&NO_PERMISSION).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_names_no_source_is_invoked() {
    assert_the_row(&UNNARROWED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_is_narrowed_to_its_whole_api_is_invoked() {
    assert_the_row(&THIS_API_WILDCARD).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_is_narrowed_to_that_route_is_invoked() {
    assert_the_row(&THIS_ROUTE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_is_narrowed_to_another_api_is_not_invoked() {
    assert_the_row(&ANOTHER_API).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_names_another_principal_is_not_invoked() {
    assert_the_row(&ANOTHER_PRINCIPAL).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_whose_permission_names_another_action_is_not_invoked() {
    assert_the_row(&ANOTHER_ACTION).await;
}
