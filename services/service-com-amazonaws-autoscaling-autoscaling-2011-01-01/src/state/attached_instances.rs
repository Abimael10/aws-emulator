// SPDX-License-Identifier: CC0-1.0

//! Bringing the account's own Amazon EC2 instances INTO an Auto Scaling group: the one
//! transition `AttachInstances` performs.
//!
//! # The rule this module implements
//!
//! "Attaches one or more EC2 instances to the specified Auto Scaling group. When you attach
//! instances, Amazon EC2 Auto Scaling increases the desired capacity of the group by the
//! number of instances being attached. If the number of instances being attached plus the
//! desired capacity of the group exceeds the maximum size of the group, the operation
//! fails" (`smithy_query(query="doc",shape="AttachInstances")`). So one attach is two facts
//! about the group, and nothing else: each named instance becomes an instance OF the group
//! — reported [`LIFECYCLE_PENDING`] until it is in service, the way every instance a group
//! brings into service is — and the group's desired capacity rises by how many were
//! attached, which is what stops the group launching a replacement for any of them
//! ([`AutoScalingState::apply_capacity`], the one convergence every change to a group's
//! size goes through).
//!
//! # The instances an attach may take
//!
//! This service does not launch them: the ids a request names are the account's OWN Amazon
//! EC2 instances ([`super::ec2_instances`]), so an attach can only take one the account
//! actually holds. The requirements the user guide states for attaching an instance are
//! exactly that: the instance is in the `running` state, it is not a member of another Auto
//! Scaling group, and it was launched into one of the Availability Zones the group is
//! configured for. An id failing any of them refuses the call WHOLE — nothing is attached
//! and the group's capacity does not move — because the capacity a successful attach
//! reports is the number of instances it took, so a call that could take only some of them
//! has no capacity to report.
//!
//! "If there are target groups attached to your Auto Scaling group, the instances are also
//! registered with the target groups" (same documentation), which falls out of the group
//! HOLDING them — the registrations a group's attachments owe are recomputed from its
//! instances ([`super::elbv2_targets`]) — rather than being written again here.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::{
    ec2_instances, AutoScalingState, StoredActivity, StoredInstance, HEALTH_STATUS_HEALTHY,
    LIFECYCLE_PENDING, LIFECYCLE_TRANSITION,
};

/// The `Activity$Description` the scaling activity for one attach carries, naming the
/// instance it brought into the group — the one wording for it in this crate.
#[must_use]
pub fn attach_description(instance_id: &str) -> String {
    format!("Attaching EC2 instance: {instance_id}")
}

/// What the activities of an attach record as the reason they began: a caller named the
/// instances, which is what tells an attached instance apart from one the group launched
/// for itself.
///
/// Reads as the subject of [`AutoScalingState::capacity_change_cause`], since an attach
/// always moves the group's desired capacity.
const ATTACH_REASON: &str = "a user request explicitly attached instances to the group";

/// Why an attach could not be made, for the operation to state the refusal each situation
/// belongs to.
#[derive(Debug)]
pub enum AttachRefusal {
    /// The account's own Amazon EC2 store holds no instance under that id, so there is
    /// nothing to attach and no instance the group could report.
    UnknownInstance(String),
    /// The instance is not in the `running` state Amazon EC2 reports it has to be in to be
    /// attached; carries the state it IS in, as Amazon EC2 names it.
    NotRunning {
        /// The instance the request named.
        instance_id: String,
        /// The `InstanceStateName` Amazon EC2 reports for it.
        state: String,
    },
    /// The instance is already an instance of an Auto Scaling group; carries the group
    /// that holds it, which may be the one the request named.
    AlreadyInGroup {
        /// The instance the request named.
        instance_id: String,
        /// The group that already holds it.
        group_name: String,
    },
    /// The instance was launched into an Availability Zone the group is not configured
    /// for, so the group could never have placed it there itself.
    ZoneNotConfigured {
        /// The instance the request named.
        instance_id: String,
        /// The zone Amazon EC2 holds it in.
        availability_zone: String,
        /// The zones the group IS configured for, which is the list the refusal states so
        /// the caller can read a zone the attach would be admitted from out of it.
        group_zones: Vec<String>,
    },
    /// Attaching the named instances would take the group's desired capacity past its own
    /// `MaxSize`, which the operation documents as a failure.
    PastMaxSize {
        /// The three sizes the group is configured with now, which is what the caller has to
        /// change for the attach to be admitted.
        sizes: super::GroupSizes,
        /// How many instances the request asked to attach.
        attaching: usize,
    },
}

impl AttachRefusal {
    /// The sentence this refusal carries.
    ///
    /// Each is about ONE thing the request asked for: the fixed text states the rule and
    /// the variable parts — the instance id, the group name, the state Amazon EC2 reports
    /// the instance in, the zone it is in and the sizes the attach crossed — come from the
    /// request and from what this service and Amazon EC2 hold.
    #[must_use]
    pub fn message(&self, group_name: &str) -> String {
        match self {
            // THIS service's own sentence for an id it could resolve to no instance of the
            // account: an attach names the instance it cannot take, bracketed, and states
            // that the id is not one this operation can act on. Amazon EC2 resolved the id,
            // but the refusal is Auto Scaling's, so its own wording is what a caller reads.
            Self::UnknownInstance(instance_id) => {
                format!("Instance [{instance_id}] is invalid.")
            }
            Self::NotRunning { instance_id, state } => format!(
                "Instance {instance_id} is in {state} state, not in {} state, and cannot be \
                 attached.",
                ec2_instances::running_state()
            ),
            Self::AlreadyInGroup {
                instance_id,
                group_name,
            } => super::instance_already_in_group_message(instance_id, group_name),
            // The group's configured zone list is the actionable half of this refusal — it is
            // what a caller picks a placeable instance from — so the sentence states that
            // list, the group it belongs to, and the instance with the zone it occupies. The
            // list is comma-separated the way every set of values this service states is
            // ([`super::joined_values`]), and the zone the instance occupies is bracketed
            // beside it.
            Self::ZoneNotConfigured {
                instance_id,
                availability_zone,
                group_zones,
            } => format!(
                "The list of availability zones [{}] for the group {group_name} does not \
                 contain the zone occupied by the instance {instance_id} \
                 [{availability_zone}]",
                super::joined_values(group_zones.iter().cloned())
            ),
            // The group's own sizes do not admit the attach, so the refusal is the one both
            // moves of a group's instances state ([`super::group_size_bounds`]): the sizes it
            // holds now, the instances this call asked for, and the remedy.
            Self::PastMaxSize { sizes, attaching } => super::group_sizes_refuse_move_message(
                group_name,
                *sizes,
                super::GroupSizeMove::Attach,
                *attaching,
            ),
        }
    }
}

impl AutoScalingState {
    /// Attach the instances `instance_ids` names to the group and answer the scaling
    /// activities that did it.
    ///
    /// THE ONE attach transition: every rule of it is decided here — which instances may be
    /// taken, the capacity the group is left converging on and the activity each attach
    /// records — so an operation only has to state the refusal.
    ///
    /// An id the request repeats names ONE instance. A request naming no instance at all
    /// attaches none and records no activity: nothing was owed. Answers
    /// [`AttachRefusal`] without changing anything when any named instance cannot be
    /// taken, or when the attach would take the group past its own `MaxSize`.
    pub fn attach_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_ids: &[String],
        now: SystemTime,
    ) -> Result<Vec<StoredActivity>, AttachRefusal> {
        let Some(group) = self.groups.get(group_name) else {
            return Ok(Vec::new());
        };
        // An id the request repeats names one instance, so it is attached once and counts
        // once towards the capacity the group is left with. Each id is admitted through a
        // SET, so one pass over the request costs the same per id however many it carries.
        let mut admitted: BTreeSet<&str> = BTreeSet::new();
        let mut wanted: Vec<String> = Vec::new();
        for instance_id in instance_ids {
            if admitted.insert(instance_id.as_str()) {
                wanted.push(instance_id.clone());
            }
        }
        if wanted.is_empty() {
            return Ok(Vec::new());
        }

        let arn = group.arn.clone();
        let zones = group.availability_zones.clone();
        let protected = group.new_instances_protected_from_scale_in;
        let desired_capacity = group.desired_capacity;
        let (min_size, max_size) = (group.min_size, group.max_size);

        // An instance ALREADY held by a group is not one an attach may take: an instance
        // belongs to at most one Auto Scaling group, and the group that holds it is the one
        // the refusal names — the request's own group when it is attaching one twice. The
        // instances of a group's WARM POOL are instances of that group too ("in the warm
        // pool or in any state except Terminated for the Auto Scaling group"), so one of
        // those is not free either.
        //
        // WHICH group holds each instance of the account is read in ONE pass, as the index
        // the requested ids are then looked up in: the cost of a call is the account's
        // inventory once, never that inventory once per id the request carries.
        let holders: BTreeMap<&str, &str> = self
            .groups
            .values()
            .flat_map(|holder| {
                holder
                    .instances
                    .iter()
                    .chain(holder.warm_pool_instances().iter())
                    .map(move |instance| {
                        (instance.instance_id.as_str(), holder.name.as_str())
                    })
            })
            .collect();
        for instance_id in &wanted {
            if let Some(holder) = holders.get(instance_id.as_str()) {
                return Err(AttachRefusal::AlreadyInGroup {
                    instance_id: instance_id.clone(),
                    group_name: (*holder).to_string(),
                });
            }
        }
        // What the account's own Amazon EC2 store holds for each of them, read in ONE pass
        // so the cost of a call does not grow with the account's inventory.
        let held = ec2_instances::held(engine, &wanted);
        for instance_id in &wanted {
            let Some(instance) = held.get(instance_id) else {
                return Err(AttachRefusal::UnknownInstance(instance_id.clone()));
            };
            if instance.state != ec2_instances::running_state() {
                return Err(AttachRefusal::NotRunning {
                    instance_id: instance_id.clone(),
                    state: instance.state.clone(),
                });
            }
            if !zones.contains(&instance.availability_zone) {
                return Err(AttachRefusal::ZoneNotConfigured {
                    instance_id: instance_id.clone(),
                    availability_zone: instance.availability_zone.clone(),
                    group_zones: zones.clone(),
                });
            }
        }

        // What the attached instances will contribute to the group's capacity, in the units
        // its size is stated in: an instance of a type the group weights brings its own
        // weight's worth of capacity with it, which is what the group's size rises by.
        let units = wanted.iter().fold(0i32, |units, instance_id| {
            let instance_type = held.get(instance_id).map(|held| held.instance_type.as_str());
            units.saturating_add(group.capacity_units_for_type(instance_type))
        });
        let after = desired_capacity.saturating_add(units);
        if after > max_size {
            return Err(AttachRefusal::PastMaxSize {
                sizes: super::GroupSizes {
                    min_size,
                    max_size,
                    desired_capacity,
                },
                // The instances the attach would take, each id once: an id the request
                // repeats names one instance, so it is one instance the refusal counts.
                attaching: wanted.len(),
            });
        }

        let due = now + LIFECYCLE_TRANSITION;
        // The group's own view of each attached instance: the id and the placement Amazon
        // EC2 holds for it, what Amazon EC2 says it runs, and the transient state every
        // instance coming into service is reported in. The launch-source members stay
        // unfilled — this group launched none of them — and the instance keeps the launch
        // time Amazon EC2 recorded, which is the age a scale-in's age-ordered policies
        // order it by ([`super::termination_policies`]).
        let first_sequence = self.next_instance_launch_seq;
        self.next_instance_launch_seq =
            first_sequence.saturating_add(u64::try_from(wanted.len()).unwrap_or(0));
        let attached: Vec<StoredInstance> = wanted
            .iter()
            .enumerate()
            .filter_map(|(offset, instance_id)| {
                let instance = held.get(instance_id)?;
                Some(StoredInstance {
                    instance_id: instance_id.clone(),
                    availability_zone: instance.availability_zone.clone(),
                    lifecycle_state: LIFECYCLE_PENDING.to_string(),
                    health_status: HEALTH_STATUS_HEALTHY.to_string(),
                    launch_configuration_name: None,
                    launch_template: None,
                    instance_type: Some(instance.instance_type.clone()),
                    image_id: instance.image_id.clone(),
                    protected_from_scale_in: protected,
                    launched_at: instance.launched_at,
                    launch_sequence: first_sequence
                        .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX)),
                    transition_due: Some(due),
                    // An instance the group has just taken in has failed no health check,
                    // so the group's check owes nothing on it.
                    health_check_due: None,
                    // The group's own launch lifecycle hooks are put in front of the
                    // attached instance once it is placed below
                    // ([`super::lifecycle_actions`]).
                    pending_lifecycle_actions: Vec::new(),
                    held_step: None,
                    // An instance a CALLER attaches is registered with the group's load
                    // balancing by the attach itself — `AttachInstances` is not the process
                    // that adds the instances a group launches
                    // ([`super::suspended_processes::PROCESS_ADD_TO_LOAD_BALANCER`]) — so
                    // nothing is withheld from it.
                    load_balancer_registration_withheld: false,
                })
            })
            .collect();
        let attached_ids: Vec<String> = attached
            .iter()
            .map(|instance| instance.instance_id.clone())
            .collect();
        if let Some(group) = self.groups.get_mut(group_name) {
            group.instances.extend(attached);
        }
        // An instance an attach brought into the group is coming into service, so every
        // launch lifecycle hook the group configures holds it exactly as it holds one the
        // group launched itself ([`super::lifecycle_actions`]).
        self.hold_lifecycle_transition(
            engine,
            group_name,
            super::HeldTransition::Launching,
            &attached_ids,
            now,
        );
        // Amazon EC2 Auto Scaling puts its own group tag on every instance of a group, so
        // the instance an attach took reports the group it is now part of where Amazon EC2
        // reports an instance's tags.
        ec2_instances::record_group_membership(engine, &attached_ids, group_name);

        let cause = Self::capacity_change_cause(now, ATTACH_REASON, desired_capacity, after);
        // One activity per instance attached, completing when its transient state does, so
        // the activity a client reads reports the attach it is watching.
        let activity_ids: Vec<String> = attached_ids
            .iter()
            .map(|instance_id| {
                self.record_activity(
                    engine,
                    &super::ActivityRecord {
                        group_name,
                        group_arn: &arn,
                        description: attach_description(instance_id),
                        cause: cause.clone(),
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
        // The group then converges on the capacity the attach leaves it with, through the
        // same path every other change to its size takes: the attached instances already
        // count towards it, so nothing is launched for them and nothing is scaled in to
        // make room.
        self.apply_capacity(engine, group_name, after, &cause, now);
        Ok(activities)
    }
}
