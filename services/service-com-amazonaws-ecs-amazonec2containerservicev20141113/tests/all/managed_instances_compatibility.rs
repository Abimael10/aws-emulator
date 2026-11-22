// SPDX-License-Identifier: CC0-1.0

//! Managed-instances capacity among the launch types a registered revision reports, read back
//! through both projections of the stored revision.
//!
//! The rule under test: `TaskDefinition$compatibilities` ("Amazon ECS validates the task
//! definition parameters with those supported by the launch type") is DERIVED from the stored
//! revision's own parameters over the whole `Compatibility` enum
//! (`smithy_query(query="shape",shape="com.amazonaws.ecs#Compatibility")` -> `EC2`, `FARGATE`,
//! `EXTERNAL`, `MANAGED_INSTANCES`), reported in the order recorded ECS projections carry —
//! `EXTERNAL`, `EC2`, `MANAGED_INSTANCES`, `FARGATE` — and Amazon ECS Managed Instances run a
//! task on EC2 capacity Amazon ECS provisions and operates, so every revision the caller's own
//! EC2 capacity supports is validated against that launch type too, whatever network mode it
//! declares and whether or not the registration asked to be validated against it.
//!
//! The rows span what the rule generalises over:
//!
//! * the NETWORK MODE, which is what decides the launch types around managed instances: the
//!   documented default, each member of the `NetworkMode` enum, and `awsvpc` with and without
//!   a task size;
//! * what the registration ASKED to be validated against (`requiresCompatibilities`): nothing,
//!   managed instances alone, and managed instances beside another launch type — none of which
//!   changes the derived list;
//! * a revision Fargate does NOT support (an Amazon ECR image with no `executionRoleArn`),
//!   where managed instances are still reported because the caller's own capacity can pull it;
//! * and both projections of the same stored revision — the registration's echo and
//!   `DescribeTaskDefinition` — so a list rendered off the request rather than off the record
//!   shows up as a difference between them.
//!
//! The POSITION of each launch type is asserted, not just its membership: a client that reads
//! the list positionally must name the same launch type the real service named.

use serde_json::{json, Value};

use crate::ecs_wire::{list, ok, service};
use crate::task_lifecycle::container;

/// A public image every container here starts from, which needs no principal to pull.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:latest";

/// An image held in this account's own Amazon ECR registry, which Amazon ECS pulls AS the task
/// execution role ("the task execution role that grants the Amazon ECS container agent
/// permission to make Amazon Web Services API calls on your behalf").
const ECR_IMAGE: &str = "000000000000.dkr.ecr.us-west-2.amazonaws.com/app:latest";

/// A legal Fargate task size, from `RegisterTaskDefinition$memory`'s documented cpu/memory
/// pairs.
const TASK_CPU: &str = "512";
const TASK_MEMORY: &str = "1024";

/// The `compatibilities` of one projection, in the order it reports them.
fn compatibilities(definition: &Value, case: &str) -> Vec<String> {
    list(definition, "compatibilities", case)
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a string launch type, got {value}")
                })
                .to_string()
        })
        .collect()
}

/// Register `request` under `family` and read the revision back, answering what each
/// projection reported.
async fn register_and_describe(
    router: &smithy_server_runtime::MultiServiceRouter,
    family: &str,
    mut request: Value,
    case: &str,
) -> Vec<(&'static str, Value)> {
    request["family"] = json!(family);
    let registered = ok(router, "RegisterTaskDefinition", request, case).await;
    let described = ok(
        router,
        "DescribeTaskDefinition",
        json!({ "taskDefinition": family }),
        case,
    )
    .await;
    vec![
        (
            "RegisterTaskDefinition",
            registered["taskDefinition"].clone(),
        ),
        ("DescribeTaskDefinition", described["taskDefinition"].clone()),
    ]
}

/// RULE: managed-instances capacity is reported wherever the caller's own EC2 capacity is, in
/// the position the launch-type order gives it, through both projections of the revision.
#[tokio::test]
async fn managed_instances_is_derived_wherever_ec2_capacity_is() {
    let router = service();

    // (case, the request beside the family and containers, the launch types the revision must
    // report in order).
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "the documented default network mode",
            json!({}),
            vec!["EXTERNAL", "EC2", "MANAGED_INSTANCES"],
        ),
        (
            "bridge",
            json!({"networkMode": "bridge"}),
            vec!["EXTERNAL", "EC2", "MANAGED_INSTANCES"],
        ),
        (
            "host",
            json!({"networkMode": "host"}),
            vec!["EXTERNAL", "EC2", "MANAGED_INSTANCES"],
        ),
        (
            "none",
            json!({"networkMode": "none"}),
            vec!["EXTERNAL", "EC2", "MANAGED_INSTANCES"],
        ),
        (
            "awsvpc without a task size",
            json!({"networkMode": "awsvpc"}),
            vec!["EC2", "MANAGED_INSTANCES"],
        ),
        (
            "awsvpc with a task size",
            json!({"networkMode": "awsvpc", "cpu": TASK_CPU, "memory": TASK_MEMORY}),
            vec!["EC2", "MANAGED_INSTANCES", "FARGATE"],
        ),
        (
            "asking to be validated against managed instances alone",
            json!({"networkMode": "bridge", "requiresCompatibilities": ["MANAGED_INSTANCES"]}),
            vec!["EXTERNAL", "EC2", "MANAGED_INSTANCES"],
        ),
        (
            "asking to be validated against Fargate and managed instances",
            json!({
                "networkMode": "awsvpc",
                "cpu": TASK_CPU,
                "memory": TASK_MEMORY,
                "requiresCompatibilities": ["FARGATE", "MANAGED_INSTANCES"],
            }),
            vec!["EC2", "MANAGED_INSTANCES", "FARGATE"],
        ),
    ];

    for (index, (case, mut request, expected)) in rows.into_iter().enumerate() {
        request["containerDefinitions"] = json!([container("app", IMAGE)]);
        for (source, definition) in
            register_and_describe(&router, &format!("mifam{index}"), request, case).await
        {
            let actual = compatibilities(&definition, case);
            assert_eq!(
                actual, expected,
                "case {case:?}: {source} expected compatibilities {expected:?}, got {actual:?} \
                 in {definition}"
            );
        }
    }
}

/// RULE, where FARGATE drops out: a revision whose image can be pulled only AS the task
/// execution role, and that names none, is not validated against Fargate — Amazon ECS's own
/// capacity has no container instance whose role the agent could pull under — while the
/// caller's own capacity, managed instances included, still supports it. Naming the execution
/// role puts Fargate back, and managed instances stay either way.
#[tokio::test]
async fn an_image_needing_a_principal_drops_fargate_and_keeps_managed_instances() {
    let router = service();

    // (case, the executionRoleArn the registration names, the launch types it must report).
    let rows: Vec<(&str, Option<&str>, Vec<&str>)> = vec![
        (
            "an Amazon ECR image with no execution role",
            None,
            vec!["EC2", "MANAGED_INSTANCES"],
        ),
        (
            "the same image with an execution role",
            Some("arn:aws:iam::000000000000:role/ecsTaskExecutionRole"),
            vec!["EC2", "MANAGED_INSTANCES", "FARGATE"],
        ),
    ];

    for (index, (case, execution_role, expected)) in rows.into_iter().enumerate() {
        let mut request = json!({
            "networkMode": "awsvpc",
            "cpu": TASK_CPU,
            "memory": TASK_MEMORY,
            "containerDefinitions": [container("app", ECR_IMAGE)],
        });
        if let Some(role) = execution_role {
            request["executionRoleArn"] = json!(role);
        }
        for (source, definition) in
            register_and_describe(&router, &format!("mipull{index}"), request, case).await
        {
            let actual = compatibilities(&definition, case);
            assert_eq!(
                actual, expected,
                "case {case:?}: {source} expected compatibilities {expected:?}, got {actual:?} \
                 in {definition}"
            );
        }
    }
}
