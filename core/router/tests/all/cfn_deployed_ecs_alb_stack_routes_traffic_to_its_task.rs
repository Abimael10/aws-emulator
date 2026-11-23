// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an Application Load Balancer, its `ip` target group and
//! listener, an Amazon ECS cluster, an `awslogs`-configured task definition and the service bound
//! to that group converges all the way to a ROUTED request: the client that deployed it reads a
//! terminal verdict out of every service it polls, and the address the stack published answers it.
//!
//! RULE: **a stack that declares that estate deploys it in the services that own each part, so a
//! client polling the deployed estate reaches a terminal state at every step — the stack settles
//! at `CREATE_COMPLETE`, the service's `runningCount` reaches the `DesiredCount` it declared with
//! its `PRIMARY` deployment at `rolloutState` `COMPLETED`, each running task's own address is a
//! `healthy` target of the declared group, and a request to the address the load balancer
//! PUBLISHED is answered by the server that task's container runs, with that target's own status
//! and bytes — whatever count the declaration asks for, however the client addresses the published
//! load balancer, and whichever document it asks for.**
//!
//! The grounds. An `AWS::ECS::Service` is created by `CreateService`, so the service really runs
//! the task set its `DesiredCount` asks for and converges on it ("when the service reaches a steady
//! state, the deployment transitions to a `COMPLETED` state", `Deployment$rolloutState`, whose
//! value set is `COMPLETED | FAILED | IN_PROGRESS`); `LoadBalancer$targetGroupArn` binds those
//! tasks into the declared group and "tasks that use the `awsvpc` network mode are associated with
//! an elastic network interface", so each registration is a task's own address;
//! `TargetHealth$State` `healthy` is the state of a target the load balancer may send requests to,
//! so a target reported healthy is one a request REACHES; `LoadBalancer$DNSName` is "the public DNS
//! name of the load balancer", which is the address a client is handed and sends traffic to; and
//! what answers there is what the container's own `command` — "the command that's passed to the
//! container" — started, serving out of the container's own filesystem. The load balancer's own
//! `502` is the answer for a target it cannot establish a connection to, so a registration reported
//! healthy is never answered with one.
//!
//! Six crates have to agree for that to hold, which is why this suite lives here: CloudFormation
//! makes every create call, Amazon EC2 owns the network the placement resolves against and the
//! security-group rules that decide whether a check or a request ARRIVES, Amazon ECS owns the
//! service, its tasks and the container each one runs, IAM holds the execution role the `awslogs`
//! driver is initialised as, Elastic Load Balancing v2 publishes the address, holds the
//! registrations and answers the client, and CloudWatch Logs holds the group that driver delivers
//! to. Everything is driven through the real operations on ONE engine, so every assertion is about
//! what each service persisted.
//!
//! What the siblings leave open, and why this suite is not one of them.
//! [`crate::cloudformation_alb_fargate_service_converges`] deploys this estate and stops at the
//! health verdict — it never sends the request the published address exists for;
//! [`crate::alb_published_address_reaches_its_ecs_task`] sends that request but stands the topology
//! up through the services' own APIs rather than by deploying a stack; and
//! [`crate::cfn_deployed_ecs_service_task_reaches_a_launch_state`] deploys an `awslogs`-configured
//! service with no load balancer in front of it at all. The whole deploy-and-route workflow — a
//! template in, a `200` out of the address it published — is the state no suite held.
//!
//! The rows span what the rule generalises over: HOW the client addresses the published load
//! balancer (a real connection DIALLED to the authority the stack's output carries, and that same
//! authority sent as a `Host`), WHICH document it asks for (the page the container's program wrote
//! at the root of the directory its server serves, one it wrote deeper in that directory, and a
//! path the container holds nothing at — the TARGET's own `404`, which is a status of the target
//! and not of the load balancer), and HOW MANY tasks the declaration asks for (the recorded single
//! task, and a declaration asking for two, so the group holds one registration per task and the
//! answer is still a task's own document).
//!
//! Every clock-owed move — the stack's walk, the tasks' start, the registrations they owe, the
//! health the load balancer's own checks determine — is POLLED for until it holds, never slept for,
//! and each deadline is generous: a row asserts that the move HAPPENS, not how fast this host runs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The fixtures for "a client on the address an Application Load Balancer published": the one
// hand-written raw HTTP/1.1 exchange on a published authority, the host-style request on the
// plane, the hostname rule that says which task's container wrote a document, and the state a
// target the checks reached carries. Owned by the suite whose header invites a suite measuring
// another rule of this topology to call them rather than keep a second copy
// ([`crate::ecs_alb_serves_its_awsvpc_tasks_container`]).
use crate::ecs_alb_serves_its_awsvpc_tasks_container::{
    container_hostname, dialled, get, Answered, HEALTHY,
};
// The ONE percent-encoder, element readers, keyed-list pairing, output reader and address-in-block
// test this test crate keeps for awsQuery suites (`all/query_wire.rs`: "Nothing new should write
// another one — call this module").
use crate::query_wire::{block_contains, elements, form_encode, opt_element, outputs_of, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions a CloudFormation and an Elastic Load Balancing v2 request carry.
const CFN_API_VERSION: &str = "2010-05-15";
const ELB_API_VERSION: &str = "2015-12-01";

/// The `X-Amz-Target` prefixes of the two awsJson services this suite reads through.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// How long a wait for a clock-owed move gives up after, and how often it looks again. Generous
/// on purpose: the deadline is only here so a condition nothing will ever satisfy reads as a
/// failure instead of a hang, and every wait below looks for its CONDITION rather than for a span
/// to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// The stack statuses a create is still in flight in, and the one it finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Deployment$status` of the deployment a client waits on, and the `Deployment$rolloutState` of
/// one that has reached its steady state.
const PRIMARY: &str = "PRIMARY";
const ROLLOUT_COMPLETED: &str = "COMPLETED";

/// The network every stack below declares: the block its VPC takes, and the two subnets its load
/// balancer and its tasks are placed in.
const VPC_CIDR: &str = "10.40.0.0/16";
const SUBNET_A_CIDR: &str = "10.40.1.0/24";
const SUBNET_B_CIDR: &str = "10.40.2.0/24";

/// The port the container's server listens on, which is the `ContainerPort` the revision declares,
/// the port the binding names and the target group's own.
const PORT: u16 = 80;

/// The `HealthCheckIntervalSeconds` every declared target group states: the shortest the member
/// admits ("the range is 5-300"), so a suite waiting for a verdict waits the least the
/// configuration allows.
const HEALTH_CHECK_INTERVAL_SECONDS: u16 = 5;

/// The marker the container's own program writes into every document it serves, so a body anything
/// else produced fails the row. Carried literally in [`CONTAINER_COMMAND`] below, because a
/// container's `command` is text the revision holds.
const MARKER: &str = "ROUTED-THROUGH-THE-STACK";

/// The container's program: it writes its own page at the root of the directory it serves and a
/// second copy of it deeper in that directory, each carrying the container's own hostname, and then
/// runs the image's own small HTTP server in the foreground — which is what keeps the task
/// `RUNNING` and what answers the load balancer.
const CONTAINER_COMMAND: &str = concat!(
    "mkdir -p /www/pages",
    " && echo ROUTED-THROUGH-THE-STACK $(hostname) > /www/index.html",
    " && echo ROUTED-THROUGH-THE-STACK $(hostname) > /www/pages/deep.html",
    " && httpd -f -p 80 -h /www",
);

/// The paths the rows ask for: the root of the served directory, a document deeper inside it, and
/// a path the container holds nothing at.
const ROOT_PATH: &str = "/";
const DEEP_PATH: &str = "/pages/deep.html";
const MISSING_PATH: &str = "/pages/not-written.html";

/// The status the target answers a document it holds with, and the one its own server answers a
/// path its document root holds nothing at.
const SERVED_STATUS: u16 = 200;
const NOT_FOUND_STATUS: u16 = 404;

/// The status the LOAD BALANCER answers for a target it cannot establish a connection to, which is
/// what no row of a healthy registration may be answered with. Named here so a row's message says
/// what the status it got would have meant.
const TARGET_UNREACHABLE_STATUS: u16 = 502;

/// How a client ADDRESSES the load balancer the stack deployed.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// A real connection to the authority the stack's output carries, which is what the client
    /// handed that address opens.
    Dialled,
    /// That same authority sent as the request's `Host`.
    HostStyle,
}

/// One request row: how the client addresses the published load balancer, what it asks for, and
/// what it must be answered with.
struct Row {
    case: &'static str,
    addressing: Addressing,
    path: &'static str,
    status: u16,
}

const ROWS: &[Row] = &[
    Row {
        case: "the published address dialled for the page at the served root",
        addressing: Addressing::Dialled,
        path: ROOT_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published address dialled for a document deeper in the served directory",
        addressing: Addressing::Dialled,
        path: DEEP_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published address dialled for a path the container holds nothing at",
        addressing: Addressing::Dialled,
        path: MISSING_PATH,
        status: NOT_FOUND_STATUS,
    },
    Row {
        case: "the published authority as the Host, for the page at the served root",
        addressing: Addressing::HostStyle,
        path: ROOT_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published authority as the Host, for a document deeper in the directory",
        addressing: Addressing::HostStyle,
        path: DEEP_PATH,
        status: SERVED_STATUS,
    },
    Row {
        case: "the published authority as the Host, for a path the container holds nothing at",
        addressing: Addressing::HostStyle,
        path: MISSING_PATH,
        status: NOT_FOUND_STATUS,
    },
];

/// One deployment: how many tasks its service declares, and the names its own resources take so
/// two deployments never collide in the services that hold them.
struct Deployment {
    case: &'static str,
    desired: u64,
    stack_name: &'static str,
    /// The load balancer's and the target group's names, which must be unique per account.
    load_balancer: &'static str,
    target_group: &'static str,
    /// The cluster, the task-definition family and the service's own name.
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
    /// The group the revision's `awslogs` configuration delivers to.
    log_group: &'static str,
}

/// The recorded shape: a declaration asking for one task.
const ONE_TASK: Deployment = Deployment {
    case: "the recorded count: a stack whose service declares one task",
    desired: 1,
    stack_name: "routed-one",
    load_balancer: "routed-one-alb",
    target_group: "routed-one-tg",
    cluster: "routed-one-cluster",
    family: "routedonetask",
    service: "routed-one-service",
    log_group: "/ecs/routed-one",
};

/// The same declaration asking for two, which is the count the rule generalises over.
const TWO_TASKS: Deployment = Deployment {
    case: "a stack whose service declares two tasks",
    desired: 2,
    stack_name: "routed-two",
    load_balancer: "routed-two-alb",
    target_group: "routed-two-tg",
    cluster: "routed-two-cluster",
    family: "routedtwotask",
    service: "routed-two-service",
    log_group: "/ecs/routed-two",
};

/// The capability a create has to acknowledge for a template that declares an IAM resource: "you
/// must explicitly acknowledge that your stack template contains certain capabilities in order for
/// CloudFormation to create the stack" (`CreateStackInput$Capabilities`). The role below is
/// declared with no name of its own, so the broader acknowledgement a custom-named IAM resource
/// needs is not the one this template asks for.
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";

/// The service principal Amazon ECS retrieves a task's execution resources as, which the declared
/// role's trust policy therefore has to admit.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The emulator this suite drives: the seven services that have to agree, over ONE engine, exactly
/// as `core/router/src/lib.rs` mounts them — the CLASSIC Elastic Load Balancing front door
/// included, since the two Elastic Load Balancing models share one endpoint and one signing name
/// there.
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation on a service whose model declares awsQuery, answered with its status and XML
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

/// One CloudFormation call that must be served, answered with its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        CFN_API_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with body \
         {body}"
    );
    body
}

/// One Elastic Load Balancing v2 call that must be served, answered with its XML body.
async fn elbv2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "elasticloadbalancingv2",
        ELB_API_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ElasticLoadBalancingV2 {action}, got {status} with \
         body {body}"
    );
    body
}

/// Call one operation on a service whose model declares `awsJson1_1`, answered with its status and
/// parsed body.
async fn rpc(
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
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon ECS call that must be served, answered with its parsed body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, parsed) = rpc(router, "ecs", ECS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call that must be served, answered with its parsed body.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, parsed) = rpc(router, "logs", LOGS_TARGET_PREFIX, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// The template one deployment declares: the network, the two security groups that decide what
/// reaches what, the log group the container's output is delivered to, the load balancer with its
/// target group and listener, and the ECS cluster, task definition and service behind it.
fn template(deployment: &Deployment) -> String {
    format!(
        r#"Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: {VPC_CIDR}
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: {SUBNET_A_CIDR}
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: {SUBNET_B_CIDR}
      AvailabilityZone: {REGION}b
  Gateway:
    Type: AWS::EC2::InternetGateway
  GatewayAttachment:
    Type: AWS::EC2::VPCGatewayAttachment
    Properties:
      VpcId: !Ref Vpc
      InternetGatewayId: !Ref Gateway
  Routes:
    Type: AWS::EC2::RouteTable
    Properties:
      VpcId: !Ref Vpc
  DefaultRoute:
    Type: AWS::EC2::Route
    DependsOn: GatewayAttachment
    Properties:
      RouteTableId: !Ref Routes
      DestinationCidrBlock: 0.0.0.0/0
      GatewayId: !Ref Gateway
  SubnetARoutes:
    Type: AWS::EC2::SubnetRouteTableAssociation
    Properties:
      SubnetId: !Ref SubnetA
      RouteTableId: !Ref Routes
  SubnetBRoutes:
    Type: AWS::EC2::SubnetRouteTableAssociation
    Properties:
      SubnetId: !Ref SubnetB
      RouteTableId: !Ref Routes
  AlbSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group of the load balancer
      VpcId: !Ref Vpc
      SecurityGroupIngress:
        - IpProtocol: tcp
          FromPort: {PORT}
          ToPort: {PORT}
          CidrIp: 0.0.0.0/0
  TaskSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group of the tasks
      VpcId: !Ref Vpc
      SecurityGroupIngress:
        - IpProtocol: tcp
          FromPort: {PORT}
          ToPort: {PORT}
          SourceSecurityGroupId: !GetAtt AlbSecurityGroup.GroupId
  LogGroup:
    Type: AWS::Logs::LogGroup
    Properties:
      LogGroupName: {log_group}
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service: {ECS_TASKS_PRINCIPAL}
      Policies:
        - PolicyName: awslogs-delivery
          PolicyDocument:
            Version: "2012-10-17"
            Statement:
              - Effect: Allow
                Action:
                  - logs:CreateLogStream
                  - logs:PutLogEvents
                Resource: "*"
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {load_balancer}
      Scheme: internet-facing
      SecurityGroups:
        - !GetAtt AlbSecurityGroup.GroupId
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
  TargetGroup:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {target_group}
      Port: {PORT}
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
      HealthCheckProtocol: HTTP
      HealthCheckPath: /
      HealthCheckIntervalSeconds: {HEALTH_CHECK_INTERVAL_SECONDS}
      HealthyThresholdCount: 2
      UnhealthyThresholdCount: 2
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: {PORT}
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref TargetGroup
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
  Task:
    Type: AWS::ECS::TaskDefinition
    DependsOn: LogGroup
    Properties:
      Family: {family}
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/docker/library/nginx:alpine
          Essential: true
          PortMappings:
            - ContainerPort: {PORT}
              Protocol: tcp
          Command:
            - /bin/sh
            - -c
            - {CONTAINER_COMMAND}
          LogConfiguration:
            LogDriver: awslogs
            Options:
              awslogs-group: !Ref LogGroup
              awslogs-region: {REGION}
              awslogs-stream-prefix: app
  Service:
    Type: AWS::ECS::Service
    DependsOn: Listener
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired}
      LaunchType: FARGATE
      HealthCheckGracePeriodSeconds: 120
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          SecurityGroups:
            - !GetAtt TaskSecurityGroup.GroupId
          Subnets:
            - !Ref SubnetA
            - !Ref SubnetB
      LoadBalancers:
        - ContainerName: app
          ContainerPort: {PORT}
          TargetGroupArn: !Ref TargetGroup
Outputs:
  ServiceName:
    Value: !GetAtt Service.Name
  TargetGroupArn:
    Value: !Ref TargetGroup
  LoadBalancerDnsName:
    Value: !GetAtt Alb.DNSName
  LogGroupName:
    Value: !Ref LogGroup
"#,
        load_balancer = deployment.load_balancer,
        target_group = deployment.target_group,
        cluster = deployment.cluster,
        family = deployment.family,
        service = deployment.service,
        log_group = deployment.log_group,
        desired = deployment.desired,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED: looked at
/// again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(router, case, "DescribeStacks", &[("StackName", stack_name)]).await;
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
    let body =
        cloudformation(router, case, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// Deploy one stack and answer the outputs it published, having asserted it reached
/// `CREATE_COMPLETE` and that every output a client addresses the estate by carries a value.
async fn deployed(
    router: &MultiServiceRouter,
    deployment: &Deployment,
) -> BTreeMap<String, String> {
    let case = deployment.case;
    let body = template(deployment);
    cloudformation(
        router,
        case,
        "CreateStack",
        &[
            ("StackName", deployment.stack_name),
            ("TemplateBody", &body),
            // The template declares the execution role the `awslogs` driver is initialised as.
            ("Capabilities.member.1", CAPABILITY_IAM),
        ],
    )
    .await;

    let described = settled_stack(router, case, deployment.stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}; the reasons its events \
         recorded were {:?} and its own were {:?}",
        stack_failures(router, case, deployment.stack_name).await,
        elements(&described, "StackStatusReason"),
    );

    let outputs = outputs_of(&described);
    for key in [
        "ServiceName",
        "TargetGroupArn",
        "LoadBalancerDnsName",
        "LogGroupName",
    ] {
        assert!(
            outputs.get(key).is_some_and(|value| !value.is_empty()),
            "case {case:?}: the output {key} must carry a value, got {outputs:?}"
        );
    }
    assert_eq!(
        outputs.get("ServiceName").map(String::as_str),
        Some(deployment.service),
        "case {case:?}: the stack must publish the service it created, got {outputs:?}"
    );
    outputs
}

/// One value an output must carry, panicking with the whole map when it does not.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
}

/// The service's `PRIMARY` deployment — the one a client waiting on a rollout reads — or `Null`
/// when the service reports none.
fn primary_deployment(service: &Value) -> Value {
    service["deployments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|deployment| deployment["status"].as_str() == Some(PRIMARY))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The service one deployment's stack created, as `DescribeServices` reports it once it has
/// SETTLED: read again until its `runningCount` is the count the declaration asked for AND its
/// `PRIMARY` deployment's rollout has reached its steady state, or the deadline passes.
///
/// `CreateService` answers with the task set placed and nothing running yet — the convergence is
/// owed by the clock — so what is waited for is the CONDITION rather than a span of it.
async fn settled_service(router: &MultiServiceRouter, deployment: &Deployment) -> Value {
    let case = deployment.case;
    let request = json!({ "cluster": deployment.cluster, "services": [deployment.service] });
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ecs(router, case, "DescribeServices", &request).await;
        let service = described["services"][0].clone();
        let rollout = primary_deployment(&service);
        let settled = service["runningCount"].as_u64() == Some(deployment.desired)
            && rollout["rolloutState"].as_str() == Some(ROLLOUT_COMPLETED);
        if settled || Instant::now() >= deadline {
            return service;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `TargetHealth$State` `DescribeTargetHealth` reports for each registration of one group,
/// keyed by the target's own `Id`.
///
/// Each pair is read from the DESCRIPTION that carries both — the keyed-list reading this test
/// crate keeps once ([`pairs_of`]) — so a description that ever omitted one half contributes
/// nothing instead of misaligning the rest.
async fn target_health(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
) -> BTreeMap<String, String> {
    let body = elbv2(
        router,
        case,
        "DescribeTargetHealth",
        &[("TargetGroupArn", target_group_arn)],
    )
    .await;
    pairs_of(&body, "Id", "State")
}

/// The target health of one group once it holds `wanted` registrations and every one of them is
/// `healthy` — looked at again until that holds, or the deadline passes.
///
/// A second reading of a poll the sibling suite also keeps, rather than a call into it: that one
/// is written around its own cluster constant and its own ECS reads, which a stack-deployed
/// estate does not share, so a failure here names THIS deployment's group and count.
async fn healthy_targets(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
    wanted: usize,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_health(router, case, target_group_arn).await;
        let settled = reported.len() == wanted && reported.values().all(|state| state == HEALTHY);
        if settled || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The private IPv4 address of every task the service runs, read off the interface each task was
/// allocated.
async fn task_addresses(router: &MultiServiceRouter, deployment: &Deployment) -> Vec<String> {
    let case = deployment.case;
    let listed = ecs(
        router,
        case,
        "ListTasks",
        &json!({
            "cluster": deployment.cluster,
            "serviceName": deployment.service,
            "desiredStatus": "RUNNING",
        }),
    )
    .await;
    let arns: Vec<&str> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        case,
        "DescribeTasks",
        &json!({ "cluster": deployment.cluster, "tasks": arns }),
    )
    .await;
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| {
            task["attachments"]
                .as_array()?
                .iter()
                .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
                .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                .and_then(|detail| detail["value"].as_str())
                .map(str::to_string)
        })
        .collect()
}

/// Send one request to the published load balancer the way `row` addresses it.
async fn addressed(router: &MultiServiceRouter, row: &Row, dns_name: &str) -> Answered {
    match row.addressing {
        Addressing::Dialled => dialled(row.case, dns_name, row.path).await,
        Addressing::HostStyle => get(router, dns_name, row.path).await,
    }
}

/// Assert `answered` is what the row expects: the TARGET's own status, and — for a document the
/// container holds — the marker its program wrote beside the hostname of the task that served it
/// ([`container_hostname`]). For the status the target answered for a path it holds nothing at,
/// what matters is that the answer carries no document of the container at all.
fn assert_answered(case: &str, answered: &Answered, status: u16, hostnames: &[String]) {
    assert_eq!(
        answered.status, status,
        "case {case:?}: expected the target's own HTTP {status}, got {} with body {:?} (a \
         {TARGET_UNREACHABLE_STATUS} would be the load balancer's own answer for a target it \
         cannot connect to, which a registration reported {HEALTHY} must never earn)",
        answered.status, answered.body
    );
    if status != SERVED_STATUS {
        assert!(
            !answered.body.contains(MARKER),
            "case {case:?}: a status the target answered for a path it holds nothing at carries no \
             document of the container, got {:?}",
            answered.body
        );
        return;
    }
    let fields: Vec<&str> = answered.body.split_whitespace().collect();
    assert_eq!(
        fields.first().copied(),
        Some(MARKER),
        "case {case:?}: expected the document the container's own program wrote, got {:?}",
        answered.body
    );
    assert!(
        fields
            .get(1)
            .is_some_and(|served| hostnames.iter().any(|known| known == served)),
        "case {case:?}: expected the document of one of the registered tasks {hostnames:?}, got \
         {:?}",
        answered.body
    );
}

/// Every registration of the group is `healthy`, names a task of the service and sits in the block
/// the stack's own VPC was declared with; answer the hostnames of the containers behind them.
async fn registered_hostnames(
    router: &MultiServiceRouter,
    deployment: &Deployment,
    target_group_arn: &str,
) -> Vec<String> {
    let case = deployment.case;
    let wanted = deployment.desired as usize;
    let reported = healthy_targets(router, case, target_group_arn, wanted).await;
    assert_eq!(
        reported.len(),
        wanted,
        "case {case:?}: expected one target description per task the declaration asked for, got \
         {reported:?}"
    );
    for (id, state) in &reported {
        assert_eq!(
            state, HEALTHY,
            "case {case:?}: expected the target {id} to be {HEALTHY}, got {state}"
        );
        assert!(
            block_contains(VPC_CIDR, id),
            "case {case:?}: a registration names the task's own address in the declared VPC \
             {VPC_CIDR}, got {id}"
        );
    }
    let addresses = task_addresses(router, deployment).await;
    assert_eq!(
        addresses.len(),
        wanted,
        "case {case:?}: expected one running task per task the declaration asked for, got \
         {addresses:?}"
    );
    for address in &addresses {
        assert!(
            reported.contains_key(address),
            "case {case:?}: the task at {address} must be a target of the declared group, got \
             {reported:?}"
        );
    }
    addresses.iter().map(|a| container_hostname(a)).collect()
}

/// Assert the service converged on the count its declaration asked for, with its `PRIMARY`
/// deployment's rollout at its steady state.
async fn assert_converged(router: &MultiServiceRouter, deployment: &Deployment) {
    let case = deployment.case;
    let service = settled_service(router, deployment).await;
    assert_eq!(
        service["serviceName"].as_str(),
        Some(deployment.service),
        "case {case:?}: Amazon ECS must hold the service the stack created, got {service}"
    );
    assert_eq!(
        service["desiredCount"].as_u64(),
        Some(deployment.desired),
        "case {case:?}: the service must keep the count the declaration asked for, got {service}"
    );
    assert_eq!(
        service["runningCount"].as_u64(),
        Some(deployment.desired),
        "case {case:?}: the service must run the task set the declaration asked for, got {service}"
    );
    let rollout = primary_deployment(&service);
    assert_eq!(
        rollout["rolloutState"].as_str(),
        Some(ROLLOUT_COMPLETED),
        "case {case:?}: the rollout the create started must reach its steady state, got {service}"
    );
}

/// Assert CloudWatch Logs really holds the group the revision's `awslogs` configuration delivers
/// to, which is the group the stack declared and published.
async fn assert_log_group(router: &MultiServiceRouter, deployment: &Deployment, published: &str) {
    let case = deployment.case;
    assert_eq!(
        published, deployment.log_group,
        "case {case:?}: the stack must publish the group it declared, got {published:?}"
    );
    let described = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": published }),
    )
    .await;
    let held: Vec<&str> = described["logGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| group["logGroupName"].as_str())
        .collect();
    assert_eq!(
        held,
        vec![published],
        "case {case:?}: CloudWatch Logs must hold the awslogs destination the stack declared, got \
         {held:?}"
    );
}

/// RULE: the stack deployed, the service converged, the registration healthy — and every
/// addressing of the address the stack PUBLISHED answered by the server the task's container runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stack_deployed_alb_ecs_service_answers_every_addressing_of_its_published_address() {
    let router = build_router();
    let deployment = &ONE_TASK;
    let case = deployment.case;

    let outputs = deployed(&router, deployment).await;
    let target_group_arn = output(&outputs, case, "TargetGroupArn");
    let dns_name = output(&outputs, case, "LoadBalancerDnsName");
    assert_log_group(&router, deployment, &output(&outputs, case, "LogGroupName")).await;
    assert_converged(&router, deployment).await;
    // The registration and its health come first: the rule is about what a target the load
    // balancer's own checks found healthy is answered with.
    let hostnames = registered_hostnames(&router, deployment, &target_group_arn).await;

    for row in ROWS {
        let answered = addressed(&router, row, &dns_name).await;
        assert_answered(row.case, &answered, row.status, &hostnames);
    }

    // The health the client read as proof the path is wired is still what the group reports, so
    // nothing about serving those requests moved it.
    let after = target_health(&router, case, &target_group_arn).await;
    assert!(
        after.len() == deployment.desired as usize
            && after.values().all(|state| state == HEALTHY),
        "case {case:?}: serving requests must leave the registration exactly as it was, got \
         {after:?}"
    );
}

/// RULE: the same holds when the declaration asks for more than one task — the group holds one
/// healthy registration per task, and the answer is still a task's own document.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stack_declaring_two_tasks_is_routed_to_either_of_its_tasks() {
    let router = build_router();
    let deployment = &TWO_TASKS;
    let case = deployment.case;

    let outputs = deployed(&router, deployment).await;
    let target_group_arn = output(&outputs, case, "TargetGroupArn");
    let dns_name = output(&outputs, case, "LoadBalancerDnsName");
    assert_log_group(&router, deployment, &output(&outputs, case, "LogGroupName")).await;
    assert_converged(&router, deployment).await;
    let hostnames = registered_hostnames(&router, deployment, &target_group_arn).await;
    assert_ne!(
        hostnames[0], hostnames[1],
        "case {case:?}: each awsvpc task holds an interface of its own, so the two registrations \
         must name two addresses, got {hostnames:?}"
    );

    // The client sends what it was handed the address for, and is answered by one of the tasks
    // behind it — that task's own document, not a status of the load balancer's.
    for row in ROWS {
        let answered = addressed(&router, row, &dns_name).await;
        assert_answered(row.case, &answered, row.status, &hostnames);
    }
}
