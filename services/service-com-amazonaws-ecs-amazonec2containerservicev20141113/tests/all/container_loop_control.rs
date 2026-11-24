// SPDX-License-Identifier: CC0-1.0

//! The LOOP CONTROL and the output FORMATTING a container's program is written with, driven
//! end to end through `RunTask`, `DescribeTasks` and the stream the `awslogs` driver delivers
//! to.
//!
//! RULE: a container's program is the shell program its launch names, and the session that
//! performs it performs the commands of the forms it performs. The loops are performed
//! (`while`, `until`, `for`), so the two special built-ins that CONTROL a loop are too:
//! `break` "shall exit from the smallest enclosing for, while, or until loop" and `continue`
//! "shall continue with the next iteration of the smallest enclosing for, while, or until
//! loop", each over "the n-th enclosing loop" its optional count names, with "if n is greater
//! than the number of enclosing loops, the outermost enclosing loop shall be used". And the
//! utility that WRITES a formatted line is performed beside `echo`, because its whole effect
//! is the container's own output: "the printf utility shall write formatted operands to the
//! standard output". So a program written as a RETRY LOOP that leaves the loop on success and
//! reports what happened with a format runs to its end — `Container$exitCode` ("the exit code
//! returned from the container") is the status that program ended with, the lines it wrote are
//! the events of its own log stream, and the task reaches `STOPPED` because its essential
//! container exited.
//!
//! That shape is what every one-shot diagnostic task is written as ("try the call, report
//! OK and leave the loop, else try again"), so a session that performs the loop and not the
//! command that leaves it performs no such program at all: the run claims no status, the task
//! never stops, and nothing about the work is observable.
//!
//! The rows of the first table differ along what the rule generalises over: WHICH loop form is
//! being left (`while`, `until`, `for`), WHICH control leaves it (`break`, `continue`), how
//! many levels it leaves (none written, `1`, `2`, a count past the loops there are), where the
//! control stands (the body, an `if` branch, a `case` branch), and the forms that leave
//! nothing (a control with no enclosing loop at all, a count that is not a positive integer).
//! Each program ends with a DIFFERENT status, so no row can pass on another row's answer.
//!
//! The rows of the second table differ along the conversions and escapes one format carries,
//! how many arguments it is given, and a conversion the utility is not performing — and they
//! are read back as the EVENTS the container's stream holds, which is where a client reads
//! what a task reported.
//!
//! Nothing here reaches into a state struct: the cluster, the revisions and the launches go
//! through the ECS front door, the execution role through IAM's own `CreateRole`, the group
//! through CloudWatch Logs' own `CreateLogGroup`, and every value asserted is read back
//! through `DescribeTasks` and `GetLogEvents`. Every wait polls for its CONDITION until a
//! generous deadline passes, so a loaded host looks again instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, present_count, seed_awsvpc_subnets, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster every launch below runs in.
const CLUSTER: &str = "loopcontrol";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image the containers start from: nothing is fetched from it, what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The log group the formatting rows deliver to, and the `awslogs-stream-prefix` their
/// configuration names — so their stream is `<prefix>/<container>/<task id>`, spelled out
/// here rather than imported so a wrong naming in the implementation cannot make a test pass.
const LOG_GROUP: &str = "/emulator/ecs/loop-control";
const STREAM_PREFIX: &str = "report";

/// The task execution role the revisions that deliver output name — "the task execution role
/// that grants the Amazon ECS container agent permission to make Amazon Web Services API
/// calls on your behalf" (`TaskDefinition$executionRoleArn`), under which the `awslogs`
/// driver creates the stream and writes to it.
const EXECUTION_ROLE: &str = "loopControlExecutionRole";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to
/// do it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// `Task$stopCode` of a task whose essential container exited, spelled out rather than
/// imported for the same reason the stream naming is.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How often a poll re-reads the emulator, and how long it keeps looking. The deadline is
/// generous on purpose: it is only here so a move that never happens fails the test instead
/// of hanging it, and nothing below asserts on how long anything took.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// One row of the loop-control table: a shell program, and the status it ends with.
struct Program {
    case: &'static str,
    /// The program the container's `command` hands the shell.
    program: &'static str,
    /// The status that program chooses, which is the container's `exitCode`.
    exit_code: i64,
}

const LOOP_CONTROL: &[Program] = &[
    Program {
        // The shape of every one-shot probe: a retry loop left the moment the work succeeded.
        case: "a `while` retry loop left by `break` as soon as its attempt succeeded",
        program: "N=0; while [ $N -lt 9 ]; do N=$((N + 1)); if [ $N -eq 3 ]; then break; fi; \
                  done; exit $N",
        exit_code: 3,
    },
    Program {
        case: "a `for` loop left by `break` at the word it was looking for",
        program: "T=0; for W in a b c d; do T=$((T + 1)); [ \"$W\" = b ] && break; done; exit $T",
        exit_code: 2,
    },
    Program {
        case: "an `until` loop left by `break`",
        program: "N=0; until [ $N -gt 9 ]; do N=$((N + 1)); [ $N -eq 4 ] && break; done; exit $N",
        exit_code: 4,
    },
    Program {
        case: "a `while` loop that runs to its own end, with no control performed at all",
        program: "N=0; while [ $N -lt 5 ]; do N=$((N + 1)); done; exit $N",
        exit_code: 5,
    },
    Program {
        case: "`continue` skipping the rest of one iteration",
        program: "T=0; for W in a b c d; do [ \"$W\" = b ] && continue; T=$((T + 1)); done; \
                  exit $T",
        exit_code: 3,
    },
    Program {
        case: "`break` inside a `case` branch, which leaves the loop around it",
        program: "T=0; for W in a b c; do case \"$W\" in b) break;; esac; T=$((T + 1)); done; \
                  exit $T",
        exit_code: 1,
    },
    Program {
        case: "`break 1`, which leaves the inner loop and lets the outer one go on",
        program: "T=0; for A in 1 2; do for B in 1 2 3; do T=$((T + 1)); break 1; done; done; \
                  exit $T",
        exit_code: 2,
    },
    Program {
        case: "`break 2`, which leaves both loops at once",
        program: "T=0; for A in 1 2 3; do for B in 1 2 3; do T=$((T + 1)); break 2; done; \
                  T=$((T + 50)); done; exit $T",
        exit_code: 1,
    },
    Program {
        case: "`continue 2`, which goes on with the outer loop's next word",
        program: "T=0; for A in 1 2; do for B in 1 2 3; do T=$((T + 1)); continue 2; done; \
                  T=$((T + 50)); done; exit $T",
        exit_code: 2,
    },
    Program {
        // "If n is greater than the number of enclosing loops, the outermost enclosing loop
        // shall be used."
        case: "a count past the loops there are, which leaves the outermost one",
        program: "T=0; for A in 1 2 3; do T=$((T + 1)); break 9; done; exit $T",
        exit_code: 1,
    },
    Program {
        // A control performed where there is no loop leaves nothing and answers success, so
        // the `&&` after it runs.
        case: "`break` with no enclosing loop at all, which answers success",
        program: "break && exit 11; exit 61",
        exit_code: 11,
    },
    Program {
        case: "`continue` with no enclosing loop at all, which answers success",
        program: "continue && exit 12; exit 62",
        exit_code: 12,
    },
    Program {
        // A count that is not a positive integer is the command's own usage failure: it
        // leaves no loop, so the body goes on and the loop ends by its own list.
        case: "a count that is not a positive integer, which is the command's usage failure",
        program: "S=0; for A in 1; do break x; S=$?; done; [ $S -eq 1 ] && exit 13; exit 63",
        exit_code: 13,
    },
];

/// One row of the formatting table: what the program writes with, and the lines its container
/// delivers.
struct Report {
    case: &'static str,
    /// The program the container's `command` hands the shell.
    program: &'static str,
    /// The events its own log stream is to hold, in order.
    lines: &'static [&'static str],
    /// The status the program ends with.
    exit_code: i64,
}

const REPORTS: &[Report] = &[
    Report {
        // The recorded shape: a retry loop that reports the outcome with a format and leaves
        // the loop, which is what makes a one-shot task's result readable.
        case: "a retry loop that reports its outcome with a format and leaves the loop",
        program: "for A in 1 2 3; do if [ $A -eq 2 ]; then \
                  printf 'OUTCOME=%s ATTEMPT=%d\\n' OK $A; break; fi; done; exit 0",
        lines: &["OUTCOME=OK ATTEMPT=2"],
        exit_code: 0,
    },
    Report {
        case: "a format of one string conversion",
        program: "printf '%s\\n' ready; exit 21",
        lines: &["ready"],
        exit_code: 21,
    },
    Report {
        case: "a format carrying two conversions and literal text",
        program: "printf 'count=%d name=%s\\n' 7 widget; exit 22",
        lines: &["count=7 name=widget"],
        exit_code: 22,
    },
    Report {
        case: "a per-cent sign, which is written once",
        program: "printf 'done 100%%\\n'; exit 23",
        lines: &["done 100%"],
        exit_code: 23,
    },
    Report {
        case: "two line endings in one format, which are two events",
        program: "printf '%s\\n%s\\n' first second; exit 24",
        lines: &["first", "second"],
        exit_code: 24,
    },
    Report {
        // "%d" reads its argument as an integer, and a value that is not one is worth zero.
        case: "an integer conversion over a value that is no integer",
        program: "printf 'n=%d\\n' notanumber; exit 25",
        lines: &["n=0"],
        exit_code: 25,
    },
    Report {
        case: "a conversion with no argument left, which takes the empty string",
        program: "printf 'wrapped[%s]\\n'; exit 26",
        lines: &["wrapped[]"],
        exit_code: 26,
    },
    Report {
        case: "a tab escape between two conversions",
        program: "printf '%s\\t%s\\n' left right; exit 27",
        lines: &["left\tright"],
        exit_code: 27,
    },
    Report {
        case: "the value a format wrote, read back through a command substitution",
        program: "V=$(printf '%s-%d' part 4); [ \"$V\" = part-4 ] && exit 28; exit 68",
        lines: &[],
        exit_code: 28,
    },
    Report {
        // A conversion the utility is not performing writes NOTHING and answers its own
        // failure, with its diagnostic on standard error — which the `awslogs` driver
        // delivers beside standard output.
        case: "a conversion the utility does not perform, which writes nothing",
        program: "printf 'q=%q\\n' x; printf 'status=%d\\n' $?; exit 29",
        lines: &["printf: q=%q\\n: invalid format", "status=1"],
        exit_code: 29,
    },
];

/// A router carrying Amazon ECS, IAM — which holds the execution role Amazon ECS assumes
/// before it launches a task — and CloudWatch Logs, which a container's output is delivered
/// to, over ONE shared engine, with the subnets an `awsvpc` placement names in the account's
/// Amazon VPC records.
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One CloudWatch Logs call, the way a client makes it: `POST /` with
/// `X-Amz-Target: Logs_20140328.<operation>` and a credential scoped to `logs`, which is what
/// the router picks the service from.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
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
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected CloudWatch Logs {operation} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// Create the task execution role the delivering revisions name, through IAM's OWN API, with
/// a trust policy that lets Amazon ECS assume it and the grant the `awslogs` driver's two
/// calls need, and answer its ARN.
async fn create_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    })
    .to_string();
    crate::ecs_wire::iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    let delivery = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    crate::ecs_wire::iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "log-delivery".to_string()),
            ("PolicyDocument", delivery),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{EXECUTION_ROLE}")
}

/// Register one revision of `family` whose single essential container runs `program` through a
/// shell, delivering its output through the `awslogs` driver when `execution_role` is given.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    program: &str,
    execution_role: Option<&str>,
    case: &str,
) {
    let mut container = json!({
        "name": CONTAINER,
        "image": IMAGE,
        "memory": 128,
        "essential": true,
        "command": ["sh", "-c", program],
    });
    let mut request = json!({ "family": family });
    if let Some(role) = execution_role {
        container["logConfiguration"] = json!({
            "logDriver": "awslogs",
            "options": {
                "awslogs-group": LOG_GROUP,
                "awslogs-region": SIGNED_REGION,
                "awslogs-stream-prefix": STREAM_PREFIX,
            },
        });
        request["executionRoleArn"] = json!(role);
    }
    request["containerDefinitions"] = json!([container]);
    ok(router, "RegisterTaskDefinition", request, case).await;
}

/// `RunTask` one task of `family`, and answer its ARN.
async fn run_task(router: &MultiServiceRouter, family: &str, case: &str) -> String {
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

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
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

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
///
/// Waiting for the CONDITION rather than for a span: a standalone task is stopped once the
/// essential container it was placed to run has exited, which is what makes that container's
/// run certainly over.
async fn stopped_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe(router, arn, case).await;
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

/// The one container of a task's record.
fn container_of(task: &Value, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"))
}

/// The events the stream of the task `arn` holds, in order.
async fn delivered_lines(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<String> {
    let task_id = arn.rsplit('/').next().unwrap_or_default();
    let read = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}"),
            "startFromHead": true,
        }),
        case,
    )
    .await;
    read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// RULE: the session that performs a container's loops performs the commands that LEAVE one,
/// so a program written as a retry loop ends where its `break` or `continue` says and the
/// container reports the status that program chose.
#[tokio::test]
async fn a_program_that_leaves_its_loop_reports_the_status_it_chose() {
    let case = "loop control";
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, case).await;

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Program, String)> = Vec::new();
    for (index, row) in LOOP_CONTROL.iter().enumerate() {
        let family = format!("loopcontrol{index}");
        register(&router, &family, row.program, None, row.case).await;
        let arn = run_task(&router, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, arn, case).await;
        let reported = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            reported,
            Some(row.exit_code),
            "case {case:?}: expected exitCode {:?}, got {reported:?} in {task}",
            row.exit_code
        );
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop of a task whose container ran and exited, got \
             {task}"
        );
    }
}

/// RULE: the utility that writes a FORMATTED line is one of the commands this container
/// provides, so the lines a program reports with it are the events of its own log stream and
/// the program still ends with the status it chose.
#[tokio::test]
async fn a_formatted_report_reaches_the_containers_own_log_stream() {
    let case = "formatted reports";
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let execution_role = create_execution_role(&router, case).await;
    logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
        case,
    )
    .await;

    let mut launched: Vec<(&Report, String)> = Vec::new();
    for (index, row) in REPORTS.iter().enumerate() {
        let family = format!("report{index}");
        register(
            &router,
            &family,
            row.program,
            Some(&execution_role),
            row.case,
        )
        .await;
        let arn = run_task(&router, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, arn, case).await;
        let reported = present_count(&container_of(&task, case), "exitCode", case);
        assert_eq!(
            reported,
            Some(row.exit_code),
            "case {case:?}: expected exitCode {:?}, got {reported:?} in {task}",
            row.exit_code
        );
        let lines = delivered_lines(&router, arn, case).await;
        assert_eq!(
            lines,
            row.lines.iter().map(|line| (*line).to_string()).collect::<Vec<String>>(),
            "case {case:?}: expected the lines the program wrote, got {lines:?}"
        );
    }
}
