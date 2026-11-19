// SPDX-License-Identifier: CC0-1.0

//! What refuses a stack UPDATE before it deploys anything: the verdicts CloudFormation
//! reaches from the PLAN alone, without asking any owning service for a thing.
//!
//! Two of them, and they are weighed in this order:
//!
//! * the stack POLICY in force, which says which update action may be performed on which of
//!   the stack's resources ([`super::stack_policy`]);
//! * a REPLACEMENT the plan requires that CloudFormation cannot carry out, because the
//!   resource it would replace is one the template names itself
//!   ([`super::custom_named_replacement`]).
//!
//! Both leave the stack in the same shape, which is why they are answered through one type:
//! the resource the verdict is about reports its own `UPDATE_FAILED` carrying what that
//! verdict says, the update deploys NOTHING — so the template, the parameters and every
//! resource stay exactly as they were — and the stack rolls back to the configuration still
//! in force, unless its caller asked for the failed state to be kept (`DisableRollback`).
//!
//! What the STACK reports it failed for differs, and that is the one thing each verdict
//! decides for itself ([`RefusedUpdate::stack_reason`]).
//!
//! Nothing here reads or writes the store: the operation that is deploying computes the plan,
//! asks for the verdict, and fails the update itself. Both operations that deploy an update
//! to a live stack — `UpdateStack` and `ExecuteChangeSet` — ask with the plan their own
//! request would carry out, so one rule decides them alike.

use super::change_plan::StoredResourceChange;
use super::provisioning::ResourceTransition;
use super::{custom_named_replacement, stack_policy, DeployedResource};

/// A verdict that refuses an update before anything is deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusedUpdate {
    /// The stack policy in force does not permit an action the update plans.
    Denied(stack_policy::Denial),
    /// The update requires replacing a resource whose name the template states itself.
    CannotReplaceACustomNamedResource(custom_named_replacement::RefusedReplacement),
}

impl RefusedUpdate {
    /// The verdict that refuses this plan, or `None` where neither does.
    ///
    /// `governing_policy` is the stack policy this update is held to, `None` where it is held
    /// to none; `planned` the resource-level plan the update would carry out
    /// ([`super::change_plan`]); and `deployed` the resources the stack has right now, which
    /// is what the names a replacement would collide with are read off.
    ///
    /// The policy is weighed FIRST: "updates on the resources you specify are denied", so a
    /// protected resource is one CloudFormation performs no action on at all — which it
    /// decides without having to work out what that action would have cost.
    pub fn of(
        governing_policy: Option<&str>,
        planned: &[StoredResourceChange],
        deployed: &[DeployedResource],
    ) -> Option<Self> {
        let denial = governing_policy.and_then(|policy| {
            stack_policy::denied(policy, &stack_policy::planned_actions(planned))
        });
        if let Some(denial) = denial {
            return Some(Self::Denied(denial));
        }
        custom_named_replacement::refused_replacement(planned, deployed)
            .map(Self::CannotReplaceACustomNamedResource)
    }

    /// The transitions the refused resource goes through: its own `UPDATE_FAILED` carrying
    /// this verdict, and — where the failed update is rolled back — the rollback skipping it,
    /// nothing having been done to it.
    pub fn transitions(&self, rolled_back: bool) -> Vec<ResourceTransition> {
        match self {
            Self::Denied(denial) => denial.transitions(rolled_back),
            Self::CannotReplaceACustomNamedResource(refused) => refused.transitions(rolled_back),
        }
    }

    /// What the STACK reports it failed for.
    ///
    /// For a POLICY denial it is the verdict itself: nothing was asked of any resource —
    /// CloudFormation denied the action by the policy in force — so the denial is the whole
    /// account of why the update did not happen.
    ///
    /// For a replacement it cannot carry out it is the summary sentence an update whose
    /// RESOURCES failed reports ([`super::resource_failure_summary`]): that resource's own
    /// update FAILED, and the stack-level field is the one a client parses to recover the set
    /// of failed logical ids, while the diagnosis belongs to the resource's own event.
    pub fn stack_reason(&self) -> String {
        match self {
            Self::Denied(denial) => denial.reason(),
            Self::CannotReplaceACustomNamedResource(refused) => super::resource_failure_summary(
                super::failed_action::UPDATE,
                [refused.logical_id.as_str()],
            ),
        }
    }
}
