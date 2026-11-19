// SPDX-License-Identifier: CC0-1.0

//! A container whose program starts a SERVER delivers that server's own startup output to the
//! log group its `awslogs` driver names.
//!
//! RULE: a container definition's `logConfiguration` names "the log driver to use for the
//! container" and the `awslogs` driver "sends container logs to Amazon CloudWatch Logs", so
//! every line the container's program writes becomes one event of the stream that driver
//! created — and a program that starts a server in the foreground writes what that server
//! writes as it begins serving, before any request is made to it. What is delivered is each
//! server's OWN output: the nginx master logs the notice it writes once its workers are up to
//! its error log, which that image routes to the container's standard error, and the
//! interpreter's own HTTP server module prints the address and port it is serving on. A server
//! whose startup output this emulator does not hold writes nothing, and a container that starts
//! no server at all has only the lines its own program wrote: no event is invented for either.
//!
//! The grounds. `TaskDefinition$containerDefinitions` carries the `command` — "the command
//! that's passed to the container" — and a web server image's documented command is an
//! invocation of its server in the foreground, which is what makes such a task's container the
//! thing that answers on `LoadBalancer$containerPort`. A client's only evidence that the
//! container behind such a task is really serving rather than merely reported `RUNNING` is what
//! that stream holds, which is why the stream of a running web-server task is not empty.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! owns the task, the interface its `awsvpc` placement is allocated and the container's
//! program, CloudWatch Logs owns the group, the streams and the events, and IAM holds the
//! execution role the driver is initialised as — over the network Amazon EC2 owns. Everything
//! is driven through the real operations on ONE engine, so every assertion is about what each
//! service persisted; nothing reaches into a state struct.
//!
//! The rows differ along what the rule generalises over: which server the command starts (the
//! `nginx` image's own command, the same server behind a program that writes its page first,
//! the Apache HTTP Server image's foreground wrapper, the interpreter's HTTP server module),
//! whether the invocation names the port that module listens on, whether the program writes
//! lines of its own beside the server's, and a program that starts no server at all.
//!
//! Every clock-owed move — the interface the task is allocated, the task's own start, the
//! driver's stream and the delivery into it — is POLLED for until it holds, never slept for,
//! and the deadline is only here so a failure is a failure rather than a hang: nothing asserts
//! how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefixes the two awsJson services name an operation under.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The API versions the two Query services' bodies carry.
const EC2_VERSION: &str = "2016-11-15";
const IAM_VERSION: &str = "2010-05-08";

/// The cluster every task below runs in, and the one group every driver delivers to: a stream
/// name carries the task's own id, so one group holds one stream per row without collision.
const CLUSTER: &str = "server-startup";
const LOG_GROUP: &str = "/ecs/server-startup";

/// The one container of every revision below, and the port it declares.
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// The image every container names. Nothing is asserted about it: what runs is the `command`
/// the definition carries.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:alpine";

/// The task execution role the Fargate launches below run under, and the trust policy that lets
/// Amazon ECS assume it — "the task execution role that grants the Amazon ECS container agent
/// permission to make Amazon Web Services API calls on your behalf"
/// (`TaskDefinition$executionRoleArn`), which is the principal the `awslogs` driver's own stream
/// creation and delivery are made as.
const EXECUTION_ROLE_NAME: &str = "ecsTaskExecutionRole";
const EXECUTION_ROLE_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ecs-tasks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The managed policy AWS publishes for exactly that role, which is how the console and AWS's
/// own templates grant it the `logs:CreateLogStream` / `logs:PutLogEvents` the driver needs.
const EXECUTION_ROLE_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// `Task$lastStatus` of a task whose containers have started.
const RUNNING: &str = "RUNNING";

/// How long a row waits for the clock-owed moves it depends on, and how often it looks again.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// What the nginx master's startup notice carries after the instant it was logged at: the level
/// its error log spells, the process that wrote it with its one thread, and the notice itself —
/// `<instant> [notice] <pid>#<tid>: start worker processes`, the form every line of that log
/// takes. Spelled out here rather than imported, so a wrong notice in the implementation cannot
/// make this suite pass.
const NGINX_NOTICE_TAIL: &str = "[notice] 1#1: start worker processes";

/// How long the instant at the head of such a line is: `2026/11/08 19:18:14`, the calendar date
/// and the time of day that log spells, separated by one space.
const NGINX_INSTANT_CHARS: usize = 19;

/// What one row expects one event of its container's stream to be.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// A line the program itself wrote, verbatim.
    Line(&'static str),
    /// The notice the nginx master logs once its workers are up, whose instant is this run's
    /// own and so is read by its SHAPE ([`is_nginx_notice`]).
    NginxStartupNotice,
}

/// One row: the program the container runs, where its output is delivered, and the events that
/// stream must then hold, in order.
struct Row {
    case: &'static str,
    command: &'static [&'static str],
    /// The `awslogs-stream-prefix` this row's configuration names, which is the first segment
    /// of the stream its container delivers to.
    prefix: &'static str,
    family: &'static str,
    expected: &'static [Expected],
}

const ROWS: &[Row] = &[
    Row {
        case: "the nginx image's own command",
        command: &["nginx", "-g", "daemon off;"],
        prefix: "nginx-direct",
        family: "server-startup-nginx-direct",
        expected: &[Expected::NginxStartupNotice],
    },
    Row {
        // The shape the recorded workloads take: a line that WRITES the page the server will
        // serve — into the container's own layer, so it is no event of the log — and then the
        // server the image ships.
        case: "a program that writes its page and then runs that server",
        command: &[
            "sh",
            "-c",
            "echo marker > /usr/share/nginx/html/index.html && nginx -g 'daemon off;'",
        ],
        prefix: "nginx-page",
        family: "server-startup-nginx-page",
        expected: &[Expected::NginxStartupNotice],
    },
    Row {
        case: "a program that writes a line of its own before the server it starts",
        command: &["sh", "-c", "echo booting; nginx -g 'daemon off;'"],
        prefix: "nginx-booting",
        family: "server-startup-nginx-booting",
        expected: &[Expected::Line("booting"), Expected::NginxStartupNotice],
    },
    Row {
        // The module PRINTS the address and the port it serves on, so a row naming a port of
        // its own is answered with that port rather than with the module's default.
        case: "the interpreter's HTTP server module, on the port the invocation names",
        command: &[
            "python3",
            "-m",
            "http.server",
            "8080",
            "--directory",
            "/usr/share/nginx/html",
        ],
        prefix: "module-port",
        family: "server-startup-module-port",
        expected: &[Expected::Line(
            "Serving HTTP on 0.0.0.0 port 8080 (http://0.0.0.0:8080/) ...",
        )],
    },
    Row {
        case: "the same module with no port of its own, which serves its documented default",
        command: &["python3", "-m", "http.server"],
        prefix: "module-default",
        family: "server-startup-module-default",
        expected: &[Expected::Line(
            "Serving HTTP on 0.0.0.0 port 8000 (http://0.0.0.0:8000/) ...",
        )],
    },
    Row {
        // A server of the catalog whose startup output this emulator does not hold: the
        // container's own line is delivered and NOTHING is invented for the server.
        case: "a server whose startup output this emulator does not hold",
        command: &["sh", "-c", "echo starting; httpd-foreground"],
        prefix: "httpd",
        family: "server-startup-httpd",
        expected: &[Expected::Line("starting")],
    },
    Row {
        // And a program that starts no server at all: its own line, and nothing else.
        case: "a program that starts no server at all",
        command: &["sh", "-c", "echo up; sleep 300"],
        prefix: "no-server",
        family: "server-startup-no-server",
        expected: &[Expected::Line("up")],
    },
];

/// Whether `message` is the notice the nginx master logs once its workers are up: an instant of
/// the form its error log spells, then [`NGINX_NOTICE_TAIL`].
///
/// The INSTANT is this run's own — the moment the server started — so it is read by its shape
/// rather than compared against a value: the calendar date and the time of day, each field a
/// run of digits between that log's own separators.
fn is_nginx_notice(message: &str) -> bool {
    let Some((instant, tail)) = message.split_once(' ').and_then(|(date, rest)| {
        rest.split_once(' ')
            .map(|(time, tail)| (format!("{date} {time}"), tail))
    }) else {
        return false;
    };
    if instant.chars().count() != NGINX_INSTANT_CHARS {
        return false;
    }
    let shaped = instant
        .chars()
        .enumerate()
        .all(|(index, character)| match index {
            4 | 7 => character == '/',
            10 => character == ' ',
            13 | 16 => character == ':',
            _ => character.is_ascii_digit(),
        });
    shaped && tail == NGINX_NOTICE_TAIL
}

/// Whether `message` is what `expected` describes.
fn matches(expected: Expected, message: &str) -> bool {
    match expected {
        Expected::Line(line) => message == line,
        Expected::NginxStartupNotice => is_nginx_notice(message),
    }
}

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation on a service whose model declares a Query protocol, answered with its XML
/// body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one operation on a service whose model declares awsJson1_1.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one Amazon ECS operation that must succeed, and answer its payload.
async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, payload) = json_call(router, "ecs", ECS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {payload}"
    );
    payload
}

/// Call one CloudWatch Logs operation that must succeed, and answer its payload.
async fn logs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, payload) = json_call(router, "logs", LOGS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from logs {operation}, got {status} with {payload}"
    );
    payload
}

/// The account's own default subnets, read out of Amazon EC2 rather than invented — the network
/// an `awsvpc` placement of this account resolves against.
async fn default_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(
        router,
        "ec2",
        EC2_VERSION,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from DescribeSubnets, got {status} with body {body}"
    );
    let subnets = elements(&body, "subnetId");
    assert!(
        !subnets.is_empty(),
        "setup: the account must hold default subnets, got {subnets:?}"
    );
    subnets
}

/// Create the task execution role the launches run under, with AWS's own managed policy for it
/// attached, and answer the ARN IAM itself minted for it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let (status, created) = query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("AssumeRolePolicyDocument", EXECUTION_ROLE_TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from CreateRole, got {status} with body {created}"
    );
    let arn = element(&created, "Arn");
    let (status, attached) = query(
        router,
        "iam",
        IAM_VERSION,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("PolicyArn", EXECUTION_ROLE_POLICY_ARN),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from AttachRolePolicy, got {status} with body {attached}"
    );
    arn
}

/// Register one row's revision and launch one standalone task of it, answering that task's ARN.
async fn launch(
    router: &MultiServiceRouter,
    row: &Row,
    role_arn: &str,
    subnets: &[String],
) -> String {
    let case = row.case;
    ecs_ok(
        router,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "command": row.command,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": {
                        "awslogs-group": LOG_GROUP,
                        "awslogs-region": REGION,
                        "awslogs-stream-prefix": row.prefix,
                    },
                },
            }],
        }),
    )
    .await;
    let launched = ecs_ok(
        router,
        case,
        "RunTask",
        &json!({
            "cluster": CLUSTER,
            "taskDefinition": row.family,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": subnets,
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    launched["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: RunTask reported no task: {launched}"))
        .to_string()
}

/// Read one task again until it reports `RUNNING`, so what is asserted afterwards is what a
/// started container delivered rather than a reading taken before anything of it ran.
async fn running_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs_ok(
            router,
            case,
            "DescribeTasks",
            &json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        )
        .await;
        let task = described["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some(RUNNING) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never reached {RUNNING}, last describe was {described}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The messages CloudWatch Logs holds in `stream`, oldest first. A stream the group does not
/// hold yet reads as no messages, which is also what a stream nothing was delivered to holds.
async fn stream_messages(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (status, read) = json_call(
        router,
        "logs",
        LOGS_TARGET_PREFIX,
        "GetLogEvents",
        &json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    read["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str())
        .map(str::to_string)
        .collect()
}

/// The messages of one stream once it holds as many as the row expects — looked at again until
/// it does, or the deadline passes: what a container writes is delivered as it writes it.
async fn settled_messages(router: &MultiServiceRouter, row: &Row, stream: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let messages = stream_messages(router, stream).await;
        if messages.len() >= row.expected.len() || Instant::now() >= deadline {
            return messages;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The stream one row's container delivers to: `<prefix>/<container name>/<task id>`, the naming
/// the driver's own `awslogs-stream-prefix` documentation states. Spelled out here rather than
/// imported, so a wrong naming in the implementation cannot make this suite pass.
fn stream_of(row: &Row, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or(task_arn);
    format!("{}/{CONTAINER}/{task_id}", row.prefix)
}

#[tokio::test]
async fn a_container_delivers_the_startup_output_of_the_server_its_program_starts() {
    let router = build_router();
    let subnets = default_subnets(&router).await;
    let role_arn = execution_role(&router).await;
    ecs_ok(
        &router,
        "the cluster every row's task runs in",
        "CreateCluster",
        &json!({ "clusterName": CLUSTER }),
    )
    .await;
    logs_ok(
        &router,
        "the group every row's driver delivers to",
        "CreateLogGroup",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;

    // Every row's task is launched up front, so the clock-owed work of all of them falls due at
    // once rather than one row's wait after another's.
    let mut launched: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        let task_arn = launch(&router, row, &role_arn, &subnets).await;
        launched.push((row, task_arn));
    }

    for (row, task_arn) in &launched {
        let case = row.case;
        let task = running_task(&router, case, task_arn).await;
        assert_eq!(
            task["lastStatus"].as_str(),
            Some(RUNNING),
            "case {case:?}: the task whose container wrote must be running, got {task}"
        );
        let stream = stream_of(row, task_arn);
        let messages = settled_messages(&router, row, &stream).await;
        assert_eq!(
            messages.len(),
            row.expected.len(),
            "case {case:?}: expected {} event(s) in {stream}, got {messages:?}",
            row.expected.len()
        );
        for (expected, message) in row.expected.iter().zip(&messages) {
            assert!(
                matches(*expected, message),
                "case {case:?}: expected {expected:?} in {stream}, got {message:?} among \
                 {messages:?}"
            );
        }
    }
}
