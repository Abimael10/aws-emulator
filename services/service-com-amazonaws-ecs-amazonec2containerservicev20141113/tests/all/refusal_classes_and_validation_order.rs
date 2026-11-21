// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a request earns when more than one rule could refuse it, and which
//! modelled error class each condition is answered in, driven end to end over the shared
//! wire harness in [`crate::ecs_wire`].
//!
//! Four rules, each asserted over a table that spans the class rather than one recorded
//! request:
//!
//! 1. A Fargate platform version the service WITHDREW is an entitlement condition
//!    (`AccessDeniedException`, HTTP 403), while a value that never named a published
//!    platform version stays `PlatformUnknownException` (HTTP 400). The catalogue of
//!    withdrawn versions is `state::RETIRED_PLATFORM_VERSIONS`, read off the
//!    `platformVersion` doc's "Fargate platform versions" page (its deprecation notice),
//!    and the published ones off the same page.
//! 2. WHERE a placement would run is judged against the revision's declared compatibility
//!    (`TaskDefinition$compatibilities`: "Amazon ECS validates the task definition
//!    parameters with those supported by the launch type") BEFORE the other rules that
//!    revision keys — so a placement that names a launch type the revision does not support
//!    is refused for that, whatever else it pairs with the revision.
//! 3. `ManagedScaling$minimumScalingStepSize` and `$maximumScalingStepSize` are the floor
//!    and the ceiling of one scale-out step, so a block whose minimum is above its maximum
//!    is refused — before the request is weighed against what the account already holds.
//! 4. `ContinueServiceDeployment$hookId` takes the form the service mints
//!    (`ecs-{hookType}-{id}`, the id spelled as a UUID); a hookId naming a hook of that
//!    rollout whose action has already been taken asks for an action that has already
//!    happened, so it is answered and changes nothing.
//! 5. The `cluster` a request names is the SCOPE it acts in, so it is resolved before any
//!    other resource the request references: a request naming a cluster this account does
//!    not hold earns `ClusterNotFoundException` whatever else it names, and the refusal the
//!    other resource would have earned is what the same request gets once the cluster is
//!    there.
//!
//! Every case goes through the same front door a client uses, so nothing reaches into the
//! state struct and a response that is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::{group_arn, managed_instances};
// The rollout a PAUSE hook belongs to is read back, waited for and picked apart through the
// sibling suite that owns those readers, rather than copied here.
use crate::continue_service_deployment_validation::{
    awaiting_hook, describe_deployment, deployment_awaiting_a_hook,
};
use crate::ecs_wire::{
    answered, awsvpc_network_configuration, create_auto_scaling_group, create_cluster,
    cluster_with_capacity, list, ok, refusal, register_capacity, service, string,
};
use crate::task_lifecycle::container;

/// The modelled error code a bad parameter is answered in.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The modelled error code this service answers a fault in what the client SENT in —
/// "it might be specifying an identifier that isn't valid".
const CLIENT_EXCEPTION: &str = "ClientException";

/// The `AccessDeniedException` sentence for a platform the account may not place tasks on
/// (`state`'s `PLATFORM_ACCESS_DENIED_MESSAGE`): fixed text, naming nothing from the
/// request.
const PLATFORM_ACCESS_DENIED: &str = "You do not have authorization to access the specified platform.";

/// The `PlatformUnknownException` sentence (`state`'s `PLATFORM_UNKNOWN_MESSAGE`).
const PLATFORM_UNKNOWN: &str = "The specified platform does not exist.";

/// The refusal a placement earns when the revision it names was never validated against the
/// launch type it would run on (`state`'s `validate_launch_type`), for the Fargate launch
/// type every row below names.
const FARGATE_UNSUPPORTED: &str = "Task definition does not support launch_type FARGATE.";

/// The refusal a placement earns when it carries a `networkConfiguration` the network mode
/// of its revision gives nothing to configure (`state`'s
/// `NETWORK_CONFIGURATION_NOT_VALID_MESSAGE`).
const NETWORK_CONFIGURATION_NOT_VALID: &str =
    "Network Configuration is not valid for the given networkMode of this task definition.";

/// The refusal an `awsvpc` placement that carries none earns (`state`'s
/// `AWSVPC_NETWORK_CONFIGURATION_REQUIRED_MESSAGE`).
const AWSVPC_CONFIGURATION_REQUIRED: &str =
    "Network Configuration must be provided when networkMode 'awsvpc' is specified.";

/// The refusal a `managedScaling` block whose step sizes do not bound each other earns
/// (`state`'s `MANAGED_SCALING_STEP_SIZE_MESSAGE`).
const STEP_SIZE_INVALID: &str =
    "The minimum or maximum scaling step size value is invalid. Specify a minimum scaling \
     step size value that is less than or equal to the maximum scaling step size value and \
     try again.";

/// The refusal a second provider over a group another one already scales earns (`state`'s
/// `AUTO_SCALING_GROUP_IN_USE_MESSAGE`) — the conflict rule row 3's ordering case has to
/// NOT be answered with.
const GROUP_IN_USE: &str =
    "The specified Auto Scaling group ARN is already being used by another capacity \
     provider. Specify a unique Auto Scaling group ARN and try again.";

/// The `DeploymentLifecycleHookStatus` a hook whose action was taken carries.
const SUCCEEDED: &str = "SUCCEEDED";

/// Register a revision of `family` in `network_mode`, asking to be validated against
/// `requires` — `requiresCompatibilities` is "The task launch type that Amazon ECS
/// validates the task definition against", so it decides which launch types the revision
/// can be placed with.
async fn register_in_mode(
    router: &MultiServiceRouter,
    family: &str,
    network_mode: &str,
    requires: &[&str],
    case: &str,
) -> String {
    let mut request = json!({
        "family": family,
        "networkMode": network_mode,
        "containerDefinitions": [container("app", "public.ecr.aws/nginx/nginx:latest")],
    });
    if !requires.is_empty() {
        request["requiresCompatibilities"] = json!(requires);
        // "This field is required for Fargate" — a revision validated against Fargate
        // carries the task-level size its tasks are given.
        request["cpu"] = json!("256");
        request["memory"] = json!("512");
    }
    let body = ok(router, "RegisterTaskDefinition", request, case).await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// A Fargate-compatible revision: `awsvpc` ("For Amazon ECS tasks on Fargate, the awsvpc
/// network mode is required") with the task-level size Fargate requires.
async fn register_fargate(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    register_in_mode(router, family, "awsvpc", &["FARGATE"], case).await
}

// ---------------------------------------------------------------------------
// 1. A withdrawn platform version is an entitlement condition
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_withdrawn_platform_version_is_refused_as_an_entitlement() {
    let router = service();
    let setup = "platform entitlement";
    create_cluster(&router, "platforms", setup).await;
    register_fargate(&router, "platformfam", setup).await;
    // One service to UPDATE, so the rule is asserted on the update path too. It names no
    // platform version of its own, so its create is the LATEST one.
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": "platforms",
            "serviceName": "platformsvc",
            "taskDefinition": "platformfam",
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        setup,
    )
    .await;

    // Every version Fargate published and withdrew, from `state::RETIRED_PLATFORM_VERSIONS`
    // — including the two the report did not cite — through each operation that pairs a
    // `platformVersion` with a placement.
    for version in service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::RETIRED_PLATFORM_VERSIONS
    {
        for (operation, request) in placement_requests(version) {
            let case = format!("{operation} with the withdrawn platformVersion {version}");
            let (status, body) = answered(&router, operation, request).await;
            assert_eq!(
                status, 403,
                "case {case:?}: expected the entitlement refusal's status, got {status} with \
                 {body}"
            );
            assert!(
                body["__type"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("AccessDeniedException"),
                "case {case:?}: expected an AccessDeniedException, got {body}"
            );
            assert_eq!(
                body["message"].as_str().unwrap_or_default(),
                PLATFORM_ACCESS_DENIED,
                "case {case:?}: expected the fixed sentence for this condition, got {body}"
            );
        }
    }

    // A value that never named a published platform version is the other condition: the
    // rows are one just past the newest Linux version, one below every published version,
    // and one that is not a version at all.
    for version in ["1.5.0", "0.9.0", "1.4.1", "no-such-version"] {
        for (operation, request) in placement_requests(version) {
            let case = format!("{operation} with the unknown platformVersion {version}");
            let (code, message) = refusal(&router, operation, request, &case).await;
            assert_eq!(
                code, "PlatformUnknownException",
                "case {case:?}: expected the unknown-platform class, got {code} with \
                 {message:?}"
            );
            assert_eq!(
                message, PLATFORM_UNKNOWN,
                "case {case:?}: expected the fixed sentence for this condition, got \
                 {message:?}"
            );
        }
    }

    // And the versions the page still publishes place the task: the rule withdraws exactly
    // the versions the catalogue names.
    for version in ["LATEST", "1.4.0"] {
        let case = format!("the published platformVersion {version}");
        let body = ok(
            &router,
            "RunTask",
            json!({
                "cluster": "platforms",
                "taskDefinition": "platformfam",
                "launchType": "FARGATE",
                "platformVersion": version,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
            &case,
        )
        .await;
        assert_eq!(
            list(&body, "tasks", &case).len(),
            1,
            "case {case:?}: expected a published platform version to place the task, got \
             {body}"
        );
    }
}

/// The same `platformVersion` carried by each operation that pairs one with a placement,
/// as the request that operation takes.
fn placement_requests(version: &str) -> [(&'static str, Value); 3] {
    [
        (
            "RunTask",
            json!({
                "cluster": "platforms",
                "taskDefinition": "platformfam",
                "launchType": "FARGATE",
                "platformVersion": version,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
        ),
        (
            "CreateService",
            json!({
                "cluster": "platforms",
                "serviceName": format!("svc-{}", version.replace('.', "-")),
                "taskDefinition": "platformfam",
                "desiredCount": 0,
                "launchType": "FARGATE",
                "platformVersion": version,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
        ),
        (
            "UpdateService",
            json!({
                "cluster": "platforms",
                "service": "platformsvc",
                "platformVersion": version,
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// 2. The launch type is judged before the other rules keyed on the revision
// ---------------------------------------------------------------------------

/// One row of the ordering table: a placement that breaks the launch-type rule AND the
/// network-configuration rule at once must be refused for the launch type.
struct OrderCase {
    case: &'static str,
    /// The family the placement names, registered below.
    family: &'static str,
    /// What the placement names as its infrastructure, as the request members that say it.
    placement: fn() -> Value,
    /// The one sentence the refusal must read.
    expected: &'static str,
}

#[tokio::test]
async fn the_launch_type_is_judged_before_the_network_configuration() {
    let router = service();
    let setup = "refusal order";
    cluster_with_capacity(&router, "order", setup).await;
    // The Fargate providers have to be associated with the cluster before a strategy of
    // this cluster's may name them, which is a rule of its own and not the one under test.
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "order",
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        setup,
    )
    .await;
    register_in_mode(&router, "bridgefam", "bridge", &["EC2"], setup).await;
    register_in_mode(&router, "hostfam", "host", &["EC2"], setup).await;
    register_fargate(&router, "vpcfam", setup).await;

    // The rows differ along the network mode of the revision (bridge, host), HOW the
    // placement names Fargate (its launch type, or a capacity provider strategy naming one
    // of the Fargate providers), and how many entries that strategy has. The last two rows
    // are the controls: with a launch type the revision does support, the network-
    // configuration rule is the one that fires, in each of its two directions.
    let rows = [
        OrderCase {
            case: "a bridge revision placed on Fargate by launch type",
            family: "bridgefam",
            placement: || {
                json!({
                    "launchType": "FARGATE",
                    "networkConfiguration": awsvpc_network_configuration(),
                })
            },
            expected: FARGATE_UNSUPPORTED,
        },
        OrderCase {
            case: "a host revision placed on Fargate by launch type",
            family: "hostfam",
            placement: || {
                json!({
                    "launchType": "FARGATE",
                    "networkConfiguration": awsvpc_network_configuration(),
                })
            },
            expected: FARGATE_UNSUPPORTED,
        },
        OrderCase {
            case: "a bridge revision placed on Fargate by a one-entry strategy",
            family: "bridgefam",
            placement: || {
                json!({
                    "capacityProviderStrategy": [
                        { "capacityProvider": "FARGATE", "weight": 1 },
                    ],
                    "networkConfiguration": awsvpc_network_configuration(),
                })
            },
            expected: FARGATE_UNSUPPORTED,
        },
        OrderCase {
            case: "a host revision placed on Fargate by a two-entry strategy",
            family: "hostfam",
            placement: || {
                json!({
                    "capacityProviderStrategy": [
                        { "capacityProvider": "FARGATE", "weight": 1, "base": 1 },
                        { "capacityProvider": "FARGATE_SPOT", "weight": 4 },
                    ],
                    "networkConfiguration": awsvpc_network_configuration(),
                })
            },
            expected: FARGATE_UNSUPPORTED,
        },
        OrderCase {
            case: "a bridge revision placed on the launch type it supports, with a \
                   configuration it has nothing to configure",
            family: "bridgefam",
            placement: || {
                json!({
                    "launchType": "EC2",
                    "networkConfiguration": awsvpc_network_configuration(),
                })
            },
            expected: NETWORK_CONFIGURATION_NOT_VALID,
        },
        OrderCase {
            case: "an awsvpc revision placed on the launch type it supports, with no \
                   configuration at all",
            family: "vpcfam",
            placement: || json!({ "launchType": "FARGATE" }),
            expected: AWSVPC_CONFIGURATION_REQUIRED,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        for operation in ["RunTask", "CreateService"] {
            let case = format!("{operation}: {}", row.case);
            let mut request = (row.placement)();
            request["cluster"] = json!("order");
            request["taskDefinition"] = json!(row.family);
            if operation == "CreateService" {
                request["serviceName"] = json!(format!("ordered{index}"));
                request["desiredCount"] = json!(0);
            }
            let (code, message) = refusal(&router, operation, request, &case).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?}: expected an {INVALID_PARAMETER}, got {code} with {message:?}"
            );
            assert_eq!(
                message, row.expected,
                "case {case:?}: expected the refusal to read {:?}, got {message:?}",
                row.expected
            );
        }
    }

    // Nothing any of those refusals accepted: no service was stored and no task was placed.
    let case = "after the refusals";
    let listed = ok(&router, "ListTasks", json!({ "cluster": "order" }), case).await;
    assert!(
        list(&listed, "taskArns", case).is_empty(),
        "case {case:?}: expected a refused placement to store no task, got {listed}"
    );
    let services = ok(&router, "ListServices", json!({ "cluster": "order" }), case).await;
    assert!(
        list(&services, "serviceArns", case).is_empty(),
        "case {case:?}: expected a refused create to store no service, got {services}"
    );
}

/// The same rule on the two operations that place from a revision a request MOVES to: an
/// update of a running service, and a task set of an `EXTERNAL`-controlled one.
///
/// Each names Fargate through a capacity-provider strategy — the other way a placement says
/// where it runs — over a revision validated against `EC2` only, and carries a
/// `networkConfiguration` its network mode gives nothing to configure, so both rules apply
/// and the launch type is the one answered.
#[tokio::test]
async fn an_update_and_a_task_set_judge_the_launch_type_first_too() {
    let router = service();
    let setup = "update and task-set order";
    cluster_with_capacity(&router, "moved", setup).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "moved",
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        setup,
    )
    .await;
    let bridge = register_in_mode(&router, "movedfam", "bridge", &["EC2"], setup).await;
    // The service the update moves: created on the launch type its revision supports.
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": "moved",
            "serviceName": "movedsvc",
            "taskDefinition": bridge,
            "desiredCount": 0,
            "launchType": "EC2",
        }),
        setup,
    )
    .await;
    // And the service a task set belongs to: "This is used when a service uses the EXTERNAL
    // deployment controller type."
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": "moved",
            "serviceName": "movedext",
            "desiredCount": 0,
            "deploymentController": { "type": "EXTERNAL" },
        }),
        setup,
    )
    .await;

    let fargate_strategy = json!([{ "capacityProvider": "FARGATE", "weight": 1 }]);
    for (case, operation, request) in [
        (
            "UpdateService moving the service onto a Fargate strategy",
            "UpdateService",
            json!({
                "cluster": "moved",
                "service": "movedsvc",
                "taskDefinition": bridge,
                "capacityProviderStrategy": fargate_strategy,
                // "When switching from launch type to capacity provider strategy ... you
                // must force a new deployment", which is a rule of its own and not the one
                // under test.
                "forceNewDeployment": true,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
        ),
        (
            "CreateTaskSet placing the revision on a Fargate strategy",
            "CreateTaskSet",
            json!({
                "cluster": "moved",
                "service": "movedext",
                "taskDefinition": bridge,
                "capacityProviderStrategy": fargate_strategy,
                "networkConfiguration": awsvpc_network_configuration(),
            }),
        ),
    ] {
        let (code, message) = refusal(&router, operation, request, case).await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected an {INVALID_PARAMETER}, got {code} with {message:?}"
        );
        assert_eq!(
            message, FARGATE_UNSUPPORTED,
            "case {case:?}: expected the refusal to read {FARGATE_UNSUPPORTED:?}, got \
             {message:?}"
        );
    }

    // And the withdrawn-platform entitlement on the one of those two the first table could
    // not reach: a task set names a `platformVersion` of its own.
    for version in service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::RETIRED_PLATFORM_VERSIONS
    {
        let case = format!("CreateTaskSet with the withdrawn platformVersion {version}");
        let (status, body) = answered(
            &router,
            "CreateTaskSet",
            json!({
                "cluster": "moved",
                "service": "movedext",
                "taskDefinition": bridge,
                "launchType": "EC2",
                "platformVersion": version,
            }),
        )
        .await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the entitlement refusal's status, got {status} with \
             {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains("AccessDeniedException"),
            "case {case:?}: expected an AccessDeniedException, got {body}"
        );
        assert_eq!(
            body["message"].as_str().unwrap_or_default(),
            PLATFORM_ACCESS_DENIED,
            "case {case:?}: expected the fixed sentence for this condition, got {body}"
        );
    }

    // Nothing those refusals accepted: the service still runs on the launch type it was
    // created with, and its EXTERNAL sibling holds no task set.
    let case = "after the refusals";
    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": "moved", "services": ["movedsvc", "movedext"] }),
        case,
    )
    .await;
    let services = list(&described, "services", case);
    assert_eq!(
        services.len(),
        2,
        "case {case:?}: expected both services to be described, got {described}"
    );
    assert_eq!(
        services[0]["launchType"],
        json!("EC2"),
        "case {case:?}: expected the refused update to leave the launch type alone, got {}",
        services[0]
    );
    assert!(
        list(&services[1], "taskSets", case).is_empty(),
        "case {case:?}: expected a refused CreateTaskSet to store no task set, got {}",
        services[1]
    );
}

// ---------------------------------------------------------------------------
// 3. The managed-scaling step sizes bound each other
// ---------------------------------------------------------------------------

/// One row of the step-size table: the `managedScaling` members the request names, and
/// whether the pair describes a scale-out step at all.
struct StepSizeCase {
    case: &'static str,
    /// `minimumScalingStepSize`, or `None` for a request that names none (the documented
    /// default of `1` then stands).
    minimum: Option<i64>,
    /// `maximumScalingStepSize`, or `None` for a request that names none (the documented
    /// default of `10000` then stands).
    maximum: Option<i64>,
    /// The values the stored provider must report, for a row that is accepted.
    stored: Option<(i64, i64)>,
}

#[tokio::test]
async fn managed_scaling_step_sizes_must_bound_each_other() {
    let router = service();
    let setup = "step sizes";
    create_cluster(&router, "scaling", setup).await;

    // The rows differ along which of the two members the request names, and where the pair
    // sits against the member range the model publishes (`ManagedScaling$minimumScalingStepSize`
    // / `$maximumScalingStepSize`: `@range` 1..10000 each): equal at each published bound,
    // a proper range, the pair one step past each other, and each member defaulted.
    let rows = [
        StepSizeCase {
            case: "equal at the published minimum",
            minimum: Some(1),
            maximum: Some(1),
            stored: Some((1, 1)),
        },
        StepSizeCase {
            case: "equal at the published maximum",
            minimum: Some(10000),
            maximum: Some(10000),
            stored: Some((10000, 10000)),
        },
        StepSizeCase {
            case: "a proper range",
            minimum: Some(2),
            maximum: Some(20),
            stored: Some((2, 20)),
        },
        StepSizeCase {
            case: "the minimum one step above the maximum",
            minimum: Some(2),
            maximum: Some(1),
            stored: None,
        },
        StepSizeCase {
            case: "the minimum at the published maximum, above a lower maximum",
            minimum: Some(10000),
            maximum: Some(9999),
            stored: None,
        },
        StepSizeCase {
            case: "a minimum against the defaulted maximum",
            minimum: Some(500),
            maximum: None,
            stored: Some((500, 10000)),
        },
        StepSizeCase {
            case: "a maximum at the published minimum against the defaulted minimum",
            minimum: None,
            maximum: Some(1),
            stored: Some((1, 1)),
        },
        StepSizeCase {
            case: "a maximum above the defaulted minimum",
            minimum: None,
            maximum: Some(7),
            stored: Some((1, 7)),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Each provider binds its OWN Auto Scaling group: a group another provider already
        // scales is a conflict of its own, which is the ordering case below.
        let group = format!("asg-step-{index}");
        create_auto_scaling_group(&router, &group, case).await;
        let mut scaling = json!({ "status": "ENABLED" });
        if let Some(minimum) = row.minimum {
            scaling["minimumScalingStepSize"] = json!(minimum);
        }
        if let Some(maximum) = row.maximum {
            scaling["maximumScalingStepSize"] = json!(maximum);
        }
        let name = format!("steps{index}");
        let request = json!({
            "name": name,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": group,
                "managedScaling": scaling,
            },
        });
        match row.stored {
            None => {
                let (code, message) =
                    refusal(&router, "CreateCapacityProvider", request, case).await;
                assert_eq!(
                    code, CLIENT_EXCEPTION,
                    "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code} with {message:?}"
                );
                assert_eq!(
                    message, STEP_SIZE_INVALID,
                    "case {case:?}: expected the step-size refusal, got {message:?}"
                );
                // And nothing was stored for it: the name is still free.
                let described = ok(
                    &router,
                    "DescribeCapacityProviders",
                    json!({ "capacityProviders": [name] }),
                    case,
                )
                .await;
                assert!(
                    list(&described, "capacityProviders", case).is_empty(),
                    "case {case:?}: expected a refused create to store no provider, got \
                     {described}"
                );
            }
            Some((minimum, maximum)) => {
                ok(&router, "CreateCapacityProvider", request, case).await;
                // Read the stored provider back through the operation that reports it, so
                // the accepted pair is the one a later caller sees.
                let described = ok(
                    &router,
                    "DescribeCapacityProviders",
                    json!({ "capacityProviders": [name] }),
                    case,
                )
                .await;
                let providers = list(&described, "capacityProviders", case);
                assert_eq!(
                    providers.len(),
                    1,
                    "case {case:?}: expected the created provider to read back, got \
                     {described}"
                );
                let stored = &providers[0]["autoScalingGroupProvider"]["managedScaling"];
                assert_eq!(
                    (
                        stored["minimumScalingStepSize"].as_i64(),
                        stored["maximumScalingStepSize"].as_i64()
                    ),
                    (Some(minimum), Some(maximum)),
                    "case {case:?}: expected the stored step sizes, got {stored}"
                );
            }
        }
    }

    // The ORDERING: a request whose step sizes describe no step AND whose group another
    // provider already scales is refused for the step sizes — the parameter is judged
    // before the request is weighed against what the account holds.
    let case = "a bad step size over a group already in use";
    create_auto_scaling_group(&router, "asg-taken", case).await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "holder",
            "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-taken" },
        }),
        case,
    )
    .await;
    let (code, message) = refusal(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "second",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": "asg-taken",
                "managedScaling": {
                    "status": "ENABLED",
                    "minimumScalingStepSize": 20,
                    "maximumScalingStepSize": 2,
                },
            },
        }),
        case,
    )
    .await;
    assert_eq!(
        code, CLIENT_EXCEPTION,
        "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code} with {message:?}"
    );
    assert_eq!(
        message, STEP_SIZE_INVALID,
        "case {case:?}: expected the step-size refusal and not {GROUP_IN_USE:?}, got \
         {message:?}"
    );

    // With the step sizes fixed, the SAME request earns the conflict rule's own refusal, so
    // the ordering above is the only thing that row proved.
    let case = "a sound step size over a group already in use";
    let (code, message) = refusal(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "second",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": "asg-taken",
                "managedScaling": {
                    "status": "ENABLED",
                    "minimumScalingStepSize": 2,
                    "maximumScalingStepSize": 20,
                },
            },
        }),
        case,
    )
    .await;
    assert_eq!(
        code, CLIENT_EXCEPTION,
        "case {case:?}: expected a {CLIENT_EXCEPTION}, got {code} with {message:?}"
    );
    assert_eq!(
        message, GROUP_IN_USE,
        "case {case:?}: expected the conflict rule's own refusal, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. A hookId is the form the service mints, and the action is taken once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_action_on_a_hook_is_taken_once_and_a_repeat_changes_nothing() {
    let router = service();
    let case = "hook action taken once";
    cluster_with_capacity(&router, "hooks", case).await;
    register_capacity(&router, "hooks", case).await;
    let definition = register_in_mode(&router, "hookfam", "bridge", &[], case).await;
    let body = ok(
        &router,
        "CreateService",
        json!({
            "cluster": "hooks",
            "serviceName": "hooked",
            "taskDefinition": definition,
            "desiredCount": 1,
            "deploymentConfiguration": {
                "strategy": "BLUE_GREEN",
                // "You must provide this parameter when you use the BLUE_GREEN deployment
                // strategy."
                "bakeTimeInMinutes": 1,
                "lifecycleHooks": [{
                    "targetType": "PAUSE",
                    "lifecycleStages": ["POST_SCALE_UP"],
                }],
            },
        }),
        case,
    )
    .await;
    let arn = string(&body["service"], "currentServiceDeployment", case);

    let hook = awaiting_hook(&deployment_awaiting_a_hook(&router, &arn, case).await, case);
    let hook_id = string(&hook, "hookId", case);

    // The id the rollout REPORTS is one this operation accepts, and the action is taken on
    // it: the hook it leaves behind is the one whose action succeeded.
    for attempt in ["the action", "the same action again"] {
        let case = format!("{case}: {attempt}");
        let answer = ok(
            &router,
            "ContinueServiceDeployment",
            json!({ "serviceDeploymentArn": arn, "hookId": hook_id, "action": "CONTINUE" }),
            &case,
        )
        .await;
        assert_eq!(
            string(&answer, "serviceDeploymentArn", &case),
            arn,
            "case {case:?}: expected the continued rollout's own ARN"
        );
        let deployment = describe_deployment(&router, &arn, &case).await;
        let hooks = list(&deployment, "lifecycleHookDetails", &case);
        assert_eq!(
            hooks.len(),
            1,
            "case {case:?}: expected the rollout to hold the one hook it reached, got \
             {deployment}"
        );
        assert_eq!(
            string(&hooks[0], "status", &case),
            SUCCEEDED,
            "case {case:?}: expected the hook to carry the status its taken action left, \
             got {deployment}"
        );
    }

    // The OTHER action on that same hook is not a repeat of a call that has had its effect:
    // the rollout has already continued past the stage, and this one asks it to roll back
    // instead, which it can no longer do — so it is refused and nothing moves.
    let case = "the other action on the hook that was continued";
    let (code, message) = refusal(
        &router,
        "ContinueServiceDeployment",
        json!({ "serviceDeploymentArn": arn, "hookId": hook_id, "action": "ROLLBACK" }),
        case,
    )
    .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "case {case:?}: expected an {INVALID_PARAMETER}, got {code} with {message:?}"
    );
    assert!(
        message.contains(&hook_id) && message.contains(&arn),
        "case {case:?}: expected the refusal to name {hook_id} and {arn}, got {message:?}"
    );
    let deployment = describe_deployment(&router, &arn, case).await;
    assert_eq!(
        string(&list(&deployment, "lifecycleHookDetails", case)[0], "status", case),
        SUCCEEDED,
        "case {case:?}: expected the refused action to leave the hook as its taken one did, \
         got {deployment}"
    );

    // A hookId of the FORM this service mints that names no hook of that rollout is still
    // refused, and says so about the hook and the rollout it was asked for.
    let case = "a well-formed id of no hook of this rollout";
    let unminted = "ecs-pause-00000000-0000-0000-0000-0000000000ff";
    let (code, message) = refusal(
        &router,
        "ContinueServiceDeployment",
        json!({ "serviceDeploymentArn": arn, "hookId": unminted }),
        case,
    )
    .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "case {case:?}: expected an {INVALID_PARAMETER}, got {code} with {message:?}"
    );
    assert!(
        message.contains(unminted) && message.contains(&arn),
        "case {case:?}: expected the refusal to name {unminted} and {arn}, got {message:?}"
    );

    // And a hookId outside that form is refused for its FORM, whichever part of it is
    // wrong: the id segment is spelled as a UUID, so a value that is not one names no hook.
    for (case, hook_id) in [
        ("an id segment that is not a UUID", "ecs-pause-not-a-real-hook-id"),
        ("an id segment of one group", "ecs-pause-0001"),
        ("no form at all", "not-a-paused-hook"),
    ] {
        let (code, message) = refusal(
            &router,
            "ContinueServiceDeployment",
            json!({ "serviceDeploymentArn": arn, "hookId": hook_id }),
            case,
        )
        .await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected an {INVALID_PARAMETER}, got {code} with {message:?}"
        );
        assert_eq!(
            message, "Invalid hookId: hookId must follow the format: ecs-{hookType}-{id}",
            "case {case:?}: expected the format refusal, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The cluster a request names is resolved before anything else it references
// ---------------------------------------------------------------------------

/// The modelled error code a `cluster` that resolves to nothing is answered in.
const CLUSTER_NOT_FOUND_CODE: &str = "ClusterNotFoundException";

/// `ClusterNotFoundException$message` for a reference that resolves to no cluster record
/// (`state`'s `CLUSTER_NOT_FOUND_MESSAGE`): fixed text, naming nothing from the request.
const CLUSTER_NOT_FOUND: &str = "Cluster not found.";

/// The same error's message for a cluster this account still holds as the record a delete
/// left behind (`state`'s `CLUSTER_INACTIVE_MESSAGE`) — the sentence that tells a deleted
/// cluster apart from one this account never held.
const CLUSTER_INACTIVE: &str = "The referenced cluster was inactive.";

/// The refusal an `autoScalingGroupProvider` naming a group Amazon EC2 Auto Scaling holds
/// none of earns (`state`'s `AUTO_SCALING_GROUP_NOT_FOUND_MESSAGE`) — the refusal the rows
/// below must NOT be answered with while their cluster is missing, and the one the same
/// request earns once it is there.
const GROUP_NOT_FOUND: &str = "The specified Auto Scaling group does not exist. Specify a \
                               valid Auto Scaling group and try again.";

/// The refusal a reference naming no capacity provider of the account earns (`state`'s
/// `CAPACITY_PROVIDER_MISSING_MESSAGE`).
const PROVIDER_MISSING: &str = "The specified capacity provider does not exist. Specify a \
                                valid name or ARN and try again.";

/// The group a provider of the setup already scales, and the one nothing does.
const BOUND_GROUP: &str = "asg-bound";
const FREE_GROUP: &str = "asg-held";

/// The capacity provider the setup creates, so a row can name one the account really holds.
const HELD_PROVIDER: &str = "boundprovider";

/// One row of the cluster-resolution table: an operation of the capacity-provider family and
/// the members it carries BESIDE the `cluster`, which the test fills in per cluster state.
struct ClusterFirstCase {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
}

/// The rows differ along the OPERATION and along what else the request references — a group
/// the account holds none of, one it holds, one another provider already scales, capacity
/// Amazon ECS would stand up, a provider name nothing holds, and one it does — because the
/// rule is about which of the two resolutions answers first, not about any one of them.
fn cluster_first_rows() -> Vec<ClusterFirstCase> {
    vec![
        ClusterFirstCase {
            case: "a create over a group the account holds none of",
            operation: "CreateCapacityProvider",
            request: || {
                json!({
                    "name": "ghostcluster",
                    "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-nowhere" },
                })
            },
        },
        ClusterFirstCase {
            case: "a create over a group the account does hold",
            operation: "CreateCapacityProvider",
            request: || {
                json!({
                    "name": "heldgroup",
                    "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn(FREE_GROUP) },
                })
            },
        },
        ClusterFirstCase {
            case: "a create over a group another provider already scales",
            operation: "CreateCapacityProvider",
            request: || {
                json!({
                    "name": "boundgroup",
                    "autoScalingGroupProvider": { "autoScalingGroupArn": BOUND_GROUP },
                })
            },
        },
        ClusterFirstCase {
            case: "a create of capacity Amazon ECS would stand up",
            operation: "CreateCapacityProvider",
            request: || {
                json!({ "name": "standsup", "managedInstancesProvider": managed_instances() })
            },
        },
        ClusterFirstCase {
            case: "a create under a name the account already holds",
            operation: "CreateCapacityProvider",
            request: || {
                json!({
                    "name": HELD_PROVIDER,
                    "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-nowhere" },
                })
            },
        },
        ClusterFirstCase {
            case: "a describe of a provider nothing holds",
            operation: "DescribeCapacityProviders",
            request: || json!({ "capacityProviders": ["nosuchprovider"] }),
        },
        ClusterFirstCase {
            case: "a describe of every provider in the cluster's scope",
            operation: "DescribeCapacityProviders",
            request: || json!({}),
        },
        ClusterFirstCase {
            case: "an update of a provider nothing holds",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": "nosuchprovider",
                    "autoScalingGroupProvider": { "managedScaling": { "status": "DISABLED" } },
                })
            },
        },
        ClusterFirstCase {
            case: "an update of a provider the account does hold",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": HELD_PROVIDER,
                    "autoScalingGroupProvider": { "managedScaling": { "status": "DISABLED" } },
                })
            },
        },
        ClusterFirstCase {
            case: "an association of a provider nothing holds",
            operation: "PutClusterCapacityProviders",
            request: || {
                json!({
                    "capacityProviders": ["nosuchprovider"],
                    "defaultCapacityProviderStrategy": [],
                })
            },
        },
        ClusterFirstCase {
            case: "an association of a provider the account does hold",
            operation: "PutClusterCapacityProviders",
            request: || {
                json!({
                    "capacityProviders": [HELD_PROVIDER],
                    "defaultCapacityProviderStrategy": [],
                })
            },
        },
    ]
}

#[tokio::test]
async fn the_cluster_a_request_names_is_resolved_before_what_else_it_references() {
    let router = service();
    let setup = "setup";
    create_auto_scaling_group(&router, FREE_GROUP, setup).await;
    create_auto_scaling_group(&router, BOUND_GROUP, setup).await;
    // A provider over one of those groups, so a row can name a provider — and a group —
    // the account really holds.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": HELD_PROVIDER,
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn(BOUND_GROUP) },
        }),
        setup,
    )
    .await;
    // The cluster every control row acts in, and the tombstone a delete leaves behind.
    create_cluster(&router, "resolved", setup).await;
    create_cluster(&router, "deleted", setup).await;
    ok(&router, "DeleteCluster", json!({ "cluster": "deleted" }), setup).await;

    // Every row, against a reference that resolves to NO cluster record and against the
    // record a delete left: the same error code either way, and the sentence that says
    // which of the two states the cluster is in.
    for (cluster, expected_message) in [
        ("nosuchcluster", CLUSTER_NOT_FOUND),
        ("deleted", CLUSTER_INACTIVE),
    ] {
        for row in cluster_first_rows() {
            let case = format!("{} in {cluster:?}", row.case);
            let mut request = (row.request)();
            request["cluster"] = json!(cluster);
            let (code, message) = refusal(&router, row.operation, request, &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (CLUSTER_NOT_FOUND_CODE, expected_message),
                "case {case:?}: expected the cluster to be resolved first, got {code} with \
                 {message:?}"
            );
        }
    }

    // Nothing above was written: a refused call leaves the account holding exactly the
    // providers it held, so the rows were refused rather than half-applied.
    let case = "the refused calls created no provider";
    let described = ok(&router, "DescribeCapacityProviders", json!({}), case).await;
    let names: Vec<String> = list(&described, "capacityProviders", case)
        .iter()
        .map(|provider| string(provider, "name", case))
        .collect();
    assert_eq!(
        names,
        vec![
            "FARGATE".to_string(),
            "FARGATE_SPOT".to_string(),
            HELD_PROVIDER.to_string()
        ],
        "case {case:?}: expected only the provider the setup created, got {described}"
    );

    // The CONTROLS: with the cluster there, the very resources the rows named earn their own
    // refusals — so what the rows proved is the ORDER, not that these conditions go
    // unreported.
    let case = "a create over a group the account holds none of, in a cluster that is there";
    let (code, message) = refusal(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "ghostgroup",
            "cluster": "resolved",
            "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-nowhere" },
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (CLIENT_EXCEPTION, GROUP_NOT_FOUND),
        "case {case:?}: expected the group's own refusal, got {code} with {message:?}"
    );

    let case = "an update of a provider nothing holds, in a cluster that is there";
    let (code, message) = refusal(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "nosuchprovider",
            "cluster": "resolved",
            "autoScalingGroupProvider": { "managedScaling": { "status": "DISABLED" } },
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (CLIENT_EXCEPTION, PROVIDER_MISSING),
        "case {case:?}: expected the provider's own refusal, got {code} with {message:?}"
    );

    let case = "an association of a provider nothing holds, in a cluster that is there";
    let (code, message) = refusal(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "resolved",
            "capacityProviders": ["nosuchprovider"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "case {case:?}: expected the association's own refusal, got {code} with {message:?}"
    );

    // And the create the first row asked for is ACCEPTED once it names the cluster that is
    // there: the refusals above were about the cluster, not about the request's own shape.
    let case = "the create the rows asked for, in a cluster that is there";
    let created = ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "heldgroup",
            "cluster": "resolved",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn(FREE_GROUP) },
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&created["capacityProvider"], "name", case),
        "heldgroup",
        "case {case:?}: expected the provider the create was asked for, got {created}"
    );
}

