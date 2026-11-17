// SPDX-License-Identifier: CC0-1.0

//! The WAITERS the AWS SDK a managed `python3.x` runtime supplies carries, driven from inside
//! a running function: IAM holds the execution role, STS mints the session the handler's calls
//! are signed with, CloudFormation archives the handler into its deployment package, Lambda
//! runs it and answers its own `GetFunction` polls, and DynamoDB and Kinesis mint the
//! resources the waits are about — so the suite lives here: no single-service suite can see a
//! wait a FUNCTION made come to an end.
//!
//! RULE: the AWS SDK a managed `python3.x` runtime puts on the handler's import path carries
//! each service's own WAITERS — `client.get_waiter(<name>)` answers the waiter the service
//! publishes under that name, which polls that waiter's own operation through the same client
//! until one of its acceptors decides the wait: a success acceptor returns, a failure acceptor
//! and a wait that runs out of attempts raise `botocore.exceptions.WaiterError` carrying the
//! answer the wait ended on, and a refusal no acceptor names ends the wait too;
//! `client.waiter_names` reports the waiters a service publishes, and a name it publishes none
//! under raises `ValueError`.
//!
//! This is the interface a handler that has just asked for a resource uses to wait for it —
//! `boto3.client("dynamodb").get_waiter("table_exists").wait(TableName=…)` after a
//! `CreateTable` — so without it such a handler faults where AWS runs it to completion.
//!
//! The rows differ along what the rule generalises over: WHICH service the client was built
//! for (DynamoDB, Lambda and Kinesis, so no row's outcome can come from one service's own
//! bookkeeping), WHICH waiter of it is asked for, WHICH matcher its acceptors judge an answer
//! with (a member of the answer, the HTTP status it came back on, and the error code a refusal
//! carries), and WHICH outcome the wait reaches (a state the clock owes, a resource already
//! gone, one that never arrives, a refusal, and a name no waiter exists under). Two rows read
//! the waited-for resource back through its own service afterwards, so a wait that ended
//! without the resource reaching the state fails here.
//!
//! The waiter definitions the rows assert — the operation a waiter polls, how long it waits
//! and how many polls it makes — are the ones the AWS SDKs' own waiter data publishes for the
//! service, and cover entries beyond the `table_exists` a bootstrap handler reaches for.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region every resource belongs to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the function's stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The function the rows drive, and the role it runs as.
const FUNCTION: &str = "xsvc-sdk-waiters-driver";
const ROLE: &str = "xsvc-sdk-waiters-role";

/// The stack that deploys the function.
const STACK: &str = "xsvc-sdk-waiters";

/// How long one invocation is given, as the function's own `Timeout`: a row that polls waits
/// out a transition the clock owes, and this leaves that wait generous room.
const FUNCTION_TIMEOUT: u64 = 120;

/// The name no function of this account carries, which the "never arrives" row waits on.
const ABSENT_FUNCTION: &str = "xsvc-sdk-waiters-no-such-function";

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every row runs: one case per entry of its own table, each asking a client for a
/// waiter and reporting what the wait did.
const WAITER_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError, WaiterError

DDB = boto3.client("dynamodb")
LAMBDA = boto3.client("lambda")
KINESIS = boto3.client("kinesis")

# What the cases that really poll wait with: the resource each waits for settles a span after
# the call that asked for it, so the polls are close together and there are plenty of them.
POLLING = {"Delay": 1, "MaxAttempts": 60}


def _create_table(name):
    DDB.create_table(
        TableName=name,
        AttributeDefinitions=[{"AttributeName": "pk", "AttributeType": "S"}],
        KeySchema=[{"AttributeName": "pk", "KeyType": "HASH"}],
        BillingMode="PAY_PER_REQUEST",
    )


def table_reached(event):
    _create_table(event["table"])
    DDB.get_waiter("table_exists").wait(TableName=event["table"], WaiterConfig=POLLING)
    described = DDB.describe_table(TableName=event["table"])
    return {"status": described["Table"]["TableStatus"]}


def one_attempt_only(event):
    _create_table(event["table"])
    DDB.get_waiter("table_exists").wait(TableName=event["table"], WaiterConfig=POLLING)
    try:
        DDB.get_waiter("table_not_exists").wait(
            TableName=event["table"], WaiterConfig={"Delay": 1, "MaxAttempts": 1}
        )
    except WaiterError as error:
        return {
            "failed": str(error),
            "last_status": error.last_response["Table"]["TableStatus"],
        }
    return {"failed": None}


def absent_table(event):
    DDB.get_waiter("table_not_exists").wait(
        TableName=event["table"], WaiterConfig=POLLING
    )
    return {"waited": "table_not_exists"}


def deleted_table(event):
    _create_table(event["table"])
    DDB.get_waiter("table_exists").wait(TableName=event["table"], WaiterConfig=POLLING)
    DDB.delete_table(TableName=event["table"])
    DDB.get_waiter("table_not_exists").wait(
        TableName=event["table"], WaiterConfig=POLLING
    )
    try:
        DDB.describe_table(TableName=event["table"])
    except ClientError as error:
        return {"gone": error.response["Error"]["Code"]}
    return {"gone": None}


def no_such_waiter(event):
    try:
        DDB.get_waiter(event["waiter"])
    except ValueError as error:
        return {"value_error": str(error)}
    return {"value_error": None}


def published_waiters(event):
    return {
        "dynamodb": DDB.waiter_names,
        "lambda": LAMBDA.waiter_names,
        "kinesis": KINESIS.waiter_names,
    }


def definitions(event):
    reported = {}
    for name, client in (("table_exists", DDB), ("function_updated", LAMBDA)):
        waiter = client.get_waiter(name)
        reported[name] = {
            "name": waiter.name,
            "operation": waiter.config.operation,
            "delay": waiter.config.delay,
            "max_attempts": waiter.config.max_attempts,
        }
    return reported


def running_function(event):
    LAMBDA.get_waiter("function_exists").wait(
        FunctionName=event["function"], WaiterConfig=POLLING
    )
    LAMBDA.get_waiter("function_active_v2").wait(
        FunctionName=event["function"], WaiterConfig=POLLING
    )
    return {"exists": True, "active": True}


def absent_function(event):
    try:
        LAMBDA.get_waiter("function_exists").wait(
            FunctionName=event["function"], WaiterConfig={"Delay": 0, "MaxAttempts": 2}
        )
    except WaiterError as error:
        return {"failed": str(error), "code": error.last_response["Error"]["Code"]}
    return {"failed": None}


def stream_reached(event):
    KINESIS.create_stream(StreamName=event["stream"], ShardCount=1)
    KINESIS.get_waiter("stream_exists").wait(
        StreamName=event["stream"], WaiterConfig=POLLING
    )
    described = KINESIS.describe_stream(StreamName=event["stream"])
    return {"status": described["StreamDescription"]["StreamStatus"]}


def resource_table(event):
    table = boto3.resource("dynamodb").Table(event["table"])
    _create_table(event["table"])
    table.wait_until_exists(WaiterConfig=POLLING)
    return {"status": table.table_status, "waiters": table.meta.client.waiter_names}


def refused_wait(event):
    try:
        DDB.get_waiter("table_exists").wait(
            TableName=event["table"], WaiterConfig=POLLING
        )
    except WaiterError as error:
        return {"failed": str(error), "code": error.last_response["Error"]["Code"]}
    return {"failed": None}


CASES = {
    "a table reached by its own waiter": table_reached,
    "a wait given one attempt on a resource in the other state": one_attempt_only,
    "a table that was never there": absent_table,
    "a table that has been deleted": deleted_table,
    "a name no waiter exists under": no_such_waiter,
    "the waiters each client has": published_waiters,
    "the definition a waiter carries": definitions,
    "the running function itself": running_function,
    "a function that never arrives": absent_function,
    "a stream reached by its own waiter": stream_reached,
    "a table resource waiting on itself": resource_table,
    "a wait on a refusal no acceptor names": refused_wait,
}


def handler(event, context):
    return CASES[event["case"]](event)
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the table and the stream its waiters poll are the records those
/// services hold.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and not the function's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One DynamoDB operation over its `awsJson1_0` wire, made as the account's own credential.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("dynamodb"))
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
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

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The inline document the execution role carries: the calls the cases make, and nothing else.
fn role_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": [
                    "dynamodb:CreateTable",
                    "dynamodb:DescribeTable",
                    "dynamodb:DeleteTable",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": ["kinesis:CreateStream", "kinesis:DescribeStream"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": ["lambda:GetFunction", "lambda:GetFunctionConfiguration"],
                "Resource": "*",
            },
        ],
    })
    .to_string()
}

/// Create the execution role through IAM's own `CreateRole`/`PutRolePolicy`, and answer the
/// ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({ROLE:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({ROLE:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "waiter-grants"),
            ("PolicyDocument", &role_document()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({ROLE:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Deploy the function through CloudFormation, which archives the inline source into the
/// deployment package, and wait until Lambda reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Driver": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": WAITER_HANDLER},
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
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let mut reported = stack_status(router, STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, STACK).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );

    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// Invoke the driver with one case's event, answering what the handler reported — asserting on
/// the way that the run reported no function error, which is what an interface the runtime
/// does not supply reports instead of a result.
async fn drive(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?}: the handler must run to completion, got {faulted:?} with {payload}"
    );
    payload
}

/// One row: the case the handler runs, the event it is handed, and what it must report.
struct Row {
    case: &'static str,
    event: Value,
    expected: Value,
}

/// The table every row of the first suite drives, so each row's resource is its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a table reached by its own waiter",
            event: json!({ "table": "xsvc-waiters-created" }),
            expected: json!({ "status": "ACTIVE" }),
        },
        Row {
            case: "a wait given one attempt on a resource in the other state",
            event: json!({ "table": "xsvc-waiters-one-attempt" }),
            expected: json!({
                "failed": "Waiter TableNotExists failed: Max attempts exceeded",
                "last_status": "ACTIVE",
            }),
        },
        Row {
            case: "a table that was never there",
            event: json!({ "table": "xsvc-waiters-never-there" }),
            expected: json!({ "waited": "table_not_exists" }),
        },
        Row {
            case: "a table that has been deleted",
            event: json!({ "table": "xsvc-waiters-deleted" }),
            expected: json!({ "gone": "ResourceNotFoundException" }),
        },
        Row {
            case: "a name no waiter exists under",
            event: json!({ "waiter": "table_is_ready" }),
            expected: json!({ "value_error": "Waiter does not exist: table_is_ready" }),
        },
        Row {
            case: "the waiters each client has",
            event: json!({}),
            expected: json!({
                "dynamodb": ["table_exists", "table_not_exists"],
                "lambda": [
                    "function_active",
                    "function_active_v2",
                    "function_exists",
                    "function_updated",
                    "function_updated_v2",
                    "published_version_active",
                ],
                "kinesis": ["stream_exists", "stream_not_exists"],
            }),
        },
        Row {
            case: "the definition a waiter carries",
            event: json!({}),
            expected: json!({
                "table_exists": {
                    "name": "TableExists",
                    "operation": "DescribeTable",
                    "delay": 20,
                    "max_attempts": 25,
                },
                "function_updated": {
                    "name": "FunctionUpdated",
                    "operation": "GetFunctionConfiguration",
                    "delay": 5,
                    "max_attempts": 60,
                },
            }),
        },
        Row {
            case: "the running function itself",
            event: json!({ "function": FUNCTION }),
            expected: json!({ "exists": true, "active": true }),
        },
        Row {
            case: "a function that never arrives",
            event: json!({ "function": ABSENT_FUNCTION }),
            expected: json!({
                "failed": "Waiter FunctionExists failed: Max attempts exceeded. Previously \
                           accepted state: Matched expected service error code: \
                           ResourceNotFoundException",
                "code": "ResourceNotFoundException",
            }),
        },
        Row {
            case: "a stream reached by its own waiter",
            event: json!({ "stream": "xsvc-waiters-stream" }),
            expected: json!({ "status": "ACTIVE" }),
        },
        Row {
            case: "a table resource waiting on itself",
            event: json!({ "table": "xsvc-waiters-resource" }),
            expected: json!({
                "status": "ACTIVE",
                "waiters": ["table_exists", "table_not_exists"],
            }),
        },
    ]
}

/// Every row's wait is the service's own waiter, polled through the client the handler built
/// and ended by the acceptor that waiter's definition states.
#[tokio::test]
async fn an_in_function_waiter_is_the_services_own() {
    let router = build_router();
    let role_arn = create_role(&router).await;
    deploy_function(&router, &role_arn).await;

    for row in rows() {
        let mut event = row.event.clone();
        event["case"] = json!(row.case);
        let reported = drive(&router, row.case, &event).await;
        assert_eq!(
            reported, row.expected,
            "case {:?}: expected {}, got {reported}",
            row.case, row.expected
        );
    }

    // What the services THEMSELVES hold after the waits: a wait that returned on a state the
    // resource never reached, or a wait for a deletion that never happened, fails here.
    for (case, table, expected) in [
        ("a table reached by its own waiter", "xsvc-waiters-created", Some("ACTIVE")),
        ("a table resource waiting on itself", "xsvc-waiters-resource", Some("ACTIVE")),
        ("a table that has been deleted", "xsvc-waiters-deleted", None),
    ] {
        let (status, described) = dynamodb(&router, "DescribeTable", &json!({"TableName": table})).await;
        let held = described["Table"]["TableStatus"].as_str();
        match expected {
            Some(wanted) => assert_eq!(
                held,
                Some(wanted),
                "case {case:?}: expected DynamoDB to report {table:?} {wanted}, got {status} \
                 with {described}"
            ),
            None => assert_eq!(
                status, 400,
                "case {case:?}: expected DynamoDB to hold no table {table:?}, got {status} \
                 with {described}"
            ),
        }
    }
}

/// A refusal no acceptor of the waiter names ends the wait, carrying the code and the sentence
/// the service itself answered rather than being polled through to the attempt limit.
#[tokio::test]
async fn a_refusal_no_acceptor_names_ends_the_wait() {
    let router = build_router();
    let role_arn = create_role(&router).await;
    deploy_function(&router, &role_arn).await;

    // A name shorter than the shortest a table may carry, so every poll is refused for the
    // request itself rather than for anything about a table's state.
    let case = "a wait on a refusal no acceptor names";
    let reported = drive(&router, case, &json!({ "case": case, "table": "x" })).await;
    assert_eq!(
        reported["code"],
        json!("ValidationException"),
        "case {case:?}: expected the service's own code, got {reported}"
    );
    let failed = reported["failed"].as_str().unwrap_or_default();
    let expected = "Waiter TableExists failed: An error occurred (ValidationException): ";
    assert!(
        failed.starts_with(expected),
        "case {case:?}: expected the wait to end on {expected:?}, got {failed:?}"
    );
}
