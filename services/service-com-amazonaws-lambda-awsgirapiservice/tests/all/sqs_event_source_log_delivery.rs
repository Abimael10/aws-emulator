// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of a batch an event source mapping on an Amazon SQS queue delivered: the
//! handler's own log lines in the function's CloudWatch Logs group, and the queue drained of
//! the messages that invocation processed.
//!
//! `CreateEventSourceMapping` "creates a mapping between an event source and an Lambda Event
//! Function. Lambda reads items from the event source and invokes the function", and `Enabled`
//! is "when true, the event source mapping is active. When false, Lambda pauses polling and
//! invocation" — so the RULE asserted here, in one sentence: while a mapping on a queue reports
//! `Enabled`, each batch of that queue's messages is handed to the mapping's function by the
//! mapping's own poller with NO client action at all, and that invocation is a full invocation
//! of the function, so its log is delivered to the function's log group — the one its
//! `LoggingConfig$LogGroup` names, else the default `/aws/lambda/<function name>` — and the
//! messages it processed are deleted from the queue.
//!
//! That is the pair a client has to watch a consumer by: nothing about a mapping's poller is
//! readable until the handler's own output appears in the group and the queue drains, so a
//! mapping that reported `Enabled` while nothing was delivered would show up here as a group
//! holding no line of the handler and a queue still holding its messages.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with Lambda,
//! Amazon SQS and CloudWatch Logs on ONE engine, as `core/router` assembles them: the queue is
//! created and written through Amazon SQS's own surface, the function carries a REAL deployment
//! package whose handler prints a line per record it was handed, and the log is read back with
//! CloudWatch Logs' `DescribeLogGroups`, `DescribeLogStreams` and `GetLogEvents` — so a line no
//! handler wrote, or a message no invocation consumed, fails here.
//!
//! The rows differ along what the rule generalises over: what the bodies are (plain text, JSON,
//! non-ASCII text), how many messages are waiting, how many one batch may hold (so a row is
//! delivered in one batch and a row one message per batch), and which group the function logs
//! to (the default, and one its `LoggingConfig` names). The second test varies how the message
//! ARRIVED on the polled queue: it is a RETRY queue, reached by the move a source queue's own
//! `RedrivePolicy` owes a message no consumer there processed, and its mapping consumes and
//! logs it exactly as it does one that was sent straight to it.
//!
//! Nothing here waits a span of its own choosing: every state a test needs is waited for by
//! looking again until it holds ([`WATCH_BUDGET`] is only the backstop against a chain that
//! never gets there), so no assertion depends on how fast the host runs.
//!
//! Both tests RUN handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process, and a suite that
//! keeps several of those slots busy starves every other suite's runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    call_sqs, create_queue, get_mapping, given_enabled_mapping, queue_held, router_on,
    send_message, LOOK_AGAIN,
};
use crate::invocation_log_delivery::{
    create_function, events, groups, streams, Created, PYTHON_RUNTIME,
};

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted beside the three services below, so nothing is known about the role and no read or
/// write of the chain is withheld from it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long every polled queue below hides a message it hands over for, in seconds. It is
/// above the `Timeout` the functions here are created with (ten seconds), which is what a
/// mapping between the two is held to, and long enough that a message an invocation is working
/// on is not handed over a second time while it runs.
const VISIBILITY_TIMEOUT_SECONDS: i64 = 30;

/// How long the SOURCE queue of the retry-queue test hides a message for: short, so the
/// delivery its `RedrivePolicy` allows is used up and the move to the retry queue becomes due
/// promptly. No mapping is made on that queue, so it is held to no function's `Timeout`.
const SOURCE_VISIBILITY_TIMEOUT_SECONDS: i64 = 1;

/// How long a test watches for the state the clock owes it — the mapping's poller taking a
/// batch, the invocation running, its log reaching the group and the queue draining — before
/// it is a failure.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged. The bound is
/// taken from what the EMULATOR itself allows ONE attempt rather than picked here — the wait
/// for a share of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]) — with room for the several passes a row waits for, because a
/// whole-workspace run shares its cores with every other suite.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(30));

/// What the handler prints before the body of each record it was handed. ONE spelling of it,
/// bound into the handler's source and into what the assertions look for.
const MARKER: &str = "processed body: ";

/// The handler every function below runs: it prints a line per record of the batch it was
/// handed, taken from that record's own body, so a line in the log group is a consequence of
/// the record having reached the handler.
///
/// It also asserts the envelope Lambda hands an Amazon SQS batch over as, so a batch delivered
/// in some other shape fails the run rather than being logged.
fn logging_handler() -> String {
    format!(
        r#"
MARKER = "{MARKER}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        if record["eventSource"] != "aws:sqs":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if not record["messageId"]:
            raise ValueError("a record carries no messageId")
        print(MARKER + record["body"])
    return len(records)
"#
    )
}

/// The line the handler writes for the message whose body is `body`.
fn expected_line(body: &str) -> String {
    format!("{MARKER}{body}")
}

/// Lambda, Amazon SQS and CloudWatch Logs over ONE shared engine — the three services this
/// chain spans, mounted the way the production router mounts them.
///
/// `pub(crate)` for the reason [`logged_lines`] is: the suite that watches a partial-batch
/// report feed a queue's redrive (`tests/all/sqs_event_source_partial_batch_redrive.rs`) spans
/// the same three services, so it mounts them through this one harness.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// Every log line the group `group` holds, across every stream of it, as a client reads them:
/// `DescribeLogStreams` and then `GetLogEvents` on each stream.
///
/// A group no delivery has created yet holds none, which is what this answers while the chain
/// is still in flight.
///
/// `pub(crate)` because the suite that reads a handler's own line for another rule
/// (`tests/all/sqs_event_source_revoked_downstream_grant.rs`) reads the group through this one
/// reader rather than a second copy of it.
pub(crate) async fn logged_lines(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for stream in streams(router, case, group).await {
        for (_, message) in events(router, case, group, &stream).await {
            lines.push(message);
        }
    }
    lines
}

/// Watch the queue `url` addresses and the log group `group` until the queue holds nothing and
/// every line of `wanted` has been delivered, answering what was last read as
/// `(messages held, lines)`.
///
/// Both halves of the rule are waited for TOGETHER: the delivery of a batch, the run of the
/// handler, the write of its log and the deletion of the messages are one chain, and a reader
/// that stopped at the first of them would be reading an instant of its own choosing in the
/// middle of it.
async fn drained_and_logged(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    group: &str,
    wanted: &[String],
) -> (u32, Vec<String>) {
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        let lines = logged_lines(router, case, group).await;
        let held = queue_held(router, case, url).await;
        let settled = held == 0
            && wanted
                .iter()
                .all(|line| lines.iter().any(|written| written.contains(line)));
        if settled || Instant::now() >= deadline {
            return (held, lines);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the bodies waiting on the queue, how many of them one batch may hold, and the group
/// the function's configuration names (`None` to take the default).
struct Row {
    case: &'static str,
    bodies: &'static [&'static str],
    batch_size: i64,
    log_group: Option<&'static str>,
}

/// The catalog: one message and several, bodies that are plain text, JSON and non-ASCII text, a
/// batch ceiling above what is waiting and one message per batch, and both the default log group
/// and one the function's `LoggingConfig` names.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one plain-text body, logged to the default group",
            bodies: &["retry-me-once"],
            batch_size: 10,
            log_group: None,
        },
        Row {
            case: "a json body and a non-ascii one in one batch",
            bodies: &[r#"{"order":17,"items":["a","b"]}"#, "ünïcödé ⚑ body"],
            batch_size: 10,
            log_group: None,
        },
        Row {
            case: "two bodies read one per batch, logged to a group the function names",
            bodies: &["first-of-two", "second-of-two"],
            batch_size: 1,
            log_group: Some("/aws/lambda/sqs-mapping-named-group"),
        },
    ]
}

/// RULE: a message waiting on a queue whose mapping reports `Enabled` is handed to the mapping's
/// function with no client action, that invocation's log is delivered to the function's log
/// group, and the message is deleted from the queue.
#[tokio::test]
async fn a_batch_a_mapping_delivered_is_logged_to_the_functions_group_and_leaves_the_queue() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let function = format!("sqs-mapping-log-{index}");
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("sqs-mapping-log-{index}"),
            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        let source = logging_handler();
        create_function(
            &router,
            &Created {
                case,
                name: &function,
                source: &source,
                runtime: PYTHON_RUNTIME,
                role: ROLE,
                log_group: row.log_group,
            },
        )
        .await;
        let uuid = given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": arn,
                "BatchSize": row.batch_size,
                "Enabled": true,
            }),
        )
        .await;

        for body in row.bodies {
            send_message(&router, case, &url, body, None).await;
        }

        // Where the log has to be: the group the function's configuration names, else the
        // default its own documentation gives.
        let group = row
            .log_group
            .map_or_else(|| format!("/aws/lambda/{function}"), str::to_string);
        let wanted: Vec<String> = row.bodies.iter().map(|body| expected_line(body)).collect();
        let (held, lines) = drained_and_logged(&router, case, &url, &group, &wanted).await;

        // The handler ran on every message: its own line for each body is an event of the
        // function's group.
        for (body, line) in row.bodies.iter().zip(&wanted) {
            assert!(
                lines.iter().any(|written| written.contains(line)),
                "case {case:?}: the handler's line for {body:.40?} must be delivered to \
                 {group:?}, got {lines:?}"
            );
        }
        // And the queue drained: a message leaves it once the invocation it was handed to
        // processed it.
        assert_eq!(
            held, 0,
            "case {case:?}: the queue must hold no message once its batch was processed, it \
             holds {held}"
        );
        // The group the lines are in is the one the rule names, reported by CloudWatch Logs
        // itself rather than inferred from the read above.
        let named = groups(&router, case, &group).await;
        assert!(
            named.iter().any(|found| found == &group),
            "case {case:?}: the delivery must have created {group:?}, got {named:?}"
        );
        // The mapping says the attempt it made of the batch succeeded, which is what deleted
        // the messages.
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            reading.string("LastProcessingResult").as_deref(),
            Some(LAST_PROCESSING_RESULT_OK),
            "case {case:?}: the mapping must report the attempt that consumed its batch as \
             {LAST_PROCESSING_RESULT_OK}, got {}",
            reading.text()
        );
    }
}

/// RULE, over the other way a message reaches a polled queue: a queue whose mapping reports
/// `Enabled` is consumed whatever put the message there, so a message a source queue's
/// `RedrivePolicy` moved to it — the retry path — is handed to the mapping's function, logged
/// by it and deleted, exactly as one sent straight to the queue is.
#[tokio::test]
async fn a_message_redriven_to_a_retry_queue_is_consumed_through_that_queues_mapping() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a message moved to the retry queue its source's redrive policy names";
    let router = emulator();
    let function = "sqs-retry-queue-consumer";
    // The retry queue first: the source queue's policy names it, so it has to exist before
    // that policy is accepted.
    let (retry_url, retry_arn) = create_queue(
        &router,
        case,
        "sqs-retry-queue",
        &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT_SECONDS.to_string() }),
    )
    .await;
    let (source_url, _source_arn) = create_queue(
        &router,
        case,
        "sqs-retry-queue-source",
        &json!({
            "VisibilityTimeout": SOURCE_VISIBILITY_TIMEOUT_SECONDS.to_string(),
            // "The number of times a message is delivered to the source queue before being
            // moved to the dead-letter queue."
            "RedrivePolicy": json!({
                "deadLetterTargetArn": retry_arn,
                "maxReceiveCount": 1,
            })
            .to_string(),
        }),
    )
    .await;
    let source = logging_handler();
    create_function(
        &router,
        &Created {
            case,
            name: function,
            source: &source,
            runtime: PYTHON_RUNTIME,
            role: ROLE,
            log_group: None,
        },
    )
    .await;
    // The mapping is on the RETRY queue, which is the pair this test is about: nothing polls
    // the source queue.
    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": retry_arn,
            "BatchSize": 10,
            "Enabled": true,
        }),
    )
    .await;

    let body = "retry-me-through-the-mapping";
    send_message(&router, case, &source_url, body, None).await;

    // A consumer of the SOURCE queue takes the one delivery its policy allows and never
    // deletes the message, so the move to the retry queue falls due. The receives are made
    // until the source queue has let the message go — the move is owed at the receive that
    // finds the message visible again with its allowance spent, so the test looks again
    // rather than assuming one receive is enough.
    let deadline = Instant::now() + WATCH_BUDGET;
    let on_source = loop {
        let (status, answered) = call_sqs(
            &router,
            "ReceiveMessage",
            &json!({
                "QueueUrl": source_url,
                "MaxNumberOfMessages": 10,
                "VisibilityTimeout": SOURCE_VISIBILITY_TIMEOUT_SECONDS,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ReceiveMessage to answer 200, got {status} {answered}"
        );
        let held = queue_held(&router, case, &source_url).await;
        if held == 0 || Instant::now() >= deadline {
            break held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        on_source, 0,
        "case {case:?}: the source queue must let the message go once its redrive policy is \
         spent, it holds {on_source}"
    );

    // And the retry queue's own mapping consumes it: the handler logs the body and the queue
    // drains, with no client action on the retry queue at all.
    let group = format!("/aws/lambda/{function}");
    let wanted = vec![expected_line(body)];
    let (held, lines) = drained_and_logged(&router, case, &retry_url, &group, &wanted).await;
    assert!(
        lines
            .iter()
            .any(|written| written.contains(&expected_line(body))),
        "case {case:?}: the handler's line for the redriven message must be delivered to \
         {group:?}, got {lines:?}"
    );
    assert_eq!(
        held, 0,
        "case {case:?}: the retry queue must drain once its mapping's function processed the \
         message, it holds {held}"
    );
    let reading = get_mapping(&router, &uuid).await;
    assert_eq!(
        reading.string("LastProcessingResult").as_deref(),
        Some(LAST_PROCESSING_RESULT_OK),
        "case {case:?}: the mapping must report the attempt that consumed the redriven message \
         as {LAST_PROCESSING_RESULT_OK}, got {}",
        reading.text()
    );
}
