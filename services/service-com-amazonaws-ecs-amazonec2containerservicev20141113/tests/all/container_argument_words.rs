// SPDX-License-Identifier: CC0-1.0

//! A container ARGUMENT that is spelled `NAME=VALUE`, driven end to end over the wire.
//!
//! RULE under test: **where a `NAME=VALUE` word stands decides what it is. POSIX: "assignment
//! to a variable shall occur only in the command prefix", so such a word standing AFTER the
//! command name is one of that command's ARGUMENTS — `echo IDX=1` writes the line `IDX=1` and
//! sets no variable — while one standing BEFORE the command name, or standing alone as the
//! whole command, is an assignment, as are the operands of the utilities that take one
//! (`export NAME=VALUE`).** What the container wrote is then delivered to its `awslogs`
//! stream, one event per line (`LogConfiguration$logDriver` `awslogs` "sends container logs to
//! Amazon CloudWatch Logs"), which is where a client reads a container's output.
//!
//! Why it matters: `NAME=VALUE` is how a container program labels what it computed, so a word
//! read as an assignment instead of an argument silently turns the line the program wrote into
//! an empty one — the container exits 0 having delivered nothing.
//!
//! The rows differ along what the rule generalises over: WHERE the word stands (after the
//! command name, before it, alone, as a utility's operand), how many of them one command
//! carries, where the VALUE comes from (literal, a parameter expansion, an arithmetic
//! expansion, an unset parameter, quoted text), and the form of the name in front of the `=`
//! (a plain name, an option spelling no variable could carry).
//!
//! CloudWatch Logs is mounted BESIDE Amazon ECS on the SAME engine, so what is asserted is
//! what that service holds for the stream, read back through its own `GetLogEvents`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, present_count, seed_awsvpc_subnets, string, SIGNED_REGION};

/// The log group every container below delivers to.
const LOG_GROUP: &str = "/emulator/ecs/argument-words";

/// The `awslogs-stream-prefix` the task definitions below configure.
const STREAM_PREFIX: &str = "words";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from: one this emulator holds no image of, so what each row
/// observes is its container's own program.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// How often a poll looks again, and how long it keeps looking: far past the clock-owed
/// transitions a task takes to start and to stop, so a loaded machine delays a row instead of
/// breaking it. Only the loop's CONDITION ends a wait.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// Amazon ECS and CloudWatch Logs over ONE shared engine, holding the subnet an `awsvpc`
/// placement of this suite names.
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

/// One CloudWatch Logs call, the way a client makes it.
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

/// The messages CloudWatch Logs holds in `stream`, oldest first.
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
/// `<prefix>/<container name>/<task id>`, spelled out here rather than imported.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// What one row declares and what its container must then have written.
struct Row {
    case: &'static str,
    /// The shell program the container runs.
    program: &'static str,
    /// The variables its process is given.
    environment: &'static [(&'static str, &'static str)],
    /// The lines its `awslogs` stream must hold, in order.
    delivered: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a literal NAME=VALUE argument",
        program: "echo k=v",
        environment: &[],
        delivered: &["k=v"],
    },
    Row {
        case: "an argument whose value is a parameter expansion",
        program: "echo greeting=$GREETING",
        environment: &[("GREETING", "hello")],
        delivered: &["greeting=hello"],
    },
    Row {
        case: "an argument whose value is an arithmetic expansion",
        program: "echo total=$((6*7))",
        environment: &[],
        delivered: &["total=42"],
    },
    Row {
        case: "an argument whose value is an unset parameter",
        program: "echo empty=$NOTHING_SET_HERE",
        environment: &[],
        delivered: &["empty="],
    },
    Row {
        case: "an argument whose value is quoted",
        program: "echo \"quoted=two words\"",
        environment: &[],
        delivered: &["quoted=two words"],
    },
    Row {
        case: "several of them in one command",
        program: "echo a=1 b=2 c=3",
        environment: &[],
        delivered: &["a=1 b=2 c=3"],
    },
    Row {
        case: "an option spelling no variable name could carry",
        program: "echo --option=value",
        environment: &[],
        delivered: &["--option=value"],
    },
    Row {
        case: "the argument sets no variable",
        program: "echo x=9; echo read=$x",
        environment: &[],
        delivered: &["x=9", "read="],
    },
    Row {
        case: "a word standing alone IS an assignment",
        program: "v=5; echo v=$v",
        environment: &[],
        delivered: &["v=5"],
    },
    Row {
        case: "the operand of the utility that takes one IS an assignment",
        program: "export e=7; echo e=$e",
        environment: &[],
        delivered: &["e=7"],
    },
    Row {
        case: "a word before the command name IS an assignment, so it is no argument of it",
        program: "p=1 echo ran; echo after=$p",
        environment: &[],
        delivered: &["ran", "after="],
    },
];

/// RULE: a `NAME=VALUE` word after the command name is an ARGUMENT of that command, one
/// before it (or standing alone, or the operand of a utility that takes one) is an
/// assignment — and the line the command wrote is delivered to the container's `awslogs`
/// stream.
#[tokio::test]
async fn a_name_equals_value_argument_is_written_rather_than_assigned() {
    let router = emulator();
    let cluster = "argument-words";
    cluster_with_capacity(&router, cluster, "argument words").await;
    let (status, body) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateLogGroup answered {status} with {body}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let family = format!("argument-words-{index}");
        let definition = ok(
            &router,
            "RegisterTaskDefinition",
            json!({
                "family": &family,
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": IMAGE,
                    "memory": 128,
                    "command": ["sh", "-c", row.program],
                    "environment": row.environment
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
                }],
            }),
            case,
        )
        .await;
        let definition_arn = string(&definition["taskDefinition"], "taskDefinitionArn", case);
        let launched = ok(
            &router,
            "RunTask",
            json!({
                "cluster": cluster,
                "taskDefinition": definition_arn,
                "launchType": "EC2",
            }),
            case,
        )
        .await;
        let tasks = list(&launched, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected RunTask to place one task, got {launched}"
        );
        let task_arn = string(&tasks[0], "taskArn", case);
        let stream = stream_of(&task_arn);

        // The container's program ends on its own, with the status a program that wrote its
        // lines ends with: every row above runs to completion, so the exit status is zero.
        let deadline = Instant::now() + POLL_BUDGET;
        let exit_code = loop {
            let described = ok(
                &router,
                "DescribeTasks",
                json!({ "cluster": cluster, "tasks": [&task_arn] }),
                case,
            )
            .await;
            let described_tasks = list(&described, "tasks", case);
            let containers = list(&described_tasks[0], "containers", case);
            if let Some(code) = present_count(&containers[0], "exitCode", case) {
                break code;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the container never reported an exit status, last {described}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            exit_code, 0,
            "case {case:?}: expected the program to exit 0, got {exit_code}"
        );

        // What it WROTE, as the log service holds it: a wait for the CONDITION, because the
        // delivery of the last line and the exit status are two writes.
        let expected: Vec<String> = row.delivered.iter().map(|line| (*line).to_string()).collect();
        let deadline = Instant::now() + POLL_BUDGET;
        loop {
            let held = delivered(&router, &stream).await;
            if held == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {stream} to hold {expected:?}, got {held:?}"
            );
            ::tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
