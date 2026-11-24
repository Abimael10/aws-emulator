// SPDX-License-Identifier: CC0-1.0

//! What a FILTERED Amazon SQS subscription receives on the emulator a client STARTS: the built
//! binary, on its own port, with request authentication on, reached over TLS with SigV4-signed
//! calls — the configuration a recorded client drives, and the one in which Amazon SNS has to
//! reach the queue as its own service principal for anything to be delivered at all.
//!
//! RULE: **a subscription's `FilterPolicy` decides ONE message's delivery at the instant that
//! message is published — "FilterPolicy – The simple JSON object that lets your subscriber
//! receive only a subset of messages, rather than receiving every message published to the
//! topic" (`smithy_query(query="doc",shape="SubscribeInput",member="Attributes")`), applied by
//! the `Publish` that "delivers the message to each endpoint that is subscribed to the topic"
//! — so the policy IN FORCE at a publish governs that publish alone: a message the policy
//! rejected is dropped then and there and is never queued, re-evaluated or replayed, and a
//! later write of the attribute governs every publish after it without releasing anything an
//! earlier policy withheld and without delivering anything a second time. What a receive then
//! hands over is what the queue holds: "if a message is available, the call returns sooner than
//! WaitTimeSeconds. If no messages are available and the wait time expires, the call does not
//! return a message list"
//! (`smithy_query(query="doc",shape="ReceiveMessageRequest",member="WaitTimeSeconds")`).**
//!
//! The rows differ along what the rule generalises over: the message property the policy
//! judges, the values published under it, and how the policy is widened afterwards (another
//! value added to its list, or the list replaced by a condition of another operator).
//!
//! What this suite adds to the in-process cover of the same rule
//! ([`crate::sns_widened_filter_policy_admits_the_publishes_that_follow`], which assembles the
//! router with authentication OFF) is the STARTED binary: every call is signed and verified,
//! each arrives over TLS, and the fan-out into the queue is a cross-service call this process
//! makes as `sns.amazonaws.com` against the queue's own policy. A withholding that is really a
//! delivery nobody could make would pass in process and fail here, which is why the walk is
//! driven end to end on the started emulator: the unfiltered TWIN subscription tells a message
//! the policy withheld apart from one the fan-out never made, and the depths Amazon SQS reports
//! for the filtered queue show nothing was kept for it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{decoded_entries, opt_element};

/// The service principal Amazon SNS delivers to a subscribed queue as, which is the identity a
/// queue policy has to grant for a notification to reach the queue at all.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The depth members Amazon SQS reports, each of which stays at zero for a queue whose
/// subscription the policy withheld the message from
/// (`smithy_query(query="doc",shape="GetQueueAttributesRequest",member="AttributeNames")`).
const DEPTH_ATTRIBUTES: &[&str] = &[
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
    "ApproximateNumberOfMessagesDelayed",
];

/// The wait each receive below asks for, in seconds: "the duration (in seconds) for which the
/// call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`). It is the wait a client observing a delivery it
/// cannot otherwise be told about makes, and what makes a receive that hands nothing over
/// evidence that nothing was there to hand over.
const WAIT_TIME_SECONDS: u64 = 5;

/// How long a delivery is looked for before the queue is reported to hold none: far longer than
/// a fan-out made in this process takes, so a loaded host changes no verdict. The CONDITION is
/// waited for, never a span.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// One Amazon SNS `awsQuery` call (API version 2010-03-31) that must be answered.
fn sns(emulator: &Started, case: &str, action: &str, members: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", "2010-03-31", action, members);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SNS {action}, got {status} with {text}; \
         the log:\n{}",
        emulator.log()
    );
    text
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be answered.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {text}; the log:\n{}",
        emulator.log()
    );
    if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

/// The queue policy AWS documents for a queue subscribed to an SNS topic: the service
/// principal, allowed `sqs:SendMessage` on that queue for messages of that topic alone.
fn grant_to_sns(queue_arn: &str, topic_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": SNS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": topic_arn}},
        }],
    })
    .to_string()
}

/// A queue called `name` that the topic `topic_arn` may deliver to, as `(url, arn)`.
fn given_queue(emulator: &Started, case: &str, name: &str, topic_arn: &str) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": name }));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let reported = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    );
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": grant_to_sns(&arn, topic_arn) },
        }),
    );
    (url, arn)
}

/// Subscribe the queue `queue_arn` to `topic` with `attributes` as the subscription's own, and
/// answer the subscription's ARN.
fn subscribe(
    emulator: &Started,
    case: &str,
    topic: &str,
    queue_arn: &str,
    attributes: &[(&str, &str)],
) -> String {
    let mut members: Vec<(String, String)> = vec![
        ("TopicArn".to_string(), topic.to_string()),
        ("Protocol".to_string(), "sqs".to_string()),
        ("Endpoint".to_string(), queue_arn.to_string()),
        ("ReturnSubscriptionArn".to_string(), "true".to_string()),
    ];
    for (at, (name, value)) in attributes.iter().enumerate() {
        let entry = at + 1;
        members.push((format!("Attributes.entry.{entry}.key"), (*name).to_string()));
        members.push((
            format!("Attributes.entry.{entry}.value"),
            (*value).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let answered = sns(emulator, case, "Subscribe", &borrowed);
    opt_element(&answered, "SubscriptionArn")
        .unwrap_or_else(|| panic!("case {case:?}: expected a SubscriptionArn, got {answered}"))
}

/// Publish `body` to `topic` carrying the one `String` message attribute `(name, value)`.
fn publish(emulator: &Started, case: &str, topic: &str, body: &str, attribute: (&str, &str)) {
    sns(
        emulator,
        case,
        "Publish",
        &[
            ("TopicArn", topic),
            ("Message", body),
            ("MessageAttributes.entry.1.Name", attribute.0),
            ("MessageAttributes.entry.1.Value.DataType", "String"),
            ("MessageAttributes.entry.1.Value.StringValue", attribute.1),
        ],
    );
}

/// ONE long-polling receive of the queue at `url`, as the published messages the notifications
/// it handed over carry — each one deleted, so a later look finds only later deliveries.
///
/// The receive asks for [`WAIT_TIME_SECONDS`], so a hand-over is answered as soon as there is
/// one and an empty answer is one the queue had nothing to hand over for the whole wait.
fn long_polled(emulator: &Started, case: &str, url: &str) -> Vec<String> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    );
    let mut carried = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        carried.push(published_message(
            message["Body"].as_str().unwrap_or_default(),
        ));
        sqs(
            emulator,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        );
    }
    carried
}

/// What the queue at `url` holds once a delivery has reached it: looked for again until it has
/// one, or until [`DELIVERY_DEADLINE`] has passed.
fn delivered(emulator: &Started, case: &str, url: &str, what: &str) -> Vec<String> {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let held = long_polled(emulator, case, url);
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {what} on the subscribed queue, the queue handed nothing \
             over; the log:\n{}",
            emulator.log()
        );
    }
}

/// The published message the notification `body` carries, or the body itself when the
/// subscription asked for raw delivery.
fn published_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|envelope| {
            envelope
                .get("Message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string())
}

/// Assert that every depth Amazon SQS reports for the queue at `url` is zero: a message the
/// policy withheld is not waiting, not in flight and not delayed.
fn assert_empty_depth(emulator: &Started, case: &str, url: &str) {
    let reported = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": DEPTH_ATTRIBUTES }),
    );
    for name in DEPTH_ATTRIBUTES {
        // The member has to be THERE: a reader that omits it must fail here rather than read as
        // a queue holding nothing.
        let depth = reported["Attributes"][name].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected Amazon SQS to report {name}, got {reported}")
        });
        assert_eq!(
            depth, "0",
            "case {case:?}: expected {name} to be 0 on the queue the policy withheld the \
             message from, got {depth:?} in {reported}"
        );
    }
}

/// The `FilterPolicy` the subscription reports right now, or `None` when it reports none.
fn reported_filter_policy(emulator: &Started, case: &str, subscription: &str) -> Option<String> {
    let answered = sns(
        emulator,
        case,
        "GetSubscriptionAttributes",
        &[("SubscriptionArn", subscription)],
    );
    decoded_entries(&answered).remove("FilterPolicy")
}

/// One row: the property the policy judges, the values published under it, and the widening
/// written after the withheld publish.
struct Row {
    case: &'static str,
    /// The message attribute the policy judges.
    property: &'static str,
    /// The value the narrow policy admits.
    admitted: &'static str,
    /// The value the narrow policy withholds and the widened one admits.
    withheld: &'static str,
    /// The policy in force for the first two publishes.
    narrow: &'static str,
    /// The policy written afterwards.
    widened: &'static str,
}

/// The rows: each judges a property of its own and is widened a different way.
const ROWS: &[Row] = &[
    Row {
        case: "another value added to the policy's list",
        property: "kind",
        admitted: "wanted",
        withheld: "unwanted",
        narrow: r#"{"kind": ["wanted"]}"#,
        widened: r#"{"kind": ["wanted", "unwanted"]}"#,
    },
    Row {
        case: "the list replaced by an anything-but condition",
        property: "region",
        admitted: "eu-west-1",
        withheld: "eu-north-1",
        narrow: r#"{"region": ["eu-west-1"]}"#,
        widened: r#"{"region": [{"anything-but": "ap-south-1"}]}"#,
    },
];

/// RULE (see the module docs): on the emulator a client starts, the policy in force at each
/// publish decides that publish alone — the admitted message is delivered, the withheld one
/// reaches the unfiltered twin and nothing else, and the widening written afterwards admits the
/// publishes that follow it while releasing no backlog and redelivering nothing.
#[test]
fn a_filtered_sqs_subscription_takes_what_the_policy_in_force_at_each_publish_admits() {
    let emulator = Started::start();
    for (at, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("started-filtered-{at}");
        let topic = opt_element(
            &sns(&emulator, case, "CreateTopic", &[("Name", &name)]),
            "TopicArn",
        )
        .unwrap_or_else(|| panic!("case {case:?}: CreateTopic must answer the topic's ARN"));
        let (subject_url, subject_arn) =
            given_queue(&emulator, case, &format!("{name}-subject"), &topic);
        let (twin_url, twin_arn) = given_queue(&emulator, case, &format!("{name}-twin"), &topic);

        let subject = subscribe(
            &emulator,
            case,
            &topic,
            &subject_arn,
            &[
                ("FilterPolicy", row.narrow),
                ("RawMessageDelivery", "false"),
            ],
        );
        let twin = subscribe(
            &emulator,
            case,
            &topic,
            &twin_arn,
            &[("RawMessageDelivery", "false")],
        );
        assert_eq!(
            reported_filter_policy(&emulator, case, &subject).as_deref(),
            Some(row.narrow),
            "case {case:?}: the subscription must report the policy it was subscribed with"
        );
        assert_eq!(
            reported_filter_policy(&emulator, case, &twin),
            None,
            "case {case:?}: the twin was given no filter policy, so it reports none"
        );

        // 1. The publish the narrow policy admits is delivered to the filtered subscription,
        //    and to the unfiltered twin with it.
        let warm = format!("{{\"nonce\":\"warm-{at}\"}}");
        publish(&emulator, case, &topic, &warm, (row.property, row.admitted));
        assert_eq!(
            delivered(&emulator, case, &subject_url, "the admitted publish"),
            vec![warm.clone()],
            "case {case:?}: the policy admits that publish, so it must be delivered"
        );
        assert_eq!(
            delivered(&emulator, case, &twin_url, "the admitted publish"),
            vec![warm.clone()],
            "case {case:?}: the unfiltered twin receives every publish"
        );

        // 2. The publish the narrow policy rejects reaches the twin — which is what tells a
        //    withheld delivery apart from a fan-out nobody made — and is withheld from the
        //    filtered subscription, whose queue reports nothing of it at all.
        let held_back = format!("{{\"nonce\":\"withheld-{at}\"}}");
        publish(
            &emulator,
            case,
            &topic,
            &held_back,
            (row.property, row.withheld),
        );
        assert_eq!(
            delivered(&emulator, case, &twin_url, "the withheld publish"),
            vec![held_back.clone()],
            "case {case:?}: the unfiltered twin receives the publish the policy rejected"
        );
        let withheld_from = long_polled(&emulator, case, &subject_url);
        assert!(
            withheld_from.is_empty(),
            "case {case:?}: the policy rejects that publish, so the queue must hand nothing \
             over, got {withheld_from:?}"
        );
        assert_empty_depth(&emulator, case, &subject_url);

        // 3. The widened policy is stored and reported, and releases nothing the narrow one
        //    withheld: a rejected message was dropped at its publish, so there is no backlog.
        sns(
            &emulator,
            case,
            "SetSubscriptionAttributes",
            &[
                ("SubscriptionArn", &subject),
                ("AttributeName", "FilterPolicy"),
                ("AttributeValue", row.widened),
            ],
        );
        assert_eq!(
            reported_filter_policy(&emulator, case, &subject).as_deref(),
            Some(row.widened),
            "case {case:?}: the widened policy must be the one the subscription reports"
        );
        let released = long_polled(&emulator, case, &subject_url);
        assert!(
            released.is_empty(),
            "case {case:?}: widening the policy must release no backlog, got {released:?}"
        );

        // 4. And the next publish of the class the widening admits IS delivered, while nothing
        //    already published arrives again.
        let probe = format!("{{\"nonce\":\"probe-{at}\"}}");
        publish(
            &emulator,
            case,
            &topic,
            &probe,
            (row.property, row.withheld),
        );
        assert_eq!(
            delivered(&emulator, case, &subject_url, "the readmitted publish"),
            vec![probe.clone()],
            "case {case:?}: the publish after the widening must be delivered under it"
        );
        let after = long_polled(&emulator, case, &subject_url);
        assert!(
            after.is_empty(),
            "case {case:?}: neither the withheld nor the delivered message may arrive again, \
             got {after:?}"
        );
        assert_empty_depth(&emulator, case, &subject_url);
    }
}
