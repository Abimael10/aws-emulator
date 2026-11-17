// SPDX-License-Identifier: CC0-1.0

//! The authorization decision a call that addresses a RECOVERY POINT by its ARN
//! takes before it resolves that point.
//!
//! # The rule this module implements
//!
//! AWS Backup authorizes such a call on the recovery point the request NAMES,
//! and it authorizes before it resolves: a recovery point belongs to the account
//! its ARN's account field names, and nothing a principal of another account
//! holds can grant access to it — only that account's own resource-based policy
//! could, and this emulator serves exactly one account
//! ([`engine::emulator_account_id`]). So an ARN whose account field is not that
//! account is refused for lack of privileges.
//!
//! The SENTENCE that refusal carries is the one IAM states when the resource a
//! request names grants the calling principal nothing
//! ([`::service_common::iam_authorization::not_authorized_by_resource_policy`]),
//! which is exactly the decision taken here: the resource lies in another account,
//! so only a resource-based policy of that account could have allowed it, and none
//! does. Every value in it comes from the request — the principal the credentials
//! it signed with speak for, the action it asked for (`backup:<Operation>`), and
//! the recovery-point ARN it carried, verbatim — so a client that parses the
//! refusal learns which principal lacked which action on which resource and can
//! name the IAM permission it is missing. Nothing about the resource is looked up
//! to state it, which is why it can still be stated before the point is resolved.
//!
//! Refusing it as ABSENT would be wrong twice over: a caller would learn from the
//! refusal what another account does or does not hold, and a client that branches
//! on the authorization refusal — to re-assume a role, to surface a permissions
//! error, to stop retrying — would instead read a call the service refused as a
//! point that is already gone. Naming the ARN the REQUEST carried discloses
//! nothing further: it is a value the caller supplied, and the refusal is the same
//! whether or not that point exists.
//!
//! The decision is taken from the REQUEST alone, so it needs no store of this
//! service's and runs ahead of every lookup. It is stated once here, for every
//! call that addresses a recovery point by ARN, so the delete, the read and the
//! two jobs that take one as input cannot drift into different answers — or
//! different wordings — for one condition.

/// The authorization decision for a call that addresses the recovery point
/// `recovery_point_arn` names: `Ok(())` when the ARN names a recovery point of
/// the account this emulator serves, and the refusal otherwise.
///
/// `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]), which names the IAM action
/// `backup:<Operation>` the refusal reports; `recovery_point_arn` is the ARN the
/// request carried, which it reports as the resource. The principal it blames is
/// the one the request's credentials speak for
/// ([`super::caller_principal_arn`]) — the same identity `sts:GetCallerIdentity`
/// answers with for them.
pub fn authorized_recovery_point<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    recovery_point_arn: &str,
) -> Result<(), crate::scaffolded::AccessDeniedException> {
    let own_account = ::service_common::arn::parse(recovery_point_arn)
        .is_some_and(|arn| arn.account == ::engine::emulator_account_id());
    if own_account {
        return Ok(());
    }
    let action = format!("{}:{operation}", super::ACTION_PREFIX);
    Err(crate::scaffolded::AccessDeniedException {
        message: ::service_common::iam_authorization::not_authorized_by_resource_policy(
            &super::caller_principal_arn(engine, ctx),
            &action,
            recovery_point_arn,
        ),
    })
}
