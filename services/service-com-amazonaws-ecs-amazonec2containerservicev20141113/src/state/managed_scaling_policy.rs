// SPDX-License-Identifier: CC0-1.0

//! The TARGET-TRACKING SCALING POLICY Amazon ECS owns on the Auto Scaling group of a
//! capacity provider whose managed scaling is on — and the CloudWatch alarms that policy is
//! driven by.
//!
//! # The rule this module implements
//!
//! `ManagedScaling`'s own documentation: "When managed scaling is turned on, Amazon ECS
//! manages the scale-in and scale-out actions of the Auto Scaling group. Amazon ECS manages a
//! target tracking scaling policy using an Amazon ECS managed CloudWatch metric with the
//! specified `targetCapacity` value as the target value for the metric." So a provider over a
//! caller's group with managed scaling ENABLED is a provider Amazon ECS holds a policy for,
//! on THAT group, tracking THAT metric at the provider's own `targetCapacity`:
//!
//! * the metric is the one Amazon ECS publishes for the pair it reserves capacity for — the
//!   `CapacityProviderReservation` series of the `AWS/ECS/ManagedScaling` namespace, which is
//!   counted per capacity provider per cluster, so the series is named by the two dimensions
//!   [`CAPACITY_PROVIDER_NAME_DIMENSION`] and [`CLUSTER_NAME_DIMENSION`];
//! * the target value is `managedScaling$targetCapacity`, "the target capacity utilization as
//!   a percentage for the capacity provider";
//! * the policy belongs to Amazon ECS rather than to the caller, so the caller never put it
//!   and never names it: it is created, re-targeted and removed as the provider's own
//!   configuration changes.
//!
//! The cluster is part of the metric, so the policy exists for a provider that HAS one: a
//! managed-instances-scoped provider names its cluster itself, and a provider over a group of
//! the caller's gets one when it is associated with a cluster
//! (`PutClusterCapacityProviders`). A provider associated with no cluster reserves capacity
//! for nothing, so there is no series to track and no policy — and a provider whose managed
//! scaling is off has none either: "If managed scaling is off, the user must manage the
//! scaling of the Auto Scaling group."
//!
//! # Where the policy and the alarms LIVE
//!
//! Both belong to the services that own them, so neither is a copy kept here: the policy is a
//! scaling policy of Amazon EC2 Auto Scaling's — `DescribePolicies` on the group reports it,
//! with the ARN and the alarm identity that service mints — and its alarms are CloudWatch's,
//! materialised through that service's OWN policy-alarm writer
//! ([`auto_scaling::scaling_policies::sync_policy_alarms`]), so an alarm a policy reports is
//! an alarm `DescribeAlarms` resolves.
//!
//! What Amazon ECS owns is derived from this service's records, so the reconciliation is
//! total rather than incremental ([`settle_managed_scaling_policies`]): the policies the
//! providers of the account want are written, and the ones Amazon ECS holds that no provider
//! wants any more are removed with their alarms. A policy that already states what its
//! provider asks for is left exactly as it stands, so repeating the pass changes nothing a
//! client can read.
//!
//! LOCK ORDER: this service's store is read and its lock RELEASED before the Auto Scaling
//! store's is taken, and that one is released before CloudWatch's — the same one-way order
//! [`auto_scaling_group_binding`] and [`settle_managed_capacity_scaling`] keep, which is what
//! makes a lock cycle between the stores unreachable.

use super::*;

use ::std::collections::BTreeMap;

use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::ops::types as auto_scaling_types;
use ::service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state as auto_scaling;

use auto_scaling::scaling_policies as policies;

/// The CloudWatch namespace of the metric Amazon ECS manages the scaling of a capacity
/// provider's group by: the managed-scaling series Amazon ECS publishes itself, named for the
/// service and the feature that owns it.
pub const MANAGED_SCALING_METRIC_NAMESPACE: &str = "AWS/ECS/ManagedScaling";

/// The metric itself: how much of the capacity behind a provider is reserved by the work the
/// cluster has routed through it, as a percentage — which is what a `targetCapacity` is a
/// target FOR.
pub const MANAGED_SCALING_METRIC_NAME: &str = "CapacityProviderReservation";

/// The dimension naming the capacity provider a reservation is counted for.
pub const CAPACITY_PROVIDER_NAME_DIMENSION: &str = "CapacityProviderName";

/// The dimension naming the cluster it is counted in: the series is per `(provider, cluster)`
/// pair, since one provider may be associated with more than one cluster.
pub const CLUSTER_NAME_DIMENSION: &str = "ClusterName";

/// The statistic of that series the policy tracks: the reservation is a percentage of the
/// capacity, so the average over the period is what the target is held against.
pub const MANAGED_SCALING_METRIC_STATISTIC: &str = "Average";

/// The prefix of the name Amazon ECS gives the policy it manages, which says whose policy it
/// is: the rest is an opaque id, so the name is no caller's to collide with.
pub const MANAGED_SCALING_POLICY_NAME_PREFIX: &str = "ECSManagedAutoScalingPolicy";

/// The policy ONE capacity provider wants: which group it is on, which series it tracks, and
/// the target value it holds that series at.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedScalingPolicy {
    /// The Auto Scaling group the policy is on, by NAME — the key Amazon EC2 Auto Scaling
    /// holds its groups and their policies under ([`auto_scaling_group_key`]).
    pub group_name: String,
    /// The capacity provider the policy scales for, which is one of the two dimensions of the
    /// series it tracks.
    pub provider_name: String,
    /// The cluster the provider reserves capacity for, which is the other.
    pub cluster_name: String,
    /// `managedScaling$targetCapacity`: the percentage the series is held at.
    pub target_capacity: i32,
    /// `managedScaling$instanceWarmupPeriod`, "the period of time, in seconds, after a newly
    /// launched Amazon EC2 instance can contribute to CloudWatch metrics for Auto Scaling
    /// group" — which is the warm-up a scaling policy of that group states
    /// (`ScalingPolicy$EstimatedInstanceWarmup`).
    pub instance_warmup_period: Option<i32>,
}

impl ManagedScalingPolicy {
    /// The `TargetTrackingConfiguration` this policy IS: the managed-scaling series, named by
    /// its two dimensions, held at the provider's target capacity.
    ///
    /// Scaling IN is part of what managed scaling does ("Amazon ECS manages the scale-in and
    /// scale-out actions of the Auto Scaling group"), so the configuration disables none of
    /// it, which is also the member's own default.
    fn target_tracking(&self) -> auto_scaling_types::TargetTrackingConfiguration {
        auto_scaling_types::TargetTrackingConfiguration {
            predefined_metric_specification: None,
            customized_metric_specification: Some(
                auto_scaling_types::CustomizedMetricSpecification {
                    metric_name: Some(MANAGED_SCALING_METRIC_NAME.to_string()),
                    namespace: Some(MANAGED_SCALING_METRIC_NAMESPACE.to_string()),
                    dimensions: Some(vec![
                        auto_scaling_types::MetricDimension {
                            name: CAPACITY_PROVIDER_NAME_DIMENSION.to_string(),
                            value: self.provider_name.clone(),
                        },
                        auto_scaling_types::MetricDimension {
                            name: CLUSTER_NAME_DIMENSION.to_string(),
                            value: self.cluster_name.clone(),
                        },
                    ]),
                    statistic: Some(MANAGED_SCALING_METRIC_STATISTIC.to_string()),
                    unit: None,
                    period: None,
                    metrics: None,
                },
            ),
            target_value: f64::from(self.target_capacity),
            disable_scale_in: Some(false),
        }
    }
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every managed-scaling policy the providers of this account want, one per provider whose
    /// group Amazon ECS manages the scaling of and whose cluster is known.
    ///
    /// Read under this store's lock and answered as owned values, because the policies are
    /// written against the Auto Scaling store (see the module's LOCK ORDER note).
    pub fn managed_scaling_policies(&self) -> Vec<ManagedScalingPolicy> {
        let mut wanted: Vec<ManagedScalingPolicy> = self
            .capacity_providers
            .values()
            .filter(|provider| provider.status != CAPACITY_PROVIDER_INACTIVE)
            .filter_map(|provider| self.managed_scaling_policy(provider))
            .collect();
        // One policy per group and provider, in a settled order, so what the pass writes does
        // not depend on how the store happened to iterate.
        wanted.sort_by(|left, right| {
            (&left.group_name, &left.provider_name).cmp(&(&right.group_name, &right.provider_name))
        });
        wanted
    }

    /// The policy ONE provider wants, or `None` for a provider Amazon ECS manages the scaling
    /// of nothing for: one that is not over a group of the caller's, one whose managed scaling
    /// is off ([`managed_scaling_enabled`], the same reading that decides whether a placement
    /// through it waits for capacity to be acquired), and one that reserves capacity for no
    /// cluster.
    fn managed_scaling_policy(
        &self,
        provider: &CapacityProviderRecord,
    ) -> Option<ManagedScalingPolicy> {
        let group = provider.auto_scaling_group_provider.as_ref()?;
        if !managed_scaling_enabled(provider) {
            return None;
        }
        let scaling = group.managed_scaling.as_ref();
        Some(ManagedScalingPolicy {
            group_name: auto_scaling_group_key(&group.auto_scaling_group_arn).to_string(),
            provider_name: provider.name.clone(),
            cluster_name: self.capacity_provider_cluster(provider)?,
            target_capacity: scaling
                .and_then(|scaling| scaling.target_capacity)
                .unwrap_or(MANAGED_SCALING_DEFAULT_TARGET_CAPACITY),
            instance_warmup_period: scaling.and_then(|scaling| scaling.instance_warmup_period),
        })
    }

    /// The cluster a provider reserves capacity FOR: the cluster it is scoped to where it has
    /// one, and otherwise the cluster it is associated with — the association
    /// `PutClusterCapacityProviders` writes onto the cluster, read as the provider it REFERS
    /// to so a cluster that listed the provider by ARN names the same provider its short name
    /// does.
    ///
    /// WHY ONE cluster decides it, for a provider several clusters list: the policy drives ONE
    /// Auto Scaling group's capacity, and two target-tracking policies on one group would scale
    /// it against each other — each holding its own cluster's reservation at the target and
    /// undoing the other's action — so the group carries one policy whatever a caller associated
    /// the provider with. Which cluster that is, is settled by NAME rather than left to
    /// iteration order, so two reads of the same store answer identically. The capacity itself
    /// is the group's and so is shared, which is why Amazon ECS publishes the reservation per
    /// `(provider, cluster)` pair and a provider is meant to back one cluster's capacity.
    fn capacity_provider_cluster(&self, provider: &CapacityProviderRecord) -> Option<String> {
        if let Some(cluster) = provider.cluster.as_deref() {
            return self
                .has_active_cluster(cluster)
                .then(|| cluster.to_string());
        }
        let mut associated: Vec<&str> = self
            .clusters
            .values()
            .filter(|cluster| cluster.status == CLUSTER_ACTIVE)
            .filter(|cluster| {
                cluster
                    .capacity_providers
                    .iter()
                    .any(|reference| {
                        capacity_provider_name_from_reference(reference) == provider.name
                    })
            })
            .map(|cluster| cluster.name.as_str())
            .collect();
        associated.sort_unstable();
        associated.first().map(|name| (*name).to_string())
    }
}

/// The capacity provider a stored scaling policy is Amazon ECS's own for, or `None` for a
/// policy that is not one of Amazon ECS's.
///
/// Ownership is read off the policy ITSELF rather than from a record kept here — the name
/// Amazon ECS mints for it ([`MANAGED_SCALING_POLICY_NAME_PREFIX`]) over the managed-scaling
/// series of one capacity provider — which is what lets the pass below find the policy it
/// wrote last time without keeping a second copy of Amazon EC2 Auto Scaling's state. BOTH
/// halves are required, so a policy a CALLER put through `PutScalingPolicy` is never read as
/// Amazon ECS's, whatever metric that caller chose to track: the pass would otherwise retire
/// a policy of theirs the moment no provider wanted one like it.
fn managed_scaling_provider<'a>(
    key: &policies::PolicyKey,
    policy: &'a policies::StoredScalingPolicy,
) -> Option<&'a str> {
    if policy.policy_type != policies::POLICY_TYPE_TARGET_TRACKING
        || !key
            .policy_name
            .starts_with(MANAGED_SCALING_POLICY_NAME_PREFIX)
    {
        return None;
    }
    let customized = policy
        .target_tracking_configuration
        .as_ref()?
        .customized_metric_specification
        .as_ref()?;
    if customized.namespace.as_deref() != Some(MANAGED_SCALING_METRIC_NAMESPACE)
        || customized.metric_name.as_deref() != Some(MANAGED_SCALING_METRIC_NAME)
    {
        return None;
    }
    customized
        .dimensions
        .as_ref()?
        .iter()
        .find(|dimension| dimension.name == CAPACITY_PROVIDER_NAME_DIMENSION)
        .map(|dimension| dimension.value.as_str())
}

/// What a policy STATES, as the two sides of "this policy already says what its provider
/// asks for" are compared on.
///
/// The configuration is a tree of shapes carrying a derived `Debug` rather than an equality,
/// so rendering it is what stands in for comparing it member by member — the same reading
/// [`capacity_provider_binding_fingerprint`] is written on. Comparing it is what keeps the
/// pass idempotent: a policy that is already right is not rewritten, so neither it nor its
/// alarms report a configuration this pass touched.
fn policy_fingerprint(policy: &policies::StoredScalingPolicy) -> String {
    format!(
        "{:?}|{:?}|{}",
        policy.target_tracking_configuration, policy.estimated_instance_warmup, policy.enabled
    )
}

/// Bring the managed-scaling policies of the account's capacity providers into line with the
/// providers themselves, as of `now`: write the policy each one wants onto its group, and take
/// away the ones Amazon ECS holds that no provider wants any more, with their alarms.
///
/// Called by the operations that CHANGE what a provider is — the create and update of a
/// provider, the association that gives it a cluster, and the delete that retires it — with
/// NO guard of this service's store held (see the module's LOCK ORDER note). An engine
/// carrying no Auto Scaling store holds no group to put a policy on, so nothing is owed.
pub fn settle_managed_scaling_policies<Eng: Send + Sync + 'static>(engine: &Eng, now: SystemTime) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let wanted = state.lock().managed_scaling_policies();
    let Some(auto_scaling_state) = auto_scaling::service_state(engine) else {
        return;
    };
    let mut written: Vec<(policies::PolicyKey, policies::StoredScalingPolicy)> = Vec::new();
    let mut retired: BTreeMap<policies::PolicyKey, policies::StoredScalingPolicy> = BTreeMap::new();
    {
        // Under the Auto Scaling store's lock alone: the policies are that service's own
        // records, and nothing of this service is reached while it is held.
        let mut store = auto_scaling_state.lock();
        // The policies Amazon ECS already holds there, each with the provider it is for.
        let held: Vec<(policies::PolicyKey, String)> = store
            .scaling_policies
            .iter()
            .filter_map(|(key, policy)| {
                managed_scaling_provider(key, policy)
                    .map(|provider| (key.clone(), provider.to_string()))
            })
            .collect();
        for policy in &wanted {
            let Some(region) = store
                .groups
                .get(&policy.group_name)
                .map(|group| group.region.clone())
            else {
                // A group Amazon EC2 Auto Scaling does not hold carries no policy: nothing
                // here invents a policy on a group a read of that service would not resolve.
                continue;
            };
            let existing_key = held
                .iter()
                .find(|(key, provider)| {
                    key.auto_scaling_group_name == policy.group_name
                        && provider == &policy.provider_name
                })
                .map(|(key, _)| key.clone());
            // A policy Amazon ECS has already put on this group is UPDATED under the name,
            // the ARN and the alarm identity it was created with, so a re-target changes the
            // target value of the policy the client is already reading rather than adding a
            // second one.
            let (key, existing) = match existing_key {
                Some(key) => {
                    let existing = store.scaling_policies.get(&key).cloned();
                    (key, existing)
                }
                None => {
                    if store.scaling_policies.len() >= policies::MAX_SCALING_POLICIES {
                        // The Auto Scaling store holds no more policies. The provider itself is
                        // stored and answered for, so nothing is failed for it; the policy that
                        // could not be put there is reported where a developer can see it,
                        // rather than leaving a provider with no policy and no reason.
                        ::tracing::warn!(
                            capacity_provider = policy.provider_name.as_str(),
                            auto_scaling_group = policy.group_name.as_str(),
                            "the Auto Scaling policy store is at capacity, so the managed \
                             target-tracking policy for this capacity provider was not put on \
                             its group"
                        );
                        continue;
                    }
                    let policy_name = format!(
                        "{MANAGED_SCALING_POLICY_NAME_PREFIX}-{}",
                        store.mint_resource_id()
                    );
                    (
                        policies::PolicyKey {
                            auto_scaling_group_name: policy.group_name.clone(),
                            policy_name,
                        },
                        None,
                    )
                }
            };
            let (policy_arn, created_time, alarm_id) = match existing.as_ref() {
                Some(existing) => (
                    existing.policy_arn.clone(),
                    existing.created_time,
                    existing.alarm_id.clone(),
                ),
                None => {
                    let resource_id = store.mint_resource_id();
                    let alarm_id = store.mint_resource_id();
                    (policies::policy_arn(&region, &resource_id, &key), now, alarm_id)
                }
            };
            let desired = policies::StoredScalingPolicy {
                policy_arn,
                region,
                policy_type: policies::POLICY_TYPE_TARGET_TRACKING.to_string(),
                // The members a target-tracking policy does not own are stored on none of
                // them, exactly as a put of one through that service's own API stores it
                // (`policy_members_of_type`).
                adjustment_type: None,
                min_adjustment_step: None,
                min_adjustment_magnitude: None,
                scaling_adjustment: None,
                cooldown: None,
                step_adjustments: Vec::new(),
                metric_aggregation_type: None,
                estimated_instance_warmup: policy.instance_warmup_period,
                enabled: true,
                target_tracking_configuration: Some(policy.target_tracking()),
                predictive_scaling_configuration: None,
                // The alarms are CloudWatch's, read back onto the policy by the sync below.
                alarms: existing
                    .as_ref()
                    .map(|existing| existing.alarms.clone())
                    .unwrap_or_default(),
                alarm_id,
                created_time,
            };
            // A policy that already states this, with its alarms in place, is left exactly as
            // it stands: this pass runs on every change to any provider, and one provider's
            // change is no reason to rewrite another provider's policy or re-announce its
            // alarms.
            if existing.as_ref().is_some_and(|existing| {
                policy_fingerprint(existing) == policy_fingerprint(&desired)
                    && !existing.alarms.is_empty()
            }) {
                continue;
            }
            store.scaling_policies.insert(key.clone(), desired.clone());
            written.push((key, desired));
        }
        // A policy Amazon ECS holds for a provider that no longer wants one — its managed
        // scaling turned off, its association removed, the provider retired — is Amazon ECS's
        // to take away again, since nothing else put it there.
        for (key, provider) in held {
            let still_wanted = wanted.iter().any(|policy| {
                policy.group_name == key.auto_scaling_group_name && policy.provider_name == provider
            });
            if still_wanted {
                continue;
            }
            if let Some(removed) = store.scaling_policies.remove(&key) {
                retired.insert(key, removed);
            }
        }
    }
    // With the Auto Scaling store's lock released: the alarms the policies are driven by, in
    // CloudWatch's own store, through that service's own writer — so an alarm a policy reports
    // is one `DescribeAlarms` resolves.
    for (key, policy) in &written {
        let alarms = policies::sync_policy_alarms(engine, key, policy);
        let mut store = auto_scaling_state.lock();
        if let Some(stored) = store.scaling_policies.get_mut(key) {
            stored.alarms = alarms;
        }
    }
    if !retired.is_empty() {
        policies::remove_policy_alarms(engine, &retired);
    }
}
