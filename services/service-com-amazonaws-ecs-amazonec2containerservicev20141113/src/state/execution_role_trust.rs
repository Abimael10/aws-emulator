// SPDX-License-Identifier: CC0-1.0

//! The TRUST POLICY of a role an Amazon ECS task runs under, and the one reading of it this
//! service takes.
//!
//! A task's execution role (`TaskDefinition$executionRoleArn`, "the task execution role that
//! grants the Amazon ECS container agent permission to make Amazon Web Services API calls on
//! your behalf") and its task role (`TaskDefinition$taskRoleArn`, "the IAM role that
//! containers in this task can assume") are both reached by ASSUMING them, and a role is
//! assumable only by the principals its own trust policy admits
//! (`CreateRole$AssumeRolePolicyDocument`: "The trust relationship policy document that grants
//! an entity permission to assume the role"). The principal Amazon ECS assumes either of them
//! as is [`ECS_TASKS_SERVICE_PRINCIPAL`], so ONE question decides both: does the trust policy
//! the role carries let that principal perform
//! [`::service_common::iam_trust_policy::ASSUME_ROLE_ACTION`]?
//!
//! That question is [`role_withholds_assumption`], and the sentence a task stopped by its
//! answer carries is [`unable_to_assume_role_message`]. WHICH roles a launch has Amazon ECS
//! assume, and what a launch it cannot assume one of does, is [`super::launch_role_trust`].
//!
//! WHEN the assume is taken decides who is told it failed. A task placed onto the cluster's
//! own capacity is assumed for after it is placed, by the agent on the instance that runs it,
//! so the answer reaches the caller as that task's stop. A task Amazon ECS provisions ITSELF
//! while it answers the call that asked for it — a Fargate placement a client's own `RunTask`
//! makes — is assumed for before anything is placed, so the answer is the call's: it is
//! refused with [`unable_to_assume_provided_role_message`] and no task is created
//! ([`roles_withholding_assumption`] is how a handler asks, holding no guard of this
//! service's store).
//!
//! The decision is taken against the account's OWN IAM records — the ones `CreateRole` and
//! `UpdateAssumeRolePolicy` wrote, reached through the shared engine — and through the tree's
//! one reading of a trust document
//! ([`::service_common::iam_trust_policy::admits_service_principal`]), so no two services
//! handed the same document answer differently.
//!
//! Only what IAM ANSWERED is refused: a role this account's IAM holds no record for (it may
//! have been provisioned out of band, which is the ordinary way these roles exist), a role
//! carrying no trust document, and a document this cannot read as a policy are each evidence
//! of nothing.

/// The service principal Amazon ECS assumes a task's roles as, which is therefore the
/// principal the trust policy of a task's execution role and task role has to admit (the
/// `ecs-tasks` service principal of the Amazon ECS task roles, Amazon Elastic Container
/// Service Developer Guide).
pub const ECS_TASKS_SERVICE_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The IAM store a launch's roles are resolved against.
///
/// It is the IAM service's OWN state struct, reached through the SAME shared engine this
/// service's store lives in, so Amazon ECS reads the very records `CreateRole` and
/// `UpdateAssumeRolePolicy` wrote rather than a second copy of them.
pub type IamState =
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The `stoppedReason` of a task whose role Amazon ECS could not assume: the fixed sentence
/// Amazon ECS words such a stop with, carrying the role the launch resolved.
pub fn unable_to_assume_role_message(role_arn: &str) -> String {
    format!(
        "ECS was unable to assume the configured role '{role_arn}'. Please verify that the \
         ECS service principal is allowed to assume this role."
    )
}

/// The refusal of a PLACEMENT Amazon ECS takes the assume for while it answers the call: the
/// fixed sentence it words that refusal with, carrying the role the placement resolved.
///
/// Amazon ECS states the remediation the caller has to make — the trust relationship of the
/// role that was passed — because the caller is still there to be told; the stop of a task
/// already placed states what happened to the task instead
/// ([`unable_to_assume_role_message`]).
pub fn unable_to_assume_provided_role_message(role_arn: &str) -> String {
    format!(
        "ECS was unable to assume the role '{role_arn}' that was provided for this task. \
         Please verify that the role being passed has the proper trust relationship and \
         permissions and that your IAM user has permissions to pass this role."
    )
}

/// Whether a trust document is a policy that does not let [`ECS_TASKS_SERVICE_PRINCIPAL`]
/// perform the assume: the ONE reading of a document this module makes, so the launch step
/// and the pre-placement refusal below cannot judge one role two ways.
///
/// `false` for a role carrying no document and for one this cannot read as a policy, for the
/// reason the module docs give.
fn withholds_assumption(document: Option<&str>) -> bool {
    document.and_then(|document| {
        ::service_common::iam_trust_policy::admits_service_principal(
            document,
            ECS_TASKS_SERVICE_PRINCIPAL,
        )
    }) == Some(false)
}

/// Whether the account's IAM ANSWERED that `role_arn` is a role Amazon ECS cannot assume for
/// a task: IAM holds that role, and the trust policy it carries is a policy that does not let
/// [`ECS_TASKS_SERVICE_PRINCIPAL`] assume it.
///
/// `false` for everything that is not such an answer — a role this store holds no record for,
/// a role carrying no trust document, and a document that cannot be read as a policy — for
/// the reason the module docs give.
pub fn role_withholds_assumption(iam: &IamState, role_arn: &str) -> bool {
    iam.roles
        .values()
        .find(|role| role.arn == role_arn)
        .is_some_and(|role| withholds_assumption(role.assume_role_policy_document.as_deref()))
}

/// Every role of this account IAM answers that Amazon ECS may not assume for a task
/// ([`role_withholds_assumption`]), read out of the IAM store under THAT store's own lock and
/// handed back owned.
///
/// This is what lets a placement decide its resolved roles with no guard of this service's
/// store held, which is the lock order every cross-store step of a launch takes
/// (`super::launch_role_trust`). Bounded by what IAM HOLDS, not by the request.
///
/// Empty for an engine carrying no IAM store: nothing was answered, so nothing is refused.
pub fn roles_withholding_assumption<Eng: Send + Sync + 'static>(engine: &Eng) -> Vec<String> {
    let Some(iam) =
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)
    else {
        return Vec::new();
    };
    let store = iam.lock();
    store
        .roles
        .values()
        .filter(|role| withholds_assumption(role.assume_role_policy_document.as_deref()))
        .map(|role| role.arn.clone())
        .collect()
}
