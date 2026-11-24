// SPDX-License-Identifier: CC0-1.0

//! Managed-instances capacity against the launch types a registered revision reports, read
//! back through both projections of the stored revision.
//!
//! The rule under test: `TaskDefinition$compatibilities` ("Amazon ECS validates the task
//! definition parameters with those supported by the launch type") is DERIVED from the
//! revision's OWN parameters over the launch types a recorded ECS projection reports —
//! `EXTERNAL`, `EC2` and `FARGATE`, the infrastructure a placement names outright — and over
//! nothing else: `MANAGED_INSTANCES` is a member of the `Compatibility` enum
//! (`smithy_query(query="shape", shape="com.amazonaws.ecs#Compatibility")` -> `EC2`,
//! `FARGATE`, `EXTERNAL`, `MANAGED_INSTANCES`) that a revision reports nothing about,
//! because that capacity is reached through a capacity provider rather than by naming a
//! launch type (`RunTask$launchType`: "If you want to use Amazon ECS Managed Instances, you
//! must use the `capacityProviderStrategy` request parameter and omit the `launchType`
//! request parameter").
//!
//! What the registration ASKED to be validated against is reported separately and
//! unchanged, so a request that names managed instances in its `requiresCompatibilities`
//! reads that member back verbatim and still reports a derived list without it.
//!
//! The rows span what the rule generalises over:
//!
//! * the NETWORK MODE, which is what decides the launch types reported: the documented
//!   default, each member of the `NetworkMode` enum, and `awsvpc` with and without a task
//!   size;
//! * what the registration ASKED to be validated against (`requiresCompatibilities`):
//!   nothing, managed instances alone, and managed instances beside another launch type —
//!   none of which adds a launch type to the derived list;
//! * whether the ACCOUNT holds a managed-instances capacity provider, which the derivation
//!   does not read: the same rows are registered against an account that created one and
//!   against one that created none, and both report the same list;
//! * a revision Fargate does NOT support (an Amazon ECR image with no `executionRoleArn`),
//!   where only the Fargate answer changes;
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

/// Assert `definition` reports exactly `expected`, and that managed-instances capacity is
/// named in no position of it.
fn reports(definition: &Value, source: &str, expected: &[&str], case: &str) {
    let actual = compatibilities(definition, case);
    assert_eq!(
        actual, expected,
        "case {case:?}: {source} expected compatibilities {expected:?}, got {actual:?} in \
         {definition}"
    );
    assert!(
        !actual.iter().any(|launch_type| launch_type == MANAGED_INSTANCES),
        "case {case:?}: {source} must report no {MANAGED_INSTANCES} compatibility, got \
         {actual:?} in {definition}"
    );
}

/// Give the account a managed-instances capacity provider: a cluster, and a provider of that
/// class scoped to it — the only way an account comes to hold that capacity.
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

/// (case, the request beside the family and containers, the launch types the revision
/// reports).
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

/// RULE: a revision reports no managed-instances compatibility, whatever it declares and
/// whatever it asked to be validated against — in an account that created no capacity
/// provider of that class.
#[tokio::test]
async fn no_revision_reports_managed_instances() {
    let router = service();

    for (index, (case, mut request, expected)) in rows().into_iter().enumerate() {
        request["containerDefinitions"] = json!([container("app", IMAGE)]);
        for (source, definition) in
            register_and_describe(&router, &format!("mifam{index}"), request, case).await
        {
            reports(&definition, source, &expected, case);
        }
    }
}

/// RULE, the other side of it: the derivation does not read the account's capacity providers,
/// so the same rows registered in an account that HOLDS a managed-instances capacity provider
/// report exactly the same list.
#[tokio::test]
async fn the_reported_list_does_not_turn_on_the_capacity_the_account_holds() {
    let router = service();
    create_managed_instances_capacity(&router, "mifleetcluster", "mifleet", "setup").await;

    for (index, (case, mut request, expected)) in rows().into_iter().enumerate() {
        request["containerDefinitions"] = json!([container("app", IMAGE)]);
        for (source, definition) in
            register_and_describe(&router, &format!("miheld{index}"), request, case).await
        {
            reports(&definition, source, &expected, case);
        }
    }
}

/// RULE, over time: the derived list is the validation the REGISTRATION performed, so reading a
/// revision back never rewrites it — a revision registered before the account held any
/// managed-instances capacity provider reports the same list after one exists, and so does the
/// next revision of the same family.
#[tokio::test]
async fn the_derived_list_is_the_one_the_registration_validated() {
    let router = service();
    let case = "a revision registered before a capacity provider existed";
    let family = "mitimeline";
    let request = json!({
        "networkMode": "awsvpc",
        "cpu": TASK_CPU,
        "memory": TASK_MEMORY,
        "containerDefinitions": [container("app", IMAGE)],
    });
    let expected = ["EC2", "FARGATE"];

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
    reports(
        &before["taskDefinition"],
        "RegisterTaskDefinition",
        &expected,
        case,
    );

    create_managed_instances_capacity(&router, "milatercluster", "milater", case).await;

    let reread = describe(&router, &format!("{family}:1"), case).await;
    reports(
        &reread,
        "DescribeTaskDefinition after a provider was created",
        &expected,
        case,
    );

    let case = "the next revision, registered against an account holding a provider";
    for (source, definition) in register_and_describe(&router, family, request, case).await {
        reports(&definition, source, &expected, case);
    }
}

/// RULE, where FARGATE drops out: a revision whose image can be pulled only AS the task
/// execution role, and that names none, is not validated against Fargate — Amazon ECS's own
/// Fargate capacity has no container instance whose role the agent could pull under — while the
/// caller's own capacity still supports it. Naming the execution role puts Fargate back and
/// moves nothing else.
#[tokio::test]
async fn an_image_needing_a_principal_drops_only_fargate() {
    let router = service();
    create_managed_instances_capacity(&router, "mipullcluster", "mipull", "setup").await;

    // (case, the executionRoleArn the registration names, the launch types it must report).
    let rows: Vec<(&str, Option<&str>, Vec<&str>)> = vec![
        (
            "an Amazon ECR image with no execution role",
            None,
            vec!["EC2"],
        ),
        (
            "the same image with an execution role",
            Some("arn:aws:iam::000000000000:role/ecsTaskExecutionRole"),
            vec!["EC2", "FARGATE"],
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
            reports(&definition, source, &expected, case);
        }
    }
}
