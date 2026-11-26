// SPDX-License-Identifier: CC0-1.0

//! The Fargate service a CloudFormation stack deploys RUNS IN THE SECURITY GROUP that same
//! stack created, and the boundary of the rule its sibling suite pins: a container whose
//! program really ENDS stops its task as the exit of an essential container.
//!
//! RULE under test, in two parts:
//!
//! * **An `AWS::ECS::Service` a stack declares is the service Amazon ECS's own `CreateService`
//!   made, with the network the declaration named**: the service that ECS answers for under
//!   the physical id the stack published carries an `awsvpcConfiguration` whose `subnets` and
//!   `securityGroups` are the ids the OWNING services minted for the resources the same stack
//!   declared — so the group an `AWS::EC2::SecurityGroup` created is the group the placement
//!   really runs under, not a name nothing resolves.
//! * **`STOPPED` with `TaskStopCode` `EssentialContainerExited` is for a container whose
//!   program really ended**: a task whose essential container runs a program that comes to its
//!   own end stops with that code and the status the program ended with, and what the container
//!   wrote before it ended is still delivered to the group its `awslogs` configuration names.
//!   A container whose program is still running is the other half of the rule, which
//!   [`crate::cfn_deployed_fargate_service_holds_its_running_task`] pins over the forms a
//!   long-lived program takes.
//!
//! Sources: `smithy_query(query="operation",operation="DescribeServices")` for
//! `Service$networkConfiguration` and `failures` ("any failures associated with the call");
//! `smithy_query(query="shape",shape="AwsVpcConfiguration")` for `subnets` ("The IDs of the
//! subnets associated with the task or service") and `securityGroups` ("The IDs of the security
//! groups associated with the task or service"); `smithy_query(query="operation",
//! operation="DescribeTasks")` for `Task$stopCode`, `Task$stoppedReason` and
//! `Container$exitCode`; `smithy_query(query="shape",shape="TaskStopCode")` for the member a
//! task whose essential container exited stops with.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls and publishes each resource's physical id, Amazon EC2
//! holds the security group and the default subnet, Amazon ECS owns the service and the task it
//! places, and CloudWatch Logs holds what the container delivered. Nothing reaches into a
//! store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output readers this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions a CloudFormation and an ec2Query request carry.
const CFN_API_VERSION: &str = "2010-05-15";
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The one container of the revision below, which is also the `awslogs-stream-prefix` its log
/// configuration states.
const CONTAINER: &str = "app";

/// The line that container writes before its program comes to its end.
const SPOKEN_LINE: &str = "the deployed task ran and ended";

/// `Task$lastStatus` of a task that has stopped, and the `TaskStopCode` of one whose essential
/// container exited.
const STOPPED: &str = "STOPPED";
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The capability a create has to acknowledge for a template that creates IAM resources
/// (`CreateStack$Capabilities`).
const IAM_CAPABILITY: &str = "CAPABILITY_IAM";

/// The stack statuses a create is still in flight in, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The managed policy AWS publishes for a task execution role
/// (`AmazonECSTaskExecutionRolePolicy`, the task execution role of the Amazon ECS Developer
/// Guide), the only thing granting the deployed role the delivery the `awslogs` driver makes as
/// it.
const MANAGED_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The names this suite's one stack gives the resources it declares.
const STACK: &str = "owngroupfargate";
const LOG_GROUP: &str = "/emulator/cfn/own-group";
const CLUSTER: &str = "owngroupfargatecluster";
const FAMILY: &str = "owngroupfargatetask";
const SERVICE: &str = "owngroupfargatesvc";

/// How long a wait keeps looking for the clock-owed moves this suite depends on, and how often
/// it looks again. Many times the span those moves take: the deadline is only here so a failure
/// is a failure rather than a hang, and every wait looks for its CONDITION.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// flow spans CloudFormation, IAM, Amazon EC2, Amazon ECS and CloudWatch Logs, so what answers
/// each call here is the service the binary mounts rather than a hand-picked subset.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
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

/// The account's own default VPC and one of its default subnets, read out of Amazon EC2 itself.
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

/// The template this suite deploys: a security group of the default VPC, the execution role
/// whose permissions are the AWS managed policy alone, the log group the container's output goes
/// to, and the cluster, Fargate revision and service that place the task — whose container
/// writes one line and then comes to its end.
fn template(vpc: &str, subnet: &str) -> String {
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
      LogGroupName: {LOG_GROUP}
      RetentionInDays: 1
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {CLUSTER}
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {FAMILY}
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: public.ecr.aws/docker/library/busybox:1.36
          Essential: true
          Command:
            - sh
            - -c
            - echo "{SPOKEN_LINE}"
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
              awslogs-stream-prefix: {CONTAINER}
  Service:
    Type: AWS::ECS::Service
    Properties:
      ServiceName: {SERVICE}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: 1
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
"#
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED.
async fn settled_stack(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, "DescribeStacks", &[("StackName", STACK)], case).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        let in_flight = reported == CREATE_IN_PROGRESS || reported == ROLLBACK_IN_PROGRESS;
        if !in_flight || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every `ResourceStatusReason` the stack's event stream carries.
async fn stack_failures(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)], case).await;
    elements(&body, "ResourceStatusReason")
}

/// The ARN of the one task the deployed service placed, looked for until it has one.
async fn placed_task(router: &MultiServiceRouter, case: &str, cluster: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = ecs(
            router,
            "ListTasks",
            json!({ "cluster": cluster, "serviceName": SERVICE }),
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
        if let Some(first) = arns.first() {
            return first.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployed service to place a task, got {listed}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One task as `DescribeTasks` reports it once it has STOPPED.
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
            "DescribeTasks",
            json!({ "cluster": cluster, "tasks": [task_arn] }),
            case,
        )
        .await;
        let task = described["tasks"][0].clone();
        if task["lastStatus"].as_str() == Some(STOPPED) || Instant::now() >= deadline {
            return task;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The messages CloudWatch Logs holds under the stream prefix the configuration states, looked
/// for until the group holds any.
async fn delivered_lines(router: &MultiServiceRouter) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = rpc(
            router,
            "logs",
            LOGS_TARGET_PREFIX,
            "FilterLogEvents",
            json!({ "logGroupName": LOG_GROUP, "logStreamNamePrefix": CONTAINER }),
        )
        .await;
        let lines: Vec<String> = if status == 200 {
            body["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        } else {
            Vec::new()
        };
        if !lines.is_empty() || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_stack_deployed_service_runs_in_the_group_the_stack_created() {
    let case = "a deployed Fargate service in the security group its own stack created";
    let router = build_router();
    let (vpc, subnet) = default_network(&router, case).await;
    let body = template(&vpc, &subnet);
    cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            ("Capabilities.member.1", IAM_CAPABILITY),
        ],
        case,
    )
    .await;
    let described = settled_stack(&router, case).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
        stack_failures(&router, case).await,
    );
    let outputs = outputs_of(&described);
    let cluster = outputs
        .get("ClusterOut")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no ClusterOut output in {outputs:?}"));

    // The ids the OWNING services minted for the resources the stack declared.
    let resources = cloudformation(
        &router,
        "DescribeStackResources",
        &[("StackName", STACK)],
        case,
    )
    .await;
    let physical = physical_ids_of(&resources);
    let group = physical
        .get("ServiceSecurityGroup")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no group id in {physical:?}"));
    let held = ec2(
        &router,
        "DescribeSecurityGroups",
        &[("GroupId.1", &group)],
        case,
    )
    .await;
    assert_eq!(
        elements(&held, "groupId"),
        vec![group.clone()],
        "case {case:?}: expected Amazon EC2 to hold the group the stack created, got {held}"
    );

    // ...and the deployed service really runs under them.
    let reported = ecs(
        &router,
        "DescribeServices",
        json!({ "cluster": cluster, "services": [SERVICE] }),
        case,
    )
    .await;
    assert!(
        reported["failures"].as_array().is_none_or(Vec::is_empty),
        "case {case:?}: expected Amazon ECS to hold the deployed service, got {reported}"
    );
    let awsvpc = &reported["services"][0]["networkConfiguration"]["awsvpcConfiguration"];
    assert_eq!(
        awsvpc["securityGroups"].as_array().map(Vec::as_slice),
        Some([json!(group)].as_slice()),
        "case {case:?}: expected the placement to run under the stack's own group, got {reported}"
    );
    assert_eq!(
        awsvpc["subnets"].as_array().map(Vec::as_slice),
        Some([json!(subnet)].as_slice()),
        "case {case:?}: expected the placement in the subnet the stack named, got {reported}"
    );

    // The BOUNDARY of the long-lived-program rule: this container's program comes to its own
    // end, so its task stops as the exit of an essential container with the status that program
    // ended with — and the line it wrote first is delivered all the same.
    let task_arn = placed_task(&router, case, &cluster).await;
    let ended = stopped_task(&router, case, &cluster, &task_arn).await;
    assert_eq!(
        ended["lastStatus"].as_str(),
        Some(STOPPED),
        "case {case:?}: expected the task of an ended program to stop, got {ended}"
    );
    assert_eq!(
        ended["stopCode"].as_str(),
        Some(ESSENTIAL_CONTAINER_EXITED),
        "case {case:?}: expected the stop of a container that ended, got {ended}"
    );
    assert_eq!(
        ended["containers"][0]["exitCode"].as_i64(),
        Some(0),
        "case {case:?}: expected the status the program ended with, got {ended}"
    );
    let lines = delivered_lines(&router).await;
    assert!(
        lines.iter().any(|line| line.contains(SPOKEN_LINE)),
        "case {case:?}: expected the container's line in {LOG_GROUP}, got {lines:?}"
    );
}
