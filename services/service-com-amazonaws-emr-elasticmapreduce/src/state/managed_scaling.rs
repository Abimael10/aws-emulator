// SPDX-License-Identifier: CC0-1.0

//! The MANAGED SCALING activity of a cluster: the capacity its managed scaling policy keeps
//! its core and task collections at.
//!
//! One concern of this service's state, over the policy `PutManagedScalingPolicy` and
//! `RunJobFlow` attach ([`ClusterRecord::managed_scaling_policy`]). A policy is not merely
//! recorded: "managed scaling policy for an Amazon EMR cluster. The policy specifies the
//! limits for resources that can be added or terminated from a cluster. The policy only
//! applies to the core and task nodes. The master node cannot be scaled after initial
//! configuration" (`ManagedScalingPolicy` doc, Smithy model) — so a cluster carrying one has
//! its core and task capacity moved by Amazon EMR itself, and never its primary.
//!
//! WHAT it is moved to comes from the policy's compute limits: "the upper boundary of Amazon
//! EC2 units … managed scaling activities are not allowed beyond this boundary" and the same
//! for the lower one (`ComputeLimits$MaximumCapacityUnits`, `$MinimumCapacityUnits`), with
//! "the upper boundary of Amazon EC2 units for core node type in a cluster"
//! (`$MaximumCoreCapacityUnits`) bounding the part of it the core collection may hold. The
//! units are the policy's own: "it is measured through vCPU cores or instances for instance
//! groups and measured through units for instance fleets"
//! (`ComputeLimits$UnitType`), which is what [`compute_limits_unit_type`] names.
//!
//! WHEN it is moved is the DEMAND the cluster is under. Amazon EMR scales out for work that
//! is waiting on capacity; what this emulator observes of that is the work the cluster's queue
//! still holds ([`ClusterRecord::carries_submitted_work`]), since nothing here measures the
//! YARN containers a real scaling decision reads. So a cluster with work outstanding has
//! capacity added up to the ceiling its policy publishes, and a cluster with none is left
//! exactly as it is: no capacity is RELEASED for a utilisation this emulator does not measure.
//!
//! Neither boundary is applied to a capacity a CALLER asked for. The limits bound "the managed
//! scaling ACTIVITY of a cluster" (`ComputeLimits` doc) rather than what a
//! `ModifyInstanceGroups` request asks a collection for, so a target above the ceiling is left
//! where the caller put it — and `MinimumCapacityUnits` is read the same way: it is the floor
//! an ACTIVITY may not go below, not capacity provisioned over a collection a caller has just
//! sized, so a cluster under no demand is left as it is rather than raised to it.
//!
//! The activity is the CLOCK's, not a caller's: the next one comes due a span after the last
//! ([`MANAGED_SCALING_EVALUATION_SPAN`]) whether or not anybody looks, which is why
//! [`ClusterRecord::advance_to`] applies it with every other transition the clock owes. It
//! moves a collection's TARGET; the instances that target asks for are provisioned by the one
//! settling of capacity every other target move goes through
//! ([`super::settle_cluster_capacity`]).

use super::*;

use crate::ops::types::{ComputeLimits, InstanceFleetModifyConfig};

/// `com.amazonaws.emr#ComputeLimitsUnitType`: "the unit type used for specifying a managed
/// scaling policy" (`smithy_query(query="shape", shape="ComputeLimitsUnitType")`).
pub mod compute_limits_unit_type {
    /// Units are instances, which is how an instance-group cluster states them.
    pub const INSTANCES: &str = "Instances";
    /// Units are vCPU cores, which an instance-group cluster may state them in instead.
    pub const VCPU: &str = "VCPU";
    /// Units are instance fleet units, which is how an instance-fleet cluster states them.
    pub const INSTANCE_FLEET_UNITS: &str = "InstanceFleetUnits";
}

/// How long after the last one the next managed scaling activity of a cluster comes due.
///
/// CHOSEN, not documented: the real service evaluates a cluster's metrics continuously and
/// takes minutes to add capacity. It is the span every other clock-owed move of this service
/// takes ([`TRANSITION_SPAN`]) rather than a longer one of its own, because an activity is not
/// a state a client waits on — it is how often the policy is RE-READ, and a cluster that is
/// already at its ceiling moves nothing however often that happens. It is deliberately
/// non-zero, so a caller reading straight after attaching a policy still sees the capacity the
/// cluster was launched with, and the capacity each activity adds is bounded
/// ([`MAX_INSTANCES_PER_SCALING_ACTIVITY`]). Stored as a due instant — nothing sleeps.
pub const MANAGED_SCALING_EVALUATION_SPAN: Duration = TRANSITION_SPAN;

/// How many instances ONE managed scaling activity adds to a collection.
///
/// CHOSEN, not documented: Amazon EMR adds capacity in increments and evaluates again, so a
/// cluster far below its ceiling is taken there over several activities rather than in one.
/// It also bounds what ONE caller pays for an activity it made due, since every added
/// instance is a launch in the service that owns instances.
pub const MAX_INSTANCES_PER_SCALING_ACTIVITY: i32 = 8;

/// One collection a managed scaling activity may move, and the units one of its instances
/// counts as.
#[derive(Debug, Clone)]
struct ScalableCollection {
    /// The collection this is, by id — an instance group's, or an instance fleet's.
    id: String,
    /// Whether it is an instance FLEET, whose capacity is stated in units rather than in
    /// instances ([`ClusterRecord::add_fleet_units`]).
    is_fleet: bool,
    /// Whether it is the CORE collection, which is what
    /// `ComputeLimits$MaximumCoreCapacityUnits` bounds.
    is_core: bool,
    /// The units ONE instance of it counts as, under the policy's unit type.
    units_per_instance: i64,
    /// The units it is asking for now.
    held_units: i64,
    /// The instances it is asking for now, for a collection that states them (a group).
    target: i32,
}

impl ClusterRecord {
    /// Whether this cluster has work OUTSTANDING: a step of its queue that has not finished —
    /// "a list of steps to run" (`RunJobFlow$Steps` doc) and everything `AddJobFlowSteps` has
    /// added to it, less the ones that have reached a terminal state
    /// ([`StepRecord::is_terminal`]).
    ///
    /// This is the demand a managed scaling activity reads (see the module docs): the
    /// applications a cluster's steps submit are what asks its core and task collections for
    /// capacity, and a cluster whose queue holds nothing still to run asks for none.
    pub fn carries_submitted_work(&self) -> bool {
        self.steps.iter().any(|step| !step.is_terminal())
    }

    /// Apply the managed scaling activity this cluster's policy already owes, and nothing
    /// else.
    ///
    /// The activity was owed at the instant [`MANAGED_SCALING_EVALUATION_SPAN`] after the
    /// last one whether or not anybody looked, so a reporting operation may apply it. A
    /// cluster carrying no policy owes none, and neither does one that is not ACTIVE: a
    /// cluster on its way out releases its whole capacity rather than scaling it.
    pub(crate) fn apply_due_managed_scaling(&mut self, now: SystemTime) {
        let Some(limits) = self
            .managed_scaling_policy
            .as_ref()
            .and_then(|policy| policy.compute_limits.clone())
        else {
            self.managed_scaling_due_at = None;
            return;
        };
        if !cluster_state::ACTIVE.contains(&self.status.state.as_str()) {
            self.managed_scaling_due_at = None;
            return;
        }
        let Some(due) = self.managed_scaling_due_at else {
            // The policy has just been attached: the first activity is due a span from now,
            // so the cluster is still reporting the capacity it was launched with.
            self.managed_scaling_due_at = Some(now + MANAGED_SCALING_EVALUATION_SPAN);
            return;
        };
        if now < due {
            return;
        }
        self.managed_scaling_due_at = Some(now + MANAGED_SCALING_EVALUATION_SPAN);
        self.scale_to_compute_limits(&limits, now);
    }

    /// Add the capacity the activity that is due asks for, as far as `limits` admits.
    fn scale_to_compute_limits(&mut self, limits: &ComputeLimits, now: SystemTime) {
        // Amazon EMR scales out for work that is waiting on capacity, and this emulator
        // measures no utilisation to scale IN on (see the module docs), so a cluster that
        // has been given no work is left exactly as it is.
        if !self.carries_submitted_work() {
            return;
        }
        let Some(scalable) = self.scalable_collections(&limits.unit_type) else {
            return;
        };
        // What the cluster's core and task collections are asking for now, in the policy's
        // own units.
        let held: i64 = scalable.iter().fold(0i64, |total, collection| {
            total.saturating_add(collection.held_units)
        });
        let maximum = i64::from(limits.maximum_capacity_units.max(0));
        if maximum < i64::from(limits.minimum_capacity_units.max(0)) {
            // A policy whose boundaries cross admits no capacity at all, so no activity
            // follows from it.
            return;
        }
        // "Managed scaling activities are not allowed beyond this boundary"
        // (`ComputeLimits$MaximumCapacityUnits` doc): the ceiling is where the activity
        // stops, so a cluster already holding it is left as it is — which is also what a
        // caller-submitted target above it is left as, since the limits bound the ACTIVITY
        // rather than what a caller asks a collection for.
        if held >= maximum {
            return;
        }
        self.add_scaling_units(
            &scalable,
            maximum - held,
            maximum_core_units(limits, maximum),
            now,
        );
    }

    /// The collections one activity may move, and the units one instance of each counts as —
    /// or `None` when the policy's unit type says nothing about this cluster's collections.
    ///
    /// "The policy only applies to the core and task nodes. The master node cannot be scaled
    /// after initial configuration" (`ManagedScalingPolicy` doc), so the primary collection is
    /// never among them.
    ///
    /// WHICH units a collection is measured in is the policy's unit type: "it is measured
    /// through vCPU cores or instances for instance groups and measured through units for
    /// instance fleets" (`ComputeLimits$MaximumCapacityUnits` doc). So an instance GROUP is
    /// measured in instances — one unit each — or in the vCPU cores of its own instance type
    /// ([`vcpu_for_size`]), and an instance FLEET in the units its target capacities are
    /// stated in, where one instance of the type it provisions supplies "the number of units
    /// that a provisioned instance of this type provides toward fulfilling the target
    /// capacities" (`InstanceTypeConfig$WeightedCapacity` doc). A unit type that measures the
    /// other collection kind, and a group instance type whose cores this service derives
    /// nothing for, leave the cluster unscaled rather than scaled by a count assumed for it.
    fn scalable_collections(&self, unit_type: &str) -> Option<Vec<ScalableCollection>> {
        let mut scalable: Vec<ScalableCollection> = Vec::new();
        for group in &self.groups {
            if !instance_group_type::AUTO_SCALABLE.contains(&group.config.instance_role.as_str()) {
                continue;
            }
            let units_per_instance = match unit_type {
                compute_limits_unit_type::INSTANCES => 1,
                compute_limits_unit_type::VCPU => {
                    // At least one: the units an instance counts as divide what the ceiling
                    // admits ([`Self::add_scaling_units`]), so a count of zero would name no
                    // capacity at all.
                    i64::from(instance_type_vcpu(&group.config.instance_type)?).max(1)
                }
                _ => return None,
            };
            let target = group.config.instance_count.max(0);
            scalable.push(ScalableCollection {
                id: group.id.clone(),
                is_fleet: false,
                is_core: group.config.instance_role == instance_group_type::CORE,
                units_per_instance,
                held_units: i64::from(target).saturating_mul(units_per_instance),
                target,
            });
        }
        for fleet in &self.fleets {
            if !instance_group_type::AUTO_SCALABLE
                .contains(&fleet.config.instance_fleet_type.as_str())
            {
                continue;
            }
            if unit_type != compute_limits_unit_type::INSTANCE_FLEET_UNITS {
                return None;
            }
            let held_units = i64::from(fleet.config.target_on_demand_capacity.unwrap_or(0).max(0))
                .saturating_add(i64::from(
                    fleet.config.target_spot_capacity.unwrap_or(0).max(0),
                ));
            scalable.push(ScalableCollection {
                id: fleet.id.clone(),
                is_fleet: true,
                is_core: fleet.config.instance_fleet_type == instance_group_type::CORE,
                // At least one, for the reason the vCPU reading above states.
                units_per_instance: fleet_units_per_instance(fleet).max(1),
                held_units,
                // A fleet states its capacity in units, so it asks for no instance count of
                // its own ([`fleet_launches`] derives the instances from those units).
                target: 0,
            });
        }
        (!scalable.is_empty()).then_some(scalable)
    }

    /// Add `units` of capacity across `scalable`, as far as the activity's own increment and
    /// the core boundary admit.
    ///
    /// The CORE collection is filled first and only up to `maximum_core_units` — "the upper
    /// boundary of Amazon EC2 units for core node type in a cluster"
    /// (`ComputeLimits$MaximumCoreCapacityUnits` doc) — and the task collections take what is
    /// left, which is the order Amazon EMR adds nodes in.
    fn add_scaling_units(
        &mut self,
        scalable: &[ScalableCollection],
        units: i64,
        maximum_core_units: i64,
        now: SystemTime,
    ) {
        let core_held: i64 = scalable
            .iter()
            .filter(|collection| collection.is_core)
            .fold(0i64, |total, collection| {
                total.saturating_add(collection.held_units)
            });
        let mut remaining = units;
        let mut core_room = maximum_core_units.saturating_sub(core_held).max(0);
        let mut added = 0i32;
        // The core collection first, then the task ones — each in the order the cluster
        // carries them.
        for pass_is_core in [true, false] {
            for collection in scalable
                .iter()
                .filter(|collection| collection.is_core == pass_is_core)
            {
                if remaining <= 0 || added >= MAX_INSTANCES_PER_SCALING_ACTIVITY {
                    return;
                }
                let admitted = if collection.is_core {
                    remaining.min(core_room)
                } else {
                    remaining
                };
                // The units are a BOUNDARY, so a collection whose instances do not divide
                // what is left takes only the whole instances that fit under it. The divisor
                // is the units ONE instance supplies, which is never zero — a collection
                // supplying none would name no capacity at all — and a division by one that
                // was is answered with no instance rather than performed.
                let mut instances = admitted
                    .checked_div(collection.units_per_instance)
                    .unwrap_or_default();
                instances = instances.min(i64::from(
                    MAX_INSTANCES_PER_SCALING_ACTIVITY.saturating_sub(added),
                ));
                if instances <= 0 {
                    continue;
                }
                let instances = i32::try_from(instances).unwrap_or(i32::MAX);
                let moved = if collection.is_fleet {
                    self.add_fleet_units(collection, instances)
                } else {
                    self.add_group_instances(collection, instances, now)
                };
                if moved <= 0 {
                    continue;
                }
                added = added.saturating_add(moved);
                let spent = i64::from(moved).saturating_mul(collection.units_per_instance);
                remaining = remaining.saturating_sub(spent);
                if collection.is_core {
                    core_room = core_room.saturating_sub(spent).max(0);
                }
            }
        }
    }

    /// Raise the instance GROUP `collection` by `instances` instances, answering how many it
    /// really took: "the target number of instances for the instance group"
    /// (`InstanceGroupConfig$InstanceCount` doc) is what a group states its capacity as, so
    /// the activity moves that target exactly as a resize does
    /// ([`InstanceGroupRecord::begin_resize`]) and the instances it asks for are provisioned by
    /// the one settling of capacity ([`settle_cluster_capacity`]). A group already holding as
    /// many instances as Amazon EMR provisions for one ([`MAX_INSTANCES_PER_GROUP`]) takes
    /// none.
    fn add_group_instances(
        &mut self,
        collection: &ScalableCollection,
        instances: i32,
        now: SystemTime,
    ) -> i32 {
        let target = collection
            .target
            .saturating_add(instances)
            .min(MAX_INSTANCES_PER_GROUP);
        let moved = target.saturating_sub(collection.target);
        if moved <= 0 {
            return 0;
        }
        if let Some(group) = self.group_mut(&collection.id) {
            group.begin_resize(target, now);
        }
        moved
    }

    /// Raise the instance FLEET `collection` by the units `instances` instances of it supply,
    /// answering how many instances it really took.
    ///
    /// A fleet states its capacity in UNITS, not instances: "the target capacity of On-Demand
    /// units for the instance fleet, which determines how many On-Demand Instances to
    /// provision" (`InstanceFleet$TargetOnDemandCapacity` doc), so the activity raises that
    /// target by the units those instances supply — through the one modification a fleet's
    /// capacity moves by ([`InstanceFleetRecord::apply_modification`]), so a managed scaling
    /// activity and a `ModifyInstanceFleet` resize leave the fleet the same way. The On-Demand
    /// target is the one raised, because the units a cluster provisions by default are
    /// On-Demand ones ([`market_type::ON_DEMAND`]). A fleet whose targets already need as many
    /// instances as Amazon EMR provisions for one ([`MAX_INSTANCES_PER_FLEET`]) takes none.
    fn add_fleet_units(&mut self, collection: &ScalableCollection, instances: i32) -> i32 {
        let added_units = i64::from(instances).saturating_mul(collection.units_per_instance);
        let Some(fleet) = self.fleet_mut(&collection.id) else {
            return 0;
        };
        let on_demand = i64::from(fleet.config.target_on_demand_capacity.unwrap_or(0).max(0));
        let spot = i64::from(fleet.config.target_spot_capacity.unwrap_or(0).max(0));
        let raised = on_demand.saturating_add(added_units);
        if instance_fleet_instance_ceiling_violation(
            raised,
            spot,
            fleet.config.instance_type_configs.as_deref().unwrap_or(&[]),
        )
        .is_some()
        {
            return 0;
        }
        let Ok(raised) = i32::try_from(raised) else {
            return 0;
        };
        fleet.apply_modification(&InstanceFleetModifyConfig {
            instance_fleet_id: collection.id.clone(),
            target_on_demand_capacity: Some(raised),
            ..InstanceFleetModifyConfig::default()
        });
        instances
    }
}

/// The units ONE instance the fleet `fleet` provisions supplies toward its target capacities.
///
/// Read through the ONE reading of that weight this crate has
/// ([`provisioned_weighted_capacity`]), of the type the fleet provisions — the FIRST its
/// configuration lists, which is the one this service provisions a fleet's capacity from
/// ([`fleet_launches`]) — so the units an activity adds and the instances the fleet then
/// launches for them agree.
fn fleet_units_per_instance(fleet: &InstanceFleetRecord) -> i64 {
    i64::from(provisioned_weighted_capacity(
        fleet
            .config
            .instance_type_configs
            .as_deref()
            .unwrap_or(&[])
            .first(),
    ))
}

/// "The upper boundary of Amazon EC2 units for core node type in a cluster"
/// (`ComputeLimits$MaximumCoreCapacityUnits` doc) as the policy publishes it, and the whole
/// ceiling for a policy that publishes none: the member is optional, and a policy that says
/// nothing about the core part of its capacity bounds it by the capacity itself.
fn maximum_core_units(limits: &ComputeLimits, maximum: i64) -> i64 {
    limits
        .maximum_core_capacity_units
        .map_or(maximum, |units| i64::from(units.max(0)))
}

/// The vCPU cores an instance of `instance_type` carries, read off the one size ladder this
/// service derives instance-type facts from ([`vcpu_for_size`]) — so a managed scaling
/// activity measured in vCPU cores and `ListSupportedInstanceTypes` cannot disagree about one
/// type. `None` for a type whose size that ladder does not scale.
fn instance_type_vcpu(instance_type: &str) -> Option<i32> {
    let (_, size) = instance_type.split_once('.')?;
    vcpu_for_size(size)
}
