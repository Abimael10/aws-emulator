// SPDX-License-Identifier: CC0-1.0

//! What ONE event's fan-out reaches is decided by the rules and targets that matched
//! it, and never by what else the same `PutEvents` request carried.
//!
//! The RULE asserted here, one level above any single shape:
//!
//! > "For each matching rule, EventBridge delivers the event to every target of that
//! > rule." So the number of deliveries one accepted entry makes is the number of
//! > (matching rule, target) pairs the CLIENT configured on the bus — up to the
//! > bounds in force, [`MAX_RULES_PER_BUS`] rules each with at most
//! > [`MAX_TARGETS_PER_RULE`] targets — whatever the other entries of the same
//! > request matched. A delivery withheld because the request ALSO carried other
//! > entries, or because other rules on the bus were matched first, is an event that
//! > silently never arrives: the publisher is answered `FailedEntryCount` 0 and the
//! > target is never written to.
//!
//! The rows span what makes a request's fan-out large — many rules matching one
//! entry, a full batch of entries over a handful of rules, and a rule carrying the
//! most targets a rule may have beside many other matching rules — and each asserts
//! the EXACT number of deliveries its configuration asks for, counted off the one
//! queue every target in the row points at.
//!
//! `tests/all/bulk_fan_out.rs` is the suite for WHICH events a pattern selects over a
//! bulk workload; this one is about how MANY deliveries one request makes.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    MAX_RULES_PER_BUS, MAX_TARGETS_PER_RULE,
};

use crate::target_dispatch::{
    emulator, events, given_queue, sqs, Emulator, DETAIL_TYPE, SOURCE,
};

/// How many entries one `PutEvents` request may carry: `Entries` is modelled
/// `@length` max 10 ("you can batch multiple event entries into one request").
const MAX_ENTRIES: usize = 10;

/// An ENABLED rule called `rule` on the default bus matching this suite's source,
/// with `targets` copies of the queue `arn` as its targets.
async fn given_rule(emulator: &Emulator, rule: &str, arn: &str, targets: usize) {
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": rule,
            "EventPattern": json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {rule}: PutRule answered {status} {body}");
    let targets: Vec<Value> = (0..targets)
        .map(|index| json!({"Id": format!("q{index}"), "Arn": arn}))
        .collect();
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({"Rule": rule, "Targets": targets}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {rule}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {rule}: PutTargets reported a failed entry: {body}"
    );
}

/// How many messages the queue at `url` holds, deleting each as it is read.
async fn delivered(emulator: &Emulator, case: &str, url: &str) -> usize {
    let mut counted = 0;
    loop {
        let (status, body) = sqs(
            emulator,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: ReceiveMessage answered {status} {body}"
        );
        let messages = body["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return counted;
        }
        for message in messages {
            counted += 1;
            let receipt = message["ReceiptHandle"].as_str().unwrap_or_default();
            let (status, body) = sqs(
                emulator,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": receipt}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: DeleteMessage answered {status} {body}"
            );
        }
    }
}

/// One row: how many rules match, how many targets each carries, and how many
/// entries the request publishes.
struct Row {
    case: &'static str,
    rules: usize,
    targets_per_rule: usize,
    entries: usize,
}

/// RULE: every entry of an accepted `PutEvents` request is delivered to every target
/// of every rule whose pattern matched it, however many rules the bus carries,
/// however many targets those rules carry and however many entries the request did.
#[tokio::test]
async fn one_requests_fan_out_is_what_its_rules_and_targets_ask_for() {
    let rows = [
        Row {
            case: "one entry over many matching rules",
            rules: 120,
            targets_per_rule: 1,
            entries: 1,
        },
        Row {
            case: "a full batch of entries over a dozen matching rules",
            rules: 12,
            targets_per_rule: 1,
            entries: MAX_ENTRIES,
        },
        Row {
            case: "a full batch over rules carrying the most targets a rule may have",
            rules: 8,
            targets_per_rule: MAX_TARGETS_PER_RULE,
            entries: MAX_ENTRIES,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        assert!(
            row.rules <= MAX_RULES_PER_BUS && row.targets_per_rule <= MAX_TARGETS_PER_RULE,
            "case {case:?}: the row must stay inside the bounds a bus admits ({} rules, {} \
             targets per rule)",
            MAX_RULES_PER_BUS,
            MAX_TARGETS_PER_RULE
        );
        // One emulator per row, so the rules of one row are not also matched by the
        // next row's publication.
        let emulator = emulator();
        let (url, arn) = given_queue(&emulator, &format!("fan-out-{index}")).await;
        for rule in 0..row.rules {
            given_rule(
                &emulator,
                &format!("fan-out-{index}-{rule}"),
                &arn,
                row.targets_per_rule,
            )
            .await;
        }

        let entries: Vec<Value> = (0..row.entries)
            .map(|entry| {
                json!({
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({"entry": entry}).to_string(),
                })
            })
            .collect();
        let (status, body) = events(&emulator, "PutEvents", &json!({"Entries": entries})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutEvents reported a failed entry: {body}"
        );

        let wanted = row.entries * row.rules * row.targets_per_rule;
        let counted = delivered(&emulator, case, &url).await;
        assert_eq!(
            counted, wanted,
            "case {case:?}: expected {} entries x {} rules x {} targets = {wanted} deliveries \
             on the queue every target names, got {counted}",
            row.entries, row.rules, row.targets_per_rule
        );
    }
}
