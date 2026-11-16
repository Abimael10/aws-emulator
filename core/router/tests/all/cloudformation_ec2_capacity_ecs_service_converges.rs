// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an ECS cluster, the Amazon EC2 capacity it runs on and
//! an `AWS::ECS::Service` behind a target group CONVERGES: the service the stack created is
//! read back through `DescribeServices` and its `runningCount` rises to the `DesiredCount` the
//! template declared, on the container instances the stack's own Auto Scaling group launched.
//!
//! RULE: an `AWS::ECS::Service` is created in Amazon ECS by `CreateService`, which "runs and
//! maintains your desired number of tasks from a specified task definition", and "the EC2
//! launch type runs your tasks on Amazon EC2 instances registered to your cluster" — so a
//! service a stack declared is maintained by the same scheduler as one a client created, and it
//! converges on its declared count as the capacity its cluster is given registers, whichever
//! order the two arrive in.
//!
//! Five crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls, Amazon EC2 holds the launch template and the
//! instances, Amazon EC2 Auto Scaling launches them into the group, Amazon ECS owns the cluster
//! that registers them and the service that places tasks on them, and Elastic Load Balancing v2
//! holds the target group the service binds. Everything is driven through the real operations
//! on one engine, so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: how many tasks the service is asked
//! for (one, the recorded count, and two), and whether the service names its launch type or
//! leaves it to the default.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, the agent registration its instances owe their cluster and the tasks the scheduler then
/// places — and how often it looks again. The deadline is only here so a failure is a failure
/// rather than a hang.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// The stack statuses a create passes through, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Task$lastStatus` of a task whose containers have started.
const RUNNING: &str = "RUNNING";

/// One row: how many tasks the stack's service is asked for, whether it names its launch type,
/// and the names its own resources take so two rows never collide.
struct Row {
    case: &'static str,
    desired: usize,
    launch_type: Option<&'static str>,
    /// How the row's launch template names the AMI its instances boot: a literal id, or the
    /// `{{resolve:ssm:…}}` dynamic reference to the public parameter AWS publishes the
    /// ECS-optimized AMI's id under.
    image: &'static str,
    /// How the task execution role is granted the `logs:CreateLogStream` / `logs:PutLogEvents`
    /// the `awslogs` driver is initialised with.
    grant: Grant,
    stack_name: &'static str,
    load_balancer: &'static str,
    target_group: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

/// How one row's template grants the execution role the permission the `awslogs` driver's
/// initialisation needs: AWS's own managed policy for the purpose, or an inline policy of the
/// template's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    ManagedPolicy,
    InlinePolicy,
}

/// A literal AMI id, well-formed, which is all a launch needs.
const LITERAL_IMAGE: &str = "ami-0123456789abcdef0";

/// The dynamic reference to the public Systems Manager parameter AWS publishes the
/// ECS-optimized AMI's id under — the way AWS's own ECS cluster templates pick that AMI.
const SSM_IMAGE: &str =
    "'{{resolve:ssm:/aws/service/ecs/optimized-ami/amazon-linux-2/recommended/image_id}}'";

const ROWS: &[Row] = &[
    Row {
        case: "the recorded count: one task on declared EC2 capacity",
        desired: 1,
        launch_type: Some("EC2"),
        image: LITERAL_IMAGE,
        grant: Grant::ManagedPolicy,
        stack_name: "ec2cap-one",
        load_balancer: "ec2cap-one-alb",
        target_group: "ec2cap-one-tg",
        cluster: "ec2cap-one-cluster",
        family: "ec2cap-one-task",
        service: "ec2cap-one-service",
    },
    Row {
        case: "two tasks on the same declared capacity",
        desired: 2,
        launch_type: Some("EC2"),
        image: LITERAL_IMAGE,
        grant: Grant::InlinePolicy,
        stack_name: "ec2cap-two",
        load_balancer: "ec2cap-two-alb",
        target_group: "ec2cap-two-tg",
        cluster: "ec2cap-two-cluster",
        family: "ec2cap-two-task",
        service: "ec2cap-two-service",
    },
    Row {
        case: "no launch type at all, and the AMI picked by its public SSM parameter",
        desired: 1,
        launch_type: None,
        image: SSM_IMAGE,
        grant: Grant::ManagedPolicy,
        stack_name: "ec2cap-default",
        load_balancer: "ec2cap-default-alb",
        target_group: "ec2cap-default-tg",
        cluster: "ec2cap-default-cluster",
        family: "ec2cap-default-task",
        service: "ec2cap-default-service",
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
        service_com_amazonaws_autoscaling_autoscaling_2011_01_01::EmulatedService::new(Arc::clone(
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
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
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

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon ECS operation over its awsJson1_1 wire.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The template one row deploys: the network, the capacity (a launch template whose user data
/// configures the container agent with the stack's own cluster, and the Auto Scaling group that
/// launches it), the target group the service binds, and the ECS cluster, task definition and
/// service themselves.
fn template(row: &Row) -> String {
    let launch_type = match row.launch_type {
        Some(launch_type) => format!("      LaunchType: {launch_type}\n"),
        None => String::new(),
    };
    // The permission the `awslogs` driver is initialised with: Amazon ECS creates the stream as
    // the task execution role, so the role has to carry it either way.
    let grant = match row.grant {
        Grant::ManagedPolicy => concat!(
            "      ManagedPolicyArns:\n",
            "        - arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy\n",
        )
        .to_string(),
        Grant::InlinePolicy => concat!(
            "      Policies:\n",
            "        - PolicyName: logs\n",
            "          PolicyDocument:\n",
            "            Version: \"2012-10-17\"\n",
            "            Statement:\n",
            "              - Effect: Allow\n",
            "                Action:\n",
            "                  - logs:CreateLogStream\n",
            "                  - logs:PutLogEvents\n",
            "                Resource: \"*\"\n",
        )
        .to_string(),
    };
    format!(
        r##"Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.0.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.0.1.0/24
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.0.2.0/24
      AvailabilityZone: {REGION}b
  TargetGroup:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {target_group}
      Port: 80
      Protocol: HTTP
      TargetType: instance
      VpcId: !Ref Vpc
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {load_balancer}
      Scheme: internal
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: 80
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref TargetGroup
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
  LaunchTemplate:
    Type: AWS::EC2::LaunchTemplate
    Properties:
      LaunchTemplateName: {cluster}-lt
      LaunchTemplateData:
        ImageId: {image}
        InstanceType: t3.micro
        UserData:
          Fn::Base64: !Sub "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
  Capacity:
    Type: AWS::AutoScaling::AutoScalingGroup
    DependsOn: Cluster
    Properties:
      MinSize: "1"
      MaxSize: "2"
      DesiredCapacity: "1"
      LaunchTemplate:
        LaunchTemplateId: !Ref LaunchTemplate
        Version: !GetAtt LaunchTemplate.LatestVersionNumber
      VPCZoneIdentifier:
        - !Ref SubnetA
  LogGroup:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: /ecs/{family}
      RetentionInDays: 7
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      RoleName: {family}-execution
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal:
              Service: ecs-tasks.amazonaws.com
            Action: sts:AssumeRole
{grant}  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {family}
      NetworkMode: bridge
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/nginx/nginx:latest
          Memory: 256
          Essential: true
          PortMappings:
            - ContainerPort: 80
              Protocol: tcp
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
              awslogs-stream-prefix: app
  Service:
    Type: AWS::ECS::Service
    DependsOn:
      - Capacity
      - Listener
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired}
{launch_type}      LoadBalancers:
        - ContainerName: app
          ContainerPort: 80
          TargetGroupArn: !Ref TargetGroup
Outputs:
  ClusterName:
    Value: !GetAtt Cluster.Arn
  ServiceName:
    Value: !GetAtt Service.Name
  TargetGroupArn:
    Value: !Ref TargetGroup
"##,
        target_group = row.target_group,
        load_balancer = row.load_balancer,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        desired = row.desired,
        image = row.image,
        launch_type = launch_type,
        grant = grant,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED:
/// looked at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeStacks, got {status} with body {body}"
        );
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
async fn stack_failures(router: &MultiServiceRouter, case: &str, stack_name: &str) -> Vec<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeStackEvents, got {status} with body {body}"
    );
    elements(&body, "ResourceStatusReason")
}

/// The service one row's stack created, as `DescribeServices` reports it once its
/// `runningCount` has reached the count the template declared — read again until it does, or
/// the deadline passes.
async fn settled_service(router: &MultiServiceRouter, case: &str, row: &Row) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, services) = ecs(
            router,
            "DescribeServices",
            &json!({ "cluster": row.cluster, "services": [row.service] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeServices, got {status} with {services}"
        );
        let settled =
            services["services"][0]["runningCount"].as_u64() == Some(row.desired as u64);
        if settled || Instant::now() >= deadline {
            return services;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `lastStatus` of every task the service runs, read through the listing and the describe a
/// client polls.
async fn task_statuses(router: &MultiServiceRouter, case: &str, row: &Row) -> Vec<String> {
    let (status, listed) = ecs(
        router,
        "ListTasks",
        &json!({ "cluster": row.cluster, "serviceName": row.service }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListTasks, got {status} with body {listed}"
    );
    let arns: Vec<&str> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let (status, described) = ecs(
        router,
        "DescribeTasks",
        &json!({ "cluster": row.cluster, "tasks": arns }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeTasks, got {status} with body {described}"
    );
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| task["lastStatus"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_stack_declared_ecs_service_converges_on_the_capacity_the_stack_declared() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = template(row);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack_name),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateStack, got {status} with body {created}"
        );

        let described = settled_stack(&router, case, row.stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: the stack must complete; what failed was {:?} and the reasons it \
             recorded were {:?}",
            stack_failures(&router, case, row.stack_name).await,
            elements(&described, "StackStatusReason"),
        );

        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("ServiceName").map(String::as_str),
            Some(row.service),
            "case {case:?}: the stack must report the service it created, got {outputs:?}"
        );

        // The service the stack created is READ BACK through `DescribeServices`, and it
        // converges on the count the template declared.
        let services = settled_service(&router, case, row).await;
        let service = &services["services"][0];
        assert_eq!(
            service["serviceName"].as_str(),
            Some(row.service),
            "case {case:?}: Amazon ECS must hold the service the stack created, got {services}"
        );
        assert_eq!(
            service["runningCount"].as_u64(),
            Some(row.desired as u64),
            "case {case:?}: the service must run the task set the stack asked for, got \
             {services} with the service events {:?}",
            service["events"]
        );

        // ...and each of those tasks really reached `RUNNING`.
        let statuses = task_statuses(&router, case, row).await;
        assert_eq!(
            statuses.len(),
            row.desired,
            "case {case:?}: expected one task per asked-for task, got {statuses:?}"
        );
        for status in &statuses {
            assert_eq!(
                status, RUNNING,
                "case {case:?}: expected every task of the service to be {RUNNING}, got \
                 {statuses:?}"
            );
        }
    }
}
