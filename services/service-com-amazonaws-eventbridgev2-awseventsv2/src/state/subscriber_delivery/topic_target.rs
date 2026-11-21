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

/// Publish one message per event to `topic_arn`, in batches the topic accepts,
/// and report the deliveries it did not take.
pub(super) async fn publish<'a>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    region: &str,
    topic_arn: &str,
    bodies: &'a [Delivery],
) -> Vec<(&'a Delivery, Failure)> {
    let client = ::service_common::sns_client::sns_client(callback, region);
    let parameters = subscriber.invoke_configuration.sns_parameters.as_ref();
    let mut undelivered = Vec::new();
    for (batch, chunk) in bodies.chunks(MAX_TOPIC_BATCH_ENTRIES).enumerate() {
        let offset = batch * MAX_TOPIC_BATCH_ENTRIES;
        let mut request = client.publish_batch().topic_arn(topic_arn);
        for (position, delivery) in chunk.iter().enumerate() {
            let mut entry = ::aws_sdk_sns::types::PublishBatchRequestEntry::builder()
                // An entry id identifies one message WITHIN its batch, and is
                // this delivery's position in the window, so what the topic
                // reports about an entry is read back onto the event it
                // carried.
                .id((offset + position).to_string())
                .message(delivery.body.as_str());
            if let Some(parameters) = parameters {
                // A FIFO topic takes no message without a group, so the group
                // the subscriber names travels with every delivery to it.
                if let Some(group) = parameters.message_group_id.as_deref() {
                    entry = entry.message_group_id(group);
                }
                if let Some(deduplication_id) = parameters.message_deduplication_id.as_deref() {
                    entry = entry.message_deduplication_id(deduplication_id);
                }
                // "Subject line for email protocol subscriptions" and
                // "per-protocol message formatting mode, forwarded to SNS
                // Publish unchanged" (`SnsParameters`): both are the topic's to
                // read, so they travel as the subscriber wrote them.
                if let Some(subject) = parameters.subject.as_deref() {
                    entry = entry.subject(subject);
                }
                if let Some(structure) = parameters.message_structure.as_deref() {
                    entry = entry.message_structure(structure);
                }
                for (name, value) in parameters.message_attributes.iter().flatten() {
                    if let Some(attribute) = message_attribute(subscriber, value) {
                        entry = entry.message_attributes(name, attribute);
                    }
                }
            }
            match entry.build() {
                Ok(entry) => request = request.publish_batch_request_entries(entry),
                Err(error) => ::tracing::warn!(
                    subscriber = %subscriber.arn,
                    %error,
                    "an EventBridge subscriber's delivery could not be built as a topic message"
                ),
            }
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
                        .and_then(|position| bodies.get(position))
                    {
                        Some(delivery) => undelivered.push((
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
                    undelivered.extend(chunk.iter().map(|delivery| (delivery, failure.clone())));
                }
            }
        }
    }
    undelivered
}

/// One `MessageAttributes` entry of the subscriber's `SnsParameters` as the
/// topic takes it, or `None` for one this delivery cannot carry.
///
/// A text value travels with its declared `DataType` — "requiredness and the
/// accepted vocabulary belong to SNS, which rejects an attribute without a data
/// type on delivery" (`SnsMessageAttributeValue$DataType`) — defaulting to the
/// type of the value it carries where the subscriber declared none, since the
/// member's own default is empty text, which no attribute may be sent with. A
/// `BinaryValue` is NOT carried: this slice decodes none, and an attribute whose
/// value was not carried is one the topic's subscriptions would filter on
/// wrongly.
///
/// `SnsParameters` is a @sensitive shape, so no value of it reaches a log: what
/// is reported is the subscriber and what could not be carried.
fn message_attribute(
    subscriber: &Subscriber,
    value: &crate::ops::types::SnsMessageAttributeValue,
) -> Option<::aws_sdk_sns::types::MessageAttributeValue> {
    let Some(text) = value.string_value.as_deref() else {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            "an EventBridge subscriber's message attribute carries no StringValue and was not \
             carried to its topic target"
        );
        return None;
    };
    let data_type = value
        .data_type
        .as_deref()
        .filter(|declared| !declared.is_empty())
        .unwrap_or(super::STRING_ATTRIBUTE_TYPE);
    match ::aws_sdk_sns::types::MessageAttributeValue::builder()
        .data_type(data_type)
        .string_value(text)
        .build()
    {
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
