// SPDX-License-Identifier: CC0-1.0

//! A scaling plan's SCALABLE TARGET registration is withdrawn when the plan stops covering
//! the resource — through the WHOLE mounted service set, the one `core/router/src/lib.rs`
//! assembles for the binary.
//!
//! The RULE under test: **a scaling plan registers the scalable target each of its scaling
//! instructions describes, so a resource the instruction set in force no longer covers —
//! one instruction dropped by an update, every instruction by a delete — has that
//! registration withdrawn along with the policies derived from it, and Application Auto
//! Scaling then reports neither a scalable target nor a scaling policy for it; a resource
//! the set still covers keeps both.**
//!
//! Three services have to agree for that to hold: AWS Auto Scaling registers and
//! deregisters, Application Auto Scaling holds the registration and reports it, and
//! DynamoDB holds the table whose provisioned capacity the target scales. The withdrawal
//! AWS Auto Scaling makes is an in-process call addressed the way a client's own
//! `application-autoscaling`-scoped call is addressed, so WHICH mounted model claims it is
//! only decidable with every service mounted. The plan's own diff rule — which resources
//! an instruction set registers, and which a replacement set drops — is pinned over tables
//! beside the implementation, in
//! `services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/src/state/application_auto_scaling.rs`;
//! what only this suite can reach is the registration where it lives.
//!
//! The harness is the sibling suite's
//! (`all/autoscaling_plans_policies_reach_application_auto_scaling.rs`), which pins what a
//! plan INSTALLS over the same three services: the two suites address the services, build
//! an instruction and wait for a plan to be applied through one set of helpers, so they
//! cannot drift apart about how a plan is driven.
//!
//! The rows differ along what the rule generalises over: whether the dropped resource
//! differs from the kept one in its scalable DIMENSION (two halves of one table's
//! provisioned capacity) or in its RESOURCE (two tables), and which metric each tracks.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::autoscaling_plans_policies_reach_application_auto_scaling::{
    applied, application_auto_scaling, given_provisioned_table, held_policies, instruction, plans,
    rpc, CREATED_MESSAGE, LOOK_AGAIN, MAX_CAPACITY, MIN_CAPACITY, PLANS_SIGNING_NAME,
    PLANS_TARGET_PREFIX, SETTLE_DEADLINE, UPDATED_MESSAGE,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router;

/// The namespace every instruction here names: the service whose scalable targets
/// Application Auto Scaling holds for a DynamoDB table.
const NAMESPACE: &str = "dynamodb";

/// The media type an `awsJson1_1` request carries, for the one call here that is expected
/// to be REFUSED and so cannot go through the harness's status-asserting helper.
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// One scalable resource a plan's instruction covers: the table it is on, the dimension it
/// scales and the metric it tracks.
#[derive(Debug, Clone, Copy)]
struct Resource {
    table: &'static str,
    dimension: &'static str,
    metric: &'static str,
}

impl Resource {
    /// The `ResourceId` Application Auto Scaling knows this resource by.
    fn resource_id(&self) -> String {
        format!("table/{}", self.table)
    }
}

/// The error code an `awsJson1_1` refusal carries, as its `__type` names it.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Poll `DescribeScalingPlans` until the plan `name` is no longer reported.
async fn gone(router: &MultiServiceRouter, case: &str, name: &str) {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < SETTLE_DEADLINE {
        last = plans(
            router,
            case,
            "DescribeScalingPlans",
            &json!({ "ScalingPlanNames": [name] }),
        )
        .await;
        if last["ScalingPlans"]
            .as_array()
            .is_some_and(|reported| reported.is_empty())
        {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the deleted plan {name:?} is still reported, last {last}");
}

/// The scalable targets Application Auto Scaling itself holds for one scalable resource.
async fn held_targets(router: &MultiServiceRouter, case: &str, resource: &Resource) -> Vec<Value> {
    let body = application_auto_scaling(
        router,
        case,
        "DescribeScalableTargets",
        &json!({
            "ServiceNamespace": NAMESPACE,
            "ResourceIds": [resource.resource_id()],
            "ScalableDimension": resource.dimension,
        }),
    )
    .await;
    body["ScalableTargets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Poll Application Auto Scaling until it holds exactly `targets` scalable targets and
/// `policies` scaling policies for `resource`.
async fn held_until(
    router: &MultiServiceRouter,
    case: &str,
    resource: &Resource,
    targets: usize,
    policies: usize,
) {
    let started = Instant::now();
    let mut last = (Vec::new(), Vec::new());
    while started.elapsed() < SETTLE_DEADLINE {
        last = (
            held_targets(router, case, resource).await,
            held_policies(router, case, resource.table, resource.dimension).await,
        );
        if last.0.len() == targets && last.1.len() == policies {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: Application Auto Scaling never held {targets} scalable target(s) and \
         {policies} policy(ies) for {} {}, last {last:?}",
        resource.resource_id(),
        resource.dimension
    );
}

/// One instruction over `resource`, tracking its metric to `target` inside the capacity
/// window the harness states.
fn covering(resource: &Resource, target: f64) -> Value {
    instruction(
        resource.table,
        resource.dimension,
        resource.metric,
        target,
        MIN_CAPACITY,
        MAX_CAPACITY,
    )
}

#[tokio::test]
async fn a_resource_a_plan_stops_covering_loses_its_registration() {
    // (case, the resource the plan keeps covering, the resource it drops)
    let rows: &[(&str, Resource, Resource)] = &[
        (
            "two halves of one table's provisioned capacity",
            Resource {
                table: "plan-withdrawn-one",
                dimension: "dynamodb:table:ReadCapacityUnits",
                metric: "DynamoDBReadCapacityUtilization",
            },
            Resource {
                table: "plan-withdrawn-one",
                dimension: "dynamodb:table:WriteCapacityUnits",
                metric: "DynamoDBWriteCapacityUtilization",
            },
        ),
        (
            "the same dimension of two different tables",
            Resource {
                table: "plan-withdrawn-kept",
                dimension: "dynamodb:table:WriteCapacityUnits",
                metric: "DynamoDBWriteCapacityUtilization",
            },
            Resource {
                table: "plan-withdrawn-dropped",
                dimension: "dynamodb:table:WriteCapacityUnits",
                metric: "DynamoDBWriteCapacityUtilization",
            },
        ),
    ];

    for (case, kept, dropped) in rows {
        let router = router();
        let plan = "withdrawal-scaling-plan";
        given_provisioned_table(&router, case, kept.table).await;
        if dropped.table != kept.table {
            given_provisioned_table(&router, case, dropped.table).await;
        }

        plans(
            &router,
            case,
            "CreateScalingPlan",
            &json!({
                "ScalingPlanName": plan,
                "ApplicationSource": { "TagFilters": [{ "Key": "suite", "Values": [plan] }] },
                "ScalingInstructions": [covering(kept, 55.0), covering(dropped, 45.0)],
            }),
        )
        .await;
        applied(&router, case, plan, CREATED_MESSAGE).await;

        // Both resources the plan covers carry a registration with the instruction's own
        // capacity window, and the policy derived from it.
        for resource in [kept, dropped] {
            held_until(&router, case, resource, 1, 1).await;
            let targets = held_targets(&router, case, resource).await;
            assert_eq!(
                (
                    targets[0]["MinCapacity"].as_i64(),
                    targets[0]["MaxCapacity"].as_i64()
                ),
                (Some(MIN_CAPACITY), Some(MAX_CAPACITY)),
                "case {case:?}: the plan must register the capacity window its instruction \
                 states for {} {}, got {targets:?}",
                resource.resource_id(),
                resource.dimension
            );
        }

        // An update that drops one instruction withdraws THAT resource's registration and
        // its policy, and leaves the resource still covered holding both.
        plans(
            &router,
            case,
            "UpdateScalingPlan",
            &json!({
                "ScalingPlanName": plan,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": [covering(kept, 65.0)],
            }),
        )
        .await;
        applied(&router, case, plan, UPDATED_MESSAGE).await;
        held_until(&router, case, dropped, 0, 0).await;
        held_until(&router, case, kept, 1, 1).await;

        // Deleting the plan takes the whole registration with it: the resource stops being
        // one Application Auto Scaling manages at all.
        plans(
            &router,
            case,
            "DeleteScalingPlan",
            &json!({ "ScalingPlanName": plan, "ScalingPlanVersion": 1 }),
        )
        .await;
        held_until(&router, case, kept, 0, 0).await;
        gone(&router, case, plan).await;

        // …and the plan itself is gone, so a mutation of it is refused rather than
        // re-installing anything.
        let (status, body) = rpc(
            &router,
            PLANS_SIGNING_NAME,
            PLANS_TARGET_PREFIX,
            AWS_JSON_1_1,
            "UpdateScalingPlan",
            &json!({
                "ScalingPlanName": plan,
                "ScalingPlanVersion": 1,
                "ScalingInstructions": [covering(kept, 65.0)],
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ObjectNotFoundException"),
            "case {case:?}: an update of the deleted plan must be refused, got {status} with \
             {body}"
        );
        held_until(&router, case, kept, 0, 0).await;
    }
}
