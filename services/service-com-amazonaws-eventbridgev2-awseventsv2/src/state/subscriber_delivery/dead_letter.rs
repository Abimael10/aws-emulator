// SPDX-License-Identifier: CC0-1.0

//! What becomes of a delivery that did NOT happen: the record of it, and where
//! that record goes.
//!
//! `OnFailureConfiguration` is "where a failed delivery is sent", and a
//! subscriber carries one, so a delivery the target refused — or one the role it
//! would have been made under does not allow — leaves the one record a client
//! still has of those events: a message on the resource that member names. A
//! subscriber configuring none leaves none, which is its configuration saying so
//! rather than a failure of this module.
//!
//! The record is a DOCUMENT about the delivery that did not happen, as recorded
//! behaviour shapes it: the subscriber whose delivery failed, the target it was
//! for, the CONDITION it failed under and the sentence AWS words for that
//! condition, how many retries were spent, and the identity of each event that
//! was not delivered ([`record`]) — a POINTER at the events rather than a copy
//! of them, since the bus they were published to retains them. One record per
//! failure, so a window whose deliveries all failed the same way is one record
//! listing them all.
//!
//! The write itself is NOT held to the subscriber's role: the record exists so a
//! client can see a delivery that did not happen, and a delivery that did not
//! happen for want of a permission must not erase its own evidence for the same
//! want. WHICH call writes it follows from the kind of resource the member names
//! ([`super::Target`]), which is the same two kinds a subscriber targets.

use ::serde_json::json;

use super::{Delivery, Subscriber, Target};

/// The condition a delivery this service COULD NOT MAKE is recorded under: the
/// permissions in force on the acting identity did not allow it, or the target
/// was not there to take it.
///
/// Recorded behaviour words both as one class, because the sentence it states
/// for them ([`Failure::inaccessible`]) is about both: the target may not exist,
/// or the role may not be allowed to reach it.
pub(super) const ACCESS_DENIED: &str = "ACCESS_DENIED";

/// The condition a delivery is recorded under when the answer it failed with is
/// not one about reaching the target at all: what there is to go on is that
/// answer itself, which the record carries in full.
pub(super) const DELIVERY_FAILED: &str = "DELIVERY_FAILED";

/// How many retries a failed delivery reports having spent.
///
/// This slice attempts a delivery ONCE: a refusal of the identity is not an
/// answer a retry changes, and no other failure is retried either, so no record
/// may report attempts nothing made.
const RETRY_ATTEMPTS_SPENT: u32 = 0;

/// The wire error code an emulated service answers for an operation nobody has
/// implemented yet (`crate::scaffolded`), which is a gap in this emulator rather
/// than a refusal of the event.
const NOT_IMPLEMENTED_CODE: &str = "NotImplemented";

/// Every error code a target's service answers when it refuses the IDENTITY the
/// delivery was made as — an authorization failure, or one the key the target is
/// encrypted under decided, which each service relays under a code of its own.
///
/// Source: the models of the two services a subscriber's target lives in,
/// `smithy_query(query="operation", operation="SendMessageBatch")` of Amazon SQS
/// and `operation="PublishBatch"` of Amazon SNS, whose error shapes carry these
/// `awsQueryError` codes and shape names.
const ACCESS_DENIED_CODES: [&str; 7] = [
    // Amazon SQS and Amazon SNS both answer an unauthorized call under one of
    // these, by `awsQueryError` code or by shape name.
    "AccessDenied",
    "AccessDeniedException",
    "AuthorizationError",
    "AuthorizationErrorException",
    // The refusal each service relays from the key its messages are sealed
    // under: `KMS.AccessDeniedException` is Amazon SQS's, `KMSAccessDenied`
    // Amazon SNS's.
    "KMS.AccessDeniedException",
    "KMSAccessDenied",
    "KMSAccessDeniedException",
];

/// Every error code those same two services answer for a RESOURCE THAT IS NOT
/// THERE, from the same models.
const ABSENT_RESOURCE_CODES: [&str; 5] = [
    // Amazon SQS: `QueueDoesNotExist`, whose query code is the prefixed one.
    "AWS.SimpleQueueService.NonExistentQueue",
    "QueueDoesNotExist",
    // Amazon SNS's `NotFoundException`, by query code and by shape name.
    "NotFound",
    "NotFoundException",
    "ResourceNotFoundException",
];

/// Why ONE delivery did not happen: the condition the record reports it under,
/// and the sentence that condition is stated with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failure {
    /// One of [`ACCESS_DENIED`] and [`DELIVERY_FAILED`].
    code: &'static str,
    /// What the record says about the failure: the sentence AWS words for the
    /// condition, or the answer the target's service gave.
    message: String,
}

impl Failure {
    /// The failure of a delivery this service could not make to `target_arn`:
    /// the identity was refused, or the target was not there.
    ///
    /// The sentence is the one AWS words for that condition, whose only
    /// variable part is the target the delivery was for — read off the
    /// subscriber's own configuration.
    pub(super) fn inaccessible(target_arn: &str) -> Self {
        Self {
            code: ACCESS_DENIED,
            message: format!(
                "EventBridge could not access {target_arn}. Check that it exists and that the \
                 subscriber's delivery role is allowed to access it."
            ),
        }
    }

    /// The failure of a whole call the target's service did not take, or `None`
    /// for an answer that is no refusal of the event at all: an operation this
    /// emulator has not implemented, and a call that never reached the service,
    /// are gaps in this emulator rather than deliveries AWS would record as
    /// failed.
    pub(super) fn answered<E>(error: &E, target_arn: &str) -> Option<Self>
    where
        E: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::error::Error,
    {
        let message = ::aws_smithy_types::error::display::DisplayErrorContext(error).to_string();
        match ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(error) {
            Some(NOT_IMPLEMENTED_CODE) | None => None,
            Some(code) => Some(Self::classified(code, &message, target_arn)),
        }
    }

    /// The failure of ONE entry of a batch the target's service reported on: the
    /// code and the message that service answered for that entry, which is all
    /// there is about it.
    pub(super) fn entry(code: &str, message: Option<&str>, target_arn: &str) -> Self {
        Self::classified(code, message.unwrap_or(code), target_arn)
    }

    /// The failure an answer carrying the error code `code` is recorded as: one
    /// about REACHING the target is the class recorded behaviour words a
    /// sentence for, and every other answer is reported as that answer.
    ///
    /// The condition is this service's own classification and never the target
    /// service's error code, so a consumer of the destination keys its alarms
    /// off the same few conditions whichever service the target lives in.
    fn classified(code: &str, message: &str, target_arn: &str) -> Self {
        if ACCESS_DENIED_CODES.contains(&code) || ABSENT_RESOURCE_CODES.contains(&code) {
            return Self::inaccessible(target_arn);
        }
        Self {
            code: DELIVERY_FAILED,
            message: message.to_string(),
        }
    }
}

/// Record the deliveries of `undelivered` where this subscriber's
/// `OnFailureConfiguration` names, as the deliveries to `target_arn` that
/// failed.
pub(super) async fn park(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    target_arn: &str,
    undelivered: &[(&Delivery, Failure)],
) {
    if undelivered.is_empty() {
        return;
    }
    let configured = subscriber
        .on_failure_configuration
        .as_ref()
        .and_then(|configuration| configuration.arn.as_deref())
        .filter(|arn| !arn.is_empty());
    let Some(configured) = configured else {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            target = %target_arn,
            failed = undelivered.len(),
            "an EventBridge subscriber's failed deliveries were not recorded: it configures no \
             OnFailureConfiguration"
        );
        return;
    };
    let destination = match Target::resolve(configured, &subscriber.region) {
        Ok(destination) => destination,
        Err(reason) => {
            ::tracing::warn!(
                subscriber = %subscriber.arn,
                destination = %configured,
                "an EventBridge subscriber's failed deliveries were not recorded: {reason}"
            );
            return;
        }
    };
    for (failure, failed) in by_failure(undelivered) {
        let body = record(subscriber, target_arn, failure, &failed).to_string();
        write(callback, subscriber, &destination, &body).await;
    }
}

/// The failed deliveries of one window gathered by the failure they share, in
/// the order they failed.
///
/// A record states ONE condition and ONE message, so deliveries that failed the
/// same way travel in one record and deliveries that failed differently in one
/// record each.
fn by_failure<'a>(
    undelivered: &'a [(&'a Delivery, Failure)],
) -> Vec<(&'a Failure, Vec<&'a Delivery>)> {
    let mut gathered: Vec<(&Failure, Vec<&Delivery>)> = Vec::new();
    for (delivery, failure) in undelivered {
        match gathered.iter_mut().find(|(held, _)| *held == failure) {
            Some((_, deliveries)) => deliveries.push(delivery),
            None => gathered.push((failure, vec![delivery])),
        }
    }
    gathered
}

/// The record of the deliveries `failed` that did not happen.
///
/// Every member is read off the stored subscriber, the deliveries that were
/// attempted and the answer they failed with: the subscriber whose delivery it
/// was, the target it was for, the condition and its sentence, the retries spent
/// ([`RETRY_ATTEMPTS_SPENT`]) and the identity of each event that was not
/// delivered.
fn record(
    subscriber: &Subscriber,
    target_arn: &str,
    failure: &Failure,
    failed: &[&Delivery],
) -> ::serde_json::Value {
    json!({
        "subscriberArn": subscriber.arn,
        "targetArn": target_arn,
        "errorCode": failure.code,
        "errorMessage": failure.message,
        "retryAttempts": RETRY_ATTEMPTS_SPENT,
        "failedMessages": failed
            .iter()
            .map(|delivery| json!({ "eventId": delivery.event_id }))
            .collect::<Vec<::serde_json::Value>>(),
    })
}

/// Write ONE record to `destination`, through the one API that resource's
/// service takes a message by.
async fn write(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    subscriber: &Subscriber,
    destination: &Target,
    body: &str,
) {
    let answered = match destination {
        Target::Queue { url, .. } => super::sqs_client(callback, destination.region())
            .send_message()
            .queue_url(url)
            .message_body(body)
            .send()
            .await
            .map_err(|error| rendered(&error))
            .map(|_| ()),
        Target::Topic { arn, .. } => {
            ::service_common::sns_client::sns_client(callback, destination.region())
                .publish()
                .topic_arn(arn)
                .message(body)
                .send()
                .await
                .map_err(|error| rendered(&error))
                .map(|_| ())
        }
    };
    if let Err(answered) = answered {
        ::tracing::warn!(
            subscriber = %subscriber.arn,
            destination = %destination.arn(),
            answered = %answered,
            "an EventBridge subscriber's failed delivery could not be recorded where its \
             OnFailureConfiguration names"
        );
    }
}

/// What a destination that did not take a record answered, as a log reports it.
fn rendered<E>(error: &E) -> String
where
    E: ::std::error::Error,
{
    ::aws_smithy_types::error::display::DisplayErrorContext(error).to_string()
}

#[cfg(test)]
mod tests {
    use super::{Failure, ACCESS_DENIED, DELIVERY_FAILED};

    /// The target a failure below is about.
    const TARGET: &str = "arn:aws:sns:us-west-2:000000000000:orders";

    /// RULE: the condition a failed delivery is recorded under is this
    /// service's own classification of the answer — one about REACHING the
    /// target, which is the class AWS words one sentence for, or the answer
    /// itself — and never the target service's own error code.
    ///
    /// The rows differ along which service answered and which of the two classes
    /// its code belongs to, which is what the rule generalises over. Each code
    /// is one the model of the service a target lives in declares
    /// (`smithy_query(query="operation", operation="SendMessageBatch")` of
    /// Amazon SQS, `operation="PublishBatch"` of Amazon SNS).
    #[test]
    fn a_failures_condition_is_this_services_own_classification() {
        // (case, the code the target's service answered, the condition)
        let rows: [(&str, &str, &str); 9] = [
            (
                "the queue refused the identity",
                "AccessDenied",
                ACCESS_DENIED,
            ),
            (
                "the topic refused the identity",
                "AuthorizationError",
                ACCESS_DENIED,
            ),
            (
                "the key the queue's messages are sealed under refused",
                "KMS.AccessDeniedException",
                ACCESS_DENIED,
            ),
            (
                "the key the topic's messages are sealed under refused",
                "KMSAccessDenied",
                ACCESS_DENIED,
            ),
            (
                "the queue is not there",
                "AWS.SimpleQueueService.NonExistentQueue",
                ACCESS_DENIED,
            ),
            ("the topic is not there", "NotFound", ACCESS_DENIED),
            (
                "the batch was larger than the service takes",
                "TooManyEntriesInBatchRequest",
                DELIVERY_FAILED,
            ),
            ("the service faulted", "InternalError", DELIVERY_FAILED),
            (
                "a code neither class names",
                "InvalidParameterValue",
                DELIVERY_FAILED,
            ),
        ];
        for (case, code, expected) in rows {
            let classified = Failure::entry(code, Some("what the service answered"), TARGET);
            assert_eq!(
                classified.code, expected,
                "case {case:?}: expected {expected:?}, got {classified:?}"
            );
            if expected == ACCESS_DENIED {
                // The class AWS words one sentence for: the only variable part
                // of it is the target the delivery was for.
                assert_eq!(
                    classified.message,
                    Failure::inaccessible(TARGET).message,
                    "case {case:?}: expected the sentence of the class, got {classified:?}"
                );
                assert!(
                    classified.message.contains(TARGET),
                    "case {case:?}: the sentence must name the target, got {classified:?}"
                );
            } else {
                // Every other answer is reported AS that answer.
                assert_eq!(
                    classified.message, "what the service answered",
                    "case {case:?}: expected the answer the service gave, got {classified:?}"
                );
            }
        }
    }
}
