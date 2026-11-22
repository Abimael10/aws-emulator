// SPDX-License-Identifier: CC0-1.0

//! Which capacity provider PLACED a task, when the run named none.
//!
//! The rule under test (`RunTask$capacityProviderStrategy`: "If no
//! `capacityProviderStrategy` or `launchType` is specified, the
//! `defaultCapacityProviderStrategy` for the cluster is used"): a placement that names
//! neither takes the cluster's own default strategy, and the task it creates reports the
//! provider that strategy named in `capacityProviderName` beside the launch type that
//! provider's capacity runs on (`Task$launchType` is "The infrastructure where your task
//! runs on", and both Fargate providers run tasks on Fargate); a placement that names a
//! strategy of its own uses THAT one instead, and one that names a launch type carries the
//! launch type and no provider. The cluster's default strategy is itself stored as set,
//! entry for entry, so `DescribeClusters` echoes the `base` and `weight` each entry was
//! given.
//!
//! A strategy of several providers is a DISTRIBUTION over them rather than one of them:
//! `base` "designates how many tasks, at a minimum, to run on the specified capacity
//! provider" and is satisfied first, and `weight` "designates the relative percentage of
//! the total number of tasks launched that should use the specified capacity provider"
//! once every base is met — which the second test here pins over a table of
//! `(base, weight)` pairs.
//!
//! The first table spans what the run names (nothing, a strategy, a launch type) and what
//! the cluster has to fall back on (a default strategy, or none at all), over the
//! predefined Fargate providers every account holds.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so nothing reaches into the state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, list, ok, poll_until, register_capacity, service,
    string,
};

/// A Fargate-compatible revision: Fargate validates the task definition against the
/// launch type, so it has to be registered for it — `awsvpc`, with one of the published
/// task sizes.
async fn register_fargate_definition(router: &MultiServiceRouter, family: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "requiresCompatibilities": ["FARGATE"],
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "web",
                "image": "public.ecr.aws/nginx/nginx:latest",
                "cpu": 0,
                "memory": 128,
            }],
        }),
        case,
    )
    .await;
}

/// What one row's `RunTask` names for its placement.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// Neither a launch type nor a strategy, so the cluster's default decides.
    Nothing,
    /// A strategy of its own, naming this provider.
    Strategy(&'static str),
    /// A launch type.
    LaunchType(&'static str),
}

/// One row: what the run names, and the `(capacityProviderName, launchType)` pair the
/// task it creates must report.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    names: Names,
    expect_provider: Option<&'static str>,
    expect_launch_type: Option<&'static str>,
}

#[tokio::test]
async fn a_run_that_names_no_placement_is_placed_by_the_clusters_default_strategy() {
    let router = service();
    let cluster = "defaulted";
    create_cluster(&router, cluster, "setup").await;
    register_fargate_definition(&router, "defaulted", "setup").await;

    // The cluster's default strategy, with a `base` and a `weight` the echo below is
    // checked against.
    let strategy = json!([{ "capacityProvider": "FARGATE_SPOT", "weight": 1, "base": 0 }]);
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": strategy.clone(),
        }),
        "setup",
    )
    .await;

    // "DescribeClusters echoes defaultCapacityProviderStrategy exactly as set": the
    // strategy a placement falls back on is the stored one, so it is read back first.
    let case = "the cluster echoes the default strategy it was given";
    let body = ok(
        &router,
        "DescribeClusters",
        json!({ "clusters": [cluster] }),
        case,
    )
    .await;
    let described = list(&body, "clusters", case);
    assert_eq!(
        described[0]["defaultCapacityProviderStrategy"],
        strategy,
        "case {case:?}: the stored default strategy must read back as set, got {body}"
    );

    let rows = [
        Row {
            case: "the run names neither, so the cluster's default places it",
            names: Names::Nothing,
            expect_provider: Some("FARGATE_SPOT"),
            // The provider that placed it runs its tasks on Fargate, which is the
            // infrastructure the task reports whichever of the two Fargate providers the
            // placement was routed through.
            expect_launch_type: Some("FARGATE"),
        },
        Row {
            case: "the run names a strategy of its own, which overrides the default",
            names: Names::Strategy("FARGATE"),
            expect_provider: Some("FARGATE"),
            expect_launch_type: Some("FARGATE"),
        },
        Row {
            case: "the run names a launch type, which is not a provider at all",
            names: Names::LaunchType("FARGATE"),
            expect_provider: None,
            expect_launch_type: Some("FARGATE"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut request = json!({
            "cluster": cluster,
            "taskDefinition": "defaulted",
            "networkConfiguration": awsvpc_network_configuration(),
        });
        match row.names {
            Names::Nothing => {}
            Names::Strategy(provider) => {
                request["capacityProviderStrategy"] =
                    json!([{ "capacityProvider": provider, "weight": 1 }]);
            }
            Names::LaunchType(launch_type) => request["launchType"] = json!(launch_type),
        }

        let body = ok(&router, "RunTask", request, case).await;
        let tasks = list(&body, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected one launched task, got {body}"
        );
        // The attribution is STORED with the task, so the describe reports it too: a
        // client that comes back later still learns which provider placed the task.
        let arn = string(&tasks[0], "taskArn", case);
        let read = ok(
            &router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [arn] }),
            case,
        )
        .await;
        let read = list(&read, "tasks", case);
        for reported in [&tasks[0], &read[0]] {
            assert_eq!(
                reported["capacityProviderName"].as_str(),
                row.expect_provider,
                "case {case:?}: unexpected capacityProviderName in {reported}"
            );
            assert_eq!(
                reported["launchType"].as_str(),
                row.expect_launch_type,
                "case {case:?}: unexpected launchType in {reported}"
            );
        }
    }
}

/// One row of the distribution table: the strategy a run names, how many tasks it asks
/// for, and the provider each of those tasks must be placed through, in the order the run
/// places them.
#[derive(Debug, Clone, Copy)]
struct SpreadRow {
    case: &'static str,
    /// `(capacityProvider, base, weight)` per strategy entry, in the order the request
    /// states them.
    strategy: &'static [(&'static str, i64, i64)],
    expect: &'static [&'static str],
}

#[tokio::test]
async fn a_strategy_places_its_base_first_and_spreads_the_rest_by_weight() {
    let router = service();
    let cluster = "spread";
    create_cluster(&router, cluster, "setup").await;
    register_fargate_definition(&router, "spread", "setup").await;
    // A strategy places a task only through a provider its cluster is associated with, so
    // both predefined Fargate providers are associated before the table runs.
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        "setup",
    )
    .await;

    // The rows differ along what the two members say: a base on one provider with all the
    // weight on the other, a base on the only unweighted provider, equal weights, weights
    // in a 1:4 ratio (the `weight` doc's own example: "If capacityProviderA has weight 1
    // and capacityProviderB has weight 4, then for every 1 task on A, 4 tasks will run on
    // B"), and a provider whose weight is 0 and which therefore "can't be used to place
    // tasks".
    let rows = [
        SpreadRow {
            case: "a base on one provider, all the weight on the other",
            strategy: &[("FARGATE", 0, 1), ("FARGATE_SPOT", 1, 0)],
            expect: &["FARGATE_SPOT", "FARGATE", "FARGATE"],
        },
        SpreadRow {
            case: "the base is taken first, then the only weighted provider",
            strategy: &[("FARGATE", 1, 0), ("FARGATE_SPOT", 0, 1)],
            expect: &["FARGATE", "FARGATE_SPOT"],
        },
        SpreadRow {
            case: "equal weights split the tasks evenly",
            strategy: &[("FARGATE", 0, 1), ("FARGATE_SPOT", 0, 1)],
            expect: &["FARGATE", "FARGATE_SPOT", "FARGATE", "FARGATE_SPOT"],
        },
        SpreadRow {
            case: "four times the weight takes four times the tasks",
            strategy: &[("FARGATE", 0, 1), ("FARGATE_SPOT", 0, 4)],
            expect: &[
                "FARGATE_SPOT",
                "FARGATE_SPOT",
                "FARGATE_SPOT",
                "FARGATE",
                "FARGATE_SPOT",
            ],
        },
        SpreadRow {
            case: "a provider with no weight and no base places nothing",
            strategy: &[("FARGATE", 0, 0), ("FARGATE_SPOT", 0, 1)],
            expect: &["FARGATE_SPOT", "FARGATE_SPOT"],
        },
    ];

    for row in &rows {
        let case = row.case;
        let strategy: Vec<Value> = row
            .strategy
            .iter()
            .map(|(provider, base, weight)| {
                json!({ "capacityProvider": provider, "base": base, "weight": weight })
            })
            .collect();
        let body = ok(
            &router,
            "RunTask",
            json!({
                "cluster": cluster,
                "taskDefinition": "spread",
                "count": row.expect.len(),
                "capacityProviderStrategy": strategy,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
            case,
        )
        .await;
        let tasks = list(&body, "tasks", case);
        let placed: Vec<String> = tasks
            .iter()
            .map(|task| string(task, "capacityProviderName", case))
            .collect();
        assert_eq!(
            placed,
            row.expect.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "case {case:?}: unexpected distribution over the strategy, got {body}"
        );
        // Each of them runs on Fargate whichever of the two providers placed it, and the
        // attribution is STORED, so the describe reports the same pair.
        let arns: Vec<String> = tasks
            .iter()
            .map(|task| string(task, "taskArn", case))
            .collect();
        let read = ok(
            &router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": arns }),
            case,
        )
        .await;
        for task in list(&read, "tasks", case) {
            assert_eq!(
                task["launchType"].as_str(),
                Some("FARGATE"),
                "case {case:?}: a task placed through a Fargate provider runs on Fargate, \
                 got {task}"
            );
        }
        let mut stored: Vec<String> = list(&read, "tasks", case)
            .iter()
            .map(|task| string(task, "capacityProviderName", case))
            .collect();
        stored.sort();
        let mut expected: Vec<String> = row.expect.iter().map(ToString::to_string).collect();
        expected.sort();
        assert_eq!(
            stored, expected,
            "case {case:?}: the stored attribution must match what the run reported, got {read}"
        );
    }
}

#[tokio::test]
async fn every_task_a_distributed_run_places_reaches_running() {
    // A distribution over providers is a distribution of REAL placements: each task the
    // strategy routed somewhere is a task that goes on to run, so the run below is followed
    // to the state every one of its tasks settles in.
    let router = service();
    let cluster = "spreadruns";
    let case = "a base on one provider and the weight on the other";
    create_cluster(&router, cluster, case).await;
    register_fargate_definition(&router, "spreadruns", case).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    let body = ok(
        &router,
        "RunTask",
        json!({
            "cluster": cluster,
            "taskDefinition": "spreadruns",
            "count": 3,
            "capacityProviderStrategy": [
                { "capacityProvider": "FARGATE", "weight": 1, "base": 0 },
                { "capacityProvider": "FARGATE_SPOT", "weight": 0, "base": 1 },
            ],
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let arns: Vec<String> = list(&body, "tasks", case)
        .iter()
        .map(|task| string(task, "taskArn", case))
        .collect();

    // The start of each task is the CLOCK's, so the suite waits for the status rather than
    // for a span.
    let read = poll_until(
        case,
        "every task of the run to reach RUNNING",
        || {
            ok(
                &router,
                "DescribeTasks",
                json!({ "cluster": cluster, "tasks": arns.clone() }),
                case,
            )
        },
        |body| {
            body["tasks"].as_array().is_some_and(|tasks| {
                tasks.len() == 3
                    && tasks
                        .iter()
                        .all(|task| task["lastStatus"] == json!("RUNNING"))
            })
        },
    )
    .await;
    for task in list(&read, "tasks", case) {
        assert!(
            list(&task, "containers", case)
                .iter()
                .all(|container| container["lastStatus"] == json!("RUNNING")),
            "case {case:?}: every container of a running task runs, got {task}"
        );
    }
}

#[tokio::test]
async fn a_services_scheduler_spreads_its_tasks_over_the_same_distribution() {
    // The same two members decide a SERVICE's placements: `CreateService
    // $capacityProviderStrategy` is the same shape as a run's, so the scheduler places the
    // service's tasks over it base-first and then by weight, and the service still reaches
    // its steady state.
    let router = service();
    let cluster = "servicespread";
    let case = "a service over a base on one provider and the weight on the other";
    create_cluster(&router, cluster, case).await;
    register_fargate_definition(&router, "servicespread", case).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": cluster,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": "spread",
            "taskDefinition": "servicespread",
            "desiredCount": 3,
            "capacityProviderStrategy": [
                { "capacityProvider": "FARGATE", "weight": 1, "base": 0 },
                { "capacityProvider": "FARGATE_SPOT", "weight": 0, "base": 1 },
            ],
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;

    // The placements are the SCHEDULER's, so the suite waits for the count it converges on
    // rather than for a span.
    let listed = poll_until(
        case,
        "the service to hold the three tasks it wants",
        || {
            ok(
                &router,
                "ListTasks",
                json!({ "cluster": cluster, "serviceName": "spread" }),
                case,
            )
        },
        |body| {
            body["taskArns"]
                .as_array()
                .is_some_and(|arns| arns.len() == 3)
        },
    )
    .await;
    let read = ok(
        &router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": list(&listed, "taskArns", case) }),
        case,
    )
    .await;
    let mut placed: Vec<String> = list(&read, "tasks", case)
        .iter()
        .map(|task| string(task, "capacityProviderName", case))
        .collect();
    placed.sort();
    assert_eq!(
        placed,
        vec![
            "FARGATE".to_string(),
            "FARGATE".to_string(),
            "FARGATE_SPOT".to_string()
        ],
        "case {case:?}: the base takes one task and the weight the rest, got {read}"
    );
}

#[tokio::test]
async fn a_cluster_with_no_default_strategy_places_the_run_on_its_own_capacity() {
    // The other half of the same sentence: with no strategy anywhere, there is no
    // provider to attribute the placement to, and the task runs on the capacity
    // registered to the cluster under the default launch type.
    let router = service();
    let cluster = "undefaulted";
    let case = "no strategy anywhere";
    create_cluster(&router, cluster, case).await;
    register_capacity(&router, cluster, case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "undefaulted",
            "containerDefinitions": [{
                "name": "web",
                "image": "public.ecr.aws/nginx/nginx:latest",
                "cpu": 0,
                "memory": 128,
            }],
        }),
        case,
    )
    .await;

    let body = ok(
        &router,
        "RunTask",
        json!({ "cluster": cluster, "taskDefinition": "undefaulted" }),
        case,
    )
    .await;
    let task: &Value = &list(&body, "tasks", case)[0];
    assert_eq!(
        (
            task["capacityProviderName"].as_str(),
            task["launchType"].as_str()
        ),
        (None, Some("EC2")),
        "case {case:?}: expected the run to carry the default launch type and no \
         provider, got {task}"
    );
}
