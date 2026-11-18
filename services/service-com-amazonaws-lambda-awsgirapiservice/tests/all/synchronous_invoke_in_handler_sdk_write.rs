// SPDX-License-Identifier: CC0-1.0

//! What a SYNCHRONOUS invocation has already done by the time it answers: the AWS call the
//! handler itself made, read back in the service that owns it.
//!
//! RULE: a `RequestResponse` invocation runs the handler to completion before it answers —
//! "`RequestResponse` (default) – Invoke the function synchronously. Keep the connection open
//! until the function returns a response or times out"
//! (`smithy_query(query="doc", shape="InvocationRequest", member="InvocationType")`) — so a call
//! the handler made through the SDK its managed runtime supplies has ALREADY taken effect in the
//! service that owns it when the caller reads the response: the FIRST read a client makes after
//! that answer finds it. Such a call is made as the function's own execution role, which "grants
//! the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), so one the policies in force on that role do not allow is the
//! owning service's own refusal — which the handler sees, and the caller then reads in the
//! handler's own answer, with nothing written in that service.
//!
//! So a client may rely on work a handler performed during a synchronous invocation having
//! happened when the call returns. A handler whose write silently vanished would turn a
//! completed invocation into a lost record with no error anywhere, and a side channel a handler
//! reports itself on — the one way an independent observer tells whether a function was invoked
//! at all — would report nothing.
//!
//! The rows differ along what the rule generalises over: HOW the handler learns the address of
//! the resource it writes (an environment variable the function carries, a member of the event
//! it was handed), WHAT SHAPE the statement granting that write is written in (its `Action` and
//! `Resource` as single strings and as lists, a namespace wildcard, a bare wildcard over every
//! action and resource), WHICH path the invocation carries, and WHETHER the role allows the
//! write at all (withheld outright, and granted on another resource of the same account) — each
//! of which is a refusal the owning service answers rather than a write that quietly did not
//! happen.
//!
//! Every row asserts both halves: what the invocation answered, and what the QUEUE holds on the
//! first read after that answer. The queue, the role and the token service that mints the role's
//! session are the real ones, mounted on ONE engine exactly as `core/router` assembles them, so
//! the message read back is the one Amazon SQS holds.
//!
//! What the neighbours pin and this suite does not repeat: `handler_secret_relayed_to_queue.rs`
//! drives the two-hop relay and the forms a secret may be named by, and
//! `core/router/tests/all/elbv2_desync_mitigation_before_the_lambda_target.rs` drives the same
//! control leg on the assembled, authenticating router. What only this one drives is the SHAPE of
//! the grant and of the addressing behind one in-handler write, and the refusal each shape earns.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, call_sqs, create_queue, router_on};
use crate::invoke_request_id::{is_request_id, one_invocation_stream_at_a_time};
// The one .zip writer this crate's suites package a Python handler with.
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the module and attribute its package
/// carries the handler as.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what any run
/// needs on any host, because what is under test is the write the handler made and not how fast
/// an interpreter gets through it.
const FUNCTION_TIMEOUT: i64 = 120;

/// The body the handler answers with when its write went through, and when the service refused
/// it.
const REACHED: &str = "TARGET-REACHED";
const REFUSED: &str = "TARGET-REFUSED";

/// The code Amazon SQS refuses a call no policy in force allows with.
const ACCESS_DENIED: &str = "AccessDenied";

/// How many rows the table has, which is how many queues and functions the suite builds for it.
const ROW_COUNT: usize = 6;

/// The handler every row runs: report this invocation to the queue the event names or the
/// environment carries, and answer either the id the send came back with or the refusal that
/// stopped it. The client is built at module scope, which is where a managed runtime's handler
/// builds one.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3
from botocore.exceptions import ClientError

QUEUES = boto3.client("sqs")


def lambda_handler(event, context):
    queue_url = event.get("queue_url") or os.environ["QUEUE_URL"]
    report = {"path": event["path"], "request_id": context.aws_request_id}
    try:
        sent = QUEUES.send_message(
            QueueUrl=queue_url, MessageBody=json.dumps(report, sort_keys=True)
        )
    except ClientError as refused:
        return {
            "statusCode": 502,
            "body": "TARGET-REFUSED",
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
        }
    return {"statusCode": 200, "body": "TARGET-REACHED", "message_id": sent["MessageId"]}
"#;

/// AWS Lambda over the SAME engine as the queue its handlers write to, the IAM store its
/// execution roles live in and the token service their sessions are minted by — what
/// `core/router` assembles. A handler's own SDK call is dispatched back into this router, so the
/// queue it sends to is the one this suite created.
fn side_channel_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// How the handler learns which queue to report to.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// An environment variable the function carries, which is where a function reporting to one
    /// fixed resource reads it.
    Environment,
    /// A member of the event the invocation carried.
    Event,
}

/// What a row must come to.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The send went through: the message is on the queue by the time the call answered.
    Sent,
    /// The role does not allow the send: the handler reports the refusal and the queue is
    /// untouched.
    Refused,
}

/// One row: how the queue is named, the statements of the role's one inline policy, the path the
/// invocation carries, and what it must come to.
struct Row {
    case: &'static str,
    named: Named,
    statements: Value,
    path: &'static str,
    expected: Expected,
}

/// The rows, stated against `arn` — the row's own queue — and `other`, another queue of the same
/// account.
fn rows(arn: &str, other: &str) -> Vec<Row> {
    vec![
        Row {
            case: "the queue named by an environment variable, granted as single strings",
            named: Named::Environment,
            statements: json!([{
                "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": arn,
            }]),
            path: "/direct",
            expected: Expected::Sent,
        },
        Row {
            case: "the queue named by the event, granted with both elements as lists",
            named: Named::Event,
            statements: json!([{
                "Effect": "Allow", "Action": ["sqs:SendMessage"], "Resource": [arn],
            }]),
            path: "/",
            expected: Expected::Sent,
        },
        Row {
            case: "granted through the service's namespace wildcard",
            named: Named::Environment,
            statements: json!([{ "Effect": "Allow", "Action": "sqs:*", "Resource": "*" }]),
            path: "/a/b?x=1",
            expected: Expected::Sent,
        },
        Row {
            case: "granted through a bare wildcard over every action and resource",
            named: Named::Event,
            statements: json!([{ "Effect": "Allow", "Action": "*", "Resource": "*" }]),
            path: "/nested/path/with%20space",
            expected: Expected::Sent,
        },
        Row {
            case: "a role carrying no grant on the service at all",
            named: Named::Environment,
            statements: json!([{
                "Effect": "Allow",
                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            }]),
            path: "/withheld",
            expected: Expected::Refused,
        },
        Row {
            case: "the send granted on another queue of the same account",
            named: Named::Event,
            statements: json!([{
                "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": other,
            }]),
            path: "/elsewhere",
            expected: Expected::Refused,
        },
    ]
}

/// The role one row runs as.
fn role_name(index: usize) -> String {
    format!("in-handler-write-role-{index}")
}

/// The function one row invokes.
fn function_name(index: usize) -> String {
    format!("in-handler-write-{index}")
}

/// Create the function `name`, running as `role_arn`, carrying `queue_url` as the `QUEUE_URL`
/// variable its handler may read.
async fn create_reporter(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    queue_url: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { "QUEUE_URL": queue_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
}

/// Everything the queue `url` addresses hands over RIGHT NOW, as the documents its bodies carry:
/// one `ReceiveMessage` that waits for nothing, which is the first read a client makes after the
/// invocation answered.
async fn taken_now(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, taken) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage to answer 200, got {status} {taken}"
    );
    taken["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let body = message["Body"].as_str().unwrap_or_default();
                    serde_json::from_str(body).unwrap_or(Value::Null)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How Amazon SQS words the refusal of a send no identity-based policy allows: the session of the
/// function's own execution role, the action as that service names it in a refusal, and the queue
/// the send addressed.
fn refusal(role: &str, function: &str, queue_arn: &str) -> String {
    format!(
        "User: arn:aws:sts::{account}:assumed-role/{role}/{function} is not authorized to \
         perform: sqs:sendmessage on resource: {queue_arn} because no identity-based policy \
         allows the sqs:sendmessage action",
        account = ::engine::emulator_account_id(),
    )
}

/// RULE: a synchronous invocation runs the handler to completion before it answers, so the AWS
/// call the handler made has already taken effect in the service that owns it when the caller
/// reads the response — and a call the policies in force on the function's execution role do not
/// allow is that service's own refusal, leaving its state as it was.
#[tokio::test]
async fn a_synchronous_invocation_has_already_made_its_handlers_own_aws_call() {
    let _stream = one_invocation_stream_at_a_time().await;
    let setup = "setup";
    let router = side_channel_router();

    // Another queue of the same account, for the row whose grant names the wrong resource.
    let (_other_url, other_arn) =
        create_queue(&router, setup, "in-handler-write-other", &json!({})).await;
    assert_eq!(
        rows(&other_arn, &other_arn).len(),
        ROW_COUNT,
        "the table and the resources this suite builds for it must be the same length"
    );

    // One queue per row, so what a row reads back is its own invocation's report and nothing
    // left over.
    let mut queues: Vec<(String, String)> = Vec::new();
    for index in 0..ROW_COUNT {
        queues.push(
            create_queue(
                &router,
                setup,
                &format!("in-handler-write-queue-{index}"),
                &json!({}),
            )
            .await,
        );
    }

    // The roles and the functions are created FIRST, so the windows their creates opened run
    // down together rather than one after another.
    for (index, (queue_url, queue_arn)) in queues.iter().enumerate() {
        let row = &rows(queue_arn, &other_arn)[index];
        let role = given_role(
            &router,
            row.case,
            &role_name(index),
            Some(row.statements.clone()),
        )
        .await;
        create_reporter(&router, row.case, &function_name(index), &role, queue_url).await;
    }

    for (index, (queue_url, queue_arn)) in queues.iter().enumerate() {
        let row = &rows(queue_arn, &other_arn)[index];
        let case = row.case;
        let function = function_name(index);
        await_function_active(&router, case, &function).await;

        let mut event = json!({ "path": row.path, "httpMethod": "POST" });
        if matches!(row.named, Named::Event) {
            event["queue_url"] = json!(queue_url);
        }
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            &event,
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected Invoke to answer 200, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the handler must run to completion, got the function error {:?} \
             with {}",
            answered.header("x-amz-function-error"),
            answered.text()
        );
        let payload = answered.json();

        // The first read a client makes after that answer.
        let held = taken_now(&router, case, queue_url).await;

        match row.expected {
            Expected::Sent => {
                assert_eq!(
                    payload["body"],
                    json!(REACHED),
                    "case {case:?}: expected the handler's own answer, got {payload}"
                );
                let message_id = payload["message_id"].as_str().unwrap_or_default();
                assert!(
                    is_request_id(message_id),
                    "case {case:?}: expected the MessageId Amazon SQS minted, got \
                     {message_id:?} in {payload}"
                );
                assert_eq!(
                    held.len(),
                    1,
                    "case {case:?}: the write the handler made must be on the queue by the time \
                     the invocation answered, got {held:?}"
                );
                assert_eq!(
                    held[0]["path"],
                    json!(row.path),
                    "case {case:?}: expected the report to carry the path the invocation \
                     carried, got {}",
                    held[0]
                );
                let reported = held[0]["request_id"].as_str().unwrap_or_default();
                assert!(
                    is_request_id(reported),
                    "case {case:?}: expected the report to carry this invocation's own request \
                     id, got {reported:?} in {}",
                    held[0]
                );
            }
            Expected::Refused => {
                assert_eq!(
                    payload["body"],
                    json!(REFUSED),
                    "case {case:?}: expected the handler to report the refusal, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected the code Amazon SQS answered, got {payload}"
                );
                let expected = refusal(&role_name(index), &function, queue_arn);
                assert_eq!(
                    payload["message"],
                    json!(expected),
                    "case {case:?}: expected the refusal {expected:?}, got {payload}"
                );
                assert!(
                    held.is_empty(),
                    "case {case:?}: a refused send must leave the queue as it was, got {held:?}"
                );
            }
        }
    }
}
