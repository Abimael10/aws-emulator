// SPDX-License-Identifier: CC0-1.0

//! A target-tracking scaling policy moves its group's capacity when an alarm it owns fires —
//! over the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles for the
//! binary.
//!
//! The RULE under test: **a `TargetTrackingScaling` policy states the value the metric it
//! watches is to sit at per unit of the capacity the group maintains
//! (`TargetTrackingConfiguration$TargetValue`) rather than an adjustment, so the policy is
//! performed by the alarms it owns: when one of them hands this service a transition, the
//! group's `DesiredCapacity` moves to the smallest capacity at which the value that alarm
//! graded divides down to the target — `ceil(current × metric ÷ target)` — held inside the
//! group's own `MinSize`..`MaxSize`, never downward for a policy whose `DisableScaleIn` bars
//! it from removing an instance, not at all for a policy that is disabled, and recording a
//! scaling activity that names the alarm and the policy.**
//!
//! Three services have to agree for that to hold: CloudWatch owns the alarms the policy mints,
//! their evaluation and the record of the action they executed, Auto Scaling owns the policy,
//! the group and its capacity, and Amazon EC2 holds the launch template the group is built on
//! and the instances it launches. The alarm SET a target-tracking put mints is pinned over its
//! own table by
//! `services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/target_tracking_alarm_series.rs`,
//! and the dispatch of an alarm action by
//! `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/`; what
//! only this suite reaches is the capacity the hand-off between the two moves.
//!
//! The rows differ along what the rule generalises over: the capacity the group starts from,
//! the ratio of the graded value to the target (above it, just above it, at it, under it, far
//! above it), which of the policy's two alarms hands the value over, the aggregation period
//! the policy's metric is graded on, whether the ratio is held at a bound, whether scaling in
//! is disabled, and whether the policy is enabled. The last test drives the same path from
//! datapoints a client PUBLISHES, so nothing of it is forced.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, cw, router};
use crate::query_wire::{element, elements, form_encode, opt_element};

/// The awsQuery API version an Auto Scaling client sends, and the ec2Query one Amazon EC2
/// expects.
const AUTOSCALING_VERSION: &str = "2011-01-01";
const EC2_VERSION: &str = "2016-11-15";

/// The Region the whole wiring of a row lives in: a policy's alarms are evaluated in the
/// Region the group lives in, so group, policy and alarms share one.
const REGION: &str = "us-west-2";

/// The series the policies track: a customer namespace, so the datapoints go over the wire the
/// way a client's own do (`PutMetricData` refuses an `AWS/` namespace).
const NAMESPACE: &str = "Custom/Tracking";
const METRIC: &str = "Backlog";

/// How often a poll looks again, and how many times, before what it waits for is called
/// undelivered.
///
/// A BACKSTOP, not a schedule: every wait below polls for the CONDITION it needs — an action
/// CloudWatch has recorded, a capacity the group reports — so a loaded machine delays a row
/// instead of breaking it.
const LOOK_AGAIN: Duration = Duration::from_millis(25);
const LOOKS: usize = 400;

/// How many times a row that expects NO movement looks again before it believes the capacity
/// was left alone.
///
/// Each look is a read of the group, and a read is what retires whatever the clock has made
/// due, so looking repeatedly is what gives a hand-over that WOULD move the capacity every
/// chance to do so. Nothing is asserted about how long that takes.
const HELD_LOOKS: usize = 40;

/// How long the published-datapoint test waits for the alarm its policy owns to grade the
/// series and the capacity to follow, and how often it looks.
///
/// Generous: the alarm grades whole periods that have CLOSED, so the deadline leaves room for
/// several of them on a loaded machine. A row that never gets there fails on what it last read
/// rather than hanging, and nothing is asserted about the elapsed time.
const PUBLISHED_LOOKS: usize = 1_200;
const PUBLISHED_LOOK_AGAIN: Duration = Duration::from_millis(100);

/// One form-wire call against the service whose signing name is `signing_name`, answered as
/// `(status, body)`.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, REGION))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// One Auto Scaling call that must answer 200.
async fn asg(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) =
        form_call(router, "autoscaling", AUTOSCALING_VERSION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from autoscaling {action}, got {status} with {body}"
    );
    body
}

/// One parameter pair.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The owned parameter pairs of a request, as the borrowed pairs the wire helper takes.
fn borrowed(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// Create the launch template the group is built on — Amazon EC2's own resource — and the
/// group itself, at `initial` capacity inside `bounds`, with the processes `suspended` names
/// suspended.
///
/// The group is created at the bottom of its range and brought to `initial` after that
/// suspension, and a `MinSize` the row asks for is raised onto it afterwards — which is why a
/// group whose range does not start at zero is created at zero and updated.
async fn group_with(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    bounds: (i32, i32),
    initial: i32,
    suspended: &[&str],
) {
    let template = format!("{name}-template");
    let (status, created) = form_call(
        router,
        "ec2",
        EC2_VERSION,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template.as_str()),
            ("LaunchTemplateData.ImageId", "ami-0123456789abcdef0"),
            ("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon EC2 must create the launch template, got {status} with {created}"
    );
    assert!(
        opt_element(&created, "launchTemplateId").is_some(),
        "case {case:?}: Amazon EC2 must report the template it created, got {created}"
    );

    let (min_size, max_size) = bounds;
    let create = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", "0"),
        p("DefaultCooldown", "0"),
        p("AvailabilityZones.member.1", format!("{REGION}a")),
    ];
    asg(router, case, "CreateAutoScalingGroup", &borrowed(&create)).await;

    if !suspended.is_empty() {
        let mut suspend = vec![p("AutoScalingGroupName", name)];
        for (index, process) in suspended.iter().enumerate() {
            suspend.push(p(
                &format!("ScalingProcesses.member.{}", index + 1),
                (*process).to_string(),
            ));
        }
        asg(router, case, "SuspendProcesses", &borrowed(&suspend)).await;
    }

    if initial != 0 {
        let set = vec![
            p("AutoScalingGroupName", name),
            p("DesiredCapacity", initial.to_string()),
        ];
        asg(router, case, "SetDesiredCapacity", &borrowed(&set)).await;
    }
    if min_size != 0 {
        let update = vec![
            p("AutoScalingGroupName", name),
            p("MinSize", min_size.to_string()),
        ];
        asg(router, case, "UpdateAutoScalingGroup", &borrowed(&update)).await;
    }
    let reported = desired_capacity(router, case, name).await;
    assert_eq!(
        reported, initial,
        "case {case:?}: the group must start at {initial}, got {reported}"
    );
}

/// What a target-tracking put states about the series it tracks and the target it holds it at.
struct Tracked<'a> {
    /// The dimension value that tells this policy's series from every other row's.
    dimension: &'a str,
    /// `TargetTrackingConfiguration$TargetValue`.
    target_value: f64,
    /// `CustomizedMetricSpecification$Period`, the aggregation period its alarms grade on.
    period: i32,
    /// `TargetTrackingConfiguration$DisableScaleIn`.
    disable_scale_in: bool,
    /// `PutScalingPolicyType$Enabled`.
    enabled: bool,
}

/// Put one target-tracking policy on `group` and answer its `PolicyARN` and the names of the
/// alarms the service minted for it.
async fn put_tracking_policy(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    policy: &str,
    tracked: &Tracked<'_>,
) -> (String, Vec<String>) {
    let spec = "TargetTrackingConfiguration.CustomizedMetricSpecification";
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", policy),
        p("PolicyType", "TargetTrackingScaling"),
        p(&format!("{spec}.Namespace"), NAMESPACE),
        p(&format!("{spec}.MetricName"), METRIC),
        p(&format!("{spec}.Statistic"), "Average"),
        p(&format!("{spec}.Period"), tracked.period.to_string()),
        p(&format!("{spec}.Dimensions.member.1.Name"), "Series"),
        p(
            &format!("{spec}.Dimensions.member.1.Value"),
            tracked.dimension,
        ),
        p(
            "TargetTrackingConfiguration.TargetValue",
            format!("{}", tracked.target_value),
        ),
    ];
    if tracked.disable_scale_in {
        params.push(p("TargetTrackingConfiguration.DisableScaleIn", "true"));
    }
    if !tracked.enabled {
        params.push(p("Enabled", "false"));
    }
    let body = asg(router, case, "PutScalingPolicy", &borrowed(&params)).await;
    let arn = element(&body, "PolicyARN");
    assert!(
        arn.contains(":scalingPolicy:"),
        "case {case:?}: PutScalingPolicy must report the policy's ARN, got {body}"
    );
    let alarms = elements(&body, "AlarmName");
    assert!(
        !alarms.is_empty(),
        "case {case:?}: a target-tracking put must report the alarms it minted, got {body}"
    );
    (arn, alarms)
}

/// The name of the policy's alarm whose kind segment is `segment` (`AlarmHigh`, `AlarmLow`).
fn alarm_of_kind(case: &str, alarms: &[String], segment: &str) -> String {
    alarms
        .iter()
        .find(|name| name.contains(segment))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the policy must hold an {segment} alarm, got {alarms:?}")
        })
        .clone()
}

/// The `DesiredCapacity` the group reports right now.
async fn desired_capacity(router: &MultiServiceRouter, case: &str, group: &str) -> i32 {
    let body = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    element(&body, "DesiredCapacity")
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: DesiredCapacity must be a number, got {body}"))
}

/// The `MinSize` and `MaxSize` the group reports right now, which a policy never changes.
async fn bounds(router: &MultiServiceRouter, case: &str, group: &str) -> (String, String) {
    let body = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    (element(&body, "MinSize"), element(&body, "MaxSize"))
}

/// The instance ids the group reports as its members right now.
async fn instance_ids(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let body = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    elements(&body, "InstanceId")
}

/// The causes of the scaling activities the group has recorded.
async fn activity_causes(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let body = asg(
        router,
        case,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    elements(&body, "Cause")
}

/// Move the alarm `name` into ALARM carrying `graded` as the values its decision rests on,
/// which is the transition that executes the actions configured for that state: "When the
/// updated state differs from the previous value, the action configured for the appropriate
/// state is invoked" (`SetAlarmState`).
async fn force_alarm(router: &MultiServiceRouter, case: &str, name: &str, graded: f64) {
    cw(
        router,
        case,
        REGION,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": "ALARM",
            "StateReason": "driven by this suite",
            "StateReasonData": json!({"recentDatapoints": [graded]}).to_string(),
        }),
    )
    .await;
}

/// Wait until CloudWatch has recorded an `Action` history item naming `action` for the alarm
/// `name`.
///
/// The hand-over to the scaling service is queued by the same pass that records the item, so an
/// item a reader can see means the transition has been handed over: every assertion about what
/// Auto Scaling then did is made after this.
async fn recorded_action(router: &MultiServiceRouter, case: &str, alarm: &str, action: &str) {
    for _ in 0..LOOKS {
        let answer = cw(
            router,
            case,
            REGION,
            "DescribeAlarmHistory",
            &json!({"AlarmName": alarm, "HistoryItemType": "Action"}),
        )
        .await;
        let items = answer
            .get("AlarmHistoryItems")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.iter().any(|item| {
            item.get("HistoryData")
                .and_then(Value::as_str)
                .is_some_and(|data| data.contains(action))
        }) {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: CloudWatch recorded no Action item naming {action} for {alarm}");
}

/// Wait until the group reports `wanted` as its `DesiredCapacity`, and answer what it reports.
async fn settled_capacity(router: &MultiServiceRouter, case: &str, group: &str, wanted: i32) -> i32 {
    let mut reported = desired_capacity(router, case, group).await;
    for _ in 0..LOOKS {
        if reported == wanted {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
        reported = desired_capacity(router, case, group).await;
    }
    reported
}

/// Require that the group's capacity STAYS at `held` over repeated reads, each of which is a
/// chance for a hand-over to move it.
async fn assert_capacity_held(router: &MultiServiceRouter, case: &str, group: &str, held: i32) {
    for _ in 0..HELD_LOOKS {
        let reported = desired_capacity(router, case, group).await;
        assert_eq!(
            reported, held,
            "case {case:?}: expected the capacity to stay at {held}, got {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the table: what the policy tracks, what its alarm graded, and where the group
/// ends up.
struct Row<'a> {
    /// The case every assertion names.
    case: &'a str,
    /// The group's `MinSize` and `MaxSize`, which a policy may not change.
    bounds: (i32, i32),
    /// The `DesiredCapacity` the group starts at.
    initial: i32,
    /// `TargetValue`, and the value the alarm that fires graded.
    target_value: f64,
    graded: f64,
    /// The aggregation period the policy's metric is graded on.
    period: i32,
    /// Which of the policy's alarms hands the value over.
    alarm_segment: &'a str,
    /// Whether the policy may remove an instance, and whether it is enabled at all.
    disable_scale_in: bool,
    enabled: bool,
    /// The `DesiredCapacity` the group holds once the alarm has fired.
    expected: i32,
}

#[tokio::test]
async fn a_target_tracking_alarm_moves_the_capacity_to_the_tracked_ratio() {
    let rows: &[Row<'_>] = &[
        Row {
            // ceil(1 × 150 / 50) = 3.
            case: "a value at three times the target scales out to the ratio",
            bounds: (0, 4),
            initial: 1,
            target_value: 50.0,
            graded: 150.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: true,
            expected: 3,
        },
        Row {
            // ceil(2 × 55 / 50) = ceil(2.2) = 3: a capacity below the quotient would leave
            // the metric above the target, so the fraction is taken UP.
            case: "a value just above the target takes the next whole instance",
            bounds: (0, 6),
            initial: 2,
            target_value: 50.0,
            graded: 55.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: true,
            expected: 3,
        },
        Row {
            // ceil(2 × 50 / 50) = 2.
            case: "a value at the target leaves the capacity where it is",
            bounds: (0, 6),
            initial: 2,
            target_value: 50.0,
            graded: 50.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: true,
            expected: 2,
        },
        Row {
            // ceil(4 × 25 / 50) = 2, handed over by the alarm that watches for a value under
            // the target.
            case: "a value at half the target scales in to the ratio",
            bounds: (0, 6),
            initial: 4,
            target_value: 50.0,
            graded: 25.0,
            period: 60,
            alarm_segment: "AlarmLow",
            disable_scale_in: false,
            enabled: true,
            expected: 2,
        },
        Row {
            // ceil(2 × 0.6 / 0.2) = 6, over a target that is a fraction and a metric graded on
            // a high-resolution period.
            case: "a fractional target on a high-resolution period tracks the same ratio",
            bounds: (0, 8),
            initial: 2,
            target_value: 0.2,
            graded: 0.6,
            period: 10,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: true,
            expected: 6,
        },
        Row {
            // ceil(1 × 500 / 50) = 10, held at the group's own ceiling.
            case: "a ratio above MaxSize is held at the bound",
            bounds: (0, 3),
            initial: 1,
            target_value: 50.0,
            graded: 500.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: true,
            expected: 3,
        },
        Row {
            // ceil(4 × 5 / 50) = 1, held at the group's own floor.
            case: "a ratio below MinSize is held at the bound",
            bounds: (2, 6),
            initial: 4,
            target_value: 50.0,
            graded: 5.0,
            period: 60,
            alarm_segment: "AlarmLow",
            disable_scale_in: false,
            enabled: true,
            expected: 2,
        },
        Row {
            // The ratio asks for 2 of the 4 the group holds, and a policy that cannot remove
            // an instance does not move the capacity down for it.
            case: "a scale-in the ratio asks for is withheld by DisableScaleIn",
            bounds: (0, 6),
            initial: 4,
            target_value: 50.0,
            graded: 25.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: true,
            enabled: true,
            expected: 4,
        },
        Row {
            case: "a disabled policy is not performed at all",
            bounds: (0, 6),
            initial: 1,
            target_value: 50.0,
            graded: 150.0,
            period: 60,
            alarm_segment: "AlarmHigh",
            disable_scale_in: false,
            enabled: false,
            expected: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = router();
        let case = row.case;
        let group = format!("tracking-group-{index}");
        let policy_name = format!("tracking-policy-{index}");
        let dimension = format!("series-{index}");

        // `Launch` is suspended, so each row records the capacity it was moved to without
        // provisioning anything; the launching path is pinned by the published-datapoint test
        // below.
        group_with(&router, case, &group, row.bounds, row.initial, &["Launch"]).await;
        let (policy, alarms) = put_tracking_policy(
            &router,
            case,
            &group,
            &policy_name,
            &Tracked {
                dimension: &dimension,
                target_value: row.target_value,
                period: row.period,
                disable_scale_in: row.disable_scale_in,
                enabled: row.enabled,
            },
        )
        .await;
        for alarm in &alarms {
            assert!(
                alarm.contains(&group),
                "case {case:?}: the policy's alarms name the group they scale, got {alarm}"
            );
        }

        let alarm = alarm_of_kind(case, &alarms, row.alarm_segment);
        force_alarm(&router, case, &alarm, row.graded).await;

        // CloudWatch records the action it executed whatever Auto Scaling then decides, so
        // this holds for the rows that move nothing as well.
        recorded_action(&router, case, &alarm, &policy).await;

        if row.expected == row.initial {
            assert_capacity_held(&router, case, &group, row.initial).await;
        } else {
            let reported = settled_capacity(&router, case, &group, row.expected).await;
            assert_eq!(
                reported, row.expected,
                "case {case:?}: expected DesiredCapacity {}, got {reported}",
                row.expected
            );
        }

        // A policy that is not performed at all performs nothing: beside the capacity it left
        // alone, the group records no scaling activity naming the policy — which a group whose
        // capacity moved and was put back would. The activity an alarm-driven change DOES
        // record is pinned by the published-datapoint test below, over a group that provisions.
        if !row.enabled {
            let causes = activity_causes(&router, case, &group).await;
            assert!(
                !causes.iter().any(|cause| cause.contains(&policy_name)),
                "case {case:?}: a policy that is not performed leaves no activity naming it, \
                 got {causes:?}"
            );
        }

        // The bounds are the group's own: a policy moves the capacity between them and never
        // moves them.
        let (min_size, max_size) = bounds(&router, case, &group).await;
        assert_eq!(
            (min_size.as_str(), max_size.as_str()),
            (
                row.bounds.0.to_string().as_str(),
                row.bounds.1.to_string().as_str()
            ),
            "case {case:?}: MinSize/MaxSize must be left alone, got {min_size}/{max_size}"
        );
    }
}

/// The instant, in epoch seconds, this run is at.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Publish `value` for the series `dimension` over the last `periods` whole minutes, which is
/// the range an alarm with a minute period grades.
///
/// A time stamp is what decides which period a datapoint falls in — "If you do not specify a
/// time stamp, the time stamp of the data point is the time that `PutMetricData` was called"
/// (`MetricDatum$Timestamp`) — so one datapoint per minute of the range fills every window the
/// evaluation grades, however fast this run reaches the poll below.
async fn publish_over(
    router: &MultiServiceRouter,
    case: &str,
    dimension: &str,
    value: f64,
    periods: u32,
) {
    let now = now_secs();
    for period in 0..periods {
        cw(
            router,
            case,
            REGION,
            "PutMetricData",
            &json!({
                "Namespace": NAMESPACE,
                "MetricData": [{
                    "MetricName": METRIC,
                    "Dimensions": [{"Name": "Series", "Value": dimension}],
                    "Timestamp": now - f64::from(period) * 60.0,
                    "Value": value,
                }],
            }),
        )
        .await;
    }
}

/// Wait until `group` reports `wanted`, polling on the published-datapoint deadline.
async fn published_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: i32,
) -> i32 {
    let mut reported = desired_capacity(router, case, group).await;
    for _ in 0..PUBLISHED_LOOKS {
        if reported == wanted {
            return reported;
        }
        tokio::time::sleep(PUBLISHED_LOOK_AGAIN).await;
        reported = desired_capacity(router, case, group).await;
    }
    reported
}

#[tokio::test]
async fn published_datapoints_above_the_target_scale_the_group_and_launch_its_instances() {
    let case = "two identical groups, one fed three times its target and one fed the target";
    let router = router();

    // Two groups configured identically, each with its own target-tracking policy over its own
    // series, differing only in the value published into that series. Nothing is suspended, so
    // the capacity the policy asks for is converged on: the group launches the account's own
    // Amazon EC2 instances for it.
    let mut wiring = Vec::new();
    for (group, dimension, value, expected) in [
        ("tracking-subject", "subject", 150.0, 3),
        ("tracking-control", "control", 50.0, 1),
    ] {
        group_with(&router, case, group, (1, 4), 1, &[]).await;
        let (policy, alarms) = put_tracking_policy(
            &router,
            case,
            group,
            &format!("{group}-policy"),
            &Tracked {
                dimension,
                target_value: 50.0,
                period: 60,
                disable_scale_in: false,
                enabled: true,
            },
        )
        .await;
        publish_over(&router, case, dimension, value, 8).await;
        wiring.push((group, policy, alarms, expected));
    }

    // The subject's own alarm grades the series it was fed and hands the policy the value:
    // nothing is forced here, and the capacity follows from the datapoints alone.
    let (subject, subject_policy, subject_alarms, _) = &wiring[0];
    let subject_high = alarm_of_kind(case, subject_alarms, "AlarmHigh");
    recorded_action(&router, case, &subject_high, subject_policy).await;

    let reported = published_capacity(&router, case, subject, 3).await;
    assert_eq!(
        reported, 3,
        "case {case:?}: a series at three times the target moves the capacity to the ratio, got \
         {reported}"
    );
    let instances = instance_ids(&router, case, subject).await;
    assert_eq!(
        instances.len(),
        3,
        "case {case:?}: the group launches the capacity it was moved to, got {instances:?}"
    );
    let causes = activity_causes(&router, case, subject).await;
    assert!(
        causes
            .iter()
            .any(|cause| cause.contains("-policy") && cause.contains(&subject_high)),
        "case {case:?}: an activity must name the alarm that drove the policy, got {causes:?}"
    );

    // The control is fed exactly its target, so neither of its alarms breaches and its capacity
    // is left alone — which is what tells a scale-out from a group that would move anyway.
    let (control, _, _, _) = &wiring[1];
    assert_capacity_held(&router, case, control, 1).await;
}
