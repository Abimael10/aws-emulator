// SPDX-License-Identifier: CC0-1.0

//! What a `Map` state's ITEMS come to when the work of each one is a Lambda invocation, over
//! BOTH of the members the Amazon States Language lets a `Map` name its processor by.
//!
//! RULE: a `Map` state runs the processor it declares once per element of the array its
//! `ItemsPath` selects — whichever of the two members names that processor, the current
//! `ItemProcessor` or the language's earlier `Iterator`, which "names the same work" — with at
//! most `MaxConcurrency` of those runs in flight at one instant and no bound at all where that
//! member is the documented `0` or absent; so an execution of such a definition settles
//! `SUCCEEDED` (`smithy_query(query="shape",shape="ExecutionStatus")`) carrying as its `output`
//! one result per item, each the result of that item's own processor run, in the items' own
//! order, and `DescribeExecution` reports neither `error` nor `cause`
//! (`smithy_query(query="operation",operation="DescribeExecution")` carries both only for a
//! failure).
//!
//! Four crates have to agree for the rule to hold — Step Functions interprets the definition
//! and makes the optimized `arn:aws:states:::lambda:invoke` call per item, IAM holds the
//! documents that call is judged against, CloudFormation archives the inline handler into the
//! function's deployment package and Lambda runs it — so the suite lives here; a crate-local
//! suite sees one side of it only. What the neighbours already pin and this one does not
//! repeat: `sfn_map_items_run_their_work_together` pins that `MaxConcurrency` really bounds how
//! many items have work in flight at one instant, and `sfn_map_iterations_see_their_siblings`
//! pins what one iteration can observe of the others. What only this suite reaches is the
//! member the processor is NAMED by, and that a `Map` whose items are Lambda invocations
//! settles `SUCCEEDED` with each item's returned value as its result.
//!
//! The rows differ along what the rule generalises over: which member names the processor, the
//! `MaxConcurrency` declared (one, a bound inside the item count, a bound past it, the
//! documented "no limit" of `0`, and no member at all) and how many items the input carries
//! (none, one, several).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The reader of one element of an awsQuery answer, the percent-encoder for an
// `application/x-www-form-urlencoded` component and the poll that waits for a stack to settle:
// the `all` target is one crate root, so this suite borrows all three rather than carrying a
// second copy of each.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the function and the state
/// machines belong to. The stack poll this suite borrows reads that Region, so both halves
/// speak for one deployment.
const REGION: &str = "us-east-1";

/// The terminal status every row's execution must reach, and the one it sits in while its items
/// are still being worked through (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const RUNNING: &str = "RUNNING";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack the function and its execution role are deployed by, and the function's name.
const FUNCTION_STACK: &str = "xsvc-map-member-stack";
const FUNCTION_NAME: &str = "xsvc-map-member-worker";

/// The optimized Lambda invocation a `Map` processor's `Task` names: the integration whose
/// result is the `Invoke` answer, so `Payload` is what the function returned.
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// How long one invocation is given, as the function's own `Timeout`: generous, because what is
/// under test is the result each item came to and not how fast a host gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// What the handler multiplies its item by, so every item's result can only have come from a
/// run of that source over that item.
const FACTOR: i64 = 3;

/// How often to look while waiting for work the clock owes — a stack reaching
/// [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an execution leaving
/// [`RUNNING`] — and how many looks to take. The spans are those services' own, so no fixed
/// sleep names the instant one ends.
///
/// The deadline is ten minutes per condition because the host this suite runs on may be working
/// through the rest of the workspace's tests at the same time; nothing here waits it out, so a
/// run whose conditions hold promptly takes seconds and only a run where one never holds spends
/// it.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 6_000;

/// The handler every item's `Task` invokes: it computes its answer from the item it was handed
/// and RETURNS it, so a result the execution reports can only have come from a run of this
/// source over that item.
const WORKER_HANDLER: &str = r#"
def handler(event, context):
    return event["n"] * 3
"#;

/// The emulator as it is ASSEMBLED, on one engine: every call below is served by the same store,
/// so the function a Map item invokes is the function this suite deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One call to an AWS JSON service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call of `service`, answered with its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One IAM call, requiring a 200.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {body}"
    );
    body
}

/// One Lambda `restJson1` read, as its parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> Value {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let body = response.body();
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()))
    }
}

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions, so the identity each item's
/// integration call is made as is a session of this role and the statements below are what
/// decide it.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    service: &str,
    statements: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// Deploy the function every item's `Task` invokes and answer its ARN, once Lambda reports it
/// [`FUNCTION_ACTIVE`].
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Worker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": WORKER_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with \
         {answered}"
    );
    let mut reported = stack_status(router, FUNCTION_STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, FUNCTION_STACK).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {}",
        reported.1
    );

    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let uri = format!("/2015-03-31/functions/{FUNCTION_NAME}/configuration");
        configuration = lambda_get(router, &uri).await;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: the function must report its ARN, got {configuration}")
            });
            return arn.to_string();
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// Create a state machine over `definition` and answer its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &Value,
    role_arn: &str,
) -> String {
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": role_arn,
        }),
    )
    .await;
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
        .to_string()
}

/// Start one execution carrying `input` and answer the `DescribeExecution` that settled it,
/// looked at again until it has left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> Value {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": name,
            "input": input.to_string(),
        }),
    )
    .await;
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartExecution must answer an ARN, got {started}")
        })
        .to_string();
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if reported["status"].as_str() != Some(RUNNING) {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {reported}");
}

/// The JSON document a member of a response carries as text — an execution's `output`.
fn document(carrier: &Value, member: &str, case: &str) -> Value {
    let text = carrier[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} carrying a JSON document, got {carrier}")
    });
    serde_json::from_str(text).unwrap_or_else(|why| {
        panic!("case {case:?}: the {member} must be a JSON document, got {text:?} ({why})")
    })
}

/// The definition one row runs: ONE `Map` state over the items its input carries, naming its
/// processor by the row's member and declaring the row's `MaxConcurrency`, whose processor is a
/// single `Task` invoking the function with the item as its payload.
///
/// `OutputPath` on that `Task` takes `Payload` out of the integration's answer — the
/// optimized invocation reports the `Invoke` response, of which `Payload` is what the function
/// returned — so each item's result is the function's own value.
fn definition(processor_member: &str, concurrency: Option<i64>, function_arn: &str) -> Value {
    let processor = json!({
        "StartAt": "Work",
        "States": {
            "Work": {
                "Type": "Task",
                "Resource": INVOKE_RESOURCE,
                "Parameters": {
                    "FunctionName": function_arn,
                    "Payload.$": "$",
                },
                "OutputPath": "$.Payload",
                "End": true,
            },
        },
    });
    let mut state = json!({
        "Type": "Map",
        "ItemsPath": "$.items",
        "End": true,
    });
    state[processor_member] = processor;
    if let Some(concurrency) = concurrency {
        state["MaxConcurrency"] = json!(concurrency);
    }
    json!({ "StartAt": "Fan", "States": { "Fan": state } })
}

/// One row of the table: the member the `Map` names its processor by, the `MaxConcurrency` it
/// declares and how many items the execution's input carries.
struct Row {
    case: &'static str,
    /// `ItemProcessor`, or the language's earlier `Iterator` — the two members a `Map` may name
    /// the same work by (the service refuses a `Map` carrying neither).
    processor_member: &'static str,
    /// `MaxConcurrency` as the state declares it, or `None` for a state that declares none.
    /// `0` is the Amazon States Language's documented "no limit" for that member.
    concurrency: Option<i64>,
    /// How many items the input's `items` array holds.
    items: usize,
}

/// The catalog. The members are the two the language defines for a `Map`'s processor; the
/// concurrencies are one (serialised), a bound inside the item count, a bound past it, the
/// documented "no limit" of `0` and no member at all; the item counts are none, one and
/// several.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the legacy Iterator member, bounded inside its item count",
            processor_member: "Iterator",
            concurrency: Some(2),
            items: 5,
        },
        Row {
            case: "the legacy Iterator member, serialised by a MaxConcurrency of one",
            processor_member: "Iterator",
            concurrency: Some(1),
            items: 3,
        },
        Row {
            case: "the legacy Iterator member, under MaxConcurrency 0's documented no limit",
            processor_member: "Iterator",
            concurrency: Some(0),
            items: 4,
        },
        Row {
            case: "the legacy Iterator member, bounded past its item count",
            processor_member: "Iterator",
            concurrency: Some(9),
            items: 3,
        },
        Row {
            case: "the legacy Iterator member declaring no MaxConcurrency",
            processor_member: "Iterator",
            concurrency: None,
            items: 3,
        },
        Row {
            case: "the legacy Iterator member over one item",
            processor_member: "Iterator",
            concurrency: Some(2),
            items: 1,
        },
        Row {
            case: "the legacy Iterator member over an empty array",
            processor_member: "Iterator",
            concurrency: Some(2),
            items: 0,
        },
        Row {
            case: "the ItemProcessor member, bounded inside its item count",
            processor_member: "ItemProcessor",
            concurrency: Some(2),
            items: 5,
        },
        Row {
            case: "the ItemProcessor member declaring no MaxConcurrency",
            processor_member: "ItemProcessor",
            concurrency: None,
            items: 4,
        },
        Row {
            case: "the ItemProcessor member over an empty array",
            processor_member: "ItemProcessor",
            concurrency: None,
            items: 0,
        },
    ]
}

/// RULE: a `Map` whose processor is named by either member runs that processor once per item of
/// `ItemsPath`, so the execution settles `SUCCEEDED` with one result per item — each the value
/// the item's own Lambda invocation returned — in the items' own order, and reports no `error`
/// or `cause`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_map_state_runs_its_lambda_items_through_either_processor_member() {
    let setup = "the function and roles every row shares";
    let router = build_router();

    let function_role = given_role(
        &router,
        setup,
        "xsvc-map-member-handler-role",
        "lambda.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }]),
    )
    .await;
    let function_arn = given_function(&router, setup, &function_role).await;
    let machine_role = given_role(
        &router,
        setup,
        "xsvc-map-member-machine-role",
        "states.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": "lambda:InvokeFunction",
            "Resource": function_arn.clone(),
        }]),
    )
    .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let items: Vec<Value> = (0..row.items)
            .map(|item| json!({ "n": item as i64 + 1 }))
            .collect();
        let wanted: Vec<Value> = (0..row.items)
            .map(|item| json!((item as i64 + 1) * FACTOR))
            .collect();

        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-map-member-machine-{index}"),
            &definition(row.processor_member, row.concurrency, &function_arn),
            &machine_role,
        )
        .await;
        let reported = ran(
            &router,
            case,
            &machine_arn,
            "only",
            &json!({ "items": items }),
        )
        .await;

        assert_eq!(
            reported["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: a Map naming its processor by {:?} must settle {SUCCEEDED}, got \
             {reported}",
            row.processor_member
        );
        assert_eq!(
            document(&reported, "output", case),
            Value::Array(wanted.clone()),
            "case {case:?}: expected one result per item, {}, got {reported}",
            Value::Array(wanted)
        );
        assert_eq!(
            reported.get("error"),
            None,
            "case {case:?}: a succeeded execution reports no error, got {reported}"
        );
        assert_eq!(
            reported.get("cause"),
            None,
            "case {case:?}: a succeeded execution reports no cause, got {reported}"
        );
    }
}
