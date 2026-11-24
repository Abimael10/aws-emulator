// SPDX-License-Identifier: CC0-1.0

//! The target tracking scaling policies a scaling plan reports are the ones Application
//! Auto Scaling holds — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **a scaling plan's target tracking scaling policy is an
//! Application Auto Scaling resource, so the policy AWS Auto Scaling reports for a plan's
//! resource is one that service's own `DescribeScalingPolicies` reports, under the name
//! the plan derives for it (`AutoScaling-<ScalingPlanName>-<generated id>`), tracking the
//! configuration the instruction in force states — and an `UpdateScalingPlan` that moves
//! what the plan applies to that resource, or a `DeleteScalingPlan` that removes it, moves
//! that same record.**
//!
//! What an update moves and what it leaves alone is the plan's own rule, pinned over
//! tables in
//! `services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/tests/all/update_keeps_an_applied_policys_target_value.rs`:
//! the update's instruction set is the one in force, member for member, the target value
//! of a metric the plan already tracks included. So the update below moves both the
//! capacity window the plan registers and the value its policy tracks to, and what this
//! suite reaches is that both halves land where the policy and the scalable target really
//! live.
//!
//! Three services have to agree for that to hold: AWS Auto Scaling writes the policy,
//! Application Auto Scaling holds it and reports it, and DynamoDB holds the table whose
//! provisioned capacity the scalable target scales. The write AWS Auto Scaling makes is an
//! in-process call addressed the way a client's own `application-autoscaling`-scoped call
//! is addressed, so WHICH mounted model claims it is only decidable with every service
//! mounted;
//! `services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/tests/all/update_replaces_the_instruction_set.rs`
//! pins the plan's own rules — what an update puts in force, and how a policy is named —
//! over tables of instructions, and what only this suite can reach is the policy where it
//! lives.
//!
//! The rows differ along what the rule generalises over: which half of a table's
//! provisioned capacity the instruction scales, the metric it tracks, the target value the
//! create states, the value the update re-points it to, and the capacity window the update
//! moves it to.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};

/// The Region the whole wiring lives in: a table name resolves inside one Region, so the
/// plan, its scalable target and its policies live there too.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefixes of the three services this suite drives, and the signing
/// name each is addressed by.
pub(crate) const PLANS_TARGET_PREFIX: &str = "AnyScaleScalingPlannerFrontendService";
pub(crate) const PLANS_SIGNING_NAME: &str = "autoscaling-plans";
const APP_SCALING_TARGET_PREFIX: &str = "AnyScaleFrontendService";
const APP_SCALING_SIGNING_NAME: &str = "application-autoscaling";
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";
const DYNAMODB_SIGNING_NAME: &str = "dynamodb";

/// The literal prefix of the name a plan derives for a policy it installs, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make this
/// suite pass.
const POLICY_NAME_PREFIX: &str = "AutoScaling-";

/// The one `PolicyType` a scaling plan installs.
const TARGET_TRACKING_SCALING: &str = "TargetTrackingScaling";

/// The narratives a plan reports once its create, and once an update of it, has been
/// applied.
pub(crate) const CREATED_MESSAGE: &str =
    "Scaling plan has been created and applied to all resources.";
pub(crate) const UPDATED_MESSAGE: &str = "Scaling plan has been updated successfully.";

/// The capacity window every instruction here states, and the wider one its update moves
/// to — a change the plan APPLIES to the resource, which is what makes the update
/// observable both on the plan's narrative and on the scalable target.
pub(crate) const MIN_CAPACITY: i64 = 1;
pub(crate) const MAX_CAPACITY: i64 = 10;
const UPDATED_MIN_CAPACITY: i64 = 2;
const UPDATED_MAX_CAPACITY: i64 = 20;

/// How long a lifecycle step is given to come due, polled rather than waited out: the
/// deadline is generous only so a failure is a failure rather than a hang.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One `X-Amz-Target` RPC call, answering the status and the parsed body.
pub(crate) async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One AWS Auto Scaling (`autoscaling-plans`) call that must answer 200.
pub(crate) async fn plans(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        PLANS_SIGNING_NAME,
        PLANS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from AWS Auto Scaling {operation} {body}, got \
         {status} with {parsed}"
    );
    parsed
}

/// One Application Auto Scaling call that must answer 200.
pub(crate) async fn application_auto_scaling(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        APP_SCALING_SIGNING_NAME,
        APP_SCALING_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Application Auto Scaling {operation} {body}, \
         got {status} with {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call that must answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        DYNAMODB_SIGNING_NAME,
        DYNAMODB_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation} {body}, got {status} \
         with {parsed}"
    );
    parsed
}

/// A table whose throughput is PROVISIONED — the only mode with a provisioned value for a
/// scalable target to scale — polled until it reports `ACTIVE`.
pub(crate) async fn given_provisioned_table(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) {
    dynamodb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": 1},
        }),
    )
    .await;
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < SETTLE_DEADLINE {
        last = dynamodb(router, case, "DescribeTable", &json!({"TableName": name})).await;
        if last["Table"]["TableStatus"] == json!("ACTIVE") {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the table {name:?} never reached ACTIVE, last {last}");
}

/// One scaling instruction over `table`'s `dimension`, tracking `metric` to `target`,
/// with the capacity window `min`..`max`.
pub(crate) fn instruction(
    table: &str,
    dimension: &str,
    metric: &str,
    target: f64,
    min: i64,
    max: i64,
) -> Value {
    json!({
        "ServiceNamespace": "dynamodb",
        "ResourceId": format!("table/{table}"),
        "ScalableDimension": dimension,
        "MinCapacity": min,
        "MaxCapacity": max,
        "TargetTrackingConfigurations": [{
            "PredefinedScalingMetricSpecification": { "PredefinedScalingMetricType": metric },
            "TargetValue": target
        }]
    })
}

/// The one plan `name`, as `DescribeScalingPlans` reports it.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let body = plans(
        router,
        case,
        "DescribeScalingPlans",
        &json!({ "ScalingPlanNames": [name] }),
    )
    .await;
    body["ScalingPlans"]
        .as_array()
        .and_then(|plans| plans.first().cloned())
        .unwrap_or(Value::Null)
}

/// Poll the plan `name` until it reports `Active` with `message`, answering what it
/// reported then.
pub(crate) async fn applied(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    message: &str,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < SETTLE_DEADLINE {
        last = described(router, case, name).await;
        if last["StatusCode"] == json!("Active") && last["StatusMessage"] == json!(message) {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the plan {name:?} never reported Active with {message:?}, last {last}");
}

/// The `ScalingPolicies` the plan `name` reports for its one resource.
async fn reported_policies(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let body = plans(
        router,
        case,
        "DescribeScalingPlanResources",
        &json!({ "ScalingPlanName": name, "ScalingPlanVersion": 1 }),
    )
    .await;
    let resources = body["ScalingPlanResources"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ScalingPlanResources in {body}"));
    assert_eq!(
        resources.len(),
        1,
        "case {case:?}: the plan covers one resource, got {resources:?}"
    );
    resources[0]["ScalingPolicies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The policies Application Auto Scaling itself holds for one scalable resource.
pub(crate) async fn held_policies(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    dimension: &str,
) -> Vec<Value> {
    let body = application_auto_scaling(
        router,
        case,
        "DescribeScalingPolicies",
        &json!({
            "ServiceNamespace": "dynamodb",
            "ResourceId": format!("table/{table}"),
            "ScalableDimension": dimension,
        }),
    )
    .await;
    body["ScalingPolicies"].as_array().cloned().unwrap_or_default()
}

/// Poll Application Auto Scaling until it holds exactly `wanted` policies for the
/// resource, answering what it held then.
async fn held_until(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    dimension: &str,
    wanted: usize,
) -> Vec<Value> {
    let started = Instant::now();
    let mut last = Vec::new();
    while started.elapsed() < SETTLE_DEADLINE {
        last = held_policies(router, case, table, dimension).await;
        if last.len() == wanted {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: Application Auto Scaling never held {wanted} policies for \
         table/{table} {dimension}, last {last:?}"
    );
}

#[tokio::test]
async fn a_plans_target_tracking_policy_is_held_by_application_auto_scaling() {
    // (case, the dimension the instruction scales, the metric it tracks, the target value
    //  the create states, the target value the update re-points the policy to)
    let rows: &[(&str, &str, &str, f64, f64)] = &[
        (
            "the read half of a table's provisioned capacity",
            "dynamodb:table:ReadCapacityUnits",
            "DynamoDBReadCapacityUtilization",
            50.0,
            75.0,
        ),
        (
            "the write half, tracked to another value and lowered by the update",
            "dynamodb:table:WriteCapacityUnits",
            "DynamoDBWriteCapacityUtilization",
            40.0,
            12.5,
        ),
    ];

    for (case, dimension, metric, created_target, updated_target) in rows {
        let router = router();
        let table = "plan-scaled-table";
        let plan = "router-scaling-plan";
        given_provisioned_table(&router, case, table).await;

        plans(
            &router,
            case,
            "CreateScalingPlan",
            &json!({
                "ScalingPlanName": plan,
                "ApplicationSource": { "TagFilters": [{ "Key": "suite", "Values": [plan] }] },
                "ScalingInstructions": [instruction(
                    table,
                    dimension,
                    metric,
                    *created_target,
                    MIN_CAPACITY,
                    MAX_CAPACITY,
                )],
            }),
        )
        .await;
        applied(&router, case, plan, CREATED_MESSAGE).await;

        // What the plan reports: one derived policy, named after the plan.
        let reported = reported_policies(&router, case, plan).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: the applied plan must report its one policy, got {reported:?}"
        );
        let policy_name = reported[0]["PolicyName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let prefix = format!("{POLICY_NAME_PREFIX}{plan}-");
        assert!(
            policy_name.starts_with(&prefix),
            "case {case:?}: a derived policy must be named {prefix:?} plus a generated id, \
             got {policy_name:?}"
        );

        // …and that policy is the one Application Auto Scaling holds: same name, same
        // type, tracking the configuration the instruction states.
        let held = held_until(&router, case, table, dimension, 1).await;
        assert_eq!(
            held[0]["PolicyName"],
            json!(policy_name),
            "case {case:?}: Application Auto Scaling must hold the policy the plan \
             reports, got {held:?}"
        );
        assert_eq!(
            held[0]["PolicyType"],
            json!(TARGET_TRACKING_SCALING),
            "case {case:?}: a plan installs target tracking policies, got {held:?}"
        );
        let configuration = &held[0]["TargetTrackingScalingPolicyConfiguration"];
        assert_eq!(
            (
                configuration["TargetValue"].as_f64(),
                configuration["PredefinedMetricSpecification"]["PredefinedMetricType"].clone()
            ),
            (Some(*created_target), json!(metric)),
            "case {case:?}: the held policy must track the configuration in force, got \
             {held:?}"
        );
        // The scalable target the policy moves is registered with the instruction's own
        // capacity window.
        let targets = application_auto_scaling(
            &router,
            case,
            "DescribeScalableTargets",
            &json!({
                "ServiceNamespace": "dynamodb",
                "ResourceIds": [format!("table/{table}")],
                "ScalableDimension": dimension,
            }),
        )
        .await;
        assert_eq!(
            (
                targets["ScalableTargets"][0]["MinCapacity"].as_i64(),
                targets["ScalableTargets"][0]["MaxCapacity"].as_i64()
            ),
            (Some(MIN_CAPACITY), Some(MAX_CAPACITY)),
            "case {case:?}: the plan must register the capacity window its instruction \
             states, got {targets}"
        );

        // An update that moves what the plan applies to the resource — here both the
        // value its policy tracks to and the capacity window its scalable target is
        // registered with — moves the very same records: the policy keeps its name and
        // reports the value the update sent, and the scalable target reports the new
        // window.
        plans(
            &router,
            case,
            "UpdateScalingPlan",
            &json!({
                "ScalingPlanName": plan,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": [instruction(
                    table,
                    dimension,
                    metric,
                    *updated_target,
                    UPDATED_MIN_CAPACITY,
                    UPDATED_MAX_CAPACITY,
                )],
            }),
        )
        .await;
        let updated = applied(&router, case, plan, UPDATED_MESSAGE).await;
        assert_eq!(
            updated["ScalingPlanVersion"].as_i64(),
            Some(1),
            "case {case:?}: an in-place update keeps the plan's version, got {updated}"
        );

        let held = held_until(&router, case, table, dimension, 1).await;
        let configuration = &held[0]["TargetTrackingScalingPolicyConfiguration"];
        assert_eq!(
            (held[0]["PolicyName"].clone(), configuration["TargetValue"].as_f64()),
            (json!(policy_name), Some(*updated_target)),
            "case {case:?}: the update must re-point the policy it installed, not replace \
             it, got {held:?}"
        );
        // The plan's own read-backs report the same value the policy holds: the plan's
        // instruction and the policy it derives for the resource cannot disagree.
        assert_eq!(
            updated["ScalingInstructions"][0]["TargetTrackingConfigurations"][0]["TargetValue"]
                .as_f64(),
            Some(*updated_target),
            "case {case:?}: the updated plan must report the target value it was sent, \
             got {updated}"
        );
        let reported = reported_policies(&router, case, plan).await;
        assert_eq!(
            (
                reported[0]["PolicyName"].clone(),
                reported[0]["TargetTrackingConfiguration"]["TargetValue"].as_f64()
            ),
            (json!(policy_name), Some(*updated_target)),
            "case {case:?}: the plan's resource must report the re-pointed policy under \
             the name it had, got {reported:?}"
        );
        let targets = application_auto_scaling(
            &router,
            case,
            "DescribeScalableTargets",
            &json!({
                "ServiceNamespace": "dynamodb",
                "ResourceIds": [format!("table/{table}")],
                "ScalableDimension": dimension,
            }),
        )
        .await;
        assert_eq!(
            (
                targets["ScalableTargets"][0]["MinCapacity"].as_i64(),
                targets["ScalableTargets"][0]["MaxCapacity"].as_i64()
            ),
            (Some(UPDATED_MIN_CAPACITY), Some(UPDATED_MAX_CAPACITY)),
            "case {case:?}: the update must register the capacity window its instruction \
             states, got {targets}"
        );

        // Deleting the plan deletes the policy it installed: nothing is left tracking the
        // metric for a plan that is gone.
        plans(
            &router,
            case,
            "DeleteScalingPlan",
            &json!({ "ScalingPlanName": plan, "ScalingPlanVersion": 1 }),
        )
        .await;
        let left = held_until(&router, case, table, dimension, 0).await;
        assert!(
            left.is_empty(),
            "case {case:?}: a deleted plan leaves no policy behind, got {left:?}"
        );
    }
}
