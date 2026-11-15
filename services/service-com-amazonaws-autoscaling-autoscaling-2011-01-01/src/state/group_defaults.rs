// SPDX-License-Identifier: CC0-1.0

//! The members of an Auto Scaling group the SERVICE decides when the request leaves them
//! out.
//!
//! Three of the option sets a group carries are not optional ON THE GROUP: every group has
//! a Capacity Reservation preference, a capacity distribution strategy across its zones
//! and an instance lifecycle policy, whether or not the request that created it said so.
//! Each of their documented defaults is what the group then runs under
//! (`CapacityDistributionStrategy`: "the default is `balanced-best-effort`";
//! `CapacityReservationPreference`: `default` — "Auto Scaling uses the Capacity
//! Reservation preference from your launch template or an open Capacity Reservation";
//! `RetentionTriggers$TerminateHookAbandon`: "set to `terminate` for default termination
//! behavior"), so a read of the group reports the value in force rather than an absent
//! member a client would have to know the default of.
//!
//! This module is the one place each default is applied, for the two operations that write
//! the group: a value a request states is kept exactly as written, and only the member it
//! left out is filled in.

use crate::ops::types::{
    AvailabilityZoneDistribution, CapacityReservationSpecification, InstanceLifecyclePolicy,
    RetentionTriggers,
};

/// `CapacityReservationSpecification$CapacityReservationPreference` when the request states
/// none: the preference whose own documentation describes it as the one Auto Scaling takes
/// from the launch template or an open Capacity Reservation.
pub const DEFAULT_CAPACITY_RESERVATION_PREFERENCE: &str = "default";

/// `AvailabilityZoneDistribution$CapacityDistributionStrategy` when the request states
/// none, from that member's own documentation: "the default is `balanced-best-effort`".
pub const DEFAULT_CAPACITY_DISTRIBUTION_STRATEGY: &str = "balanced-best-effort";

/// `RetentionTriggers$TerminateHookAbandon` when the request states none, from that
/// member's own documentation: "set to `terminate` for default termination behavior".
pub const DEFAULT_TERMINATE_HOOK_ABANDON: &str = "terminate";

/// The Capacity Reservation specification the group runs under, given what the request
/// stated.
#[must_use]
pub fn capacity_reservation_specification(
    requested: Option<CapacityReservationSpecification>,
) -> CapacityReservationSpecification {
    let mut specification = requested.unwrap_or_default();
    if specification.capacity_reservation_preference.is_none() {
        specification.capacity_reservation_preference =
            Some(DEFAULT_CAPACITY_RESERVATION_PREFERENCE.to_string());
    }
    specification
}

/// The Availability Zone distribution the group runs under, given what the request stated.
#[must_use]
pub fn availability_zone_distribution(
    requested: Option<AvailabilityZoneDistribution>,
) -> AvailabilityZoneDistribution {
    let mut distribution = requested.unwrap_or_default();
    if distribution.capacity_distribution_strategy.is_none() {
        distribution.capacity_distribution_strategy =
            Some(DEFAULT_CAPACITY_DISTRIBUTION_STRATEGY.to_string());
    }
    distribution
}

/// The instance lifecycle policy the group runs under, given what the request stated.
#[must_use]
pub fn instance_lifecycle_policy(
    requested: Option<InstanceLifecyclePolicy>,
) -> InstanceLifecyclePolicy {
    let mut policy = requested.unwrap_or_default();
    let mut triggers = policy.retention_triggers.unwrap_or_default();
    if triggers.terminate_hook_abandon.is_none() {
        triggers.terminate_hook_abandon = Some(DEFAULT_TERMINATE_HOOK_ABANDON.to_string());
    }
    policy.retention_triggers = Some(RetentionTriggers {
        terminate_hook_abandon: triggers.terminate_hook_abandon,
    });
    policy
}
