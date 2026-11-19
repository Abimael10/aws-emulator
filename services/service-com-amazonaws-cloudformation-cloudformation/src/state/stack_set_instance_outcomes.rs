// SPDX-License-Identifier: CC0-1.0

//! What the STACKS of a deployment's stack instances make due for the StackSet operation
//! that deployed them.
//!
//! ## The rule this module implements
//!
//! A StackSet operation that creates or updates stack instances IS the deployment of those
//! instances' own stacks: "the operation completed creating or updating all the specified
//! stacks without exceeding the failure tolerance for the operation" is what `SUCCEEDED`
//! means, and "the operation exceeded the specified failure tolerance … if the number of
//! failed stacks within a Region exceeds the failure tolerance, the status of the operation
//! in the Region is set to `FAILED`. This in turn sets the status of the operation as a whole
//! to `FAILED`" (`StackSetOperation$Status`). Two things follow, and this module is both:
//!
//! * the operation is not CONCLUDED while a stack it is deploying is still working — its
//!   conclusion waits for theirs, so a deployment whose stack takes longer than one status
//!   transition (a resource-signal wait nothing signals) is still `RUNNING` while that stack
//!   is; and
//! * the operation's terminal status, and its
//!   `StatusDetails$FailedStackInstancesCount` — "the number of stack instances for which the
//!   StackSet operation failed" — are counted off what those stacks REACHED, per Region,
//!   against the tolerance the request asked for
//!   (`StackSetOperationPreferences$FailureToleranceCount` / `FailureTolerancePercentage`,
//!   "by default, 0 is specified").
//!
//! An instance whose stack did not carry the deployment through is reported as the failure it
//! is: "OUTDATED: The stack isn't currently up to date with the StackSet because the
//! associated stack failed during a `CreateStackSet` or `UpdateStackSet` operation", with
//! `StackInstanceStatus$DetailedStatus` `FAILED` and the `StatusReason` CloudFormation words
//! for a stack instance — the template resource that failed, its type and that resource's own
//! failure message ([`instance_failure_reason`]).
//!
//! ## Where it runs
//!
//! From the store's own settle ([`super::ComAmazonawsCloudformationCloudformationState::settle`]),
//! between the pass that settles the STACKS and the one that concludes the stack SETS: a
//! stack reaches its terminal status on the clock, so what that status owes the operation
//! deploying it was owed at the same instant even if nobody had called. It writes nothing
//! for an operation no deployment is in flight for, and nothing for an instance whose own
//! failure the handler that deployed it already recorded — each failure is reported once, by
//! whoever observed it.
//!
//! EVERY settle of the store runs it, and they do not all reach the same work: the rollback a
//! stack's expired resource-signal wait owes the services that own its resources is made from
//! the request path alone ([`super::provisioning::settle_expired_wait_failures`]), never from
//! under the store lock. So a stack still working is reported as still working whichever call
//! settled the store ([`progress_of`]), and the outcome a deployment concludes with is the
//! same for a reader, for a background conclusion ([`super::stack_set_events::settle_when_due`])
//! and for an event task — never the optimistic outcome the call that started it installed.
//!
//! ## Bounds
//!
//! One pass over the stack sets whose deployments are in flight, and one lookup per instance
//! those deployments act on: both are already bounded
//! ([`super::stack_sets::MAX_INSTANCES_PER_STACK_SET`]), and a store with no stack-set
//! deployment in flight — every store that holds no stack set, and every stack set between
//! operations — leaves after one pass over the stack sets it holds.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::stack_sets::{
    detailed_status, instance_status, operation_action, operation_status, StoredStackSet,
    StoredStackSetOperation,
};
use super::{
    operation_carried_through, ComAmazonawsCloudformationCloudformationState, StoredStack,
    STATUS_TRANSITION_DELAY,
};

/// How far the stack of ONE stack instance has got with the deployment in flight.
#[derive(Debug, Clone)]
enum InstanceStack {
    /// Still working, with the EARLIEST instant its terminal status can fall due at — the
    /// later of the transition it is headed for and the resource-signal waits still holding
    /// it, and never an instant already past, since a stack still working has not reached
    /// anything yet ([`progress_of`]).
    SettlingAt(SystemTime),
    /// Still working, and what ends the wait is an owning service's own answer about a
    /// resource, which no read of this store can ask for
    /// ([`super::provisioning::advance_stabilisations`]): the clock owes nothing for it, so
    /// the operation's conclusion is not held on it.
    AwaitingService,
    /// Terminal, having made the change the deployment set out to make.
    CarriedThrough,
    /// Terminal WITHOUT having made it, with the reason the instance reports.
    Failed(String),
}

impl ComAmazonawsCloudformationCloudformationState {
    /// Carry what the stacks of the instances a stack-set deployment acts on have reached
    /// into that deployment: hold its conclusion while one of them is still working, and
    /// write the outcome they add up to once they are all terminal.
    pub(super) fn settle_stack_set_instance_outcomes(&mut self, now: SystemTime) {
        // The stacks the deployments now in flight are deploying, read FIRST: the pass below
        // mutates the stack sets, so what it reads about the stacks is taken before it.
        let deploying: BTreeSet<String> = self
            .stack_sets
            .iter()
            .flat_map(|stack_set| {
                let settling = settling_deployments(stack_set);
                stack_set
                    .instances
                    .iter()
                    .filter(move |instance| settling.contains(&instance.last_operation_id))
                    .filter_map(|instance| instance.stack_id.clone())
            })
            .collect();
        if deploying.is_empty() {
            return;
        }
        let progress: BTreeMap<String, InstanceStack> = self
            .stacks
            .iter()
            .filter(|stack| deploying.contains(&stack.stack_id))
            .map(|stack| (stack.stack_id.clone(), progress_of(stack, now)))
            .collect();
        for stack_set in &mut self.stack_sets {
            apply_instance_outcomes(stack_set, &progress, now);
        }
    }
}

/// The `OperationId`s of the DEPLOYMENTS this stack set has in flight — the operations whose
/// conclusion the stacks of its instances decide.
///
/// Only a `CREATE` or an `UPDATE` deploys an instance's stack, so only those two are decided
/// here; a deletion and a drift detection conclude on their own terms. An operation the
/// caller has asked to STOP is concluding as the stop it was given
/// ([`crate::ops::stop_stack_set_operation`]), which is not an outcome of the stacks either.
fn settling_deployments(stack_set: &StoredStackSet) -> BTreeSet<String> {
    stack_set
        .operations
        .iter()
        .filter(|operation| is_settling_deployment(operation))
        .map(|operation| operation.operation_id.clone())
        .collect()
}

/// Whether `operation` is a deployment still in flight whose conclusion is the one these
/// stacks decide.
fn is_settling_deployment(operation: &StoredStackSetOperation) -> bool {
    (operation.action == operation_action::CREATE
        || operation.action == operation_action::UPDATE)
        && operation.status == operation_status::RUNNING
        && operation.pending.as_ref().is_some_and(|owed| {
            owed.status == operation_status::SUCCEEDED || owed.status == operation_status::FAILED
        })
}

/// How far one instance's stack has got.
fn progress_of(stack: &StoredStack, now: SystemTime) -> InstanceStack {
    let Some(pending) = stack.pending.as_ref() else {
        // Nothing is in flight on the stack, so what it is reporting is what its last
        // operation reached ([`operation_carried_through`]).
        return if operation_carried_through(&stack.status) {
            InstanceStack::CarriedThrough
        } else {
            InstanceStack::Failed(instance_failure_reason(stack))
        };
    };
    if !stack.stabilisations.is_empty()
        || stack.owed_rollback.is_some()
        || stack.unreverted_rollback.is_some()
    {
        return InstanceStack::AwaitingService;
    }
    // "CloudFormation doesn't proceed with a stack creation or update until resources receive
    // the required number of signals or the timeout period is exceeded" (`SignalResource`): a
    // wait still holding the stack puts its terminal status at the instant that wait gives up
    // rather than at the transition's own.
    let mut settles_at = pending.due_at;
    for barrier in &stack.wait_conditions {
        if !stack.barrier_satisfied(barrier) {
            settles_at = settles_at.max(barrier.deadline());
        }
    }
    // A stack still carrying the deployment whose own owed instant has ALREADY PASSED is one
    // the clock alone will not move: what its terminal status waits for is work only a
    // request can perform — the rollback a wait that RAN OUT owes the services that own the
    // stack's resources ([`super::provisioning::settle_expired_wait_failures`] makes those
    // calls, which cannot be made from under this lock). The deployment is still deploying
    // that stack either way, so its conclusion is HELD rather than read as the success
    // nothing reached: held to one transition span from here, the span the operation that
    // stack's own failure opens takes, and re-read at every settle until the stack is
    // terminal. Without this the conclusion would depend on WHICH call settles the store —
    // a request, which materialises that failure first, or a settle that cannot.
    //
    // So the hold lasts as long as the work it waits for is unreachable: until the next
    // request of the stack's own Region, which is the one call that performs it and then
    // leaves the stack with a terminal status or a fresh due instant of its own. The
    // deployment stays in the status it IS in meanwhile — reported RUNNING to every reader,
    // never concluded — which is what a conclusion nothing has reached means.
    if settles_at <= now {
        settles_at = now
            .checked_add(STATUS_TRANSITION_DELAY)
            .unwrap_or(settles_at);
    }
    InstanceStack::SettlingAt(settles_at)
}

/// The `StatusReason` CloudFormation reports on a stack instance whose own stack failed.
///
/// Worded where every path that fails an instance words it
/// ([`super::stack_set_instance_stacks`]), so the explanation a failure the clock found later
/// carries is the one the handler that deployed the instance would have carried: the template
/// resource that failed, its type and that resource's own failure message — or, for a stack
/// whose failure no resource carries, the stack's own `StackStatusReason`.
fn instance_failure_reason(stack: &StoredStack) -> String {
    super::stack_set_instance_stacks::failure_reason(stack).unwrap_or_default()
}

/// Write what the stacks of `stack_set`'s instances have reached onto the deployments in
/// flight over them.
fn apply_instance_outcomes(
    stack_set: &mut StoredStackSet,
    progress: &BTreeMap<String, InstanceStack>,
    now: SystemTime,
) {
    for operation_id in settling_deployments(stack_set) {
        let reached = reached_by(stack_set, &operation_id, progress);
        // Each instance whose stack failed is reported as the failure it is, as soon as that
        // stack reached it: "OUTDATED: The stack isn't currently up to date with the StackSet
        // because the associated stack failed during a CreateStackSet or UpdateStackSet
        // operation", with the deployment FAILED in that account and Region.
        for (account, region, reason) in &reached.failed {
            if let Some(instance) = stack_set
                .instances
                .iter_mut()
                .find(|held| held.is_in(account, region))
            {
                instance.status = instance_status::OUTDATED.to_string();
                instance.detailed_status = detailed_status::FAILED.to_string();
                instance.status_reason = Some(reason.clone());
            }
        }
        let Some(operation) = stack_set
            .operations
            .iter_mut()
            .find(|held| held.operation_id == operation_id)
        else {
            continue;
        };
        // A stack this deployment is still deploying holds its conclusion back: the
        // operation ends when the stacks do, so the instant it is owed at is the last of
        // theirs.
        if let Some(settling_at) = reached.settling_at {
            if let Some(owed) = operation.pending.as_mut() {
                if owed.due_at < settling_at {
                    owed.due_at = settling_at;
                }
            }
            continue;
        }
        // Every stack is terminal, so the outcome is theirs: FAILED where a Region's failures
        // exceeded the tolerance the request asked for, and the count is the instances the
        // operation failed in.
        let exceeded = reached.failed_by_region.iter().any(|(region, failed)| {
            *failed
                > tolerated(
                    operation,
                    reached.acted_on_by_region.get(region).copied().unwrap_or(0),
                )
        });
        // A conclusion the stacks have overtaken is due as soon as THEY finished: the wait
        // that held it open is over. Never sooner than the span this store's own operations
        // take ([`crate::state::STATUS_TRANSITION_DELAY`]), so a deployment whose stacks were
        // already settled when it started — one that ADOPTS existing stacks — is still in
        // flight for a caller that acts straight after the call.
        let not_before = operation
            .creation
            .checked_add(STATUS_TRANSITION_DELAY)
            .unwrap_or(operation.creation);
        let Some(owed) = operation.pending.as_mut() else {
            continue;
        };
        owed.status = if exceeded {
            operation_status::FAILED
        } else {
            operation_status::SUCCEEDED
        }
        .to_string();
        owed.failed_stack_instances_count =
            Some(i32::try_from(reached.failed.len()).unwrap_or(i32::MAX));
        owed.status_reason = reached.failed.first().map(|(_, _, reason)| reason.clone());
        owed.due_at = owed.due_at.min(now.max(not_before));
    }
}

/// What the stacks of the instances ONE deployment acts on have reached.
#[derive(Debug, Default)]
struct Reached {
    /// The last instant a stack still being deployed can reach its terminal status at, at the
    /// earliest ([`InstanceStack::SettlingAt`]), or `None` when every one of them is terminal.
    settling_at: Option<SystemTime>,
    /// One entry per instance whose stack failed: the account, the Region and the reason the
    /// instance reports.
    failed: Vec<(String, String, String)>,
    /// How many of those failed in each Region, which is where the tolerance is applied.
    failed_by_region: BTreeMap<String, usize>,
    /// How many instances the deployment acted on in each Region, which is what a tolerance
    /// stated as a PERCENTAGE is a percentage of.
    acted_on_by_region: BTreeMap<String, usize>,
}

/// Read what the stacks of the instances `operation_id` acted on have reached.
fn reached_by(
    stack_set: &StoredStackSet,
    operation_id: &str,
    progress: &BTreeMap<String, InstanceStack>,
) -> Reached {
    let mut reached = Reached::default();
    for instance in &stack_set.instances {
        if instance.last_operation_id != operation_id {
            continue;
        }
        // "StackSets will cancel all the unstarted stack instance deployments": a deployment
        // the caller's stop cancelled is not one of this operation's outcomes.
        if instance.detailed_status == detailed_status::CANCELLED {
            continue;
        }
        *reached
            .acted_on_by_region
            .entry(instance.region.clone())
            .or_default() += 1;
        // An instance no deployment produced a stack for has no stack to wait for and none
        // to read an outcome off.
        let Some(stack_id) = instance.stack_id.as_deref() else {
            continue;
        };
        match progress.get(stack_id) {
            Some(InstanceStack::SettlingAt(at)) => {
                reached.settling_at =
                    Some(reached.settling_at.map_or(*at, |held| held.max(*at)));
            }
            Some(InstanceStack::Failed(reason)) => {
                // A failure the handler that deployed this instance already recorded is
                // reported in ITS words: each failure is worded once, by whoever observed it.
                let recorded = instance
                    .status_reason
                    .clone()
                    .filter(|_| instance.detailed_status == detailed_status::FAILED);
                reached.failed.push((
                    instance.account.clone(),
                    instance.region.clone(),
                    recorded.unwrap_or_else(|| reason.clone()),
                ));
                *reached
                    .failed_by_region
                    .entry(instance.region.clone())
                    .or_default() += 1;
            }
            Some(InstanceStack::CarriedThrough) | Some(InstanceStack::AwaitingService)
            | None => {}
        }
    }
    reached
}

/// How many failures one Region of this operation tolerates before the operation is FAILED.
///
/// `FailureToleranceCount` is "the number of accounts per Region this operation can fail in
/// before CloudFormation stops the operation in that Region", and
/// `FailureTolerancePercentage` is the same number "per Region" as a percentage of the
/// accounts the operation acts on there — "when calculating the number of accounts based on
/// the specified percentage, CloudFormation rounds *down* to the next whole number". An
/// operation that asked for neither is held to the documented default of both: "by default,
/// 0 is specified".
fn tolerated(operation: &StoredStackSetOperation, acted_on: usize) -> usize {
    let Some(preferences) = operation.preferences.as_ref() else {
        return 0;
    };
    if let Some(count) = preferences.failure_tolerance_count {
        return usize::try_from(count).unwrap_or(0);
    }
    match preferences.failure_tolerance_percentage {
        Some(percentage) => {
            let percentage = usize::try_from(percentage.clamp(0, 100)).unwrap_or(0);
            acted_on.saturating_mul(percentage) / 100
        }
        None => 0,
    }
}
