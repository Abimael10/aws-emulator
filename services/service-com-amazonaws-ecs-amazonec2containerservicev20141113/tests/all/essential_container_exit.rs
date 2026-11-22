// SPDX-License-Identifier: CC0-1.0

//! What a service does when the CONTAINER of one of its tasks ends: the task that is
//! stopped with it, the replacement its scheduler places, and the rollout the deployment
//! circuit breaker fails when the replacements keep ending the same way.
//!
//! The rules under test:
//!
//! 1. `ContainerDefinition$essential`: "If the `essential` parameter of a container is
//!    marked as `true`, and that container fails or stops for any reason, all other
//!    containers that are part of the task are stopped" — so a task whose ESSENTIAL
//!    container's program has ended is stopped, with the `TaskStopCode`
//!    `EssentialContainerExited` ("The task stopped because an essential container
//!    exited"), whatever status that container exited with; and a task whose NON-essential
//!    container ends keeps running, because "if the `essential` parameter of a container is
//!    marked as `false`, its failure doesn't affect the rest of the containers in a task".
//! 2. The same rule for a STANDALONE task, which is the one the clock stops rather than a
//!    scheduler: `Container$exitCode` is "the exit code returned from the container", so a
//!    task is stopped with `EssentialContainerExited` — and with the sentence Amazon ECS
//!    words for it — exactly when an ESSENTIAL container of it has EXITED. A task no
//!    essential container of which produced an exit status has had no essential container
//!    exit, so it keeps the `RUNNING` status Amazon ECS reports for a task whose containers
//!    are running and carries no `stopCode` and no `stoppedReason` at all — however long it
//!    has been running, and whatever of its containers' programs this emulator was able to
//!    perform — while every container of it still runs its own program to the end and
//!    reports the status that program ended with.
//! 3. `Deployment$failedTasks`: "The number of consecutively failed tasks in the
//!    deployment. A task is considered a failure if the service scheduler can't launch the
//!    task, the task doesn't transition to a `RUNNING` state, or if it fails any of its
//!    defined health checks and is stopped" — with the note that "once a service deployment
//!    has one or more successfully running tasks, the failed task count resets to zero". A
//!    rollout whose tasks keep ending therefore reaches the circuit breaker's threshold,
//!    and `Deployment$rolloutState` makes the result terminal: "If the service fails to
//!    reach a steady state and circuit breaker is turned on, the deployment transitions to
//!    a `FAILED` state", with `DeploymentCircuitBreaker$rollback` deciding where that
//!    leaves the service — "the service is rolled back to the last deployment that
//!    completed successfully".
//! 4. The other side of that same sentence: a rollout has reached the steady state it was
//!    rolling out ("When the service reaches a steady state, the deployment transitions to a
//!    `COMPLETED` state") only once the task set it placed has STAYED up, so a task set that
//!    stood at the count the service wants for one instant settles nothing — a revision
//!    whose containers end as soon as they start is never reported as a rollout that
//!    succeeded, and the breaker watching it takes it to a terminal `ROLLBACK` status
//!    instead.
//!
//! The containers below run REAL programs through the container data plane (the same
//! session a `RunTask` container's command runs in), so the exit each row asserts on is one
//! a program produced rather than one this suite wrote into the state struct. Every value
//! asserted is read back through `DescribeServices`, `ListTasks` and `DescribeTasks`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, count, describe_service_deployment, list, ok, present_count, service,
    string,
};

/// The cluster every service below runs in.
const CLUSTER: &str = "exiting";

/// The image every container below names. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The command a container that stays up runs.
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// How often a poll re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying. Generous: a rollout takes one scheduler step per task it
/// replaces, the breaker tolerates several failures before it fires, and every poll stops
/// the instant its own condition holds.
const POLL_BUDGET: Duration = Duration::from_secs(90);

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The `rolloutState` of a rollout the circuit breaker failed, and the reason it carries.
const FAILED: &str = "FAILED";

/// One container definition running `command`, essential or not.
fn container(name: &str, command: &[&str], essential: bool) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "memory": 128,
        "essential": essential,
        "command": command,
    })
}

/// Register one revision of `family` with `containers`, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    containers: Vec<Value>,
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

/// Create one service of `definition` in [`CLUSTER`], with `configuration` as its
/// `deploymentConfiguration`.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    configuration: Value,
    case: &str,
) {
    ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
            "deploymentConfiguration": configuration,
        }),
        case,
    )
    .await;
}

/// The service `DescribeServices` reports.
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

/// The service's current (`PRIMARY`) deployment.
fn primary(view: &Value, case: &str) -> Value {
    list(view, "deployments", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {view}"))
}

/// Re-read one service until `settled` holds of it, and answer it.
async fn poll_service(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    what: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let view = describe(router, name, case).await;
        if settled(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {what}, got {view}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The task ARNs `ListTasks` reports for one service, in the `desiredStatus` asked for.
async fn task_arns(
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
    list(&body, "taskArns", case)
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect()
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// One row of the essential-container table: what the revision's containers do, and whether
/// the task they belong to is stopped when the one that ends has ended.
#[derive(Debug, Clone, Copy)]
struct ExitCase {
    case: &'static str,
    /// The command the container that ENDS runs.
    command: &'static [&'static str],
    /// Whether that container is essential.
    essential: bool,
    /// Whether a SECOND, essential container that stays up is in the revision beside it.
    beside_a_container_that_stays_up: bool,
    /// Whether the task is stopped when that container has ended.
    stops_the_task: bool,
}

#[tokio::test]
async fn a_service_task_is_stopped_and_replaced_when_its_essential_container_ends() {
    let rows: [ExitCase; 4] = [
        ExitCase {
            // "that container fails or stops FOR ANY REASON": a clean exit ends the task
            // exactly as a failing one does.
            case: "an essential container that exits zero",
            command: &["sh", "-c", "exit 0"],
            essential: true,
            beside_a_container_that_stays_up: false,
            stops_the_task: true,
        },
        ExitCase {
            case: "an essential container that exits non-zero",
            command: &["sh", "-c", "echo leaving; exit 7"],
            essential: true,
            beside_a_container_that_stays_up: false,
            stops_the_task: true,
        },
        ExitCase {
            // The same exit beside a container that stays up: the task still stops,
            // because "all other containers that are part of the task are stopped".
            case: "an essential container that exits beside one that stays up",
            command: &["sh", "-c", "exit 3"],
            essential: true,
            beside_a_container_that_stays_up: true,
            stops_the_task: true,
        },
        ExitCase {
            // "If the essential parameter of a container is marked as false, its failure
            // doesn't affect the rest of the containers in a task."
            case: "a non-essential container that exits",
            command: &["sh", "-c", "exit 9"],
            essential: false,
            beside_a_container_that_stays_up: true,
            stops_the_task: false,
        },
    ];

    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let family = format!("exitingfamily{index}");
        let name = format!("exitingservice{index}");
        let mut containers = vec![container("ending", row.command, row.essential)];
        if row.beside_a_container_that_stays_up {
            containers.push(container("staying", &STAYS_UP, true));
        }
        let definition = register(&router, &family, containers, case).await;
        // The circuit breaker is OFF here: this row is about the task, and a rollout the
        // breaker could fail would stop the replacing this row observes.
        create_service(&router, &name, &definition, json!({}), case).await;

        if row.stops_the_task {
            // The task is stopped with the code a task whose essential container exited
            // earns, and the scheduler maintains the count: a replacement is placed.
            let deadline = Instant::now() + POLL_BUDGET;
            let (arn, task) = loop {
                let arns = task_arns(&router, &name, "STOPPED", case).await;
                if let Some(arn) = arns.first() {
                    let task = describe_task(&router, arn, case).await;
                    if !string(&task, "stopCode", case).is_empty() {
                        break (arn.clone(), task);
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected a stopped task with a stop code, got {arns:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            assert_eq!(
                string(&task, "stopCode", case),
                ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: a task whose essential container ended carries the stop \
                 code for it, got {task}"
            );
            assert!(
                !string(&task, "stoppedReason", case).is_empty(),
                "case {case:?}: such a task says why it stopped, got {task}"
            );
            let replaced = poll_service(
                &router,
                &name,
                case,
                "the scheduler to place a replacement",
                |view| count(view, "runningCount", case) + count(view, "pendingCount", case) > 0,
            )
            .await;
            let live = task_arns(&router, &name, "RUNNING", case).await;
            assert!(
                live.iter().all(|other| *other != arn),
                "case {case:?}: the replacement is a new task, got {live:?} beside {arn} \
                 ({replaced})"
            );
        } else {
            // The task keeps running: its essential container is still up, so nothing the
            // non-essential one did ends it.
            let running = poll_service(
                &router,
                &name,
                case,
                "the service to hold its task",
                |view| count(view, "runningCount", case) == 1,
            )
            .await;
            assert_eq!(
                count(&running, "runningCount", case),
                1,
                "case {case:?}: a non-essential container's exit ends no task, got {running}"
            );
            let stopped = task_arns(&router, &name, "STOPPED", case).await;
            assert!(
                stopped.is_empty(),
                "case {case:?}: no task of this service is stopped, got {stopped:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_circuit_breaker_fails_and_rolls_back_a_rollout_whose_tasks_keep_ending() {
    let case = "a rollout whose tasks keep ending";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let family = "breakerexiting";
    let good = register(
        &router,
        family,
        vec![container("app", &STAYS_UP, true)],
        case,
    )
    .await;
    let bad = register(
        &router,
        family,
        vec![container("app", &["sh", "-c", "exit 1"], true)],
        case,
    )
    .await;
    let name = "breakerexiting";
    create_service(
        &router,
        name,
        &good,
        json!({ "deploymentCircuitBreaker": { "enable": true, "rollback": true } }),
        case,
    )
    .await;
    // The rollout that COMPLETED, which is the one a rollback puts back.
    poll_service(
        &router,
        name,
        case,
        "the first rollout to complete",
        |view| string(&primary(view, case), "rolloutState", case) == "COMPLETED",
    )
    .await;

    ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "taskDefinition": bad }),
        case,
    )
    .await;

    // The breaker counts the tasks the rollout loses and fails it once they reach its
    // threshold: the rollout leaves IN_PROGRESS for the terminal FAILED, with the reason
    // that names the breaker.
    let failed = poll_service(
        &router,
        name,
        case,
        "the circuit breaker to fail the rollout",
        |view| {
            list(view, "deployments", case)
                .iter()
                .any(|deployment| string(deployment, "rolloutState", case) == FAILED)
        },
    )
    .await;
    let reason = list(&failed, "deployments", case)
        .iter()
        .find(|deployment| string(deployment, "rolloutState", case) == FAILED)
        .map(|deployment| string(deployment, "rolloutStateReason", case))
        .unwrap_or_default();
    assert!(
        reason.contains("circuit breaker"),
        "case {case:?}: the failed rollout's reason names the breaker, got {reason:?} \
         ({failed})"
    );

    // And the rollback takes the service back to the revision that completed, running the
    // count it wants.
    let rolled_back = poll_service(
        &router,
        name,
        case,
        "the service to be rolled back to the revision that completed",
        |view| {
            string(view, "taskDefinition", case) == good
                && count(view, "runningCount", case) == 1
                && count(view, "pendingCount", case) == 0
        },
    )
    .await;
    assert_eq!(
        string(&rolled_back, "taskDefinition", case),
        good,
        "case {case:?}: expected the service back on the revision that completed, got \
         {rolled_back}"
    );
}

// --- rule 2: the STANDALONE task, whose stop the clock materialises ----------------------

/// The `stoppedReason` that goes with [`ESSENTIAL_CONTAINER_EXITED`], spelled out here for
/// the reason that constant is.
const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// How long a row watches a standalone task that must go on running: past the span in which
/// a standalone task whose essential container HAS exited is stopped (three seconds in
/// `state::CONTAINER_WORK_SPAN`), so a task stopped without any essential container of it
/// having exited is observed being stopped rather than read once before it is.
const WATCH_WINDOW: Duration = Duration::from_secs(6);

/// How long the slow program of the second standalone test runs for: longer than that same
/// span, so the container is still performing its program at the instant a stop would have
/// fallen due and the status it reports was produced after it.
const SLOW_PROGRAM_SECONDS: u64 = 4;

/// How long the two tests below keep looking for a condition before they give up. Generous:
/// the slow program above runs for several seconds inside a session of this process, and
/// these tests run beside every other test of the workspace, which can slow every span down.
/// The deadline is only here so a failure is a failure rather than a hang.
const STANDALONE_POLL_BUDGET: Duration = Duration::from_secs(240);

/// One container of a standalone row: what it runs, whether it is essential, and the status
/// it must report once the task has settled.
struct StandaloneContainer {
    name: &'static str,
    /// The argument vector the container's image is handed, and `None` for a launch that
    /// names none at all — whose program is the image's own entry point, which this emulator
    /// does not hold.
    command: Option<&'static [&'static str]>,
    /// `ContainerDefinition$essential`.
    essential: bool,
    /// `Container$exitCode`, and `None` for a container that produces no exit status.
    exit_code: Option<i64>,
}

/// What a standalone row expects of the task once it has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ends {
    /// An essential container exited with this status, so the task is stopped for it.
    EssentialExit(i64),
    /// No essential container exited, so the task goes on running.
    RunsOn,
}

/// One row of the standalone stop table.
struct StandaloneRow {
    case: &'static str,
    containers: &'static [StandaloneContainer],
    ends: Ends,
}

/// The rows differ along what the rule generalises over — WHY no essential container has
/// exited: its program runs on, its program needs an interpreter this emulator has none of,
/// its launch names no program at all, and the container that did exit was not essential —
/// against the contrast rows where an essential container really does exit.
const STANDALONE_ROWS: &[StandaloneRow] = &[
    StandaloneRow {
        case: "a program that runs until the container is stopped",
        containers: &[StandaloneContainer {
            name: "app",
            command: Some(&["sh", "-c", "echo up; sleep 600"]),
            essential: true,
            exit_code: None,
        }],
        ends: Ends::RunsOn,
    },
    StandaloneRow {
        // The shape the recorded workloads take: an interpreter handed an inline program,
        // and one that does not return — a server in the foreground, which AWS reports
        // RUNNING for as long as it serves. This emulator holds no interpreter, so nothing
        // of that program runs and nothing is claimed about it.
        case: "an interpreter running an inline program, which this emulator has none of",
        containers: &[StandaloneContainer {
            name: "guest",
            command: Some(&[
                "python3",
                "-c",
                "import http.server, socketserver\n\
                 socketserver.TCPServer(('', 8080), http.server.SimpleHTTPRequestHandler)\
                 .serve_forever()",
            ]),
            essential: true,
            exit_code: None,
        }],
        ends: Ends::RunsOn,
    },
    StandaloneRow {
        case: "a launch that names no argument vector at all",
        containers: &[StandaloneContainer {
            name: "imageentrypoint",
            command: None,
            essential: true,
            exit_code: None,
        }],
        ends: Ends::RunsOn,
    },
    StandaloneRow {
        // "If the `essential` parameter of a container is marked as `false`, its failure
        // doesn't affect the rest of the containers in a task": the exit is REPORTED and the
        // task runs on.
        case: "a container that exits while it is not essential, beside one that runs on",
        containers: &[
            StandaloneContainer {
                name: "sidecar",
                command: Some(&["sh", "-c", "exit 0"]),
                essential: false,
                exit_code: Some(0),
            },
            StandaloneContainer {
                name: "app",
                command: Some(&STAYS_UP),
                essential: true,
                exit_code: None,
            },
        ],
        ends: Ends::RunsOn,
    },
    StandaloneRow {
        // Two containers that exit while NEITHER of them is essential, beside the essential
        // one every task has ("all tasks must have at least one essential container"): each
        // reports the status its own program produced, and no exit of theirs stops the task.
        case: "two containers that exit while neither is essential",
        containers: &[
            StandaloneContainer {
                name: "first",
                command: Some(&["sh", "-c", "exit 0"]),
                essential: false,
                exit_code: Some(0),
            },
            StandaloneContainer {
                name: "second",
                command: Some(&["sh", "-c", "exit 6"]),
                essential: false,
                exit_code: Some(6),
            },
            StandaloneContainer {
                name: "app",
                command: Some(&STAYS_UP),
                essential: true,
                exit_code: None,
            },
        ],
        ends: Ends::RunsOn,
    },
    StandaloneRow {
        case: "a program that ends successfully",
        containers: &[StandaloneContainer {
            name: "app",
            command: Some(&["sh", "-c", "echo done"]),
            essential: true,
            exit_code: Some(0),
        }],
        ends: Ends::EssentialExit(0),
    },
    StandaloneRow {
        // The shape a run-to-completion task takes: the program writes a line, WAITS past
        // the span a standalone task's work is reported running for, and then ends of its
        // own accord. The status the container reports is the one its own program produced,
        // never the one a container ended from OUTSIDE the task would carry.
        case: "a program that writes a line, waits and then ends on its own",
        containers: &[StandaloneContainer {
            name: "app",
            command: Some(&["/bin/sh", "-c", "echo ready; sleep 4"]),
            essential: true,
            exit_code: Some(0),
        }],
        ends: Ends::EssentialExit(0),
    },
    StandaloneRow {
        case: "a program that ends with a status of its own",
        containers: &[StandaloneContainer {
            name: "app",
            command: Some(&["sh", "-c", "echo leaving; exit 9"]),
            essential: true,
            exit_code: Some(9),
        }],
        ends: Ends::EssentialExit(9),
    },
    StandaloneRow {
        case: "the essential one of two containers exits",
        containers: &[
            StandaloneContainer {
                name: "app",
                command: Some(&["sh", "-c", "exit 4"]),
                essential: true,
                exit_code: Some(4),
            },
            StandaloneContainer {
                name: "sidecar",
                command: Some(&STAYS_UP),
                essential: false,
                exit_code: None,
            },
        ],
        ends: Ends::EssentialExit(4),
    },
];

/// The container definition one standalone row's container is registered as.
fn standalone_definition(held: &StandaloneContainer) -> Value {
    let mut definition = json!({
        "name": held.name,
        "image": IMAGE,
        "memory": 128,
        "essential": held.essential,
    });
    if let Some(command) = held.command {
        definition["command"] = json!(command);
    }
    definition
}

/// `RunTask` one standalone task of `family` in [`CLUSTER`], and answer its ARN.
async fn run_standalone(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a task back, got {body}"));
    string(task, "taskArn", case)
}

/// Read one task again until its `lastStatus` is `status`, and answer it.
async fn task_at(router: &MultiServiceRouter, arn: &str, status: &str, case: &str) -> Value {
    let deadline = Instant::now() + STANDALONE_POLL_BUDGET;
    loop {
        let task = describe_task(router, arn, case).await;
        if string(&task, "lastStatus", case) == status {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach {status} within the poll budget, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The exit status one named container of a described task reports, and `None` for a
/// container that reported none.
fn exit_code_of(task: &Value, name: &str, case: &str) -> Option<i64> {
    let containers = list(task, "containers", case);
    let container = containers
        .iter()
        .find(|held| string(held, "name", case) == name)
        .unwrap_or_else(|| panic!("case {case:?}: expected a container named {name}, got {task}"));
    present_count(container, "exitCode", case)
}

/// Assert of one described task that it claims NO stop: no `stopCode` and no `stoppedReason`
/// reach the wire, and it is both running and wanted running.
fn claims_no_stop(task: &Value, case: &str) {
    for member in ["stopCode", "stoppedReason"] {
        assert_eq!(
            task.get(member).and_then(Value::as_str),
            None,
            "case {case:?}: a task no essential container of which exited carries no \
             {member}, got {task}"
        );
    }
    for member in ["lastStatus", "desiredStatus"] {
        assert_eq!(
            string(task, member, case),
            "RUNNING",
            "case {case:?}: a task no essential container of which exited goes on running, \
             its {member} too, got {task}"
        );
    }
}

/// RULE 2: a standalone task is stopped as one whose essential container exited exactly
/// when an essential container of it really exited.
#[tokio::test]
async fn a_standalone_task_is_stopped_only_when_an_essential_container_exits() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "stops that need an exit").await;

    // Every row's task is launched FIRST and read afterwards: the runs happen in parallel in
    // the one emulator, so the test waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&StandaloneRow, String)> = Vec::new();
    for (index, row) in STANDALONE_ROWS.iter().enumerate() {
        let family = format!("standalonestop{index}");
        let definitions: Vec<Value> = row.containers.iter().map(standalone_definition).collect();
        ok(
            &router,
            "RegisterTaskDefinition",
            json!({ "family": &family, "containerDefinitions": definitions }),
            row.case,
        )
        .await;
        let arn = run_standalone(&router, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let Ends::EssentialExit(status) = row.ends else {
            continue;
        };
        let task = task_at(&router, arn, "STOPPED", case).await;
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop code of an essential container's exit, got {task}"
        );
        assert_eq!(
            string(&task, "stoppedReason", case),
            ESSENTIAL_CONTAINER_EXITED_REASON,
            "case {case:?}: expected the reason Amazon ECS words for that code, got {task}"
        );
        let essential = row
            .containers
            .iter()
            .find(|held| held.essential)
            .unwrap_or_else(|| panic!("case {case:?}: this row has an essential container"));
        let reported = exit_code_of(&task, essential.name, case);
        assert_eq!(
            reported,
            Some(status),
            "case {case:?}: expected the exiting container to report {status}, got \
             {reported:?} in {task}"
        );
    }

    // The rows that go on running: each task reaches `RUNNING` and STAYS there, claiming no
    // stop. Watched in ONE loop over every such row, so the window is spent once rather than
    // once per row.
    let running: Vec<(&StandaloneRow, &String)> = launched
        .iter()
        .filter(|(row, _)| row.ends == Ends::RunsOn)
        .map(|(row, arn)| (*row, arn))
        .collect();
    for (row, arn) in &running {
        task_at(&router, arn, "RUNNING", row.case).await;
    }
    let until = Instant::now() + WATCH_WINDOW;
    while Instant::now() < until {
        tokio::time::sleep(POLL_INTERVAL).await;
        for (row, arn) in &running {
            let task = describe_task(&router, arn, row.case).await;
            claims_no_stop(&task, row.case);
        }
    }
    // And what each container of such a task reports, read once the window is over: the
    // status its own program ended with, and nothing for a program that is still going or
    // that was never performed.
    for (row, arn) in &running {
        let task = describe_task(&router, arn, row.case).await;
        for held in row.containers {
            let reported = exit_code_of(&task, held.name, row.case);
            assert_eq!(
                reported, held.exit_code,
                "case {:?}: expected container {} to report exitCode {:?}, got {reported:?} \
                 in {task}",
                row.case, held.name, held.exit_code
            );
        }
    }
}

/// RULE 2 (the other half): every container of such a task runs its own program to the end
/// and reports the status it ended with — including a container whose program is still going
/// long after the span a stop would have fallen in, and a sibling that only reads what
/// another container has written.
#[tokio::test]
async fn a_container_still_running_past_the_stop_span_reports_its_own_exit_status() {
    let router = service();
    let case = "a slow writer and the sibling that waits for it";
    cluster_with_capacity(&router, CLUSTER, case).await;

    let family = "slowexits";
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "volumes": [{ "name": "shared", "host": {} }],
            "containerDefinitions": [
                {
                    // The essential container every task has ("all tasks must have at least
                    // one essential container"), and it runs on: no essential container of
                    // this task exits, so nothing stops the task while the two below take
                    // their time.
                    "name": "app",
                    "image": IMAGE,
                    "memory": 128,
                    "essential": true,
                    "command": STAYS_UP,
                },
                {
                    "name": "writer",
                    "image": IMAGE,
                    "memory": 128,
                    // Neither of these two is essential, so no exit of theirs stops the
                    // task: what each reports is its own program's status alone.
                    "essential": false,
                    "command": ["sh", "-c",
                        format!("sleep {SLOW_PROGRAM_SECONDS}; echo payload > /shared/payload")],
                    "mountPoints": [
                        { "sourceVolume": "shared", "containerPath": "/shared", "readOnly": false },
                    ],
                },
                {
                    "name": "reader",
                    "image": IMAGE,
                    "memory": 128,
                    "essential": false,
                    // Waits for the CONDITION — the file the writer writes — rather than for
                    // a span of its own, so what it reads is what the writer wrote.
                    "command": ["sh", "-c",
                        "while [ ! -f /shared/payload ]; do sleep 1; done; cat /shared/payload"],
                    "mountPoints": [
                        { "sourceVolume": "shared", "containerPath": "/shared", "readOnly": true },
                    ],
                },
            ],
        }),
        case,
    )
    .await;
    let arn = run_standalone(&router, family, case).await;

    // Both programs end, however long after the stop span that is, and each reports the
    // status it ended with.
    let deadline = Instant::now() + STANDALONE_POLL_BUDGET;
    let settled = loop {
        let task = describe_task(&router, &arn, case).await;
        let codes: Vec<Option<i64>> = ["writer", "reader"]
            .iter()
            .map(|name| exit_code_of(&task, name, case))
            .collect();
        if codes.iter().all(Option::is_some) {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected both containers to report an exit status, got {codes:?} \
             in {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    for name in ["writer", "reader"] {
        let reported = exit_code_of(&settled, name, case);
        assert_eq!(
            reported,
            Some(0),
            "case {case:?}: expected container {name} to report exitCode 0, got {reported:?} \
             in {settled}"
        );
    }
    // And the task itself claims no stop: neither of those two was essential, and the
    // essential container of the task has not exited.
    claims_no_stop(&settled, case);
}

// --- rule 4: a rollout is not a steady state until its task set has STAYED up -------------

/// Every TERMINAL `ServiceDeploymentStatus` a rollout can end in
/// (`smithy_query(query="shape", shape="ServiceDeploymentStatus")` -> PENDING, SUCCESSFUL,
/// STOPPED, STOP_REQUESTED, IN_PROGRESS, ROLLBACK_REQUESTED, ROLLBACK_IN_PROGRESS,
/// ROLLBACK_SUCCESSFUL, ROLLBACK_FAILED): the four a rollout stops moving in.
const TERMINAL_DEPLOYMENT_STATUSES: [&str; 4] = [
    "SUCCESSFUL",
    "STOPPED",
    "ROLLBACK_SUCCESSFUL",
    "ROLLBACK_FAILED",
];

/// The `ServiceDeploymentStatus` of a rollout that reached the steady state it was rolling
/// out — the one a rollout whose task set does not stay up must never be reported in.
const SUCCESSFUL: &str = "SUCCESSFUL";

/// One row: what the rollout's own revision does once its task starts, the bounds the
/// rolling update runs under, and where that leaves the rollout.
struct RolloutRow {
    case: &'static str,
    /// The service (and task-definition family) of this row.
    name: &'static str,
    /// The command the revision this rollout rolls out runs.
    command: &'static [&'static str],
    /// Whether the service is created straight onto that revision — a FIRST rollout, which
    /// has no earlier task set of the service standing beside the one it places — or is
    /// created on a revision that stays up and then updated onto it.
    first_rollout: bool,
    /// Whether the rollout the update replaces is waited for before the update is made. A
    /// row that does not wait releases over a rollout the clock has not settled yet, so the
    /// service holds no rollout recorded as completed when the breaker fires.
    waits_for_the_rollout_it_replaces: bool,
    /// `(minimumHealthyPercent, maximumPercent)` the service is created with: the lower
    /// limit on the tasks that "must continue to run" through the rollout and the upper
    /// limit on those "allowed in the RUNNING or PENDING state".
    percentages: (i64, i64),
    /// Whether the service must be holding a RUNNING task once the rollout has ended: a
    /// floor of the whole desired count is one the rollout may not take the service below,
    /// so the task set that was running before it keeps the service up through it.
    keeps_a_running_task: bool,
    /// The terminal status the rollout ends in.
    ends: &'static str,
}

/// The rows. They differ along what the rule generalises over: whether the rollout's task
/// stays up at all, HOW it ends when it does not (a clean exit and a failing one, both of
/// which are a task the service could not keep), whether anything of the service was running
/// before the rollout started, and the bounds the rolling update runs under.
const ROLLOUT_ROWS: [RolloutRow; 5] = [
    RolloutRow {
        case: "a first rollout whose task exits non-zero as soon as it starts",
        name: "firstrollout",
        command: &["sh", "-c", "exit 1"],
        first_rollout: true,
        waits_for_the_rollout_it_replaces: false,
        percentages: (0, 100),
        // Nothing of this service was ever running, so the floor has nothing to keep up.
        keeps_a_running_task: false,
        // "If rollback is on, when a service deployment fails, the service is rolled back to
        // the last deployment that completed successfully" — a service whose FIRST rollout
        // failed has none, and no earlier rollout at all, so the rollback it asked for has
        // nothing to put back.
        ends: "ROLLBACK_FAILED",
    },
    RolloutRow {
        case: "an update onto a revision whose task exits zero as soon as it starts",
        name: "updatedrollout",
        command: &["sh", "-c", "exit 0"],
        first_rollout: false,
        waits_for_the_rollout_it_replaces: true,
        // A floor of none lets the scheduler take the task set it is replacing off before
        // the replacement is up, so the rollout's own task set is all that stands for it.
        percentages: (0, 100),
        keeps_a_running_task: false,
        ends: "ROLLBACK_SUCCESSFUL",
    },
    RolloutRow {
        case: "the same update under the documented default bounds",
        name: "defaultbounds",
        command: &["sh", "-c", "exit 1"],
        first_rollout: false,
        waits_for_the_rollout_it_replaces: true,
        // The defaults a `CreateService` with no percentages of its own takes for a REPLICA
        // service, stated here because what they decide is this row's subject.
        percentages: (100, 200),
        // A floor of the whole desired count is one the rollout may not take the service
        // below: the task the rollout is replacing keeps running until a replacement has
        // STAYED up, and a revision whose tasks end at once never produces one.
        keeps_a_running_task: true,
        ends: "ROLLBACK_SUCCESSFUL",
    },
    RolloutRow {
        case: "an update released over a rollout the clock had not settled yet",
        name: "unsettledrollout",
        command: &["sh", "-c", "exit 1"],
        first_rollout: false,
        // The release is made without waiting, so the rollout it replaces is superseded
        // before it reached a steady state and the service holds NO rollout recorded as
        // completed — the revision this one is replacing is still the one the service was
        // running, which is where the rollback goes.
        waits_for_the_rollout_it_replaces: false,
        percentages: (100, 200),
        keeps_a_running_task: true,
        ends: "ROLLBACK_SUCCESSFUL",
    },
    RolloutRow {
        case: "an update onto a revision whose task stays up",
        name: "staysuprollout",
        command: &STAYS_UP,
        first_rollout: false,
        waits_for_the_rollout_it_replaces: true,
        percentages: (100, 200),
        keeps_a_running_task: true,
        ends: SUCCESSFUL,
    },
];

/// Watch one rollout until it reaches a terminal status, and answer every status it was seen
/// in, in the order it was seen in them.
async fn observed_statuses(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let record = describe_service_deployment(router, arn, case).await;
        let status = string(&record, "status", case);
        if seen.last() != Some(&status) {
            seen.push(status.clone());
        }
        if TERMINAL_DEPLOYMENT_STATUSES.contains(&status.as_str()) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rollout to reach a terminal status, saw {seen:?}: \
             {record}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a rollout has reached the steady state it was rolling out only once the task set it
/// placed has STAYED up — a task the service is about to lose is not one the rollout
/// achieved — so a revision whose containers end as soon as they start is never reported
/// SUCCESSFUL, and the circuit breaker watching it drives the rollout to a ROLLBACK status
/// instead, onto the revision the service was running; a rollout whose task set does stay up
/// still reaches SUCCESSFUL.
#[tokio::test]
async fn a_rollout_whose_task_set_does_not_stay_up_is_never_reported_successful() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "rollout steady state").await;
    for row in &ROLLOUT_ROWS {
        let case = row.case;
        // The revision the service is CREATED on: for a row that updates onto the rolled-out
        // revision, one that stays up, registered first so it is the earlier revision of the
        // family; for a first rollout, the rolled-out revision itself.
        let created_on = if row.first_rollout {
            register(
                &router,
                row.name,
                vec![container("app", row.command, true)],
                case,
            )
            .await
        } else {
            register(
                &router,
                row.name,
                vec![container("app", &STAYS_UP, true)],
                case,
            )
            .await
        };
        let rolling_out = if row.first_rollout {
            created_on.clone()
        } else {
            register(
                &router,
                row.name,
                vec![container("app", row.command, true)],
                case,
            )
            .await
        };
        // Every service configures the breaker that watches its rollouts, under the bounds
        // this row states: "the lower limit on the number of tasks ... that must continue to
        // run" and "the upper limit on the number of tasks ... that are allowed in the
        // RUNNING or PENDING state".
        let (minimum, maximum) = row.percentages;
        let configuration = json!({
            "deploymentCircuitBreaker": { "enable": true, "rollback": true },
            "minimumHealthyPercent": minimum,
            "maximumPercent": maximum,
        });
        create_service(&router, row.name, &created_on, configuration, case).await;
        if !row.first_rollout {
            if row.waits_for_the_rollout_it_replaces {
                // The rollout that COMPLETED, which is what a rollback has to put back.
                poll_service(&router, row.name, case, "the first rollout to complete", |view| {
                    string(&primary(view, case), "rolloutState", case) == "COMPLETED"
                        && count(view, "runningCount", case) == 1
                })
                .await;
            }
            ok(
                &router,
                "UpdateService",
                json!({
                    "cluster": CLUSTER,
                    "service": row.name,
                    "taskDefinition": rolling_out,
                }),
                case,
            )
            .await;
        }
        let view = describe(&router, row.name, case).await;
        let rollout = string(&view, "currentServiceDeployment", case);

        let seen = observed_statuses(&router, &rollout, case).await;
        assert_eq!(
            seen.last().map(String::as_str),
            Some(row.ends),
            "case {case:?}: expected the rollout to end {:?}, saw {seen:?}",
            row.ends
        );
        if row.ends != SUCCESSFUL {
            assert!(
                !seen.iter().any(|status| status == SUCCESSFUL),
                "case {case:?}: expected a rollout whose task set did not stay up never to be \
                 reported {SUCCESSFUL:?}, saw {seen:?}"
            );
        }
        // Where the rollout left the SERVICE: the revision it was rolling out when it stayed
        // up, and the one that completed before it when the rollback put that back.
        let expected_revision = if row.ends == "ROLLBACK_SUCCESSFUL" {
            created_on.clone()
        } else {
            rolling_out.clone()
        };
        let settled = poll_service(
            &router,
            row.name,
            case,
            "the service to settle on the revision the rollout left it on",
            |view| string(view, "taskDefinition", case) == expected_revision,
        )
        .await;
        assert_eq!(
            string(&settled, "taskDefinition", case),
            expected_revision,
            "case {case:?}: expected the service to be left on the revision the rollout's own \
             end names, got {settled}"
        );
        // And what the floor the rollout ran under owed the service: a lower limit on the
        // tasks that "must continue to run" is one a rollout may not take the service below,
        // however badly the revision it was rolling out ends, so the service is still
        // holding a task of its own once the rollout has.
        if row.keeps_a_running_task {
            let running = running_task_count(&router, row.name, case).await;
            assert!(
                running >= 1,
                "case {case:?}: expected the floor to keep a task of the service running \
                 through the rollout, got {running} running task(s)"
            );
        }
        // The rollout the service ends up on is one of its own: the revision the rollback
        // put back is deployed by a rollout like any other, so the service converges on the
        // count it wants and that rollout reports the steady state it reached. A rollout the
        // breaker failed with nothing to put back is the exception the same doc names: "a
        // deployment in FAILED state doesn't launch any new tasks", so the service it failed
        // converges on nothing.
        if row.ends != "ROLLBACK_FAILED" {
            let converged = poll_service(
                &router,
                row.name,
                case,
                "the service to converge on the rollout it was left on",
                |view| {
                    string(&primary(view, case), "rolloutState", case) == "COMPLETED"
                        && count(view, "runningCount", case) == 1
                },
            )
            .await;
            assert_eq!(
                string(&primary(&converged, case), "taskDefinition", case),
                expected_revision,
                "case {case:?}: expected the converged rollout to run the revision the \
                 service was left on, got {converged}"
            );
        }
    }
}

/// How many tasks of one service are RUNNING, read back through `ListTasks` and
/// `DescribeTasks` — the task's own reported status rather than a count the service
/// published.
async fn running_task_count(router: &MultiServiceRouter, name: &str, case: &str) -> usize {
    let mut running = 0;
    for arn in task_arns(router, name, "RUNNING", case).await {
        let task = describe_task(router, &arn, case).await;
        if string(&task, "lastStatus", case) == "RUNNING" {
            running += 1;
        }
    }
    running
}
