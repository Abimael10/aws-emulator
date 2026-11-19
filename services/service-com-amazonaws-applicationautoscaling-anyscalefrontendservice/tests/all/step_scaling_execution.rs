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
//! And a breach does not stop at one execution. An alarm that STAYS in `ALARM` keeps invoking
//! the actions it names, at the cadence it is evaluated at ("Valid values are 10, 20, 30, and
//! any multiple of 60", `PutMetricAlarmInput$Period`), so a step scaling policy is re-invoked
//! for as long as the breach lasts: the invocations that fall inside its `Cooldown` add
//! nothing, the first one after that wait has elapsed applies the next step adjustment, and the
//! capacity goes on advancing one step per wait until the registered window is what stops it
//! ([`a_continuing_breach_applies_the_next_step_after_every_cooldown`]).
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
/// any multiple of 60" (`PutMetricAlarmInput$Period`) — so the invocations an alarm that stays
/// in `ALARM` makes are this far apart, and a row can watch several of them.
const CONTINUING_PERIOD_SECS: i32 = 10;

/// How long a row that keeps a breach alive waits for the capacity it expects.
///
/// Generous: many invocation cadences, so a row fails on a capacity that never moves again
/// rather than on a host that was busy for one of them. Nothing in the emulator sleeps for a
/// movement, so a row that is owed one gets it as soon as the clock takes its next pass.
const CONTINUING_WAIT: Duration = Duration::from_secs(120);

/// How often a row that keeps a breach alive looks again, and publishes the datapoint that
/// keeps the alarm breaching.
const CONTINUING_POLL: Duration = Duration::from_millis(500);

/// How long a row that expects NO further movement keeps watching: several invocation
/// cadences, which is more than a row whose wait has elapsed takes two of its steps in.
const QUIET_WATCH: Duration = Duration::from_secs(25);

/// One continuing breach to pin: the window the target is registered with, the capacity the
/// ECS service starts at, the policy the still-breaching alarm keeps invoking, and the
/// capacities those invocations have to drive it through.
struct ContinuingRow {
    case: &'static str,
    /// The `desiredCount` the ECS service is created with.
    starting: i64,
    min: i64,
    max: i64,
    adjustment_type: &'static str,
    steps: &'static [Step],
    /// The policy's `Cooldown`, in seconds: the wait every invocation after a movement is held
    /// for.
    cooldown: i64,
    /// The alarm's `ComparisonOperator`, which names the side of the threshold it breaches on.
    comparison_operator: &'static str,
    /// How far past the threshold each published datapoint is, signed in the direction of the
    /// breach.
    breach: f64,
    /// The `NotScaledReason$Code` the invocation after the climb is withheld with: the bound
    /// of the registered window the capacity has reached.
    reason_code: &'static str,
    /// The capacity each successive execution lands the target on, in order: the first step,
    /// and then the step every invocation after the cooldown adds — the last of them being the
    /// bound the registered window stops the policy at.
    capacities: &'static [i64],
}

/// The rows differ in how the adjustment is interpreted, in its magnitude and direction, in
/// the wait between two invocations that move capacity, and in which bound of the registered
/// window ends the climb.
const CONTINUING_CATALOG: &[ContinuingRow] = &[
    // One task per invocation, with no wait at all between them, to the ceiling.
    ContinuingRow {
        case: "one task per invocation to the ceiling",
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
    // Two tasks per invocation, with a wait LONGER than the cadence the alarm invokes at: the
    // invocations inside it add nothing, and the first one after it applies the next step.
    ContinuingRow {
        case: "two tasks per invocation, one wait apart",
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
    // The mirror of the first row: a breach BELOW the threshold keeps removing tasks, down to
    // the floor.
    ContinuingRow {
        case: "three tasks per invocation down to the floor",
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
    // A percentage is a percentage of the capacity each invocation FINDS, so the steps grow:
    // 50% of 4 is 2, and 50% of the 6 the next invocation finds is 3.
    ContinuingRow {
        case: "a percentage of the capacity each invocation finds",
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
/// evaluated on [`CONTINUING_PERIOD_SECS`], so the invocations it makes while it stays in
/// `ALARM` are that far apart.
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
/// what the row waits on is the policy being invoked again rather than the alarm moving.
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

/// Keep the breach alive and wait for the log to report a decision WITHHELD with `code`,
/// answering the reason that decision carries.
///
/// A decision that moved no capacity is reported only in the view that asks for it
/// (`DescribeScalingActivitiesRequest$IncludeNotScaledActivities`), so that is the view polled
/// here; `Value::Null` is what a wait that never saw one answers, which fails the row on the
/// reason it was looking for.
async fn await_withheld_reason(
    emulator: &Emulator,
    resource_id: &str,
    breach: f64,
    code: &str,
    case: &str,
) -> Value {
    let deadline = std::time::Instant::now() + CONTINUING_WAIT;
    loop {
        publish_breach(emulator, breach, case).await;
        let found = not_scaled_reasons(emulator, resource_id, case)
            .await
            .into_iter()
            .find(|reason| reason["Code"] == json!(code));
        if let Some(found) = found {
            return found;
        }
        if std::time::Instant::now() >= deadline {
            return Value::Null;
        }
        tokio::time::sleep(CONTINUING_POLL).await;
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
/// this is the log of what the successive invocations really did to the resource.
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
async fn a_continuing_breach_applies_the_next_step_after_every_cooldown() {
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

        // The alarm goes on reporting the same breach, so the policy is invoked again and
        // again: each capacity the row names is reached in turn, however many invocations the
        // wait between two of them suppresses.
        for expected in row.capacities {
            let reached = await_while_breaching(
                &emulator,
                cluster,
                service,
                row.breach,
                *expected,
                CONTINUING_WAIT,
                case,
            )
            .await;
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected the still-breaching alarm to drive the ECS \
                 desiredCount to {expected}, got {reached:?}"
            );
        }

        // The last capacity is the bound the registered window stops the policy at, and the
        // invocations keep coming: the next one is DECIDED and withheld, which is what the
        // reason the model names says about it — "Application Auto Scaling will not scale
        // capacity to values that are outside of the minimum and maximum range".
        let withheld =
            await_withheld_reason(&emulator, &resource_id, row.breach, row.reason_code, case).await;
        assert_eq!(
            withheld["Code"].as_str().unwrap_or_default(),
            row.reason_code,
            "case {case:?}: expected a later invocation to be withheld as {}, got {withheld:?}",
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

        // And each of those executions is one movement of the log, in the order they happened.
        let expected: Vec<String> = row
            .capacities
            .iter()
            .map(|capacity| format!("Setting desired count to {capacity}."))
            .collect();
        let moved = movement_descriptions(&emulator, &resource_id, case).await;
        assert_eq!(
            moved, expected,
            "case {case:?}: expected one scaling activity per step, got {moved:?}"
        );
    }
}

#[tokio::test]
async fn invocations_inside_the_cooldown_add_nothing() {
    // The same continuing breach with a wait far longer than the window this row watches: the
    // invocations that keep arriving inside it are suppressed, so the capacity the first step
    // set is the one the target holds for the whole of it.
    let case = "a continuing breach inside the cooldown";
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
        "case {case:?}: expected the first invocation to add one task, got {first:?}"
    );

    let settled =
        await_while_breaching(&emulator, cluster, service, 25.0, 3, QUIET_WATCH, case).await;
    assert_eq!(
        settled, 2,
        "case {case:?}: expected every invocation inside the wait to add nothing, got {settled:?}"
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
        let (status, body) = emulator
            .call_cloudwatch("DescribeAlarms", json!({"AlarmNames": [alarm_name]}))
            .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected DescribeAlarms to answer 200, got {status} {body:?}",
            row.case
        );
        let state = body["MetricAlarms"][0]["StateValue"]
            .as_str()
            .unwrap_or_default()
            .to_string();
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
        // is several of the invocation cadences that alarm goes on invoking the policy at.
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
