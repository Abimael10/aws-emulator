// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateScalingPlan` does to the target value of a metric whose policy the plan
//! has already APPLIED — and when the applied status re-narrates itself as an update.
//!
//! Two rules are pinned here, and they are one rule read twice: a plan applies the
//! instruction set the last request put in force, and reports what it applied.
//!
//! * `UpdateScalingPlan` "Updates the specified scaling plan"
//!   (`smithy_query(query="operation",operation="UpdateScalingPlan")`) and takes the
//!   plan's whole `ScalingInstructions` list, so the list the request carries is the
//!   configuration in force afterwards, member for member — the `TargetValue` of a target
//!   tracking configuration included, whether or not a policy the plan has already
//!   applied tracks that metric. `TargetTrackingConfiguration$TargetValue` is "The target
//!   value for the metric" with no update-time exception
//!   (`smithy_query(query="shape",shape="TargetTrackingConfiguration")`), and the one
//!   documented control over what happens to a resource's pre-existing policies,
//!   `ScalingInstruction$ScalingPolicyUpdateBehavior`, is scoped to policies the service
//!   did not create: "Controls whether a resource's externally created scaling policies
//!   are kept or replaced" (`smithy_query(query="shape",shape="ScalingInstruction")`). So
//!   nothing the request sends is held back, and the policy the plan derives for a metric
//!   it still tracks is re-pointed at the new value under the name it already had: the
//!   name is a function of the plan, the resource and the metric, not of the value.
//! * `ScalingPlan$StatusMessage` is "A simple message about the current status of the
//!   scaling plan" (`smithy_query(query="shape",shape="ScalingPlan")`), and the applied
//!   status is reached by APPLYING a configuration to the plan's resources, so the
//!   narrative of an applied plan reports the mutation that last changed what is applied:
//!   an update that leaves the plan's resources with the same capacity bands and the same
//!   target tracking policies applied nothing, and the narrative of the application in
//!   force stands. Since neither `ScalingPlanVersion` ("This value is always 1") nor
//!   `CreationTime` moves on an update, that narrative is what tells a caller the update
//!   took.
//!
//! The rows of each table differ along what the rules generalise over: whether the metric
//! in force had an applied policy at all, the FORM of the metric (predefined, predefined
//! with a resource label, customized), the direction the requested value moves, the NUMBER
//! of configurations and of resources, which members the update moves beside the value,
//! and whether the update turns dynamic scaling on or off.
//!
//! The harness (router, dispatch, lifecycle clock) is reused from
//! [`crate::scaling_plan_lifecycle`] and the two describe readers from
//! [`crate::applied_policies_and_status_narratives`]; nothing here builds a second one.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::applied_policies_and_status_narratives::{applied_plan, described, resources};
use crate::scaling_plan_lifecycle::{stack_source, Emulator};

/// The plan every case writes to.
const PLAN: &str = "my-plan";

/// The narrative an applied plan reports while the create is the last thing it applied.
pub(crate) const CREATED_MESSAGE: &str =
    "Scaling plan has been created and applied to all resources.";

/// The narrative an applied plan reports once an update changed what it applies.
pub(crate) const UPDATED_MESSAGE: &str = "Scaling plan has been updated successfully.";

/// The identity of the metric one target tracking configuration tracks: every modelled
/// part that says WHICH metric it is.
///
/// For a predefined metric that is the metric type together with the resource label it is
/// measured on ("Identifies the resource associated with the metric type",
/// `smithy_query(query="doc",shape="PredefinedScalingMetricSpecification",member="ResourceLabel")`);
/// for a customized one the namespace, the metric name, the statistic and the SET of
/// dimensions it was published with.
///
/// `pub(crate)` so every suite that has to say which configuration tracks which metric
/// reads one definition rather than keeping a second beside it.
pub(crate) fn metric_identity(configuration: &Value) -> String {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    if let Some(predefined) = configuration.get("PredefinedScalingMetricSpecification") {
        return format!(
            "predefined {} on {}",
            text(&predefined["PredefinedScalingMetricType"]),
            text(&predefined["ResourceLabel"])
        );
    }
    let customized = &configuration["CustomizedScalingMetricSpecification"];
    let mut dimensions: Vec<String> = customized["Dimensions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|dimension| format!("{}={}", text(&dimension["Name"]), text(&dimension["Value"])))
        .collect();
    dimensions.sort();
    dimensions.dedup();
    format!(
        "customized {}/{}/{}[{}]",
        text(&customized["Namespace"]),
        text(&customized["MetricName"]),
        text(&customized["Statistic"]),
        dimensions.join(",")
    )
}

/// Whether the plan has an applied target tracking policy for each metric `instruction`
/// configures: it has, unless the instruction turned dynamic scaling off.
fn applies_policies(instruction: &Value) -> bool {
    instruction["DisableDynamicScaling"] != json!(true)
}

/// One target tracking configuration over a predefined metric.
fn predefined(metric: &str, target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": { "PredefinedScalingMetricType": metric },
        "TargetValue": target
    })
}

/// One target tracking configuration over the ALB metric and the target group its
/// resource label identifies.
fn alb(target_group: &str, target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": {
            "PredefinedScalingMetricType": "ALBRequestCountPerTarget",
            "ResourceLabel": format!("app/my-lb/abc123/targetgroup/{target_group}/111")
        },
        "TargetValue": target
    })
}

/// One target tracking configuration over a customized metric.
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

/// A DynamoDB table instruction over `configurations`, with the extra members `extras`
/// carries merged in.
fn table(configurations: Vec<Value>, extras: Value) -> Value {
    let mut instruction = json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": "table/my-table",
        "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
        "MinCapacity": 1,
        "MaxCapacity": 100,
        "TargetTrackingConfigurations": configurations
    });
    for (member, value) in extras.as_object().cloned().unwrap_or_default() {
        instruction[member] = value;
    }
    instruction
}

/// An Auto Scaling group instruction over `configurations` — a second kind of scalable
/// resource, and the one the ALB metric is measured for.
fn asg(configurations: Vec<Value>) -> Value {
    json!({
        "ServiceNamespace": "autoscaling",
        "ResourceId": "autoScalingGroup/my-asg",
        "ScalableDimension": "autoscaling:autoScalingGroup:DesiredCapacity",
        "MinCapacity": 1,
        "MaxCapacity": 10,
        "TargetTrackingConfigurations": configurations
    })
}

/// One row: the instructions in force, the instructions the update carries, and the
/// target value every configuration in force afterwards must report, in the order the
/// update's instructions and their configurations are written.
///
/// The target values are stated LITERALLY per row, beside the whole instruction set the
/// update sent: a row says which value it expects to be in force, so a misreading shared
/// by the rule and its expectation cannot pass unnoticed.
struct Case {
    label: &'static str,
    in_force: Vec<Value>,
    requested: Vec<Value>,
    targets: &'static [f64],
}

/// The table the replacement rule spans.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a policied predefined metric takes the value the update sends",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({}),
            )],
            targets: &[75.0],
        },
        Case {
            label: "the same, with the requested value below the one in force",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 12.5)],
                json!({}),
            )],
            targets: &[12.5],
        },
        Case {
            label: "the value in force re-sent unchanged stays in force",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            targets: &[50.0],
        },
        Case {
            label: "the value moves while the capacity bounds move too",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({ "MinCapacity": 4, "MaxCapacity": 400 }),
            )],
            targets: &[75.0],
        },
        Case {
            label: "every other member of a policied configuration moves with it",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![json!({
                    "PredefinedScalingMetricSpecification": {
                        "PredefinedScalingMetricType": "DynamoDBReadCapacityUtilization"
                    },
                    "TargetValue": 75.0,
                    "DisableScaleIn": true,
                    "ScaleOutCooldown": 90,
                    "ScaleInCooldown": 600
                })],
                json!({}),
            )],
            targets: &[75.0],
        },
        Case {
            label: "the value moves while the instruction asks for a policy replacement",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({ "ScalingPolicyUpdateBehavior": "ReplaceExternalPolicies" }),
            )],
            targets: &[75.0],
        },
        Case {
            label: "a resource with no applied policy takes the update's value",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 40.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 70.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
            targets: &[70.0],
        },
        Case {
            label: "a resource whose dynamic scaling the update turns back on takes its value",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 40.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 70.0)],
                json!({}),
            )],
            targets: &[70.0],
        },
        Case {
            label: "a policied metric takes the update's value as it turns scaling off",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
            targets: &[75.0],
        },
        Case {
            label: "a metric the resource did not track takes the update's value beside it",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![table(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 75.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
            targets: &[75.0, 30.0],
        },
        Case {
            label: "a policied customized metric takes the update's value too",
            in_force: vec![table(vec![customized("queue-depth", 25.0)], json!({}))],
            requested: vec![table(vec![customized("queue-depth", 60.0)], json!({}))],
            targets: &[60.0],
        },
        Case {
            label: "a configuration dropped from a resource leaves the survivor's value sent",
            in_force: vec![table(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 50.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
            requested: vec![table(vec![customized("queue-depth", 60.0)], json!({}))],
            targets: &[60.0],
        },
        Case {
            label: "the configurations of one resource are reordered and re-pointed",
            in_force: vec![table(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 50.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
            requested: vec![table(
                vec![
                    customized("queue-depth", 60.0),
                    predefined("DynamoDBReadCapacityUtilization", 75.0),
                ],
                json!({}),
            )],
            targets: &[60.0, 75.0],
        },
        Case {
            label: "a metric re-pointed at another target group takes the update's value",
            in_force: vec![asg(vec![alb("tg-a", 1_000.0)])],
            requested: vec![asg(vec![alb("tg-b", 500.0)])],
            targets: &[500.0],
        },
        Case {
            label: "a metric re-sent on the same target group takes it as well",
            in_force: vec![asg(vec![alb("tg-a", 1_000.0)])],
            requested: vec![asg(vec![alb("tg-a", 500.0)])],
            targets: &[500.0],
        },
        Case {
            label: "a resource the plan did not cover takes the update's values",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![
                table(
                    vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                    json!({}),
                ),
                asg(vec![predefined("ASGAverageCPUUtilization", 40.0)]),
            ],
            targets: &[75.0, 40.0],
        },
        Case {
            label: "the one resource in force is swapped for another",
            in_force: vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
            requested: vec![asg(vec![predefined("ASGAverageCPUUtilization", 40.0)])],
            targets: &[40.0],
        },
    ]
}

/// Update `PLAN` with `instructions` and let the clock apply the update.
async fn update_plan(emulator: &Emulator, instructions: &[Value]) {
    emulator
        .ok(
            "UpdateScalingPlan",
            json!({
                "ScalingPlanName": PLAN,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": instructions,
            }),
        )
        .await;
    emulator.make_all_due();
}

/// The target tracking configurations the policies of `PLAN`'s resources track, in the
/// order the resources and their policies are reported.
async fn tracked_configurations(emulator: &Emulator) -> Vec<Value> {
    resources(emulator, PLAN)
        .await
        .iter()
        .flat_map(|resource| {
            resource
                .get("ScalingPolicies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|policy| policy["TargetTrackingConfiguration"].clone())
                .collect::<Vec<Value>>()
        })
        .collect()
}

/// The configurations `configurations` holds, keyed by the metric each tracks: one plan
/// covers one resource per instruction and one policy per metric, so this is a comparison
/// that does not depend on the order the service reports its resources in.
fn by_metric(configurations: &[Value]) -> BTreeMap<String, Value> {
    let keyed: BTreeMap<String, Value> = configurations
        .iter()
        .map(|configuration| (metric_identity(configuration), configuration.clone()))
        .collect();
    assert_eq!(
        keyed.len(),
        configurations.len(),
        "every row of this suite tracks each metric once across the whole plan, so the \
         metric is a key: got {configurations:?}"
    );
    keyed
}

#[tokio::test]
async fn an_update_puts_the_target_value_it_sends_in_force() {
    for case in cases() {
        let emulator = Emulator::new();
        let label = case.label;
        applied_plan(&emulator, PLAN, case.in_force.clone()).await;

        update_plan(&emulator, &case.requested).await;

        let expected = case.requested.clone();
        let plan = described(&emulator, PLAN).await;
        assert_eq!(
            plan.get("ScalingInstructions"),
            Some(&Value::Array(expected.clone())),
            "case {label:?}: wrong instruction set in force after the update, got {plan}"
        );

        // The row's own literal expectation of which target value is in force, beside the
        // whole set above: the value the update sent, whether or not an applied policy
        // tracked that metric before.
        let reported: Vec<f64> = expected
            .iter()
            .flat_map(|instruction| {
                instruction["TargetTrackingConfigurations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .map(|configuration| configuration["TargetValue"].as_f64().unwrap_or_default())
            .collect();
        assert_eq!(
            reported,
            case.targets.to_vec(),
            "case {label:?}: expected the target values {:?} in force, got {reported:?}",
            case.targets
        );

        // The same rule read through the resource envelope: the policies on the resources
        // track the configurations in force, so the value a policy tracks and the value
        // the plan reports for its metric can never disagree. Both sides are keyed by the
        // metric, because the resources are reported in the service's own order.
        let tracked = by_metric(&tracked_configurations(&emulator).await);
        let applied = by_metric(
            &expected
                .iter()
                .filter(|instruction| applies_policies(instruction))
                .flat_map(|instruction| {
                    instruction["TargetTrackingConfigurations"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .collect::<Vec<Value>>(),
        );
        assert_eq!(
            tracked, applied,
            "case {label:?}: the applied policies must track the configurations in force"
        );
    }
}

#[tokio::test]
async fn a_re_pointed_policy_keeps_the_name_it_had() {
    // A policy's name is a function of the plan, the resource and the metric, so an update
    // that only moves what the policy tracks TO re-points the policy a client is already
    // following rather than minting another: same name, new target value.
    let emulator = Emulator::new();
    let in_force = vec![table(
        vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
        json!({}),
    )];
    applied_plan(&emulator, PLAN, in_force.clone()).await;

    let before = resources(&emulator, PLAN).await;
    let policy_before = before[0]["ScalingPolicies"][0].clone();

    update_plan(
        &emulator,
        &[table(
            vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
            json!({ "MinCapacity": 2, "MaxCapacity": 200 }),
        )],
    )
    .await;

    let after = resources(&emulator, PLAN).await;
    let policy_after = after[0]["ScalingPolicies"][0].clone();
    assert_eq!(
        (
            policy_after["PolicyName"].clone(),
            policy_after["PolicyType"].clone()
        ),
        (
            policy_before["PolicyName"].clone(),
            policy_before["PolicyType"].clone()
        ),
        "the policy of a metric the plan still tracks keeps the name and type it had, got \
         {policy_after} after {policy_before}"
    );
    assert_eq!(
        policy_after["TargetTrackingConfiguration"]["TargetValue"].as_f64(),
        Some(75.0),
        "the re-pointed policy must track the value the update sent, got {policy_after}"
    );
    // The members the update moved beside the value are in force too.
    let plan = described(&emulator, PLAN).await;
    let instruction = &plan["ScalingInstructions"][0];
    assert_eq!(
        (
            instruction.get("MinCapacity"),
            instruction.get("MaxCapacity")
        ),
        (Some(&json!(2)), Some(&json!(200))),
        "the capacity bounds the update carried are in force, got {plan}"
    );
}

/// One row of the narrative table: what the update carries, and whether it changes what
/// the plan applies to its resources.
struct NarrativeCase {
    label: &'static str,
    request_extras: Value,
    applied_change: bool,
}

/// The table the narrative rule spans: each row updates a plan applied over one DynamoDB
/// table tracking `DynamoDBReadCapacityUtilization` at 50.
fn narrative_cases() -> Vec<NarrativeCase> {
    vec![
        NarrativeCase {
            label: "nothing beyond the plan's identity",
            request_extras: json!({}),
            applied_change: false,
        },
        NarrativeCase {
            label: "the instruction set in force, re-sent as it stands",
            request_extras: json!({
                "ScalingInstructions": [table(
                    vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                    json!({}),
                )],
            }),
            applied_change: false,
        },
        NarrativeCase {
            label: "a new target value for the metric the applied policy tracks",
            request_extras: json!({
                "ScalingInstructions": [table(
                    vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                    json!({}),
                )],
            }),
            applied_change: true,
        },
        NarrativeCase {
            label: "a wider capacity band for the resource",
            request_extras: json!({
                "ScalingInstructions": [table(
                    vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                    json!({ "MinCapacity": 2, "MaxCapacity": 200 }),
                )],
            }),
            applied_change: true,
        },
        NarrativeCase {
            label: "a second metric on the resource, so a second policy",
            request_extras: json!({
                "ScalingInstructions": [table(
                    vec![
                        predefined("DynamoDBReadCapacityUtilization", 50.0),
                        customized("queue-depth", 30.0),
                    ],
                    json!({}),
                )],
            }),
            applied_change: true,
        },
        NarrativeCase {
            label: "dynamic scaling turned off, so the policy is removed",
            request_extras: json!({
                "ScalingInstructions": [table(
                    vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                    json!({ "DisableDynamicScaling": true }),
                )],
            }),
            applied_change: true,
        },
        NarrativeCase {
            label: "another resource for the plan to cover",
            request_extras: json!({
                "ScalingInstructions": [
                    table(
                        vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                        json!({}),
                    ),
                    asg(vec![predefined("ASGAverageCPUUtilization", 40.0)]),
                ],
            }),
            applied_change: true,
        },
        NarrativeCase {
            label: "another application source for the plan",
            request_extras: json!({ "ApplicationSource": stack_source(9) }),
            applied_change: true,
        },
    ]
}

#[tokio::test]
async fn an_applied_plan_is_narrated_as_updated_only_when_the_update_changed_what_it_applies() {
    for case in narrative_cases() {
        let label = case.label;
        let emulator = Emulator::new();
        applied_plan(
            &emulator,
            PLAN,
            vec![table(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
        )
        .await;

        let created = described(&emulator, PLAN).await;
        assert_eq!(
            (created.get("StatusCode"), created.get("StatusMessage")),
            (Some(&json!("Active")), Some(&json!(CREATED_MESSAGE))),
            "case {label:?}: an applied create reports its own narrative, got {created}"
        );

        let mut request = json!({ "ScalingPlanName": PLAN, "ScalingPlanVersion": 1 });
        for (member, value) in case.request_extras.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        emulator.ok("UpdateScalingPlan", request).await;

        // Whatever the update carried, the plan says it is being updated while the
        // transition is in flight: the request was accepted.
        emulator.hold_pending(PLAN);
        let updating = described(&emulator, PLAN).await;
        assert_eq!(
            updating.get("StatusCode"),
            Some(&json!("UpdateInProgress")),
            "case {label:?}: an accepted update leaves the plan being updated, got \
             {updating}"
        );

        emulator.make_all_due();
        let updated = described(&emulator, PLAN).await;
        let expected = if case.applied_change {
            UPDATED_MESSAGE
        } else {
            CREATED_MESSAGE
        };
        assert_eq!(
            (updated.get("StatusCode"), updated.get("StatusMessage")),
            (Some(&json!("Active")), Some(&json!(expected))),
            "case {label:?}: an applied plan reports the mutation that last changed what \
             it applies, so the narrative must be {expected:?}, got {updated}"
        );
        // Neither the version nor the creation time moves, whichever narrative it is.
        assert_eq!(
            (
                updated.get("ScalingPlanVersion"),
                updated.get("CreationTime")
            ),
            (
                created.get("ScalingPlanVersion"),
                created.get("CreationTime")
            ),
            "case {label:?}: an in-place update moves neither the version nor the \
             creation time, got {updated} after {created}"
        );
    }
}
