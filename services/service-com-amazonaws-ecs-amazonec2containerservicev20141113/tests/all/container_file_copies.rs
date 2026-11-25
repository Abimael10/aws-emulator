// SPDX-License-Identifier: CC0-1.0

//! The files a container's program COPIES and MOVES, driven end to end through `RunTask`,
//! `DescribeTasks` and the stream the `awslogs` driver delivers to.
//!
//! RULE: a container's program is the shell program its launch names, and the session that
//! performs it performs every utility whose whole effect is inside that container — its
//! streams, its status and its OWN FILESYSTEM. Duplicating and renaming what that filesystem
//! holds are two such utilities: "the `cp` utility shall copy the contents of `source_file` to
//! the destination path named by `target_file`" (and, where the target is a directory, to
//! "`target`/`basename source_file`", with `-R`/`-r` copying "file hierarchies"), and "the `mv`
//! utility shall move the file named by the `source_file` operand to the destination specified
//! by the `target_file`" — so after a move the destination holds what the source held and the
//! source is gone. `Container$exitCode` ("the exit code returned from the container") is the
//! status the program that used them ended with, and the lines it wrote are the events of its
//! own log stream.
//!
//! Why it matters here: the ONE thing an official web-server image provides a container of it
//! is the directory its shipped configuration serves and the page inside it
//! (`state::container_servers::image_shipped_files`), and the documented way to serve that page
//! under another path is to COPY it there. A session that performs the `mkdir` and not the `cp`
//! performs no such program at all: the run claims no status, the server the program would have
//! started never runs, and nothing the container was launched to serve is served.
//!
//! The rows differ along what the rule generalises over: WHICH utility is used (`cp`, `mv`),
//! what it carries (one file, a file into a directory, a whole hierarchy with `-r`), WHERE the
//! bytes come from (the page the image shipped, a document the program wrote itself), and the
//! two failures a performed invocation reports in its own wording (a directory with no tree
//! asked for, a source the filesystem holds nothing at).
//!
//! Nothing here reaches into a state struct: the cluster, the revisions and the launches go
//! through the ECS front door, the execution role through IAM's own `CreateRole`, the group
//! through CloudWatch Logs' own `CreateLogGroup`, and every value asserted is read back through
//! `DescribeTasks` and `GetLogEvents`. Every wait polls for its CONDITION until a generous
//! deadline passes, so a loaded host looks again instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, present_count, seed_awsvpc_subnets, string, ACCOUNT,
    SIGNED_REGION,
};

/// The cluster every launch below runs in.
const CLUSTER: &str = "filecopies";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image every container starts from: the Apache HTTP Server's official image, which ships
/// the page below inside the root its configuration serves.
const IMAGE: &str = "public.ecr.aws/docker/library/httpd:2.4";

/// The directory that image's shipped configuration serves, and the page it ships inside it —
/// spelled out here rather than imported, so a wrong value in the implementation cannot make a
/// row pass. Every row that copies the image's own page reads it from that root.
const SHIPPED_PAGE: &str = "<html><body><h1>It works!</h1></body></html>";

/// The log group every row delivers to, and the `awslogs-stream-prefix` its configuration
/// names — so its stream is `<prefix>/<container>/<task id>`.
const LOG_GROUP: &str = "/emulator/ecs/file-copies";
const STREAM_PREFIX: &str = "copies";

/// The task execution role the revisions name, under which the `awslogs` driver creates the
/// stream and writes to it.
const EXECUTION_ROLE: &str = "fileCopyExecutionRole";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to do
/// it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How often a poll re-reads the emulator, and how long it keeps looking. The deadline is
/// generous on purpose: it is only here so a move that never happens fails the test instead of
/// hanging it, and nothing below asserts on how long anything took.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = Duration::from_secs(240);

/// One row: the program a container runs, the lines it delivers and the status it ends with.
struct Row {
    case: &'static str,
    /// The program the container's `command` hands the shell.
    program: &'static str,
    /// The events its own log stream is to hold, in order.
    lines: &'static [&'static str],
    /// The status the program ends with.
    exit_code: i64,
}

const ROWS: &[Row] = &[
    Row {
        // The recorded shape: the page the IMAGE shipped, put where the service serves it from.
        case: "the page the image shipped, copied to the path the container serves it at",
        program: "mkdir -p /usr/local/apache2/htdocs/svc && \
                  cp /usr/local/apache2/htdocs/index.html \
                  /usr/local/apache2/htdocs/svc/index.html && \
                  cat /usr/local/apache2/htdocs/svc/index.html && exit 31",
        lines: &[SHIPPED_PAGE],
        exit_code: 31,
    },
    Row {
        // "the destination path named by target/basename source_file": the target is the
        // DIRECTORY, and the copy keeps the source's own name inside it.
        case: "a file copied INTO a directory, which keeps its own name there",
        program: "mkdir -p /usr/local/apache2/htdocs/into && \
                  cp /usr/local/apache2/htdocs/index.html /usr/local/apache2/htdocs/into && \
                  cat /usr/local/apache2/htdocs/into/index.html && exit 32",
        lines: &[SHIPPED_PAGE],
        exit_code: 32,
    },
    Row {
        case: "a whole hierarchy copied with the tree option, every file under it with it",
        program: "mkdir -p /usr/local/apache2/htdocs/deep && \
                  echo deeper > /usr/local/apache2/htdocs/deep/page.html && \
                  cp -r /usr/local/apache2/htdocs /site && \
                  cat /site/index.html /site/deep/page.html && exit 33",
        lines: &[SHIPPED_PAGE, "deeper"],
        exit_code: 33,
    },
    Row {
        // A move leaves the destination holding what the source held, and the source gone —
        // which is what the `if` below branches on.
        case: "a document the program wrote, moved to another path",
        program: "echo moved > /one.txt && mv /one.txt /two.txt && cat /two.txt; \
                  if test -e /one.txt; then exit 64; else exit 34; fi",
        lines: &["moved"],
        exit_code: 34,
    },
    Row {
        case: "a document moved INTO a directory",
        program: "mkdir -p /box && echo boxed > /item.txt && mv /item.txt /box && \
                  cat /box/item.txt && exit 35",
        lines: &["boxed"],
        exit_code: 35,
    },
    Row {
        // A directory with no tree asked for is "omitted" and the utility fails, so the `&&`
        // after it does not run and the program reports the status the utility answered.
        case: "a directory with no tree option, which the utility omits and reports",
        program: "cp /usr/local/apache2/htdocs /site && exit 65; exit $?",
        lines: &["cp: -r not specified; omitting directory '/usr/local/apache2/htdocs'"],
        exit_code: 1,
    },
    Row {
        case: "a source the container's filesystem holds nothing at",
        program: "cp /absent.html /there.html && exit 66; exit $?",
        lines: &["cp: cannot stat '/absent.html': No such file or directory"],
        exit_code: 1,
    },
];

/// A router carrying Amazon ECS, IAM — which holds the execution role Amazon ECS assumes before
/// it launches a task — and CloudWatch Logs, which a container's output is delivered to, over
/// ONE shared engine, with the subnets an `awsvpc` placement names in the account's Amazon VPC
/// records.
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

/// One CloudWatch Logs call, the way a client makes it.
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

/// Create the task execution role the revisions name, through IAM's OWN API, with a trust
/// policy that lets Amazon ECS assume it and the grant the `awslogs` driver's two calls need,
/// and answer its ARN.
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
/// shell and delivers its output through the `awslogs` driver.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    program: &str,
    execution_role: &str,
    case: &str,
) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": ["sh", "-c", program],
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

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it.
///
/// Waiting for the CONDITION rather than for a span: a standalone task is stopped once the
/// essential container it was placed to run has exited, which is what makes that container's
/// run certainly over.
async fn stopped_task(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let body = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [arn] }),
            case,
        )
        .await;
        let task = list(&body, "tasks", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"));
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

/// RULE: the session that performs a container's program performs the utilities that DUPLICATE
/// and RENAME what the container's own filesystem holds, so a program that copies the page its
/// image shipped — or moves a document it wrote itself — runs to its end, reports the status it
/// chose, and leaves the bytes where it put them.
#[tokio::test]
async fn a_program_that_copies_and_moves_its_files_runs_to_its_end() {
    let case = "file copies";
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

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel in
    // the one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("filecopies{index}");
        register(&router, &family, row.program, &execution_role, row.case).await;
        let arn = run_task(&router, &family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, arn, case).await;
        let container = list(&task, "containers", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"));
        let reported = present_count(&container, "exitCode", case);
        assert_eq!(
            reported,
            Some(row.exit_code),
            "case {case:?}: expected exitCode {:?}, got {reported:?} in {task}",
            row.exit_code
        );
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop of a task whose container ran and exited, got {task}"
        );
        let lines = delivered_lines(&router, arn, case).await;
        assert_eq!(
            lines,
            row.lines
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the lines the program wrote, got {lines:?}"
        );
    }
}
