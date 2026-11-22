// SPDX-License-Identifier: CC0-1.0

//! Managed-instances capacity among the launch types a registered revision reports, read back
//! through both projections of the stored revision.
//!
//! The rule under test: `TaskDefinition$compatibilities` ("Amazon ECS validates the task
//! definition parameters with those supported by the launch type") is DERIVED over the whole
//! `Compatibility` enum (`smithy_query(query="shape", shape="com.amazonaws.ecs#Compatibility")`
//! -> `EC2`, `FARGATE`, `EXTERNAL`, `MANAGED_INSTANCES`) from the revision's own parameters and
//! from the classes of capacity the account can place a task on at all — and managed instances
//! are the one class no account holds by default. That capacity exists only as a capacity
//! provider somebody created for a cluster (`CapacityProvider$cluster`: "Managed instances
//! capacity providers are cluster-scoped"), and a placement reaches it only through such a
//! provider (`RunTask$launchType`: "If you want to use Amazon ECS Managed Instances, you must
//! use the `capacityProviderStrategy` request parameter and omit the `launchType` request
//! parameter"), while the caller's own container instances (`EC2`, `EXTERNAL`) and the Fargate
//! providers Amazon ECS holds for every account need no such create. So an account holding no
//! managed-instances provider reports NO revision as validated against that launch type,
//! whatever the revision declares, and an account holding one reports it wherever the caller's
//! own EC2 capacity is reported — in the position the launch-type order gives it (`EXTERNAL`,
//! `EC2`, `MANAGED_INSTANCES`, `FARGATE`).
//!
//! The rows span what the rule generalises over:
//!
//! * the NETWORK MODE, which is what decides the launch types around managed instances: the
//!   documented default, each member of the `NetworkMode` enum, and `awsvpc` with and without
//!   a task size;
//! * what the registration ASKED to be validated against (`requiresCompatibilities`): nothing,
//!   managed instances alone, and managed instances beside another launch type — none of which
//!   changes the derived list either way;
//! * whether the ACCOUNT holds managed-instances capacity, which is what the rule turns on,
//!   including a revision registered before that capacity was created;
//! * a revision Fargate does NOT support (an Amazon ECR image with no `executionRoleArn`),
//!   where the managed-instances answer is unchanged by the Fargate one;
//! * and both projections of the same stored revision — the registration's echo and
//!   `DescribeTaskDefinition` — so a list rendered off the request rather than off the record
//!   shows up as a difference between them.
//!
//! The POSITION of each launch type is asserted, not just its membership: a client that reads
//! the list positionally must name the same launch type the real service named.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::managed_instances;
use crate::ecs_wire::{create_cluster, list, ok, service};
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

/// The launch type this suite is about.
const MANAGED_INSTANCES: &str = "MANAGED_INSTANCES";

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

/// The same expected list for an account that HOLDS managed-instances capacity: that launch
/// type in the position the launch-type order gives it, straight after the caller's own EC2
/// capacity and before Fargate.
fn with_managed_instances(expected: &[&str]) -> Vec<String> {
    let mut reported: Vec<String> = Vec::with_capacity(expected.len() + 1);
    for launch_type in expected {
        reported.push((*launch_type).to_string());
        if *launch_type == "EC2" {
            reported.push(MANAGED_INSTANCES.to_string());
        }
    }
    reported
}

/// Give the account Amazon ECS Managed Instances capacity: a cluster, and a capacity provider
/// of that class scoped to it — the only way an account comes to hold that capacity.
async fn create_managed_instances_capacity(
    router: &MultiServiceRouter,
    cluster: &str,
    provider: &str,
    case: &str,
) {
    create_cluster(router, cluster, case).await;
    ok(
        router,
        "CreateCapacityProvider",
        json!({
            "name": provider,
            "cluster": cluster,
            "managedInstancesProvider": managed_instances(),
        }),
        case,
    )
    .await;
}

/// Register `request` under `family` and read the revision back, answering what each
/// projection reported.
async fn register_and_describe(
    router: &MultiServiceRouter,
    family: &str,
    mut request: Value,
    case: &str,
) -> Vec<(&'static str, Value)> {
    request["family"] = json!(family);
    let registered = ok(router, "RegisterTaskDefinition", request, case).await;
    let described = describe(router, family, case).await;
    vec![
        (
            "RegisterTaskDefinition",
            registered["taskDefinition"].clone(),
        ),
        ("DescribeTaskDefinition", described),
    ]
}

/// The stored revision `reference` names, as `DescribeTaskDefinition` reports it.
async fn describe(router: &MultiServiceRouter, reference: &str, case: &str) -> Value {
    let described = ok(
        router,
        "DescribeTaskDefinition",
        json!({ "taskDefinition": reference }),
        case,
    )
    .await;
    described["taskDefinition"].clone()
}

/// (case, the request beside the family and containers, the launch types the revision reports
/// in an account holding NO managed-instances capacity).
fn rows() -> Vec<(&'static str, Value, Vec<&'static str>)> {
    vec![
        (
            "the documented default network mode",
            json!({}),
            vec!["EXTERNAL", "EC2"],
        ),
        (
            "bridge",
            json!({"networkMode": "bridge"}),
            vec!["EXTERNAL", "EC2"],
        ),
        (
            "host",
            json!({"networkMode": "host"}),
            vec!["EXTERNAL", "EC2"],
        ),
        (
            "none",
            json!({"networkMode": "none"}),
            vec!["EXTERNAL", "EC2"],
        ),
        (
            "awsvpc without a task size",
            json!({"networkMode": "awsvpc"}),
            vec!["EC2"],
        ),
        (
            "awsvpc with a task size",
            json!({"networkMode": "awsvpc", "cpu": TASK_CPU, "memory": TASK_MEMORY}),
            vec!["EC2", "FARGATE"],
        ),
        (
            "asking to be validated against managed instances alone",
            json!({"networkMode": "bridge", "requiresCompatibilities": ["MANAGED_INSTANCES"]}),
            vec!["EXTERNAL", "EC2"],
        ),
        (
            "asking to be validated against Fargate and managed instances",
            json!({
                "networkMode": "awsvpc",
                "cpu": TASK_CPU,
                "memory": TASK_MEMORY,
                "requiresCompatibilities": ["FARGATE", "MANAGED_INSTANCES"],
            }),
            vec!["EC2", "FARGATE"],
        ),
    ]
}

/// RULE: an account that holds no managed-instances capacity reports no revision as validated
/// against that launch type, whatever the revision declares and whatever it asked to be
/// validated against.
#[tokio::test]
async fn no_revision_reports_managed_instances_without_that_capacity() {
    let router = service();

    for (index, (case, mut request, expected)) in rows().into_iter().enumerate() {
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

/// RULE, once the capacity exists: a revision registered in an account that holds a
/// managed-instances capacity provider reports that launch type wherever the caller's own EC2
/// capacity is reported, in the position the launch-type order gives it.
#[tokio::test]
async fn every_revision_reports_managed_instances_once_that_capacity_exists() {
    let router = service();
    create_managed_instances_capacity(&router, "mifleetcluster", "mifleet", "setup").await;

    for (index, (case, mut request, without)) in rows().into_iter().enumerate() {
        request["containerDefinitions"] = json!([container("app", IMAGE)]);
        let expected = with_managed_instances(&without);
        for (source, definition) in
            register_and_describe(&router, &format!("miheld{index}"), request, case).await
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

/// RULE, over time: the derived list is the validation the REGISTRATION performed, so reading a
/// revision back never rewrites it — a revision registered before the account held
/// managed-instances capacity reports the same list after the provider exists, and the next
/// revision of the same family, registered against the account that now holds it, reports the
/// launch type.
#[tokio::test]
async fn the_derived_list_is_the_one_the_registration_validated() {
    let router = service();
    let case = "a revision registered before the capacity existed";
    let family = "mitimeline";
    let request = json!({
        "networkMode": "awsvpc",
        "cpu": TASK_CPU,
        "memory": TASK_MEMORY,
        "containerDefinitions": [container("app", IMAGE)],
    });

    let before = ok(
        &router,
        "RegisterTaskDefinition",
        {
            let mut first = request.clone();
            first["family"] = json!(family);
            first
        },
        case,
    )
    .await;
    let expected_before = vec!["EC2".to_string(), "FARGATE".to_string()];
    let actual = compatibilities(&before["taskDefinition"], case);
    assert_eq!(
        actual, expected_before,
        "case {case:?}: RegisterTaskDefinition expected compatibilities {expected_before:?}, got \
         {actual:?} in {before}"
    );

    create_managed_instances_capacity(&router, "milatercluster", "milater", case).await;

    let reread = describe(&router, &format!("{family}:1"), case).await;
    let actual = compatibilities(&reread, case);
    assert_eq!(
        actual, expected_before,
        "case {case:?}: DescribeTaskDefinition must report the validation the registration \
         performed, expected {expected_before:?}, got {actual:?} in {reread}"
    );

    let case = "the next revision, registered against an account holding the capacity";
    let expected_after = with_managed_instances(&["EC2", "FARGATE"]);
    for (source, definition) in register_and_describe(&router, family, request, case).await {
        let actual = compatibilities(&definition, case);
        assert_eq!(
            actual, expected_after,
            "case {case:?}: {source} expected compatibilities {expected_after:?}, got {actual:?} \
             in {definition}"
        );
    }
}

/// RULE, where FARGATE drops out: a revision whose image can be pulled only AS the task
/// execution role, and that names none, is not validated against Fargate — Amazon ECS's own
/// capacity has no container instance whose role the agent could pull under — while the
/// caller's own capacity still supports it. The managed-instances answer is the account's
/// capacity either way, so naming the execution role puts Fargate back and moves nothing else.
#[tokio::test]
async fn an_image_needing_a_principal_drops_only_fargate() {
    let router = service();
    create_managed_instances_capacity(&router, "mipullcluster", "mipull", "setup").await;

    // (case, the executionRoleArn the registration names, the launch types it must report).
    let rows: Vec<(&str, Option<&str>, Vec<String>)> = vec![
        (
            "an Amazon ECR image with no execution role",
            None,
            with_managed_instances(&["EC2"]),
        ),
        (
            "the same image with an execution role",
            Some("arn:aws:iam::000000000000:role/ecsTaskExecutionRole"),
            with_managed_instances(&["EC2", "FARGATE"]),
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
            register_and_describe(&router, &format!("mipullfam{index}"), request, case).await
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
