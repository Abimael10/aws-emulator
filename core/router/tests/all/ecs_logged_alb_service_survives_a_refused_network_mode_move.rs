// SPDX-License-Identifier: CC0-1.0

//! An ALB-fronted `awsvpc` Amazon ECS service whose container delivers its own output through the
//! `awslogs` driver, across a REFUSED move onto another network mode.
//!
//! RULE: **a converged load-balanced Amazon ECS service answers a request on its load balancer's
//! published address with the bytes its container wrote and delivers that same container's
//! standard output into the log group its log configuration names; a refused `UpdateService`
//! changes none of that — a refusal writes nothing, so the service stays on its own revision with
//! the same target healthy, the same address serving the same bytes and the same delivered lines
//! readable — and a move onto a revision in the mode the service's binding and placement WERE
//! validated for is accepted, after which the task it places serves its own bytes and delivers
//! its own output into that same group.**
//!
//! The grounds. `LoadBalancer$targetGroupArn` binds a service's tasks into a target group and
//! "tasks that use the `awsvpc` network mode are associated with an elastic network interface",
//! so the target is the task's own address and what answers there is what the container's own
//! `command` — "the command that's passed to the container" — started. `LogConfiguration
//! $logDriver` names "the log driver to use for the container" and the `awslogs` driver "sends
//! container logs to Amazon CloudWatch Logs", under a stream its `awslogs-stream-prefix`
//! documentation names from the prefix, the container's name and the task's id.
//! `TaskDefinition$compatibilities` is the placement half — "Amazon ECS validates the task
//! definition parameters with those supported by the launch type" — so a revision never validated
//! against the launch type the service places with cannot be placed with it, and `UpdateService`
//! refuses such a request with `InvalidParameterException` (`smithy_query(query = "operation",
//! operation = "UpdateService")`: client, HTTP 400). A modelled refusal is an answer, not a
//! change: nothing of the request it refused is written, so everything the service was serving
//! and delivering before it is what it serves and delivers after it.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! owns the service, its task, that task's interface and the container's program, Elastic Load
//! Balancing v2 holds the registration and answers the client, and CloudWatch Logs holds the
//! group, the stream and the events (with Amazon EC2 owning the network the three stand on and
//! IAM the execution role the driver is initialised as). Everything is driven through the real
//! operations on ONE shared engine, so every assertion is about what each service persisted.
//!
//! The rows differ along what the rule generalises over: the network mode the refused move names —
//! every value of the ECS model's `NetworkMode` enum other than the `awsvpc` one the service runs
//! (`smithy_query(query = "shape", shape = "NetworkMode")`: `bridge`, `host`, `awsvpc`, `none`),
//! with the `bridge` row the one a client was reported against — crossed with the three things
//! that must be unchanged after each refusal: the revision the service runs, the health of its
//! registration, the bytes the published address serves and the lines its stream carries. The last
//! row is the accepted move, which is what makes the refusals above a refusal of the MODE rather
//! than of updating at all.
//!
//! The topology this stands on is owned by its two siblings —
//! `all/ecs_alb_serves_its_awsvpc_tasks_container.rs` owns the network, the front door and the
//! service, `all/ecs_served_containers_output_reaches_its_log_stream.rs` owns the emulator
//! carrying CloudWatch Logs and IAM, the execution role and the reading of a stream — so this
//! suite CALLS their helpers rather than keeping a second copy of them. Every clock-owed move is
//! polled for until it holds, never slept for, and each deadline is generous: a row asserts that
//! the move HAPPENS, not how fast this host runs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The fixture for "an Application Load Balancer in front of an `awsvpc` Amazon ECS service".
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    create_service, ecs, ecs_ok, front_door, get, health, healthy_target, seed_network, CLUSTER,
    CONTAINER, HEALTHY, LAUNCH_TYPE_MESSAGE,
};
// The fixture for "that same service, logging through the `awslogs` driver".
use crate::ecs_served_containers_output_reaches_its_log_stream::{
    delivered_lines, emulator, execution_role, id_of, logs_ok,
};

/// The region the driver's configuration names, which is the region every helper above signs for.
const REGION: &str = "us-east-1";

/// The service this suite runs, and the families its revisions are registered in.
const SERVICE: &str = "loggedmove";
const LIVE_FAMILY: &str = "loggedmovelive";
const REPAIRED_FAMILY: &str = "loggedmoverepaired";
const CANDIDATE_FAMILY: &str = "loggedmovecandidate";

/// The group every container below delivers to, and the stream prefix its configuration names.
const LOG_GROUP: &str = "/ecs/logged-move";
const STREAM_PREFIX: &str = "ecs";

/// The image every container below runs, and the port its server listens on — the `containerPort`
/// the revision declares and the port the binding and the target group name.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";
const PORT: i64 = 80;

/// The directory the container's server serves, which its own program writes the document into.
const SERVED_DIRECTORY: &str = "/www";

/// The markers the two revisions write, and the word each one's startup line carries after it, so
/// a body or a line anything else produced fails an assertion.
const LIVE_MARKER: &str = "LOGGED-MOVE-A";
const LIVE_WORD: &str = "httpd-start";
const REPAIRED_MARKER: &str = "LOGGED-MOVE-B";
const REPAIRED_WORD: &str = "httpd-restart";

/// The status every answered call carries, and the one a refusal does.
const OK: u16 = 200;
const REFUSED: u16 = 400;

/// `Task$desiredStatus` of the tasks a running service placed.
const RUNNING: &str = "RUNNING";

/// How long a clock-owed move is waited for before a row gives up, and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// Every network mode other than the `awsvpc` one the live service runs — the whole rest of the
/// ECS model's `NetworkMode` enum.
const OTHER_MODES: [&str; 3] = ["bridge", "host", "none"];

/// The program one revision's container runs: it writes the document the load balancer will serve,
/// reports on its standard output that it is starting the server, and then runs that server in the
/// foreground, so the container goes on running — and serving — for as long as the service does.
fn program(marker: &str, word: &str) -> String {
    format!(
        "mkdir -p {SERVED_DIRECTORY} && echo {marker} > {SERVED_DIRECTORY}/index.html && \
         echo \"{marker} {word}\" && httpd -f -p {PORT} -h {SERVED_DIRECTORY}"
    )
}

/// The startup line one revision's container writes.
fn startup_line(marker: &str, word: &str) -> String {
    format!("{marker} {word}")
}

/// Register one `awsvpc`, Fargate-compatible revision of `family` whose single container serves
/// `marker` over HTTP and delivers its own output through the `awslogs` driver as the execution
/// role, and answer its ARN.
async fn register_logged_revision(
    router: &MultiServiceRouter,
    family: &str,
    marker: &str,
    word: &str,
    role_arn: &str,
) -> String {
    let registered = ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "memory": 128,
                "portMappings": [{ "containerPort": PORT, "protocol": "tcp" }],
                "command": ["/bin/sh", "-c", program(marker, word)],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the registration of {family} must answer an ARN, got {registered}"))
        .to_string()
}

/// The one container definition a candidate revision in `network_mode` declares.
///
/// A revision in the `none` mode publishes no port mapping: a task of it has no network of its own
/// for one.
fn candidate_container(network_mode: &str) -> Value {
    let mut container = json!({
        "name": CONTAINER,
        "image": IMAGE,
        "memory": 128,
    });
    if network_mode != "none" {
        container["portMappings"] = json!([{ "containerPort": PORT }]);
    }
    container
}

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

/// The ids of the tasks the service is running, which are the ids the driver names their streams
/// after.
async fn running_task_ids(router: &MultiServiceRouter) -> Vec<String> {
    let listed = ecs_ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": SERVICE, "desiredStatus": RUNNING }),
    )
    .await;
    listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|arn| id_of(arn).to_string())
        .collect()
}

/// The stream the container of the task `task_id` delivers to: the prefix the configuration names,
/// the container's name and that task's id.
fn stream_of(task_id: &str) -> String {
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// Every stream the log group holds, in the order CloudWatch Logs reports them.
async fn streams(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let described = logs_ok(
        router,
        case,
        "DescribeLogStreams",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    described["logStreams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stream| stream["logStreamName"].as_str())
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_network_mode_move_leaves_a_logged_alb_service_serving_and_delivering() {
    let router = emulator();
    let network = seed_network(&router).await;
    let role_arn = execution_role(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // The topology the rule stands on: the group the driver delivers into, an `awsvpc` Fargate
    // service with one task attached to the `ip` target group the listener forwards to, converged
    // until its target is healthy.
    let case = "a live ALB-attached awsvpc Fargate service logging through awslogs";
    logs_ok(
        &router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    let revision =
        register_logged_revision(&router, LIVE_FAMILY, LIVE_MARKER, LIVE_WORD, &role_arn).await;
    let (group, dns_name) =
        front_door(&router, &network, case, "loggedmove-tg", "loggedmove-lb", PORT).await;
    create_service(&router, &network, SERVICE, &revision, &group, PORT).await;
    let registered = healthy_target(&router, case, &group, SERVICE).await;

    // What the published address serves is that container's own document, and what the group's
    // stream carries is that same container's own startup line.
    let serving = get(&router, &dns_name, "/").await;
    assert_eq!(
        (serving.status, serving.body.trim()),
        (OK, LIVE_MARKER),
        "case {case:?}: expected the live revision's own body, got {} with {:?}",
        serving.status,
        serving.body
    );
    let live_ids = running_task_ids(&router).await;
    assert_eq!(
        live_ids.len(),
        1,
        "case {case:?}: expected exactly one running task, got {live_ids:?}"
    );
    let live_stream = stream_of(&live_ids[0]);
    let live_line = startup_line(LIVE_MARKER, LIVE_WORD);
    let delivered = delivered_lines(&router, case, LOG_GROUP, &live_stream, &live_line).await;
    assert!(
        delivered.iter().any(|message| message.contains(&live_line)),
        "case {case:?}: expected a line holding {live_line:?} in {live_stream:?}, got {delivered:?}"
    );

    for mode in OTHER_MODES {
        // A revision in that mode which DOES register — one asking for no Fargate compatibility of
        // its own — is one this service's placement was never validated against, so the move onto
        // it is refused.
        let case = format!("moving the logged service onto a {mode} revision");
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

        // And the refusal wrote nothing: the service still runs its own revision, the same target
        // is still healthy, the published address still serves that revision's bytes, and the
        // container's line is still readable in the stream its driver delivers to.
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
            (OK, LIVE_MARKER),
            "case {case:?}: a refused update must leave the served bytes alone, got {} with {:?}",
            served.status,
            served.body
        );
        let delivered =
            delivered_lines(&router, &case, LOG_GROUP, &live_stream, &live_line).await;
        assert!(
            delivered.iter().any(|message| message.contains(&live_line)),
            "case {case:?}: a refused update must leave the delivered lines readable, got \
             {delivered:?} in {live_stream:?}"
        );
        let held = streams(&router, &case).await;
        assert!(
            held.contains(&live_stream),
            "case {case:?}: expected {LOG_GROUP:?} to still hold {live_stream:?}, got {held:?}"
        );
    }

    // The accepted move: a revision in the mode the service's configuration and its binding WERE
    // validated for is taken, and the task it places both serves ITS marker and delivers ITS own
    // startup line into the same group — polled for, because the rollout is clock-owed.
    let case = "moving the logged service onto another awsvpc Fargate revision";
    let repaired = register_logged_revision(
        &router,
        REPAIRED_FAMILY,
        REPAIRED_MARKER,
        REPAIRED_WORD,
        &role_arn,
    )
    .await;
    ecs_ok(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": SERVICE,
            "taskDefinition": repaired.clone(),
        }),
    )
    .await;
    let running = running_revision(&router, case).await;
    assert_eq!(
        running, repaired,
        "case {case:?}: expected the accepted update to move the service onto {repaired}, got \
         {running}"
    );
    let deadline = Instant::now() + DEADLINE;
    let repaired_id = loop {
        let served = get(&router, &dns_name, "/").await;
        let ids = running_task_ids(&router).await;
        let replacement = ids.iter().find(|id| **id != live_ids[0]).cloned();
        if served.status == OK && served.body.trim() == REPAIRED_MARKER {
            if let Some(id) = replacement {
                break id;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the repaired revision's own body {REPAIRED_MARKER:?} from a \
             replacement task, got {} with {:?} and the running tasks {ids:?}",
            served.status,
            served.body
        );
        tokio::time::sleep(POLL).await;
    };
    let repaired_stream = stream_of(&repaired_id);
    let repaired_line = startup_line(REPAIRED_MARKER, REPAIRED_WORD);
    let delivered =
        delivered_lines(&router, case, LOG_GROUP, &repaired_stream, &repaired_line).await;
    assert!(
        delivered
            .iter()
            .any(|message| message.contains(&repaired_line)),
        "case {case:?}: expected a line holding {repaired_line:?} in {repaired_stream:?}, got \
         {delivered:?}"
    );
}
