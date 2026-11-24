// SPDX-License-Identifier: CC0-1.0

//! What an update does to the target value of a metric, whether or not a policy of the
//! resource tracks it.
//!
//! The rule: `UpdateScalingPlan` puts the instruction set it carries in force, so the
//! target value of every target tracking configuration it sends is the value the plan
//! reports and the value the derived policy tracks to — whether or not the plan had
//! already applied a policy for that metric. A resource whose instruction turned dynamic
//! scaling off (`DisableDynamicScaling` "Controls whether dynamic scaling by AWS Auto
//! Scaling is disabled. When dynamic scaling is enabled, AWS Auto Scaling creates target
//! tracking scaling policies based on the specified target tracking configurations",
//! `smithy_query(query="shape",shape="ScalingInstruction")`) carries no policy at all, so
//! the value in force is simply the one it was sent; one that leaves dynamic scaling
//! enabled carries a policy per configuration, and those policies track to the values the
//! instruction in force states.
//!
//! [`crate::update_replaces_the_instruction_set`] spans the whole replacement rule; the
//! rows here differ along whether the instruction in force had applied a policy for the
//! metric — which changes nothing about the value in force, and is why it is crossed here
//! — with the form of the metric (predefined vs customized), the number of configurations
//! on the resource, the kind of scalable resource, and whether the update itself moves the
//! dynamic-scaling flag.
//!
//! The harness (router, dispatch, lifecycle clock) is reused from
//! [`crate::scaling_plan_lifecycle`]; nothing here builds a second one.

use serde_json::{json, Value};

use crate::applied_policies_and_status_narratives::{applied_plan, described, resources};
use crate::scaling_plan_lifecycle::Emulator;

/// A target tracking configuration on the one predefined metric the DynamoDB
/// read-capacity dimension supports
/// (`smithy_query(query="shape",shape="ScalingMetricType")`, mirrored in
/// `src/ops/types/shared_types.rs` as `SCALING_METRIC_TYPE_VALUES`).
fn predefined(target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": {
            "PredefinedScalingMetricType": "DynamoDBReadCapacityUtilization"
        },
        "TargetValue": target
    })
}

/// A target tracking configuration on a customized metric, the other FORM a metric a
/// resource tracks can take.
fn customized(target: f64) -> Value {
    json!({
        "CustomizedScalingMetricSpecification": {
            "MetricName": "queue-depth",
            "Namespace": "MyApp",
            "Statistic": "Sum"
        },
        "TargetValue": target
    })
}

/// A DynamoDB table instruction tracking `configurations`, with `DisableDynamicScaling`
/// carried only when the case states it — the member's documented default is enabled
/// (`false`), so a case can leave it out, turn it off or state it on.
fn table_instruction(configurations: Vec<Value>, disable_dynamic_scaling: Option<bool>) -> Value {
    let mut instruction = json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": "table/my-table",
        "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
        "MinCapacity": 1,
        "MaxCapacity": 100,
        "TargetTrackingConfigurations": configurations
    });
    if let Some(disabled) = disable_dynamic_scaling {
        instruction["DisableDynamicScaling"] = json!(disabled);
    }
    instruction
}

/// An Aurora DB cluster instruction, so the rule is exercised on a second kind of
/// scalable resource too.
fn cluster_instruction(target: f64, disable_dynamic_scaling: bool) -> Value {
    json!({
        "ServiceNamespace": "rds",
        "ResourceId": "cluster:my-db-cluster",
        "ScalableDimension": "rds:cluster:ReadReplicaCount",
        "MinCapacity": 1,
        "MaxCapacity": 15,
        "TargetTrackingConfigurations": [customized(target)],
        "DisableDynamicScaling": disable_dynamic_scaling
    })
}

/// One row: the instructions in force and the instructions the update carries. The
/// instructions the plan reports afterwards are the ones the update carried.
struct Case {
    label: &'static str,
    in_force: Vec<Value>,
    requested: Vec<Value>,
}

/// The table this suite spans.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "no policy tracked the predefined metric, so the update's value takes effect",
            in_force: vec![table_instruction(vec![predefined(40.0)], Some(true))],
            requested: vec![table_instruction(vec![predefined(70.0)], Some(true))],
        },
        Case {
            label: "no policy tracked the customized metric either",
            in_force: vec![table_instruction(vec![customized(25.0)], Some(true))],
            requested: vec![table_instruction(vec![customized(60.0)], Some(true))],
        },
        Case {
            label: "every configuration of a resource with no policy takes the update's value",
            in_force: vec![table_instruction(
                vec![predefined(40.0), customized(25.0)],
                Some(true),
            )],
            requested: vec![table_instruction(
                vec![predefined(70.0), customized(60.0)],
                Some(true),
            )],
        },
        Case {
            label: "an update that turns dynamic scaling back on sets the value its policy tracks",
            in_force: vec![table_instruction(vec![predefined(40.0)], Some(true))],
            // The member is omitted, which is its documented default (enabled).
            requested: vec![table_instruction(vec![predefined(70.0)], None)],
        },
        Case {
            label: "the same, with dynamic scaling stated as enabled rather than omitted",
            in_force: vec![table_instruction(vec![predefined(40.0)], Some(true))],
            requested: vec![table_instruction(vec![predefined(70.0)], Some(false))],
        },
        Case {
            label: "an Aurora DB cluster with no policy takes the update's value too",
            in_force: vec![cluster_instruction(25.0, true)],
            requested: vec![cluster_instruction(60.0, true)],
        },
        Case {
            // The other side of the crossing: there IS a policy here, tracking the value
            // in force, and the update re-points it at the value it sends.
            label: "a policy tracking the metric is re-pointed at the update's value",
            in_force: vec![table_instruction(vec![predefined(40.0)], None)],
            requested: vec![table_instruction(vec![predefined(70.0)], None)],
        },
        Case {
            label: "a policied metric takes the update's value as it turns scaling off",
            in_force: vec![table_instruction(vec![predefined(40.0)], None)],
            requested: vec![table_instruction(vec![predefined(70.0)], Some(true))],
        },
    ]
}

/// The target tracking configurations the policies on `name`'s resources track, in the
/// order the resources and their policies are reported. A resource with no policy
/// contributes nothing, and the member is absent rather than an empty list.
async fn tracked_configurations(emulator: &Emulator, name: &str, label: &str) -> Vec<Value> {
    resources(emulator, name)
        .await
        .iter()
        .flat_map(|resource| {
            let policies = match resource.get("ScalingPolicies") {
                None => Vec::new(),
                Some(policies) => policies
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!("case {label:?}: ScalingPolicies is not a list in {resource:?}")
                    })
                    .clone(),
            };
            policies
                .into_iter()
                .map(|policy| policy["TargetTrackingConfiguration"].clone())
                .collect::<Vec<Value>>()
        })
        .collect()
}

#[tokio::test]
async fn an_update_sets_the_target_value_of_every_metric_it_carries() {
    for case in cases() {
        let emulator = Emulator::new();
        let label = case.label;
        applied_plan(&emulator, "my-plan", case.in_force.clone()).await;

        emulator
            .ok(
                "UpdateScalingPlan",
                json!({
                    "ScalingPlanName": "my-plan",
                    "ScalingPlanVersion": 1,
                    "ScalingInstructions": case.requested.clone(),
                }),
            )
            .await;
        emulator.make_all_due();

        let expected = case.requested.clone();
        let plan = described(&emulator, "my-plan").await;
        assert_eq!(
            plan.get("ScalingInstructions"),
            Some(&Value::Array(expected.clone())),
            "case {label:?}: wrong ScalingInstructions in force after the update, got {plan:?}"
        );

        // Whatever the instructions in force say, the policies on the resources track to
        // them: a resource whose instruction disabled dynamic scaling carries none, and
        // one whose instruction enables it carries a policy per configuration.
        let expected_configurations: Vec<Value> = expected
            .iter()
            .filter(|instruction| instruction["DisableDynamicScaling"] != json!(true))
            .flat_map(|instruction| {
                instruction["TargetTrackingConfigurations"]
                    .as_array()
                    .expect("the case's configurations")
                    .clone()
            })
            .collect();
        let tracked = tracked_configurations(&emulator, "my-plan", label).await;
        assert_eq!(
            tracked, expected_configurations,
            "case {label:?}: the applied policies must track the configurations in force"
        );
    }
}

#[tokio::test]
async fn a_resource_takes_the_target_value_of_every_update() {
    let emulator = Emulator::new();
    applied_plan(
        &emulator,
        "my-plan",
        vec![table_instruction(vec![predefined(40.0)], Some(true))],
    )
    .await;

    // Each update in turn sets the value of the metric, with dynamic scaling off and then
    // back on: the value in force is the one the LAST update sent, however many ran before
    // it and whether or not the plan had a policy tracking that metric by then.
    for (disabled, target) in [
        (Some(true), 70.0),
        (Some(true), 90.0),
        (None, 10.0),
        (Some(false), 55.0),
        (None, 80.0),
    ] {
        let requested = vec![table_instruction(vec![predefined(target)], disabled)];
        emulator
            .ok(
                "UpdateScalingPlan",
                json!({
                    "ScalingPlanName": "my-plan",
                    "ScalingPlanVersion": 1,
                    "ScalingInstructions": requested.clone(),
                }),
            )
            .await;
        emulator.make_all_due();

        let plan = described(&emulator, "my-plan").await;
        assert_eq!(
            plan.get("ScalingInstructions"),
            Some(&Value::Array(requested.clone())),
            "case target {target:?} (dynamic scaling disabled: {disabled:?}): the update's \
             target value must be the one in force, got {plan:?}"
        );
    }
}
