// SPDX-License-Identifier: CC0-1.0

//! An HTTP api whose `AWS_PROXY` integration names its Lambda target THROUGH A STAGE VARIABLE:
//! which function each stage of one deployment reaches, and what admits the invocation of the
//! function that stage resolved.
//!
//! RULE: **an `AWS_PROXY` integration's `IntegrationUri` may name its function through a
//! `${stageVariables.<name>}` reference, and the gateway substitutes the value the INVOKED
//! stage carries for that variable before it looks the target up — so one route, one
//! integration record and one deployment reach a different function per stage. Everything that
//! follows is decided about the function THAT stage resolved: the invocation is made only where
//! that function's own resource-based policy allows `apigateway.amazonaws.com` the
//! `lambda:InvokeFunction` action for the `execute-api` ARN of the route being served, an
//! invocation it does not admit is NEVER MADE, and the event the function is handed carries
//! that stage's own `stageVariables` map and `requestContext.stage`. A reference the serving
//! stage carries no variable for resolves to no function at all. Both of those are the class an
//! HTTP api answers `500` `{"message":"Internal Server Error"}` for — an answer, never a
//! connection left open — and adding the grant on the resolved function, with no change to the
//! api, the route, the integration, the deployment or the stage, makes the same stage serve its
//! OWN target.**
//!
//! `CreateIntegrationRequest$IntegrationUri` is "for a Lambda integration, specify the URI of a
//! Lambda function"; `CreateStageRequest$StageVariables` is "a map that defines the stage
//! variables for a Stage"; and `AddPermission$Principal` is the identifier of the service a
//! grant is made to, which for an api's integration is API Gateway's own.
//!
//! Five crates have to agree for that to hold — CloudFormation archives the inline handlers
//! into deployment packages this emulator can run, IAM holds the execution role they run as,
//! Lambda holds the two functions and their resource policies and RUNS a handler, Amazon SQS
//! holds what a handler wrote, and API Gateway v2 resolves the stage, the route, the stage
//! variable and the grant — so the suite lives here rather than in any one crate.
//!
//! Whether a function RAN is read where a client reads it: off the queue that function's own
//! handler writes to, never off an emulator internal, so the refused rows assert that the
//! resolved package never ran rather than only that the route answered `500`. The queue is read
//! behind an ORDERING BARRIER — a LATER request to the sibling stage that does serve, whose own
//! record is waited for — so nothing here depends on how fast the host runs.
//!
//! The rows span what the rule generalises over: which stage of the one api serves the request
//! (one whose variable names a function the gateway may invoke, one whose variable names a
//! function it may not, and one carrying no variable of that name at all), and whether the
//! grant on the resolved function exists at the moment of the request.
//!
//! What the sibling suites pin and this one does not restate: which policy shapes admit a
//! gateway's invocation at all
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/http_api_lambda_invoke_permission.rs`,
//! and `cloudformation_http_api_lambda_invoke_permission.rs` beside this file), what a stage
//! variable's VALUE may be (`.../tests/all/stage_variable_values.rs`), and the per-stage
//! resolution of an `HTTP_PROXY` upstream (`.../tests/all/stage_variable_backend_selection.rs`).
//! What is added here is the Lambda target resolved per stage together with the grant taken on
//! the function that resolution picked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, drain, execution_role, lambda, wait_for, ACCOUNT, CREATE_COMPLETE,
    LOOK_INTERVAL, ROLE_NAME,
};

/// The Region every request here is signed for — the one `wait_for_stack_status` and the
/// helpers above poll in, and so the Region the records of this suite live in.
const REGION: &str = "us-east-1";

/// The stack this suite deploys its two functions and their sink queues from.
const STACK: &str = "apigatewayv2-stage-variable-target";

/// The service principal an api invokes a route's function as, and the action an invocation
/// performs.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The stage variable the integration's target is resolved from. A name
/// `CreateStageRequest$StageVariables` admits: letters, digits and underscores.
const VARIABLE: &str = "fn";

/// The one route the api carries, and the path a request to it takes under a stage.
const ROUTE_KEY: &str = "GET /who";
const ROUTE_PATH: &str = "/who";
const ROUTE_METHOD: &str = "GET";

/// The three stages of the ONE deployment, and the logical id of the function each one's
/// variable names.
const PERMITTED_STAGE: &str = "permitted";
const REFUSED_STAGE: &str = "refused";
const UNSET_STAGE: &str = "unset";
const PERMITTED: &str = "Permitted";
const REFUSED: &str = "Refused";

/// The status each handler answers when it runs, so a served row is told from a refused one by
/// the status as well as by the body and the record.
const SERVED: u16 = 200;

/// The status and the sentence an HTTP api answers for an integration that produced no
/// response, which is the class both refused rows fall into. The sentence is the whole fixed
/// text of the body: it has no variable part.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";

/// The members each handler reports, in its response body and in the record it writes: which
/// function ran, which request it ran for, and the two members of the event this rule is about.
const RAN_IN: &str = "ranIn";
const CALL: &str = "call";
const REPORTED_VARIABLES: &str = "stageVariables";
const REPORTED_STAGE: &str = "stage";

/// How long a function is given to report itself ready before the suite gives up on it. The
/// span is AWS Lambda's own and the wait is on that CONDITION; this is only the deadline past
/// which the suite stops looking, so it is generous enough for a loaded machine.
const READY_BUDGET: Duration = Duration::from_secs(120);

/// CloudFormation, API Gateway v2, Lambda, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the deployed stage
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

/// One API Gateway v2 control-plane call over its `restJson1` wire, which must answer
/// `expected`.
async fn apigatewayv2(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body
        .as_ref()
        .map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/apigateway/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let answered = Answered {
        status: parts.status.as_u16(),
        body,
    };
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A member of a control-plane answer, as the string it must be.
fn as_str(case: &str, document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: expected a {member} in {document}"))
        .to_string()
}

/// One request on the deployed stage, as a client of a published HTTP api endpoint makes it:
/// unsigned, on the api's own host, with no credential of any kind. `call` is the marker the
/// handler records, so one request's record is told from another's.
async fn invoke(router: &MultiServiceRouter, host: &str, stage: &str, call: &str) -> Answered {
    let request = http::Request::builder()
        .method(ROUTE_METHOD)
        .uri(format!("https://{host}/{stage}{ROUTE_PATH}?call={call}"))
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

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function
/// is ready to be invoked" (`FunctionConfiguration$State`). The span is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (_status, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        last = reported;
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: Lambda never reported {name} Active, last read {last}");
}

/// Grant the gateway `lambda:InvokeFunction` on `function`, narrowed to every stage, method and
/// path of `api_id` — the `aws:SourceArn` form a client grants an api's integration under.
async fn gateway_may_invoke(router: &MultiServiceRouter, case: &str, function: &str, api_id: &str) {
    let source_arn = format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api_id}/*/*");
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": format!("gateway-may-invoke-{function}"),
            "Action": INVOKE_ACTION,
            "Principal": GATEWAY_PRINCIPAL,
            "SourceArn": source_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission on {function} answered {status} with {granted}"
    );
}

/// The handler both functions carry inline, exactly as a synthesised app carries one: it records
/// on its OWN sink queue which function ran, which request it ran for and the two members of the
/// event this rule is about, and answers that same record as its response. A caller substitutes
/// its own marker for `FUNCTION_MARKER` and its sink's name for `SINK_QUEUE_NAME`; the queue is
/// addressed by NAME through the service's own `GetQueueUrl`, so the source carries nothing the
/// deployment had to compute.
const RECORDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    params = event.get("queryStringParameters") or {}
    context_of = event.get("requestContext") or {}
    record = {
        "ranIn": "FUNCTION_MARKER",
        "call": params.get("call"),
        "stageVariables": event.get("stageVariables"),
        "stage": context_of.get("stage"),
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

/// The function name the stack gives the function declared under `logical_id`, which is also
/// the VALUE a stage's variable carries for it.
fn function_name(logical_id: &str) -> String {
    format!("{STACK}-{}", logical_id.to_lowercase())
}

/// The name of the sink queue the function declared under `logical_id` records on.
fn sink_name(logical_id: &str) -> String {
    format!("{STACK}-{}-sink", logical_id.to_lowercase())
}

/// The template this suite deploys: one sink queue and one function per logical id, each
/// function recording on its own sink. Neither function carries a resource policy: the grant is
/// what the cases below vary, so it is added through Lambda's own `AddPermission` rather than
/// declared here.
fn template() -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for logical_id in [PERMITTED, REFUSED] {
        let sink = sink_name(logical_id);
        resources.insert(
            format!("{logical_id}Sink"),
            json!({
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": sink },
            }),
        );
        resources.insert(
            logical_id.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(logical_id),
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 30,
                    "Code": {
                        "ZipFile": RECORDING_SOURCE
                            .replace("FUNCTION_MARKER", logical_id)
                            .replace("SINK_QUEUE_NAME", &sink),
                    },
                },
            }),
        );
        outputs.insert(
            format!("{logical_id}Arn"),
            json!({ "Value": { "Fn::GetAtt": [logical_id, "Arn"] } }),
        );
        outputs.insert(
            format!("{logical_id}SinkUrl"),
            json!({ "Value": { "Ref": format!("{logical_id}Sink") } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The estate one case drives.
struct Estate {
    router: MultiServiceRouter,
    api_id: String,
    /// The host the api's own `ApiEndpoint` names, under which a stage is reached by putting
    /// its name at the head of the path.
    host: String,
    /// Each function's sink queue URL, by logical id.
    permitted_sink: String,
    refused_sink: String,
}

/// Stand up the estate: the two functions with their sinks, then ONE api with ONE `AWS_PROXY`
/// integration whose target is a stage-variable reference, ONE route over it, ONE deployment,
/// and three stages of that deployment.
async fn given_estate() -> Estate {
    let router = build_router();
    execution_role(&router).await;
    cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template())],
    )
    .await;
    let (status, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "setup: expected the functions to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let published = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("setup: expected the stack to publish {name}, got {described}")
        })
    };

    // The integration's URI names its function through the variable, so the account and the
    // Region in it are the ones the stack's own functions really carry: substituting a
    // function's name into the template has to give that function's ARN, or the reference
    // resolves to an address no function has.
    let reference = format!("${{stageVariables.{VARIABLE}}}");
    let templated_uri = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{reference}");
    for logical_id in [PERMITTED, REFUSED] {
        let resolved = templated_uri.replace(&reference, &function_name(logical_id));
        let arn = published(&format!("{logical_id}Arn"));
        assert_eq!(
            resolved, arn,
            "setup: the target this stage variable resolves to must be the ARN Lambda holds \
             for {logical_id}, got {resolved:?} against {arn:?}"
        );
        wait_until_active(&router, &function_name(logical_id)).await;
    }

    let case = "the stage-variable target api";
    let api = apigatewayv2(
        &router,
        case,
        "POST",
        "/v2/apis",
        Some(json!({ "name": STACK, "protocolType": "HTTP" })),
        201,
    )
    .await;
    let api_id = as_str(case, &api, "apiId");
    let host = as_str(case, &api, "apiEndpoint")
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_else(|| panic!("setup {case:?}: expected a scheme in the announced endpoint"));

    let integration = apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationMethod": "POST",
            "integrationUri": templated_uri,
            "payloadFormatVersion": "2.0"
        })),
        201,
    )
    .await;
    let integration_id = as_str(case, &integration, "integrationId");
    apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": ROUTE_KEY,
            "target": format!("integrations/{integration_id}")
        })),
        201,
    )
    .await;
    let deployment = apigatewayv2(
        &router,
        case,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({})),
        201,
    )
    .await;
    let deployment_id = as_str(case, &deployment, "deploymentId");

    // Three stages of the SAME deployment: the fan-out below is theirs alone.
    for (stage, variables) in [
        (PERMITTED_STAGE, json!({ VARIABLE: function_name(PERMITTED) })),
        (REFUSED_STAGE, json!({ VARIABLE: function_name(REFUSED) })),
        // A stage that carries a variable, but not the one the URI names.
        (UNSET_STAGE, json!({ "tenant": "no-backend" })),
    ] {
        apigatewayv2(
            &router,
            stage,
            "POST",
            &format!("/v2/apis/{api_id}/stages"),
            Some(json!({
                "stageName": stage,
                "deploymentId": deployment_id,
                "stageVariables": variables
            })),
            201,
        )
        .await;
    }

    // The grant exists on ONE of the two functions.
    gateway_may_invoke(&router, case, &function_name(PERMITTED), &api_id).await;

    Estate {
        router,
        api_id,
        host,
        permitted_sink: published(&format!("{PERMITTED}SinkUrl")),
        refused_sink: published(&format!("{REFUSED}SinkUrl")),
    }
}

/// The record a served request owes: which function ran, which call it ran for, the serving
/// stage's own variable map and that stage's name.
fn expected_record(logical_id: &str, call: &str, stage: &str) -> Value {
    json!({
        RAN_IN: logical_id,
        CALL: call,
        REPORTED_VARIABLES: { VARIABLE: function_name(logical_id) },
        REPORTED_STAGE: stage,
    })
}

/// Invoke `stage` and assert it was answered by the function its OWN variable names, told about
/// that stage and that stage's variables — and that the function really ran, which is read off
/// its own sink queue.
async fn assert_served_by_its_own_target(
    estate: &Estate,
    case: &str,
    stage: &str,
    logical_id: &str,
    sink: &str,
    call: &str,
) {
    let answered = invoke(&estate.router, &estate.host, stage, call).await;
    assert_eq!(
        answered.status,
        SERVED,
        "case {case:?}: expected the function this stage's variable names to answer {SERVED}, \
         got {} with {:?}",
        answered.status,
        answered.text()
    );
    let owed = expected_record(logical_id, call, stage);
    assert_eq!(
        answered.json(),
        owed,
        "case {case:?}: expected the response {owed}, got {:?}",
        answered.text()
    );
    let recorded = wait_for(&estate.router, sink, call).await;
    assert!(
        recorded.iter().any(|body| body.contains(call)),
        "case {case:?}: expected {logical_id} to have run and recorded {call:?}, got {recorded:?}"
    );
}

/// Assert that `answered` is the gateway's own answer for an integration that produced no
/// response: the status, and the whole fixed sentence of its body.
fn assert_integration_fault(case: &str, answered: &Answered) {
    assert_eq!(
        answered.status,
        INTEGRATION_FAULT,
        "case {case:?}: expected {INTEGRATION_FAULT} rather than no answer at all, got {} with \
         {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.json(),
        json!({ "message": INTEGRATION_FAULT_MESSAGE }),
        "case {case:?}: expected the gateway's own sentence for an integration that produced no \
         response, got {:?}",
        answered.text()
    );
}

/// One stage that must be refused: its name, the call marker its requests carry, and why.
struct RefusedRow {
    case: &'static str,
    stage: &'static str,
    call: &'static str,
}

/// The rows differ along WHY the stage's reference names no invocable function: it resolves to a
/// function the gateway holds no grant on, and it resolves to nothing at all.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "the stage whose variable names a function the gateway may not invoke",
        stage: REFUSED_STAGE,
        call: "refused-stage-call",
    },
    RefusedRow {
        case: "the stage carrying no variable of the name the target reference uses",
        stage: UNSET_STAGE,
        call: "unset-stage-call",
    },
];

#[tokio::test(flavor = "multi_thread")]
async fn each_stage_resolves_its_own_lambda_target_and_is_judged_against_that_function() {
    let estate = given_estate().await;

    // The stage whose variable names the function the gateway may invoke is served by that
    // function, and the event it ran for names that stage and its variables.
    assert_served_by_its_own_target(
        &estate,
        "the stage whose variable names a function the gateway may invoke",
        PERMITTED_STAGE,
        PERMITTED,
        &estate.permitted_sink,
        "permitted-stage-call",
    )
    .await;

    for (index, row) in REFUSED_ROWS.iter().enumerate() {
        let case = row.case;
        // Driven twice: the refusal is repeatable rather than a one-off.
        for attempt in 1..=2 {
            let call = format!("{}-attempt-{attempt}", row.call);
            let answered = invoke(&estate.router, &estate.host, row.stage, &call).await;
            assert_integration_fault(&format!("{case} (attempt {attempt})"), &answered);
        }

        // The ordering barrier: a LATER request to the sibling stage that DOES serve, whose own
        // record is waited for. Any invocation the refused requests had made would have had
        // every chance to show up by the time that record is there.
        let barrier = format!("barrier-{index}");
        assert_served_by_its_own_target(
            &estate,
            case,
            PERMITTED_STAGE,
            PERMITTED,
            &estate.permitted_sink,
            &barrier,
        )
        .await;
        let on_refused_sink = drain(&estate.router, &estate.refused_sink).await;
        assert!(
            on_refused_sink.is_empty(),
            "case {case:?}: expected no function to have been invoked for this stage, so its \
             sink holds nothing, got {on_refused_sink:?}"
        );
        let left_on_permitted = drain(&estate.router, &estate.permitted_sink).await;
        assert!(
            !left_on_permitted
                .iter()
                .any(|body| body.contains(row.call)),
            "case {case:?}: this stage's request must reach no other stage's function either, \
             got {left_on_permitted:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn granting_the_resolved_function_makes_that_stage_serve_its_own_target() {
    let estate = given_estate().await;

    // Before the grant: the gateway answers its own fault for the stage whose variable names
    // the ungranted function.
    let answered = invoke(
        &estate.router,
        &estate.host,
        REFUSED_STAGE,
        "before-the-grant",
    )
    .await;
    assert_integration_fault("before the grant on the resolved function", &answered);

    // The grant alone: no api, route, integration, deployment or stage is touched.
    gateway_may_invoke(
        &estate.router,
        "the grant on the resolved function",
        &function_name(REFUSED),
        &estate.api_id,
    )
    .await;

    assert_served_by_its_own_target(
        &estate,
        "the same stage, once its resolved function grants the invocation",
        REFUSED_STAGE,
        REFUSED,
        &estate.refused_sink,
        "after-the-grant",
    )
    .await;

    // And the fan-out is unchanged: the sibling stage still reaches the function ITS own
    // variable names.
    assert_served_by_its_own_target(
        &estate,
        "the sibling stage, after the other stage's function was granted",
        PERMITTED_STAGE,
        PERMITTED,
        &estate.permitted_sink,
        "sibling-after-the-grant",
    )
    .await;
}
