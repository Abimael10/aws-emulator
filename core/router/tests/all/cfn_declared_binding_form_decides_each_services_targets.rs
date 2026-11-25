// SPDX-License-Identifier: CC0-1.0

//! ONE CloudFormation stack, two Elastic Load Balancing v2 target groups and four Amazon ECS
//! services whose declarations differ only in their `LoadBalancers`: the stack settles, every
//! service places its tasks, and WHICH group each task is a target of is decided by that
//! service's own binding and by nothing else.
//!
//! RULE: an `AWS::ECS::Service` is created in Amazon ECS by `CreateService`, so the service
//! really converges on the task set its `DesiredCount` asks for ("when the service reaches a
//! steady state, the deployment transitions to a `COMPLETED` state",
//! `Deployment$rolloutState`); and Amazon ECS registers each running task of an `awsvpc`
//! service as a target of exactly the groups the service's own `loadBalancers` bindings NAME —
//! at the private IPv4 address of the interface its placement gave it — so a service whose
//! declaration names no binding, in any of the forms a template can spell that (the property
//! left out, or declared as an empty list), reports `Service$loadBalancers` as an empty list and
//! is a target of NO group in ANY state, while a service that names one group is a target of
//! that group and not of the group its sibling named.
//!
//! The negative half of that rule is only observable beside the positive half, which is why one
//! stack holds both: a group that is empty proves nothing on its own, since the registration a
//! running task earns is owed by the clock, so each group is read again over the whole window
//! its siblings' tasks come up and register in, and every target id it was EVER reported
//! holding — whatever state that reading carried — is what the rows are judged on.
//!
//! The rows differ along what the rule generalises over: the FORM the declaration takes (a
//! binding on the stack's first group, a binding on its second, the property omitted
//! altogether, the property declared as an empty list) and HOW MANY tasks the service runs, so
//! a group bound by a two-task service must hold one target per running task of it. The
//! ECR-cached image dimension of the same rule is pinned by
//! `cfn_service_binding_decides_its_ecr_backed_targets`; this suite varies the declaration.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls, Amazon ECS owns the services, their tasks and the
//! registration those tasks earn, Amazon EC2 owns the network the placement resolves against
//! and the security groups that decide whether a health check arrives, and Elastic Load
//! Balancing v2 holds the registrations and reports their health. Everything is driven through
//! the real operations on one engine, so what is asserted is what each service persisted.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers, output reader and keyed-list pairing this test
// crate keeps for awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one
// — call this module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, each service's rollout placing its tasks, and the registration and health checks that
/// follow — and how often it looks again. Several times the span those moves take: the deadline
/// is only here so a failure is a failure rather than a hang, and every wait looks for its
/// CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// How many further looks each group gets once every expected registration is in place, before
/// the suite judges what was never registered absent. Together with [`POLL`] a window far
/// longer than the span a running task's registration takes, so an absence asserted here is one
/// that held rather than one read too early.
const QUIET_LOOKS: usize = 60;

/// The `TargetHealth$State` a target the load balancer's checks reached carries.
const HEALTHY: &str = "healthy";

/// The stack statuses a create is still in flight in, and the one a create finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Deployment$rolloutState` of a rollout that has reached its steady state.
const ROLLOUT_COMPLETED: &str = "COMPLETED";

/// The stack this suite deploys, and the names its resources take inside the services that hold
/// them.
const STACK: &str = "binding-form";
const CLUSTER: &str = "binding-form-cluster";
const LOAD_BALANCER: &str = "binding-form-alb";
const GROUP_A: &str = "binding-form-a";
const GROUP_B: &str = "binding-form-b";

/// The container the task definition declares, and the port it listens on.
const CONTAINER: &str = "app";
const CONTAINER_PORT: u64 = 80;

/// The outputs the stack publishes the two groups' ARNs under, and the logical ids of the
/// groups themselves.
const GROUP_A_OUTPUT: &str = "GroupAArn";
const GROUP_B_OUTPUT: &str = "GroupBArn";
const GROUP_A_LOGICAL: &str = "GroupA";
const GROUP_B_LOGICAL: &str = "GroupB";

/// How one row's service declares its `LoadBalancers` property.
enum Form {
    /// A binding naming the group with this logical id.
    Binding(&'static str),
    /// No `LoadBalancers` property at all.
    Omitted,
    /// A `LoadBalancers` property declared as an empty list.
    EmptyList,
}

/// One row: the service the stack declares, the form its `LoadBalancers` takes, how many tasks
/// it runs, and the group — named by the output the stack publishes its ARN under — its running
/// tasks must therefore be targets of.
struct Row {
    case: &'static str,
    logical: &'static str,
    service: &'static str,
    form: Form,
    desired: usize,
    registers_in: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a binding on the stack's first group, over two tasks",
        logical: "ServiceBoundA",
        service: "binding-form-bound-a",
        form: Form::Binding(GROUP_A_LOGICAL),
        desired: 2,
        registers_in: Some(GROUP_A_OUTPUT),
    },
    Row {
        case: "the recorded wiring: a binding on the stack's second group, over one task",
        logical: "ServiceBoundB",
        service: "binding-form-bound-b",
        form: Form::Binding(GROUP_B_LOGICAL),
        desired: 1,
        registers_in: Some(GROUP_B_OUTPUT),
    },
    Row {
        case: "the negative control: no LoadBalancers property at all",
        logical: "ServiceOmitted",
        service: "binding-form-omitted",
        form: Form::Omitted,
        desired: 1,
        registers_in: None,
    },
    Row {
        case: "the negative control spelled as an empty LoadBalancers list",
        logical: "ServiceEmpty",
        service: "binding-form-empty",
        form: Form::EmptyList,
        desired: 1,
        registers_in: None,
    },
];

/// The outputs the two groups are read through, in the order the stack declares them.
const GROUP_OUTPUTS: &[&str] = &[GROUP_A_OUTPUT, GROUP_B_OUTPUT];

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them — the CLASSIC Elastic Load Balancing front door
/// included, since the two Elastic Load Balancing models share one endpoint and one signing
/// name there.
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

/// Call one operation on a service whose model declares awsQuery, answered with its XML body.
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

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one Elastic Load Balancing v2 operation over its awsQuery wire.
async fn elbv2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "elasticloadbalancingv2",
        "2015-12-01",
        action,
        members,
    )
    .await
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

/// The `LoadBalancers` lines one row's service declaration carries, indented as a property of
/// it: nothing at all, an empty list, or the one binding the row names.
fn declared_bindings(row: &Row) -> String {
    match row.form {
        Form::Omitted => String::new(),
        Form::EmptyList => "      LoadBalancers: []\n".to_string(),
        Form::Binding(group) => format!(
            "      LoadBalancers:\n        - ContainerName: {CONTAINER}\n          \
             ContainerPort: {CONTAINER_PORT}\n          TargetGroupArn: !Ref {group}\n"
        ),
    }
}

/// One row's `AWS::ECS::Service` declaration, which differs from its siblings' only in its
/// `LoadBalancers` and its `DesiredCount`.
fn declared_service(row: &Row) -> String {
    format!(
        r#"  {logical}:
    Type: AWS::ECS::Service
    DependsOn:
      - ListenerA
      - ListenerB
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired}
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          SecurityGroups:
            - !GetAtt TaskSecurityGroup.GroupId
          Subnets:
            - !Ref SubnetA
            - !Ref SubnetB
{bindings}"#,
        logical = row.logical,
        service = row.service,
        desired = row.desired,
        bindings = declared_bindings(row),
    )
}

/// The template this suite deploys: the network, the security groups that decide what reaches
/// what, one load balancer with TWO target groups and a listener forwarding to each, the ECS
/// cluster and task definition every service shares, and the rows' services.
fn template() -> String {
    let mut body = format!(
        r#"Resources:
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
          FromPort: 80
          ToPort: 80
          CidrIp: 0.0.0.0/0
  TaskSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the group of the tasks
      VpcId: !Ref Vpc
      SecurityGroupIngress:
        - IpProtocol: tcp
          FromPort: {CONTAINER_PORT}
          ToPort: {CONTAINER_PORT}
          SourceSecurityGroupId: !GetAtt AlbSecurityGroup.GroupId
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {LOAD_BALANCER}
      Scheme: internet-facing
      SecurityGroups:
        - !GetAtt AlbSecurityGroup.GroupId
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
  {GROUP_A_LOGICAL}:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {GROUP_A}
      Port: {CONTAINER_PORT}
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
      HealthCheckProtocol: HTTP
      HealthCheckPath: /
      HealthCheckIntervalSeconds: 5
      UnhealthyThresholdCount: 2
      HealthyThresholdCount: 2
  {GROUP_B_LOGICAL}:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {GROUP_B}
      Port: {CONTAINER_PORT}
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
      HealthCheckProtocol: HTTP
      HealthCheckPath: /
      HealthCheckIntervalSeconds: 5
      UnhealthyThresholdCount: 2
      HealthyThresholdCount: 2
  ListenerA:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: 80
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref {GROUP_A_LOGICAL}
  ListenerB:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: 8080
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref {GROUP_B_LOGICAL}
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {CLUSTER}
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: binding-form-task
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: public.ecr.aws/nginx/nginx:latest
          Essential: true
          PortMappings:
            - ContainerPort: {CONTAINER_PORT}
              Protocol: tcp
"#
    );
    for row in ROWS {
        body.push_str(&declared_service(row));
    }
    body.push_str(&format!(
        r#"Outputs:
  ClusterArn:
    Value: !GetAtt Cluster.Arn
  {GROUP_A_OUTPUT}:
    Value: !Ref {GROUP_A_LOGICAL}
  {GROUP_B_OUTPUT}:
    Value: !Ref {GROUP_B_LOGICAL}
"#
    ));
    body
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED:
/// looked at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
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

/// Every `ResourceStatusReason` the stack's event stream carries, which is where the refusal a
/// failing resource earned is reported.
async fn stack_failures(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeStackEvents, got {status} with body {body}"
    );
    elements(&body, "ResourceStatusReason")
}

/// The four services the stack created, as `DescribeServices` reports them, keyed by name.
async fn described_services(router: &MultiServiceRouter, case: &str) -> BTreeMap<String, Value> {
    let names: Vec<&str> = ROWS.iter().map(|row| row.service).collect();
    let (status, answered) = ecs(
        router,
        "DescribeServices",
        &json!({ "cluster": CLUSTER, "services": names }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from DescribeServices, got {status} with {answered}"
    );
    answered["services"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|service| {
            let name = service["serviceName"].as_str()?.to_string();
            Some((name, service.clone()))
        })
        .collect()
}

/// Whether one service has SETTLED: it runs the task set its declaration asked for and its
/// primary deployment's rollout has reached its steady state.
///
/// `CreateService` answers with the task set placed and nothing running yet — the convergence is
/// owed by the clock — so this is the CONDITION every wait below looks for.
fn settled(service: &Value, row: &Row) -> bool {
    service["runningCount"].as_u64() == Some(row.desired as u64)
        && service["deployments"][0]["rolloutState"].as_str() == Some(ROLLOUT_COMPLETED)
}

/// The `TargetHealth$State` `DescribeTargetHealth` reports for each registration of one group,
/// keyed by the target's own `Id`, accumulated into `seen` as well.
///
/// Each pair is read from the DESCRIPTION that carries both — the keyed-list reading this test
/// crate keeps once ([`pairs_of`]) — so a description that ever omitted one half contributes
/// nothing instead of misaligning the rest. `seen` keeps every state each id was EVER reported
/// in, which is what says a registration that appeared for one reading and went away again was
/// there.
async fn look(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
    seen: &mut BTreeMap<String, BTreeSet<String>>,
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
    let reported = pairs_of(&body, "Id", "State");
    for (id, state) in &reported {
        seen.entry(id.clone()).or_default().insert(state.clone());
    }
    reported
}

/// The private IPv4 address of every running task the service `service` runs, read off the
/// interface each task's placement allocated it.
async fn task_addresses(router: &MultiServiceRouter, case: &str, service: &str) -> Vec<String> {
    let (status, listed) = ecs(
        router,
        "ListTasks",
        &json!({ "cluster": CLUSTER, "serviceName": service, "desiredStatus": "RUNNING" }),
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
        &json!({ "cluster": CLUSTER, "tasks": arns }),
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

#[tokio::test]
async fn a_stacks_target_registrations_follow_each_services_own_declared_binding() {
    let router = build_router();
    let body = template();

    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the stack this suite deploys\": expected 200 from CreateStack, got {status} with \
         body {created}"
    );

    let described = settled_stack(&router, "the stack this suite deploys").await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case \"the stack this suite deploys\": the stack must complete; the reasons it recorded \
         were {:?} and {:?}",
        stack_failures(&router, "the stack this suite deploys").await,
        elements(&described, "StackStatusReason"),
    );

    // The identifiers a client addresses the deployed groups by, which is what every read below
    // is made through.
    let outputs = outputs_of(&described);
    let groups: Vec<(&str, String)> = GROUP_OUTPUTS
        .iter()
        .map(|key| {
            let arn = outputs.get(*key).cloned().unwrap_or_else(|| {
                panic!("case {key:?}: the stack must publish the group's ARN, got {outputs:?}")
            });
            assert!(
                !arn.is_empty(),
                "case {key:?}: the published ARN must carry a value, got {outputs:?}"
            );
            (*key, arn)
        })
        .collect();

    // Which addresses each group must hold: one per running task of every service whose own
    // declaration names it, and nothing else.
    let mut seen: BTreeMap<&str, BTreeMap<String, BTreeSet<String>>> = GROUP_OUTPUTS
        .iter()
        .map(|key| (*key, BTreeMap::new()))
        .collect();

    // Every service converges on the task set its declaration asked for — and both groups are
    // read on every look while that happens, so a registration that leaks into the wrong group
    // at any instant of the window the tasks come up in is recorded.
    let deadline = Instant::now() + DEADLINE;
    let mut services = loop {
        let services = described_services(&router, "the services the stack declares").await;
        for (key, arn) in &groups {
            let accumulated = seen.get_mut(key).expect("a group this suite reads");
            look(&router, key, arn, accumulated).await;
        }
        let converged = ROWS.iter().all(|row| {
            services
                .get(row.service)
                .is_some_and(|service| settled(service, row))
        });
        if converged || Instant::now() >= deadline {
            break services;
        }
        tokio::time::sleep(POLL).await;
    };

    let mut addresses: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for row in ROWS {
        let case = row.case;
        let service = services.remove(row.service).unwrap_or(Value::Null);
        assert_eq!(
            service["serviceName"].as_str(),
            Some(row.service),
            "case {case:?}: Amazon ECS must hold the service the stack created, got {service}"
        );
        assert_eq!(
            service["runningCount"].as_u64(),
            Some(row.desired as u64),
            "case {case:?}: the service must run the task set its declaration asks for, got \
             {service}"
        );
        assert_eq!(
            service["deployments"][0]["rolloutState"].as_str(),
            Some(ROLLOUT_COMPLETED),
            "case {case:?}: the rollout the create started must reach its steady state, got \
             {service}"
        );

        // The bindings the service REPORTS are the ones its declaration named: the one group for
        // a row that names one, and an empty list for either form of naming none.
        let reported = service["loadBalancers"].clone();
        match row.registers_in {
            None => assert_eq!(
                reported,
                json!([]),
                "case {case:?}: a service whose declaration names no binding must report none, \
                 got {reported}"
            ),
            Some(key) => {
                let arn = outputs.get(key).cloned().unwrap_or_default();
                let bindings = reported.as_array().cloned().unwrap_or_default();
                assert_eq!(
                    bindings.len(),
                    1,
                    "case {case:?}: the service must report the one binding its declaration \
                     named, got {reported}"
                );
                assert_eq!(
                    bindings[0]["targetGroupArn"].as_str(),
                    Some(arn.as_str()),
                    "case {case:?}: the binding must name the group the declaration named, got \
                     {reported}"
                );
                assert_eq!(
                    bindings[0]["containerName"].as_str(),
                    Some(CONTAINER),
                    "case {case:?}: the binding must name the declared container, got {reported}"
                );
                assert_eq!(
                    bindings[0]["containerPort"].as_u64(),
                    Some(CONTAINER_PORT),
                    "case {case:?}: the binding must name the declared container port, got \
                     {reported}"
                );
            }
        }

        // ...and it really placed its tasks, each at an address of its own.
        let placed = task_addresses(&router, case, row.service).await;
        let distinct: BTreeSet<&String> = placed.iter().collect();
        assert_eq!(
            distinct.len(),
            row.desired,
            "case {case:?}: expected one running task with an address of its own per asked-for \
             task, got {placed:?}"
        );
        addresses.insert(row.service, placed);
    }

    // What each group must hold once everything has settled: the addresses of the tasks of the
    // services that named it.
    let expected: BTreeMap<&str, BTreeSet<String>> = GROUP_OUTPUTS
        .iter()
        .map(|key| {
            let wanted = ROWS
                .iter()
                .filter(|row| row.registers_in == Some(*key))
                .flat_map(|row| addresses[row.service].iter().cloned())
                .collect();
            (*key, wanted)
        })
        .collect();

    // Each group reaches exactly those registrations, every one of them `healthy`...
    let deadline = Instant::now() + DEADLINE;
    let mut last: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    loop {
        let mut reached = true;
        for (key, arn) in &groups {
            let accumulated = seen.get_mut(key).expect("a group this suite reads");
            let reported = look(&router, key, arn, accumulated).await;
            let wanted = &expected[key];
            reached &= reported.len() == wanted.len()
                && reported
                    .iter()
                    .all(|(id, state)| wanted.contains(id) && state == HEALTHY);
            last.insert(*key, reported);
        }
        if reached || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }

    // ...and nothing else is ever a target of them, over a window far longer than the span a
    // running task's registration takes.
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL).await;
        for (key, arn) in &groups {
            let accumulated = seen.get_mut(key).expect("a group this suite reads");
            let reported = look(&router, key, arn, accumulated).await;
            last.insert(*key, reported);
        }
    }

    for (key, _) in &groups {
        let wanted = &expected[key];
        let reported = &last[key];
        assert_eq!(
            reported.keys().cloned().collect::<BTreeSet<String>>(),
            *wanted,
            "case {key:?}: the group must hold exactly the tasks of the services that named it, \
             got {reported:?}"
        );
        for (id, state) in reported {
            assert_eq!(
                state, HEALTHY,
                "case {key:?}: expected the target at {id} to be {HEALTHY}, got {state}"
            );
        }
        let ever: BTreeSet<String> = seen[key].keys().cloned().collect();
        assert_eq!(
            ever, *wanted,
            "case {key:?}: nothing but those tasks may EVER be a target of the group, got {:?}",
            seen[key]
        );
    }

    // The negative control, stated per row: the task of a service whose declaration names no
    // binding is a target of NO group, in ANY state — not merely an unhealthy one.
    for row in ROWS.iter().filter(|row| row.registers_in.is_none()) {
        let case = row.case;
        for address in &addresses[row.service] {
            for (key, _) in &groups {
                assert!(
                    !seen[key].contains_key(address),
                    "case {case:?}: the task at {address} must never be a target of {key}, got {:?}",
                    seen[key]
                );
            }
        }
    }
}
