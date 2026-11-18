// SPDX-License-Identifier: CC0-1.0

//! The second half of a deployment's walk: the resources it could not create YET.
//!
//! ## The rule this module implements
//!
//! CloudFormation creates a stack's resources in dependency order — "a resource that
//! depends on another is scheduled only once that one has finished" — and what a
//! declaration reads of the resource it depends on is that resource's PUBLISHED values: the
//! physical id a `Ref` resolves to, and the attributes an `Fn::GetAtt` resolves against. A
//! resource whose create the owning service has ACCEPTED has not finished while that service
//! is still bringing it up, and the values it only publishes once it is up — the address a
//! cluster serves on, the endpoint a stream is written to — are not there to be read yet. The
//! same relation holds for a CUSTOM RESOURCE, whose attributes are the `Data` its provider
//! answers with ([`super::custom_resource`]), which arrives after the create was sent. Either
//! way a resource whose declaration reads one of those values cannot be created in the same
//! pass as the resource it reads: the document that would be sent does not carry the value
//! yet.
//!
//! So the walk WITHHOLDS such a resource ([`super::awaits_a_dependency`]) and holds it at a
//! wait of its own ([`crate::state::stabilising::Awaited::Deferred`]), which keeps the stack
//! operation open exactly as the awaited resource's own wait does. [`resume`] is the pass
//! that finishes the walk: once the waits a withheld resource was waiting for have released,
//! the declarations are resolved ONCE MORE against the records as they now stand
//! ([`crate::state::StoredStack::resolve_deferred_declarations`]) and the resource is created
//! with the document that resolution produced.
//!
//! A declaration that STILL reads an attribute the resource it names does not publish has
//! read one that resource does not have: the answer is in hand — the owning service reports
//! it ready, or the provider's `Data` has arrived — so the attribute does not exist, and the
//! consuming resource fails with CloudFormation's attribute error
//! ([`crate::state::template::unsupported_attribute_reason`]) — which rolls the stack back,
//! as any resource's failed create does.
//!
//! A withheld resource's own dependents are withheld behind IT, so one pass releases ONE
//! level of a chain. [`resume`] therefore keeps passing while a pass still creates something:
//! what a level's creation unblocks is the next level, and holding that over to the next call
//! would make the rest of the walk wait on how often the client happens to read the stack —
//! and spend a withheld resource's own window ([`crate::state::stabilising`]) waiting for its
//! turn rather than for the service that owns it.
//!
//! ## Bounds
//!
//! Every pass creates at least one withheld resource or the cascade stops, and a resource is
//! created at most once, because creating it releases its wait — so the passes of one call are
//! bounded by the resources a template may declare
//! ([`crate::state::template::MAX_RESOURCES_PER_TEMPLATE`]), which is the same walk a
//! single-pass deployment makes. The waits themselves are bounded with the operation that
//! opened them ([`crate::state::stabilising`]), and a WITHHELD one is bounded by the wait it
//! is waiting FOR: while the resources its declaration reads are still in flight, its own
//! window is not checked ([`resumption_of`]), because nothing has been asked of any owning
//! service for it — so the chain is bounded by one window per level, and a level nothing will
//! ever unblock ends when the wait above it runs out on the same window every other wait of a
//! creation does.

use super::*;

/// What one stack's resumable work is: the resources to create now, the waits that creating
/// them ends, and the waits that are over because the resource cannot be created at all.
struct Resumption {
    stack_id: String,
    provenance: StackProvenance,
    /// The resources to create, in the order the deployment declares them, with their
    /// declarations resolved against the records now in force.
    create: Vec<StoredResource>,
    /// The waits creating those resources ends.
    resumed: Vec<super::super::stabilising::StabilisationBarrier>,
    /// The waits that are over because the resource they hold cannot be created: the
    /// declaration reads an attribute nothing publishes, or the wait ran out.
    failed: Vec<(super::super::stabilising::StabilisationBarrier, String)>,
    /// The records the stack ALREADY has for the resources this pass acts on: what a
    /// withheld resource the stack already holds is reconciled against, so a declaration
    /// whose value has now landed is APPLIED to the resource that exists rather than created
    /// a second time ([`reconcile`] reads the same relation for a deployment's own walk).
    /// Only those resources, because what a walk's `previous` set does not re-declare it
    /// removes.
    deployed: Vec<StoredResource>,
}

/// Create the resources the deployments in flight on the stacks of `region` WITHHELD, now
/// that what they were waiting for has answered.
///
/// Read under one lock, performed with it released and recorded under a second, exactly as
/// [`super::advance_stabilisations`] — which drives this — performs every other call a
/// stack's own progress owes.
///
/// `scope` is the pass's own ([`super::progress_probe::Scope`]): a read this service made
/// while provisioning resumes the stack it is reading, and a client's call every stack of the
/// Region.
pub(super) async fn resume(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
    scope: &super::progress_probe::Scope,
) {
    // One pass per LEVEL of a withheld chain, for as long as a pass still creates something:
    // see the module docs for why the cascade is not left to the next call, and for what
    // bounds it.
    for _ in 0..super::super::template::MAX_RESOURCES_PER_TEMPLATE {
        // BOXED, for the reason the walk's own provider calls are ([`reconcile`]): a pass runs
        // the whole deployment walk, so its state is held off this function's future rather
        // than on the stack of the read that resumed it.
        if !Box::pin(resume_pass(ctx, state, region, scope)).await {
            return;
        }
    }
}

/// ONE pass of [`resume`]: create the resources whose waits have answered, and answer whether
/// it created any — which is whether another level may now be creatable.
async fn resume_pass(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
    scope: &super::progress_probe::Scope,
) -> bool {
    let resumptions: Vec<Resumption> = {
        let mut store = state.lock();
        let now = SystemTime::now();
        store
            .stacks
            .iter_mut()
            .filter(|stack| stack.region == region && scope.covers(stack))
            .filter_map(|stack| resumption_of(stack, now))
            .collect()
    };
    let progressed = resumptions
        .iter()
        .any(|resumption| !resumption.create.is_empty());
    for resumption in resumptions {
        let materialised = Box::pin(reconcile(
            ctx,
            region,
            &resumption.provenance,
            resumption.create,
            &resumption.deployed,
            None,
            Policies::Honoured,
        ))
        .await;
        let now = SystemTime::now();
        let unwinding = {
            let mut store = state.lock();
            let Some(stack) = store.resolve_mut(region, &resumption.stack_id) else {
                continue;
            };
            stack.schedule_resource_events(&materialised.transitions);
            let records = stack.current_resources().to_vec();
            let merged = merged_records(&records, materialised.resources);
            stack.record_materialised_resources(merged);
            // A resource this pass created whose owning service does not report it ready
            // yet — a custom resource withheld behind another one — holds the operation
            // open in turn.
            stack.record_stabilisations(&materialised.stabilising, now);
            let mut taken: Option<Vec<StoredResource>> = None;
            for barrier in &resumption.resumed {
                match failure_of(&materialised.transitions, &barrier.logical_id) {
                    // The create this pass made for the resource failed, so the wait it was
                    // held at is over with that failure — reported on the resource exactly
                    // as a failure of the first pass is.
                    Some(reason) => {
                        taken = taken
                            .or_else(|| stack.fail_stabilisation(now, barrier, &reason));
                    }
                    None => stack.release_stabilisation(barrier, now),
                }
            }
            for (barrier, reason) in &resumption.failed {
                taken = taken.or_else(|| stack.fail_stabilisation(now, barrier, reason));
            }
            taken.map(|resources| {
                (
                    resources,
                    stack.held_resource_ids(),
                    dependencies_in_flight(stack),
                    stack.rolled_back_resource_policies(),
                )
            })
        };
        // "ROLLBACK — Roll back the stack creation. This is the default": the resources the
        // failed create had already made are removed again in the services that own them,
        // the same work every other failure of a create owes.
        let Some((resources, held, needs, policies)) = unwinding else {
            continue;
        };
        let deleted = unwind(
            ctx,
            region,
            &resources,
            &Teardown {
                stack_id: &resumption.stack_id,
                held: &held,
                needs: &needs,
                on_refusal: OnRefusal::Stop,
                policies,
                // The first walk of this rollback: nothing has been refused yet.
                left_alone: NOTHING_LEFT_ALONE,
            },
        )
        .await;
        let mut store = state.lock();
        let Some(stack) = store.resolve_mut(region, &resumption.stack_id) else {
            continue;
        };
        stack.schedule_resource_events(&deleted.transitions);
        stack.record_materialised_resources(deleted.resources);
        stack.record_stabilisations(&deleted.removing, SystemTime::now());
        if let Some(failure) = deleted.failure {
            // Reported once this teardown has asked about every resource it still can
            // ([`super::super::StoredStack::hold_teardown_failure`]).
            if !stack.hold_teardown_failure(
                &deleted.transitions,
                &deleted.removing,
                &deleted.refused,
                super::super::status::ROLLBACK_FAILED,
                &failure,
            ) {
                stack.fail_transition(
                    SystemTime::now(),
                    super::super::status::ROLLBACK_FAILED,
                    failure,
                );
            }
        }
    }
    progressed
}

/// What `stack` can resume now, or `None` for a stack with nothing withheld — or nothing
/// withheld whose waits have answered.
///
/// A withheld resource is resumable once NO wait of the operation holds any resource it
/// depends on: the records those declarations read are then the ones the owning services
/// published. The declarations are resolved again here, under the same lock the records are
/// read under, so the document each create carries is one reading of the stack.
fn resumption_of(
    stack: &mut super::super::StoredStack,
    now: SystemTime,
) -> Option<Resumption> {
    let withheld: Vec<super::super::stabilising::StabilisationBarrier> = stack
        .open_stabilisations()
        .into_iter()
        .filter(|barrier| barrier.awaited == super::super::stabilising::Awaited::Deferred)
        .collect();
    if withheld.is_empty() {
        return None;
    }
    // Every resource of this operation that is still held at a wait: what a withheld
    // resource may not be created ahead of.
    let waiting: BTreeSet<String> = stack
        .open_stabilisations()
        .iter()
        .map(|barrier| barrier.logical_id.clone())
        .collect();
    let dependencies = dependencies_in_flight(stack);
    let mut expired: Vec<(super::super::stabilising::StabilisationBarrier, String)> = Vec::new();
    let mut resumable: Vec<super::super::stabilising::StabilisationBarrier> = Vec::new();
    for barrier in withheld {
        // A resource that is still waiting for what its declaration READS has had nothing
        // asked of anybody for it — no create was made, so there is no answer whose window
        // could run out ([`super::advance_stabilisations_in`] puts no question for a
        // `Deferred` wait). What bounds it is the wait held for the resource it is waiting
        // FOR, which has a window of its own and fails the whole operation when that runs
        // out. So its turn is not charged to it: a chain of withheld resources is bounded by
        // one window PER LEVEL, exactly as the levels of a nested deployment are, rather than
        // by one window for the whole chain — which would fail the deepest level for having
        // waited for the levels above it.
        if awaits_a_dependency(&barrier.logical_id, &dependencies, &waiting) {
            continue;
        }
        if barrier.has_expired(now) {
            let reason = barrier.not_stabilised_reason(&barrier.reported_state);
            expired.push((barrier, reason));
            continue;
        }
        // A withheld resource ANOTHER PASS is already creating is not this one's to create:
        // the two passes would send its provider two requests, and the stack would hold a
        // wait nothing answers ([`crate::state::stabilising::StabilisationBarrier::resumed_at`]).
        if barrier.resumed_at.is_some() {
            continue;
        }
        resumable.push(barrier);
    }
    if resumable.is_empty() && expired.is_empty() {
        return None;
    }
    // The declarations, resolved once more against the records now in force: what each
    // create below is asked for, and which of them reads an attribute nothing publishes.
    let unresolved = stack.resolve_deferred_declarations();
    let mut failed = expired;
    let mut resumed = Vec::new();
    let mut create = Vec::new();
    let records = stack.current_resources().to_vec();
    for barrier in resumable {
        match unresolved.get(&barrier.logical_id) {
            Some(reason) => failed.push((barrier, reason.clone())),
            None => {
                if let Some(record) = records
                    .iter()
                    .find(|record| record.logical_id == barrier.logical_id)
                {
                    create.push(record.clone());
                }
                resumed.push(barrier);
            }
        }
    }
    // ...and the records the stack ALREADY holds for exactly those resources: a withheld
    // resource the stack has is one whose declaration this pass APPLIES to it, which is what
    // the walk reads its previous record for. Nothing else is in the set: what a walk's
    // previous set does not re-declare, it removes.
    let deployed: Vec<StoredResource> = stack
        .resources
        .iter()
        .map(|held| held.resource.clone())
        .filter(|held| {
            resumed
                .iter()
                .any(|barrier| barrier.logical_id == held.logical_id)
        })
        .collect();
    // This pass's creations are CLAIMED while the lock is still held, so a pass running
    // beside it leaves them alone — the one place that mark is set, beside the one place it
    // is read above.
    for barrier in &resumed {
        if let Some(held) = stack
            .stabilisations
            .iter_mut()
            .find(|held| held.is_the_same_wait(barrier) && !held.released())
        {
            held.resumed_at = Some(now);
        }
    }
    Some(Resumption {
        stack_id: stack.stack_id.clone(),
        provenance: super::StackProvenance {
            // What is STILL unresolved after the re-resolution just above: a resource whose
            // read has landed is created by this pass, and one whose dependency has not
            // published it yet is withheld again rather than created without the member that
            // reads it ([`super::StackProvenance::reads_unpublished_attributes`]).
            reads_unpublished_attributes: stack
                .declarations_reading_unpublished_attributes(),
            ..stack.provenance()
        },
        create,
        resumed,
        failed,
        deployed,
    })
}

/// `records` with every entry this pass acted on replaced by what it left, which is the
/// whole set the stack records ([`crate::state::StoredStack::record_materialised_resources`]
/// installs the set, not a delta).
fn merged_records(
    records: &[StoredResource],
    materialised: Vec<StoredResource>,
) -> Vec<StoredResource> {
    let mut merged = records.to_vec();
    for resource in materialised {
        match merged
            .iter_mut()
            .find(|held| held.logical_id == resource.logical_id)
        {
            Some(held) => *held = resource,
            None => merged.push(resource),
        }
    }
    merged
}

/// The reason the transition of `logical_id` in `transitions` FAILED, for a pass that could
/// not create it; `None` where the resource's transitions report no failure.
fn failure_of(transitions: &[ResourceTransition], logical_id: &str) -> Option<String> {
    transitions
        .iter()
        .filter(|transition| transition.logical_id == logical_id)
        .find(|transition| transition.status.ends_with(FAILED_SUFFIX))
        .map(|transition| {
            transition
                .reason
                .clone()
                .unwrap_or_else(|| transition.status.clone())
        })
}

/// The suffix every failed `ResourceStatus` ends in ([`crate::state::resource_status`]).
const FAILED_SUFFIX: &str = "_FAILED";
