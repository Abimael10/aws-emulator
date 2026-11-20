// SPDX-License-Identifier: CC0-1.0

//! A LIVE Application Load Balancer-attached `awsvpc` Fargate Amazon ECS service, over every
//! network mode a caller may try to move it onto.
//!
//! RULE: **a service's task-definition `networkMode` decides how its tasks are reached — an
//! `awsvpc` task is registered at the address of the elastic network interface it was allocated,
//! which is why the target group its binding names carries the `ip` target type — and it decides
//! which launch type the revision was validated for. So once such a service is serving through
//! its load balancer, EVERY move onto another network mode is refused `400`: a revision asking
//! for this service's launch type in another mode cannot be registered at all, and one that does
//! register in another mode is refused by `UpdateService` with `InvalidParameterException`,
//! leaving the service on its own revision with the same registration healthy and the same bytes
//! served. Only a revision in the mode the service's configuration and its binding were
//! validated for moves it.**
//!
//! The grounds. `NetworkMode` is `bridge`, `host`, `awsvpc` or `none` (`smithy_query(query =
//! "shape", shape = "NetworkMode")` on the ECS model), and `LoadBalancer$targetGroupArn` is the
//! pairing half: "If your service's task definition uses the `awsvpc` network mode, you must
//! choose `ip` as the target type, not `instance`. Do this when creating your target groups
//! because tasks that use the `awsvpc` network mode are associated with an elastic network
//! interface, not an Amazon EC2 instance. This network mode is required for the Fargate launch
//! type." The placement half is `TaskDefinition$compatibilities` — "Amazon ECS validates the task
//! definition parameters with those supported by the launch type" — so a revision never validated
//! against the launch type the service places with cannot be placed with it.
//! `InvalidParameterException` is what `UpdateService` models for a parameter that is not valid
//! for the state the request names (`smithy_query(query = "operation", operation =
//! "UpdateService")`: client, HTTP 400).
//!
//! The rows span what the rule generalises over: every other value of the `NetworkMode` enum,
//! each one both as a registration that asks for the service's launch type and as an update onto
//! a revision that registers without it, crossed with what the service, the target group and the
//! client report AFTER the refusal.
//!
//! The topology this stands on — EC2's network and its security-group rule, the `ip` target group
//! and the listener that forwards to it, the Fargate revision whose container starts a server and
//! the service that places it — is the fixture its sibling `all/ecs_alb_serves_its_awsvpc_tasks_container.rs`
//! owns, so this suite CALLS that one's helpers rather than keeping a second copy of them. Every
//! clock-owed move is polled for until it holds, never slept for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The fixture for "an Application Load Balancer in front of an `awsvpc` Amazon ECS service", and
// the constants its helpers place and assert with.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    create_service, ecs, ecs_ok, emulator, front_door, get, health, healthy_target,
    register_revision, seed_network, serving_command, CLUSTER, CONTAINER,
    FARGATE_NETWORK_MODE_MESSAGE, HEALTHY, LAUNCH_TYPE_MESSAGE,
};

/// The service this suite runs, the family its revisions are registered in, and the family every
/// candidate revision in another mode is registered in.
const SERVICE: &str = "modemovesvc";
const LIVE_FAMILY: &str = "modemovelive";
const CANDIDATE_FAMILY: &str = "modemovecandidate";

/// The port the container's server listens on, which is the `containerPort` the revision declares
/// and the port the binding and the target group name.
const PORT: i64 = 80;

/// The marker the live revision's container writes into the document it serves, so a body
/// anything else produced fails an assertion.
const MARKER: &str = "MODE-MOVE-REVISION-A";

/// The statuses read here: the one every answered call carries, and the one a refusal does.
const OK: u16 = 200;
const REFUSED: u16 = 400;

/// Every network mode other than the `awsvpc` one the live service runs — the whole rest of the
/// ECS model's `NetworkMode` enum. The `bridge` row is the one a client was reported against; it
/// stays in the table because the rule is about the MODE being another one, and a table that
/// skipped the reported row would leave the class covered everywhere but where it was seen.
const OTHER_MODES: [&str; 3] = ["bridge", "host", "none"];

/// One ECS call that must be refused, answered with the short name of the modelled error and the
/// message it carries — the two things a client keys on.
async fn ecs_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> (String, String) {
    let (status, parsed) = ecs(router, operation, body).await;
    assert_eq!(
        status, REFUSED,
        "case {case:?}: expected ECS {operation} to answer {REFUSED}, got {status} with {parsed}"
    );
    let code = parsed["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let message = parsed["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {parsed}"))
        .to_string();
    (code, message)
}

/// The one container definition every candidate revision declares, in `network_mode`.
///
/// A revision in the `none` mode publishes no port mapping: a task of it has no network of its own
/// for one.
fn candidate_container(network_mode: &str) -> Value {
    let mut container = json!({
        "name": CONTAINER,
        "image": "public.ecr.aws/docker/library/nginx:alpine",
        "memory": 128,
    });
    if network_mode != "none" {
        container["portMappings"] = json!([{ "containerPort": PORT }]);
    }
    container
}

/// The revision the service reports it runs.
async fn running_revision(router: &MultiServiceRouter, case: &str) -> String {
    let described = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [SERVICE] }),
    )
    .await;
    described["services"][0]["taskDefinition"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a taskDefinition, got {described}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_alb_attached_awsvpc_service_refuses_every_move_into_another_network_mode() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // The topology the rule stands on: an `awsvpc` Fargate service with one task, attached to the
    // `ip` target group the listener forwards to, converged until its target is healthy and the
    // load balancer serves the container's own bytes.
    let case = "a live ALB-attached awsvpc Fargate service";
    let command = serving_command(MARKER);
    let borrowed: Vec<&str> = command.iter().map(String::as_str).collect();
    let revision = register_revision(&router, LIVE_FAMILY, PORT, &borrowed).await;
    let (group, dns_name) =
        front_door(&router, &network, case, "modemove-tg", "modemove-lb", PORT).await;
    create_service(&router, &network, SERVICE, &revision, &group, PORT).await;
    let registered = healthy_target(&router, case, &group, SERVICE).await;
    let serving = get(&router, &dns_name, "/").await;
    assert_eq!(
        (serving.status, serving.body.trim()),
        (OK, MARKER),
        "case {case:?}: expected the live revision's own body, got {} with {:?}",
        serving.status,
        serving.body
    );

    for mode in OTHER_MODES {
        // Half one of the rule: a revision that asks for the launch type this service places with
        // cannot even be REGISTERED in another mode, so there is no such revision for a move to
        // name.
        let case = format!("registering a Fargate revision in the {mode} network mode");
        let (code, message) = ecs_refusal(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": format!("{CANDIDATE_FAMILY}fargate{mode}"),
                "networkMode": mode,
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "containerDefinitions": [candidate_container(mode)],
            }),
            &case,
        )
        .await;
        assert_eq!(
            code, "ClientException",
            "case {case:?}: expected the modelled ClientException, got {code} with {message:?}"
        );
        assert_eq!(
            message, FARGATE_NETWORK_MODE_MESSAGE,
            "case {case:?}: expected {FARGATE_NETWORK_MODE_MESSAGE:?}, got {message:?}"
        );

        // Half two: a revision in that mode which DOES register — one asking for no Fargate
        // compatibility of its own — is one this service's placement was never validated against,
        // so the move onto it is refused.
        let case = format!("moving the live service onto a {mode} revision");
        let candidate = ecs_ok(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": format!("{CANDIDATE_FAMILY}{mode}"),
                "networkMode": mode,
                "containerDefinitions": [candidate_container(mode)],
            }),
        )
        .await;
        let candidate = candidate["taskDefinition"]["taskDefinitionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {candidate}"))
            .to_string();
        let (code, message) = ecs_refusal(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": SERVICE,
                "taskDefinition": candidate,
            }),
            &case,
        )
        .await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected the modelled InvalidParameterException, got {code} with \
             {message:?}"
        );
        assert_eq!(
            message, LAUNCH_TYPE_MESSAGE,
            "case {case:?}: expected {LAUNCH_TYPE_MESSAGE:?}, got {message:?}"
        );

        // And the refusal changed nothing: the service still runs its own revision, the same
        // target is still healthy, and the load balancer still serves that revision's bytes.
        let running = running_revision(&router, &case).await;
        assert_eq!(
            running, revision,
            "case {case:?}: a refused update must leave the service on {revision}, got {running}"
        );
        let still = health(&router, &group).await;
        assert_eq!(
            still,
            vec![(registered.clone(), HEALTHY.to_string())],
            "case {case:?}: a refused update must leave the registration alone, got {still:?}"
        );
        let served = get(&router, &dns_name, "/").await;
        assert_eq!(
            (served.status, served.body.trim()),
            (OK, MARKER),
            "case {case:?}: a refused update must leave the served bytes alone, got {} with {:?}",
            served.status,
            served.body
        );
    }

    // The control: a revision in the mode the service's configuration and its binding WERE
    // validated for is accepted, and the service reports it runs that revision. What the load
    // balancer then serves for such a move is the sibling suite's own row.
    let case = "moving the live service onto another awsvpc Fargate revision";
    let control = register_revision(&router, LIVE_FAMILY, PORT, &borrowed).await;
    assert_ne!(
        control, revision,
        "case {case:?}: the control must be a NEW revision of the family, got {control}"
    );
    ecs_ok(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": SERVICE,
            "taskDefinition": control.clone(),
        }),
    )
    .await;
    let running = running_revision(&router, case).await;
    assert_eq!(
        running, control,
        "case {case:?}: expected the accepted update to move the service onto {control}, got \
         {running}"
    );
}
