// SPDX-License-Identifier: CC0-1.0

//! The ECS-on-EC2 capacity-provider pattern, deployed DECLARATIVELY: a CloudFormation stack
//! that declares a launch template, an Auto Scaling group holding NO capacity at all, an
//! `AWS::ECS::CapacityProvider` over that group with managed scaling turned on, a cluster that
//! names the provider, a `bridge` task definition and a service that asks for tasks through
//! that provider — and the whole thing CONVERGES.
//!
//! RULE under test, from `ManagedScaling`'s own documentation ("When managed scaling is turned
//! on, Amazon ECS manages the scale-in and scale-out actions of the Auto Scaling group"; "If
//! managed scaling is off, the user must manage the scaling of the Auto Scaling group") read
//! together with the two step members that bound one action (`minimumScalingStepSize`: "the
//! minimum number of Amazon EC2 instances that Amazon ECS will scale out at one time ... When
//! additional capacity is required, Amazon ECS will scale up the minimum scaling step size even
//! if the actual demand is less than the minimum scaling step size"; `maximumScalingStepSize`:
//! "the maximum number of Amazon EC2 instances that Amazon ECS will scale out at one time"):
//!
//! > **A cluster holding tasks it cannot place through a capacity provider whose Auto Scaling
//! > group's scaling Amazon ECS manages is answered by ADDING capacity**: the group's desired
//! > capacity is raised by the shortfall brought inside the provider's two step sizes and held
//! > at the group's own maximum, the instances the group launches register as container
//! > instances of that cluster, and the parked tasks are then placed on them — regardless of
//! > whether the strategy naming the provider is the cluster's default or the service's own,
//! > and regardless of who created the resources.
//!
//! The second half of what a declarative client waits on is pinned beside it: a `CreateStack`
//! reaches a TERMINAL `StackStatus`, so a client's wait-for-stack call ends rather than hanging
//! — and for a stack whose resources are all valid that status is `CREATE_COMPLETE`.
//!
//! Five crates have to agree for this to hold, which is why the suite lives here:
//! CloudFormation makes every create call, Amazon EC2 holds the launch template and the
//! instances, Amazon EC2 Auto Scaling holds the group and launches into it, Amazon ECS owns the
//! provider, the cluster that registers the instances and the service that parks and then
//! places the tasks. Nothing is written into a store by hand: every assertion reads what one of
//! those services persisted, through its own API.
//!
//! The rows differ along what the rule generalises over: how many tasks are asked for, WHERE
//! the capacity-provider strategy is declared (the cluster's `DefaultCapacityProviderStrategy`
//! or the service's own `CapacityProviderStrategy`), and the provider's two step sizes — a
//! maximum that caps a two-task shortfall, and a minimum that is a FLOOR under a one-task one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix Amazon ECS's model fixes (service shape
/// `AmazonEC2ContainerServiceV20141113`); it speaks awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// How a read waits for the clock-owed moves this scenario is made of — the stack's resources
/// coming up, the scale-out Amazon ECS owes the group, the agent registration the launched
/// instances owe their cluster, and the placement the parked tasks then get. The deadline is
/// only here so a failure is a failure rather than a hang; what every case asserts is the
/// CONDITION, never how long it took.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(250);
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The terminal statuses a `CreateStack` may settle in, every one of which ends a client's
/// wait, and the one a stack of valid resources reaches.
const TERMINAL_CREATE_STATUSES: [&str; 4] = [
    "CREATE_COMPLETE",
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// `ContainerInstance$status` of a registration whose instance can take task placements.
const CONTAINER_INSTANCE_ACTIVE: &str = "ACTIVE";

/// Where the strategy that routes the service's tasks through the capacity provider is
/// declared: on the cluster, as the strategy it uses when a request names none, or on the
/// service itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// The cluster's `DefaultCapacityProviderStrategy` — "the capacity provider strategy to
    /// set as the default for the cluster".
    ClusterDefault,
    /// The service's own `CapacityProviderStrategy`.
    ServiceOwn,
}

/// One row of the table: the numbers the declaration carries, the capacity the rule then owes
/// the group, and the names this row's own resources take so no two rows collide.
struct Row {
    case: &'static str,
    /// `AWS::ECS::Service`'s `DesiredCount`: how many tasks the cluster is asked to run, none
    /// of which it can place when the service is created.
    desired_count: u64,
    /// Where the capacity-provider strategy is declared.
    strategy: Strategy,
    /// `ManagedScaling$minimumScalingStepSize` and `maximumScalingStepSize` the provider
    /// declares.
    minimum_step: u64,
    maximum_step: u64,
    /// `MaxSize` of the group, which is the ceiling over any capacity it may hold. Its
    /// `MinSize` and `DesiredCapacity` are zero in every row: the whole point of the pattern
    /// is that the capacity is Amazon ECS's to acquire.
    max_size: u64,
    /// The capacity the group must hold once the scale-out has been taken: the shortfall
    /// inside both step sizes. Asserted as a FLOOR together with the group's own maximum as
    /// the ceiling, because a cluster that still holds an unplaced task is owed another action.
    capacity_owed: u64,
    stack_name: &'static str,
    cluster: &'static str,
    family: &'static str,
    service: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded shape: one task through the cluster's default strategy, one step of \
               one",
        desired_count: 1,
        strategy: Strategy::ClusterDefault,
        minimum_step: 1,
        maximum_step: 1,
        max_size: 3,
        capacity_owed: 1,
        stack_name: "mscfn-default-one",
        cluster: "mscfn-default-one-cluster",
        family: "mscfn-default-one-task",
        service: "mscfn-default-one-service",
    },
    Row {
        case: "two tasks through the service's own strategy: the shortfall, under a maximum step \
               that admits it",
        desired_count: 2,
        strategy: Strategy::ServiceOwn,
        minimum_step: 1,
        maximum_step: 2,
        max_size: 4,
        capacity_owed: 2,
        stack_name: "mscfn-own-two",
        cluster: "mscfn-own-two-cluster",
        family: "mscfn-own-two-task",
        service: "mscfn-own-two-service",
    },
    Row {
        case: "one task with a minimum step of two: the floor, not the demand",
        desired_count: 1,
        strategy: Strategy::ServiceOwn,
        minimum_step: 2,
        maximum_step: 10,
        max_size: 3,
        capacity_owed: 2,
        stack_name: "mscfn-floor-two",
        cluster: "mscfn-floor-two-cluster",
        family: "mscfn-floor-two-task",
        service: "mscfn-floor-two-service",
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
    router.register_service(
        service_com_amazonaws_autoscaling_autoscaling_2011_01_01::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    // The managed-scaling policy Amazon ECS mints for such a provider is a scaling policy of
    // the group, driven by alarms of CloudWatch's: both owners are mounted so the create path
    // writes where it really writes.
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
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

/// Call one operation over an awsQuery wire: the form-encoded body an AWS SDK sends, answered
/// with the HTTP status and the XML body.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
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

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", "2010-05-15", action, params).await
}

/// One Amazon EC2 Auto Scaling call.
async fn autoscaling(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "autoscaling", "2011-01-01", action, params).await
}

/// Call one Amazon ECS operation over its awsJson1_1 wire.
async fn ecs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The template one row deploys: the network, the launch template whose user data configures
/// the container agent with this stack's own cluster, the EMPTY Auto Scaling group, the
/// capacity provider over it, the cluster that names the provider, the task definition and the
/// service that asks for tasks it cannot place.
fn template(row: &Row) -> String {
    // The strategy entry, written where this row declares it. `Base` and `Weight` are the
    // recorded values of the pattern: every task of the service goes through the one provider.
    let entry = "        - CapacityProvider: !Ref Provider\n          Weight: 1\n          \
                 Base: 0\n";
    let cluster_strategy = match row.strategy {
        Strategy::ClusterDefault => {
            format!("      DefaultCapacityProviderStrategy:\n{entry}")
        }
        Strategy::ServiceOwn => String::new(),
    };
    let service_strategy = match row.strategy {
        Strategy::ClusterDefault => String::new(),
        Strategy::ServiceOwn => format!("      CapacityProviderStrategy:\n{entry}"),
    };
    format!(
        r##"Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.90.0.0/16
  SubnetA:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.90.1.0/24
      AvailabilityZone: {REGION}a
  SubnetB:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.90.2.0/24
      AvailabilityZone: {REGION}b
  InstanceSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the container instances of {cluster}
      VpcId: !Ref Vpc
  LaunchTemplate:
    Type: AWS::EC2::LaunchTemplate
    Properties:
      LaunchTemplateName: {cluster}-lt
      LaunchTemplateData:
        ImageId: ami-0123456789abcdef0
        InstanceType: t3.micro
        SecurityGroupIds:
          - !Ref InstanceSecurityGroup
        UserData:
          Fn::Base64: !Sub "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
  Capacity:
    Type: AWS::AutoScaling::AutoScalingGroup
    Properties:
      MinSize: "0"
      MaxSize: "{max_size}"
      DesiredCapacity: "0"
      VPCZoneIdentifier:
        - !Ref SubnetA
        - !Ref SubnetB
      LaunchTemplate:
        LaunchTemplateId: !Ref LaunchTemplate
        Version: !GetAtt LaunchTemplate.LatestVersionNumber
  Provider:
    Type: AWS::ECS::CapacityProvider
    Properties:
      AutoScalingGroupProvider:
        AutoScalingGroupArn: !Ref Capacity
        ManagedScaling:
          Status: ENABLED
          TargetCapacity: 100
          MinimumScalingStepSize: {minimum_step}
          MaximumScalingStepSize: {maximum_step}
          InstanceWarmupPeriod: 60
  Cluster:
    Type: AWS::ECS::Cluster
    Properties:
      ClusterName: {cluster}
      CapacityProviders:
        - !Ref Provider
{cluster_strategy}  Task:
    Type: AWS::ECS::TaskDefinition
    Properties:
      Family: {family}
      NetworkMode: bridge
      RequiresCompatibilities:
        - EC2
      ContainerDefinitions:
        - Name: app
          Image: public.ecr.aws/nginx/nginx:latest
          Memory: 256
          Essential: true
          PortMappings:
            - ContainerPort: 80
              Protocol: tcp
  Service:
    Type: AWS::ECS::Service
    Properties:
      ServiceName: {service}
      Cluster: !Ref Cluster
      TaskDefinition: !Ref Task
      DesiredCount: {desired_count}
{service_strategy}Outputs:
  GroupName:
    Value: !Ref Capacity
  ProviderName:
    Value: !Ref Provider
  ServiceName:
    Value: !GetAtt Service.Name
"##,
        cluster = row.cluster,
        family = row.family,
        service = row.service,
        desired_count = row.desired_count,
        max_size = row.max_size,
        minimum_step = row.minimum_step,
        maximum_step = row.maximum_step,
        cluster_strategy = cluster_strategy,
        service_strategy = service_strategy,
    )
}

/// Read the stack until it reports a status that is no longer in progress, and answer that
/// read's body.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if TERMINAL_CREATE_STATUSES.contains(&reported.as_str()) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack} to reach one of {TERMINAL_CREATE_STATUSES:?}, last \
             read {body}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The reason each FAILED resource of the stack carries, as `(logical id, reason)` — read from
/// the event member that carries both, so no event's reason is paired with another's id.
async fn failures_of(router: &MultiServiceRouter, stack: &str) -> Vec<(String, String)> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.ends_with("_FAILED") {
                return None;
            }
            let id = elements(member, "LogicalResourceId").into_iter().next()?;
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some((id, reason))
        })
        .collect()
}

/// The capacity Amazon EC2 Auto Scaling holds for `group`: its `DesiredCapacity` and the number
/// of instances it reports in the group.
async fn held_capacity(router: &MultiServiceRouter, case: &str, group: &str) -> (u64, usize) {
    let (status, body) = autoscaling(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAutoScalingGroups for {group:?}, got \
         {status} with body {body}"
    );
    let desired = element(&body, "DesiredCapacity")
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("case {case:?}: unreadable DesiredCapacity in {body}"));
    (desired, elements(&body, "InstanceId").len())
}

/// The container instances `cluster` holds, as `(arn, status)` pairs read through the listing
/// and the describe a client polls.
async fn container_instances(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
) -> Vec<(String, String)> {
    let (status, listed) = ecs(
        router,
        "ListContainerInstances",
        &json!({ "cluster": cluster }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListContainerInstances, got {status} with {listed}"
    );
    let arns: Vec<&str> = listed["containerInstanceArns"]
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
        "DescribeContainerInstances",
        &json!({ "cluster": cluster, "containerInstances": arns }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeContainerInstances, got {status} with \
         {described}"
    );
    described["containerInstances"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|instance| {
            Some((
                instance["containerInstanceArn"].as_str()?.to_string(),
                instance["status"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect()
}

/// The service one row's stack created, as `DescribeServices` reports it.
async fn described_service(router: &MultiServiceRouter, case: &str, row: &Row) -> Value {
    let (status, services) = ecs(
        router,
        "DescribeServices",
        &json!({ "cluster": row.cluster, "services": [row.service] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeServices, got {status} with {services}"
    );
    services
}

/// What one row is waiting for, read in one pass so a failure quotes every part of it.
#[derive(Debug, Clone)]
struct Observed {
    desired_capacity: u64,
    group_instances: usize,
    container_instances: Vec<(String, String)>,
    running_count: u64,
}

/// Read the three services until the capacity Amazon ECS owes the group has been acquired, has
/// registered and is running the tasks — or the deadline passes, in which case the last reading
/// is answered and the assertions below report exactly what was and was not there.
async fn settled_capacity(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    group: &str,
) -> Observed {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (desired_capacity, group_instances) = held_capacity(router, case, group).await;
        let registered = container_instances(router, case, row.cluster).await;
        let services = described_service(router, case, row).await;
        let running_count = services["services"][0]["runningCount"]
            .as_u64()
            .unwrap_or_default();
        let observed = Observed {
            desired_capacity,
            group_instances,
            container_instances: registered,
            running_count,
        };
        let settled = observed.desired_capacity >= row.capacity_owed
            && !observed.container_instances.is_empty()
            && observed.running_count == row.desired_count;
        if settled || Instant::now() >= deadline {
            return observed;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

#[tokio::test]
async fn a_declared_managed_scaling_provider_acquires_the_capacity_its_tasks_need() {
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
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );

        // HALF ONE: the create SETTLES. A client's wait-for-stack call ends, at a terminal
        // status — and for a stack of valid resources that status is CREATE_COMPLETE.
        let described = settled_stack(&router, case, row.stack_name).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?}; the \
             resources that failed were {:?}",
            failures_of(&router, row.stack_name).await,
        );

        let outputs = outputs_of(&described);
        let group = outputs
            .get("GroupName")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no GroupName output in {described}"));
        assert_eq!(
            outputs.get("ServiceName").map(String::as_str),
            Some(row.service),
            "case {case:?}: expected the stack to report the service it created, got {outputs:?}"
        );

        // HALF TWO: the capacity Amazon ECS owes the empty group for the tasks it cannot place.
        let observed = settled_capacity(&router, case, row, &group).await;

        let owed = row.capacity_owed;
        let max_size = row.max_size;
        assert!(
            observed.desired_capacity >= owed && observed.desired_capacity <= max_size,
            "case {case:?}: expected the group's DesiredCapacity raised to at least {owed} and \
             no higher than its MaxSize {max_size}, got {observed:?}"
        );
        assert!(
            observed.group_instances >= usize::try_from(owed).unwrap_or(usize::MAX),
            "case {case:?}: expected the group to hold at least {owed} launched instance(s), got \
             {observed:?}"
        );

        // The instances the group launched are the cluster's own capacity: at least one
        // `containerInstanceArn`, every one of them reported ACTIVE.
        assert!(
            !observed.container_instances.is_empty(),
            "case {case:?}: expected at least one container instance registered into \
             {cluster:?}, got {observed:?}",
            cluster = row.cluster,
        );
        for (arn, status) in &observed.container_instances {
            assert_eq!(
                status, CONTAINER_INSTANCE_ACTIVE,
                "case {case:?}: expected {arn:?} reported {CONTAINER_INSTANCE_ACTIVE}, got \
                 {observed:?}"
            );
        }

        // ...and the tasks that were parked on that capacity are running on it.
        let expected_running = row.desired_count;
        assert_eq!(
            observed.running_count, expected_running,
            "case {case:?}: expected the service to run the {expected_running} task(s) the stack \
             asked for, got {observed:?} with the service {:?}",
            described_service(&router, case, row).await,
        );
    }
}
