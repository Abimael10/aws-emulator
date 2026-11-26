// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed Amazon ECS Fargate service HOLDS a running task whose container
//! program is long-lived, and that container's output reaches the log group the stack declared.
//!
//! RULE under test: **nothing is waiting for the program of a container whose task a SERVICE
//! maintains to END — a service "runs and maintains your desired number of tasks from a
//! specified task definition" (`CreateService`), so the end of such a task is the task leaving
//! the set the scheduler holds rather than a verdict a caller polls for — so every wait that
//! program names is a span the container is UP for, wherever the wait stands in it, and so is a
//! FOLLOW, which "shall not terminate after the last line of the input file has been copied"
//! (POSIX, `tail`). A task whose essential container is still running therefore reports
//! `Task$lastStatus` `RUNNING` with no `stopCode`/`stoppedReason` and no container `exitCode`,
//! and keeps reporting it; the service holds the `runningCount` its `desiredCount` asked for;
//! and every line such a container wrote is delivered to the group its `awslogs` configuration
//! names. `STOPPED` with `TaskStopCode` `EssentialContainerExited` is for a container whose
//! program really ended.**
//!
//! Sources: `smithy_query(query="operation",operation="DescribeTasks")` for `Task$lastStatus`
//! ("The last known status for the task"), `Task$stopCode`, `Task$stoppedReason` ("The reason
//! that the task was stopped") and `Container$exitCode` ("The exit code returned from the
//! container"); `smithy_query(query="shape",shape="TaskStopCode")` for the member a task whose
//! essential container exited stops with; `smithy_query(query="operation",
//! operation="DescribeServices")` for `Service$runningCount` ("The number of tasks in the
//! cluster that are in the RUNNING state") and `Service$desiredCount`;
//! `smithy_query(query="operation",operation="ListTasks")` for the `RUNNING` default of its
//! `desiredStatus` filter. POSIX states what the wait means for the program ("the `sleep`
//! utility shall suspend execution for at least the integral number of seconds specified by the
//! `time` operand").
//!
//! Five crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes every create call, IAM holds the execution role whose only permissions
//! come from the AWS managed policy the template attaches, Amazon EC2 holds the default network
//! the placement resolves against and the security group the stack adds to it, Amazon ECS owns
//! the cluster, the revision, the service and the task it places, and CloudWatch Logs holds the
//! group and what was delivered to it. Nothing reaches into a store: every fact asserted is
//! read back through the owning service's own API.
//!
//! The rows differ along what the rule generalises over: the FORM the container's program takes
//! (a wait the program ends on after writing a line, a wait among further steps, a wait the
//! program goes round a loop on, a wait given as the whole argument vector, a FOLLOW of a file
//! nothing more is written to, and a server image that is given no command at all), HOW LONG
//! that wait names, whether the container WRITES anything at all, and how many tasks the
//! service is asked for. The BOUNDARY — a program that really ends, whose task stops as an
//! essential container's exit — is
//! [`crate::cfn_fargate_service_in_its_own_security_group_runs`].
//!
//! Every clock-owed move (the stack's resources coming up, the task's placement, its start and
//! the delivery of what it wrote) is POLLED for until it holds, never slept for, and the
//! deadline is only here so a failure is a failure rather than a hang: nothing asserts how long
//! anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output reader this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions a CloudFormation and an ec2Query request carry.
const CFN_API_VERSION: &str = "2010-05-15";
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The one container of every revision below, which is also the `awslogs-stream-prefix` its log
/// configuration states.
const CONTAINER: &str = "app";

/// `Task$lastStatus` members this suite names.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The capability a create has to acknowledge for a template that creates IAM resources
/// (`CreateStack$Capabilities`).
const IAM_CAPABILITY: &str = "CAPABILITY_IAM";

/// The stack statuses a create is still in flight in, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The managed policy AWS publishes for a task execution role
/// (`AmazonECSTaskExecutionRolePolicy`, the task execution role of the Amazon ECS Developer
/// Guide), which is the ONLY thing granting the deployed role the stream creation and delivery
/// the `awslogs` driver makes as it.
const MANAGED_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// How long a row waits for the clock-owed moves it depends on, and how often it looks again.
/// Many times the span those moves take: the deadline is only here so a failure is a failure
/// rather than a hang, and every wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// How long a row that must GO ON running re-reads the task and its service for, inside the
/// loop that spends it. Longer than the span a container whose wait was taken as an account of
/// work it did would have exited in, so a task still running at the end of this window is one
/// that is being held rather than one read too early. Every row's own wait is far longer than
/// this window, and nothing is asserted about how long anything took.
const HOLD_WINDOW: Duration = Duration::from_secs(4);

/// One row: the program the deployed revision's container runs, how many tasks the service is
/// asked for, and the names this row's own resources take so no two rows collide in the
/// services that hold them.
struct Row {
    case: &'static str,
    /// The `Command` entries the container runs, or none at all — a container whose image
    /// speaks for itself.
    command: &'static [&'static str],
    image: &'static str,
    /// A line the container's own output must carry, or `None` for a program that writes
    /// nothing.
    writes: Option<&'static str>,
    desired: u64,
    stack: &'static str,
    log_group: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

/// The image every row that states a program of its own names. Nothing is fetched from it: what
/// runs is the `command` the revision states.
const SHELL_IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The image of the row that is given no command at all, whose own documented command is a
/// server.
const SERVER_IMAGE: &str = "public.ecr.aws/docker/library/nginx:1.27";

const ROWS: &[Row] = &[
    Row {
        // The recorded shape: a workload that announces itself and then stays up.
        case: "a wait the program ends on after writing a line",
        command: &["sh", "-c", "echo the deployed task started; sleep 3600"],
        image: SHELL_IMAGE,
        writes: Some("the deployed task started"),
        desired: 1,
        stack: "heldfargate-announced",
        log_group: "/emulator/cfn/held-announced",
        cluster: "heldfargateannounced",
        family: "heldfargateannouncedtask",
        service: "heldfargateannouncedsvc",
    },
    Row {
        // The wait stands BETWEEN two steps of the program: the container is up for it, so the
        // step after it is still to come.
        case: "a wait among the program's further steps",
        command: &["sh", "-c", "echo first; sleep 600; echo second"],
        image: SHELL_IMAGE,
        writes: Some("first"),
        desired: 2,
        stack: "heldfargate-between",
        log_group: "/emulator/cfn/held-between",
        cluster: "heldfargatebetween",
        family: "heldfargatebetweentask",
        service: "heldfargatebetweensvc",
    },
    Row {
        case: "a wait the program goes round a loop on",
        command: &["sh", "-c", "while true; do echo tick; sleep 5; done"],
        image: SHELL_IMAGE,
        writes: Some("tick"),
        desired: 1,
        stack: "heldfargate-loop",
        log_group: "/emulator/cfn/held-loop",
        cluster: "heldfargateloop",
        family: "heldfargatelooptask",
        service: "heldfargateloopsvc",
    },
    Row {
        // The whole program is the wait, written as the vector the image exec's: the container
        // is up for it and writes nothing at all.
        case: "a wait given as the whole argument vector",
        command: &["sleep", "1800"],
        image: SHELL_IMAGE,
        writes: None,
        desired: 1,
        stack: "heldfargate-bare",
        log_group: "/emulator/cfn/held-bare",
        cluster: "heldfargatebare",
        family: "heldfargatebaretask",
        service: "heldfargatebaresvc",
    },
    Row {
        // The other canonical keep-alive idiom: a FOLLOW, which "shall not terminate after the
        // last line of the input file has been copied" (POSIX, `tail`).
        case: "a follow of a file nothing more is written to",
        command: &["sh", "-c", "echo ready; tail -f /dev/null"],
        image: SHELL_IMAGE,
        writes: Some("ready"),
        desired: 1,
        stack: "heldfargate-follow",
        log_group: "/emulator/cfn/held-follow",
        cluster: "heldfargatefollow",
        family: "heldfargatefollowtask",
        service: "heldfargatefollowsvc",
    },
    Row {
        case: "a server image given no command at all",
        command: &[],
        image: SERVER_IMAGE,
        writes: Some("worker processes"),
        desired: 1,
        stack: "heldfargate-server",
        log_group: "/emulator/cfn/held-server",
        cluster: "heldfargateserver",
        family: "heldfargateservertask",
        service: "heldfargateserversvc",
    },
];

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation of a service whose model declares a form-encoded query wire, answered
/// with its status and XML body.
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
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    let (status, body) = query(router, "cloudformation", CFN_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudFormation {action}, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 call that must succeed, answered with its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from EC2 {action}, got {status} with body {body}"
    );
    body
}

/// Call one operation of a service whose model declares `awsJson1_1`, answered with its status
/// and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    request: Value,
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
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon ECS call that must succeed, answered with its parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: Value, case: &str) -> Value {
    let (status, parsed) = rpc(router, "ecs", ECS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The account's own default VPC and one of its default subnets, read out of Amazon EC2 itself:
/// the network a deployment that creates none of its own places its workload in.
async fn default_network(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let described = ec2(
        router,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
        case,
    )
    .await;
    let vpcs = elements(&described, "vpcId");
    assert!(
        !vpcs.is_empty(),
        "case {case:?}: the account must hold a default VPC, got {described}"
    );
    let described = ec2(
        router,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
        case,
    )
    .await;
    let subnets = elements(&described, "subnetId");
    assert!(
        !subnets.is_empty(),
        "case {case:?}: the default VPC must hold a default subnet, got {described}"
    );
    (vpcs[0].clone(), subnets[0].clone())
}

/// The template one row deploys: a security group of the default VPC, the execution role whose
/// permissions are the AWS managed policy alone, the log group the container's output is
/// delivered to, and the cluster, Fargate revision and service that place the task.
fn template(row: &Row, vpc: &str, subnet: &str) -> String {
    let command = if row.command.is_empty() {
        String::new()
    } else {
        let entries = row
            .command
            .iter()
            .map(|entry| format!("\n            - {entry:?}"))
            .collect::<String>();
        format!("\n          Command:{entries}")
    };
    format!(
        r#"Parameters:
  VpcId:
    Type: AWS::EC2::VPC::Id
    Default: {vpc}
  SubnetId:
    Type: AWS::EC2::Subnet::Id
    Default: {subnet}
Resources:
  ServiceSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group of the deployed service
      VpcId: !Ref VpcId
      SecurityGroupEgress:
        - IpProtocol: "-1"
          CidrIp: 0.0.0.0/0
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service: ecs-tasks.amazonaws.com
      ManagedPolicyArns:
        - {MANAGED_EXECUTION_POLICY}
  LogGroup:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: {log_group}
      RetentionInDays: 1
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
          Image: {image}
          Essential: true{command}
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
              awslogs-stream-prefix: {CONTAINER}
  Service:
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
            - !Ref SubnetId
          SecurityGroups:
            - !Ref ServiceSecurityGroup
Outputs:
  ClusterOut:
    Value: !GetAtt Cluster.Arn
  ServiceOut:
    Value: !GetAtt Service.Name
  LogGroupOut:
    Value: !Ref LogGroup
"#,
        log_group = row.log_group,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        image = row.image,
        desired = row.desired,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED: looked
/// at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, "DescribeStacks", &[("StackName", stack)], case).await;
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
    let body = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)], case).await;
    elements(&body, "ResourceStatusReason")
}

/// The task ARNs `ListTasks` reports for one deployed service — the tasks whose desired status
/// is `RUNNING`, which is that filter's default — looked at again until the service has placed
/// as many as it was asked for, or the deadline passes.
async fn placed_tasks(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    service: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs(
            router,
            "ListTasks",
            json!({ "cluster": cluster, "serviceName": service }),
            case,
        )
        .await;
        let arns: Vec<String> = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if arns.len() >= wanted || Instant::now() >= deadline {
            return arns;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// One task as `DescribeTasks` reports it, looked at again until `settled` holds of its
/// `lastStatus` or the deadline passes.
async fn task_reaching(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    task_arn: &str,
    settled: impl Fn(&str) -> bool,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(
            router,
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [task_arn] }),
            case,
        )
        .await;
        let task = described["tasks"][0].clone();
        let reported = task["lastStatus"].as_str().unwrap_or_default();
        if settled(reported) || Instant::now() >= deadline {
            return task;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The messages CloudWatch Logs holds in the streams of one group under the prefix the row's log
/// configuration states, looked at again until it holds any or the deadline passes.
async fn delivered_lines(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let lines = read_group(router, log_group).await;
        if !lines.is_empty() || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What one group holds right now.
async fn read_group(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
    let (status, body) = rpc(
        router,
        "logs",
        LOGS_TARGET_PREFIX,
        "FilterLogEvents",
        json!({ "logGroupName": log_group, "logStreamNamePrefix": CONTAINER }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect()
}

/// The service as `DescribeServices` reports it.
async fn described_service(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    service: &str,
) -> Value {
    let body = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": cluster, "services": [service] }),
        case,
    )
    .await;
    assert!(
        body["failures"].as_array().is_none_or(Vec::is_empty),
        "case {case:?}: expected Amazon ECS to hold the deployed service, got {body}"
    );
    body["services"][0].clone()
}

#[tokio::test]
async fn a_stack_deployed_fargate_service_holds_its_running_task_and_delivers_its_output() {
    let router = build_router();
    let (vpc, subnet) = default_network(&router, "the shared estate").await;

    for row in ROWS {
        let case = row.case;
        let body = template(row, &vpc, &subnet);
        cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", IAM_CAPABILITY),
            ],
            case,
        )
        .await;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
            stack_failures(&router, case, row.stack).await,
        );

        // The handles a client addresses the deployed estate by.
        let outputs = outputs_of(&described);
        let cluster = outputs
            .get("ClusterOut")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no ClusterOut output in {outputs:?}"));
        let service = outputs
            .get("ServiceOut")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no ServiceOut output in {outputs:?}"));
        assert_eq!(
            outputs.get("LogGroupOut").map(String::as_str),
            Some(row.log_group),
            "case {case:?}: the stack must report the group it declared, got {outputs:?}"
        );

        // The scheduler places the task set the declaration asked for...
        let wanted = usize::try_from(row.desired).expect("a count this host can hold");
        let tasks = placed_tasks(&router, case, &cluster, &service, wanted).await;
        assert_eq!(
            tasks.len(),
            wanted,
            "case {case:?}: expected {wanted} placed task ARN(s) for the deployed service, got \
             {tasks:?}"
        );

        // ...each task comes up and REPORTS running, with nothing of a stop on it...
        let launched = task_reaching(&router, case, &cluster, &tasks[0], |status| {
            status == RUNNING || status == STOPPED
        })
        .await;
        assert_eq!(
            launched["lastStatus"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected the placed task to run its long-lived program, got {launched}"
        );
        assert_eq!(
            launched["stopCode"].as_str(),
            None,
            "case {case:?}: a running task carries no stop code, got {launched}"
        );
        assert_eq!(
            launched["stoppedReason"].as_str(),
            None,
            "case {case:?}: a running task carries no stopped reason, got {launched}"
        );
        assert_eq!(
            launched["containers"][0]["exitCode"].as_i64(),
            None,
            "case {case:?}: a container that has not ended returns no exit code, got {launched}"
        );

        // ...what it wrote is delivered to the group the stack declared...
        if let Some(written) = row.writes {
            let lines = delivered_lines(&router, row.log_group).await;
            assert!(
                lines.iter().any(|line| line.contains(written)),
                "case {case:?}: expected {written:?} in the group {}, got {lines:?}",
                row.log_group
            );
        }

        // ...and it KEEPS reporting it, beside a service that holds the count it was asked
        // for: a container whose wait was taken as an account of work it did would have exited
        // inside this window, leaving the scheduler replacing a task for ever.
        let holding = Instant::now() + HOLD_WINDOW;
        loop {
            let held = task_reaching(&router, case, &cluster, &tasks[0], |_| true).await;
            assert_eq!(
                held["lastStatus"].as_str(),
                Some(RUNNING),
                "case {case:?}: expected the task to go on running, got {held}"
            );
            let reported = described_service(&router, case, &cluster, &service).await;
            assert_eq!(
                reported["runningCount"].as_u64(),
                Some(row.desired),
                "case {case:?}: expected the service to hold the count it was asked for, got \
                 {reported}"
            );
            if Instant::now() >= holding {
                break;
            }
            tokio::time::sleep(POLL).await;
        }

        // Nothing of this service has stopped: no task left the set it holds.
        let stopped = ecs(
            &router,
            "ListTasks",
            json!({
                "cluster": cluster,
                "serviceName": service,
                "desiredStatus": STOPPED,
            }),
            case,
        )
        .await;
        assert_eq!(
            stopped["taskArns"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: expected no stopped task of the service, got {stopped}"
        );
    }
}
