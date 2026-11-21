// SPDX-License-Identifier: CC0-1.0

//! Whether the ROLE a subscriber delivers under may make the call its target
//! takes.
//!
//! "IAM role the service assumes to invoke the target"
//! (`InvokeConfiguration$RoleArn`), so a delivery is made AS A SESSION of that
//! role and the permissions in force on it decide the call — whatever the
//! publisher of the event may do. One reading for every kind of target, because
//! they differ only in the action the call performs and the resource it acts on
//! ([`super::Target::action`]), so a refusal of one reads exactly like a
//! refusal of another.
//!
//! The question itself is the tree's one reading of what a role may do
//! ([`::service_common::iam_role`]), asked in the order the delivery itself
//! takes it:
//!
//! * is it a role this service may act under at all? "IAM role the service
//!   assumes to invoke the target. Must belong to the calling account"
//!   (`InvokeConfiguration$RoleArn`), so a value naming no role of THIS account
//!   is one no delivery can be made under, and neither is a role IAM answers is
//!   not there, nor one whose trust document admits this service under none of
//!   the names it is written under
//!   ([`::service_common::iam_role::role_is_unassumable_through`]).
//! * do the permissions in force on it allow the call? A role IAM answers about
//!   may make the call only where they ALLOW it — a policy set that merely says
//!   nothing about the action grants nothing, which is IAM's default deny — and
//!   a statement that denies it refuses the call whatever is granted beside it
//!   ([`::service_common::iam_role::role_action_decision_through`]).
//!
//! Everything that is NOT such an answer withholds nothing: an answer this
//! emulator could not read, a failed hop, and an emulator with no IAM mounted at
//! all. A refusal has to be grounded in what IAM said, or in the value itself.
//!
//! Nothing here writes: it asks IAM what the role may do and states a sentence.

/// The service principal a role EventBridge V2 delivers under is assumed by.
///
/// A service is named in a trust document by its own principal name, and this
/// service's is built on its ARN namespace (`events`, the `arnNamespace` of its
/// `aws.api#service` trait); its regional endpoint prefix (`eventsv2`) is the
/// other name the same service is written under, so a document naming either
/// admits it — the reading
/// [`::service_common::iam_role::role_is_unassumable_through`] takes of ONE
/// spelling, asked for each.
const SERVICE_PRINCIPALS: [&str; 2] = ["events.amazonaws.com", "eventsv2.amazonaws.com"];

/// The IAM action the service performs to act under a subscriber's role, which a
/// refusal of that step names.
const ASSUME_ROLE_ACTION: &str = "sts:AssumeRole";

/// The sentence a delivery the role's own permissions do not allow is recorded
/// with, or `None` when nothing is known against the role.
///
/// `region` is the Region the hop to IAM is made in; `action` and `resource`
/// are the call the delivery would make ([`super::Target::action`],
/// [`super::Target::arn`]).
pub(super) async fn withheld(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Option<String> {
    use ::service_common::iam_authorization::Decision;

    // A role of ANOTHER account is one this service may not act under at all:
    // "IAM role the service assumes to invoke the target. Must belong to the
    // calling account" (`InvokeConfiguration$RoleArn`). No hop is made for it,
    // because another account's IAM is not this emulator's to ask — the value
    // itself is the answer.
    let names_no_role_of_this_account =
        ::service_common::iam_role::role_name(role_arn).is_none();
    if names_no_role_of_this_account || cannot_be_assumed(callback, region, role_arn).await {
        // The assume is what AWS refuses here, so the sentence is about that
        // step: the service that would have acted under the role, the action
        // the assume performs and the role it names.
        return Some(::service_common::iam_authorization::not_authorized_to_perform(
            SERVICE_PRINCIPALS[0],
            ASSUME_ROLE_ACTION,
            role_arn,
        ));
    }
    let reason = match ::service_common::iam_role::role_action_decision_through(
        callback, region, role_arn, action, resource,
        // The delivery carries no condition context of its own: nothing is
        // guessed for a key it does not state, which is the fail-closed reading
        // every clause this emulator cannot decide is given.
        &[],
    )
    .await
    {
        // Nothing was learned about the role, so nothing is refused.
        None => return None,
        Some(Decision::Allowed) => return None,
        Some(Decision::Denied) => {
            ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        }
        Some(Decision::NotStated) => {
            ::service_common::iam_authorization::no_identity_policy_allows(action)
        }
    };
    Some(
        ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &::service_common::iam_role::assumed_role_session_arn(role_arn, DELIVERY_SESSION_NAME),
            action,
            resource,
            &reason,
        ),
    )
}

/// Whether IAM ANSWERED that this service cannot act under the role `role_arn`
/// names: this account holds no such role, or the trust document the role
/// carries admits this service under NONE of the names it is written under.
///
/// A document admitting ANY of those names admits this service, so the reading
/// is asked per name and the role is unassumable only when every one of them is
/// withheld.
async fn cannot_be_assumed(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    role_arn: &str,
) -> bool {
    for principal in SERVICE_PRINCIPALS {
        if !::service_common::iam_role::role_is_unassumable_through(
            callback, region, role_arn, principal,
        )
        .await
        {
            return false;
        }
    }
    true
}

/// The name of the role SESSION a subscriber's deliveries are made under, which
/// a refusal naming the acting principal carries: a role is never acted as
/// directly, so the principal is a session of it
/// ([`::service_common::iam_role::assumed_role_session_arn`]). Stated once for
/// this service, so every delivery of every subscriber is made as the same kind
/// of session.
const DELIVERY_SESSION_NAME: &str = "EventBridgeV2SubscriberDelivery";
