// SPDX-License-Identifier: CC0-1.0

//! Whether the ROLE a rule's target names is itself a grant of that target's own
//! API call.
//!
//! `Target$RoleArn` is "the Amazon Resource Name (ARN) of the IAM role to be used
//! for this target when the rule is triggered", and EventBridge makes that
//! target's call ACTING AS that role — so the identity the call is made as is the
//! role, and the documents in force on it are documents in force over the call.
//! For the target kinds whose call EventBridge makes as its OWN service principal
//! instead ("for Lambda functions, Amazon SNS topics and Amazon SQS queues,
//! EventBridge relies on resource-based policies"), that principal holds no
//! identity-based policies, which is why the target's own document is the whole
//! decision there — but only for a target that names NO role. A target that names
//! one is reached as that role, and within one account "the resource-based policy
//! and the identity-based policy are evaluated together, and access is granted if
//! either allows it": the role's own permissions are then a second grant of the
//! same call, so a function, queue, topic or log group whose own document grants
//! this service nothing still receives the event when the role the caller handed
//! EventBridge may make that call.
//!
//! This answers the GRANT alone — whether the role's permissions allow the call —
//! and never a refusal: what a role BARS is [`super::delivery_refusal`], asked of
//! the kinds AWS invokes under the role, and whether the role can be assumed at
//! all is [`super::role_assumption`], asked of every target that names one before
//! the call is reached. So a role nothing can be shown about (one the store does
//! not hold, one whose documents cannot be read) grants nothing here and leaves
//! the target's own document deciding the delivery, exactly as a target naming no
//! role does.

use super::*;

/// Whether the permissions in force on the role `role_arn` ALLOW the invocation
/// `attempt` describes, made for the rule `rule_arn`.
///
/// `false` for everything that is not a shown grant: a target naming no role, a
/// `RoleArn` the account holds no role for, a role EventBridge cannot assume for
/// this rule (such a delivery fails at that step, which is
/// [`super::role_assumption::assume_role_refusal`]'s answer and not this one's),
/// a role whose documents cannot all be read, and a role whose documents state
/// nothing about the call or deny it.
///
/// The documents are the account's own IAM records — the ones
/// `CreateRole`/`PutRolePolicy`/`AttachRolePolicy` wrote — read through the one
/// shared reader ([`super::role_documents`]) and evaluated by the one shared
/// evaluator, so this service's reading of a role is every service's.
pub fn role_grants_delivery(
    role_arn: Option<&str>,
    rule_arn: &str,
    attempt: &DeliveryAttempt<'_>,
    iam: &IamState,
) -> bool {
    let Some(role_arn) = role_arn.filter(|arn| !arn.is_empty()) else {
        return false;
    };
    if role_assumption::assume_role_refusal(Some(role_arn), rule_arn, iam).is_some() {
        return false;
    }
    role_identity_decision(role_arn, attempt, iam)
        == Some(::service_common::iam_authorization::Decision::Allowed)
}

/// What the documents in force on the role `role_arn` STATE about the invocation
/// `attempt` describes, or `None` when nothing can be shown about them — the ONE
/// read of that question, shared with the REFUSAL half of it
/// ([`super::delivery_refusal`], which asks it of the kinds AWS invokes under the
/// role), so the two readings of a role cannot drift.
///
/// `None` is what [`super::role_documents`] cannot show: a `RoleArn` the account
/// holds no role for, a role carrying an attachment whose document this account
/// does not hold, and one carrying more documents than the gate reads.
pub fn role_identity_decision(
    role_arn: &str,
    attempt: &DeliveryAttempt<'_>,
    iam: &IamState,
) -> Option<::service_common::iam_authorization::Decision> {
    let documents = role_documents(role_arn, iam)?;
    let request =
        ::service_common::iam_authorization::Request::new(attempt.action, attempt.resource, &[]);
    Some(::service_common::iam_authorization::identity_decision(
        &documents, &request,
    ))
}
