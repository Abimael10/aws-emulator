// SPDX-License-Identifier: CC0-1.0

//! The delivery profile of a BULK publication onto a custom bus: how many
//! messages one queue target receives when a request's fan-out hands it more
//! messages than a single batched send carries, and that each of them is the
//! event it was held for.
//!
//! The RULE asserted here: every ENABLED rule whose `EventPattern` matches an
//! accepted event delivers that event to each of its targets at least once, so a
//! queue named by SEVERAL matching rules receives one message per (event, rule)
//! and a queue named by one receives one message per event — whatever order the
//! service groups the sends in, and however many messages that is. Each message
//! carries that event's own envelope and its own `TraceHeader`, which a grouping
//! could silently cross: "an X-Ray trace header … that contains the trace-id
//! associated with the event" belongs to the entry that carried it, not to the
//! batch its message travelled in.
//!
//! The rows span the class the rule generalises over — how many messages one
//! request hands ONE queue: fewer than a batched send carries, exactly one
//! batch's worth, and more than one batch's worth (two rules over one queue, and
//! a bulk workload over two). `tests/all/bulk_fan_out.rs` is the suite for the
//! fan-out ACROSS the rules of a bulk workload; this one is about what one queue
//! receives. The harness is the crate's shared one
//! (`tests/all/target_dispatch.rs`).

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::target_dispatch::{
    emulator, events, given_queue, sqs, Emulator, DETAIL_TYPE, SOURCE,
};

/// How many entries one `PutEvents` request carries: `Entries` is modelled
/// `@length` max 10, which is also the most messages one batched send carries —
/// so a queue named by two matching rules is handed more than one batch's worth
/// by a single request.
const BATCH: usize = 10;

/// An ENABLED rule called `rule` on `bus` matching every event this suite
/// publishes, whose only target is the queue `arn`.
async fn given_rule(emulator: &Emulator, case: &str, bus: &str, rule: &str, arn: &str) {
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": rule,
            "EventBusName": bus,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: PutRule {rule} answered {status} {body}"
    );
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": rule,
            "EventBusName": bus,
            "Targets": [{ "Id": "q", "Arn": arn }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: PutTargets {rule} answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case}: PutTargets {rule} reported a failed entry: {body}"
    );
}

/// Every message waiting on the queue at `url`, received until it reports none
/// left and deleted as it is read, as the envelope and the trace header each one
/// carries.
///
/// The system attributes are asked for by name, because Amazon SQS reports one
/// only when the receive requests it; the crate's shared `drain` takes one batch
/// and no attributes, which neither a bulk queue nor a trace assertion can use.
async fn drain(emulator: &Emulator, case: &str, url: &str) -> Vec<(Value, Option<String>)> {
    let mut received: Vec<(Value, Option<String>)> = Vec::new();
    loop {
        let (status, body) = sqs(
            emulator,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "MessageSystemAttributeNames": ["All"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: ReceiveMessage answered {status} {body}"
        );
        let messages = body["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return received;
        }
        for message in messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case}: a delivered body is not JSON ({error}): {text}")
            });
            let trace = message["Attributes"]["AWSTraceHeader"]
                .as_str()
                .map(str::to_string);
            received.push((envelope, trace));
            let receipt = message["ReceiptHandle"].as_str().unwrap_or_default();
            let (status, body) = sqs(
                emulator,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteMessage answered {status} {body}"
            );
        }
    }
}

/// Publish `entries` events onto `bus` in requests of [`BATCH`] entries, each
/// carrying its own index and its own trace header.
async fn publish(emulator: &Emulator, case: &str, bus: &str, entries: usize) {
    let mut sent = 0usize;
    while sent < entries {
        let chunk = (entries - sent).min(BATCH);
        let batch: Vec<Value> = (0..chunk)
            .map(|offset| {
                let index = sent + offset;
                json!({
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": bus,
                    "TraceHeader": format!("Root=1-00000000-{index:024}"),
                    "Detail": json!({ "id": index }).to_string(),
                })
            })
            .collect();
        let (status, body) = events(emulator, "PutEvents", &json!({ "Entries": batch })).await;
        assert_eq!(
            status, 200,
            "case {case}: PutEvents answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case}: PutEvents reported a failed entry: {body}"
        );
        sent += chunk;
    }
}

/// RULE: one queue target receives one message per (event, matching rule),
/// whatever that count is and however the sends were grouped, and each message
/// carries its own event's envelope and trace header.
#[tokio::test]
async fn one_queue_receives_one_message_per_event_and_matching_rule() {
    // Each row: the case, how many events the workload publishes, and how many
    // ENABLED rules over the SAME queue match them — so the messages one request
    // hands that queue are (the request's entries x the rules), which the rows
    // take below, at, and above what one batched send carries.
    let rows: [(&str, usize, usize); 4] = [
        ("fewer messages than one batched send carries", 4, 1),
        ("exactly one batched send's worth", BATCH, 1),
        ("two rules over one queue: twice a request's entries", BATCH, 2),
        ("a bulk workload over two rules", 50, 2),
    ];

    for (case, entries, rules) in rows {
        let emulator = emulator();
        let bus = "profile-bus";
        let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": bus })).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateEventBus answered {status} {body}"
        );
        let (url, arn) = given_queue(&emulator, "profile-queue").await;
        for rule in 0..rules {
            given_rule(&emulator, case, bus, &format!("rule-{rule}"), &arn).await;
        }

        publish(&emulator, case, bus, entries).await;

        let delivered = drain(&emulator, case, &url).await;
        assert_eq!(
            delivered.len(),
            entries * rules,
            "case {case}: expected {} messages on the queue, got {}",
            entries * rules,
            delivered.len()
        );
        // Each published event, and how many messages carried it.
        let mut carried: BTreeMap<i64, usize> = BTreeMap::new();
        for (envelope, trace) in &delivered {
            let id = envelope["detail"]["id"].as_i64().unwrap_or_else(|| {
                panic!("case {case}: a delivered envelope names no published id: {envelope}")
            });
            *carried.entry(id).or_default() += 1;
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case}: expected the published source on every message, got {envelope}"
            );
            // The trace header belongs to the ENTRY that carried it, so the
            // message's own system attribute names that entry and no other.
            assert_eq!(
                trace.as_deref(),
                Some(format!("Root=1-00000000-{id:024}").as_str()),
                "case {case}: expected event {id}'s own trace header on its message, got {trace:?}"
            );
        }
        let expected: BTreeMap<i64, usize> = (0..entries)
            .map(|index| (i64::try_from(index).unwrap_or_default(), rules))
            .collect();
        assert_eq!(
            carried, expected,
            "case {case}: expected every published event on the queue once per matching rule"
        );
    }
}
