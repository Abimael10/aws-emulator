// SPDX-License-Identifier: CC0-1.0

//! Whether the caller may hand this service the IAM role a data transformation
//! job would run as, and how a request that may not is refused.
//!
//! `StartDataTransformationJob` takes "the Amazon Resource Name (ARN) of the
//! Amazon Web Services Identity and Access Management (IAM) role that HealthLake
//! assumes to read from and write to the specified Amazon S3 locations"
//! (`StartDataTransformationJobRequest$DataAccessRoleArn`), so handing that role
//! over is a PASS of the role to this service: the request is allowed only where
//! the role may be passed to it, and a role whose trust policy admits some other
//! principal is one this service could never assume. The job would therefore read
//! nothing and write nothing, which is why the submission is refused with the
//! modelled `AccessDeniedException` instead of a job record being minted for work
//! that cannot run.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **what the role has to trust** — [`DATA_ACCESS_SERVICE_PRINCIPAL`], this
//!    service's own principal, which is what assumes the role;
//! 2. **what the account's IAM says about the role** —
//!    [`::service_common::iam_role::held_role_is_unassumable`], the tree's shared
//!    reading of a caller-supplied execution role (which role of this account the
//!    ARN names, the in-process `GetRole`, and the one reading of a trust document
//!    in [`::service_common::iam_trust_policy`]), so this service and every other
//!    one handed the same document decide alike;
//! 3. **who asked** — the principal the request's credential speaks for, resolved
//!    where `sts:GetCallerIdentity` resolves it ([`caller_principal_arn`]), so the
//!    refusal blames the identity the caller reads back for those credentials.
//!
//! Nothing is refused on evidence this emulator does not have. Each of these shows
//! nothing about the role and lets the submission through: a value that names no
//! role of this account (another account's IAM is not this emulator's to ask), a
//! role this account's IAM holds no record of — an access role is routinely
//! provisioned out of band, so its absence here is evidence about this store and
//! not about the role — and an answer this emulator could not read, including an
//! emulator with no IAM mounted at all.

use std::time::SystemTime;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

/// The service principal a `DataAccessRoleArn` has to trust for this service to be
/// able to use it, in the global spelling a trust policy names a service by.
pub const DATA_ACCESS_SERVICE_PRINCIPAL: &str = "healthlake.amazonaws.com";

/// The IAM action a caller performs when it hands a service a role for the service
/// to assume, as a refusal names it.
pub const PASS_ROLE_ACTION: &str = "iam:PassRole";

/// How a submission that hands this service a role it may not pass is worded: the
/// standard authorization sentence, whose variable parts are the identity the
/// request speaks for and the role ARN it asked to pass.
///
/// Both values are the ones the decision was taken about — the caller IAM resolved
/// the credential to, and the `DataAccessRoleArn` the request carried, bounded by
/// the modelled `DataTransformationIamRoleArn` (20..2048,
/// `^arn:aws(-[^:]+)?:iam::[0-9]{12}:role/.+$`) the boundary validator has already
/// enforced — so a caller correlating the refusal with what it sent can.
pub fn pass_role_denied_message(caller_arn: &str, role_arn: &str) -> String {
    format!(
        "User: {caller_arn} is not authorized to perform: {PASS_ROLE_ACTION} on resource: \
         {role_arn}"
    )
}

/// `true` — refuse the submission — when the account's IAM ANSWERED that
/// `role_arn` is a role it HOLDS whose trust policy does not let this service
/// assume it, and `false` when it is usable or when nothing could be read to
/// decide it (see the module docs for the cases that show nothing).
pub async fn data_access_role_is_unpassable(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::held_role_is_unassumable(
        ctx,
        region,
        role_arn,
        DATA_ACCESS_SERVICE_PRINCIPAL,
    )
    .await
}

/// The ARN of the principal this request speaks for: the identity the request's
/// access key id resolves to.
///
/// ONE resolution of that key id — the one `sts:GetCallerIdentity` answers the same
/// request with, over the session registry and the account's IAM records in the
/// SAME shared engine — so the principal a refusal blames is the principal the
/// caller reads as itself. A credential that names no session speaks for the IAM
/// user holding that access key, and a key no record names for the account's own
/// published credential.
///
/// An engine carrying no session registry (a router that mounts this service
/// without the token service) can hold no session, so the only identity a request
/// can present there is the one its long-term key resolves to.
pub fn caller_principal_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let access_key_id = ::emulator_server::extract_akid(&ctx.parts().headers);
    let long_term = long_term_identity(engine, &access_key_id);
    let Some(registry) = sts::service_state(engine) else {
        return long_term.arn;
    };
    let resolved = sts::resolve_request_caller(
        &mut registry.lock(),
        &access_key_id,
        SystemTime::now(),
        ctx.credential_store(),
        long_term.clone(),
    )
    .map(|caller| caller.identity);
    resolved.unwrap_or(long_term).arn
}

/// The identity a LONG-TERM access key speaks for: the token service's OWN
/// resolution of such a key against the account's IAM records — the user the
/// account holds the key for, and, for a key no IAM record names, the account's own
/// principal.
///
/// Called rather than reproduced, so a caller reading its identity through
/// `sts:GetCallerIdentity` and a caller reading the principal this service blamed
/// for the same credential see one principal.
fn long_term_identity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    access_key_id: &str,
) -> sts::Identity {
    let iam = sts::iam_state(engine);
    let guard = iam.as_ref().map(|iam| iam.lock());
    sts::long_term_identity(guard.as_deref(), access_key_id)
}
