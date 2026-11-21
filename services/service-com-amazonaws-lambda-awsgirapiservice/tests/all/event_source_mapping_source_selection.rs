// SPDX-License-Identifier: CC0-1.0

//! WHICH queue an event source mapping on Amazon SQS consumes, and which it leaves alone.
//!
//! The RULE asserted over the table below, in one sentence: a mapping reads exactly the one
//! event source its `EventSourceArn` names — "the Amazon Resource Name (ARN) of the event
//! source" (`smithy_query(query="operation", operation="CreateEventSourceMapping")`), of
//! which "Amazon Simple Queue Service – The ARN of the queue" is this suite's kind — so one
//! poll pass hands its function the records of THAT queue and deletes the ones the attempt
//! consumed, while the messages of every other queue the account holds stay where they were
//! sent, never delivered and never counted against it. A mapping whose ARN names a queue
//! nothing was sent to therefore invokes nothing at all, however much is waiting elsewhere.
//!
//! Which records reached the function is read out of the function's OWN run: the handler of
//! a real deployment package raises on any record whose `eventSourceARN` is not the ARN its
//! mapping names, and reports as unprocessed exactly the records whose body carries the
//! marker below — so the messages the mapping then deletes, and the attempt it records, are
//! consequences of the records having arrived and of the report having been read. Nothing
//! here invokes the function itself.
//!
//! The same run is also read OUT OF BAND, where a client of such a pipeline reads it: the
//! handler forwards the body of every record it is handed to a queue of its own, with the SDK
//! its managed runtime supplies, and that queue is one no mapping names. So each row reads
//! back the bodies that really reached a running handler — which must be exactly the ones sent
//! to the mapped queue and never a body of another queue, for the mapping whose own queue is
//! empty not a single body — rather than only what the mapping reports about its attempt.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with
//! Lambda and Amazon SQS on ONE engine, as `core/router` assembles them. No row sleeps and
//! no row waits out a span: the poll passes are the suite's own
//! (`crate::event_source_mappings::DrivenPolling`), so what each assertion reads is the
//! state one finished pass left.
//!
//! The rows differ along what the rule generalises over: how many queues exist (two and
//! three), which of them the mapping names (the first, the middle, the last), whether that
//! queue is standard or FIFO, how many records one batch may hold (one, and a ceiling above
//! what is waiting), how many messages wait on the mapped queue and on the others, and the
//! leg where the mapped queue is the EMPTY one.

use std::collections::BTreeSet;

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    LAST_PROCESSING_RESULT_OK, REPORT_BATCH_ITEM_FAILURES,
};

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_queue, create_running_function,
    driven_router_with_queues, get_mapping, held_until, messages_held, queue_message_counts,
    send_message, taken_bodies, STATE_ENABLED,
};

/// The mapped function's `Timeout` and the `VisibilityTimeout` of every queue below: the
/// same value, which is the pairing `CreateEventSourceMapping` accepts (a queue must outlast
/// the function it feeds). Long enough that a message one pass took is still out of the
/// queue while the row reads what that pass left.
const TIMEOUT_SECONDS: i64 = 30;

/// The marker a message body carries to be one the handler reports as unprocessed. Nothing
/// in the emulator reads it: the handler reads it out of the record's own body.
///
/// Every row that marks a record reads its batch under a ceiling that holds the whole of
/// what is waiting, so each batch handed over also carries a record the handler processes:
/// what a batch reported unprocessed in its ENTIRETY comes to is a rule of its own — an
/// attempt that consumed nothing — and it belongs to the suite over partial reports
/// (`tests/all/sqs_event_source_handler_run.rs`), not to this one.
const POISON: &str = "poison";

/// The handler every row runs: it refuses a record that came from any queue but the one its
/// mapping names, forwards the body of every record it is handed to the queue its environment
/// names, and reports as unprocessed the records whose body carries [`POISON`].
///
/// So an attempt that is recorded as having succeeded is one where every record handed over
/// belonged to the mapped queue, the messages deleted are the ones that batch processed, and
/// the bodies on the forwarding queue are the ones that reached this handler's own run.
fn selecting_handler() -> String {
    format!(
        r#"
import os

import boto3

QUEUE_ARN = os.environ["QUEUE_ARN"]
SINK_URL = os.environ["{SINK_VARIABLE}"]
POISON = "{POISON}"
QUEUES = boto3.client("sqs")


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        if record["eventSource"] != "aws:sqs":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if record["eventSourceARN"] != QUEUE_ARN:
            raise ValueError("eventSourceARN is " + repr(record["eventSourceARN"]))
        QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=record["body"])
    return {{
        "batchItemFailures": [
            {{"itemIdentifier": record["messageId"]}}
            for record in records
            if POISON in record["body"]
        ]
    }}
"#
    )
}

/// The environment variable [`selecting_handler`] reads the queue it forwards each record's
/// body to from.
const SINK_VARIABLE: &str = "SINK_URL";

/// One queue of a row: a label for its name, whether it is FIFO, and the bodies waiting on
/// it when the passes are made.
struct Queue {
    label: &'static str,
    fifo: bool,
    bodies: &'static [&'static str],
}

/// One row: the queues that exist, which of them the mapping's `EventSourceArn` names, and
/// the `BatchSize` it reads that one under.
struct Row {
    case: &'static str,
    queues: &'static [Queue],
    /// The index in `queues` of the queue the mapping names.
    mapped: usize,
    /// `BatchSize`: "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
}

/// The catalog: the mapped queue first and in the middle of three; a standard mapped queue
/// beside a FIFO one and a FIFO mapped queue beside a standard one; a batch ceiling above
/// what is waiting and one of a single record; a mapped queue that drains completely and one
/// left holding what its function reported; and the leg the negative check is about — a
/// mapping whose queue is empty while another queue holds everything.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two standard queues, the first mapped",
            queues: &[
                Queue {
                    label: "a",
                    fifo: false,
                    bodies: &["keep-a1", "poison-a2", "keep-a3"],
                },
                Queue {
                    label: "b",
                    fifo: false,
                    bodies: &["other-b1", "other-b2"],
                },
            ],
            mapped: 0,
            batch_size: 10,
        },
        Row {
            case: "two standard queues, the second mapped, one record per batch",
            queues: &[
                Queue {
                    label: "a",
                    fifo: false,
                    bodies: &["other-a1", "other-a2"],
                },
                Queue {
                    label: "b",
                    fifo: false,
                    bodies: &["keep-b1", "keep-b2"],
                },
            ],
            mapped: 1,
            batch_size: 1,
        },
        Row {
            case: "three queues, the middle one mapped, the last FIFO",
            queues: &[
                Queue {
                    label: "a",
                    fifo: false,
                    bodies: &["other-a1"],
                },
                Queue {
                    label: "b",
                    fifo: false,
                    bodies: &["keep-b1", "keep-b2"],
                },
                Queue {
                    label: "c",
                    fifo: true,
                    bodies: &["other-c1", "other-c2"],
                },
            ],
            mapped: 1,
            batch_size: 10,
        },
        Row {
            case: "a FIFO queue mapped beside a standard one",
            queues: &[
                Queue {
                    label: "a",
                    fifo: true,
                    bodies: &["keep-a1", "poison-a2"],
                },
                Queue {
                    label: "b",
                    fifo: false,
                    bodies: &["other-b1"],
                },
            ],
            mapped: 0,
            batch_size: 10,
        },
        Row {
            case: "the mapped queue is the empty one",
            queues: &[
                Queue {
                    label: "a",
                    fifo: false,
                    bodies: &[],
                },
                Queue {
                    label: "b",
                    fifo: false,
                    bodies: &["other-b1", "other-b2", "other-b3"],
                },
            ],
            mapped: 0,
            batch_size: 10,
        },
    ]
}

/// The attributes a queue of this kind is created with: the shared visibility timeout, plus
/// what makes a FIFO queue one ("If you create a FIFO queue, your queue name must end with
/// the .fifo suffix", and content-based deduplication so a send needs no deduplication id).
fn attributes_of(queue: &Queue) -> serde_json::Value {
    let mut attributes = json!({ "VisibilityTimeout": TIMEOUT_SECONDS.to_string() });
    if queue.fifo {
        attributes["FifoQueue"] = json!("true");
        attributes["ContentBasedDeduplication"] = json!("true");
    }
    attributes
}

/// The name a row's queue is created under, with the suffix a FIFO queue's name must end in.
fn queue_name(index: usize, queue: &Queue) -> String {
    let base = format!("esm-select-{index}-{}", queue.label);
    if queue.fifo {
        format!("{base}.fifo")
    } else {
        base
    }
}

#[tokio::test]
async fn a_mapping_consumes_only_the_queue_its_event_source_arn_names() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (router, mut driven) = driven_router_with_queues();

        // Every queue of the row, created and read back through Amazon SQS's own surface, so
        // the mapping is made on the ARN the queue itself reports.
        let mut queues: Vec<(String, String)> = Vec::new();
        for queue in row.queues {
            queues.push(
                create_queue(
                    &router,
                    case,
                    &queue_name(index, queue),
                    &attributes_of(queue),
                )
                .await,
            );
        }
        let (mapped_url, mapped_arn) = queues[row.mapped].clone();

        // The queue the handler forwards every record it is handed to: a queue of this row
        // that no mapping names, which is where the records that reached the function's own
        // run are read back.
        let (sink_url, _) = create_queue(
            &router,
            case,
            &format!("esm-select-{index}-sink"),
            &json!({}),
        )
        .await;

        let function = format!("esm-select-fn-{index}");
        create_running_function(
            &router,
            case,
            &function,
            &selecting_handler(),
            TIMEOUT_SECONDS,
            &json!({ "QUEUE_ARN": mapped_arn, SINK_VARIABLE: sink_url }),
        )
        .await;
        let created = create_mapping(
            &router,
            &json!({
                "FunctionName": function,
                "EventSourceArn": mapped_arn,
                "BatchSize": row.batch_size,
                "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
                "Enabled": true,
            }),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

        // What is waiting, on every queue of the row. A FIFO queue's send carries a message
        // group, one of its own per message, so nothing of what follows turns on a group
        // being held back.
        for (queue, (url, _)) in row.queues.iter().zip(&queues) {
            for (position, body) in queue.bodies.iter().enumerate() {
                let group = format!("{}-{position}", queue.label);
                let group = if queue.fifo { Some(group.as_str()) } else { None };
                send_message(&router, case, url, body, group).await;
            }
        }

        // A pass per waiting message and one more, so whatever the batch ceiling left behind
        // is got through and a pass is made that has nothing of its own to take.
        for _ in 0..=row.queues[row.mapped].bodies.len() {
            driven.poll_once().await;
        }

        // The mapped queue: every message taken out of it, and the ones the report named
        // left for redelivery — which is what the attempt consumed, not what this row
        // assumed.
        let unprocessed: BTreeSet<String> = row.queues[row.mapped]
            .bodies
            .iter()
            .filter(|body| body.contains(POISON))
            .map(|body| (*body).to_string())
            .collect();
        let counts = queue_message_counts(&router, case, &mapped_url).await;
        assert_eq!(
            (counts.0, messages_held(counts)),
            (
                0,
                u32::try_from(unprocessed.len()).expect("a row's record count fits")
            ),
            "case {case:?}: the mapped queue must be left holding only the {} record(s) the \
             report named, got {counts:?} as (visible, in flight, delayed)",
            unprocessed.len()
        );
        let left: BTreeSet<String> = held_until(driven.engine(), &mapped_arn)
            .into_keys()
            .collect();
        assert_eq!(
            left, unprocessed,
            "case {case:?}: the mapped queue must still hold exactly the bodies the report \
             named, got {left:?}"
        );

        // Every OTHER queue: untouched. Nothing was taken out of it, nothing is in flight
        // with anyone, and no message of it was ever hidden by a delivery — so none of its
        // records can have reached the function.
        for (position, (queue, (url, arn))) in row.queues.iter().zip(&queues).enumerate() {
            if position == row.mapped {
                continue;
            }
            let waiting =
                u32::try_from(queue.bodies.len()).expect("a row's record count fits");
            let counts = queue_message_counts(&router, case, url).await;
            assert_eq!(
                counts,
                (waiting, 0, 0),
                "case {case:?}: the queue {:?} is not the mapping's event source, so all \
                 {waiting} of its message(s) must still be visible on it, got {counts:?} as \
                 (visible, in flight, delayed)",
                queue.label
            );
            let holds = held_until(driven.engine(), arn);
            let bodies: BTreeSet<String> = holds.keys().cloned().collect();
            let expected: BTreeSet<String> = queue
                .bodies
                .iter()
                .map(|body| (*body).to_string())
                .collect();
            assert_eq!(
                bodies, expected,
                "case {case:?}: the queue {:?} must still hold every body sent to it, got \
                 {bodies:?}",
                queue.label
            );
            assert!(
                holds.values().all(Option::is_none),
                "case {case:?}: no message of the queue {:?} was handed to anyone, so none of \
                 them can be hidden by a delivery, got {holds:?}",
                queue.label
            );
        }

        // And what REACHED the function, read where a client of such a pipeline reads it:
        // every body sent to the mapped queue, and no body of any other queue — so a mapping
        // whose own queue was empty forwarded nothing at all.
        let mut forwarded: BTreeSet<String> = BTreeSet::new();
        loop {
            let taken = taken_bodies(&router, case, &sink_url).await;
            if taken.is_empty() {
                break;
            }
            forwarded.extend(taken);
        }
        let delivered: BTreeSet<String> = row.queues[row.mapped]
            .bodies
            .iter()
            .map(|body| (*body).to_string())
            .collect();
        assert_eq!(
            forwarded, delivered,
            "case {case:?}: the handler must be handed exactly the bodies of the queue the \
             mapping names, so it forwards {delivered:?}, got {forwarded:?}"
        );

        // And what the mapping says it did: an attempt for the batch it took, and none at
        // all for the mapping whose own queue was empty — "the result of the event source
        // mapping's last processing attempt".
        let expected_result = if row.queues[row.mapped].bodies.is_empty() {
            None
        } else {
            Some(LAST_PROCESSING_RESULT_OK.to_string())
        };
        let reading = get_mapping(&router, &uuid).await;
        assert_eq!(
            reading.string("LastProcessingResult"),
            expected_result,
            "case {case:?}: the mapping must report {expected_result:?} as the attempt it \
             made, got {}",
            reading.text()
        );
    }
}
