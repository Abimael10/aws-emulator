// SPDX-License-Identifier: CC0-1.0

//! The counter a container's program PUBLISHES and the parameter it READS, driven end to end
//! through `RunTask` and read back through CloudWatch's own `GetMetricStatistics` and the stream
//! the `awslogs` driver delivers to.
//!
//! RULE: a container's program is the shell program its launch names, and the AWS CLI its session
//! provides really makes the call each command line names, against this account's own services
//! and signed as the task role — "the IAM role that grants containers in the task permission to
//! call Amazon Web Services APIs on your behalf" (`TaskDefinition$taskRoleArn`). So
//! `aws cloudwatch put-metric-data` ("publishes metric data to Amazon CloudWatch. CloudWatch
//! associates the data with the specified metric") leaves a datapoint of the metric the line
//! named, in EITHER of the forms that command takes its data in — the observation named inline
//! and the datum document — and `aws ssm get-parameter` ("gets information about a single
//! parameter") answers the value the account holds for it. A container that resolves no task role
//! holds no credential at all, so its CLI locates none and makes no call.
//!
//! Why it matters here: a workload whose whole behaviour lives inside a container — publishing
//! its counters, reading its feature flags — produces NOTHING observable while the control plane
//! reports a healthy task unless those command lines are really carried out.
//!
//! The rows differ along what the rule generalises over: WHICH form the publish names its
//! observations in (one inline, two in a document), how many of them one call carries, and
//! whether the task's containers hold a credential at all. The permission differential the same
//! rule decides — a task role withholding one action being refused exactly that call, with X-Ray
//! and IAM in it — is asserted in `core/router/tests/all/ecs_container_publishes_its_telemetry.rs`,
//! which spans more services than this crate's suite mounts.
//!
//! Nothing here reaches into a state struct: the cluster, the revisions and the launches go
//! through the ECS front door, the roles and their policies through IAM's own `CreateRole` and
//! `PutRolePolicy`, the parameter through Systems Manager's own `PutParameter`, and every value
//! asserted is read back through `DescribeTasks`, `GetLogEvents` and `GetMetricStatistics`. Every
//! wait polls for its CONDITION until a generous deadline passes, so a loaded host looks again
//! instead of failing.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, iam, list, ok, present_count, string, ACCOUNT, SIGNED_REGION,
};

/// The cluster every launch below runs in.
const CLUSTER: &str = "telemetry";

/// The one container of every revision below.
const CONTAINER: &str = "publisher";

/// The image every container starts from: the official AWS CLI image, which is what a workload
/// whose program is a sequence of `aws` commands is routinely built on. Its own entry point is
/// that program, so every revision states the `entryPoint` the shell program it runs needs.
const IMAGE: &str = "public.ecr.aws/aws-cli/aws-cli:2.36.44";

/// The log group every row delivers to, and the `awslogs-stream-prefix` its configuration names —
/// so its stream is `<prefix>/<container>/<task id>`.
const LOG_GROUP: &str = "/emulator/ecs/telemetry";
const STREAM_PREFIX: &str = "publisher";

/// The task execution role the revisions name, under which the `awslogs` driver creates the
/// stream and writes to it, and the task role whose session the programs' calls are signed with.
const EXECUTION_ROLE: &str = "telemetryExecutionRole";
const TASK_ROLE: &str = "telemetryTaskRole";

/// The service principal Amazon ECS assumes a task's roles as, and the action it performs to do
/// it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The parameter every row's program reads its configuration from, and the value the account
/// holds for it.
const PARAMETER: &str = "/flint/prod/quartz-latency-mode";
const PARAMETER_VALUE: &str = "degraded";

/// The metric every row's program publishes its counter as, and the dimension it carries.
const METRIC: &str = "TraceWrites";
const RESULT_DIMENSION: &str = "Result";

/// `Task$stopCode` of a task whose essential container exited.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// How often a poll re-reads the emulator, and how long it keeps looking. The deadline is
/// generous on purpose: it is only here so a publish that never happens fails the test instead of
/// hanging it, and nothing below asserts on how long anything took.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The period a metric read aggregates into, and the span it reads around the publish — both
/// generous, so a read is never a race with the clock.
const PERIOD_SECS: i64 = 60;
const WINDOW_SECS: i64 = 300;

/// One row: the publish a container's program makes, and the observations it therefore leaves.
struct Row {
    case: &'static str,
    /// The family its revision is registered under.
    family: &'static str,
    /// Whether the revision names the task role — a revision naming none has containers that hold
    /// no credential.
    names_task_role: bool,
    /// The namespace it publishes into, so each row's read is about its own task alone.
    namespace: &'static str,
    /// What the publish's arguments are, after `aws cloudwatch put-metric-data --namespace <ns>`.
    publish: &'static str,
    /// The observations the row leaves, as `(the Result dimension, how many)`.
    expected: &'static [(&'static str, f64)],
    /// What the program reports the publish's own status as.
    published: &'static str,
    /// Whether its CLI located a credential at all.
    calls: bool,
    /// The status the program ends with.
    exit_code: i64,
}

/// The two outcomes a program reports the publish under.
const PUBLISH_OK: &str = "OK";
const PUBLISH_FAILED: &str = "FAILED";

const ROWS: &[Row] = &[
    Row {
        case: "one observation named inline, in the command's shorthand form",
        family: "telemetryinline",
        names_task_role: true,
        namespace: "flint/inline",
        publish: "--metric-name TraceWrites --value 1 --unit Count --dimensions Result=ok",
        expected: &[("ok", 1.0)],
        published: PUBLISH_OK,
        calls: true,
        exit_code: 41,
    },
    Row {
        case: "two observations in one call, in the command's datum document form",
        family: "telemetrydocument",
        names_task_role: true,
        namespace: "flint/document",
        publish: "--metric-data \
                  '[{\"MetricName\":\"TraceWrites\",\"Value\":1,\"Unit\":\"Count\",\
                  \"Dimensions\":[{\"Name\":\"Result\",\"Value\":\"ok\"}]},\
                  {\"MetricName\":\"TraceWrites\",\"Value\":2,\"Unit\":\"Count\",\
                  \"Dimensions\":[{\"Name\":\"Result\",\"Value\":\"denied\"}]}]'",
        expected: &[("ok", 1.0), ("denied", 1.0)],
        published: PUBLISH_OK,
        calls: true,
        exit_code: 41,
    },
    Row {
        case: "a revision naming no task role, whose containers hold no credential",
        family: "telemetryroleless",
        names_task_role: false,
        namespace: "flint/roleless",
        publish: "--metric-name TraceWrites --value 1 --unit Count --dimensions Result=ok",
        expected: &[("ok", 0.0)],
        published: PUBLISH_FAILED,
        calls: false,
        exit_code: 41,
    },
];

/// A router carrying Amazon ECS and the services a container's program reaches through the CLI its
/// session provides, over ONE shared engine: IAM and the token service, which hold the task role
/// and mint the session its calls are signed with; CloudWatch, which the counter is published to;
/// Systems Manager, which holds the parameter; and CloudWatch Logs, which the container's output
/// is delivered to.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
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
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One `x-amz-json-1.1` call the way a client makes it, to the service `signing_name` claims under
/// the target prefix `prefix`, which must succeed.
async fn json_ok(
    router: &MultiServiceRouter,
    signing_name: &str,
    prefix: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{signing_name}.{SIGNED_REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /{signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
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
        "case {case:?}: expected {signing_name} {operation} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// One `x-amz-json-1.0` CloudWatch call, which must succeed.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /monitoring/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected CloudWatch {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Create one role through IAM's OWN API, with a trust policy that lets Amazon ECS assume it and
/// `actions` as its one inline policy, and answer its ARN.
async fn create_role(
    router: &MultiServiceRouter,
    name: &str,
    actions: &[&str],
    case: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": ECS_TASKS_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", document),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The program one row's container runs: it reads the parameter the account holds and reports what
/// it read, then publishes its counter and reports how that call went.
///
/// The read asks for the answer as text — "the output is formatted as multiple lines of
/// tab-separated string values" — so the value the account holds is among the words the command
/// wrote, which is what the case below looks for in the line the program echoed.
fn program(row: &Row) -> String {
    format!(
        "MODE=$(aws ssm get-parameter --name {PARAMETER} --output text); \
         echo \"MODE=$MODE\"; \
         if aws cloudwatch put-metric-data --namespace {namespace} {publish}; \
         then echo \"PUBLISHED={PUBLISH_OK}\"; else echo \"PUBLISHED={PUBLISH_FAILED}\"; fi; \
         exit {exit_code}",
        namespace = row.namespace,
        publish = row.publish,
        exit_code = row.exit_code,
    )
}

/// Register one revision of `row`'s family whose single essential container runs its program
/// through a shell and delivers its output through the `awslogs` driver.
///
/// `entryPoint` is stated because the image's own entry point is the CLI itself: "the entry point
/// that's passed to the container" (`ContainerDefinition$entryPoint`) is what the runtime exec's,
/// and a task definition carrying a script states the shell there exactly like this.
async fn register(router: &MultiServiceRouter, row: &Row, execution_role: &str, task_role: &str) {
    let mut definition = json!({
        "family": row.family,
        "executionRoleArn": execution_role,
        "containerDefinitions": [{
            "name": CONTAINER,
            "image": IMAGE,
            "memory": 128,
            "essential": true,
            "entryPoint": ["sh", "-c"],
            "command": [program(row)],
            "logConfiguration": {
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": LOG_GROUP,
                    "awslogs-region": SIGNED_REGION,
                    "awslogs-stream-prefix": STREAM_PREFIX,
                },
            },
        }],
    });
    if row.names_task_role {
        definition["taskRoleArn"] = json!(task_role);
    }
    ok(router, "RegisterTaskDefinition", definition, row.case).await;
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

/// Read one task again until its `lastStatus` is `STOPPED`, and answer it: a standalone task is
/// stopped once the essential container it was placed to run has exited, which is what makes that
/// container's program certainly over.
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
    let read = json_ok(
        router,
        "logs",
        "Logs_20140328",
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

/// The value a line `NAME=<value>` carries, and `None` when no line names it.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// How many observations the metric `METRIC` of `namespace` holds under `Result=<result>`, as a
/// client reads them: every datapoint's `SampleCount`, summed over the window around `at`.
async fn published_samples(
    router: &MultiServiceRouter,
    namespace: &str,
    result: &str,
    at: i64,
    case: &str,
) -> f64 {
    let read = cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": namespace,
            "MetricName": METRIC,
            "Dimensions": [{ "Name": RESULT_DIMENSION, "Value": result }],
            "StartTime": at - WINDOW_SECS,
            "EndTime": at + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["SampleCount", "Sum"],
        }),
        case,
    )
    .await;
    read["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["SampleCount"].as_f64())
        .sum()
}

/// RULE: the AWS CLI a container's session provides really makes the call each of its command
/// lines names — the parameter read and the metric publish, in either form that command takes its
/// data in — signed as the task role, so what the program published is readable from CloudWatch
/// itself and a container holding no credential publishes nothing.
#[tokio::test]
async fn a_containers_program_publishes_its_counter_and_reads_its_parameter() {
    let case = "container telemetry";
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, case).await;
    json_ok(
        &router,
        "logs",
        "Logs_20140328",
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
        case,
    )
    .await;
    json_ok(
        &router,
        "ssm",
        "AmazonSSM",
        "PutParameter",
        json!({ "Name": PARAMETER, "Value": PARAMETER_VALUE, "Type": "String" }),
        case,
    )
    .await;
    let execution_role = create_role(
        &router,
        EXECUTION_ROLE,
        &["logs:CreateLogStream", "logs:PutLogEvents"],
        case,
    )
    .await;
    let task_role = create_role(
        &router,
        TASK_ROLE,
        &["cloudwatch:PutMetricData", "ssm:GetParameter"],
        case,
    )
    .await;

    // The instant the reads below ask around, taken once so every row's window is the same.
    let at = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_secs(),
    )
    .expect("the epoch seconds fit");

    // Every row's task is launched FIRST and polled afterwards: the runs happen in parallel in the
    // one emulator, so the suite waits for the longest of them rather than for their sum.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        register(&router, row, &execution_role, &task_role).await;
        let arn = run_task(&router, row.family, row.case).await;
        launched.push((row, arn));
    }

    for (row, arn) in &launched {
        let case = row.case;
        let task = stopped_task(&router, arn, case).await;
        let container = list(&task, "containers", case)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected a container, got {task}"));
        let reported_code = present_count(&container, "exitCode", case);
        assert_eq!(
            reported_code,
            Some(row.exit_code),
            "case {case:?}: expected the program to run to its end and exit {:?}, got \
             {reported_code:?} in {task}",
            row.exit_code
        );
        assert_eq!(
            string(&task, "stopCode", case),
            ESSENTIAL_CONTAINER_EXITED,
            "case {case:?}: expected the stop of a task whose container ran and exited, got {task}"
        );

        // What the program READ, and how its publish went — both as the program itself reported
        // them on its own output. The read's answer carries the parameter's members as text, so
        // the value the account holds is among them; a container holding no credential made no
        // call at all and read nothing.
        let lines = delivered_lines(&router, arn, case).await;
        let reported_mode = reported(&lines, "MODE");
        match row.calls {
            true => assert!(
                reported_mode
                    .as_deref()
                    .is_some_and(|read| read.contains(PARAMETER_VALUE)),
                "case {case:?}: expected the program to report the parameter's value \
                 {PARAMETER_VALUE:?} among what the read answered, got {lines:?}"
            ),
            false => assert_eq!(
                reported_mode.as_deref(),
                Some(""),
                "case {case:?}: expected the program to report nothing read, got {lines:?}"
            ),
        }
        assert_eq!(
            reported(&lines, "PUBLISHED").as_deref(),
            Some(row.published),
            "case {case:?}: expected the program to report the publish as {:?}, got {lines:?}",
            row.published
        );

        // What it PUBLISHED, read back through CloudWatch's own reader.
        for (result, expected) in row.expected {
            let published = published_samples(&router, row.namespace, result, at, case).await;
            assert_eq!(
                published, *expected,
                "case {case:?}: expected {expected} observation(s) of {METRIC} under \
                 {RESULT_DIMENSION}={result:?} in {:?}, got {published}",
                row.namespace
            );
        }
    }
}
