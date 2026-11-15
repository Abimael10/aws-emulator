// SPDX-License-Identifier: CC0-1.0

//! How a service reports a placement its scheduler CANNOT make: the event it writes and
//! the run status it carries while the shortfall lasts.
//!
//! The scheduler keeps a service at the count it wants, and where the capacity to do that
//! does not exist it reports the shortfall in two places at once — the service's event
//! stream (`Service$events`, "The event stream for your service") and `Service$runStatus`.
//! The two are one verdict with two spellings, so they are decided together here and
//! cannot disagree:
//!
//! * the EVENT is the Amazon ECS service event message for a placement that found nowhere
//!   to run ([`super::unable_to_place_message`]), whose `reason` clause says WHICH capacity
//!   was missing;
//! * the RUN STATUS is the error code that classifies the same condition for a client that
//!   reads codes rather than sentences ([`PlacementShortfall::run_status`]), which is what a
//!   caller needs to tell "still converging" from "will never converge" without parsing
//!   text.
//!
//! `Service$runStatus` is not in the Smithy model — it is one of the members the real
//! service returns beside the modelled ones — so its values are the ones recorded responses
//! carry: the `Error.`-prefixed code of the condition while the scheduler cannot place, and
//! the scheduler-progress value otherwise
//! ([`super::ComAmazonawsEcsAmazonec2containerservicev20141113State::service_run_status`]).

use super::*;

/// `Service$runStatus` of a service whose tasks are placed on capacity REGISTERED to its
/// cluster, of which the cluster holds none.
pub const RUN_STATUS_CLUSTER_EMPTY: &str = "Error.ClusterEmpty";

/// `Service$runStatus` of a service whose tasks are placed through a CAPACITY PROVIDER that
/// holds no capacity to place them on.
pub const RUN_STATUS_CAPACITY_PROVIDER_EMPTY: &str = "Error.CapacityProviderEmpty";

/// The `reason` clause of the placement-failure event when the capacity the placement needs
/// is a container instance of the service's own cluster, of which there is none.
///
/// The same sentence `RunTask` refuses that condition with
/// ([`super::NO_CONTAINER_INSTANCES_MESSAGE`]), because both describe the one condition.
pub const NO_CLUSTER_CAPACITY_REASON: &str = NO_CONTAINER_INSTANCES_MESSAGE;

/// The `reason` clause of the same event when the placement runs through a CAPACITY
/// PROVIDER that holds none.
///
/// A FIXED sentence, worded over the capacity provider the placement took rather than over
/// the cluster: the event names the service it is about, so the clause names no provider.
pub const NO_CAPACITY_PROVIDER_CAPACITY_REASON: &str =
    "No Container Instances were found in your capacity provider.";

/// A placement the scheduler cannot make, as the service REPORTS it: the sentence its event
/// stream carries and the code `Service$runStatus` classifies it with.
///
/// One value for both spellings, so a service whose events say the cluster is empty cannot
/// report a run status about a capacity provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementShortfall {
    /// The `reason` clause of the event ([`super::unable_to_place_message`]).
    pub reason: &'static str,
    /// The `Service$runStatus` code for the same condition.
    pub run_status: &'static str,
}

impl PlacementShortfall {
    /// The shortfall of a cluster that holds no container instance to place the service's
    /// tasks on.
    pub const CLUSTER_EMPTY: Self = Self {
        reason: NO_CLUSTER_CAPACITY_REASON,
        run_status: RUN_STATUS_CLUSTER_EMPTY,
    };

    /// The shortfall of a capacity provider that holds no capacity to place them on.
    pub const CAPACITY_PROVIDER_EMPTY: Self = Self {
        reason: NO_CAPACITY_PROVIDER_CAPACITY_REASON,
        run_status: RUN_STATUS_CAPACITY_PROVIDER_EMPTY,
    };
}
