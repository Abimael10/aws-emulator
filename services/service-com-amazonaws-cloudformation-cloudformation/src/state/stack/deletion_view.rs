// SPDX-License-Identifier: CC0-1.0

//! What a stack whose own DELETION is in flight reports for each of its resources, and what a
//! removal that FAILED leaves behind.
//!
//! ## The rule this module implements
//!
//! `StackResource$ResourceStatus` is the resource's "Current status", so what a resource of a
//! stack being deleted reports is the state of ITS OWN removal: `DELETE_IN_PROGRESS` once
//! CloudFormation has asked for that resource to be removed and until the removal is
//! concluded, and the status that removal concluded with afterwards. It is the deletion's
//! counterpart of what a create or an update reports while it is still in flight
//! ([`super::StoredStack::in_progress_status_of`]) — read while the operation is under way,
//! off the same records.
//!
//! A resource the deletion has NOT asked about yet keeps the status it carries, because
//! nothing has happened to that resource: "CloudFormation deletes the resources in reverse
//! order", so a resource is removed only once the resources that DEPEND on it are gone — and
//! the removal of a CUSTOM RESOURCE is not carried out until its provider answers at the
//! request's `ResponseURL` ([`crate::state::provisioning::custom_resource`]). While such a
//! removal is outstanding the resources behind it are left exactly as they are (the outstanding
//! pass of [`crate::state::provisioning::delete`]'s own walk is what asks about them once it
//! answers), and that is what they report. Recorded evidence: with a custom resource's
//! `Delete` outstanding at its provider, the custom resource reads `DELETE_IN_PROGRESS` while
//! the resource it DEPENDS ON — which the teardown is holding back behind it — still reads the
//! status its create left.
//!
//! So a record reports the removal under way only where this deletion has already asked about
//! it, which is where the stack no longer holds a resource for it to ask about: the teardown
//! walk stops recording a resource as materialised the moment its removal is accepted
//! ([`crate::state::provisioning::delete`]), so a record that still carries one is a resource
//! the teardown is keeping until the removals before it answer. (A record nothing ever
//! materialised — a declaration whose create failed, or one of a type no service of this
//! emulator provisions — has no resource to remove either way, and the teardown reports it
//! through the same pair.)
//!
//! Two records are left exactly as they stand besides:
//!
//! * one this deletion has already CONCLUDED — the resource an owning service or a provider
//!   refused to remove (`DELETE_FAILED`), and whatever a pass of the teardown recorded as it
//!   went ([`super::StoredStack::record_resource_deletion_statuses`]): its status was written
//!   at or after this deletion started, so it IS what became of the resource under this
//!   operation;
//! * one no deletion is responsible for any more, because an operation already took the
//!   resource away (`DELETE_COMPLETE`) or RELEASED it (`DELETE_SKIPPED`, a `Retain` policy or a
//!   `RetainResources` of this very request) — nothing is asked of the service that owns it, so
//!   there is no removal of it under way.
//!
//! Nothing in the reported view writes: it is computed from the records and the pending
//! transition, so a read of a stack being deleted changes nothing a later caller sees. The one
//! write here is what a removal's FAILURE leaves
//! ([`StoredStack::keeps_the_resource_whose_removal_failed`]).

use super::*;

impl StoredStack {
    /// Report `deployed` as a read of this stack reports it while the stack's own DELETION is
    /// in flight; the records are left unchanged for a stack that is not being deleted.
    pub(super) fn under_a_deletion_in_flight(&self, deployed: &mut [DeployedResource]) {
        let Some(started_at) = self.deletion_in_flight_since() else {
            return;
        };
        for record in deployed.iter_mut() {
            if !removal_under_way(record, started_at) {
                continue;
            }
            record.status = resource_status::DELETE_IN_PROGRESS.to_string();
            // A removal that is merely under way has nothing to report about the resource
            // beyond the status itself: the sentence belongs to the outcome — the refusal of
            // the service that owns it, or the reason a provider answered — and neither has
            // happened yet.
            record.status_reason = None;
            record.status_timestamp = started_at;
        }
    }

    /// The instant the deletion in flight on this stack STARTED, or `None` when no deletion
    /// of it is in flight.
    ///
    /// The stack has to be settling into a deletion's own terminal status, not merely to be in
    /// `DELETE_IN_PROGRESS`: a deletion CloudFormation CANCELLED reports that status and is
    /// headed back to the healthy status the stack held, with nothing of it disposed of
    /// ([`StoredStack::begin_canceled_deletion`]), so no resource of it is being deleted at
    /// all.
    fn deletion_in_flight_since(&self) -> Option<SystemTime> {
        if self.status != status::DELETE_IN_PROGRESS {
            return None;
        }
        let pending = self.pending.as_ref()?;
        let taking_resources_down =
            pending.status == status::DELETE_COMPLETE || pending.status == status::DELETE_FAILED;
        taking_resources_down.then_some(pending.started_at)
    }

    /// The resource `logical_id` is STILL THERE: the removal the teardown had accepted for it
    /// did not happen.
    ///
    /// The walk stops recording a resource as materialised as soon as the owning service — or
    /// the PROVIDER of a custom resource — accepts the removal, because from then on the stack
    /// owes no further call about it. A removal that then FAILS is the opposite: a provider's
    /// `FAILED` answer fails the resource, and the resource it failed on is one nothing took
    /// away, so the stack holds it again — which is what makes a later deletion of the stack
    /// (the documented way out of `DELETE_FAILED`) ask the owning service, or the provider, to
    /// remove it once more rather than report a deletion of a resource it never asked about
    /// again.
    ///
    /// Called where a removal's wait ends in a failure ([`StoredStack::fail_stabilisation`]),
    /// beside the `DELETE_FAILED` that failure records on the same record.
    pub(super) fn keeps_the_resource_whose_removal_failed(&mut self, logical_id: &str) {
        if let Some(deployed) = self
            .resources
            .iter_mut()
            .find(|deployed| deployed.resource.logical_id == logical_id)
        {
            deployed.resource.materialised = true;
        }
    }
}

/// Whether the deletion that started at `started_at` has a removal of `record` UNDER WAY —
/// asked for and not concluded — which is what the record then reports.
fn removal_under_way(record: &DeployedResource, started_at: SystemTime) -> bool {
    if released_from_the_deletion(&record.status) {
        return false;
    }
    // A status this deletion itself wrote is what became of the resource under it, not a
    // removal still under way.
    if record.status_timestamp >= started_at {
        return false;
    }
    // ...and a resource the stack still holds MATERIALISED is one this deletion has not asked
    // about yet: the walk stops recording it the moment a removal is accepted, so what is left
    // carrying it is a resource the teardown is keeping until the removals before it answer.
    !record.resource.materialised
}

/// Whether a record in `status` is one NO deletion of the stack is responsible for: the
/// resource was taken away by an operation that already reported it, or released where it is.
fn released_from_the_deletion(status: &str) -> bool {
    status == resource_status::DELETE_COMPLETE || status == resource_status::DELETE_SKIPPED
}
