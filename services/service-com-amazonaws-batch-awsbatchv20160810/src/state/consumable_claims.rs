// SPDX-License-Identifier: CC0-1.0

//! The CONSUMABLE RESOURCES a job holds while it runs, and the capacity they bound.
//!
//! # The rule this module implements
//!
//! A consumable resource is a quantity jobs take and give back:
//! `ConsumableResourceSummary$totalQuantity` is "the total amount of the consumable resource
//! that is available", `inUseQuantity` is "the amount of the consumable resource that is
//! currently in use" and `availableQuantity` is what is left of the total once the quantity
//! in use is taken off ([`ConsumableResource::available_quantity`]). A job states what it
//! takes through `consumableResourceProperties.consumableResourceList[].quantity` ("the
//! quantity of the consumable resource that is needed"), so:
//!
//! * a job is ADMITTED onto compute capacity only while every resource it names has that
//!   much available — the quantity is a CONCURRENCY bound, so a claimant that does not fit
//!   waits in `RUNNABLE`, where a job waits to be placed, until one that holds the quantity
//!   gives it back ([`ComAmazonawsBatchAwsbatchv20160810State::consumable_claims_admit`]);
//! * the admitted job HOLDS what it claimed from that instant, which is what `inUseQuantity`
//!   reports while it runs;
//! * it holds it until it reaches a terminal `JobStatus` — whatever took it there — because
//!   from then on it can consume nothing more
//!   ([`ComAmazonawsBatchAwsbatchv20160810State::release_consumable_claims`], called from
//!   the one terminal path).
//!
//! What giving it back MEANS is the resource's own `resourceType`, which "indicates whether
//! the resource is available to be re-used after a job completes": a `REPLENISHABLE`
//! resource is, so its whole total is available again, while a `NON_REPLENISHABLE` one is
//! not — the quantity the job consumed is gone from the total as well as from what is in
//! use, so the resource ends up with that much less to give.
//!
//! Nothing here is invented: every quantity is the one the request stated, held against the
//! resource that request named, and the accounting is symmetric — one charge per admission,
//! one release per job, each recorded on the job itself ([`Job::consumable_claims_held`]) so
//! a repeated materialisation cannot charge twice and a release cannot give back what was
//! never taken.

use super::*;

/// One consumable-resource claim a job HOLDS: the resource, by the name this store keys it
/// by, and how much of it the job took.
///
/// Recorded on the job at the instant it was admitted, so the release gives back exactly what
/// the admission took even if the resource's own total has been updated in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumableClaim {
    /// The name of the consumable resource — what [`ConsumableResource`]s are keyed by, so a
    /// claim naming the resource by ARN and one naming it by name resolve to one record.
    pub resource_name: String,
    /// How much of it this job holds: the `quantity` its requirement stated.
    pub quantity: i64,
}

/// The detail `DeleteConsumableResource` refuses a resource whose quantity is in use with.
///
/// Fixed text with no variable part, as AWS is recorded to word it: the resource is still
/// backing jobs that hold its quantity, so the delete is refused and the resource stays.
pub const CONSUMABLE_RESOURCE_IN_USE_MESSAGE: &str =
    "Consumable resource cannot be deleted when in use";

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// What `id` WOULD take of the consumable resources its requirements name: one entry per
    /// resource, carrying the quantity its requirements add up to for that resource.
    ///
    /// A requirement naming nothing, one naming a resource this store does not hold (there is
    /// no quantity to account against it) and one asking for nothing at all take nothing, so
    /// none of them appears. Held in a [`BTreeMap`] while it is built, so two requirements
    /// naming ONE resource — by its name and by its ARN — are summed into the one claim
    /// against that resource rather than weighed separately against its total.
    ///
    /// Costs one walk of the job's own requirement list, which `RegisterJobDefinition` and
    /// `SubmitJob` bound.
    fn consumable_claims_of(&self, id: &str) -> Vec<ConsumableClaim> {
        let Some(job) = self.jobs.get(id) else {
            return Vec::new();
        };
        let mut claimed: BTreeMap<String, i64> = BTreeMap::new();
        let requirements = job
            .consumable_resource_properties
            .iter()
            .filter_map(|properties| properties.consumable_resource_list.as_deref())
            .flatten();
        for requirement in requirements {
            let Some(reference) = requirement.consumable_resource.as_deref() else {
                continue;
            };
            let Some(name) = self.consumable_resource_name(reference) else {
                continue;
            };
            let quantity = requirement.quantity.unwrap_or_default();
            if quantity <= 0 {
                continue;
            }
            let held = claimed.entry(name).or_default();
            *held = held.saturating_add(quantity);
        }
        claimed
            .into_iter()
            .map(|(resource_name, quantity)| ConsumableClaim {
                resource_name,
                quantity,
            })
            .collect()
    }

    /// Whether the consumable resources `id` names still have the quantity it asks for.
    ///
    /// The quantity is a concurrency bound, so each claim is weighed against what is
    /// AVAILABLE — the total less what the jobs already holding it took — and a job that does
    /// not fit is not admitted. A job that is already holding its claims fits by definition:
    /// it took them once and is not charged again.
    pub(super) fn consumable_claims_admit(&self, id: &str) -> bool {
        if self
            .jobs
            .get(id)
            .is_some_and(|job| !job.consumable_claims_held.is_empty())
        {
            return true;
        }
        self.consumable_claims_of(id).iter().all(|claim| {
            self.consumable_resources
                .get(&claim.resource_name)
                .is_none_or(|resource| resource.available_quantity() >= claim.quantity)
        })
    }

    /// Take what `id` claims of each consumable resource it names, and record on the job what
    /// it now holds.
    ///
    /// Called from the ONE place a job is admitted onto compute capacity, and idempotent: a
    /// job that already holds its claims is left exactly as it is, so a placement that is
    /// retried — a dispatch given up on and taken again — charges the resource once.
    pub(super) fn charge_consumable_claims(&mut self, id: &str) {
        if self
            .jobs
            .get(id)
            .is_none_or(|job| !job.consumable_claims_held.is_empty())
        {
            return;
        }
        let claims = self.consumable_claims_of(id);
        if claims.is_empty() {
            return;
        }
        for claim in &claims {
            if let Some(resource) = self.consumable_resources.get_mut(&claim.resource_name) {
                resource.in_use_quantity = resource.in_use_quantity.saturating_add(claim.quantity);
            }
        }
        if let Some(job) = self.jobs.get_mut(id) {
            job.consumable_claims_held = claims;
        }
    }

    /// Give back what `id` holds of each consumable resource, and answer whether it held
    /// anything at all.
    ///
    /// Called from the ONE terminal path ([`Self::finish_job`]), so every route into a
    /// terminal status releases the quantity the job held — the clock, `CancelJob`,
    /// `TerminateJob`, the attempt timeout, the deletion of its queue. A `REPLENISHABLE`
    /// resource is "available to be re-used after a job completes", so only its
    /// `inUseQuantity` goes back down; a `NON_REPLENISHABLE` one is not, so the quantity this
    /// job consumed leaves its `totalQuantity` as well and is never offered again.
    pub(super) fn release_consumable_claims(&mut self, id: &str) -> bool {
        let claims = match self.jobs.get_mut(id) {
            Some(job) if !job.consumable_claims_held.is_empty() => {
                ::std::mem::take(&mut job.consumable_claims_held)
            }
            _ => return false,
        };
        for claim in claims {
            if let Some(resource) = self.consumable_resources.get_mut(&claim.resource_name) {
                resource.in_use_quantity = resource
                    .in_use_quantity
                    .saturating_sub(claim.quantity)
                    .max(0);
                if resource.resource_type == RESOURCE_TYPE_NON_REPLENISHABLE {
                    resource.total_quantity =
                        resource.total_quantity.saturating_sub(claim.quantity).max(0);
                }
            }
        }
        true
    }
}
