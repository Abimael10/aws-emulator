// SPDX-License-Identifier: CC0-1.0

//! Starting a container INITIALISES its `awslogs` log driver: the group the configuration
//! names holds one stream per started container, and the task behind a bound target group is
//! registered at the address of the interface it was allocated.
//!
//! RULE: a container definition's `logConfiguration` names "the log driver to use for the
//! container", and the `awslogs` driver "sends container logs to Amazon CloudWatch Logs" —
//! the driver is given its arguments and CREATES the stream when the container is created,
//! before anything of the container has written a line, so a started container has a stream
//! in the group its `awslogs-group` option names whatever its program then does. The stream's
//! name is the one the driver's own `awslogs-stream-prefix` documentation states — the
//! prefix, the container's name and the task's id — and a configuration naming no prefix
//! leaves the driver naming the stream after the container it delivers for. The EVENTS in a
//! stream are the container's own output: a container whose program this emulator runs
//! delivers the lines it wrote, and one whose image holds the program (no `entryPoint` and no
//! `command` anywhere) has its stream and writes nothing into it, because no program of the
//! container's ran to write anything.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here: Amazon
//! ECS owns the service, its tasks and the containers whose drivers are initialised,
//! CloudWatch Logs owns the group, the streams and the events, IAM holds the execution role
//! the driver is initialised as, and Elastic Load Balancing v2 holds the registration the
//! service's `loadBalancers` earn — over the network Amazon EC2 owns. Everything is driven
//! through the real operations on one engine, so what is asserted is what each service
//! persisted.
//!
//! The rows differ along what the rule generalises over: how many containers one task
//! declares a delivery for, whether a container's program is one this emulator runs or one
//! its image holds, whether the configuration names a stream prefix at all, and how the
//! execution role the driver is initialised as is granted the permission that call needs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefixes the two awsJson services name an operation under.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The Elastic Load Balancing v2 API version an awsQuery body carries, the ec2Query one, and
/// the IAM one.
const ELB_VERSION: &str = "2015-12-01";
const EC2_VERSION: &str = "2016-11-15";
const IAM_VERSION: &str = "2010-05-08";

/// The task execution roles the Fargate launches below run under, and the trust policy that
/// lets Amazon ECS assume either: "the task execution role that grants the Amazon ECS
/// container agent permission to make Amazon Web Services API calls on your behalf"
/// (`TaskDefinition$executionRoleArn`), which is the principal the `awslogs` driver's own
/// stream creation is made as.
///
/// There are two because the GRANT a row's role carries is one of the things the rule
/// generalises over: the managed policy AWS publishes for the purpose, and an inline policy
/// of the account's own scoped to the group the driver writes to.
const MANAGED_GRANT_ROLE_NAME: &str = "ecsTaskExecutionRole";
const INLINE_GRANT_ROLE_NAME: &str = "ecsTaskExecutionRoleInline";
const EXECUTION_ROLE_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ecs-tasks.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The managed policy AWS publishes for exactly this role, which is how the console and AWS's
/// own templates grant a task execution role the `logs:CreateLogStream` / `logs:PutLogEvents`
/// the `awslogs` driver is initialised with.
const EXECUTION_ROLE_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// How a row's execution role is granted that permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// AWS's own managed policy, attached to the role.
    ManagedPolicy,
    /// An inline policy of the account's own, scoped to the streams of the group the row's
    /// driver writes to.
    InlinePolicy,
}

/// How long a row waits for the clock-owed moves it depends on — the task's placement, its
/// interface, its start, the registration that start owes and the driver initialisation that
/// comes with it — and how often it looks again. The deadline is only here so a failure is a
/// failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// `Task$lastStatus` of a task whose containers have started.
const RUNNING: &str = "RUNNING";

/// The image every container below names: one this emulator holds no program for, which is
/// exactly the point of the rows whose containers name no program — nothing of such a
/// container runs, so its log stream carries nothing. (An image whose own command this
/// emulator does hold, a web server's, would have its program run and write its own output:
/// `container_servers::image_declared_command`.)
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The port the containers declare and the bound target group forwards to.
const CONTAINER_PORT: i64 = 80;

/// One container a row's task definition declares.
struct Container {
    name: &'static str,
    /// The `command` the definition gives it, or `None` for a container whose image's own
    /// entry point is what would run.
    command: Option<&'static [&'static str]>,
    /// Whether the container is essential, so a row can hold a container that EXITS beside
    /// one that keeps the task up.
    essential: bool,
    /// The `awslogs-stream-prefix` the configuration names, or `None` for one that names no
    /// prefix at all.
    stream_prefix: Option<&'static str>,
    /// The line the container's program writes, for a row that asserts the delivery of it.
    written_line: Option<&'static str>,
}

/// One row: the containers one service's task runs, and the names its own resources take so
/// two rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    containers: &'static [Container],
    log_group: &'static str,
    /// How the execution role this row's launch runs as is granted the permission the
    /// driver's own stream creation needs.
    grant: Grant,
    target_group: &'static str,
    /// The port of the shared load balancer's listener that forwards to this row's target
    /// group: a target group a service binds has to be associated with a load balancer, and a
    /// listener is what associates it.
    listener_port: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

/// The program the row that asserts a delivery gives its container, and the line it writes.
const WRITER_COMMAND: &[&str] = &["sh", "-c", "echo the sidecar wrote this"];
const WRITTEN_LINE: &str = "the sidecar wrote this";

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: one container whose image holds its program",
        containers: &[Container {
            name: "app",
            command: None,
            essential: true,
            stream_prefix: Some("ecs"),
            written_line: None,
        }],
        log_group: "/ecs/awslogs-single",
        grant: Grant::ManagedPolicy,
        target_group: "awslogs-single-tg",
        listener_port: "80",
        cluster: "awslogs-single-cluster",
        family: "awslogs-single-task",
        service: "awslogs-single-service",
    },
    Row {
        case: "two containers, one of whose programs this emulator runs",
        containers: &[
            Container {
                name: "app",
                command: None,
                essential: true,
                stream_prefix: Some("ecs"),
                written_line: None,
            },
            Container {
                name: "sidecar",
                command: Some(WRITER_COMMAND),
                essential: false,
                stream_prefix: Some("ecs"),
                written_line: Some(WRITTEN_LINE),
            },
        ],
        log_group: "/ecs/awslogs-pair",
        grant: Grant::ManagedPolicy,
        target_group: "awslogs-pair-tg",
        listener_port: "8080",
        cluster: "awslogs-pair-cluster",
        family: "awslogs-pair-task",
        service: "awslogs-pair-service",
    },
    Row {
        case: "a configuration that names no stream prefix",
        containers: &[Container {
            name: "app",
            command: None,
            essential: true,
            stream_prefix: None,
            written_line: None,
        }],
        log_group: "/ecs/awslogs-unprefixed",
        grant: Grant::InlinePolicy,
        target_group: "awslogs-unprefixed-tg",
        listener_port: "8081",
        cluster: "awslogs-unprefixed-cluster",
        family: "awslogs-unprefixed-task",
        service: "awslogs-unprefixed-service",
    },
];

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them — the CLASSIC Elastic Load Balancing front door
/// included, since the two Elastic Load Balancing models share one endpoint and one signing
/// name there.
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            engine,
        ),
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

/// Call one operation on a service whose model declares a Query protocol, answered with its
/// XML body.
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

/// Call one Elastic Load Balancing v2 operation, addressed by this emulator's own routing key
/// for that model.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "elasticloadbalancingv2",
        ELB_VERSION,
        action,
        members,
    )
    .await
}

/// Call one Amazon EC2 operation.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(router, "ec2", EC2_VERSION, action, members).await
}

/// Call one IAM operation.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(router, "iam", IAM_VERSION, action, members).await
}

/// Create a task execution role with the trust policy Amazon ECS assumes it under, and answer
/// its ARN — the role IAM itself holds, not one this suite made up.
async fn create_role(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", EXECUTION_ROLE_TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from CreateRole for {name}, got {status} with body {created}"
    );
    element(&created, "Arn")
}

/// The two execution roles the rows run as, keyed by the grant each carries: one holding AWS's
/// own managed policy for the purpose, and one holding an inline policy of the account's own
/// scoped to the streams of the group its row's driver writes to.
async fn execution_roles(router: &MultiServiceRouter) -> BTreeMap<&'static str, String> {
    let managed = create_role(router, MANAGED_GRANT_ROLE_NAME).await;
    let (status, attached) = iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", MANAGED_GRANT_ROLE_NAME),
            ("PolicyArn", EXECUTION_ROLE_POLICY_ARN),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from AttachRolePolicy, got {status} with body {attached}"
    );

    let inline = create_role(router, INLINE_GRANT_ROLE_NAME).await;
    // The grant a client writes by hand: the two actions the driver's initialisation and its
    // delivery make, on the streams of the group its configuration names.
    let groups: Vec<String> = ROWS
        .iter()
        .filter(|row| row.grant == Grant::InlinePolicy)
        .map(|row| {
            format!(
                "\"arn:aws:logs:{REGION}:{}:log-group:{}:*\"",
                engine::emulator_account_id(),
                row.log_group
            )
        })
        .collect();
    let document = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\"Action\":\
         [\"logs:CreateLogStream\",\"logs:PutLogEvents\"],\"Resource\":[{}]}}]}}",
        groups.join(",")
    );
    let (status, put) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", INLINE_GRANT_ROLE_NAME),
            ("PolicyName", "awslogs-delivery"),
            ("PolicyDocument", document.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from PutRolePolicy, got {status} with body {put}"
    );

    [("managed", managed), ("inline", inline)]
        .into_iter()
        .collect()
}

/// The key one row's grant names its role under.
fn grant_key(grant: Grant) -> &'static str {
    match grant {
        Grant::ManagedPolicy => "managed",
        Grant::InlinePolicy => "inline",
    }
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

/// Call one Amazon ECS operation.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    json_call(router, "ecs", ECS_TARGET_PREFIX, operation, request).await
}

/// Call one CloudWatch Logs operation.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    json_call(router, "logs", LOGS_TARGET_PREFIX, operation, request).await
}

/// Call one Amazon ECS operation that must succeed, and answer its payload.
async fn ecs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, payload) = ecs(router, operation, request).await;
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
    let (status, payload) = logs(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from logs {operation}, got {status} with {payload}"
    );
    payload
}

/// The account's own default subnets, read out of Amazon EC2 rather than invented — the
/// network an `awsvpc` placement of this account resolves against.
async fn default_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = ec2(
        router,
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
        subnets.len() >= 2,
        "setup: the account must hold default subnets, got {subnets:?}"
    );
    subnets
}

/// The VPC the subnet `subnet_id` belongs to, as Amazon EC2 reports it.
async fn vpc_of(router: &MultiServiceRouter, subnet_id: &str) -> String {
    let (status, body) = ec2(router, "DescribeSubnets", &[("SubnetId.1", subnet_id)]).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from DescribeSubnets, got {status} with body {body}"
    );
    element(&body, "vpcId")
}

/// Create the load balancer every row's target group is associated through, placed in the
/// account's own default subnets, and answer its ARN.
///
/// A service's `loadBalancers` may only bind a target group that HAS a load balancer, so the
/// estate holds one: the rows differ in what their containers log, not in their front door.
async fn load_balancer(router: &MultiServiceRouter, subnets: &[String]) -> String {
    let (status, created) = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", "awslogs-rows-alb"),
            ("Scheme", "internal"),
            ("Subnets.member.1", subnets[0].as_str()),
            ("Subnets.member.2", subnets[1].as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from CreateLoadBalancer, got {status} with body {created}"
    );
    element(&created, "LoadBalancerArn")
}

/// Associate one row's target group with that load balancer, through a listener of its own
/// that forwards to it.
async fn forward_to(
    router: &MultiServiceRouter,
    case: &str,
    load_balancer_arn: &str,
    port: &str,
    target_group_arn: &str,
) {
    let (status, created) = elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer_arn),
            ("Protocol", "HTTP"),
            ("Port", port),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", target_group_arn),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CreateListener, got {status} with body {created}"
    );
}

/// The `containerDefinitions` one row registers.
fn container_definitions(row: &Row) -> Value {
    let definitions: Vec<Value> = row
        .containers
        .iter()
        .map(|container| {
            let mut options = json!({
                "awslogs-group": row.log_group,
                "awslogs-region": REGION,
            });
            if let Some(prefix) = container.stream_prefix {
                options["awslogs-stream-prefix"] = json!(prefix);
            }
            let mut definition = json!({
                "name": container.name,
                "image": IMAGE,
                "essential": container.essential,
                "portMappings": [{ "containerPort": CONTAINER_PORT, "protocol": "tcp" }],
                "logConfiguration": { "logDriver": "awslogs", "options": options },
            });
            if let Some(command) = container.command {
                definition["command"] = json!(command);
            }
            definition
        })
        .collect();
    Value::Array(definitions)
}

/// One task of the service `service`, as `DescribeTasks` reports it once it is RUNNING —
/// looked at again until one is, or the deadline passes.
async fn running_task(router: &MultiServiceRouter, case: &str, row: &Row) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs_ok(
            router,
            case,
            "ListTasks",
            &json!({ "cluster": row.cluster, "serviceName": row.service, "desiredStatus": RUNNING }),
        )
        .await;
        let arns: Vec<&str> = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !arns.is_empty() {
            let described = ecs_ok(
                router,
                case,
                "DescribeTasks",
                &json!({ "cluster": row.cluster, "tasks": arns }),
            )
            .await;
            let task = described["tasks"][0].clone();
            if task["lastStatus"].as_str() == Some(RUNNING) {
                return task;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no task of the service reached {RUNNING}, last listing was {listed}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The private IPv4 address of the interface `task` was allocated, read off its attachments.
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

/// The last segment of an ARN, which is the id of the thing it names.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// The stream each container of `task` is expected to deliver to: the prefix, the container's
/// name and the task's id, and the container's own id for a configuration naming no prefix —
/// the naming the driver's `awslogs-stream-prefix` documentation states.
fn expected_streams(case: &str, row: &Row, task: &Value) -> BTreeSet<String> {
    let task_id = id_of(
        task["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the task carries no ARN: {task}")),
    );
    row.containers
        .iter()
        .map(|container| match container.stream_prefix {
            Some(prefix) => format!("{prefix}/{}/{task_id}", container.name),
            None => id_of(container_arn(case, task, container.name)).to_string(),
        })
        .collect()
}

/// The ARN Amazon ECS minted for the container `name` of `task`.
fn container_arn<'a>(case: &str, task: &'a Value, name: &str) -> &'a str {
    task["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|container| container["name"].as_str() == Some(name))
        .and_then(|container| container["containerArn"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: the task holds no container {name:?}: {task}"))
}

/// The streams `DescribeLogStreams` reports in one group.
async fn reported_streams(router: &MultiServiceRouter, case: &str, group: &str) -> BTreeSet<String> {
    let described = logs_ok(
        router,
        case,
        "DescribeLogStreams",
        &json!({ "logGroupName": group }),
    )
    .await;
    described["logStreams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stream| stream["logStreamName"].as_str())
        .map(str::to_string)
        .collect()
}

/// The streams of one group once they are the ones `wanted` — looked at again until they are,
/// or the deadline passes, because the driver initialisation a started container owes is a
/// clock-owed move rather than something the create answered with.
async fn settled_streams(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: &BTreeSet<String>,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_streams(router, case, group).await;
        if &reported == wanted || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The messages `GetLogEvents` reports in one stream, oldest first.
async fn stream_messages(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
) -> Vec<String> {
    let read = logs_ok(
        router,
        case,
        "GetLogEvents",
        &json!({ "logGroupName": group, "logStreamName": stream, "startFromHead": true }),
    )
    .await;
    read["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str())
        .map(str::to_string)
        .collect()
}

/// The messages of one stream once it carries the line `wanted` — looked at again until it
/// does, or the deadline passes: a container's output is delivered as its program writes it.
async fn settled_messages(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
    wanted: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let messages = stream_messages(router, case, group, stream).await;
        if messages.iter().any(|message| message == wanted) || Instant::now() >= deadline {
            return messages;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `(Id, Port)` of every registration one target group holds, as `DescribeTargetHealth`
/// reports them.
async fn registered_targets(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
) -> BTreeMap<String, String> {
    let (status, body) = elbv2(
        router,
        "DescribeTargetHealth",
        &[("TargetGroupArn", target_group_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeTargetHealth, got {status} with body {body}"
    );
    pairs_of(&body, "Id", "Port")
}

/// The registrations of one group once it holds the address `wanted` — looked at again until
/// it does, or the deadline passes.
async fn settled_targets(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
    wanted: &str,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = registered_targets(router, case, target_group_arn).await;
        if reported.contains_key(wanted) || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_started_containers_awslogs_driver_opens_its_stream_in_the_configured_group() {
    let router = build_router();
    let subnets = default_subnets(&router).await;
    let vpc = vpc_of(&router, &subnets[0]).await;
    let execution_roles = execution_roles(&router).await;
    let load_balancer_arn = load_balancer(&router, &subnets).await;

    for row in ROWS {
        let case = row.case;

        // The group the driver delivers to, and the group the service's tasks are registered
        // into, each created through its own service's own operation.
        logs_ok(
            &router,
            case,
            "CreateLogGroup",
            &json!({ "logGroupName": row.log_group }),
        )
        .await;
        let (status, created) = elbv2(
            &router,
            "CreateTargetGroup",
            &[
                ("Name", row.target_group),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("VpcId", vpc.as_str()),
                ("TargetType", "ip"),
                ("HealthCheckProtocol", "HTTP"),
                ("HealthCheckPath", "/"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateTargetGroup, got {status} with body {created}"
        );
        let target_group_arn = element(&created, "TargetGroupArn");
        forward_to(
            &router,
            case,
            &load_balancer_arn,
            row.listener_port,
            &target_group_arn,
        )
        .await;

        ecs_ok(
            &router,
            case,
            "CreateCluster",
            &json!({ "clusterName": row.cluster }),
        )
        .await;
        let registered = ecs_ok(
            &router,
            case,
            "RegisterTaskDefinition",
            &json!({
                "family": row.family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "256",
                "memory": "512",
                "executionRoleArn": execution_roles[grant_key(row.grant)],
                "containerDefinitions": container_definitions(row),
            }),
        )
        .await;
        let task_definition = registered["taskDefinition"]["taskDefinitionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no revision ARN in {registered}"))
            .to_string();

        ecs_ok(
            &router,
            case,
            "CreateService",
            &json!({
                "cluster": row.cluster,
                "serviceName": row.service,
                "taskDefinition": task_definition,
                "desiredCount": 1,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": subnets,
                        "assignPublicIp": "ENABLED",
                    }
                },
                "loadBalancers": [{
                    "targetGroupArn": target_group_arn,
                    "containerName": row.containers[0].name,
                    "containerPort": CONTAINER_PORT,
                }],
            }),
        )
        .await;

        // The task really runs, on an interface of its own.
        let task = running_task(&router, case, row).await;
        let address = task_address(case, &task);

        // ...and it is a target of the group its `loadBalancers` bound, at that address on
        // the container port the binding named.
        let targets = settled_targets(&router, case, &target_group_arn, &address).await;
        let expected_port = CONTAINER_PORT.to_string();
        assert_eq!(
            targets.get(address.as_str()),
            Some(&expected_port),
            "case {case:?}: the task at {address} must be a target of the group on port \
             {CONTAINER_PORT}, got {targets:?}"
        );

        // ...and every container of it has the stream its driver was configured with, in the
        // group that configuration names.
        let wanted = expected_streams(case, row, &task);
        let reported = settled_streams(&router, case, row.log_group, &wanted).await;
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected one stream per started container in {}, got {reported:?}",
            row.log_group
        );

        // The EVENTS are the containers' own output: the one whose program this emulator runs
        // delivers the line it wrote, and a container whose image holds its program writes
        // nothing, so its stream carries no event.
        for container in row.containers {
            let stream = match container.stream_prefix {
                Some(prefix) => format!(
                    "{prefix}/{}/{}",
                    container.name,
                    id_of(task["taskArn"].as_str().unwrap_or_default())
                ),
                None => id_of(container_arn(case, &task, container.name)).to_string(),
            };
            match container.written_line {
                Some(line) => {
                    let messages =
                        settled_messages(&router, case, row.log_group, &stream, line).await;
                    assert!(
                        messages.iter().any(|message| message == line),
                        "case {case:?}: the stream {stream} of container {} must carry the \
                         line its program wrote, got {messages:?}",
                        container.name
                    );
                }
                None => {
                    let messages = stream_messages(&router, case, row.log_group, &stream).await;
                    assert!(
                        messages.is_empty(),
                        "case {case:?}: the stream {stream} of container {} must carry no \
                         event, because no program of that container ran, got {messages:?}",
                        container.name
                    );
                }
            }
        }
    }
}
