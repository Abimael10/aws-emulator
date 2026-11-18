// SPDX-License-Identifier: CC0-1.0

//! `ExecutePolicy`: performing a scaling policy of an Auto Scaling group, over the
//! awsQuery wire.
//!
//! The rules under test:
//!
//! 1. Performing a policy applies THE POLICY'S OWN ADJUSTMENT to the capacity the group
//!    maintains: `AdjustmentType`'s "valid values are ChangeInCapacity, ExactCapacity, and
//!    PercentChangeInCapacity" (`AdjustmentType$AdjustmentType`) say how the number it
//!    carries is read, a percentage is taken toward zero but never to nothing and
//!    `MinAdjustmentMagnitude` raises that floor, and the resulting capacity is held inside
//!    the group's own `MinSize`..`MaxSize`. A `StepScaling` policy's adjustment is the step
//!    whose interval holds the difference between the request's `MetricValue` and its
//!    `BreachThreshold`, with the lower bound inclusive above the threshold and the upper
//!    bound inclusive below it (`StepAdjustment`).
//! 2. The group then CONVERGES on that capacity the way it does for every other change of
//!    its size: it launches the account's own Amazon EC2 instances to match, and the
//!    scaling activity it records names the policy that asked for the change.
//! 3. "The name or ARN of the policy" (`ExecutePolicyType$PolicyName`): either spelling
//!    names the same policy, and a name nothing answers to — or a group the account does
//!    not hold — is refused with HTTP 400 `ValidationError`, leaving the group as it was.
//! 4. The two cooldowns the model states: `HonorCooldown` makes a `SimpleScaling` policy
//!    "wait for the cooldown period to complete before executing the policy", so the change
//!    falls due at that cooldown's end instead of being applied at once; and the policy's
//!    own `Cooldown` "overrides the default cooldown period defined for the Auto Scaling
//!    group" for the cooldown the performed policy leaves behind.
//! 5. A policy type that states no adjustment of its own cannot be performed, and the two
//!    metric members are "Required if the policy type is StepScaling and not supported
//!    otherwise" (`ExecutePolicyType$MetricValue`).
//!
//! Each table spans what its rule generalises over — the adjustment type, the sign and
//! magnitude of the adjustment, which bound the result is held at, where the metric value
//! falls among the steps, the spelling of the policy and the policy's type — rather than
//! one recorded row, and every assertion names its case and the value actually reported.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    activity_causes, borrowed, ec2_query, emulator, p, query, tag_value, tag_values, REGION,
};

/// The launch source every group in this suite is created from — a launch TEMPLATE,
/// because no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`). Seeded into the account's own Amazon EC2
/// store by the shared harness.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// How long a clock-owed condition is waited for before the wait becomes a failure.
///
/// A BACKSTOP, not a schedule: every wait below polls for the CONDITION it needs, and this
/// bound only turns something that never happens into a failing test rather than a hang.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The Availability Zone the groups of this suite place their instances in: the Region's
/// FIRST zone, read from Amazon EC2's own catalog rather than written out here.
fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .expect("the Region has Availability Zones")
        .zone_name
        .clone()
}

/// Create one group and assert the create succeeded.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    sizes: (i32, i32, i32),
    default_cooldown: Option<i32>,
) {
    let (min_size, max_size, desired_capacity) = sizes;
    let zone = zone();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", min_size.to_string()),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired_capacity.to_string()),
        p("AvailabilityZones.member.1", zone),
    ];
    if let Some(cooldown) = default_cooldown {
        params.push(p("DefaultCooldown", cooldown.to_string()));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body}"
    );
}

/// Put one scaling policy and answer the `PolicyARN` the service minted for it.
async fn put_policy(
    router: &MultiServiceRouter,
    group: &str,
    policy: &str,
    members: &[(String, String)],
) -> String {
    let mut params = vec![p("AutoScalingGroupName", group), p("PolicyName", policy)];
    params.extend(members.iter().cloned());
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the policy {policy:?} must be put, got {status} with {body}"
    );
    let arn = tag_value(&body, "PolicyARN");
    assert!(
        !arn.is_empty(),
        "PutScalingPolicy must report the policy's ARN, got {body}"
    );
    arn
}

/// POST one `ExecutePolicy` and answer its status and body.
async fn execute_policy(
    router: &MultiServiceRouter,
    members: &[(String, String)],
) -> (u16, String) {
    query(router, "ExecutePolicy", &borrowed(members)).await
}

/// The `DesiredCapacity` the group reports right now.
async fn desired_capacity(router: &MultiServiceRouter, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    tag_value(&body, "DesiredCapacity")
}

/// The instance ids and lifecycle states the group reports right now.
async fn group_instances(router: &MultiServiceRouter, group: &str) -> (Vec<String>, Vec<String>) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    (
        tag_values(&body, "InstanceId"),
        tag_values(&body, "LifecycleState"),
    )
}

// The causes of the scaling activities the group has recorded, newest first, come from the
// harness's own reader (`crate::support::activity_causes`), so this suite and the others
// that assert on a cause share one copy of that read.

/// Wait until the group reports `wanted` as its `DesiredCapacity`, or the deadline passes;
/// answer what it last reported.
async fn wait_for_desired_capacity(
    router: &MultiServiceRouter,
    group: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = desired_capacity(router, group).await;
        if held == wanted || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until the group holds `wanted` instances, every one of them `InService`, or the
/// deadline passes; answer the ids and states it last reported.
async fn wait_for_instances(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (ids, states) = group_instances(router, group).await;
        let settled = ids.len() == wanted
            && states.len() == wanted
            && states
                .iter()
                .all(|state| state == autoscaling_in_service_state());
        if settled || Instant::now() >= deadline {
            return (ids, states);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `LifecycleState` an instance of a group in service is reported in, taken from the
/// service's own statement of it rather than written out here.
fn autoscaling_in_service_state() -> &'static str {
    service_com_amazonaws_autoscaling_autoscaling_2011_01_01::state::LIFECYCLE_IN_SERVICE
}

/// The members of a `SimpleScaling` put: one adjustment, read by `AdjustmentType`.
fn simple_policy(adjustment_type: &str, scaling_adjustment: i32) -> Vec<(String, String)> {
    vec![
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", adjustment_type),
        p("ScalingAdjustment", scaling_adjustment.to_string()),
    ]
}

/// The members of a `StepScaling` put whose steps are `(lower bound, upper bound,
/// adjustment)` triples; a bound the triple leaves out is unbounded in that direction.
fn step_policy(steps: &[(Option<f64>, Option<f64>, i32)]) -> Vec<(String, String)> {
    let mut params = vec![
        p("PolicyType", "StepScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
    ];
    for (index, (lower, upper, adjustment)) in steps.iter().enumerate() {
        let member = index + 1;
        if let Some(lower) = lower {
            params.push(p(
                &format!("StepAdjustments.member.{member}.MetricIntervalLowerBound"),
                lower.to_string(),
            ));
        }
        if let Some(upper) = upper {
            params.push(p(
                &format!("StepAdjustments.member.{member}.MetricIntervalUpperBound"),
                upper.to_string(),
            ));
        }
        params.push(p(
            &format!("StepAdjustments.member.{member}.ScalingAdjustment"),
            adjustment.to_string(),
        ));
    }
    params
}

/// The two members a step policy is graded by.
fn graded_at(metric_value: f64, breach_threshold: f64) -> Vec<(String, String)> {
    vec![
        p("MetricValue", metric_value.to_string()),
        p("BreachThreshold", breach_threshold.to_string()),
    ]
}

/// One execution: the group it is performed on, what the policy carries, what the request
/// carries, and the capacity the group holds afterwards.
struct CapacityCase<'a> {
    case: &'a str,
    /// `(MinSize, MaxSize, DesiredCapacity)` the group is created with.
    sizes: (i32, i32, i32),
    policy: Vec<(String, String)>,
    request: Vec<(String, String)>,
    expected: i32,
}

/// Rule 1: the adjustment the policy states is what moves the group's capacity, read by
/// its `AdjustmentType`, selected by the metric value for a step policy, and held inside
/// the group's own bounds.
#[tokio::test]
async fn a_performed_policy_applies_its_own_adjustment() {
    let (router, _engine) = emulator();

    let cases = [
        CapacityCase {
            case: "ChangeInCapacity adds to the current capacity",
            sizes: (0, 4, 1),
            policy: simple_policy("ChangeInCapacity", 1),
            request: Vec::new(),
            expected: 2,
        },
        CapacityCase {
            case: "a negative ChangeInCapacity removes from it",
            sizes: (0, 4, 2),
            policy: simple_policy("ChangeInCapacity", -1),
            request: Vec::new(),
            expected: 1,
        },
        CapacityCase {
            case: "ExactCapacity IS the capacity",
            sizes: (0, 4, 1),
            policy: simple_policy("ExactCapacity", 3),
            request: Vec::new(),
            expected: 3,
        },
        CapacityCase {
            case: "PercentChangeInCapacity moves it by a percentage of itself",
            sizes: (0, 6, 2),
            policy: simple_policy("PercentChangeInCapacity", 100),
            request: Vec::new(),
            expected: 4,
        },
        CapacityCase {
            case: "a fractional percentage is taken toward zero, never to nothing",
            sizes: (0, 6, 3),
            policy: simple_policy("PercentChangeInCapacity", 25),
            request: Vec::new(),
            expected: 4,
        },
        CapacityCase {
            case: "MinAdjustmentMagnitude raises that floor",
            sizes: (0, 8, 4),
            policy: {
                let mut policy = simple_policy("PercentChangeInCapacity", 25);
                policy.push(p("MinAdjustmentMagnitude", "2"));
                policy
            },
            request: Vec::new(),
            expected: 6,
        },
        CapacityCase {
            case: "a negative percentage scales in",
            sizes: (0, 6, 4),
            policy: simple_policy("PercentChangeInCapacity", -50),
            request: Vec::new(),
            expected: 2,
        },
        CapacityCase {
            case: "a percentage of nothing is nothing",
            sizes: (0, 6, 0),
            policy: simple_policy("PercentChangeInCapacity", 100),
            request: Vec::new(),
            expected: 0,
        },
        CapacityCase {
            case: "an adjustment past the group's maximum stops at it",
            sizes: (0, 3, 1),
            policy: simple_policy("ChangeInCapacity", 10),
            request: Vec::new(),
            expected: 3,
        },
        CapacityCase {
            case: "an adjustment past the group's minimum stops at it",
            sizes: (1, 4, 3),
            policy: simple_policy("ChangeInCapacity", -10),
            request: Vec::new(),
            expected: 1,
        },
        CapacityCase {
            case: "a step policy applies the step the metric value falls in",
            sizes: (0, 8, 2),
            policy: step_policy(&[
                (Some(0.0), Some(10.0), 1),
                (Some(10.0), None, 3),
            ]),
            request: graded_at(59.0, 50.0),
            expected: 3,
        },
        CapacityCase {
            case: "a metric value farther past the threshold falls in the next step",
            sizes: (0, 8, 2),
            policy: step_policy(&[
                (Some(0.0), Some(10.0), 1),
                (Some(10.0), None, 3),
            ]),
            request: graded_at(65.0, 50.0),
            expected: 5,
        },
        CapacityCase {
            case: "at the threshold itself the lower bound includes the metric value",
            sizes: (0, 8, 2),
            policy: step_policy(&[
                (Some(0.0), Some(10.0), 1),
                (Some(10.0), None, 3),
            ]),
            request: graded_at(50.0, 50.0),
            expected: 3,
        },
        CapacityCase {
            case: "below the threshold the upper bound includes the metric value",
            sizes: (0, 8, 3),
            policy: step_policy(&[
                (Some(-10.0), Some(0.0), -1),
                (None, Some(-10.0), -2),
            ]),
            request: graded_at(45.0, 50.0),
            expected: 2,
        },
        CapacityCase {
            case: "farther below the threshold the next step down applies",
            sizes: (0, 8, 3),
            policy: step_policy(&[
                (Some(-10.0), Some(0.0), -1),
                (None, Some(-10.0), -2),
            ]),
            request: graded_at(30.0, 50.0),
            expected: 1,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let group = format!("adjustments-{index}");
        let policy = format!("policy-{index}");
        create_group(&router, &group, case.sizes, None).await;
        put_policy(&router, &group, &policy, &case.policy).await;

        let mut request = vec![
            p("AutoScalingGroupName", group.clone()),
            p("PolicyName", policy.clone()),
        ];
        request.extend(case.request.iter().cloned());
        let (status, body) = execute_policy(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<ExecutePolicyResponse"),
            "case {:?}: expected this operation's own success envelope, got {body}",
            case.case
        );

        let held = desired_capacity(&router, &group).await;
        assert_eq!(
            held,
            case.expected.to_string(),
            "case {:?}: expected DesiredCapacity {}, got {held:?}",
            case.case,
            case.expected
        );
    }
}

/// Rule 2: the group converges on the capacity the policy asked for — it launches the
/// account's own Amazon EC2 instances to match — and the activity it records names the
/// policy.
#[tokio::test]
async fn a_performed_policy_launches_instances_and_records_its_activity() {
    let (router, _engine) = emulator();
    create_group(&router, "converging", (1, 3, 1), None).await;
    let (ids_before, _) = wait_for_instances(&router, "converging", 1).await;
    assert_eq!(
        ids_before.len(),
        1,
        "the baseline instance must reach service, got {ids_before:?}"
    );
    put_policy(
        &router,
        "converging",
        "scale-out",
        &simple_policy("ChangeInCapacity", 1),
    )
    .await;

    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "converging"),
            p("PolicyName", "scale-out"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "ExecutePolicy expected 200, got {status}: {body}"
    );

    let (ids, states) = wait_for_instances(&router, "converging", 2).await;
    assert_eq!(
        ids.len(),
        2,
        "the group must hold the instance the policy asked for, got {ids:?} in {states:?}"
    );
    assert!(
        states
            .iter()
            .all(|state| state == autoscaling_in_service_state()),
        "every instance of the group must reach service, got {states:?}"
    );

    // Every id the group reports names an instance of the account, so Amazon EC2's own
    // read resolves it.
    for instance_id in &ids {
        let (status, body) = ec2_query(&router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
        assert_eq!(
            status, 200,
            "Amazon EC2 must resolve {instance_id:?}, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "instanceId"),
            *instance_id,
            "Amazon EC2 must report {instance_id:?}, got {body}"
        );
    }

    let causes = activity_causes(&router, "converging").await;
    assert!(
        causes
            .iter()
            .any(|cause| cause.contains("executed policy scale-out")
                && cause.contains("from 1 to 2")),
        "a scaling activity must name the policy that asked for the change and the \
         capacities it moved between, got {causes:?}"
    );
}

/// One spelling of the policy a request names.
struct NamingCase<'a> {
    case: &'a str,
    /// Whether the request carries `AutoScalingGroupName`.
    names_group: bool,
    /// Whether the request names the policy by its ARN rather than by its name.
    by_arn: bool,
}

/// Rule 3: "The name or ARN of the policy" — either spelling names the same policy, with
/// or without the group beside it.
#[tokio::test]
async fn a_policy_is_named_by_name_or_by_arn() {
    let cases = [
        NamingCase {
            case: "by name, inside the group the request names",
            names_group: true,
            by_arn: false,
        },
        NamingCase {
            case: "by ARN, with the group named too",
            names_group: true,
            by_arn: true,
        },
        NamingCase {
            case: "by ARN alone",
            names_group: false,
            by_arn: true,
        },
        NamingCase {
            case: "by name alone, which only one group holds",
            names_group: false,
            by_arn: false,
        },
    ];

    for case in &cases {
        // One emulator per case, so a policy named without its group is resolved against a
        // store holding exactly the policies this case put there.
        let (router, _engine) = emulator();
        create_group(&router, "named", (0, 4, 1), None).await;
        let arn = put_policy(
            &router,
            "named",
            "spelled",
            &simple_policy("ChangeInCapacity", 1),
        )
        .await;

        let mut request = vec![p(
            "PolicyName",
            if case.by_arn {
                arn.clone()
            } else {
                "spelled".to_string()
            },
        )];
        if case.names_group {
            request.push(p("AutoScalingGroupName", "named"));
        }
        let (status, body) = execute_policy(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
            case.case
        );
        let held = desired_capacity(&router, "named").await;
        assert_eq!(
            held, "2",
            "case {:?}: expected DesiredCapacity 2, got {held:?}",
            case.case
        );
    }
}

/// One refused execution: what the policy carries, what the request carries, and the
/// sentence the refusal states.
struct RefusalCase<'a> {
    case: &'a str,
    /// The policy to put for this case, with the name to put it under; `None` for a case
    /// that names no stored policy at all.
    policy: Option<(&'a str, Vec<(String, String)>)>,
    request: Vec<(String, String)>,
    message: String,
}

/// Rules 3 and 5: a request that names no policy, or one whose policy cannot be performed
/// from the members it carries, is refused with HTTP 400 `ValidationError` — and the group
/// is left exactly as it was.
#[tokio::test]
async fn an_unperformable_request_is_refused_and_changes_nothing() {
    let (router, _engine) = emulator();
    create_group(&router, "refusing", (0, 4, 1), None).await;

    let target_tracking = vec![
        p("PolicyType", "TargetTrackingScaling"),
        p("TargetTrackingConfiguration.TargetValue", "40"),
        p(
            "TargetTrackingConfiguration.PredefinedMetricSpecification.PredefinedMetricType",
            "ASGAverageCPUUtilization",
        ),
    ];
    let predictive = vec![
        p("PolicyType", "PredictiveScaling"),
        p(
            "PredictiveScalingConfiguration.MetricSpecifications.member.1.TargetValue",
            "40",
        ),
        p(
            "PredictiveScalingConfiguration.MetricSpecifications.member.1\
             .PredefinedMetricPairSpecification.PredefinedMetricType",
            "ASGCPUUtilization",
        ),
    ];

    let cases = [
        RefusalCase {
            case: "a policy name nothing answers to",
            policy: None,
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "no-such-policy"),
            ],
            message: "Policy name not found - no-such-policy".to_string(),
        },
        RefusalCase {
            case: "a group the account does not hold",
            policy: None,
            request: vec![
                p("AutoScalingGroupName", "no-such-group"),
                p("PolicyName", "whatever"),
            ],
            message: "AutoScalingGroup name not found - AutoScalingGroup no-such-group not found"
                .to_string(),
        },
        RefusalCase {
            case: "a target-tracking policy, which states a target rather than an adjustment",
            policy: Some(("tracking", target_tracking)),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "tracking"),
            ],
            message: "ExecutePolicy is not supported for scaling policies of type \
                      TargetTrackingScaling."
                .to_string(),
        },
        RefusalCase {
            case: "a predictive-scaling policy, for the same reason",
            policy: Some(("predicting", predictive)),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "predicting"),
            ],
            message: "ExecutePolicy is not supported for scaling policies of type \
                      PredictiveScaling."
                .to_string(),
        },
        RefusalCase {
            case: "a step policy graded by no metric value",
            policy: Some(("stepping", step_policy(&[(Some(0.0), None, 1)]))),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "stepping"),
            ],
            message: "1 validation error detected: Value null at 'metricValue' failed to \
                      satisfy constraint: Member must not be null"
                .to_string(),
        },
        RefusalCase {
            case: "a step policy graded against no breach threshold",
            policy: Some(("stepping", step_policy(&[(Some(0.0), None, 1)]))),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "stepping"),
                p("MetricValue", "59"),
            ],
            message: "1 validation error detected: Value null at 'breachThreshold' failed to \
                      satisfy constraint: Member must not be null"
                .to_string(),
        },
        RefusalCase {
            case: "a metric value no step of the policy covers",
            policy: Some(("stepping", step_policy(&[(Some(10.0), Some(20.0), 1)]))),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "stepping"),
                p("MetricValue", "51"),
                p("BreachThreshold", "50"),
            ],
            message: "No StepAdjustment of the policy matches the metric value 51 for the \
                      breach threshold 50."
                .to_string(),
        },
        RefusalCase {
            case: "a metric value sent for a simple policy, which does not use one",
            policy: Some(("simple", simple_policy("ChangeInCapacity", 1))),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "simple"),
                p("MetricValue", "59"),
                p("BreachThreshold", "50"),
            ],
            message: "MetricValue is not supported for scaling policies of type SimpleScaling."
                .to_string(),
        },
        RefusalCase {
            case: "a breach threshold sent for a simple policy",
            policy: Some(("simple", simple_policy("ChangeInCapacity", 1))),
            request: vec![
                p("AutoScalingGroupName", "refusing"),
                p("PolicyName", "simple"),
                p("BreachThreshold", "50"),
            ],
            message: "BreachThreshold is not supported for scaling policies of type \
                      SimpleScaling."
                .to_string(),
        },
    ];

    for case in &cases {
        if let Some((name, members)) = case.policy.as_ref() {
            put_policy(&router, "refusing", name, members).await;
        }
        let (status, body) = execute_policy(&router, &case.request).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<Code>ValidationError</Code>"),
            "case {:?}: expected the ValidationError code, got {body}",
            case.case
        );
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, case.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            case.case, case.message
        );
        let held = desired_capacity(&router, "refusing").await;
        assert_eq!(
            held, "1",
            "case {:?}: a refused execution leaves the group's capacity alone, got {held:?}",
            case.case
        );
    }
}

/// Rule 4: `HonorCooldown` waits the group's running cooldown out instead of performing the
/// policy at once, and without it the policy is performed straight away.
#[tokio::test]
async fn honor_cooldown_waits_for_the_groups_cooldown() {
    // A cooldown far longer than this test can run, so "not applied yet" is decided by the
    // behaviour under test rather than by how fast the host is.
    const LONG_COOLDOWN: i32 = 3_600;
    // One a poll loop can wait out.
    const SHORT_COOLDOWN: i32 = 1;

    let (router, _engine) = emulator();
    for (group, cooldown) in [
        ("waiting", LONG_COOLDOWN),
        ("waited-out", SHORT_COOLDOWN),
        ("not-waiting", LONG_COOLDOWN),
        ("stepping", LONG_COOLDOWN),
    ] {
        create_group(&router, group, (0, 4, 0), Some(cooldown)).await;
        put_policy(
            &router,
            group,
            "scale-out",
            &simple_policy("ChangeInCapacity", 1),
        )
        .await;
        // The capacity change that starts the group's cooldown, through the operation that
        // sets a capacity outright.
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[("AutoScalingGroupName", group), ("DesiredCapacity", "1")],
        )
        .await;
        assert_eq!(
            status, 200,
            "the cooldown-starting change on {group:?} expected 200, got {status}: {body}"
        );
    }

    // Honoured while the cooldown runs: the call succeeds and the change is not applied.
    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "waiting"),
            p("PolicyName", "scale-out"),
            p("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
        "honoured during a long cooldown"
    );
    let held = desired_capacity(&router, "waiting").await;
    assert_eq!(
        held, "1",
        "case {:?}: the change waits for the cooldown, so the capacity is still the one the \
         group held, got {held:?}",
        "honoured during a long cooldown"
    );

    // Honoured over a cooldown that does elapse: the change falls due at its end and is
    // applied with no further call.
    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "waited-out"),
            p("PolicyName", "scale-out"),
            p("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
        "honoured over a cooldown that elapses"
    );
    let held = wait_for_desired_capacity(&router, "waited-out", "2").await;
    assert_eq!(
        held, "2",
        "case {:?}: the deferred change must be applied once the cooldown has elapsed, got \
         {held:?}",
        "honoured over a cooldown that elapses"
    );

    // Not honoured: "By default, Amazon EC2 Auto Scaling does not honor the cooldown period
    // during manual scaling activities", so the policy is performed at once.
    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "not-waiting"),
            p("PolicyName", "scale-out"),
            p("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
        "not honoured during a long cooldown"
    );
    let held = desired_capacity(&router, "not-waiting").await;
    assert_eq!(
        held, "2",
        "case {:?}: expected DesiredCapacity 2, got {held:?}",
        "not honoured during a long cooldown"
    );

    // "Valid only if the policy type is SimpleScaling": a STEP policy carries no cooldown to
    // honour, so the member holds its change back for nothing and the policy is performed at
    // once even while the group's own cooldown runs.
    put_policy(
        &router,
        "stepping",
        "step-out",
        &step_policy(&[(Some(0.0), None, 1)]),
    )
    .await;
    let mut request = vec![
        p("AutoScalingGroupName", "stepping"),
        p("PolicyName", "step-out"),
        p("HonorCooldown", "true"),
    ];
    request.extend(graded_at(59.0, 50.0));
    let (status, body) = execute_policy(&router, &request).await;
    assert_eq!(
        status, 200,
        "case {:?}: ExecutePolicy expected 200, got {status}: {body}",
        "honoured on a step policy during a long cooldown"
    );
    let held = desired_capacity(&router, "stepping").await;
    assert_eq!(
        held, "2",
        "case {:?}: a step policy honours no cooldown, so the change is applied at once: \
         expected DesiredCapacity 2, got {held:?}",
        "honoured on a step policy during a long cooldown"
    );
}

/// Rule 4 again, the other cooldown: the cooldown a performed policy LEAVES BEHIND is the
/// policy's own when it states one, and the group's `DefaultCooldown` otherwise — which is
/// observable through a second execution that honours it.
#[tokio::test]
async fn the_policys_own_cooldown_overrides_the_groups_default() {
    let cases = [
        // A policy that states no cooldown leaves the group's own long one in force, so the
        // next honoured execution waits.
        ("the group's default stands", None, "1"),
        // One that states none at all leaves nothing to wait for, so it is applied at once.
        ("the policy's own overrides it", Some(0), "2"),
    ];

    for (case, cooldown, expected) in cases {
        let (router, _engine) = emulator();
        create_group(&router, "cooling", (0, 4, 0), Some(3_600)).await;
        let mut members = simple_policy("ChangeInCapacity", 1);
        if let Some(cooldown) = cooldown {
            members.push(p("Cooldown", cooldown.to_string()));
        }
        put_policy(&router, "cooling", "scale-out", &members).await;

        // The first execution moves the capacity to 1 and leaves a cooldown behind.
        let (status, body) = execute_policy(
            &router,
            &[
                p("AutoScalingGroupName", "cooling"),
                p("PolicyName", "scale-out"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first ExecutePolicy expected 200, got {status}: {body}"
        );
        let held = desired_capacity(&router, "cooling").await;
        assert_eq!(
            held, "1",
            "case {case:?}: the first execution must move the capacity to 1, got {held:?}"
        );

        // The second one honours whatever cooldown the first left behind.
        let (status, body) = execute_policy(
            &router,
            &[
                p("AutoScalingGroupName", "cooling"),
                p("PolicyName", "scale-out"),
                p("HonorCooldown", "true"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second ExecutePolicy expected 200, got {status}: {body}"
        );
        let held = desired_capacity(&router, "cooling").await;
        assert_eq!(
            held, expected,
            "case {case:?}: expected DesiredCapacity {expected}, got {held:?}"
        );
    }
}

/// Rule 4, the two cooldowns TOGETHER: a deferred execution leaves behind the cooldown the
/// POLICY states, exactly as one applied at once does — so once the deferral has been applied
/// a further honoured execution has nothing left to wait for.
///
/// The group's own `DefaultCooldown` is the short one the deferral waits out, and the policy
/// states none at all; if the deferred application fell back to the group's default instead of
/// the policy's own, the second execution below would be deferred too and the capacity it asks
/// for would not be there when the call answers.
#[tokio::test]
async fn a_deferred_execution_leaves_the_policys_own_cooldown_behind() {
    let (router, _engine) = emulator();
    create_group(&router, "deferring", (0, 4, 0), Some(1)).await;
    let mut members = simple_policy("ChangeInCapacity", 1);
    members.push(p("Cooldown", "0"));
    put_policy(&router, "deferring", "scale-out", &members).await;

    // The change that puts the group in its own (short) cooldown.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", "deferring"),
            ("DesiredCapacity", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the cooldown-starting change expected 200, got {status}: {body}"
    );

    // Honoured while that cooldown runs, so the change falls due at its end.
    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "deferring"),
            p("PolicyName", "scale-out"),
            p("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the deferred ExecutePolicy expected 200, got {status}: {body}"
    );
    let held = wait_for_desired_capacity(&router, "deferring", "2").await;
    assert_eq!(
        held, "2",
        "the deferred change must be applied once the cooldown has elapsed, got {held:?}"
    );

    // The policy's own cooldown is what it left behind, so this one is applied at once.
    let (status, body) = execute_policy(
        &router,
        &[
            p("AutoScalingGroupName", "deferring"),
            p("PolicyName", "scale-out"),
            p("HonorCooldown", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the following ExecutePolicy expected 200, got {status}: {body}"
    );
    let held = desired_capacity(&router, "deferring").await;
    assert_eq!(
        held, "3",
        "a deferred execution leaves the policy's own cooldown behind, so the next honoured \
         execution waits for nothing: expected DesiredCapacity 3, got {held:?}"
    );
}
