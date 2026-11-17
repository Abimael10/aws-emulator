// SPDX-License-Identifier: CC0-1.0

//! Performing a scaling policy of an Auto Scaling group: which policy a request names,
//! the capacity performing it moves the group to, and the cooldown it leaves behind.
//!
//! # The rule this module implements
//!
//! A scaling policy states an ADJUSTMENT of the capacity the group maintains, and
//! performing the policy applies that adjustment: `AdjustmentType` says how the number it
//! carries is read — "The valid values are ChangeInCapacity, ExactCapacity, and
//! PercentChangeInCapacity" (`AdjustmentType$AdjustmentType`) — and the resulting capacity
//! is brought inside the group's own `MinSize`..`MaxSize`, because a group never holds a
//! capacity outside the bounds it was configured with. From there the group converges the
//! way it does for every other change of its size
//! ([`AutoScalingState::apply_capacity`]): it launches what is missing and scales in what
//! is surplus, and each movement records its own scaling activity.
//!
//! Which number is adjusted by depends on the policy's type, and only the two types that
//! state one of their own can be performed at all ([`EXECUTABLE_POLICY_TYPES`]):
//!
//! * a `SimpleScaling` policy IS one adjustment — "Required if the policy type is
//!   SimpleScaling. (Not used with any other policy type.)"
//!   (`PutScalingPolicyType$ScalingAdjustment`) — so performing it applies that one;
//! * a `StepScaling` policy carries a step per interval of the difference between the
//!   metric and the alarm threshold, so performing it needs the two values that locate the
//!   metric in those intervals: "The metric value to compare to BreachThreshold … Required
//!   if the policy type is StepScaling and not supported otherwise"
//!   (`ExecutePolicyType$MetricValue`) and the threshold itself
//!   (`$BreachThreshold`). "If you specify a metric value that doesn't correspond to a step
//!   adjustment for the policy, the call returns an error."
//!
//! A `TargetTrackingScaling` or `PredictiveScaling` policy states a TARGET for the metric
//! its own alarms watch rather than an adjustment, so no adjustment can be read off it,
//! and `ExecutePolicy`'s own members carry none for it.
//!
//! # The cooldowns
//!
//! Two different ones, both the model's own sentences:
//!
//! * `HonorCooldown` — "Indicates whether Amazon EC2 Auto Scaling waits for the cooldown
//!   period to complete before executing the policy. Valid only if the policy type is
//!   SimpleScaling" — so a simple policy performed with it while the group is still in the
//!   cooldown of its last change is WAITED OUT: the capacity change is stored as due at
//!   that cooldown's own end instant and materialised there
//!   ([`AutoScalingState::materialize_due`]), exactly as a deferred `SetDesiredCapacity`
//!   is, rather than being applied at once. A policy of any OTHER type honours no cooldown,
//!   so the member holds its change back for nothing and is not refused: "valid only if" is
//!   the same sentence `PutScalingPolicyType$Cooldown` carries, and a member of that class
//!   is kept by the type that owns it and ignored by the types that do not
//!   ([`super::scaling_policies::policy_members_of_type`]) rather than refusing the
//!   request. The members this operation documents as "not supported otherwise" ARE
//!   refused — that is a different sentence.
//! * `Cooldown` — "The duration of the policy's cooldown period, in seconds. When a
//!   cooldown period is specified here, it overrides the default cooldown period defined
//!   for the Auto Scaling group" (`PutScalingPolicyType$Cooldown`) — so the cooldown the
//!   performed policy LEAVES BEHIND is the policy's own when it carries one, and the
//!   group's `DefaultCooldown` otherwise.

use std::time::SystemTime;

use super::scaling_policies::{POLICY_TYPE_SIMPLE_SCALING, POLICY_TYPE_STEP_SCALING};
use super::{AutoScalingState, PendingCapacity, PolicyKey};

/// The policy types a request can perform: the ones whose own configuration states the
/// adjustment to apply, in the order [`super::scaling_policies::POLICY_TYPES`] lists them.
pub const EXECUTABLE_POLICY_TYPES: &[&str] = &[POLICY_TYPE_STEP_SCALING, POLICY_TYPE_SIMPLE_SCALING];

/// Whether a policy of `policy_type` states an adjustment of its own, and so can be
/// performed from the policy document and the request alone.
#[must_use]
pub fn is_executable_policy_type(policy_type: &str) -> bool {
    EXECUTABLE_POLICY_TYPES.contains(&policy_type)
}

/// Whether a policy of `policy_type` is graded against the metric value a request carries:
/// the step policy's own members, "Required if the policy type is StepScaling and not
/// supported otherwise" (`ExecutePolicyType$MetricValue`, `$BreachThreshold`).
#[must_use]
pub fn grades_a_metric_value(policy_type: &str) -> bool {
    policy_type == POLICY_TYPE_STEP_SCALING
}

/// The number of instances a `PercentChangeInCapacity` adjustment moves the capacity by at
/// least, when the policy states no `MinAdjustmentMagnitude` of its own.
///
/// The percentage of a capacity is a fraction, and the service takes it toward zero: "If
/// the calculated amount … is not an integer … values between 0 and 1 are rounded to 1"
/// (`AdjustmentType`'s scaling-adjustment-types documentation, which
/// `MinAdjustmentMagnitude` raises this floor over). So a non-zero percentage moves the
/// capacity by at least one instance in the direction it names, rather than reporting a
/// change it did not make.
pub const DEFAULT_MIN_ADJUSTMENT_MAGNITUDE: i32 = 1;

/// The breach a step policy is graded by: the metric value the request stated and the alarm
/// threshold it is compared to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricBreach {
    /// `ExecutePolicyType$MetricValue`.
    pub metric_value: f64,
    /// `ExecutePolicyType$BreachThreshold`.
    pub breach_threshold: f64,
}

impl MetricBreach {
    /// How far the metric is from the threshold a step's interval is measured from.
    #[must_use]
    fn difference(self) -> f64 {
        self.metric_value - self.breach_threshold
    }

    /// Whether the metric is ABOVE the threshold, which decides which end of a step's
    /// interval is inclusive. At the threshold itself the metric is not below it, so it is
    /// graded as the smallest breach above — the end the lower bound includes.
    #[must_use]
    fn above(self) -> bool {
        self.metric_value >= self.breach_threshold
    }
}

/// What a request asks of the policy it names, beside the policy itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct PolicyExecutionRequest {
    /// `ExecutePolicyType$HonorCooldown`.
    pub honor_cooldown: bool,
    /// `ExecutePolicyType$MetricValue`.
    pub metric_value: Option<f64>,
    /// `ExecutePolicyType$BreachThreshold`.
    pub breach_threshold: Option<f64>,
}

/// What performing a policy did: the capacity the group moved between, and the instant the
/// change was deferred to when the cooldown was honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyExecution {
    /// The desired capacity the group held before the policy was performed.
    pub from: i32,
    /// The desired capacity the policy asks for, inside the group's own bounds.
    pub to: i32,
    /// When the change falls due, for a change that waited for the group's cooldown to
    /// complete; `None` for one applied straight away.
    pub deferred_until: Option<SystemTime>,
}

/// Why a policy could not be performed, for the operation to state the refusal each
/// situation belongs to.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyExecutionRefusal {
    /// The policy is gone, or the group it belonged to is.
    NoSuchPolicy,
    /// The policy's type states no adjustment to apply; carries that type.
    UnexecutablePolicyType(String),
    /// A member this policy's type needs is absent; carries the wire path the service
    /// reports that member under.
    MemberRequired(&'static str),
    /// A member this policy's type does not use was supplied; carries the member as the
    /// model names it, and the policy's type.
    MemberNotSupported {
        /// The member the request carried.
        member: &'static str,
        /// The type of the policy it was sent for.
        policy_type: String,
    },
    /// No step of the policy covers the metric value the request stated.
    NoMatchingStep(MetricBreach),
    /// The policy states an adjustment the service cannot read: no `AdjustmentType`, or one
    /// outside [`super::ADJUSTMENT_TYPES`].
    UnreadableAdjustment,
    /// The group still holds instances from an earlier scaling activity, so the launches
    /// this change needs do not fit beside them yet.
    InstancesStillTerminating,
}

/// The wire path `MetricValue` is reported under in a constraint sentence, as
/// [`super::scaling_policies::COOLDOWN_MEMBER_PATH`] is for a cooldown.
pub const METRIC_VALUE_MEMBER_PATH: &str = "metricValue";

/// The wire path `BreachThreshold` is reported under in a constraint sentence.
pub const BREACH_THRESHOLD_MEMBER_PATH: &str = "breachThreshold";

/// `MetricValue` as the model names it, for the sentence that refuses it on a policy whose
/// type does not use it.
pub const METRIC_VALUE_MEMBER: &str = "MetricValue";

/// `BreachThreshold` as the model names it (same sentence).
pub const BREACH_THRESHOLD_MEMBER: &str = "BreachThreshold";

/// The refusal of a policy name nothing in the account answers to.
///
/// The service's own absent-resource sentence, in the form that carries the asked-for name
/// as its detail (`super::absent_group_message_as_name` is the group's): the resource word
/// is the policy's, the name is the only part taken from the request, and the rest is fixed
/// text.
#[must_use]
pub fn absent_policy_message(policy_name: &str) -> String {
    format!("Policy name not found - {policy_name}")
}

/// The refusal of a plain policy NAME that more than one Auto Scaling group holds a policy
/// of, with no `AutoScalingGroupName` to tell them apart.
///
/// A policy name is unique within a group rather than within the account, so such a request
/// names no one policy; the sentence says which member resolves it. The name is its only
/// variable part.
#[must_use]
pub fn ambiguous_policy_message(policy_name: &str) -> String {
    format!(
        "Policy name {policy_name} is used by more than one Auto Scaling group. Specify \
         AutoScalingGroupName to name the policy to execute."
    )
}

/// The refusal of a member, or of the operation itself, for a policy whose type does not
/// use it: `subject` is what was asked for and `policy_type` the type that does not take
/// it.
///
/// One sentence for every "not supported otherwise" and "valid only if the policy type is"
/// rule of this operation, so the family is worded once.
#[must_use]
pub fn not_supported_for_policy_type_message(subject: &str, policy_type: &str) -> String {
    format!("{subject} is not supported for scaling policies of type {policy_type}.")
}

/// The refusal of a metric value no step of the policy covers: "If you specify a metric
/// value that doesn't correspond to a step adjustment for the policy, the call returns an
/// error" (`ExecutePolicyType$MetricValue`).
///
/// The two values come from the request; the rest of the sentence is fixed text.
#[must_use]
pub fn no_matching_step_message(breach: MetricBreach) -> String {
    format!(
        "No StepAdjustment of the policy matches the metric value {} for the breach threshold \
         {}.",
        breach.metric_value, breach.breach_threshold
    )
}

/// The sentence this operation refuses an unreadable adjustment with: the policy carries no
/// `AdjustmentType`, so there is no way to read the number it scales by.
pub const UNREADABLE_ADJUSTMENT_MESSAGE: &str =
    "The scaling policy states no AdjustmentType, so its scaling adjustment cannot be applied.";

impl PolicyExecutionRefusal {
    /// The sentence this refusal carries, where `policy_name` is the policy the request
    /// named and `group_name` the group it belongs to.
    #[must_use]
    pub fn message(&self, policy_name: &str, group_name: &str) -> String {
        match self {
            Self::NoSuchPolicy => absent_policy_message(policy_name),
            Self::UnexecutablePolicyType(policy_type) => {
                not_supported_for_policy_type_message("ExecutePolicy", policy_type)
            }
            Self::MemberRequired(member_path) => {
                super::scaling_policies::member_must_not_be_null_message(member_path)
            }
            Self::MemberNotSupported {
                member,
                policy_type,
            } => not_supported_for_policy_type_message(member, policy_type),
            Self::NoMatchingStep(breach) => no_matching_step_message(*breach),
            Self::UnreadableAdjustment => UNREADABLE_ADJUSTMENT_MESSAGE.to_string(),
            Self::InstancesStillTerminating => {
                super::instances_still_terminating_message(group_name)
            }
        }
    }
}

/// Why a policy NAME resolved to no one policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyLookupFailure {
    /// Nothing in the account answers to it.
    NotFound,
    /// Several groups hold a policy of that name and the request named no group.
    Ambiguous,
}

/// The policy a request names: "The name or ARN of the policy"
/// (`ExecutePolicyType$PolicyName`), resolved against what the store holds.
///
/// An ARN names the policy outright, wherever it lives. A plain NAME is unique only within
/// a group, so it is resolved inside the group the request names; a request that names no
/// group resolves such a name when exactly ONE group holds a policy of it, and is otherwise
/// told which member tells them apart. A group the request names that holds no policy of
/// that name resolves nothing, whatever other groups hold.
pub fn resolve_policy(
    state: &AutoScalingState,
    group_name: Option<&str>,
    policy_name_or_arn: &str,
) -> Result<PolicyKey, PolicyLookupFailure> {
    let in_group = |key: &PolicyKey| {
        group_name.is_none_or(|group| key.auto_scaling_group_name == group)
    };
    let by_arn = state
        .scaling_policies
        .iter()
        .find(|(key, policy)| in_group(key) && policy.policy_arn == policy_name_or_arn)
        .map(|(key, _)| key.clone());
    if let Some(key) = by_arn {
        return Ok(key);
    }
    let mut by_name: Vec<PolicyKey> = state
        .scaling_policies
        .keys()
        .filter(|key| in_group(key) && key.policy_name == policy_name_or_arn)
        .cloned()
        .collect();
    match by_name.len() {
        0 => Err(PolicyLookupFailure::NotFound),
        1 => by_name.pop().ok_or(PolicyLookupFailure::NotFound),
        _ => Err(PolicyLookupFailure::Ambiguous),
    }
}

impl AutoScalingState {
    /// Perform the scaling policy `key` names, and answer the capacity it moved the group
    /// between.
    ///
    /// THE ONE place a policy is performed: the adjustment is read off the policy, applied
    /// to the capacity the group maintains, held inside the group's own bounds and then
    /// converged on through [`AutoScalingState::apply_capacity`], so a policy-driven change
    /// launches and terminates the account's own Amazon EC2 instances and records its
    /// activities exactly as a manual capacity change does. The activity's cause names the
    /// policy that asked for it ([`policy_execution_cause`]).
    ///
    /// Nothing is written when the policy cannot be performed: every rule the request and
    /// the policy are taken against is checked before the group is touched.
    pub fn execute_scaling_policy<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        key: &PolicyKey,
        request: PolicyExecutionRequest,
        now: SystemTime,
    ) -> Result<PolicyExecution, PolicyExecutionRefusal> {
        let Some(policy) = self.scaling_policies.get(key).cloned() else {
            return Err(PolicyExecutionRefusal::NoSuchPolicy);
        };
        let Some(group) = self.groups.get(&key.auto_scaling_group_name) else {
            return Err(PolicyExecutionRefusal::NoSuchPolicy);
        };
        let policy_type = policy.policy_type.clone();
        if !is_executable_policy_type(&policy_type) {
            return Err(PolicyExecutionRefusal::UnexecutablePolicyType(policy_type));
        }
        let breach = metric_breach(&policy_type, request)?;

        // The adjustment the policy states for this request: its own for a simple policy,
        // and the step the breach falls in for a step policy.
        let adjustment = match breach {
            Some(breach) => applicable_step_adjustment(&policy.step_adjustments, breach)
                .ok_or(PolicyExecutionRefusal::NoMatchingStep(breach))?,
            None => policy
                .scaling_adjustment
                .ok_or(PolicyExecutionRefusal::UnreadableAdjustment)?,
        };
        let from = group.desired_capacity;
        let adjustment_type = policy
            .adjustment_type
            .as_deref()
            .ok_or(PolicyExecutionRefusal::UnreadableAdjustment)?;
        let asked = adjusted_capacity(
            adjustment_type,
            adjustment,
            policy.min_adjustment_magnitude.or(policy.min_adjustment_step),
            from,
        )
        .ok_or(PolicyExecutionRefusal::UnreadableAdjustment)?;
        // A group never holds a capacity outside the bounds it was configured with, so a
        // policy that asks for more than its `MaxSize` or less than its `MinSize` moves it
        // to the bound rather than being refused. The two bounds are read LOWEST FIRST:
        // nothing on this path establishes their order, and a capacity change must not be
        // the request that panics on a group whose stored sizes are inverted.
        let (floor, ceiling) = if group.min_size <= group.max_size {
            (group.min_size, group.max_size)
        } else {
            (group.max_size, group.min_size)
        };
        let to = asked.clamp(floor, ceiling);

        // Instances from an earlier change are stored until they finish terminating, and
        // they occupy the group's ceiling while they do: a change whose launches would not
        // fit beside them cannot be started yet.
        if group.launches_needed(to) > group.launch_headroom() {
            return Err(PolicyExecutionRefusal::InstancesStillTerminating);
        }
        let in_cooldown = group.in_cooldown(now);
        let cooldown_until = group.cooldown_until;
        let cause = policy_execution_cause(&key.policy_name, from, to);

        // "Valid only if the policy type is SimpleScaling": only a simple policy waits for
        // the group's cooldown, and only while one is still running — which is the cooldown's
        // own end instant, the one the change then falls due at.
        let waits_for = cooldown_until.filter(|_| {
            request.honor_cooldown && policy_type == POLICY_TYPE_SIMPLE_SCALING && in_cooldown
        });
        if let Some(due) = waits_for {
            if let Some(group) = self.groups.get_mut(&key.auto_scaling_group_name) {
                group.pending_capacity = Some(PendingCapacity {
                    desired_capacity: to,
                    due,
                    cause,
                    // The cooldown this execution leaves behind is the POLICY's when it
                    // carries one, whether the change is applied now or when the deferral
                    // comes due, so it travels with the deferred change.
                    cooldown_override: policy.cooldown,
                });
            }
            return Ok(PolicyExecution {
                from,
                to,
                deferred_until: Some(due),
            });
        }

        self.apply_capacity(engine, &key.auto_scaling_group_name, to, &cause, now);
        // The cooldown the change leaves behind is the POLICY's when it carries one, which
        // "overrides the default cooldown period defined for the Auto Scaling group" the
        // convergence above started — through the one writer of that override, which is also
        // what applies it to a deferred execution.
        self.override_cooldown(&key.auto_scaling_group_name, policy.cooldown, now);
        Ok(PolicyExecution {
            from,
            to,
            deferred_until: None,
        })
    }
}

/// The breach a policy of `policy_type` is graded by, read off the request.
///
/// `None` for a policy whose type carries its own adjustment. The two metric members are
/// "Required if the policy type is StepScaling and not supported otherwise", so each is
/// refused where it does not belong and missed where it does.
fn metric_breach(
    policy_type: &str,
    request: PolicyExecutionRequest,
) -> Result<Option<MetricBreach>, PolicyExecutionRefusal> {
    if !grades_a_metric_value(policy_type) {
        for (value, member) in [
            (request.metric_value, METRIC_VALUE_MEMBER),
            (request.breach_threshold, BREACH_THRESHOLD_MEMBER),
        ] {
            if value.is_some() {
                return Err(PolicyExecutionRefusal::MemberNotSupported {
                    member,
                    policy_type: policy_type.to_string(),
                });
            }
        }
        return Ok(None);
    }
    let Some(metric_value) = request.metric_value else {
        return Err(PolicyExecutionRefusal::MemberRequired(
            METRIC_VALUE_MEMBER_PATH,
        ));
    };
    let Some(breach_threshold) = request.breach_threshold else {
        return Err(PolicyExecutionRefusal::MemberRequired(
            BREACH_THRESHOLD_MEMBER_PATH,
        ));
    };
    Ok(Some(MetricBreach {
        metric_value,
        breach_threshold,
    }))
}

/// The `ScalingAdjustment` of the step whose interval the breach falls in.
///
/// A step's interval is of the DIFFERENCE between the metric and the breach threshold, and
/// which end is inclusive follows the direction of the breach: above the threshold "the
/// lower bound is inclusive (the metric must be greater than or equal to the threshold plus
/// the lower bound)" and the upper bound is exclusive
/// (`StepAdjustment$MetricIntervalLowerBound`); below it the lower bound is exclusive and
/// the upper bound inclusive. A bound the step leaves out is unbounded in that direction,
/// and so is one that is not a finite number — nothing can be compared against it.
#[must_use]
pub fn applicable_step_adjustment(
    steps: &[crate::ops::types::StepAdjustment],
    breach: MetricBreach,
) -> Option<i32> {
    let finite = |bound: Option<f64>| bound.filter(|value| value.is_finite());
    let difference = breach.difference();
    steps
        .iter()
        .find(|step| {
            let lower = finite(step.metric_interval_lower_bound).unwrap_or(f64::NEG_INFINITY);
            let upper = finite(step.metric_interval_upper_bound).unwrap_or(f64::INFINITY);
            if breach.above() {
                difference >= lower && difference < upper
            } else {
                difference > lower && difference <= upper
            }
        })
        .map(|step| step.scaling_adjustment)
}

/// The capacity an adjustment asks for, from the capacity the group maintains now.
///
/// Each arm is one sentence of the service's own documentation of the adjustment types
/// ([`super::ADJUSTMENT_TYPES`]): `ChangeInCapacity` adds to the current capacity
/// ("a positive value adds to the current capacity while a negative number removes from
/// the current capacity", `StepAdjustment$ScalingAdjustment`), `ExactCapacity` IS the
/// capacity, and `PercentChangeInCapacity` moves it by a percentage of itself
/// ([`percentage_magnitude`]). `None` for an adjustment type outside that catalog, which
/// states no arithmetic at all.
#[must_use]
pub fn adjusted_capacity(
    adjustment_type: &str,
    adjustment: i32,
    min_adjustment_magnitude: Option<i32>,
    current: i32,
) -> Option<i32> {
    let capacity = match adjustment_type {
        super::ADJUSTMENT_TYPE_CHANGE_IN_CAPACITY => i64::from(current) + i64::from(adjustment),
        super::ADJUSTMENT_TYPE_EXACT_CAPACITY => i64::from(adjustment),
        super::ADJUSTMENT_TYPE_PERCENT_CHANGE_IN_CAPACITY => {
            i64::from(current)
                + percentage_magnitude(current, adjustment, min_adjustment_magnitude)
        }
        _ => return None,
    };
    Some(capacity.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

/// How many instances a `PercentChangeInCapacity` adjustment moves the capacity by.
///
/// The percentage is OF the capacity the group maintains, so a percentage of NOTHING is
/// nothing: a group maintaining no instance is not scaled out by a share of itself. For every
/// other capacity the number of instances the percentage comes to is a fraction: it is taken
/// toward zero, and never to nothing — a non-zero percentage of a non-zero capacity moves it
/// by at least [`DEFAULT_MIN_ADJUSTMENT_MAGNITUDE`] instance in the direction it names.
/// `MinAdjustmentMagnitude` raises that floor: "The minimum value to scale by when the
/// adjustment type is PercentChangeInCapacity. For example, suppose that you create a step
/// scaling policy to scale out an Auto Scaling group by 25 percent and you specify a
/// MinAdjustmentMagnitude of 2. If the group has 4 instances and the scaling policy is
/// performed, 25 percent of 4 is 1. However, because you specified a MinAdjustmentMagnitude
/// of 2, Amazon EC2 Auto Scaling scales out the group by 2 instances"
/// (`PutScalingPolicyType$MinAdjustmentMagnitude`).
#[must_use]
pub fn percentage_magnitude(
    current: i32,
    adjustment: i32,
    min_adjustment_magnitude: Option<i32>,
) -> i64 {
    if adjustment == 0 || current == 0 {
        return 0;
    }
    let exact = f64::from(current) * f64::from(adjustment) / 100.0;
    let floor = i64::from(
        min_adjustment_magnitude
            .unwrap_or(DEFAULT_MIN_ADJUSTMENT_MAGNITUDE)
            .max(DEFAULT_MIN_ADJUSTMENT_MAGNITUDE),
    );
    let magnitude = (exact.abs().trunc() as i64).max(floor);
    magnitude * i64::from(adjustment.signum())
}

/// The cause a policy-driven capacity change records: that a policy asked for it, which
/// policy, and the capacities it moved between.
///
/// The subject of [`AutoScalingState::capacity_change_cause`], as every other capacity
/// change of this service words its own.
#[must_use]
pub fn policy_execution_cause(policy_name: &str, from: i32, to: i32) -> String {
    AutoScalingState::capacity_change_cause(
        &format!("a user request executed policy {policy_name}"),
        from,
        to,
    )
}
