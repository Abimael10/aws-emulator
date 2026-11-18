// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER may hand this service the IAM role its request names.
//!
//! A request that carries a `Target.RoleArn` hands EventBridge Scheduler a role the
//! service will act as when it invokes the target — "the Amazon Resource Name (ARN) of
//! the IAM role that EventBridge Scheduler will use for this target when the schedule
//! is invoked" (`Target$RoleArn`). Handing a role to a service is an action of the
//! CALLER's, not of the service: it is permitted only where the identity-based policies
//! in force on the calling principal allow it [`PASS_ROLE_ACTION`] for that very role.
//! Without that step a principal allowed this service's own create can give a schedule
//! any role of the account and have the service act as it, which is an escalation past
//! what that principal holds.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the IAM
//!    store of the same shared engine, which holds both halves of that question: the
//!    registry `iam:CreateAccessKey` wrote for a long-term key, and the role-session
//!    record the minting operation left for a temporary one. So the principal a refusal
//!    blames is the one the account really holds for the credential the request signed
//!    with;
//! 2. **what it may do** — the identity-based documents the IAM store holds for that
//!    principal, and the permissions boundary that caps them, read through IAM's own
//!    accessors (`principal_policies`, `permissions_boundary_document`) so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts
//!    the refusal again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE
//!    evaluation of policy documents against `(action, resource)`, so this service and
//!    every other one handed the same documents decide alike: an explicit `Deny` wins
//!    over every `Allow`, an `Allow` about the request grants it, and with neither the
//!    request falls to IAM's default deny.
//!
//! The sentence a refusal is worded with is the shared one
//! ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), built
//! from the principal the credential speaks for, the action and the role ARN the REQUEST
//! carried — so nothing has to be looked up to state it.
//!
//! Only the store lookups live here; the lookups stay in this crate because hoisting them
//! into `service-common` would make every service depending on that crate link the IAM
//! service crate.
//!
//! All three pieces above are the ONE reading of a request's caller this crate has
//! ([`super::caller_identity`]), shared with the gate that decides the request's own action
//! ([`super::caller_authorization`]), so two gates handed the same credential blame the same
//! principal; this module adds only WHICH action and WHICH resource the pass is judged on,
//! and the condition key the pass itself fixes. What a decision does not bound is stated
//! there, and holds for the pass too:
//!
//! ## What this gate does not bound
//!
//! Nothing is refused on evidence this emulator does not have:
//!
//! * **the account's own published credential**, which no IAM record holds policies for —
//!   the position AWS account root credentials are in — may pass any role, which is also
//!   what keeps an emulated service calling this one through the in-process callback from
//!   being refused for want of a grant;
//! * **a principal carrying an attachment whose document this account does not hold** (an
//!   AWS-managed policy belongs to AWS, so the store holds no document for it) is not
//!   refused on a grant that cannot be read, though an explicit `Deny` in a document that
//!   CAN be read still refuses it;
//! * **an engine carrying no IAM store**, where no policy can exist at all.
//!
//! Whether the role the request names EXISTS is deliberately not part of this question:
//! `iam:PassRole` is about what the CALLER may hand over, so it is decided from the
//! caller's documents and the ARN the request carried. That is also what keeps a refused
//! call from telling an unauthorized caller whether a role is there — the gate is taken
//! BEFORE the role is resolved against IAM
//! ([`super::execution_role_violation`]) and before anything is written.

use ::service_common::iam_authorization as iam;

use super::caller_identity::{identity_denial, request_caller};

/// The IAM action a request that hands this service a role performs: "To pass a role to
/// an AWS service, a user must have permissions to pass the role to the service".
pub const PASS_ROLE_ACTION: &str = "iam:PassRole";

/// `iam:PassedToService` — IAM's condition key for the SERVICE a role is being passed to,
/// which AWS guidance tells a caller to scope an `iam:PassRole` grant with and which IAM
/// populates from the call itself rather than from the request.
///
/// The service this request would hand the role to is this one, so the value is this
/// service's own principal — read from the one place this crate spells it
/// ([`super::SCHEDULER_SERVICE_PRINCIPAL`]), the principal a schedule's execution role has
/// to trust — and a grant conditioned on another service's principal does not cover the
/// pass.
const PASSED_TO_SERVICE_CONDITION_KEY: &str = "iam:PassedToService";

/// Why this caller may not hand the role `role_arn` to this service, or `None` when it may
/// — including every caller this gate does not bound (see the module docs).
///
/// Call it BEFORE this service's store is read or written, and with the ARN the request
/// CARRIED rather than one resolved from a stored record, so the decision does not depend
/// on whether that role exists. Nothing is cached, so a policy just written decides the
/// next request.
pub fn pass_role_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    role_arn: &str,
) -> Option<String> {
    let caller = request_caller(engine, ctx);
    let context = pass_context();
    identity_denial(
        &caller,
        PASS_ROLE_ACTION,
        role_arn,
        &iam::Request::new(PASS_ROLE_ACTION, role_arn, &caller.principals)
            .with_context(&context),
    )
}

/// The condition context this pass carries: the SERVICE the role is being passed to
/// ([`PASSED_TO_SERVICE_CONDITION_KEY`]), which is the one value a grant of this request
/// can be scoped by and the only key the call itself fixes. Nothing else is filled in: a
/// clause over a key this request does not carry is left undecided by the shared
/// evaluator rather than guessed at, which is what keeps the decision fail-closed.
fn pass_context() -> [(String, String); 1] {
    [(
        PASSED_TO_SERVICE_CONDITION_KEY.to_string(),
        super::SCHEDULER_SERVICE_PRINCIPAL.to_string(),
    )]
}

#[cfg(test)]
mod tests {
    use ::service_common::iam_authorization::Decision;

    use super::*;

    /// RULE: the refusal names the principal, the `iam:PassRole` action, the ROLE the
    /// request named and which of IAM's rules withheld the grant — every part taken from
    /// the request and the stored policies.
    ///
    /// The rows are the four reasons a decision of this gate can rest on.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_role() {
        let principal = "arn:aws:iam::000000000000:user/least-privilege";
        let role = "arn:aws:iam::000000000000:role/SchedulerRole";
        for (case, reason) in [
            (
                "nothing allows it",
                iam::no_identity_policy_allows(PASS_ROLE_ACTION),
            ),
            (
                "an explicit deny",
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            ),
            (
                "a boundary that allows nothing",
                iam::no_permissions_boundary_allows(PASS_ROLE_ACTION),
            ),
            (
                "a boundary that denies",
                iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            ),
        ] {
            let stated =
                iam::not_authorized_by_identity_policy(principal, PASS_ROLE_ACTION, role, &reason);
            let expected = format!(
                "User: {principal} is not authorized to perform: {PASS_ROLE_ACTION} on \
                 resource: {role} {reason}"
            );
            assert_eq!(
                stated, expected,
                "case {case:?}: expected IAM's wording {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: the decision is the shared evaluation of the caller's documents, so a policy
    /// scoped to the roles it may pass grants exactly those and nothing else.
    #[test]
    fn a_pass_role_grant_covers_the_roles_it_names() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"iam:PassRole",
            "Resource":"arn:aws:iam::000000000000:role/scheduler/*"}]}"#;
        let context = pass_context();
        // (case, the role the request named, the decision the documents reach)
        for (case, role, expected) in [
            (
                "a role the grant covers",
                "arn:aws:iam::000000000000:role/scheduler/Runner",
                Decision::Allowed,
            ),
            (
                "a role outside the grant",
                "arn:aws:iam::000000000000:role/Other",
                Decision::NotStated,
            ),
            (
                "a role of another account",
                "arn:aws:iam::210987654321:role/scheduler/Runner",
                Decision::NotStated,
            ),
        ] {
            let reached = iam::identity_decision(
                &[document.to_string()],
                &iam::Request::new(PASS_ROLE_ACTION, role, &[]).with_context(&context),
            );
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {expected:?} for {role}, got {reached:?}"
            );
        }
    }

    /// RULE: the pass carries the SERVICE it hands the role to, so a grant scoped with
    /// `iam:PassedToService` — the scoping AWS guidance tells a caller to write — is
    /// decided on its own terms: it covers a pass to this service and no other.
    #[test]
    fn a_grant_scoped_to_a_service_covers_a_pass_to_that_service() {
        let role = "arn:aws:iam::000000000000:role/scheduler/Runner";
        let scoped_to = |service: &str| {
            format!(
                r#"{{"Statement":[{{"Effect":"Allow","Action":"iam:PassRole","Resource":"*",
                "Condition":{{"StringEquals":{{"{PASSED_TO_SERVICE_CONDITION_KEY}":"{service}"}}}}}}]}}"#
            )
        };
        for (case, document, expected) in [
            (
                "a grant scoped to this service",
                scoped_to(super::super::SCHEDULER_SERVICE_PRINCIPAL),
                Decision::Allowed,
            ),
            (
                "a grant scoped to another service",
                scoped_to("ec2.amazonaws.com"),
                Decision::NotStated,
            ),
        ] {
            let context = pass_context();
            let reached = iam::identity_decision(
                &[document],
                &iam::Request::new(PASS_ROLE_ACTION, role, &[]).with_context(&context),
            );
            assert_eq!(
                reached, expected,
                "case {case:?}: expected {expected:?}, got {reached:?}"
            );
        }
    }
}
