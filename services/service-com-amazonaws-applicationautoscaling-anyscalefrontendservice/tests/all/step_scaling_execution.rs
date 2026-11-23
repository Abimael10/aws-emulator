// SPDX-License-Identifier: CC0-1.0

//! EXECUTING a step scaling policy: what a CloudWatch alarm breach does to the resource a
//! scalable target scales, end to end over the wire.
//!
//! The rule under test: when an alarm that executes a scaling policy as one of its actions is
//! in `ALARM`, Application Auto Scaling executes that policy — it picks the step adjustment
//! whose threshold-relative interval the breach falls in, computes the capacity that
//! adjustment asks for as its `AdjustmentType` interprets it, CLAMPS that capacity into the
//! `MinCapacity`/`MaxCapacity` the scalable target carries at that instant (an adjustment that
//! overshoots a bound is performed AT the bound, not refused), sets the capacity of the
//! resource in the service that owns it, and records the movement as one scaling activity.
//!
//! The docs the rows are read off:
//!
//! * `StepAdjustment$MetricIntervalLowerBound` / `$MetricIntervalUpperBound`: "The lower bound
//!   for the difference between the alarm threshold and the CloudWatch metric. If the metric
//!   value is above the breach threshold, the lower bound is inclusive […] Otherwise, it's
//!   exclusive", and the mirror of that for the upper bound; "A null value indicates negative
//!   infinity" / "positive infinity".
//! * `StepScalingPolicyConfiguration$AdjustmentType`: "Specifies how the ScalingAdjustment
//!   value in a StepAdjustment is interpreted […] ChangeInCapacity, ExactCapacity, and
//!   PercentChangeInCapacity" (`ADJUSTMENT_TYPE_VALUES`).
//! * `StepScalingPolicyConfiguration$MinAdjustmentMagnitude`: "The minimum value to scale by
//!   when the adjustment type is PercentChangeInCapacity."
//! * `StepScalingPolicyConfiguration$Cooldown`: "The amount of time, in seconds, to wait for a
//!   previous scaling activity to take effect. If not specified, the default value is 300."
//! * `RegisterScalableTarget$ScalableDimension`: "ecs:service:DesiredCount - The task count of
//!   an ECS service", which is the capacity an `ecs` target moves.
//!
//! And a breach does not stop at one execution — but what raises the next one is the ALARM
//! MOVING, not the state it holds. "The actions to execute when this alarm transitions to the
//! ALARM state from any other state" (`PutMetricAlarmInput$AlarmActions`) is the whole of the
//! wiring, so a metric that goes on breaching while the alarm stays in `ALARM` executes the
//! policy once: the capacity moves by one step adjustment and then HOLDS, however many further
//! evaluations grade the same state. The next step comes when the alarm leaves `ALARM` and
//! returns to it, and then only outside the policy's `Cooldown` — the wait a transition inside
//! is netted against ([`a_breach_that_does_not_let_up_moves_the_target_once_per_transition`]).
//! And the movement that one transition performed settles where a movement settles: terminal
//! at `Successful` once the owning service reports the capacity in effect, with no later
//! movement to override it ([`a_sustained_breach_settles_one_successful_activity`]).
//!
//! The same two rules together bound what the policies of ONE scalable target can reach. A
//! policy whose alarm merely stays in `ALARM` raises nothing further, and one triggered while
//! another policy's wait is still running is decided against the capacity that policy's
//! decision was taken FROM — so the capacity a client reads is one adjustment past that base
//! and never the sum of the adjustments the triggered policies state
//! ([`a_policy_triggered_inside_another_policys_wait_adds_one_adjustment_and_no_more`],
//! [`a_policy_triggered_inside_another_policys_wait_is_bounded_by_the_window_in_force`]).
//!
//! And what bounds a SUCCESSION of actions on one policy is the same rule applied at each of
//! them: one action applies the one adjustment whose interval the breach falls in, to the
//! capacity the target holds when the action is raised, clamped into the registered window — so
//! after `n` actions the capacity is `n` single adjustments from where it started and never more,
//! whichever `AdjustmentType` states them and however many steps the configuration carries
//! ([`each_alarm_action_applies_exactly_one_step_adjustment`]).
//!
//! Every case goes through the same front door a client uses — the policy, the alarm, the
//! breach and the read-back of the ECS service are all requests — so nothing reaches into a
//! state struct and a capacity that is not computed from persisted state cannot pass.

use std::time::Duration;

use serde_json::{json, Value};

use crate::emulator::{Emulator, PolicyAlarm};

/// The namespace an `ecs:service:DesiredCount` target is registered in.
const NAMESPACE: &str = "ecs";

/// The dimension every row scales.
const DIMENSION: &str = "ecs:service:DesiredCount";

/// The metric namespace the suite's own alarms watch. A customer namespace, so a datapoint for
/// it can be published over the wire the way a client publishes one.
const METRIC_NAMESPACE: &str = "Emulator/StepScaling";

/// The metric those alarms watch inside it.
const METRIC_NAME: &str = "Load";

/// The threshold every alarm in the suite breaches, so a step's interval is a difference from
/// this value.
const THRESHOLD: f64 = 50.0;

/// The `Period` the suite's alarms are evaluated on, in seconds
/// ([`crate::emulator::Emulator::given_policy_alarm`]).
const ALARM_PERIOD_SECS: f64 = 60.0;

/// A time stamp inside the newest `period`-second window a CloudWatch alarm's evaluation grades:
/// the middle of the newest window that has already CLOSED.
///
/// A metric is aggregated into periods anchored at multiples of the period and an alarm is
/// evaluated over the periods that are complete, so a datapoint meant to decide an alarm now is
/// stamped here rather than in the period the clock is still in. The middle of the window is what
/// keeps the placement independent of where in a period the suite happens to run.
fn graded_window_stamp(period: f64) -> f64 {
    let now = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    ((now / period).floor() - 1.0) * period + period / 2.0
}

/// How long a poll that waits for the scaling clock keeps looking, and how long it sleeps
/// between looks. Generous: the clock wakes up several times inside this window, and nothing
/// in the emulator sleeps for it.
const CLOCK_WAIT: Duration = Duration::from_secs(6);
/// See [`CLOCK_WAIT`].
const CLOCK_POLL: Duration = Duration::from_millis(100);

/// One step adjustment of a row's policy: the bounds of the metric-to-threshold difference it
/// covers, and the adjustment it asks for.
struct Step {
    lower: Option<f64>,
    upper: Option<f64>,
    adjustment: i64,
}

/// One execution to pin: the window the target is registered with, the capacity the ECS
/// service starts at, the policy that fires, the direction its alarm breaches in, and the
/// capacity the movement must land on.
struct ExecutionRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with, which is the capacity the
    /// adjustment is computed from.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    steps: &'static [Step],
    min_adjustment_magnitude: Option<i64>,
    /// The alarm's `ComparisonOperator`, which names the side of the threshold the breach is
    /// on.
    comparison_operator: &'static str,
    /// Where the capacity lands once the adjustment is clamped into the window.
    capacity: i64,
}

/// The rows differ in the adjustment type, in the direction and magnitude of the adjustment,
/// in whether the result falls inside the window or past either bound, and in how many steps
/// the policy carries.
const EXECUTION_CATALOG: &[ExecutionRow] = &[
    // The adjustment overshoots the ceiling: the movement happens AT the ceiling.
    ExecutionRow {
        case: "change in capacity past the ceiling",
        starting: 4,
        min: 4,
        max: 6,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 10 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 6,
    },
    // The mirror of it: an adjustment that undershoots the floor moves to the floor.
    ExecutionRow {
        case: "change in capacity past the floor",
        starting: 4,
        min: 2,
        max: 6,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: None, upper: Some(0.0), adjustment: -10 }],
        min_adjustment_magnitude: None,
        comparison_operator: "LessThanThreshold",
        capacity: 2,
    },
    // An adjustment inside the window is performed as it stands.
    ExecutionRow {
        case: "change in capacity inside the window",
        starting: 4,
        min: 1,
        max: 10,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 3 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 7,
    },
    // `ExactCapacity`: the adjustment IS the capacity, and it is clamped like any other.
    ExecutionRow {
        case: "exact capacity past the ceiling",
        starting: 4,
        min: 1,
        max: 6,
        adjustment_type: "ExactCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 9 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 6,
    },
    ExecutionRow {
        case: "exact capacity inside the window",
        starting: 4,
        min: 1,
        max: 10,
        adjustment_type: "ExactCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 2,
    },
    // A percentage of the capacity: 30% of 7 is 2.1 units, taken toward zero.
    ExecutionRow {
        case: "percent change toward zero",
        starting: 7,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 30 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 9,
    },
    // 10% of 4 is 0.4 units, which is not nothing: a non-zero percentage moves by a unit.
    ExecutionRow {
        case: "percent change under one unit",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 10 }],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 5,
    },
    // The same percentage with the documented floor on its magnitude.
    ExecutionRow {
        case: "percent change under the minimum magnitude",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 10 }],
        min_adjustment_magnitude: Some(3),
        comparison_operator: "GreaterThanThreshold",
        capacity: 7,
    },
    // A percentage scale-in: 25% of 10 is 2.5 units, taken toward zero, removed.
    ExecutionRow {
        case: "percent change scaling in",
        starting: 10,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: None, upper: Some(0.0), adjustment: -25 }],
        min_adjustment_magnitude: None,
        comparison_operator: "LessThanThreshold",
        capacity: 8,
    },
    // Several steps: a breach whose size the alarm's state carries no datapoint for is the
    // SMALLEST one in its direction, the threshold itself, so the step at the threshold is the
    // one that fires.
    ExecutionRow {
        case: "the step at the threshold of a multi-step policy",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(10.0), adjustment: 1 },
            Step { lower: Some(10.0), upper: None, adjustment: 5 },
        ],
        min_adjustment_magnitude: None,
        comparison_operator: "GreaterThanThreshold",
        capacity: 5,
    },
];

/// `StepAdjustments` as the request carries them.
fn step_adjustments(steps: &[Step]) -> Value {
    Value::Array(
        steps
            .iter()
            .map(|step| {
                let mut adjustment = json!({"ScalingAdjustment": step.adjustment});
                if let Some(lower) = step.lower {
                    adjustment["MetricIntervalLowerBound"] = json!(lower);
                }
                if let Some(upper) = step.upper {
                    adjustment["MetricIntervalUpperBound"] = json!(upper);
                }
                adjustment
            })
            .collect(),
    )
}

/// The three identity members of an `ecs` scalable target on `resource_id`.
fn target(resource_id: &str) -> Value {
    json!({
        "ServiceNamespace": NAMESPACE,
        "ResourceId": resource_id,
        "ScalableDimension": DIMENSION,
    })
}

/// Register the target `resource_id` names with the window `min`..`max`.
async fn register(emulator: &Emulator, resource_id: &str, min: i64, max: i64, case: &str) {
    let mut request = target(resource_id);
    request["MinCapacity"] = json!(min);
    request["MaxCapacity"] = json!(max);
    let (status, body) = emulator.call("RegisterScalableTarget", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected RegisterScalableTarget to answer 200, got {status} {body:?}"
    );
}

/// Put a step scaling policy and answer the `PolicyARN` it reports, which is the action an
/// alarm executes.
async fn put_step_policy(
    emulator: &Emulator,
    resource_id: &str,
    policy_name: &str,
    configuration: Value,
    case: &str,
) -> String {
    let mut request = target(resource_id);
    request["PolicyName"] = json!(policy_name);
    request["PolicyType"] = json!("StepScaling");
    request["StepScalingPolicyConfiguration"] = configuration;
    let (status, body) = emulator.call("PutScalingPolicy", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutScalingPolicy to answer 200, got {status} {body:?}"
    );
    body["PolicyARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a PolicyARN, got {body:?}"))
        .to_string()
}

/// The scaling activities recorded for one target, newest first.
async fn activities(emulator: &Emulator, resource_id: &str, case: &str) -> Vec<Value> {
    let (status, body) = emulator
        .call("DescribeScalingActivities", target(resource_id))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeScalingActivities to answer 200, got {status} {body:?}"
    );
    body["ScalingActivities"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ScalingActivities list, got {body:?}"))
        .clone()
}

/// Wait for the ECS service's `desiredCount` to reach `expected`, answering what it last read.
///
/// Nothing in the emulator sleeps for a movement: the poll is the TEST waiting for the clock
/// that executes a breach, so a row that never moves fails on the value it last saw.
async fn await_desired_count(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    expected: i64,
) -> i64 {
    let deadline = std::time::Instant::now() + CLOCK_WAIT;
    let mut last = emulator.ecs_desired_count(cluster, service).await;
    while last != expected && std::time::Instant::now() < deadline {
        tokio::time::sleep(CLOCK_POLL).await;
        last = emulator.ecs_desired_count(cluster, service).await;
    }
    last
}

#[tokio::test]
async fn an_alarm_breach_executes_the_policy_clamped_into_the_registered_window() {
    for row in EXECUTION_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let cluster = "scaled";
        let service = "web";
        let resource_id = emulator
            .given_ecs_service(cluster, service, row.starting)
            .await;
        register(&emulator, &resource_id, row.min, row.max, case).await;

        let mut configuration = json!({
            "AdjustmentType": row.adjustment_type,
            "StepAdjustments": step_adjustments(row.steps),
            // Nothing in a row breaches twice, so the wait a previous activity is given does
            // not come into it; it is named rather than defaulted so each row states it.
            "Cooldown": 0,
        });
        if let Some(magnitude) = row.min_adjustment_magnitude {
            configuration["MinAdjustmentMagnitude"] = json!(magnitude);
        }
        let policy_arn =
            put_step_policy(&emulator, &resource_id, "step-policy", configuration, case).await;

        // Before the breach nothing has moved: the policy on its own changes no capacity.
        assert_eq!(
            emulator.ecs_desired_count(cluster, service).await,
            row.starting,
            "case {case:?}: expected putting the policy to move no capacity"
        );

        let alarm_name = format!("{case}-alarm");
        emulator
            .given_policy_alarm(&PolicyAlarm {
                name: &alarm_name,
                policy_arn: &policy_arn,
                metric_namespace: METRIC_NAMESPACE,
                metric_name: METRIC_NAME,
                threshold: THRESHOLD,
                comparison_operator: row.comparison_operator,
            })
            .await;
        emulator.set_alarm_state(&alarm_name, "ALARM").await;

        let moved = await_desired_count(&emulator, cluster, service, row.capacity).await;
        assert_eq!(
            moved, row.capacity,
            "case {case:?}: expected the ECS desiredCount to reach {}, got {moved:?}",
            row.capacity
        );

        // The movement is reported as one scaling activity, which names the capacity it set
        // and the alarm and policy that caused it.
        let listed = activities(&emulator, &resource_id, case).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one scaling activity, got {listed:?}"
        );
        let activity = &listed[0];
        let expected_description = format!("Setting desired count to {}.", row.capacity);
        assert_eq!(
            activity["Description"],
            json!(expected_description),
            "case {case:?}: expected Description {expected_description:?}, got {activity:?}"
        );
        let expected_cause =
            format!("monitor alarm {alarm_name} in state ALARM triggered policy step-policy");
        assert_eq!(
            activity["Cause"],
            json!(expected_cause),
            "case {case:?}: expected Cause {expected_cause:?}, got {activity:?}"
        );
        assert_eq!(
            activity["NotScaledReasons"],
            Value::Null,
            "case {case:?}: expected a scaled activity to carry no NotScaledReasons, \
             got {activity:?}"
        );

        // And the window is what held the movement: the target reports the same bounds it was
        // registered with, whatever the adjustment asked for.
        let (status, body) = emulator
            .call(
                "DescribeScalableTargets",
                json!({"ServiceNamespace": NAMESPACE, "ResourceIds": [resource_id]}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeScalableTargets to answer 200, got {status} {body:?}"
        );
        let reported = &body["ScalableTargets"][0];
        assert_eq!(
            (reported["MinCapacity"].as_i64(), reported["MaxCapacity"].as_i64()),
            (Some(row.min), Some(row.max)),
            "case {case:?}: expected the registered window to be unchanged, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_breach_whose_size_the_alarm_measured_picks_the_step_that_size_falls_in() {
    // The same two-step policy as the catalogue's multi-step row, breached by a DATAPOINT 25
    // past the threshold instead of by a forced state: the difference falls in the second
    // step's interval, so that step is the one performed.
    let case = "measured breach past the first step";
    let emulator = Emulator::empty();
    let cluster = "measured";
    let service = "web";
    let resource_id = emulator.given_ecs_service(cluster, service, 4).await;
    register(&emulator, &resource_id, 1, 20, case).await;
    let policy_arn = put_step_policy(
        &emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": "ChangeInCapacity",
            "Cooldown": 0,
            "StepAdjustments": [
                {"MetricIntervalLowerBound": 0, "MetricIntervalUpperBound": 10,
                 "ScalingAdjustment": 1},
                {"MetricIntervalLowerBound": 10, "ScalingAdjustment": 5},
            ],
        }),
        case,
    )
    .await;
    let alarm_name = "measured-alarm";
    emulator
        .given_policy_alarm(&PolicyAlarm {
            name: alarm_name,
            policy_arn: &policy_arn,
            metric_namespace: METRIC_NAMESPACE,
            metric_name: METRIC_NAME,
            threshold: THRESHOLD,
            comparison_operator: "GreaterThanThreshold",
        })
        .await;

    // One datapoint 25 past the threshold, published the way a client publishes one, inside the
    // newest period the alarm's evaluation GRADES: a metric is aggregated into periods anchored
    // at multiples of the alarm's `Period` and an evaluation grades the periods that have
    // CLOSED, so a datapoint stamped in the period the clock is still in would be graded only
    // once that period ends. The alarm CloudWatch evaluates from it is what fires the policy.
    let (status, body) = emulator
        .call_cloudwatch(
            "PutMetricData",
            json!({
                "Namespace": METRIC_NAMESPACE,
                "MetricData": [{
                    "MetricName": METRIC_NAME,
                    "Value": THRESHOLD + 25.0,
                    "Timestamp": graded_window_stamp(ALARM_PERIOD_SECS),
                }],
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutMetricData to answer 200, got {status} {body:?}"
    );

    let moved = await_desired_count(&emulator, cluster, service, 9).await;
    assert_eq!(
        moved, 9,
        "case {case:?}: expected the second step (+5) to be the one performed, \
         so a desiredCount of 9, got {moved:?}"
    );
}

#[tokio::test]
async fn one_transition_is_executed_once_and_the_next_waits_for_the_cooldown() {
    let case = "one transition, one execution";
    let emulator = Emulator::empty();
    let cluster = "cooled";
    let service = "web";
    let resource_id = emulator.given_ecs_service(cluster, service, 4).await;
    register(&emulator, &resource_id, 1, 20, case).await;
    let policy_arn = put_step_policy(
        &emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": "ChangeInCapacity",
            "StepAdjustments": [{"MetricIntervalLowerBound": 0, "ScalingAdjustment": 2}],
        }),
        case,
    )
    .await;
    let alarm_name = "cooled-alarm";
    emulator
        .given_policy_alarm(&PolicyAlarm {
            name: alarm_name,
            policy_arn: &policy_arn,
            metric_namespace: METRIC_NAMESPACE,
            metric_name: METRIC_NAME,
            threshold: THRESHOLD,
            comparison_operator: "GreaterThanThreshold",
        })
        .await;
    emulator.set_alarm_state(alarm_name, "ALARM").await;

    let moved = await_desired_count(&emulator, cluster, service, 6).await;
    assert_eq!(
        moved, 6,
        "case {case:?}: expected the first breach to add 2 tasks, got {moved:?}"
    );

    // The alarm is STILL in ALARM: that is the same transition, which has already been
    // performed, so no further pass performs it again.
    for _ in 0..3 {
        let listed = activities(&emulator, &resource_id, case).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the one transition to be executed once, got {listed:?}"
        );
        tokio::time::sleep(CLOCK_POLL).await;
    }
    assert_eq!(
        emulator.ecs_desired_count(cluster, service).await,
        6,
        "case {case:?}: expected the capacity to stay where the one execution left it"
    );

    // A NEW transition into ALARM is a new breach — but this policy named no `Cooldown`, so
    // the documented default wait of 300 seconds is still in force: the breach waits, and
    // records no activity and no movement inside it.
    emulator.set_alarm_state(alarm_name, "OK").await;
    emulator.set_alarm_state(alarm_name, "ALARM").await;
    let listed = activities(&emulator, &resource_id, case).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected a breach inside the cooldown to record no activity, \
         got {listed:?}"
    );
    assert_eq!(
        emulator.ecs_desired_count(cluster, service).await,
        6,
        "case {case:?}: expected a breach inside the cooldown to move no capacity"
    );
}

#[tokio::test]
async fn a_suspended_direction_performs_no_movement() {
    let case = "scale out suspended";
    let emulator = Emulator::empty();
    let cluster = "suspended";
    let service = "web";
    let resource_id = emulator.given_ecs_service(cluster, service, 4).await;
    let mut registration = target(&resource_id);
    registration["MinCapacity"] = json!(1);
    registration["MaxCapacity"] = json!(20);
    // "DynamicScalingOutSuspended — Whether scale out by a target tracking scaling policy or a
    // step scaling policy is suspended."
    registration["SuspendedState"] = json!({"DynamicScalingOutSuspended": true});
    let (status, body) = emulator
        .call("RegisterScalableTarget", registration)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected RegisterScalableTarget to answer 200, got {status} {body:?}"
    );
    let policy_arn = put_step_policy(
        &emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": "ChangeInCapacity",
            "Cooldown": 0,
            "StepAdjustments": [{"MetricIntervalLowerBound": 0, "ScalingAdjustment": 2}],
        }),
        case,
    )
    .await;
    let alarm_name = "suspended-alarm";
    emulator
        .given_policy_alarm(&PolicyAlarm {
            name: alarm_name,
            policy_arn: &policy_arn,
            metric_namespace: METRIC_NAMESPACE,
            metric_name: METRIC_NAME,
            threshold: THRESHOLD,
            comparison_operator: "GreaterThanThreshold",
        })
        .await;
    emulator.set_alarm_state(alarm_name, "ALARM").await;

    // Give the clock the same window a row that DOES move is given, so this asserts the
    // movement never happens rather than that it had not happened yet.
    let settled = await_desired_count(&emulator, cluster, service, 6).await;
    assert_eq!(
        settled, 4,
        "case {case:?}: expected a suspended scale-out direction to move no capacity, \
         got {settled:?}"
    );
    let listed = activities(&emulator, &resource_id, case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected no scaling activity at all, got {listed:?}"
    );
}

#[tokio::test]
async fn an_alarm_wired_to_another_policy_executes_nothing() {
    let case = "an alarm that names no policy of this target";
    let emulator = Emulator::empty();
    let cluster = "unwired";
    let service = "web";
    let resource_id = emulator.given_ecs_service(cluster, service, 4).await;
    register(&emulator, &resource_id, 1, 20, case).await;
    put_step_policy(
        &emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": "ChangeInCapacity",
            "Cooldown": 0,
            "StepAdjustments": [{"MetricIntervalLowerBound": 0, "ScalingAdjustment": 2}],
        }),
        case,
    )
    .await;
    // An alarm whose action is an SNS topic rather than this policy: the wiring is what makes
    // a policy alarm-driven, so a breach of this one executes nothing.
    let alarm_name = "unwired-alarm";
    emulator
        .given_policy_alarm(&PolicyAlarm {
            name: alarm_name,
            policy_arn: &format!(
                "arn:aws:sns:{}:{}:ops",
                crate::emulator::REGION,
                crate::emulator::ACCOUNT
            ),
            metric_namespace: METRIC_NAMESPACE,
            metric_name: METRIC_NAME,
            threshold: THRESHOLD,
            comparison_operator: "GreaterThanThreshold",
        })
        .await;
    emulator.set_alarm_state(alarm_name, "ALARM").await;

    let settled = await_desired_count(&emulator, cluster, service, 6).await;
    assert_eq!(
        settled, 4,
        "case {case:?}: expected no execution at all, got a desiredCount of {settled:?}"
    );
    let listed = activities(&emulator, &resource_id, case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected no scaling activity at all, got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// A breach that does not let up
// ---------------------------------------------------------------------------

/// The `Period` the alarm of a CONTINUING breach is evaluated on, in seconds.
///
/// The shortest cadence CloudWatch evaluates an alarm at — "Valid values are 10, 20, 30, and
/// any multiple of 60" (`PutMetricAlarmInput$Period`) — so a row that watches an alarm which
/// stays in `ALARM` watches over many evaluations of it, and a row that lets the breach up and
/// drives it back gets its next transition inside a few of them.
const CONTINUING_PERIOD_SECS: i32 = 10;

/// How long a row that keeps a breach alive waits for the capacity it expects.
///
/// Generous: many evaluation cadences, so a row fails on a capacity that never moves rather
/// than on a host that was busy for one of them. Nothing in the emulator sleeps for a
/// movement, so a row that is owed one gets it as soon as the clock takes its next pass.
const CONTINUING_WAIT: Duration = Duration::from_secs(120);

/// How often a row that keeps a breach alive looks again, and publishes the datapoint that
/// keeps the alarm breaching.
const CONTINUING_POLL: Duration = Duration::from_millis(500);

/// How long a row that expects NO further movement keeps watching: several evaluation cadences
/// of the still-breaching alarm, which is far more than one pass of the scaling clock takes to
/// move a capacity it is owed.
const QUIET_WATCH: Duration = Duration::from_secs(25);

/// One continuing breach to pin: the window the target is registered with, the capacity the
/// ECS service starts at, the policy the breaching alarm executes, and the capacities the
/// successive TRANSITIONS of that alarm have to drive it through.
struct ContinuingRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    steps: &'static [Step],
    /// The policy's `Cooldown`, in seconds: the wait a transition that arrives inside it is
    /// netted against, which for a repeat of the same step leaves nothing to apply.
    cooldown: i64,
    /// The alarm's `ComparisonOperator`, which names the side of the threshold it breaches on.
    comparison_operator: &'static str,
    /// How far past the threshold each published datapoint is, signed in the direction of the
    /// breach.
    breach: f64,
    /// The `NotScaledReason$Code` the transition after the climb is withheld with: the bound
    /// of the registered window the capacity has reached.
    reason_code: &'static str,
    /// The capacity each successive execution lands the target on, in order: the step the first
    /// transition applies, and then the step the next transition into `ALARM` adds — the last of
    /// them being the bound the registered window stops the policy at.
    capacities: &'static [i64],
}

/// The rows differ in how the adjustment is interpreted, in its magnitude and direction, in the
/// wait a transition is netted against, and in which bound of the registered window ends the
/// climb.
const CONTINUING_CATALOG: &[ContinuingRow] = &[
    // One task per transition, with no wait at all between them, to the ceiling.
    ContinuingRow {
        case: "one task per transition to the ceiling",
        starting: 1,
        min: 1,
        max: 3,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 1 }],
        cooldown: 0,
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        reason_code: "AlreadyAtMaxCapacity",
        capacities: &[2, 3],
    },
    // Two tasks per transition, with a wait the repeated step is netted against: a transition
    // inside it adds nothing, and the first one after it applies the next step.
    ContinuingRow {
        case: "two tasks per transition, one wait apart",
        starting: 1,
        min: 1,
        max: 5,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        cooldown: 20,
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        reason_code: "AlreadyAtMaxCapacity",
        capacities: &[3, 5],
    },
    // The mirror of the first row: a breach BELOW the threshold removes tasks, down to the
    // floor.
    ContinuingRow {
        case: "three tasks per transition down to the floor",
        starting: 8,
        min: 2,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: None, upper: Some(0.0), adjustment: -3 }],
        cooldown: 0,
        comparison_operator: "LessThanThreshold",
        breach: -25.0,
        reason_code: "AlreadyAtMinCapacity",
        capacities: &[5, 2],
    },
    // A percentage is a percentage of the capacity the execution FINDS, so the steps grow:
    // 50% of 4 is 2, and 50% of the 6 the next transition finds is 3.
    ContinuingRow {
        case: "a percentage of the capacity each execution finds",
        starting: 4,
        min: 1,
        max: 9,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 50 }],
        cooldown: 0,
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        reason_code: "AlreadyAtMaxCapacity",
        capacities: &[6, 9],
    },
];

/// Create an alarm that drives a continuing breach of `metric_name`: the same wiring a step
/// scaling policy is attached by — the policy's ARN is the action the alarm executes —
/// evaluated on [`CONTINUING_PERIOD_SECS`], so a datapoint published for it is graded that
/// often.
async fn given_breaching_alarm_on(
    emulator: &Emulator,
    alarm_name: &str,
    metric_name: &str,
    policy_arn: &str,
    comparison_operator: &str,
    case: &str,
) {
    let (status, body) = emulator
        .call_cloudwatch(
            "PutMetricAlarm",
            json!({
                "AlarmName": alarm_name,
                "Namespace": METRIC_NAMESPACE,
                "MetricName": metric_name,
                "Statistic": "Average",
                "Period": CONTINUING_PERIOD_SECS,
                "EvaluationPeriods": 1,
                "Threshold": THRESHOLD,
                "ComparisonOperator": comparison_operator,
                "AlarmActions": [policy_arn],
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutMetricAlarm for {alarm_name:?} to answer 200, \
         got {status} {body:?}"
    );
}

/// Create the alarm a continuing breach of the suite's own metric is driven by
/// ([`given_breaching_alarm_on`]).
async fn given_continuing_alarm(
    emulator: &Emulator,
    alarm_name: &str,
    policy_arn: &str,
    comparison_operator: &str,
    case: &str,
) {
    given_breaching_alarm_on(
        emulator,
        alarm_name,
        METRIC_NAME,
        policy_arn,
        comparison_operator,
        case,
    )
    .await;
}

/// Publish one datapoint `offset` past the threshold on `metric_name`, the way a client under a
/// load that does not let up keeps publishing the series its alarm grades.
///
/// High resolution, so the datapoint stands for the second it was taken at rather than for the
/// whole minute, and stamped inside the newest period the evaluation grades
/// ([`graded_window_stamp`]) — which is what keeps the alarm the policy is wired to IN `ALARM`.
async fn publish_breach_on(emulator: &Emulator, metric_name: &str, offset: f64, case: &str) {
    let (status, body) = emulator
        .call_cloudwatch(
            "PutMetricData",
            json!({
                "Namespace": METRIC_NAMESPACE,
                "MetricData": [{
                    "MetricName": metric_name,
                    "Value": THRESHOLD + offset,
                    "Timestamp": graded_window_stamp(f64::from(CONTINUING_PERIOD_SECS)),
                    "StorageResolution": 1,
                }],
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutMetricData for {metric_name:?} to answer 200, \
         got {status} {body:?}"
    );
}

/// Publish one datapoint `offset` past the threshold on the suite's own metric
/// ([`publish_breach_on`]).
async fn publish_breach(emulator: &Emulator, offset: f64, case: &str) {
    publish_breach_on(emulator, METRIC_NAME, offset, case).await;
}

/// Keep the breach alive and wait for the ECS service's `desiredCount` to reach `expected`,
/// answering what it last read.
///
/// The poll is the TEST waiting for the clock that executes a breach — nothing in the emulator
/// sleeps for one — and every look publishes the datapoint that keeps the alarm breaching, so
/// what the row waits on is the execution the alarm's own transition raised.
async fn await_while_breaching(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    breach: f64,
    expected: i64,
    wait: Duration,
    case: &str,
) -> i64 {
    let deadline = std::time::Instant::now() + wait;
    loop {
        publish_breach(emulator, breach, case).await;
        let last = emulator.ecs_desired_count(cluster, service).await;
        if last == expected || std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep the breach alive for the whole of `watch` and answer the `desiredCount` the ECS service
/// last reported — returning as soon as it LEAVES `held`, which is the movement a row that
/// expects none fails on.
async fn watch_while_breaching(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    breach: f64,
    held: i64,
    watch: Duration,
    case: &str,
) -> i64 {
    let deadline = std::time::Instant::now() + watch;
    loop {
        publish_breach(emulator, breach, case).await;
        let last = emulator.ecs_desired_count(cluster, service).await;
        if last != held || std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// The state one alarm reports now.
async fn alarm_state(emulator: &Emulator, alarm_name: &str, case: &str) -> String {
    let (status, body) = emulator
        .call_cloudwatch("DescribeAlarms", json!({"AlarmNames": [alarm_name]}))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeAlarms to answer 200, got {status} {body:?}"
    );
    body["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Let the breach UP and drive it back, so the alarm transitions into `ALARM` afresh — the state
/// change that executes the actions it names again.
///
/// The letting up is `SetAlarmState`, the way a client moves an alarm without waiting for a
/// series to age out of its evaluation range; the return is the alarm's OWN evaluation of the
/// still-breaching datapoints, so what comes back is a transition CloudWatch decided and the
/// breach it carries is the measurement that decided it.
async fn given_fresh_transition(emulator: &Emulator, alarm_name: &str, breach: f64, case: &str) {
    emulator.set_alarm_state(alarm_name, "OK").await;
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach(emulator, breach, case).await;
        if alarm_state(emulator, alarm_name, case).await == "ALARM" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the still-breaching datapoints to put {alarm_name:?} back \
             in ALARM"
        );
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// How long one fresh transition is watched before a row raises another.
///
/// A transition raised inside the policy's `Cooldown` is netted against the movement in flight
/// and adds nothing, so a row whose wait has not elapsed yet gets its next step from a LATER
/// transition; this is how long each of them is given, inside the row's own generous deadline.
const TRANSITION_SETTLE: Duration = Duration::from_secs(5);

/// Raise fresh transitions of the alarm until the ECS service's `desiredCount` reaches
/// `expected`, answering what it last read.
async fn await_through_fresh_transitions(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    alarm_name: &str,
    breach: f64,
    expected: i64,
    case: &str,
) -> i64 {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        given_fresh_transition(emulator, alarm_name, breach, case).await;
        let last = await_while_breaching(
            emulator,
            cluster,
            service,
            breach,
            expected,
            TRANSITION_SETTLE,
            case,
        )
        .await;
        if last == expected || std::time::Instant::now() >= deadline {
            return last;
        }
    }
}

/// Raise fresh transitions of the alarm until the log reports a decision WITHHELD with `code`,
/// answering the reason that decision carries.
///
/// A decision that moved no capacity is reported only in the view that asks for it
/// (`DescribeScalingActivitiesRequest$IncludeNotScaledActivities`), so that is the view polled
/// here; `Value::Null` is what a wait that never saw one answers, which fails the row on the
/// reason it was looking for.
async fn await_withheld_reason(
    emulator: &Emulator,
    resource_id: &str,
    alarm_name: &str,
    breach: f64,
    code: &str,
    case: &str,
) -> Value {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        given_fresh_transition(emulator, alarm_name, breach, case).await;
        let settle = std::time::Instant::now() + TRANSITION_SETTLE;
        loop {
            let found = not_scaled_reasons(emulator, resource_id, case)
                .await
                .into_iter()
                .find(|reason| reason["Code"] == json!(code));
            if let Some(found) = found {
                return found;
            }
            if std::time::Instant::now() >= settle {
                break;
            }
            tokio::time::sleep(CONTINUING_POLL).await;
        }
        if std::time::Instant::now() >= deadline {
            return Value::Null;
        }
    }
}

/// The reason each withheld decision of one target carries, newest first.
async fn not_scaled_reasons(emulator: &Emulator, resource_id: &str, case: &str) -> Vec<Value> {
    let mut request = target(resource_id);
    request["IncludeNotScaledActivities"] = json!(true);
    let (status, body) = emulator.call("DescribeScalingActivities", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeScalingActivities to answer 200, got {status} {body:?}"
    );
    body["ScalingActivities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|activity| activity["NotScaledReasons"][0].clone())
        .filter(|reason| !reason.is_null())
        .collect()
}

/// What each activity that MOVED capacity says it set the capacity to, oldest first.
///
/// A decision that moved no capacity reports the reason it was withheld
/// (`ScalingActivity$NotScaledReasons`), so the movements are the activities that carry none:
/// this is the log of what the successive executions really did to the resource.
async fn movement_descriptions(emulator: &Emulator, resource_id: &str, case: &str) -> Vec<String> {
    let mut listed = activities(emulator, resource_id, case).await;
    listed.reverse();
    listed
        .iter()
        .filter(|activity| activity["NotScaledReasons"] == Value::Null)
        .map(|activity| {
            activity["Description"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a Description, got {activity:?}"))
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn a_breach_that_does_not_let_up_moves_the_target_once_per_transition() {
    for row in CONTINUING_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let cluster = "continuing";
        let service = "web";
        let resource_id = emulator
            .given_ecs_service(cluster, service, row.starting)
            .await;
        register(&emulator, &resource_id, row.min, row.max, case).await;
        let policy_arn = put_step_policy(
            &emulator,
            &resource_id,
            "step-policy",
            json!({
                "AdjustmentType": row.adjustment_type,
                "StepAdjustments": step_adjustments(row.steps),
                "Cooldown": row.cooldown,
            }),
            case,
        )
        .await;
        let alarm_name = "continuing-alarm";
        given_continuing_alarm(
            &emulator,
            alarm_name,
            &policy_arn,
            row.comparison_operator,
            case,
        )
        .await;

        // The alarm's transition into `ALARM` executes the policy, so the capacity moves by the
        // step whose interval the breach falls in.
        let first = *row.capacities.first().expect("a row names a capacity");
        let reached = await_while_breaching(
            &emulator,
            cluster,
            service,
            row.breach,
            first,
            CONTINUING_WAIT,
            case,
        )
        .await;
        assert_eq!(
            reached, first,
            "case {case:?}: expected the alarm's transition into ALARM to drive the ECS \
             desiredCount to {first}, got {reached:?}"
        );

        // And then it HOLDS. The metric goes on breaching and the alarm goes on being graded,
        // but it does not change state again, so there is no further action to execute: one
        // transition is one step adjustment, not one per evaluation.
        let held = watch_while_breaching(
            &emulator,
            cluster,
            service,
            row.breach,
            first,
            QUIET_WATCH,
            case,
        )
        .await;
        assert_eq!(
            held, first,
            "case {case:?}: expected the capacity to hold at {first} while the alarm merely \
             stays in ALARM, got {held:?}"
        );
        let state = alarm_state(&emulator, alarm_name, case).await;
        assert_eq!(
            state, "ALARM",
            "case {case:?}: expected the alarm to have been breaching throughout the watch, \
             got {state:?}"
        );

        // The next step comes from the next TRANSITION: the breach lets up, the alarm leaves
        // `ALARM`, and the still-breaching series puts it back there. A transition that lands
        // inside the policy's wait is netted against the movement in flight and adds nothing,
        // so it is the first one after that wait which applies the step.
        for expected in row.capacities.iter().skip(1) {
            let moved = await_through_fresh_transitions(
                &emulator,
                cluster,
                service,
                alarm_name,
                row.breach,
                *expected,
                case,
            )
            .await;
            assert_eq!(
                moved, *expected,
                "case {case:?}: expected a fresh transition into ALARM to drive the ECS \
                 desiredCount to {expected}, got {moved:?}"
            );
        }

        // The last capacity is the bound the registered window stops the policy at, so a
        // transition after it is DECIDED and withheld, which is what the reason the model names
        // says about it — "Application Auto Scaling will not scale capacity to values that are
        // outside of the minimum and maximum range".
        let withheld = await_withheld_reason(
            &emulator,
            &resource_id,
            alarm_name,
            row.breach,
            row.reason_code,
            case,
        )
        .await;
        assert_eq!(
            withheld["Code"].as_str().unwrap_or_default(),
            row.reason_code,
            "case {case:?}: expected a later transition to be withheld as {}, got {withheld:?}",
            row.reason_code
        );
        let last = *row.capacities.last().expect("a row names a capacity");
        assert_eq!(
            withheld["CurrentCapacity"].as_i64(),
            Some(last),
            "case {case:?}: expected the withheld decision to report the capacity the climb \
             ended at, got {withheld:?}"
        );
        let settled = emulator.ecs_desired_count(cluster, service).await;
        assert_eq!(
            settled, last,
            "case {case:?}: expected the registered window to stop the climb at {last}, \
             got {settled:?}"
        );

        // And each of those executions is one movement of the log, in the order they happened:
        // one per transition the policy was executed for, and none for the evaluations in
        // between.
        let expected: Vec<String> = row
            .capacities
            .iter()
            .map(|capacity| format!("Setting desired count to {capacity}."))
            .collect();
        let moved = movement_descriptions(&emulator, &resource_id, case).await;
        assert_eq!(
            moved, expected,
            "case {case:?}: expected one scaling activity per transition, got {moved:?}"
        );
    }
}

#[tokio::test]
async fn transitions_inside_the_cooldown_add_nothing() {
    // The same continuing breach with a wait far longer than the window this row watches: the
    // alarm lets up and comes back inside that wait, so each of those transitions is netted
    // against the movement still in flight — a repeat of the same step leaves nothing to apply —
    // and the capacity the first step set is the one the target holds for the whole of it.
    let case = "a breach that comes back inside the cooldown";
    let emulator = Emulator::empty();
    let cluster = "cooling";
    let service = "web";
    let resource_id = emulator.given_ecs_service(cluster, service, 1).await;
    register(&emulator, &resource_id, 1, 20, case).await;
    let policy_arn = put_step_policy(
        &emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": "ChangeInCapacity",
            "StepAdjustments": [{"MetricIntervalLowerBound": 0, "ScalingAdjustment": 1}],
            "Cooldown": 600,
        }),
        case,
    )
    .await;
    let alarm_name = "cooling-alarm";
    given_continuing_alarm(
        &emulator,
        alarm_name,
        &policy_arn,
        "GreaterThanThreshold",
        case,
    )
    .await;

    let first = await_while_breaching(&emulator, cluster, service, 25.0, 2, CONTINUING_WAIT, case)
        .await;
    assert_eq!(
        first, 2,
        "case {case:?}: expected the alarm's transition into ALARM to add one task, got {first:?}"
    );

    // Three further transitions, every one of them inside the 600-second wait the policy asked
    // for.
    for _ in 0..3 {
        given_fresh_transition(&emulator, alarm_name, 25.0, case).await;
    }
    let settled =
        watch_while_breaching(&emulator, cluster, service, 25.0, 2, QUIET_WATCH, case).await;
    assert_eq!(
        settled, 2,
        "case {case:?}: expected every transition inside the wait to add nothing, got {settled:?}"
    );
    let moved = movement_descriptions(&emulator, &resource_id, case).await;
    assert_eq!(
        moved,
        vec!["Setting desired count to 2.".to_string()],
        "case {case:?}: expected the one movement the wait has not elapsed since, got {moved:?}"
    );
}

// ---------------------------------------------------------------------------
// An action raised while the wait for the previous one is still running
// ---------------------------------------------------------------------------
//
// The wait a policy asks for — "the amount of time, in seconds, to wait for a previous scaling
// activity to take effect" (`StepScalingPolicyConfiguration$Cooldown`) — is not a window that
// discards what happens inside it. The movement it is a wait FOR is still in flight, so the
// capacity that movement already contributes is part of the capacity the next action of the
// same policy asks for rather than something that action adds to: an action raised inside the
// wait is computed from the capacity the in-flight movement started at, which nets that
// contribution out, and the REMAINDER alone is applied straight away. An action that asks for
// no more than the in-flight movement already delivered adds nothing, and a scale in is held
// back until the wait has elapsed.
//
// Each row raises the second action through a SECOND alarm of the same policy, on a metric of
// its own: "The actions to execute when this alarm transitions to the ALARM state"
// (`PutMetricAlarmInput$AlarmActions`) is the wiring, and a policy is the action of as many
// alarms as a client wires it to — so the two breaches a row drives are two measurements, each
// graded by its own alarm, rather than one alarm's state read twice.

/// The metric the alarm that fires SECOND watches, inside [`METRIC_NAMESPACE`]: its own
/// series, so the size of the breach it grades is independent of the one the first alarm
/// graded.
const SECOND_METRIC_NAME: &str = "LoadInsideTheWait";

/// The wait every row of these two catalogues gives a previous scaling activity, in seconds:
/// far longer than a row watches, so the second action of a row always lands INSIDE it.
const LONG_COOLDOWN_SECS: i64 = 600;

/// One action raised inside the wait for a previous one: the window the target is registered
/// with, the capacity the ECS service starts at, the policy both alarms execute, the breach
/// each of them grades, and where the capacity ends up.
struct NettingRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    steps: &'static [Step],
    /// The `ComparisonOperator` of the alarm that fires first, which names the side of the
    /// threshold its breach is on.
    first_operator: &'static str,
    /// How far past the threshold the first alarm's datapoint is, signed in the direction of
    /// its breach.
    first_breach: f64,
    /// The capacity that first action lands the target on, which is the movement the wait is
    /// then a wait for.
    first_capacity: i64,
    /// The `ComparisonOperator` of the alarm that fires while that wait is still running.
    second_operator: &'static str,
    /// How far past the threshold ITS datapoint is, signed in the direction of its breach.
    second_breach: f64,
    /// Where the capacity ends up: the netted remainder for an action that asks for more than
    /// the in-flight movement delivered, and the capacity the first action set for one the
    /// wait holds back.
    netted_capacity: i64,
}

/// The rows an action inside the wait MOVES capacity for. They differ in how the adjustment is
/// interpreted, in how far up the steps the second breach lands, in which bound of the
/// registered window the remainder is clamped by, and in the direction each of the two actions
/// is in.
const NETTED_CATALOG: &[NettingRow] = &[
    // The shape the rule is about: a breach that re-lands a step further up while the wait is
    // still running. The second step asks for 5 tasks on top of the 4 the target held when the
    // first step was computed, of which that step has already delivered 2 — so 3 are left.
    NettingRow {
        case: "a breach a step further up adds only what the first step has not delivered",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(10.0), adjustment: 1 },
            Step { lower: Some(10.0), upper: Some(20.0), adjustment: 2 },
            Step { lower: Some(20.0), upper: None, adjustment: 5 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 15.0,
        first_capacity: 6,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 9,
    },
    // Two steps further up, from the smallest step of the three: the whole remainder of the
    // larger step is applied at once rather than one step at a time.
    NettingRow {
        case: "a breach two steps further up adds the whole remainder at once",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(10.0), adjustment: 1 },
            Step { lower: Some(10.0), upper: Some(20.0), adjustment: 2 },
            Step { lower: Some(20.0), upper: None, adjustment: 8 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 5.0,
        first_capacity: 5,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 12,
    },
    // The remainder is a capacity like any other, so the registered window is what bounds it:
    // 4 plus 9 is above the ceiling, and the movement is performed AT the ceiling.
    NettingRow {
        case: "the remainder is clamped into the registered window",
        starting: 4,
        min: 1,
        max: 7,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(20.0), adjustment: 2 },
            Step { lower: Some(20.0), upper: None, adjustment: 9 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 10.0,
        first_capacity: 6,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 7,
    },
    // "For exact capacity, you must specify a non-negative value": the step names the capacity
    // itself, so what the in-flight movement delivered is already part of it and the action
    // inside the wait lands on the capacity its own step names.
    NettingRow {
        case: "an exact capacity inside the wait is the capacity its step names",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ExactCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(20.0), adjustment: 6 },
            Step { lower: Some(20.0), upper: None, adjustment: 9 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 10.0,
        first_capacity: 6,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 9,
    },
    // A percentage is a percentage of the capacity the adjustment is computed from, which
    // inside the wait is the capacity the in-flight movement started at: 100% of 4 is 4, of
    // which the first step's 50% has already delivered 2.
    NettingRow {
        case: "a percentage of the capacity the in-flight movement started at",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(20.0), adjustment: 50 },
            Step { lower: Some(20.0), upper: None, adjustment: 100 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 10.0,
        first_capacity: 6,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 8,
    },
    // The other direction: the movement in flight REMOVED capacity, so it contributes nothing
    // to a scale out and there is nothing to net — the step is added to the capacity the target
    // holds, in full, without waiting the wait out.
    NettingRow {
        case: "a scale out inside the wait for a scale in is performed in full",
        starting: 8,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: None, upper: Some(0.0), adjustment: -3 },
            Step { lower: Some(0.0), upper: None, adjustment: 5 },
        ],
        first_operator: "LessThanThreshold",
        first_breach: -25.0,
        first_capacity: 5,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 10,
    },
];

/// The rows an action inside the wait moves NO capacity for: the capacity it asks for is one
/// the in-flight movement has already delivered, or it is in the direction a wait holds back.
const HELD_CATALOG: &[NettingRow] = &[
    // The step the second breach lands in asks for exactly what the first one did, so netting
    // the in-flight movement out of it leaves nothing to apply.
    NettingRow {
        case: "a step the in-flight movement has already delivered in full adds nothing",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(20.0), adjustment: 2 },
            Step { lower: Some(20.0), upper: None, adjustment: 2 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 10.0,
        first_capacity: 6,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 6,
    },
    // And a step that asks for LESS than the in-flight movement delivered takes nothing back:
    // the capacity the target holds stands until the wait has elapsed.
    NettingRow {
        case: "a smaller step inside the wait takes nothing back",
        starting: 4,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: Some(0.0), upper: Some(20.0), adjustment: 5 },
            Step { lower: Some(20.0), upper: None, adjustment: 2 },
        ],
        first_operator: "GreaterThanThreshold",
        first_breach: 10.0,
        first_capacity: 9,
        second_operator: "GreaterThanThreshold",
        second_breach: 25.0,
        netted_capacity: 9,
    },
    // A deeper scale in inside the wait for a scale in: the direction a wait holds back, so it
    // adds nothing however much further down the steps the breach lands.
    NettingRow {
        case: "a deeper scale in inside the wait is held back",
        starting: 8,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[
            Step { lower: None, upper: Some(-10.0), adjustment: -4 },
            Step { lower: Some(-10.0), upper: Some(0.0), adjustment: -1 },
        ],
        first_operator: "LessThanThreshold",
        first_breach: -5.0,
        first_capacity: 7,
        second_operator: "LessThanThreshold",
        second_breach: -25.0,
        netted_capacity: 7,
    },
];

/// Keep a row's FIRST breach alive and wait for the ECS service's `desiredCount` to reach
/// `expected`, answering what it last read.
async fn await_first_action(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    row: &NettingRow,
    expected: i64,
) -> i64 {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach_on(emulator, METRIC_NAME, row.first_breach, row.case).await;
        let last = emulator.ecs_desired_count(cluster, service).await;
        if last == expected || std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep BOTH of a row's breaches alive and wait for the ECS service's `desiredCount` to leave
/// `held`, answering what it last read.
///
/// `wait` is what the row is waiting FOR: a row that expects the netted remainder waits for the
/// capacity to move at all, and a row that expects nothing watches for the whole span and
/// answers the capacity it still finds.
async fn await_action_inside_the_wait(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    row: &NettingRow,
    held: i64,
    wait: Duration,
) -> i64 {
    let deadline = std::time::Instant::now() + wait;
    loop {
        publish_breach_on(emulator, METRIC_NAME, row.first_breach, row.case).await;
        publish_breach_on(emulator, SECOND_METRIC_NAME, row.second_breach, row.case).await;
        let last = emulator.ecs_desired_count(cluster, service).await;
        if last != held || std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep a row's SECOND breach alive and wait for its alarm to report `ALARM`, answering the
/// state it last reported.
///
/// What a row that expects no movement needs pinned: the action was raised — the alarm that
/// executes the policy is breaching — and the wait is what left the capacity where it was.
async fn await_second_alarm_breaching(
    emulator: &Emulator,
    alarm_name: &str,
    row: &NettingRow,
) -> String {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach_on(emulator, SECOND_METRIC_NAME, row.second_breach, row.case).await;
        let state = alarm_state(emulator, alarm_name, row.case).await;
        if state == "ALARM" || std::time::Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Stand a row up: the ECS service, the registered target, the policy both alarms execute and
/// the two alarms, answering the target's `ResourceId` and the name of the alarm that fires
/// second.
async fn given_netting_row(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    row: &NettingRow,
) -> (String, String) {
    let case = row.case;
    let resource_id = emulator
        .given_ecs_service(cluster, service, row.starting)
        .await;
    register(emulator, &resource_id, row.min, row.max, case).await;
    let policy_arn = put_step_policy(
        emulator,
        &resource_id,
        "step-policy",
        json!({
            "AdjustmentType": row.adjustment_type,
            "StepAdjustments": step_adjustments(row.steps),
            "Cooldown": LONG_COOLDOWN_SECS,
        }),
        case,
    )
    .await;
    let second_alarm = "second-alarm".to_string();
    given_breaching_alarm_on(
        emulator,
        "first-alarm",
        METRIC_NAME,
        &policy_arn,
        row.first_operator,
        case,
    )
    .await;
    given_breaching_alarm_on(
        emulator,
        &second_alarm,
        SECOND_METRIC_NAME,
        &policy_arn,
        row.second_operator,
        case,
    )
    .await;
    (resource_id, second_alarm)
}

#[tokio::test]
async fn an_action_inside_the_wait_applies_the_remainder_the_movement_in_flight_has_not_delivered()
{
    for row in NETTED_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let cluster = "netting";
        let service = "web";
        let (resource_id, _) = given_netting_row(&emulator, cluster, service, row).await;

        let first = await_first_action(&emulator, cluster, service, row, row.first_capacity).await;
        assert_eq!(
            first, row.first_capacity,
            "case {case:?}: expected the first breach to move the ECS desiredCount to {}, \
             got {first:?}",
            row.first_capacity
        );

        // The second alarm breaches while the wait for that movement is still running — the
        // policy asked for 600 seconds of it — so what the capacity does next is what the
        // action raised inside the wait did.
        let netted = await_action_inside_the_wait(
            &emulator,
            cluster,
            service,
            row,
            row.first_capacity,
            CONTINUING_WAIT,
        )
        .await;
        assert_eq!(
            netted, row.netted_capacity,
            "case {case:?}: expected the action raised inside the wait to move the ECS \
             desiredCount to {}, got {netted:?}",
            row.netted_capacity
        );

        // Two movements, in the order they happened: the wait suppressed neither, and the
        // second one landed on the netted capacity rather than on top of the first.
        let expected = vec![
            format!("Setting desired count to {}.", row.first_capacity),
            format!("Setting desired count to {}.", row.netted_capacity),
        ];
        let moved = movement_descriptions(&emulator, &resource_id, case).await;
        assert_eq!(
            moved, expected,
            "case {case:?}: expected the first movement and the netted one, got {moved:?}"
        );
    }
}

#[tokio::test]
async fn an_action_inside_the_wait_the_movement_in_flight_covers_moves_no_capacity() {
    for row in HELD_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let cluster = "held";
        let service = "web";
        let (resource_id, second_alarm) = given_netting_row(&emulator, cluster, service, row).await;

        let first = await_first_action(&emulator, cluster, service, row, row.first_capacity).await;
        assert_eq!(
            first, row.first_capacity,
            "case {case:?}: expected the first breach to move the ECS desiredCount to {}, \
             got {first:?}",
            row.first_capacity
        );

        // The second action really is raised: its alarm is in `ALARM`, which is what executes
        // the policy as one of its actions.
        let state = await_second_alarm_breaching(&emulator, &second_alarm, row).await;
        assert_eq!(
            state, "ALARM",
            "case {case:?}: expected the second alarm to be breaching, got {state:?}"
        );

        // And the capacity stays where the first movement left it for the whole watch, which
        // is several of the evaluation cadences of the alarm that raised it.
        let held = await_action_inside_the_wait(
            &emulator,
            cluster,
            service,
            row,
            row.first_capacity,
            QUIET_WATCH,
        )
        .await;
        assert_eq!(
            held, row.netted_capacity,
            "case {case:?}: expected the action raised inside the wait to leave the ECS \
             desiredCount at {}, got {held:?}",
            row.netted_capacity
        );
        let moved = movement_descriptions(&emulator, &resource_id, case).await;
        assert_eq!(
            moved,
            vec![format!("Setting desired count to {}.", row.first_capacity)],
            "case {case:?}: expected the one movement the wait has not elapsed since, \
             got {moved:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The movement a sustained breach performed settles at Successful
// ---------------------------------------------------------------------------
//
// A movement reaches its terminal status once the service that OWNS the scaled resource has
// made the capacity it set take effect — `Service$runningCount`, "the number of tasks in the
// cluster that are in the RUNNING state", reaching the `desiredCount` the movement asked for —
// and `ScalingActivity$StatusCode` is where the log says so: "Indicates the status of the
// scaling activity", one of `ScalingActivityStatusCode` (`Pending`, `InProgress`, `Successful`,
// `Overridden`, `Unfulfilled`, `Failed`).
//
// Which is only reachable because the breach's own alarm raises ONE action: a movement that a
// later movement of the same target displaces ends `Overridden` rather than `Successful` —
// the model keeps that value beside it for exactly that outcome — so a breach re-executed on
// every evaluation would leave a log in which no activity is ever successful, however long a
// client polls and however plainly the owning service reports the new count.

/// The status a movement the owning service has fulfilled reaches.
const SUCCESSFUL: &str = "Successful";

/// One sustained breach whose single movement has to settle: the window the target is
/// registered with, the capacity the ECS service starts at, the step adjustment the breach
/// fires, and the capacity the one movement lands on.
struct SettledRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    steps: &'static [Step],
    /// The alarm's `ComparisonOperator`, which names the side of the threshold it breaches on.
    comparison_operator: &'static str,
    /// How far past the threshold each published datapoint is, signed in the direction of the
    /// breach.
    breach: f64,
    /// The capacity the movement sets, which is the count ECS then has to be running.
    capacity: i64,
}

/// The rows differ in the DIRECTION the one movement is in, in how its adjustment is
/// interpreted (the three `AdjustmentType` values the model declares), and in whether the
/// registered window is what decides the capacity it lands on.
const SETTLED_CATALOG: &[SettledRow] = &[
    // A scale out by a task count: two more tasks for the cluster to place and run.
    SettledRow {
        case: "a scale out by a task count",
        starting: 2,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        capacity: 4,
    },
    // "For exact capacity, you must specify a non-negative value": the step names the count
    // itself.
    SettledRow {
        case: "a scale out to an exact capacity",
        starting: 2,
        min: 1,
        max: 20,
        adjustment_type: "ExactCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 5 }],
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        capacity: 5,
    },
    // A percentage of the capacity the execution finds: 100% of 2 is 2 more.
    SettledRow {
        case: "a scale out by a percentage",
        starting: 2,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 100 }],
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        capacity: 4,
    },
    // The other direction: a scale in, whose capacity the cluster reaches by STOPPING tasks.
    SettledRow {
        case: "a scale in by a task count",
        starting: 6,
        min: 1,
        max: 20,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: None, upper: Some(0.0), adjustment: -2 }],
        comparison_operator: "LessThanThreshold",
        breach: -25.0,
        capacity: 4,
    },
    // An adjustment that overshoots the registered ceiling is performed AT the ceiling, and
    // that capacity settles like any other.
    SettledRow {
        case: "a scale out clamped at the registered ceiling",
        starting: 2,
        min: 1,
        max: 5,
        adjustment_type: "ChangeInCapacity",
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 9 }],
        comparison_operator: "GreaterThanThreshold",
        breach: 25.0,
        capacity: 5,
    },
];

/// Keep the breach alive and wait for ECS to report `expected` tasks RUNNING for the service,
/// answering what it last read.
async fn await_while_breaching_running(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    breach: f64,
    expected: i64,
    case: &str,
) -> i64 {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach(emulator, breach, case).await;
        let last = emulator.ecs_running_count(cluster, service).await;
        if last == expected || std::time::Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep the breach alive and wait for the newest activity of the target to report
/// `StatusCode` `Successful`, answering the activity it last read.
async fn await_successful_activity(
    emulator: &Emulator,
    resource_id: &str,
    breach: f64,
    case: &str,
) -> Value {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach(emulator, breach, case).await;
        let newest = activities(emulator, resource_id, case)
            .await
            .first()
            .cloned()
            .unwrap_or(Value::Null);
        if newest["StatusCode"] == json!(SUCCESSFUL) || std::time::Instant::now() >= deadline {
            return newest;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

#[tokio::test]
async fn a_sustained_breach_settles_one_successful_activity() {
    for row in SETTLED_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let cluster = "settling";
        let service = "web";
        let resource_id = emulator
            .given_ecs_service(cluster, service, row.starting)
            .await;
        register(&emulator, &resource_id, row.min, row.max, case).await;
        let policy_arn = put_step_policy(
            &emulator,
            &resource_id,
            "step-policy",
            json!({
                "AdjustmentType": row.adjustment_type,
                "StepAdjustments": step_adjustments(row.steps),
                // No wait at all, so nothing but the one-action-per-transition rule can be what
                // keeps the breach from being applied again.
                "Cooldown": 0,
            }),
            case,
        )
        .await;
        let alarm_name = "settling-alarm";
        given_continuing_alarm(
            &emulator,
            alarm_name,
            &policy_arn,
            row.comparison_operator,
            case,
        )
        .await;

        // The breach moves the ECS service's desired count once, and the cluster then has to
        // make that count real.
        let desired = await_while_breaching(
            &emulator,
            cluster,
            service,
            row.breach,
            row.capacity,
            CONTINUING_WAIT,
            case,
        )
        .await;
        assert_eq!(
            desired, row.capacity,
            "case {case:?}: expected the breach to move the ECS desiredCount to {}, \
             got {desired:?}",
            row.capacity
        );
        let running = await_while_breaching_running(
            &emulator,
            cluster,
            service,
            row.breach,
            row.capacity,
            case,
        )
        .await;
        assert_eq!(
            running, row.capacity,
            "case {case:?}: expected ECS to report {} tasks running, got {running:?}",
            row.capacity
        );

        // So the movement that set that count settles terminal: `Successful`, with the
        // `EndTime` of the instant it was fulfilled at and the `Description` of the change it
        // made.
        let settled = await_successful_activity(&emulator, &resource_id, row.breach, case).await;
        assert_eq!(
            settled["StatusCode"],
            json!(SUCCESSFUL),
            "case {case:?}: expected the fulfilled movement to report {SUCCESSFUL}, \
             got {settled:?}"
        );
        assert_eq!(
            settled["Description"],
            json!(format!("Setting desired count to {}.", row.capacity)),
            "case {case:?}: expected the movement's own Description, got {settled:?}"
        );
        let start = settled["StartTime"].as_f64().unwrap_or_default();
        let end = settled["EndTime"].as_f64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a fulfilled movement to carry an EndTime, got {settled:?}"
            )
        });
        assert!(
            end >= start,
            "case {case:?}: expected EndTime {end} at or after StartTime {start}, got {settled:?}"
        );

        // And it STAYS successful while the metric goes on breaching: the alarm does not
        // transition again, so no later movement displaces this one and the log a client polls
        // really does hold a successful activity for the capacity that moved.
        let held = watch_while_breaching(
            &emulator,
            cluster,
            service,
            row.breach,
            row.capacity,
            QUIET_WATCH,
            case,
        )
        .await;
        assert_eq!(
            held, row.capacity,
            "case {case:?}: expected the capacity to hold at {} while the alarm stays in ALARM, \
             got {held:?}",
            row.capacity
        );
        let listed = activities(&emulator, &resource_id, case).await;
        let successful: Vec<&Value> = listed
            .iter()
            .filter(|activity| activity["StatusCode"] == json!(SUCCESSFUL))
            .collect();
        assert_eq!(
            successful.len(),
            1,
            "case {case:?}: expected exactly one successful activity, got {listed:?}"
        );
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the breach to record one activity in all, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// TWO policies of one target, one of them inside a long wait
// ---------------------------------------------------------------------------
//
// The rule these rows pin: the capacity a scalable target reaches is bounded by ONE adjustment
// applied to the capacity of the scaling decision in force — never the sum of the adjustments
// the triggered policies state. Two things together make that bound:
//
// * what executes a policy is the alarm CHANGING state — "The actions to execute when this
//   alarm transitions to the ALARM state from any other state"
//   (`PutMetricAlarmInput$AlarmActions`) — so a policy whose alarm merely STAYS in `ALARM`
//   while its metric goes on breaching has already performed the action that breach raised;
// * and for the span a policy asks to "wait for a previous scaling activity to take effect"
//   (`StepScalingPolicyConfiguration$Cooldown`) the movement that started the wait is still in
//   flight, so a further action of that policy applies only the remainder that movement has
//   not delivered — nothing at all for a repeat of the same step — and a policy of the same
//   target triggered inside the wait computes its own adjustment from the capacity that
//   decision was taken FROM rather than from the capacity it left behind, "Application Auto
//   Scaling [giving] precedence to the policy that provides the largest capacity".
//
// Each row drives the shape a client gets from a long wait: a `control` policy whose wait is
// far longer than the row runs, whose alarm is fed throughout and so never leaves `ALARM`
// after its one transition, beside a `subject` policy with no wait at all whose own metric is
// only published later — until then its alarm is in `INSUFFICIENT_DATA` and executes nothing.
// The rows differ in the two adjustments, in how each is interpreted, in the starting
// capacity, in the registered window the result is clamped into, in whether the subject policy
// asks for MORE capacity than the control's decision settled or for less, and in whether its
// alarm reaches the fed series by being REPOINTED at it — "When you update an existing alarm,
// its state is left unchanged", so a repaired alarm transitions once, out of the state the
// update left it in, exactly as one created on that series does.

/// The policy whose wait is still running when the other one is triggered.
const CONTROL_POLICY: &str = "control";
/// The alarm wired to it, on the suite's own metric, which every phase of a row keeps
/// breaching.
const CONTROL_ALARM: &str = "control-alarm";
/// The policy with no wait of its own, triggered while the control policy's wait runs.
const SUBJECT_POLICY: &str = "subject";
/// The alarm wired to it, on [`SECOND_METRIC_NAME`], which only the last phase of a row feeds.
const SUBJECT_ALARM: &str = "subject-alarm";

/// A metric inside [`METRIC_NAMESPACE`] that NOTHING ever publishes: the series a row's subject
/// alarm watches until that row repoints it at the fed one.
const UNFED_METRIC_NAME: &str = "LoadNeverPublished";

/// How long a row watches a target it expects to hold still, publishing the breach that keeps
/// the control alarm in `ALARM` throughout.
///
/// Several evaluation cadences of that alarm ([`CONTINUING_PERIOD_SECS`]) and many passes of
/// the scaling clock, which is what an execution no policy is owed would arrive on. Nothing in
/// the emulator sleeps for a movement, so a row that is owed one gets it inside the first few
/// of those passes.
const TWIN_WATCH: Duration = Duration::from_secs(12);

/// One pair of policies on one scalable target, one of them inside a long wait.
struct TwinRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with, which is the capacity the control
    /// policy's decision is taken from.
    starting: i64,
    min: i64,
    max: i64,
    /// The wait the control policy asks for, in seconds: far longer than the row runs, so
    /// every later action of that policy lands inside it.
    control_cooldown: i64,
    control_adjustment_type: &'static str,
    control_step: &'static [Step],
    /// The capacity the control alarm's one transition lands the target on.
    after_control: i64,
    subject_adjustment_type: &'static str,
    subject_step: &'static [Step],
    /// The capacity the target holds once the subject policy has been triggered too: its own
    /// adjustment computed from the capacity the control's decision was taken FROM, clamped
    /// into the registered window — and the capacity that decision settled, where the subject
    /// policy asks for no more than it.
    after_subject: i64,
    /// What each movement of the log set the capacity to, oldest first: one per execution that
    /// moved capacity, and no second one for the control policy.
    movements: &'static [i64],
    /// Capacities the resource must never be reported at — the ACCUMULATION of the two
    /// adjustments above all.
    never: &'static [i64],
    /// Whether the subject alarm reaches the fed series by being REPOINTED at it: created on a
    /// series nothing publishes ([`UNFED_METRIC_NAME`]) and written again, as a client repairs
    /// an alarm that was watching the wrong dimension, rather than created on the fed one.
    repoint_subject: bool,
    /// How far past the threshold each published datapoint is.
    breach: f64,
}

/// The rows whose subject policy asks for MORE capacity than the control's decision settled, so
/// it moves the target — by its own adjustment from that decision's base capacity, and never by
/// that plus the step the control policy has already delivered.
const TWIN_NETTED_CATALOG: &[TwinRow] = &[
    // The shape a client that sizes a long wait to bound how fast a service grows gets: one
    // task from the control policy, then two from the subject policy — computed from the one
    // task the control's decision started at, so three in all and never four or five.
    TwinRow {
        case: "a +2 policy triggered inside a +1 policy's hour-long wait",
        starting: 1,
        min: 1,
        max: 6,
        control_cooldown: 3_600,
        control_adjustment_type: "ChangeInCapacity",
        control_step: &[Step { lower: Some(0.0), upper: None, adjustment: 1 }],
        after_control: 2,
        subject_adjustment_type: "ChangeInCapacity",
        subject_step: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        after_subject: 3,
        movements: &[2, 3],
        never: &[4, 5, 6],
        repoint_subject: true,
        breach: 25.0,
    },
    // Larger steps and a wider window: +2 then +3, which is five tasks from the two the
    // decision started at and never the seven the two adjustments add up to.
    TwinRow {
        case: "a +3 policy triggered inside a +2 policy's half-hour wait",
        starting: 2,
        min: 1,
        max: 10,
        control_cooldown: 1_800,
        control_adjustment_type: "ChangeInCapacity",
        control_step: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        after_control: 4,
        subject_adjustment_type: "ChangeInCapacity",
        subject_step: &[Step { lower: Some(0.0), upper: None, adjustment: 3 }],
        after_subject: 5,
        movements: &[4, 5],
        never: &[6, 7, 9],
        repoint_subject: false,
        breach: 25.0,
    },
];

/// The rows where the registered window or the decision already in force is what bounds the
/// subject policy: an `ExactCapacity` ask past the ceiling is performed AT the ceiling, and an
/// ask that provides no more capacity than the control's decision settled is suppressed
/// entirely.
const TWIN_BOUNDED_CATALOG: &[TwinRow] = &[
    TwinRow {
        case: "an ExactCapacity policy asking past the ceiling inside the other's wait",
        starting: 2,
        min: 1,
        max: 5,
        control_cooldown: 3_600,
        control_adjustment_type: "ChangeInCapacity",
        control_step: &[Step { lower: Some(0.0), upper: None, adjustment: 1 }],
        after_control: 3,
        subject_adjustment_type: "ExactCapacity",
        subject_step: &[Step { lower: Some(0.0), upper: None, adjustment: 9 }],
        after_subject: 5,
        movements: &[3, 5],
        never: &[4, 6, 9],
        repoint_subject: true,
        breach: 25.0,
    },
    TwinRow {
        case: "a +1 policy triggered inside a +4 policy's wait is suppressed entirely",
        starting: 3,
        min: 1,
        max: 12,
        control_cooldown: 3_600,
        control_adjustment_type: "ChangeInCapacity",
        control_step: &[Step { lower: Some(0.0), upper: None, adjustment: 4 }],
        after_control: 7,
        subject_adjustment_type: "ChangeInCapacity",
        subject_step: &[Step { lower: Some(0.0), upper: None, adjustment: 1 }],
        after_subject: 7,
        movements: &[7],
        never: &[4, 8, 11, 12],
        repoint_subject: false,
        breach: 25.0,
    },
];

/// The series a row's subject alarm watches before it is triggered: the fed one, or the one
/// nothing publishes for a row that REPOINTS the alarm later.
fn subject_metric_before(row: &TwinRow) -> &'static str {
    if row.repoint_subject {
        UNFED_METRIC_NAME
    } else {
        SECOND_METRIC_NAME
    }
}

/// Publish the breach each phase of a row feeds: always the control alarm's metric, and the
/// subject alarm's too once the row has started feeding it.
async fn feed_twin(emulator: &Emulator, row: &TwinRow, feed_subject: bool, case: &str) {
    publish_breach_on(emulator, METRIC_NAME, row.breach, case).await;
    if feed_subject {
        publish_breach_on(emulator, SECOND_METRIC_NAME, row.breach, case).await;
    }
}

/// Keep the breaches alive until the ECS service's `desiredCount` reaches `wanted`, answering
/// every DISTINCT capacity it was reported at in the order they were read.
///
/// The poll is the TEST waiting for the clock that executes a breach, so a row whose capacity
/// never moves fails on the readings it collected rather than on how fast the host ran.
async fn drive_twin_until(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    row: &TwinRow,
    feed_subject: bool,
    wanted: i64,
    case: &str,
) -> Vec<i64> {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    let mut readings: Vec<i64> = Vec::new();
    loop {
        feed_twin(emulator, row, feed_subject, case).await;
        let count = emulator.ecs_desired_count(cluster, service).await;
        if readings.last() != Some(&count) {
            readings.push(count);
        }
        if count == wanted || std::time::Instant::now() >= deadline {
            return readings;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep the breaches alive for the whole of [`TWIN_WATCH`], answering every distinct capacity
/// the ECS service was reported at: this is what a row that expects NO further movement fails
/// on.
async fn watch_twin(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    row: &TwinRow,
    feed_subject: bool,
    case: &str,
) -> Vec<i64> {
    let deadline = std::time::Instant::now() + TWIN_WATCH;
    let mut readings: Vec<i64> = Vec::new();
    loop {
        feed_twin(emulator, row, feed_subject, case).await;
        let count = emulator.ecs_desired_count(cluster, service).await;
        if readings.last() != Some(&count) {
            readings.push(count);
        }
        if std::time::Instant::now() >= deadline {
            return readings;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Keep both breaches alive until the subject alarm reports `ALARM`, which is the instant its
/// policy's one action was raised at.
///
/// A row whose subject policy is SUPPRESSED expects no capacity movement from it at all, so the
/// trigger itself is what has to be waited for: without this the row would read the capacity it
/// already held and conclude before the alarm had graded anything.
async fn await_subject_alarm(emulator: &Emulator, row: &TwinRow, case: &str) {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        feed_twin(emulator, row, true, case).await;
        let state = alarm_state(emulator, SUBJECT_ALARM, case).await;
        if state == "ALARM" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the published datapoints to put the subject alarm in \
             ALARM, it reports {state:?}"
        );
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// Let the control alarm's breach UP and drive it back, with BOTH series still breaching while
/// it returns, so that alarm transitions into `ALARM` afresh — the state change that raises its
/// policy's actions again, this time inside the wait its own first movement started.
///
/// The subject alarm's series is published throughout: a gap in it would let that alarm lapse
/// and come back too, which is an action of ITS own rather than the one this phase is about.
async fn given_fresh_control_transition(emulator: &Emulator, row: &TwinRow, case: &str) {
    emulator.set_alarm_state(CONTROL_ALARM, "OK").await;
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        feed_twin(emulator, row, true, case).await;
        let state = alarm_state(emulator, CONTROL_ALARM, case).await;
        if state == "ALARM" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the still-breaching datapoints to put the control alarm \
             back in ALARM, it reports {state:?}"
        );
        tokio::time::sleep(CONTINUING_POLL).await;
    }
}

/// One report per capacity of `forbidden` that `readings` holds.
fn forbidden_readings(readings: &[i64], forbidden: &[i64]) -> Vec<i64> {
    forbidden
        .iter()
        .copied()
        .filter(|capacity| readings.contains(capacity))
        .collect()
}

/// Drive one twin row end to end: the target, both policies, both alarms, the control breach
/// alone, and then the subject breach beside it — holding the row's assertions at each stage.
async fn drive_twin(row: &TwinRow) {
    let case = row.case;
    let emulator = Emulator::empty();
    let cluster = "twinned";
    let service = "web";
    let resource_id = emulator
        .given_ecs_service(cluster, service, row.starting)
        .await;
    register(&emulator, &resource_id, row.min, row.max, case).await;
    let control_arn = put_step_policy(
        &emulator,
        &resource_id,
        CONTROL_POLICY,
        json!({
            "AdjustmentType": row.control_adjustment_type,
            "StepAdjustments": step_adjustments(row.control_step),
            "Cooldown": row.control_cooldown,
        }),
        case,
    )
    .await;
    let subject_arn = put_step_policy(
        &emulator,
        &resource_id,
        SUBJECT_POLICY,
        json!({
            "AdjustmentType": row.subject_adjustment_type,
            "StepAdjustments": step_adjustments(row.subject_step),
            // No wait of its own, so nothing this policy asks for is held back by one: the
            // bound on what it can reach is the decision the control policy settled.
            "Cooldown": 0,
        }),
        case,
    )
    .await;
    given_breaching_alarm_on(
        &emulator,
        CONTROL_ALARM,
        METRIC_NAME,
        &control_arn,
        "GreaterThanThreshold",
        case,
    )
    .await;
    given_breaching_alarm_on(
        &emulator,
        SUBJECT_ALARM,
        subject_metric_before(row),
        &subject_arn,
        "GreaterThanThreshold",
        case,
    )
    .await;

    // The control alarm's transition into `ALARM` is the only action raised so far, so the
    // capacity moves by the control policy's own step and by nothing else.
    let readings = drive_twin_until(
        &emulator,
        cluster,
        service,
        row,
        false,
        row.after_control,
        case,
    )
    .await;
    let reached = readings.last().copied().unwrap_or_default();
    assert_eq!(
        reached, row.after_control,
        "case {case:?}: expected the control alarm's transition to drive the ECS desiredCount \
         to {}, got {reached:?} after reading {readings:?}",
        row.after_control
    );

    // And it HOLDS there. The metric goes on breaching and the alarm goes on being graded, but
    // it does not change state again, so there is no further action of that policy to execute —
    // and its own wait is running throughout anyway. The subject alarm, whose metric nothing
    // has published, has no datapoint to be graded on at all.
    let watched = watch_twin(&emulator, cluster, service, row, false, case).await;
    let held = watched.last().copied().unwrap_or_default();
    assert_eq!(
        held, row.after_control,
        "case {case:?}: expected the capacity to hold at {} while the control alarm merely \
         stays in ALARM, got {held:?} after reading {watched:?}",
        row.after_control
    );
    let state = alarm_state(&emulator, CONTROL_ALARM, case).await;
    assert_eq!(
        state, "ALARM",
        "case {case:?}: expected the control alarm to have been breaching throughout the \
         watch, got {state:?}"
    );
    let state = alarm_state(&emulator, SUBJECT_ALARM, case).await;
    assert_eq!(
        state, "INSUFFICIENT_DATA",
        "case {case:?}: expected the never-fed subject alarm to report INSUFFICIENT_DATA, got \
         {state:?}"
    );

    // Now the subject alarm's own metric breaches too, so its transition raises its policy's
    // one action — decided against the scaling decision the control policy settled rather than
    // against the capacity that decision left behind. A row that REPOINTS the alarm writes it
    // again onto the fed series first: "When you update an existing alarm, its state is left
    // unchanged", so the repaired alarm is still the one that has graded nothing.
    if row.repoint_subject {
        given_breaching_alarm_on(
            &emulator,
            SUBJECT_ALARM,
            SECOND_METRIC_NAME,
            &subject_arn,
            "GreaterThanThreshold",
            case,
        )
        .await;
        let state = alarm_state(&emulator, SUBJECT_ALARM, case).await;
        assert_eq!(
            state, "INSUFFICIENT_DATA",
            "case {case:?}: expected the repointed subject alarm to keep the state the update \
             left unchanged, got {state:?}"
        );
    }
    await_subject_alarm(&emulator, row, case).await;
    let readings = drive_twin_until(
        &emulator,
        cluster,
        service,
        row,
        true,
        row.after_subject,
        case,
    )
    .await;
    let reached = readings.last().copied().unwrap_or_default();
    assert_eq!(
        reached, row.after_subject,
        "case {case:?}: expected the subject policy's action to leave the ECS desiredCount at \
         {}, got {reached:?} after reading {readings:?}",
        row.after_subject
    );
    let forbidden = forbidden_readings(&readings, row.never);
    assert!(
        forbidden.is_empty(),
        "case {case:?}: expected the desiredCount never to be reported at any of {:?}, got \
         {forbidden:?} among {readings:?}",
        row.never
    );

    // Both alarms stay in `ALARM` from here, and neither policy has an action left to perform:
    // the capacity holds at the bound one adjustment from the decision's base capacity put it
    // at, rather than creeping up once per evaluation.
    let watched = watch_twin(&emulator, cluster, service, row, true, case).await;
    let settled = watched.last().copied().unwrap_or_default();
    assert_eq!(
        settled, row.after_subject,
        "case {case:?}: expected the capacity to hold at {} while both alarms merely stay in \
         ALARM, got {settled:?} after reading {watched:?}",
        row.after_subject
    );
    let forbidden = forbidden_readings(&watched, row.never);
    assert!(
        forbidden.is_empty(),
        "case {case:?}: expected the settled desiredCount never to be reported at any of {:?}, \
         got {forbidden:?} among {watched:?}",
        row.never
    );
    let state = alarm_state(&emulator, CONTROL_ALARM, case).await;
    assert_eq!(
        state, "ALARM",
        "case {case:?}: expected the control alarm never to have left ALARM, got {state:?}"
    );
    let state = alarm_state(&emulator, SUBJECT_ALARM, case).await;
    assert_eq!(
        state, "ALARM",
        "case {case:?}: expected the fed subject alarm to have stayed in ALARM, got {state:?}"
    );

    // And a FRESH transition of the control alarm adds nothing either, because it lands inside
    // the wait that policy asked for: the movement that wait is a wait FOR is what a repeat of
    // the same step is netted against, and it has no remainder left to deliver.
    given_fresh_control_transition(&emulator, row, case).await;
    let watched = watch_twin(&emulator, cluster, service, row, true, case).await;
    let settled = watched.last().copied().unwrap_or_default();
    assert_eq!(
        settled, row.after_subject,
        "case {case:?}: expected a fresh transition inside the control policy's wait to leave \
         the capacity at {}, got {settled:?} after reading {watched:?}",
        row.after_subject
    );
    let forbidden = forbidden_readings(&watched, row.never);
    assert!(
        forbidden.is_empty(),
        "case {case:?}: expected the capacity never to be reported at any of {:?} after the \
         fresh transition, got {forbidden:?} among {watched:?}",
        row.never
    );

    // And the log says the same thing the capacity does: one movement per execution that moved
    // capacity, in the order they happened, with exactly one of them attributable to the
    // control policy — whose alarm transitioned once.
    let expected: Vec<String> = row
        .movements
        .iter()
        .map(|capacity| format!("Setting desired count to {capacity}."))
        .collect();
    let moved = movement_descriptions(&emulator, &resource_id, case).await;
    assert_eq!(
        moved, expected,
        "case {case:?}: expected one activity per execution that moved capacity, got {moved:?}"
    );
    let listed = activities(&emulator, &resource_id, case).await;
    let by_control: Vec<String> = listed
        .iter()
        .filter_map(|activity| activity["Cause"].as_str())
        .filter(|cause| cause.ends_with(&format!("triggered policy {CONTROL_POLICY}")))
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        by_control.len(),
        1,
        "case {case:?}: expected exactly one activity caused by the control policy's one \
         transition, got {by_control:?}"
    );
}

#[tokio::test]
async fn a_policy_triggered_inside_another_policys_wait_adds_one_adjustment_and_no_more() {
    for row in TWIN_NETTED_CATALOG {
        drive_twin(row).await;
    }
}

#[tokio::test]
async fn a_policy_triggered_inside_another_policys_wait_is_bounded_by_the_window_in_force() {
    for row in TWIN_BOUNDED_CATALOG {
        drive_twin(row).await;
    }
}

/// How many actions a row of [`PER_ACTION_CATALOG`] raises on its one policy: the alarm leaves
/// `ALARM` and returns that many times, so the policy is handed that many actions to perform.
///
/// More than two, because what this pins is an INEQUALITY that has to hold at every action
/// rather than at the first one: a pass that applied a step twice, or that replayed an earlier
/// action beside the current one, would land the capacity past the single step the action states
/// at whichever action it happened on.
const ACTIONS_PER_ROW: usize = 3;

/// One SUCCESSION of alarm actions on one step scaling policy, each raised outside any wait of
/// that policy's own, and the capacity the target holds after each of them.
///
/// The rows differ along what the rule generalises over: the `AdjustmentType` the step is
/// interpreted by, the direction it moves in, the floor on a percentage's magnitude, how many
/// steps the configuration carries, and whether the registered ceiling is what the action lands
/// against.
struct PerActionRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with, which is the capacity the FIRST
    /// action's adjustment is computed from.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    min_adjustment_magnitude: Option<i64>,
    steps: &'static [Step],
    /// The alarm's `ComparisonOperator`, which names the side of the threshold the breach is on.
    comparison_operator: &'static str,
    /// The capacity the target must hold after each successive action, in order: the ONE
    /// adjustment whose interval that action's breach falls in, applied to the capacity the
    /// target held when the action was raised and clamped into the registered window.
    after_each_action: &'static [i64; ACTIONS_PER_ROW],
    /// Capacities the resource must never be reported at: what a step applied more than once per
    /// action, or the step the breach does NOT fall in, would reach.
    never: &'static [i64],
}

/// The successions to pin. Every row's policy names `Cooldown` 0, so no action of it is inside a
/// wait of its own and each one is decided from the capacity the target holds — which is what
/// makes the capacity after `n` actions the sum of `n` single adjustments and never of more.
const PER_ACTION_CATALOG: &[PerActionRow] = &[
    // The shape a client that sizes a step to bound how fast a service grows gets: two tasks per
    // action and never four, however many actions follow.
    PerActionRow {
        case: "a +2 ChangeInCapacity policy adds two tasks per action",
        starting: 1,
        min: 1,
        max: 12,
        adjustment_type: "ChangeInCapacity",
        min_adjustment_magnitude: None,
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 2 }],
        comparison_operator: "GreaterThanThreshold",
        after_each_action: &[3, 5, 7],
        // 9 and 11 are a fourth and a fifth application of the same step.
        never: &[9, 11],
    },
    // The mirror of it, in the direction a breach BELOW the threshold names: one task removed per
    // action, and never two.
    PerActionRow {
        case: "a -1 ChangeInCapacity policy removes one task per action",
        starting: 5,
        min: 1,
        max: 10,
        adjustment_type: "ChangeInCapacity",
        min_adjustment_magnitude: None,
        steps: &[Step { lower: None, upper: Some(0.0), adjustment: -1 }],
        comparison_operator: "LessThanThreshold",
        after_each_action: &[4, 3, 2],
        // 1 is a fourth removal, which is also the registered floor: the three actions of this
        // row may not reach it.
        never: &[1],
    },
    // A percentage, with the documented floor on the units it comes to
    // (`StepScalingPolicyConfiguration$MinAdjustmentMagnitude`): 50% of 3 is 1.5 units, taken
    // toward zero to 1 and raised to the floor of 2; of 5 it is 2.5, taken to 2, already at the
    // floor; of 7 it is 3.5, taken to 3.
    PerActionRow {
        case: "a PercentChangeInCapacity policy applies its magnitude once per action",
        starting: 3,
        min: 1,
        max: 20,
        adjustment_type: "PercentChangeInCapacity",
        min_adjustment_magnitude: Some(2),
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 50 }],
        comparison_operator: "GreaterThanThreshold",
        after_each_action: &[5, 7, 10],
        // 12 and 15 are a second application of the same percentage inside one action.
        never: &[12, 15],
    },
    // Several steps: the action applies the ONE whose interval the breach falls in. A forced
    // alarm state carries no datapoint, so the breach is the smallest one in its direction — the
    // threshold itself — and the step at the threshold is the one every action of this row
    // performs, never the one a breach ten past it would fall in.
    PerActionRow {
        case: "a two-step policy applies only the step the breach falls in",
        starting: 1,
        min: 1,
        max: 30,
        adjustment_type: "ChangeInCapacity",
        min_adjustment_magnitude: None,
        steps: &[
            Step { lower: Some(0.0), upper: Some(10.0), adjustment: 2 },
            Step { lower: Some(10.0), upper: None, adjustment: 9 },
        ],
        comparison_operator: "GreaterThanThreshold",
        after_each_action: &[3, 5, 7],
        // 10, 12 and 14 are what the other step would reach, and 16 is both steps together.
        never: &[10, 12, 14, 16],
    },
    // The registered ceiling is what bounds the succession: the second action's step overshoots
    // it and is performed AT it, and the third asks past a bound the target already sits at, so
    // nothing moves at all.
    PerActionRow {
        case: "a +5 policy is performed at the ceiling and then holds there",
        starting: 2,
        min: 1,
        max: 8,
        adjustment_type: "ChangeInCapacity",
        min_adjustment_magnitude: None,
        steps: &[Step { lower: Some(0.0), upper: None, adjustment: 5 }],
        comparison_operator: "GreaterThanThreshold",
        after_each_action: &[7, 8, 8],
        // 12 and 13 are the unclamped sums, and 9 is anything past the registered ceiling.
        never: &[9, 12, 13],
    },
];

/// Every DISTINCT `desiredCount` the ECS service was reported at on its way to `expected`, in the
/// order they were read, stopping as soon as it is reported there.
///
/// The poll is the TEST waiting for the clock that executes a breach, so a row whose capacity
/// overshoots the step it was owed fails on the readings it collected rather than on how fast the
/// host ran.
async fn readings_until(
    emulator: &Emulator,
    cluster: &str,
    service: &str,
    expected: i64,
) -> Vec<i64> {
    let deadline = std::time::Instant::now() + CLOCK_WAIT;
    let mut readings: Vec<i64> = Vec::new();
    loop {
        let count = emulator.ecs_desired_count(cluster, service).await;
        if readings.last() != Some(&count) {
            readings.push(count);
        }
        if count == expected || std::time::Instant::now() >= deadline {
            return readings;
        }
        tokio::time::sleep(CLOCK_POLL).await;
    }
}

/// Every distinct `desiredCount` the service was reported at over the WHOLE wait: what an action
/// that must move no capacity is held to, since a movement it was not owed would arrive on one of
/// the clock's wake-ups inside it.
async fn readings_while_held(emulator: &Emulator, cluster: &str, service: &str) -> Vec<i64> {
    let deadline = std::time::Instant::now() + CLOCK_WAIT;
    let mut readings: Vec<i64> = Vec::new();
    loop {
        let count = emulator.ecs_desired_count(cluster, service).await;
        if readings.last() != Some(&count) {
            readings.push(count);
        }
        if std::time::Instant::now() >= deadline {
            return readings;
        }
        tokio::time::sleep(CLOCK_POLL).await;
    }
}

/// Raise ONE alarm action: the alarm LEAVES `ALARM` and returns to it, which is the state change
/// that executes the actions it names — "The actions to execute when this alarm transitions to
/// the ALARM state from any other state" (`PutMetricAlarmInput$AlarmActions`).
///
/// The first action of a row is the alarm's first entry into `ALARM`, so there is nothing to
/// leave yet.
async fn raise_action(emulator: &Emulator, alarm_name: &str, index: usize) {
    if index > 0 {
        emulator.set_alarm_state(alarm_name, "OK").await;
    }
    emulator.set_alarm_state(alarm_name, "ALARM").await;
}

/// Drive one succession end to end: the target, the policy, the alarm, and then one action after
/// another — holding, at EVERY action, that the capacity moved by the single adjustment that
/// action's breach falls in and by no more.
async fn drive_per_action(row: &PerActionRow) {
    let case = row.case;
    let emulator = Emulator::empty();
    let cluster = "per-action";
    let service = "web";
    let resource_id = emulator
        .given_ecs_service(cluster, service, row.starting)
        .await;
    register(&emulator, &resource_id, row.min, row.max, case).await;
    let mut configuration = json!({
        "AdjustmentType": row.adjustment_type,
        "StepAdjustments": step_adjustments(row.steps),
        // No wait of this policy's own, so every action of it is decided from the capacity the
        // target holds rather than netted against a movement still in flight: what bounds the
        // succession is the one adjustment each action states.
        "Cooldown": 0,
    });
    if let Some(magnitude) = row.min_adjustment_magnitude {
        configuration["MinAdjustmentMagnitude"] = json!(magnitude);
    }
    let policy_arn =
        put_step_policy(&emulator, &resource_id, "step-policy", configuration, case).await;
    let alarm_name = "per-action-alarm";
    emulator
        .given_policy_alarm(&PolicyAlarm {
            name: alarm_name,
            policy_arn: &policy_arn,
            metric_namespace: METRIC_NAMESPACE,
            metric_name: METRIC_NAME,
            threshold: THRESHOLD,
            comparison_operator: row.comparison_operator,
        })
        .await;

    let mut held = row.starting;
    let mut movements: Vec<i64> = Vec::new();
    for (index, expected) in row.after_each_action.iter().copied().enumerate() {
        raise_action(&emulator, alarm_name, index).await;
        // An action that asks for the capacity the target already holds — or past a bound it
        // already sits at — is watched for the whole wait instead, because what it must do is
        // nothing.
        let readings = if expected == held {
            readings_while_held(&emulator, cluster, service).await
        } else {
            readings_until(&emulator, cluster, service, expected).await
        };
        let reached = readings.last().copied().unwrap_or(held);
        assert_eq!(
            reached, expected,
            "case {case:?}: expected action {} of {ACTIONS_PER_ROW} to leave the ECS \
             desiredCount at {expected}, got {reached:?} after reading {readings:?}",
            index + 1
        );
        // Nothing on the way past the single adjustment this action states: every capacity the
        // resource was reported at lies between the one it held when the action was raised and
        // the one that adjustment asks for.
        let (low, high) = (held.min(expected), held.max(expected));
        let outside: Vec<i64> = readings
            .iter()
            .copied()
            .filter(|count| *count < low || *count > high)
            .collect();
        assert!(
            outside.is_empty(),
            "case {case:?}: expected action {} of {ACTIONS_PER_ROW} to move the desiredCount no \
             further than {high} from {held}, got {outside:?} among {readings:?}",
            index + 1
        );
        let forbidden = forbidden_readings(&readings, row.never);
        assert!(
            forbidden.is_empty(),
            "case {case:?}: expected the desiredCount never to be reported at any of {:?}, got \
             {forbidden:?} among {readings:?}",
            row.never
        );
        if expected != held {
            movements.push(expected);
        }
        held = expected;
    }

    // And the activity log says exactly what the capacity did: one movement per action that moved
    // capacity, in the order they happened, and none for the action that was owed nothing.
    let expected_log: Vec<String> = movements
        .iter()
        .map(|capacity| format!("Setting desired count to {capacity}."))
        .collect();
    let moved = movement_descriptions(&emulator, &resource_id, case).await;
    assert_eq!(
        moved, expected_log,
        "case {case:?}: expected one activity per action that moved capacity, got {moved:?}"
    );
}

#[tokio::test]
async fn each_alarm_action_applies_exactly_one_step_adjustment() {
    for row in PER_ACTION_CATALOG {
        drive_per_action(row).await;
    }
}
