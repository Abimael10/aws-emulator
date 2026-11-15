// SPDX-License-Identifier: CC0-1.0

//! Whether S3 Batch Operations can ASSUME the role a submitted job runs as.
//!
//! A job's tasks do not run as whoever created the job: Batch Operations carries
//! them out ACTING AS the job's own role — "the Identity and Access Management
//! (IAM) role that Batch Operations uses to run this job's operation on every
//! object in the manifest" (`JobDescriptor$RoleArn`) — so a role the service
//! cannot assume is a job with nothing to run under, which settles failed with
//! that reason instead of mutating a single object of its list.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the job's own stored `RoleArn`, read off the record the run is
//!    about to carry out, so a job is judged by the role it carries;
//! 2. **what the role trusts** — the `AssumeRolePolicyDocument` the IAM store
//!    holds for it, read out of IAM's OWN state in the SAME shared engine, so
//!    this service sees exactly what `CreateRole` and `UpdateAssumeRolePolicy`
//!    wrote rather than a second copy of it;
//! 3. **the decision** — [`::service_common::iam_trust_policy`], the tree's ONE
//!    reading of a trust document, so this service and every other one handed the
//!    same document answer alike.
//!
//! Nothing is refused on evidence this emulator does not have. Each of these
//! shows nothing about the role and lets the job run: an engine carrying no IAM
//! store, a `RoleArn` the store holds no role for (an execution role is routinely
//! provisioned out of band, and it may belong to another account), a role
//! carrying no trust document at all, and a document that cannot be read as a
//! policy. What a stored document DOES state is decided in IAM's own order — an
//! explicit `Deny` wins, a grant admits, a document that says nothing about this
//! principal withholds the assume — because the trust document is the authority
//! on who may assume the role.

/// The IAM store a job's `RoleArn` is resolved against.
///
/// It is the IAM service's OWN state struct, reached through the SAME shared
/// engine this service's store lives in, so a job reads the very records IAM's
/// handlers wrote.
pub type IamState =
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The service principal a job's role has to trust for S3 Batch Operations to
/// run that job's tasks under it, as a trust document names a principal.
pub const BATCH_OPERATIONS_PRINCIPAL: &str = "batchoperations.s3.amazonaws.com";

/// Why S3 Batch Operations cannot assume the role `role_arn` AT ALL, or `None`
/// when nothing of the kind can be shown (see the module docs for the cases that
/// show nothing).
///
/// The sentence states the denial, names the role — the one variable part — and
/// states the grant its trust policy is missing, and it is what the failed job
/// reports in `JobDescriptor$FailureReasons`.
pub fn assume_refusal(role_arn: &str, iam: &IamState) -> Option<String> {
    let role = iam.roles.values().find(|role| role.arn == role_arn)?;
    let trust = role.assume_role_policy_document.as_deref()?;
    match ::service_common::iam_trust_policy::admits_service_principal(
        trust,
        BATCH_OPERATIONS_PRINCIPAL,
    ) {
        Some(false) => Some(assume_failure_reason(role_arn)),
        _ => None,
    }
}

/// [`assume_refusal`] against the IAM store of the shared engine this service
/// runs in, for a caller that holds the engine rather than a locked IAM state.
pub fn engine_assume_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
) -> Option<String> {
    let iam =
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)?;
    let held = iam.lock();
    assume_refusal(role_arn, &held)
}

/// The reason a job reports when its role cannot be assumed: the DENIAL this
/// failure class is (`AccessDenied`), naming WHICH role was denied — the one
/// variable part, taken from the job's own `RoleArn` — and the grant its trust
/// policy is missing: [`BATCH_OPERATIONS_PRINCIPAL`] performing
/// [`::service_common::iam_trust_policy::ASSUME_ROLE_ACTION`] on it.
///
/// The sentence that states the denial and names the role comes first; the
/// sentence that states the missing grant follows whenever both fit inside
/// [`FAILURE_REASON_LENGTH_MAX`], the `@length` of `JobFailureReason`. A role
/// ARN is `@length(max: 2048)` while the reason is bounded at 256, so for a long
/// ARN the explanation is left out rather than cut mid-sentence — what cannot be
/// dropped is the denial and the role it is about.
///
/// The denial is what this failure IS, so the reason opens with it: the document
/// the account's IAM holds for that role withholds the grant, so resubmitting
/// the same job cannot run it. A reason that instead opened by reporting the
/// assume as not having succeeded YET would be the sentence of another case — a
/// grant that has not reached this service — which a client tells apart by the
/// reason and answers by submitting the job again rather than by reading the
/// refusal.
pub fn assume_failure_reason(role_arn: &str) -> String {
    let named = format!("Access denied for the IAM role {role_arn}.");
    let because = format!(
        " Its trust policy must allow {BATCH_OPERATIONS_PRINCIPAL} to perform {}.",
        ::service_common::iam_trust_policy::ASSUME_ROLE_ACTION
    );
    let bound = super::batch_job_worker::FAILURE_REASON_LENGTH_MAX;
    if named.chars().count() + because.chars().count() <= bound {
        return format!("{named}{because}");
    }
    super::batch_job_worker::within(&named, bound)
}
