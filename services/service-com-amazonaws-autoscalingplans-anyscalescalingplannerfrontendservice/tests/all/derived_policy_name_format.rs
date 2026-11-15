// SPDX-License-Identifier: CC0-1.0

//! The FORM of the name a scaling plan gives the target tracking policy it mints.
//!
//! The rule pinned here: a plan-managed target tracking policy is named with the fixed
//! prefix `AutoScaling-`, then the plan's own `ScalingPlanName` verbatim, then `-`, then a
//! UUID the service mints for that policy — one flat token, with no `/` anywhere in it.
//! `ScalingPolicy$PolicyName` constrains only the result (1-256 printable characters,
//! `smithy_query(query="shape",shape="ScalingPolicy")`), so the format itself is RECORDED
//! behaviour; what the model does add is that a `ScalingPlanName` may carry no `/`, `:` or
//! `|` (`smithy_query(query="shape",shape="ScalingPlanName")`: pattern
//! `^[\p{Print}&&[^|:/]]+$`), so a name built this way holds a separator nowhere.
//!
//! Why the form matters rather than the value: the policy name is the handle a client
//! reaches the underlying policy by through Application Auto Scaling (`DescribePolicies`,
//! alarm wiring, IAM resource patterns), and anything written for AWS's format keys off
//! the `AutoScaling-` prefix and parses the rest as one token. The UUID itself is minted
//! per policy and is not asserted beyond its FORM, its stability across reads and its
//! being distinct per policy — [`crate::policy_name_identity`] spans what it is a
//! function of.
//!
//! The harness (router, dispatch, lifecycle clock) is reused from
//! [`crate::scaling_plan_lifecycle`] and the two describe readers from
//! [`crate::applied_policies_and_status_narratives`]; nothing here builds a second one.

use serde_json::{json, Value};

use crate::applied_policies_and_status_narratives::{applied_plan, resources};
use crate::scaling_plan_lifecycle::Emulator;

/// The fixed prefix every derived policy name opens with.
const POLICY_NAME_PREFIX: &str = "AutoScaling-";

/// The documented bounds of `ScalingPolicy$PolicyName`
/// (`smithy_query(query="shape",shape="ScalingPolicy")`: len 1..256, pattern
/// `^\p{Print}+$`).
const MIN_POLICY_NAME: usize = 1;
const MAX_POLICY_NAME: usize = 256;

/// The documented maximum length of a `ScalingPlanName`
/// (`smithy_query(query="shape",shape="ScalingPlanName")`: len 1..128).
const MAX_PLAN_NAME: usize = 128;

/// The four groups of a UUID's `8-4-4-4-12` hexadecimal form, in order.
const UUID_GROUPS: &[usize] = &[8, 4, 4, 4, 12];

/// One target tracking configuration over a predefined metric
/// (`smithy_query(query="shape",shape="ScalingMetricType")` lists the types).
fn predefined(metric: &str, target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": { "PredefinedScalingMetricType": metric },
        "TargetValue": target
    })
}

/// One target tracking configuration over the ALB metric, which carries the resource
/// label that identifies the target group it is measured on.
fn alb(label: &str, target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": {
            "PredefinedScalingMetricType": "ALBRequestCountPerTarget",
            "ResourceLabel": label
        },
        "TargetValue": target
    })
}

/// One target tracking configuration over a customized metric — the other FORM a tracked
/// metric takes.
fn customized(metric_name: &str, target: f64) -> Value {
    json!({
        "CustomizedScalingMetricSpecification": {
            "Namespace": "MyApp",
            "MetricName": metric_name,
            "Statistic": "Sum"
        },
        "TargetValue": target
    })
}

/// An Auto Scaling group instruction over `configurations`.
fn asg(name: &str, configurations: Vec<Value>) -> Value {
    json!({
        "ServiceNamespace": "autoscaling",
        "ResourceId": format!("autoScalingGroup/{name}"),
        "ScalableDimension": "autoscaling:autoScalingGroup:DesiredCapacity",
        "MinCapacity": 1,
        "MaxCapacity": 10,
        "TargetTrackingConfigurations": configurations
    })
}

/// A DynamoDB table instruction over `configurations`.
fn table(configurations: Vec<Value>) -> Value {
    json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": "table/my-table",
        "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
        "MinCapacity": 1,
        "MaxCapacity": 100,
        "TargetTrackingConfigurations": configurations
    })
}

/// Every `PolicyName` the plan `name` reports, in the order they are reported.
async fn policy_names(emulator: &Emulator, name: &str) -> Vec<String> {
    resources(emulator, name)
        .await
        .iter()
        .flat_map(|resource| {
            resource
                .get("ScalingPolicies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|policy| {
                    policy["PolicyName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<String>>()
        })
        .collect()
}

/// Assert that `name` is `AutoScaling-<plan>-<uuid>`: the fixed prefix, the plan name
/// verbatim, `-`, and a UUID in its `8-4-4-4-12` hexadecimal form — and nothing else.
fn assert_derived_name(case: &str, plan: &str, name: &str) {
    let expected_prefix = format!("{POLICY_NAME_PREFIX}{plan}-");
    let id = name.strip_prefix(&expected_prefix).unwrap_or_else(|| {
        panic!(
            "case {case:?}: a derived policy name must open with {expected_prefix:?}, got \
             {name:?}"
        )
    });

    let groups: Vec<&str> = id.split('-').collect();
    assert_eq!(
        groups.iter().map(|group| group.len()).collect::<Vec<usize>>(),
        UUID_GROUPS.to_vec(),
        "case {case:?}: the generated id must be a UUID in its 8-4-4-4-12 form, got \
         {id:?} in {name:?}"
    );
    for group in &groups {
        assert!(
            group
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()),
            "case {case:?}: every group of the UUID is lower-case hexadecimal, got \
             {group:?} in {name:?}"
        );
    }

    // The whole value is ONE flat token: a client that splits a policy name on `/` must
    // find no segment boundary in it, which is also why a `ScalingPlanName` may carry no
    // `/` itself.
    assert!(
        !name.contains('/'),
        "case {case:?}: a derived policy name carries no '/', got {name:?}"
    );

    let length = name.chars().count();
    assert!(
        (MIN_POLICY_NAME..=MAX_POLICY_NAME).contains(&length) && !name.chars().any(char::is_control),
        "case {case:?}: PolicyName must be {MIN_POLICY_NAME}-{MAX_POLICY_NAME} printable \
         characters, got {length} in {name:?}"
    );
}

/// One row: the plan name a policy is derived for, and the instructions that mint the
/// policies.
struct Case {
    label: &'static str,
    plan: String,
    instructions: Vec<Value>,
    policies: usize,
}

/// The table this suite spans. Rows differ along what the rule generalises over: the plan
/// NAME the derivation interpolates (plain, punctuation the modelled pattern admits, a
/// space, one character, the 128-character maximum), the NUMBER of policies a plan mints
/// (one, two on one resource, one on each of two resources), and the FORM of the metric
/// each policy tracks (predefined, predefined with a resource label, customized).
fn cases() -> Vec<Case> {
    let cpu = predefined("ASGAverageCPUUtilization", 40.0);
    vec![
        Case {
            label: "a plain plan name, one policy",
            plan: "my-plan".to_string(),
            instructions: vec![asg("asg-a", vec![cpu.clone()])],
            policies: 1,
        },
        Case {
            label: "punctuation the plan-name pattern admits",
            plan: "my.plan_1-#2".to_string(),
            instructions: vec![asg("asg-a", vec![cpu.clone()])],
            policies: 1,
        },
        Case {
            label: "a plan name holding a space",
            plan: "my plan".to_string(),
            instructions: vec![asg("asg-a", vec![cpu.clone()])],
            policies: 1,
        },
        Case {
            label: "the shortest plan name",
            plan: "p".to_string(),
            instructions: vec![asg("asg-a", vec![cpu.clone()])],
            policies: 1,
        },
        Case {
            label: "the longest plan name, two policies on one resource",
            plan: "p".repeat(MAX_PLAN_NAME),
            instructions: vec![asg(
                "asg-a",
                vec![cpu.clone(), customized("backlog-per-instance", 12.5)],
            )],
            policies: 2,
        },
        Case {
            label: "two resources, one policy each",
            plan: "two-resources".to_string(),
            instructions: vec![
                asg("asg-a", vec![cpu.clone()]),
                table(vec![predefined("DynamoDBReadCapacityUtilization", 50.0)]),
            ],
            policies: 2,
        },
        Case {
            label: "a metric measured on a resource label",
            plan: "label-plan".to_string(),
            instructions: vec![asg(
                "asg-a",
                vec![alb("app/my-lb/abc123/targetgroup/tg-a/111", 1_000.0)],
            )],
            policies: 1,
        },
        Case {
            label: "a customized metric alone",
            plan: "customized".to_string(),
            instructions: vec![table(vec![customized("queue-depth", 30.0)])],
            policies: 1,
        },
    ]
}

#[tokio::test]
async fn every_derived_policy_is_named_autoscaling_the_plan_and_a_uuid() {
    for case in cases() {
        let emulator = Emulator::new();
        applied_plan(&emulator, &case.plan, case.instructions.clone()).await;

        let names = policy_names(&emulator, &case.plan).await;
        assert_eq!(
            names.len(),
            case.policies,
            "case {:?}: one policy per target tracking configuration, got {names:?}",
            case.label
        );
        for name in &names {
            assert_derived_name(case.label, &case.plan, name);
        }

        // The UUID is minted per POLICY, so two policies of one plan never share one.
        let mut distinct = names.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            names.len(),
            "case {:?}: every policy of a plan needs its own name, got {names:?}",
            case.label
        );

        // And a read does not re-mint it: the same state reports the same names.
        let again = policy_names(&emulator, &case.plan).await;
        assert_eq!(
            again, names,
            "case {:?}: two reads of the same state report the same names, got {again:?} \
             after {names:?}",
            case.label
        );
    }
}

#[tokio::test]
async fn a_policy_minted_under_two_plans_carries_each_plans_own_name() {
    // The plan name inside the policy name is the plan that installed the policy, so the
    // same instruction set under two plan names is two sets of names, each selectable by
    // its own `AutoScaling-<plan>-` prefix.
    let emulator = Emulator::new();
    let instructions = vec![asg("asg-a", vec![predefined("ASGAverageCPUUtilization", 40.0)])];
    for (plan, source) in [("first-plan", 1), ("second-plan", 2)] {
        emulator
            .ok(
                "CreateScalingPlan",
                json!({
                    "ScalingPlanName": plan,
                    "ApplicationSource": crate::scaling_plan_lifecycle::stack_source(source),
                    "ScalingInstructions": instructions,
                }),
            )
            .await;
    }
    emulator.make_all_due();

    for plan in ["first-plan", "second-plan"] {
        let names = policy_names(&emulator, plan).await;
        assert_eq!(
            names.len(),
            1,
            "case {plan:?}: the plan must report its one policy, got {names:?}"
        );
        assert_derived_name(plan, plan, &names[0]);
    }

    let first = policy_names(&emulator, "first-plan").await;
    let second = policy_names(&emulator, "second-plan").await;
    assert_ne!(
        first, second,
        "two plans over one resource name their policies apart, got {first:?} and \
         {second:?}"
    );
}
