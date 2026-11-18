// SPDX-License-Identifier: CC0-1.0

//! The Elastic Load Balancing v2 TARGETS an Auto Scaling group's instances occupy in the
//! target groups it is attached to — the registration that makes a load-balanced group
//! serve traffic, and the drain that takes an instance leaving the group out of rotation.
//!
//! `CreateAutoScalingGroup$TargetGroupARNs` is "the Amazon Resource Names (ARN) of the
//! target groups to associate with the Auto Scaling group. Instances are registered as
//! targets in a target group, and traffic is routed to the target group", and the same
//! association is what `AttachLoadBalancerTargetGroups` adds and
//! `DetachLoadBalancerTargetGroups` removes ("when you detach a target group, it enters the
//! Removing state while deregistering the instances in the group"). So an instance of an
//! attached group is a target of that target group, at the target group's own port, and an
//! instance on its way out of the group leaves it — through `draining`, for the group's
//! deregistration delay, exactly as `DeregisterTargets` takes a target out (ELBv2
//! `state::target_health`).
//!
//! The registrations live in the Elastic Load Balancing v2 service's OWN store, written
//! through its own seam, so a target this service registers is one that service returns
//! from its own read (`DescribeTargetHealth`), counts against its own target quota and
//! health-checks exactly as it checks a `RegisterTargets` caller's own instance target.
//! Nothing about the instance is declared here beyond where it is: an instance id names an
//! Amazon EC2 instance whose listening ports this emulator does not know, and the security
//! groups governing it are the ones its own network interface reports.
//!
//! # Reconciled, never appended
//!
//! The registrations this service owns in a target group are exactly the ones a group's
//! live instances need, so the set is RECOMPUTED from this store ([`wanted_of`]) and the
//! target groups are brought in line with it ([`reconcile`]): an instance that launched is
//! registered, an instance that is terminating or gone is deregistered, an attachment that
//! was detached takes its registrations with it, and a registration a `RegisterTargets`
//! CALLER made is never touched (it carries no `RegisteredTarget::registered_for`, which is
//! what tells the two apart).
//!
//! Only a target group whose `TargetType` is `instance` receives anything: an Auto Scaling
//! group's instances are Amazon EC2 instances, and an `ip`, `lambda` or `alb` target group
//! takes a different kind of target altogether, so none is invented for it.
//!
//! # One group at a time
//!
//! A reconcile is scoped to ONE Auto Scaling group ([`settle_group`]): a write to a group
//! owes the registrations of that group and of no other, and a pass claims only the
//! registrations whose workload names it ([`Scope::claims`]), so one group's reconcile
//! cannot touch another's. The clock pump, whose wake-up can move an instance of any group,
//! walks the groups one at a time ([`settle_every_group`]) for the same reason — so the
//! registrations ONE group's instances need is the most that is ever computed at once,
//! bounded by that group's own attachments ([`crate::state::MAX_TARGET_GROUPS_PER_GROUP`])
//! times its own instances ([`crate::state::MAX_GROUP_CAPACITY`]).
//!
//! # What an attachment is REPORTED as
//!
//! The state a client reads off an attachment ([`attachment_states`]) is derived from the
//! same two facts, every time it is asked for: the registrations the attachment owes and the
//! ones Elastic Load Balancing v2 holds. So an attachment is `Adding` while an instance of
//! the group is not registered there yet, `Added` once every one of them is, and `InService`
//! once that service reports one of those registrations healthy — nothing is stored beside
//! the attachment to say so, and no read moves it.
//!
//! # An attachment whose target group is gone
//!
//! Registering the instance is PART of bringing it into service, so an attachment whose
//! target group Elastic Load Balancing v2 no longer holds ([`unresolved_attachments`])
//! stops the launch that would have to register there: the group launches nothing and
//! records the refusal that names the target group instead
//! ([`launch_blocked_message`]), which is what a client reads off the group's scaling
//! activities.
//!
//! The ATTACHMENT itself is left exactly as the client made it. An attachment is the
//! client's own association, removed by the call that removes associations
//! (`DetachLoadBalancerTargetGroups`, `DetachTrafficSources`), so a group goes on
//! reporting it and goes on refusing the launch until the client takes it off — which is
//! what makes the remediation a client performs (detach the target group that is gone,
//! attach one that exists, scale out again) land, and keeps a detach of what the group
//! still reports from being refused as naming no attachment of the group.
//!
//! # A target group this endpoint could never register in
//!
//! A target group is identified within ONE Region and ONE account, and the Auto Scaling
//! group it serves is the one in that same Region, so an association whose ARN carries
//! another Region or account — or is no target group ARN at all — names nothing that could
//! ever take a registration. Such a value is refused on the INPUT
//! ([`first_unattachable_target_group`]), before the group is touched, rather than stored as
//! attachment that can never resolve.
//!
//! LOCKING. A reconcile reads the wanted set under this service's own lock, RELEASES it,
//! and only then takes the Elastic Load Balancing v2 store's lock for the one pass that
//! registers and deregisters; nothing awaits while either is held. A read of that store
//! ([`unresolved_attachments`]) takes its lock for the read alone and locks nothing of
//! this service inside it, so the order is the same one. Elastic Load Balancing v2
//! never reaches this service, so the two locks are never taken in the other order and no
//! cycle exists.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state as elbv2;

use crate::state::{AutoScalingState, StoredGroup};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances,
/// which is the only kind an Auto Scaling group's instances can be registered in
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` of a traffic source whose `Identifier` is a target
/// group ARN of Elastic Load Balancing v2 — "`elbv2` if `Identifier` is the ARN of an
/// Application Load Balancer, Gateway Load Balancer, or Network Load Balancer target group".
pub const TRAFFIC_SOURCE_TYPE_ELBV2: &str = "elbv2";

/// One registration an Auto Scaling group owes in one Elastic Load Balancing v2 target
/// group.
///
/// Every field is read off a stored group and one of its stored instances, so a registration
/// stands for an instance a `DescribeAutoScalingGroups` of the same instant reports — and an
/// Amazon EC2 instance of the account — and never a second, invented copy of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRegistration {
    /// The target group the instance is registered in.
    pub target_group_arn: String,
    /// The instance's own id, which is what an `instance` target group's targets are
    /// identified by.
    pub instance_id: String,
    /// The Availability Zone the instance runs in, as the group holds it.
    pub availability_zone: String,
    /// The workload this registration stands for, which is what lets this service take its
    /// own registrations out again and leave every other registration alone
    /// ([`workload`]).
    pub workload: String,
}

/// The workload identifier a registration of `instance_id` for the group `group_arn`
/// carries: the group the instance belongs to and the instance itself.
///
/// Built from the group's ARN, so it names a resource of THIS service and two groups never
/// claim one another's registrations ([`Scope::claims`]).
pub fn workload(group_arn: &str, instance_id: &str) -> String {
    format!("{group_arn}/{instance_id}")
}

/// The group ARN a workload [`workload`] built names: everything before the instance id it
/// ends with.
///
/// Read from the RIGHT, because a group name may itself contain a `/`
/// (`AutoScalingGroupName` admits every ASCII character 33 to 126 but the colon), so only
/// the last separator is the one this form added.
fn group_arn_of(workload: &str) -> Option<&str> {
    workload.rsplit_once('/').map(|(group_arn, _)| group_arn)
}

/// Which registrations one reconcile may add and remove.
///
/// A registration another emulated service's scheduler made through the same seam, and one a
/// `RegisterTargets` caller made, are claimed by neither variant: both are left exactly as
/// they are.
#[derive(Debug, Clone)]
enum Scope {
    /// The registrations of ONE Auto Scaling group, named by the suffix its ARN ends with
    /// ([`crate::state::group_arn`]) — what a write to that group owes, and nothing else.
    /// It claims the group's registrations whether or not the group still exists, so a
    /// deleted group's targets drain.
    OneGroup { arn_suffix: String },
}

impl Scope {
    /// The scope of a reconcile of the group `group_name`.
    fn of_group(group_name: &str) -> Self {
        Self::OneGroup {
            arn_suffix: format!(":autoScalingGroupName/{group_name}"),
        }
    }

    /// Whether a registration standing for `workload` is one this reconcile may act on.
    fn claims(&self, workload: &str) -> bool {
        match self {
            Self::OneGroup { arn_suffix } => {
                group_arn_of(workload).is_some_and(|group_arn| group_arn.ends_with(arn_suffix))
            }
        }
    }
}

/// Every Elastic Load Balancing v2 target group the group `group` is ATTACHED to, in
/// attachment order and once each however many times it is named.
///
/// A target group is counted from both forms the attachment takes — the group's own
/// `TargetGroupARNs` and an `elbv2` traffic source — because either one associates that
/// target group with the group.
pub fn attached_arns(group: &StoredGroup) -> Vec<&str> {
    let mut arns: Vec<&str> = Vec::new();
    let attached = group.attached_target_groups().iter().map(String::as_str);
    let from_traffic_sources = group
        .traffic_sources
        .iter()
        .flatten()
        .filter(|source| {
            source
                .r#type
                .as_deref()
                .is_some_and(|kind| kind == TRAFFIC_SOURCE_TYPE_ELBV2)
        })
        .map(|source| source.identifier.as_str());
    for arn in attached.chain(from_traffic_sources) {
        if !arns.contains(&arn) {
            arns.push(arn);
        }
    }
    arns
}

/// What a load-balancer attachment of an Auto Scaling group has to NAME: an Elastic Load
/// Balancing v2 TARGET GROUP, whose ARN carries that service's own namespace and the
/// `targetgroup` resource type (`CreateTargetGroup` mints
/// `arn:aws:elasticloadbalancing:<region>:<account>:targetgroup/<name>/<id>`). A value of
/// any other form names no target group of any account.
const TARGET_GROUP: ::service_common::arn::Requirement = ::service_common::arn::Requirement {
    vendor: elbv2::SERVICE_NAMESPACE,
    resource_type: Some("targetgroup"),
    label: "an Elastic Load Balancing target group",
};

/// Whether `arn` names a target group an Auto Scaling group of the Region `region` can be
/// associated with: a target group ARN ([`TARGET_GROUP`]) whose Region and account are the
/// ones the request is addressed to.
///
/// A target group is identified WITHIN one Region and one account, and the group it serves
/// is the one in that same Region, so an ARN carrying any other Region or account names
/// nothing this endpoint could ever register an instance in — as unresolvable here as a
/// value that is no ARN at all.
fn is_attachable_target_group(region: &str, arn: &str) -> bool {
    ::service_common::arn::parse(arn).is_some_and(|parsed| {
        TARGET_GROUP.admits(arn)
            && parsed.region == region
            && parsed.account == ::engine::emulator_account_id()
    })
}

/// The FIRST entry of `requested` that names no target group an Auto Scaling group of the
/// Region `region` can be associated with, in request order — what makes a request naming it
/// a bad REQUEST rather than one the store could not satisfy. `None` when every entry is one
/// such a group may hold.
pub fn first_unattachable_target_group<'a, I>(region: &str, requested: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    requested
        .into_iter()
        .find(|arn| !is_attachable_target_group(region, arn))
}

/// The message Amazon EC2 Auto Scaling refuses a request whose `TargetGroupARNs` names a
/// target group it cannot resolve with — the whole sentence is fixed text, exactly as the
/// same refusal for a Classic Load Balancer names no load balancer either.
///
/// The refusal is for the request as a WHOLE: a call that mixes resolvable ARNs with one
/// that is not attaches none of them, so the message says what was wrong with the request
/// rather than reporting a partial result.
pub const UNATTACHABLE_TARGET_GROUPS_MESSAGE: &str =
    "Provided Target Groups may not be valid. Please ensure they exist and try again.";

/// Whether a traffic source's identifier names an Elastic Load Balancing v2 TARGET GROUP, so
/// it has to resolve to one.
///
/// `TrafficSourceIdentifier$Type` says what the identifier is — "`elbv2` if `Identifier` is
/// the ARN of an Application Load Balancer, Gateway Load Balancer, or Network Load Balancer
/// target group", "`elb` if `Identifier` is the name of a Classic Load Balancer",
/// "`vpc-lattice` if `Identifier` is the ARN of a VPC Lattice target group" — and it is
/// "required if the identifier is the name of a Classic Load Balancer", so a request may leave
/// it out for an ARN. An identifier the request typed as `elbv2` is one, and one that is
/// WRITTEN as an Elastic Load Balancing target group ARN is one whatever the request left
/// out; nothing else is.
pub fn names_a_target_group(source_type: Option<&str>, identifier: &str) -> bool {
    match source_type.map(str::trim) {
        Some(TRAFFIC_SOURCE_TYPE_ELBV2) => true,
        Some(stated) if !stated.is_empty() => false,
        _ => TARGET_GROUP.admits(identifier),
    }
}

/// The FIRST entry of `requested` that names no target group this endpoint could associate
/// an Auto Scaling group of the Region `region` with, in request order — either because the
/// value is not a target group ARN of that Region and account
/// ([`first_unattachable_target_group`]) or because Elastic Load Balancing v2 holds NO
/// target group under it. `None` when every entry resolves.
///
/// Resolving the ARN is what makes an attachment one that can take a registration: the
/// association is what registers the group's instances as targets of that target group, so
/// an ARN that resolves to nothing is an association no instance could ever be registered
/// through — the condition the refusal above words ("please ensure they exist").
///
/// A READ of the other service's store, under that store's lock alone, exactly as
/// [`unresolved_attachments`] reads it — this module's header states the order. On an engine
/// that carries no such store the existence half answers nothing: there is then no target
/// group this service could register into at all, so nothing may be concluded from its
/// silence and only the ARN's own form decides.
pub fn first_unresolvable_target_group<'a, Eng: Send + Sync + 'static, I>(
    engine: &Eng,
    region: &str,
    requested: I,
) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let requested: Vec<&str> = requested.into_iter().collect();
    if let Some(unattachable) = first_unattachable_target_group(region, requested.iter().copied()) {
        return Some(unattachable.to_string());
    }
    let state = elbv2::service_state(engine)?;
    let store = state.lock();
    requested
        .into_iter()
        .find(|arn| !store.target_groups.contains_key(*arn))
        .map(str::to_string)
}

/// Whether an instance running in `instance_vpc` is OUTSIDE the VPC `target_group_vpc` of a
/// target group — the one condition that makes a registration of it there impossible.
///
/// "The specified target does not exist, is not in the same VPC as the target group, or has
/// an unsupported instance type" is what Elastic Load Balancing v2 refuses a
/// `RegisterTargets` of such a target with (`InvalidTargetException`), and an Auto Scaling
/// group's own registration is the same write into the same target group: an instance of a
/// group attached to a target group of another VPC is therefore NOT registered there, while
/// the attachment itself stands exactly as the client made it.
///
/// `false` whenever the question is not decided — a target group carrying no VPC (a `lambda`
/// or `alb` group belongs to none) and an instance whose VPC the account's Amazon EC2 store
/// did not answer ([`crate::state::ec2_instances::vpcs_of`]) — because a value neither side
/// reports cannot disagree, and nothing is withheld on silence.
fn in_another_vpc(target_group_vpc: Option<&str>, instance_vpc: Option<&str>) -> bool {
    match (target_group_vpc, instance_vpc) {
        (Some(group), Some(instance)) => !group.is_empty() && !instance.is_empty() && group != instance,
        _ => false,
    }
}

/// The target groups `group` is attached to that Elastic Load Balancing v2 no longer
/// holds, in attachment order: the attachments that can take no registration at all,
/// because the target group they name has been deleted in that service since it was
/// attached.
///
/// A READ of the other service's store, under that store's lock alone — this module's
/// header states the order, and nothing of this service is locked inside it.
///
/// Answers none when that store is unreachable (the engine is not the shared in-memory
/// one): there is then no target group this service could have registered into, so no
/// attachment can be said to have lost one.
pub fn unresolved_attachments<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group: &StoredGroup,
) -> Vec<String> {
    let arns = attached_arns(group);
    if arns.is_empty() {
        return Vec::new();
    }
    let Some(state) = elbv2::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    arns.into_iter()
        .filter(|arn| !store.target_groups.contains_key(*arn))
        .map(str::to_string)
        .collect()
}

/// How Elastic Load Balancing v2 names ITSELF in the refusal a caller of it reports —
/// the service's own `cloudFormationName`
/// (`smithy_query(query="service_info")` of that service's model), which is the name an
/// AWS SDK renders in the exception it raises for a failed call.
const ELBV2_SERVICE_NAME: &str = "ElasticLoadBalancingV2";

/// The wire error code and HTTP status Elastic Load Balancing v2 answers a call naming a
/// target group it does not hold with: its `TargetGroupNotFoundException`'s own
/// `awsQueryError` code and `httpError`
/// (`smithy_query(query="shape",shape="TargetGroupNotFoundException")`).
const TARGET_GROUP_NOT_FOUND_CODE: &str = "TargetGroupNotFound";
const TARGET_GROUP_NOT_FOUND_STATUS: u16 = 400;

/// How Amazon EC2 Auto Scaling itself words a launch it could not bring into a target
/// group: the fixed text it opens the activity's `StatusMessage` with, followed by the
/// target group it could not put the instance into.
///
/// RECORDED wording, and the whole of what a client matches on — the message of the
/// activity Amazon EC2 Auto Scaling records when registering a launched instance with an
/// attachment fails reads `Failed to update target group ` and then the target group's own
/// ARN, whatever it was about that target group that made the registration impossible.
const FAILED_TARGET_GROUP_UPDATE: &str = "Failed to update target group";

/// What the scaling activity of a launch that could not be registered reports as its
/// `StatusMessage`: the target groups the launch could not be brought into, the refusal
/// Elastic Load Balancing v2 answered for them, and the launch that therefore did not
/// happen.
///
/// It opens with Amazon EC2 Auto Scaling's own fixed text for a registration that failed
/// ([`FAILED_TARGET_GROUP_UPDATE`]), naming the target groups it could not update, because
/// that is the sentence a client reads off the activity to tell a launch blocked by its
/// load balancing from one blocked by anything else.
///
/// What follows is Elastic Load Balancing v2's own refusal — its `TargetGroupNotFound`
/// wording for the identifiers a call names (`elbv2::target_groups_not_found_message`),
/// with the service, status and code an AWS SDK renders beside the message of a failed
/// call, so a client reads here exactly what it would read from a call of its own against
/// the deleted target group. The sentence that ends it is Amazon EC2 Auto Scaling's own
/// again: the launch it did not make.
pub fn launch_blocked_message(unresolved: &[String]) -> String {
    format!(
        "{FAILED_TARGET_GROUP_UPDATE} {}: {} (Service: {ELBV2_SERVICE_NAME}, Status Code: \
         {TARGET_GROUP_NOT_FOUND_STATUS}, Error Code: {TARGET_GROUP_NOT_FOUND_CODE}). Launching \
         EC2 instance failed.",
        unresolved.join(", "),
        elbv2::target_groups_not_found_message(unresolved.iter().map(String::as_str))
    )
}

/// Every registration the group `group_name` owes right now, in attachment order and, within
/// an attachment, in instance order. Empty for a name this store holds no group for, which is
/// what a deleted group owes.
///
/// An instance counts while it is not on its way out of the group: an instance that is still
/// `Pending` has launched and is being brought into service, which is what the registration
/// is part of, and one that is `Terminating` is leaving. A target group is counted from both
/// forms the attachment takes — the group's own `TargetGroupARNs` and an `elbv2` traffic
/// source — because either one associates that target group with the group, and each target
/// group is counted once however many times it is named.
pub fn wanted_of(store: &AutoScalingState, group_name: &str) -> Vec<TargetRegistration> {
    let Some(group) = store.groups.get(group_name) else {
        return Vec::new();
    };
    let mut registrations = Vec::new();
    for arn in attached_arns(group) {
        for instance in &group.instances {
            if instance.is_leaving_group() {
                continue;
            }
            registrations.push(TargetRegistration {
                target_group_arn: arn.to_string(),
                instance_id: instance.instance_id.clone(),
                availability_zone: instance.availability_zone.clone(),
                workload: workload(&group.arn, &instance.instance_id),
            });
        }
    }
    registrations
}

/// The `LoadBalancerTargetGroupState$State` / `TrafficSourceState$State` values an
/// attachment of a target group to an Auto Scaling group is reported in, from the model's
/// own catalog for them
/// (`smithy_query(query="doc",shape="LoadBalancerTargetGroupState",member="State")`).
pub const ATTACHMENT_ADDING: &str = "Adding";
pub const ATTACHMENT_ADDED: &str = "Added";
pub const ATTACHMENT_IN_SERVICE: &str = "InService";

/// The `HealthCheckType` value that makes a group's health checks Elastic Load Balancing's
/// own — "`ELB` - Use Elastic Load Balancing health checks"
/// (`crate::state::HEALTH_CHECK_TYPES`).
const HEALTH_CHECK_TYPE_ELB: &str = "ELB";

/// Whether Elastic Load Balancing health checks are enabled for the group: its
/// `HealthCheckType` names `ELB`, which the member admits beside the other types as a
/// comma-separated list ([`crate::state::is_valid_health_check_type`]).
fn elb_health_checks_enabled(group: &StoredGroup) -> bool {
    group
        .health_check_type
        .split(',')
        .any(|kind| kind.trim() == HEALTH_CHECK_TYPE_ELB)
}

/// The state each target group in `arns` is reported in for the group `group`, in the order
/// given — what `DescribeLoadBalancerTargetGroups` answers for an attachment.
///
/// DERIVED, every time, from the registrations the attachment owes and the ones Elastic Load
/// Balancing v2 actually holds, so the state follows the group's instances and that service's
/// own health checks rather than anything stored beside the attachment:
///
/// * `Adding` — "the Auto Scaling instances are being registered with the target group":
///   some instance of the group is not registered there yet, which is also what an
///   attachment whose target group that service no longer holds, and one whose target group
///   takes a different kind of target altogether, is left in.
/// * `Added` — "all Auto Scaling instances are registered with the target group", which a
///   group holding no instance satisfies with none.
/// * `InService` — "at least one Auto Scaling instance passed an `ELB` health check": one of
///   those registrations is reported healthy by the service that checks it, and Elastic Load
///   Balancing health checks are enabled for the group ("if Elastic Load Balancing health
///   checks are enabled for the Auto Scaling group, the state transitions to InService after
///   at least one Auto Scaling instance passes the health check",
///   `DescribeLoadBalancerTargetGroups`).
///
/// A READ of the other service's store, exactly as [`unresolved_attachments`] reads it: it
/// takes that store's lock for the read alone and locks nothing of this service inside it,
/// which is the order this module's header states. It writes NOTHING — a registration a
/// drain has already taken out of a target group is skipped here rather than retired, which
/// the write that owes that removal does ([`reconcile`]).
pub fn attachment_states<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group: &StoredGroup,
    arns: &[String],
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
    let Some(state) = elbv2::service_state(engine) else {
        return arns.iter().map(|_| unregistered).collect();
    };
    let elb_health_checked = elb_health_checks_enabled(group);
    let store = state.lock();
    arns.iter()
        .map(|arn| {
            let Some(target_group) = store.target_groups.get(arn) else {
                return unregistered;
            };
            // The instances of THIS group that the target group holds a live registration
            // for, and whether any of those registrations has passed a health check. A
            // registration of another group, of another emulated service's scheduler or of a
            // `RegisterTargets` caller stands for no instance of this group, so none of them
            // counts ([`Scope::claims`]), and one that is draining is leaving the target
            // group rather than registered in it.
            let mut registered: BTreeSet<&str> = BTreeSet::new();
            let mut passed_a_health_check = false;
            for target in target_group.targets.values() {
                if target.draining_until.is_some() {
                    continue;
                }
                let Some(workload) = target.registered_for.as_deref() else {
                    continue;
                };
                if !owed.contains(workload) {
                    continue;
                }
                registered.insert(workload);
                passed_a_health_check = passed_a_health_check
                    || target_group
                        .health_of(target, now)
                        .state
                        .as_deref()
                        .is_some_and(|health| health == elbv2::target_health::HEALTHY_STATE);
            }
            if registered.len() < owed.len() {
                return ATTACHMENT_ADDING;
            }
            if elb_health_checked && passed_a_health_check {
                return ATTACHMENT_IN_SERVICE;
            }
            ATTACHMENT_ADDED
        })
        .collect()
}

/// The instances of each group of `groups` whose Elastic Load Balancing registration that
/// service reports UNHEALTHY, keyed by the group's name.
///
/// "`ELB` - Use Elastic Load Balancing health checks" (`HealthCheckType`): a group configured
/// that way takes the verdict of the load balancer checking its instances as the health of
/// those instances, so an instance whose target that service reports unhealthy in ANY target
/// group the group is attached to is one the group's own health check has found out of
/// service. The verdict itself is never computed here — it is read from the service that
/// performs the checks ([`elbv2::target_health::TargetGroupRecord::health_of`]), exactly as
/// [`attachment_states`] reads the healthy half of it — so the two services report one
/// registration's health one way.
///
/// A group that does NOT derive its instances' health from that service (its `HealthCheckType`
/// names only `EC2`, or it is attached to nothing) is absent from the answer rather than
/// present with an empty set: there is no verdict of that service for its instances, so a
/// caller has nothing to apply to them. A registration that is draining is leaving the target
/// group and is no verdict about the instance either.
///
/// A READ of the other service's store, under that store's lock alone and taken ONCE for every
/// group given — this module's header states the order — and nothing of this service is locked
/// inside it. It writes NOTHING.
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
        .filter(|group| elb_health_checks_enabled(group) && !attached_arns(group).is_empty())
        .collect();
    if deriving.is_empty() {
        return BTreeMap::new();
    }
    let Some(state) = elbv2::service_state(engine) else {
        return BTreeMap::new();
    };
    let store = state.lock();
    deriving
        .into_iter()
        .map(|group| {
            // Which instance of THIS group each registration stands for, named by the
            // workload the registration carries: a registration of another group, of another
            // emulated service's scheduler or of a `RegisterTargets` caller stands for no
            // instance of this group and is read for nobody ([`Scope::claims`]).
            let instances: BTreeMap<String, &str> = group
                .active_instances()
                .map(|instance| {
                    (
                        workload(&group.arn, &instance.instance_id),
                        instance.instance_id.as_str(),
                    )
                })
                .collect();
            let mut unhealthy: BTreeSet<String> = BTreeSet::new();
            for arn in attached_arns(group) {
                let Some(target_group) = store.target_groups.get(arn) else {
                    continue;
                };
                for target in target_group.targets.values() {
                    if target.draining_until.is_some() {
                        continue;
                    }
                    let Some(instance_id) = target
                        .registered_for
                        .as_deref()
                        .and_then(|workload| instances.get(workload))
                    else {
                        continue;
                    };
                    if target_group.health_of(target, now).state.as_deref()
                        == Some(elbv2::target_health::UNHEALTHY_STATE)
                    {
                        unhealthy.insert((*instance_id).to_string());
                    }
                }
            }
            (group.name.clone(), unhealthy)
        })
        .collect()
}

/// Bring the Elastic Load Balancing v2 target groups in line with what the group
/// `group_name` now holds: the ONE call a write that moved that group's instances or its
/// attachments makes for it.
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
    let wanted = wanted_of(&state.lock(), group_name);
    reconcile(engine, group_name, &wanted, SystemTime::now());
}

/// Settle every group this store holds, ONE GROUP AT A TIME: what the clock pump owes, whose
/// wake-up can have moved an instance of any group.
///
/// The group names are read under this store's lock and the guard released before the first
/// reconcile, and each group is then settled on its own — so the registrations of one group
/// is the most this pass ever computes at once, however many groups the store holds (itself
/// bounded by [`crate::state::MAX_GROUPS`]).
pub fn settle_every_group<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = crate::state::service_state(engine) else {
        return;
    };
    let names: Vec<String> = state.lock().groups.keys().cloned().collect();
    for name in names {
        settle_group(engine, &name);
    }
}

/// Make the Elastic Load Balancing v2 registrations of the group `group_name` exactly
/// `wanted`, in that service's own store through the one shared engine.
///
/// A registration of that group that `wanted` no longer holds is DEREGISTERED the way
/// `DeregisterTargets` deregisters one: it enters `draining` and leaves the target group once
/// that group's `deregistration_delay.timeout_seconds` has passed, which is the contract a
/// client that drains connections before an instance goes away waits on. A registration of
/// any other group, of another emulated service's scheduler, or of a `RegisterTargets`
/// caller is left exactly as it is ([`Scope::claims`]).
///
/// Nothing happens when the Elastic Load Balancing v2 store is unreachable (the engine is
/// not the shared in-memory one): there is then no target group to register into and none
/// this service could have registered into, so there is nothing to add and nothing to clean
/// up — which is why this answers nothing rather than failing the request that called it.
///
/// A registration is withheld from a target group of ANOTHER VPC ([`in_another_vpc`]): the
/// instance could not be registered there by a caller either, so it is not registered here.
pub fn reconcile<Eng: Send + Sync + 'static>(
    engine: &Eng,
    group_name: &str,
    wanted: &[TargetRegistration],
    now: SystemTime,
) {
    let Some(state) = elbv2::service_state(engine) else {
        return;
    };
    // Where each instance of the wanted set RUNS, read from the account's own Amazon EC2
    // store BEFORE the Elastic Load Balancing v2 store is locked, so the two stores are
    // never held at once — the lock order this module's header states.
    let vpcs = crate::state::ec2_instances::vpcs_of(
        engine,
        wanted
            .iter()
            .map(|registration| registration.instance_id.as_str()),
    );
    let scope = Scope::of_group(group_name);
    // The wanted set INDEXED by the target group it belongs to, once, so the pass below
    // costs each target group only what that target group itself holds rather than a walk
    // over every registration of the group.
    let mut by_target_group: BTreeMap<&str, Vec<&TargetRegistration>> = BTreeMap::new();
    for registration in wanted {
        by_target_group
            .entry(registration.target_group_arn.as_str())
            .or_default()
            .push(registration);
    }
    let mut store = state.lock();
    // The registrations this reconcile ADDED, each as the key the target group holds it
    // under: the write that creates a registration owes it its first health check, and
    // nothing else is checked for it.
    let mut added: Vec<(String, elbv2::TargetKey)> = Vec::new();
    for (arn, group) in store.target_groups.iter_mut() {
        // What the clock already owes this group: a registration whose drain is over has
        // left it, so it occupies neither a key nor the group's quota.
        group.settle_draining(now);
        // The registrations wanted in THIS target group, each under the key the group would
        // store it at — an instance target takes the target group's own port, which is where
        // "the load balancer routes requests to registered targets using the protocol and
        // port for the target group".
        let keys: Vec<(elbv2::TargetKey, &TargetRegistration)> = by_target_group
            .get(arn.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            // A target group of another VPC takes none of this group's instances.
            .filter(|registration| {
                !in_another_vpc(
                    group.vpc_id.as_deref(),
                    vpcs.get(&registration.instance_id).map(String::as_str),
                )
            })
            .map(|registration| {
                (
                    (registration.instance_id.clone(), group.port),
                    *registration,
                )
            })
            .collect();
        // Every registration of THIS group that the set above no longer holds.
        for (key, workload) in group.scheduler_registrations() {
            if !scope.claims(&workload) {
                continue;
            }
            if !keys.iter().any(|(held, _)| *held == key) {
                group.deregister(&key, now);
            }
        }
        // An Amazon EC2 instance is a target of an `instance` group alone.
        if group.target_type != TARGET_TYPE_INSTANCE {
            continue;
        }
        for (key, registration) in keys {
            // Whether the group ALREADY holds this registration decides how to read the
            // answer below: a key that is already there is the steady state, while a key
            // that is not and still is not afterwards is a registration the group had no
            // room for.
            let held = group.targets.contains_key(&key);
            let registered = group.register_for(
                &elbv2::SchedulerRegistration {
                    id: registration.instance_id.clone(),
                    // The target group's own port, which `register_for` fills in.
                    port: None,
                    availability_zone: Some(registration.availability_zone.clone()),
                    workload: registration.workload.clone(),
                    // An instance id names no workload whose listening ports this emulator
                    // knows, and the security groups governing it are the ones its own
                    // network interface reports — the same two unknowns a `RegisterTargets`
                    // caller's instance target carries.
                    serves_ports: Vec::new().into(),
                    guarded_by: Vec::new().into(),
                },
                now,
            );
            // A group already holding its quota of targets takes no further registration, so
            // the instance is reachable through no target: said rather than left to be read
            // as an instance nothing routes to.
            if !held && !registered {
                ::tracing::warn!(
                    target_group = %arn,
                    instance = %registration.instance_id,
                    "an Auto Scaling instance could not be registered as a target: the target \
                     group holds its quota of targets"
                );
            }
            if registered {
                added.push((arn.clone(), key));
            }
        }
    }
    // The registrations this reconcile made are CHECKED at once, the way the Elastic Load
    // Balancing v2 write that registers a caller's own targets checks them: the state a
    // registration is reported in must follow those checks from the moment it is registered
    // rather than wait for a clock wake-up. Only the registrations this call added, so a
    // reconcile that moved nothing checks nothing; with the guard this reconcile held
    // RELEASED, so the check takes the store's lock itself.
    drop(store);
    elbv2::check_registrations(engine, &added);
}
