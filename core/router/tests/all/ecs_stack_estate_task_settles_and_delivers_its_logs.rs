// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed Amazon ECS estate is REACHABLE in the services that own its
//! resources, and a standalone task placed on it SETTLES and delivers what its container wrote.
//!
//! RULE under test, in three parts:
//!
//! * **A resource a stack reports `CREATE_COMPLETE` for exists in the service that OWNS it,
//!   addressable there by the physical id the stack published.** So the repository an
//!   `AWS::ECR::Repository` declares is answered by Amazon ECR's own `DescribeRepositories`
//!   under that name — `RepositoryNotFoundException` is for "the repository with the specified
//!   name" that the registry does not hold, not for one a deployment just made — whether the
//!   template named it or CloudFormation generated the name; and the service an
//!   `AWS::ECS::Service` declares is answered by Amazon ECS's own `DescribeServices` in
//!   `services`, with nothing in `failures` ("any failures associated with the call").
//! * **A standalone task reaches `STOPPED` when an essential container of it exits**: the task
//!   reports `stoppedAt` ("the Unix timestamp for the time when the task was stopped"),
//!   `stopCode` `EssentialContainerExited` of the `TaskStopCode` enum, the reason AWS words that
//!   stop with, and the status the program ended with as the container's `exitCode` — so a
//!   client polling `DescribeTasks` for a terminal task terminates instead of running out of
//!   budget.
//! * **Every line that container wrote is delivered to the group its `awslogs` configuration
//!   names**, as one log event per line, readable there with `FilterLogEvents`, in the stream
//!   the driver's own `awslogs-stream-prefix` documentation names
//!   (`<prefix>/<container name>/<task id>`) — and, for a configuration naming no prefix, the
//!   stream the driver falls back to, named after the container it delivers for.
//!
//! Sources: `smithy_query(query="shape",shape="Task,TaskStopCode")` for the stopped projection
//! and the stop code of an essential container's exit,
//! `smithy_query(query="operation",operation="DescribeServices")` for `services`/`failures`, and
//! `smithy_query(query="operation",operation="DescribeRepositories")` for what the registry
//! answers about a name it does not hold.
//!
//! Five crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes every create call, IAM holds the execution role the driver is
//! initialised as, Amazon ECR holds the repository, Amazon ECS owns the cluster, the revision,
//! the service and the task that runs, CloudWatch Logs holds the group and what reached it, and
//! Amazon EC2 holds the network the placement resolves against. Nothing reaches into a store:
//! every fact asserted is read back through the owning service's own API.
//!
//! The rows differ along what the rule generalises over: WHO named the repository (the template,
//! and CloudFormation — so the repository is addressed by a name nobody wrote), HOW MANY lines
//! the container writes (one, and two), the STATUS its program ends with (zero, and a failure),
//! WHEN it ends relative to the span a standalone task's work is reported running for
//! ([`WORK_SPAN`] — inside it, and past it), whether the log configuration names a STREAM PREFIX
//! at all, and how many tasks the declared service keeps. A recording of one stack whose task
//! writes a single marker line is one row.
//!
//! Every clock-owed move — the stack's resources coming up, the task's placement, its start, the
//! end of its program and the delivery of what it wrote — is POLLED for until it holds, never
//! slept for, and the deadline is only here so a failure is a failure rather than a hang:
//! nothing asserts how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output readers this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions a CloudFormation and an ec2Query request carry.
const CFN_API_VERSION: &str = "2010-05-15";
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives, and the content type
/// their models declare.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const JSON_1_1: &str = "application/x-amz-json-1.1";

/// The one container of every revision below.
const CONTAINER: &str = "app";

/// The image that container names: a public registry reference, reachable from a default subnet
/// with a public address — what the rows are about is the task's settling and its delivery, not
/// which registry served the image.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The service principal Amazon ECS assumes a task's execution role as.
const ECS_TASKS: &str = "ecs-tasks.amazonaws.com";

/// The managed policy AWS publishes for a task execution role, which is how the console and
/// AWS's own templates grant the `logs:CreateLogStream` / `logs:PutLogEvents` the `awslogs`
/// driver is initialised with.
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The capability a create has to acknowledge for a template naming the IAM role it creates:
/// "if you have IAM resources with custom names, you must specify CAPABILITY_NAMED_IAM"
/// (`CreateStack$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The stack statuses a create is still in flight in, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Task$lastStatus` of a task that has stopped: the terminal status a client polls for.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` and `Task$stoppedReason` of a task stopped because an essential container of
/// it exited — the `TaskStopCode` member for that stop, and the sentence Amazon ECS words it
/// with. Spelled out here rather than imported from the service crate, so a wrong constant in
/// the implementation cannot make this suite pass.
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";
const ESSENTIAL_CONTAINER_EXITED_REASON: &str = "Essential container in task exited";

/// The span a STANDALONE task's work is reported running for, read from the service itself: the
/// rows below are placed on either side of it, because the rule says that span decides only WHEN
/// a stop may be reported, never THAT one is.
const WORK_SPAN: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::CONTAINER_WORK_SPAN;

/// How long a row waits for the clock-owed moves it depends on, and how often it looks again.
/// Several times the span those moves take: the deadline is only here so a failure is a failure
/// rather than a hang, and every wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// One row: the estate one stack deploys, and the standalone task then placed on it.
struct Row {
    case: &'static str,
    stack: &'static str,
    role: &'static str,
    /// The name the template gives the repository, and `None` where the template gives none —
    /// so CloudFormation names it, and the repository is addressed by a name nobody wrote.
    repository: Option<&'static str>,
    log_group: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
    /// `AWS::ECS::Service`'s own `DesiredCount`, which `DescribeServices` has to report back.
    desired: u64,
    /// The `awslogs-stream-prefix` the log configuration states, and `None` where it states
    /// none.
    stream_prefix: Option<&'static str>,
    /// Every line the container's program writes, in order — one delivered log event each.
    lines: &'static [&'static str],
    /// How long that program keeps running before it exits, in seconds.
    runs_for_secs: u64,
    /// The status it ends with, which the container reports as its `exitCode`.
    exit_code: i64,
}

/// The table spans the rule rather than restating one template: who named the repository, how
/// many lines the container writes, the status and timing of its exit, whether the delivery
/// names a stream prefix, and how many tasks the declared service keeps.
const ROWS: &[Row] = &[
    Row {
        case: "a template-named repository, one line, an exit inside the work span, a prefixed \
               stream",
        stack: "ecsestate-named",
        role: "ecsEstateNamed",
        repository: Some("ecsestate/named"),
        log_group: "/emulator/cfn/ecs-estate-named",
        cluster: "ecsestatenamed",
        family: "ecsestatenamedtask",
        service: "ecsestatenamedsvc",
        desired: 1,
        stream_prefix: Some(CONTAINER),
        lines: &["the standalone task wrote its marker"],
        runs_for_secs: 1,
        exit_code: 0,
    },
    Row {
        case: "a repository CloudFormation named, two lines, a failing exit past the work span, \
               a prefixed stream",
        stack: "ecsestate-generated",
        role: "ecsEstateGenerated",
        repository: None,
        log_group: "/emulator/cfn/ecs-estate-generated",
        cluster: "ecsestategenerated",
        family: "ecsestategeneratedtask",
        service: "ecsestategeneratedsvc",
        desired: 0,
        stream_prefix: Some("tasks"),
        lines: &[
            "the standalone task opened its work",
            "the standalone task gave up on its work",
        ],
        runs_for_secs: 5,
        exit_code: 4,
    },
    Row {
        case: "a delivery naming no stream prefix at all",
        stack: "ecsestate-unprefixed",
        role: "ecsEstateUnprefixed",
        repository: Some("ecsestate/unprefixed"),
        log_group: "/emulator/cfn/ecs-estate-unprefixed",
        cluster: "ecsestateunprefixed",
        family: "ecsestateunprefixedtask",
        service: "ecsestateunprefixedsvc",
        desired: 1,
        stream_prefix: None,
        lines: &["the unprefixed delivery carried this line"],
        runs_for_secs: 1,
        exit_code: 0,
    },
];

/// One emulator carrying every service this path crosses, over ONE shared engine, exactly as the
/// production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
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

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and the signature are not credentials: this router runs with request authentication off, so
/// nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one operation of a service whose model declares a form-encoded query wire, answered with
/// its status and XML body.
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

/// One CloudFormation call that must succeed, answered with its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "cloudformation", CFN_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One Amazon EC2 call that must succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from EC2 {action}, got {status} with {body}"
    );
    body
}

/// One awsJson call, answered with its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", JSON_1_1)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
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
    (status, parsed)
}

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = rpc(router, "ecs", ECS_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECR call, answered with its status and parsed body — a status this suite asserts
/// on, because a repository the registry does not hold is a REFUSAL rather than an empty page.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "ecr", ECR_TARGET_PREFIX, operation, body).await
}

/// One CloudWatch Logs call that must succeed.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = rpc(router, "logs", LOGS_TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudWatch Logs {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A default subnet of the account's own default VPC, read out of Amazon EC2 itself: the network
/// a deployment that creates none of its own places its workload in, and — a default subnet
/// being a public one — the path a task given a public address takes to the registry.
async fn default_subnet(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2(
        router,
        case,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
    )
    .await;
    let subnets = elements(&body, "subnetId");
    assert!(
        !subnets.is_empty(),
        "case {case:?}: the account's default VPC must hold a default subnet, got {body}"
    );
    subnets[0].clone()
}

/// The program one row's container runs: every line it writes, then the wait and the status its
/// program ends with.
fn program(row: &Row) -> String {
    let written: String = row
        .lines
        .iter()
        .map(|line| format!("echo \"{line}\"; "))
        .collect();
    format!(
        "{written}sleep {}; exit {}",
        row.runs_for_secs, row.exit_code
    )
}

/// The template one row deploys: the repository, the execution role the `awslogs` driver is
/// initialised as, the group it delivers into, and the cluster, Fargate revision and service of
/// the estate.
fn template(row: &Row, subnet: &str) -> String {
    let repository_properties = match row.repository {
        Some(name) => format!("    Properties:\n      RepositoryName: {name}\n"),
        None => String::new(),
    };
    let prefix_option = match row.stream_prefix {
        Some(prefix) => format!("              awslogs-stream-prefix: {prefix}\n"),
        None => String::new(),
    };
    format!(
        r#"Resources:
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      RoleName: {role}
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service: {ECS_TASKS}
      ManagedPolicyArns:
        - {EXECUTION_ROLE_POLICY}
  Repository:
    Type: AWS::ECR::Repository
{repository_properties}  LogGroup:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: {log_group}
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {family}
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: {IMAGE}
          Essential: true
          Command:
            - sh
            - -c
            - '{program}'
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
{prefix_option}  Service:
    Type: AWS::ECS::Service
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired}
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          Subnets:
            - {subnet}
Outputs:
  ClusterName:
    Value: !Ref Cluster
  ServiceName:
    Value: !GetAtt Service.Name
  TaskDefinition:
    Value: !Ref Task
  LogGroupName:
    Value: !Ref LogGroup
  RepositoryName:
    Value: !Ref Repository
  RepositoryUri:
    Value: !GetAtt Repository.RepositoryUri
"#,
        role = row.role,
        log_group = row.log_group,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        desired = row.desired,
        program = program(row),
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED: looked
/// at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, case, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        let in_flight = reported == CREATE_IN_PROGRESS || reported == ROLLBACK_IN_PROGRESS;
        if !in_flight || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every `ResourceStatusReason` one stack's event stream carries, which is where the refusal a
/// failing resource earned is reported.
async fn stack_failures(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let body = cloudformation(router, case, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&body, "ResourceStatusReason")
}

/// One output of a settled stack, or a panic naming the case and what the stack did publish.
fn output(case: &str, described: &str, key: &str) -> String {
    let outputs = outputs_of(described);
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output, got {outputs:?}"))
}

/// `RunTask` one row's revision into the cluster its stack deployed, and answer the ARN of the
/// task it placed.
async fn run_task(
    router: &MultiServiceRouter,
    row: &Row,
    cluster: &str,
    task_definition: &str,
    subnet: &str,
) -> String {
    let case = row.case;
    let placed = ecs(
        router,
        case,
        "RunTask",
        json!({
            "cluster": cluster,
            "taskDefinition": task_definition,
            "launchType": "FARGATE",
            "count": 1,
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [subnet],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    let tasks = placed["tasks"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {placed}"
    );
    tasks[0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the placed task must carry a taskArn, got {placed}")
        })
        .to_string()
}

/// One task as `DescribeTasks` reports it once it has STOPPED, looked at again until it has or
/// the deadline passes.
async fn stopped_task(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    task_arn: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            case,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [task_arn] }),
        )
        .await;
        let task = described["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some(STOPPED) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the placed task to reach {STOPPED}, got {task}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The messages `FilterLogEvents` reports for one group in exactly one of its streams, as the
/// reader hands them back.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    log_group: &str,
    stream: &str,
) -> Vec<String> {
    let found = logs(
        router,
        case,
        "FilterLogEvents",
        json!({ "logGroupName": log_group, "logStreamNames": [stream] }),
    )
    .await;
    found["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// The messages of one stream once it holds at least `wanted` of them, looked at again until it
/// does or the deadline passes: a delivery is a clock-owed move, so what is waited for is the
/// CONDITION rather than a span of it.
async fn delivered_lines(
    router: &MultiServiceRouter,
    case: &str,
    log_group: &str,
    stream: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let lines = delivered(router, case, log_group, stream).await;
        if lines.len() >= wanted || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A timestamp member of a task, as the epoch instant the awsJson wire carries it as.
fn instant_of(case: &str, task: &Value, member: &str) -> f64 {
    task[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} on the task, got {task}"))
}

/// The last segment of one ARN: a task's id, or a container's.
fn last_segment(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// The stream one row's delivery goes to: the prefix, the container's name and the task's id as
/// the `awslogs-stream-prefix` documentation names them, and — for a configuration naming no
/// prefix — the container the driver delivers for.
fn expected_stream(row: &Row, task: &Value, case: &str) -> String {
    let task_arn = task["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a taskArn on the task, got {task}"));
    match row.stream_prefix {
        Some(prefix) => format!("{prefix}/{CONTAINER}/{}", last_segment(task_arn)),
        None => {
            let container_arn = task["containers"][0]["containerArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a containerArn on the task, got {task}")
                });
            last_segment(container_arn).to_string()
        }
    }
}

/// RULE: a deployed estate is reachable in the services that own its resources, and a standalone
/// task placed on it settles and delivers what its container wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployed_estate_settles_its_task_and_delivers_its_output() {
    // The table really does span the dimension the stop rule is about: a program that ends
    // inside the span a standalone task's work is reported running for, and one that outlasts
    // it.
    assert!(
        ROWS.iter()
            .any(|row| Duration::from_secs(row.runs_for_secs) < WORK_SPAN)
            && ROWS
                .iter()
                .any(|row| Duration::from_secs(row.runs_for_secs) > WORK_SPAN),
        "the rows must fall on both sides of the {WORK_SPAN:?} work span, got {:?}",
        ROWS.iter().map(|row| row.runs_for_secs).collect::<Vec<_>>()
    );

    let router = emulator();
    let subnet = default_subnet(&router, "the shared network").await;

    for row in ROWS {
        let case = row.case;
        let body = template(row, &subnet);
        cloudformation(
            &router,
            case,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", NAMED_IAM),
            ],
        )
        .await;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
            stack_failures(&router, case, row.stack).await,
        );

        // The handles a client reads off the stack, and the physical ids it recorded.
        let cluster = output(case, &described, "ClusterName");
        let service = output(case, &described, "ServiceName");
        let task_definition = output(case, &described, "TaskDefinition");
        let log_group = output(case, &described, "LogGroupName");
        let repository = output(case, &described, "RepositoryName");
        let repository_uri = output(case, &described, "RepositoryUri");
        let resources = cloudformation(
            &router,
            case,
            "DescribeStackResources",
            &[("StackName", row.stack)],
        )
        .await;
        let physical = physical_ids_of(&resources);

        // Part one: the repository the stack declared is a repository AMAZON ECR holds, under
        // the name the stack published — whoever chose that name.
        assert_eq!(
            physical.get("Repository").map(String::as_str),
            Some(repository.as_str()),
            "case {case:?}: the stack's repository resource must be the name it published, got \
             {physical:?} against {repository:?}"
        );
        if let Some(declared) = row.repository {
            assert_eq!(
                repository, declared,
                "case {case:?}: a template-named repository keeps its name, got {repository:?}"
            );
        } else {
            assert!(
                !repository.is_empty() && repository != "Repository",
                "case {case:?}: CloudFormation must name the repository the template left \
                 unnamed, got {repository:?}"
            );
        }
        let (status, held) = ecr(
            &router,
            "DescribeRepositories",
            json!({ "repositoryNames": [repository.clone()] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ECR DescribeRepositories for the deployed \
             repository {repository:?}, got {status} with {held}"
        );
        let repositories = held["repositories"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            repositories.len(),
            1,
            "case {case:?}: expected exactly the deployed repository back, got {held}"
        );
        assert_eq!(
            repositories[0]["repositoryName"].as_str(),
            Some(repository.as_str()),
            "case {case:?}: expected Amazon ECR to report {repository:?}, got {held}"
        );
        assert_eq!(
            repositories[0]["repositoryUri"].as_str(),
            Some(repository_uri.as_str()),
            "case {case:?}: the URI the stack published must be the one Amazon ECR reports, got \
             {repository_uri:?} against {held}"
        );

        // Part two: the service the stack declared is a service AMAZON ECS holds, answered in
        // `services` with nothing in `failures`.
        let services = ecs(
            &router,
            case,
            "DescribeServices",
            json!({ "cluster": cluster.clone(), "services": [service.clone()] }),
        )
        .await;
        let failures = services["failures"].as_array().cloned().unwrap_or_default();
        assert!(
            failures.is_empty(),
            "case {case:?}: expected no failures describing the deployed service {service:?}, got \
             {services}"
        );
        let described_services = services["services"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            described_services.len(),
            1,
            "case {case:?}: expected exactly the deployed service back, got {services}"
        );
        assert_eq!(
            described_services[0]["serviceName"].as_str(),
            Some(service.as_str()),
            "case {case:?}: expected Amazon ECS to report {service:?}, got {services}"
        );
        assert_eq!(
            described_services[0]["serviceArn"].as_str(),
            physical.get("Service").map(String::as_str),
            "case {case:?}: the service's ARN must be the physical id the stack recorded, got \
             {services} against {physical:?}"
        );
        assert_eq!(
            described_services[0]["desiredCount"].as_u64(),
            Some(row.desired),
            "case {case:?}: expected the declared desiredCount {}, got {services}",
            row.desired
        );

        // Part three: a standalone task of the deployed revision SETTLES, and states the exit
        // that stopped it.
        let task_arn = run_task(&router, row, &cluster, &task_definition, &subnet).await;
        let task = stopped_task(&router, case, &cluster, &task_arn).await;
        assert_eq!(
            task["stopCode"].as_str(),
            Some(ESSENTIAL_CONTAINER_EXITED),
            "case {case:?}: expected the stop code of an essential container's exit, got {task}"
        );
        assert_eq!(
            task["stoppedReason"].as_str(),
            Some(ESSENTIAL_CONTAINER_EXITED_REASON),
            "case {case:?}: expected the stopped reason of an essential container's exit, got \
             {task}"
        );
        assert_eq!(
            task["containers"][0]["exitCode"].as_i64(),
            Some(row.exit_code),
            "case {case:?}: expected the container's exit code {}, got {task}",
            row.exit_code
        );
        let created = instant_of(case, &task, "createdAt");
        let started = instant_of(case, &task, "startedAt");
        let stopped = instant_of(case, &task, "stoppedAt");
        assert!(
            created <= started && started <= stopped,
            "case {case:?}: expected createdAt <= startedAt <= stoppedAt, got {created}, \
             {started} and {stopped}"
        );

        // ...and every line it wrote reached the group its configuration names, in the stream
        // the driver names, one event per line.
        let stream = expected_stream(row, &task, case);
        let mut messages =
            delivered_lines(&router, case, &log_group, &stream, row.lines.len()).await;
        messages.sort();
        let mut expected: Vec<String> = row.lines.iter().map(|line| line.to_string()).collect();
        expected.sort();
        assert_eq!(
            messages, expected,
            "case {case:?}: expected exactly one event per line written in {stream:?} of \
             {log_group:?}, got {messages:?}"
        );
    }
}
