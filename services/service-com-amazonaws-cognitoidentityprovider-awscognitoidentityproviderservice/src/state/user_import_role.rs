// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs role a user import job runs under, and whether Amazon
//! Cognito can take it on.
//!
//! `CloudWatchLogsRoleArn` is "the role Amazon Resource Name (ARN) for the Amazon
//! CloudWatch Logging role for the user import job"
//! (`UserImportJobType.CloudWatchLogsRoleArn` in this service's model), and the
//! job writes its per-row results through that role. A role Amazon Cognito cannot
//! assume is one the job could never log through, so the import that would depend
//! on it is refused rather than run: the precondition `StartUserImportJob` asks
//! before it consumes the job's CSV file. The CREATE stores the ARN as submitted
//! — the role is validated where the import runs — so a job naming an
//! unassumable role exists and is refused its start for as long as the trust
//! policy withholds the assume.
//!
//! The reading is the tree's one reading of a caller-supplied role
//! (`service_common::iam_role::role_is_unassumable`): which role of this account
//! the ARN names, the in-process `GetRole`, and the trust-policy evaluation. So
//! nothing is refused on an answer this emulator could not read — a role of
//! another account, or an emulator with no IAM to ask — and a refusal always
//! rests on what IAM said. The grant is read afresh on every call, so a trust
//! policy corrected in place lifts the refusal on the next request and one broken
//! in place reinstates it.

use super::*;

/// The service principal Amazon Cognito takes a user import job's CloudWatch Logs
/// role on as, which that role's trust policy has to admit for `sts:AssumeRole`.
///
/// The same principal a pool's SMS caller role has to trust: one service, one
/// principal, so the two gates cannot disagree about who is assuming.
pub const IMPORT_LOGS_SERVICE_PRINCIPAL: &str = COGNITO_IDP_SERVICE_PRINCIPAL;

/// The sentence a start of a user import job is refused with when the role the
/// job names does not let Amazon Cognito assume it.
///
/// Fixed text, naming neither the role nor the job: the gate is about the ONE
/// role a job logs through, so the sentence states the defect of that role's
/// configuration and carries no value from the request.
pub const IMPORT_LOGS_ROLE_TRUST_MESSAGE: &str =
    "The configured Cloudwatch Logs Role is missing permissions or has an invalid trust policy.";

/// Whether the CloudWatch Logs role `role_arn` is one Amazon Cognito cannot take
/// on, so a user import job that names it could never log through it.
///
/// `true` only where IAM ANSWERED that it holds no such role of this account, or
/// that the role it holds carries a trust policy withholding the assume from
/// [`IMPORT_LOGS_SERVICE_PRINCIPAL`]; `false` for everything that is not such an
/// answer (see the module docs). Await it with no state guard held.
pub async fn import_logs_role_is_unassumable(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        role_arn,
        IMPORT_LOGS_SERVICE_PRINCIPAL,
    )
    .await
}
