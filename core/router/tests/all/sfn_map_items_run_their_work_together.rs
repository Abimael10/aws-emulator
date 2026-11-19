// SPDX-License-Identifier: CC0-1.0

//! Whether the items of a Step Functions `Map` state have their WORK being done at the same
//! time, asked of the assembled emulator: Step Functions interprets the state, Lambda runs the
//! function each item's `Task` invokes, DynamoDB holds the tally those runs count themselves
//! in, and CloudFormation archives the handler into the function's deployment package — five
//! crates, so the question lives here.
//!
//! RULE: "MaxConcurrency […] an upper bound on how many invocations of the Iterator may run in
//! parallel". A `Map` state's items are therefore performed CONCURRENTLY, up to that many at
//! one instant: of a state's items, up to `MaxConcurrency` of them have their work in flight
//! together, and never more than that — `0` is the member's own documented "no limit", and `1`
//! is one item at a time. So a client sizing a workload on the member gets the parallelism it
//! configured, and an item's work can observe a sibling item's work running beside it.
//!
//! Each item's work here is one invocation of a function that COUNTS ITSELF into a tally of the
//! invocations in flight — an item of its own in DynamoDB, counted up as the run begins and
//! down as it ends — and then reads that tally until it holds as many as the row asks for or it
//! runs out of looks. Each row therefore asserts both halves of the rule off what the runs
//! themselves observed: the concurrency REACHED the figure the limit allows over that many
//! items, and no run ever observed more than it. No clock of this suite decides either, and no
//! run waits a fixed span for a condition that has already held: a run stops at the look where
//! the figure it waits for holds, and the looks it may take are generous.
//!
//! The rows differ along what the rule generalises over: the `MaxConcurrency` declared (a limit
//! equal to the item count, a limit smaller than it, the documented "no limit" of `0`, and `1`),
//! the item count (two, three, four — so a limit that divides the items and one that leaves a
//! remainder), and whether the figure a row's runs wait for is one its limit allows (where the
//! rendezvous is what ends them) or one it forbids (where each run is observed over its whole
//! budget of looks, and the greatest figure observed is the limit itself).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the table, the function and
/// the state machine belong to.
const REGION: &str = "us-east-1";

/// The table every run reports itself as running in, the name of its one key attribute, and
/// the attribute the runs of one row count themselves in.
const TABLE: &str = "sfn-map-concurrency-markers";
/// See [`TABLE`].
const TABLE_KEY: &str = "pk";
/// See [`TABLE`].
const RUNNING_ATTRIBUTE: &str = "running";

/// The function every item's `Task` invokes, and the stack that deploys it.
const FUNCTION: &str = "sfn-map-concurrency-worker";
/// See [`FUNCTION`].
const FUNCTION_STACK: &str = "sfn-map-concurrency-stack";

/// The `StackStatus` that stack must reach, and the `State` a function reports once it can be
/// invoked (`FunctionConfiguration$State`: "Active" once "the function is ready to be invoked").
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const FUNCTION_ACTIVE: &str = "Active";

/// The terminal status an execution whose every item ended reaches (`ExecutionStatus`), and the
/// one it sits in until then.
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const RUNNING: &str = "RUNNING";

/// The `Resource` of the optimized Lambda integration, which names the service and the action
/// rather than the function.
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// One invocation's own allowance, as the function's `Timeout`: well past the longest a run can
/// take here (its looks, below), because what is under test is how many runs overlap and not
/// how fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long one run waits between two looks at how many invocations are in flight. How MANY
/// looks it may take is the row's own ([`Row::looks`]).
const SECONDS_BETWEEN_LOOKS: f64 = 0.1;

/// How many execution environments are made to stand before the rows run, which is the most
/// items any row has: an invocation served by a standing environment pays no INIT phase, so no
/// row turns on how long building an interpreter takes on this host.
const WARMED_ENVIRONMENTS: usize = 4;

/// How often this suite looks while waiting for work the clock owes — a created table reaching
/// `ACTIVE`, a stack reaching [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an
/// execution leaving [`RUNNING`] — and how many looks it takes. The deadline is generous, so a
/// loaded host cannot decide any row's outcome, and every loop stops as soon as its condition
/// holds.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// The handler every item of every row invokes.
///
/// It counts ITSELF into the row's own tally of the runs in flight, reads that tally until it
/// holds as many as the row asks for (or runs out of looks), and counts itself out before it
/// answers — so the tally is the number of this function's invocations that are running at that
/// instant. What the handler reports is the greatest number it ever read, which is the
/// concurrency the emulator really gave the state's items.
const WORKER_HANDLER: &str = r#"
import time

import boto3

CLIENT = boto3.client("dynamodb")


def counted(table, key, delta):
    CLIENT.update_item(
        TableName=table,
        Key={"pk": {"S": key}},
        UpdateExpression="ADD running :delta",
        ExpressionAttributeValues={":delta": {"N": str(delta)}},
    )


def running_now(table, key):
    read = CLIENT.get_item(
        TableName=table,
        Key={"pk": {"S": key}},
        ConsistentRead=True,
    )
    return int(read["Item"]["running"]["N"])


def handler(event, context):
    table = event["table"]
    key = event["run"]
    want = int(event["want"])
    looks = int(event["looks"])
    pause = float(event["pause"])
    counted(table, key, 1)
    together = 0
    for taken in range(looks):
        seen = running_now(table, key)
        if seen > together:
            together = seen
        if together >= want:
            break
        time.sleep(pause)
    counted(table, key, -1)
    return {"item": event["item"], "together": together}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the invocation a `Task` makes and the
/// DynamoDB call a running handler makes are both dispatched back into this same router, so the
/// tally the runs count themselves into is the one their siblings read.
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

/// One DynamoDB call, requiring a 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "dynamodb", "DynamoDB_20120810", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` request, as its status and parsed body.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsQuery call to IAM, requiring a 200, answered with its XML body.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {text}"
    );
    text
}

/// One awsQuery call to CloudFormation, as its status and its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions and a function's own execution
/// role by Lambda, so the trusted service is the caller's.
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

/// Create the table the runs count themselves in, and wait until DynamoDB reports it
/// `ACTIVE` — "you can perform read and write operations only on an ACTIVE table".
async fn given_table(router: &MultiServiceRouter, case: &str) {
    dynamodb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": TABLE_KEY, "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": TABLE_KEY, "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = dynamodb(router, case, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: DynamoDB must report {TABLE:?} ACTIVE, last saw {described}");
}

/// Deploy the function every item invokes, wait until Lambda reports it [`FUNCTION_ACTIVE`], and
/// answer its ARN.
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
                    "FunctionName": FUNCTION,
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
    let (status, answered) = cloudformation(
        router,
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

    let mut arn = String::new();
    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let (_status, reported) = lambda(
            router,
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        )
        .await;
        configuration = reported;
        if let Some(reported) = configuration["FunctionArn"].as_str() {
            arn = reported.to_string();
        }
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) && !arn.is_empty() {
            return arn;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// The definition every row runs: ONE `Map` state over the items its input carries, declaring
/// the row's `MaxConcurrency`, whose processor is a single `Task` invoking the function with the
/// document the `ItemSelector` built for that item.
fn definition(concurrency: i64, function_arn: &str) -> Value {
    json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemsPath": "$.items",
                "MaxConcurrency": concurrency,
                "ItemSelector": {
                    "item.$": "$$.Map.Item.Value",
                    "table": TABLE,
                    "run.$": "$.run",
                    "want.$": "$.want",
                    "looks.$": "$.looks",
                    "pause.$": "$.pause",
                },
                "ItemProcessor": {
                    "StartAt": "Work",
                    "States": {
                        "Work": {
                            "Type": "Task",
                            "Resource": INVOKE_RESOURCE,
                            "Parameters": {
                                "FunctionName": function_arn,
                                "Payload.$": "$",
                            },
                            "End": true,
                        },
                    },
                },
                "End": true,
            },
        },
    })
}

/// Start one execution of `machine_arn` over `items` and report the `DescribeExecution` that
/// settled it, looked at until it has left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    input: &Value,
) -> Value {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": "only",
            "input": input.to_string(),
        }),
    )
    .await;
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartExecution must answer an ARN, got {started}"))
        .to_string();
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described["status"].as_str() != Some(RUNNING) {
            return described;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {described}");
}

/// Seed the tally `key` at nothing, so the runs that count themselves into it read a figure
/// rather than nothing.
async fn given_tally(router: &MultiServiceRouter, case: &str, key: &str) {
    dynamodb(
        router,
        case,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {
                TABLE_KEY: { "S": key },
                RUNNING_ATTRIBUTE: { "N": "0" },
            },
        }),
    )
    .await;
}

/// Make [`WARMED_ENVIRONMENTS`] execution environments STAND, by running the same `Map` over
/// that many items with every run waiting for nothing.
///
/// Each of those invocations builds an environment of its own and gives it back when it ends,
/// so the rows below are served by standing ones: an invocation that pays no INIT phase begins
/// its work as soon as it is handed over, and no row turns on how long building an interpreter
/// takes on this host. It asserts nothing about concurrency — it is a `given`.
async fn given_warm_environments(
    router: &MultiServiceRouter,
    case: &str,
    machine_role: &str,
    function_arn: &str,
) {
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({
            "name": "sfn-map-concurrency-warming",
            "definition": definition(
                i64::try_from(WARMED_ENVIRONMENTS).unwrap_or(i64::MAX),
                function_arn,
            )
            .to_string(),
            "roleArn": machine_role,
        }),
    )
    .await;
    let machine_arn = created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
        .to_string();
    given_tally(router, case, "warming").await;
    let items: Vec<Value> = (0..WARMED_ENVIRONMENTS).map(|item| json!(item)).collect();
    let settled = ran(
        router,
        case,
        &machine_arn,
        &json!({
            "run": "warming",
            // Its own count, which every run reads at its first look.
            "want": 1,
            "looks": RENDEZVOUS_LOOKS,
            "pause": SECONDS_BETWEEN_LOOKS,
            "items": items,
        }),
    )
    .await;
    assert_eq!(
        settled["status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: the warming run must succeed, got {settled}"
    );
}

/// One row: the `MaxConcurrency` the state declares, how many items it is given, how many
/// invocations running at once each of its runs waits for, how many looks it may take while
/// waiting, and the two bounds the greatest number any of them observed must satisfy.
struct Row {
    case: &'static str,
    /// `MaxConcurrency` as the state declares it; `0` is the member's own documented "no limit".
    concurrency: i64,
    items: usize,
    /// How many invocations running together each of this row's runs waits for. A figure the
    /// row's limit ALLOWS is a rendezvous: every run of a full chunk stops at the look where it
    /// holds, so the row costs what the rendezvous costs. A figure the limit FORBIDS is never
    /// reached, so each run spends its whole budget of looks — which is what a row asking only
    /// about the bound does, and why its budget is the smaller one.
    want: usize,
    /// How many looks one run of this row may take.
    looks: u64,
    /// The greatest number of invocations running at once that must have been observed: the
    /// concurrency the state's limit allows over this many items.
    at_least: usize,
    /// And the greatest number that may be: the limit is an upper bound, so no run may ever
    /// have seen more than this.
    at_most: usize,
}

/// How many looks a run of a row whose `want` the limit allows may take: spent only when the
/// rendezvous does not happen, so it is generous — a run of this emulator takes however long
/// the host gives it, and no row may turn on that.
const RENDEZVOUS_LOOKS: u64 = 150;

/// How many a run of a row asking only about the BOUND may take: every run of such a row spends
/// all of them, because the figure it waits for is one the limit forbids, so the budget is the
/// window those runs are observed over rather than a ceiling on a failure.
const BOUNDED_LOOKS: u64 = 40;

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two items under a limit of two: both invocations run together",
            concurrency: 2,
            items: 2,
            want: 2,
            looks: RENDEZVOUS_LOOKS,
            at_least: 2,
            at_most: 2,
        },
        Row {
            case: "four items under a limit of two: two run together, twice over",
            concurrency: 2,
            items: 4,
            want: 2,
            looks: RENDEZVOUS_LOOKS,
            at_least: 2,
            at_most: 2,
        },
        Row {
            case: "three items under a limit of three: every invocation runs together",
            concurrency: 3,
            items: 3,
            want: 3,
            looks: RENDEZVOUS_LOOKS,
            at_least: 3,
            at_most: 3,
        },
        Row {
            case: "three items under the documented no-limit of zero",
            concurrency: 0,
            items: 3,
            want: 3,
            looks: RENDEZVOUS_LOOKS,
            at_least: 3,
            at_most: 3,
        },
        Row {
            case: "three items under a limit of two, which never run three together",
            concurrency: 2,
            items: 3,
            // One more than the limit allows, so no run ever stops early and each is observed
            // over its whole budget of looks.
            want: 3,
            looks: BOUNDED_LOOKS,
            at_least: 2,
            at_most: 2,
        },
        Row {
            case: "two items under a limit of one, which runs them one at a time",
            concurrency: 1,
            items: 2,
            want: 2,
            looks: BOUNDED_LOOKS,
            at_least: 1,
            at_most: 1,
        },
    ]
}

/// A `Map` state performs up to `MaxConcurrency` of its items' work at one instant, and no more
/// than that: each row's runs report how many of them were in flight together, measured by the
/// runs themselves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn map_items_work_is_performed_up_to_max_concurrency_at_once() {
    let router = build_router();
    let setup = "the function, the table and the roles every row runs on";
    given_table(&router, setup).await;
    let function_role = given_role(
        &router,
        setup,
        "sfn-map-concurrency-handler-role",
        "lambda.amazonaws.com",
        json!([
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            { "Effect": "Allow", "Action": "dynamodb:*", "Resource": "*" },
        ]),
    )
    .await;
    let machine_role = given_role(
        &router,
        setup,
        "sfn-map-concurrency-machine-role",
        "states.amazonaws.com",
        json!([{ "Effect": "Allow", "Action": "lambda:InvokeFunction", "Resource": "*" }]),
    )
    .await;
    let function_arn = given_function(&router, setup, &function_role).await;
    given_warm_environments(&router, setup, &machine_role, &function_arn).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let created = sfn(
            &router,
            case,
            "CreateStateMachine",
            &json!({
                "name": format!("sfn-map-concurrency-{index}"),
                "definition": definition(row.concurrency, &function_arn).to_string(),
                "roleArn": machine_role,
            }),
        )
        .await;
        let machine_arn = created["stateMachineArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
            .to_string();
        // Every row counts its runs in a tally of its own, seeded at nothing, so one row's
        // runs are never counted in another's.
        let tally = format!("run-{index}");
        given_tally(&router, case, &tally).await;
        let items: Vec<Value> = (0..row.items).map(|item| json!(item)).collect();
        let settled = ran(
            &router,
            case,
            &machine_arn,
            &json!({
                "run": tally,
                "want": row.want,
                "looks": row.looks,
                "pause": SECONDS_BETWEEN_LOOKS,
                "items": items,
            }),
        )
        .await;
        assert_eq!(
            settled["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the run to succeed, got {settled}"
        );
        let output: Value = serde_json::from_str(settled["output"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: expected JSON output, got {error} from {settled}")
            });
        let reported: Vec<usize> = output
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected an array, got {output}"))
            .iter()
            .map(|item| {
                usize::try_from(item["Payload"]["together"].as_u64().unwrap_or_default())
                    .unwrap_or_default()
            })
            .collect();
        let most = reported.iter().copied().max().unwrap_or_default();
        assert_eq!(
            most, row.at_least,
            "case {case:?}: expected {} of this state's items to have their work in flight \
             together under MaxConcurrency {}, got {reported:?} from {output}",
            row.at_least, row.concurrency
        );
        assert!(
            reported.iter().all(|seen| *seen <= row.at_most),
            "case {case:?}: MaxConcurrency {} is an upper bound, so no run may observe more than \
             {} invocations in flight, got {reported:?} from {output}",
            row.concurrency,
            row.at_most
        );
        // Every run counted itself out again, so the tally is back where it started: what the
        // runs read was the invocations in flight and nothing left over.
        let read = dynamodb(
            &router,
            case,
            "GetItem",
            &json!({
                "TableName": TABLE,
                "Key": { TABLE_KEY: { "S": tally } },
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            read["Item"][RUNNING_ATTRIBUTE]["N"].as_str(),
            Some("0"),
            "case {case:?}: expected every run to have counted itself out again, got {read}"
        );
    }
}

