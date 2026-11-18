// SPDX-License-Identifier: CC0-1.0

//! What a container whose PROGRAM this emulator does not run reports, and what it does NOT
//! report.
//!
//! The rule under test, over the whole class rather than one command:
//!
//! > A container's `exitCode` is "the exit code returned from the container", so it is
//! > reported only for a program that RAN and ended: a launch whose argument vector needs a
//! > command, a command form or an expansion this emulator's container session does not
//! > perform has run nothing of the image's own program, so the container reports NO exit
//! > code — exactly as a container whose launch names no argument vector at all reports none
//! > — and the task it belongs to is therefore not stopped as a task whose essential
//! > container exited. A program that ends with a status of its own still reports that
//! > status.
//!
//! Which matters beyond the one container, and is the second rule here: a SERVICE's task
//! whose program is one of those is not stopped and replaced over and over, so the
//! registration it earns in the Elastic Load Balancing v2 target group its service binds
//! ("the full Amazon Resource Name (ARN) of the Elastic Load Balancing target group ...
//! associated with a service", `LoadBalancer$targetGroupArn`) is ONE registration that
//! reaches a health verdict, rather than a target set that churns between a draining
//! address and a newly registering one.
//!
//! The rows span what the rule generalises over: WHICH part of a program is not performed —
//! a utility the session has no program for (named directly in the vector and inside a shell
//! program), an unsupported compound form, an unsupported assignment form — and where it sits
//! in the program (first, after a command that did run, behind an `&&`). The contrast rows are
//! programs that really do end with a status, which still report it — including the ones that
//! read and write the container's own FILESYSTEM (`state::container_filesystem`), which is
//! something the session does perform.
//!
//! Nothing here reaches into a state struct: every value asserted is read back through
//! `DescribeTasks`, `DescribeServices` and `DescribeTargetHealth`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, present_count, seed_awsvpc_subnets, string, AWSVPC_START,
    AWSVPC_SUBNET,
};
use crate::load_balancer_target_groups::{all_texts, elb_call};

/// The image every container below names. Nothing is fetched from it: what the emulator runs
/// is the `command` the revision states, and the point of these rows is that it cannot.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// How often a poll re-reads a surface it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// How long a poll keeps trying before it gives up. Generous: a standalone task runs for the
/// span its work takes before it stops, and an `awsvpc` task's start waits for the interface
/// Amazon ECS attaches for it — and this suite runs beside every other test of the workspace,
/// which can slow every one of those spans down. The deadline is only here so a failure is a
/// failure rather than a hang.
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// `Task$stopCode` of a task whose essential container exited, spelled out here rather than
/// imported so a wrong constant in the implementation cannot make a test pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// One row of the exit-status table.
struct Row {
    case: &'static str,
    /// The argument vector the container's image is handed.
    command: &'static [&'static str],
    /// The status the container is expected to report, and `None` for a program this
    /// emulator runs nothing of — which reports no status at all.
    exit_code: Option<i64>,
}

const ROWS: &[Row] = &[
    // The CONFINEMENT rows: this container has no process to start and no host to reach, so
    // each of these forms runs nothing — and claims nothing.
    Row {
        case: "a vector naming a utility the container has no program for",
        command: &["definitely-not-a-utility"],
        exit_code: None,
    },
    Row {
        case: "a compound form this container does not perform",
        command: &["sh", "-c", "for name in one two; do echo $name; done"],
        exit_code: None,
    },
    Row {
        case: "an appending assignment, a form this container does not perform",
        command: &["sh", "-c", "PATH+=/opt/bin"],
        exit_code: None,
    },
    Row {
        // The shape the recorded workloads take: a line that RUNS — a redirection into the
        // container's own writable layer — and then the server the image holds, which this
        // emulator has no program for.
        case: "a program that writes and then needs the image's own server",
        command: &[
            "sh",
            "-c",
            "echo marker > /usr/share/nginx/html/index.html && nginx -g 'daemon off;'",
        ],
        exit_code: None,
    },
    // The CONTRAST rows: programs that really end, which report the status they ended with.
    Row {
        case: "a program that ends with a status of its own",
        command: &["sh", "-c", "echo leaving; exit 7"],
        exit_code: Some(7),
    },
    Row {
        case: "a vector the image exec's directly, which runs to success",
        command: &["echo", "direct"],
        exit_code: Some(0),
    },
    Row {
        // The container HAS a filesystem (`state::container_filesystem`) and this emulator
        // holds no image, so a path the image would have provided is one that filesystem
        // holds nothing at: the read fails, which is a status the program really ended with.
        case: "a read of a path the container's filesystem holds nothing at",
        command: &["sh", "-c", "cat /etc/passwd"],
        exit_code: Some(1),
    },
    Row {
        // No host path is reachable from it: the write lands in the container's own layer,
        // which is memory of this process.
        case: "a redirection, which writes the container's own layer",
        command: &[
            "sh",
            "-c",
            "echo secret > /tmp/emulator-escape && cat /tmp/emulator-escape",
        ],
        exit_code: Some(0),
    },
    Row {
        case: "a command substitution, whose commands run in this same session",
        command: &["sh", "-c", "echo $(echo inner)"],
        exit_code: Some(0),
    },
];

/// Register one revision of `family` whose single essential container runs `command`.
async fn register(router: &MultiServiceRouter, family: &str, command: &[&str], case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": command,
            }],
        }),
        case,
    )
    .await;
}

/// `RunTask` one task of `family` in `cluster`, and answer its ARN.
async fn run_task(router: &MultiServiceRouter, cluster: &str, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({ "cluster": cluster, "taskDefinition": family }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a task back, got {body}"));
    string(task, "taskArn", case)
}

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
///
/// Waiting for the CONDITION rather than for a span: a standalone task is stopped once the
/// work its containers were placed to do is over, which is what makes its container's run
/// certainly finished — so what is asserted afterwards is what the run left behind rather
/// than a reading taken too early.
async fn stopped_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, cluster, arn, case).await;
        if string(&task, "lastStatus", case) == "STOPPED" {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to stop within the poll budget, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a container reports an `exitCode` only for a program that ran and ended.
#[tokio::test]
async fn a_program_this_container_does_not_perform_claims_no_exit_status() {
    let router = crate::ecs_wire::service();
    let cluster = "notperformed";
    cluster_with_capacity(&router, cluster, "programs not performed").await;

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel
    // in the one emulator, so the suite waits for the longest of them rather than for their
    // sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("notperformed{index}");
        register(&router, &family, row.command, row.case).await;
        let arn = run_task(&router, cluster, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, arn, case).await;
        let containers = list(&task, "containers", case);
        let container = containers
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"));
        let reported = present_count(container, "exitCode", case);
        assert_eq!(
            reported, row.exit_code,
            "case {case:?}: expected exitCode {:?}, got {reported:?} in {task}",
            row.exit_code
        );
        if row.exit_code.is_none() {
            // Nothing ran, so nothing exited: the stop such a task earns is the end of the
            // work a standalone task was placed to do, and no container of it claims a
            // status.
            assert!(
                containers
                    .iter()
                    .all(|held| present_count(held, "exitCode", case).is_none()),
                "case {case:?}: no container of this task reports a status, got {task}"
            );
        } else {
            assert_eq!(
                string(&task, "stopCode", case),
                ESSENTIAL_CONTAINER_EXITED,
                "case {case:?}: a task whose essential container really exited carries the \
                 stop code for it, got {task}"
            );
        }
    }
}

/// The ELBv2 target group name and the container port every row of the service rule uses.
const CONTAINER_PORT: i64 = 80;

/// An emulator with Amazon ECS and Elastic Load Balancing v2 on ONE engine, with the subnet
/// an `awsvpc` placement resolves against in the account's Amazon VPC records.
fn emulator_with_elbv2() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// `(target id, state)` for every registration one group reports.
async fn target_health(
    router: &MultiServiceRouter,
    group: &str,
    case: &str,
) -> Vec<(String, String)> {
    let (status, body) =
        elb_call(router, "DescribeTargetHealth", &[("TargetGroupArn", group)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTargetHealth must succeed, got {status} with {body}"
    );
    let ids = all_texts(&body, "Id");
    let states = all_texts(&body, "State");
    assert_eq!(
        ids.len(),
        states.len(),
        "case {case:?}: expected one state per described target, got {ids:?} and {states:?}"
    );
    ids.into_iter().zip(states).collect()
}

/// The ARNs of the tasks one service is running, as `ListTasks` reports them.
async fn running_task_arns(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
    case: &str,
) -> Vec<String> {
    let body = ok(
        router,
        "ListTasks",
        json!({ "cluster": cluster, "serviceName": service, "desiredStatus": "RUNNING" }),
        case,
    )
    .await;
    list(&body, "taskArns", case)
        .iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: a load-balanced service whose container's program this emulator runs nothing of
/// keeps its task, so the target it registered reaches a health verdict instead of churning.
#[tokio::test]
async fn a_load_balanced_services_target_settles_when_its_program_is_not_performed() {
    // Two rows, each in an emulator of its own so they share nothing, run CONCURRENTLY:
    // every wait is the clock settling a transition, and waiting them out one after another
    // would hold this binary for twice as long as the rule needs.
    tokio::join!(
        one_service_row(
            "a program that needs the image's own server",
            &[
                "sh",
                "-c",
                "echo marker > /usr/share/nginx/html/index.html && nginx -g 'daemon off;'",
            ],
        ),
        one_service_row(
            "a vector naming a utility the container has no program for",
            &["definitely-not-a-utility"],
        ),
    );
}

/// One row of the service table: a load-balanced service whose container runs `command`.
async fn one_service_row(case: &str, command: &[&str]) {
    let router = emulator_with_elbv2();
    let cluster = "churn";
    let service = "web";
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": cluster }),
        case,
    )
    .await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": cluster,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [{
                "name": "web",
                "image": IMAGE,
                "essential": true,
                "command": command,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
            }],
        }),
        case,
    )
    .await;
    let group =
        crate::load_balancer_target_groups::create_target_group(&router, "churn-tg", "ip", case)
            .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": service,
            "taskDefinition": cluster,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [AWSVPC_SUBNET],
                    "securityGroups": ["sg-0123456789abcdef0"],
                }
            },
            "loadBalancers": [{
                "targetGroupArn": &group,
                "containerName": "web",
                "containerPort": CONTAINER_PORT,
            }],
        }),
        case,
    )
    .await;

    // The task the service placed comes up and STAYS up: the registration it earns reaches
    // the `healthy` the load balancer's checks give it, which a task that was stopped and
    // replaced never gets to.
    let deadline = Instant::now() + POLL_BUDGET + AWSVPC_START;
    let healthy = loop {
        let reported = target_health(&router, &group, case).await;
        if reported.len() == 1 && reported.iter().all(|(_, state)| state == "healthy") {
            break reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected exactly one healthy target of the group, got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let running = running_task_arns(&router, cluster, service, case).await;
    assert_eq!(
        running.len(),
        1,
        "case {case:?}: expected the service to hold one running task, got {running:?}"
    );

    // ...and it is STILL that one task, at that one address, after a window far longer than
    // the stop and replacement a task whose container exited would have taken: a target set
    // that churns is read here as the second address it registers.
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL_INTERVAL).await;
        let reported = target_health(&router, &group, case).await;
        assert_eq!(
            reported, healthy,
            "case {case:?}: the group must keep the one registration it reached a verdict \
             on, got {reported:?}"
        );
        let live = running_task_arns(&router, cluster, service, case).await;
        assert_eq!(
            live, running,
            "case {case:?}: the service must keep the task it placed, got {live:?}"
        );
    }
}

/// How many further looks a row takes at its group once the target is healthy. Together with
/// [`POLL_INTERVAL`] a window several seconds long — far past the two transitions a stop and
/// its replacement take — so a target set that churns is observed churning rather than read
/// once before it does.
const QUIET_LOOKS: usize = 30;
