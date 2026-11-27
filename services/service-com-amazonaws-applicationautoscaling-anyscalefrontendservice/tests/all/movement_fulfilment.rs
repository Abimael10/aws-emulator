// SPDX-License-Identifier: CC0-1.0

//! When a capacity movement's scaling activity reaches its TERMINAL status, end to end over
//! the wire.
//!
//! The rule under test: setting a capacity and that capacity being applied are two different
//! things. This service decides a capacity and sets it on the resource — which is what the
//! activity's `Description` states ("Setting desired count to 3.") — and the service that owns
//! that resource then has to take it on, which is what the same activity's `StatusMessage`
//! says it is still waiting for ("Successfully set desired count to 3. Waiting for change to be
//! fulfilled by ecs."). So a movement reports `InProgress` when it is made, and `Successful` —
//! with an `EndTime` and the settled message ("Change successfully fulfilled by ecs.") — once
//! the owning service has that capacity in effect on the scaled property.
//!
//! What is fulfilled is the CHANGE, not the work the changed capacity implies. The capacity a
//! movement sets is a property OF the resource — `ecs:service:DesiredCount` is "The task count
//! of an ECS service", which is `Service$desiredCount` — so the owning service has fulfilled
//! the change once it HOLDS that count. What its scheduler then does to honour the count is its
//! own work: `Service$runningCount`, "the number of tasks in the cluster that are in the
//! RUNNING state", catches the desired count up on that service's clock, and on a cluster with
//! nowhere to place a task it never does. So the rows run on BOTH estates — a cluster with a
//! container instance and a cluster with none — and the movement settles on both, while only
//! the first ever runs the tasks the new count asks for. Those are the two facts the
//! `runs_tasks` rows are asserted on.
//!
//! The docs the rows are read off:
//!
//! * `RegisterScalableTarget$ScalableDimension`: "ecs:service:DesiredCount - The task count of
//!   an ECS service", and `Service$desiredCount`: "The desired number of instantiations of the
//!   task definition to keep running on the service" — the capacity an
//!   `ecs:service:DesiredCount` movement asks the owning service for.
//! * `Service$runningCount`: "The number of tasks in the cluster that are in the RUNNING
//!   state" — the count the scheduler drives towards once it holds the new desired count, which
//!   a cluster with nowhere to place a task never reaches, and which the change being fulfilled
//!   does not wait on.
//! * `ScalingActivity$StatusCode` / `$StatusMessage` / `$EndTime`: "Indicates the status of the
//!   scaling activity", "A simple message about the current status of the scaling activity" and
//!   "The Unix timestamp for when the scaling activity ended"
//!   (`smithy_query(query="shape",shape="com.amazonaws.applicationautoscaling#ScalingActivity")`),
//!   whose values are the `ScalingActivityStatusCode` enum (`Pending`, `InProgress`,
//!   `Successful`, `Overridden`, `Unfulfilled`, `Failed`).
//! * `PutMetricAlarmInput$AlarmActions`: "The actions to execute when this alarm transitions to
//!   the ALARM state from any other state", which is what wires the alarm of the second suite
//!   to the step scaling policy it executes.
//!
//! Both suites drive the production router through the shared harness in
//! `tests/all/emulator.rs`: the first over a table of movement paths, directions, magnitudes
//! and namespaces, the second over the alarm-driven step scaling topology on both kinds of
//! estate.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_applicationautoscaling_anyscalefrontendservice::state::ACTIVITY_FULFILMENT;

use crate::emulator::{resource_id_for, Emulator};

/// The status a movement carries until the owning service has the capacity it set in effect.
const IN_PROGRESS: &str = "InProgress";
/// The status it reaches once that service does. See [`IN_PROGRESS`].
const SUCCESSFUL: &str = "Successful";

/// How long a case waits before a movement the owning service has taken on must have settled:
/// the span this service gives a movement, plus generous room for the owning service to place
/// or stop the tasks the new count asks for and for the requests that read it. Nothing in the
/// emulator sleeps for it.
fn settling_wait() -> Duration {
    ACTIVITY_FULFILMENT + Duration::from_secs(20)
}

/// How long a case WATCHES a status it expects to HOLD: twice the span this service gives a
/// movement, so an activity that leaves the status it settled on is caught doing it, and the
/// clock of the service that owns the scaled resource has had every chance to place a task.
fn settled_watch() -> Duration {
    ACTIVITY_FULFILMENT * 2
}

/// How long a bounded poll sleeps between looks.
const POLL: Duration = Duration::from_millis(100);

/// How long a case waits for an alarm-driven breach to reach the resource it scales.
///
/// A just-published datapoint is inside the alarm's evaluation range only once that range has
/// rolled past it, so which evaluation pass first grades it depends on where in the alarm's
/// period the publish landed — the same bounded wait `published_breach_execution.rs` gives
/// that cadence.
const BREACH_WAIT: Duration = Duration::from_secs(15);

/// The activity log of one target, newest first.
async fn activities(emulator: &Emulator, target: &Value, case: &str) -> Vec<Value> {
    let mut request = target.clone();
    request["IncludeNotScaledActivities"] = json!(true);
    let (status, body) = emulator.call("DescribeScalingActivities", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeScalingActivities to answer 200, got {status} {body:?}"
    );
    body["ScalingActivities"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ScalingActivities list, got {body:?}"))
        .clone()
}

/// The newest activity of one target.
async fn newest(emulator: &Emulator, target: &Value, case: &str) -> Value {
    let listed = activities(emulator, target, case).await;
    listed
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected at least one scaling activity"))
}

/// A member of an activity as text.
fn field(activity: &Value, member: &str) -> String {
    activity[member].as_str().unwrap_or_default().to_string()
}

/// The newest activity of one target for the whole of `within`, answered as it was last read —
/// returning as soon as it LEAVES `wanted`, which is what a case that expects a status to hold
/// fails on.
async fn watched_status(
    emulator: &Emulator,
    target: &Value,
    wanted: &str,
    within: Duration,
    case: &str,
) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let last = newest(emulator, target, case).await;
        if field(&last, "StatusCode") != wanted || Instant::now() >= deadline {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The newest activity of one target once it carries `wanted`, or once the deadline is up — so
/// a case whose movement never reaches that status fails on what it last read.
async fn awaited_status(
    emulator: &Emulator,
    target: &Value,
    wanted: &str,
    within: Duration,
    case: &str,
) -> Value {
    let deadline = Instant::now() + within;
    let mut last = newest(emulator, target, case).await;
    while field(&last, "StatusCode") != wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        last = newest(emulator, target, case).await;
    }
    last
}

/// The `runningCount` ECS reports for the suite's service once it reaches `wanted`, or once the
/// deadline is up — so a case whose cluster never places the tasks fails on what it last read.
async fn awaited_running_count(emulator: &Emulator, wanted: i64, within: Duration) -> i64 {
    let deadline = Instant::now() + within;
    let mut last = emulator.ecs_running_count(CLUSTER, SERVICE).await;
    while last != wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        last = emulator.ecs_running_count(CLUSTER, SERVICE).await;
    }
    last
}

/// The message a movement still waiting on the owning service reports.
fn awaiting_message(noun: &str, capacity: i64, namespace: &str) -> String {
    format!(
        "Successfully set {noun} to {capacity}. Waiting for change to be fulfilled by \
         {namespace}."
    )
}

/// The message the same movement reports once that service has made the capacity take effect.
fn fulfilled_message(noun: &str, capacity: i64, namespace: &str) -> String {
    format!(
        "Successfully set {noun} to {capacity}. Change successfully fulfilled by {namespace}."
    )
}

/// Assert that `activity` is a movement to `capacity` the owning service has still to take on:
/// the non-terminal status, the awaited message and no `EndTime` to report.
fn assert_awaiting(activity: &Value, noun: &str, capacity: i64, namespace: &str, case: &str) {
    let expected = awaiting_message(noun, capacity, namespace);
    assert_eq!(
        field(activity, "StatusCode"),
        IN_PROGRESS,
        "case {case:?}: expected a movement the owning service has not fulfilled to be \
         {IN_PROGRESS}, got {activity:?}"
    );
    assert_eq!(
        activity["StatusMessage"],
        json!(expected),
        "case {case:?}: expected StatusMessage {expected:?}, got {activity:?}"
    );
    assert_eq!(
        activity["EndTime"],
        Value::Null,
        "case {case:?}: expected a movement that has not ended to carry no EndTime, \
         got {activity:?}"
    );
}

/// Assert that `activity` is a movement to `capacity` the owning service has taken on: the
/// terminal status, the settled message and an `EndTime` at or after its `StartTime`.
fn assert_fulfilled(activity: &Value, noun: &str, capacity: i64, namespace: &str, case: &str) {
    let expected = fulfilled_message(noun, capacity, namespace);
    assert_eq!(
        field(activity, "StatusCode"),
        SUCCESSFUL,
        "case {case:?}: expected a movement the owning service fulfilled to be {SUCCESSFUL}, \
         got {activity:?}"
    );
    assert_eq!(
        activity["StatusMessage"],
        json!(expected),
        "case {case:?}: expected StatusMessage {expected:?}, got {activity:?}"
    );
    let start = activity["StartTime"].as_f64().unwrap_or_default();
    let end = activity["EndTime"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: expected a fulfilled movement to carry an EndTime, got {activity:?}")
    });
    assert!(
        end >= start,
        "case {case:?}: expected EndTime {end} at or after StartTime {start}, got {activity:?}"
    );
}

/// The dimension the `ecs` rows of every suite below scale: "ecs:service:DesiredCount - The
/// task count of an ECS service".
const ECS_DIMENSION: &str = "ecs:service:DesiredCount";

/// That dimension's scaling property read as words, which the messages of its activities
/// carry. See [`ECS_DIMENSION`].
const ECS_NOUN: &str = "desired count";

/// One registration-driven movement: the estate the resource lives on, where its capacity
/// starts, the window registered next, and where the movement lands.
struct MovementRow {
    case: &'static str,
    /// The `ScalableDimension` the row scales, which names the namespace too.
    dimension: &'static str,
    /// Whether the estate the resource lives on can RUN what the capacity asks for — for
    /// `ecs`, whether its cluster has a container instance to place tasks on. It is NOT what
    /// the movement waits on: a row whose cluster has none settles like any other, and its
    /// `runningCount` is asserted to stay at nothing while it does.
    runs_tasks: bool,
    /// The capacity the resource is created at, inside the first registered window.
    starting: i64,
    /// The window registered next, which is what moves the capacity.
    window: (i64, i64),
    /// Where the capacity lands under that window.
    capacity: i64,
    /// The dimension's scaling property read as words, which the messages carry.
    noun: &'static str,
}

/// The rows differ in the direction the capacity moves, in its magnitude, in whether the estate
/// can run the tasks the capacity asks for at all, and in whether the owning service's stored
/// capacity can be read back in the first place.
const MOVEMENT_CATALOG: &[MovementRow] = &[
    // A scale in on a cluster that can place tasks: the scheduler brings the task set down to
    // the new count, and the movement settles.
    MovementRow {
        case: "a scale in an ECS cluster with capacity runs",
        dimension: ECS_DIMENSION,
        runs_tasks: true,
        starting: 6,
        window: (1, 3),
        capacity: 3,
        noun: ECS_NOUN,
    },
    // The symmetric leg: a scale out of three tasks the same cluster can place.
    MovementRow {
        case: "a scale out an ECS cluster with capacity runs",
        dimension: ECS_DIMENSION,
        runs_tasks: true,
        starting: 2,
        window: (5, 9),
        capacity: 5,
        noun: ECS_NOUN,
    },
    // The estate of the recorded scenario: a cluster with no container instance, where the
    // EC2 launch type has nowhere to run a task. Amazon ECS holds the count all the same, so
    // the change is fulfilled while none of the tasks it asks for ever runs.
    MovementRow {
        case: "a scale in an ECS cluster with no container instance settles",
        dimension: ECS_DIMENSION,
        runs_tasks: false,
        starting: 6,
        window: (1, 3),
        capacity: 3,
        noun: ECS_NOUN,
    },
    // The same estate scaling out, by a larger magnitude.
    MovementRow {
        case: "a scale out an ECS cluster with no container instance settles",
        dimension: ECS_DIMENSION,
        runs_tasks: false,
        starting: 2,
        window: (7, 12),
        capacity: 7,
        noun: ECS_NOUN,
    },
    // And by a single task, which is the smallest change there is.
    MovementRow {
        case: "a single-task scale in an ECS cluster with no container instance settles",
        dimension: ECS_DIMENSION,
        runs_tasks: false,
        starting: 4,
        window: (1, 3),
        capacity: 3,
        noun: ECS_NOUN,
    },
    // A namespace whose owning service's capacity this emulator cannot read back at all: the
    // span this service gives a movement is the whole of the wait.
    MovementRow {
        case: "a namespace whose owning service's capacity cannot be read back",
        dimension: "lambda:function:ProvisionedConcurrency",
        runs_tasks: true,
        starting: 2,
        window: (6, 8),
        capacity: 6,
        noun: "provisioned concurrency",
    },
];

/// The namespace a dimension belongs to: its first segment.
fn namespace_of(dimension: &str) -> &str {
    dimension.split(':').next().unwrap_or_default()
}

/// The three identity members of a target on `dimension`.
fn target_of(dimension: &str, resource_id: &str) -> Value {
    json!({
        "ServiceNamespace": namespace_of(dimension),
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
    })
}

/// Register the target `resource_id` names on `dimension` with the window `min`..`max`.
async fn register(
    emulator: &Emulator,
    dimension: &str,
    resource_id: &str,
    (min, max): (i64, i64),
    case: &str,
) {
    let mut request = target_of(dimension, resource_id);
    request["MinCapacity"] = json!(min);
    request["MaxCapacity"] = json!(max);
    let (status, body) = emulator.call("RegisterScalableTarget", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected RegisterScalableTarget {min}..{max} to answer 200, \
         got {status} {body:?}"
    );
}

/// The ECS cluster and service of a row, and the resource created for it.
const CLUSTER: &str = "fulfilment";
/// See [`CLUSTER`].
const SERVICE: &str = "web";

/// Create the resource a row scales and answer the `ResourceId` its target names it by.
async fn given_resource(emulator: &Emulator, row: &MovementRow) -> String {
    if namespace_of(row.dimension) != "ecs" {
        // A namespace whose owning service this emulator holds no store for has nothing to
        // create: the registration is accepted on its own terms.
        return resource_id_for(row.dimension);
    }
    if row.runs_tasks {
        emulator
            .given_ecs_service(CLUSTER, SERVICE, row.starting)
            .await
    } else {
        emulator
            .given_ecs_service_without_capacity(CLUSTER, SERVICE, row.starting)
            .await
    }
}

#[tokio::test]
async fn a_movement_settles_once_the_owning_service_holds_the_capacity_it_set() {
    for row in MOVEMENT_CATALOG {
        let case = row.case;
        let namespace = namespace_of(row.dimension);
        let emulator = Emulator::empty();
        let resource_id = given_resource(&emulator, row).await;
        let target = target_of(row.dimension, &resource_id);

        // The first registration declares a window the capacity already sits inside, so it
        // moves nothing and the log opens empty.
        register(
            &emulator,
            row.dimension,
            &resource_id,
            (1.min(row.starting), row.starting.max(row.window.1)),
            case,
        )
        .await;
        let none_yet = activities(&emulator, &target, case).await;
        assert!(
            none_yet.is_empty(),
            "case {case:?}: expected a first registration to record no activity, got {none_yet:?}"
        );

        // The movement: the window registered next holds the capacity at `row.capacity`.
        register(&emulator, row.dimension, &resource_id, row.window, case).await;
        let started = newest(&emulator, &target, case).await;
        assert_awaiting(&started, row.noun, row.capacity, namespace, case);
        let expected_description = format!("Setting {} to {}.", row.noun, row.capacity);
        assert_eq!(
            started["Description"],
            json!(expected_description),
            "case {case:?}: expected Description {expected_description:?}, got {started:?}"
        );

        if namespace == "ecs" {
            // The movement itself happened: what is outstanding is the owning service's
            // fulfilment of it, not this service's decision.
            let desired = emulator.ecs_desired_count(CLUSTER, SERVICE).await;
            assert_eq!(
                desired, row.capacity,
                "case {case:?}: expected the ECS desiredCount to be {}, got {desired}",
                row.capacity
            );
        }

        // The owning service HOLDS the capacity this movement set, so the change took effect
        // and the activity settles — the same activity, not a second one — whether or not the
        // estate can run what the count asks for.
        let settled = awaited_status(&emulator, &target, SUCCESSFUL, settling_wait(), case).await;
        assert_fulfilled(&settled, row.noun, row.capacity, namespace, case);
        assert_eq!(
            settled["ActivityId"], started["ActivityId"],
            "case {case:?}: expected the same activity to settle, not a second one, \
             got {settled:?}"
        );
        // And it stays settled: a terminal status is terminal.
        let held = watched_status(&emulator, &target, SUCCESSFUL, settled_watch(), case).await;
        assert_fulfilled(&held, row.noun, row.capacity, namespace, case);

        if namespace == "ecs" {
            // What the scheduler then does with that count is a different fact from the change
            // having been fulfilled, and is read back where it lives: a cluster with somewhere
            // to run the tasks runs them, and one with nowhere to run them has run none of them
            // by the end of the watch above.
            let expected = if row.runs_tasks { row.capacity } else { 0 };
            let running = if row.runs_tasks {
                awaited_running_count(&emulator, expected, settling_wait()).await
            } else {
                emulator.ecs_running_count(CLUSTER, SERVICE).await
            };
            assert_eq!(
                running, expected,
                "case {case:?}: expected the ECS runningCount to be {expected}, got {running}"
            );
        }

        // And a read does not change what the next reader sees.
        let again = activities(&emulator, &target, case).await;
        let once_more = activities(&emulator, &target, case).await;
        assert_eq!(
            again, once_more,
            "case {case:?}: expected a second read to answer identically, got {once_more:?}"
        );
    }
}

/// The metric namespace the alarms of the second suite watch. A customer namespace, so the
/// alarm is one a client could create itself.
const METRIC_NAMESPACE: &str = "Emulator/MovementFulfilment";
/// The metric those alarms watch inside it.
const METRIC_NAME: &str = "NonBreachDriver";
/// The value published to it, which sits below every row's threshold.
const PUBLISHED_VALUE: f64 = 50.0;
/// The threshold a `LessThanThreshold` alarm on that value breaches.
const THRESHOLD: f64 = 90.0;
/// The policy every row of the second suite puts.
const POLICY_NAME: &str = "scale-in";

/// One alarm-driven step scaling breach: the estate the ECS service lives on, where its count
/// starts, the step the policy carries and where the movement lands.
struct BreachRow {
    case: &'static str,
    /// Whether the cluster has a container instance to place the tasks on. See
    /// [`MovementRow::runs_tasks`].
    runs_tasks: bool,
    starting: i64,
    /// The window the target is registered with, which admits the adjustment in full.
    window: (i64, i64),
    /// The `ScalingAdjustment` of the single step, negative to remove capacity.
    adjustment: i64,
    /// Where the count lands: the adjustment applied to the starting count.
    capacity: i64,
}

/// The rows differ in whether the estate can run the tasks the new count asks for and in the
/// magnitude of the step; every row carries the topology of the recorded scenario — a
/// `LessThanThreshold` alarm whose `AlarmActions` name the policy, fed by a published datapoint.
const BREACH_CATALOG: &[BreachRow] = &[
    BreachRow {
        case: "a breach on a cluster with no container instance",
        runs_tasks: false,
        starting: 6,
        window: (1, 40),
        adjustment: -3,
        capacity: 3,
    },
    BreachRow {
        case: "a breach on a cluster with capacity",
        runs_tasks: true,
        starting: 6,
        window: (1, 40),
        adjustment: -3,
        capacity: 3,
    },
    BreachRow {
        case: "a single-task breach on a cluster with no container instance",
        runs_tasks: false,
        starting: 4,
        window: (1, 40),
        adjustment: -1,
        capacity: 3,
    },
];

/// Wait for the ECS service's `desiredCount` to reach `expected`, answering what it last read.
async fn await_desired_count(emulator: &Emulator, expected: i64, within: Duration) -> i64 {
    let deadline = Instant::now() + within;
    let mut last = emulator.ecs_desired_count(CLUSTER, SERVICE).await;
    while last != expected && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        last = emulator.ecs_desired_count(CLUSTER, SERVICE).await;
    }
    last
}

#[tokio::test]
async fn an_alarm_driven_step_scaling_movement_settles_on_the_count_the_ecs_service_holds() {
    for row in BREACH_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let resource_id = if row.runs_tasks {
            emulator
                .given_ecs_service(CLUSTER, SERVICE, row.starting)
                .await
        } else {
            emulator
                .given_ecs_service_without_capacity(CLUSTER, SERVICE, row.starting)
                .await
        };
        let target = target_of(ECS_DIMENSION, &resource_id);
        register(&emulator, ECS_DIMENSION, &resource_id, row.window, case).await;

        let mut request = target.clone();
        request["PolicyName"] = json!(POLICY_NAME);
        request["PolicyType"] = json!("StepScaling");
        request["StepScalingPolicyConfiguration"] = json!({
            "AdjustmentType": "ChangeInCapacity",
            "MetricAggregationType": "Maximum",
            // Nothing here breaches twice, so the wait a previous activity is given is named
            // rather than left to its default.
            "Cooldown": 0,
            "StepAdjustments": [
                {"MetricIntervalUpperBound": 0.0, "ScalingAdjustment": row.adjustment},
            ],
        });
        let (status, body) = emulator.call("PutScalingPolicy", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutScalingPolicy to answer 200, got {status} {body:?}"
        );
        let policy_arn = body["PolicyARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a PolicyARN, got {body:?}"))
            .to_string();

        // The alarm a client wires itself: its actions name the policy's ARN, and the
        // published value sits below the threshold it compares against.
        let alarm_name = format!("{}-alarm", case.replace(' ', "-"));
        let (status, body) = emulator
            .call_cloudwatch(
                "PutMetricAlarm",
                json!({
                    "AlarmName": alarm_name,
                    "Namespace": METRIC_NAMESPACE,
                    "MetricName": METRIC_NAME,
                    "Dimensions": [{"Name": "ServiceName", "Value": SERVICE}],
                    "Statistic": "Maximum",
                    "Period": 10,
                    "EvaluationPeriods": 1,
                    "DatapointsToAlarm": 1,
                    "Threshold": THRESHOLD,
                    "ComparisonOperator": "LessThanThreshold",
                    "TreatMissingData": "notBreaching",
                    "AlarmActions": [policy_arn],
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutMetricAlarm to answer 200, got {status} {body:?}"
        );
        let (status, body) = emulator
            .call_cloudwatch(
                "PutMetricData",
                json!({
                    "Namespace": METRIC_NAMESPACE,
                    "MetricData": [{
                        "MetricName": METRIC_NAME,
                        "Dimensions": [{"Name": "ServiceName", "Value": SERVICE}],
                        "Value": PUBLISHED_VALUE,
                        // The alarm above is evaluated on a sub-minute period, which a series
                        // is readable at only when it is STORED at that resolution: "Data
                        // points with a period of less than 60 seconds are available only for
                        // custom metrics defined with a StorageResolution of 1"
                        // (`MetricDatum$StorageResolution`), a regular-resolution datapoint
                        // holding no sub-minute detail at all.
                        "StorageResolution": 1,
                    }],
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutMetricData to answer 200, got {status} {body:?}"
        );

        let moved = await_desired_count(&emulator, row.capacity, BREACH_WAIT).await;
        assert_eq!(
            moved, row.capacity,
            "case {case:?}: expected the breach to move the ECS desiredCount to {}, got {moved}",
            row.capacity
        );

        // The activity the breach recorded names the capacity it set, the alarm that fired and
        // the policy it executed.
        let activity = newest(&emulator, &target, case).await;
        let expected_description = format!("Setting {ECS_NOUN} to {}.", row.capacity);
        assert_eq!(
            activity["Description"],
            json!(expected_description),
            "case {case:?}: expected Description {expected_description:?}, got {activity:?}"
        );
        let cause = field(&activity, "Cause");
        assert!(
            cause.contains(&alarm_name) && cause.contains(POLICY_NAME),
            "case {case:?}: expected the Cause to name the alarm {alarm_name:?} and the policy \
             {POLICY_NAME:?}, got {cause:?}"
        );

        // The owning service HOLDS the count the breach set, so the change took effect and the
        // activity settles — the same activity, not a second one — on either estate.
        let settled = awaited_status(&emulator, &target, SUCCESSFUL, settling_wait(), case).await;
        assert_fulfilled(&settled, ECS_NOUN, row.capacity, "ecs", case);
        assert_eq!(
            settled["ActivityId"], activity["ActivityId"],
            "case {case:?}: expected the same activity to settle, not a second one, \
             got {settled:?}"
        );
        let held = watched_status(&emulator, &target, SUCCESSFUL, settled_watch(), case).await;
        assert_fulfilled(&held, ECS_NOUN, row.capacity, "ecs", case);

        // And what the scheduler did with that count, read back where it lives: a cluster with
        // no container instance has run none of the tasks the new count asks for by the end of
        // the watch above, and the breach's movement settled all the same.
        let expected = if row.runs_tasks { row.capacity } else { 0 };
        let running = if row.runs_tasks {
            awaited_running_count(&emulator, expected, settling_wait()).await
        } else {
            emulator.ecs_running_count(CLUSTER, SERVICE).await
        };
        assert_eq!(
            running, expected,
            "case {case:?}: expected the ECS runningCount to be {expected}, got {running}"
        );
    }
}

