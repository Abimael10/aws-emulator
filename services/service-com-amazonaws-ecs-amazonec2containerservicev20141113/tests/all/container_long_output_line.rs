// SPDX-License-Identifier: CC0-1.0

//! A container output LINE longer than ONE `awslogs` log event, driven end to end over HTTP.
//!
//! The rule under test, over a table of programs rather than one:
//!
//! > The `awslogs` driver accounts every event it builds against the per-event ceiling
//! > CloudWatch Logs states its event quota at, each event costing its UTF-8 message plus the
//! > documented 26 bytes of per-event overhead, so a line LONGER than that ceiling leaves is
//! > SPLIT into consecutive events of the whole stream whose concatenation is the line again —
//! > and the container that wrote it runs to its end and reports the status its program exited
//! > with, because a line past the ceiling is a line a container legitimately writes rather
//! > than one the run is given up on.
//!
//! The rows span what the rule generalises over: HOW the program builds a line that long — a
//! value doubled in a variable, a word of the program's own text, a value handed to it in its
//! environment, a line appended to a character at a time (so the writes are many and no single
//! one is long), and a vector the image exec's with the line as one argument — and the
//! LENGTHS: under the ceiling (one event), just past it (two), and several times it (several).
//!
//! CloudWatch Logs is mounted BESIDE Amazon ECS on the SAME engine, so what the driver
//! delivered is read back with that service's own `GetLogEvents`: nothing here reads the
//! producer's own buffer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, logs, ok, seed_awsvpc_subnets, string, SIGNED_REGION,
};

/// The log group every container below delivers to, created through CloudWatch Logs' own API
/// before any task is launched — the way a client sets one up.
const LOG_GROUP: &str = "/emulator/ecs/long-lines";

/// The `awslogs-stream-prefix` the task definitions below configure, and so the first segment
/// of every stream name they deliver to.
const STREAM_PREFIX: &str = "big";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The longest MESSAGE one delivered event carries, spelled out here rather than imported so a
/// wrong ceiling in the implementation cannot make a test pass: CloudWatch Logs states its log
/// event quota at 256 KiB ("Event size: 256 KB maximum") and `PutLogEvents` accounts a batch
/// as "the sum of all event messages in UTF-8, plus 26 bytes for each log event", so the
/// longest message one event may carry is 262144 - 26.
const MAX_EVENT_MESSAGE_BYTES: usize = 262_144 - 26;

/// How often a poll re-reads the emulator.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up. Generous: a container that assembles a
/// line of a few hundred thousand characters one character at a time performs a few hundred
/// thousand commands, and the budget has to hold on a loaded machine as well as an idle one.
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// A router carrying Amazon ECS and CloudWatch Logs over ONE shared engine — the two services
/// a container's output travels through, mounted the way the production router mounts them.
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

/// The messages CloudWatch Logs holds in `stream`, oldest first — the events the container's
/// own output became. An unknown stream reads as no messages.
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
/// `awslogs-stream-prefix` documentation states. Spelled out here rather than imported.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// Register one revision of `family` whose single container runs `command` with `environment`
/// in its process's environment, delivering through the `awslogs` driver, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    command: &[String],
    environment: &[(String, String)],
    case: &str,
) -> String {
    let container = json!({
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
    });
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({ "family": family, "containerDefinitions": [container] }),
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
        "case {case:?}: expected DescribeTasks to describe one task, got {body}"
    );
    tasks[0].clone()
}

/// Read the task again until it has STOPPED — waiting for the CONDITION rather than for a
/// span, since the run of a container's program takes as long as the program takes.
async fn stopped_task(
    router: &MultiServiceRouter,
    cluster: &str,
    task_arn: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, cluster, task_arn, case).await;
        if task["lastStatus"].as_str() == Some("STOPPED") {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never stopped within {POLL_BUDGET:?}, its last status was \
             {:?}",
            task["lastStatus"].as_str()
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: a program that writes ONE line, and the line it writes.
struct Row {
    /// What this row is called in every assertion.
    case: &'static str,
    /// The `command` its container definition states.
    command: Vec<String>,
    /// The `environment` its container definition states.
    environment: Vec<(String, String)>,
    /// The line the program writes, which the delivered events must reassemble.
    line: String,
}

/// `command`'s words as the owned strings a row holds.
fn words(command: &[&str]) -> Vec<String> {
    command.iter().map(|word| (*word).to_string()).collect()
}

/// The rows of the table: five ways of writing one long line, at three lengths around the
/// per-event ceiling.
fn rows() -> Vec<Row> {
    // A value doubled from ten characters until it passes the ceiling: 10 * 2^15, the first
    // such length, whose text is the ten digits repeated.
    let doubled = "0123456789".repeat(32_768);
    // Several times the ceiling, so the split is into more than two events.
    let several = "A".repeat(3 * MAX_EVENT_MESSAGE_BYTES + 7);
    // Just past the ceiling, so the split is into exactly two and the second is one byte.
    let just_past = "B".repeat(MAX_EVENT_MESSAGE_BYTES + 1);
    // Under the ceiling, so there is no split at all: the contrast row.
    let under = "C".repeat(MAX_EVENT_MESSAGE_BYTES - 1);
    // Appended a character at a time, which is the many-writes form.
    let appended = "x".repeat(MAX_EVENT_MESSAGE_BYTES + 100);
    vec![
        Row {
            case: "a value doubled in a variable until it passes the ceiling",
            command: words(&[
                "sh",
                "-c",
                "s=0123456789; while [ ${#s} -lt 262118 ]; do s=\"$s$s\"; done; \
                 printf '%s\\n' \"$s\"",
            ]),
            environment: Vec::new(),
            line: doubled,
        },
        Row {
            case: "a line several times the ceiling, as a word of the program's own text",
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("printf '%s\\n' '{several}'"),
            ],
            environment: Vec::new(),
            line: several,
        },
        Row {
            case: "a line one byte past the ceiling, handed to the program in its environment",
            command: words(&["sh", "-c", "printf '%s\\n' \"$LINE\""]),
            environment: vec![("LINE".to_string(), just_past.clone())],
            line: just_past,
        },
        Row {
            case: "a line one byte under the ceiling, which is delivered in one event",
            command: words(&["sh", "-c", "printf '%s\\n' \"$LINE\""]),
            environment: vec![("LINE".to_string(), under.clone())],
            line: under,
        },
        Row {
            case: "a line appended to one character at a time and ended at the end",
            command: words(&[
                "sh",
                "-c",
                "i=0; while [ $i -lt 262218 ]; do printf x; i=$((i+1)); done; echo",
            ]),
            environment: Vec::new(),
            line: appended,
        },
        Row {
            case: "a vector the image exec's with the line as one argument",
            command: vec!["echo".to_string(), "D".repeat(2 * MAX_EVENT_MESSAGE_BYTES)],
            environment: Vec::new(),
            line: "D".repeat(2 * MAX_EVENT_MESSAGE_BYTES),
        },
    ]
}

/// RULE: a container whose program writes a line longer than one log event runs to its end
/// and exits with its program's own status, and the `awslogs` driver delivers that line as
/// consecutive events of its stream, each at most the longest message one event carries, whose
/// concatenation is the line.
#[tokio::test]
async fn a_line_longer_than_one_log_event_is_split_into_events_that_reassemble_it() {
    let router = emulator();
    let cluster = "long-lines";
    cluster_with_capacity(&router, cluster, "long lines").await;
    let (status, body) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected CreateLogGroup to answer 200, got {status} with {body}"
    );

    let rows = rows();
    // Every row's task is launched FIRST and read afterwards, so the runs happen together in
    // the one emulator rather than one after another.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let family = format!("long-line-{index}");
        let definition = register(&router, &family, &row.command, &row.environment, row.case).await;
        launched.push((row, run_task(&router, cluster, &definition, row.case).await));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, cluster, task_arn, case).await;
        let containers = list(&task, "containers", case);
        let exit_code = containers
            .first()
            .and_then(|container| container["exitCode"].as_i64());
        assert_eq!(
            exit_code,
            Some(0),
            "case {case:?}: expected the container to exit 0, got {exit_code:?} in {:?}",
            containers.first().map(ToString::to_string)
        );

        let stream = stream_of(task_arn);
        let events = delivered(&router, &stream).await;
        let lengths: Vec<usize> = events.iter().map(String::len).collect();

        // Each event carries at most the longest message one event may carry, and a line
        // longer than that is split into as few events as that allows.
        let wanted = row.line.len().div_ceil(MAX_EVENT_MESSAGE_BYTES);
        assert_eq!(
            events.len(),
            wanted,
            "case {case:?}: expected the {}-byte line in {wanted} event(s), got {} with lengths \
             {lengths:?}",
            row.line.len(),
            events.len()
        );
        for (index, event) in events.iter().enumerate() {
            assert!(
                event.len() <= MAX_EVENT_MESSAGE_BYTES,
                "case {case:?}: event {index} carries {} bytes, past the {MAX_EVENT_MESSAGE_BYTES} \
                 one event may carry; lengths {lengths:?}",
                event.len()
            );
        }
        // The concatenation is the line again, byte for byte.
        let reassembled: String = events.concat();
        assert!(
            reassembled == row.line,
            "case {case:?}: the delivered events do not reassemble the {}-byte line: {} bytes \
             came back, in events of {lengths:?}",
            row.line.len(),
            reassembled.len()
        );
    }
}
