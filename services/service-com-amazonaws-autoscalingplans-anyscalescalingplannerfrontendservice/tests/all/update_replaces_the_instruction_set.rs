// SPDX-License-Identifier: CC0-1.0

//! What an in-place `UpdateScalingPlan` leaves behind, and how the policies a plan
//! derives from its instructions are named.
//!
//! Two rules are pinned here:
//!
//! * `UpdateScalingPlan` "Updates the specified scaling plan"
//!   (`smithy_query(query="operation",operation="UpdateScalingPlan")`) and takes the
//!   plan's whole `ScalingInstructions` list, so the list the request carries is the
//!   configuration in force afterwards, entry for entry and member for member — a
//!   resource it no longer names is dropped, one it adds is covered, and the target
//!   tracking policies the plan derives track exactly the configurations it asked for.
//!   No member is held back, the target value of a metric the plan already tracks
//!   included, which is the rule
//!   [`crate::update_keeps_an_applied_policys_target_value`] spans and which every row
//!   here is read through. The operation answers an EMPTY body and moves
//!   neither `ScalingPlanVersion` ("This value is always 1") nor `CreationTime`, so the
//!   applied plan's `StatusMessage` — "A simple message about the current status of the
//!   scaling plan" — is what reports that the update took: a created-and-applied plan and
//!   a plan whose update changed what it applies report two different sentences.
//! * the `PolicyName` of a policy a plan derives from a target tracking configuration is
//!   built from the plan: the literal `AutoScaling-`, the `ScalingPlanName` verbatim, a
//!   `-`, and an id the service generates for that policy. The name is the only handle
//!   joining a plan to the policies it installed, so a client selects them by that
//!   derivation; `ScalingPolicy$PolicyName` only constrains the result to 1-256
//!   printable characters (`smithy_query(query="shape",shape="ScalingPolicy")`), and the
//!   generated id is not asserted beyond being present, stable and distinct per policy.
//!
//! The instruction set a plan may hold is also BOUNDED — one plan covers at most
//! `MAX_SCALING_INSTRUCTIONS` resources, which both write paths refuse a longer list
//! against with the one sentence this crate words a list past its bound with — so the
//! policies a plan installs, and the work of installing them, cannot grow without limit.
//!
//! The harness (router, dispatch, lifecycle clock) is reused from
//! [`crate::scaling_plan_lifecycle`] and the two describe readers from
//! [`crate::applied_policies_and_status_narratives`]; nothing here builds a second one.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_autoscalingplans_anyscalescalingplannerfrontendservice::state::{
    MAX_SCALING_INSTRUCTIONS, SCALING_INSTRUCTION_ENTRIES,
};

use crate::applied_policies_and_status_narratives::{applied_plan, described, resources};
use crate::documented_entry_count_wording::entry_count_refusal;
use crate::scaling_plan_lifecycle::{error_code, error_message, stack_source, Emulator};

/// The narrative a plan reports once its creation has been applied.
const CREATED_MESSAGE: &str = "Scaling plan has been created and applied to all resources.";

/// The narrative a plan reports once an in-place update of it has been applied.
const UPDATED_MESSAGE: &str = "Scaling plan has been updated successfully.";

/// The narrative a plan reports while the update is still being applied.
const UPDATING_MESSAGE: &str = "Scaling plan is being updated.";

/// The literal prefix of a derived policy's name, ahead of the plan's own name.
const POLICY_NAME_PREFIX: &str = "AutoScaling-";

/// The documented bounds of `ScalingPolicy$PolicyName`
/// (`smithy_query(query="shape",shape="ScalingPolicy")`: len 1..256, pattern
/// `^\p{Print}+$`).
const MIN_POLICY_NAME: usize = 1;
const MAX_POLICY_NAME: usize = 256;

/// The documented maximum length of a `ScalingPlanName`
/// (`smithy_query(query="shape",shape="ScalingPlanName")`: len 1..128).
const MAX_PLAN_NAME: usize = 128;

/// A target tracking configuration over a predefined metric
/// (`smithy_query(query="shape",shape="ScalingMetricType")`).
fn predefined(metric: &str, target: f64) -> Value {
    json!({
        "PredefinedScalingMetricSpecification": { "PredefinedScalingMetricType": metric },
        "TargetValue": target
    })
}

/// A target tracking configuration over a customized metric — the other FORM a tracked
/// metric can take. `Statistic` is a modelled `MetricStatistic` value.
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
fn table_instruction(configurations: Vec<Value>, extras: Value) -> Value {
    let mut instruction = json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": "table/my-table",
        "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
        "MinCapacity": 1,
        "MaxCapacity": 10,
        "TargetTrackingConfigurations": configurations
    });
    for (member, value) in extras.as_object().cloned().unwrap_or_default() {
        instruction[member] = value;
    }
    instruction
}

/// An ECS service instruction, so a row can move the plan onto another resource.
fn service_instruction(target: f64) -> Value {
    json!({
        "ServiceNamespace": "ecs",
        "ResourceId": "service/default/sample-webapp",
        "ScalableDimension": "ecs:service:DesiredCount",
        "MinCapacity": 2,
        "MaxCapacity": 20,
        "TargetTrackingConfigurations": [predefined("ECSServiceAverageCPUUtilization", target)]
    })
}

/// One row of the replacement table: the instructions in force and the instructions the
/// update carries. What the plan must report afterwards is the second list, verbatim.
struct Case {
    label: &'static str,
    in_force: Vec<Value>,
    requested: Vec<Value>,
}

/// The table this suite spans. Rows differ along what the rule generalises over: WHICH
/// member of a re-sent instruction moves (the target value, the capacity bounds, the
/// cooldowns and the scale-in flag), the NUMBER of configurations on one resource
/// (added, dropped), the NUMBER of resources the plan covers (added, dropped, swapped),
/// the FORM of the tracked metric (predefined vs customized), the direction the target
/// value moves, whether the instruction asks for external policies to be replaced, and
/// whether dynamic scaling is on at all.
fn cases() -> Vec<Case> {
    let tracked = table_instruction(
        vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
        json!({}),
    );
    vec![
        Case {
            label: "the target value of the one tracked metric rises",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({}),
            )],
        },
        Case {
            label: "the target value of the one tracked metric falls",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 12.5)],
                json!({}),
            )],
        },
        Case {
            label: "the target value moves while the instruction asks for a policy replacement",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({ "ScalingPolicyUpdateBehavior": "ReplaceExternalPolicies" }),
            )],
        },
        Case {
            label: "the capacity bounds move with the target value",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 65.0)],
                json!({ "MinCapacity": 4, "MaxCapacity": 400 }),
            )],
        },
        Case {
            label: "the cooldowns and the scale-in flag of the tracked metric move too",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
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
        },
        Case {
            label: "a customized metric is added beside the tracked one",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 75.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
        },
        Case {
            label: "one of two configurations is dropped and the survivor re-pointed",
            in_force: vec![table_instruction(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 50.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
            requested: vec![table_instruction(vec![customized("queue-depth", 60.0)], json!({}))],
        },
        Case {
            label: "a customized metric already tracked takes the update's value",
            in_force: vec![table_instruction(vec![customized("queue-depth", 25.0)], json!({}))],
            requested: vec![table_instruction(vec![customized("queue-depth", 60.0)], json!({}))],
        },
        Case {
            label: "a second resource is added to the plan",
            in_force: vec![tracked.clone()],
            requested: vec![
                table_instruction(
                    vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                    json!({}),
                ),
                service_instruction(40.0),
            ],
        },
        Case {
            label: "the one resource in force is swapped for another",
            in_force: vec![tracked.clone()],
            requested: vec![service_instruction(55.5)],
        },
        Case {
            label: "the resource in force keeps its metric but turns dynamic scaling off",
            in_force: vec![tracked.clone()],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 75.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
        },
        Case {
            label: "a resource whose dynamic scaling was off gets it back with a new value",
            in_force: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 40.0)],
                json!({ "DisableDynamicScaling": true }),
            )],
            requested: vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 70.0)],
                json!({ "DisableDynamicScaling": false }),
            )],
        },
    ]
}

/// The target tracking configurations the policies of `name`'s resources track, in the
/// order the resources and their policies are reported.
async fn tracked_configurations(emulator: &Emulator, name: &str) -> Vec<Value> {
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
                .map(|policy| policy["TargetTrackingConfiguration"].clone())
                .collect::<Vec<Value>>()
        })
        .collect()
}

/// The configurations the instructions `requested` ask policies for: one per
/// configuration of every instruction that leaves dynamic scaling enabled.
fn expected_configurations(requested: &[Value]) -> Vec<Value> {
    requested
        .iter()
        .filter(|instruction| instruction["DisableDynamicScaling"] != json!(true))
        .flat_map(|instruction| {
            instruction["TargetTrackingConfigurations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// Update `name` with `instructions` and let the clock apply the update.
async fn update_plan(emulator: &Emulator, name: &str, instructions: &[Value]) -> Value {
    let body = emulator
        .ok(
            "UpdateScalingPlan",
            json!({
                "ScalingPlanName": name,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": instructions,
            }),
        )
        .await;
    emulator.make_all_due();
    body
}

#[tokio::test]
async fn an_update_puts_the_instruction_set_it_carries_in_force() {
    for case in cases() {
        let emulator = Emulator::new();
        let label = case.label;
        applied_plan(&emulator, "my-plan", case.in_force.clone()).await;

        let body = update_plan(&emulator, "my-plan", &case.requested).await;
        assert_eq!(
            body,
            json!({}),
            "case {label:?}: UpdateScalingPlan answers an empty body, got {body:?}"
        );

        let expected = case.requested.clone();
        let plan = described(&emulator, "my-plan").await;
        assert_eq!(
            plan.get("ScalingInstructions"),
            Some(&Value::Array(expected.clone())),
            "case {label:?}: the update's instruction set must be the one in force, got \
             {plan:?}"
        );

        // The same rule read through the resource envelope: the derived policies track
        // the configurations now in force, and nothing else.
        let tracked = tracked_configurations(&emulator, "my-plan").await;
        assert_eq!(
            tracked,
            expected_configurations(&expected),
            "case {label:?}: the derived policies must track the configurations the \
             update put in force, got {tracked:?}"
        );
    }
}

#[tokio::test]
async fn an_applied_update_is_reported_as_an_update_while_version_and_creation_time_stay() {
    let emulator = Emulator::new();
    let instructions = vec![table_instruction(
        vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
        json!({}),
    )];
    applied_plan(&emulator, "my-plan", instructions.clone()).await;

    let created = described(&emulator, "my-plan").await;
    assert_eq!(
        (created.get("StatusCode"), created.get("StatusMessage")),
        (Some(&json!("Active")), Some(&json!(CREATED_MESSAGE))),
        "case created: an applied create reports its own narrative, got {created:?}"
    );

    // Each row updates the plan again with a change the plan APPLIES to its resources,
    // differing in what the request carries: a wider capacity band, a second metric, and
    // a new application source. (An update that changes nothing applied keeps the
    // narrative of the application in force; that half of the rule is spanned by
    // [`crate::update_keeps_an_applied_policys_target_value`].)
    let updates: Vec<(&str, Value)> = vec![
        (
            "a wider capacity band for the resource",
            json!({
                "ScalingInstructions": [table_instruction(
                    vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                    json!({ "MinCapacity": 4, "MaxCapacity": 400 }),
                )],
            }),
        ),
        (
            "a second metric on the resource",
            json!({
                "ScalingInstructions": [table_instruction(
                    vec![
                        predefined("DynamoDBReadCapacityUtilization", 50.0),
                        customized("queue-depth", 30.0),
                    ],
                    json!({ "MinCapacity": 4, "MaxCapacity": 400 }),
                )],
            }),
        ),
        (
            "another application source",
            json!({ "ApplicationSource": stack_source(2) }),
        ),
    ];

    for (label, extras) in updates {
        let mut request = json!({ "ScalingPlanName": "my-plan", "ScalingPlanVersion": 1 });
        for (member, value) in extras.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        emulator.ok("UpdateScalingPlan", request).await;

        // While the update is being applied the plan says so, and no read of it claims
        // the create narrative again.
        emulator.hold_pending("my-plan");
        let updating = described(&emulator, "my-plan").await;
        assert_eq!(
            (updating.get("StatusCode"), updating.get("StatusMessage")),
            (
                Some(&json!("UpdateInProgress")),
                Some(&json!(UPDATING_MESSAGE))
            ),
            "case {label:?}: a plan being updated reports its own narrative, got \
             {updating:?}"
        );

        emulator.make_all_due();
        let updated = described(&emulator, "my-plan").await;
        assert_eq!(
            (updated.get("StatusCode"), updated.get("StatusMessage")),
            (Some(&json!("Active")), Some(&json!(UPDATED_MESSAGE))),
            "case {label:?}: an applied update is reported as an update, got {updated:?}"
        );
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
             creation time, got {updated:?} after {created:?}"
        );
    }

    // The narrative is per PLAN: one created beside the updated one still reports the
    // create narrative.
    emulator
        .ok(
            "CreateScalingPlan",
            json!({
                "ScalingPlanName": "other-plan",
                "ApplicationSource": stack_source(7),
                "ScalingInstructions": instructions,
            }),
        )
        .await;
    emulator.make_all_due();
    let other = described(&emulator, "other-plan").await;
    assert_eq!(
        (other.get("StatusCode"), other.get("StatusMessage")),
        (Some(&json!("Active")), Some(&json!(CREATED_MESSAGE))),
        "case a second plan: a created plan reports the create narrative however its \
         neighbours were written, got {other:?}"
    );
}

/// The `PolicyName`s of every policy `name` reports, in the order they are reported.
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

#[tokio::test]
async fn a_derived_policy_is_named_after_the_plan_that_installed_it() {
    // Rows differ along the plan NAME the derivation carries (plain, punctuation the
    // modelled pattern admits, a space, one character, the 128-character maximum) and
    // along the instruction set the policies come from (one resource, two resources, two
    // configurations on one resource, a customized metric).
    let plan_names: Vec<(&str, String)> = vec![
        ("a plain name", "my-plan".to_string()),
        ("punctuation the pattern admits", "my.plan_1-#2".to_string()),
        ("a name holding a space", "my plan".to_string()),
        ("the shortest name", "p".to_string()),
        ("the longest name", "p".repeat(MAX_PLAN_NAME)),
    ];
    let instruction_sets: Vec<(&str, Vec<Value>)> = vec![
        (
            "one resource, one predefined metric",
            vec![table_instruction(
                vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                json!({}),
            )],
        ),
        (
            "one resource, two metrics of both forms",
            vec![table_instruction(
                vec![
                    predefined("DynamoDBReadCapacityUtilization", 50.0),
                    customized("queue-depth", 30.0),
                ],
                json!({}),
            )],
        ),
        (
            "two resources",
            vec![
                table_instruction(
                    vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
                    json!({}),
                ),
                service_instruction(40.0),
            ],
        ),
    ];

    for (name_label, plan) in &plan_names {
        for (set_label, instructions) in &instruction_sets {
            let case = format!("{name_label} / {set_label}");
            let emulator = Emulator::new();
            applied_plan(&emulator, plan, instructions.clone()).await;

            let names = policy_names(&emulator, plan).await;
            assert_eq!(
                names.len(),
                expected_configurations(instructions).len(),
                "case {case:?}: one policy per target tracking configuration, got \
                 {names:?}"
            );

            let prefix = format!("{POLICY_NAME_PREFIX}{plan}-");
            let mut generated_ids: BTreeSet<&str> = BTreeSet::new();
            for policy_name in &names {
                let generated = policy_name.strip_prefix(&prefix).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a derived policy must be named {prefix:?} plus a \
                         generated id, got {policy_name:?}"
                    )
                });
                assert!(
                    !generated.is_empty(),
                    "case {case:?}: the generated id of {policy_name:?} must not be empty"
                );
                let length = policy_name.chars().count();
                assert!(
                    (MIN_POLICY_NAME..=MAX_POLICY_NAME).contains(&length)
                        && !policy_name.chars().any(char::is_control),
                    "case {case:?}: PolicyName must be {MIN_POLICY_NAME}-{MAX_POLICY_NAME} \
                     printable characters, got {length} in {policy_name:?}"
                );
                assert!(
                    generated_ids.insert(generated),
                    "case {case:?}: every policy of a plan needs its own generated id, \
                     got {names:?}"
                );
            }

            // A read does not re-mint the names, and neither does an update that leaves
            // the resources and their metrics where they were.
            let again = policy_names(&emulator, plan).await;
            assert_eq!(
                again, names,
                "case {case:?}: two reads of the same state report the same names"
            );
            update_plan(&emulator, plan, instructions).await;
            let after = policy_names(&emulator, plan).await;
            assert_eq!(
                after, names,
                "case {case:?}: an update that re-sends the instruction set leaves the \
                 derived names alone, got {after:?} after {names:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_plan_name_a_policy_carries_is_the_plan_it_belongs_to() {
    // The same instruction set under two plan names: each policy is named after ITS
    // plan, so a client filtering by the prefix of one plan finds none of the other's.
    let instructions = vec![table_instruction(
        vec![predefined("DynamoDBReadCapacityUtilization", 50.0)],
        json!({}),
    )];
    let emulator = Emulator::new();
    for (index, plan) in ["plan-one", "plan-two"].iter().enumerate() {
        emulator
            .ok(
                "CreateScalingPlan",
                json!({
                    "ScalingPlanName": plan,
                    "ApplicationSource": stack_source(index + 1),
                    "ScalingInstructions": instructions.clone(),
                }),
            )
            .await;
    }
    emulator.make_all_due();

    let first = policy_names(&emulator, "plan-one").await;
    let second = policy_names(&emulator, "plan-two").await;
    for (plan, names, other) in [
        ("plan-one", &first, &second),
        ("plan-two", &second, &first),
    ] {
        let prefix = format!("{POLICY_NAME_PREFIX}{plan}-");
        assert!(
            names.iter().all(|name| name.starts_with(&prefix)),
            "case {plan:?}: every policy of a plan is named {prefix:?} plus a generated \
             id, got {names:?}"
        );
        assert!(
            other.iter().all(|name| !name.starts_with(&prefix)),
            "case {plan:?}: no policy of another plan may carry {prefix:?}, got {other:?}"
        );
    }
    // Two plans over one instruction set: the generated id identifies the policy inside
    // its plan, so the names differ in the plan segment and nowhere else matters.
    assert_ne!(
        first, second,
        "two plans over one instruction set must report two sets of policy names, got \
         {first:?} and {second:?}"
    );
}

/// A DynamoDB table instruction for the `n`-th distinct table: "Each scaling instruction
/// applies to one resource", so a plan holding many instructions holds many resources.
fn nth_table_instruction(n: usize) -> Value {
    json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": format!("table/table-{n}"),
        "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
        "MinCapacity": 1,
        "MaxCapacity": 10,
        "TargetTrackingConfigurations": [predefined("DynamoDBReadCapacityUtilization", 50.0)]
    })
}

#[tokio::test]
async fn a_plan_holds_at_most_the_scaling_instructions_this_service_admits() {
    // One plan is bounded, so the instruction set a write path carries is refused past
    // that bound on BOTH write paths, with the one refusal this crate words a list past
    // its bound with. The counts span the bound: well inside it, one below it, exactly on
    // it, one past it, and far past it.
    let refusal = entry_count_refusal(SCALING_INSTRUCTION_ENTRIES, MAX_SCALING_INSTRUCTIONS);
    for count in [
        1,
        MAX_SCALING_INSTRUCTIONS - 1,
        MAX_SCALING_INSTRUCTIONS,
        MAX_SCALING_INSTRUCTIONS + 1,
        MAX_SCALING_INSTRUCTIONS * 3,
    ] {
        let instructions: Vec<Value> = (0..count).map(nth_table_instruction).collect();
        let emulator = Emulator::new();
        applied_plan(
            &emulator,
            "stored-plan",
            vec![nth_table_instruction(MAX_SCALING_INSTRUCTIONS * 4)],
        )
        .await;

        let requests: Vec<(&str, Value)> = vec![
            (
                "CreateScalingPlan",
                json!({
                    "ScalingPlanName": "a-new-plan",
                    "ApplicationSource": stack_source(2),
                    "ScalingInstructions": instructions.clone(),
                }),
            ),
            (
                "UpdateScalingPlan",
                json!({
                    "ScalingPlanName": "stored-plan",
                    "ScalingPlanVersion": 1,
                    "ScalingInstructions": instructions.clone(),
                }),
            ),
        ];
        for (operation, request) in requests {
            let case = format!("{operation} with {count} instructions");
            let (status, body) = emulator.call(operation, &request).await;
            if count > MAX_SCALING_INSTRUCTIONS {
                assert_eq!(
                    (status, error_code(&body), error_message(&body)),
                    (400, "ValidationException", refusal.as_str()),
                    "case {case:?}: a list past the bound is refused with {refusal:?}, got \
                     {status} with {body}"
                );
                continue;
            }
            assert_eq!(
                status, 200,
                "case {case:?}: a list inside the bound is served, got {status} with {body}"
            );
            emulator.make_all_due();
            // A list at its bound is STORED rather than merely not refused.
            let name = if operation == "CreateScalingPlan" {
                "a-new-plan"
            } else {
                "stored-plan"
            };
            let plan = described(&emulator, name).await;
            assert_eq!(
                plan["ScalingInstructions"].as_array().map(Vec::len),
                Some(count),
                "case {case:?}: every instruction of an accepted list must be in force, got \
                 {plan:?}"
            );
        }
    }
}
