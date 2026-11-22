// SPDX-License-Identifier: CC0-1.0

//! The container of an ALB-fronted `awsvpc` Amazon ECS service delivers its own standard
//! output to CloudWatch Logs WHILE it goes on serving: the load balancer answers with the
//! bytes that container wrote, and the same container's startup line is readable from the
//! stream its `awslogs` driver delivers to.
//!
//! RULE: the `awslogs` driver "sends container logs to Amazon CloudWatch Logs", so every
//! complete line a started container writes on its standard output is an event of the stream
//! its configuration names — one line at a time, as the container writes it — whether or not
//! the container's program has ended; a container that starts a server in the foreground never
//! ends, so a delivery that waited for the program to finish would deliver nothing for exactly
//! the containers a load-balanced service is made of.
//!
//! The grounds. `LogConfiguration$logDriver` names "the log driver to use for the container"
//! and the driver's `awslogs-stream-prefix` documentation fixes the stream's name as the
//! prefix, the container's name and the task's id, with a configuration naming no prefix
//! leaving the driver naming the stream after the container it delivers for.
//! `LoadBalancer$targetGroupArn` binds the service's tasks into a target group and "tasks that
//! use the `awsvpc` network mode are associated with an elastic network interface", so the
//! target is the task's own address and what answers there is what the container's own
//! `command` — "the command that's passed to the container" — started. The two facts stand on
//! ONE container: the same program writes the line and serves the document.
//!
//! Five crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! owns the service, its task, that task's interface and the container's program, Amazon EC2
//! owns the network and the security-group rule that lets a health check arrive, Elastic Load
//! Balancing v2 holds the registration and answers the client, CloudWatch Logs holds the group,
//! the stream and the events, and IAM holds the execution role the driver is initialised as.
//! Everything is driven through the real operations on ONE shared engine, so every assertion is
//! about what each service persisted.
//!
//! The rows differ along what the rule generalises over: which server the container's program
//! starts and under which invocation (the applet's own name, and the same applet through the
//! multi-call binary it ships as), the port it listens on, the directory it serves, the image
//! the container names, and whether the driver's configuration names a stream prefix at all —
//! which is what decides the stream's name.
//!
//! Every clock-owed move — the task's start, the registration it owes, the health the checks
//! determine, the driver's initialisation and the delivery of a line — is POLLED for until it
//! holds, never slept for, and each deadline is generous: a row asserts that the move HAPPENS,
//! not how fast this host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The fixture for "an Application Load Balancer in front of an `awsvpc` Amazon ECS service" is
// owned by the suite that first stood it up; this suite measures ANOTHER rule on that same
// topology, so it calls those helpers rather than keeping a second copy of them.
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    container_hostname, create_service, ecs_ok, front_door, get, healthy_target, seed_network,
    CLUSTER, CONTAINER,
};
// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix the CloudWatch Logs model names an operation under.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The IAM API version an awsQuery body carries.
const IAM_VERSION: &str = "2010-05-08";

/// The task execution role the Fargate launches below run under, and the trust policy that lets
/// Amazon ECS assume it: "the task execution role that grants the Amazon ECS container agent
/// permission to make Amazon Web Services API calls on your behalf"
/// (`TaskDefinition$executionRoleArn`), which is the principal the `awslogs` driver's own stream
/// creation and delivery are made as.
const EXECUTION_ROLE_NAME: &str = "ecsServedTaskExecutionRole";
const EXECUTION_ROLE_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ecs-tasks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The managed policy AWS publishes for exactly that role, which is how the console and AWS's
/// own templates grant a task execution role the `logs:CreateLogStream` / `logs:PutLogEvents`
/// the driver needs.
const EXECUTION_ROLE_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The HTTP status a served document is answered with.
const OK: u16 = 200;

/// `Task$lastStatus` of a task whose containers have started, and `Service$status` of a service
/// that is running.
const RUNNING: &str = "RUNNING";
const ACTIVE: &str = "ACTIVE";

/// How long a clock-owed move is waited for before a row gives up, and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// One row: the service a container of one image serves and logs from, and the names its own
/// resources take so two rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    family: &'static str,
    service_name: &'static str,
    group_name: &'static str,
    load_balancer_name: &'static str,
    log_group: &'static str,
    /// The `awslogs-stream-prefix` the configuration names, or `None` for a configuration that
    /// names none — which leaves the driver naming the stream after the container.
    stream_prefix: Option<&'static str>,
    image: &'static str,
    /// The port the container's server listens on, which the binding and the group forward to.
    port: i64,
    /// The directory the server serves, which the container's own program writes the document
    /// into.
    served_directory: &'static str,
    /// The invocation that starts the server, as the program's last command.
    start_server: &'static str,
    /// The text the body and the logged line are built from.
    marker: &'static str,
    /// The word the logged startup line carries after the marker.
    startup_word: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: the small server's own invocation, under a prefixed stream",
        family: "loggedserved",
        service_name: "loggedserved",
        group_name: "logged-served",
        load_balancer_name: "logged-served-lb",
        log_group: "/ecs/logged-served",
        stream_prefix: Some("ecs"),
        image: "public.ecr.aws/docker/library/busybox:latest",
        port: 80,
        served_directory: "/www",
        start_server: "httpd -f -p 80 -h /www",
        marker: "SERVED-LOGGED-A",
        startup_word: "httpd-start",
    },
    Row {
        case: "the same applet through its multi-call binary, on another port and no prefix",
        family: "loggedapplet",
        service_name: "loggedapplet",
        group_name: "logged-applet",
        load_balancer_name: "logged-applet-lb",
        log_group: "/ecs/logged-applet",
        stream_prefix: None,
        image: "public.ecr.aws/docker/library/httpd:2.4",
        port: 8080,
        served_directory: "/srv/site",
        start_server: "busybox httpd -f -p 8080 -h /srv/site",
        marker: "SERVED-LOGGED-B",
        startup_word: "server-start",
    },
];

/// The program one row's container runs: it writes the document the load balancer will serve —
/// the marker and the name the runtime gave this container — reports on its standard output that
/// it is starting the server, and then runs that server in the foreground, so the container goes
/// on running for as long as the service does.
fn program(row: &Row) -> String {
    format!(
        "mkdir -p {directory} && echo \"{marker} $(hostname)\" > {directory}/index.html && \
         echo \"{marker} {word}\" && {server}",
        directory = row.served_directory,
        marker = row.marker,
        word = row.startup_word,
        server = row.start_server,
    )
}

/// The startup line one row's container writes.
fn startup_line(row: &Row) -> String {
    format!("{} {}", row.marker, row.startup_word)
}

/// One emulator carrying the five services this suite exercises over ONE shared engine, exactly
/// as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
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

/// Call one IAM action, answered with its status and XML body.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Create the task execution role in IAM itself, grant it AWS's own managed policy for the
/// purpose, and answer the ARN IAM minted for it.
async fn execution_role(router: &MultiServiceRouter) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("AssumeRolePolicyDocument", EXECUTION_ROLE_TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, OK,
        "setup: expected {OK} from CreateRole, got {status} with body {created}"
    );
    let arn = element(&created, "Arn");
    let (status, attached) = iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE_NAME),
            ("PolicyArn", EXECUTION_ROLE_POLICY_ARN),
        ],
    )
    .await;
    assert_eq!(
        status, OK,
        "setup: expected {OK} from AttachRolePolicy, got {status} with body {attached}"
    );
    arn
}

/// Call one CloudWatch Logs operation, answered with its status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one CloudWatch Logs operation that must succeed, and answer its payload.
async fn logs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, payload) = logs(router, operation, request).await;
    assert_eq!(
        status, OK,
        "case {case:?}: expected {OK} from logs {operation}, got {status} with {payload}"
    );
    payload
}

/// Register one `awsvpc`, Fargate-compatible revision of this row's family: one essential
/// container of the row's image, listening on the row's port, running the row's program and
/// delivering its output through the `awslogs` driver as the execution role.
async fn register_logging_revision(
    router: &MultiServiceRouter,
    row: &Row,
    role_arn: &str,
) -> String {
    let mut options = json!({
        "awslogs-group": row.log_group,
        "awslogs-region": REGION,
    });
    if let Some(prefix) = row.stream_prefix {
        options["awslogs-stream-prefix"] = json!(prefix);
    }
    let registered = ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": row.image,
                "essential": true,
                "memory": 128,
                "portMappings": [{ "containerPort": row.port, "protocol": "tcp" }],
                "command": ["/bin/sh", "-c", program(row)],
                "logConfiguration": {
                    "logDriver": "awslogs",
                    "options": options,
                },
            }],
        }),
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: the registration must answer an ARN, got {registered}",
                row.case
            )
        })
        .to_string()
}

/// The last segment of an ARN, which is the id of the thing it names.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// One task of this row's service, as `DescribeTasks` reports it once it is RUNNING and the
/// service reports it as its one running task — looked at again until that holds, or the
/// deadline passes.
async fn running_task(router: &MultiServiceRouter, row: &Row) -> Value {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    loop {
        let described = ecs_ok(
            router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [row.service_name] }),
        )
        .await;
        let service = &described["services"][0];
        let steady = service["status"].as_str() == Some(ACTIVE)
            && service["runningCount"].as_i64() == Some(1)
            && service["desiredCount"].as_i64() == Some(1);
        if steady {
            let listed = ecs_ok(
                router,
                "ListTasks",
                json!({
                    "cluster": CLUSTER,
                    "serviceName": row.service_name,
                    "desiredStatus": RUNNING,
                }),
            )
            .await;
            let arns: Vec<&str> = listed["taskArns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if !arns.is_empty() {
                let tasks = ecs_ok(
                    router,
                    "DescribeTasks",
                    json!({ "cluster": CLUSTER, "tasks": arns }),
                )
                .await;
                let task = tasks["tasks"][0].clone();
                if task["lastStatus"].as_str() == Some(RUNNING) {
                    return task;
                }
                last = task;
            }
        } else {
            last = described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the service {ACTIVE} with one {RUNNING} task and \
             runningCount == desiredCount == 1, last read was {last}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The private IPv4 address of the interface `task` was allocated, read off its attachments —
/// the address an `awsvpc` task is reached at, and so the one its registration names.
fn task_address(case: &str, task: &Value) -> String {
    task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
        .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
        .and_then(|detail| detail["value"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {case:?}: the task carries no interface address: {task}"))
}

/// The stream this row's container delivers to: the prefix, the container's name and the task's
/// id, and the container's own id for a configuration naming no prefix.
fn expected_stream(row: &Row, task: &Value) -> String {
    let case = row.case;
    match row.stream_prefix {
        Some(prefix) => {
            let task_arn = task["taskArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the task carries no ARN: {task}"));
            format!("{prefix}/{CONTAINER}/{}", id_of(task_arn))
        }
        None => {
            let container_arn = task["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|container| container["name"].as_str() == Some(CONTAINER))
                .and_then(|container| container["containerArn"].as_str())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the task holds no container {CONTAINER:?}: {task}")
                });
            id_of(container_arn).to_string()
        }
    }
}

/// The messages `GetLogEvents` reports in one stream, oldest first.
async fn stream_messages(router: &MultiServiceRouter, group: &str, stream: &str) -> Vec<String> {
    let (status, read) = logs(
        router,
        "GetLogEvents",
        &json!({ "logGroupName": group, "logStreamName": stream, "startFromHead": true }),
    )
    .await;
    if status != OK {
        // The stream the driver owes may not be there yet, which is the poll's own business.
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

/// Wait until the stream `stream` of `group` carries a message holding `line`, and answer every
/// message it then holds.
///
/// Polls the CONDITION — the driver's initialisation and the delivery of a line the container
/// wrote are both clock-owed — rather than waiting a fixed time.
async fn delivered_lines(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
    line: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let messages = stream_messages(router, group, stream).await;
        if messages.iter().any(|message| message.contains(line)) {
            return messages;
        }
        if Instant::now() >= deadline {
            let streams = logs_ok(
                router,
                case,
                "DescribeLogStreams",
                &json!({ "logGroupName": group }),
            )
            .await;
            panic!(
                "case {case:?}: expected the stream {stream:?} of {group:?} to carry a line \
                 holding {line:?}, got {messages:?}; the group reports {streams}"
            );
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_serving_containers_own_output_reaches_its_awslogs_stream() {
    let router = emulator();
    let network = seed_network(&router).await;
    let role_arn = execution_role(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    for row in ROWS {
        let case = row.case;
        logs_ok(
            &router,
            case,
            "CreateLogGroup",
            &json!({ "logGroupName": row.log_group }),
        )
        .await;
        let revision = register_logging_revision(&router, row, &role_arn).await;
        let (group, dns_name) = front_door(
            &router,
            &network,
            case,
            row.group_name,
            row.load_balancer_name,
            row.port,
        )
        .await;
        create_service(
            &router,
            &network,
            row.service_name,
            &revision,
            &group,
            row.port,
        )
        .await;

        // The registration and its health come first, because a load balancer forwards only to
        // a target its own checks found healthy.
        let registered = healthy_target(&router, case, &group, row.service_name).await;
        let task = running_task(&router, row).await;
        let address = task_address(case, &task);
        assert_eq!(
            address, registered,
            "case {case:?}: the registration must name the task's own interface address, got \
             {registered:?} against the task's {address:?}"
        );

        // What the load balancer answers is the document that container's program wrote: the
        // marker and the name the runtime gave the container, which is the private DNS hostname
        // of the very address the registration names.
        let answered = get(&router, &dns_name, "/").await;
        assert_eq!(
            answered.status, OK,
            "case {case:?}: expected HTTP {OK} through the load balancer, got {} with body {:?}",
            answered.status, answered.body
        );
        let fields: Vec<&str> = answered.body.split_whitespace().collect();
        assert_eq!(
            fields,
            vec![row.marker, container_hostname(&registered).as_str()],
            "case {case:?}: expected the container's own two-field body, got {:?}",
            answered.body
        );

        // And the same container's standard output is readable from the stream its driver
        // delivers to, WHILE it goes on serving: the program never ended.
        let stream = expected_stream(row, &task);
        let wanted = startup_line(row);
        let delivered = delivered_lines(&router, case, row.log_group, &stream, &wanted).await;
        assert!(
            delivered.iter().any(|message| message.contains(&wanted)),
            "case {case:?}: expected a line holding {wanted:?} in {stream:?}, got {delivered:?}"
        );

        // The container is still the one serving, which is what makes the delivery above a
        // delivery from a RUNNING container rather than from one that had ended.
        let still = get(&router, &dns_name, "/").await;
        assert_eq!(
            (still.status, still.body.trim().to_string()),
            (
                OK,
                format!("{} {}", row.marker, container_hostname(&registered))
            ),
            "case {case:?}: the container must still be serving after its line was read, got {} \
             with {:?}",
            still.status,
            still.body
        );
    }
}
