// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed Amazon ECS estate — a task execution role, an `awslogs` log
//! group, a cluster, a task definition and a service — really PLACES a task, and that task
//! reaches a TERMINAL launch state.
//!
//! RULE: an `AWS::ECS::Service` a stack declares is the service Amazon ECS's own
//! `CreateService` makes, so the scheduler maintains the task set its `DesiredCount` asks for:
//! `ListTasks(cluster, serviceName)` answers with one task ARN per task the scheduler placed,
//! and each of those tasks leaves its provisioning statuses for a terminal launch state —
//! `RUNNING` when Amazon ECS could assume the revision's execution role (a role is reached by
//! ASSUMING it, so its trust policy has to admit the `ecs-tasks` service principal), and
//! `STOPPED` when it could not. A task that runs has what its container writes delivered to the
//! group its `awslogs` configuration names, under the stream prefix that configuration states;
//! a task whose containers never started delivers nothing.
//!
//! Five crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls, IAM holds the role and the trust document in force
//! on it, CloudWatch Logs holds the group and what was delivered to it, Amazon EC2 holds the
//! network the placement resolves against, and Amazon ECS owns the service, the task and the
//! launch it attempts. Everything is driven through the real operations on one engine, so what
//! is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: the PRINCIPAL the deployed execution
//! role's trust policy admits — the `ecs-tasks` one Amazon ECS assumes it as, a different
//! service principal, and a document admitting that principal beside another — which is what
//! decides whether the placed task reaches `RUNNING` or `STOPPED`. The second test varies the
//! LAUNCH TYPE instead: a service of the EC2 launch type over the capacity its own stack
//! brought up.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output reader this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query request carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefixes of the awsJson services this suite drives.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The one container of every task definition below, which is also the `awslogs-stream-prefix`
/// its log configuration states.
const CONTAINER: &str = "app";

/// The line the container writes on its standard output, which is what the `awslogs` driver
/// delivers to the group the stack declared.
const SPOKEN_LINE: &str = "the deployed task started";

/// `Task$lastStatus` of a task that is running, and of one that has stopped: the two TERMINAL
/// launch states a placed task reaches.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The capability a create has to acknowledge for a template that names the IAM role it
/// creates: "if you have IAM resources with custom names, you must specify
/// CAPABILITY_NAMED_IAM" (`CreateStack$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The stack statuses a create is still in flight in, and the one a create finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, the task set the service's rollout places, and the launch each task then attempts — and
/// how often it looks again. Several times the span those moves take: the deadline is only
/// here so a failure is a failure rather than a hang, and every wait looks for its CONDITION
/// rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// How many of those looks a row that expects NO delivered event takes at the group before
/// judging it empty. Together with [`POLL`] a window far longer than the span a running task's
/// first delivery takes, so an emptiness this row asserts is one that held rather than one
/// read too early.
const QUIET_LOOKS: usize = 60;

/// The service principal Amazon ECS assumes a task's execution role as.
const ECS_TASKS: &str = "ecs-tasks.amazonaws.com";

/// One row: the trust policy the stack's execution role is deployed with, the launch state the
/// task the service places then reaches, and the names this row's own resources take so two
/// rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    /// The `Principal.Service` list the deployed role's trust policy admits.
    trusted: &'static [&'static str],
    /// The `Task$lastStatus` the placed task must reach.
    expected: &'static str,
    stack: &'static str,
    role: &'static str,
    log_group: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded trust: a role the ecs-tasks principal may assume",
        trusted: &[ECS_TASKS],
        expected: RUNNING,
        stack: "ecsrolestack-trusted",
        role: "ecsRoleStackTrusted",
        log_group: "/emulator/cfn/ecs-trusted",
        cluster: "ecsrolestacktrusted",
        family: "ecsrolestacktrustedtask",
        service: "ecsrolestacktrustedsvc",
    },
    Row {
        case: "a role trusting another service principal only",
        trusted: &["ec2.amazonaws.com"],
        expected: STOPPED,
        stack: "ecsrolestack-elsewhere",
        role: "ecsRoleStackElsewhere",
        log_group: "/emulator/cfn/ecs-elsewhere",
        cluster: "ecsrolestackelsewhere",
        family: "ecsrolestackelsewheretask",
        service: "ecsrolestackelsewheresvc",
    },
    Row {
        case: "a role trusting the ecs-tasks principal beside another",
        trusted: &["ecs.amazonaws.com", ECS_TASKS],
        expected: RUNNING,
        stack: "ecsrolestack-both",
        role: "ecsRoleStackBoth",
        log_group: "/emulator/cfn/ecs-both",
        cluster: "ecsrolestackboth",
        family: "ecsrolestackbothtask",
        service: "ecsrolestackbothsvc",
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
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
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
    let (status, body) = query(router, "cloudformation", "2010-05-15", action, members).await;
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

/// A default subnet of the account's own default VPC, read out of Amazon EC2 itself: the
/// network a deployment that creates none of its own places its workload in.
async fn default_subnet(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2(
        router,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "default-for-az"),
            ("Filter.1.Value.1", "true"),
        ],
        case,
    )
    .await;
    let subnets = elements(&body, "subnetId");
    assert!(
        !subnets.is_empty(),
        "case {case:?}: the account's default VPC must hold a default subnet, got {body}"
    );
    subnets[0].clone()
}

/// The trust policy one row's role is deployed with: the principals it admits, as the YAML the
/// template carries.
fn trust_document(row: &Row) -> String {
    let services = row
        .trusted
        .iter()
        .map(|service| format!("\n                  - {service}"))
        .collect::<String>();
    format!(
        r#"
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service:{services}"#
    )
}

/// The template one row deploys: the execution role whose trust policy the row varies, the log
/// group the container's output is delivered to, and the cluster, task definition and service
/// that place the task.
fn template(row: &Row, subnet: &str) -> String {
    format!(
        r#"Resources:
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      RoleName: {role}
      AssumeRolePolicyDocument:{trust}
      Policies:
        - PolicyName: delivery
          PolicyDocument:
            Version: "2012-10-17"
            Statement:
              - Effect: Allow
                Action:
                  - logs:CreateLogStream
                  - logs:PutLogEvents
                Resource: "*"
  LogGroup:
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
          Image: public.ecr.aws/amazonlinux/amazonlinux:2
          Essential: true
          Command:
            - sh
            - -c
            - echo "{SPOKEN_LINE}"; sleep 3600
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
      DesiredCount: 1
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          Subnets:
            - {subnet}
Outputs:
  ClusterOut:
    Value: !GetAtt Cluster.Arn
  ServiceOut:
    Value: !GetAtt Service.Name
  LogGroupOut:
    Value: !Ref LogGroup
"#,
        role = row.role,
        trust = trust_document(row),
        log_group = row.log_group,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED:
/// looked at again until its status is terminal, or the deadline passes.
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

/// The task ARNs `ListTasks` reports for one deployed service, looked at again until at least
/// one appears or the deadline passes: the scheduler places the task set the clock owes, so
/// what is waited for is the CONDITION rather than a span of it.
async fn placed_tasks(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    service: &str,
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
        if !arns.is_empty() || Instant::now() >= deadline {
            return arns;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// One task as `DescribeTasks` reports it once it has reached a TERMINAL launch state, looked
/// at again until it does or the deadline passes.
async fn launched_task(
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
        let reported = task["lastStatus"].as_str().unwrap_or_default();
        if reported == RUNNING || reported == STOPPED || Instant::now() >= deadline {
            return task;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The messages CloudWatch Logs holds in the streams of one log group under the prefix the
/// row's log configuration states.
async fn delivered(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
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

/// The delivered lines of one group once it holds any, looked at again until it does or the
/// deadline passes.
async fn delivered_lines(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let lines = delivered(router, log_group).await;
        if !lines.is_empty() || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What one group holds after a WINDOW of being looked at again: the first non-empty reading
/// if any look finds one, and otherwise nothing.
///
/// What a row that expects NO delivery needs: an empty group proves nothing on its own, since
/// a delivery is a clock-owed move, so the group is read over a window far past the span a
/// running task's first delivery takes.
async fn quiet_group(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL).await;
        let lines = delivered(router, log_group).await;
        if !lines.is_empty() {
            return lines;
        }
    }
    Vec::new()
}

#[tokio::test]
async fn a_stack_deployed_ecs_service_places_a_task_that_reaches_a_launch_state() {
    let router = build_router();
    let subnet = default_subnet(&router, "the shared estate").await;

    for row in ROWS {
        let case = row.case;
        let body = template(row, &subnet);
        cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", NAMED_IAM),
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
            service, row.service,
            "case {case:?}: the stack must report the service it created, got {outputs:?}"
        );

        // The scheduler PLACES the task set the declaration asked for...
        let tasks = placed_tasks(&router, case, &cluster, &service).await;
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected one placed task ARN for the deployed service, got {tasks:?}"
        );

        // ...and the task reaches the launch state the trust policy the stack deployed
        // decides.
        let launched = launched_task(&router, case, &cluster, &tasks[0]).await;
        let reported = launched["lastStatus"].as_str().unwrap_or_default();
        assert_eq!(
            reported, row.expected,
            "case {case:?}: expected the placed task to reach {}, got {launched}",
            row.expected
        );

        // A task that RAN delivered what its container wrote to the group the stack declared;
        // one whose containers never started delivered nothing.
        if row.expected == RUNNING {
            let lines = delivered_lines(&router, row.log_group).await;
            assert!(
                lines.iter().any(|line| line.contains(SPOKEN_LINE)),
                "case {case:?}: expected the container's output in {}, got {lines:?}",
                row.log_group
            );
        } else {
            let lines = quiet_group(&router, row.log_group).await;
            assert!(
                lines.is_empty(),
                "case {case:?}: expected no delivery into {}, got {lines:?}",
                row.log_group
            );
        }
    }
}

/// The same rule along the other dimension it generalises over: a service of the EC2 LAUNCH
/// TYPE, whose task the scheduler places on the capacity its own stack brought up — an
/// instance of the ECS-optimized image whose user data joins the cluster.
#[tokio::test]
async fn an_ec2_launch_type_service_converges_on_the_capacity_its_stack_brought_up() {
    let router = build_router();
    let case = "a service of the EC2 launch type over the stack's own instance";
    let subnet = default_subnet(&router, case).await;
    let stack = "ecsec2stack";
    let cluster = "ecsec2cluster";
    let service = "ecsec2service";
    let body = format!(
        r#"Resources:
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
  Instance:
    Type: AWS::EC2::Instance
    Properties:
      ImageId: '{{{{resolve:ssm:/aws/service/ecs/optimized-ami/amazon-linux-2/recommended/image_id}}}}'
      InstanceType: t3.micro
      SubnetId: {subnet}
      UserData:
        Fn::Base64: |
          #!/bin/bash
          echo ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {cluster}
      NetworkMode: bridge
      RequiresCompatibilities:
        - EC2
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: public.ecr.aws/amazonlinux/amazonlinux:2
          Essential: true
          Cpu: 128
          Memory: 256
          Command:
            - sh
            - -c
            - echo "{SPOKEN_LINE}"; sleep 3600
  Service:
    Type: AWS::ECS::Service
    DependsOn: Instance
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: 1
      LaunchType: EC2
Outputs:
  ClusterOut:
    Value: !GetAtt Cluster.Arn
"#
    );
    cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &body)],
        case,
    )
    .await;
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
        stack_failures(&router, case, stack).await,
    );
    let deadline = Instant::now() + DEADLINE;
    let mut last;
    loop {
        last = ecs(
            &router,
            "DescribeServices",
            json!({ "cluster": cluster, "services": [service] }),
            case,
        )
        .await;
        if last["services"][0]["runningCount"].as_u64() == Some(1) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        last["services"][0]["runningCount"].as_u64(),
        Some(1),
        "case {case:?}: the service must run the task its declaration asked for, got {last}"
    );
}

/// The stack's own records agree with what Amazon ECS holds: the service resource's physical
/// id is the ARN Amazon ECS minted, which is the handle every call about the service takes.
#[tokio::test]
async fn the_stacks_service_resource_is_the_arn_amazon_ecs_minted() {
    let router = build_router();
    let case = "the physical id of a deployed service";
    let subnet = default_subnet(&router, case).await;
    let row = &ROWS[0];
    let stack = "ecsrolestack-identity";
    let cluster = "ecsrolestackidentity";
    let body = template(row, &subnet)
        .replace(row.role, "ecsRoleStackIdentity")
        .replace(row.log_group, "/emulator/cfn/ecs-identity")
        .replace(row.family, "ecsrolestackidentitytask")
        .replace(row.service, "ecsrolestackidentitysvc")
        .replace(row.cluster, cluster);
    cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", NAMED_IAM),
        ],
        case,
    )
    .await;
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
        stack_failures(&router, case, stack).await,
    );
    let resources = cloudformation(
        &router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", "Service")],
        case,
    )
    .await;
    let physical = element(&resources, "PhysicalResourceId");
    let held = ecs(
        &router,
        "DescribeServices",
        json!({ "cluster": cluster, "services": [physical.clone()] }),
        case,
    )
    .await;
    assert_eq!(
        held["services"][0]["serviceArn"].as_str(),
        Some(physical.as_str()),
        "case {case:?}: expected Amazon ECS to hold the service the stack reports as \
         {physical}, got {held}"
    );
}
