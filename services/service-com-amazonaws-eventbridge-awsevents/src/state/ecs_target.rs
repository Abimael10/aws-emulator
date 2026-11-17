// SPDX-License-Identifier: CC0-1.0

//! What the policies in force on a rule target's role have to say before an Amazon ECS
//! TASK TARGET of that rule is run.
//!
//! An Amazon ECS target is one EventBridge invokes ACTING AS the role the target names (the
//! `RoleArn` class, as against the targets whose own resource-based policy decides them), so
//! the documents in force on that role are what decide it — the shared reading every other
//! delivery of this service is decided through ([`super::delivery_refusal`]). What is
//! particular to this target kind is the pair of actions the invocation performs, and the
//! resource each of them is performed ON:
//!
//! * [`RUN_TASK_ACTION`], on the TASK DEFINITION the run names rather than on the cluster —
//!   the resource type Amazon ECS publishes for that action — with the cluster the task is
//!   placed in carried as the condition key [`CLUSTER_CONDITION_KEY`], so a grant scoped to
//!   one cluster authorizes runs into that cluster and no other;
//! * [`pass_role::PASS_ROLE_ACTION`], on each role the run HANDS Amazon ECS: "to pass a role
//!   to a service, a user must have permission to pass the role to the service", and a run
//!   hands over the task role its containers assume and the execution role the agent acts
//!   as. The key [`PASSED_TO_SERVICE_KEY`] carries the service they are handed to
//!   ([`TASKS_SERVICE_PRINCIPAL`]), so a grant scoped by it decides this hand-over and
//!   refuses the same role being handed to another service. A run that hands over no role
//!   performs the action on nothing, so nothing is asked of the role for it.
//!
//! Both are decided against the account's OWN IAM records, through
//! [`::service_common::iam_authorization`] — the tree's one evaluation of policy documents
//! against `(action, resource, condition context)` — and both answer the condition
//! [`NO_PERMISSIONS`], the one every refusal of this service for want of a permission
//! carries.
//!
//! The emulator refuses only what it can SHOW, so each of these is evidence of nothing and
//! lets the run go ahead, exactly as [`super::delivery_refusal`] has it: a target naming no
//! role, an engine with no IAM store, a `RoleArn` the store holds no role for, and a role
//! carrying more documents than [`super::MAX_ROLE_DOCUMENTS`]. Whether the role can be
//! ASSUMED at all is a question about the role alone and is decided once, before any call
//! ([`super::role_assumption`]), so it is not asked again here.

use ::service_common::iam_authorization::{self as iam, Decision};

use super::*;

/// The action EventBridge performs on an Amazon ECS target: it runs the task the target's
/// `EcsParameters` name ("when a rule is triggered ... EventBridge calls `RunTask`").
pub const RUN_TASK_ACTION: &str = "ecs:RunTask";

/// The condition key naming the CLUSTER a run places its task in — the key Amazon ECS
/// publishes for that action, so a grant can be scoped to the clusters the account means a
/// role to run tasks in.
const CLUSTER_CONDITION_KEY: &str = "ecs:cluster";

/// The condition keys a run DECLARES it models: the cluster is known in full, which is what
/// lets a clause written with IAM's `IfExists` suffix, or a negated clause, be answered. A
/// key outside this set leaves its statement undecided, which never grants and never weakens
/// a deny.
const RUN_TASK_CONDITION_KEYS: [&str; 1] = [CLUSTER_CONDITION_KEY];

/// The service principal Amazon ECS assumes a task's roles as, which is the service those
/// roles are handed TO.
pub const TASKS_SERVICE_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The condition key naming the service a role is being handed to, as the hand-over gate of
/// this service's own writers reads it ([`pass_role`]).
const PASSED_TO_SERVICE_KEY: &str = "iam:PassedToService";

/// The condition keys a hand-over declares it models, for the reason
/// [`RUN_TASK_CONDITION_KEYS`] gives.
const PASS_ROLE_CONDITION_KEYS: [&str; 1] = [PASSED_TO_SERVICE_KEY];

/// Why the policies in force on `role_arn` bar the RUN this target's parameters describe, or
/// `None` when they permit it.
///
/// `task_definition_arn` is the resource the action is performed on and `cluster_arn` the
/// cluster the run places into, carried as the condition key above.
#[must_use]
pub fn run_task_refusal(
    role_arn: &str,
    attempt: &DeliveryAttempt<'_>,
    task_definition_arn: &str,
    cluster_arn: &str,
    iam: &IamState,
) -> Option<(String, String)> {
    let context = [(
        CLUSTER_CONDITION_KEY.to_string(),
        cluster_arn.to_string(),
    )];
    action_refusal(
        role_arn,
        attempt,
        iam,
        RUN_TASK_ACTION,
        task_definition_arn,
        &context,
        &RUN_TASK_CONDITION_KEYS,
    )
}

/// Why the policies in force on `role_arn` bar HANDING Amazon ECS one of the roles `passed`
/// names, or `None` when they permit every one of them.
///
/// Asked over the roles in the order the run hands them over, so the first one the policies
/// withhold is what the refusal names — and nothing is run, because a run AWS refuses places
/// no task. A run that hands over no role is permitted by this gate without a document being
/// read.
#[must_use]
pub fn passed_roles_refusal(
    role_arn: &str,
    attempt: &DeliveryAttempt<'_>,
    passed: &[String],
    iam: &IamState,
) -> Option<(String, String)> {
    let context = [(
        PASSED_TO_SERVICE_KEY.to_string(),
        TASKS_SERVICE_PRINCIPAL.to_string(),
    )];
    passed.iter().find_map(|handed| {
        action_refusal(
            role_arn,
            attempt,
            iam,
            pass_role::PASS_ROLE_ACTION,
            handed,
            &context,
            &PASS_ROLE_CONDITION_KEYS,
        )
    })
}

/// Why the documents in force on `role_arn` do not permit `action` on `resource` under
/// `context`, worded the way every authorization refusal of this service is
/// ([`not_authorized`]), or `None` when they do.
///
/// The identity the refusal names is the SESSION the target's role is invoked in
/// ([`target_role_session_name`]), which is the identity the call would have been made as —
/// the same principal spelling [`super::delivery_refusal`] names, so two gates over one
/// delivery cannot word the same refusal two ways.
fn action_refusal(
    role_arn: &str,
    attempt: &DeliveryAttempt<'_>,
    iam: &IamState,
    action: &str,
    resource: &str,
    context: &[(String, String)],
    published: &[&str],
) -> Option<(String, String)> {
    let role = iam.roles.values().find(|role| role.arn == role_arn)?;
    let documents = role_documents(role_arn, iam)?;
    // An identity-based document states no `Principal` element, so the decision rests on the
    // action, the resource and the context.
    let request = iam::Request::new(action, resource, &[])
        .with_context(context)
        .with_published_keys(published);
    let decision = iam::identity_decision(&documents, &request);
    if decision == Decision::Allowed {
        return None;
    }
    // Nothing stated is an implicit deny in IAM, and the documents in force on the role ARE
    // the authority on what it may do. Which of the two it was is what the sentence's closing
    // clause states, because they ask for different remedies.
    let reason = if decision == Decision::Denied {
        iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
    } else {
        iam::no_identity_policy_allows(action)
    };
    Some((
        NO_PERMISSIONS.to_string(),
        not_authorized(
            &assumed_role_principal(role_arn, &role.role_name, attempt),
            action,
            resource,
            &reason,
        ),
    ))
}
