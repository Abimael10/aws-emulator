// SPDX-License-Identifier: CC0-1.0

//! The transitions `UpdateService` takes a service's PLACEMENT through, driven end to end
//! over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test, as `UpdateServiceRequest$capacityProviderStrategy` states it
//! (`smithy_query(query="doc",shape="UpdateServiceRequest",member="capacityProviderStrategy")`
//! lists the valid transitions, closing with "Update the Auto Scaling group or Fargate
//! capacity provider back to the launch type. Pass an empty list in the
//! `capacityProviderStrategy` parameter"): a `capacityProviderStrategy` the request NAMES is
//! the placement the service is left with, and an EMPTY list takes the service off its
//! strategy and back onto a launch type — the one it ran before a strategy was put on it,
//! and for a service that never held one, the class of capacity the strategy it comes off
//! was placing its tasks on. A request that names the member at all is what moves the
//! placement; one that omits it changes neither half. A service reports exactly one of the
//! two ("A service will return either a capacityProviderStrategy or launchType parameter,
//! but not both"), so the half it is not on is absent from the answer.
//!
//! The rows differ along what the rule generalises over: the launch type the service starts
//! on (`EC2`, `FARGATE`), whether it starts on a launch type at all or on a strategy, the
//! CLASS of capacity provider the strategy routes through (the Fargate providers every
//! account holds, and a provider over an Auto Scaling group of the caller's — the two
//! classes the documented transitions name), and how many strategies the service is taken
//! through before the empty list comes.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `UpdateService`'s own answer and then through `DescribeServices`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_auto_scaling_group, create_cluster, list, ok, service,
    with_members,
};

/// The cluster every service below runs in.
const CLUSTER: &str = "transitions";

/// The task-definition family every service below runs: `awsvpc` with a task size, so the
/// revision is validated against both the Amazon EC2 and the Fargate capacity
/// (`TaskDefinition$compatibilities`) and can be placed either way.
const FAMILY: &str = "transitapp";

/// The Fargate capacity providers every account holds — "available to all accounts and only
/// need to be associated with a cluster to be used in a capacity provider strategy"
/// (`CapacityProviderStrategyItem`'s own doc).
const FARGATE_PROVIDER: &str = "FARGATE";
const FARGATE_SPOT_PROVIDER: &str = "FARGATE_SPOT";

/// A capacity provider over an Auto Scaling group of the caller's, which is the other class
/// of provider the documented transitions name, and the Auto Scaling group it binds onto.
const GROUP_PROVIDER: &str = "groupprovider";
const GROUP: &str = "asg-transitions";

/// The two launch types the rows below start on, as `LaunchType` spells them
/// (`smithy_query(query="shape", shape="LaunchType")` -> EC2, FARGATE, EXTERNAL,
/// MANAGED_INSTANCES).
const EC2: &str = "EC2";
const FARGATE: &str = "FARGATE";

/// A `capacityProviderStrategy` routing every task through `provider`.
fn strategy(provider: &str) -> Value {
    json!([{ "capacityProvider": provider, "weight": 1 }])
}

/// Stand up the cluster, the providers a strategy below may name, and the revision every
/// service runs.
async fn fixture(router: &MultiServiceRouter) {
    let case = "setup";
    create_cluster(router, CLUSTER, case).await;
    create_auto_scaling_group(router, GROUP, case).await;
    ok(
        router,
        "CreateCapacityProvider",
        json!({
            "name": GROUP_PROVIDER,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": GROUP,
                // Managed scaling is what has Amazon ECS add the capacity behind the
                // provider, so a placement routed through it is one Amazon ECS can satisfy.
                "managedScaling": { "status": "ENABLED", "targetCapacity": 100 },
            },
        }),
        case,
    )
    .await;
    ok(
        router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": CLUSTER,
            "capacityProviders": [FARGATE_PROVIDER, FARGATE_SPOT_PROVIDER, GROUP_PROVIDER],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/nginx/nginx:latest",
                "cpu": 0,
                "memory": 128,
            }],
        }),
        case,
    )
    .await;
}

/// Create the service `name` with the placement members `placement` states, and nothing else
/// of the request differing row by row.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    placement: Value,
    case: &str,
) -> Value {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": name,
        "taskDefinition": FAMILY,
        // Nothing here is about the tasks a service places, so none is asked for: the
        // placement is the whole of what each row reads back.
        "desiredCount": 0,
        "networkConfiguration": awsvpc_network_configuration(),
    });
    with_members(&mut request, placement);
    let body = ok(router, "CreateService", request, case).await;
    body["service"].clone()
}

/// Update the service `name` with the members `members` states, answering the service the
/// update itself reports.
async fn update_service(
    router: &MultiServiceRouter,
    name: &str,
    members: Value,
    case: &str,
) -> Value {
    let mut request = json!({ "cluster": CLUSTER, "service": name });
    with_members(&mut request, members);
    let body = ok(router, "UpdateService", request, case).await;
    body["service"].clone()
}

/// The service `name` as `DescribeServices` reports it.
async fn describe_service(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service {name} back, got {body}"))
}

/// The placement a reported service carries: the `launchType` it states, and the capacity
/// providers its `capacityProviderStrategy` routes through, in order.
fn placement_of(reported: &Value, case: &str) -> (Option<String>, Vec<String>) {
    let launch_type = match &reported["launchType"] {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        other => panic!("case {case:?}: expected a string launchType, got {other}"),
    };
    let providers = list(reported, "capacityProviderStrategy", case)
        .iter()
        .map(|item| match &item["capacityProvider"] {
            Value::String(value) => value.clone(),
            other => panic!("case {case:?}: expected a strategy entry's provider, got {other}"),
        })
        .collect();
    (launch_type, providers)
}

/// Assert both the update's OWN answer and a later read report the placement
/// `(launch_type, providers)` — a write whose answer and whose store disagree is its own
/// defect.
async fn assert_placement(
    router: &MultiServiceRouter,
    name: &str,
    answered: &Value,
    expected: (Option<&str>, Vec<&str>),
    case: &str,
) {
    let expected = (
        expected.0.map(ToString::to_string),
        expected
            .1
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>(),
    );
    let reported = placement_of(answered, case);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the answer to report the placement {expected:?}, got \
         {reported:?} in {answered}"
    );
    let read_back = describe_service(router, name, case).await;
    let reported = placement_of(&read_back, case);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the stored placement {expected:?}, got {reported:?} in \
         {read_back}"
    );
}

/// One row of the transition table: what the service is created on, the strategies it is
/// taken through, and the launch type the empty list then leaves it on.
struct Row {
    case: &'static str,
    /// The placement member `CreateService` carries — a `launchType` or a
    /// `capacityProviderStrategy`.
    created: Value,
    /// The launch type the created service reports, before any update: `None` for a service
    /// created on a strategy.
    created_launch_type: Option<&'static str>,
    /// The capacity providers the created service's strategy routes through.
    created_providers: Vec<&'static str>,
    /// Every capacity provider the service is moved ONTO in turn, before the empty list.
    moves: Vec<&'static str>,
    /// The launch type the empty list leaves the service on.
    restored: &'static str,
}

#[tokio::test]
async fn an_empty_strategy_takes_a_service_back_to_a_launch_type() {
    let router = service();
    fixture(&router).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "an EC2 launch type moved onto a Fargate capacity provider",
            created: json!({ "launchType": EC2 }),
            created_launch_type: Some(EC2),
            created_providers: Vec::new(),
            moves: vec![FARGATE_PROVIDER],
            restored: EC2,
        },
        Row {
            case: "a FARGATE launch type moved onto an Auto Scaling group capacity provider",
            created: json!({ "launchType": FARGATE }),
            created_launch_type: Some(FARGATE),
            created_providers: Vec::new(),
            moves: vec![GROUP_PROVIDER],
            restored: FARGATE,
        },
        Row {
            case: "an EC2 launch type moved through two strategies",
            created: json!({ "launchType": EC2 }),
            created_launch_type: Some(EC2),
            created_providers: Vec::new(),
            moves: vec![FARGATE_PROVIDER, FARGATE_SPOT_PROVIDER],
            restored: EC2,
        },
        Row {
            case: "a FARGATE launch type moved off a Fargate provider onto a group provider",
            created: json!({ "launchType": FARGATE }),
            created_launch_type: Some(FARGATE),
            created_providers: Vec::new(),
            moves: vec![FARGATE_SPOT_PROVIDER, GROUP_PROVIDER],
            restored: FARGATE,
        },
        Row {
            // A service created on a strategy held no launch type for the strategy to be
            // put on top of, so the one it comes back onto is the class of capacity the
            // strategy was placing its tasks on — Fargate, for the Fargate providers.
            case: "a service created on a Fargate strategy, which never held a launch type",
            created: json!({ "capacityProviderStrategy": strategy(FARGATE_PROVIDER) }),
            created_launch_type: None,
            created_providers: vec![FARGATE_PROVIDER],
            moves: Vec::new(),
            restored: FARGATE,
        },
        Row {
            // The same, for a provider over an Auto Scaling group: those tasks run on the
            // Amazon EC2 instances the group registers into the cluster.
            case: "a service created on an Auto Scaling group strategy",
            created: json!({ "capacityProviderStrategy": strategy(GROUP_PROVIDER) }),
            created_launch_type: None,
            created_providers: vec![GROUP_PROVIDER],
            moves: Vec::new(),
            restored: EC2,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("transit{index}");
        let created = create_service(&router, &name, row.created, case).await;
        assert_placement(
            &router,
            &name,
            &created,
            (row.created_launch_type, row.created_providers.clone()),
            case,
        )
        .await;

        for provider in &row.moves {
            // "When switching from launch type to capacity provider strategy on an existing
            // service, or making a change to a capacity provider strategy on a service that
            // is already using one, you must force a new deployment": the tasks already
            // running were placed under the configuration the service is taken off.
            let answered = update_service(
                &router,
                &name,
                json!({
                    "capacityProviderStrategy": strategy(provider),
                    "forceNewDeployment": true,
                }),
                case,
            )
            .await;
            assert_placement(&router, &name, &answered, (None, vec![*provider]), case).await;
        }

        // The documented way back: the empty list, which is no strategy to switch ONTO and
        // so needs no forced deployment of its own.
        let answered =
            update_service(&router, &name, json!({ "capacityProviderStrategy": [] }), case).await;
        assert_placement(
            &router,
            &name,
            &answered,
            (Some(row.restored), Vec::new()),
            case,
        )
        .await;
    }
}

#[tokio::test]
async fn an_update_that_names_no_strategy_moves_neither_half_of_the_placement() {
    let router = service();
    fixture(&router).await;

    // A service on a launch type and a service on a strategy, each updated with a member
    // that is not the placement at all: the half each is on is the half it keeps.
    let case = "a service on a launch type, updated without a strategy";
    let name = "keepslaunchtype";
    create_service(&router, name, json!({ "launchType": EC2 }), case).await;
    let answered = update_service(&router, name, json!({ "desiredCount": 0 }), case).await;
    assert_placement(&router, name, &answered, (Some(EC2), Vec::new()), case).await;

    let case = "a service on a strategy, updated without a strategy";
    let name = "keepsstrategy";
    create_service(
        &router,
        name,
        json!({ "capacityProviderStrategy": strategy(FARGATE_PROVIDER) }),
        case,
    )
    .await;
    let answered = update_service(&router, name, json!({ "desiredCount": 0 }), case).await;
    assert_placement(
        &router,
        name,
        &answered,
        (None, vec![FARGATE_PROVIDER]),
        case,
    )
    .await;

    // And the empty list on a service that is already on a launch type leaves it there: it
    // takes the service off a strategy, and this one is on none.
    let case = "an empty strategy on a service already on a launch type";
    let answered = update_service(
        &router,
        "keepslaunchtype",
        json!({ "capacityProviderStrategy": [] }),
        case,
    )
    .await;
    assert_placement(
        &router,
        "keepslaunchtype",
        &answered,
        (Some(EC2), Vec::new()),
        case,
    )
    .await;
}
