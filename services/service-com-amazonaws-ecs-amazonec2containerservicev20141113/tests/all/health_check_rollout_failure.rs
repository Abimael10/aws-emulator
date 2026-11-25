// SPDX-License-Identifier: CC0-1.0

//! What the DEPLOYMENT CIRCUIT BREAKER makes of a rollout whose containers never pass their
//! HEALTH CHECK — the failure source `Deployment$failedTasks` names last — driven end to end
//! over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > A task of a service whose own scheduler places its tasks and whose essential container
//! > fails its declared health check is stopped by that scheduler, and the rollout that placed
//! > it counts the stop as one of its failed tasks (`Deployment$failedTasks`: "A task is
//! > considered a failure if the service scheduler can't launch the task, the task doesn't
//! > transition to a `RUNNING` state, or if it fails any of its defined health checks and is
//! > stopped"). Once those stops reach the failure threshold the service's deployment circuit
//! > breaker computes, that deployment leaves `IN_PROGRESS` for the TERMINAL `FAILED` state
//! > with a `rolloutStateReason` naming the breaker (`Deployment$rolloutState`: "If the service
//! > fails to reach a steady state and circuit breaker is turned on, the deployment transitions
//! > to a `FAILED` state. A deployment in `FAILED` state doesn't launch any new tasks"), and
//! > `DeploymentCircuitBreaker$rollback` decides where that leaves the service: "If rollback is
//! > on, when a service deployment fails, the service is rolled back to the last deployment
//! > that completed successfully." A breaker the request does not turn on fails nothing
//! > ("Determines whether to use the deployment circuit breaker logic for the service"), and
//! > neither do readings the scheduler is not acting on yet — the span
//! > `CreateService$healthCheckGracePeriodSeconds` names is "the period of time, in seconds,
//! > that the Amazon ECS service scheduler ignores unhealthy Elastic Load Balancing, VPC
//! > Lattice, and container health checks after a task has first started", so a rollout inside
//! > it has lost no task for the breaker to count.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the THRESHOLD the breaker computes, over the `ThresholdConfiguration$type` forms the
//!   model names (`smithy_query(query="shape", shape="ThresholdType")` -> COUNT,
//!   BOUNDED_PERCENT, UNBOUNDED_PERCENT): the documented default, which multiplies 50 by the
//!   desired count and clamps "to a minimum of `3`"; a count used directly; and an unbounded
//!   percentage of that count;
//! * how many health-check verdicts it takes to reach that threshold, and over how many
//!   tasks — a two-task service whose tasks fail together, and single-task services whose
//!   replacement launches have to fail one after another;
//! * whether `rollback` is on, which is the difference between a service taken back to the
//!   revision that completed and one left on the revision that failed;
//! * whether the breaker is turned ON at all;
//! * and whether the service asked the scheduler to IGNORE those checks for a grace period,
//!   which is the row where the failing readings exist and the breaker still counts nothing.
//!
//! The same rule reached through the other failure source — a launch Amazon ECS cannot make at
//! all — is the table in [`crate::deployment_circuit_breaker`], and what one task's failing
//! probe earns it is [`crate::container_health_checks`]; this suite is the accounting BETWEEN
//! them.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeServices` and `DescribeServiceDeployments`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, count, describe_service_deployment, list, ok, poll_until, service,
    string, POLL_INTERVAL,
};

/// The cluster every service below runs in.
const CLUSTER: &str = "healthbreaker";

/// The task-definition family every service below rolls out: revision 1 passes its probe,
/// revision 2 never does.
const FAMILY: &str = "healthbreakerapp";

/// `Deployment$rolloutState` of a rollout still placing tasks.
const IN_PROGRESS: &str = "IN_PROGRESS";

/// `Deployment$rolloutState` of a rollout that reached the steady state it was rolling out.
const COMPLETED: &str = "COMPLETED";

/// `Deployment$rolloutState` of a rollout a monitor failed.
const FAILED: &str = "FAILED";

/// The `rolloutStateReason` a rollout the circuit breaker failed carries.
const FAILURE_REASON: &str = "ECS deployment circuit breaker: tasks failed to start.";

/// The `statusReason` the rollout's own record carries once the breaker ROLLED IT BACK, and
/// the `Rollback$reason` that rollback carries.
const ROLLED_BACK_REASON: &str =
    "Service deployment rolled back because the circuit breaker threshold was exceeded.";

/// The same for a rollout the breaker failed with no rollback configured.
const STOPPED_REASON: &str =
    "Service deployment stopped because the circuit breaker threshold was exceeded.";

/// The `HealthCheck$interval` every probe below runs at: the smallest the member documents
/// ("You may specify between 5 and 300 seconds"), so a verdict lands inside a poll budget
/// without naming a value AWS refuses.
const INTERVAL_SECONDS: i64 = 5;

/// `HealthCheck$retries` every probe below declares: "the number of times to retry a failed
/// health check before the container is considered unhealthy" — one, so a row spends one
/// execution per verdict.
const RETRIES: i64 = 1;

/// The `healthCheckGracePeriodSeconds` the grace-period row asks for: longer than this suite
/// observes it, so the stop that row's failing readings would otherwise earn is still being
/// ignored when the row is read.
const GRACE_PERIOD_SECONDS: i64 = 300;

/// How long the ONE loop that watches every row for the `FAILED` state keeps looking.
///
/// Deeper than this crate's shared budget, because the slowest row's threshold is reached
/// only after three health-check verdicts, each on a replacement task of its own, and this
/// suite runs beside every other test of the workspace — the deadline is only here so a
/// failure is a failure rather than a hang, and the loop stops the instant every row has been
/// seen.
const FAILURE_BUDGET: Duration = Duration::from_secs(150);

/// How many times an INVARIANT — something the rule says does not change — is looked at
/// before a case accepts it. Each look is a fresh read through the front door, which
/// materialises whatever the clock owes, so the invariant is checked against progress rather
/// than against a wait.
const LOOKS_FOR_AN_INVARIANT: usize = 5;

/// One container definition whose essential container stays up and runs `probe` as its
/// health check.
fn probing_container(probe: &str) -> Value {
    json!({
        "name": "app",
        "image": "public.ecr.aws/amazonlinux/amazonlinux:latest",
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

/// Register one revision of [`FAMILY`] whose container runs `probe`, and answer its ARN.
async fn register_revision(router: &MultiServiceRouter, probe: &str, case: &str) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "containerDefinitions": [probing_container(probe)],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Where the breaker leaves the service, for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The breaker failed the rollout and rolled the service back to the revision that
    /// completed, which the scheduler then runs again.
    RolledBack,
    /// The breaker failed the rollout and left the service on the revision that failed.
    Failed,
    /// Nothing fails this rollout, so it never leaves `IN_PROGRESS`.
    StillRolling,
}

/// One row: a service, how the breaker it was created with is configured, and what that
/// configuration owes it.
struct Row {
    case: &'static str,
    /// `Service$serviceName`, one per row.
    service: &'static str,
    /// `Service$desiredCount` the service is created with.
    desired: i64,
    /// The `deploymentCircuitBreaker` the create configures.
    breaker: Value,
    /// `CreateService$healthCheckGracePeriodSeconds`, for the row that asks the scheduler to
    /// ignore the failing readings.
    grace: Option<i64>,
    /// The failure threshold that breaker computes for this desired count, as
    /// `ThresholdConfiguration$type` documents it, and `0` for a breaker that is not on.
    threshold: i64,
    outcome: Outcome,
}

/// The rows, each its own service in the one cluster.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the default threshold over a two-task service, rollback on",
            service: "defaultbounded",
            desired: 2,
            // "By default, the threshold configuration uses a type of BOUNDED_PERCENT with a
            // value of 50", whose product with a desired count of 2 clamps up to 3.
            breaker: json!({ "enable": true, "rollback": true }),
            grace: None,
            threshold: 3,
            outcome: Outcome::RolledBack,
        },
        Row {
            case: "a count threshold reached by a replacement task, no rollback",
            service: "countnorollback",
            desired: 1,
            breaker: json!({
                "enable": true,
                "rollback": false,
                "thresholdConfiguration": { "type": "COUNT", "value": 2 },
            }),
            grace: None,
            threshold: 2,
            outcome: Outcome::Failed,
        },
        Row {
            case: "an unbounded percentage of the desired count, rollback on",
            service: "unboundedrollback",
            desired: 1,
            breaker: json!({
                "enable": true,
                "rollback": true,
                "thresholdConfiguration": { "type": "UNBOUNDED_PERCENT", "value": 100 },
            }),
            grace: None,
            threshold: 1,
            outcome: Outcome::RolledBack,
        },
        Row {
            case: "a breaker the request leaves turned off",
            service: "breakeroff",
            desired: 1,
            breaker: json!({ "enable": false, "rollback": true }),
            grace: None,
            threshold: 0,
            outcome: Outcome::StillRolling,
        },
        Row {
            case: "a grace period the scheduler still ignores the failing checks for",
            service: "gracedbreaker",
            desired: 1,
            breaker: json!({
                "enable": true,
                "rollback": true,
                "thresholdConfiguration": { "type": "COUNT", "value": 1 },
            }),
            grace: Some(GRACE_PERIOD_SECONDS),
            threshold: 1,
            outcome: Outcome::StillRolling,
        },
    ]
}

/// One service, as `DescribeServices` reports it now.
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

/// The deployment of `service_view` that is rolling out `task_definition`, whichever position
/// it is reported in — the entry a rollback supersedes as PRIMARY while leaving it listed.
fn deployment_of(service_view: &Value, task_definition: &str, case: &str) -> Option<Value> {
    list(service_view, "deployments", case)
        .into_iter()
        .find(|deployment| deployment["taskDefinition"].as_str() == Some(task_definition))
}

#[tokio::test]
async fn health_check_failures_fail_a_watched_rollout_and_decide_where_it_leaves_the_service() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's service runs in").await;
    let healthy = register_revision(&router, "exit 0", "the revision that passes its probe").await;
    let failing = register_revision(&router, "exit 1", "the revision that never does").await;

    let rows = rows();

    // Every service is created on the revision whose probe PASSES, so each has a deployment
    // that completed successfully — the one a rollback puts back.
    for row in &rows {
        let mut request = json!({
            "cluster": CLUSTER,
            "serviceName": row.service,
            "taskDefinition": healthy,
            "desiredCount": row.desired,
            "launchType": "EC2",
            "deploymentController": { "type": "ECS" },
            "deploymentConfiguration": { "deploymentCircuitBreaker": row.breaker.clone() },
        });
        if let Some(seconds) = row.grace {
            request["healthCheckGracePeriodSeconds"] = json!(seconds);
        }
        ok(&router, "CreateService", request, row.case).await;
    }

    // The steady state that first rollout reaches once its tasks run AND their probes have
    // passed, which is what makes it "the last deployment that completed successfully".
    let mut completed_revisions: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let desired = row.desired;
        let service_view = poll_until(
            case,
            "the first rollout to complete with every task running",
            || describe(&router, row.service, case),
            move |service_view| {
                string(&primary(service_view, "the first rollout"), "rolloutState", case)
                    == COMPLETED
                    && count(service_view, "runningCount", case) == desired
            },
        )
        .await;
        // `ServiceCurrentRevisionSummary$arn`: "The ARN of the current service revision" — the
        // revision a rollback of the next rollout has to put back.
        let revision = list(&service_view, "currentServiceRevisions", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a current service revision, got {service_view}")
            });
        completed_revisions.push(string(&revision, "arn", case));
    }

    // The rollout nothing can make healthy.
    let mut rollouts: Vec<String> = Vec::new();
    for row in &rows {
        let body = ok(
            &router,
            "UpdateService",
            json!({ "cluster": CLUSTER, "service": row.service, "taskDefinition": failing }),
            row.case,
        )
        .await;
        rollouts.push(string(&body["service"], "currentServiceDeployment", row.case));
    }

    // Every WATCHED row's rollout reaches the terminal `FAILED` state, which is the state a
    // client waiting on a deploy keys on. It is reported on the SERVICE until the rollback the
    // failure started replaces it, so all of those rows are watched in ONE loop — each look
    // re-reads every row not seen failed yet — rather than one row's wait after another's,
    // which would spend one row's rollback span waiting for the next row's failure.
    let watched: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.outcome != Outcome::StillRolling)
        .map(|(index, _)| index)
        .collect();
    let mut failed_rollouts: Vec<Option<Value>> = rows.iter().map(|_| None).collect();
    let deadline = Instant::now() + FAILURE_BUDGET;
    loop {
        let mut pending: Vec<&str> = Vec::new();
        for &index in &watched {
            if failed_rollouts[index].is_some() {
                continue;
            }
            let row = &rows[index];
            let service_view = describe(&router, row.service, row.case).await;
            match deployment_of(&service_view, &failing, row.case) {
                Some(rollout) if rollout["rolloutState"].as_str() == Some(FAILED) => {
                    failed_rollouts[index] = Some(rollout);
                }
                _ => pending.push(row.case),
            }
        }
        if pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected every watched rollout of the never-healthy revision to reach the \
             terminal FAILED state, still waiting on {pending:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Each of them reports the breaker as what failed it, over the tasks it lost.
    for &index in &watched {
        let row = &rows[index];
        let case = row.case;
        let threshold = row.threshold;
        let rollout = failed_rollouts[index]
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: expected the failed rollout to be seen"));
        assert_eq!(
            string(&rollout, "rolloutStateReason", case),
            FAILURE_REASON,
            "case {case:?}: expected the reason to name the circuit breaker, got {rollout}"
        );
        assert!(
            count(&rollout, "failedTasks", case) >= threshold,
            "case {case:?}: expected at least {threshold} failed tasks counted against the \
             rollout, got {rollout}"
        );

        match row.outcome {
            Outcome::RolledBack => {
                let desired = row.desired;
                let restored = healthy.clone();
                let service_view = poll_until(
                    case,
                    "the service to be rolled back to the revision that completed, running the \
                     task count it wants",
                    || describe(&router, row.service, case),
                    move |service_view| {
                        let deployment = primary(service_view, "the rollback");
                        string(service_view, "taskDefinition", case) == restored
                            && string(&deployment, "taskDefinition", case) == restored
                            && string(&deployment, "rolloutState", case) == COMPLETED
                            && count(service_view, "runningCount", case) == desired
                    },
                )
                .await;
                let deployment = primary(&service_view, case);
                assert_eq!(
                    count(&deployment, "runningCount", case),
                    row.desired,
                    "case {case:?}: expected the rolled-back deployment to converge on the \
                     desired count, got {deployment}"
                );
                // The rollout the breaker failed, as a resource of its own: ended rolled back,
                // onto the revision that completed.
                let record =
                    describe_service_deployment(&router, &rollouts[index], case).await;
                assert_eq!(
                    string(&record, "status", case),
                    "ROLLBACK_SUCCESSFUL",
                    "case {case:?}: expected the failed rollout to end rolled back, got {record}"
                );
                assert_eq!(
                    string(&record, "statusReason", case),
                    ROLLED_BACK_REASON,
                    "case {case:?}: expected the status reason to report the rollback the \
                     breaker made, got {record}"
                );
                assert_eq!(
                    string(&record["rollback"], "reason", case),
                    ROLLED_BACK_REASON,
                    "case {case:?}: expected the rollback to name the circuit breaker, got \
                     {record}"
                );
                assert_eq!(
                    string(&record["rollback"], "serviceRevisionArn", case),
                    completed_revisions[index],
                    "case {case:?}: expected the rollback to deploy the revision that \
                     completed, got {record}"
                );
                assert_eq!(
                    string(&record["deploymentCircuitBreaker"], "status", case),
                    "TRIGGERED",
                    "case {case:?}: expected the breaker to report that it fired, got {record}"
                );
                assert!(
                    count(&record["deploymentCircuitBreaker"], "failureCount", case) >= threshold,
                    "case {case:?}: expected at least {threshold} counted failures, got {record}"
                );
                assert_eq!(
                    count(&record["deploymentCircuitBreaker"], "threshold", case),
                    threshold,
                    "case {case:?}: expected the documented threshold, got {record}"
                );
            }
            Outcome::Failed => {
                // "If it is turned on, a service deployment will transition to a failed state
                // and stop launching new tasks": with no rollback configured the service stays
                // on the revision that failed, which is checked over repeated reads — each of
                // which materialises whatever the clock owes — so a rollback nobody asked for
                // cannot arrive a span later.
                for look in 0..LOOKS_FOR_AN_INVARIANT {
                    let service_view = describe(&router, row.service, case).await;
                    assert_eq!(
                        string(&service_view, "taskDefinition", case),
                        failing,
                        "case {case:?}: expected a service with no rollback to keep the \
                         revision that failed (look {look}), got {service_view}"
                    );
                    assert_eq!(
                        string(&primary(&service_view, case), "rolloutState", case),
                        FAILED,
                        "case {case:?}: expected the failed rollout to stay terminal (look \
                         {look}), got {service_view}"
                    );
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                let record = describe_service_deployment(&router, &rollouts[index], case).await;
                assert_eq!(
                    string(&record, "status", case),
                    "STOPPED",
                    "case {case:?}: expected the failed rollout to end stopped, got {record}"
                );
                assert_eq!(
                    string(&record, "statusReason", case),
                    STOPPED_REASON,
                    "case {case:?}: expected the status reason to name the circuit breaker, \
                     got {record}"
                );
                assert!(
                    record["rollback"].is_null(),
                    "case {case:?}: expected no rollback on a rollout nothing rolled back, got \
                     {record}"
                );
                assert_eq!(
                    string(&record["deploymentCircuitBreaker"], "status", case),
                    "TRIGGERED",
                    "case {case:?}: expected the breaker to report that it fired, got {record}"
                );
                assert_eq!(
                    count(&record["deploymentCircuitBreaker"], "threshold", case),
                    threshold,
                    "case {case:?}: expected the documented threshold, got {record}"
                );
            }
            Outcome::StillRolling => unreachable!("filtered out above"),
        }
    }

    // The rows nothing fails, read LAST — after every watched row above has reached its end,
    // so each has had at least as long as a breaker needs and still carries no terminal state.
    for (index, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.outcome == Outcome::StillRolling)
    {
        let case = row.case;
        // A service whose scheduler IS acting on the failing readings loses a task per verdict,
        // which the rollout counts whether or not a breaker is watching; the one whose grace
        // period is still running has lost none. Either way the rollout stays under way.
        let counts_failures = row.grace.is_none();
        if counts_failures {
            poll_until(
                case,
                "the rollout to count the tasks the scheduler stopped for failing their probes",
                || describe(&router, row.service, case),
                |service_view| {
                    deployment_of(service_view, &failing, case)
                        .is_some_and(|rollout| count(&rollout, "failedTasks", case) >= 1)
                },
            )
            .await;
        }
        for look in 0..LOOKS_FOR_AN_INVARIANT {
            let service_view = describe(&router, row.service, case).await;
            let rollout = deployment_of(&service_view, &failing, case).unwrap_or_else(|| {
                panic!("case {case:?}: expected the rollout to still be reported, got {service_view}")
            });
            assert_eq!(
                string(&rollout, "rolloutState", case),
                IN_PROGRESS,
                "case {case:?}: expected a rollout no breaker fails to stay rolling (look \
                 {look}), got {service_view}"
            );
            assert_eq!(
                string(&service_view, "taskDefinition", case),
                failing,
                "case {case:?}: expected the service to keep the revision it was moved onto \
                 (look {look}), got {service_view}"
            );
            if counts_failures {
                assert!(
                    count(&rollout, "failedTasks", case) >= 1,
                    "case {case:?}: expected the tasks the scheduler stopped to be counted \
                     (look {look}), got {rollout}"
                );
            } else {
                // "The period of time ... that the Amazon ECS service scheduler ignores
                // unhealthy ... container health checks after a task has first started": the
                // readings are there, and no task has been stopped for them, so the breaker
                // has nothing to count.
                assert_eq!(
                    count(&rollout, "failedTasks", case),
                    0,
                    "case {case:?}: expected no failed task while the grace period runs (look \
                     {look}), got {rollout}"
                );
                assert_eq!(
                    count(&rollout, "runningCount", case),
                    row.desired,
                    "case {case:?}: expected the unhealthy task to keep running inside the \
                     grace period (look {look}), got {rollout}"
                );
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        // And the rollout's own record says which it is: a breaker nothing turned on is
        // `DISABLED`, and one that is watching a rollout it has not failed is `MONITORING`.
        let record = describe_service_deployment(&router, &rollouts[index], case).await;
        assert_eq!(
            string(&record, "status", case),
            IN_PROGRESS,
            "case {case:?}: expected the rollout's own record to stay under way, got {record}"
        );
        let breaker = &record["deploymentCircuitBreaker"];
        let expected_status = if row.threshold == 0 {
            "DISABLED"
        } else {
            "MONITORING"
        };
        assert_eq!(
            string(breaker, "status", case),
            expected_status,
            "case {case:?}: expected the breaker to report {expected_status}, got {record}"
        );
        assert_eq!(
            count(breaker, "threshold", case),
            row.threshold,
            "case {case:?}: expected the documented threshold, got {record}"
        );
        if !counts_failures {
            assert_eq!(
                count(breaker, "failureCount", case),
                0,
                "case {case:?}: expected the watching breaker to have counted nothing, got \
                 {record}"
            );
        }
    }
}
