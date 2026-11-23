// SPDX-License-Identifier: CC0-1.0

//! Whether a delivery-status feedback role an attribute write NAMES is one this service could
//! ever assume FOR THE RESOURCE the write configures, asked before the attribute lands on it.
//!
//! "The <ENDPOINT>SuccessFeedbackRoleArn and <ENDPOINT>FailureFeedbackRoleArn attributes are
//! used to give Amazon SNS write access to use CloudWatch Logs on your behalf"
//! (`SetTopicAttributesInput$AttributeName` doc), and the same sentence for the platform
//! application's own pair ("`SuccessFeedbackRoleArn` – IAM role ARN used to give Amazon SNS
//! write access to use CloudWatch Logs on your behalf",
//! `SetPlatformApplicationAttributesInput$Attributes` doc): the value names an IAM role Amazon
//! SNS ASSUMES to write a delivery's status record ([`super::delivery_status`]), so a role
//! whose trust policy does not grant this service's principal
//! ([`super::SERVICE_PRINCIPAL`]) `sts:AssumeRole` is a role the feature could never work
//! under. SNS refuses such a write rather than storing a configuration it cannot honour, and
//! the resource reads back carrying no such attribute.
//!
//! The assume SNS would make is an assume FOR ONE RESOURCE — the topic, or the platform
//! application, whose delivery-status records the role would be assumed to write — and it
//! carries the confused-deputy keys of that work ([`SOURCE_ARN_CONDITION_KEY`] naming that
//! resource and [`SOURCE_ACCOUNT_CONDITION_KEY`] naming the account it belongs to), so a trust
//! statement's `Condition` is part of its grant: a role whose trust is narrowed to one resource
//! is a role SNS cannot assume for a sibling, and naming it on that sibling is refused. That is
//! the whole point of scoping a logging role by `aws:SourceArn`.
//!
//! The question is the tree's one reading of a role's trust relationship for one assume
//! (`service_common::iam_role::held_role_assume_is_unauthorized`), so the document that
//! decides it is the one a client's `CreateRole` or `UpdateAssumeRolePolicy` left on the role,
//! and the reading of it is the same one the publish-time assume takes. It is the HELD
//! reading: a role this account's IAM holds nothing under is no evidence about the role the
//! caller named — a feedback role is routinely provisioned out of band — so only a stored
//! trust document that itself withholds the assume is refused.

use std::collections::HashMap;

/// The condition key naming the RESOURCE a cross-service assume is made for — the topic or
/// platform application whose delivery-status records the role would be assumed to write.
pub const SOURCE_ARN_CONDITION_KEY: &str = "aws:SourceArn";

/// The condition key naming the ACCOUNT that resource belongs to.
pub const SOURCE_ACCOUNT_CONDITION_KEY: &str = "aws:SourceAccount";

/// The condition keys the assume of a feedback role FOR the resource `resource_arn` carries.
///
/// A key the resource's own ARN does not state is left out rather than guessed at: a clause
/// over a key the assume carries no value for is a clause that cannot be shown to hold, which
/// is the fail-closed reading the shared evaluation takes of it.
#[must_use]
pub fn assume_context(resource_arn: &str) -> Vec<(String, String)> {
    let mut context = vec![(
        SOURCE_ARN_CONDITION_KEY.to_string(),
        resource_arn.to_string(),
    )];
    if let Some(account) = ::service_common::arn::parse(resource_arn)
        .map(|arn| arn.account)
        .filter(|account| !account.is_empty())
    {
        context.push((
            SOURCE_ACCOUNT_CONDITION_KEY.to_string(),
            account.to_string(),
        ));
    }
    context
}

/// The refusal a write of the attribute `name` to `value` on the resource `resource_arn`
/// answers when the value names a role this service could not assume for that resource, or
/// `None` for every other value.
///
/// Answers `None` for a name that is no feedback-role attribute, so a caller hands it every
/// attribute a write carries and the rule decides which of them it is about. The sentence is
/// the one SNS states for a value it cannot use as a logging role
/// ([`super::topic_attributes::not_a_valid_logging_role`]), whichever half of the rule — the
/// ARN's form or the trust policy behind it — the value broke.
///
/// Call it with NO store guard held: it asks IAM over the in-process callback.
pub async fn untrusted_role_refusal(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_arn: &str,
    name: &str,
    value: &str,
) -> Option<String> {
    if !name.ends_with(super::FEEDBACK_ROLE_ARN_SUFFIX) {
        return None;
    }
    ::service_common::iam_role::held_role_assume_is_unauthorized(
        ctx,
        region,
        value,
        super::SERVICE_PRINCIPAL,
        &assume_context(resource_arn),
    )
    .await
    .then(|| super::topic_attributes::not_a_valid_logging_role(name, value))
}

/// The refusal the feedback roles `attributes` names answer for the resource `resource_arn`,
/// or `None` when every one of them is a role this service could assume for it.
///
/// The map form of [`untrusted_role_refusal`], for the write path that carries a whole
/// attribute map: the entries are asked about in name order, so a map naming two unusable
/// roles is refused with the same sentence whichever order the request spelled them in.
pub async fn untrusted_roles_refusal(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_arn: &str,
    attributes: &HashMap<String, String>,
) -> Option<String> {
    let mut names: Vec<&String> = attributes
        .keys()
        .filter(|name| name.ends_with(super::FEEDBACK_ROLE_ARN_SUFFIX))
        .collect();
    names.sort();
    for name in names {
        if let Some(message) =
            untrusted_role_refusal(ctx, region, resource_arn, name, &attributes[name]).await
        {
            return Some(message);
        }
    }
    None
}
