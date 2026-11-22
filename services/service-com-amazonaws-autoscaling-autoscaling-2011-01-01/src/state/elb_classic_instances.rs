// SPDX-License-Identifier: CC0-1.0

//! The Classic Load Balancer REGISTRATIONS an Auto Scaling group's instances hold in the
//! load balancers it is attached to — the registration that puts a load-balanced group's
//! instances behind the load balancer, and the deregistration that takes an instance
//! leaving the group out of rotation.
//!
//! `CreateAutoScalingGroup$LoadBalancerNames` is "a list of Classic Load Balancers
//! associated with this Auto Scaling group", and the same association is what
//! `AttachLoadBalancers` adds — "Amazon EC2 Auto Scaling registers the running instances
//! with these Classic Load Balancers" — and `DetachLoadBalancers` removes. So an instance of
//! an attached group is a registered instance of that load balancer, and an instance on its
//! way out of the group is deregistered from it.
//!
//! The registrations live in the Elastic Load Balancing service's OWN store, written through
//! that service's own seam, so an instance this service registers is one that service
//! returns from its own read (`DescribeInstanceHealth`), counts against its own registration
//! quota and health-checks exactly as it checks a `RegisterInstancesWithLoadBalancer`
//! caller's own instance.
//!
//! # Reconciled, never appended
//!
//! The registrations this service owns in a load balancer are exactly the ones a group's live
//! instances need, so the set is RECOMPUTED from this store ([`wanted_of`]) and the load
//! balancers are brought in line with it ([`reconcile`]): an instance that launched is
//! registered, an instance that is terminating or gone is deregistered, an attachment that
//! was detached takes its registrations with it, and a registration a
//! `RegisterInstancesWithLoadBalancer` CALLER made is never touched — it carries no workload
//! of this service ([`crate::state::elbv2_targets::belongs_to_group`] is what tells the two
//! apart, over the same workload form the target-group registrations carry).
//!
//! A pass claims only the registrations whose workload names the group being settled, so one
//! group's reconcile cannot touch another's — and a group that has been DELETED is settled
//! the same way, which is what drains the registrations it left behind.
//!
//! # What an attachment is REPORTED as
//!
//! The state a client reads off an attachment ([`attachment_states`]) is derived from the
//! same two facts, every time it is asked for: the registrations the attachment owes and the
//! ones Elastic Load Balancing holds. So an attachment is `Adding` while an instance of the
//! group is not registered there yet, `Added` once every one of them is, and `InService` once
//! that service reports one of those registrations in service — nothing is stored beside the
//! attachment to say so.
//!
//! # Lock order
//!
//! This store first, then the other services': a reconcile reads what one group owes under
//! this store's lock, releases it, and only then writes Elastic Load Balancing's store; a
//! read of an attachment's state is already under this store's lock, and takes the Amazon EC2
//! store's and then the Elastic Load Balancing store's inside it. No pass holds two of the
//! other services' locks at once.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::state as elb;

use crate::state::elbv2_targets::{
    belongs_to_group, elb_health_checks_enabled, workload, ATTACHMENT_ADDED, ATTACHMENT_ADDING,
    ATTACHMENT_IN_SERVICE,
};
use crate::state::{AutoScalingState, StoredGroup};

/// The `TrafficSourceIdentifier$Type` of a traffic source whose `Identifier` is the NAME of a
/// Classic Load Balancer — "`elb` if `Identifier` is the name of a Classic Load Balancer".
pub const TRAFFIC_SOURCE_TYPE_ELB: &str = "elb";

/// The message Amazon EC2 Auto Scaling refuses a request whose `LoadBalancerNames` names a
/// load balancer it cannot resolve with: the whole sentence pair is fixed text, naming no
/// value from the request.
///
/// The refusal is for the request as a WHOLE: a call that mixes load balancers the account
/// holds with one it does not attaches none of them, so the message says what was wrong with
/// the request rather than reporting a partial result.
pub const UNRESOLVABLE_LOAD_BALANCERS_MESSAGE: &str =
    "Provided Load Balancers may not be valid. Please ensure they exist and try again.";

/// One registration an Auto Scaling group owes in one Classic Load Balancer.
///
/// Both ids are read off a stored group and one of its stored instances, so a registration
/// stands for an instance a `DescribeAutoScalingGroups` of the same instant reports — and an
/// Amazon EC2 instance of the account — and never a second, invented copy of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRegistration {
    /// The load balancer the instance is registered with, by the name it is identified by
    /// within its Region.
    pub load_balancer_name: String,
    /// The instance's own id, which is what a Classic Load Balancer's registrations are
    /// identified by.
    pub instance_id: String,
    /// The workload this registration stands for, which is what lets this service take its
    /// own registrations out again and leave every other registration alone.
    pub workload: String,
}

/// Every Classic Load Balancer the group `group` is ATTACHED to, in attachment order and
/// once each however many times it is named.
///
/// A load balancer is counted from both forms the attachment takes — the group's own
/// `LoadBalancerNames` and an `elb` traffic source — because either one associates that load
/// balancer with the group.
pub fn attached_names(group: &StoredGroup) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    let attached = group.attached_load_balancers().iter().map(String::as_str);
    let from_traffic_sources = group
        .traffic_sources
        .iter()
        .flatten()
        .filter(|source| {
            source
                .r#type
                .as_deref()
                .is_some_and(|kind| kind.trim() == TRAFFIC_SOURCE_TYPE_ELB)
        })
        .map(|source| source.identifier.as_str());
    for name in attached.chain(from_traffic_sources) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every registration the group `group_name` owes right now, in attachment order and, within
/// an attachment, in instance order. Empty for a name this store holds no group for, which is
/// what a deleted group owes.
///
/// An instance counts while it is not on its way out of the group: an instance that is still
/// `Pending` has launched and is being brought into service, which is what the registration
/// is part of, and one that is `Terminating` is leaving.
pub fn wanted_of(store: &AutoScalingState, group_name: &str) -> Vec<InstanceRegistration> {
    let Some(group) = store.groups.get(group_name) else {
        return Vec::new();
    };
    let mut registrations = Vec::new();
    for name in attached_names(group) {
        for instance in &group.instances {
            if instance.is_leaving_group() {
                continue;
            }
            registrations.push(InstanceRegistration {
                load_balancer_name: name.to_string(),
                instance_id: instance.instance_id.clone(),
                workload: workload(&group.arn, &instance.instance_id),
            });
        }
    }
    registrations
}

/// The Region the group `group_name` is in, which is the Region its load balancers are
/// identified in. `None` for a name this store holds no group for.
fn region_of(store: &AutoScalingState, group_name: &str) -> Option<String> {
    store
        .groups
        .get(group_name)
        .map(|group| group.region.clone())
}

/// The FIRST entry of `requested` that names no Classic Load Balancer of the Region
/// `region`, in request order — what makes a request naming it a bad REQUEST rather than one
/// the store could not satisfy. `None` when every entry resolves.
///
/// Resolving the name is what makes an attachment one that can take a registration: the
/// association is what registers the group's instances with that load balancer, so a name
/// that resolves to nothing is an association no instance could ever be registered through —
/// the condition the refusal above words ("please ensure they exist").
///
/// A READ of the other service's store, under that store's lock alone. On an engine that
/// carries no such store this answers nothing: there is then no load balancer this service
/// could register into at all, so nothing may be concluded from its silence.
pub fn first_unresolvable_load_balancer<'a, Eng: Send + Sync + 'static, I>(
    engine: &Eng,
    region: &str,
    requested: I,
) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let state = elb::service_state(engine)?;
    let store = state.lock();
    let held = store.load_balancers_in(region);
    requested
        .into_iter()
        .find(|name| held.is_none_or(|records| !records.contains_key(*name)))
        .map(str::to_string)
}

/// The state each load balancer in `names` is reported in for the group `group`, in the order
/// given — what `DescribeLoadBalancers` answers for an attachment.
///
/// DERIVED, every time, from the registrations the attachment owes and the ones Elastic Load
/// Balancing actually holds, so the state follows the group's instances and that service's
/// own health checks rather than anything stored beside the attachment:
///
/// * `Adding` — "the Auto Scaling instances are being registered with the load balancer":
///   some instance of the group is not registered there yet, which is also what an attachment
///   whose load balancer that service no longer holds is left in.
/// * `Added` — "all Auto Scaling instances are registered with the load balancer", which a
///   group holding no instance satisfies with none.
/// * `InService` — "at least one Auto Scaling instance passed an `ELB` health check": one of
///   those registrations is reported in service by the service that checks it, and Elastic
///   Load Balancing health checks are enabled for the group ("if Elastic Load Balancing
///   health checks are enabled for the Auto Scaling group, the state transitions to InService
///   after at least one Auto Scaling instance passes the health check",
///   `DescribeLoadBalancers`).
///
/// The health checks the clock has already made due are run first, exactly as that service's
/// own `DescribeInstanceHealth` runs them: they were owed at `now` whether or not anybody
/// called, so the verdict this read reports does not depend on who read it. Nothing else is
/// written, and no attachment is moved by being read.
pub fn attachment_states<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group: &StoredGroup,
    names: &[String],
    now: SystemTime,
) -> Vec<&'static str> {
    // One registration per instance of the group that is not on its way out: the very set a
    // reconcile puts in place ([`wanted_of`]), named by the workload each registration of it
    // carries.
    let owed: BTreeSet<String> = group
        .active_instances()
        .map(|instance| workload(&group.arn, &instance.instance_id))
        .collect();
    // What an attachment is in before anything of it is registered — which is where every
    // attachment of a group that holds instances starts, and where one whose registrations
    // cannot be read stays.
    let unregistered = if owed.is_empty() {
        ATTACHMENT_ADDED
    } else {
        ATTACHMENT_ADDING
    };
    // What one check of each of the group's instances observes, read from the account's own
    // Amazon EC2 store BEFORE the Elastic Load Balancing store is locked, so the two stores
    // are never held at once — the lock order this module's header states.
    let instance_ids: BTreeSet<String> = group
        .active_instances()
        .map(|instance| instance.instance_id.clone())
        .collect();
    // What those load balancers CHECK with — their own `HealthCheck$Target`s, read under
    // that service's lock and released before anything is observed, so each check of an
    // instance is the one the load balancer performing it is configured for.
    let targets = elb::health_check_targets(
        engine,
        names.iter().map(|name| (group.region.as_str(), name.as_str())),
    );
    let observed = elb::observe_instances(engine, &instance_ids, &targets);
    let Some(state) = elb::service_state(engine) else {
        return names.iter().map(|_| unregistered).collect();
    };
    let elb_health_checked = elb_health_checks_enabled(group);
    let mut store = state.lock();
    let Some(records) = store.load_balancers_in_mut(&group.region) else {
        return names.iter().map(|_| unregistered).collect();
    };
    names
        .iter()
        .map(|name| {
            let Some(record) = records.get_mut(name) else {
                return unregistered;
            };
            // The checks this load balancer's registrations owed at `now`, run before their
            // verdicts are read.
            record.settle_instance_health(&observed, now);
            // The instances of THIS group that the load balancer holds a registration for,
            // and whether any of those registrations is in service. A registration of
            // another group, of another emulated service's scheduler or of a
            // `RegisterInstancesWithLoadBalancer` caller stands for no instance of this
            // group, so none of them counts.
            let mut registered: BTreeSet<String> = BTreeSet::new();
            let mut one_in_service = false;
            for (instance_id, held_for) in record.instances.scheduler_registrations() {
                if !owed.contains(&held_for) {
                    continue;
                }
                registered.insert(held_for);
                one_in_service = one_in_service
                    || record.instance_health(&instance_id).state == elb::STATE_IN_SERVICE;
            }
            if registered.len() < owed.len() {
                return ATTACHMENT_ADDING;
            }
            if elb_health_checked && one_in_service {
                return ATTACHMENT_IN_SERVICE;
            }
            ATTACHMENT_ADDED
        })
        .collect()
}

/// The instances of each group of `groups` whose Classic Load Balancer registration that
/// service reports as having FAILED the load balancer's health check, keyed by the group's
/// name.
///
/// "`ELB` - Use Elastic Load Balancing health checks" (`HealthCheckType`): a group configured
/// that way takes the verdict of the load balancer checking its instances as the health of
/// those instances, and a Classic Load Balancer the group is attached to checks them exactly
/// as a target group does — so this is the Classic half of the verdict
/// [`crate::state::elbv2_targets::elb_unhealthy_instances`] reads for a target group, in the
/// same form, and the two are read together
/// ([`AutoScalingState::reconcile_elb_instance_health`]).
///
/// The verdict itself is never computed here. It is read from the service that performs the
/// checks ([`elb::LoadBalancer::instance_health`]), whose own thresholds decide it, and only a
/// registration that failed them for the INSTANCE's own doing ([`elb::REASON_CODE_INSTANCE`])
/// counts: a registration whose checks are still running is out of service for Elastic Load
/// Balancing's own reason ([`elb::REASON_CODE_ELB`]), which is no failed check, exactly as a
/// target group's `initial` is none.
///
/// A group that does NOT derive its instances' health from that service (its `HealthCheckType`
/// names only `EC2`, or it is attached to no Classic Load Balancer) is absent from the answer
/// rather than present with an empty set: there is no verdict of that service for its
/// instances, so a caller has nothing to apply to them.
///
/// The checks the clock has already made due are run first, exactly as [`attachment_states`]
/// and that service's own `DescribeInstanceHealth` run them: they were owed at `now` whether
/// or not anybody called. What one of them observes is read from the account's own Amazon EC2
/// store BEFORE the Elastic Load Balancing store is locked, and once for every group given, so
/// the two stores are never held at once — the lock order this module's header states.
pub fn elb_unhealthy_instances<'a, Eng: Send + Sync + 'static, I>(
    engine: &Eng,
    groups: I,
    now: SystemTime,
) -> BTreeMap<String, BTreeSet<String>>
where
    I: IntoIterator<Item = &'a StoredGroup>,
{
    // Only the groups that take their instances' health from that service, and only while
    // they are attached to something that could report one: for every other group this reads
    // nothing and locks nothing at all.
    let deriving: Vec<&StoredGroup> = groups
        .into_iter()
        .filter(|group| elb_health_checks_enabled(group) && !attached_names(group).is_empty())
        .collect();
    if deriving.is_empty() {
        return BTreeMap::new();
    }
    // The store those verdicts live in, resolved BEFORE anything is observed: with no such
    // store there is no verdict to read for anybody, so the Amazon EC2 store is not locked for
    // an answer nothing would be read out of. Resolving the handle takes no lock of either.
    let Some(state) = elb::service_state(engine) else {
        return BTreeMap::new();
    };
    // What one check of each of those groups' instances observes, read once for all of them —
    // and before the Elastic Load Balancing store is locked, so the two stores are never held
    // at once.
    let instance_ids: BTreeSet<String> = deriving
        .iter()
        .flat_map(|group| group.active_instances())
        .map(|instance| instance.instance_id.clone())
        .collect();
    // What the load balancers checking them check WITH, read under that service's lock and
    // released before anything is observed: a check of an instance is the one the load
    // balancer performing it is configured for.
    let placed: Vec<(String, String)> = deriving
        .iter()
        .flat_map(|group| {
            attached_names(group)
                .into_iter()
                .map(|name| (group.region.clone(), name.to_string()))
        })
        .collect();
    let targets = elb::health_check_targets(
        engine,
        placed
            .iter()
            .map(|(region, name)| (region.as_str(), name.as_str())),
    );
    let observed = elb::observe_instances(engine, &instance_ids, &targets);
    let mut store = state.lock();
    deriving
        .into_iter()
        .map(|group| {
            // Which instance of THIS group each registration stands for, named by the
            // workload the registration carries: a registration of another group, of another
            // emulated service's scheduler or of a `RegisterInstancesWithLoadBalancer` caller
            // stands for no instance of this group and is read for nobody.
            let owed: BTreeSet<String> = group
                .active_instances()
                .map(|instance| workload(&group.arn, &instance.instance_id))
                .collect();
            let mut unhealthy: BTreeSet<String> = BTreeSet::new();
            // A Region this store holds no load balancer of at all reports no failed check of
            // anybody: the group is answered with the empty set — present, because it IS a
            // group that derives its instances' health from that service — exactly as an
            // attachment whose load balancer that service no longer holds reports none.
            let Some(records) = store.load_balancers_in_mut(&group.region) else {
                return (group.name.clone(), unhealthy);
            };
            for name in attached_names(group) {
                let Some(record) = records.get_mut(name) else {
                    continue;
                };
                // The checks this load balancer's registrations owed at `now`, run before
                // their verdicts are read.
                record.settle_instance_health(&observed, now);
                for (instance_id, held_for) in record.instances.scheduler_registrations() {
                    if !owed.contains(&held_for) {
                        continue;
                    }
                    let reported = record.instance_health(&instance_id);
                    if reported.state == elb::STATE_OUT_OF_SERVICE
                        && reported.reason_code == elb::REASON_CODE_INSTANCE
                    {
                        unhealthy.insert(instance_id);
                    }
                }
            }
            (group.name.clone(), unhealthy)
        })
        .collect()
}

/// Bring the Classic Load Balancers in line with what the group `group_name` now holds: the
/// ONE call a write that moved that group's instances or its attachments makes for it.
///
/// Called with NO guard of this store held: it takes this store's lock to read the set that
/// group needs ([`wanted_of`]), releases it, and only then writes the other service's store
/// ([`reconcile`]) — the lock order this module's header states.
///
/// Reconciling is idempotent, so a call that moved nothing writes nothing: the set it reads
/// is the set already in place.
pub fn settle_group<Eng: Send + Sync + 'static>(engine: &Eng, group_name: &str) {
    let Some(state) = crate::state::service_state(engine) else {
        return;
    };
    // The guard this statement takes is a TEMPORARY: it is released at the end of the
    // statement, so nothing below holds it.
    let (region, wanted) = {
        let store = state.lock();
        (region_of(&store, group_name), wanted_of(&store, group_name))
    };
    reconcile(
        engine,
        group_name,
        region.as_deref(),
        &wanted,
        SystemTime::now(),
    );
}

/// Make the Classic Load Balancer registrations of the group `group_name` exactly `wanted`,
/// in Elastic Load Balancing's own store through the one shared engine.
///
/// A registration of that group that `wanted` no longer holds is DEREGISTERED the way
/// `DeregisterInstancesFromLoadBalancer` deregisters one. A registration of another group, of
/// another emulated service's scheduler, or of a `RegisterInstancesWithLoadBalancer` caller
/// is left exactly as it is.
///
/// `region` is the Region the group is in, and `None` for a group this store no longer holds:
/// a registration is only ever ADDED in the group's own Region, because a load balancer's
/// name identifies it within one Region, while the registrations the group left behind are
/// claimed wherever they are — which is what drains a deleted group's.
///
/// Nothing happens when the Elastic Load Balancing store is unreachable (the engine is not
/// the shared in-memory one): there is then no load balancer to register into and none this
/// service could have registered into, so there is nothing to add and nothing to clean up.
pub fn reconcile<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group_name: &str,
    region: Option<&str>,
    wanted: &[InstanceRegistration],
    now: SystemTime,
) {
    let Some(state) = elb::service_state(engine) else {
        return;
    };
    // The wanted set INDEXED by the load balancer it belongs to, once, so the pass below
    // costs each load balancer only what that load balancer itself holds rather than a walk
    // over every registration of the group.
    let mut by_load_balancer: BTreeMap<&str, Vec<&InstanceRegistration>> = BTreeMap::new();
    for registration in wanted {
        by_load_balancer
            .entry(registration.load_balancer_name.as_str())
            .or_default()
            .push(registration);
    }
    let mut store = state.lock();
    for (held_region, records) in store.regions.iter_mut() {
        let group_region = region.is_some_and(|region| region == held_region);
        for (name, record) in records.iter_mut() {
            // The registrations wanted in THIS load balancer: none at all unless it is one of
            // the group's own Region.
            let keys: Vec<&InstanceRegistration> = if group_region {
                by_load_balancer
                    .get(name.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .to_vec()
            } else {
                Vec::new()
            };
            // Every registration of THIS group that the set above no longer holds.
            let retired: BTreeSet<String> = record
                .instances
                .scheduler_registrations()
                .into_iter()
                .filter(|(instance_id, held_for)| {
                    belongs_to_group(held_for, group_name)
                        && !keys
                            .iter()
                            .any(|registration| registration.instance_id == *instance_id)
                })
                .map(|(instance_id, _)| instance_id)
                .collect();
            record.deregister_instances(&retired);
            for registration in keys {
                record.register_instance_for(
                    &registration.instance_id,
                    &registration.workload,
                    now,
                );
            }
        }
    }
}
