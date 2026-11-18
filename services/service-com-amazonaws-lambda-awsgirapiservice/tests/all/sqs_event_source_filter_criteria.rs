// SPDX-License-Identifier: CC0-1.0

//! WHICH messages of a mapped Amazon SQS queue reach the function, asserted through the
//! function's own run: the handler of a real deployment package forwards every record it is
//! handed to a second queue, so a body read back off that queue is a message that really
//! reached a running handler and a body missing from it is one no invocation was made for.
//!
//! RULE: `FilterCriteria` is "an object that defines the filter criteria that determine whether
//! Lambda should process an event" and `FilterCriteria$Filters` is "a list of filters", each
//! carrying a `Filter$Pattern` — so the criteria are a decision about ONE record, a record at
//! least one of the patterns matches is one the function is invoked with, and a record none of
//! them matches is not handed over at all. On a QUEUE source the message a record was built
//! for leaves the queue either way: the admitted one when the invocation it was handed to
//! succeeded, the rejected one because nothing else would ever take it.
//!
//! The rows differ along what the rule generalises over: where in the record the pattern's
//! clause points (a member of a JSON body, a member nested deeper inside it, the body itself
//! when it is a plain string, and a member outside the body), what the clause tests (an exact
//! value, one of a numeric comparison's bounds, a prefix), and how many patterns the criteria
//! carry (one, and two of which either admits).
//!
//! The harness — the router with Lambda and Amazon SQS on one engine, the queue fixture, the
//! function whose package runs and the reader of the sink queue — is the shared one in
//! `event_source_mappings.rs`, so this suite drives the same wire as every other over this
//! surface.
//!
//! Nothing here waits out a span of its own choosing and nothing races the background poller:
//! this suite takes the store's poll claim and MAKES each pass itself ([`DrivenPolling`]), so a
//! pass has happened when the call that made it answers — every batch it took has been handed
//! to the function and what the attempt came to is recorded. The suite RUNS handlers, so it
//! takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_queue, create_running_function,
    driven_router_with_queues, forwarded_records, queue_held, send_attributed_message,
    send_message, DrivenPolling, STATE_ENABLED,
};

/// The `VisibilityTimeout` every source queue hides a delivered batch for, and the `Timeout` of
/// the function it maps onto: a queue only maps onto a function it outlasts, so the two are the
/// same span.
const ATTEMPT_SECONDS: i64 = 10;

/// How long this suite gives the sink queue to hand over the records the passes already made:
/// the passes are awaited, so what is left is the receive of messages that are already on the
/// queue. Patience and not a weakened expectation — every assertion below is unchanged.
const SINK_BUDGET: Duration = Duration::from_secs(30);

/// How many passes a row's messages may take to leave their queue, per message sent.
///
/// A count of PASSES and not a span: one pass takes a batch, filters it and hands the admitted
/// messages over, so a row needs one — this is the room above that, for a pass whose
/// invocation had to wait for its share of the run capacity and was retried.
const PASSES_PER_MESSAGE: usize = 4;

/// The one function every mapping of this suite invokes.
const FUNCTION: &str = "esm-filter-fn";

/// The handler that function runs: it forwards each record it is handed, verbatim, to the queue
/// its environment names, so what this suite reads back is the record a running handler was
/// given.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3

SINK_URL = os.environ["SINK_URL"]


def lambda_handler(event, context):
    client = boto3.client("sqs")
    forwarded = []
    for record in event["Records"]:
        client.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(record))
        forwarded.append(record["messageId"])
    return {"forwarded": forwarded}
"#;

/// One message a row sends: its body, the one message attribute it carries (as a `String`), and
/// whether the row's criteria admit it.
struct Sent {
    body: &'static str,
    attribute: Option<(&'static str, &'static str)>,
    admitted: bool,
}

/// One row: the patterns the mapping's `FilterCriteria` carry, and the messages waiting on its
/// own queue.
struct Row {
    case: &'static str,
    patterns: &'static [&'static str],
    sent: &'static [Sent],
}

/// The catalog. Every clause is written in the event-pattern language `FilterCriteria$Filters`
/// takes, and every body is its row's own so a forwarded record belongs to exactly one row.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a member of a json body",
            patterns: &[r#"{"body":{"kind":["wanted"]}}"#],
            sent: &[
                Sent {
                    body: r#"{"kind":"wanted","row":"member"}"#,
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind":"other","row":"member"}"#,
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            case: "a member nested inside a json body",
            patterns: &[r#"{"body":{"order":{"type":["express"]}}}"#],
            sent: &[
                Sent {
                    body: r#"{"order":{"type":"express"},"row":"nested"}"#,
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"order":{"type":"ground"},"row":"nested"}"#,
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            // "If your filter criteria expects body to be in a valid JSON format": a body that
            // spells no document is the string it is, and a pattern names that string.
            case: "a body that is a plain string",
            patterns: &[r#"{"body":["ping-plain"]}"#],
            sent: &[
                Sent {
                    body: "ping-plain",
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: "pong-plain",
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            case: "two patterns, either of which admits",
            patterns: &[
                r#"{"body":{"kind":["first"]}}"#,
                r#"{"body":{"kind":["second"]}}"#,
            ],
            sent: &[
                Sent {
                    body: r#"{"kind":"first","row":"ored"}"#,
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind":"second","row":"ored"}"#,
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"kind":"third","row":"ored"}"#,
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            case: "a numeric comparison on a json body member",
            patterns: &[r#"{"body":{"amount":[{"numeric":[">",100]}]}}"#],
            sent: &[
                Sent {
                    body: r#"{"amount":150,"row":"numeric"}"#,
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: r#"{"amount":50,"row":"numeric"}"#,
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            case: "a prefix of a plain string body",
            patterns: &[r#"{"body":[{"prefix":"order-"}]}"#],
            sent: &[
                Sent {
                    body: "order-7788",
                    attribute: None,
                    admitted: true,
                },
                Sent {
                    body: "invoice-7788",
                    attribute: None,
                    admitted: false,
                },
            ],
        },
        Row {
            // A clause outside the body: the criteria decide about the whole record, so a
            // message's own attributes decide as much as its body does.
            case: "a message attribute of the record",
            patterns: &[r#"{"messageAttributes":{"kind":{"stringValue":["wanted"]}}}"#],
            sent: &[
                Sent {
                    body: "attributed-wanted",
                    attribute: Some(("kind", "wanted")),
                    admitted: true,
                },
                Sent {
                    body: "attributed-other",
                    attribute: Some(("kind", "other")),
                    admitted: false,
                },
            ],
        },
    ]
}

/// Send one row's message, with the attribute it carries when it carries one.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, message: &Sent) {
    match message.attribute {
        Some((name, value)) => {
            send_attributed_message(
                router,
                case,
                url,
                message.body,
                &json!({ name: { "DataType": "String", "StringValue": value } }),
            )
            .await;
        }
        None => send_message(router, case, url, message.body, None).await,
    }
}

/// Make passes until every source queue of `sources` is empty, or until `passes` have been
/// made: a message leaves its queue because the filter rejected it or because the invocation
/// it was handed to succeeded, so an empty queue is the whole of what the rule owes.
async fn drain_the_sources(
    router: &MultiServiceRouter,
    driven: &mut DrivenPolling,
    sources: &[(String, String)],
    passes: usize,
) -> Vec<u32> {
    let mut held: Vec<u32> = Vec::new();
    for _ in 0..passes {
        driven.poll_once().await;
        held.clear();
        for (url, _) in sources {
            held.push(queue_held(router, "the source queues", url).await);
        }
        if held.iter().all(|count| *count == 0) {
            break;
        }
    }
    held
}

/// RULE: a message at least one of the mapping's `FilterCriteria` patterns matches is handed to
/// the function, one none of them matches is not, and either way the message leaves the queue.
#[tokio::test]
async fn filter_criteria_decide_which_queue_messages_reach_the_function() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let rows = rows();
    let (router, mut driven) = driven_router_with_queues();
    let (sink_url, _) = create_queue(&router, "the sink", "esm-filter-sink", &json!({})).await;
    create_running_function(
        &router,
        "the forwarding function",
        FUNCTION,
        FORWARDING_HANDLER,
        ATTEMPT_SECONDS,
        &json!({ "SINK_URL": sink_url }),
    )
    .await;

    // One queue and one mapping per row, each polling for the same function: a record names the
    // queue it came from, so one sink serves every row.
    let mut sources: Vec<(String, String)> = Vec::new();
    let mut sent = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("esm-filter-source-{index}"),
            &json!({ "VisibilityTimeout": ATTEMPT_SECONDS.to_string() }),
        )
        .await;
        let created = create_mapping(
            &router,
            &json!({
                "FunctionName": FUNCTION,
                "EventSourceArn": arn,
                "BatchSize": 10,
                "Enabled": true,
                "FilterCriteria": {
                    "Filters": row
                        .patterns
                        .iter()
                        .map(|pattern| json!({ "Pattern": pattern }))
                        .collect::<Vec<Value>>(),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: CreateEventSourceMapping answered {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;
        for message in row.sent {
            send(&router, case, &url, message).await;
            sent += 1;
        }
        sources.push((url, arn));
    }

    // The passes this suite makes are the whole of what the mappings do: every queue comes to
    // hold nothing, because the filter removed what it rejected and the invocations consumed
    // what it admitted.
    let held = drain_the_sources(
        &router,
        &mut driven,
        &sources,
        sent.saturating_mul(PASSES_PER_MESSAGE),
    )
    .await;
    assert!(
        held.iter().all(|count| *count == 0),
        "every source queue must be left holding nothing — the filter removes what it rejects \
         and an invocation consumes what it admits — got {held:?} message(s) held"
    );

    // And the handler was invoked with exactly the messages the criteria admit: a rejected one
    // reaching it would be a record here, and an admitted one missing would be an invocation
    // never made.
    let admitted: BTreeSet<String> = rows
        .iter()
        .flat_map(|row| row.sent.iter())
        .filter(|message| message.admitted)
        .map(|message| message.body.to_string())
        .collect();
    let records = forwarded_records(&router, &sink_url, admitted.len(), SINK_BUDGET).await;
    let delivered: BTreeSet<String> = records.keys().cloned().collect();
    assert_eq!(
        delivered, admitted,
        "the function must be invoked with exactly the messages the criteria admit"
    );

    // And each record came from the queue whose criteria admitted it.
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (_, arn) = &sources[index];
        for message in row.sent.iter().filter(|message| message.admitted) {
            let record = records
                .get(message.body)
                .unwrap_or_else(|| panic!("case {case:?}: no record for {:?}", message.body));
            assert_eq!(
                record["eventSourceARN"].as_str(),
                Some(arn.as_str()),
                "case {case:?}: expected the record for {:?} to name {arn}, got {record}",
                message.body
            );
        }
    }
}
