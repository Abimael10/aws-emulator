// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an ALB, a target group, a Fargate ECS service and the
//! network they run in CONVERGES: the stack completes, and the tasks the service runs are
//! reported as HEALTHY targets of the group the stack published.
//!
//! RULE: an `AWS::ECS::Service` is created in Amazon ECS by `CreateService`, so the service
//! really runs the task set its `DesiredCount` asks for and CONVERGES on it ("when the service
//! reaches a steady state, the deployment transitions to a `COMPLETED` state",
//! `Deployment$rolloutState`); and each running task of a service whose `LoadBalancers` bind a
//! target group is a target of that group at the address of the interface it was allocated,
//! which the load balancer's health checks then report `healthy`. So a client that deploys
//! this stack and polls `DescribeTargetHealth` on the group the outputs name reaches a verdict
//! — one `TargetHealthDescription` per running task — instead of polling a group nothing ever
//! registers into.
//!
//! Four crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls, Amazon ECS owns the service, its tasks and the
//! registration they earn, Amazon EC2 owns the network the placement resolves against and the
//! security-group rules that decide whether a check ARRIVES, and Elastic Load Balancing v2
//! holds the registration and reports its health. Everything is driven through the real
//! operations on one engine, so what is asserted is what each service persisted.
//!
//! The rows differ along what the rule generalises over: how many tasks the service is asked
//! for — none (so the group holds nothing), one (the count a recording shows) and three (so the
//! group holds one description per task, each at its own address).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and keyed-list pairing this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, pairs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, its service's rollout reaching a steady state, the registration that rollout owes and
/// the health checks of it, which come the target group's own `HealthCheckIntervalSeconds`
/// apart (the template below configures the shortest the member allows, so a verdict is a
/// threshold of checks and a few tens of seconds away) — and how often it looks again.
/// Several times that span: the deadline is only here so a failure is a failure rather than a
/// hang, and every wait looks for its condition rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// How many of those looks a row that wants NO task takes at its group before judging it
/// empty. Together with [`POLL`] a window far longer than the span a running task's
/// registration and its first health check take, so an emptiness this row asserts is one that
/// held rather than one read too early.
const QUIET_LOOKS: usize = 60;

/// The `TargetHealth$State` a target the load balancer's checks reached carries.
const HEALTHY: &str = "healthy";

/// The stack status a create is still in flight in, the one a failed create's rollback is in,
/// and the one a create finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Deployment$rolloutState` of a rollout that has reached its steady state.
const ROLLOUT_COMPLETED: &str = "COMPLETED";

/// One row: how many tasks the stack's service is asked for, and the names its own resources
/// take so two rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    desired: usize,
    stack_name: &'static str,
    /// The load balancer's and the target group's names, which must be unique per account.
    load_balancer: &'static str,
    target_group: &'static str,
    /// The cluster, the task-definition family and the service's own name.
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a service that wants no task at all",
        desired: 0,
        stack_name: "fargate-none",
        load_balancer: "fargate-none-alb",
        target_group: "fargate-none-tg",
        cluster: "fargate-none-cluster",
        family: "fargate-none-task",
        service: "fargate-none-service",
    },
    Row {
        case: "the recorded count: a service that wants one task",
        desired: 1,
        stack_name: "fargate-one",
        load_balancer: "fargate-one-alb",
        target_group: "fargate-one-tg",
        cluster: "fargate-one-cluster",
        family: "fargate-one-task",
        service: "fargate-one-service",
    },
    Row {
        case: "a service that wants three tasks",
        desired: 3,
        stack_name: "fargate-three",
        load_balancer: "fargate-three-alb",
        target_group: "fargate-three-tg",
        cluster: "fargate-three-cluster",
        family: "fargate-three-task",
        service: "fargate-three-service",
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
    host: &str,
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
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
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
    query(
        router,
        "cloudformation",
        "cloudformation",
        "2010-05-15",
        action,
        members,
    )
    .await
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

/// The template one row deploys: the network, the two security groups that decide what reaches
/// what, the load balancer with its target group and listener, and the ECS cluster, task
/// definition and service behind it.
fn template(row: &Row) -> String {
    format!(
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
          FromPort: 80
          ToPort: 80
          SourceSecurityGroupId: !GetAtt AlbSecurityGroup.GroupId
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
      Port: 80
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
      HealthCheckProtocol: HTTP
      HealthCheckPath: /
      HealthCheckIntervalSeconds: 5
      UnhealthyThresholdCount: 2
      HealthyThresholdCount: 2
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
  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {family}
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/nginx/nginx:latest
          Essential: true
          PortMappings:
            - ContainerPort: 80
              Protocol: tcp
  Service:
    Type: AWS::ECS::Service
    DependsOn: Listener
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
      LoadBalancers:
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
  LoadBalancerDnsName:
    Value: !GetAtt Alb.DNSName
"#,
        load_balancer = row.load_balancer,
        target_group = row.target_group,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        desired = row.desired,
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

/// The service one row's stack created, as `DescribeServices` reports it once it has SETTLED:
/// read again until its `runningCount` is the count the stack asked for AND its primary
/// deployment's rollout has reached its steady state, or the deadline passes.
///
/// `CreateService` answers with the task set PLACED and nothing running yet — the convergence
/// is owed by the clock — so what is waited for is the CONDITION rather than a span of it.
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
        let service = &services["services"][0];
        let settled = service["runningCount"].as_u64() == Some(row.desired as u64)
            && service["deployments"][0]["rolloutState"].as_str() == Some(ROLLOUT_COMPLETED);
        if settled || Instant::now() >= deadline {
            return services;
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
    pairs_of(&body, "Id", "State")
}

/// The target health of one group once every description it holds is `healthy` AND there are
/// `wanted` of them — looked at again until that holds, or the deadline passes.
///
/// A row that wants NO task is the one the condition holds for on the first look, so it is
/// looked at again over a window afterwards and the LAST reading answered: a group that is
/// empty now and holds a registration a moment later must fail that row rather than pass it.
async fn healthy_targets(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
    wanted: usize,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_health(router, case, target_group_arn).await;
        let settled =
            reported.len() == wanted && reported.values().all(|state| state == HEALTHY);
        if settled {
            return match wanted {
                0 => quiet_group(router, case, target_group_arn).await,
                _ => reported,
            };
        }
        if Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What one group reports after a WINDOW of being looked at again: the last reading, and the
/// first non-empty one if any look finds a registration.
///
/// What a row that wants no task needs: an empty group proves nothing on its own, since the
/// registration a task owes is a clock-owed move, so the group is read over a window long
/// enough for the span a running task's registration takes ([`QUIET_LOOKS`] of [`POLL`], far
/// past it) and the first reading that holds anything is what the row is judged on.
async fn quiet_group(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
) -> BTreeMap<String, String> {
    let mut last = BTreeMap::new();
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL).await;
        last = target_health(router, case, target_group_arn).await;
        if !last.is_empty() {
            return last;
        }
    }
    last
}

/// The private IPv4 address of every task the service `service` runs in `cluster`, read off the
/// interface each task was allocated.
async fn task_addresses(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    service: &str,
) -> Vec<String> {
    let (status, listed) = ecs(
        router,
        "ListTasks",
        &json!({ "cluster": cluster, "serviceName": service, "desiredStatus": "RUNNING" }),
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
        &json!({ "cluster": cluster, "tasks": arns }),
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
async fn a_stack_deployed_alb_fargate_service_converges_on_healthy_targets() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = template(row);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &body)],
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

        // Every output carries a VALUE: the identifiers a client addresses the deployed
        // resources by.
        let outputs = outputs_of(&described);
        for key in [
            "ClusterName",
            "ServiceName",
            "TargetGroupArn",
            "LoadBalancerDnsName",
        ] {
            assert!(
                outputs.get(key).is_some_and(|value| !value.is_empty()),
                "case {case:?}: output {key} must carry a value, got {outputs:?}"
            );
        }
        assert_eq!(
            outputs.get("ServiceName").map(String::as_str),
            Some(row.service),
            "case {case:?}: the stack must report the service it created, got {outputs:?}"
        );

        // Amazon ECS really holds the service, and it CONVERGES on the task set the stack
        // asked for: `CreateService` answers with nothing running yet and a rollout still
        // `IN_PROGRESS`, so the service is looked at again until it has reached its steady
        // state.
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
             {services}"
        );
        assert_eq!(
            service["deployments"][0]["rolloutState"].as_str(),
            Some(ROLLOUT_COMPLETED),
            "case {case:?}: the rollout the create started must reach its steady state, got \
             {services}"
        );

        // ...and every one of those tasks is a HEALTHY target of the group the stack
        // published, at the address of the interface it was allocated.
        let target_group_arn = outputs
            .get("TargetGroupArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn output in {outputs:?}"));
        let reported = healthy_targets(&router, case, &target_group_arn, row.desired).await;
        assert_eq!(
            reported.len(),
            row.desired,
            "case {case:?}: expected one target description per running task, got {reported:?}"
        );
        for (id, state) in &reported {
            assert_eq!(
                state, HEALTHY,
                "case {case:?}: expected target {id} to be {HEALTHY}, got {state}"
            );
        }
        let addresses = task_addresses(&router, case, row.cluster, row.service).await;
        assert_eq!(
            addresses.len(),
            row.desired,
            "case {case:?}: expected one running task per asked-for task, got {addresses:?}"
        );
        for address in &addresses {
            assert!(
                reported.contains_key(address),
                "case {case:?}: the task at {address} must be a target of the group, got \
                 {reported:?}"
            );
        }
    }
}
