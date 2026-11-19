// SPDX-License-Identifier: CC0-1.0

//! The container DATA PLANE and the scheduler's STEADY STATE, driven end to end over HTTP.
//!
//! Three rules are under test here, each over a table of inputs:
//!
//! 1. A container's `command` is RUN: what the program writes on its standard output is
//!    delivered to the log group its `logConfiguration` names, one log event per line, in a
//!    stream named `<awslogs-stream-prefix>/<container name>/<task id>`; and the status the
//!    program ends with is the container's `exitCode`.
//! 2. `StopTask` sends the container the stop signal ("a stop signal value ... will default
//!    to SIGTERM"), so a container that handles it exits with the status its own handler
//!    chose and one that does not is ended by the signal (status 128 + 15).
//! 3. A service "runs and maintains your desired number of tasks": a stopped task of a
//!    service is REPLACED by a new task of the same task definition, while an identical
//!    standalone `RunTask` task — which no scheduler maintains a count for — is never
//!    relaunched. A service's task reports `group` `service:<service name>`; a standalone
//!    task does not.
//! 4. A VOLUME the task definition declares is mounted into the container at each of its
//!    `mountPoints[].containerPath`, over ONE namespace, and a mount point whose `readOnly`
//!    is `true` has every write to it refused with the kernel's EROFS while the read through
//!    it still answers.
//! 5. A container's process runs with the RUNTIME SETTINGS its task definition and its
//!    placement resolved — `workingDirectory`, `user`, `ulimits` and the task's
//!    `ephemeralStorage` — so the utilities that report a process's own session answer those
//!    values from inside the container: `pwd`, `id`, `ulimit` and `df`.
//!
//! CloudWatch Logs is mounted BESIDE Amazon ECS on the SAME engine, so a container's output
//! travels the real in-process callback into the real log service and is read back with that
//! service's own `GetLogEvents`: nothing here reads the producer's own buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, cluster_with_capacity, create_cluster, list, ok, present_count,
    seed_awsvpc_subnets, string, ACCOUNT, SIGNED_REGION,
};

/// The log group every container below delivers to, created through CloudWatch Logs' own
/// API before any task is launched — the way a client sets one up.
const LOG_GROUP: &str = "/emulator/ecs/containers";

/// The `awslogs-stream-prefix` the task definitions below configure, and so the first
/// segment of every stream name they deliver to.
const STREAM_PREFIX: &str = "run";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from; irrelevant to these rules, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// How often a poll re-reads the emulator.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: comfortably past the clock-owed
/// transitions a task takes to start and to stop (`state::TRANSITION_SPAN` is 500 ms), with
/// margin for a loaded test machine.
const POLL_BUDGET: Duration = Duration::from_secs(10);

/// How long a row waits before asserting that something did NOT happen: long enough that it
/// would have by now.
const QUIET_BUDGET: Duration = Duration::from_secs(3);

/// A router carrying Amazon ECS and CloudWatch Logs over ONE shared engine — the two
/// services a container's output travels through, mounted the way the production router
/// mounts them — holding the one Amazon VPC subnet an `awsvpc` placement of this suite names
/// in the account's records, which a Fargate task is allocated its interface in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_awsvpc_subnets(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One CloudWatch Logs call, the way a client makes it: `POST /` with
/// `X-Amz-Target: Logs_20140328.<operation>` and a credential scoped to `logs`, which is
/// what the router picks the service from.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /logs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// Create the log group every container of a suite delivers to.
async fn create_log_group(router: &MultiServiceRouter, case: &str) {
    let (status, body) = logs(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLogGroup to answer 200, got {status} with {body}"
    );
}

/// The messages CloudWatch Logs holds in `stream`, oldest first — the events the container's
/// own output became. An unknown stream reads as no messages, which is what a stream nothing
/// was delivered to holds.
async fn delivered(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming the driver's own
/// `awslogs-stream-prefix` documentation states. Spelled out here rather than imported, so
/// a wrong naming in the implementation cannot make a test pass.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// One container definition that RUNS `command`, delivering its output to [`LOG_GROUP`]
/// through the `awslogs` driver, with `environment` in its process's environment.
fn container(command: &[&str], environment: &[(&str, &str)]) -> Value {
    json!({
        "name": CONTAINER,
        "image": IMAGE,
        "memory": 128,
        "command": command,
        "environment": environment
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect::<Vec<Value>>(),
        "logConfiguration": {
            "logDriver": "awslogs",
            "options": {
                "awslogs-group": LOG_GROUP,
                "awslogs-region": SIGNED_REGION,
                "awslogs-stream-prefix": STREAM_PREFIX,
            },
        },
    })
}

/// Register one revision of `family` whose single container runs `command`, and answer its
/// ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    command: &[&str],
    environment: &[(&str, &str)],
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [container(command, environment)],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Launch one standalone task of `task_definition` in `cluster` and answer its ARN.
async fn run_task(
    router: &MultiServiceRouter,
    cluster: &str,
    task_definition: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": cluster,
            "taskDefinition": task_definition,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {body}"
    );
    string(&tasks[0], "taskArn", case)
}

/// The task `task_arn` as `DescribeTasks` reports it now.
async fn describe(router: &MultiServiceRouter, cluster: &str, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [task_arn] }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected DescribeTasks to describe {task_arn}, got {body}"
    );
    tasks[0].clone()
}

/// The ARNs of every task of `cluster`, whatever its status.
async fn all_task_arns(router: &MultiServiceRouter, cluster: &str, case: &str) -> Vec<String> {
    let mut arns = Vec::new();
    for status in ["RUNNING", "PENDING", "STOPPED"] {
        let body = ok(
            router,
            "ListTasks",
            json!({ "cluster": cluster, "desiredStatus": status }),
            case,
        )
        .await;
        for arn in list(&body, "taskArns", case) {
            if let Some(arn) = arn.as_str() {
                arns.push(arn.to_string());
            }
        }
    }
    arns.sort();
    arns.dedup();
    arns
}

/// Poll `probe` until it answers `Some`, or [`POLL_BUDGET`] runs out — for the transitions
/// the clock owes and the deliveries a container's own run makes.
async fn poll_until<T, F, Fut>(probe: F, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    poll_until_within(probe, POLL_BUDGET, description).await
}

/// The same, with the budget the caller gives it: a Fargate `awsvpc` task waits for the
/// interface Amazon ECS creates and attaches for it before anything of it starts, which is
/// several spans more than a task placed on capacity already registered to its cluster.
async fn poll_until_within<T, F, Fut>(probe: F, budget: Duration, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + budget;
    loop {
        if let Some(answer) = probe().await {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "{description}: not observed within {budget:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE 1: what a container writes is delivered to its own `awslogs` stream, one event per
/// line, and the status its program ends with is the container's `exitCode`.
///
/// The rows span what the rule generalises over: how many lines the program writes, the
/// status it ends with, where the text comes from (literal, the container's environment),
/// whether a write ends its line at all, which command FORM produced it (a simple command,
/// an and-or list, a loop) and whether the launch named a shell program or an argument
/// vector the image exec's directly.
///
/// A program whose vector needs a command, a command form or an expansion this container
/// does not perform ends with no status of its own at all, which is
/// `all/container_program_not_performed.rs`.
#[tokio::test]
async fn a_containers_output_and_exit_status_are_its_programs_own() {
    let router = emulator();
    let cluster = "runtime";
    cluster_with_capacity(&router, cluster, "container runtime").await;
    create_log_group(&router, "container runtime").await;

    struct Case {
        name: &'static str,
        command: &'static [&'static str],
        environment: &'static [(&'static str, &'static str)],
        delivered: &'static [&'static str],
        exit_code: i64,
    }

    let cases = [
        Case {
            name: "one line per echo",
            command: &["sh", "-c", "echo one; echo two"],
            environment: &[],
            delivered: &["one", "two"],
            exit_code: 0,
        },
        Case {
            name: "the status exit names",
            command: &["sh", "-c", "echo leaving; exit 7"],
            environment: &[],
            delivered: &["leaving"],
            exit_code: 7,
        },
        Case {
            name: "a status past one byte wraps as a shell wraps it",
            command: &["sh", "-c", "exit 300"],
            environment: &[],
            delivered: &[],
            exit_code: 44,
        },
        Case {
            name: "the environment the container was given",
            command: &["sh", "-c", "echo $GREETING ${MISSING:-default}"],
            environment: &[("GREETING", "hello")],
            delivered: &["hello default"],
            exit_code: 0,
        },
        Case {
            // The two default-value forms differ on a variable that is SET but empty:
            // `${NAME-WORD}` takes the default only when the name is unset, `${NAME:-WORD}`
            // also when it is empty.
            name: "the two default-value forms on an empty-but-set variable",
            command: &[
                "sh",
                "-c",
                "echo \"${BLANK-unset-only}\" \"${BLANK:-unset-or-empty}\"",
            ],
            environment: &[("BLANK", "")],
            delivered: &[" unset-or-empty"],
            exit_code: 0,
        },
        Case {
            name: "a write that does not end its line",
            command: &["sh", "-c", "echo -n half; echo rest"],
            environment: &[],
            delivered: &["halfrest"],
            exit_code: 0,
        },
        Case {
            name: "an and-or list",
            command: &["sh", "-c", "false || echo fallback && echo both"],
            environment: &[],
            delivered: &["fallback", "both"],
            exit_code: 0,
        },
        Case {
            name: "a loop whose condition is false at once",
            command: &["sh", "-c", "echo start; while false; do echo never; done; exit 3"],
            environment: &[],
            delivered: &["start"],
            exit_code: 3,
        },
        Case {
            name: "an argument vector the image exec's directly",
            command: &["echo", "direct"],
            environment: &[],
            delivered: &["direct"],
            exit_code: 0,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.name;
        let family = format!("runtime-{index}");
        let definition = register(
            &router,
            &family,
            case.command,
            case.environment,
            name,
        )
        .await;
        let task_arn = run_task(&router, cluster, &definition, name).await;
        let stream = stream_of(&task_arn);

        // The container's program ends on its own, and the status it ended with is what the
        // container reports.
        let exit_code = poll_until(
            || async {
                let task = describe(&router, cluster, &task_arn, name).await;
                let containers = list(&task, "containers", name);
                present_count(&containers[0], "exitCode", name)
            },
            &format!("case {name:?}: the container's exitCode"),
        )
        .await;
        assert_eq!(
            exit_code, case.exit_code,
            "case {name:?}: expected exitCode {}, got {exit_code}",
            case.exit_code
        );

        if case.delivered.is_empty() {
            // Nothing was written, so nothing is delivered — asserted after the status
            // arrived, which is after everything the program wrote would have.
            let events = delivered(&router, &stream).await;
            assert!(
                events.is_empty(),
                "case {name:?}: expected no log events in {stream}, got {events:?}"
            );
            continue;
        }
        let expected: Vec<String> = case.delivered.iter().map(|line| line.to_string()).collect();
        let events = poll_until(
            || async {
                let events = delivered(&router, &stream).await;
                (events.len() >= expected.len()).then_some(events)
            },
            &format!("case {name:?}: {} line(s) in {stream}", expected.len()),
        )
        .await;
        assert_eq!(
            events, expected,
            "case {name:?}: expected {expected:?} in {stream}, got {events:?}"
        );
    }
}

/// RULE 2: `StopTask` sends the container the stop signal, so a container that HANDLES it
/// exits with the status its own handler chose and one that does not is ended by the signal.
///
/// The rows span what the rule generalises over: whether the program installed a handler at
/// all, which status the handler exits with, and whether it writes anything before it does.
#[tokio::test]
async fn a_stop_sends_the_container_the_signal_its_program_answers() {
    let router = emulator();
    let cluster = "signals";
    cluster_with_capacity(&router, cluster, "stop signal").await;
    create_log_group(&router, "stop signal").await;

    struct Case {
        name: &'static str,
        /// The program, which must stay running until the signal arrives.
        command: &'static [&'static str],
        /// What the handler writes before it exits, if anything.
        farewell: Option<&'static str>,
        /// The status the container is expected to report.
        exit_code: i64,
    }

    let cases = [
        Case {
            name: "a handler that writes and exits with its own status",
            command: &[
                "sh",
                "-c",
                "echo up; trap 'echo down; exit 42' TERM; while true; do sleep 1; done",
            ],
            farewell: Some("down"),
            exit_code: 42,
        },
        Case {
            name: "a handler spelled with the signal's SIG name",
            command: &[
                "sh",
                "-c",
                "echo up; trap 'exit 9' SIGTERM; sleep infinity",
            ],
            farewell: None,
            exit_code: 9,
        },
        Case {
            name: "no handler at all, so the signal ends the program",
            command: &["sh", "-c", "echo up; sleep 3600"],
            farewell: None,
            // "the exit status shall be 128 plus the signal number": SIGTERM is 15.
            exit_code: 143,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.name;
        let family = format!("signals-{index}");
        let definition = register(&router, &family, case.command, &[], name).await;
        let task_arn = run_task(&router, cluster, &definition, name).await;
        let stream = stream_of(&task_arn);

        // Every program writes its boot line first, which is what says the container is
        // really running before anything is stopped.
        poll_until(
            || async {
                let events = delivered(&router, &stream).await;
                (events.first().map(String::as_str) == Some("up")).then_some(())
            },
            &format!("case {name:?}: the boot line in {stream}"),
        )
        .await;

        ok(
            &router,
            "StopTask",
            json!({ "cluster": cluster, "task": task_arn }),
            name,
        )
        .await;

        let exit_code = poll_until(
            || async {
                let task = describe(&router, cluster, &task_arn, name).await;
                let containers = list(&task, "containers", name);
                present_count(&containers[0], "exitCode", name)
            },
            &format!("case {name:?}: the stopped container's exitCode"),
        )
        .await;
        assert_eq!(
            exit_code, case.exit_code,
            "case {name:?}: expected exitCode {}, got {exit_code}",
            case.exit_code
        );

        let events = delivered(&router, &stream).await;
        match case.farewell {
            Some(farewell) => assert_eq!(
                events.last().map(String::as_str),
                Some(farewell),
                "case {name:?}: expected the handler's line {farewell:?} last in {stream}, \
                 got {events:?}"
            ),
            None => assert_eq!(
                events,
                vec!["up".to_string()],
                "case {name:?}: expected only the boot line in {stream}, got {events:?}"
            ),
        }
    }
}

/// The BOUND on a container's run: a program that never ends is given up on, and nothing is
/// claimed about the container it was running.
///
/// The run's step budget is this emulator's, not the service's, so a program written as an
/// endless loop cannot hold the emulator for as long as the process lives: it is ABANDONED
/// once the budget is spent, which reports no `exitCode` — no status was produced — and
/// leaves the emulator serving every other call.
///
/// The rows span what the bound has to hold for: a loop whose commands this container
/// PERFORMS, and loops whose condition and body are commands it DECLINES (so they perform no
/// command at all) — in both polarities, `until` on a declined command and `while` on a
/// negated one.
#[tokio::test]
async fn a_program_that_never_ends_is_bounded_and_claims_no_exit_status() {
    let router = emulator();
    let cluster = "bounded";
    cluster_with_capacity(&router, cluster, "endless program").await;
    create_log_group(&router, "endless program").await;

    let cases: [(&str, &str); 3] = [
        (
            "a loop of commands the container performs",
            "echo up; while true; do :; done",
        ),
        (
            "an until loop whose condition and body are commands it declines",
            "echo up; until definitely-not-a-utility; do definitely-not-a-utility; done",
        ),
        (
            "a while loop whose condition is a declined command negated",
            "echo up; while ! definitely-not-a-utility; do definitely-not-a-utility; done",
        ),
    ];

    for (index, (name, script)) in cases.iter().enumerate() {
        let family = format!("bounded-{index}");
        let definition = register(&router, &family, &["sh", "-c", script], &[], name).await;
        let task_arn = run_task(&router, cluster, &definition, name).await;
        let stream = stream_of(&task_arn);

        // The program really started: its first line is delivered before the loop.
        poll_until(
            || async {
                let events = delivered(&router, &stream).await;
                (events == vec!["up".to_string()]).then_some(())
            },
            &format!("case {name:?}: the boot line in {stream}"),
        )
        .await;

        // Long enough for the budget to be spent, and for an exit status to have been
        // recorded if one were ever going to be.
        ::tokio::time::sleep(QUIET_BUDGET).await;
        let task = describe(&router, cluster, &task_arn, name).await;
        let containers = list(&task, "containers", name);
        assert_eq!(
            present_count(&containers[0], "exitCode", name),
            None,
            "case {name:?}: a run that was given up on claims no exit status, got {task}"
        );
        // The emulator is still serving: the call above answered, and so does this one.
        let events = delivered(&router, &stream).await;
        assert_eq!(
            events,
            vec!["up".to_string()],
            "case {name:?}: the abandoned run delivered nothing after its boot line, got \
             {events:?}"
        );
    }
}

/// RULE 3: a service maintains its desired count, so a stopped task of a service is
/// REPLACED, while an identical standalone task is never relaunched — and the two are told
/// apart on the record by the `group` each reports.
///
/// Both halves run on the SAME task definition in the SAME cluster, so the only difference
/// between them is which of the two launched the task: the service's scheduler, or
/// `RunTask`. Each container writes its boot line and its farewell line to its OWN stream
/// and exits with its handler's status, which is how a client observes that the replacement
/// is a container that really ran.
#[tokio::test]
async fn a_stopped_service_task_is_replaced_while_a_standalone_task_is_not() {
    let case = "service replacement";
    let router = emulator();
    let cluster = "steady";
    cluster_with_capacity(&router, cluster, case).await;
    create_log_group(&router, case).await;

    let command = [
        "sh",
        "-c",
        "echo booted; trap 'echo farewell; exit 42' TERM; while true; do sleep 1; done",
    ];
    let definition = register(&router, "steady", &command, &[], case).await;

    let service_name = "steady-service";
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": service_name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;

    // The scheduler's own task, once it is running.
    let service_task = poll_until(
        || async {
            let body = ok(
                &router,
                "ListTasks",
                json!({ "cluster": cluster, "serviceName": service_name }),
                case,
            )
            .await;
            let arns = list(&body, "taskArns", case);
            let arn = arns.first()?.as_str()?.to_string();
            let task = describe(&router, cluster, &arn, case).await;
            (string(&task, "lastStatus", case) == "RUNNING").then_some(arn)
        },
        "the service's first task reaching RUNNING",
    )
    .await;
    let service_group = string(&describe(&router, cluster, &service_task, case).await, "group", case);
    assert_eq!(
        service_group,
        format!("service:{service_name}"),
        "the task a service placed reports the service's own group, got {service_group:?}"
    );

    // The identical standalone task, which no scheduler maintains a count for.
    let standalone = run_task(&router, cluster, &definition, case).await;
    poll_until(
        || async {
            let task = describe(&router, cluster, &standalone, case).await;
            (string(&task, "lastStatus", case) == "RUNNING").then_some(())
        },
        "the standalone task reaching RUNNING",
    )
    .await;
    let standalone_group = string(&describe(&router, cluster, &standalone, case).await, "group", case);
    assert!(
        !standalone_group.starts_with("service:"),
        "a standalone task reports no service group, got {standalone_group:?}"
    );

    // Both containers are really running: each wrote its boot line into its own stream.
    for (which, task_arn) in [("service", &service_task), ("standalone", &standalone)] {
        let stream = stream_of(task_arn);
        poll_until(
            || async {
                let events = delivered(&router, &stream).await;
                (events == vec!["booted".to_string()]).then_some(())
            },
            &format!("the {which} container's boot line in {stream}"),
        )
        .await;
    }

    for task_arn in [&service_task, &standalone] {
        ok(
            &router,
            "StopTask",
            json!({ "cluster": cluster, "task": task_arn }),
            case,
        )
        .await;
    }

    // The service's task is REPLACED: a task that is not the stopped one reaches RUNNING on
    // the same task definition, under the same group.
    let replacement = poll_until(
        || async {
            let body = ok(
                &router,
                "ListTasks",
                json!({
                    "cluster": cluster,
                    "serviceName": service_name,
                    "desiredStatus": "RUNNING",
                }),
                case,
            )
            .await;
            for arn in list(&body, "taskArns", case) {
                let arn = arn.as_str()?.to_string();
                if arn == service_task {
                    continue;
                }
                let task = describe(&router, cluster, &arn, case).await;
                if string(&task, "lastStatus", case) == "RUNNING" {
                    return Some((arn, task));
                }
            }
            None
        },
        "a replacement task of the service reaching RUNNING",
    )
    .await;
    assert_eq!(
        string(&replacement.1, "taskDefinitionArn", case),
        definition,
        "the replacement runs the same task definition, got {}",
        replacement.1
    );
    assert_eq!(
        string(&replacement.1, "group", case),
        format!("service:{service_name}"),
        "the replacement is the service's own task, got {}",
        replacement.1
    );
    // It is a container that really ran: its own stream carries its own boot line.
    let replacement_stream = stream_of(&replacement.0);
    poll_until(
        || async {
            let events = delivered(&router, &replacement_stream).await;
            (events == vec!["booted".to_string()]).then_some(())
        },
        &format!("the replacement container's boot line in {replacement_stream}"),
    )
    .await;

    // Both stopped containers handled the signal: each wrote its farewell line and exited
    // with its handler's status.
    for (which, task_arn) in [("service", &service_task), ("standalone", &standalone)] {
        let exit_code = poll_until(
            || async {
                let task = describe(&router, cluster, task_arn, case).await;
                let containers = list(&task, "containers", case);
                present_count(&containers[0], "exitCode", case)
            },
            &format!("the {which} container's exitCode"),
        )
        .await;
        assert_eq!(
            exit_code, 42,
            "the {which} container exits with its handler's status, got {exit_code}"
        );
        let stream = stream_of(task_arn);
        let events = delivered(&router, &stream).await;
        assert_eq!(
            events,
            vec!["booted".to_string(), "farewell".to_string()],
            "the {which} container's stream carries its boot and farewell lines, got {events:?}"
        );
    }

    // The standalone task is NOT relaunched: after long enough for a replacement to have
    // appeared, the cluster holds exactly the tasks the service placed plus the one
    // `RunTask` placed.
    ::tokio::time::sleep(QUIET_BUDGET).await;
    let arns = all_task_arns(&router, cluster, case).await;
    let standalone_tasks: Vec<String> = {
        let mut standalone_tasks = Vec::new();
        for arn in &arns {
            let task = describe(&router, cluster, arn, case).await;
            if !string(&task, "group", case).starts_with("service:") {
                standalone_tasks.push(arn.clone());
            }
        }
        standalone_tasks
    };
    assert_eq!(
        standalone_tasks,
        vec![standalone.clone()],
        "no standalone task is relaunched, so the only one is the task RunTask placed; \
         the cluster holds {arns:?}"
    );
}

/// RULE 4: a VOLUME the task definition declares is mounted into the container at each of its
/// `mountPoints[].containerPath` as a real filesystem over ONE namespace — so a file written
/// through one mount point of a volume is read back through another mount point of the same
/// volume — and a mount point whose `readOnly` is `true` gives the container read-only access:
/// the kernel refuses every write to it with EROFS, whose text is `Read-only file system`,
/// while the read through it still answers and the volume is left as it was.
///
/// One task mounts one volume TWICE, read-write and read-only, which is what makes the two
/// halves of the rule observable in one run: the same bytes through both paths, and only one of
/// them writable. The volume is a `host` volume, whose data belongs to the task, so nothing
/// outside this task is involved; the Amazon EFS file system a volume can be backed by instead
/// — the namespace two TASKS share — is `core/router/tests/all/ecs_efs_volume_mounts.rs`,
/// because that one spans another service's store.
#[tokio::test]
async fn a_declared_volume_is_mounted_and_a_read_only_mount_point_refuses_writes() {
    let router = emulator();
    let cluster = "volumes";
    let case = "mounted volumes";
    cluster_with_capacity(&router, cluster, case).await;
    create_log_group(&router, case).await;

    let marker = "marker-the-container-wrote";
    let family = "volumemounts";
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "volumes": [{ "name": "shared", "host": {} }],
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": ["sh", "-c",
                    "set -e; \
                     echo \"$MARKER\" > /data/payload; \
                     echo \"SIZE=$(wc -c < /data/payload)\"; \
                     echo \"MODE=$(ls -l /data/payload | cut -c1-10)\"; \
                     echo \"SHARED=$(cat /readonly/payload)\"; \
                     echo appended >> /readonly/payload && echo WROTE; \
                     echo \"KEPT=$(cat /data/payload)\"; \
                     echo DONE"],
                "mountPoints": [
                    { "sourceVolume": "shared", "containerPath": "/data", "readOnly": false },
                    { "sourceVolume": "shared", "containerPath": "/readonly", "readOnly": true },
                ],
                "environment": [{ "name": "MARKER", "value": marker }],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": SIGNED_REGION,
                        "awslogs-stream-prefix": STREAM_PREFIX,
                    },
                },
            }],
        }),
        case,
    )
    .await;
    let task_arn = run_task(&router, cluster, family, case).await;
    let stream = stream_of(&task_arn);

    // The program runs to its end, which is what makes the lines before it the whole of what
    // it wrote.
    let lines = poll_until(
        || async {
            let events = delivered(&router, &stream).await;
            events
                .iter()
                .any(|line| line == "DONE")
                .then_some(events)
        },
        &format!("case {case:?}: the container's own lines in {stream}"),
    )
    .await;

    let value_of = |name: &str| -> Option<String> {
        lines
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(str::to_string)
    };
    let expected_size = (marker.len() + 1).to_string();
    let size = value_of("SIZE");
    assert_eq!(
        size.as_deref(),
        Some(expected_size.as_str()),
        "case {case:?}: expected SIZE={expected_size} from the container's own wc, got \
         {size:?} in {lines:?}"
    );
    let mode = value_of("MODE");
    assert!(
        mode.as_deref().is_some_and(|mode| mode.starts_with("-rw")),
        "case {case:?}: expected a regular writable file's mode, got {mode:?} in {lines:?}"
    );
    let shared = value_of("SHARED");
    assert_eq!(
        shared.as_deref(),
        Some(marker),
        "case {case:?}: expected the same bytes through the other mount point of the one \
         volume, got {shared:?} in {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Read-only file system")),
        "case {case:?}: expected the write through the read-only mount point to be refused \
         with the kernel's EROFS text, got {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line == "WROTE"),
        "case {case:?}: the refused write must not be reported as done, got {lines:?}"
    );
    let kept = value_of("KEPT");
    assert_eq!(
        kept.as_deref(),
        Some(marker),
        "case {case:?}: expected the refused write to have left the volume as it was, got \
         {kept:?} in {lines:?}"
    );
    let exit_code = poll_until(
        || async {
            let task = describe(&router, cluster, &task_arn, case).await;
            let containers = list(&task, "containers", case);
            present_count(&containers[0], "exitCode", case)
        },
        &format!("case {case:?}: the container's exitCode"),
    )
    .await;
    assert_eq!(
        exit_code, 0,
        "case {case:?}: expected the program to end successfully, got {exit_code} with \
         {lines:?}"
    );
}

/// The cluster the runtime-settings rows below place their Fargate tasks in.
const PROCESS_CLUSTER: &str = "containerprocess";

/// How long the runtime-settings rows keep polling before giving up. Generous: a Fargate
/// `awsvpc` task waits for the interface Amazon ECS creates and attaches for it, then for its
/// own start and its containers', and this suite runs beside every other test of the
/// workspace, which can slow each of those spans down. The deadline is only here so a failure
/// is a failure rather than a hang.
const PROCESS_POLL_BUDGET: Duration = Duration::from_secs(240);

/// What one row of the runtime-settings table asks for, and what its container must report.
struct ProcessRow {
    case: &'static str,
    /// The family prefix the row registers under, so each row reads its own revisions back.
    family: &'static str,
    /// `ContainerDefinition$workingDirectory`, when the row names one.
    working_directory: Option<&'static str>,
    /// `ContainerDefinition$user`, when the row names one.
    user: Option<&'static str>,
    /// `ContainerDefinition$ulimits`, when the row names any.
    ulimits: Option<Value>,
    /// `TaskDefinition$ephemeralStorage.sizeInGiB`, when the row expands it.
    storage_gib: Option<i32>,
    /// The program whose utilities REPORT the settings, whose lines the row asserts.
    report: &'static str,
    /// The lines those utilities must write, in order, with runs of spaces collapsed.
    expect_lines: &'static [&'static str],
    /// The program a SECOND task of the same revision runs: one that BRANCHES on each
    /// setting and exits with the status those branches chose.
    branch: &'static str,
    /// The status that program must exit with.
    expect_exit: i64,
}

/// Register one Fargate-compatible revision of `family` whose one container runs `command`
/// with `row`'s settings and delivers its output to [`LOG_GROUP`].
async fn register_process_revision(
    router: &MultiServiceRouter,
    row: &ProcessRow,
    family: &str,
    command: &str,
) {
    let mut container = json!({
        "name": CONTAINER,
        "image": IMAGE,
        "command": ["sh", "-c", command],
        "logConfiguration": {
            "logDriver": "awslogs",
            "options": {
                "awslogs-group": LOG_GROUP,
                "awslogs-region": SIGNED_REGION,
                "awslogs-stream-prefix": STREAM_PREFIX,
            },
        },
    });
    if let Some(directory) = row.working_directory {
        container["workingDirectory"] = json!(directory);
    }
    if let Some(user) = row.user {
        container["user"] = json!(user);
    }
    if let Some(ulimits) = row.ulimits.clone() {
        container["ulimits"] = ulimits;
    }
    let mut definition = json!({
        "family": family,
        "networkMode": "awsvpc",
        "requiresCompatibilities": ["FARGATE"],
        // A documented Fargate task size: 256 CPU units, 512 MiB.
        "cpu": "256",
        "memory": "512",
        // A Fargate revision whose container configures the `awslogs` driver has to name a
        // task execution role ("Fargate requires task definition to have execution role ARN
        // to support log driver awslogs"). No IAM store is mounted here, so nothing is shown
        // about this role and no gate over its documents decides anything for these launches.
        "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/container-process-exec"),
        "containerDefinitions": [container],
    });
    if let Some(size) = row.storage_gib {
        definition["ephemeralStorage"] = json!({ "sizeInGiB": size });
    }
    ok(router, "RegisterTaskDefinition", definition, row.case).await;
}

/// Launch one standalone Fargate task of `family` and answer its ARN.
async fn run_fargate_task(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": PROCESS_CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
}

/// Poll `probe` until it answers `Some`, or [`PROCESS_POLL_BUDGET`] runs out — waiting for
/// the CONDITION (a line delivered, a status reported) rather than for a span.
async fn poll_process_until<T, F, Fut>(probe: F, description: &str) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    poll_until_within(probe, PROCESS_POLL_BUDGET, description).await
}

/// RULE 5: a container's process runs with the runtime settings its task definition and its
/// placement resolved — `workingDirectory`, `user`, `ulimits` and the task's
/// `ephemeralStorage` — so the utilities that report a process's own session answer those
/// values: `pwd` the directory, `id` the user and group, `ulimit` a resource limit, and `df`
/// the space on the filesystem the container writes on.
///
/// Each row reads them back TWO ways, which is what tells a value that is merely written from
/// one a program can act on: the lines the utilities wrote, delivered through the container's
/// own `awslogs` driver and read back with CloudWatch Logs' own `GetLogEvents`, and the status
/// of a program that BRANCHES on each of them — the form a client checks its runtime settings
/// with.
#[tokio::test]
async fn a_containers_process_reports_the_runtime_settings_its_launch_resolved() {
    let router = emulator();
    create_cluster(&router, PROCESS_CLUSTER, "setup").await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": PROCESS_CLUSTER,
            "capacityProviders": ["FARGATE"],
            "defaultCapacityProviderStrategy": [],
        }),
        "setup",
    )
    .await;
    create_log_group(&router, "setup").await;

    let rows: &[ProcessRow] = &[
        ProcessRow {
            case: "every setting named",
            family: "processnamed",
            working_directory: Some("/srv/app"),
            user: Some("1234:5678"),
            ulimits: Some(json!([
                { "name": "nofile", "softLimit": 1024, "hardLimit": 4096 },
            ])),
            storage_gib: Some(50),
            report: "pwd; id -u; id -g; ulimit -n; ulimit -Hn; df -k /",
            // `df`'s own report: the header, then the row of the filesystem the container's
            // writable layer is on, sized by the task's ephemeral storage — 50 GiB in the
            // 1024-byte blocks the utility reports in.
            expect_lines: &[
                "/srv/app",
                "1234",
                "5678",
                "1024",
                "4096",
                "Filesystem 1K-blocks Used Available Use% Mounted on",
                "overlay 52428800 0 52428800 0% /",
            ],
            branch: "code=0; \
                     [ \"$(pwd)\" = /srv/app ] || code=$((code+1)); \
                     [ \"$(id -u)\" = 1234 ] || code=$((code+2)); \
                     [ \"$(ulimit -n)\" = 1024 ] || code=$((code+4)); \
                     [ \"$(ulimit -Hn)\" = 4096 ] || code=$((code+8)); \
                     exit $code",
            expect_exit: 0,
        },
        ProcessRow {
            case: "no setting named at all",
            family: "processdefault",
            working_directory: None,
            user: None,
            ulimits: None,
            storage_gib: None,
            // The defaults: the root of the container's filesystem, the superuser, the
            // `nofile` limit the `Ulimit` documentation states Fargate overrides, and the
            // ephemeral storage a Fargate task that expands none holds — 20 GiB, reported
            // here in mebibytes.
            report: "pwd; id -u; id -g; ulimit -n; ulimit -Hn; df -m /",
            expect_lines: &[
                "/",
                "0",
                "0",
                "65535",
                "65535",
                "Filesystem 1M-blocks Used Available Use% Mounted on",
                "overlay 20480 0 20480 0% /",
            ],
            branch: "code=0; \
                     [ \"$(pwd)\" = / ] || code=$((code+1)); \
                     [ \"$(id -u)\" = 0 ] || code=$((code+2)); \
                     [ \"$(id -gn)\" = root ] || code=$((code+4)); \
                     [ \"$(ulimit -n)\" = 65535 ] || code=$((code+8)); \
                     exit $code",
            expect_exit: 0,
        },
        ProcessRow {
            case: "a user named by name, and the smallest storage the member admits",
            family: "processnameduser",
            working_directory: Some("/tmp"),
            user: Some("appuser:staff"),
            ulimits: Some(json!([
                { "name": "nproc", "softLimit": -1, "hardLimit": -1 },
                { "name": "stack", "softLimit": 8388608, "hardLimit": 8388608 },
            ])),
            // "The minimum supported value is 21 GiB".
            storage_gib: Some(21),
            // A user written as a NAME is the name `id -n` answers; the limit on the stack is
            // reported in the kibibytes the utility documents, and a limit of -1 is a
            // resource that is not limited at all.
            report: "id -un; id -gn; ulimit -u; ulimit -s; df -h /tmp; pwd",
            expect_lines: &[
                "appuser",
                "staff",
                "unlimited",
                "8192",
                "Filesystem Size Used Available Use% Mounted on",
                "overlay 21.0G 0 21.0G 0% /",
                "/tmp",
            ],
            branch: "code=0; \
                     [ \"$(pwd)\" = /tmp ] || code=$((code+1)); \
                     [ \"$(id -un)\" = appuser ] || code=$((code+2)); \
                     [ \"$(ulimit -u)\" = unlimited ] || code=$((code+4)); \
                     [ \"$(ulimit -s)\" = 8192 ] || code=$((code+8)); \
                     exit $code",
            expect_exit: 0,
        },
        ProcessRow {
            case: "the largest storage the member admits, and a program that answers what it \
                   did not find",
            family: "processmax",
            working_directory: Some("/data"),
            user: Some("7"),
            ulimits: Some(json!([
                { "name": "nofile", "softLimit": 2048, "hardLimit": 8192 },
            ])),
            // "the maximum supported value is 200 GiB".
            storage_gib: Some(200),
            report: "df -k /data; id -u; ulimit -Hn",
            expect_lines: &[
                "Filesystem 1K-blocks Used Available Use% Mounted on",
                "overlay 209715200 0 209715200 0% /",
                "7",
                "8192",
            ],
            // Every clause of this program is FALSE for the settings this revision names, so
            // the status is the sum of their bits — which is what makes the zeros above an
            // observation of the settings rather than a constant.
            branch: "code=0; \
                     [ \"$(pwd)\" = /elsewhere ] || code=$((code+1)); \
                     [ \"$(id -u)\" = 8 ] || code=$((code+2)); \
                     [ \"$(ulimit -n)\" = 1 ] || code=$((code+4)); \
                     [ \"$(ulimit -Hn)\" = 2 ] || code=$((code+8)); \
                     exit $code",
            expect_exit: 15,
        },
    ];

    // Every row's two tasks are launched FIRST and polled afterwards: the runs happen in
    // parallel in the one emulator, so the suite waits for the longest of them rather than
    // for their sum.
    let mut launched: Vec<(&ProcessRow, String, String)> = Vec::new();
    for row in rows {
        let report_family = format!("{}report", row.family);
        register_process_revision(&router, row, &report_family, row.report).await;
        let report_arn = run_fargate_task(&router, &report_family, row.case).await;
        let branch_family = format!("{}branch", row.family);
        register_process_revision(&router, row, &branch_family, row.branch).await;
        let branch_arn = run_fargate_task(&router, &branch_family, row.case).await;
        launched.push((row, report_arn, branch_arn));
    }

    for (row, report_arn, branch_arn) in &launched {
        let case = row.case;
        let stream = stream_of(report_arn);
        let wanted = row.expect_lines.len();
        let lines = poll_process_until(
            || async {
                let events = delivered(&router, &stream).await;
                (events.len() >= wanted).then_some(events)
            },
            &format!("case {case:?}: the {wanted} lines the utilities write in {stream}"),
        )
        .await;
        // Runs of spaces collapsed: `df` ALIGNS its columns, and what a program reads out of
        // the report is its fields rather than the padding between them.
        let written: Vec<String> = lines
            .iter()
            .map(|line| line.split_whitespace().collect::<Vec<&str>>().join(" "))
            .collect();
        assert_eq!(
            written,
            row.expect_lines
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the utilities to report the settings the launch \
             resolved, got {written:?}"
        );

        let exit_code = poll_process_until(
            || async {
                let task = describe(&router, PROCESS_CLUSTER, branch_arn, case).await;
                let containers = list(&task, "containers", case);
                present_count(containers.first()?, "exitCode", case)
            },
            &format!("case {case:?}: the branching program's exitCode"),
        )
        .await;
        assert_eq!(
            exit_code, row.expect_exit,
            "case {case:?}: expected the branching program to exit {}, got {exit_code}",
            row.expect_exit
        );
    }
}
