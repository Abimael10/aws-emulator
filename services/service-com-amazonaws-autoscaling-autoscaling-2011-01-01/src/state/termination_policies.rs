// SPDX-License-Identifier: CC0-1.0

//! Which instances a scale-in of an Auto Scaling group takes out of service.
//!
//! A capacity DECREASE does not name the instances to terminate, so the group's own
//! `TerminationPolicies` decide: "A policy or a list of policies that are used to select
//! the instances to terminate. The policies are executed in the order that you list
//! them" (`smithy_query(query="doc",shape="UpdateAutoScalingGroupType",member="TerminationPolicies")`),
//! with the policies a group may name listed by that same documentation
//! ([`super::TERMINATION_POLICIES`]).
//!
//! So selection here is ONE ordered pass: the candidates — the group's instances that
//! are not already on their way out and not protected from scale-in — are ordered by
//! the FIRST policy the group lists, instances the first policy cannot tell apart are
//! ordered by the second, and so on; the scale-in then takes as many from the front of
//! that order as it has surplus. This is the one place that order is computed, so every
//! capacity decrease — `UpdateAutoScalingGroup`, `SetDesiredCapacity`, a policy a
//! caller executed, a scheduled action, a change the clock made due — selects its
//! victims the same way.
//!
//! An instance protected from scale-in is never a candidate, whatever the policies say:
//! that is what the protection means. A termination a caller asked for BY INSTANCE ID
//! does not come through here at all — it names what it terminates.
//!
//! # What each policy orders by
//!
//! * [`OLDEST_INSTANCE`] — the oldest instance of the group first, by when it was
//!   launched ([`super::StoredInstance::launched_at`]) and then by where it falls in the
//!   launch order ([`super::StoredInstance::launch_sequence`]), since the instances of
//!   one capacity change are launched one after another.
//! * [`NEWEST_INSTANCE`] — the newest instance of the group first, the mirror of it.
//! * [`CLOSEST_TO_NEXT_INSTANCE_HOUR`] — the instance with the least of its current
//!   usage hour left first, so the hour already paid for is used up
//!   ([`BILLING_HOUR`]). Instances in the same usage hour are left to the policy after
//!   it.
//! * [`OLDEST_LAUNCH_CONFIGURATION`] — the instance whose launch configuration was
//!   created earliest first, which is what phases a previous configuration out. An
//!   instance that was not launched from a launch configuration the account still holds
//!   has no such age, so this policy orders those after the ones it does rank and leaves
//!   instances of one age to the policy after it.
//! * [`OLDEST_LAUNCH_TEMPLATE`] — the instance launched from the lowest launch-template
//!   version first, which is what phases a noncurrent template out. An instance whose
//!   launch template states no numbered version (`$Latest` and `$Default` both name the
//!   version in force) is running the current one, so it is ordered after the ones this
//!   policy ranks, and instances of one version are left to the policy after it.
//! * [`DEFAULT`], [`ALLOCATION_STRATEGY`] and the Lambda-ARN form
//!   ([`super::TERMINATION_POLICY_LAMBDA_PREFIX`]) express no preference here: the
//!   default policy's own inputs (Availability Zone rebalancing, the billing hour and
//!   the launch-template age together), the allocation strategy of a mixed instances
//!   policy, and a custom policy's own function are not modelled by this emulator, so a
//!   group that names one of them falls through to the order below.
//!
//! A group's list is therefore applied as [`ORDERING_POLICIES`] narrows it: the policies
//! that order nothing are skipped, and a policy the list repeats is applied once, since
//! ordering by one key again cannot move what that same key already ordered. One
//! selection makes at most one ordering pass per [`ORDERING_POLICIES`] entry, over at
//! most the group's own instances, however long a list the group was given.
//!
//! Instances no policy of the group could tell apart are taken most recently launched
//! first — the order a group that configures nothing but [`DEFAULT`] scales in by. Only
//! the two age-ordered policies order every candidate, because the oldest (or newest)
//! instance of a group is ONE instance; every other policy orders by its own key alone,
//! so what it ties on really does reach the policy after it, and only what every policy
//! of the group tied on reaches this fallback.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::{StoredGroup, StoredInstance, StoredLaunchConfiguration};

/// The policy a group that configures none applies ([`super::DEFAULT_TERMINATION_POLICY`]).
pub const DEFAULT: &str = "Default";
/// Align the instances left to the allocation strategy of the group's mixed instances
/// policy.
pub const ALLOCATION_STRATEGY: &str = "AllocationStrategy";
/// Terminate the instances closest to the next instance hour.
pub const CLOSEST_TO_NEXT_INSTANCE_HOUR: &str = "ClosestToNextInstanceHour";
/// Terminate the newest instance of the group.
pub const NEWEST_INSTANCE: &str = "NewestInstance";
/// Terminate the oldest instance of the group.
pub const OLDEST_INSTANCE: &str = "OldestInstance";
/// Terminate the instances with the oldest launch configuration.
pub const OLDEST_LAUNCH_CONFIGURATION: &str = "OldestLaunchConfiguration";
/// Terminate the instances with the oldest launch template.
pub const OLDEST_LAUNCH_TEMPLATE: &str = "OldestLaunchTemplate";

/// The policies that ORDER the candidates, and so the only ones a scale-in has to
/// apply: the rest express no preference here (see this module's header), and a policy
/// a group repeats decides nothing its first mention did not, so the ordering passes one
/// selection makes are bounded by this list however long a `TerminationPolicies` list
/// the group was given.
const ORDERING_POLICIES: &[&str] = &[
    CLOSEST_TO_NEXT_INSTANCE_HOUR,
    NEWEST_INSTANCE,
    OLDEST_INSTANCE,
    OLDEST_LAUNCH_CONFIGURATION,
    OLDEST_LAUNCH_TEMPLATE,
];

/// The hour an instance's usage is billed in, which
/// [`CLOSEST_TO_NEXT_INSTANCE_HOUR`] orders by how much is left of.
const BILLING_HOUR: Duration = Duration::from_secs(60 * 60);

/// The instances a scale-in of `surplus_units` capacity units takes out of `group`, in the
/// order it takes them.
///
/// `surplus_units` is a capacity in the group's own units
/// ([`StoredGroup::surplus_capacity_units`]), so each candidate covers the units ITS
/// instance type is weighted at and a scale-in of a weighted group takes fewer instances
/// than the surplus names: for an unweighted group every candidate covers one unit, which is
/// the instance count this answered before any group had weights. An instance worth more
/// units than the surplus left is passed over — the group has to go on fulfilling the
/// capacity asked for, so a scale-in never takes its capacity below it.
///
/// Answers fewer instances than the surplus would pay for when the group holds fewer
/// candidates — an instance already `Terminating` is already leaving, and one protected from
/// scale-in is not chosen at all — and never names one instance twice.
///
/// `launch_configurations` is the account's own collection, which
/// [`OLDEST_LAUNCH_CONFIGURATION`] resolves an instance's configuration age against;
/// `now` is the instant the scale-in happens, which
/// [`CLOSEST_TO_NEXT_INSTANCE_HOUR`] measures the usage hour from.
pub fn scale_in_selection(
    group: &StoredGroup,
    launch_configurations: &BTreeMap<String, StoredLaunchConfiguration>,
    surplus_units: i32,
    now: SystemTime,
) -> Vec<String> {
    if surplus_units <= 0 {
        return Vec::new();
    }
    let mut ordered = candidates(group);
    // The policies are executed in the order the group lists them, so the FIRST one
    // decides and each later one only orders what the ones before it left tied. Every
    // ordering below is STABLE and keyed on what its own policy prefers and nothing
    // else, so ordering by the last policy first and by the first policy last leaves
    // exactly that precedence.
    for policy in applicable_policies(&group.termination_policies)
        .into_iter()
        .rev()
    {
        order_by(&mut ordered, policy, launch_configurations, now);
    }
    // Each instance pays for the units its own type is weighted at, so the surplus is
    // spent down the selection order rather than counted in instances.
    let mut remaining = surplus_units;
    let mut taken: Vec<String> = Vec::new();
    for instance in ordered {
        if remaining <= 0 {
            break;
        }
        let units = group.instance_capacity_units(instance);
        if units <= remaining {
            remaining -= units;
            taken.push(instance.instance_id.clone());
        }
    }
    taken
}

/// The policies of `policies` a selection has to apply, in the order they are listed.
///
/// A policy that orders nothing ([`ORDERING_POLICIES`]) and a policy the list repeats
/// are both left out: the first decides nothing, and the second decides nothing its
/// earlier mention did not, since ordering by one key again cannot move what that same
/// key already ordered. So the answer holds at most one entry per [`ORDERING_POLICIES`]
/// entry — the bound on how many ordering passes ONE selection makes, whatever list of
/// policies a caller gave the group.
fn applicable_policies(policies: &[String]) -> Vec<&str> {
    let mut applicable: Vec<&str> = Vec::with_capacity(ORDERING_POLICIES.len());
    for policy in policies {
        if applicable.len() == ORDERING_POLICIES.len() {
            // Every policy that could order anything is already in: nothing further
            // down the list can add one.
            break;
        }
        let policy = policy.as_str();
        if ORDERING_POLICIES.contains(&policy) && !applicable.contains(&policy) {
            applicable.push(policy);
        }
    }
    applicable
}

/// The instances of `group` a scale-in may choose from, most recently launched first —
/// the order a scale-in falls back to for instances none of the group's policies tells
/// apart.
fn candidates(group: &StoredGroup) -> Vec<&StoredInstance> {
    let mut candidates: Vec<&StoredInstance> = group
        .instances
        .iter()
        .filter(|instance| !instance.is_leaving_group() && !instance.protected_from_scale_in)
        .collect();
    candidates.sort_by_key(|instance| Reverse((instance.launched_at, instance.launch_sequence)));
    candidates
}

/// Order `ordered` by what `policy` prefers to terminate, leaving the instances it
/// cannot tell apart in the order they already have.
fn order_by(
    ordered: &mut [&StoredInstance],
    policy: &str,
    launch_configurations: &BTreeMap<String, StoredLaunchConfiguration>,
    now: SystemTime,
) {
    match policy {
        OLDEST_INSTANCE => {
            ordered.sort_by_key(|instance| (instance.launched_at, instance.launch_sequence));
        }
        NEWEST_INSTANCE => {
            ordered
                .sort_by_key(|instance| Reverse((instance.launched_at, instance.launch_sequence)));
        }
        CLOSEST_TO_NEXT_INSTANCE_HOUR => {
            ordered.sort_by_key(|instance| left_of_the_billing_hour(instance.launched_at, now));
        }
        OLDEST_LAUNCH_CONFIGURATION => {
            ordered.sort_by_key(|instance| launch_configuration_age(instance, launch_configurations));
        }
        OLDEST_LAUNCH_TEMPLATE => {
            ordered.sort_by_key(|instance| launch_template_age(instance));
        }
        // The default policy, the allocation strategy and a custom policy's own Lambda
        // function are not modelled: they order nothing, so the instances keep the
        // order the policies around them left them in.
        _ => {}
    }
}

/// How much of its current usage hour an instance launched at `launched_at` has left at
/// `now`: the smaller it is, the closer that instance is to the next instance hour.
fn left_of_the_billing_hour(launched_at: SystemTime, now: SystemTime) -> Duration {
    let used = now
        .duration_since(launched_at)
        .unwrap_or_default()
        .as_secs()
        % BILLING_HOUR.as_secs();
    BILLING_HOUR - Duration::from_secs(used)
}

/// When the launch configuration this instance was launched from was created, for
/// ordering by configuration age: an instance with no such configuration answers
/// `None`, which this policy orders AFTER every instance it can rank — the `true` of
/// the pair sorts last.
fn launch_configuration_age(
    instance: &StoredInstance,
    launch_configurations: &BTreeMap<String, StoredLaunchConfiguration>,
) -> (bool, SystemTime) {
    match instance
        .launch_configuration_name
        .as_deref()
        .and_then(|name| launch_configurations.get(name))
    {
        Some(configuration) => (false, configuration.created_time),
        None => (true, SystemTime::UNIX_EPOCH),
    }
}

/// The launch-template version this instance was launched from, for ordering by
/// template age: a lower version is the older template. An instance whose launch
/// template states no numbered version is running the version in force, so it is
/// ordered after every instance this policy can rank.
fn launch_template_age(instance: &StoredInstance) -> (bool, u64) {
    match instance
        .launch_template
        .as_ref()
        .and_then(|template| template.version.as_deref())
        .and_then(|version| version.parse::<u64>().ok())
    {
        Some(version) => (false, version),
        None => (true, 0),
    }
}
