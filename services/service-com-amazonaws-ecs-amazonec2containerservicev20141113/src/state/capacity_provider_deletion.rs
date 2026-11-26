// SPDX-License-Identifier: CC0-1.0

//! RETIRING a capacity provider: the cluster association that holds the delete off, the
//! `(status, updateStatus)` pair each outcome leaves on the stored provider, and the
//! clock-owed end of a delete that was accepted.
//!
//! The delete's other documented precondition — "Prior to a capacity provider being
//! deleted, the capacity provider must be removed from the capacity provider strategy from
//! all services" — needs no test of its own, because the CLUSTER ASSOCIATION subsumes it:
//! a provider an active service's `capacityProviderStrategy` names is one
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::capacity_providers_in_use`]
//! reports for that service's cluster, and `PutClusterCapacityProviders` refuses to
//! disassociate such a provider ([`CAPACITY_PROVIDER_IN_USE_MESSAGE`]) — so while any
//! service's strategy names it, the association is still there and
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::capacity_provider_is_associated`]
//! already holds the delete off.

use super::*;

/// `CapacityProvider$updateStatus` while an accepted delete is still running — the
/// `CapacityProviderUpdateStatus` value for it (`smithy_query` query="shape"
/// shape="CapacityProviderUpdateStatus"). The capacity the provider bound onto is still
/// being released at this point, so the delete is reported in flight and
/// [`CAPACITY_PROVIDER_DELETE_COMPLETE`] is owed by the clock.
pub const CAPACITY_PROVIDER_DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";

/// `CapacityProvider$updateStatus` once that delete has finished: the terminal status of a
/// deleted provider, reported from the instant the release falls due.
pub const CAPACITY_PROVIDER_DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// `CapacityProvider$updateStatus` for a delete Amazon ECS did NOT carry out because the
/// provider is still associated with a cluster.
///
/// The `DeleteCapacityProvider` doc states the condition — "Only capacity providers that
/// aren't associated with a cluster can be deleted. To remove a capacity provider from a
/// cluster, you can either use PutClusterCapacityProviders or delete the cluster" — and the
/// recorded behaviour reports it on the PROVIDER rather than as an error: the call is
/// answered, the provider keeps the status it had, and this is the update status that says
/// the delete it was asked for did not happen.
pub const CAPACITY_PROVIDER_DELETE_FAILED: &str = "DELETE_FAILED";

/// `ClientException$message` for an update of a capacity provider this account DELETED.
///
/// A delete KEEPS the provider as a tombstone ([`CapacityProviderRecord::retire`]) that every
/// later read still reports, so such a reference RESOLVES — to a provider nothing can be
/// written onto, which is what this sentence states, as against the one a reference resolving
/// to NO record of the account earns ([`CAPACITY_PROVIDER_MISSING_MESSAGE`]). The two are
/// what let a client reconciling a delete of its own against an update still in flight tell
/// that provider from an identifier it got wrong.
///
/// Fixed text for this class: the sentence names no provider, no cluster and no value, so
/// nothing in it comes from the request or from the store.
pub const DELETED_CAPACITY_PROVIDER_UPDATE_MESSAGE: &str =
    "The capacity provider could not be updated because it has been deleted.";

/// Which sentence an UPDATE of the capacity provider a reference names earns when that
/// reference names no LIVE provider of the account: `resolved` is whether the account still
/// holds a RECORD under the name — the tombstone a delete left behind
/// ([`DELETED_CAPACITY_PROVIDER_UPDATE_MESSAGE`]) rather than a name no provider of the
/// account ever answered to ([`CAPACITY_PROVIDER_MISSING_MESSAGE`]).
///
/// The ONE place that distinction is drawn for the update, which is the operation whose
/// outcome depends on it: a delete of a provider that is already retired is `@idempotent` and
/// reports that provider rather than refusing it, and a batch read reports both conditions as
/// the provider it describes.
pub fn capacity_provider_update_refusal_message(resolved: bool) -> &'static str {
    if resolved {
        DELETED_CAPACITY_PROVIDER_UPDATE_MESSAGE
    } else {
        CAPACITY_PROVIDER_MISSING_MESSAGE
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Whether a LIVE cluster of this account still lists the capacity provider `name`
    /// among the providers associated with it, which is what a delete of that provider is
    /// held off by.
    ///
    /// `Cluster$capacityProviders` is "the capacity providers associated with the cluster",
    /// and `PutClusterCapacityProviders` is what writes it — so that list is where the
    /// association lives, and a provider named in it is one the cluster may place tasks
    /// through. A DELETED cluster holds nothing open: "To remove a capacity provider from a
    /// cluster, you can either use PutClusterCapacityProviders or delete the cluster", so
    /// the tombstone a delete leaves behind is not an association — which is the same
    /// liveness test every operation that resolves ONE cluster to work in makes
    /// ([`Self::active_cluster`]), read across the whole map here because the association
    /// may be held by any cluster of the account. Both sides are compared as the provider
    /// they REFER to ([`capacity_provider_name_from_reference`]), so a cluster that stored
    /// the provider by ARN still holds the same provider the delete named by its short name.
    pub fn capacity_provider_is_associated(&self, name: &str) -> bool {
        self.clusters
            .values()
            .filter(|cluster| cluster.status != CLUSTER_INACTIVE)
            .any(|cluster| {
                cluster
                    .capacity_providers
                    .iter()
                    .any(|reference| capacity_provider_name_from_reference(reference) == name)
            })
    }
}

impl CapacityProviderRecord {
    /// Record on this provider that the delete it was asked for did not happen: its status
    /// is left exactly as it was and its update status becomes
    /// [`CAPACITY_PROVIDER_DELETE_FAILED`], so every later read of the provider reports the
    /// hold the caller has to clear.
    pub fn report_delete_failed(&mut self) {
        self.update_status = Some(CAPACITY_PROVIDER_DELETE_FAILED.to_string());
    }

    /// Retire this provider: it becomes [`CAPACITY_PROVIDER_INACTIVE`] — the
    /// `CapacityProvider$status` doc, "When a capacity provider is successfully deleted, it
    /// has an `INACTIVE` status" — from this instant, since no cluster may use it from
    /// here on, while releasing the capacity it bound onto is the update left in flight
    /// ([`CAPACITY_PROVIDER_DELETE_IN_PROGRESS`]) and the clock owes its completion
    /// [`TRANSITION_SPAN`] later.
    ///
    /// The record is KEPT as a tombstone, as the `DescribeCapacityProviders` of a deleted
    /// provider still reports it; what a tombstone no longer holds is its name or its Auto
    /// Scaling group against a new provider.
    ///
    /// `@idempotent`: a provider that is already retired is left exactly as it stands, so a
    /// repeated delete has the effect of the first one only.
    pub fn retire(&mut self, now: SystemTime) {
        if self.status == CAPACITY_PROVIDER_INACTIVE {
            return;
        }
        self.status = CAPACITY_PROVIDER_INACTIVE.to_string();
        self.update_status = Some(CAPACITY_PROVIDER_DELETE_IN_PROGRESS.to_string());
        // A provider whose own capacity Amazon ECS was still standing up stops standing it
        // up: the create this instant would have landed is not owed to a provider that is
        // being released. Neither is the end of an update of that capacity, for the same
        // reason — the release is what is in flight for the provider from here on.
        self.active_due_at = None;
        self.update_due_at = None;
        self.delete_due_at = Some(now + TRANSITION_SPAN);
    }
}
