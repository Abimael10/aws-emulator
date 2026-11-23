// SPDX-License-Identifier: CC0-1.0

//! The call a subscriber whose target is an Amazon SNS TOPIC delivers with: one
//! message per accepted event, published to that topic.
//!
//! "Values are forwarded to the SNS `PublishBatch` API" (`SnsParameters`), so
//! that is the call made here, and each accepted event is one ENTRY of it — a
//! batch carries up to ten entries ("the batch request contains more entries
//! than permissible (more than 10)",
//! `com.amazonaws.sns#TooManyEntriesInBatchRequestException`), so a fan-out of
//! more events than that is published as several calls rather than one the topic
//! refuses. What the topic then does with each message is that service's own:
//! its subscriptions carry it, so what a client observes is what Amazon SNS's
//! own handlers wrote.
//!
//! The sibling of the queue call in [`super`], and held to the same two rules:
//! the role decides the call before it is made ([`super::delivery_authorization`])
//! and an event the topic did not take is recorded as a failed delivery
//! ([`super::dead_letter`]).

use super::dead_letter::Failure;
use super::{Delivery, Subscriber};

/// How many messages one `PublishBatch` call carries at most: "the batch request
/// contains more entries than permissible (more than 10)"
/// (`com.amazonaws.sns#TooManyEntriesInBatchRequestException`).
const MAX_TOPIC_BATCH_ENTRIES: usize = 10;

/// The `PublishBatch` entry one delivery to a topic travels as, or `None` for
/// a delivery the topic could take no entry for.
///
/// Each value the subscriber's `SnsParameters` carries is "a literal or a
/// JSONata expression", so it is resolved against the event the entry carries
/// ([`super::configured`]) exactly as a queue target's values are.
pub(super) fn entry(
    subscriber: &Subscriber,
    delivery: &Delivery,
    position: usize,
    budget: &mut crate::state::EvaluationBudget,
) -> Option<::aws_sdk_sns::types::PublishBatchRequestEntry> {
    let parameters = subscriber.invoke_configuration.sns_parameters.as_ref();
    let fifo = super::is_fifo_target(subscriber.target_arn());
    let mut entry = ::aws_sdk_sns::types::PublishBatchRequestEntry::builder()
        // An entry id identifies one message WITHIN its batch, and is this
        // delivery's position in the window, so what the topic reports about an
        // entry is read back onto the event it carried.
        .id(position.to_string())
        .message(delivery.body.as_str());
    if let Some(group) = super::group_of(
        subscriber,
        delivery,
        parameters.and_then(|parameters| parameters.message_group_id.as_deref()),
        fifo,
        budget,
    ) {
        entry = entry.message_group_id(group);
    }
    if let Some(token) = super::deduplication_token_of(
        subscriber,
        delivery,
        parameters.and_then(|parameters| parameters.message_deduplication_id.as_deref()),
        fifo,
        budget,
    ) {
        entry = entry.message_deduplication_id(token);
    }
    if let Some(parameters) = parameters {
        // "Subject line for email protocol subscriptions" and "per-protocol
        // message formatting mode, forwarded to SNS Publish unchanged"
        // (`SnsParameters`): both are the topic's to read, so they travel as
        // the subscriber configured them.
        if let Some(subject) =
            super::configured(subscriber, delivery, parameters.subject.as_deref(), budget)
        {
            entry = entry.subject(subject);
        }
        if let Some(structure) = super::configured(
            subscriber,
            delivery,
            parameters.message_structure.as_deref(),
            budget,
        ) {
            entry = entry.message_structure(structure);
        }
        for (name, value) in parameters.message_attributes.iter().flatten() {
            // The NAME of an attribute is a configured value too, so it is
            // resolved against the event exactly as its value is.
            let Some(name) = super::configured(subscriber, delivery, Some(name.as_str()), budget)
            else {
                continue;
            };
            if let Some(attribute) = message_attribute(subscriber, delivery, value, budget) {
                entry = entry.message_attributes(name, attribute);
            }
        }
    }
    match entry.build() {
        Ok(entry) => Some(entry),
        Err(error) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                %error,
                "an EventBridge subscriber's delivery could not be built as a topic message"
            );
            None
        }
    }
}

/// Publish the prepared entries to `topic_arn`, in batches the topic accepts,
/// and report the deliveries it did not take.
pub(super) async fn publish<'a>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    region: &str,
    topic_arn: &str,
    prepared: &'a [(Delivery, ::aws_sdk_sns::types::PublishBatchRequestEntry)],
) -> Vec<(&'a Delivery, Failure)> {
    let client = ::service_common::sns_client::sns_client(callback, region);
    let mut undelivered = Vec::new();
    for chunk in prepared.chunks(MAX_TOPIC_BATCH_ENTRIES) {
        let mut request = client.publish_batch().topic_arn(topic_arn);
        for (_, entry) in chunk {
            request = request.publish_batch_request_entries(entry.clone());
        }
        match request.send().await {
            Ok(answer) => {
                for failed in answer.failed() {
                    ::tracing::warn!(
                        subscriber = %subscriber.arn,
                        topic = %topic_arn,
                        code = %failed.code(),
                        "an EventBridge subscriber's delivery was refused by its topic target"
                    );
                    match failed
                        .id()
                        .parse::<usize>()
                        .ok()
                        .and_then(|position| prepared.get(position))
                    {
                        Some((delivery, _)) => undelivered.push((
                            delivery,
                            Failure::entry(failed.code(), failed.message(), topic_arn),
                        )),
                        None => ::tracing::warn!(
                            subscriber = %subscriber.arn,
                            topic = %topic_arn,
                            "a topic target reported a batch entry this delivery did not publish"
                        ),
                    }
                }
            }
            Err(error) => {
                ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    topic = %topic_arn,
                    error = ?error,
                    "an EventBridge subscriber's delivery did not reach its topic target"
                );
                // The whole call failed, so every event of this batch failed
                // with it — unless the answer is no refusal of the event at all
                // ([`Failure::answered`]), which is a gap in this emulator
                // rather than a delivery AWS would record as failed.
                if let Some(failure) = Failure::answered(&error, topic_arn) {
                    undelivered.extend(
                        chunk
                            .iter()
                            .map(|(delivery, _)| (delivery, failure.clone())),
                    );
                }
            }
        }
    }
    undelivered
}

/// One `MessageAttributes` entry of the subscriber's `SnsParameters` as the
/// topic takes it, or `None` for one this delivery cannot carry.
///
/// A text value is a literal or a JSONata expression resolved against the event
/// ([`super::configured`]), and travels with its declared `DataType` — 
/// "requiredness and the accepted vocabulary belong to SNS, which rejects an
/// attribute without a data type on delivery"
/// (`SnsMessageAttributeValue$DataType`) — defaulting to the type of the value it
/// carries where the subscriber declared none, since the member's own default is
/// empty text, which no attribute may be sent with. A `BinaryValue` is the
/// base64 of the bytes the attribute carries, which is the form Amazon SNS takes
/// a binary attribute's value in.
///
/// `SnsParameters` is a @sensitive shape, so no value of it reaches a log: what
/// is reported is the subscriber and what could not be carried.
fn message_attribute(
    subscriber: &Subscriber,
    delivery: &Delivery,
    value: &crate::ops::types::SnsMessageAttributeValue,
    budget: &mut crate::state::EvaluationBudget,
) -> Option<::aws_sdk_sns::types::MessageAttributeValue> {
    let mut attribute = ::aws_sdk_sns::types::MessageAttributeValue::builder();
    if let Some(encoded) = value.binary_value.as_deref() {
        let Ok(bytes) = ::aws_smithy_types::base64::decode(encoded) else {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                "an EventBridge subscriber's message attribute carries a BinaryValue that is not \
                 base64 and was not carried to its topic target"
            );
            return None;
        };
        attribute = attribute
            .data_type(declared_type(value, super::BINARY_ATTRIBUTE_TYPE))
            .binary_value(::aws_smithy_types::Blob::new(bytes));
    } else {
        let Some(text) =
            super::configured(subscriber, delivery, value.string_value.as_deref(), budget)
        else {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                "an EventBridge subscriber's message attribute carries no value and was not \
                 carried to its topic target"
            );
            return None;
        };
        attribute = attribute
            .data_type(declared_type(value, super::STRING_ATTRIBUTE_TYPE))
            .string_value(text);
    }
    match attribute.build() {
        Ok(attribute) => Some(attribute),
        Err(error) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                %error,
                "an EventBridge subscriber's message attribute could not be built"
            );
            None
        }
    }
}

/// The `DataType` an attribute travels with: the one the subscriber declared,
/// and `fallback` — the type of the value it configured — for an attribute
/// declaring none.
fn declared_type(
    value: &crate::ops::types::SnsMessageAttributeValue,
    fallback: &'static str,
) -> String {
    value
        .data_type
        .as_deref()
        .filter(|declared| !declared.is_empty())
        .unwrap_or(fallback)
        .to_string()
}
