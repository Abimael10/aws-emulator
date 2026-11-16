// SPDX-License-Identifier: CC0-1.0

//! A BULK publication onto a custom bus reaches the queue target of every rule
//! whose `EventPattern` matches it, over the four pattern forms a fan-out is
//! partitioned by.
//!
//! The RULE asserted here: `PutEvents` on a custom bus accepts entries in batches
//! of up to ten and answers `FailedEntryCount` 0 with an `EventId` per accepted
//! entry; every ENABLED rule on that bus whose `EventPattern` matches an accepted
//! event delivers that event to each of its targets at least once, independent of
//! order, and the deliveries a bulk workload makes converge. Each delivery arrives
//! on the target queue as the EventBridge envelope whose `detail` is the published
//! detail.
//!
//! The rows span the pattern forms the partition is built from — `prefix`, a
//! `numeric` range, a plain value list over `detail-type`, and `anything-but` —
//! and each asserts the exact set of published ids its rule's queue received: no
//! expected event missing, no foreign event delivered. The complementary `prefix`
//! and `anything-but` rules must share no event and together cover exactly what
//! the match-everything rule received.
//!
//! The harness is the crate's shared one (`tests/all/target_dispatch.rs`): one
//! engine with EventBridge and the services a target lives in mounted on it, and
//! one signed-shaped `awsJson` call per request. `tests/all/bulk_profile.rs` is
//! the suite for what ONE queue receives when a request hands it more messages
//! than a single batched send carries.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::target_dispatch::{
    emulator, events, given_queue_with_policy, sqs, Emulator, DETAIL_TYPE, REGION, SOURCE,
};

/// How many events the workload publishes.
const EVENTS: usize = 1_000;

/// How many entries one `PutEvents` request carries: `Entries` is modelled
/// `@length` max 10 ("you can batch multiple event entries into one request").
const BATCH: usize = 10;

/// How many distinct tenants the workload cycles through, so the `prefix` rule
/// selects one in [`TENANTS`] of the events and the complementary `anything-but`
/// rule the rest.
const TENANTS: usize = 20;

/// The policy a fan-out subscriber's queue carries: this service's principal
/// allowed `sqs:SendMessage`, scoped by `aws:SourceArn` under `ArnLike` to the
/// rules of `bus` — the form a subscriber of one bus is granted by.
fn granting_the_rules_of(bus: &str) -> impl Fn(&str) -> String + '_ {
    move |queue_arn: &str| {
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowEventBridge",
                "Effect": "Allow",
                "Principal": {
                    "Service": service_com_amazonaws_eventbridge_awsevents::state::SERVICE_PRINCIPAL
                },
                "Action": "sqs:SendMessage",
                "Resource": queue_arn,
                "Condition": {"ArnLike": {"aws:SourceArn": format!(
                    "arn:aws:events:{REGION}:{}:rule/{bus}/*",
                    engine::emulator_account_id()
                )}},
            }],
        })
        .to_string()
    }
}

/// An ENABLED rule called `rule` on `bus` carrying `pattern`, whose only target is
/// the queue `arn`.
async fn given_rule(emulator: &Emulator, bus: &str, rule: &str, pattern: &Value, arn: &str) {
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": rule,
            "EventBusName": bus,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "case {rule}: PutRule answered {status} {body}");
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
    assert_eq!(status, 200, "case {rule}: PutTargets answered {status} {body}");
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {rule}: PutTargets reported a failed entry: {body}"
    );
}

/// Every published id the queue at `url` received, draining it until it reports
/// nothing left and deleting each message as it is read.
pub(crate) async fn drained(emulator: &Emulator, case: &str, url: &str) -> BTreeSet<i64> {
    let mut ids = BTreeSet::new();
    loop {
        let (status, body) = sqs(
            emulator,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: ReceiveMessage answered {status} {body}"
        );
        let messages = body["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return ids;
        }
        for message in messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case}: a delivered body is not JSON ({error}): {text}")
            });
            ids.insert(envelope["detail"]["id"].as_i64().unwrap_or_else(|| {
                panic!("case {case}: a delivered envelope names no published id: {envelope}")
            }));
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

/// RULE: every ENABLED rule whose pattern matches an accepted event delivers it to
/// that rule's queue target, over a bulk workload published in batches of ten.
#[tokio::test]
async fn a_bulk_publication_reaches_every_matching_rules_queue() {
    let emulator = emulator();
    let bus = "bulk-bus";
    let (status, body) = events(&emulator, "CreateEventBus", &json!({ "Name": bus })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");

    // Each row: the rule, its pattern, and which published index that pattern
    // selects. Sources: the EventBridge event-pattern reference for `prefix`,
    // `numeric` and `anything-but`, and a member's plain value list.
    type Row = (&'static str, Value, fn(usize) -> bool);
    /// One rule's queue, with the predicate deciding which events it must hold.
    type Subscriber = (&'static str, String, fn(usize) -> bool);
    let rows: [Row; 4] = [
        (
            "prefix",
            json!({"source": [SOURCE], "detail": {"tenant": [{"prefix": "t-000"}]}}),
            |index| index % TENANTS == 0,
        ),
        (
            "numeric",
            json!({"source": [SOURCE], "detail": {"n": [{"numeric": [">=", EVENTS / 2]}]}}),
            |index| index >= EVENTS / 2,
        ),
        (
            "everything",
            json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
            |_| true,
        ),
        (
            "anything-but",
            json!({"source": [SOURCE], "detail": {"tenant": [{"anything-but": ["t-000"]}]}}),
            |index| index % TENANTS != 0,
        ),
    ];

    let granting = granting_the_rules_of(bus);
    let mut queues: Vec<Subscriber> = Vec::new();
    for (rule, pattern, selects) in &rows {
        let (url, arn) =
            given_queue_with_policy(&emulator, &format!("bulk-{rule}"), Some(&granting)).await;
        given_rule(&emulator, bus, rule, pattern, &arn).await;
        queues.push((rule, url, *selects));
    }

    // The workload, in requests of `BATCH` entries: every batch answers
    // `FailedEntryCount` 0 with an `EventId` per entry, and no two entries are
    // given the same id.
    let mut minted: BTreeSet<String> = BTreeSet::new();
    for chunk in 0..EVENTS / BATCH {
        let entries: Vec<Value> = (0..BATCH)
            .map(|offset| {
                let index = chunk * BATCH + offset;
                json!({
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": bus,
                    "Detail": json!({
                        "tenant": format!("t-{:03}", index % TENANTS),
                        "n": index,
                        "id": index,
                    }).to_string(),
                })
            })
            .collect();
        let (status, body) = events(&emulator, "PutEvents", &json!({ "Entries": entries })).await;
        assert_eq!(
            status, 200,
            "case batch {chunk}: PutEvents answered {status} {body}"
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case batch {chunk}: PutEvents reported a failed entry: {body}"
        );
        for entry in body["Entries"].as_array().cloned().unwrap_or_default() {
            let id = entry["EventId"]
                .as_str()
                .unwrap_or_else(|| panic!("case batch {chunk}: no EventId in {entry}"))
                .to_string();
            assert!(
                minted.insert(id.clone()),
                "case batch {chunk}: the EventId {id} was minted for another entry too"
            );
        }
    }
    assert_eq!(
        minted.len(),
        EVENTS,
        "expected one EventId per accepted entry, got {}",
        minted.len()
    );

    let mut received: Vec<(&str, BTreeSet<i64>)> = Vec::new();
    for (rule, url, selects) in queues {
        let ids = drained(&emulator, rule, &url).await;
        let expected: BTreeSet<i64> = (0..EVENTS)
            .filter(|index| selects(*index))
            .map(|index| i64::try_from(index).unwrap_or_default())
            .collect();
        assert_eq!(
            ids, expected,
            "case {rule}: expected the {} published ids this rule's pattern selects, got {}",
            expected.len(),
            ids.len()
        );
        received.push((rule, ids));
    }

    // The complementary pair: the `prefix` and `anything-but` sets share nothing
    // and together are exactly what the match-everything rule received.
    let set = |name: &str| -> BTreeSet<i64> {
        received
            .iter()
            .find(|(rule, _)| *rule == name)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_else(|| panic!("the suite drained a queue for {name}"))
    };
    let (prefix, anything_but, everything) = (set("prefix"), set("anything-but"), set("everything"));
    assert!(
        prefix.is_disjoint(&anything_but),
        "expected the prefix and anything-but sets to share no event, they share {:?}",
        prefix.intersection(&anything_but).collect::<Vec<_>>()
    );
    let together: BTreeSet<i64> = prefix.union(&anything_but).copied().collect();
    assert_eq!(
        together, everything,
        "expected the prefix and anything-but sets together to cover exactly what the \
         match-everything rule received ({} ids), got {}",
        everything.len(),
        together.len()
    );
}
