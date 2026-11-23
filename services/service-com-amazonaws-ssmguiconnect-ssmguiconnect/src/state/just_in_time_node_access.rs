// SPDX-License-Identifier: CC0-1.0

//! Whether the account is enrolled in just-in-time node access in the Region a request was
//! signed for — the account-level precondition `UpdateConnectionRecordingPreferences` is
//! gated on.
//!
//! Recording RDP connections is a just-in-time node access feature: an account that has not
//! set that workflow up cannot configure the recording of its sessions, which is the refusal
//! [`super::JUST_IN_TIME_NODE_ACCESS_DISABLED`] reports. The gate is therefore a question
//! about the ACCOUNT's own persisted state, and it is answered from the one place this
//! emulator holds that state: Systems Manager's access-request approval policies, the
//! documents of type `AutoApprovalPolicy` / `ManualApprovalPolicy` whose presence is what
//! turns the workflow on for a Region
//! ([`ssm_store::ComAmazonawsSsmAmazonssmState::has_access_request_approval_policy`], the
//! same predicate that service's own just-in-time node access operations are decided by).
//!
//! Reading that service's store — rather than keeping a second copy of the answer here — is
//! what makes the gate's input reachable by a request: a caller enrolls the account by
//! creating the approval policy through Systems Manager's own `CreateDocument`, and
//! withdraws the enrollment by deleting it, and the next Update sees either. It is the same
//! shape as [`super::authorization`], which reads the IAM and STS stores through their own
//! accessors to decide what the caller may do.
//!
//! The decision is taken per REGION, because an approval policy is a regional document and
//! the recording preferences are themselves a per-account/per-Region singleton: the Region
//! is the one the request's SigV4 credential scope names, read through Systems Manager's own
//! reading of it so the Region a policy is looked up under is exactly the one a
//! `CreateDocument` signed for the same scope stored it under.
//!
//! Nothing here writes to either store, and the question is asked with no guard of this
//! service's own state held.

use ::service_com_amazonaws_ssm_amazonssm::state as ssm_store;

/// Whether the account this request speaks for is enrolled in just-in-time node access in
/// the Region the request was signed for.
///
/// `false` for a fresh account, which holds no approval policy anywhere — the state AWS
/// leaves an account in until the workflow is set up — and `false` again once the policy
/// that enrolled a Region is gone.
#[must_use]
pub fn account_is_enrolled<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &::http::request::Parts,
) -> bool {
    let region = ssm_store::request_region(parts);
    ssm_store::service_state(engine)
        .is_some_and(|ssm| ssm.lock().has_access_request_approval_policy(&region))
}
