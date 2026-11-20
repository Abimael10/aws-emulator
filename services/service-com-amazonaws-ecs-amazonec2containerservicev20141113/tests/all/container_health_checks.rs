// SPDX-License-Identifier: CC0-1.0

//! The container HEALTH CHECK a task definition declares, driven end to end over the shared
//! wire harness in [`crate::ecs_wire`].
//!
//! The rules under test, over the whole class rather than one request:
//!
//! > A container whose definition declares a `healthCheck` has that command EXECUTED while
//! > the container runs, and `DescribeTasks` reports the result: a command that exits zero
//! > takes the container to `HEALTHY` ("An exit code of 0 indicates success"), one that
//! > exits non-zero takes it to `UNHEALTHY` once it has failed `retries` times
//! > (`HealthCheck$retries`: "the number of times to retry a failed health check before the
//! > container is considered unhealthy"), and a `startPeriod` only SUPPRESSES that until the
//! > period runs out ("the optional grace period to provide containers time to bootstrap
//! > before failed health checks count towards the maximum number of retries"). A container
//! > with no health check reports `UNKNOWN` for ever. The TASK's own `healthStatus` is the
//! > reading of its ESSENTIAL containers (`Task$healthStatus`), and a container whose
//! > `dependsOn` names a `HEALTHY` condition does not START until the container it names is
//! > healthy (`ContainerDependency$condition`: `HEALTHY` "validates that the dependent
//! > container passes its Docker health check before permitting other containers to start").
//! > `HealthCheck$interval` is "the time period in seconds between each health check
//! > execution", so it spaces the executions that FOLLOW one: a container is evaluated once
//! > it runs, whatever interval its check declares.
//!
//! The table spans what the rule generalises over: the FORM the command takes (`CMD-SHELL`
//! and `CMD`, the two forms `HealthCheck$command` documents), the outcome it produces
//! (passing, failing), how many retries it takes to be believed, the interval its executions
//! are spaced by (the smallest and the largest the member admits), whether a grace period is
//! declared, and whether a health check is declared at all. One recording of one passing
//! check is one row of it. The rows at the LARGEST interval are the ones that pin the last
//! clause of the rule: their verdict arrives because the first execution is not held off by
//! the spacing the member documents, so a row declaring 300 seconds reports it inside this
//! suite's poll budget rather than five minutes later.
//!
//! The tests at the END of this suite are what the SERVICE SCHEDULER makes of those same
//! readings, which is a rule of its own:
//!
//! > A task of a service whose own scheduler places its tasks is STOPPED and REPLACED once an
//! > essential container of it reports `UNHEALTHY` — the scheduler acts on container health
//! > checks, which is what `CreateService$healthCheckGracePeriodSeconds` names a period for
//! > ("the period of time ... that the Amazon ECS service scheduler ignores unhealthy Elastic
//! > Load Balancing, VPC Lattice, and container health checks after a task has first
//! > started") and what the `minimumHealthyPercent` doc describes from the other side ("the
//! > scheduler stops the unhealthy tasks one-by-one ... to clear up capacity to launch
//! > replacement tasks") — and the ROLLOUT that placed the task counts that stop as one of
//! > its failed tasks (`Deployment$failedTasks`: "A task is considered a failure if the
//! > service scheduler can't launch the task, the task doesn't transition to a `RUNNING`
//! > state, or if it fails any of its defined health checks and is stopped"), so a revision
//! > whose containers never pass their probe is a rollout the deployment circuit breaker
//! > fails and rolls back. Those failures ACCUMULATE: the breaker's count resets only when a
//! > task of the rollout reaches a healthy state (`DeploymentCircuitBreaker$resetOnHealthyTask`:
//! > "a task that reaches a healthy state resets the failure count to `0`", and
//! > `Deployment$failedTasks`: "once a service deployment has one or more successfully running
//! > tasks, the failed task count resets to zero"), so a replacement that is merely up, with
//! > its own check still being evaluated, resets nothing and the count climbs to whatever
//! > threshold the breaker computes. A STANDALONE task has no scheduler maintaining a count,
//! > so it is not stopped for an unhealthy container at all — which is what the table above
//! > pins.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through
//! `DescribeTasks`, so a status that is not computed from persisted state cannot pass.
//!
//! The three tests at the very END pin a third rule — WHAT the probe sees when it asks about
//! the container's FILESYSTEM — and are introduced where they stand.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, count, list, ok, service, string};

/// The cluster every task below runs in.
const CLUSTER: &str = "healthchecks";

/// The interval every row's check runs at: the smallest `HealthCheck$interval` the member
/// documents ("You may specify between 5 and 300 seconds"), so a row's verdict lands inside
/// a poll budget without naming a value AWS refuses.
const INTERVAL_SECONDS: i64 = 5;

/// The LONGEST `HealthCheck$interval` the member documents ("You may specify between 5 and
/// 300 seconds"), which the rows that run at it are there for: that member spaces the
/// executions that FOLLOW one, so a container whose check declares the longest interval AWS
/// admits still reports the verdict of its FIRST execution rather than reporting `UNKNOWN`
/// for the whole span.
const MAX_INTERVAL_SECONDS: i64 = 300;

/// The `HealthCheck$startPeriod` the grace-period case declares, in the same documented
/// range ("You can specify between 0 and 300 seconds") and FOUR intervals long, so a failing
/// execution lands inside it well before it expires.
const START_PERIOD_SECONDS: i64 = INTERVAL_SECONDS * 4;

/// How often a poll re-reads the task it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How long a poll keeps trying: enough interval spans for the deepest row's verdict —
/// the grace period, then `retries` failing executions, then the dependent container's own
/// first check — with generous margin for a loaded test machine, since what is under test is
/// WHICH status is reached and not how fast.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// How long the SERVICE-side tests at the end of this suite keep trying.
///
/// Deeper than the budget above, because a service's verdict is several of those spans per
/// TASK: the scheduler condemns a task only after `retries` failing executions one `interval`
/// apart, each replacement starts afresh, and the circuit breaker fires only once as many
/// tasks as its threshold tolerates have been lost. This suite also runs beside every other
/// test of the workspace, which can stretch each of those spans, so the deadline is generous
/// — it is only here so a failure is a failure rather than a hang, and every poll stops the
/// instant its own condition holds.
const SERVICE_POLL_BUDGET: Duration = Duration::from_secs(240);

/// How many times an INVARIANT — something the rule says does not change — is looked at
/// before the case accepts it. Each look is a fresh read through the front door, which
/// materialises whatever the clock owes, so the invariant is checked against progress rather
/// than against a wait.
const LOOKS_FOR_AN_INVARIANT: usize = 5;

/// The command a container runs so that it stays up for as long as a row observes it: a
/// container's own program is what keeps its task running, and a task whose essential
/// container has exited is stopped ("The task stopped because an essential container
/// exited").
fn stays_up() -> Value {
    json!(["sh", "-c", "sleep 300"])
}

/// A container definition named `name` that stays up, carrying `health_check` when one is
/// given.
fn container_with(name: &str, health_check: Value) -> Value {
    let mut container = json!({
        "name": name,
        "image": "public.ecr.aws/amazonlinux/amazonlinux:latest",
        "memory": 128,
        "command": stays_up(),
    });
    if !health_check.is_null() {
        container["healthCheck"] = health_check;
    }
    container
}

/// One `healthCheck` object: the command array, the interval, how many retries a failure
/// takes to be believed, and the grace period when the row declares one.
fn health_check(command: Value, retries: i64, start_period: Option<i64>) -> Value {
    health_check_every(command, INTERVAL_SECONDS, retries, start_period)
}

/// The same, for a row that names the `interval` its executions are spaced by.
fn health_check_every(
    command: Value,
    interval: i64,
    retries: i64,
    start_period: Option<i64>,
) -> Value {
    let mut check = json!({
        "command": command,
        "interval": interval,
        "timeout": 2,
        "retries": retries,
    });
    if let Some(seconds) = start_period {
        check["startPeriod"] = json!(seconds);
    }
    check
}

/// Register one revision of `family` with `containers` and run one task of it, answering the
/// task's ARN.
async fn run_one(
    router: &MultiServiceRouter,
    family: &str,
    containers: Value,
    case: &str,
) -> String {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": containers }),
        case,
    )
    .await;
    let run = ok(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family, "count": 1 }),
        case,
    )
    .await;
    let tasks = list(&run, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: RunTask must place the one task it was asked for, got {run}"
    );
    string(&tasks[0], "taskArn", case)
}

/// The task as `DescribeTasks` reports it now.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let described = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    let tasks = list(&described, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: the task must be describable, got {described}"
    );
    tasks[0].clone()
}

/// The `healthStatus` the container called `name` reports in `task`.
fn container_health(task: &Value, name: &str, case: &str) -> String {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(name))
        .map(|container| {
            container["healthStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no container {name:?} in {task}"))
}

/// The `lastStatus` the container called `name` reports in `task`.
fn container_status(task: &Value, name: &str, case: &str) -> String {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(name))
        .map(|container| {
            container["lastStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no container {name:?} in {task}"))
}

/// Wait until `reached` holds of the described task, answering the task as it was then — and
/// failing with the last reading when the budget runs out.
async fn poll_until(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
    wanted: &str,
    reached: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, task_arn, case).await;
        if reached(&task) || Instant::now() >= deadline {
            assert!(
                reached(&task),
                "case {case:?}: expected {wanted}, got {task}"
            );
            return task;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the health-check table.
struct Row {
    case: &'static str,
    /// The task-definition family, and so the container the row's one container carries.
    family: &'static str,
    /// The `healthCheck` the container declares, or `Value::Null` for a container with none.
    check: Value,
    /// The `healthStatus` the container — and so the task, whose only container is
    /// essential — must reach.
    expected: &'static str,
}

/// The rows: every form `HealthCheck$command` documents, each with a passing and a failing
/// outcome, plus the container that declares no check at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a CMD-SHELL probe that exits zero",
            family: "healthshellpass",
            check: health_check(json!(["CMD-SHELL", "exit 0"]), 1, None),
            expected: "HEALTHY",
        },
        Row {
            case: "a CMD-SHELL probe that exits non-zero, believed after one try",
            family: "healthshellfail",
            check: health_check(json!(["CMD-SHELL", "exit 1"]), 1, None),
            expected: "UNHEALTHY",
        },
        Row {
            case: "a CMD probe that exits zero",
            family: "healthexecpass",
            check: health_check(json!(["CMD", "true"]), 2, None),
            expected: "HEALTHY",
        },
        Row {
            case: "a CMD probe that exits non-zero, believed after two tries",
            family: "healthexecfail",
            check: health_check(json!(["CMD", "false"]), 2, None),
            expected: "UNHEALTHY",
        },
        Row {
            case: "a probe whose command this container cannot run at all",
            family: "healthnocommand",
            check: health_check(json!(["CMD-SHELL", "/usr/bin/nothing-here"]), 1, None),
            expected: "UNHEALTHY",
        },
        Row {
            case: "a passing probe whose interval is the longest the member allows",
            family: "healthlongpass",
            check: health_check_every(json!(["CMD-SHELL", "exit 0"]), MAX_INTERVAL_SECONDS, 1, None),
            expected: "HEALTHY",
        },
        Row {
            case: "a failing probe believed on its first execution, at that same interval",
            family: "healthlongfail",
            check: health_check_every(json!(["CMD-SHELL", "exit 1"]), MAX_INTERVAL_SECONDS, 1, None),
            expected: "UNHEALTHY",
        },
        Row {
            case: "a container that declares no health check",
            family: "healthnone",
            check: Value::Null,
            expected: "UNKNOWN",
        },
    ]
}

#[tokio::test]
async fn a_declared_health_check_runs_and_decides_the_container_and_task_health_status() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's task runs in").await;

    // Every row's task is placed up front, so the executions of all of them fall due at
    // once rather than one row's budget after another's.
    let rows = rows();
    let mut placed: Vec<String> = Vec::new();
    for row in &rows {
        let containers = json!([container_with("app", row.check.clone())]);
        placed.push(run_one(&router, row.family, containers, row.case).await);
    }

    for (row, task_arn) in rows.iter().zip(&placed) {
        let case = row.case;
        let expected = row.expected;
        // A container with no check never leaves UNKNOWN, so that row is read once the task
        // is running rather than waited on: there is no transition to wait for.
        let task = poll_until(
            &router,
            task_arn,
            case,
            "the declared check to decide the container's health",
            |task| {
                task["lastStatus"].as_str() == Some("RUNNING")
                    && container_health(task, "app", case) == expected
            },
        )
        .await;
        assert_eq!(
            container_health(&task, "app", case),
            expected,
            "case {case:?}: expected the container to report {expected}, got {task}"
        );
        // "If all essential containers in the task are reporting as HEALTHY, the task status
        // also reports as HEALTHY": this task's one container is essential by default.
        assert_eq!(
            task["healthStatus"].as_str(),
            Some(expected),
            "case {case:?}: the task reports the reading of its essential container, got \
             {task}"
        );
        // The probe reports on the container; it does not stop the task it runs in.
        assert_eq!(
            task["lastStatus"].as_str(),
            Some("RUNNING"),
            "case {case:?}: a health check does not move the task's own status, got {task}"
        );
    }
}

#[tokio::test]
async fn a_start_period_suppresses_unhealthy_only_until_it_expires() {
    let router = service();
    let case = "a failing probe under a grace period";
    cluster_with_capacity(&router, CLUSTER, case).await;

    // The SUBJECT declares a grace period several intervals long; the TWIN beside it runs
    // the SAME failing probe with the same single retry and NO grace period. The twin is
    // what makes the suppression observable without timing anything: the instant the twin
    // reports UNHEALTHY, a failing execution of that very check has been counted, so a
    // subject that is still "being evaluated" is one whose failures were suppressed — and an
    // implementation that ignored `startPeriod` would have flipped both at once.
    let subject = json!([container_with(
        "app",
        health_check(json!(["CMD-SHELL", "exit 1"]), 1, Some(START_PERIOD_SECONDS)),
    )]);
    let twin = json!([container_with(
        "app",
        health_check(json!(["CMD-SHELL", "exit 1"]), 1, None),
    )]);
    let subject_arn = run_one(&router, "healthgrace", subject, case).await;
    let twin_arn = run_one(&router, "healthnograce", twin, case).await;

    // 1. The twin's first failing execution counts at once, so it reaches UNHEALTHY.
    poll_until(
        &router,
        &twin_arn,
        "the twin with no grace period",
        "the failing probe to be counted at once without a grace period",
        |task| container_health(task, "app", "the twin with no grace period") == "UNHEALTHY",
    )
    .await;

    // 2. With the twin already unhealthy, the subject is still being evaluated: its own
    //    failures fall inside the period it asked for and count toward nothing. Asserted
    //    over repeated reads, each of which materialises whatever the clock owes.
    for look in 0..LOOKS_FOR_AN_INVARIANT {
        let task = describe(&router, &subject_arn, case).await;
        assert_eq!(
            container_health(&task, "app", case),
            "UNKNOWN",
            "case {case:?}: a failure inside the grace period counts toward nothing, so the \
             container is still being evaluated (look {look}), got {task}"
        );
        assert_eq!(
            task["healthStatus"].as_str(),
            Some("UNKNOWN"),
            "case {case:?}: the task reports what its essential container reports, got {task}"
        );
    }

    // 3. And the suppression ENDS with the period: once it has run out the failures count
    //    and the subject reaches UNHEALTHY too, with no further call from anybody.
    poll_until(
        &router,
        &subject_arn,
        case,
        "the grace period to expire and let the failures count",
        |task| container_health(task, "app", case) == "UNHEALTHY",
    )
    .await;
}

#[tokio::test]
async fn a_container_that_depends_on_a_healthy_container_waits_for_the_check_to_pass() {
    let router = service();
    let case = "a dependsOn HEALTHY gate";
    cluster_with_capacity(&router, CLUSTER, case).await;

    // `db` passes its check only after its own grace period, so the gate is observable: the
    // `app` container may not start until `db` is HEALTHY, and `other`, which depends on
    // nothing, starts with the task.
    let mut db = container_with("db", health_check(json!(["CMD-SHELL", "exit 0"]), 1, None));
    db["essential"] = json!(true);
    let mut app = container_with("app", Value::Null);
    app["dependsOn"] = json!([{ "containerName": "db", "condition": "HEALTHY" }]);
    let other = container_with("other", Value::Null);
    let task_arn = run_one(
        &router,
        "healthdependson",
        json!([db, app, other]),
        case,
    )
    .await;

    // Before the first execution of db's check there is nothing to open the gate, so app is
    // still PENDING while the task itself is RUNNING and the independent container is too.
    let running = poll_until(&router, &task_arn, case, "the task to be running", |task| {
        task["lastStatus"].as_str() == Some("RUNNING")
    })
    .await;
    assert_eq!(
        container_status(&running, "app", case),
        "PENDING",
        "case {case:?}: a container that waits on a HEALTHY condition does not start with \
         its task, got {running}"
    );
    assert_eq!(
        container_status(&running, "other", case),
        "RUNNING",
        "case {case:?}: a container that waits on nothing starts with its task, got {running}"
    );
    // The task's own reading is UNKNOWN while an essential container has produced no result.
    assert_eq!(
        running["healthStatus"].as_str(),
        Some("UNKNOWN"),
        "case {case:?}: a task whose essential container is still being evaluated reports \
         UNKNOWN, got {running}"
    );

    // Once db's check passes, the gate opens and app starts.
    let started = poll_until(
        &router,
        &task_arn,
        case,
        "the dependent container to start once its dependency is healthy",
        |task| container_status(task, "app", case) == "RUNNING",
    )
    .await;
    assert_eq!(
        container_health(&started, "db", case),
        "HEALTHY",
        "case {case:?}: the gate opens on the dependency's own HEALTHY reading, got {started}"
    );
}

/// The `stopCode` a stop the SERVICE SCHEDULER made carries, from the `TaskStopCode` enum
/// (`smithy_query(query="shape", shape="TaskStopCode")` -> TaskFailedToStart,
/// EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption,
/// TerminationNotice, InfrastructureHealth). Spelled out here rather than imported, so a
/// wrong constant in the implementation cannot make a test pass.
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";

/// The `stoppedReason` of a task the scheduler stopped because its health check failed.
const HEALTH_CHECK_FAILED_REASON: &str = "Task failed container health checks";

/// The failure threshold the breaker in the rollback test is configured with, as a
/// `ThresholdConfiguration` of type `COUNT`, which "uses `value` directly as the threshold":
/// ONE lost task, so the test spends one health-check verdict rather than one per task the
/// default formula would need.
const THRESHOLD: i64 = 1;

/// Register one revision of `family` with `containers` and answer its ARN.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    containers: Value,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": containers }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Create one EC2-launch-type service of ONE task of `definition`, whose deployment
/// configuration carries `breaker` when the case asks for a circuit breaker. Answers the
/// service as the create reported it.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    breaker: Value,
    case: &str,
) -> Value {
    let mut configuration = json!({});
    if !breaker.is_null() {
        configuration["deploymentCircuitBreaker"] = breaker;
    }
    let body = ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
            "deploymentController": { "type": "ECS" },
            "deploymentConfiguration": configuration,
        }),
        case,
    )
    .await;
    body["service"].clone()
}

/// The ARNs `ListTasks` reports for the service `name` under `desired_status`.
async fn service_task_arns(
    router: &MultiServiceRouter,
    name: &str,
    desired_status: &str,
    case: &str,
) -> Vec<String> {
    let body = ok(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "desiredStatus": desired_status,
        }),
        case,
    )
    .await;
    let mut arns: Vec<String> = list(&body, "taskArns", case)
        .into_iter()
        .map(|arn| {
            arn.as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a task ARN, got {arn}"))
                .to_string()
        })
        .collect();
    arns.sort();
    arns
}

/// Wait until the service `name` holds exactly one task it still wants running, and answer
/// that task's ARN.
async fn await_one_running_task(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let running = service_task_arns(router, name, "RUNNING", case).await;
        if let [only] = running.as_slice() {
            let task = describe(router, only, case).await;
            if task["lastStatus"].as_str() == Some("RUNNING") {
                return only.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service to place one running task, got {running:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The id segment of a task ARN — what a service event names a task by.
fn task_id(task_arn: &str) -> &str {
    task_arn.rsplit('/').next().unwrap_or(task_arn)
}

/// The service as `DescribeServices` reports it now.
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {body}"))
}

/// The service's current (`PRIMARY`) deployment, which is the first one it reports.
fn primary_deployment(service_view: &Value, case: &str) -> Value {
    list(service_view, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {service_view}"))
}

/// One row of the SERVICE table: the containers a service's revision declares, and whether
/// the scheduler has to replace the task it placed.
struct ServiceRow {
    case: &'static str,
    /// The task-definition family AND the service name this row uses.
    name: &'static str,
    /// The container definitions its revision declares.
    containers: Value,
    /// Whether the task the service placed must be stopped and replaced.
    replaced: bool,
}

/// The rows, spanning what the rule generalises over: which reading the essential container
/// produces, how many retries that reading took, whether the failing container is essential
/// at all, and whether a check is declared.
fn service_rows() -> Vec<ServiceRow> {
    let mut essential = container_with("app", Value::Null);
    essential["essential"] = json!(true);
    let mut sidecar = container_with("side", health_check(json!(["CMD-SHELL", "exit 1"]), 1, None));
    sidecar["essential"] = json!(false);
    vec![
        ServiceRow {
            case: "an essential container whose probe fails after one retry",
            name: "svcfailone",
            containers: json!([container_with(
                "app",
                health_check(json!(["CMD-SHELL", "exit 1"]), 1, None),
            )]),
            replaced: true,
        },
        ServiceRow {
            case: "an essential container whose probe fails after two retries",
            name: "svcfailtwo",
            containers: json!([container_with(
                "app",
                health_check(json!(["CMD", "false"]), 2, None),
            )]),
            replaced: true,
        },
        ServiceRow {
            case: "an essential container whose probe passes",
            name: "svcpass",
            containers: json!([container_with(
                "app",
                health_check(json!(["CMD-SHELL", "exit 0"]), 1, None),
            )]),
            replaced: false,
        },
        ServiceRow {
            case: "a NON-essential container whose probe fails",
            name: "svcsidecar",
            containers: json!([essential, sidecar]),
            replaced: false,
        },
        ServiceRow {
            case: "a container that declares no health check at all",
            name: "svcnocheck",
            containers: json!([container_with("app", Value::Null)]),
            replaced: false,
        },
    ]
}

#[tokio::test]
async fn a_service_task_whose_essential_container_fails_its_check_is_stopped_and_replaced() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's service runs in").await;

    // Every row's service is created up front, so the executions of all of them fall due at
    // once rather than one row's budget after another's.
    let rows = service_rows();
    for row in &rows {
        let definition =
            register_revision(&router, row.name, row.containers.clone(), row.case).await;
        // No circuit breaker: what these rows are about is the REPLACEMENT the scheduler
        // makes, which a breaker would eventually cut short.
        create_service(&router, row.name, &definition, Value::Null, row.case).await;
    }
    let mut placed = Vec::new();
    for row in &rows {
        placed.push(await_one_running_task(&router, row.name, row.case).await);
    }

    // The rows whose ESSENTIAL container fails its check: that task is stopped — with the
    // stop code the scheduler's own stops carry and the sentence Amazon ECS words this one in
    // — and the service places another in its place.
    for (row, first) in rows.iter().zip(&placed).filter(|(row, _)| row.replaced) {
        let case = row.case;
        let deadline = Instant::now() + SERVICE_POLL_BUDGET;
        let replacement = loop {
            let stopped = service_task_arns(&router, row.name, "STOPPED", case).await;
            let running = service_task_arns(&router, row.name, "RUNNING", case).await;
            let replacement = running.iter().find(|arn| *arn != first).cloned();
            if stopped.contains(first) {
                if let Some(replacement) = replacement {
                    break replacement;
                }
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the unhealthy task {first} to be stopped and \
                 replaced, got stopped {stopped:?} and running {running:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_ne!(
            &replacement, first,
            "case {case:?}: expected the replacement to be a new task, got {replacement}"
        );
        let stopped = describe(&router, first, case).await;
        assert_eq!(
            stopped["stopCode"].as_str(),
            Some(SERVICE_SCHEDULER_INITIATED),
            "case {case:?}: expected the stop to be the service scheduler's, got {stopped}"
        );
        assert_eq!(
            stopped["stoppedReason"].as_str(),
            Some(HEALTH_CHECK_FAILED_REASON),
            "case {case:?}: expected the reason Amazon ECS words a failed health check in, \
             got {stopped}"
        );
        // The scheduler says so on the service's own event stream, naming the task it
        // stopped.
        let service_view = describe_service(&router, row.name, case).await;
        let id = task_id(first);
        let events = list(&service_view, "events", case);
        assert!(
            events.iter().any(|event| {
                let message = event["message"].as_str().unwrap_or_default();
                message.contains(id) && message.contains("failed container health checks")
            }),
            "case {case:?}: expected an event naming the task {id} that failed its checks, \
             got {service_view}"
        );
    }

    // And the rows where no essential container failed its check keep the very task they
    // placed. Read AFTER every replacement above, so each of these services has had at least
    // as long as a replacement takes — and over repeated reads, each of which materialises
    // whatever the clock owes.
    for (row, first) in rows.iter().zip(&placed).filter(|(row, _)| !row.replaced) {
        let case = row.case;
        for look in 0..LOOKS_FOR_AN_INVARIANT {
            let running = service_task_arns(&router, row.name, "RUNNING", case).await;
            let stopped = service_task_arns(&router, row.name, "STOPPED", case).await;
            assert_eq!(
                running.as_slice(),
                ::std::slice::from_ref(first),
                "case {case:?}: expected the task the service placed to keep running (look \
                 {look}), got {running:?}"
            );
            assert!(
                stopped.is_empty(),
                "case {case:?}: expected no task of this service to be stopped (look \
                 {look}), got {stopped:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

#[tokio::test]
async fn a_rollout_whose_container_never_passes_its_probe_is_failed_and_rolled_back() {
    let router = service();
    let case = "a revision whose container never passes its probe";
    cluster_with_capacity(&router, CLUSTER, case).await;
    let family = "breakerhealth";
    let name = "breakerhealth";

    // Revision 1 passes its check; revision 2 runs but never passes.
    let healthy = register_revision(
        &router,
        family,
        json!([container_with(
            "app",
            health_check(json!(["CMD-SHELL", "exit 0"]), 1, None),
        )]),
        case,
    )
    .await;
    create_service(
        &router,
        name,
        &healthy,
        // `ThresholdConfiguration$type` COUNT "uses `value` directly as the threshold", so ONE
        // lost task fires this breaker: what is under test here is the FAILURE a health
        // check produces and where it leaves the service, not how the threshold is computed
        // — the formula, including the default that clamps "to a minimum of 3", is the
        // table in `crate::deployment_circuit_breaker`.
        json!({
            "enable": true,
            "rollback": true,
            "thresholdConfiguration": { "type": "COUNT", "value": THRESHOLD },
        }),
        case,
    )
    .await;

    // The first rollout completes once the task is running AND its check has passed: a
    // rollout whose containers are still being evaluated has reached no steady state.
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let deployment = primary_deployment(&service_view, case);
        if string(&deployment, "rolloutState", case) == "COMPLETED"
            && count(&service_view, "runningCount", case) == 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the first rollout to complete with its task running, \
             got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    let first_task = await_one_running_task(&router, name, case).await;
    let described = describe(&router, &first_task, case).await;
    assert_eq!(
        container_health(&described, "app", case),
        "HEALTHY",
        "case {case:?}: expected the completed rollout's container to report HEALTHY, got \
         {described}"
    );

    // The rollout nothing can make healthy.
    let failing = register_revision(
        &router,
        family,
        json!([container_with(
            "app",
            health_check(json!(["CMD-SHELL", "exit 1"]), 1, None),
        )]),
        case,
    )
    .await;
    let updated = ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "taskDefinition": failing }),
        case,
    )
    .await;
    let rollout = string(&updated["service"], "currentServiceDeployment", case);

    // The breaker takes that rollout to the TERMINAL failed state, with the reason that
    // names the breaker: "If the service fails to reach a steady state and circuit breaker
    // is turned on, the deployment transitions to a FAILED state."
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let failed = list(&service_view, "deployments", case).into_iter().find(
            |deployment| {
                deployment["taskDefinition"].as_str() == Some(failing.as_str())
                    && deployment["rolloutState"].as_str() == Some("FAILED")
            },
        );
        if let Some(failed) = failed {
            assert_eq!(
                string(&failed, "rolloutStateReason", case),
                "ECS deployment circuit breaker: tasks failed to start.",
                "case {case:?}: expected the reason to name the circuit breaker, got {failed}"
            );
            assert!(
                count(&failed, "failedTasks", case) >= THRESHOLD,
                "case {case:?}: expected at least the threshold's worth of failed tasks \
                 counted, got {failed}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rollout of the unhealthy revision to be failed by \
             the breaker, got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    // ... and rolls the service back to the revision that completed, which runs again and
    // passes its own check.
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let deployment = primary_deployment(&service_view, case);
        if string(&service_view, "taskDefinition", case) == healthy
            && string(&deployment, "rolloutState", case) == "COMPLETED"
            && count(&service_view, "runningCount", case) == 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service to be rolled back to the revision that \
             completed, got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    let restored = await_one_running_task(&router, name, case).await;
    let described = describe(&router, &restored, case).await;
    assert_eq!(
        container_health(&described, "app", case),
        "HEALTHY",
        "case {case:?}: expected the rolled-back revision's container to serve again, got \
         {described}"
    );

    // The failed rollout, as a resource of its own: ended rolled back, by the breaker.
    let body = ok(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [rollout] }),
        case,
    )
    .await;
    let record = list(&body, "serviceDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout back, got {body}"));
    assert_eq!(
        string(&record, "status", case),
        "ROLLBACK_SUCCESSFUL",
        "case {case:?}: expected the failed rollout to end rolled back, got {record}"
    );
    assert_eq!(
        string(&record["deploymentCircuitBreaker"], "status", case),
        "TRIGGERED",
        "case {case:?}: expected the breaker to report that it fired, got {record}"
    );
    assert!(
        count(&record["deploymentCircuitBreaker"], "failureCount", case) >= THRESHOLD,
        "case {case:?}: expected the breaker to have counted its threshold's worth of \
         failures, got {record}"
    );
}

/// The failure threshold the DEFAULT breaker computes for a service of one task: the
/// `ThresholdConfiguration` default is "a type of BOUNDED_PERCENT with a value of 50", and
/// that type "calculates the failure threshold by multiplying `value` by the latest service
/// desired count, then clamping the result to a minimum of `3` and a maximum of `200`" — so
/// a one-task service takes THREE consecutively failed tasks, one per health-check verdict.
const DEFAULT_THRESHOLD: i64 = 3;

#[tokio::test]
async fn consecutive_failed_health_checks_accumulate_until_the_default_threshold_is_reached() {
    let router = service();
    let case = "a never-healthy revision under the default breaker threshold";
    cluster_with_capacity(&router, CLUSTER, case).await;
    let family = "breakerdefault";
    let name = "breakerdefault";

    let healthy = register_revision(
        &router,
        family,
        json!([container_with(
            "app",
            health_check(json!(["CMD-SHELL", "exit 0"]), 1, None),
        )]),
        case,
    )
    .await;
    create_service(
        &router,
        name,
        &healthy,
        // The breaker the recorded client configures: enabled, rolling back, and with NO
        // threshold configuration, so the threshold is the documented default formula's.
        json!({ "enable": true, "rollback": true }),
        case,
    )
    .await;

    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let deployment = primary_deployment(&service_view, case);
        if string(&deployment, "rolloutState", case) == "COMPLETED"
            && count(&service_view, "runningCount", case) == 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the first rollout to complete with its task running, \
             got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    let failing = register_revision(
        &router,
        family,
        json!([container_with(
            "app",
            health_check(json!(["CMD-SHELL", "exit 1"]), 1, None),
        )]),
        case,
    )
    .await;
    let updated = ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "taskDefinition": failing }),
        case,
    )
    .await;
    let rollout_arn = string(&updated["service"], "currentServiceDeployment", case);

    // Every task of this rollout fails its check, so NONE of them reaches a healthy state
    // and nothing resets the count: the failures the breaker has counted only ever climb,
    // up to the threshold that fails the rollout.
    //
    // The rollout's OWN record is what this waits on rather than the `deployments[]` entry:
    // that entry is superseded by the rollback the breaker starts, while the record keeps
    // what the breaker counted and where the failure left the rollout. The entry is still
    // read on every look, for the count that must never go DOWN while it is there.
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    let mut counted = 0;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let rollout = list(&service_view, "deployments", case)
            .into_iter()
            .find(|deployment| deployment["taskDefinition"].as_str() == Some(failing.as_str()));
        if let Some(rollout) = rollout {
            let failed_tasks = count(&rollout, "failedTasks", case);
            assert!(
                failed_tasks >= counted,
                "case {case:?}: expected the failures counted against a rollout no task of \
                 which ever became healthy never to go down, had {counted} and got {rollout}"
            );
            counted = failed_tasks;
            if rollout["rolloutState"].as_str() == Some("FAILED") {
                assert_eq!(
                    string(&rollout, "rolloutStateReason", case),
                    "ECS deployment circuit breaker: tasks failed to start.",
                    "case {case:?}: expected the reason to name the circuit breaker, got \
                     {rollout}"
                );
            }
        }
        let record = describe_rollout(&router, &rollout_arn, case).await;
        if record["deploymentCircuitBreaker"]["status"].as_str() == Some("TRIGGERED") {
            assert!(
                count(&record["deploymentCircuitBreaker"], "failureCount", case)
                    >= DEFAULT_THRESHOLD,
                "case {case:?}: expected at least the default threshold's worth of failures \
                 counted against the rollout the breaker fired on, got {record}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rollout of the never-healthy revision to be failed \
             by the breaker, counted {counted} failures and got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    // ... and the service is taken back to the revision that completed.
    let deadline = Instant::now() + SERVICE_POLL_BUDGET;
    loop {
        let service_view = describe_service(&router, name, case).await;
        let deployment = primary_deployment(&service_view, case);
        if string(&service_view, "taskDefinition", case) == healthy
            && string(&deployment, "rolloutState", case) == "COMPLETED"
            && count(&service_view, "runningCount", case) == 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service to be rolled back to the revision that \
             completed, got {service_view}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    // The rollout the breaker fired on ends ROLLED BACK, by the breaker: where the failure
    // left it, read off the resource that keeps it.
    let record = describe_rollout(&router, &rollout_arn, case).await;
    assert_eq!(
        string(&record, "status", case),
        "ROLLBACK_SUCCESSFUL",
        "case {case:?}: expected the failed rollout to end rolled back, got {record}"
    );
}

/// One service deployment as `DescribeServiceDeployments` reports it now.
async fn describe_rollout(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [arn] }),
        case,
    )
    .await;
    list(&body, "serviceDeployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the rollout back, got {body}"))
}

// ---------------------------------------------------------------------------------------
// What a probe sees of the container's FILESYSTEM.
//
// The rule the three tests below pin, which is a rule of its own:
//
// > A health check is "the command that the container runs to determine if it is healthy"
// > (`HealthCheck$command`), so the probe runs INSIDE the container and reads and writes the
// > CONTAINER's own filesystem — the writable layer the container's `entryPoint`/`command`
// > runs over. A path that program created is a path the probe finds, one it never wrote is
// > one the probe does not find, one it removed is gone for the probe too, and what one
// > execution of the probe leaves behind is there for the next one — while another
// > container's layer is another filesystem, which this container's probe cannot see.
//
// The rows differ along what that rule generalises over: WHAT the probe asks about the
// filesystem (whether a path exists, what a file it reads contains), WHICH form the command
// is written in (`CMD-SHELL`, `CMD`), WHERE the path is (the container's own layer, a
// directory its program made, a path named relative to `workingDirectory`), and WHO wrote
// what the probe finds — the program, an earlier execution of the probe, or a container
// beside it, which is the case the rule excludes.
// ---------------------------------------------------------------------------------------

/// The program a row's container runs: what that row writes into its own filesystem, and
/// then the wait that keeps the container up for as long as the row observes it.
fn writes_then_stays_up(writes: &str) -> Value {
    json!(["sh", "-c", format!("{writes}sleep 300")])
}

/// A container definition named `name` running `command` under `health_check`, carrying
/// `workingDirectory` when the row names one.
fn probing_container(
    name: &str,
    command: Value,
    check: Value,
    working_directory: Option<&str>,
) -> Value {
    let mut container = json!({
        "name": name,
        "image": "public.ecr.aws/amazonlinux/amazonlinux:latest",
        "memory": 128,
        "command": command,
        "healthCheck": check,
    });
    if let Some(directory) = working_directory {
        container["workingDirectory"] = json!(directory);
    }
    container
}

/// One row of the filesystem table.
struct FilesystemRow {
    case: &'static str,
    /// The task-definition family, so every row carries its own revision.
    family: &'static str,
    /// What the container's own program writes before it settles.
    writes: &'static str,
    /// `ContainerDefinition$workingDirectory`, for the row whose probe names a RELATIVE path.
    working_directory: Option<&'static str>,
    /// The `HealthCheck$command` array.
    command: Value,
    /// The `healthStatus` the container must reach.
    expected: &'static str,
}

/// The rows: every kind of question a probe asks about its container's filesystem, in both
/// forms `HealthCheck$command` documents, each coming to the reading its answer earns.
fn filesystem_rows() -> Vec<FilesystemRow> {
    vec![
        FilesystemRow {
            case: "a probe testing a path the program created",
            family: "probefilecreated",
            writes: "touch /tmp/ready; ",
            working_directory: None,
            command: json!(["CMD-SHELL", "test -f /tmp/ready"]),
            expected: "HEALTHY",
        },
        FilesystemRow {
            case: "a probe testing a path nothing wrote",
            family: "probefileabsent",
            writes: "touch /tmp/ready; ",
            working_directory: None,
            command: json!(["CMD-SHELL", "test -f /tmp/other"]),
            expected: "UNHEALTHY",
        },
        FilesystemRow {
            case: "a probe testing a path the program removed again",
            family: "probefileremoved",
            writes: "touch /tmp/ready; rm /tmp/ready; ",
            working_directory: None,
            command: json!(["CMD-SHELL", "test -f /tmp/ready"]),
            expected: "UNHEALTHY",
        },
        FilesystemRow {
            case: "a probe reading the contents the program wrote",
            family: "probefilecontents",
            writes: "echo ready > /tmp/state; ",
            working_directory: None,
            command: json!(["CMD-SHELL", "test \"$(cat /tmp/state)\" = ready"]),
            expected: "HEALTHY",
        },
        FilesystemRow {
            case: "a probe reading contents it does not accept",
            family: "probefilecontentsbad",
            writes: "echo down > /tmp/state; ",
            working_directory: None,
            command: json!(["CMD-SHELL", "test \"$(cat /tmp/state)\" = ready"]),
            expected: "UNHEALTHY",
        },
        FilesystemRow {
            case: "a CMD-form probe over a directory the program made",
            family: "probecmdform",
            writes: "mkdir -p /var/run/app; touch /var/run/app/ready; ",
            working_directory: None,
            command: json!(["CMD", "test", "-f", "/var/run/app/ready"]),
            expected: "HEALTHY",
        },
        FilesystemRow {
            case: "a probe naming the file relative to the working directory",
            family: "proberelativepath",
            writes: "touch ready; ",
            working_directory: Some("/srv/app"),
            command: json!(["CMD-SHELL", "test -f ready"]),
            expected: "HEALTHY",
        },
    ]
}

#[tokio::test]
async fn a_probe_reads_the_filesystem_the_container_s_own_program_wrote() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "the cluster every row's task runs in").await;

    // Every row's task is placed up front, so the executions of all of them fall due at once
    // rather than one row's budget after another's.
    let rows = filesystem_rows();
    let mut placed: Vec<String> = Vec::new();
    for row in &rows {
        let containers = json!([probing_container(
            "app",
            writes_then_stays_up(row.writes),
            health_check(row.command.clone(), 1, None),
            row.working_directory,
        )]);
        placed.push(run_one(&router, row.family, containers, row.case).await);
    }

    for (row, task_arn) in rows.iter().zip(&placed) {
        let case = row.case;
        let expected = row.expected;
        let task = poll_until(
            &router,
            task_arn,
            case,
            "the probe's question about the container's filesystem to be answered",
            |task| {
                task["lastStatus"].as_str() == Some("RUNNING")
                    && container_health(task, "app", case) == expected
            },
        )
        .await;
        assert_eq!(
            container_health(&task, "app", case),
            expected,
            "case {case:?}: expected the container to report {expected}, got {task}"
        );
        // "If all essential containers in the task are reporting as HEALTHY, the task status
        // also reports as HEALTHY": this task's one container is essential by default.
        assert_eq!(
            task["healthStatus"].as_str(),
            Some(expected),
            "case {case:?}: the task reports the reading of its essential container, got \
             {task}"
        );
    }
}

#[tokio::test]
async fn what_one_execution_of_a_probe_writes_is_there_for_the_next_one() {
    let router = service();
    let case = "a probe that writes its own marker on its first execution";
    cluster_with_capacity(&router, CLUSTER, case).await;

    // The probe runs in the container, so its OWN writes land on the container's layer: this
    // one fails while the marker is missing and writes it, so the next execution of the very
    // same command passes. The container's program writes nothing at all, so the only thing
    // that can have put the marker there is the probe itself.
    let containers = json!([probing_container(
        "app",
        writes_then_stays_up(""),
        health_check(
            json!([
                "CMD-SHELL",
                "if test -f /tmp/probed; then exit 0; else touch /tmp/probed; exit 1; fi",
            ]),
            1,
            None,
        ),
        None,
    )]);
    let task_arn = run_one(&router, "probewritesitsown", containers, case).await;

    let task = poll_until(
        &router,
        &task_arn,
        case,
        "the second execution to find what the first one wrote",
        |task| container_health(task, "app", case) == "HEALTHY",
    )
    .await;
    assert_eq!(
        task["healthStatus"].as_str(),
        Some("HEALTHY"),
        "case {case:?}: the task reports the reading of its essential container, got {task}"
    );
}

#[tokio::test]
async fn one_container_s_probe_does_not_see_another_container_s_filesystem() {
    let router = service();
    let case = "two containers of one task, one of them writing the marker";
    cluster_with_capacity(&router, CLUSTER, case).await;

    // `writer` creates the marker and tests for it; `watcher` writes nothing and tests for
    // the SAME path. A container's layer is its own, so only the writer's probe finds it —
    // an implementation sharing one filesystem across containers would report both healthy.
    let containers = json!([
        probing_container(
            "writer",
            writes_then_stays_up("touch /tmp/ready; "),
            health_check(json!(["CMD-SHELL", "test -f /tmp/ready"]), 1, None),
            None,
        ),
        probing_container(
            "watcher",
            writes_then_stays_up(""),
            health_check(json!(["CMD-SHELL", "test -f /tmp/ready"]), 1, None),
            None,
        ),
    ]);
    let task_arn = run_one(&router, "probepercontainer", containers, case).await;

    let task = poll_until(
        &router,
        &task_arn,
        case,
        "each container's probe to answer for its own filesystem",
        |task| {
            container_health(task, "writer", case) == "HEALTHY"
                && container_health(task, "watcher", case) == "UNHEALTHY"
        },
    )
    .await;
    assert_eq!(
        container_health(&task, "writer", case),
        "HEALTHY",
        "case {case:?}: the container that wrote the marker finds it, got {task}"
    );
    assert_eq!(
        container_health(&task, "watcher", case),
        "UNHEALTHY",
        "case {case:?}: the container beside it does not, got {task}"
    );
    // "If any essential containers in the task are reporting as UNHEALTHY ... the task
    // status also reports as UNHEALTHY": both containers of this task are essential.
    assert_eq!(
        task["healthStatus"].as_str(),
        Some("UNHEALTHY"),
        "case {case:?}: the task reports the worst reading of its essential containers, got \
         {task}"
    );
}
