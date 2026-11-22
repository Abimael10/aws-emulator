// SPDX-License-Identifier: CC0-1.0

//! The TEST message a lifecycle hook's notification target is PROVED by, and the refusal of
//! a hook whose role cannot publish to that target.
//!
//! # The rule this module implements
//!
//! A lifecycle hook's notification target is not a value the service merely stores. "When
//! you specify a notification target, Amazon EC2 Auto Scaling sends it a test message. Test
//! messages contain the following additional key-value pair: "Event":
//! "autoscaling:TEST_NOTIFICATION""
//! (`smithy_query(query="doc",shape="PutLifecycleHookType",member="NotificationTargetARN")`),
//! and the role beside it is "the ARN of the IAM role that allows the Auto Scaling group to
//! publish to the specified notification target" (same shape, `RoleARN`). So a call that
//! names either half of that pair PUBLISHES the message to the target the hook would stand
//! with, under the role it would stand with, and the publish is part of the call rather than
//! something the next capacity change gets to:
//!
//! * a role the account's own IAM answers may NOT perform the publish on that target makes
//!   the call fail — synchronously, with `ValidationError` and
//!   [`unable_to_publish_message`] — and nothing is written: the hook is not created, and an
//!   existing one keeps every member it carried;
//! * a role that may publish makes the call succeed AND the target receive one
//!   `autoscaling:TEST_NOTIFICATION` for the group, which is the only thing that proves the
//!   pair is usable before an instance is ever held by the hook.
//!
//! The rule is the one the model states for the door that PUTS a hook: it is
//! `PutLifecycleHookType$NotificationTargetARN` that carries the sentence about the test
//! message, while the `LifecycleHookSpecification$NotificationTargetARN` a group's create
//! carries states only what the target is for — so the create stores its specifications
//! without proving them, and this module is asked by `PutLifecycleHook`.
//!
//! # Only what IAM ANSWERED is withheld
//!
//! The decision is the one question a service asks before it ACTS UNDER a caller-supplied
//! role, put to IAM itself through the tree's single route to it
//! ([`::service_common::iam_role::role_withholds_action`]): the documents in force — the
//! inline ones, the attached ones and the ceiling a permissions boundary sets — are collected
//! and decided by the service that HOLDS them, so this crate grows no second evaluation of an
//! IAM document.
//!
//! It withholds NOTHING for anything that is not an answer about that role's permissions — an
//! emulator with no IAM mounted, a value naming no role of this account, a role this
//! account's IAM holds no record of (these roles are routinely provisioned out of band), or
//! an answer this emulator could not read — exactly as every other reader of a
//! caller-supplied role in this tree reads those cases, because a refusal has to be grounded
//! in what IAM said.
//!
//! # Delivery
//!
//! The test message travels the way every notification of this service does: the AWS SDK
//! over the emulator's own in-process callback ([`super::sns_notifications`] for a topic,
//! `service_common::sqs_client` for a queue), so nothing here opens a socket or leaves the
//! host — the ARN a caller supplied is not an address this code dials, it is a resource the
//! request is routed to INSIDE this process, so the target has to be one this emulator
//! itself holds or that target's own service answers that it does not exist. The address is
//! read out of the ARN through the workspace's own readings of it
//! (`service_common::sqs_client::queue_url_of_arn`, `service_common::arn::region_of`), which
//! admit only a Region of the label grammar, an account field that is an account id and a
//! name that is one path segment — so a caller cannot steer the authority a delivery is made
//! to. A delivery the target's service refuses is logged: the grounded decision above is what
//! refuses the call, and a publish that fails afterwards does not unwrite a stored hook.
//!
//! LOCK ORDER: nothing here takes a lock of this service's store, and the hop that decides
//! the publish is made with none held — callers decide between their own critical sections,
//! so no two stores are ever held at once and nothing awaits while either is held.

use std::time::SystemTime;

use super::StoredGroup;

/// The vendor field of an Amazon SQS ARN, and the action IAM publishes for the delivery of a
/// message to a queue of it.
///
/// Source: "Actions defined by Amazon SQS" (IAM Service Authorization Reference) — the
/// action a `SendMessage` to the queue is authorized by.
const SQS_PUBLISH: (&str, &str) = ("sqs", "sqs:SendMessage");

/// The same pair for an Amazon SNS topic: the vendor field of its ARN, and the action the
/// `Publish` to it is authorized by ("Actions defined by Amazon SNS").
const SNS_PUBLISH: (&str, &str) = ("sns", "sns:Publish");

/// One test message waiting to be delivered: the target it goes to and the document that
/// target receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTestMessage {
    /// The `NotificationTargetARN` the request named.
    pub target_arn: String,
    /// The notification, rendered as every notification of this service is.
    pub message: String,
}

/// The action the publish to `target_arn` is authorized by, or `None` for a value that names
/// no target this service publishes a notification to.
///
/// The vendor field of the ARN is what says which service holds the target — "you can
/// specify either an Amazon SNS topic or an Amazon SQS queue"
/// (`PutLifecycleHookType$NotificationTargetARN`) — and each of those two publishes one
/// action for the delivery. A value of any other vendor, and one this emulator reads no ARN
/// out of, names nothing to publish to, so there is no publish to authorize.
#[must_use]
pub fn publish_action(target_arn: &str) -> Option<&'static str> {
    match ::service_common::arn::vendor(target_arn)? {
        vendor if vendor == SQS_PUBLISH.0 => Some(SQS_PUBLISH.1),
        vendor if vendor == SNS_PUBLISH.0 => Some(SNS_PUBLISH.1),
        _ => None,
    }
}

/// Whether IAM ANSWERS that the role `role_arn` names may not publish to `target_arn`.
///
/// `region` is the Region the request was signed for, which is the Region the question is put
/// in. `false` — withhold nothing — for everything that is not such an answer: see this
/// module's own documentation.
pub async fn unpublishable_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    target_arn: &str,
    role_arn: &str,
) -> bool {
    // A value naming no target this service publishes to has no publish to authorize, so
    // there is nothing to ask about.
    let Some(action) = publish_action(target_arn) else {
        return false;
    };
    ::service_common::iam_role::role_withholds_action(ctx, region, role_arn, action, target_arn)
        .await
}

/// The refusal a request naming a target the role may not publish to is answered with, or
/// `None` when the publish is one the role may perform — and for everything that is not an
/// answer about that role.
///
/// `ValidationError` is the code Amazon EC2 Auto Scaling answers a request it cannot satisfy
/// with; the operation's own modelled errors are about a quota and about contention, neither
/// of which this is.
pub async fn unpublishable_target_error<E>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    target_arn: &str,
    role_arn: &str,
) -> Option<crate::scaffolded::Scaffolded<E>> {
    unpublishable_target(ctx, region, target_arn, role_arn)
        .await
        .then(|| {
            crate::scaffolded::Scaffolded::validation_error(unable_to_publish_message(
                target_arn, role_arn,
            ))
        })
}

/// What the caller is told when the test message could not be published: the two ARNs the
/// publish was attempted with, both taken from the request, and the rest fixed text telling
/// the caller which configuration to correct and that the hook was not put.
#[must_use]
pub fn unable_to_publish_message(target_arn: &str, role_arn: &str) -> String {
    format!(
        "Unable to publish test message to notification target {target_arn} using IAM role \
         {role_arn}. Please check your target and role configuration and try to put lifecycle \
         hook again."
    )
}

/// The test message `target_arn` is owed for `group`, or `None` when the value names no
/// target this service publishes to.
///
/// `request_id` is the id of the call that named the target: the notification reports which
/// request produced it, exactly as the confirmation of a notification configuration does.
#[must_use]
pub fn owed_test_message(
    group: &StoredGroup,
    target_arn: &str,
    request_id: &str,
    at: SystemTime,
) -> Option<PendingTestMessage> {
    publish_action(target_arn)?;
    Some(PendingTestMessage {
        target_arn: target_arn.to_string(),
        message: super::sns_notifications::test_notification_document(group, request_id, at),
    })
}

/// Deliver every message in `messages` to the target it names.
///
/// Called with NO store lock held, so nothing awaits while this service's state is locked. A
/// delivery the target's own service refuses — most often a target that does not exist, or
/// one no such service is mounted to hold — is logged and leaves the stored hook alone: the
/// refusal of the call is the grounded decision above.
pub async fn publish(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    messages: &[PendingTestMessage],
) {
    for message in messages {
        // Re-read from the ARN rather than carried alongside it, so the target a delivery is
        // addressed to is always one that came out of the ARN.
        let Some(vendor) = ::service_common::arn::vendor(&message.target_arn) else {
            continue;
        };
        if vendor == SNS_PUBLISH.0 {
            // A topic is published to exactly as every other notification of this service is,
            // which is also where the Region the publish is addressed in is read.
            super::sns_notifications::publish(
                ctx.callback(),
                &[super::sns_notifications::PendingNotification {
                    topic_arn: message.target_arn.clone(),
                    message: message.message.clone(),
                }],
            )
            .await;
            continue;
        }
        if vendor != SQS_PUBLISH.0 {
            continue;
        }
        // The url the queue is addressed by, through the workspace's one reading of a queue
        // ARN into one (`service_common::sqs_client`): a value whose fields name no queue is
        // not delivered to, which is what keeps a caller-written ARN from reshaping the
        // address.
        let Some(queue_url) = ::service_common::sqs_client::queue_url_of_arn(&message.target_arn)
        else {
            continue;
        };
        let Some(region) = ::service_common::arn::region_of(&message.target_arn, SQS_PUBLISH.0)
        else {
            continue;
        };
        let sent = ::service_common::sqs_client::sqs_client(ctx, region)
            .send_message()
            .queue_url(&queue_url)
            .message_body(&message.message)
            .send()
            .await;
        if let Err(error) = sent {
            ::tracing::warn!(
                target = %message.target_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the test message for a lifecycle hook's notification target could not be \
                 delivered to it"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A queue ARN of this emulator's own account, as Amazon SQS mints one.
    fn queue_arn(name: &str) -> String {
        format!(
            "arn:aws:sqs:us-west-2:{}:{name}",
            ::engine::emulator_account_id()
        )
    }

    /// RULE: the publish a notification target is tested by is the action the target's OWN
    /// service publishes for a delivery to it, and a value naming no such target has none.
    ///
    /// The rows span the vendors the member admits and the values that name no target at all.
    #[test]
    fn the_action_tested_is_the_one_the_targets_service_publishes() {
        let rows: &[(&str, String, Option<&str>)] = &[
            ("a queue of this account", queue_arn("hook-q"), Some("sqs:SendMessage")),
            (
                "a topic of this account",
                format!("arn:aws:sns:us-west-2:{}:hook-t", ::engine::emulator_account_id()),
                Some("sns:Publish"),
            ),
            (
                "a vendor this service publishes no notification to",
                "arn:aws:lambda:us-west-2:000000000000:function:hook".to_string(),
                None,
            ),
            ("no ARN at all", "not-an-arn".to_string(), None),
            ("the empty value a request clears a target with", String::new(), None),
        ];
        for (case, target, expected) in rows {
            let actual = publish_action(target);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} for {target:?}, got {actual:?}"
            );
        }
    }
}
