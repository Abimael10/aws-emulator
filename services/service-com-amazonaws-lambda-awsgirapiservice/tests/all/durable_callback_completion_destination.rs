// SPDX-License-Identifier: CC0-1.0

//! What becomes of a durable execution whose handler waits for a CALLBACK longer than one
//! invocation's own run may last, and of the completion that execution owes.
//!
//! RULE: a durable execution's wait for something OUTSIDE itself is the EXECUTION's and not the
//! invocation's — the execution suspends until a client signals the callback or the window the
//! callback was configured with closes — so neither the function's `Timeout` nor this host's
//! ceiling on one run is spent by that wait: the execution stays `RUNNING` while the callback
//! is open however long that is, the waiting handler receives what a client sends to it (or is
//! told the window closed), and the invocation record the execution's completion owes reaches
//! the destination its asynchronous-invocation configuration names
//! (`DestinationConfig$OnSuccess` for an execution that succeeded, `$OnFailure` for one that
//! failed).
//!
//! Why it matters: a callback is signalled by something outside this emulator, on its own
//! schedule. An execution whose wait is charged to the invocation's own span is cut off the
//! moment that span is up, so the signal reaches a run that is already over: the execution ends
//! in a failure nothing asked for, the handler's work after the wait never happens, and the
//! client watching the destination for the outcome of its workflow is told nothing at all.
//!
//! The rows differ along what the rule generalises over: HOW the wait ends (a client that
//! signals the callback, and a window that closes with nothing sent), which destination slot
//! the completion is therefore delivered to, and the outcome the execution and the record
//! report. Both rows configure a callback window LONGER than the function's own `Timeout`,
//! which is the span the rule is about.
//!
//! Each row also asserts the half the handler itself owns: the durable context HANDS the id it
//! minted to the callable the handler gave `wait_for_callback`, which is how a handler carries
//! that id to wherever the external work is — here, through the AWS SDK this runtime supplies,
//! onto a queue of this account, which is what a client holding nothing but that queue needs in
//! order to signal the callback at all.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, send, Answer, ACCOUNT, REGION, ROLE,
};

/// Where `Invoke` is bound, and where `PutFunctionEventInvokeConfig` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// Where the three callback operations are bound.
const CALLBACKS_URI: &str = "/2025-12-01/durable-execution-callbacks";

/// The request headers `Invoke$DurableExecutionName` and `Invoke$InvocationType` are bound to,
/// and the response header `Invoke$DurableExecutionArn` is bound to.
const NAME_HEADER: &str = "x-amz-durable-execution-name";
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const ARN_HEADER: &str = "x-amz-durable-execution-arn";

/// The module the handler lives in, and the handler itself.
const HANDLER_MODULE: &str = "lambda_function.py";
/// See [`HANDLER_MODULE`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function `Timeout` every row runs under, in seconds: the span the rule is about is
/// LONGER than this, so a wait charged to the invocation's own run would end every row.
const FUNCTION_TIMEOUT: u64 = 5;

/// The `DurableConfig$ExecutionTimeout` every row runs under, in seconds: far longer than any
/// row takes, so no execution times out as a whole.
const EXECUTION_TIMEOUT: u64 = 3_600;

/// The callback window the row a CLIENT signals runs under, in seconds: far longer than the
/// signalling takes, so nothing about that row turns on how fast the host runs — and longer
/// than [`FUNCTION_TIMEOUT`], which is what the rule is about.
const SIGNALLED_WINDOW: u64 = 600;

/// The callback window the row NOBODY signals runs under, in seconds. This is the one limit
/// that row is about, so it is the one span here that is short; it is still longer than
/// [`FUNCTION_TIMEOUT`], because a window that closes INSIDE the invocation's own span would
/// say nothing about the rule.
const CLOSING_WINDOW: u64 = 8;

/// How long the signalled row keeps looking at an execution that must stay `RUNNING` while its
/// callback is open: a generous multiple of [`FUNCTION_TIMEOUT`], so an execution cut off by
/// the invocation's own span is seen inside it.
const STILL_RUNNING_WINDOW: Duration = Duration::from_secs(3 * FUNCTION_TIMEOUT);

/// How long a row keeps looking for something the emulator owes before reporting that it never
/// arrived. Generous: it bounds a loop that polls, so a loaded host is waited for rather than
/// failed.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long between two looks of such a loop.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `ExecutionStatus` values this suite reads
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const RUNNING: &str = "RUNNING";
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// The `EventType` of the callback a handler's wait mints
/// (`smithy_query(query="shape",shape="EventType")`).
const CALLBACK_STARTED: &str = "CallbackStarted";

/// The `requestContext.condition` of the invocation record of a handler that RETURNED, and of
/// an event whose attempts are spent — the documented conditions of a destination's record.
const CONDITION_SUCCESS: &str = "Success";
const CONDITION_RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The `MaximumRetryAttempts` every configuration below states: none, the floor of that
/// member's own `@range(min: 0, max: 2)`, so each event has exactly one delivery attempt.
const NO_RETRIES: i64 = 0;

/// One row: how the wait ends, and what the execution and the record must then report.
struct Row {
    case: &'static str,
    /// The window the callback is configured with, in seconds.
    window: u64,
    /// Whether a client signals the callback while it is open.
    signalled: bool,
    /// The `ExecutionStatus` the execution must settle in.
    status: &'static str,
    /// The `requestContext.condition` the record must report.
    condition: &'static str,
}

/// The class the rule covers.
const ROWS: &[Row] = &[
    Row {
        case: "a client that signals the callback long after the invocation's own span",
        window: SIGNALLED_WINDOW,
        signalled: true,
        status: SUCCEEDED,
        condition: CONDITION_SUCCESS,
    },
    Row {
        case: "a window that closes past the invocation's own span with nothing sent",
        window: CLOSING_WINDOW,
        signalled: false,
        status: FAILED,
        condition: CONDITION_RETRIES_EXHAUSTED,
    },
];

/// The result the signalling row sends to the callback, which is what the waiting handler
/// returns.
fn sent_result() -> Value {
    json!({ "charged": true, "reference": "ch-42" })
}

/// The handler every row runs: it waits for a callback, hands the id the context minted to its
/// own callable — which sends it to the queue the event names, the way a handler carries that
/// id to the external work — and returns what the wait answered. It catches nothing, so a
/// window that closes FAILS the execution with the error the model's own callback timeout
/// carries.
const CALLBACK_HANDLER: &str = r#"
import boto3

from aws_durable_execution_sdk_python import Duration, durable_execution
from aws_durable_execution_sdk_python.config import WaitForCallbackConfig


@durable_execution
def lambda_handler(event, context):
    queues = boto3.client("sqs")

    def notify(callback_id):
        queues.send_message(QueueUrl=event["callback_queue"], MessageBody=callback_id)

    config = WaitForCallbackConfig(
        timeout=Duration.from_seconds(event["window_seconds"]),
        name="await-the-charge",
    )
    answered = context.wait_for_callback(notify, config=config)
    return {"answered": answered}
"#;

/// Lambda beside the service the destination belongs to, on ONE engine: the record is asserted
/// where a client reads it, off the queue the configuration names.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router
}

/// One Amazon SQS call (`POST /` with `X-Amz-Target: AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/sqs/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A queue of this account: its url, and the ARN a destination names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue {name:?} answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    (url, format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"))
}

/// The first message waiting on the queue `url` addresses, read as the TEXT it carries, looked
/// for again until it is there or [`PATIENCE`] runs out.
///
/// What it waits for is owed by the emulator — by the handler that was handed the callback id,
/// or by the completion of the execution — so this waits for the CONDITION rather than for a
/// span.
async fn message_on(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        let (status, received) = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ReceiveMessage answered {status} {received}"
        );
        if let Some(body) = received["Messages"]
            .as_array()
            .and_then(|messages| messages.first())
            .and_then(|message| message["Body"].as_str())
        {
            return body.to_string();
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: expected a message on {url}, got none");
}

/// The invocation record on the destination queue, read as the JSON document it carries.
async fn record_on(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let body = message_on(router, case, url).await;
    serde_json::from_str(&body).unwrap_or(Value::String(body))
}

/// A .zip deployment package holding [`CALLBACK_HANDLER`] as the handler's module.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file(HANDLER_MODULE, options)
            .expect("the entry starts");
        writer
            .write_all(CALLBACK_HANDLER.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// A durable function running [`CALLBACK_HANDLER`], with the destination slots of its
/// asynchronous-invocation configuration pointed at `destination`.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    destination: &str,
) {
    let created = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "DurableConfig": { "ExecutionTimeout": EXECUTION_TIMEOUT },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction {name:?} answered {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(engine);
    let configured = json_call(
        router,
        "PUT",
        &format!("{EVENT_INVOKE_CONFIG_URI}/{name}/event-invoke-config"),
        &json!({
            "MaximumRetryAttempts": NO_RETRIES,
            "DestinationConfig": {
                "OnSuccess": { "Destination": destination },
                "OnFailure": { "Destination": destination },
            },
        }),
    )
    .await;
    assert_eq!(
        configured.status,
        200,
        "case {case:?}: PutFunctionEventInvokeConfig answered {} {}",
        configured.status,
        String::from_utf8_lossy(&configured.body)
    );
}

/// `GetDurableExecution` on `arn`. The label is not greedy, so the ARN's slashes are
/// percent-encoded exactly as an SDK encodes them.
async fn get_execution(router: &MultiServiceRouter, arn: &str) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2025-12-01/durable-executions/{}", arn.replace('/', "%2F")),
        &Value::Null,
    )
    .await
}

/// The `Status` the execution `arn` reports right now.
async fn status_of(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let read = get_execution(router, arn).await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDurableExecution must answer 200, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()["Status"].as_str().unwrap_or_default().to_string()
}

/// The execution `arn` once it has SETTLED, looked for again until it has or [`PATIENCE`] runs
/// out.
async fn settled(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = get_execution(router, arn).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDurableExecution must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        if last["Status"].as_str() != Some(RUNNING) {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: expected the execution to settle, got {last}");
}

/// The id of the callback the handler's wait minted, off the `CallbackStarted` event the
/// execution's history took — looked for again until the event is there or [`PATIENCE`] runs
/// out, because the run is what mints it.
async fn callback_id(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = json_call(
            router,
            "GET",
            &format!(
                "/2025-12-01/durable-executions/{}/history",
                arn.replace('/', "%2F")
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDurableExecutionHistory must answer 200, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        let minted = last["Events"]
            .as_array()
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event["EventType"].as_str() == Some(CALLBACK_STARTED))
            })
            .and_then(|event| event["CallbackStartedDetails"]["CallbackId"].as_str())
            .map(str::to_string);
        if let Some(minted) = minted {
            return minted;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: expected the handler's wait to mint a callback, got {last}");
}

/// RULE: a durable execution's wait for a callback is spent by neither the function's `Timeout`
/// nor this host's ceiling on one run, so the execution stays running while the callback is
/// open, the waiting handler receives what the wait came to, and the completion's invocation
/// record reaches the destination the configuration names for that outcome.
#[tokio::test]
async fn a_durable_execution_waits_for_its_callback_past_one_runs_own_span() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let function = format!("durable-callback-destination-{index}");
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("durable-callback-dest-{index}")).await;
        // The queue the HANDLER carries the callback id to, which is a resource of the
        // handler's own and no destination of Lambda's.
        let (callback_queue_url, _) =
            given_queue(&router, case, &format!("durable-callback-ids-{index}")).await;
        given_function(&router, &engine, case, &function, &queue_arn).await;

        // The execution is started OFF the response path, which is what lets a client signal
        // the callback while the handler is still waiting on it.
        let invoked = send(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            "application/json",
            serde_json::to_vec(&json!({
                "window_seconds": row.window,
                "callback_queue": callback_queue_url,
            }))
            .expect("the event serializes"),
            &[
                (NAME_HEADER, format!("exec-{index}")),
                (INVOCATION_TYPE_HEADER, "Event".to_string()),
            ],
        )
        .await;
        assert_eq!(
            invoked.status,
            202,
            "case {case:?}: an asynchronous invocation must be accepted, got {} {}",
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(ARN_HEADER)
            .unwrap_or_else(|| panic!("case {case:?}: the invocation must answer an execution ARN"));

        // The handler has reached its wait: the callback is minted and the execution's history
        // carries the id a client signals it by.
        let callback = callback_id(&router, case, &arn).await;

        // ... and the handler was HANDED that same id, which it carried to the queue of its
        // own: a client holding nothing but that queue can signal this callback.
        let carried = message_on(&router, case, &callback_queue_url).await;
        assert_eq!(
            carried, callback,
            "case {case:?}: expected the handler to carry the id the context minted \
             ({callback:?}) to its own queue, got {carried:?}"
        );

        if row.signalled {
            // The execution stays RUNNING while that callback is open — looked at again and
            // again across a span LONGER than the function's own `Timeout`, which is the span
            // an execution whose wait was charged to its invocation would have been cut off
            // inside.
            let watching_until = Instant::now() + STILL_RUNNING_WINDOW;
            while Instant::now() < watching_until {
                let status = status_of(&router, case, &arn).await;
                assert_eq!(
                    status, RUNNING,
                    "case {case:?}: expected the execution to stay {RUNNING} while its callback \
                     is open, got {status:?}"
                );
                tokio::time::sleep(LOOK_AGAIN).await;
            }
            let sent = send(
                &router,
                "POST",
                &format!("{CALLBACKS_URI}/{callback}/succeed"),
                "application/octet-stream",
                serde_json::to_vec(&sent_result()).expect("the result serializes"),
                &[],
            )
            .await;
            assert_eq!(
                sent.status,
                200,
                "case {case:?}: a success inside the window must be accepted, got {} {}",
                sent.status,
                String::from_utf8_lossy(&sent.body)
            );
        }

        // The execution settles on what the wait came to.
        let reported = settled(&router, case, &arn).await;
        assert_eq!(
            reported["Status"].as_str(),
            Some(row.status),
            "case {case:?}: expected the execution to settle {:?}, got {reported}",
            row.status
        );
        if row.signalled {
            let result: Value = reported["Result"]
                .as_str()
                .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            assert_eq!(
                result,
                json!({ "answered": sent_result() }),
                "case {case:?}: expected the handler to receive what the client sent, got \
                 {reported}"
            );
        } else {
            // The window closed, and the error the execution carries is the callback's own
            // timeout rather than anything about the invocation's span.
            let error_type = reported["Error"]["ErrorType"].as_str().unwrap_or_default();
            assert!(
                error_type.contains("CallbackTimeout"),
                "case {case:?}: expected the execution to fail on the callback's own timeout, \
                 got {reported}"
            );
        }

        // ... and the completion the execution owes reaches the destination the configuration
        // names, reporting the condition the outcome is.
        let record = record_on(&router, case, &queue_url).await;
        assert_eq!(
            record["requestContext"]["condition"].as_str(),
            Some(row.condition),
            "case {case:?}: expected the record to report the condition {:?}, got {record}",
            row.condition
        );
        let named = record["requestContext"]["functionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            named.contains(&function),
            "case {case:?}: expected the record to name the function it is of, got {record}"
        );
    }
}
