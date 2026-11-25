// SPDX-License-Identifier: CC0-1.0

//! A bulk custom-bus fan-out onto subscriber queues that are ALREADY HOLDING messages,
//! driven against the emulator a customer STARTS, over its TLS data port, from several
//! clients at once.
//!
//! The RULE asserted here: **every ENABLED rule whose `EventPattern` matches an accepted
//! event delivers that event to each of its targets at least once, however many messages
//! that target queue is already holding.** Amazon SQS bounds a queue's depth only by the
//! in-flight quota it publishes — "approximately 120,000" messages for a standard queue,
//! which a RECEIVE is answered `OverLimit` past — and nothing by what a queue has already
//! accepted, so a producer running ahead of its consumer loses nothing: the send is taken
//! and the message is there to be drained. A subscriber of a thousand-event fan-out whose
//! queue carries a few messages from an earlier publication is exactly that producer.
//!
//! The rows are the four pattern forms one workload is partitioned by — `prefix`, a
//! `numeric` range, a plain value list over `detail-type` and `anything-but` — and each
//! asserts the exact set of published ids its queue received: no expected event missing,
//! no foreign event delivered. They differ in the FORM of the matcher and in the COUNT it
//! selects (50 / 500 / 1,000 / 950 of the same workload), and the complementary `prefix`
//! and `anything-but` sets must be disjoint and together cover exactly what the
//! match-everything rule received.
//!
//! What this adds to `crate::zz_bulk_profile`, which pins the same partition on the
//! assembled router with empty queues, is the axis that one cannot see: every queue is
//! PRE-LOADED before the workload starts, so the match-everything rule's queue holds more
//! than a thousand messages at once, and the whole exchange is made over the started
//! emulator's own TLS port by twelve clients at a time — the shape a fan-out subscriber
//! reads it in.
//!
//! Starting the built binary is the harness
//! `cfn_deployed_pipeline_on_a_started_emulator` owns; this suite drives an emulator of
//! its own through it.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};

/// The `Source` and `DetailType` every entry of the workload carries.
const SOURCE: &str = "com.example.started-fan-out";
const DETAIL_TYPE: &str = "Bulk Event";

/// How many events the workload publishes, and how many entries one request carries
/// ("you can batch multiple event entries into one request", bounded by `Entries`'
/// modelled maximum of 10).
const EVENTS: usize = 1_000;
const BATCH: usize = 10;

/// How many tenants the workload cycles through, so the `prefix` rule selects one in
/// [`TENANTS`] of the events and the complementary `anything-but` rule the rest.
const TENANTS: usize = 20;

/// How many messages each subscriber queue is holding BEFORE the workload starts, as a
/// queue whose consumer has not caught up does. The match-everything rule then hands its
/// queue [`EVENTS`] more on top of them.
const PRELOADED: usize = 5;

/// How many clients publish at once, and how many drain each queue at once.
const WORKERS: usize = 12;
const DRAINERS: usize = 3;

/// How many consecutive empty receives end one drain worker: a queue that answers this
/// many polls of a drain with nothing has nothing left for that worker. Several workers
/// share each queue, so a worker stops on what IT sees rather than on a count it was
/// told; the suite's verdict is the union of what they all received.
const EMPTY_POLLS: usize = 3;

/// One EventBridge call (`awsJson1_1`), which must answer 200.
fn events(started: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = started.call(
        "events",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AWSEvents.{operation}")),
        ],
        body.to_string().into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {operation}: EventBridge answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One Amazon SQS call (`awsJson1_0`), which must answer 200.
fn sqs(started: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = started.call(
        "sqs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AmazonSQS.{operation}")),
        ],
        body.to_string().into_bytes(),
    );
    assert_eq!(
        status, 200,
        "case {operation}: Amazon SQS answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A queue called `name` whose policy admits this service's deliveries for any rule of
/// `bus`, scoped the way a fan-out subscriber's policy is written, already holding
/// [`PRELOADED`] messages of its own; answers its url and the ARN a rule names it by.
fn queue(started: &Started, name: &str, bus: &str) -> (String, String) {
    let created = sqs(started, "CreateQueue", &json!({"QueueName": name}));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        started,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": "events.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": {"ArnLike": {"aws:SourceArn": format!(
                "arn:aws:events:{REGION}:{}:rule/{bus}/*",
                engine::EMULATOR_ACCOUNT_ID
            )}},
        }],
    })
    .to_string();
    sqs(
        started,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    );
    for index in 0..PRELOADED {
        sqs(
            started,
            "SendMessage",
            &json!({
                "QueueUrl": &url,
                "MessageBody": json!({"detail": {"id": -1, "held": index}}).to_string(),
            }),
        );
    }
    (url, arn)
}

/// One drain worker on the queue at `url`: receive, collect each published id, delete what
/// it was handed, until the queue answers [`EMPTY_POLLS`] polls in a row with nothing.
fn drain(started: &Started, case: &str, url: &str) -> BTreeSet<i64> {
    let mut ids = BTreeSet::new();
    let mut empty = 0usize;
    while empty < EMPTY_POLLS {
        let received = sqs(
            started,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        );
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            empty += 1;
            continue;
        }
        empty = 0;
        for message in messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case}: a delivered body is not JSON ({error}): {text}")
            });
            let id = envelope["detail"]["id"].as_i64().unwrap_or_else(|| {
                panic!("case {case}: a delivered envelope names no published id: {envelope}")
            });
            ids.insert(id);
            let receipt = message["ReceiptHandle"].as_str().unwrap_or_default();
            sqs(
                started,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": receipt}),
            );
        }
    }
    ids
}

/// RULE: every matching rule's queue receives every event the workload published, on a
/// started emulator, with each queue already holding messages of its own.
#[test]
fn a_bulk_fan_out_reaches_every_matching_rules_queue_that_is_already_holding_messages() {
    let started = Started::start();
    let bus = "started-fan-out-bus";
    events(&started, "CreateEventBus", &json!({"Name": bus}));

    // Each row: the rule, the pattern it selects events with, and which published index
    // that pattern selects. Sources: the EventBridge event-pattern reference for
    // `prefix`, `numeric` and `anything-but`, and a member's plain value list.
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

    let mut queues: Vec<Subscriber> = Vec::new();
    for (rule, pattern, selects) in &rows {
        let (url, arn) = queue(&started, &format!("started-{rule}"), bus);
        events(
            &started,
            "PutRule",
            &json!({
                "Name": rule,
                "EventBusName": bus,
                "EventPattern": pattern.to_string(),
                "State": "ENABLED",
            }),
        );
        let targeted = events(
            &started,
            "PutTargets",
            &json!({
                "Rule": rule,
                "EventBusName": bus,
                "Targets": [{"Id": "q", "Arn": arn}],
            }),
        );
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {rule}: PutTargets reported a failed entry: {targeted}"
        );
        queues.push((rule, url, *selects));
    }

    // The workload, published in requests of `BATCH` entries from `WORKERS` clients at
    // once: every request answers `FailedEntryCount` 0.
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let started = &started;
            let bus = bus.to_string();
            scope.spawn(move || {
                for chunk in (0..EVENTS / BATCH).filter(|chunk| chunk % WORKERS == worker) {
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
                    let answered = events(started, "PutEvents", &json!({"Entries": entries}));
                    assert_eq!(
                        answered["FailedEntryCount"],
                        json!(0),
                        "case batch {chunk}: PutEvents reported a failed entry: {answered}"
                    );
                    for entry in answered["Entries"].as_array().cloned().unwrap_or_default() {
                        assert!(
                            entry["EventId"].as_str().is_some_and(|id| !id.is_empty()),
                            "case batch {chunk}: an accepted entry carries no EventId: {entry}"
                        );
                    }
                }
            });
        }
    });

    // What each rule's queue received, drained by `DRAINERS` workers each.
    let emulator = &started;
    let drained: Vec<(&str, BTreeSet<i64>)> = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for (rule, url, _) in &queues {
            for _ in 0..DRAINERS {
                let rule: &str = rule;
                let url: &str = url;
                workers.push(scope.spawn(move || (rule, drain(emulator, rule, url))));
            }
        }
        workers
            .into_iter()
            .map(|worker| worker.join().expect("a drain worker runs to completion"))
            .collect()
    });

    let mut received: Vec<(&str, BTreeSet<i64>)> = Vec::new();
    for (rule, _, selects) in &queues {
        // Every id this rule's queue handed over, across the workers that drained it.
        let mut ids: BTreeSet<i64> = BTreeSet::new();
        for (name, got) in &drained {
            if name == rule {
                ids.extend(got.iter().copied());
            }
        }
        // The messages the queue was already holding carry no published index, so the
        // marker they share is dropped before the sets are compared.
        ids.remove(&-1);
        let expected: BTreeSet<i64> = (0..EVENTS)
            .filter(|index| selects(*index))
            .map(|index| i64::try_from(index).unwrap_or_default())
            .collect();
        let missing: Vec<i64> = expected.difference(&ids).copied().collect();
        let foreign: Vec<i64> = ids.difference(&expected).copied().collect();
        assert!(
            missing.is_empty() && foreign.is_empty(),
            "case {rule}: expected the {} published ids this pattern selects on a queue \
             already holding {PRELOADED} messages, got {} — missing {missing:?}, foreign \
             {foreign:?}",
            expected.len(),
            ids.len()
        );
        received.push((rule, ids));
    }

    // The complementary pair: the `prefix` and `anything-but` sets share nothing and
    // together are exactly what the match-everything rule received.
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
        "case complementary pair: expected the prefix and anything-but sets to share no \
         event, they share {:?}",
        prefix.intersection(&anything_but).collect::<Vec<_>>()
    );
    let together: BTreeSet<i64> = prefix.union(&anything_but).copied().collect();
    assert_eq!(
        together, everything,
        "case complementary pair: expected the prefix and anything-but sets together to \
         cover exactly the {} ids the match-everything rule received, got {}",
        everything.len(),
        together.len()
    );
}
