// SPDX-License-Identifier: CC0-1.0

//! The `Entries` bound `PutEvents` admits at, and what an ACCEPTED request's entries each
//! reach — end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter` with Amazon
//! SQS beside EventBridge on one engine, so a delivery is counted where a consumer counts it:
//! on the queue a rule targets.
//!
//! The RULE asserted here: `PutEvents.Entries` is `@required` and modelled
//! `@length(min: 1, max: 10)` (`smithy_query(query="operation", operation="PutEvents")`) and
//! the admission is all-or-nothing at that bound — a request past it is refused WHOLE with
//! one `ValidationException` and publishes none of its entries, then or later, while a
//! request inside it is accepted entire (`FailedEntryCount` 0, one non-empty EventId per
//! entry, all distinct) and EVERY accepted entry's envelope reaches each matching rule's
//! target EXACTLY ONCE, carrying that entry's own `Detail` and the `EventId` the answer
//! reported for it.
//!
//! What `tests/all/put_events_entry_count_limit.rs` asserts — the counts the bound admits and
//! the clause a refused request answers with — is not re-asserted here. This suite is about
//! what the ACCEPTED entries then are: one message per entry and no more (a fan-out that
//! delivered one entry twice, or dropped one of a batch, passes a count-only check), the
//! correspondence between the EventIds the answer reported and the envelopes delivered, and
//! that a refusal in the middle of a run leaves the next request's entries deliverable.
//!
//! The rows span the class rather than one recorded case: they differ along HOW MANY entries
//! one request carries (one, two, most of the bound, the bound itself) and along whether the
//! request before it was refused — and the whole table runs against ONE emulator in that
//! order, so an accepted request after a refused one is exercised rather than assumed. The
//! queue's grant is scoped by `aws:SourceArn` to the invoking rule, the form AWS's own target
//! guidance writes, so a grant a condition narrows is exercised as well.
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target lives in on
//! one engine) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};

use crate::target_dispatch::{
    drain, emulator, events, given_queue_with_policy, Emulator, DETAIL_TYPE, SOURCE,
};

/// `PutEventsRequestEntryList`'s modelled `@length` maximum, written out here rather than
/// imported so a wrong constant in the implementation cannot make a row pass.
const MAX_ENTRIES: usize = 10;

/// The service principal EventBridge invokes a resource-policy target as, written out here
/// for the same reason.
const SERVICE_PRINCIPAL: &str = "events.amazonaws.com";

/// The error code a refused input member of this service answers under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The custom bus every entry below is published to, and the rule on it.
const BUS: &str = "entry-once-bus";
const RULE: &str = "entry-once-rule";

/// How long a row waits for the deliveries it expects. Generous, because what it waits for is
/// a CONDITION rather than a span.
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a row keeps watching the queue after it has collected what it expected, to
/// conclude that nothing was delivered twice and that a refused request published nothing. A
/// bound on an ABSENCE, so it is the one span a slow host may spend without changing the
/// verdict: the accepted half of every row proves the same wiring delivers in a fraction of
/// it.
const QUIET_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the poll loops pause between looks at the queue.
const POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// One row: the case and how many entries the request carries. A count past [`MAX_ENTRIES`] is
/// a row that must be refused whole.
struct Row {
    case: &'static str,
    entries: usize,
}

/// `count` entries, each carrying the three members an event needs and a `Detail` naming which
/// entry of which request it is.
fn entries(label: &str, count: usize) -> Vec<Value> {
    (0..count)
        .map(|at| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": format!("{label}-{at}") }).to_string(),
                "EventBusName": BUS,
            })
        })
        .collect()
}

/// The `(marker, id)` of each delivered envelope.
fn delivered(messages: &[Value]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = messages
        .iter()
        .filter_map(|message| {
            let body = message["Body"].as_str()?;
            let envelope: Value = serde_json::from_str(body).ok()?;
            Some((
                envelope.pointer("/detail/marker")?.as_str()?.to_string(),
                envelope["id"].as_str()?.to_string(),
            ))
        })
        .collect();
    found.sort();
    found
}

/// The bus, the rule matching the published source, and the queue that rule's one target names
/// — carrying the grant this service needs, scoped by `aws:SourceArn` to the rule. Answers the
/// queue's url.
async fn given_bus_rule_and_queue(emulator: &Emulator) -> String {
    let (status, body) = events(emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
    let rule_arn = body["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no RuleArn in {body}"))
        .to_string();
    let policy = move |queue_arn: &str| {
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": SERVICE_PRINCIPAL },
                "Action": "sqs:SendMessage",
                "Resource": queue_arn,
                "Condition": { "ArnEquals": { "aws:SourceArn": &rule_arn } },
            }],
        })
        .to_string()
    };
    let (url, arn) = given_queue_with_policy(emulator, "entry-once-queue", Some(&policy)).await;
    let (status, body) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "queue", "Arn": arn }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutTargets answered {status} {body}");
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "PutTargets reported a failed entry: {body}"
    );
    url
}

/// Take everything waiting on the queue at `url`, deleting each message under the receipt
/// handle it was handed over with, and answer what was taken.
async fn take(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let messages = drain(emulator, url).await;
    for message in &messages {
        let handle = message["ReceiptHandle"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {message}"));
        let (status, body) = crate::target_dispatch::sqs(
            emulator,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteMessage answered {status} {body}"
        );
    }
    messages
}

/// Everything the queue at `url` is handed until `wanted` messages have been taken off it or a
/// generous deadline passes, and then everything more it is handed inside [`QUIET_WINDOW`]:
/// the second half is what makes a second copy of one entry's envelope visible.
async fn taken_then_watched(
    emulator: &Emulator,
    case: &str,
    url: &str,
    wanted: usize,
) -> (Vec<Value>, Vec<Value>) {
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(take(emulator, case, url).await);
        if collected.len() >= wanted || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
    let watching = std::time::Instant::now();
    let mut extra: Vec<Value> = Vec::new();
    while watching.elapsed() < QUIET_WINDOW {
        extra.extend(take(emulator, case, url).await);
        tokio::time::sleep(POLL_PAUSE).await;
    }
    (collected, extra)
}

/// Every accepted entry of one `PutEvents` request reaches the matching rule's target exactly
/// once, under the `EventId` the answer reported for it — and a request past the modelled
/// `Entries` bound is refused whole and publishes nothing, which leaves the request after it
/// deliverable.
#[tokio::test]
async fn every_accepted_entry_reaches_the_target_exactly_once() {
    // The rows run in this order against ONE emulator: an accepted request, a refused one,
    // and accepted requests after it — which is what exercises a refusal in the middle of a
    // run. (case, how many entries the request carries)
    let rows = [
        Row { case: "one entry", entries: 1 },
        Row { case: "one entry past the bound", entries: MAX_ENTRIES + 1 },
        Row { case: "two entries", entries: 2 },
        Row { case: "twice the bound", entries: MAX_ENTRIES * 2 },
        Row { case: "most of the bound", entries: MAX_ENTRIES - 1 },
        Row { case: "the bound itself", entries: MAX_ENTRIES },
    ];

    let emulator = emulator();
    let url = given_bus_rule_and_queue(&emulator).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let label = format!("r{index}");
        let submitted = entries(&label, row.entries);
        let (status, answer) =
            events(&emulator, "PutEvents", &json!({ "Entries": &submitted })).await;

        if row.entries > MAX_ENTRIES {
            // Past the bound: one refusal about the list member, and nothing published.
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {answer}"
            );
            let code = answer["__type"]
                .as_str()
                .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
                .unwrap_or_default();
            assert_eq!(
                code, VALIDATION_EXCEPTION,
                "case {case:?}: answered {answer}"
            );
            let watching = std::time::Instant::now();
            while watching.elapsed() < QUIET_WINDOW {
                let arrived = take(&emulator, case, &url).await;
                assert!(
                    arrived.is_empty(),
                    "case {case:?}: a request refused whole publishes nothing, got {:?}",
                    delivered(&arrived)
                );
                tokio::time::sleep(POLL_PAUSE).await;
            }
            continue;
        }

        // Inside the bound: accepted entire, one result entry per submitted entry, each with
        // its own non-empty EventId.
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {answer}"
        );
        assert_eq!(
            answer["FailedEntryCount"],
            json!(0),
            "case {case:?}: every entry is accepted, got {answer}"
        );
        let reported: Vec<String> = answer["Entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|entry| entry["EventId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported.len(),
            row.entries,
            "case {case:?}: one result entry per submitted entry, got {answer}"
        );
        assert!(
            reported.iter().all(|id| !id.is_empty()),
            "case {case:?}: every accepted entry carries an EventId, got {reported:?}"
        );
        let distinct: std::collections::BTreeSet<&String> = reported.iter().collect();
        assert_eq!(
            distinct.len(),
            reported.len(),
            "case {case:?}: every accepted entry's EventId is its own, got {reported:?}"
        );

        // Every one of them reaches the target exactly once, carrying its own marker and the
        // EventId the answer reported for it.
        let (collected, extra) = taken_then_watched(&emulator, case, &url, row.entries).await;
        let mut expected: Vec<(String, String)> = (0..row.entries)
            .map(|at| (format!("{label}-{at}"), reported[at].clone()))
            .collect();
        expected.sort();
        assert_eq!(
            delivered(&collected),
            expected,
            "case {case:?}: expected one envelope per accepted entry, each under the EventId \
             the answer reported, got {:?}",
            delivered(&collected)
        );
        assert!(
            extra.is_empty(),
            "case {case:?}: an accepted entry is delivered exactly once, so nothing more \
             reaches the target, got {:?}",
            delivered(&extra)
        );
    }
}
