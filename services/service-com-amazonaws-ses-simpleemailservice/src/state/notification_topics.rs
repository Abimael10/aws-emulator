// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon SNS topic an identity's notifications are to be published to EXISTS —
//! the admission check `SetIdentityNotificationTopic` runs before it binds one.
//!
//! `SetIdentityNotificationTopicRequest$SnsTopic` is "the Amazon Resource Name (ARN) of the
//! Amazon SNS topic", and the topic is Amazon SNS's resource, so Amazon SNS is the ONE answer
//! to "is it there". A value naming no topic of this account names nothing Amazon SES could
//! ever publish a bounce, complaint or delivery notification to, so it is refused when it is
//! written rather than bound to a destination no notification can reach — the same reading the
//! destination pre-flights beside this one make
//! ([`super::event_destination_targets`], [`super::receipt_destinations`]), and the same
//! question, asked once there ([`super::receipt_destinations::topic_answer`]).
//!
//! The question is put the way the framework documents a handler reaching another emulated
//! service ([`smithy_server_runtime::OperationContext::callback`]): the AWS SDK over the
//! emulator's own in-process transport, so nothing here opens a socket or leaves the host. The
//! call made is a READ, so a refused write leaves both the identity and the topic exactly as
//! the caller left them.
//!
//! A refusal is GROUNDED in what Amazon SNS stated: only its own "no such topic" is evidence
//! the topic is missing. An answer that is not Amazon SNS stating the topic's facts — an
//! emulator with no Amazon SNS mounted beside this one, a transport that carried nothing, any
//! failure to ask — establishes nothing, so it refuses nothing.
//!
//! Nothing is cached: a topic created a moment ago resolves at once, and one that was deleted
//! stops resolving at once.

/// What a topic an identity's notifications are published to is called in a log line.
const NOTIFICATION_TOPIC: &str = "the identity's Amazon SNS notification topic";

/// The sentence Amazon SES refuses an `SnsTopic` with when the value names no Amazon SNS topic
/// it could publish to: fixed text around the value the request supplied.
///
/// `SNS topic ` and ` is invalid.` are the FIXED text and the one variable part is the
/// `SnsTopic` the request carried. It is the sentence for the whole class — a value that is no
/// topic ARN of this account at all, and one that is well formed but names no topic Amazon SNS
/// holds — because to the caller both are the same fault in the same member: the value names no
/// topic. The refusal carries it with the query API's `InvalidParameterValue` at HTTP 400.
#[must_use]
pub fn invalid_sns_topic_message(sns_topic: &str) -> String {
    format!("SNS topic {sns_topic} is invalid.")
}

/// Why the notification topic `topic_arn` is not one Amazon SES could publish to, and `None`
/// when Amazon SNS holds it — or when nothing establishes otherwise.
///
/// `region` is the Region the REQUEST is signed for, which is the Region the topic belongs to:
/// no text of a request chooses which endpoint is asked.
pub async fn unreachable_notification_topic(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    topic_arn: &str,
) -> Option<String> {
    matches!(
        super::receipt_destinations::topic_answer(ctx, region, topic_arn, NOTIFICATION_TOPIC).await,
        super::receipt_destinations::TopicAnswer::NoSuchTopic
    )
    .then(|| invalid_sns_topic_message(topic_arn))
}
