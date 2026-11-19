// SPDX-License-Identifier: CC0-1.0

//! When a capacity movement's scaling activity reaches its TERMINAL status, end to end over
//! the wire.
//!
//! The rule under test: setting a capacity and that capacity taking effect are two different
//! things. This service decides a capacity and sets it on the resource — which is what the
//! activity's `Description` states ("Setting desired count to 3.") — and the service that owns
//! that resource then has to make it real, which is what the same activity's `StatusMessage`
//! says it is still waiting for ("Successfully set desired count to 3. Waiting for change to be
//! fulfilled by ecs."). So a movement reports `InProgress` for as long as the owning service
//! has not made the capacity it set take effect, and `Successful` — with the `EndTime` of that
//! instant and the settled message ("Change successfully fulfilled by ecs.") — only once it
//! has. On an estate the owning service can never satisfy, the movement therefore never
//! reaches a different status, however long a client polls.
//!
//! Where the owning service reports no in-effect capacity of its own, there is nothing to wait
//! on beyond the span this service gives a movement, which is the last row of the first table.
//!
//! The docs the rows are read off:
//!
//! * `Service$desiredCount`: "The desired number of instantiations of the task definition to
//!   keep running on the service", and `Service$runningCount`: "The number of tasks in the
//!   cluster that are in the RUNNING state" — the two counts whose difference is what an
//!   `ecs:service:DesiredCount` movement waits on.
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
//! The three suites drive the production router through the shared harness in
//! `tests/all/emulator.rs`: the first over a table of movement paths, directions and
//! namespaces, the second over the alarm-driven step scaling topology on both kinds of estate,
//! and the third over an estate that gains the capacity to fulfil a movement only AFTER that
//! movement was recorded — which is what tells a terminal status read off the owning service
//! apart from one this service's own clock would have reached anyway.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_applicationautoscaling_anyscalefrontendservice::state::ACTIVITY_FULFILMENT;

use crate::emulator::{resource_id_for, Emulator};

/// The status a movement carries until the owning service has made its capacity take effect.
const IN_PROGRESS: &str = "InProgress";
/// The status it reaches once that service has. See [`IN_PROGRESS`].
const SUCCESSFUL: &str = "Successful";

/// How long a case waits before a movement whose capacity the owning service CAN reach must
/// have settled: the span this service gives a movement, plus room for the owning service's
/// own task transitions and for the requests that read them. Nothing in the emulator sleeps
/// for it.
fn settling_wait() -> Duration {
    ACTIVITY_FULFILMENT + Duration::from_secs(3)
}

/// How long a case keeps watching a movement the owning service can NEVER fulfil before
/// concluding that it stays non-terminal: well past the span a movement is given, so a
/// movement that settled on the clock alone would be caught here.
fn unfulfilled_watch() -> Duration {
    ACTIVITY_FULFILMENT + Duration::from_secs(1)
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

/// Assert that `activity` is a movement to `capacity` that is still waiting on `namespace`:
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

/// Keep re-reading the newest activity of `target` for [`unfulfilled_watch`], asserting on
/// every look that it is still a movement to `capacity` waiting on `namespace`.
///
/// The invariant is WATCHED rather than slept through: a movement that reached a terminal
/// status at any point inside the watch fails here, instead of being missed between two looks.
/// The watch is longer than the span this service gives a movement, which is what makes it
/// evidence — a movement settled by this service's own clock would have reported a terminal
/// status before it was up.
/// It is the SAME activity throughout, `activity_id`: a movement replaced by another one inside
/// the watch fails on the look that saw it, rather than on a later read of a log that has moved
/// on.
async fn assert_stays_awaiting(
    emulator: &Emulator,
    target: &Value,
    activity_id: &Value,
    noun: &str,
    capacity: i64,
    namespace: &str,
    case: &str,
) {
    let watch_until = Instant::now() + unfulfilled_watch();
    loop {
        let held = newest(emulator, target, case).await;
        assert_awaiting(&held, noun, capacity, namespace, case);
        assert_eq!(
            &held["ActivityId"], activity_id,
            "case {case:?}: expected the same activity to still be reported, got {held:?}"
        );
        if Instant::now() >= watch_until {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Assert that `activity` is a movement to `capacity` the owning service has fulfilled: the
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
    /// Whether the estate the resource lives on can actually reach the capacity the movement
    /// sets — for `ecs`, whether its cluster has a container instance to run tasks on.
    reachable: bool,
    /// The capacity the resource is created at, inside the first registered window.
    starting: i64,
    /// The window registered next, which is what moves the capacity.
    window: (i64, i64),
    /// Where the capacity lands under that window.
    capacity: i64,
    /// The dimension's scaling property read as words, which the messages carry.
    noun: &'static str,
}

/// The rows differ in the direction the capacity moves, in its magnitude, in whether the
/// owning service can reach the capacity at all, and in whether that service reports an
/// in-effect capacity in the first place.
const MOVEMENT_CATALOG: &[MovementRow] = &[
    // A scale in on a cluster that can place tasks: the scheduler brings the task set down to
    // the new count, so the movement settles.
    MovementRow {
        case: "a scale in an ECS cluster with capacity can reach",
        dimension: ECS_DIMENSION,
        reachable: true,
        starting: 6,
        window: (1, 3),
        capacity: 3,
        noun: ECS_NOUN,
    },
    // The symmetric leg: a scale out of three tasks the same cluster can place.
    MovementRow {
        case: "a scale out an ECS cluster with capacity can reach",
        dimension: ECS_DIMENSION,
        reachable: true,
        starting: 2,
        window: (5, 9),
        capacity: 5,
        noun: ECS_NOUN,
    },
    // The estate of the recorded scenario: a cluster with no container instance, where the
    // EC2 launch type has nowhere to run a task, so the count is set and never reached.
    MovementRow {
        case: "a scale in an ECS cluster with no container instance cannot reach",
        dimension: ECS_DIMENSION,
        reachable: false,
        starting: 6,
        window: (1, 3),
        capacity: 3,
        noun: ECS_NOUN,
    },
    // The same estate scaling out, by a larger magnitude.
    MovementRow {
        case: "a scale out an ECS cluster with no container instance cannot reach",
        dimension: ECS_DIMENSION,
        reachable: false,
        starting: 2,
        window: (7, 12),
        capacity: 7,
        noun: ECS_NOUN,
    },
    // A namespace whose owning service reports no in-effect capacity to wait on at all: the
    // span this service gives a movement is the whole of the wait.
    MovementRow {
        case: "a namespace whose owning service reports no in-effect capacity",
        dimension: "lambda:function:ProvisionedConcurrency",
        reachable: true,
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
    if row.reachable {
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
async fn a_movement_settles_only_once_the_owning_service_reaches_the_capacity_it_set() {
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

        if row.reachable {
            let settled =
                awaited_status(&emulator, &target, SUCCESSFUL, settling_wait(), case).await;
            assert_fulfilled(&settled, row.noun, row.capacity, namespace, case);
            assert_eq!(
                settled["ActivityId"], started["ActivityId"],
                "case {case:?}: expected the same activity to settle, not a second one, \
                 got {settled:?}"
            );
            if namespace == "ecs" {
                let running = emulator.ecs_running_count(CLUSTER, SERVICE).await;
                assert_eq!(
                    running, row.capacity,
                    "case {case:?}: expected the ECS runningCount to have reached {}, \
                     got {running}",
                    row.capacity
                );
            }
            continue;
        }

        // The estate cannot reach the capacity, so no amount of waiting settles the movement,
        // and it is the same activity that is still reported throughout.
        assert_stays_awaiting(
            &emulator,
            &target,
            &started["ActivityId"],
            row.noun,
            row.capacity,
            namespace,
            case,
        )
        .await;
        let running = emulator.ecs_running_count(CLUSTER, SERVICE).await;
        assert_eq!(
            running, 0,
            "case {case:?}: expected a cluster with no container instance to run no task, \
             got {running}"
        );

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
    /// Whether the cluster has a container instance to run the tasks on.
    reachable: bool,
    starting: i64,
    /// The window the target is registered with, which admits the adjustment in full.
    window: (i64, i64),
    /// The `ScalingAdjustment` of the single step, negative to remove capacity.
    adjustment: i64,
    /// Where the count lands: the adjustment applied to the starting count.
    capacity: i64,
}

/// The rows differ in whether the estate can reach the new count and in the magnitude of the
/// step; both carry the topology of the recorded scenario — a `LessThanThreshold` alarm whose
/// `AlarmActions` name the policy, fed by a published datapoint.
const BREACH_CATALOG: &[BreachRow] = &[
    BreachRow {
        case: "a breach on a cluster with no container instance",
        reachable: false,
        starting: 6,
        window: (1, 40),
        adjustment: -3,
        capacity: 3,
    },
    BreachRow {
        case: "a breach on a cluster with capacity",
        reachable: true,
        starting: 6,
        window: (1, 40),
        adjustment: -3,
        capacity: 3,
    },
    BreachRow {
        case: "a single-task breach on a cluster with no container instance",
        reachable: false,
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
async fn an_alarm_driven_step_scaling_movement_waits_on_the_ecs_service_it_scales() {
    for row in BREACH_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        let resource_id = if row.reachable {
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

        if row.reachable {
            let settled =
                awaited_status(&emulator, &target, SUCCESSFUL, settling_wait(), case).await;
            assert_fulfilled(&settled, ECS_NOUN, row.capacity, "ecs", case);
            continue;
        }

        // No container instance, so Amazon ECS can never finish fulfilling the new count and
        // the movement stays non-terminal however long a client polls.
        assert_stays_awaiting(
            &emulator,
            &target,
            &activity["ActivityId"],
            ECS_NOUN,
            row.capacity,
            "ecs",
            case,
        )
        .await;
        let running = emulator.ecs_running_count(CLUSTER, SERVICE).await;
        assert_eq!(
            running, 0,
            "case {case:?}: expected a cluster with no container instance to run no task, \
             got {running}"
        );
    }
}

/// How long a row of the third suite watches a movement for the fulfilment the estate can only
/// reach once it has somewhere to run the tasks: many times the span this service gives a
/// movement, which leaves room for the scheduler's placement, for the tasks' own start and for
/// the reads that observe them. Derived from that span like the two waits above, so all three
/// move together. Nothing in the emulator sleeps for it, and the case fails on the status it
/// last read rather than on a clock reading.
fn late_fulfilment_budget() -> Duration {
    ACTIVITY_FULFILMENT * 20
}

/// One movement whose estate gains the capacity to fulfil it only AFTER the movement was
/// recorded: where the ECS service's task count starts, the window registered over it, and
/// where the movement lands.
struct LateFulfilmentRow {
    case: &'static str,
    /// The `desiredCount` the service is created with, on a cluster with no container instance.
    starting: i64,
    /// The window registered next, which is what moves the count.
    window: (i64, i64),
    /// Where the count lands under that window.
    capacity: i64,
}

/// The rows differ in the direction the capacity moves and in the magnitude it moves by, so the
/// rule is pinned on a scale in, on a scale out and on a single-task change.
const LATE_FULFILMENT_CATALOG: &[LateFulfilmentRow] = &[
    LateFulfilmentRow {
        case: "a scale in of three tasks fulfilled once the cluster gains an instance",
        starting: 6,
        window: (1, 3),
        capacity: 3,
    },
    LateFulfilmentRow {
        case: "a scale out of three tasks fulfilled once the cluster gains an instance",
        starting: 2,
        window: (5, 9),
        capacity: 5,
    },
    LateFulfilmentRow {
        case: "a single-task scale in fulfilled once the cluster gains an instance",
        starting: 4,
        window: (1, 3),
        capacity: 3,
    },
];

/// What ends a capacity movement is the OWNING service's own state, not this service's clock:
/// a movement whose capacity the estate could not reach stays `InProgress` past the span a
/// movement is given, and reaches `Successful` at the instant the estate — once it has somewhere
/// to run the tasks — reports the capacity as in effect.
///
/// The other half of the first suite's unreachable rows: those pin that such a movement never
/// settles while the estate cannot satisfy it, and these pin that the SAME activity settles when
/// it can, so neither a movement that settles on the clock alone nor one that can never settle
/// at all passes both.
#[tokio::test]
async fn a_movement_settles_when_the_owning_service_reaches_its_capacity_later() {
    for row in LATE_FULFILMENT_CATALOG {
        let case = row.case;
        let emulator = Emulator::empty();
        // A cluster with no container instance: the EC2 launch type has nowhere to run a task,
        // so no count it is given is ever in effect.
        let resource_id = emulator
            .given_ecs_service_without_capacity(CLUSTER, SERVICE, row.starting)
            .await;
        let target = target_of(ECS_DIMENSION, &resource_id);
        register(&emulator, ECS_DIMENSION, &resource_id, row.window, case).await;

        // This service did its half of the movement: Amazon ECS holds the count it set.
        let desired = emulator.ecs_desired_count(CLUSTER, SERVICE).await;
        assert_eq!(
            desired, row.capacity,
            "case {case:?}: expected the ECS desiredCount to be {}, got {desired}",
            row.capacity
        );

        // The other half is outstanding, and stays outstanding past the span a movement is
        // given, because the tasks the count wants cannot be placed anywhere.
        let started = newest(&emulator, &target, case).await;
        assert_awaiting(&started, ECS_NOUN, row.capacity, "ecs", case);
        assert_stays_awaiting(
            &emulator,
            &target,
            &started["ActivityId"],
            ECS_NOUN,
            row.capacity,
            "ecs",
            case,
        )
        .await;
        let stalled = emulator.ecs_running_count(CLUSTER, SERVICE).await;
        assert_eq!(
            stalled, 0,
            "case {case:?}: expected a cluster with no container instance to run no task, \
             got {stalled}"
        );

        // The estate gains somewhere to run them — "the EC2 launch type runs your tasks on
        // Amazon EC2 instances registered to your cluster" — so the scheduler places the task
        // set the count asks for, and THAT is what ends the activity.
        emulator.given_container_instance(CLUSTER).await;
        let settled =
            awaited_status(&emulator, &target, SUCCESSFUL, late_fulfilment_budget(), case).await;
        assert_fulfilled(&settled, ECS_NOUN, row.capacity, "ecs", case);
        assert_eq!(
            settled["ActivityId"], started["ActivityId"],
            "case {case:?}: expected the same activity to settle, not a second one, \
             got {settled:?}"
        );
        let running = emulator.ecs_running_count(CLUSTER, SERVICE).await;
        assert_eq!(
            running, row.capacity,
            "case {case:?}: expected the ECS runningCount to have reached {}, got {running}",
            row.capacity
        );

        // And the settled activity is what every later reader sees: a read changes nothing.
        let again = activities(&emulator, &target, case).await;
        let once_more = activities(&emulator, &target, case).await;
        assert_eq!(
            again, once_more,
            "case {case:?}: expected a second read to answer identically, got {once_more:?}"
        );
    }
}
