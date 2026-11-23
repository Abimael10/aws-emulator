// SPDX-License-Identifier: CC0-1.0

//! The instances a cluster's COLLECTIONS are owed: the capacity each one asks for against
//! the instances it is holding, and what reaching the asked-for capacity takes.
//!
//! One concern of this service's state, and the other half of
//! [`super::cluster_instances`]: that module says what one instance IS and asks Amazon EC2
//! for the instances a launch plans, and this one says how many a collection should be
//! holding at any instant and settles the difference.
//!
//! "The target number of instances for the instance group"
//! (`InstanceGroupConfig$InstanceCount` doc) and "the target capacity of On-Demand units for
//! the instance fleet, which determines how many On-Demand Instances to provision"
//! (`InstanceFleet$TargetOnDemandCapacity` doc) are TARGETS: whatever moves one — the launch
//! that stated it, a `ModifyInstanceGroups` / `ModifyInstanceFleet` resize, or a managed
//! scaling activity ([`super::managed_scaling`]) — Amazon EMR then provisions or releases
//! Amazon EC2 instances until the collection holds that many. So a raised target really
//! launches the additional instances, which `ListInstances` then reports for the collection,
//! and a lowered one really terminates the instances past it; a collection whose target did
//! not move is left exactly as it is, which is what makes this settling idempotent.
//!
//! The instances themselves are Amazon EC2's resource, so they are launched and terminated
//! THERE ([`super::launch_cluster_instances`], [`super::terminate_cluster_instances`]) and
//! this service keeps the ids Amazon EC2 minted: capacity it did not provide is not recorded.

use super::*;

/// What ONE cluster's collections ask of Amazon EC2 right now.
#[derive(Debug, Default)]
pub struct CapacityChange {
    /// The instances to launch, one entry per collection and market that is short of its
    /// target.
    pub launches: Vec<InstanceLaunch>,
    /// The instance records to release, by their Amazon EMR ids: the newest instances of a
    /// collection that is holding more than its target asks for.
    pub released: Vec<String>,
}

impl CapacityChange {
    /// Whether nothing is owed, which is the state a settled cluster is in.
    pub fn is_empty(&self) -> bool {
        self.launches.is_empty() && self.released.is_empty()
    }
}

impl ClusterRecord {
    /// The instances of the instance group `group_id` this cluster is still holding, in
    /// launch order.
    ///
    /// An instance that has been terminated is no part of its collection's capacity — "all
    /// Amazon EC2 instances have been terminated" is what the end of a collection means — so
    /// what a collection HOLDS is its live records ([`ClusterInstanceRecord::is_live`]).
    pub fn live_group_instances(&self, group_id: &str) -> Vec<&ClusterInstanceRecord> {
        self.ec2_instances
            .iter()
            .filter(|instance| {
                instance.is_live() && instance.instance_group_id.as_deref() == Some(group_id)
            })
            .collect()
    }

    /// The instances of the instance fleet `fleet_id` this cluster is still holding under the
    /// market `market`, in launch order — a fleet provisions its capacity per market, so each
    /// market's instances are counted against that market's target.
    pub fn live_fleet_instances(
        &self,
        fleet_id: &str,
        market: &str,
    ) -> Vec<&ClusterInstanceRecord> {
        self.ec2_instances
            .iter()
            .filter(|instance| {
                instance.is_live()
                    && instance.instance_fleet_id.as_deref() == Some(fleet_id)
                    && instance.market == market
            })
            .collect()
    }

    /// What this cluster's collections ask of Amazon EC2 right now: the instances each one is
    /// short of its target, and the records each one is holding past it.
    ///
    /// Nothing is owed for a cluster that is not ACTIVE: one that is shutting down or gone
    /// releases its whole capacity rather than provisioning any
    /// ([`ClusterRecord::advance_to`]), so a target on one of its collections asks for
    /// nothing. The launches are cut to the room this cluster has left
    /// ([`MAX_INSTANCES_PER_CLUSTER`]) before anything is asked of Amazon EC2, for the reason
    /// [`launches_within`] states.
    pub fn capacity_change(&self) -> CapacityChange {
        let mut change = CapacityChange::default();
        if !cluster_state::ACTIVE.contains(&self.status.state.as_str()) {
            return change;
        }
        let mut planned: Vec<InstanceLaunch> = Vec::new();
        for group in &self.groups {
            let held = self.live_group_instances(&group.id);
            let target = group.config.instance_count.max(0);
            let short = target.saturating_sub(i32::try_from(held.len()).unwrap_or(i32::MAX));
            if short > 0 {
                if let Some(mut launch) = group_launch(group) {
                    launch.count = short;
                    planned.push(launch);
                }
            }
            change
                .released
                .extend(released_ids(&held, usize::try_from(target).unwrap_or(0)));
        }
        for fleet in &self.fleets {
            // Both markets a fleet provisions, so a target lowered to ZERO units releases the
            // instances that market was holding: a market the plan names no launch for is a
            // market asking for NO instance, which is a target like any other.
            let planned_for_market = fleet_launches(fleet);
            for market in [market_type::ON_DEMAND, market_type::SPOT] {
                let held = self.live_fleet_instances(&fleet.id, market);
                let asked = planned_for_market
                    .iter()
                    .find(|launch| launch.market == market)
                    .map_or(0, |launch| launch.count.max(0));
                let short = asked.saturating_sub(i32::try_from(held.len()).unwrap_or(i32::MAX));
                if short > 0 {
                    if let Some(launch) = planned_for_market
                        .iter()
                        .find(|launch| launch.market == market)
                    {
                        let mut launch = launch.clone();
                        launch.count = short;
                        planned.push(launch);
                    }
                }
                change.released.extend(released_ids(
                    &held,
                    usize::try_from(asked).unwrap_or_default(),
                ));
            }
        }
        change.launches = launches_within(
            planned,
            MAX_INSTANCES_PER_CLUSTER.saturating_sub(self.ec2_instances.len()),
        );
        change
    }

    /// Release the instance records `released` names at `at`, answering the Amazon EC2
    /// instances they held — which is what Amazon EMR then terminates there.
    pub fn release_instances(&mut self, released: &[String], at: SystemTime) -> Vec<String> {
        let mut ec2_instance_ids: Vec<String> = Vec::new();
        for instance in &mut self.ec2_instances {
            if instance.is_live() && released.contains(&instance.id) {
                instance.release_at(at);
                ec2_instance_ids.push(instance.ec2_instance_id.clone());
            }
        }
        ec2_instance_ids
    }
}

/// The ids of the instances `held` carries past `target`: the NEWEST ones, so a collection
/// that is scaled back keeps the instances it has been running longest.
fn released_ids(held: &[&ClusterInstanceRecord], target: usize) -> Vec<String> {
    held.iter()
        .skip(target)
        .map(|instance| instance.id.clone())
        .collect()
}

/// Settle the capacity of every collection of the cluster `cluster_id`: launch the instances
/// its collections are short of their targets, and terminate the ones they hold past them.
///
/// Called by the operations that MOVE a target (`ModifyInstanceGroups`,
/// `ModifyInstanceFleet`) and by the ones that report a cluster's capacity — a managed
/// scaling activity the clock already made due ([`super::managed_scaling`]) moves a target
/// too, and the instances that target asks for are owed from that instant whether or not
/// anybody looked. Nothing is launched or terminated for a cluster whose collections are
/// already holding what they ask for, so a call that finds nothing owed writes nothing.
///
/// Neither call into the service that owns instances is made under this service's lock, and
/// one cluster's capacity is settled once at a time ([`claimed_program`]): the instances are
/// real resources of another service, so two callers reporting the same cluster do not ask
/// for the same capacity twice.
pub async fn settle_cluster_capacity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_id: &str,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // Asked BEFORE the claim is taken, so a cluster with nothing owed — every cluster that
    // is settled — costs one read of the store and no claim at all.
    let owed = {
        let mut store = state.lock();
        store
            .cluster_mut(cluster_id, now)
            .is_some_and(|cluster| !cluster.capacity_change().is_empty())
    };
    if !owed {
        return;
    }
    let Some(_claim) = claimed_program(&format!("{cluster_id}/capacity")) else {
        return;
    };
    // Read again under the claim, so what is acted on is the state no other caller is
    // already settling.
    let planned = {
        let mut store = state.lock();
        store
            .cluster_mut(cluster_id, now)
            .map(|cluster| (cluster.capacity_change(), LaunchPlacement::of(cluster)))
    };
    let Some((change, placement)) = planned else {
        return;
    };
    // The records are released FIRST and under this service's lock, so the instances Amazon
    // EC2 is asked to terminate are exactly the ones this cluster has stopped reporting as
    // live, and no second caller plans the same release.
    if !change.released.is_empty() {
        let ec2_instance_ids = {
            let mut store = state.lock();
            store
                .clusters
                .get_mut(cluster_id)
                .map(|cluster| cluster.release_instances(&change.released, now))
                .unwrap_or_default()
        };
        terminate_cluster_instances(ctx, region, &ec2_instance_ids).await;
    }
    if !change.launches.is_empty() {
        let launched = launch_cluster_instances(ctx, region, &placement, &change.launches).await;
        record_launched_instances(engine, cluster_id, launched, now).await;
    }
}

/// Record every instance Amazon EC2 launched for the cluster `cluster_id`, with the Amazon
/// EBS volumes the launch created for each one.
///
/// The ONE place a launched instance becomes a record of this service, reached by every path
/// that launches capacity — the cluster's own launch, an added collection and a settled
/// resize alike — so no path can record an instance without the volumes Amazon EC2 answered
/// for it ([`super::instance_ebs_volumes`]), which the launch itself read back.
pub async fn record_launched_instances<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cluster_id: &str,
    launched: Vec<LaunchedInstance>,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    for launched in launched {
        // The room this cluster has left is asked BEFORE an id is minted, so a launch past
        // the ceiling burns none ([`MAX_INSTANCES_PER_CLUSTER`]).
        if store
            .clusters
            .get(cluster_id)
            .is_none_or(|cluster| cluster.ec2_instances.len() >= MAX_INSTANCES_PER_CLUSTER)
        {
            return;
        }
        let instance_id = store.mint_cluster_instance_id();
        if let Some(cluster) = store.clusters.get_mut(cluster_id) {
            cluster
                .ec2_instances
                .push(ClusterInstanceRecord::launched(instance_id, launched, now));
        }
    }
}
