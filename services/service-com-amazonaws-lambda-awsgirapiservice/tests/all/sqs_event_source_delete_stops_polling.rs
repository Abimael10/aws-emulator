// SPDX-License-Identifier: CC0-1.0

//! What DELETING an event source mapping does to the consumption its poller was making:
//! `DeleteEventSourceMapping` "deletes an event source mapping … When you delete an event
//! source mapping, it enters a Deleting state and might not be completely deleted for
//! several seconds."
//!
//! RULE: a mapping only polls while this account holds it — "Lambda reads items from the
//! event source and invokes the function" (`CreateEventSourceMapping`) is what a HELD
//! mapping does — so from the moment a delete is accepted, no pass of that mapping reads its
//! event source again: a message sent to the source afterwards is neither handed to the
//! function nor taken out of the source, and stays there for an ordinary consumer to
//! receive. The teardown is observable while it happens, because the `Deleting` state the
//! delete answers with is one a client reads back ("might not be completely deleted for
//! several seconds") before the removal the clock owes falls due.
//!
//! The suite asserts BOTH halves on the same pair, because the negative half means nothing
//! without the positive one: first that the mapping really was consuming (a message sent
//! before the delete reaches the function, which relays the envelope it was handed to a
//! second queue with the SDK its managed runtime supplies, and leaves the source drained),
//! and then that nothing is consumed after it.
//!
//! Everything here is driven over the wire on ONE engine, through the services that own the
//! halves: Amazon SQS holds both queues, IAM holds the execution role the poller reads and
//! the handler sends as, and Lambda holds the function and the mapping. The passes are the
//! suite's OWN ([`DrivenPolling`]): a pass is background work and nothing a client can read
//! says whether one has happened, so the negative half is asserted after passes this suite
//! MADE rather than after a span it waited — a removed mapping that was still polling would
//! consume inside those very passes.
//!
//! The rows differ along what the rule generalises over: the `BatchSize` the mapping reads
//! in (one message per batch, and a ceiling above what is waiting) and HOW MANY messages are
//! sent after the delete (one, and several).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    EVENT_SOURCE_MAPPING_STATE_DELETING, LAST_PROCESSING_RESULT_OK,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, QUEUE_READS};
use crate::event_source_mappings::{
    await_function_active, call, call_sqs, create_queue, get_mapping, given_enabled_mapping,
    queue_message_counts, send_message, DrivenPolling, LOOK_AGAIN, MAPPINGS_URI,
};
use crate::event_source_poll_authorization::{driven_emulator_with_queues, BLOCKED_PASSES};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the relay is created with, and the module and attribute its package exposes
/// its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `eventSource` every record of a queue-sourced event carries.
const EVENT_SOURCE: &str = "aws:sqs";

/// How long one invocation of the relay is given, as the function's own `Timeout`: well past
/// what a run needs, because what is under test is the mapping's lifecycle and not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: i64 = 30;

/// How long each source queue hides a message it hands over for. A mapping is made onto a
/// function the queue outlasts, so this is above [`FUNCTION_TIMEOUT`] — and it is also what
/// makes the negative half sharp: a message a pass had taken would stay hidden for this long,
/// so a source that still reports its messages VISIBLE is one nothing polled.
const VISIBILITY_TIMEOUT: i64 = 60;

/// The environment variable the relay reads the sink queue's url from.
const SINK_VARIABLE: &str = "SINK_QUEUE_URL";

/// How long this suite waits for work it cannot make happen itself — the function going
/// `Active`, the removal the delete owes, the handler's own run inside a pass — before the
/// row is a failure.
///
/// A CEILING on patience taken from what the EMULATOR allows ONE attempt rather than picked
/// here: the wait for a share of this process's run capacity ([`PERMIT_WAIT`]) and the wait
/// for the run's own answer ([`HOST_WAIT_CEILING`]), with room beside them, because a
/// whole-workspace run shares its cores with every other suite. It is patience and not a
/// weakened expectation: every assertion below is unchanged by it.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(30));

/// The handler every row runs: relay the whole envelope it was handed to the queue its
/// environment names, with the SDK its managed runtime supplies.
///
/// The client is built at module scope, which is where a managed runtime's handler builds it.
fn relay_handler() -> String {
    format!(
        r#"
import json
import os

import boto3

QUEUES = boto3.client("sqs")
SINK_URL = os.environ["{SINK_VARIABLE}"]


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(event))
    return {{"relayed": [record["messageId"] for record in records]}}
"#
    )
}

/// Create the relay running as `role_arn`, pointed at the sink queue `sink_url` addresses,
/// and wait until it reports `Active`.
async fn create_relay(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    sink_url: &str,
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
            "Environment": { "Variables": { SINK_VARIABLE: sink_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&relay_handler())),
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
    await_function_active(router, case, name).await;
}

/// Take everything the queue `url` addresses hands over right now, DELETING each message so
/// a later look finds only what arrived after it: the body of each.
async fn take_everything(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, answered) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage to answer 200, got {status} {answered}"
    );
    let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::with_capacity(handed.len());
    for message in &handed {
        let (status, deleted) = call_sqs(
            router,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteMessage to answer 200, got {status} {deleted}"
        );
        bodies.push(
            message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a message with no Body: {message}"))
                .to_string(),
        );
    }
    bodies
}

/// Make passes until the sink queue holds a relayed envelope, and answer it as the document
/// the handler sent.
///
/// The wait is for the ENVELOPE, not for a span: each pass is this suite's own, and a host
/// slow to run a handler delays the loop instead of weakening what it asserts.
async fn relayed_envelope(
    router: &MultiServiceRouter,
    driven: &mut DrivenPolling,
    case: &str,
    sink_url: &str,
) -> Value {
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        driven.poll_once().await;
        if let Some(body) = take_everything(router, case, sink_url)
            .await
            .into_iter()
            .next()
        {
            return serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: the relayed envelope is not a document: {body} ({error})")
            });
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must hand its function the message it polled, and the \
             handler relayed nothing to the sink queue"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `DeleteEventSourceMapping` on the mapping `uuid` names.
async fn delete_mapping(
    router: &MultiServiceRouter,
    uuid: &str,
) -> crate::event_source_mappings::Answer {
    call(
        router,
        "DELETE",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &Value::Null,
    )
    .await
}

/// Wait until the mapping `uuid` named is held no more, which is the removal the delete owed:
/// a read of it is then the modelled not-found.
async fn await_removed(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        let read = get_mapping(router, uuid).await;
        if read.status == 404 {
            assert!(
                read.error_code().contains("ResourceNotFoundException"),
                "case {case:?}: a removed mapping must be the modelled not-found, got {}",
                read.text()
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} was never removed, last read {} {}",
            read.status,
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: what the mapping reads in, and how many messages are sent once it is gone.
struct Row {
    case: &'static str,
    slug: &'static str,
    batch_size: i64,
    /// How many messages the row sends after the delete, each of which must stay put.
    after: usize,
}

/// The rows: one message per batch against a ceiling above what is waiting, and one message
/// sent after the delete against several.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one message per batch, one message sent after the delete",
            slug: "esm-stop-single",
            batch_size: 1,
            after: 1,
        },
        Row {
            case: "a batch ceiling above what is waiting, three messages sent after the delete",
            slug: "esm-stop-batched",
            batch_size: 10,
            after: 3,
        },
    ]
}

/// RULE: a mapping polls only while this account holds it, so once a `DeleteEventSourceMapping`
/// has been accepted and the removal it owes is due, no pass reads the event source again — a
/// message sent afterwards is neither handed to the function nor taken out of the queue.
#[tokio::test]
async fn a_deleted_mapping_stops_polling_its_queue() {
    for row in rows() {
        let case = row.case;
        let (router, mut driven) = driven_emulator_with_queues();
        let (sink_url, sink_arn) =
            create_queue(&router, case, &format!("{}-sink", row.slug), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("{}-source", row.slug),
            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() }),
        )
        .await;

        // The least-privilege role of such a relay: the reads its poller makes on the source,
        // and the one send its handler makes to the sink.
        let role_arn = given_role(
            &router,
            case,
            &format!("{}-role", row.slug),
            Some(json!([
                logs_only(),
                allow("sqs", &QUEUE_READS, &source_arn),
                allow("sqs", &["SendMessage"], &sink_arn),
            ])),
        )
        .await;
        let function = format!("{}-relay", row.slug);
        create_relay(&router, case, &function, &role_arn, &sink_url).await;
        let uuid = given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": source_arn,
                "BatchSize": row.batch_size,
                "Enabled": true,
            }),
        )
        .await;

        // The positive control: while the mapping is held it consumes, so the message reaches
        // the handler, the envelope it was handed is relayed, and the source is drained.
        let before = format!("before-{}", row.slug);
        send_message(&router, case, &source_url, &before, None).await;
        let envelope = relayed_envelope(&router, &mut driven, case, &sink_url).await;
        let records = envelope["Records"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: the mapping must hand over one record per message it took, got \
             {envelope}"
        );
        assert_eq!(
            (
                records[0]["body"].as_str(),
                records[0]["eventSource"].as_str(),
                records[0]["eventSourceARN"].as_str(),
            ),
            (
                Some(before.as_str()),
                Some(EVENT_SOURCE),
                Some(source_arn.as_str()),
            ),
            "case {case:?}: the record must carry the message the queue held, got {envelope}"
        );
        let counts = queue_message_counts(&router, case, &source_url).await;
        assert_eq!(
            counts,
            (0, 0, 0),
            "case {case:?}: a message the invocation processed is deleted, so the source must \
             be drained, got {counts:?} as (visible, in flight, delayed)"
        );
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            reading.string("LastProcessingResult").as_deref(),
            Some(LAST_PROCESSING_RESULT_OK),
            "case {case:?}: the mapping must report the attempt it made as the successful one \
             it was, got {}",
            reading.text()
        );

        // The delete: the mapping enters `Deleting` and the removal it owes is the clock's.
        let deleted = delete_mapping(&router, &uuid).await;
        assert_eq!(
            (deleted.status, deleted.string("State").as_deref()),
            (202, Some(EVENT_SOURCE_MAPPING_STATE_DELETING)),
            "case {case:?}: the delete must answer the mapping as being deleted, got {} {}",
            deleted.status,
            deleted.text()
        );
        // "might not be completely deleted for several seconds": the state the delete
        // answered with is one a CLIENT can read back, which is what a client watching the
        // teardown waits for.
        let being_deleted = get_mapping(&router, &uuid).await;
        assert_eq!(
            (
                being_deleted.status,
                being_deleted.string("State").as_deref()
            ),
            (200, Some(EVENT_SOURCE_MAPPING_STATE_DELETING)),
            "case {case:?}: a client reading the mapping after the delete must be told it is \
             being deleted, got {} {}",
            being_deleted.status,
            being_deleted.text()
        );

        // The negative half: every message sent now stays where it was sent, through passes
        // this suite makes itself — a mapping that was still polling would consume inside
        // them. It starts INSIDE the `Deleting` window, because polling stops when the
        // delete is accepted and not when the removal it owes falls due.
        let after: Vec<String> = (0..row.after)
            .map(|index| format!("after-{}-{index}", row.slug))
            .collect();
        for body in &after {
            send_message(&router, case, &source_url, body, None).await;
        }
        let waiting = u32::try_from(after.len()).unwrap_or(u32::MAX);
        driven.poll_once().await;
        let counts = queue_message_counts(&router, case, &source_url).await;
        assert_eq!(
            counts,
            (waiting, 0, 0),
            "case {case:?}: a mapping being deleted polls nothing, so the pass made inside \
             that window must leave every message visible on the source, got {counts:?} as \
             (visible, in flight, delayed)"
        );

        // And once the removal the clock owed is due the mapping is held no more, which is
        // also when it stops being a resource a client can change.
        await_removed(&router, case, &uuid).await;
        let updated = call(
            &router,
            "PUT",
            &format!("{MAPPINGS_URI}/{uuid}"),
            &json!({ "BatchSize": 2 }),
        )
        .await;
        assert_eq!(
            (
                updated.status,
                updated.error_code().contains("ResourceNotFoundException")
            ),
            (404, true),
            "case {case:?}: a mapping this account no longer holds cannot be updated, got {} {}",
            updated.status,
            updated.text()
        );

        for pass in 0..BLOCKED_PASSES {
            driven.poll_once().await;
            let counts = queue_message_counts(&router, case, &source_url).await;
            assert_eq!(
                counts,
                (waiting, 0, 0),
                "case {case:?}: nothing polls a mapping this account no longer holds, so pass \
                 {pass} must leave every message visible on the source, got {counts:?} as \
                 (visible, in flight, delayed)"
            );
            let sink = queue_message_counts(&router, case, &sink_url).await;
            assert_eq!(
                sink,
                (0, 0, 0),
                "case {case:?}: the function must not be invoked after the mapping is gone, and \
                 pass {pass} left the sink queue reporting {sink:?} as (visible, in flight, \
                 delayed)"
            );
        }

        // And what stayed is receivable by an ordinary consumer, body for body.
        let held: BTreeSet<String> = take_everything(&router, case, &source_url)
            .await
            .into_iter()
            .collect();
        assert_eq!(
            held,
            after.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: every message sent after the delete must still be receivable, got \
             {held:?}"
        );
    }
}
