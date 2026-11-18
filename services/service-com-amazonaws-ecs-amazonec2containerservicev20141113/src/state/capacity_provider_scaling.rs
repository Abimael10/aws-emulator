// SPDX-License-Identifier: CC0-1.0

//! The scaling configuration an AUTO SCALING GROUP capacity provider is stored with: the
//! managed-scaling members Amazon ECS fills in for a create that names none, the
//! managed-draining option it turns on for it, and the termination-protection setting it
//! leaves off.
//!
//! `AutoScalingGroupProvider` is the binding onto a caller's own Auto Scaling group, and
//! every member of it that the request leaves out has a value Amazon ECS applies for it —
//! each of the managed-scaling numbers states its own in its documentation ("If this
//! parameter is omitted, the default value of ... is used"), and the provider a create
//! reports carries them whether or not the request named a `managedScaling` block at all.
//! So the DEFAULTING happens where the provider is stored, once, and every operation that
//! reports a provider reads the same record back: a client cannot be told "no managed
//! scaling" where Amazon ECS has these values in force.
//!
//! `managedScaling$status` is NOT one of them: it is the switch that decides whether Amazon
//! ECS drives the group's capacity at all ("Determines whether to use managed scaling for
//! the capacity provider"), so it stays exactly as the request left it — a provider whose
//! create named no status is one nothing turned managed scaling on for, which is also what
//! decides whether a placement through it waits for capacity to be acquired
//! ([`needs_registered_capacity_now`]).

use super::*;

/// `ManagedScaling$targetCapacity` when the request names none: "The default value of
/// `100` percent results in the Amazon EC2 instances in your Auto Scaling group being
/// completely used."
pub const MANAGED_SCALING_DEFAULT_TARGET_CAPACITY: i32 = 100;

/// `ManagedScaling$minimumScalingStepSize` when the request names none: "If this parameter
/// is omitted, the default value of `1` is used."
pub const MANAGED_SCALING_DEFAULT_MINIMUM_STEP_SIZE: i32 = 1;

/// `ManagedScaling$maximumScalingStepSize` when the request names none: "If this parameter
/// is omitted, the default value of `10000` is used."
pub const MANAGED_SCALING_DEFAULT_MAXIMUM_STEP_SIZE: i32 = 10000;

/// `ManagedScaling$instanceWarmupPeriod` when the request names none: "If this parameter is
/// omitted, the default value of `300` seconds is used."
pub const MANAGED_SCALING_DEFAULT_INSTANCE_WARMUP_PERIOD: i32 = 300;

/// `ManagedScaling$scalingApproach` of an Auto Scaling group provider: Amazon ECS scales
/// such a group by "managing a target tracking scaling policy using an Amazon ECS managed
/// CloudWatch metric" (`ManagedScaling`'s own documentation), which is the approach every
/// provider over a caller's group is scaled by, so it is the value each of them reports.
pub const MANAGED_SCALING_APPROACH_SCALING_POLICY: &str = "SCALING_POLICY";

/// `AutoScalingGroupProvider$managedDraining` when the request names none: Amazon ECS
/// "manages and gracefully drains the EC2 container instances that are in the Auto Scaling
/// group capacity provider" for a provider created without the option, which is the value
/// the recorded providers of such creates report.
pub const MANAGED_DRAINING_ENABLED: &str = "ENABLED";

/// `AutoScalingGroupProvider$managedTerminationProtection` when the request names none —
/// that member's own doc, "The default is off", in the spelling the
/// `ManagedTerminationProtection` value set gives it (`smithy_query` query="shape"
/// shape="ManagedTerminationProtection"). The setting is in force for every provider over a
/// caller's group, off being a setting like any other, so a client reading the configuration
/// back is told which of the two it is rather than nothing at all.
pub const MANAGED_TERMINATION_PROTECTION_DISABLED: &str = "DISABLED";

/// The `managedScaling` block a provider is STORED with: every number the request left out
/// filled in with the default its own member documents, and `status` left as the request
/// spelled it.
///
/// A request that named no block at all is still a provider Amazon ECS applies these to, so
/// this answers the defaulted block for `None` as well.
pub fn stored_managed_scaling(requested: Option<&ManagedScaling>) -> ManagedScaling {
    ManagedScaling {
        scaling_approach: requested
            .and_then(|scaling| scaling.scaling_approach.clone())
            .or_else(|| Some(MANAGED_SCALING_APPROACH_SCALING_POLICY.to_string())),
        status: requested.and_then(|scaling| scaling.status.clone()),
        target_capacity: requested
            .and_then(|scaling| scaling.target_capacity)
            .or(Some(MANAGED_SCALING_DEFAULT_TARGET_CAPACITY)),
        minimum_scaling_step_size: requested
            .and_then(|scaling| scaling.minimum_scaling_step_size)
            .or(Some(MANAGED_SCALING_DEFAULT_MINIMUM_STEP_SIZE)),
        maximum_scaling_step_size: requested
            .and_then(|scaling| scaling.maximum_scaling_step_size)
            .or(Some(MANAGED_SCALING_DEFAULT_MAXIMUM_STEP_SIZE)),
        instance_warmup_period: requested
            .and_then(|scaling| scaling.instance_warmup_period)
            .or(Some(MANAGED_SCALING_DEFAULT_INSTANCE_WARMUP_PERIOD)),
    }
}

/// The refusal a `managedScaling` block earns when its two scaling step sizes do not bound
/// each other, or `None` when they do.
///
/// `minimumScalingStepSize` is "the minimum number of Amazon EC2 instances that Amazon ECS
/// will scale out at one time" and `maximumScalingStepSize` the maximum, so the pair is a
/// RANGE: a minimum above the maximum describes no scale-out step at all. The member ranges
/// themselves (`@range` 1..10000 on each) are the model's and are refused at the boundary;
/// what the two values say TOGETHER is a cross-field rule, so it is checked here — on the
/// values the request carried, with each one the request left out standing at the default
/// its own member documents — the same defaults [`stored_managed_scaling`] stores the
/// provider with, so the pair judged here is the pair a later read reports.
///
/// Asked by every operation that takes a `managedScaling` block, BEFORE the request is
/// weighed against what the account already holds: a block that describes no step is a bad
/// parameter whatever else the request would conflict with.
pub fn managed_scaling_step_size_violation(
    requested: Option<&ManagedScaling>,
) -> Option<&'static str> {
    let minimum = requested
        .and_then(|scaling| scaling.minimum_scaling_step_size)
        .unwrap_or(MANAGED_SCALING_DEFAULT_MINIMUM_STEP_SIZE);
    let maximum = requested
        .and_then(|scaling| scaling.maximum_scaling_step_size)
        .unwrap_or(MANAGED_SCALING_DEFAULT_MAXIMUM_STEP_SIZE);
    (minimum > maximum).then_some(MANAGED_SCALING_STEP_SIZE_MESSAGE)
}

/// `ClientException$message` for a `managedScaling` block whose minimum scaling
/// step size is above its maximum: one fixed sentence naming the two members and the
/// relation they have to keep, and neither value the request carried.
pub const MANAGED_SCALING_STEP_SIZE_MESSAGE: &str =
    "The minimum or maximum scaling step size value is invalid. Specify a minimum scaling \
     step size value that is less than or equal to the maximum scaling step size value and \
     try again.";

/// The `autoScalingGroupProvider` a capacity provider is STORED with: the binding the
/// request carried, with the managed-scaling block, the managed-draining option and the
/// managed-termination-protection setting Amazon ECS applies to it filled in
/// ([`stored_managed_scaling`], [`MANAGED_DRAINING_ENABLED`],
/// [`MANAGED_TERMINATION_PROTECTION_DISABLED`]).
pub fn stored_auto_scaling_group_provider(
    requested: &AutoScalingGroupProvider,
) -> AutoScalingGroupProvider {
    AutoScalingGroupProvider {
        auto_scaling_group_arn: requested.auto_scaling_group_arn.clone(),
        managed_scaling: Some(stored_managed_scaling(requested.managed_scaling.as_ref())),
        managed_termination_protection: requested
            .managed_termination_protection
            .clone()
            .or_else(|| Some(MANAGED_TERMINATION_PROTECTION_DISABLED.to_string())),
        managed_draining: requested
            .managed_draining
            .clone()
            .or_else(|| Some(MANAGED_DRAINING_ENABLED.to_string())),
    }
}
