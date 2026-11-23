// SPDX-License-Identifier: CC0-1.0

//! The resources of ONE LEVEL of a stack's dependency relation, asked about TOGETHER.
//!
//! "A resource that depends on another is scheduled only once that one has finished" is the
//! whole of the ordering a stack operation follows, so the resources no declaration of the
//! template relates to one another are brought up at the same time: a stack operation costs
//! about one coming-up window per LEVEL of that relation, however many resources the level
//! declares.
//!
//! [`super::reconcile`] is what holds to that. Its walk reaches the declarations in an order
//! that puts a resource behind everything it reads, and for each one it asks the owning
//! service to create the resource and then whether that service reports the resource ready
//! ([`super::super::provisioner::Provisioner::readiness`]). The second question is the one
//! this module moves: a service that answers it only once the resource has left its own
//! creation window answers it for as long as that window has left to run, so asking it
//! BEFORE the walk reaches the next declaration is what would make a level of several
//! resources cost one window EACH. So the walk records the resource here instead
//! ([`ComingUp`]) and asks at the point where the answer is needed — when it reaches a
//! declaration that READS one of them, about the ones THAT declaration reads, and once more
//! about whatever is left when the walk is through ([`settle`]).
//!
//! Nothing about the ANSWER changes: a resource is reported created only once its owning
//! service reports it ready, one it reports still coming up is held at a wait of its own
//! ([`super::super::stabilising`]) exactly as before, and one it reports failed fails the
//! resource with that service's own reason. What changes is only WHEN the question is put,
//! and so how long a level of independent resources takes: the windows of the level run
//! together rather than one after another.

use ::std::collections::BTreeSet;

use ::serde_json::Value;

use super::{
    handler_failure_reason, merge_published_attributes, owner_for, provider_failure_reason,
    Materialised, Readiness, Reporting, ResourceTransition, Stabilising,
};

/// One resource whose create the owning service ACCEPTED, and which the walk has not yet
/// asked that service about.
///
/// Everything the readiness question needs, taken as the create answered: which resource it
/// is, the identifier that service addresses it by, the declaration an arm that waits for a
/// declared SETTING judges itself against, and how this walk reports the resource's
/// transitions ([`Reporting`]).
#[derive(Debug, Clone)]
pub(super) struct ComingUp {
    pub(super) logical_id: String,
    pub(super) resource_type: String,
    pub(super) physical_resource_id: String,
    pub(super) declared: Value,
    pub(super) reporting: Reporting,
}

/// Ask the owning services about the resources the walk has started and not asked about yet,
/// and record what each of them answered.
///
/// `wanted` is WHICH of them the answer is needed for: the set a declaration in hand reads,
/// so a resource it does not read is left coming up and the declaration is not held behind a
/// window it has nothing to do with. `None` asks about all of them, which is what the end of
/// the walk needs — no resource may be reported created without its owning service having
/// said it is ready.
///
/// This is the whole of what [`super::reconcile`]'s create path used to do inline for one
/// resource, over a SET of them: the arms are the same answers, worded and recorded the
/// same way, so a resource reaches exactly the status its owning service's answer gives it.
/// A resource asked about is taken out of the set, so it is asked about once.
pub(super) async fn settle(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    coming_up: &mut Vec<ComingUp>,
    wanted: Option<&BTreeSet<String>>,
    outcome: &mut Materialised,
    failed: &mut Vec<(String, &'static str)>,
    unpublished: &mut BTreeSet<String>,
) {
    let (asked, left): (Vec<ComingUp>, Vec<ComingUp>) = ::std::mem::take(coming_up)
        .into_iter()
        .partition(|pending| match wanted {
            Some(wanted) => wanted.contains(&pending.logical_id),
            None => true,
        });
    *coming_up = left;
    for pending in asked {
        let owner = owner_for(&pending.resource_type);
        let readiness = match owner {
            // BOXED, like every other call this walk makes: the read is another service's
            // whole handler.
            Some(provisioner) => {
                Box::pin(provisioner.readiness(
                    ctx,
                    region,
                    &pending.physical_resource_id,
                    Some(&pending.declared),
                ))
                .await
            }
            None => Ok(Readiness::Ready),
        };
        // The record the walk pushed for the resource when its create answered, which is
        // what the transition below reports and what the published values are merged into.
        let Some(at) = outcome
            .resources
            .iter()
            .position(|recorded| recorded.logical_id == pending.logical_id)
        else {
            continue;
        };
        match readiness {
            Ok(Readiness::Ready) => {
                // That service reports the resource up, so the identifiers it publishes only
                // for a resource that IS up — the address a cluster serves on, the endpoint a
                // stream is written to, the outputs a nested stack's child published — are
                // read now: a declaration reading one of them resolves against this record,
                // and the pass that releases a readiness wait reads them for the same reason
                // ([`super::advance_stabilisations_in`]).
                Box::pin(merge_published_attributes(
                    ctx,
                    region,
                    &mut outcome.resources[at],
                ))
                .await;
                let finished = ResourceTransition::finished(
                    &outcome.resources[at],
                    pending.reporting.completed,
                    None,
                );
                outcome.transitions.push(finished);
                unpublished.remove(&pending.logical_id);
            }
            Ok(Readiness::Pending(state)) => {
                // The terminal transition is planned here as it is for any other resource;
                // the gate this records is what holds it back until the owning service
                // reports the resource ready. The resource stays UNPUBLISHED: it has not
                // published the values that service only reports once it IS up, so whatever
                // reads one of them is held behind it.
                outcome.stabilising.push(Stabilising {
                    awaited: super::super::stabilising::Awaited::Ready,
                    logical_id: pending.logical_id.clone(),
                    resource_type: pending.resource_type.clone(),
                    physical_resource_id: pending.physical_resource_id.clone(),
                    reported_state: state,
                    // The wait is the OWNING SERVICE's answer about the resource, so there is
                    // no provider request it is for.
                    provider_request: None,
                });
                let finished = ResourceTransition::finished(
                    &outcome.resources[at],
                    pending.reporting.completed,
                    None,
                );
                outcome.transitions.push(finished);
            }
            // The resource exists in that service and is not going to come up, or that
            // service could not be asked about it at all: either way the create did not go
            // through, so the resource FAILED with that service's own reason. The record
            // keeps `materialised`, so the rollback still deletes what the create made, and
            // the resources that read it are left alone by the walk
            // ([`super::depends_on_a_failure`]).
            Ok(ref failure @ (Readiness::Failed(_) | Readiness::Refused(_))) => {
                failed.push((pending.logical_id.clone(), pending.reporting.action));
                let reason =
                    provider_failure_reason(owner, &pending.physical_resource_id, failure);
                let finished = ResourceTransition::finished(
                    &outcome.resources[at],
                    pending.reporting.failed,
                    Some(reason),
                );
                outcome.transitions.push(finished);
                unpublished.remove(&pending.logical_id);
            }
            Err(refusal) => {
                failed.push((pending.logical_id.clone(), pending.reporting.action));
                let reason =
                    handler_failure_reason(owner, &refusal, &pending.physical_resource_id);
                let finished = ResourceTransition::finished(
                    &outcome.resources[at],
                    pending.reporting.failed,
                    Some(reason),
                );
                outcome.transitions.push(finished);
                unpublished.remove(&pending.logical_id);
            }
        }
    }
}
