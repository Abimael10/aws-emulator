// SPDX-License-Identifier: CC0-1.0

//! Parking an Auto Scaling group's instances in `Standby`, and returning them to service.
//!
//! # The rule this module implements
//!
//! `EnterStandby` "moves the specified instances into the standby state", and the model's
//! `LifecycleState` enum names the two states that move goes through: an instance is
//! reported `EnteringStandby` while it is being parked and `Standby` once it is
//! (`smithy_query(query="shape",shape="LifecycleState")`). `ExitStandby` "moves the
//! specified instances out of the standby state", so a returning instance is reported
//! `Pending` and reaches `InService` the way every instance a group brings into service
//! does.
//!
//! A parked instance is OUT OF SERVICE, so the group's desired capacity is not met by it
//! ([`StoredInstance::is_in_standby`]). That single fact is what makes the two capacity
//! rules the operations document fall out of the group's own convergence
//! ([`AutoScalingState::apply_capacity`]) rather than being written twice:
//!
//! * "If you choose to decrement the desired capacity of the Auto Scaling group, the
//!   instances can enter standby as long as the desired capacity of the Auto Scaling group
//!   after the instances are placed into standby is equal to or greater than the minimum
//!   capacity of the group" (`EnterStandby`) — the decrement is refused when it would
//!   leave the group below its own `MinSize`, never applied in part.
//! * "If you choose not to decrement the desired capacity of the Auto Scaling group, the
//!   Auto Scaling group launches new instances to replace the instances on standby"
//!   (same) — the capacity stands while the instances stop counting towards it, so the
//!   group's convergence launches a replacement for each.
//! * "After you put the instances back in service, the desired capacity is incremented"
//!   (`ExitStandby`) — the returning instances count again, and the capacity rises with
//!   them so nothing is scaled in to make room.
//!
//! Both operations are answered with the scaling activities the move produced, which is
//! what their `Activities` output reports.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::{
    ActivityMilestone, AutoScalingState, StoredActivity, StoredInstance, GROUP_STATE_IN_SERVICE,
    LIFECYCLE_ENTERING_STANDBY, LIFECYCLE_IN_SERVICE, LIFECYCLE_PENDING, LIFECYCLE_STANDBY,
    LIFECYCLE_TRANSITION,
};

/// Which way a standby move goes: into `Standby`, or back out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandbyMove {
    /// `EnterStandby`: park the instances out of service.
    Enter,
    /// `ExitStandby`: return them to service.
    Exit,
}

impl StandbyMove {
    /// The `LifecycleState` an instance has to be in for this move to start from it.
    #[must_use]
    pub fn from_state(self) -> &'static str {
        match self {
            Self::Enter => LIFECYCLE_IN_SERVICE,
            Self::Exit => LIFECYCLE_STANDBY,
        }
    }

    /// The transient `LifecycleState` the instance is reported in while the move runs.
    #[must_use]
    fn transient_state(self) -> &'static str {
        match self {
            Self::Enter => LIFECYCLE_ENTERING_STANDBY,
            Self::Exit => LIFECYCLE_PENDING,
        }
    }

    /// The `Activity$Description` the scaling activity for this move carries, naming the
    /// instance it moved — the one wording for it in this crate.
    #[must_use]
    fn description(self, instance_id: &str) -> String {
        match self {
            Self::Enter => format!("Moving EC2 instance to Standby: {instance_id}"),
            Self::Exit => format!("Moving EC2 instance out of Standby: {instance_id}"),
        }
    }

    /// What the activities of this move record as the reason they began: a caller named
    /// the instance, which is what tells a parked instance apart from one a capacity
    /// change chose for itself.
    ///
    /// Reads as the subject of [`AutoScalingState::capacity_change_cause`] when the move
    /// also changed the group's desired capacity, and as a sentence of its own when it did
    /// not. This is the reason the CONVERGENCE the move triggers records; the activities of
    /// the move itself carry [`Self::activity_cause`].
    #[must_use]
    fn reason(self) -> &'static str {
        match self {
            Self::Enter => "a user request explicitly moved an instance of the group to Standby",
            Self::Exit => "a user request explicitly moved an instance of the group out of Standby",
        }
    }

    /// The `Activity$Cause` of the activity this move records for ONE instance: when the
    /// move began, which instance moved, that a user asked for it, and — when the group's
    /// desired capacity moved with the instance — the capacities it moved between.
    ///
    /// Worded by the one writer of this class
    /// ([`super::user_requested_instance_move_cause`]), which a termination a caller named
    /// reads the same way: this move states only WHICH move it was.
    #[must_use]
    fn activity_cause(self, at: SystemTime, instance_id: &str, from: i32, to: i32) -> String {
        let moved = match self {
            Self::Enter => "was moved to standby",
            Self::Exit => "was moved out of standby",
        };
        super::user_requested_instance_move_cause(at, instance_id, moved, from, to)
    }

    /// The milestone the activity of this move is at while it runs.
    ///
    /// A return from `Standby` is bringing an instance that already exists back into
    /// service, which is the pre-service step; a park is taking one out of service, which
    /// the service reports no finer step than being under way.
    #[must_use]
    fn milestone(self) -> ActivityMilestone {
        match self {
            Self::Enter => ActivityMilestone::InProgress,
            Self::Exit => ActivityMilestone::PreInService,
        }
    }
}

/// Why a standby move could not be made, for the operation to state the refusal each
/// situation belongs to.
#[derive(Debug)]
pub enum StandbyRefusal {
    /// `instance_id` is not an instance of the group the request named.
    NotInGroup(String),
    /// `instance_id` is an instance of the group but not in the state this move starts
    /// from.
    WrongState {
        /// The instance the request named.
        instance_id: String,
    },
    /// Parking the named instances and decrementing the capacity would leave the group
    /// below its own `MinSize`.
    BelowMinSize {
        /// The desired capacity the group would be left with.
        after: i32,
        /// The group's own `MinSize`, which it is never left under.
        min_size: i32,
    },
}

impl StandbyRefusal {
    /// The sentence this refusal carries.
    ///
    /// Each is about ONE thing the request asked for: the fixed text states the rule — for
    /// an instance on the wrong side of the move, the state the operation REQUIRES it to be
    /// in — and the variable parts come from the request and the group: the instance id, the
    /// group name and the two sizes the decrement crossed.
    #[must_use]
    pub fn message(&self, group_name: &str, move_kind: StandbyMove) -> String {
        match self {
            Self::NotInGroup(instance_id) => {
                super::the_instance_not_in_group_message(instance_id, group_name)
            }
            Self::WrongState { instance_id } => format!(
                "The instance {instance_id} is not in {}.",
                move_kind.from_state()
            ),
            Self::BelowMinSize { after, min_size } => format!(
                "The desired capacity of the Auto Scaling group after the instances are placed \
                 into Standby ({after}) is below the minimum capacity of the group ({min_size}). \
                 Either set ShouldDecrementDesiredCapacity flag to false or lower group's min \
                 size."
            ),
        }
    }
}

impl AutoScalingState {
    /// Move the instances `instance_ids` names into `Standby` — or back out of it — and
    /// answer the scaling activities that did it.
    ///
    /// THE ONE standby transition, shared by `EnterStandby` and `ExitStandby`: the two
    /// operations move one instance between the same two sides, so the membership and
    /// state rules, the activities recorded and the capacity the group is left converging
    /// on are decided here once.
    ///
    /// `should_decrement` is what an `EnterStandby` request says about the group's size;
    /// an `ExitStandby` carries no such member, and the capacity rises with the instances
    /// it returns. An id the request repeats names ONE instance. A request naming no
    /// instance at all moves none, and records no activity: nothing was owed.
    pub fn move_standby<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_ids: &[String],
        move_kind: StandbyMove,
        should_decrement: bool,
        now: SystemTime,
    ) -> Result<Vec<StoredActivity>, StandbyRefusal> {
        let Some(group) = self.groups.get(group_name) else {
            return Ok(Vec::new());
        };
        let arn = group.arn.clone();
        let desired_capacity = group.desired_capacity;
        let (min_size, max_size) = (group.min_size, group.max_size);

        // Every named instance is checked BEFORE one is moved: an id the group does not
        // hold, or one not on the side this move starts from, refuses the whole call and
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
        let mut moving: Vec<String> = Vec::new();
        for instance_id in instance_ids {
            let Some(instance) = members.get(instance_id.as_str()) else {
                return Err(StandbyRefusal::NotInGroup(instance_id.clone()));
            };
            if instance.lifecycle_state != move_kind.from_state() {
                return Err(StandbyRefusal::WrongState {
                    instance_id: instance_id.clone(),
                });
            }
            if admitted.insert(instance_id.as_str()) {
                moving.push(instance_id.clone());
            }
        }
        if moving.is_empty() {
            return Ok(Vec::new());
        }

        // What the instances being moved contribute to the group's capacity, in the units
        // its size is stated in: a parked member of a weighted group stops its own weight's
        // worth of capacity counting, and a returning one starts it counting again.
        let units = group.capacity_units_of(&moving);
        let new_desired_capacity = match move_kind {
            // Parking without the decrement leaves the capacity where it is, so the group
            // launches a replacement for each parked instance.
            StandbyMove::Enter if !should_decrement => desired_capacity,
            StandbyMove::Enter => {
                let after = desired_capacity.saturating_sub(units);
                if after < min_size {
                    return Err(StandbyRefusal::BelowMinSize { after, min_size });
                }
                after
            }
            // A returning instance counts towards the capacity again, and the capacity
            // rises with it — never above the group's own `MaxSize`, which it is never
            // left over.
            StandbyMove::Exit => desired_capacity.saturating_add(units).min(max_size),
        };
        let cause = if new_desired_capacity == desired_capacity {
            Self::reason_cause(now, move_kind.reason())
        } else {
            Self::capacity_change_cause(
                now,
                move_kind.reason(),
                desired_capacity,
                new_desired_capacity,
            )
        };

        let due = now + LIFECYCLE_TRANSITION;
        if let Some(group) = self.groups.get_mut(group_name) {
            for instance in &mut group.instances {
                if moving.contains(&instance.instance_id) {
                    instance.lifecycle_state = move_kind.transient_state().to_string();
                    instance.transition_due = Some(due);
                }
            }
        }

        // One activity per instance moved, completing when its transient state does, so
        // the activity a client reads reports the move it is watching — each one naming
        // the instance it moved and where that instance is.
        let activity_ids: Vec<String> = moving
            .iter()
            .map(|instance_id| {
                self.record_activity(
                    engine,
                    &super::ActivityRecord {
                        group_name,
                        group_arn: &arn,
                        description: move_kind.description(instance_id),
                        cause: move_kind.activity_cause(
                            now,
                            instance_id,
                            desired_capacity,
                            new_desired_capacity,
                        ),
                        group_state: GROUP_STATE_IN_SERVICE,
                        start_time: now,
                        completes_at: Some(due),
                        milestone: move_kind.milestone(),
                        instance_id: Some(instance_id),
                    },
                )
            })
            .collect();
        let activities: Vec<StoredActivity> = activity_ids
            .iter()
            .filter_map(|activity_id| self.activity(activity_id).cloned())
            .collect();
        // The group then converges on the capacity this move leaves it with, through the
        // same path every other change to its size takes: the parked instances no longer
        // count towards it, so a park without the decrement launches their replacements
        // and one with it launches nothing.
        self.apply_capacity(engine, group_name, new_desired_capacity, &cause, now);
        Ok(activities)
    }
}
