// SPDX-License-Identifier: CC0-1.0

//! Whether the IAM role a member of this service names is one Systems Manager can act as.
//!
//! A machine that registers with an activation is managed UNDER the role the activation
//! names ("The name of the Identity and Access Management (IAM) role that you want to
//! assign to the managed node. This IAM role must provide AssumeRole permissions for the
//! Amazon Web Services Systems Manager service principal ssm.amazonaws.com",
//! `CreateActivationRequest$IamRole`), so the role has to exist and admit that assume
//! before an activation is minted for it: an activation bound to a role this service
//! could never take on is a code that can register nothing. The same holds of every other
//! member of this service that hands it a role to act under — the role a hybrid node is
//! moved to (`UpdateManagedInstanceRoleRequest$IamRole`), and the role a cloud connector
//! "uses to communicate with the third-party cloud environment"
//! (`CreateCloudConnectorRequest$RoleArn`) — so the one reading serves them all: a member
//! carrying a role NAME goes through [`refusal`] and one carrying an ARN through
//! [`arn_refusal`].
//!
//! The question is the tree's shared reading of a caller-supplied role
//! ([`::service_common::iam_role::role_is_unassumable`]) — which role of this account the
//! value names, the in-process `GetRole`, and the trust-policy reading
//! ([`::service_common::iam_trust_policy`]) — so this module states only how the role NAME
//! this service's members carry addresses that role, which principal has to be admitted,
//! and how the refusal is worded. A refusal is therefore always grounded in what IAM
//! ANSWERED: this account holds no role under that name, or the trust policy of the role
//! it does hold withholds the assume from this service. Nothing that is not such an
//! answer refuses anything — an answer this emulator could not read, or an emulator with
//! no IAM mounted to ask at all.

/// The service principal a role this service acts under has to admit, in the global
/// spelling a trust policy names a service by: this service is what assumes the
/// role (`CreateActivationRequest$IamRole`: "AssumeRole permissions for the Amazon Web
/// Services Systems Manager service principal ssm.amazonaws.com").
pub const HYBRID_REGISTRATION_PRINCIPAL: &str = "ssm.amazonaws.com";

/// The ARN of the role of this account a hybrid-registration `IamRole` NAME addresses:
/// an IAM ARN carries no Region, so the account is all the name needs to become one
/// ([`::service_common::iam_policy_principals::ACCOUNT_PARTITION`],
/// [`::engine::emulator_account_id`]). A name carrying a path
/// (`service-role/<name>`) composes the ARN IAM holds such a role under.
#[must_use]
pub fn role_arn(iam_role: &str) -> String {
    format!(
        "arn:{}:iam::{}:role/{iam_role}",
        ::service_common::iam_policy_principals::ACCOUNT_PARTITION,
        ::engine::emulator_account_id()
    )
}

/// How a request naming a role this service cannot act as is refused: the fixed sentence
/// plus the ARN of the role, which is the request's own `IamRole` under this account
/// ([`role_arn`]) and the only part of the message that varies.
#[must_use]
pub fn unassumable_role_message(role_arn: &str) -> String {
    format!("Nonexistent role or missing ssm service principal in trust policy: {role_arn}")
}

/// `Some(message)` — refuse the request — when IAM ANSWERED that the role named
/// `iam_role` is one this service cannot assume, and `None` when it can be assumed or
/// when nothing could be read to decide it (see the module docs).
pub async fn refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    iam_role: &str,
) -> Option<String> {
    let role_arn = role_arn(iam_role);
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        &role_arn,
        HYBRID_REGISTRATION_PRINCIPAL,
    )
    .await
    .then(|| unassumable_role_message(&role_arn))
}

/// [`refusal`] for a member that carries the role's ARN rather than its name, which is the
/// same question about the same role: the ARN is read as handed, because it is already the
/// spelling IAM holds the role under.
///
/// An ARN naming ANOTHER account is refused without asking IAM anything. The role a resource
/// of this account acts under has to be a role of this account — IAM holds no record of
/// another account's roles for this emulator to ask about, and "nonexistent role" is what
/// naming one amounts to for the caller — so the decision is taken here rather than left to
/// the shared reading, which stays silent about another account on purpose
/// ([`::service_common::iam_role::role_is_unassumable`]). A member carrying a role NAME
/// ([`refusal`]) names no account at all, so the question does not arise there.
pub async fn arn_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> Option<String> {
    if ::service_common::iam_role::role_name(role_arn).is_none() {
        return Some(unassumable_role_message(role_arn));
    }
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        role_arn,
        HYBRID_REGISTRATION_PRINCIPAL,
    )
    .await
    .then(|| unassumable_role_message(role_arn))
}
