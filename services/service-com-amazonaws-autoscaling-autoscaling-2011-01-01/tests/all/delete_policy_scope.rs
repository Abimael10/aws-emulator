// SPDX-License-Identifier: CC0-1.0

//! What `DeletePolicy` removes, and what it leaves alone.
//!
//! "Deletes the specified scaling policy" (`DeletePolicy`) is a resolve-then-remove of ONE
//! stored policy: the one the request names, by its name inside the group it names or by the
//! ARN that names it wherever it lives ("The name or Amazon Resource Name (ARN) of the
//! policy", `DeletePolicyType$PolicyName`). The rules this suite pins, which
//! `delete_policy.rs` states for the policy and the alarms it holds, are the ones about the
//! rest of the account:
//!
//! 1. Exactly the resolved policy is removed. Another policy of the same group, and a policy
//!    of the SAME NAME on another group, stay as they were, and so do both groups.
//! 2. A policy that is no longer stored resolves nothing, so the SECOND delete of the same
//!    reference is refused with HTTP 400 `ValidationError` and the service's absent-policy
//!    sentence naming the reference the request carried — a delete is not a no-op for a
//!    policy that is gone.
//! 3. Removal frees the name: a put of that name afterwards creates the policy again and
//!    `DescribePolicies` reports it beside the one that was kept.
//!
//! The table spans what the rules generalise over: the policy's TYPE — every type
//! `PutScalingPolicyType$PolicyType` names ("TargetTrackingScaling StepScaling SimpleScaling
//! (default) PredictiveScaling") — crossed with the two spellings a request may name it by.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;

use crate::support::{borrowed, new_engine, p, query, tag_value, tag_values, xml_unescape};

/// The launch source every group in this suite is created from — a launch TEMPLATE, because
/// no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// A router carrying Amazon EC2 Auto Scaling and CloudWatch on ONE engine: a target-tracking
/// policy IS a pair of CloudWatch alarms, so the service that owns them is mounted beside
/// this one for the rows whose policy mints them.
fn router() -> MultiServiceRouter {
    let engine = new_engine();
    crate::support::seed_launch_templates(&engine, &[LAUNCH_SOURCE.1]);
    let mut router = MultiServiceRouter::new();
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Create one empty group — nothing to launch, so nothing settles on the clock while the
/// policies are written, deleted and read.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body}"
    );
}

/// Put one scaling policy and answer the ARN the service minted for it.
async fn put_policy(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    policy: &str,
    members: &[(String, String)],
) -> String {
    let mut params = vec![p("AutoScalingGroupName", group), p("PolicyName", policy)];
    params.extend(members.iter().cloned());
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the policy {policy:?} of {group:?} must be put, got {status} with {body}"
    );
    let arn = tag_value(&body, "PolicyARN");
    assert!(
        !arn.is_empty(),
        "case {case:?}: PutScalingPolicy must report the policy's ARN, got {body}"
    );
    arn
}

/// The policy names Amazon EC2 Auto Scaling reports for `group` right now, sorted so an
/// assertion is about the SET the group holds rather than the order one read answered in.
async fn held_policies(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribePolicies for {group:?} expected 200, got {status}: {body}"
    );
    let mut names = tag_values(&body, "PolicyName");
    names.sort();
    names
}

/// Whether the account still holds the group `group`, read through its own describe.
async fn group_is_held(router: &MultiServiceRouter, case: &str, group: &str) -> bool {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {group:?} expected 200, got {status}: \
         {body}"
    );
    tag_values(&body, "AutoScalingGroupName").contains(&group.to_string())
}

/// The members of a `SimpleScaling` put, the type `PutScalingPolicyType$PolicyType` calls
/// the default.
fn simple_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("ScalingAdjustment", "1"),
    ]
}

/// The members of a `StepScaling` put, with one open-ended step.
fn step_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "StepScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("StepAdjustments.member.1.MetricIntervalLowerBound", "0"),
        p("StepAdjustments.member.1.ScalingAdjustment", "1"),
    ]
}

/// The members of a `TargetTrackingScaling` put, the one type that holds alarms.
fn target_tracking_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "TargetTrackingScaling"),
        p("TargetTrackingConfiguration.TargetValue", "40"),
        p(
            "TargetTrackingConfiguration.PredefinedMetricSpecification.PredefinedMetricType",
            "ASGAverageCPUUtilization",
        ),
    ]
}

/// The members of a `PredictiveScaling` put, on a predefined metric pair.
fn predictive_policy() -> Vec<(String, String)> {
    vec![
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
    ]
}

/// How the delete names the policy it removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// By its name, inside the group the request names beside it.
    NameInGroup,
    /// By the ARN the service minted, which names it with no group beside it.
    ArnAlone,
}

/// How one case builds the members its put carries beyond the two names.
type Members = fn() -> Vec<(String, String)>;

/// One row: the policy's type, and the spelling the delete names it by.
struct Row {
    case: String,
    members: Members,
    named: Named,
}

/// The name of the policy each row deletes, held by BOTH groups of that row.
const DOOMED: &str = "doomed";

/// The name of the policy of the same group that must survive the delete.
const KEPT: &str = "kept";

/// The service's own absent-policy sentence, written out here rather than taken from the
/// implementation's builder — a test re-using that literal would assert nothing. Its only
/// variable part is the reference the request carried.
fn absent_policy_message(reference: &str) -> String {
    format!("Policy {reference} not found")
}

/// Every row of the table: each policy type, named each way.
fn rows() -> Vec<Row> {
    let types: [(&'static str, Members); 4] = [
        ("a simple-scaling policy", simple_policy),
        ("a step-scaling policy", step_policy),
        ("a target-tracking policy", target_tracking_policy),
        ("a predictive-scaling policy", predictive_policy),
    ];
    let spellings = [
        ("named by its name in its group", Named::NameInGroup),
        ("named by its ARN alone", Named::ArnAlone),
    ];
    let mut rows = Vec::new();
    for (policy_case, members) in types {
        for (spelling, named) in spellings {
            rows.push(Row {
                case: format!("{policy_case} {spelling}"),
                members,
                named,
            });
        }
    }
    rows
}

/// Rules 1, 2 and 3, for every policy type and both spellings.
#[tokio::test]
async fn a_delete_removes_one_policy_refuses_a_repeat_and_frees_the_name() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case.as_str();
        let router = router();
        let group = format!("scoped-{index}");
        let other = format!("other-{index}");
        create_group(&router, &group).await;
        create_group(&router, &other).await;

        let arn = put_policy(&router, case, &group, DOOMED, &(row.members)()).await;
        put_policy(&router, case, &group, KEPT, &simple_policy()).await;
        // The SAME policy name on another group: a delete resolves inside the group it is
        // about, so this one must still be there afterwards.
        put_policy(&router, case, &other, DOOMED, &simple_policy()).await;

        let reference = match row.named {
            Named::NameInGroup => DOOMED.to_string(),
            Named::ArnAlone => arn.clone(),
        };
        let delete_params: Vec<(String, String)> = match row.named {
            Named::NameInGroup => vec![
                p("AutoScalingGroupName", group.clone()),
                p("PolicyName", reference.clone()),
            ],
            Named::ArnAlone => vec![p("PolicyName", reference.clone())],
        };

        // RULE 1: the delete is served, and removes exactly the policy it resolved.
        let (status, body) = query(&router, "DeletePolicy", &borrowed(&delete_params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeletePolicy expected 200, got {status}: {body}"
        );
        let held = held_policies(&router, case, &group).await;
        assert_eq!(
            held,
            vec![KEPT.to_string()],
            "case {case:?}: expected only {KEPT:?} left on {group:?}, got {held:?}"
        );
        let held_elsewhere = held_policies(&router, case, &other).await;
        assert_eq!(
            held_elsewhere,
            vec![DOOMED.to_string()],
            "case {case:?}: expected the same-named policy of {other:?} untouched, got \
             {held_elsewhere:?}"
        );
        for name in [&group, &other] {
            assert!(
                group_is_held(&router, case, name).await,
                "case {case:?}: deleting a policy must leave the group {name:?} where it was"
            );
        }

        // RULE 2: the same reference now resolves nothing, so the repeat is refused.
        let (status, body) = query(&router, "DeletePolicy", &borrowed(&delete_params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a repeated DeletePolicy expected 400, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {case:?}: expected the ValidationError code on the repeat, got {body}"
        );
        let answered = xml_unescape(&tag_value(&body, "Message"));
        let expected = absent_policy_message(&reference);
        assert_eq!(
            answered, expected,
            "case {case:?}: expected the message {expected:?} on the repeat, got {answered:?}"
        );
        let held = held_policies(&router, case, &group).await;
        assert_eq!(
            held,
            vec![KEPT.to_string()],
            "case {case:?}: a refused repeat removes nothing more, got {held:?}"
        );

        // RULE 3: the name is free again.
        let recreated = put_policy(&router, case, &group, DOOMED, &(row.members)()).await;
        assert!(
            !recreated.is_empty(),
            "case {case:?}: the re-created policy must carry an ARN"
        );
        let held = held_policies(&router, case, &group).await;
        assert_eq!(
            held,
            vec![DOOMED.to_string(), KEPT.to_string()],
            "case {case:?}: expected the re-created policy reported beside {KEPT:?}, got {held:?}"
        );
    }
}
