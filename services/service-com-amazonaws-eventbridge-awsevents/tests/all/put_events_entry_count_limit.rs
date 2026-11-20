// SPDX-License-Identifier: CC0-1.0

//! The `Entries` count one `PutEvents` may carry, and what a request inside that
//! bound delivers — end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`,
//! with Amazon SQS mounted beside EventBridge on one engine so a delivery is
//! asserted where a client observes one: on the queue a rule targets.
//!
//! The RULE asserted here: `PutEvents.Entries` is `@required` and modelled
//! `@length(min: 1, max: 10)`
//! (`smithy_query(query="operation", operation="PutEvents")`), so a request
//! carrying more than ten entries is refused WHOLE — one `ValidationException`
//! (HTTP 400) naming the list member, quoting the entries the request itself
//! carried, and publishing NONE of them — while a request of exactly ten is
//! accepted and every one of the ten reaches the target of the rule that matched
//! it on the bus it was published to.
//!
//! The rows span the class rather than the recorded case: the accepted rows differ
//! along how many entries the request carries (one, half the bound, the bound
//! itself) and the refused rows along how far past the bound it is (one over, two
//! over, twice the bound) — and both halves run against a CUSTOM bus, whose rule
//! and target are the only ones the entries can reach.
//!
//! The harness (the router with EventBridge, Amazon SQS and the services a target
//! lives in on one engine) is `tests/all/target_dispatch.rs`'s, which is also where
//! the grant a target queue needs is written once.

use serde_json::{json, Value};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, sqs, Emulator, DETAIL_TYPE, SOURCE,
};

/// `PutEventsRequestEntryList`'s modelled `@length` maximum, written out here
/// rather than imported so a wrong constant in the implementation cannot make a
/// row pass.
const MAX_ENTRIES: usize = 10;

/// The custom bus every entry below is published to, and the rule on it.
pub(crate) const BUS: &str = "entry-count-bus";
const RULE: &str = "entry-count-rule";

/// The error code a refused input member of this service answers under.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The bus, the rule on it matching the published source, and the queue that
/// rule's one target names — with the policy EventBridge needs to send to it.
/// Answers the queue's url.
pub(crate) async fn given_bus_rule_and_queue(emulator: &Emulator, queue: &str) -> String {
    let (status, body) = events(emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    let (url, arn) = given_queue(emulator, queue).await;
    let pattern = json!({ "source": [SOURCE] }).to_string();
    let (status, body) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
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

/// `count` entries onto [`BUS`], each carrying the three members an event needs to
/// be published and a detail that says which of them it is.
fn entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "index": index }).to_string(),
                "EventBusName": BUS,
            })
        })
        .collect()
}

/// The `index` each delivered envelope's detail carries, in ascending order.
pub(crate) fn delivered_indexes(messages: &[Value]) -> Vec<i64> {
    let mut found: Vec<i64> = messages
        .iter()
        .filter_map(|message| {
            let body = message["Body"].as_str()?;
            let envelope: Value = serde_json::from_str(body).ok()?;
            envelope.pointer("/detail/index")?.as_i64()
        })
        .collect();
    found.sort_unstable();
    found
}

/// Everything waiting on the queue at `url`, looked at until `wanted` messages have
/// been collected or a generous deadline passes: the wait is on the CONDITION, so a
/// slow host delays the test instead of breaking it.
pub(crate) async fn received(emulator: &Emulator, url: &str, wanted: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(emulator, url).await);
        if collected.len() >= wanted || std::time::Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// How long the refused half keeps looking at the queue before concluding that a
/// request refused as a whole published nothing. A bound on an ABSENCE, so it is
/// the one span a slow host may spend without changing the verdict: the accepted
/// half above proves the same wiring delivers in a fraction of it.
const WITHHELD_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// A request of up to ten entries is accepted and every entry reaches the target of
/// the rule that matched it on the custom bus it was published to.
#[tokio::test]
async fn a_request_within_the_entry_bound_delivers_every_entry() {
    // (case, how many entries the request carries)
    let rows = [
        ("one entry", 1usize),
        ("half the bound", MAX_ENTRIES / 2),
        ("exactly the bound", MAX_ENTRIES),
    ];

    for (case, count) in rows {
        let emulator = emulator();
        let url = given_bus_rule_and_queue(&emulator, &format!("entry-count-{count}")).await;
        let (status, answer) = events(
            &emulator,
            "PutEvents",
            &json!({ "Entries": entries(count) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {answer}"
        );
        assert_eq!(
            answer["FailedEntryCount"],
            json!(0),
            "case {case:?}: every entry is accepted, got {answer}"
        );
        assert_eq!(
            answer["Entries"].as_array().map(Vec::len),
            Some(count),
            "case {case:?}: one result entry per submitted entry, got {answer}"
        );
        let messages = received(&emulator, &url, count).await;
        let indexes = delivered_indexes(&messages);
        let expected: Vec<i64> = (0..count).map(|index| index as i64).collect();
        assert_eq!(
            indexes, expected,
            "case {case:?}: expected every entry on the queue, got {indexes:?} from \
             {messages:?}"
        );

        // Each delivery is a message a consumer can TAKE: deleted under the receipt
        // handle it was handed over with, the queue holds nothing afterwards —
        // neither waiting nor in flight.
        for message in &messages {
            let handle = message["ReceiptHandle"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no ReceiptHandle in {message}"));
            let (status, deleted) = sqs(
                &emulator,
                "DeleteMessage",
                &json!({ "QueueUrl": &url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage answered {status} {deleted}"
            );
        }
        let (status, attributes) = sqs(
            &emulator,
            "GetQueueAttributes",
            &json!({
                "QueueUrl": &url,
                "AttributeNames": [
                    "ApproximateNumberOfMessages",
                    "ApproximateNumberOfMessagesNotVisible",
                ],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueueAttributes answered {status} {attributes}"
        );
        for name in [
            "ApproximateNumberOfMessages",
            "ApproximateNumberOfMessagesNotVisible",
        ] {
            assert_eq!(
                attributes["Attributes"][name].as_str(),
                Some("0"),
                "case {case:?}: every delivered message was taken off the queue, so {name} \
                 must be zero, got {attributes}"
            );
        }
    }
}

/// A request of more than ten entries is refused WHOLE: one `ValidationException`
/// naming the list member and the bound it broke, and not one of its entries
/// reaches the rule's target.
#[tokio::test]
async fn a_request_past_the_entry_bound_is_refused_whole() {
    // (case, how many entries the request carries)
    let rows = [
        ("one past the bound", MAX_ENTRIES + 1),
        ("two past the bound", MAX_ENTRIES + 2),
        ("twice the bound", MAX_ENTRIES * 2),
    ];

    for (case, count) in rows {
        let emulator = emulator();
        let url = given_bus_rule_and_queue(&emulator, &format!("over-the-bound-{count}")).await;
        let submitted = entries(count);
        let (status, refused) = events(
            &emulator,
            "PutEvents",
            &json!({ "Entries": &submitted }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {refused}"
        );
        let code = refused["__type"]
            .as_str()
            .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
            .unwrap_or_default();
        assert_eq!(
            code, VALIDATION_EXCEPTION,
            "case {case:?}: answered {refused}"
        );
        let message = refused["message"].as_str().unwrap_or_default();
        // The clause names the list member and the bound it broke, and quotes the
        // list the request carried: every entry it held is in it, which a count
        // summary would have lost.
        assert!(
            message.starts_with("1 validation error detected: Value '[")
                && message.ends_with(
                    "' at 'entries' failed to satisfy constraint: Member must have length \
                     less than or equal to 10"
                ),
            "case {case:?}: expected one clause about the entries list, got {message:?}"
        );
        for (index, _) in submitted.iter().enumerate() {
            let rendered = format!("detail={{\"index\":{index}}}");
            assert!(
                message.contains(&rendered),
                "case {case:?}: the clause quotes every submitted entry, so {rendered:?} \
                 must be in {message:?}"
            );
        }
        // Nothing was published: the request was refused as a whole, so the rule's
        // target receives none of its entries, however long the queue is watched.
        let watching = std::time::Instant::now();
        while watching.elapsed() < WITHHELD_WINDOW {
            let messages = drain(&emulator, &url).await;
            assert!(
                messages.is_empty(),
                "case {case:?}: a refused request publishes nothing, got {messages:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
