// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an Amazon ECR PULL THROUGH CACHE RULE and runs the
//! image it caches on Fargate services behind an Application Load Balancer converges: the
//! cached image's own server answers on the load balancer the stack published, and only the
//! service that declares a `LoadBalancers` binding is a target of the stack's target group.
//!
//! RULE 1: an `AWS::ECR::PullThroughCacheRule` a stack declares is the rule Amazon ECR's own
//! `CreatePullThroughCacheRule` registers, under the `EcrRepositoryPrefix` the declaration
//! states — "a pull through cache rule provides a way to cache images from an upstream
//! registry source in your Amazon ECR private registry" — so a pull of a reference under that
//! prefix really caches the upstream image into a repository of this account's registry, which
//! that registry then reports. A stack that reported the resource created while nothing
//! registered the rule publishes a prefix no image is served under, and the task definition
//! the same stack deploys cannot fetch its container at all.
//!
//! RULE 2: a container runs the program, and ships the files, of the image it ACTUALLY runs —
//! and an image an Amazon ECR repository holds as a CACHED COPY of an upstream reference IS
//! that upstream image, so what the container performs is decided by the upstream reference the
//! repository cached rather than by the cache repository's own name. A cached copy of a web
//! server image therefore starts that server, which is what makes its task a `healthy` target
//! and what answers a request the load balancer forwards.
//!
//! RULE 3: Amazon ECS registers each running task of an `awsvpc` service as a target of every
//! target group the service's own `loadBalancers` bindings name, and of nothing otherwise — so
//! the sibling service that omits the binding is a target of no group in ANY state, and reports
//! its `loadBalancers` as an empty list.
//!
//! Six crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the create calls, Amazon ECR holds the rule and the image it caches,
//! IAM holds the execution role the pull is made as, Amazon EC2 owns the network the placement
//! resolves against, Amazon ECS owns the services, their tasks and the registration they earn,
//! and Elastic Load Balancing v2 holds that registration, reports its health and forwards the
//! request. Everything is driven through the real operations on one engine, so what is asserted
//! is what each service persisted.
//!
//! The rows differ along what the rules generalise over: the prefix the stack's rule caches
//! under, whether the rule supplies an `UpstreamRepositoryPrefix` of its own, which upstream
//! repository the reference names (a publisher's own repository, the gallery's mirror of an
//! official image under two different paths, and the OTHER server of the catalogue) and which
//! tag it asks for.

use std::collections::BTreeMap;
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

/// The upstream registry every row's rule caches from, which needs no credential of its own
/// ("Amazon ECR Public" is served anonymously).
const UPSTREAM_REGISTRY_URL: &str = "public.ecr.aws";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, the task set each service's rollout places, the pull each task makes and the
/// registration and health checks that follow — and how often it looks again. Several times
/// the span those moves take: the deadline is only here so a failure is a failure rather than a
/// hang, and every wait looks for its CONDITION rather than for a span to pass.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// How many of those looks a row takes at the group before judging the UNWIRED service's task
/// absent from it. Together with [`POLL`] a window far longer than the span a running task's
/// registration takes, so an absence this row asserts is one that held rather than one read too
/// early.
const QUIET_LOOKS: usize = 60;

/// The `TargetHealth$State` a target the load balancer's checks reached carries.
const HEALTHY: &str = "healthy";

/// The stack statuses a create is still in flight in, and the one a create finishes in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `Deployment$rolloutState` of a rollout that has reached its steady state.
const ROLLOUT_COMPLETED: &str = "COMPLETED";

/// The capability a create acknowledges for a template that declares the IAM role the tasks
/// pull as ("you must explicitly acknowledge that your stack template contains certain
/// capabilities", `CreateStack$Capabilities`).
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";

/// The container every row runs, and the port it listens on.
const CONTAINER: &str = "app";
const CONTAINER_PORT: &str = "80";

/// One row: the rule the stack declares, the reference its task definition names under it, and
/// the page the upstream image's own server ships — plus the names this row's own resources
/// take so two rows never collide in the services that hold them.
struct Row {
    case: &'static str,
    /// The `EcrRepositoryPrefix` the stack's rule caches under.
    prefix: &'static str,
    /// The `UpstreamRepositoryPrefix` the rule declares, for a row whose rule supplies one.
    upstream_prefix: Option<&'static str>,
    /// The repository path the container image names UNDER the rule's own prefix.
    repository_path: &'static str,
    /// The tag the reference asks for.
    tag: &'static str,
    /// The marker of the page the upstream image's own server ships, which the load balancer
    /// answers a request with once the task is a healthy target.
    marker: &'static str,
    stack: &'static str,
    cluster: &'static str,
}

/// The rows, spanning the prefix, the upstream path, the tag and which server the cached image
/// ships. The upstream repositories are the ones the gallery this emulator serves publishes
/// (`public.ecr.aws/nginx/nginx` is the publisher's own, `docker/library/<name>` the mirror of
/// a Docker Official Image), and the markers are the pages those two servers' own packages ship
/// (`services/common/src/static_web_servers.rs`).
const ROWS: &[Row] = &[
    Row {
        case: "the publisher's own nginx repository, cached under a one-segment prefix",
        prefix: "gallery",
        upstream_prefix: None,
        repository_path: "nginx/nginx",
        tag: "latest",
        marker: "Welcome to nginx!",
        stack: "ptc-publisher",
        cluster: "ptc-publisher-cluster",
    },
    Row {
        case: "the gallery's mirror of the same official image, on another tag",
        prefix: "mirror",
        upstream_prefix: None,
        repository_path: "docker/library/nginx",
        tag: "alpine",
        marker: "Welcome to nginx!",
        stack: "ptc-mirror",
        cluster: "ptc-mirror-cluster",
    },
    Row {
        case: "the other server of the catalogue",
        prefix: "apache",
        upstream_prefix: None,
        repository_path: "docker/library/httpd",
        tag: "2.4",
        marker: "It works!",
        stack: "ptc-apache",
        cluster: "ptc-apache-cluster",
    },
    Row {
        case: "a rule that supplies the upstream repository prefix itself",
        prefix: "pinned",
        upstream_prefix: Some("docker"),
        repository_path: "library/nginx",
        tag: "latest",
        marker: "Welcome to nginx!",
        stack: "ptc-pinned",
        cluster: "ptc-pinned-cluster",
    },
];

impl Row {
    /// The service this row wires to the stack's target group, and the sibling that omits the
    /// binding.
    fn wired(&self) -> String {
        format!("{}-wired", self.stack)
    }

    fn unwired(&self) -> String {
        format!("{}-unwired", self.stack)
    }

    /// The repository of this account's registry the cache rule makes the reference resolve in:
    /// the rule's own prefix followed by the path the reference names under it.
    fn cache_repository(&self) -> String {
        format!("{}/{}", self.prefix, self.repository_path)
    }
}

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
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
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

/// Call one operation of a service whose model declares awsJson1_1.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    prefix: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{prefix}.{operation}"))
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
    rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        request,
    )
    .await
}

/// Call one Amazon ECR operation.
async fn ecr(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    rpc(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        request,
    )
    .await
}

/// The template one row deploys: the pull through cache rule, the network, the load balancer
/// with its target group and listener, the execution role the tasks pull as, and the ECS
/// cluster, task definition and two services behind it — one declaring the `LoadBalancers`
/// binding and one omitting it.
fn template(row: &Row) -> String {
    let upstream_prefix = match row.upstream_prefix {
        Some(prefix) => format!("      UpstreamRepositoryPrefix: {prefix}\n"),
        None => String::new(),
    };
    format!(
        r#"Parameters:
  ImageTag:
    Type: String
    Default: {tag}
Resources:
  CacheRule:
    Type: AWS::ECR::PullThroughCacheRule
    Properties:
      EcrRepositoryPrefix: {prefix}
      UpstreamRegistryUrl: {UPSTREAM_REGISTRY_URL}
{upstream_prefix}  Vpc:
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
          FromPort: {CONTAINER_PORT}
          ToPort: {CONTAINER_PORT}
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
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Principal:
              Service: ecs-tasks.amazonaws.com
            Action: sts:AssumeRole
      ManagedPolicyArns:
        - arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy
  Alb:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Scheme: internet-facing
      Type: application
      SecurityGroups:
        - !GetAtt AlbSecurityGroup.GroupId
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
  TargetGroup:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Port: {CONTAINER_PORT}
      Protocol: HTTP
      TargetType: ip
      VpcId: !Ref Vpc
      HealthCheckPath: /
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref Alb
      Port: {CONTAINER_PORT}
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
      Family: {stack}-task
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      RequiresCompatibilities:
        - FARGATE
      ContainerDefinitions:
        - Name: {CONTAINER}
          Image: !Sub "${{AWS::AccountId}}.dkr.ecr.${{AWS::Region}}.amazonaws.com/${{CacheRule}}/{repository_path}:${{ImageTag}}"
          Essential: true
          PortMappings:
            - ContainerPort: {CONTAINER_PORT}
              Protocol: tcp
  WiredService:
    Type: AWS::ECS::Service
    DependsOn: Listener
    Properties:
      ServiceName: {wired}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: 1
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
        - ContainerName: {CONTAINER}
          ContainerPort: {CONTAINER_PORT}
          TargetGroupArn: !Ref TargetGroup
  UnwiredService:
    Type: AWS::ECS::Service
    DependsOn: Listener
    Properties:
      ServiceName: {unwired}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: 1
      LaunchType: FARGATE
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          SecurityGroups:
            - !GetAtt TaskSecurityGroup.GroupId
          Subnets:
            - !Ref SubnetA
            - !Ref SubnetB
Outputs:
  CachePrefix:
    Value: !Ref CacheRule
  TargetGroupArn:
    Value: !Ref TargetGroup
  LoadBalancerDnsName:
    Value: !GetAtt Alb.DNSName
"#,
        tag = row.tag,
        prefix = row.prefix,
        cluster = row.cluster,
        stack = row.stack,
        repository_path = row.repository_path,
        wired = row.wired(),
        unwired = row.unwired(),
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

/// The services `DescribeServices` reports for one row, once each of them has SETTLED: read
/// again until both run the one task their declaration asks for AND each primary deployment's
/// rollout has reached its steady state, or the deadline passes.
async fn settled_services(router: &MultiServiceRouter, case: &str, row: &Row) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, services) = ecs(
            router,
            "DescribeServices",
            &json!({
                "cluster": row.cluster,
                "services": [row.wired(), row.unwired()],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeServices, got {status} with {services}"
        );
        let reported = services["services"].as_array().cloned().unwrap_or_default();
        let settled = reported.len() == 2
            && reported.iter().all(|service| {
                service["runningCount"].as_u64() == Some(1)
                    && service["deployments"][0]["rolloutState"].as_str()
                        == Some(ROLLOUT_COMPLETED)
            });
        if settled || Instant::now() >= deadline {
            return services;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `TargetHealth$State` `DescribeTargetHealth` reports for each registration of one group,
/// keyed by the target's own `Id`.
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

/// The target health of one group once every description it holds is `healthy` AND there is one
/// of them — looked at again until that holds, or the deadline passes.
async fn healthy_targets(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_health(router, case, target_group_arn).await;
        let settled =
            reported.len() == 1 && reported.values().all(|state| state == HEALTHY);
        if settled || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What one group reports after a WINDOW of being looked at again: every `Id` any look in that
/// window found a description for.
///
/// What the UNWIRED half of a row needs: an address absent from the group now proves nothing on
/// its own, since the registration a running task owes is a clock-owed move, so the group is
/// read over a window far longer than that span ([`QUIET_LOOKS`] of [`POLL`]) and the row is
/// judged on everything that ever appeared in it.
async fn every_registered_address(
    router: &MultiServiceRouter,
    case: &str,
    target_group_arn: &str,
) -> BTreeMap<String, String> {
    let mut seen = BTreeMap::new();
    for _ in 0..QUIET_LOOKS {
        tokio::time::sleep(POLL).await;
        seen.extend(target_health(router, case, target_group_arn).await);
    }
    seen
}

/// What the load balancer the stack published answers for `path`, addressed by the authority
/// the stack's own output carries.
async fn served(router: &MultiServiceRouter, dns_name: &str, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name)
        .body(Bytes::new())
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

#[tokio::test]
async fn a_stack_declared_pull_through_cache_rule_backs_the_services_it_deploys() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let body = template(row);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", CAPABILITY_IAM),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from CreateStack, got {status} with body {created}"
        );

        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(CREATE_COMPLETE),
            "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
            stack_failures(&router, case, row.stack).await,
        );
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("CachePrefix").map(String::as_str),
            Some(row.prefix),
            "case {case:?}: the stack must publish the prefix its rule caches under, got \
             {outputs:?}"
        );

        // RULE 1: Amazon ECR itself holds the rule the stack declared, with the upstream the
        // declaration states.
        let (status, rules) = ecr(
            &router,
            "DescribePullThroughCacheRules",
            &json!({ "ecrRepositoryPrefixes": [row.prefix] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribePullThroughCacheRules, got {status} with \
             {rules}"
        );
        let rule = &rules["pullThroughCacheRules"][0];
        assert_eq!(
            rule["ecrRepositoryPrefix"].as_str(),
            Some(row.prefix),
            "case {case:?}: Amazon ECR must hold the rule the stack declared, got {rules}"
        );
        assert_eq!(
            rule["upstreamRegistryUrl"].as_str(),
            Some(UPSTREAM_REGISTRY_URL),
            "case {case:?}: the rule must cache from the upstream the stack declared, got \
             {rules}"
        );
        assert_eq!(
            rule["upstreamRepositoryPrefix"].as_str(),
            row.upstream_prefix,
            "case {case:?}: the rule must carry the upstream prefix the stack declared, got \
             {rules}"
        );

        // ...and the pull the task made through it really cached the upstream image into a
        // repository of this account's registry.
        let repository = row.cache_repository();
        let (status, images) = ecr(
            &router,
            "DescribeImages",
            &json!({ "repositoryName": repository }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from DescribeImages on {repository}, got {status} with \
             {images}"
        );
        let tags: Vec<&str> = images["imageDetails"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|detail| detail["imageTags"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        assert!(
            tags.contains(&row.tag),
            "case {case:?}: the cache repository {repository} must hold the tag the task pulled, \
             got {tags:?}"
        );

        // Both services converge on the one task their declaration asks for.
        let services = settled_services(&router, case, row).await;
        for (index, service) in services["services"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            assert_eq!(
                service["runningCount"].as_u64(),
                Some(1),
                "case {case:?}: service {index} must run the task its declaration asks for, got \
                 {services}"
            );
            assert_eq!(
                service["deployments"][0]["rolloutState"].as_str(),
                Some(ROLLOUT_COMPLETED),
                "case {case:?}: service {index}'s rollout must reach its steady state, got \
                 {services}"
            );
        }
        // RULE 3: the service that declares no binding reports an EMPTY `loadBalancers` list.
        let unwired_bindings = services["services"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|service| service["serviceName"].as_str() == Some(row.unwired().as_str()))
            .map(|service| service["loadBalancers"].clone())
            .unwrap_or(Value::Null);
        assert_eq!(
            unwired_bindings,
            json!([]),
            "case {case:?}: the service that declares no binding must report none, got \
             {unwired_bindings}"
        );

        // RULE 2 + RULE 3: the WIRED service's task is the one target of the stack's group, and
        // the load balancer's checks reach the server the CACHED image runs.
        let target_group_arn = outputs
            .get("TargetGroupArn")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn output in {outputs:?}"));
        let reported = healthy_targets(&router, case, &target_group_arn).await;
        let wired = task_addresses(&router, case, row.cluster, &row.wired()).await;
        assert_eq!(
            wired.len(),
            1,
            "case {case:?}: the wired service must run one task with an address, got {wired:?}"
        );
        assert_eq!(
            reported.get(&wired[0]).map(String::as_str),
            Some(HEALTHY),
            "case {case:?}: the wired task at {} must be a {HEALTHY} target of the group, got \
             {reported:?}",
            wired[0]
        );

        // ...while the sibling that omits the binding is a target of that group in NO state,
        // over a window far longer than the span a registration takes.
        let unwired = task_addresses(&router, case, row.cluster, &row.unwired()).await;
        assert_eq!(
            unwired.len(),
            1,
            "case {case:?}: the unwired service must run one task with an address, got \
             {unwired:?}"
        );
        let ever_seen = every_registered_address(&router, case, &target_group_arn).await;
        assert!(
            !ever_seen.contains_key(&unwired[0]),
            "case {case:?}: the task of the service that declares no binding must never be a \
             target of the group, got {ever_seen:?}"
        );
        assert_eq!(
            ever_seen.len(),
            1,
            "case {case:?}: the group must hold exactly the wired service's task, got \
             {ever_seen:?}"
        );

        // RULE 2: the load balancer answers with the page the UPSTREAM image's own server
        // ships, which is the image the cache repository holds a copy of.
        let dns_name = outputs
            .get("LoadBalancerDnsName")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no LoadBalancerDnsName output in {outputs:?}")
            });
        for path in ["/", "/index.html"] {
            let (status, answered) = served(&router, &dns_name, path).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the cached image's own server to answer {path} with \
                 200, got {status} with body {answered:?}"
            );
            assert!(
                answered.contains(row.marker),
                "case {case:?}: expected the page the upstream image ships ({:?}) at {path}, got \
                 {answered:?}",
                row.marker
            );
        }
    }
}
