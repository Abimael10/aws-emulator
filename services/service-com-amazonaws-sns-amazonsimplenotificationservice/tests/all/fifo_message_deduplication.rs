// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the DEDUPLICATION of a publish to a FIFO topic, driven over the
//! wire: every call below is an awsQuery HTTP request dispatched through the router the
//! emulator serves in production, so nothing but persisted state carries information
//! between calls.
//!
//! The rule under test: a message published to a FIFO topic carries a deduplication token
//! — the `MessageDeduplicationId` the caller supplied, or the SHA-256 of the body when the
//! topic enables `ContentBasedDeduplication` — and "if a message with a particular
//! MessageDeduplicationId is sent successfully, subsequent messages within the
//! deduplication scope and interval, with the same MessageDeduplicationId, are accepted
//! successfully but aren't delivered" (`PublishInput$MessageDeduplicationId` doc): the
//! duplicate is answered with the `MessageId` and `SequenceNumber` of the copy that WAS
//! delivered, and nothing is published for it. The scope is the whole topic, or one
//! message group when the topic's `FifoThroughputScope` says so.
//!
//! The rows differ along what the rule generalises over: how the token arises (supplied or
//! derived), the deduplication scope the topic sets, whether two copies share a message
//! group, which operation published each copy (`Publish`, `PublishBatch`) and whether the
//! copies are in one request or in two.
//!
//! The last suites assert the other side of the rule: a publish that leaves the topic with
//! NO token — no `MessageDeduplicationId` and no content-based deduplication to derive one
//! from — is refused with the sentence SNS states for that condition, on the single as on
//! the batch path, and each of the two ways out that sentence names makes the same call
//! succeed.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

use super::topics_subscriptions_publish::{Answer, Sns};

/// One publish of one message: its body, its message group, and the deduplication token
/// the caller supplied (`None` leaves the topic to derive one).
type Message<'a> = (&'a str, &'a str, Option<&'a str>);

/// One row of the deduplication table: the case, the attributes its FIFO topic is created
/// with, the two publishes, and whether the second is collapsed onto the first.
type DedupCase<'a> = (
    &'a str,
    &'a [(&'a str, &'a str)],
    Message<'a>,
    Message<'a>,
    bool,
);

/// The deduplication token a content-based FIFO topic derives for `body`: "Amazon SNS
/// uses a SHA-256 hash to generate the MessageDeduplicationId using the body of the
/// message (but not the attributes of the message)"
/// (`GetTopicAttributesResponse$Attributes` doc).
fn derived_token(body: &str) -> String {
    hex::encode(Sha256::digest(body.as_bytes()))
}

/// Create a FIFO topic named `name` with `attributes` beside its FIFO flag, answering its
/// ARN.
async fn fifo_topic(sns: &Sns, name: &str, attributes: &[(&str, &str)]) -> String {
    let fifo_name = format!("{name}.fifo");
    let mut params: Vec<(String, String)> = vec![
        ("Name".to_string(), fifo_name),
        ("Attributes.entry.1.key".to_string(), "FifoTopic".to_string()),
        ("Attributes.entry.1.value".to_string(), "true".to_string()),
    ];
    for (index, (key, value)) in attributes.iter().enumerate() {
        params.push((
            format!("Attributes.entry.{}.key", index + 2),
            (*key).to_string(),
        ));
        params.push((
            format!("Attributes.entry.{}.value", index + 2),
            (*value).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let answer = sns.call("CreateTopic", &borrowed).await;
    assert_eq!(
        answer.status, 200,
        "CreateTopic {name}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    answer
        .element("TopicArn")
        .unwrap_or_else(|| panic!("CreateTopic {name}: no TopicArn in {}", answer.body))
}

/// Subscribe a queue endpoint to `topic`, answering the subscription's ARN: the delivery
/// log of this subscription is where a message that was DELIVERED is observed.
async fn subscribe(sns: &Sns, topic: &str, queue: &str) -> String {
    let endpoint = format!("arn:aws:sqs:us-east-1:000000000000:{queue}");
    let answer = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", topic),
                ("Protocol", "sqs"),
                ("Endpoint", &endpoint),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "Subscribe {queue}: expected 200, got {} / {}",
        answer.status, answer.body
    );
    answer
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("Subscribe {queue}: no SubscriptionArn in {}", answer.body))
}

/// The `(MessageId, SequenceNumber)` one accepted publish answered with.
async fn publish(sns: &Sns, case: &str, topic: &str, message: Message<'_>) -> (String, String) {
    let (body, group, token) = message;
    let mut params: Vec<(&str, &str)> = vec![
        ("TopicArn", topic),
        ("Message", body),
        ("MessageGroupId", group),
    ];
    if let Some(token) = token {
        params.push(("MessageDeduplicationId", token));
    }
    let answer = sns.call("Publish", &params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Publish expected 200, got {} / {}",
        answer.status, answer.body
    );
    (
        answer
            .element("MessageId")
            .unwrap_or_else(|| panic!("case {case:?}: no MessageId in {}", answer.body)),
        answer
            .element("SequenceNumber")
            .unwrap_or_else(|| panic!("case {case:?}: no SequenceNumber in {}", answer.body)),
    )
}

/// How many messages the delivery log holds for one subscription.
fn delivered(sns: &Sns, subscription: &str) -> usize {
    let stored = sns.state();
    let stored = stored.lock();
    stored
        .delivered
        .get(subscription)
        .map(Vec::len)
        .unwrap_or_default()
}

/// A second copy of a message already accepted inside the deduplication scope is accepted
/// too, answers with the FIRST copy's `MessageId` and `SequenceNumber`, and is not
/// delivered; a message that is NOT a duplicate inside that scope is published on its own.
#[tokio::test]
async fn a_duplicate_fifo_publish_answers_the_first_copy_and_delivers_nothing() {
    let sns = Sns::new();
    let hashed_body = derived_token("a body published earlier");
    // The attribute names and values come from the `CreateTopicInput$Attributes` catalog:
    // `ContentBasedDeduplication` ("Enables content-based deduplication for FIFO topics")
    // and `FifoThroughputScope` ("Topic … the default value" / "MessageGroup – The scope
    // of deduplication is within each individual message group").
    let cases: &[DedupCase<'_>] = &[
        (
            "the same supplied token in one message group",
            &[],
            ("first", "g1", Some("d1")),
            ("second", "g1", Some("d1")),
            true,
        ),
        (
            "the same supplied token in another message group, under the default scope",
            &[],
            ("first", "g1", Some("d1")),
            ("second", "g2", Some("d1")),
            true,
        ),
        (
            "the same supplied token in another message group, scoped to the group",
            &[("FifoThroughputScope", "MessageGroup")],
            ("first", "g1", Some("d1")),
            ("second", "g2", Some("d1")),
            false,
        ),
        (
            "the same supplied token in one message group, scoped to the group",
            &[("FifoThroughputScope", "MessageGroup")],
            ("first", "g1", Some("d1")),
            ("second", "g1", Some("d1")),
            true,
        ),
        (
            "the same supplied token under the topic scope stated explicitly",
            &[("FifoThroughputScope", "Topic")],
            ("first", "g1", Some("d1")),
            ("second", "g2", Some("d1")),
            true,
        ),
        (
            "two different supplied tokens",
            &[],
            ("one body", "g1", Some("d1")),
            ("one body", "g1", Some("d2")),
            false,
        ),
        (
            "the same body on a content based topic",
            &[("ContentBasedDeduplication", "true")],
            ("the same body", "g1", None),
            ("the same body", "g1", None),
            true,
        ),
        (
            "the same body in another group on a content based topic",
            &[("ContentBasedDeduplication", "true")],
            ("the same body", "g1", None),
            ("the same body", "g2", None),
            true,
        ),
        (
            "two different bodies on a content based topic",
            &[("ContentBasedDeduplication", "true")],
            ("one body", "g1", None),
            ("another body", "g1", None),
            false,
        ),
        // "If you send one message with ContentBasedDeduplication enabled, and then
        // another message with a MessageDeduplicationId that is the same as the one
        // generated for the first MessageDeduplicationId, the two messages are treated as
        // duplicates" (`PublishInput$MessageDeduplicationId` doc).
        (
            "a supplied token equal to the token derived for an earlier body",
            &[("ContentBasedDeduplication", "true")],
            ("a body published earlier", "g1", None),
            ("a different body entirely", "g1", Some(&hashed_body)),
            true,
        ),
        (
            "a supplied token overriding the derived one",
            &[("ContentBasedDeduplication", "true")],
            ("the same body", "g1", None),
            ("the same body", "g1", Some("explicit")),
            false,
        ),
    ];

    for (index, (case, attributes, first, second, collapsed)) in cases.iter().enumerate() {
        let topic = fifo_topic(&sns, &format!("dedup-{index:02}"), attributes).await;
        let subscription = subscribe(&sns, &topic, &format!("dedup-queue-{index:02}")).await;

        let (first_id, first_sequence) = publish(&sns, case, &topic, *first).await;
        let (second_id, second_sequence) = publish(&sns, case, &topic, *second).await;

        if *collapsed {
            assert_eq!(
                second_id, first_id,
                "case {case:?}: a duplicate must answer with the first copy's MessageId, \
                 got {second_id:?} after {first_id:?}"
            );
            assert_eq!(
                second_sequence, first_sequence,
                "case {case:?}: a duplicate must answer with the first copy's \
                 SequenceNumber, got {second_sequence:?} after {first_sequence:?}"
            );
            assert_eq!(
                delivered(&sns, &subscription),
                1,
                "case {case:?}: a duplicate must not be delivered"
            );
        } else {
            assert_ne!(
                second_id, first_id,
                "case {case:?}: a message that is no duplicate must carry a fresh \
                 MessageId, got {second_id:?} twice"
            );
            assert_ne!(
                second_sequence, first_sequence,
                "case {case:?}: a message that is no duplicate must be sequenced on its \
                 own, got {second_sequence:?} twice"
            );
            assert_eq!(
                delivered(&sns, &subscription),
                2,
                "case {case:?}: both messages must be delivered"
            );
        }
    }
}

/// The deduplication scope is ONE topic: the same token on another topic names another
/// message. A STANDARD topic deduplicates nothing, so two identical publishes to one are
/// two messages.
#[tokio::test]
async fn deduplication_is_scoped_to_the_topic_and_applies_to_fifo_topics_alone() {
    let sns = Sns::new();
    let here = fifo_topic(&sns, "dedup-here", &[]).await;
    let there = fifo_topic(&sns, "dedup-there", &[]).await;
    let message: Message<'_> = ("one body", "g1", Some("shared-token"));

    let (first, _) = publish(&sns, "the first topic", &here, message).await;
    let (other, _) = publish(&sns, "another topic", &there, message).await;
    assert_ne!(
        other, first,
        "the same token on another topic names another message, got {other:?} twice"
    );
    let (again, _) = publish(&sns, "the first topic again", &here, message).await;
    assert_eq!(
        again, first,
        "the token is still held by the topic that accepted it, got {again:?} after \
         {first:?}"
    );

    // A standard topic takes no deduplication token at all, and sequences nothing: two
    // identical publishes are two messages.
    let standard = sns.topic("dedup-standard").await;
    let subscription = subscribe(&sns, &standard, "dedup-standard-queue").await;
    let mut ids: HashSet<String> = HashSet::new();
    for attempt in 0..2 {
        let answer = sns
            .call(
                "Publish",
                &[("TopicArn", &standard), ("Message", "one body")],
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "the standard publish {attempt} expected 200, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.element("SequenceNumber"),
            None,
            "a standard topic sequences nothing, got {}",
            answer.body
        );
        ids.insert(answer.element("MessageId").unwrap_or_default());
    }
    assert_eq!(
        ids.len(),
        2,
        "a standard topic must mint a MessageId per publish, got {ids:?}"
    );
    assert_eq!(
        delivered(&sns, &subscription),
        2,
        "a standard topic must deliver every publish"
    );
}

/// `PublishBatch` deduplicates against the same records: "messages are deduplicated
/// within the batch and across batches for five minutes" (the `PublishBatch` doc), and
/// against what a single `Publish` accepted — so a duplicate entry is reported with the
/// message that WAS delivered, whichever operation delivered it.
#[tokio::test]
async fn a_batched_publish_deduplicates_within_the_batch_and_against_publish() {
    let sns = Sns::new();
    let topic = fifo_topic(&sns, "dedup-batch", &[]).await;
    let subscription = subscribe(&sns, &topic, "dedup-batch-queue").await;

    // Two entries of ONE batch carrying one token: both are reported, on one message.
    let answer = sns
        .call(
            "PublishBatch",
            &[
                ("TopicArn", &topic),
                ("PublishBatchRequestEntries.member.1.Id", "e1"),
                ("PublishBatchRequestEntries.member.1.Message", "first"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
                    "batched",
                ),
                ("PublishBatchRequestEntries.member.2.Id", "e2"),
                ("PublishBatchRequestEntries.member.2.Message", "second"),
                ("PublishBatchRequestEntries.member.2.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.2.MessageDeduplicationId",
                    "batched",
                ),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "PublishBatch expected 200, got {} / {}",
        answer.status, answer.body
    );
    let ids = answer.elements("MessageId");
    assert_eq!(
        ids.len(),
        2,
        "every entry must be reported, got {}",
        answer.body
    );
    assert_eq!(
        ids[1], ids[0],
        "a duplicate entry must report the message the batch published, got {ids:?}"
    );
    let sequences = answer.elements("SequenceNumber");
    assert_eq!(
        sequences[1], sequences[0],
        "a duplicate entry must report that message's SequenceNumber, got {sequences:?}"
    );
    assert_eq!(
        delivered(&sns, &subscription),
        1,
        "only the message the batch published may be delivered"
    );

    // The same token in a LATER batch, and in a single `Publish`: one message still.
    let again = sns
        .call(
            "PublishBatch",
            &[
                ("TopicArn", &topic),
                ("PublishBatchRequestEntries.member.1.Id", "e3"),
                ("PublishBatchRequestEntries.member.1.Message", "third"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
                    "batched",
                ),
            ],
        )
        .await;
    assert_eq!(
        again.element("MessageId").as_deref(),
        Some(ids[0].as_str()),
        "a later batch carrying the same token must report the same message, got {}",
        again.body
    );
    let (published, _) = publish(&sns, "a single publish of a batched token", &topic, ("fourth", "g1", Some("batched"))).await;
    assert_eq!(
        published, ids[0],
        "a Publish carrying a token a batch accepted must report that message, got \
         {published:?} after {:?}",
        ids[0]
    );
    assert_eq!(
        delivered(&sns, &subscription),
        1,
        "no duplicate may be delivered, whichever operation carried it"
    );

    // A token the topic has not seen is a new message, in a batch as in a publish.
    let fresh = sns
        .call(
            "PublishBatch",
            &[
                ("TopicArn", &topic),
                ("PublishBatchRequestEntries.member.1.Id", "e4"),
                ("PublishBatchRequestEntries.member.1.Message", "fifth"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
                    "unseen",
                ),
            ],
        )
        .await;
    assert_ne!(
        fresh.element("MessageId").as_deref(),
        Some(ids[0].as_str()),
        "an unseen token must be a message of its own, got {}",
        fresh.body
    );
    assert_eq!(
        delivered(&sns, &subscription),
        2,
        "the message of an unseen token must be delivered"
    );
}

/// The sentence SNS refuses a publish to a FIFO topic that leaves it with NO deduplication
/// token with: the topic derives one only with `ContentBasedDeduplication` enabled, so the
/// refusal states the caller's two ways out — "If you create a FIFO topic and this
/// attribute is false, you must specify a value for the MessageDeduplicationId parameter
/// for the Publish action" (`GetTopicAttributesResponse$Attributes` doc). It is fixed text
/// for this class: it names neither the topic, the entry, nor any value of the request.
const NO_DEDUPLICATION_TOKEN_REFUSAL: &str =
    "Invalid parameter: The topic should either have ContentBasedDeduplication enabled or \
     MessageDeduplicationId provided explicitly";

/// One refused publishing call: the case, the attributes its FIFO topic is created with
/// beside the FIFO flag, the action, and the form parameters it carries beside the topic.
type RefusedCase<'a> = (
    &'a str,
    &'a [(&'a str, &'a str)],
    &'a str,
    &'a [(&'a str, &'a str)],
);

/// Call `action` with `params` beside `topic`.
async fn publishing(sns: &Sns, topic: &str, action: &str, params: &[(&str, &str)]) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("TopicArn", topic)];
    all.extend_from_slice(params);
    sns.call(action, &all).await
}

/// A FIFO publish that leaves the topic with no deduplication token at all is refused with
/// the service's own sentence for that condition, whichever operation carried it — and a
/// batch holding one such entry is refused WHOLE, so nothing of it is published.
#[tokio::test]
async fn a_fifo_publish_with_no_deduplication_token_is_refused_with_the_services_sentence() {
    let sns = Sns::new();

    // The rows differ along what the rule generalises over: the operation, the setting the
    // topic was created with (left at its documented default or written as it), the
    // deduplication scope, how many entries the batch holds and WHICH of them carries no
    // id. The attribute names come from the `CreateTopicInput$Attributes` catalog.
    let rows: &[RefusedCase<'_>] = &[
        (
            "a publish with the setting left at its default",
            &[],
            "Publish",
            &[("Message", "a body"), ("MessageGroupId", "g1")],
        ),
        (
            "a publish to a topic whose setting is written as the default",
            &[("ContentBasedDeduplication", "false")],
            "Publish",
            &[
                ("Message", "another body entirely"),
                ("MessageGroupId", "another-group"),
            ],
        ),
        (
            "a publish to a topic deduplicating within each message group",
            &[("FifoThroughputScope", "MessageGroup")],
            "Publish",
            &[("Message", "a third body"), ("MessageGroupId", "g9")],
        ),
        (
            "a batch of one entry carrying no id",
            &[],
            "PublishBatch",
            &[
                ("PublishBatchRequestEntries.member.1.Id", "only"),
                ("PublishBatchRequestEntries.member.1.Message", "a body"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
            ],
        ),
        (
            "a batch whose LAST entry carries no id",
            &[],
            "PublishBatch",
            &[
                ("PublishBatchRequestEntries.member.1.Id", "first"),
                ("PublishBatchRequestEntries.member.1.Message", "first body"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
                    "d1",
                ),
                ("PublishBatchRequestEntries.member.2.Id", "second"),
                ("PublishBatchRequestEntries.member.2.Message", "second body"),
                ("PublishBatchRequestEntries.member.2.MessageGroupId", "g1"),
            ],
        ),
        (
            "a batch whose FIRST entry carries no id",
            &[],
            "PublishBatch",
            &[
                ("PublishBatchRequestEntries.member.1.Id", "first"),
                ("PublishBatchRequestEntries.member.1.Message", "first body"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                ("PublishBatchRequestEntries.member.2.Id", "second"),
                ("PublishBatchRequestEntries.member.2.Message", "second body"),
                ("PublishBatchRequestEntries.member.2.MessageGroupId", "g2"),
                (
                    "PublishBatchRequestEntries.member.2.MessageDeduplicationId",
                    "d2",
                ),
            ],
        ),
        (
            "a batch whose MIDDLE entry carries no id",
            &[("ContentBasedDeduplication", "false")],
            "PublishBatch",
            &[
                ("PublishBatchRequestEntries.member.1.Id", "first"),
                ("PublishBatchRequestEntries.member.1.Message", "first body"),
                ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
                    "d1",
                ),
                ("PublishBatchRequestEntries.member.2.Id", "second"),
                ("PublishBatchRequestEntries.member.2.Message", "second body"),
                ("PublishBatchRequestEntries.member.2.MessageGroupId", "g1"),
                ("PublishBatchRequestEntries.member.3.Id", "third"),
                ("PublishBatchRequestEntries.member.3.Message", "third body"),
                ("PublishBatchRequestEntries.member.3.MessageGroupId", "g1"),
                (
                    "PublishBatchRequestEntries.member.3.MessageDeduplicationId",
                    "d3",
                ),
            ],
        ),
    ];

    for (index, (case, attributes, action, params)) in rows.iter().enumerate() {
        let topic = fifo_topic(&sns, &format!("no-token-{index:02}"), attributes).await;
        let subscription = subscribe(&sns, &topic, &format!("no-token-queue-{index:02}")).await;

        let answer = publishing(&sns, &topic, action, params).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        let message = answer.element("Message");
        assert_eq!(
            message.as_deref(),
            Some(NO_DEDUPLICATION_TOKEN_REFUSAL),
            "case {case:?}: expected the message {NO_DEDUPLICATION_TOKEN_REFUSAL:?}, got \
             {message:?}"
        );
        // Nothing of the request was published: a batch holding one entry the rules refuse
        // is refused whole, so no entry is reported and no endpoint is delivered to.
        let ids = answer.elements("MessageId");
        assert!(
            ids.is_empty(),
            "case {case:?}: a refused publishing call reports no message, got {ids:?}"
        );
        let delivered_count = delivered(&sns, &subscription);
        assert_eq!(
            delivered_count, 0,
            "case {case:?}: a refused publishing call delivers nothing, got \
             {delivered_count} deliveries"
        );
    }
}

/// The refusal states TWO ways out, and each one works: supply the id, or enable the
/// setting that derives one — on the single and on the batch path.
#[tokio::test]
async fn either_way_out_the_refusal_names_makes_the_same_publish_succeed() {
    let sns = Sns::new();
    let topic = fifo_topic(&sns, "way-out", &[]).await;
    let subscription = subscribe(&sns, &topic, "way-out-queue").await;
    let batch: &[(&str, &str)] = &[
        ("PublishBatchRequestEntries.member.1.Id", "first"),
        ("PublishBatchRequestEntries.member.1.Message", "first body"),
        ("PublishBatchRequestEntries.member.1.MessageGroupId", "g1"),
        (
            "PublishBatchRequestEntries.member.1.MessageDeduplicationId",
            "supplied",
        ),
        ("PublishBatchRequestEntries.member.2.Id", "second"),
        ("PublishBatchRequestEntries.member.2.Message", "second body"),
        ("PublishBatchRequestEntries.member.2.MessageGroupId", "g1"),
    ];

    // Neither way out taken: both paths refuse, with the one sentence.
    for (case, action, params) in [
        (
            "the single path before either way out",
            "Publish",
            &[("Message", "a body"), ("MessageGroupId", "g1")][..],
        ),
        ("the batch path before either way out", "PublishBatch", batch),
    ] {
        let answer = publishing(&sns, &topic, action, params).await;
        let message = answer.element("Message");
        assert_eq!(
            message.as_deref(),
            Some(NO_DEDUPLICATION_TOKEN_REFUSAL),
            "case {case:?}: expected the message {NO_DEDUPLICATION_TOKEN_REFUSAL:?}, got \
             {message:?}"
        );
    }

    // The first way out: the publish supplies the id itself, with the setting untouched.
    let supplied = publishing(
        &sns,
        &topic,
        "Publish",
        &[
            ("Message", "a body"),
            ("MessageGroupId", "g1"),
            ("MessageDeduplicationId", "explicit"),
        ],
    )
    .await;
    assert_eq!(
        supplied.status, 200,
        "a publish supplying the id must be accepted, got {} / {}",
        supplied.status, supplied.body
    );

    // The second way out: the topic enables the setting, and the SAME calls are accepted.
    let written = sns
        .call(
            "SetTopicAttributes",
            &[
                ("TopicArn", topic.as_str()),
                ("AttributeName", "ContentBasedDeduplication"),
                ("AttributeValue", "true"),
            ],
        )
        .await;
    assert_eq!(
        written.status, 200,
        "SetTopicAttributes expected 200, got {} / {}",
        written.status, written.body
    );
    let accepted = publishing(
        &sns,
        &topic,
        "Publish",
        &[("Message", "a body"), ("MessageGroupId", "g1")],
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "a publish to a content-based topic needs no id of its own, got {} / {}",
        accepted.status, accepted.body
    );
    let batched = publishing(&sns, &topic, "PublishBatch", batch).await;
    assert_eq!(
        batched.status, 200,
        "a batch whose entries the content-based topic derives ids for must be accepted, \
         got {} / {}",
        batched.status, batched.body
    );
    let reported = batched.elements("MessageId");
    assert_eq!(
        reported.len(),
        2,
        "every entry of the accepted batch must be reported, got {}",
        batched.body
    );
    // Four distinct messages were published: the one carrying the supplied id, the one the
    // topic derived an id for, and the batch's two entries.
    let fanned_out = delivered(&sns, &subscription);
    assert_eq!(
        fanned_out, 4,
        "every accepted publish must be delivered, got {fanned_out} deliveries"
    );
}
