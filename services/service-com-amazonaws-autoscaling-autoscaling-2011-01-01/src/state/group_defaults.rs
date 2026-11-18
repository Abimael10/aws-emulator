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
//! A group launching from a MIXED INSTANCES POLICY carries one more: the policy's
//! `InstancesDistribution` states how capacity is spread over On-Demand and Spot
//! Instances, and the service spreads it one way whether or not the request said which —
//! so every member of that distribution whose documentation states a default is filled in
//! with it here too ([`mixed_instances_policy`]).
//!
//! This module is the one place each default is applied, for the two operations that write
//! the group: a value a request states is kept exactly as written, and only the member it
//! left out is filled in.

use crate::ops::types::{
    AvailabilityZoneDistribution, CapacityReservationSpecification, InstanceLifecyclePolicy,
    MixedInstancesPolicy, RetentionTriggers,
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

/// `InstancesDistribution$OnDemandAllocationStrategy` for a policy whose overrides state
/// `InstanceRequirements`: "this is the default value for Auto Scaling groups that specify
/// InstanceRequirements".
pub const DEFAULT_ON_DEMAND_ALLOCATION_STRATEGY_BY_REQUIREMENTS: &str = "lowest-price";

/// The same member for a policy whose overrides name their instance types outright: "this
/// is the default value for Auto Scaling groups that don't specify InstanceRequirements".
pub const DEFAULT_ON_DEMAND_ALLOCATION_STRATEGY: &str = "prioritized";

/// `InstancesDistribution$OnDemandBaseCapacity` when the request states none: "Default: 0".
pub const DEFAULT_ON_DEMAND_BASE_CAPACITY: i32 = 0;

/// `InstancesDistribution$OnDemandPercentageAboveBaseCapacity` when the request states
/// none: "Default: 100".
pub const DEFAULT_ON_DEMAND_PERCENTAGE_ABOVE_BASE_CAPACITY: i32 = 100;

/// `InstancesDistribution$SpotAllocationStrategy` when the request states none: the
/// strategy that member's own documentation calls "the default value".
pub const DEFAULT_SPOT_ALLOCATION_STRATEGY: &str = "lowest-price";

/// `InstancesDistribution$SpotInstancePools` when the request states none: "Default: 2".
pub const DEFAULT_SPOT_INSTANCE_POOLS: i32 = 2;

/// The mixed instances policy the group runs under, given what the request stated.
///
/// A policy's `InstancesDistribution` is how the group spreads its capacity over On-Demand
/// and Spot Instances, and the group spreads it one way whichever members the request
/// stated: each member the request left out is filled in with the default its own
/// documentation states, so a read of the group reports the distribution in force rather
/// than a null a client would have to know the default of — and a value the request stated
/// is kept exactly as written.
///
/// `OnDemandAllocationStrategy`'s default is the one of the two that member documents for
/// the group's own kind of launch source: `lowest-price` where an override states
/// `InstanceRequirements`, `prioritized` where the overrides name their instance types.
/// `SpotInstancePools` is "valid only when the SpotAllocationStrategy is `lowest-price`",
/// so it is filled in only for a distribution running under that strategy; `SpotMaxPrice`
/// documents no default, and is left exactly as the request left it.
///
/// `None` for a group launching from no mixed instances policy at all: there is then no
/// distribution to spread anything over.
#[must_use]
pub fn mixed_instances_policy(
    requested: Option<MixedInstancesPolicy>,
) -> Option<MixedInstancesPolicy> {
    let mut policy = requested?;
    let by_requirements = super::instance_requirements::states_requirements(&policy);
    let mut distribution = policy.instances_distribution.unwrap_or_default();
    if distribution.on_demand_allocation_strategy.is_none() {
        distribution.on_demand_allocation_strategy = Some(
            if by_requirements {
                DEFAULT_ON_DEMAND_ALLOCATION_STRATEGY_BY_REQUIREMENTS
            } else {
                DEFAULT_ON_DEMAND_ALLOCATION_STRATEGY
            }
            .to_string(),
        );
    }
    if distribution.on_demand_base_capacity.is_none() {
        distribution.on_demand_base_capacity = Some(DEFAULT_ON_DEMAND_BASE_CAPACITY);
    }
    if distribution
        .on_demand_percentage_above_base_capacity
        .is_none()
    {
        distribution.on_demand_percentage_above_base_capacity =
            Some(DEFAULT_ON_DEMAND_PERCENTAGE_ABOVE_BASE_CAPACITY);
    }
    if distribution.spot_allocation_strategy.is_none() {
        distribution.spot_allocation_strategy =
            Some(DEFAULT_SPOT_ALLOCATION_STRATEGY.to_string());
    }
    if distribution.spot_instance_pools.is_none()
        && distribution.spot_allocation_strategy.as_deref()
            == Some(DEFAULT_SPOT_ALLOCATION_STRATEGY)
    {
        distribution.spot_instance_pools = Some(DEFAULT_SPOT_INSTANCE_POOLS);
    }
    policy.instances_distribution = Some(distribution);
    Some(policy)
}
