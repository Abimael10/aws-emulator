// SPDX-License-Identifier: CC0-1.0

//! What still HOLDS an Auto Scaling group, and what the clock is still SETTLING on it: the
//! two facts a delete that was not asked to force itself decides its refusal from.
//!
//! # The rule this module implements
//!
//! "Deletes the specified Auto Scaling group. If the group has instances or scaling
//! activities in progress, you must specify the option to force the deletion in order for it
//! to succeed. … To remove instances from the Auto Scaling group before deleting it, call the
//! DetachInstances API with the list of instances and the option to decrement the desired
//! capacity. … To terminate all instances before deleting the Auto Scaling group, call the
//! UpdateAutoScalingGroup API and set the minimum size and desired capacity of the Auto
//! Scaling group to zero" (`smithy_query(query="doc",shape="DeleteAutoScalingGroup")`). Both
//! documented teardowns END in a delete that succeeds, so the two situations the operation
//! models are told apart rather than answered as one:
//!
//! * the group still HOLDS something — a member that is STAYING, an instance its warm pool
//!   is keeping, or capacity it still asks for — which is the standing refusal
//!   `ResourceInUseFault`, answered by taking what is left out of the group;
//! * nothing is left but a change the CLOCK is still carrying out — a member on its way out
//!   of the group, an instance the warm pool is terminating, a capacity change waiting for a
//!   cooldown — which is the transient refusal `ScalingActivityInProgressFault`, answered by
//!   retrying until it has settled.
//!
//! A member already on its way out holds the group no longer: it is leaving on its own, which
//! is the same reading the warm pool's own delete takes of an instance on its way out of the
//! pool ([`super::warm_pools::StoredWarmPool::live_size`]). A member nothing on the clock
//! will take out is NOT settling and does hold the group: one a retention trigger kept
//! ([`StoredInstance::is_retained`]) stays a member until a caller takes it out, and one
//! waiting at a lifecycle hook ([`StoredInstance::is_held_by_lifecycle_hook`]) waits for a
//! callback rather than for the clock.

use super::{StoredGroup, StoredInstance};

impl StoredInstance {
    /// Whether the CLOCK is taking this member out of its group: it is on its way out, and
    /// nothing is keeping it there.
    ///
    /// Such a member counts towards nothing the group holds — it is already leaving — while
    /// the change that is releasing it is one a caller can wait for.
    #[must_use]
    pub fn is_settling_out_of_group(&self) -> bool {
        self.is_leaving_group() && !self.is_retained() && !self.is_held_by_lifecycle_hook()
    }
}

impl StoredGroup {
    /// The members that still HOLD this group: every instance of it the clock is not taking
    /// out.
    ///
    /// Every state a member can be in, however it got there — coming into service, serving,
    /// parked in `Standby`, kept by a retention trigger, waiting at a lifecycle hook — is one
    /// the group still holds the instance in; only an instance already on its way out on the
    /// clock is not.
    pub fn holding_instances(&self) -> impl Iterator<Item = &StoredInstance> {
        self.instances
            .iter()
            .filter(|instance| !instance.is_settling_out_of_group())
    }

    /// Whether this group still holds anything: a member that is staying, an instance its
    /// warm pool is keeping, or capacity it still asks for.
    ///
    /// The capacity counts because a group whose `DesiredCapacity` is above zero maintains
    /// that capacity: it would launch what it is short of, so a group still asking for
    /// capacity is as much in use as one holding the instances that meet it.
    #[must_use]
    pub fn holds_instances_or_capacity(&self) -> bool {
        self.holding_instances().next().is_some()
            || self.warm_pool_holds_instances()
            || self.desired_capacity > 0
    }

    /// Whether this group's warm pool is keeping instances: a pool with none, and a group
    /// with no pool at all, keeps nothing.
    #[must_use]
    pub fn warm_pool_holds_instances(&self) -> bool {
        self.warm_pool().is_some_and(|pool| pool.live_size() > 0)
    }

    /// Whether a change of this group is still being carried out ON THE CLOCK: a member on
    /// its way out of it, an instance its warm pool is terminating, or a capacity change
    /// waiting for the group's cooldown to elapse.
    ///
    /// Each of these settles with no further call from anybody, so a caller that waits sees
    /// it settle; nothing here asks the caller to change the group.
    #[must_use]
    pub fn is_settling(&self) -> bool {
        self.instances
            .iter()
            .any(StoredInstance::is_settling_out_of_group)
            || self
                .warm_pool()
                .is_some_and(|pool| pool.size() > pool.live_size())
            || self.pending_capacity.is_some()
    }
}
