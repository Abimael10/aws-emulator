// SPDX-License-Identifier: CC0-1.0

//! Where the deployment circuit breaker's ROLLBACK leaves a service: the rollout that puts
//! the last good revision back has to CONVERGE, so a client polling `DescribeServices` reads
//! a `PRIMARY` deployment of that revision in `rolloutState` `COMPLETED`.
//!
//! The rule under test:
//!
//! > When a monitor a service's deployment configuration turned on fails a rollout and the
//! > configuration asked for a rollback, "the service is rolled back to the last deployment
//! > that completed successfully" (`DeploymentCircuitBreaker$rollback`). That rollback is a
//! > rollout of the restored revision, so it travels the same deployment state machine as any
//! > other: `Deployment$rolloutState` "begins in an `IN_PROGRESS` state. When the service
//! > reaches a steady state, the deployment transitions to a `COMPLETED` state." So the
//! > service ends up reporting a `PRIMARY` deployment whose `taskDefinition` is the restored
//! > revision and whose `rolloutState` is `COMPLETED`, with `Service$taskDefinition` back on
//! > that revision, `desiredCount` untouched, that many tasks `RUNNING` and `HEALTHY`, and no
//! > task of the revision the breaker rejected left running. The rollback is owed by the
//! > CLOCK, so it is one of this emulator's own transient steps after the failure that asked
//! > for it ([`LONGEST_TRANSIENT_STEP_SECONDS`]) rather than a wait sized to how often a
//! > client polls: the two instants the state machine dates it by — the scheduler's failure
//! > entry and `Rollback$startedAt` — are one step apart.
//!
//! The rows differ along the infrastructure the service's tasks are placed on, which is what
//! the rule generalises over — a rollback is the service scheduler's, and every launch type it
//! schedules for takes one:
//!
//! * a Fargate service whose `awsvpc` tasks are each allocated their own elastic network
//!   interface, so every launch of every failing attempt walks the interface's own lifecycle
//!   before the task it carries can be probed at all;
//! * a service on capacity registered to the cluster, in the `bridge` network mode, with two
//!   tasks and a `COUNT` threshold — the failure count reached by two tasks failing together
//!   rather than by one replacement after another.
//!
//! What the breaker COUNTS, the thresholds it computes and the rollout state it leaves behind
//! are [`crate::health_check_rollout_failure`]'s table; this suite is about the rollout the
//! rollback itself performs reaching its steady state.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeServices`, `DescribeTasks` and `DescribeServiceDeployments`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_millisecond_resolution, awsvpc_network_configuration, count, create_cluster,
    describe_service_deployment, list, ok, poll_until, register_capacity, service, string,
};

/// The cluster both services below run in: one holds registered capacity, which the Fargate
/// row simply does not place onto ("the capacity providers are FARGATE or FARGATE_SPOT").
const CLUSTER: &str = "cbrollbackconverge";

/// `Deployment$rolloutState` of a rollout that reached the steady state it was rolling out.
const COMPLETED: &str = "COMPLETED";

/// `Container$healthStatus` of a container whose probe passed.
const HEALTHY: &str = "HEALTHY";

/// `Task$lastStatus` of a task that is up.
const RUNNING: &str = "RUNNING";

/// The `ServiceDeploymentStatus` a rollout the breaker rolled back ends in.
const ROLLBACK_SUCCESSFUL: &str = "ROLLBACK_SUCCESSFUL";

/// How the scheduler's own entry for the rollout the breaker failed ends, on the service's
/// event stream — the entry whose instant IS the instant the breaker failed that rollout.
const DEPLOYMENT_FAILED_EVENT: &str = "deployment failed: tasks failed to start.";

/// The longest a step of this emulator's own clock takes, in seconds.
///
/// The rollback a failed deployment earns is one of those steps: every transition this
/// emulator owes is a transient step of a few seconds, so the service is taken back to the
/// revision that completed within one of them — not after a wait sized to how often some
/// client happens to poll, which is time a deploy AWS self-heals never gets back.
const LONGEST_TRANSIENT_STEP_SECONDS: f64 = 5.0;

/// `HealthCheck$interval`, at the smallest the member documents ("You may specify between 5
/// and 300 seconds"), so a verdict lands without naming a value AWS refuses.
const INTERVAL_SECONDS: i64 = 5;

/// `HealthCheck$retries`, at the smallest the member documents ("You may specify between 1 and
/// 10 retries"): one execution per verdict.
const RETRIES: i64 = 1;

/// The image every container below runs, as a public registry reference.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:latest";

/// One container definition whose essential container stays up and runs `probe` as its health
/// check.
fn probing_container(probe: &str) -> Value {
    json!({
        "name": "app",
        "image": IMAGE,
        "memory": 128,
        "command": ["sh", "-c", "sleep infinity"],
        "essential": true,
        "healthCheck": {
            "command": ["CMD-SHELL", probe],
            "interval": INTERVAL_SECONDS,
            "timeout": 2,
            "retries": RETRIES,
        },
    })
}

/// One row: the service, how its tasks are placed, and what the breaker it was created with
/// is configured with.
struct Row {
    case: &'static str,
    /// `Service$serviceName`, one per row.
    service: &'static str,
    /// The task-definition family this row rolls revisions of, one per row so the revisions
    /// of one row are not the revisions of another.
    family: &'static str,
    /// `Service$desiredCount` the service is created with, and the count a rollback leaves
    /// untouched.
    desired: i64,
    /// What the task definition declares about where it can run, and what the service's own
    /// placement names — the members that differ between a Fargate and a registered-capacity
    /// row.
    definition_members: fn() -> Value,
    placement_members: fn() -> Value,
    /// The `deploymentCircuitBreaker` the create configures.
    breaker: fn() -> Value,
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Fargate service whose awsvpc tasks each take their own interface",
            service: "fargaterollback",
            family: "cbrollbackfargate",
            desired: 1,
            definition_members: || {
                json!({
                    "requiresCompatibilities": ["FARGATE"],
                    "networkMode": "awsvpc",
                    "cpu": "256",
                    "memory": "512",
                })
            },
            placement_members: || {
                let mut members = json!({ "launchType": "FARGATE" });
                members["networkConfiguration"] = awsvpc_network_configuration();
                members
            },
            // "By default, the threshold configuration uses a type of BOUNDED_PERCENT with a
            // value of 50", whose product with a desired count of 1 clamps up to 3 — three
            // failing launches, one replacement after another.
            breaker: || json!({ "enable": true, "rollback": true }),
        },
        Row {
            case: "two bridge-mode tasks on capacity registered to the cluster",
            service: "ec2rollback",
            family: "cbrollbackec2",
            desired: 2,
            definition_members: || json!({ "networkMode": "bridge" }),
            placement_members: || json!({ "launchType": "EC2" }),
            breaker: || {
                json!({
                    "enable": true,
                    "rollback": true,
                    "thresholdConfiguration": { "type": "COUNT", "value": 2 },
                })
            },
        },
    ]
}

/// Register one revision of `family` whose container runs `probe`, and answer its ARN.
async fn register_revision(
    router: &MultiServiceRouter,
    row: &Row,
    probe: &str,
    case: &str,
) -> String {
    let mut request = json!({
        "family": row.family,
        "containerDefinitions": [probing_container(probe)],
    });
    crate::ecs_wire::with_members(&mut request, (row.definition_members)());
    let body = ok(router, "RegisterTaskDefinition", request, case).await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// The epoch-seconds instant `member` of `body` names.
fn instant(body: &Value, member: &str, case: &str) -> f64 {
    assert_millisecond_resolution(case, member, &body[member]);
    body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected an instant for {member}, got {body}"))
}

/// The instant the scheduler wrote the FAILURE of the rejected rollout at, read off the
/// service's own event stream — the instant everything the breaker then owes falls due at.
fn failure_instant(service_view: &Value, case: &str) -> f64 {
    let event = list(service_view, "events", case)
        .into_iter()
        .find(|event| string(event, "message", case).ends_with(DEPLOYMENT_FAILED_EVENT))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the scheduler's failure event, got {service_view}")
        });
    instant(&event, "createdAt", case)
}

/// One service, as `DescribeServices` reports it.
async fn describe(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// The service's current (`PRIMARY`) deployment, which is the first of the deployments it
/// reports.
fn primary(service_view: &Value, case: &str) -> Value {
    list(service_view, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {service_view}"))
}

/// Every task of `name`'s service that is still wanted running, as `DescribeTasks` reports
/// them.
async fn running_tasks(router: &MultiServiceRouter, name: &str, case: &str) -> Vec<Value> {
    let listed = ok(
        router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "serviceName": name, "desiredStatus": RUNNING }),
        case,
    )
    .await;
    let arns: Vec<Value> = list(&listed, "taskArns", case);
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
        case,
    )
    .await;
    list(&described, "tasks", case)
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_rollout_is_rolled_back_to_a_converged_primary_deployment() {
    let router = service();
    create_cluster(&router, CLUSTER, "the cluster both rows run in").await;
    register_capacity(&router, CLUSTER, "the capacity the EC2 row places onto").await;

    let rows = rows();
    let mut healthy = Vec::new();
    let mut failing = Vec::new();
    for row in &rows {
        healthy.push(register_revision(&router, row, "exit 0", row.case).await);
        failing.push(register_revision(&router, row, "exit 1", row.case).await);
    }

    // Every service is created on the revision whose probe PASSES, so each has a deployment
    // that completed successfully — the one the rollback puts back.
    for (index, row) in rows.iter().enumerate() {
        let mut request = json!({
            "cluster": CLUSTER,
            "serviceName": row.service,
            "taskDefinition": healthy[index],
            "desiredCount": row.desired,
            "deploymentConfiguration": { "deploymentCircuitBreaker": (row.breaker)() },
        });
        crate::ecs_wire::with_members(&mut request, (row.placement_members)());
        ok(&router, "CreateService", request, row.case).await;
    }

    // The first rollout converges: that is the deployment the breaker's rollback restores.
    let mut restored_deployments = Vec::new();
    for row in &rows {
        let case = row.case;
        let desired = row.desired;
        let service_view = poll_until(
            case,
            "the first rollout to complete with every task running",
            || describe(&router, row.service, case),
            move |service_view| {
                string(&primary(service_view, case), "rolloutState", case) == COMPLETED
                    && count(service_view, "runningCount", case) == desired
            },
        )
        .await;
        restored_deployments.push(string(&primary(&service_view, case), "id", case));
    }

    // And each is moved onto the revision whose probe NEVER passes.
    let mut rollouts = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut request = json!({
            "cluster": CLUSTER,
            "service": row.service,
            "taskDefinition": failing[index],
        });
        crate::ecs_wire::with_members(&mut request, (row.placement_members)());
        let body = ok(&router, "UpdateService", request, row.case).await;
        rollouts.push(string(&body["service"], "currentServiceDeployment", row.case));
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let desired = row.desired;
        let restored = healthy[index].clone();
        let rejected = failing[index].clone();
        let restored_for_poll = restored.clone();
        // The rollback the breaker performs is a rollout of the restored revision, so the
        // service converges on it: the PRIMARY deployment is that revision's and reports the
        // steady state it reached.
        let service_view = poll_until(
            case,
            "the service to be rolled back to the revision that completed, with a PRIMARY \
             deployment reporting the steady state it reached",
            || describe(&router, row.service, case),
            move |service_view| {
                let deployment = primary(service_view, case);
                string(service_view, "taskDefinition", case) == restored_for_poll
                    && string(&deployment, "taskDefinition", case) == restored_for_poll
                    && string(&deployment, "rolloutState", case) == COMPLETED
                    && count(service_view, "runningCount", case) == desired
            },
        )
        .await;
        let deployment = primary(&service_view, case);
        assert_ne!(
            string(&deployment, "id", case),
            restored_deployments[index],
            "case {case:?}: expected the rollback to run the restored revision under a \
             deployment of its own, got {deployment}"
        );
        assert_eq!(
            count(&service_view, "desiredCount", case),
            desired,
            "case {case:?}: expected the rollback to leave the desired count alone, got \
             {service_view}"
        );
        assert_eq!(
            count(&deployment, "runningCount", case),
            desired,
            "case {case:?}: expected the restored deployment to run the desired count, got \
             {deployment}"
        );

        // The tasks the service is left holding are the restored revision's, up and passing
        // their probe — and none of the rejected revision's is left running.
        let tasks = poll_until(
            case,
            "the restored revision's tasks to be running and healthy",
            || async {
                json!({ "tasks": running_tasks(&router, row.service, case).await })
            },
            move |reported| {
                let tasks = list(reported, "tasks", case);
                tasks.len() == usize::try_from(desired).unwrap_or_default()
                    && tasks.iter().all(|task| {
                        string(task, "taskDefinitionArn", case) == restored
                            && string(task, "lastStatus", case) == RUNNING
                            && string(task, "healthStatus", case) == HEALTHY
                    })
            },
        )
        .await;
        for task in list(&tasks, "tasks", case) {
            assert_ne!(
                string(&task, "taskDefinitionArn", case),
                rejected,
                "case {case:?}: expected no task of the rejected revision to survive, got \
                 {task}"
            );
        }

        // The rollout the breaker failed is the one that carries the rollback, and it ended
        // in the status a rolled-back rollout ends in.
        let record = describe_service_deployment(&router, &rollouts[index], case).await;
        assert_eq!(
            string(&record, "status", case),
            ROLLBACK_SUCCESSFUL,
            "case {case:?}: expected the failed rollout to end rolled back, got {record}"
        );

        // And it started one transient step after the failure that asked for it. Both
        // instants are the store's own — `Rollback$startedAt` and the instant the scheduler's
        // failure entry is dated at — so this is the state machine's arithmetic rather than
        // how fast the host ran.
        let failed_at = failure_instant(&service_view, case);
        let started_at = instant(&record["rollback"], "startedAt", case);
        let waited = started_at - failed_at;
        assert!(
            waited > 0.0 && waited <= LONGEST_TRANSIENT_STEP_SECONDS,
            "case {case:?}: expected the rollback to start more than nothing and at most \
             {LONGEST_TRANSIENT_STEP_SECONDS}s after the failure at {failed_at}, got \
             {started_at} ({waited}s later)"
        );
    }
}
