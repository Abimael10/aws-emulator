// SPDX-License-Identifier: CC0-1.0

//! The rest of a TEARDOWN: the resources a stack's deletion — or the rollback of a failed
//! create — has not asked about yet, because something that depends on them was still being
//! removed.
//!
//! ## The rule this module implements
//!
//! "CloudFormation deletes the resources in reverse order": a resource is removed only once
//! the resources that DEPEND on it are gone, because what depends on it is still using it.
//! A removal the owning service — or the PROVIDER of a custom resource — has only ACCEPTED
//! is not a resource that is gone: it is one still being removed, and the answer that says
//! it is gone arrives later ([`crate::state::stabilising`]). So the teardown pass leaves what
//! such a resource depends on alone ([`super::delete_pass`]) rather than asking about it while
//! the dependent is still there.
//!
//! That leaves work OWED, and this is where it is carried out: once a removal this teardown
//! was waiting for has answered and no wait of it is open any more, every resource the stack
//! still holds is asked about in a further pass, by the same walk the first pass went
//! through. The stack is not complete while it still holds one
//! ([`crate::state::StoredStack::owes_a_teardown`] is the one guard that says so), which is
//! what keeps a deletion from reporting itself finished with a resource it never deleted.
//!
//! ## One pass at a time
//!
//! A pass is OWED by the answers that have landed, and it CLAIMS them: the removal waits it
//! acts on are taken off the stack under the same lock that reads them, so a second pass —
//! the one a client's read drives beside the one a landed answer started, or the one a
//! re-entrant read from inside this pass's own calls would drive — finds nothing owed and
//! asks nobody twice. The first walk of a teardown holds no such wait at all, so a pass
//! never runs beside it.
//!
//! ## Bounds
//!
//! Each pass asks about at least one resource the stack still holds, or there is nothing left
//! to ask about, so the passes one teardown costs are bounded by the resources a stack may
//! hold ([`crate::state::template::MAX_RESOURCES_PER_TEMPLATE`]) — the same walk a
//! single-pass teardown makes. A pass whose calls a service refuses ends the teardown in its
//! own `*_FAILED` status, which owes nothing further.

use super::*;

/// What one stack's teardown still owes: the walk to re-ask, and the status a refusal leaves
/// the stack in.
struct Outstanding {
    stack_id: String,
    /// The resources this pass LEAVES ALONE: the ones the same teardown has already been
    /// refused and everything those need ([`Teardown::left_alone`]).
    left_alone: BTreeSet<String>,
    /// The stack's resource records, which is the set the walk reports back
    /// ([`crate::state::StoredStack::record_materialised_resources`] installs the set, not a
    /// delta).
    resources: Vec<StoredResource>,
    /// The resources still to take down — the ones the stack HOLDS in a service that owns
    /// them. A record whose teardown an earlier pass already reported is not in it, so
    /// nothing is reported twice.
    held: BTreeSet<String>,
    needs: Dependencies,
    policies: Policies,
    on_refusal: OnRefusal,
    /// The status a resource no service would delete leaves the stack in: the failed status
    /// of the teardown in flight.
    failed_status: &'static str,
}

/// Ask about the resources the teardowns in flight on the stacks of `region` still hold.
///
/// Claimed and read under one lock, performed with it released and recorded under a second,
/// exactly as [`super::advance_stabilisations_in`] — which drives this — performs every other
/// call a stack's own progress owes.
pub(super) async fn settle_outstanding_teardowns(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
) {
    let outstanding: Vec<Outstanding> = {
        let mut store = state.lock();
        store
            .stacks
            .iter_mut()
            .filter(|stack| stack.region == region)
            .filter_map(claimed_teardown)
            .collect()
    };
    for owed in outstanding {
        // BOXED, for the reason the walk's own provider calls are: a pass asks the services
        // that own the resources, so its whole state is held off this function's future
        // rather than on the stack of the read that drove it.
        let deleted = Box::pin(delete(
            ctx,
            region,
            &owed.resources,
            &Teardown {
                stack_id: &owed.stack_id,
                held: &owed.held,
                needs: &owed.needs,
                on_refusal: owed.on_refusal,
                policies: owed.policies,
                left_alone: &owed.left_alone,
            },
        ))
        .await;
        let now = SystemTime::now();
        let mut store = state.lock();
        let Some(stack) = store.resolve_mut(region, &owed.stack_id) else {
            continue;
        };
        // A resource whose owning service accepted the delete and still holds it holds this
        // teardown open in turn, exactly as the one this pass was waiting for did —
        // recorded BEFORE the walk's events, as every other teardown records them.
        stack.record_stabilisations(&deleted.removing, now);
        stack.schedule_resource_events(&deleted.transitions);
        stack.record_materialised_resources(deleted.resources);
        let concluded = SystemTime::now();
        if let Some(failure) = deleted.failure {
            // A resource no owning service would delete is the teardown's own failure, and
            // each resource's record takes the status this pass left it in — exactly as the
            // first walk of this teardown reports them. A refusal an EARLIER pass of the same
            // teardown reported is part of that failure too: the stack names every resource
            // it could not delete, not only the ones the last pass asked about.
            stack.record_resource_deletion_statuses(&deleted.transitions, concluded);
            let failure = match stack.teardown_refusals.is_empty() {
                true => failure,
                false => super::super::resource_failure_summary(
                    super::super::failed_action::DELETE,
                    stack
                        .teardown_refusals
                        .iter()
                        .chain(deleted.refused.iter())
                        .map(String::as_str),
                ),
            };
            stack.fail_transition(concluded, owed.failed_status, failure);
        } else if !stack.teardown_refusals.is_empty() {
            // The failure this teardown is HEADED for was held until the resources it owed
            // had been asked about ([`super::super::StoredStack::hold_teardown_failure`]):
            // this pass asked about them, so each takes the status it left them in and the
            // operation settles into that failed status with the reason it was held with.
            stack.record_resource_deletion_statuses(&deleted.transitions, concluded);
        }
    }
}

/// What `stack`'s teardown owes NOW, with the waits that owe it claimed off the stack; `None`
/// for a stack that owes nothing.
///
/// Four things have to hold: the stack is in a status in which it is taking EVERY resource it
/// holds down, a removal its teardown was waiting for has ANSWERED, no wait of that operation
/// is open any more (what the held-back resources were waiting for), and it still holds a
/// resource in a service that owns one.
fn claimed_teardown(stack: &mut super::super::StoredStack) -> Option<Outstanding> {
    let (policies, on_refusal, failed_status) = teardown_in_flight(stack)?;
    let left_alone = left_alone(stack, &stack.teardown_refusals);
    if stack.waiting_for_stabilisation() {
        return None;
    }
    // The removals this teardown was waiting for that have answered: what makes this pass
    // DUE, and what it claims — once taken off the stack, a second pass finds nothing owed
    // and asks nobody twice.
    if !stack
        .stabilisations
        .iter()
        .any(|barrier| barrier.awaited == super::super::stabilising::Awaited::Gone)
    {
        return None;
    }
    let held = still_held(stack);
    if held.is_empty() {
        return None;
    }
    stack
        .stabilisations
        .retain(|barrier| barrier.awaited != super::super::stabilising::Awaited::Gone);
    Some(Outstanding {
        stack_id: stack.stack_id.clone(),
        left_alone,
        resources: stack.current_resources().to_vec(),
        held,
        needs: dependencies_in_flight(stack),
        policies,
        on_refusal,
        failed_status,
    })
}

/// How a teardown of `stack` walks, for a stack whose teardown is in flight: the policies it
/// reads, what it does with a resource an owning service refuses, and the status such a
/// refusal leaves the stack in. `None` for a stack that is not taking down every resource it
/// holds.
///
/// The operation has to be SETTLING INTO the completed teardown, not merely to be in its
/// `*_IN_PROGRESS` status: a deletion CloudFormation CANCELLED is headed back to the healthy
/// status the stack held and takes nothing down at all, so it owes no resource a call.
fn teardown_in_flight(
    stack: &super::super::StoredStack,
) -> Option<(Policies, OnRefusal, &'static str)> {
    let settling_into = stack.pending.as_ref()?.status.as_str();
    // A teardown headed for its own FAILED status is one whose failure is HELD: the walk was
    // refused a resource and still owes the ones an unfinished removal held back
    // ([`super::super::StoredStack::hold_teardown_failure`]), which is what those refusals
    // record. Any other pending transition redirected to a failure is not a teardown with
    // work left to do.
    let held_failure = !stack.teardown_refusals.is_empty();
    match (stack.status.as_str(), settling_into) {
        // The rollback of a failed CREATE removes what that create had made, under the
        // policies the operation asked for — the same walk the failure's own path ran.
        (
            super::super::status::ROLLBACK_IN_PROGRESS,
            super::super::status::ROLLBACK_COMPLETE,
        ) => Some((
            stack.rolled_back_resource_policies(),
            OnRefusal::Stop,
            super::super::status::ROLLBACK_FAILED,
        )),
        (super::super::status::ROLLBACK_IN_PROGRESS, super::super::status::ROLLBACK_FAILED)
            if held_failure =>
        {
            Some((
                stack.rolled_back_resource_policies(),
                OnRefusal::Stop,
                super::super::status::ROLLBACK_FAILED,
            ))
        }
        // The stack's own deletion removes every resource it holds, under each resource's
        // `DeletionPolicy` and the `DeletionMode` the request chose.
        (
            super::super::status::DELETE_IN_PROGRESS,
            super::super::status::DELETE_COMPLETE,
        ) => Some((
            Policies::Honoured,
            super::super::on_refused_resource_deletion(stack.deletion_mode.as_deref()),
            super::super::status::DELETE_FAILED,
        )),
        (super::super::status::DELETE_IN_PROGRESS, super::super::status::DELETE_FAILED)
            if held_failure =>
        {
            Some((
                Policies::Honoured,
                super::super::on_refused_resource_deletion(stack.deletion_mode.as_deref()),
                super::super::status::DELETE_FAILED,
            ))
        }
        _ => None,
    }
}

/// The resources `stack` still HOLDS in a service that owns them — what a teardown of it has
/// left to ask about.
///
/// A resource the same teardown has already been refused is not among them, and neither is
/// one such a resource needs: nothing is asked about either again ([`left_alone`]), so a
/// teardown whose only resources left are those owes no further pass and settles into the
/// failed status it is headed for.
fn still_held(stack: &super::super::StoredStack) -> BTreeSet<String> {
    still_held_past(stack, &stack.teardown_refusals)
}

/// [`still_held`] for a teardown that has just been refused `refusals` — the resources it
/// would still have to ask about once those refusals are left alone.
fn still_held_past(
    stack: &super::super::StoredStack,
    refusals: &BTreeSet<String>,
) -> BTreeSet<String> {
    let left_alone = left_alone(stack, refusals);
    let holding = stack.held_resource_ids();
    stack
        .current_resources()
        .iter()
        .filter(|resource| {
            resource.materialised
                && holding.contains(&resource.logical_id)
                && !left_alone.contains(&resource.logical_id)
        })
        .map(|resource| resource.logical_id.clone())
        .collect()
}

/// What a teardown refused `refusals` leaves alone: those resources, and everything they
/// need ([`needed_by`], over the relation the teardown in flight is judged against).
///
/// A resource whose deletion an owning service refused is still there, so whatever it depends
/// on is still in use by it; and the refusal itself has been reported on it already.
fn left_alone(
    stack: &super::super::StoredStack,
    refusals: &BTreeSet<String>,
) -> BTreeSet<String> {
    if refusals.is_empty() {
        return BTreeSet::new();
    }
    let needs = dependencies_in_flight(stack);
    refusals
        .iter()
        .flat_map(|logical_id| needed_by(&needs, logical_id))
        .chain(refusals.iter().cloned())
        .collect()
}

impl super::super::StoredStack {
    /// Whether the teardown in flight on this stack still holds a resource in a service that
    /// owns it — which is what [`Self::settle`] will not complete an operation over.
    ///
    /// A deletion is not finished while a resource it is responsible for is still held there:
    /// the pass that asks about it is [`settle_outstanding_teardowns`], which only a handler
    /// can run, so the clock owes nothing until it has.
    pub fn owes_a_teardown(&self) -> bool {
        teardown_in_flight(self).is_some() && !still_held(self).is_empty()
    }

    /// Whether the teardown in flight still owes a call about a resource that is NOT one of
    /// `refusals`, nor one of those needs — which is what
    /// [`Self::hold_teardown_failure`] holds a teardown's failure for.
    pub(crate) fn teardown_owes_resources_past(&self, refusals: &BTreeSet<String>) -> bool {
        !still_held_past(self, refusals).is_empty()
    }
}
