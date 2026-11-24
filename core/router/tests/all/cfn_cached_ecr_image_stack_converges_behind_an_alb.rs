// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that wires an Amazon ECR PULL THROUGH CACHE RULE and runs the image
//! it caches on a Fargate service behind an Application Load Balancer converges: the rule is
//! registered in Amazon ECR itself, the task pulls through the cache repository that rule
//! creates, and Elastic Load Balancing v2 reports the task's address `healthy` — on the first
//! deploy and again after the image tag is rolled.
//!
//! RULE: a resource type this emulator carries a provider for is CREATED in the service that
//! owns it, so an `AWS::ECR::PullThroughCacheRule` a stack declares is the rule
//! `CreatePullThroughCacheRule` registers — reported by `DescribePullThroughCacheRules` with
//! the upstream and the prefixes the declaration stated, in force for every pull under its
//! prefix, and taken off by `DeletePullThroughCacheRule` when the stack is deleted. A pull
//! under that prefix caches the upstream image, and a cache repository holds the UPSTREAM image
//! itself, so the container of a task definition whose image is the cached URI runs that
//! upstream image's own program: a web server image serves its root, the target group
//! health-checking `/` reports the task `healthy`, and a deployment that changes nothing but
//! the tag rolls the service to a task that registers `healthy` on the same target group.
//!
//! The first test's rows differ along what the rule generalises over: the PREFIX the rule
//! caches under (one segment and two), whether the declaration states an
//! `UpstreamRepositoryPrefix` of its own, and which upstream namespace is cached beneath it.
//! The second drives the whole estate the recorded client deploys and varies the IMAGE TAG the
//! stack's parameter carries, which is the one thing its update changes.
//!
//! Six crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes every create and update call, IAM holds the execution role Amazon ECS
//! pulls as, Amazon ECR holds the cache rule and the repository the first pull creates, Amazon
//! EC2 owns the network the task is placed in and the path the pull takes, Amazon ECS owns the
//! task and its container's program, and Elastic Load Balancing v2 holds the registration and
//! the checks it is reported from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder, element readers and output reader this test crate keeps for
// awsQuery suites (`all/query_wire.rs`: "Nothing new should write another one — call this
// module").
use crate::query_wire::{element, elements, form_encode, items_of, members_of, outputs_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query, a CloudFormation, an ELBv2 and an IAM request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const CFN_VERSION: &str = "2010-05-15";
const ELB_VERSION: &str = "2015-12-01";

/// The upstream registry every rule below caches from, which is the one public registry this
/// emulator serves.
const UPSTREAM_REGISTRY: &str = "public.ecr.aws";

/// The capability a create has to acknowledge for a template that names the IAM role it
/// creates: "if you have IAM resources with custom names, you must specify
/// CAPABILITY_NAMED_IAM" (`CreateStack$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The stack statuses a deployment is still in flight in, and the two it finishes a create and
/// an update in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
const UPDATE_COMPLETE_CLEANUP: &str = "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const UPDATE_ROLLBACK_IN_PROGRESS: &str = "UPDATE_ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";

/// How long each wait below looks for its CONDITION, and how often it looks again. Generous on
/// purpose: the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// `TargetHealth$State` of a target the load balancer sends requests to.
const HEALTHY: &str = "healthy";

/// The one container every task definition below declares, and the port its image's shipped
/// configuration listens on.
const CONTAINER: &str = "app";
const CONTAINER_PORT: i64 = 80;

/// The `HealthCheckIntervalSeconds` the target group is declared with: a verdict takes a
/// threshold's worth of checks, each one interval after the last, so a suite that waits for one
/// asks for the shortest interval the member admits ("the range is 5-300").
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";

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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
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
    // The CLASSIC Elastic Load Balancing front door too: the two Elastic Load Balancing models
    // share one endpoint and one signing name, so a request CloudFormation makes for an ELBv2
    // resource reaches that door and is claimed from there by the model that declares the
    // action and the version.
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and signature are not credentials: the router above runs with request authentication off, so
/// nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
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
async fn cfn(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
    case: &str,
) -> String {
    let (status, body) = query(router, "cloudformation", CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from CloudFormation {action}, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 call that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from EC2 {action}, got {status} with body {body}"
    );
    body
}

/// One Elastic Load Balancing v2 call that must succeed, answered with its XML body.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "elasticloadbalancingv2",
        ELB_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from ELBv2 {action}, got {status} with body {body}"
    );
    body
}

/// One `awsJson1_1` RPC call, answered with its status and parsed body.
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

/// One Amazon ECS call that must succeed.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from ECS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECR call, answered with its status and parsed body: a read of a repository the
/// registry does not hold is a REFUSAL this suite reads rather than asserts away.
async fn ecr(router: &MultiServiceRouter, operation: &str, request: Value) -> (u16, Value) {
    rpc(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        request,
    )
    .await
}

/// The network the stacks below deploy into: a VPC with an internet gateway, its own MAIN route
/// table sending the default route to that gateway, two subnets in different zones, and one
/// self-referencing security group the tasks and the load balancer both sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id the template carries is one EC2
/// minted and the path the image pull takes is one its own records hold.
async fn seed_network(router: &MultiServiceRouter, cidr: &str, zones: [(&str, &str); 2]) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", cidr)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let tables = ec2(
        router,
        "DescribeRouteTables",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc.as_str()),
        ],
    )
    .await;
    let main = items_of(&tables)
        .into_iter()
        .find(|item| elements(item, "main").iter().any(|main| main == "true"))
        .and_then(|item| elements(item, "routeTableId").into_iter().next())
        .unwrap_or_else(|| panic!("the VPC must come with a main route table, got {tables}"));
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", main.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    let mut subnets = Vec::new();
    for (subnet_cidr, zone) in zones {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", subnet_cidr),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "cachedstack-group"),
                ("GroupDescription", "the tasks and the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The one rule this suite authorizes: the group takes ITSELF as a source on every port,
    // which is what lets the load balancer's check of a target in it arrive.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", "0"),
            ("IpPermissions.1.ToPort", "65535"),
            ("IpPermissions.1.Groups.1.GroupId", group.as_str()),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        group,
    }
}

/// The stack as `DescribeStacks` reports it once the deployment in flight on it has SETTLED:
/// looked at again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", stack)], case).await;
        let reported = elements(&body, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default();
        let in_flight = [
            CREATE_IN_PROGRESS,
            UPDATE_IN_PROGRESS,
            UPDATE_COMPLETE_CLEANUP,
            ROLLBACK_IN_PROGRESS,
            UPDATE_ROLLBACK_IN_PROGRESS,
            DELETE_IN_PROGRESS,
        ]
        .contains(&reported.as_str());
        if !in_flight || Instant::now() >= deadline {
            return body;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Every `ResourceStatusReason` one stack's event stream carries, which is where the refusal a
/// failing resource earned is reported.
async fn stack_reasons(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    let body = cfn(
        router,
        "DescribeStackEvents",
        &[("StackName", stack)],
        case,
    )
    .await;
    elements(&body, "ResourceStatusReason")
}

/// The rules Amazon ECR reports for one prefix, as `(upstreamRegistryUrl,
/// upstreamRepositoryPrefix)` — `None` when the registry holds no rule under it.
async fn reported_rule(router: &MultiServiceRouter, prefix: &str) -> Option<(String, String)> {
    let (status, body) = ecr(
        router,
        "DescribePullThroughCacheRules",
        json!({ "ecrRepositoryPrefixes": [prefix] }),
    )
    .await;
    if status != 200 {
        return None;
    }
    let rule = body["pullThroughCacheRules"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|rule| rule["ecrRepositoryPrefix"].as_str() == Some(prefix))?;
    Some((
        rule["upstreamRegistryUrl"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        rule["upstreamRepositoryPrefix"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    ))
}

/// One row of the first test: the pull through cache rule a stack declares, and what the
/// registry must then report for it.
struct RuleRow {
    case: &'static str,
    logical_id: &'static str,
    prefix: &'static str,
    /// The `UpstreamRepositoryPrefix` the declaration states, or `None` for one that states
    /// none — which caches from the upstream registry's ROOT, the value that registry reports
    /// for a rule declaring no prefix of its own.
    upstream_prefix: Option<&'static str>,
    /// The `upstreamRepositoryPrefix` `DescribePullThroughCacheRules` must then report, which
    /// is the one the declaration stated and nothing for a declaration that stated none.
    reported_upstream_prefix: &'static str,
}

const RULE_ROWS: [RuleRow; 3] = [
    RuleRow {
        case: "a one-segment prefix caching the upstream registry's root",
        logical_id: "RootRule",
        prefix: "mirror",
        upstream_prefix: None,
        reported_upstream_prefix: "",
    },
    RuleRow {
        case: "a two-segment prefix caching beneath an upstream namespace",
        logical_id: "NamespacedRule",
        prefix: "cached/docker",
        upstream_prefix: Some("docker/library"),
        reported_upstream_prefix: "docker/library",
    },
    RuleRow {
        case: "a second prefix caching beneath another upstream namespace",
        logical_id: "PublisherRule",
        prefix: "cached/nginx",
        upstream_prefix: Some("nginx"),
        reported_upstream_prefix: "nginx",
    },
];

/// The template the first test deploys: one `AWS::ECR::PullThroughCacheRule` per row.
fn rules_template() -> String {
    let mut template = String::from("Resources:\n");
    for row in &RULE_ROWS {
        template.push_str(&format!(
            "  {logical}:\n    Type: AWS::ECR::PullThroughCacheRule\n    Properties:\n      \
             EcrRepositoryPrefix: {prefix}\n      UpstreamRegistryUrl: {UPSTREAM_REGISTRY}\n",
            logical = row.logical_id,
            prefix = row.prefix,
        ));
        if let Some(upstream) = row.upstream_prefix {
            template.push_str(&format!("      UpstreamRepositoryPrefix: {upstream}\n"));
        }
    }
    template.push_str("Outputs:\n");
    for row in &RULE_ROWS {
        template.push_str(&format!(
            "  {logical}Out:\n    Value: !Ref {logical}\n",
            logical = row.logical_id
        ));
    }
    template
}

/// RULE: an `AWS::ECR::PullThroughCacheRule` a stack declares is the rule
/// `CreatePullThroughCacheRule` registers in Amazon ECR itself — reported by that registry's own
/// read with the upstream and the prefixes the declaration stated, published under the prefix
/// its `Ref` returns, and taken off again when the stack is deleted.
#[tokio::test]
async fn a_stacks_declared_cache_rules_are_registered_in_the_registry() {
    let router = build_router();
    let case = "the declared cache rules";
    let stack = "cachedrules";
    let template = rules_template();

    for row in &RULE_ROWS {
        let held = reported_rule(&router, row.prefix).await;
        assert_eq!(
            held, None,
            "case {:?}: the registry must hold no rule under {} before the stack deploys, got \
             {held:?}",
            row.case, row.prefix
        );
    }

    cfn(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template.as_str())],
        case,
    )
    .await;
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        elements(&described, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default(),
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {described} with reasons {:?}",
        stack_reasons(&router, case, stack).await
    );

    let outputs = outputs_of(&described);
    for row in &RULE_ROWS {
        let held = reported_rule(&router, row.prefix).await;
        assert_eq!(
            held,
            Some((
                UPSTREAM_REGISTRY.to_string(),
                row.reported_upstream_prefix.to_string()
            )),
            "case {:?}: expected the registry to hold the declared rule, got {held:?}",
            row.case
        );
        // "Ref returns the EcrRepositoryPrefix": the prefix the rule is registered under is
        // what the stack publishes for the resource.
        let published = outputs.get(&format!("{}Out", row.logical_id));
        assert_eq!(
            published.map(String::as_str),
            Some(row.prefix),
            "case {:?}: expected the stack to publish the rule's prefix, got {published:?}",
            row.case
        );
    }

    cfn(&router, "DeleteStack", &[("StackName", stack)], case).await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let gone = {
            let mut gone = true;
            for row in &RULE_ROWS {
                if reported_rule(&router, row.prefix).await.is_some() {
                    gone = false;
                }
            }
            gone
        };
        if gone {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every rule the stack registered must be taken off by its deletion, \
             and the registry still reports one"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The prefix the second test's rule caches under, and the upstream repository its task
/// definition's image addresses beneath it.
const STACK_RULE_PREFIX: &str = "deployed/mirror";
const UPSTREAM_REPOSITORY: &str = "docker/library/nginx";

/// The two tags the stack's `ImageTag` parameter carries: the one it is created with and the one
/// its update rolls to. Both are tags the gallery's mirror of that image publishes
/// (`service_com_amazonaws_ecrpublic_spencerfrontendservice::state::published_images`, the
/// catalogue of releases this emulator holds).
const FIRST_TAG: &str = "alpine";
const ROLLED_TAG: &str = "1.27-alpine";

/// The template the second test deploys: the cache rule, the execution role Amazon ECS pulls
/// as, the cluster, the task definition whose container runs the cached image, the load
/// balancer, its target group and listener, and the Fargate service bound to that group.
fn estate_template(stack: &str) -> String {
    format!(
        r#"Parameters:
  ImageTag:
    Type: String
  VpcId:
    Type: String
  SubnetA:
    Type: String
  SubnetB:
    Type: String
  SecurityGroup:
    Type: String
Resources:
  CacheRule:
    Type: AWS::ECR::PullThroughCacheRule
    Properties:
      EcrRepositoryPrefix: {STACK_RULE_PREFIX}
      UpstreamRegistryUrl: {UPSTREAM_REGISTRY}
  ExecutionRole:
    Type: AWS::IAM::Role
    Properties:
      RoleName: {stack}-execution
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement:
          - Effect: Allow
            Action: sts:AssumeRole
            Principal:
              Service: ecs-tasks.amazonaws.com
      Policies:
        - PolicyName: pull
          PolicyDocument:
            Version: "2012-10-17"
            Statement:
              - Effect: Allow
                Action:
                  - ecr:GetAuthorizationToken
                  - ecr:BatchCheckLayerAvailability
                  - ecr:BatchGetImage
                  - ecr:GetDownloadUrlForLayer
                  - ecr:CreateRepository
                  - ecr:BatchImportUpstreamImage
                Resource: "*"
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {stack}cluster
  Task:
    Type: AWS::ECS::TaskDefinition
    DependsOn: CacheRule
    Properties:
      Family: {stack}task
      Cpu: "256"
      Memory: "512"
      NetworkMode: awsvpc
      RequiresCompatibilities:
        - FARGATE
      ExecutionRoleArn: !GetAtt ExecutionRole.Arn
      ContainerDefinitions:
        - Name: {CONTAINER}
          Essential: true
          Image: !Sub "${{AWS::AccountId}}.dkr.ecr.${{AWS::Region}}.amazonaws.com/{STACK_RULE_PREFIX}/{UPSTREAM_REPOSITORY}:${{ImageTag}}"
          PortMappings:
            - ContainerPort: {CONTAINER_PORT}
              Protocol: tcp
  LoadBalancer:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {stack}-alb
      Scheme: internet-facing
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
      SecurityGroups:
        - !Ref SecurityGroup
  TargetGroup:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {stack}-tg
      Protocol: HTTP
      Port: {CONTAINER_PORT}
      TargetType: ip
      VpcId: !Ref VpcId
      HealthCheckProtocol: HTTP
      HealthCheckPath: /
      HealthCheckIntervalSeconds: {HEALTH_CHECK_INTERVAL_SECONDS}
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref LoadBalancer
      Protocol: HTTP
      Port: {CONTAINER_PORT}
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref TargetGroup
  Service:
    Type: AWS::ECS::Service
    DependsOn: Listener
    Properties:
      ServiceName: {stack}svc
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: 1
      LaunchType: FARGATE
      LoadBalancers:
        - ContainerName: {CONTAINER}
          ContainerPort: {CONTAINER_PORT}
          TargetGroupArn: !Ref TargetGroup
      NetworkConfiguration:
        AwsvpcConfiguration:
          AssignPublicIp: ENABLED
          SecurityGroups:
            - !Ref SecurityGroup
          Subnets:
            - !Ref SubnetA
            - !Ref SubnetB
Outputs:
  TargetGroupArn:
    Value: !Ref TargetGroup
  ServiceName:
    Value: !GetAtt Service.Name
  ClusterName:
    Value: !Ref Cluster
"#
    )
}

/// Every `(id, port, state)` the group `arn` reports a target at.
async fn target_health(router: &MultiServiceRouter, arn: &str) -> Vec<(String, String, String)> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|description| {
            let text = |tag: &str| {
                elements(description, tag)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            let id = text("Id");
            if id.is_empty() {
                return None;
            }
            Some((id, text("Port"), text("State")))
        })
        .collect()
}

/// The images the RUNNING tasks of one service report for their containers, which is where the
/// revision the service now runs shows.
async fn running_images(router: &MultiServiceRouter, cluster: &str, service: &str) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({ "cluster": cluster, "serviceName": service, "desiredStatus": "RUNNING" }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": arns }),
    )
    .await;
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
        .flat_map(|task| task["containers"].as_array().into_iter().flatten())
        .filter_map(|container| container["image"].as_str().map(str::to_string))
        .collect()
}

/// Wait for the group `arn` to report at least one `healthy` target whose task runs `image`,
/// which is the condition a client deploying this estate polls for.
async fn healthy_target_running(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    cluster: &str,
    service: &str,
    image: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_health(router, arn).await;
        let images = running_images(router, cluster, service).await;
        let healthy: Vec<&(String, String, String)> = reported
            .iter()
            .filter(|(_, _, state)| state == HEALTHY)
            .collect();
        let settled = !healthy.is_empty() && images.iter().any(|held| held == image);
        if settled {
            for (_, port, _) in healthy {
                assert_eq!(
                    port,
                    &CONTAINER_PORT.to_string(),
                    "case {case:?}: a healthy target must carry the bound container port, got \
                     {reported:?}"
                );
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a healthy target of a task running {image}, got {reported:?} \
             with the running images {images:?} and the stopped reasons {:?}",
            stopped_reasons(router, cluster, service).await
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// `Task$stoppedReason` of every stopped task of one service, quoted by a wait that did not
/// settle so a failure says why rather than only that nothing happened.
async fn stopped_reasons(
    router: &MultiServiceRouter,
    cluster: &str,
    service: &str,
) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({ "cluster": cluster, "serviceName": service, "desiredStatus": "STOPPED" }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": arns }),
    )
    .await;
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| task["stoppedReason"].as_str().map(str::to_string))
        .collect()
}

/// The tags the cache repository the pulls created holds, and an empty list for a repository the
/// registry does not hold at all.
async fn cached_tags(router: &MultiServiceRouter) -> Vec<String> {
    let repository = format!("{STACK_RULE_PREFIX}/{UPSTREAM_REPOSITORY}");
    let (status, body) = ecr(
        router,
        "ListImages",
        json!({ "repositoryName": repository }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["imageIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id["imageTag"].as_str().map(str::to_string))
        .collect()
}

/// RULE: the stack's cache rule is really registered, so the task pulls the upstream image
/// through the cache repository that rule creates and runs that image's own program — the target
/// group reports it `healthy` — and a deployment that changes nothing but the image tag rolls
/// the service to a task that reaches `healthy` on the same, unchanged target group.
#[tokio::test]
async fn a_cached_image_stack_converges_and_rolls_on_a_tag_change() {
    let router = build_router();
    let stack = "cachedestate";
    let network = seed_network(
        &router,
        "10.86.0.0/16",
        [("10.86.1.0/24", "us-east-1a"), ("10.86.2.0/24", "us-east-1b")],
    )
    .await;
    let template = estate_template(stack);

    // The create: the stack is deployed with the first tag.
    let created = settle_deployment(
        &router,
        "the create",
        stack,
        "CreateStack",
        &template,
        FIRST_TAG,
        &network,
    )
    .await;
    assert_eq!(
        elements(&created, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default(),
        CREATE_COMPLETE,
        "case {:?}: expected the stack to deploy, got {created} with reasons {:?}",
        "the create",
        stack_reasons(&router, "the create", stack).await
    );
    let outputs = outputs_of(&created);
    let target_group = outputs
        .get("TargetGroupArn")
        .cloned()
        .unwrap_or_else(|| panic!("the stack must publish its target group, got {created}"));
    let cluster = outputs
        .get("ClusterName")
        .cloned()
        .unwrap_or_else(|| panic!("the stack must publish its cluster, got {created}"));
    let service = outputs
        .get("ServiceName")
        .cloned()
        .unwrap_or_else(|| panic!("the stack must publish its service, got {created}"));

    let first_image = cached_image(&router, FIRST_TAG).await;
    healthy_target_running(
        &router,
        "the create",
        &target_group,
        &cluster,
        &service,
        &first_image,
    )
    .await;
    let tags = cached_tags(&router).await;
    assert!(
        tags.iter().any(|held| held == FIRST_TAG),
        "case {:?}: the pull must have cached the upstream image under the tag it asked for, got \
         {tags:?}",
        "the create"
    );

    // The revision: the same estate with nothing but the image tag changed.
    let updated = settle_deployment(
        &router,
        "the rolled tag",
        stack,
        "UpdateStack",
        &template,
        ROLLED_TAG,
        &network,
    )
    .await;
    assert_eq!(
        elements(&updated, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default(),
        UPDATE_COMPLETE,
        "case {:?}: expected the revision to deploy, got {updated} with reasons {:?}",
        "the rolled tag",
        stack_reasons(&router, "the rolled tag", stack).await
    );
    // The target group is the SAME one: the update rolls the service's tasks rather than
    // replacing what routes to them.
    let rolled_outputs = outputs_of(&updated);
    assert_eq!(
        rolled_outputs.get("TargetGroupArn"),
        Some(&target_group),
        "case {:?}: the update must keep the target group the create published, got \
         {rolled_outputs:?}",
        "the rolled tag"
    );

    let rolled_image = cached_image(&router, ROLLED_TAG).await;
    healthy_target_running(
        &router,
        "the rolled tag",
        &target_group,
        &cluster,
        &service,
        &rolled_image,
    )
    .await;
    let tags = cached_tags(&router).await;
    for expected in [FIRST_TAG, ROLLED_TAG] {
        assert!(
            tags.iter().any(|held| held == expected),
            "case {:?}: the cache repository must hold every tag a pull asked for, got {tags:?}",
            "the rolled tag"
        );
    }
}

/// The image reference the stack's task definition carries for `tag`, built from the registry
/// host Amazon ECR itself publishes for this account's registry, so the reference asserted
/// against is the one that registry answers for.
async fn cached_image(router: &MultiServiceRouter, tag: &str) -> String {
    let (status, body) = ecr(router, "DescribeRegistry", json!({})).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from ECR DescribeRegistry, got {status} with {body}"
    );
    let registry = body["registryId"]
        .as_str()
        .unwrap_or_else(|| panic!("DescribeRegistry must report a registryId, got {body}"));
    format!(
        "{registry}.dkr.ecr.{REGION}.amazonaws.com/{STACK_RULE_PREFIX}/{UPSTREAM_REPOSITORY}:{tag}"
    )
}

/// Make one deployment of the estate template with the parameters it takes, and answer the stack
/// as `DescribeStacks` reports it once that deployment has settled.
async fn settle_deployment(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    action: &str,
    template: &str,
    tag: &str,
    network: &Network,
) -> String {
    let members: Vec<(&str, &str)> = vec![
        ("StackName", stack),
        ("TemplateBody", template),
        ("Capabilities.member.1", NAMED_IAM),
        ("Parameters.member.1.ParameterKey", "ImageTag"),
        ("Parameters.member.1.ParameterValue", tag),
        ("Parameters.member.2.ParameterKey", "VpcId"),
        ("Parameters.member.2.ParameterValue", network.vpc.as_str()),
        ("Parameters.member.3.ParameterKey", "SubnetA"),
        (
            "Parameters.member.3.ParameterValue",
            network.subnets[0].as_str(),
        ),
        ("Parameters.member.4.ParameterKey", "SubnetB"),
        (
            "Parameters.member.4.ParameterValue",
            network.subnets[1].as_str(),
        ),
        ("Parameters.member.5.ParameterKey", "SecurityGroup"),
        ("Parameters.member.5.ParameterValue", network.group.as_str()),
    ];
    cfn(router, action, &members, case).await;
    settled_stack(router, case, stack).await
}
