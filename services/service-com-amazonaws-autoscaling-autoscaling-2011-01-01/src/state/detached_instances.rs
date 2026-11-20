// SPDX-License-Identifier: CC0-1.0

//! Taking an Auto Scaling group's own instances OUT of it without terminating them: the one
//! transition `DetachInstances` performs.
//!
//! # The rule this module implements
//!
//! "Removes one or more instances from the specified Auto Scaling group. After the instances
//! are detached, you can manage them independent of the Auto Scaling group. If you do not
//! specify the option to decrement the desired capacity, Amazon EC2 Auto Scaling launches
//! instances to replace the ones that are detached. … If there are target groups attached to
//! the Auto Scaling group, the instances are deregistered from the target groups"
//! (`smithy_query(query="doc",shape="DetachInstances")`). So one detach is three facts and
//! nothing else:
//!
//! * each named instance stops being an instance of the group — reported
//!   [`super::LIFECYCLE_DETACHING`] while it is being released, which is the transient state
//!   the model's `LifecycleState` enum names for it, and gone from the group once
//!   [`super::LIFECYCLE_TRANSITION`] is due;
//! * the Amazon EC2 instance KEEPS RUNNING, because the group released it rather than
//!   terminating it — which is what "you can manage them independent of the Auto Scaling
//!   group" means, and why the group tag Amazon EC2 Auto Scaling puts on its members is
//!   taken off it ([`super::ec2_instances::clear_group_membership`]);
//! * the group's desired capacity falls by what the detached instances were providing when
//!   the request asked for the decrement, and stands when it did not — so the group launches
//!   a replacement for each, through the one convergence every change to a group's size goes
//!   through ([`AutoScalingState::apply_capacity`]).
//!
//! The deregistration from the group's target groups falls out of the group no longer
//! holding them: the registrations a group's attachments owe are recomputed from its
//! instances ([`super::elbv2_targets`]), and an instance on its way out of the group is not
//! among them — so exactly the detached instances leave the target groups, and the ones the
//! same group still holds stay registered.
//!
//! # The instances a detach may release
//!
//! A detach moves a member of THE GROUP, so an id the group does not hold is refused, and so
//! is one that is in neither state a member of a group can be detached from: an instance is
//! detachable while it is `InService`, and while it is parked out of service in `Standby` —
//! the state a client puts a member in precisely in order to manage it independently — and
//! is not while it is still coming into service or already on its way out. An id failing
//! either refuses the call WHOLE — nothing is detached and the group's capacity does not
//! move — because the capacity a successful detach reports is the capacity of the instances
//! it released, so a call that could release only some of them has no capacity to report.
//! The same is true of the decrement itself: a group never holds a desired capacity below
//! its own `MinSize`, so a request that would take it under is refused rather than
//! decrementing by less than it asked for.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::{
    ec2_instances, AutoScalingState, StoredActivity, StoredInstance, GROUP_STATE_IN_SERVICE,
    LIFECYCLE_DETACHING, LIFECYCLE_IN_SERVICE, LIFECYCLE_STANDBY, LIFECYCLE_TRANSITION,
};

/// The `Activity$Description` the scaling activity for one detach carries, naming the
/// instance it released from the group — the one wording for it in this crate.
#[must_use]
pub fn detach_description(instance_id: &str) -> String {
    format!("Detaching EC2 instance: {instance_id}")
}

/// What the activities of a detach record as the reason they began: a caller named the
/// instances, which is what tells an instance a client took out of the group apart from one
/// the group scaled in for itself.
///
/// Reads as the subject of [`AutoScalingState::capacity_change_cause`] when the detach also
/// moved the group's desired capacity, and as a sentence of its own when it did not.
const DETACH_REASON: &str = "a user request explicitly detached instances from the group";

/// Whether `instance` is in a state a detach may release it from: `InService`, or parked out
/// of service in `Standby`.
///
/// Those are the two states a member of a group is detachable in — one serving traffic, one
/// a client has already taken out of service in order to manage it independently, which is
/// what a detach makes permanent. An instance still coming into service has not reached
/// either, and one already on its way out of the group is leaving for a reason the group
/// itself recorded ([`StoredInstance::is_leaving_group`]).
fn is_detachable(instance: &StoredInstance) -> bool {
    instance.lifecycle_state == LIFECYCLE_IN_SERVICE
        || instance.lifecycle_state == LIFECYCLE_STANDBY
}

/// Why a detach could not be made, for the operation to state the refusal each situation
/// belongs to.
#[derive(Debug)]
pub enum DetachRefusal {
    /// `instance_id` is not an instance of the group the request named.
    NotInGroup(String),
    /// `instance_id` is an instance of the group but in neither state a detach may release
    /// it from; carries the state it is in.
    WrongState {
        /// The instance the request named.
        instance_id: String,
        /// The `LifecycleState` the group reports it in.
        lifecycle_state: String,
    },
    /// Releasing the named instances and decrementing the capacity would leave the group
    /// below its own `MinSize`.
    BelowMinSize {
        /// The desired capacity it has now.
        desired_capacity: i32,
        /// The group's own `MinSize`, which it is never left under.
        min_size: i32,
        /// How many instances the request asked to detach.
        detaching: usize,
    },
}

impl DetachRefusal {
    /// The sentence this refusal carries.
    ///
    /// Each is about ONE thing the request asked for: the fixed text states the rule and the
    /// variable parts — the instance id, the group name, the state the instance is in and
    /// the two sizes the decrement crossed — come from the request and the group.
    #[must_use]
    pub fn message(&self, group_name: &str) -> String {
        match self {
            Self::NotInGroup(instance_id) => {
                super::the_instance_not_in_group_message(instance_id, group_name)
            }
            Self::WrongState {
                instance_id,
                lifecycle_state,
            } => format!(
                "Instance {instance_id} is in {lifecycle_state} state, not in \
                 {LIFECYCLE_IN_SERVICE} or {LIFECYCLE_STANDBY} state, and cannot be detached."
            ),
            Self::BelowMinSize {
                desired_capacity,
                min_size,
                detaching,
            } => super::min_size_decrement_breach_message(
                "Detaching",
                *desired_capacity,
                *min_size,
                *detaching,
            ),
        }
    }
}

impl AutoScalingState {
    /// Detach the instances `instance_ids` names from the group and answer the scaling
    /// activities that did it.
    ///
    /// THE ONE detach transition: every rule of it is decided here — which instances may be
    /// released, the capacity the group is left converging on and the activity each detach
    /// records — so the operation only has to state the refusal.
    ///
    /// An id the request repeats names ONE instance. A request naming no instance at all
    /// detaches none and records no activity: nothing was owed. Answers [`DetachRefusal`]
    /// without changing anything when any named instance cannot be released, or when the
    /// decrement would take the group below its own `MinSize`.
    pub fn detach_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_ids: &[String],
        should_decrement: bool,
        now: SystemTime,
    ) -> Result<Vec<StoredActivity>, DetachRefusal> {
        let Some(group) = self.groups.get(group_name) else {
            return Ok(Vec::new());
        };
        let arn = group.arn.clone();
        let desired_capacity = group.desired_capacity;
        let min_size = group.min_size;

        // Every named instance is checked BEFORE one is released: an id the group does not
        // hold, or one not in the state a detach starts from, refuses the whole call and
        // leaves every instance of the group exactly where it was.
        //
        // The group's own members are indexed in ONE pass, and each id is admitted through a
        // SET: the cost of a call is the group's membership once plus one lookup per id,
        // never the membership once per id.
        let members: BTreeMap<&str, &StoredInstance> = group
            .instances
            .iter()
            .map(|instance| (instance.instance_id.as_str(), instance))
            .collect();
        let mut admitted: BTreeSet<&str> = BTreeSet::new();
        let mut detaching: Vec<String> = Vec::new();
        for instance_id in instance_ids {
            let Some(instance) = members.get(instance_id.as_str()) else {
                return Err(DetachRefusal::NotInGroup(instance_id.clone()));
            };
            if !is_detachable(instance) {
                return Err(DetachRefusal::WrongState {
                    instance_id: instance_id.clone(),
                    lifecycle_state: instance.lifecycle_state.clone(),
                });
            }
            if admitted.insert(instance_id.as_str()) {
                detaching.push(instance_id.clone());
            }
        }
        if detaching.is_empty() {
            return Ok(Vec::new());
        }

        // What the released instances were contributing to the group's capacity, in the
        // units its size is stated in: a member of a weighted group takes its own weight's
        // worth of capacity with it, which is what the decrement below takes off.
        let units = group.capacity_units_of(&detaching);
        let new_desired_capacity = if should_decrement {
            let after = desired_capacity.saturating_sub(units);
            if after < min_size {
                return Err(DetachRefusal::BelowMinSize {
                    desired_capacity,
                    min_size,
                    detaching: detaching.len(),
                });
            }
            after
        } else {
            desired_capacity
        };
        let cause = if new_desired_capacity == desired_capacity {
            Self::reason_cause(now, DETACH_REASON)
        } else {
            Self::capacity_change_cause(
                now,
                DETACH_REASON,
                desired_capacity,
                new_desired_capacity,
            )
        };

        let due = now + LIFECYCLE_TRANSITION;
        // The released ids as the SET the pass below tests membership in, so one write costs
        // the group's members once plus one lookup each rather than a scan per member — the
        // cost this module's header states for every pass it makes over a request's ids.
        let released: BTreeSet<&str> = detaching.iter().map(String::as_str).collect();
        if let Some(group) = self.groups.get_mut(group_name) {
            for instance in &mut group.instances {
                if released.contains(instance.instance_id.as_str()) {
                    instance.lifecycle_state = LIFECYCLE_DETACHING.to_string();
                    instance.transition_due = Some(due);
                    // A member on its way out of the group is no longer one a health check
                    // of the group replaces: it is leaving for a reason the client asked
                    // for ([`super::instance_health`]).
                    instance.health_check_due = None;
                }
            }
        }
        // The instance is no longer part of the group, so the group tag Amazon EC2 Auto
        // Scaling puts on every member comes off it where the instance lives: that tag is
        // what tells an instance a group manages from one a client manages itself, which is
        // exactly what a detached instance becomes.
        ec2_instances::clear_group_membership(engine, &detaching, group_name);

        // One activity per instance detached, completing when its transient state does, so
        // the activity a client reads reports the detach it is watching.
        let activity_ids: Vec<String> = detaching
            .iter()
            .map(|instance_id| {
                self.record_activity(
                    engine,
                    &super::ActivityRecord {
                        group_name,
                        group_arn: &arn,
                        description: detach_description(instance_id),
                        cause: cause.clone(),
                        group_state: GROUP_STATE_IN_SERVICE,
                        start_time: now,
                        completes_at: Some(due),
                        milestone: super::ActivityMilestone::InProgress,
                        instance_id: Some(instance_id),
                    },
                )
            })
            .collect();
        let activities: Vec<StoredActivity> = activity_ids
            .iter()
            .filter_map(|activity_id| self.activity(activity_id).cloned())
            .collect();
        // The group then converges on the capacity the detach leaves it with, through the
        // same path every other change to its size takes: with the decrement there is
        // nothing to replace, and without it every instance this call released is replaced.
        self.apply_capacity(engine, group_name, new_desired_capacity, &cause, now);
        Ok(activities)
    }
}
