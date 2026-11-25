// SPDX-License-Identifier: CC0-1.0

//! The reference-data catalogs.
//!
//! Amazon EC2 Auto Scaling publishes a handful of fixed catalogs through zero-input
//! describes (`DescribeScalingProcessTypes`, `DescribeTerminationPolicyTypes`,
//! `DescribeAdjustmentTypes`, `DescribeAutoScalingNotificationTypes`,
//! `DescribeLifecycleHookTypes`, `DescribeMetricCollectionTypes`). They are SERVICE data,
//! not per-account state: the same answer for every caller, in every region, whatever the
//! store holds — so they live here as constants and the handlers that report them take no
//! lock and write nothing. Beside each catalog is the one reader that decides membership of
//! it, so the values a request is accepted with and the values the catalog publishes cannot
//! drift apart.
//!
//! Each catalog's doc comment names where its membership and ordering come from: normally the
//! model member whose documentation publishes them, and where that documentation is behind
//! the service, what it omits.

use super::*;

/// The processes `DescribeScalingProcessTypes` reports, and the ones a refusal of an
/// unknown process offers ([`suspended_processes::unknown_process_message`]).
///
/// Membership from the model's own documentation for `ProcessType$ProcessName`
/// ("One of the following processes: Launch, Terminate, AddToLoadBalancer,
/// AlarmNotification, AZRebalance, HealthCheck, InstanceRefresh,
/// ReplaceUnhealthy, ScheduledActions"). Held in ASCII order, which is the order
/// the service reports the catalog in.
///
/// What a suspension or a resumption may NAME is this catalog and the one process the
/// service accepts the name of without publishing it
/// ([`suspended_processes::suspendable_processes`]), so the two are read apart: this is what
/// the service publishes.
pub const SCALING_PROCESS_TYPES: &[&str] = &[
    "AZRebalance",
    "AddToLoadBalancer",
    "AlarmNotification",
    "HealthCheck",
    "InstanceRefresh",
    "Launch",
    "ReplaceUnhealthy",
    "ScheduledActions",
    "Terminate",
];

/// The type name the custom termination policy is published under.
///
/// [`TERMINATION_POLICIES`] lists the policies a group may name literally and
/// [`TERMINATION_POLICY_LAMBDA_PREFIX`] accepts the ARN form beside them; the
/// catalog names that form by its TYPE rather than by any one function's ARN.
pub const TERMINATION_POLICY_TYPE_LAMBDA: &str = "Lambda";

/// The catalog `DescribeTerminationPolicyTypes` reports: every policy
/// [`TERMINATION_POLICIES`] accepts by name, plus
/// [`TERMINATION_POLICY_TYPE_LAMBDA`] for the Lambda-ARN form the same model
/// documentation ends with, in ASCII order.
pub const TERMINATION_POLICY_TYPES: &[&str] = &[
    termination_policies::ALLOCATION_STRATEGY,
    termination_policies::CLOSEST_TO_NEXT_INSTANCE_HOUR,
    termination_policies::DEFAULT,
    TERMINATION_POLICY_TYPE_LAMBDA,
    termination_policies::NEWEST_INSTANCE,
    termination_policies::OLDEST_INSTANCE,
    termination_policies::OLDEST_LAUNCH_CONFIGURATION,
    termination_policies::OLDEST_LAUNCH_TEMPLATE,
];

/// The catalog `DescribeAdjustmentTypes` reports, from the model's own
/// documentation for `AdjustmentType$AdjustmentType`: "The policy adjustment
/// type. The valid values are ChangeInCapacity, ExactCapacity, and
/// PercentChangeInCapacity." In ASCII order.
pub const ADJUSTMENT_TYPES: &[&str] = &[
    ADJUSTMENT_TYPE_CHANGE_IN_CAPACITY,
    ADJUSTMENT_TYPE_EXACT_CAPACITY,
    ADJUSTMENT_TYPE_PERCENT_CHANGE_IN_CAPACITY,
];

/// The adjustment type that moves a group's capacity BY the number the policy carries: "a
/// positive value adds to the current capacity while a negative number removes from the
/// current capacity" (`StepAdjustment$ScalingAdjustment`).
///
/// Named beside its siblings so the ONE catalogue above is also what the arithmetic that
/// applies an adjustment reads ([`policy_execution::adjusted_capacity`]), rather than a
/// second copy of the three spellings.
pub const ADJUSTMENT_TYPE_CHANGE_IN_CAPACITY: &str = "ChangeInCapacity";

/// The adjustment type whose number IS the capacity the group is moved to.
pub const ADJUSTMENT_TYPE_EXACT_CAPACITY: &str = "ExactCapacity";

/// The adjustment type that moves the capacity by a percentage of itself.
pub const ADJUSTMENT_TYPE_PERCENT_CHANGE_IN_CAPACITY: &str = "PercentChangeInCapacity";

/// The catalog `DescribeLifecycleHookTypes` reports: the lifecycle transitions a
/// lifecycle hook may name, from the model's own documentation for
/// `PutLifecycleHookType$LifecycleTransition` ("To create a lifecycle hook for
/// scale-out events, specify autoscaling:EC2_INSTANCE_LAUNCHING. To create a
/// lifecycle hook for scale-in events, specify
/// autoscaling:EC2_INSTANCE_TERMINATING"). In ASCII order.
/// Named beside the transitions themselves ([`lifecycle_actions`]), which is where the hold
/// each one puts an instance in is implemented, so the catalog a read publishes and the
/// transitions a hold is taken at cannot drift apart.
pub const LIFECYCLE_HOOK_TYPES: &[&str] = &[TRANSITION_LAUNCHING, TRANSITION_TERMINATING];

/// The catalog `DescribeAutoScalingNotificationTypes` reports: the event types a
/// group can notify about.
///
/// The model's own documentation for `NotificationConfiguration$NotificationType`
/// ("One of the following event notification types: …") enumerates the four
/// instance-lifecycle events and the test notification, so that prose is a LOWER
/// BOUND on what the service publishes rather than the catalog itself: the service
/// also notifies about the replacement of an instance's root volume, under
/// `autoscaling:EC2_ROOT_VOLUME_REPLACEMENT` and its `_ERROR` counterpart, which a
/// group may name in a notification configuration without the prose listing it. The
/// list target is an untyped string list
/// (`smithy_query(query="shape", shape="AutoScalingNotificationTypes")`), so the
/// membership is the service's own.
///
/// In ASCII order, which is the order the service publishes them in: the
/// instance-lifecycle events, then the root-volume ones, then the test
/// notification.
///
/// The lifecycle transitions of [`LIFECYCLE_HOOK_TYPES`] are a SEPARATE catalog,
/// published by `DescribeLifecycleHookTypes`, even though the model carries both
/// through the one `AutoScalingNotificationTypes` list shape (it is the target of
/// this operation's output member and of that one's alike): a transition is what a
/// lifecycle hook is created for, not an event a group notifies about, and the
/// model documents each list on its own member.
pub const AUTO_SCALING_NOTIFICATION_TYPES: &[&str] = &[
    "autoscaling:EC2_INSTANCE_LAUNCH",
    "autoscaling:EC2_INSTANCE_LAUNCH_ERROR",
    "autoscaling:EC2_INSTANCE_TERMINATE",
    "autoscaling:EC2_INSTANCE_TERMINATE_ERROR",
    "autoscaling:EC2_ROOT_VOLUME_REPLACEMENT",
    "autoscaling:EC2_ROOT_VOLUME_REPLACEMENT_ERROR",
    "autoscaling:TEST_NOTIFICATION",
];

/// The catalog `DescribeMetricCollectionTypes` reports as `Metrics`: the group
/// metrics `EnableMetricsCollection` and `DisableMetricsCollection` name.
///
/// The model's own documentation for `MetricCollectionType$Metric` ("One of the
/// following metrics: …") enumerates only the twenty metrics that predate the
/// retained-capacity series, so that prose is a LOWER BOUND on what the service
/// publishes rather than the catalog itself: the warm pool's own minimum
/// (`WarmPoolMinSize`) and the four retained-capacity series a group's instance
/// maintenance policy reports (`GroupTerminatingRetainedInstances`,
/// `GroupTerminatingRetainedCapacity`, `WarmPoolPendingRetainedCapacity`,
/// `WarmPoolTerminatingRetainedCapacity`) are enabled by name through
/// `EnableMetricsCollection` and published by this catalog without appearing in
/// it.
///
/// ORDERED as the service publishes it, which is what a refusal naming the valid
/// metric-collection types enumerates
/// ([`metrics_collection::valid_metric_collection_types_message`]): the group's
/// three sizes first, then each group series as an instance/capacity PAIR — the
/// count of instances in a state immediately followed by the capacity those
/// instances carry — then the warm pool's own series in the same pairing, and the
/// two combined `GroupAndWarmPool*` series last. The model member's prose lists
/// every instance count before every capacity, which is a grouping of the names it
/// enumerates and not the order the service publishes them in.
pub const METRIC_COLLECTION_TYPES: &[&str] = &[
    "GroupMinSize",
    "GroupMaxSize",
    "GroupDesiredCapacity",
    "GroupInServiceInstances",
    "GroupInServiceCapacity",
    "GroupPendingInstances",
    "GroupPendingCapacity",
    "GroupTerminatingInstances",
    "GroupTerminatingCapacity",
    "GroupTerminatingRetainedInstances",
    "GroupTerminatingRetainedCapacity",
    "GroupStandbyInstances",
    "GroupStandbyCapacity",
    "GroupTotalInstances",
    "GroupTotalCapacity",
    "WarmPoolMinSize",
    "WarmPoolDesiredCapacity",
    "WarmPoolPendingCapacity",
    "WarmPoolPendingRetainedCapacity",
    "WarmPoolTerminatingCapacity",
    "WarmPoolTerminatingRetainedCapacity",
    "WarmPoolWarmedCapacity",
    "WarmPoolTotalCapacity",
    "GroupAndWarmPoolDesiredCapacity",
    "GroupAndWarmPoolTotalCapacity",
];

/// Whether `metric` is one of [`METRIC_COLLECTION_TYPES`].
///
/// The metrics `EnableMetricsCollection` and `DisableMetricsCollection` accept
/// are the metrics this catalog publishes, so both sides check membership
/// through here and cannot drift apart: a request naming a metric this answers
/// false for is refused and writes nothing ([`metrics_collection`]).
pub fn is_valid_metric_collection_type(metric: &str) -> bool {
    METRIC_COLLECTION_TYPES.contains(&metric)
}

/// The catalog `DescribeMetricCollectionTypes` reports as `Granularities`, from
/// the model's own documentation for `MetricGranularityType$Granularity`: "The
/// granularity. The only valid value is 1Minute."
pub const METRIC_GRANULARITIES: &[&str] = &["1Minute"];

/// Whether `granularity` is one of [`METRIC_GRANULARITIES`].
///
/// The granularity `EnableMetricsCollection` accepts is the granularity this
/// catalog publishes, so both sides read it from here and cannot drift apart.
pub fn is_valid_metric_granularity(granularity: &str) -> bool {
    METRIC_GRANULARITIES.contains(&granularity)
}

/// The units a `DesiredCapacityType` may name, from the model's own documentation
/// for `CreateAutoScalingGroup$DesiredCapacityType`: "Valid values: units | vcpu |
/// memory-mib".
///
/// Spelled where the arithmetic that reads them lives ([`instance_requirements`]), so the
/// value a request is accepted with and the unit a group's capacity is then counted in
/// cannot drift apart.
pub const DESIRED_CAPACITY_TYPES: &[&str] = &[
    instance_requirements::CAPACITY_TYPE_UNITS,
    instance_requirements::CAPACITY_TYPE_VCPU,
    instance_requirements::CAPACITY_TYPE_MEMORY_MIB,
];
