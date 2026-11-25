// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHAT a subscription-addressed operation SAYS when the
//! `SubscriptionArn` it was handed resolves to no subscription.
//!
//! RULE: an operation whose input names a subscription by ARN and finds no record under it
//! answers the modelled lookup miss — HTTP 404 under the `NotFound` code, as
//! `NotFoundException`'s `awsQueryError` declares it
//! (`smithy_query(query="operation",operation="GetSubscriptionAttributes")`) — with the one
//! fixed sentence `Subscription does not exist`: the resource KIND the ARN was read as, and
//! nothing of the request, so neither the ARN, the topic it names nor the subscription id
//! appears in it. The sentence is about the lookup, not about the call: the idempotent
//! `Unsubscribe` finds the same absence and answers success, and the deletion it already
//! accounted for is not accounted for twice.
//!
//! The rows below span the ways an ARN can name no subscription rather than one of them —
//! a subscription removed by `Unsubscribe`, one whose topic was deleted under it, an id no
//! subscription of a live topic carries, a live id under another topic's name, and an ARN
//! whose topic does not exist — against each operation that resolves a subscription by ARN.
//!
//! Every call goes over the wire through the router the emulator serves in production, and
//! nothing here waits out a span: the deletion the counter assertion is about is a change
//! the CLOCK owes, so the instant the topic holds it at is moved into the past through the
//! harness's own seam rather than waited for — which is what the clock would have done, and
//! says nothing about how fast this host runs.

use super::topics_subscriptions_publish::{attribute_map, Answer, Sns};

/// The code SNS names a lookup miss by, as the modelled `NotFoundException`'s
/// `awsQueryError` declares it.
const NOT_FOUND: &str = "NotFound";

/// The sentence SNS refuses a `SubscriptionArn` that resolves to no subscription with.
/// Entirely fixed text: the resource kind, and nothing of the request.
const SUBSCRIPTION_DOES_NOT_EXIST: &str = "Subscription does not exist";

/// Subscribe an `sqs` endpoint on `topic` and hand back the subscription's ARN. SNS
/// confirms an `sqs` subscription itself, so the ARN is a real one from the first call.
async fn subscribe(sns: &Sns, case: &str, topic: &str, queue: &str) -> String {
    let answer = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", topic),
                ("Protocol", "sqs"),
                (
                    "Endpoint",
                    &format!("arn:aws:sqs:us-east-1:000000000000:{queue}"),
                ),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Subscribe expected 200, got {} / {}",
        answer.status, answer.body
    );
    answer
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("case {case:?}: no SubscriptionArn in {}", answer.body))
}

/// Read a subscription by ARN.
async fn get_attributes(sns: &Sns, arn: &str) -> Answer {
    sns.call("GetSubscriptionAttributes", &[("SubscriptionArn", arn)])
        .await
}

/// Write one attribute of a subscription by ARN.
async fn set_attribute(sns: &Sns, arn: &str) -> Answer {
    sns.call(
        "SetSubscriptionAttributes",
        &[
            ("SubscriptionArn", arn),
            ("AttributeName", "RawMessageDelivery"),
            ("AttributeValue", "true"),
        ],
    )
    .await
}

/// Assert `answer` is the lookup miss, said the way SNS says it, and that the sentence
/// carries no part of the request that provoked it.
fn assert_lookup_miss(case: &str, arn: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected 404, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.code(),
        NOT_FOUND,
        "case {case:?}: expected the {NOT_FOUND} code, got {}",
        answer.body
    );
    let message = answer.element("Message").unwrap_or_default();
    assert_eq!(
        message, SUBSCRIPTION_DOES_NOT_EXIST,
        "case {case:?}: expected {SUBSCRIPTION_DOES_NOT_EXIST:?}, got {message:?}"
    );
    // Fixed text: the sentence is the same whichever ARN was asked about, so no part of
    // the value — nor of the topic or id inside it — can be in it.
    for part in arn.split(':').filter(|part| !part.is_empty()) {
        assert!(
            !message.contains(part),
            "case {case:?}: the sentence must carry no part of the request, {part:?} is in \
             {message:?}"
        );
    }
}

/// `(case, the ARN that names no subscription)` for a fresh emulator: every way the
/// lookup can miss, built from live resources where that is what makes it miss.
async fn absent_arns(sns: &Sns) -> Vec<(String, String)> {
    let topic = sns.topic("lookup-miss-topic").await;
    let other = sns.topic("lookup-miss-other-topic").await;

    let unsubscribed = subscribe(sns, "unsubscribed", &topic, "lookup-miss-gone").await;
    let answer = sns
        .call("Unsubscribe", &[("SubscriptionArn", &unsubscribed)])
        .await;
    assert_eq!(
        answer.status, 200,
        "Unsubscribe must succeed, got {} / {}",
        answer.status, answer.body
    );

    let doomed_topic = sns.topic("lookup-miss-doomed-topic").await;
    let doomed = subscribe(
        sns,
        "the topic was deleted under it",
        &doomed_topic,
        "lookup-miss-doomed",
    )
    .await;
    let answer = sns
        .call("DeleteTopic", &[("TopicArn", &doomed_topic)])
        .await;
    assert_eq!(
        answer.status, 200,
        "DeleteTopic must succeed, got {} / {}",
        answer.status, answer.body
    );

    // A subscription that IS there, whose id is borrowed below to build an ARN under
    // another topic's name: the id is real, the pairing is not.
    let live = subscribe(sns, "live", &topic, "lookup-miss-live").await;
    let live_id = live
        .rsplit(':')
        .next()
        .expect("a subscription ARN ends in its id")
        .to_string();

    vec![
        ("a subscription that was unsubscribed".to_string(), unsubscribed),
        ("a subscription whose topic was deleted".to_string(), doomed),
        (
            "an id no subscription of a live topic carries".to_string(),
            format!("{topic}:00000000-0000-4000-8000-000000000000"),
        ),
        (
            "a live subscription id under another topic's name".to_string(),
            format!("{other}:{live_id}"),
        ),
        (
            "an ARN whose topic does not exist".to_string(),
            "arn:aws:sns:us-east-1:000000000000:lookup-miss-absent-topic:\
             11111111-1111-4111-8111-111111111111"
                .to_string(),
        ),
    ]
}

/// Every operation that resolves a subscription by ARN answers the SAME fixed sentence
/// for every way the lookup can miss.
#[tokio::test]
async fn a_subscription_arn_that_names_nothing_is_the_fixed_lookup_miss_sentence() {
    let sns = Sns::new();
    for (case, arn) in absent_arns(&sns).await {
        let read = format!("GetSubscriptionAttributes on {case}");
        assert_lookup_miss(&read, &arn, &get_attributes(&sns, &arn).await);

        let write = format!("SetSubscriptionAttributes on {case}");
        assert_lookup_miss(&write, &arn, &set_attribute(&sns, &arn).await);
    }
}

/// The sentence belongs to the LOOKUP, not to any call naming an absent subscription: the
/// idempotent `Unsubscribe` finds the same absence and answers success, and the topic
/// counts the one deletion it performed once however many times it is called.
#[tokio::test]
async fn a_repeated_unsubscribe_succeeds_and_the_deletion_is_counted_once() {
    let case = "a subscription unsubscribed three times";
    let sns = Sns::new();
    let topic = sns.topic("lookup-miss-idempotent").await;
    let arn = subscribe(&sns, case, &topic, "lookup-miss-idempotent").await;

    for attempt in 1..=3 {
        let answer = sns.call("Unsubscribe", &[("SubscriptionArn", &arn)]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: Unsubscribe #{attempt} expected 200, got {} / {}",
            answer.status, answer.body
        );
        assert!(
            !answer.body.contains(SUBSCRIPTION_DOES_NOT_EXIST),
            "case {case:?}: Unsubscribe #{attempt} must not refuse an absent subscription, \
             got {}",
            answer.body
        );
    }

    // The read of the removed subscription still says the subscription is not there.
    assert_lookup_miss(case, &arn, &get_attributes(&sns, &arn).await);

    // And the deletion the FIRST call performed is accounted for exactly once: the
    // counter follows the subscriptions actually removed, not the calls made. It stands
    // until the clock makes the deletion due …
    let deleted = |answer: &Answer| -> String {
        attribute_map(answer)
            .get("SubscriptionsDeleted")
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a SubscriptionsDeleted counter, got {}",
                    answer.body
                )
            })
    };
    let before = sns.call("GetTopicAttributes", &[("TopicArn", &topic)]).await;
    assert_eq!(
        deleted(&before),
        "0",
        "case {case:?}: the deleted count stands until the clock makes the deletion due, got \
         {}",
        before.body
    );

    // … and reports the ONE deletion the three calls performed between them once it is.
    sns.make_the_deletions_due(case, &topic);
    let after = sns.call("GetTopicAttributes", &[("TopicArn", &topic)]).await;
    assert_eq!(
        deleted(&after),
        "1",
        "case {case:?}: one removed subscription is one deletion, got {}",
        after.body
    );
}
