// SPDX-License-Identifier: CC0-1.0

//! What a `FilterCriteria`-filtered mapping on a DynamoDB table's STREAM delivers to a
//! CONSUMER that waits for the delivery — the half of that rule a client sees.
//!
//! RULE: an `Enabled` event source mapping on a DynamoDB table's stream reads that stream from
//! its `StartingPosition` and invokes its function with exactly the records at least one of its
//! `FilterCriteria` patterns matches — "an object that defines the filter criteria that
//! determine whether Lambda should process an event" (`FilterCriteria`), "a list of filters"
//! (`FilterCriteria$Filters`) — ONCE each, advancing the shard's position past every record no
//! pattern matched; so a consumer of what that function forwards receives exactly one message
//! per admitted record, and never one for a record that was withheld.
//!
//! [`crate::dynamodb_stream_event_source_filter_criteria`] spans WHICH records such criteria
//! admit, and this suite reuses its wiring — the router, the forwarding function, the mapping
//! and the marker a delivered record is named by — rather than writing a second copy of it.
//! What is added here is the CONSUMER's side: the delivery is observed through a LONG POLL —
//! "The duration (in seconds) for which the call waits for a message to arrive in the queue
//! before returning. If a message is available, the call returns sooner than `WaitTimeSeconds`"
//! (`ReceiveMessageRequest$WaitTimeSeconds`) — which is how a client that cannot be told about
//! a delivery waits for one; and the verdict is the EXACT set of messages such a consumer gets,
//! so a second delivery of an admitted record or a late leak of a withheld one fails the row.
//!
//! The rows differ along what the rule generalises over: which change the pattern names, where
//! in the shard the admitted records sit (first, last, both ends), how many patterns the
//! criteria carry, how many records they admit, and whether the clause names a member at the
//! top of the record or a value nested inside the changed item's key.
//!
//! Every verdict is read off what the HANDLER forwarded, so a record that reached no invocation
//! cannot appear. Each row whose withheld record is written BEFORE a delivered one settles its
//! absence by ORDER — a shard is read in order, so once the later record's message is on the
//! queue the withheld one has been read past — and every row settles it again by draining the
//! queue with one more long poll once the expected messages have arrived: a duplicate or a
//! leaked record would be waiting there.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    given_table, write_item, Write, BUDGET, KEY, NEW_AND_OLD_IMAGES, RESULT_OK,
};
// The wiring this suite drives its own rule over, shared with the suite that spans which
// records a mapping's criteria admit, so there is one of it in this target.
use crate::dynamodb_stream_event_source_filter_criteria::{
    emulator, given_function, given_mapping, marker, INSERT, MODIFY, REMOVE, SEND_MESSAGE,
    STATE_ENABLED,
};
use crate::event_source_mapping_role_access::{allow, given_role, DYNAMODB_READS};
use crate::event_source_mappings::{call_sqs, create_queue, get_mapping};

/// The two items every row writes to, named for the order they are written in.
const FIRST_ITEM: &str = "first-item";
/// See [`FIRST_ITEM`].
const SECOND_ITEM: &str = "second-item";

/// How long ONE `ReceiveMessage` waits for a message to arrive before answering. The whole wait
/// is the QUEUE's: this suite never sleeps for a delivery, it asks the queue to wait for one.
const WAIT_TIME_SECONDS: i64 = 5;

/// ONE long poll of the output queue, as the markers the handler forwarded, each message
/// deleted so a later look finds only later ones.
///
/// The call answers as soon as a message is there and otherwise after [`WAIT_TIME_SECONDS`], so
/// nothing here waits on the clock for a delivery.
async fn polled_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, received) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage to answer 200, got {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut markers = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default().to_string();
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
        let forwarded: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a forwarded record as JSON, got {body:?} ({error})")
        });
        markers.push(marker(
            forwarded["eventName"].as_str().unwrap_or_default(),
            forwarded["key"].as_str().unwrap_or_default(),
        ));
    }
    markers
}

/// Long-poll the output queue until every marker of `expected` has arrived, answering every
/// marker seen in the order they arrived.
///
/// The deadline is GENEROUS and bounds nothing the rows are about: a delivery is given room, so
/// a loaded host cannot turn a slow one into a verdict.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET + BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(polled_markers(router, case, url).await);
        if expected.iter().all(|marker| seen.contains(marker)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to be handed {expected:?}, it forwarded {seen:?}"
        );
    }
}

/// One row: the patterns the mapping filters with, the writes made to the table, and which of
/// the records they produce the handler must and must not be handed.
struct Row {
    case: &'static str,
    /// A short name for the resources this row makes, since a role name is bounded at 64
    /// characters and the case above is prose.
    id: &'static str,
    /// The `FilterCriteria.Filters[].Pattern` values, in order.
    patterns: Vec<String>,
    /// The key and the change of each write, in the order they are made.
    writes: &'static [(&'static str, Write)],
    /// The records the handler must be handed exactly once each, as `(eventName, key)`.
    delivered: &'static [(&'static str, &'static str)],
    /// The records no invocation may be handed, as `(eventName, key)`.
    withheld: &'static [(&'static str, &'static str)],
}

/// The rows, spanning what the rule generalises over (see this file's own docs).
fn rows() -> Vec<Row> {
    vec![
        // The shape a client wires an insert-only projection in: one item written and then
        // changed, with the change the only record the criteria reject.
        Row {
            case: "insert-only-over-a-write-and-its-change",
            id: "insert-only",
            patterns: vec![format!(r#"{{"eventName":["{INSERT}"]}}"#)],
            writes: &[(FIRST_ITEM, Write::Insert), (FIRST_ITEM, Write::Overwrite)],
            delivered: &[(INSERT, FIRST_ITEM)],
            withheld: &[(MODIFY, FIRST_ITEM)],
        },
        // The same criteria with a record written BEHIND the withheld one: its arrival is what
        // proves the position moved past the change rather than stopping on it.
        Row {
            case: "insert-only-with-a-record-behind-the-withheld-change",
            id: "behind",
            patterns: vec![format!(r#"{{"eventName": ["{INSERT}"]}}"#)],
            writes: &[
                (FIRST_ITEM, Write::Insert),
                (FIRST_ITEM, Write::Overwrite),
                (SECOND_ITEM, Write::Insert),
            ],
            delivered: &[(INSERT, FIRST_ITEM), (INSERT, SECOND_ITEM)],
            withheld: &[(MODIFY, FIRST_ITEM)],
        },
        // Two patterns, ORed, with the record the criteria reject at the HEAD of the shard: the
        // records behind one that is rejected are still delivered.
        Row {
            case: "two-patterns-admit-the-change-and-the-removal",
            id: "two-patterns",
            patterns: vec![
                format!(r#"{{"eventName":["{MODIFY}"]}}"#),
                format!(r#"{{"eventName":["{REMOVE}"]}}"#),
            ],
            writes: &[
                (FIRST_ITEM, Write::Insert),
                (FIRST_ITEM, Write::Update),
                (FIRST_ITEM, Write::Delete),
            ],
            delivered: &[(MODIFY, FIRST_ITEM), (REMOVE, FIRST_ITEM)],
            withheld: &[(INSERT, FIRST_ITEM)],
        },
        // A clause naming a value NESTED inside the changed item's key rather than the change's
        // own name: of two writes of the same kind, only the one the clause names is handed
        // over.
        Row {
            case: "a-clause-nested-in-the-changed-items-key",
            id: "nested-key",
            patterns: vec![format!(
                r#"{{"dynamodb":{{"Keys":{{"{KEY}":{{"S":["{SECOND_ITEM}"]}}}}}}}}"#
            )],
            writes: &[(FIRST_ITEM, Write::Insert), (SECOND_ITEM, Write::Insert)],
            delivered: &[(INSERT, SECOND_ITEM)],
            withheld: &[(INSERT, FIRST_ITEM)],
        },
    ]
}

/// RULE: a filtered mapping on a DynamoDB table's stream hands its function exactly the records
/// a pattern matches, once each, and a consumer waiting on what that function forwards receives
/// one message per such record and none for a record that was withheld.
#[tokio::test]
async fn a_filtered_stream_mapping_delivers_each_matching_record_once_to_a_waiting_consumer() {
    for row in rows() {
        let case = row.case;
        let id = row.id;
        let router = emulator();
        let table = format!("filtered-source-{id}");
        let function = format!("filtered-consumer-{id}");
        let (queue_url, queue_arn) =
            create_queue(&router, case, &format!("filtered-sink-{id}"), &json!({})).await;
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        // The least-privilege role a client writes for this wiring: the reads consuming a
        // table's stream takes, on that stream, and the one write the handler makes.
        let role_arn = given_role(
            &router,
            case,
            &format!("filtered-role-{id}"),
            Some(json!([
                allow("dynamodb", &DYNAMODB_READS, &stream_arn),
                allow("sqs", &SEND_MESSAGE, &queue_arn),
            ])),
        )
        .await;
        given_function(&router, case, &function, &role_arn, &queue_url).await;

        // Every write is made BEFORE the mapping exists: a mapping reading from the oldest
        // retained record reads the backlog, so the criteria decide records already in the
        // shard.
        for (key, write) in row.writes {
            write_item(&router, case, &table, key, *write).await;
        }
        let patterns: Vec<&str> = row.patterns.iter().map(String::as_str).collect();
        let uuid = given_mapping(&router, case, &function, &stream_arn, &patterns).await;

        let expected: Vec<String> = row
            .delivered
            .iter()
            .map(|(event_name, key)| marker(event_name, key))
            .collect();
        let seen = awaited_markers(&router, case, &queue_url, &expected).await;
        for (event_name, key) in row.withheld {
            let withheld = marker(event_name, key);
            assert!(
                !seen.contains(&withheld),
                "case {case:?}: the criteria {patterns:?} match no {withheld} record, so no \
                 invocation may be handed it, but the handler forwarded {seen:?}"
            );
        }
        // Exactly one message per admitted record: anything else the handler forwarded would be
        // a record the criteria rejected or a second delivery of one they admitted.
        let mut sorted_seen = seen.clone();
        sorted_seen.sort();
        let mut sorted_expected = expected.clone();
        sorted_expected.sort();
        assert_eq!(
            sorted_seen, sorted_expected,
            "case {case:?}: expected exactly the records {sorted_expected:?} to be forwarded, \
             got {sorted_seen:?}"
        );
        // And the queue is quiet afterwards: one more long poll over the whole wait, so a
        // duplicate of a delivered record or a late leak of a withheld one would be waiting
        // here.
        let afterwards = polled_markers(&router, case, &queue_url).await;
        assert!(
            afterwards.is_empty(),
            "case {case:?}: every record of the stream has been read past, so nothing more may \
             reach the handler, but it went on forwarding {afterwards:?}"
        );

        // The mapping that made those deliveries is still the one a client reads back: it polls
        // its source, carries the criteria it was created with, and reports what its last
        // attempt came to.
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            read.string("State").as_deref(),
            Some(STATE_ENABLED),
            "case {case:?}: expected the mapping to report {STATE_ENABLED}, got {}",
            read.text()
        );
        let criteria: Vec<String> = read.json()["FilterCriteria"]["Filters"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|filter| filter["Pattern"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            criteria, row.patterns,
            "case {case:?}: expected the mapping to report the criteria it was created with, \
             got {}",
            read.text()
        );
        assert_eq!(
            read.string("LastProcessingResult").as_deref(),
            Some(RESULT_OK),
            "case {case:?}: expected the mapping to report {RESULT_OK} for the batches it moved, \
             got {}",
            read.text()
        );
    }
}
