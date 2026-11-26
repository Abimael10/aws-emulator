// SPDX-License-Identifier: CC0-1.0

//! The SCALING POLICY actions an alarm may carry, and what executing one does.
//!
//! "Auto Scaling policy:
//! arn:aws:autoscaling:region:account-id:scalingPolicy:policy-id:autoScalingGroupName/group-friendly-name:policyName/policy-friendly-name"
//! (`PutMetricAlarmInput$AlarmActions`, and the same for `OKActions`,
//! `InsufficientDataActions` and every action list of `PutCompositeAlarm`) — so an alarm whose
//! transition fires one of these ARNs hands the transition to the scaling service that owns
//! the policy, and that service applies the policy to the resource it scales.
//!
//! Because that sentence publishes the FORM the relative id of an ARN of this vendor is
//! written in, an ARN of it whose relative id is written some other way names no policy any
//! scaling service could resolve: [`unknown_relative_id_error`] is the one reading that says
//! so, and the alarm writers refuse such a request rather than storing an action that can
//! never be delivered.
//!
//! # What executing one is, and where it ends
//!
//! Executing the action is the HAND-OVER: the notification is addressed to the policy and
//! recorded in the alarm's own history as an action that was executed, exactly as a publish to
//! a topic is. What the receiving service then does with it — apply the adjustment, hold it
//! back because the group is inside a cooldown, decline it because the group has suspended the
//! process that receives alarm notifications — is that service's decision and no part of this
//! one: an alarm reports the action it invoked, not the capacity another service moved.
//!
//! So the hand-over is DELIVERED rather than called: it is queued on this store
//! ([`crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State::pending_scaling_policy_invocations`],
//! bounded by [`MAX_PENDING_SCALING_POLICY_INVOCATIONS`]) and taken from there by the scaling
//! service that holds the policy ([`take_invocations`]), which is the one component that can
//! resolve a policy ARN at all. Nothing is invented for an ARN no mounted service claims: the
//! delivery simply waits, and ages out of the queue behind newer ones.
//!
//! The invocation carries what the receiving service needs to apply the policy and could not
//! read anywhere else: which state the alarm moved to (an action list is selected by it, so a
//! policy may be fired for `ALARM` and another for `OK`), and the BREACH the alarm graded —
//! the threshold it compared against and the newest datapoint of the window it decided on —
//! which is what a step policy selects its step adjustment from
//! (`ExecutePolicyType$MetricValue`, `$BreachThreshold`).
//!
//! A hand-over nobody takes AGES OUT ([`MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS`])
//! rather than waiting behind the queue's bound: not every scaling service that mints a policy
//! ARN under this vendor receives its policies this way — Application Auto Scaling drives its
//! own from the alarm state it reads itself — so without that, invocations no mounted service
//! claims would be what pushes a hand-over a service IS waiting for out of the queue.

use crate::ops::types::MetricAlarm;

/// The ARN vendor of a scaling policy, for Amazon EC2 Auto Scaling and for Application Auto
/// Scaling alike: both mint their policies under this namespace.
const AUTOSCALING_ARN_VENDOR: &str = "autoscaling";

/// The ARN resource type of a scaling policy, which is what tells one from any other ARN of
/// this vendor (an Auto Scaling group, a launch configuration).
const SCALING_POLICY_RESOURCE_TYPE: &str = "scalingPolicy";

/// The field of a policy's relative id that names the policy itself, with the colon that
/// separates it from the resource the policy scales:
/// "…:policyName/`policy-friendly-name`" (`PutMetricAlarmInput$AlarmActions`).
const POLICY_NAME_FIELD: &str = ":policyName/";

/// The two forms the field BEFORE that one takes — the thing the policy scales.
///
/// "…:scalingPolicy:`policy-id`:autoScalingGroupName/`group-friendly-name`:policyName/…"
/// (`PutMetricAlarmInput$AlarmActions`) is how an Amazon EC2 Auto Scaling policy names the
/// group it scales; an Application Auto Scaling policy names the scalable target it scales in
/// the same field instead (`scalingPolicy:<policy id>:resource/<service namespace>/<resource
/// id>:policyName/<name>`, the `PolicyARN` that service mints), since both services mint their
/// policies under this one vendor.
const SCALED_RESOURCE_FIELDS: [&str; 2] = ["autoScalingGroupName/", "resource/"];

/// The form the relative id of an action of this vendor has to be written in, as a refusal of
/// one outside it states the form and as the documented action value spells it:
/// "arn:aws:autoscaling:`region`:`account-id`:scalingPolicy:`policy-id`:autoScalingGroupName/`group-friendly-name`:policyName/`policy-friendly-name`"
/// (`PutMetricAlarmInput$AlarmActions`).
const SCALING_POLICY_RELATIVE_ID_FORM: &str = "scalingPolicy:policy-id:autoScalingGroupName/\
                                               group-friendly-name:policyName/\
                                               policy-friendly-name";

/// How many undelivered scaling-policy invocations this store keeps.
///
/// The store's OWN bound, not a limit AWS publishes: an invocation waits until the service
/// that owns the policy takes it, and a policy ARN no mounted service claims would otherwise
/// keep its invocations for ever. Set far above the invocations a client can have in flight (a
/// handful of alarms, each firing a handful of actions), and the OLDEST is what a new one
/// pushes out — a scaling service reads the newest transition of each of its policies even
/// when it has not settled for a long time.
pub const MAX_PENDING_SCALING_POLICY_INVOCATIONS: usize = 64;

/// How long an untaken scaling-policy invocation waits before it is dropped, in seconds.
///
/// The store's own bound again, chosen rather than documented: AWS's own hand-off is
/// immediate, so an invocation still queued this long after it was executed is one no mounted
/// service receives its policies this way (see the module docs). Dropping it keeps such a
/// policy's invocations from filling the queue — and so from pushing out a hand-over the
/// service that does take them has not reached yet — while leaving far more time than the
/// clock of a receiving service needs to settle.
pub const MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS: f64 = 900.0;

/// One hand-over of an alarm transition to the scaling policy an action named.
#[derive(Debug, Clone)]
pub struct ScalingPolicyInvocation {
    /// The policy the action named, as its ARN: what the receiving service resolves.
    pub policy_arn: String,
    /// The alarm whose transition fired it.
    pub alarm_name: String,
    /// The state that alarm moved to, which is what selected the action list this action was
    /// in.
    pub state_value: String,
    /// The instant, in epoch seconds, the action was executed at.
    pub invoked_at_secs: f64,
    /// The newest value of the window the alarm graded, for a policy whose adjustment is
    /// selected by it; `None` for a transition that graded no datapoint (a forced state, a
    /// composite alarm, an alarm with nothing to grade).
    pub metric_value: Option<f64>,
    /// The threshold that window was compared against, from the alarm's own configuration;
    /// `None` for an alarm compared against a band rather than a number.
    pub breach_threshold: Option<f64>,
}

/// Whether `arn` names a SCALING POLICY, and the ARN itself when it does.
///
/// The partition, the Region and the account are not matched: a scaling-policy action is one
/// whatever partition it names, and where the policy lives is part of resolving it rather than
/// part of what makes the ARN a policy's. The relative id is read against the form a policy's
/// is written in ([`names_a_scaling_policy`]), which is the same reading the alarm writers
/// refuse an ARN of this vendor outside that form by — so an action an alarm may carry is
/// exactly one this module can then hand over.
#[must_use]
pub fn of_action_arn(arn: &str) -> Option<&str> {
    let parsed = ::service_common::arn::parse(arn)?;
    (parsed.vendor == AUTOSCALING_ARN_VENDOR && names_a_scaling_policy(&parsed)).then_some(arn)
}

/// Whether the relative id of `parsed` is written in the form a scaling policy's is
/// ([`SCALING_POLICY_RELATIVE_ID_FORM`]): the `scalingPolicy` resource type, the policy's id,
/// the resource the policy scales ([`SCALED_RESOURCE_FIELDS`]) and the policy's own name
/// ([`POLICY_NAME_FIELD`]), in that order and each of them named.
///
/// The id of the resource a policy scales may itself carry colons
/// (`resource/lambda/function:send-mail:BLUE`, a Lambda alias's provisioned concurrency), so
/// the fields are read from the two ends of the relative id inwards rather than by counting
/// its colons. The policy's own NAME is the last field and is read to the end of the relative
/// id, because a scaling policy's name may carry a colon too (`PutScalingPolicy$PolicyName` is
/// one string of the printable range, which admits one), so there is no trailing field to
/// reject after it.
fn names_a_scaling_policy(parsed: &::service_common::arn::Arn<'_>) -> bool {
    let Some((policy_id, scaled_and_policy)) = parsed
        .resource
        .strip_prefix(SCALING_POLICY_RESOURCE_TYPE)
        .and_then(|rest| rest.strip_prefix(':'))
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    let Some((scaled, policy_name)) = scaled_and_policy.rsplit_once(POLICY_NAME_FIELD) else {
        return false;
    };
    !policy_id.is_empty()
        && !policy_name.is_empty()
        && SCALED_RESOURCE_FIELDS
            .iter()
            .any(|field| scaled.strip_prefix(field).is_some_and(|named| !named.is_empty()))
}

/// Why no alarm can carry the action `arn`: it names this vendor, whose relative id CloudWatch
/// publishes the FORM of, with a relative id that form does not admit — `None` when `arn` names
/// another vendor's resource, or a policy.
///
/// The one reading of the relative id, shared with the reading a HAND-OVER does
/// ([`of_action_arn`]), so an action an alarm is allowed to carry is exactly one that can then
/// be delivered. The refusal names the RELATIVE ID the request sent — the resource field,
/// everything the ARN names after its account, which is the part that says which policy is
/// being asked for — and the form the vendor publishes for it. The second sentence runs
/// straight into that form with no separator, as the service words it.
#[must_use]
pub fn unknown_relative_id_error(arn: &str) -> Option<String> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != AUTOSCALING_ARN_VENDOR || names_a_scaling_policy(&parsed) {
        return None;
    }
    Some(format!(
        "The vendor \"{AUTOSCALING_ARN_VENDOR}\" cannot be used with relative id {}. Allowed \
         relative IDs must be in the format{SCALING_POLICY_RELATIVE_ID_FORM}",
        parsed.resource,
    ))
}

/// Execute the scaling-policy action `action` of `notification`: queue the hand-over for the
/// service that owns the policy, as of `now_secs`.
///
/// Its own critical section, taken and released as the action is executed, exactly as the
/// record of the execution is ([`super::record_action`]): nothing awaits while the store is
/// held, and the pass goes on to the next action with the hand-over already queued.
pub fn execute(
    state: &::engine::ServiceState<
        crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    >,
    notification: &super::AlarmNotification,
    action: &str,
    now_secs: f64,
) {
    let mut store = state.lock();
    let breach = store
        .alarms
        .get(&notification.alarm_name)
        .map(|stored| graded_breach(&stored.alarm));
    let (metric_value, breach_threshold) = breach.unwrap_or((None, None));
    queue(
        &mut store,
        ScalingPolicyInvocation {
            policy_arn: action.to_string(),
            alarm_name: notification.alarm_name.clone(),
            state_value: notification.new_state_value.clone(),
            invoked_at_secs: now_secs,
            metric_value,
            breach_threshold,
        },
        now_secs,
    );
}

/// Queue one hand-over, inside the two bounds the queue holds: nothing older than
/// [`MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS`] stays, and no more than
/// [`MAX_PENDING_SCALING_POLICY_INVOCATIONS`] wait.
///
/// The stale ones go FIRST, so a hand-over nobody receives is what the queue sheds rather than
/// a fresh one it is holding for a service that has not settled yet.
fn queue(
    store: &mut crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    invocation: ScalingPolicyInvocation,
    now_secs: f64,
) {
    store.pending_scaling_policy_invocations.push_back(invocation);
    store.pending_scaling_policy_invocations.retain(|queued| {
        now_secs - queued.invoked_at_secs <= MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS
    });
    while store.pending_scaling_policy_invocations.len() > MAX_PENDING_SCALING_POLICY_INVOCATIONS {
        store.pending_scaling_policy_invocations.pop_front();
    }
}

/// Take every queued invocation whose policy `is_owned` claims, oldest first, leaving the rest
/// queued for the service that owns them.
///
/// The seam the scaling services read this store through: a policy ARN resolves in the service
/// that minted it and nowhere else, so each one takes its own and never another's.
pub fn take_invocations(
    store: &mut crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    is_owned: impl Fn(&str) -> bool,
) -> Vec<ScalingPolicyInvocation> {
    let mut taken = Vec::new();
    let mut left =
        ::std::collections::VecDeque::with_capacity(store.pending_scaling_policy_invocations.len());
    while let Some(invocation) = store.pending_scaling_policy_invocations.pop_front() {
        if is_owned(&invocation.policy_arn) {
            taken.push(invocation);
        } else {
            left.push_back(invocation);
        }
    }
    store.pending_scaling_policy_invocations = left;
    taken
}

/// The breach the alarm's last evaluation graded: the newest value of the window it decided on
/// and the threshold it was compared against.
///
/// Both come from the alarm itself — `MetricAlarm$Threshold` and the decision record the
/// evaluation left in `$StateReasonData`, whose `recentDatapoints` are "the values the
/// evaluation really had, oldest first" ([`crate::state::alarm_evaluation`]) — so a hand-over
/// carries the numbers the state rests on rather than any value a request supplied.
fn graded_breach(alarm: &MetricAlarm) -> (Option<f64>, Option<f64>) {
    let newest = alarm
        .state_reason_data
        .as_deref()
        .and_then(|data| ::serde_json::from_str::<::serde_json::Value>(data).ok())
        .as_ref()
        .and_then(|document| document.get("recentDatapoints").cloned())
        .and_then(|points| {
            points
                .as_array()
                .and_then(|points| points.last().and_then(::serde_json::Value::as_f64))
        });
    (newest, alarm.threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One invocation of `policy_arn`, executed at `invoked_at_secs`.
    fn invocation(policy_arn: &str, invoked_at_secs: f64) -> ScalingPolicyInvocation {
        ScalingPolicyInvocation {
            policy_arn: policy_arn.to_string(),
            alarm_name: "watcher".to_string(),
            state_value: crate::state::ALARM_STATE_ALARM.to_string(),
            invoked_at_secs,
            metric_value: None,
            breach_threshold: None,
        }
    }

    /// The queue holds what a receiving service has not taken yet inside both of its bounds:
    /// an invocation nobody took for longer than the age bound is dropped, and past the count
    /// bound the OLDEST goes.
    #[test]
    fn the_queue_sheds_the_stale_invocations_before_the_fresh_ones() {
        let mut store = crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State::default();
        let now = 1_000_000.0;
        // Older than the age bound, so the next queueing drops it.
        queue(
            &mut store,
            invocation("unclaimed", now - MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS - 1.0),
            now - MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS - 1.0,
        );
        // Exactly at the bound, so it stays.
        queue(
            &mut store,
            invocation("at-the-bound", now - MAX_PENDING_SCALING_POLICY_INVOCATION_AGE_SECS),
            now,
        );
        queue(&mut store, invocation("fresh", now), now);
        let held: Vec<&str> = store
            .pending_scaling_policy_invocations
            .iter()
            .map(|queued| queued.policy_arn.as_str())
            .collect();
        assert_eq!(
            held,
            vec!["at-the-bound", "fresh"],
            "case \"an invocation past the age bound\": expected only the ones inside it, got \
             {held:?}"
        );

        for index in 0..MAX_PENDING_SCALING_POLICY_INVOCATIONS {
            queue(&mut store, invocation(&format!("p-{index}"), now), now);
        }
        assert_eq!(
            store.pending_scaling_policy_invocations.len(),
            MAX_PENDING_SCALING_POLICY_INVOCATIONS,
            "case \"past the count bound\": the queue stays inside it"
        );
        assert_eq!(
            store
                .pending_scaling_policy_invocations
                .front()
                .map(|queued| queued.policy_arn.as_str()),
            Some("p-0"),
            "case \"past the count bound\": the oldest is what a new one pushes out, so the two \
             queued before this run are the ones that went"
        );
    }

    /// A take removes the invocations of the policies the caller claims and leaves every other
    /// one queued, in order, for the service that owns it.
    #[test]
    fn a_take_leaves_another_services_invocations_queued() {
        let mut store = crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State::default();
        let now = 1_000.0;
        for policy in ["mine-1", "theirs-1", "mine-2", "theirs-2"] {
            queue(&mut store, invocation(policy, now), now);
        }
        let taken = take_invocations(&mut store, |policy_arn| policy_arn.starts_with("mine-"));
        let taken: Vec<&str> = taken.iter().map(|one| one.policy_arn.as_str()).collect();
        assert_eq!(
            taken,
            vec!["mine-1", "mine-2"],
            "case \"a take of the caller's own policies\": expected them oldest first, got \
             {taken:?}"
        );
        let left: Vec<&str> = store
            .pending_scaling_policy_invocations
            .iter()
            .map(|queued| queued.policy_arn.as_str())
            .collect();
        assert_eq!(
            left,
            vec!["theirs-1", "theirs-2"],
            "case \"a take of the caller's own policies\": every other one stays queued in \
             order, got {left:?}"
        );
    }

    /// Which ARNs name a scaling-policy action: the form the model documents for an Amazon EC2
    /// Auto Scaling policy, the one Application Auto Scaling mints, and the ARNs of other
    /// kinds that do not.
    #[test]
    fn a_scaling_policy_action_is_told_from_every_other_action_kind() {
        for (case, arn, expected) in [
            (
                "an Amazon EC2 Auto Scaling policy",
                "arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:\
                 7bb0e3a5-0000-4000-8000-000000000000:autoScalingGroupName/web:policyName/out",
                true,
            ),
            (
                "an Application Auto Scaling policy",
                "arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:\
                 7bb0e3a5-0000-4000-8000-000000000000:resource/ecs/service/web/api:policyName/out",
                true,
            ),
            (
                "another partition's policy",
                "arn:aws-us-gov:autoscaling:us-gov-west-1:000000000000:scalingPolicy:1:\
                 autoScalingGroupName/web:policyName/out",
                true,
            ),
            (
                "an Auto Scaling GROUP of the same vendor",
                "arn:aws:autoscaling:us-west-2:000000000000:autoScalingGroup:1:\
                 autoScalingGroupName/web",
                false,
            ),
            (
                "an SNS topic",
                "arn:aws:sns:us-west-2:000000000000:alarms",
                false,
            ),
            (
                "an EC2 instance action",
                "arn:aws:automate:us-west-2:ec2:terminate",
                false,
            ),
            ("not an ARN at all", "scale-out", false),
        ] {
            let found = of_action_arn(arn).is_some();
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected} for {arn:?}, got {found}"
            );
        }
    }

    /// A relative id of this vendor outside the published form is reported with the form, and
    /// one inside it — including a scaled resource whose own id carries colons — with nothing;
    /// an ARN of any other vendor is not judged by this rule at all.
    #[test]
    fn a_relative_id_outside_the_published_form_is_reported_with_that_form() {
        let head = "arn:aws:autoscaling:us-west-2:000000000000:";
        for (case, relative_id) in [
            ("a policy with no policyName field", "scalingPolicy:1:autoScalingGroupName/web"),
            ("a policy with no scaled resource", "scalingPolicy:1:policyName/out"),
            ("a policy with no id", "scalingPolicy::autoScalingGroupName/web:policyName/out"),
            ("a policy with no group named", "scalingPolicy:1:autoScalingGroupName/:policyName/out"),
            ("a policy with no name named", "scalingPolicy:1:autoScalingGroupName/web:policyName/"),
            ("a policy scaling something unnamed", "scalingPolicy:1:launchConfiguration/web:policyName/out"),
            ("the resource type alone", "scalingPolicy"),
            ("an Auto Scaling group", "autoScalingGroup:1:autoScalingGroupName/web"),
        ] {
            let reported = unknown_relative_id_error(&format!("{head}{relative_id}"));
            assert_eq!(
                reported.as_deref(),
                Some(
                    format!(
                        "The vendor \"autoscaling\" cannot be used with relative id \
                         {relative_id}. Allowed relative IDs must be in the \
                         formatscalingPolicy:policy-id:autoScalingGroupName/group-friendly-name:\
                         policyName/policy-friendly-name"
                    )
                    .as_str()
                ),
                "case {case:?}: expected the vendor refusal for {relative_id:?}, got {reported:?}"
            );
        }

        for (case, arn) in [
            (
                "an Amazon EC2 Auto Scaling policy",
                format!("{head}scalingPolicy:1:autoScalingGroupName/web:policyName/out"),
            ),
            (
                "an Application Auto Scaling policy",
                format!("{head}scalingPolicy:1:resource/ecs/service/web/api:policyName/out"),
            ),
            (
                "a policy scaling a resource whose id carries colons",
                format!("{head}scalingPolicy:1:resource/lambda/function:send-mail:BLUE:policyName/out"),
            ),
            (
                "a policy whose own name carries a colon",
                format!("{head}scalingPolicy:1:autoScalingGroupName/web:policyName/out:east"),
            ),
            (
                "an SNS topic named like a policy",
                "arn:aws:sns:us-west-2:000000000000:scalingPolicy".to_string(),
            ),
            ("an EC2 instance action", "arn:aws:automate:us-west-2:ec2:terminate".to_string()),
            ("not an ARN at all", "scale-out".to_string()),
        ] {
            let reported = unknown_relative_id_error(&arn);
            assert_eq!(
                reported, None,
                "case {case:?}: expected {arn:?} to be judged by no rule of this vendor, got \
                 {reported:?}"
            );
        }
    }

    /// The breach a hand-over carries is read off the alarm's own threshold and the newest
    /// value of the window its last evaluation graded; an alarm whose decision record holds no
    /// datapoint carries none.
    #[test]
    fn the_graded_breach_comes_from_the_alarms_own_decision_record() {
        let with_points = MetricAlarm {
            threshold: Some(50.0),
            state_reason_data: Some(
                ::serde_json::json!({"recentDatapoints": [10.0, 90.5]}).to_string(),
            ),
            ..MetricAlarm::default()
        };
        assert_eq!(
            graded_breach(&with_points),
            (Some(90.5), Some(50.0)),
            "an alarm whose record names the graded window carries its newest value"
        );

        let without_points = MetricAlarm {
            threshold: Some(50.0),
            state_reason_data: Some(::serde_json::json!({"recentDatapoints": []}).to_string()),
            ..MetricAlarm::default()
        };
        assert_eq!(
            graded_breach(&without_points),
            (None, Some(50.0)),
            "an alarm that graded no datapoint carries no metric value"
        );

        assert_eq!(
            graded_breach(&MetricAlarm::default()),
            (None, None),
            "an alarm with neither a threshold nor a decision record carries neither"
        );
    }
}
