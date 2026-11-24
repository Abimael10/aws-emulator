// SPDX-License-Identifier: CC0-1.0

//! What EVERY queue subscribed to a configuration set's SNS event destination holds once a
//! send publishes its email events — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a message an SNS subscription delivers to a queue is a message that
//! queue HOLDS — one copy on every queue subscribed to the publishing topic, counted by that
//! queue's own `ApproximateNumberOfMessages` ("the approximate number of messages available
//! for retrieval from the queue",
//! `smithy_query(query="doc",shape="QueueAttributeName",member="ApproximateNumberOfMessages")`)
//! and handed over by `ReceiveMessage` as a `Message` whose `MessageId` and `ReceiptHandle`
//! are the queue's own and whose `MD5OfBody` is the MD5 digest of the `Body` it carries
//! ("An MD5 digest of the non-URL-encoded message body string",
//! `smithy_query(query="doc",shape="Message",member="MD5OfBody")`).**
//!
//! Two things the sibling SES suites do not assert:
//!
//! * the digest is the digest OF THE BODY — recomputed here over the `Body` that arrived,
//!   rather than only checked for the shape of one; and
//! * the fan-out of an EMAIL EVENT is to every subscriber — one topic with several queues on
//!   it, so a case can tell a delivery made to one subscriber from a delivery made to all of
//!   them, and each event is paired with the send it reports.
//!
//! `ses_delivery_event_advances_the_subscribed_queue` pins the depth a single subscriber
//! reports per matched event type, and `ses_send_events_reach_a_subscribed_queue` pins what
//! each event document says; the wiring helpers of both are reused here rather than copied.
//!
//! The rows differ along what the rule generalises over: how many queues are subscribed to
//! the topic, how many event types the destination matches (so how many events one send
//! produces), and how many sends are made (so that a queue's count is what arrived rather
//! than what one send produced).

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_queue_subscribed_to, sns, Grant,
};
use crate::query_wire::element;
use crate::ses_delivery_event_advances_the_subscribed_queue::{
    assert_message_members, depth, depth_reaching, published_event, received, TAKES_THE_MESSAGE,
};
use crate::ses_send_events_reach_a_subscribed_queue::{
    given_configuration_set, given_verified_sender, router, send, REGION,
};

// The MD5 digest these cases compare a delivered body against is the one SQS computes
// `MD5OfBody` with. That it really is MD5 is pinned by the owning crate, against RFC 1321's
// published vectors and through SendMessage/ReceiveMessage
// (services/service-com-amazonaws-sqs-amazonsqs/tests/all/message_lifecycle.rs,
// `md5_of_the_body_is_the_real_digest_for_every_body`), so what is asserted here is the one
// thing that suite cannot reach: that the digest a DELIVERED message reports is the digest of
// the body that arrived with it.
use service_com_amazonaws_sqs_amazonsqs::state::md5_hex;

/// A topic of this account, answering its ARN.
async fn given_topic(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    element(
        &sns(router, case, REGION, "CreateTopic", &[("Name", name)]).await,
        "TopicArn",
    )
}

/// A queue named `name`, subscribed to `topic` and granting the SNS service principal the
/// send the documented way ("you must ensure that the Amazon SQS queue policy allows the
/// Amazon SNS topic to send messages to the queue"), scoped by condition to that topic.
/// Answers the queue's url.
///
/// Called once per subscriber, so these cases run SEVERAL queues on ONE topic: that is what
/// tells a delivery made to one subscriber from a delivery made to all of them.
async fn given_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic: &str,
) -> String {
    given_queue_subscribed_to(router, case, REGION, name, topic, Grant::ScopedToTopic).await
}

/// Assert that the `MD5OfBody` of one received `message` is the digest of the `Body` that
/// arrived with it, and answer the `(eventType, mail.messageId)` the SES event inside it
/// reports.
fn digest_and_event(case: &str, message: &Value, topic: &str) -> (String, String) {
    assert_message_members(case, message);
    let body = message["Body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a received message must carry a Body, got {message}"));
    let digest = md5_hex(body.as_bytes());
    assert_eq!(
        message["MD5OfBody"].as_str(),
        Some(digest.as_str()),
        "case {case:?}: expected MD5OfBody to be the digest of the delivered body ({digest}), \
         got {message}"
    );
    let event = published_event(case, message, topic);
    (
        event["eventType"].as_str().unwrap_or_default().to_string(),
        event["mail"]["messageId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// One case of the table below: what is wired up, and what every subscriber must end up
/// holding because of it.
struct Row {
    /// What this case is, as every assertion names it.
    case: &'static str,
    /// How many queues are subscribed to the destination's topic.
    subscribers: usize,
    /// The `MatchingEventTypes` the event destination names.
    matching: &'static [&'static str],
    /// How many sends are made through the configuration set.
    sends: usize,
    /// The events one send puts on every subscribed queue, by `eventType`.
    per_send: &'static [&'static str],
}

#[tokio::test]
async fn every_queue_subscribed_to_the_destinations_topic_holds_each_published_event() {
    let rows: &[Row] = &[
        Row {
            case: "one subscriber, the delivery event alone",
            subscribers: 1,
            matching: &["DELIVERY"],
            sends: 1,
            per_send: &["Delivery"],
        },
        Row {
            case: "two subscribers, the delivery event alone",
            subscribers: 2,
            matching: &["DELIVERY"],
            sends: 1,
            per_send: &["Delivery"],
        },
        Row {
            case: "two subscribers, the acceptance and the delivery together",
            subscribers: 2,
            matching: &["SEND", "DELIVERY"],
            sends: 1,
            per_send: &["Send", "Delivery"],
        },
        Row {
            case: "three subscribers, two sends watched for the acceptance alone",
            subscribers: 3,
            matching: &["SEND"],
            sends: 2,
            per_send: &["Send"],
        },
    ];

    for Row {
        case,
        subscribers,
        matching,
        sends,
        per_send,
    } in rows
    {
        let (router, engine) = router();
        let topic = given_topic(&router, case, "ses-fan-out").await;
        let mut urls = Vec::new();
        for index in 0..*subscribers {
            urls.push(
                given_subscriber(&router, case, &format!("ses-fan-out-{index}"), &topic).await,
            );
        }
        given_verified_sender(&router, &engine, case).await;
        given_configuration_set(&router, case, &topic, true, matching).await;

        // Nothing has been sent, so every subscriber is empty — the depth a consumer polls
        // starts at zero rather than at nothing.
        for url in &urls {
            assert_eq!(
                depth(&router, case, url).await,
                "0",
                "case {case:?}: expected the queue {url} to be empty before the send"
            );
        }

        // What every subscriber must end up holding: one copy of each matched event of each
        // send, each naming the send it reports.
        let mut expected: Vec<(String, String)> = Vec::new();
        for _ in 0..*sends {
            let message_id = send(&router, case, TAKES_THE_MESSAGE).await;
            for event_type in per_send.iter() {
                expected.push((event_type.to_string(), message_id.clone()));
            }
        }
        expected.sort();
        let expected_count = expected.len();

        for url in &urls {
            let reported = depth_reaching(&router, case, url, expected_count).await;
            assert_eq!(
                reported,
                expected_count.to_string(),
                "case {case:?}: expected the queue {url} to count the {expected_count} \
                 event(s) it was sent, got {reported:?}"
            );

            let messages = received(&router, case, url, expected_count).await;
            assert_eq!(
                messages.len(),
                expected_count,
                "case {case:?}: expected {expected_count} message(s) on the queue {url}, got \
                 {messages:?}"
            );
            let mut ids: Vec<String> = messages
                .iter()
                .map(|message| message["MessageId"].as_str().unwrap_or_default().to_string())
                .collect();
            ids.sort();
            let total = ids.len();
            ids.dedup();
            assert_eq!(
                ids.len(),
                total,
                "case {case:?}: every message on the queue {url} gets its own MessageId, got \
                 {ids:?}"
            );

            let mut arrived: Vec<(String, String)> = messages
                .iter()
                .map(|message| digest_and_event(case, message, &topic))
                .collect();
            arrived.sort();
            assert_eq!(
                arrived, expected,
                "case {case:?}: expected the queue {url} to hold the events {expected:?}, each \
                 naming the send it reports, got {arrived:?}"
            );
        }
    }
}
