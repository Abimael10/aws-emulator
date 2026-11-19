// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares a launch-template Auto Scaling group WIRED TO an
//! Elastic Load Balancing v2 target group CONVERGES: the group the stack created launches its
//! declared capacity, each instance reaches `InService`, each is registered as a target of the
//! target group the template named under `TargetGroupARNs`, and the load balancer's checks then
//! decide those registrations' health.
//!
//! RULE: a stack's `AWS::AutoScaling::AutoScalingGroup` is created through the very operation a
//! client calls — `CreateAutoScalingGroup`, whose `TargetGroupARNs` is "the Amazon Resource
//! Names (ARN) of the target groups to associate with the Auto Scaling group. Instances are
//! registered as targets in a target group" — so a group a template declared maintains its
//! `DesiredCapacity` in Amazon EC2, drives each launched instance to the `InService` lifecycle
//! state `Instance$LifecycleState` reports, and registers each one in every attached target
//! group, whose own health checks then settle each registration's `TargetHealth$State` exactly
//! as they do for a group a client created. Nothing about that chain is a property of the
//! template: the stack only makes the calls.
//!
//! Five crates have to agree for that to hold, which is why this suite lives here:
//! CloudFormation makes the creates, Amazon EC2 holds the VPC, the security groups and the
//! launch template, Amazon EC2 Auto Scaling launches the instances into the group and attaches
//! the target group, and Elastic Load Balancing v2 holds the target group, the load balancer and
//! the health verdict. Every assertion reads what one of those services persisted, through the
//! operation a client polls.
//!
//! The rows differ along what the rule generalises over: how many instances the group is asked
//! for, which security group the load balancer performing the checks is associated with and
//! whether an inbound rule of the instances' own group admits those checks — a health check is
//! a connection the load balancer opens to the target, so it arrives only where a rule names
//! both its port and that load balancer as a source — and the cadence and threshold count the
//! target group is checked at. The recorded stack (one instance, an internal Application Load
//! Balancer in the instances' own security group, whose self-referencing rule therefore admits
//! its checks, a target group checked every 10 seconds with a healthy threshold of 2) is one
//! row; a row whose load balancer sits in a security group no rule of the instances' group
//! names is the counter-case, where the same registration is made and settles `unhealthy`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions each awsQuery request below carries.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";
const ELB_VERSION: &str = "2015-12-01";
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI and instance type the launch template of every row names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The stack statuses a create is still in flight in, and the one it must finish in.
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const ROLLBACK_IN_PROGRESS: &str = "ROLLBACK_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Instance$LifecycleState` of an instance a group has finished putting into service.
const IN_SERVICE: &str = "InService";

/// How long a row waits for the clock-owed moves it depends on — the stack's resources coming
/// up, the instances the group then launches, their registration, and the health checks of each
/// registration, which come the target group's own `HealthCheckIntervalSeconds` apart so that a
/// verdict is a threshold of them away — and how often it looks again. Generous, because the
/// deadline is only here so a failure is a failure rather than a hang: every wait below looks
/// for its CONDITION, so machine load decides when a row finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// Which security group the row's load balancer is associated with, and which group an inbound
/// rule of the instances' own group names as the source of the checks it admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admit {
    /// One group for both, as the recorded stack declares it: the load balancer is associated
    /// with the instances' own group, whose self-referencing rule therefore names it.
    SharedGroup,
    /// The load balancer has a group of its own, and the instances' rule names only themselves,
    /// so no rule admits a check from it.
    Nothing,
}

/// One row: the capacity the stack's group is asked for, how the checks reach (or fail to reach)
/// its instances, the cadence and threshold the target group is checked at, the verdict every
/// registration must settle on, and the names the row's own resources take so two rows cannot
/// collide.
struct Row {
    case: &'static str,
    desired: usize,
    admit: Admit,
    /// `CreateTargetGroup$HealthCheckIntervalSeconds` — "the range is 5-300".
    interval: &'static str,
    /// `CreateTargetGroup$HealthyThresholdCount` / `UnhealthyThresholdCount` — "the range is
    /// 2-10".
    healthy_threshold: &'static str,
    unhealthy_threshold: &'static str,
    /// The `TargetHealth$State` every registration of the row must reach, and the
    /// `TargetHealth$Reason` that state carries (empty where the state is reported without one:
    /// "if the target state is healthy, a reason code is not provided").
    state: &'static str,
    reason: &'static str,
    stack: &'static str,
    group: &'static str,
    target_group: &'static str,
    load_balancer: &'static str,
}

/// The recorded stack: one instance, the load balancer in the instances' own security group,
/// checked every 10 seconds with a healthy threshold of 2.
const RECORDED: Row = Row {
    case: "the recorded stack: one instance behind an internal ALB in the instances' own group",
    desired: 1,
    admit: Admit::SharedGroup,
    interval: "10",
    healthy_threshold: "2",
    unhealthy_threshold: "2",
    state: "healthy",
    reason: "",
    stack: "cfnasgtg-recorded",
    group: "cfnasgtg-recorded-asg",
    target_group: "cfnasgtg-recorded-tg",
    load_balancer: "cfnasgtg-recorded-alb",
};

/// Two instances of the same stack shape, checked at the shortest cadence the member allows
/// with a higher healthy threshold: every instance of the declared capacity is registered.
const TWO_INSTANCES: Row = Row {
    case: "two instances of the same declared capacity, checked at the shortest cadence",
    desired: 2,
    admit: Admit::SharedGroup,
    interval: "5",
    healthy_threshold: "3",
    unhealthy_threshold: "2",
    state: "healthy",
    reason: "",
    stack: "cfnasgtg-two",
    group: "cfnasgtg-two-asg",
    target_group: "cfnasgtg-two-tg",
    load_balancer: "cfnasgtg-two-alb",
};

/// The counter-case: the same stack, with the load balancer in a group the instances' rule does
/// not name, so its checks reach nothing.
const UNREACHED: Row = Row {
    case: "one instance whose group admits no check from the ALB's own security group",
    desired: 1,
    admit: Admit::Nothing,
    interval: "5",
    healthy_threshold: "2",
    unhealthy_threshold: "2",
    state: "unhealthy",
    // What an Application Load Balancer words a check nothing answered as: "`Target.Timeout` -
    // The health check requests timed out" (`TargetHealth$Reason`).
    reason: "Target.Timeout",
    stack: "cfnasgtg-unreached",
    group: "cfnasgtg-unreached-asg",
    target_group: "cfnasgtg-unreached-tg",
    load_balancer: "cfnasgtg-unreached-alb",
};

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation action that must succeed.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "cloudformation", form(action, CFN_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One Elastic Load Balancing v2 action that must succeed.
async fn elbv2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancing",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 action that must succeed.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The `InstanceState$Name` Amazon EC2 reports for the instance `id`, and an empty string for
/// an id that service holds no instance for — which is what tells a registered target id that
/// resolves to a real instance from one that resolves to nothing.
async fn instance_state(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let body = ec2(router, case, "DescribeInstances", &[("InstanceId.1", id)]).await;
    block_of(&body, "instanceState")
        .and_then(|state| elements(state, "name").into_iter().next())
        .unwrap_or_default()
}

/// The template one row deploys: the network, the security groups and the one inbound rule the
/// row configures, the launch template, the target group, the internal Application Load Balancer
/// and its listener, and the Auto Scaling group that names the target group under
/// `TargetGroupARNs` — the shape of the recorded stack.
fn template(row: &Row) -> String {
    // The group the load balancer is associated with, and the group the instances' own inbound
    // rule names as the source it admits port 80 from.
    let (balancer_group, source_group) = match row.admit {
        Admit::SharedGroup => ("InstanceSecurityGroup", "InstanceSecurityGroup"),
        Admit::Nothing => ("BalancerSecurityGroup", "InstanceSecurityGroup"),
    };
    let balancer_security_group = match row.admit {
        Admit::SharedGroup => String::new(),
        Admit::Nothing => concat!(
            "  BalancerSecurityGroup:\n",
            "    Type: AWS::EC2::SecurityGroup\n",
            "    Properties:\n",
            "      GroupDescription: the load balancer\n",
            "      VpcId: !Ref Vpc\n",
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
  InstanceSecurityGroup:
    Type: AWS::EC2::SecurityGroup
    Properties:
      GroupDescription: the instances of the group
      VpcId: !Ref Vpc
{balancer_security_group}  InstanceIngress:
    Type: AWS::EC2::SecurityGroupIngress
    Properties:
      GroupId: !Ref InstanceSecurityGroup
      IpProtocol: tcp
      FromPort: 80
      ToPort: 80
      SourceSecurityGroupId: !Ref {source_group}
  LaunchTemplate:
    Type: AWS::EC2::LaunchTemplate
    Properties:
      LaunchTemplateName: {group}-lt
      LaunchTemplateData:
        ImageId: {IMAGE_ID}
        InstanceType: {INSTANCE_TYPE}
        SecurityGroupIds:
          - !Ref InstanceSecurityGroup
  TargetGroup:
    Type: AWS::ElasticLoadBalancingV2::TargetGroup
    Properties:
      Name: {target_group}
      Port: 80
      Protocol: HTTP
      TargetType: instance
      VpcId: !Ref Vpc
      HealthCheckIntervalSeconds: {interval}
      HealthyThresholdCount: {healthy_threshold}
      UnhealthyThresholdCount: {unhealthy_threshold}
  LoadBalancer:
    Type: AWS::ElasticLoadBalancingV2::LoadBalancer
    Properties:
      Name: {load_balancer}
      Scheme: internal
      Type: application
      SecurityGroups:
        - !Ref {balancer_group}
      Subnets:
        - !Ref SubnetA
        - !Ref SubnetB
  Listener:
    Type: AWS::ElasticLoadBalancingV2::Listener
    Properties:
      LoadBalancerArn: !Ref LoadBalancer
      Port: 80
      Protocol: HTTP
      DefaultActions:
        - Type: forward
          TargetGroupArn: !Ref TargetGroup
  AutoScalingGroup:
    Type: AWS::AutoScaling::AutoScalingGroup
    DependsOn: Listener
    Properties:
      AutoScalingGroupName: {group}
      MinSize: "{desired}"
      MaxSize: "{desired}"
      DesiredCapacity: "{desired}"
      VPCZoneIdentifier:
        - !Ref SubnetA
        - !Ref SubnetB
      LaunchTemplate:
        LaunchTemplateId: !Ref LaunchTemplate
        Version: !GetAtt LaunchTemplate.LatestVersionNumber
      TargetGroupARNs:
        - !Ref TargetGroup
Outputs:
  TargetGroupArn:
    Value: !Ref TargetGroup
  AutoScalingGroupName:
    Value: !Ref AutoScalingGroup
"##,
        balancer_security_group = balancer_security_group,
        source_group = source_group,
        balancer_group = balancer_group,
        group = row.group,
        target_group = row.target_group,
        load_balancer = row.load_balancer,
        interval = row.interval,
        healthy_threshold = row.healthy_threshold,
        unhealthy_threshold = row.unhealthy_threshold,
        desired = row.desired,
    )
}

/// The stack as `DescribeStacks` reports it once the create in flight on it has SETTLED: read
/// again until its status is terminal, or the deadline passes.
async fn settled_stack(router: &MultiServiceRouter, row: &Row) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cloudformation(
            router,
            row.case,
            "DescribeStacks",
            &[("StackName", row.stack)],
        )
        .await;
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
async fn stack_failures(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let body = cloudformation(
        router,
        row.case,
        "DescribeStackEvents",
        &[("StackName", row.stack)],
    )
    .await;
    elements(&body, "ResourceStatusReason")
}

/// The instance ids the group `row.group` reports in the `InService` lifecycle state, as a client
/// polling `DescribeAutoScalingGroups` reads them — each read off the member that carries BOTH
/// the id and the state, so an instance in another state is never mistaken for one in service.
async fn in_service_instances(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let body = asg(
        router,
        row.case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", row.group)],
    )
    .await;
    let Some(instances) = block_of(&body, "Instances") else {
        return Vec::new();
    };
    let mut found: Vec<String> = members_of(instances)
        .into_iter()
        .filter(|member| {
            elements(member, "LifecycleState")
                .first()
                .is_some_and(|state| state == IN_SERVICE)
        })
        .filter_map(|member| elements(member, "InstanceId").into_iter().next())
        .collect();
    found.sort();
    found
}

/// Wait until the group holds `row.desired` instances in service, and answer their ids.
async fn settled_capacity(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = in_service_instances(router, row).await;
        if found.len() >= row.desired || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// One registration as `DescribeTargetHealth` reports it: the target's id, the state it is in,
/// and the reason code that state carries (empty for a state reported without one).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    id: String,
    state: String,
    reason: String,
}

/// Every registration the target group `arn` holds right now, in id order — each read off the
/// `TargetHealthDescriptions` member that carries the target AND its health, so a member that
/// ever omitted half of it could not misalign the pairing.
async fn target_health(router: &MultiServiceRouter, row: &Row, arn: &str) -> Vec<Reported> {
    let body = elbv2(
        router,
        row.case,
        "DescribeTargetHealth",
        &[("TargetGroupArn", arn)],
    )
    .await;
    let Some(descriptions) = block_of(&body, "TargetHealthDescriptions") else {
        return Vec::new();
    };
    let mut found: Vec<Reported> = members_of(descriptions)
        .into_iter()
        .filter_map(|member| {
            let target = block_of(member, "Target")?;
            let health = block_of(member, "TargetHealth").unwrap_or_default();
            Some(Reported {
                id: elements(target, "Id").into_iter().next()?,
                state: elements(health, "State").into_iter().next()?,
                reason: elements(health, "Reason")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            })
        })
        .collect();
    found.sort_by(|left, right| left.id.cmp(&right.id));
    found
}

/// Wait until every registration of `arn` has left the transient state a fresh registration is
/// reported in, and answer them.
async fn settled_health(router: &MultiServiceRouter, row: &Row, arn: &str) -> Vec<Reported> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = target_health(router, row, arn).await;
        let settled = found.len() >= row.desired
            && found.iter().all(|reported| reported.state == row.state);
        if settled || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Deploy one row's stack and assert the whole chain the rule states: the stack completes with
/// the outputs it declares, the group's instances reach `InService`, exactly those instances are
/// registered in the declared target group, and each registration settles on the verdict the
/// row's inbound rules make the load balancer's checks reach.
async fn converges(row: &Row) {
    let case = row.case;
    let router = router();
    let body = template(row);
    cloudformation(
        &router,
        case,
        "CreateStack",
        &[("StackName", row.stack), ("TemplateBody", &body)],
    )
    .await;

    let described = settled_stack(&router, row).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must complete; the reasons it recorded were {:?}",
        stack_failures(&router, row).await,
    );

    // The stack exports the two values the flow reads the rest of the chain through.
    let outputs = outputs_of(&described);
    assert_eq!(
        outputs.get("AutoScalingGroupName").map(String::as_str),
        Some(row.group),
        "case {case:?}: the stack must export the group it created, got {outputs:?}"
    );
    let target_group_arn = outputs
        .get("TargetGroupArn")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn output, got {outputs:?}"));
    assert!(
        target_group_arn.contains(row.target_group),
        "case {case:?}: expected the exported ARN to name the declared target group, got \
         {target_group_arn:?}"
    );

    // The group the stack declared launches its capacity and puts every instance in service.
    let in_service = settled_capacity(&router, row).await;
    assert_eq!(
        in_service.len(),
        row.desired,
        "case {case:?}: expected {} instance(s) {IN_SERVICE}, got {in_service:?}",
        row.desired
    );

    // Each of those ids is an instance Amazon EC2 itself holds and reports RUNNING: the group
    // launched them there, so a reader of that service resolves every one.
    for id in &in_service {
        let state = instance_state(&router, case, id).await;
        assert_eq!(
            state, "running",
            "case {case:?}: expected Amazon EC2 to hold {id} running, got {state:?}"
        );
    }

    // ...and the target group holds EXACTLY those instances as its targets.
    let reported = settled_health(&router, row, &target_group_arn).await;
    let registered: Vec<String> = reported.iter().map(|entry| entry.id.clone()).collect();
    assert_eq!(
        registered, in_service,
        "case {case:?}: expected the attached target group to hold exactly the group's \
         in-service instances {in_service:?}, got {reported:?}"
    );

    // ...each with the verdict the checks the row's rules admit (or do not) decide.
    for entry in &reported {
        assert_eq!(
            entry.state, row.state,
            "case {case:?}: expected every registration to reach {:?}, got {reported:?}",
            row.state
        );
        assert_eq!(
            entry.reason, row.reason,
            "case {case:?}: expected the state to carry the reason {:?}, got {reported:?}",
            row.reason
        );
    }

    // The registration is reported through the group's own reader too, which is where a client
    // reads the attachment the template declared.
    let group_body = asg(
        &router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", row.group)],
    )
    .await;
    let attached: Vec<&str> = block_of(&group_body, "TargetGroupARNs")
        .map(members_of)
        .unwrap_or_default();
    assert!(
        attached.iter().any(|value| *value == target_group_arn),
        "case {case:?}: the described group must report the target group the template attached, \
         got {attached:?}"
    );
}

/// The recorded row, as its own test so one row's waits are never spent on another's.
#[tokio::test]
async fn a_stack_declared_asg_puts_its_instance_into_service_and_registers_it_healthy() {
    converges(&RECORDED).await;
}

/// The same stack asked for two instances, with the load balancer in its own security group.
#[tokio::test]
async fn a_stack_declared_asg_registers_every_instance_of_its_declared_capacity() {
    converges(&TWO_INSTANCES).await;
}

/// The counter-case: the registration is still made, and settles unhealthy because no rule
/// admits the load balancer's checks.
#[tokio::test]
async fn a_stack_declared_registration_no_rule_admits_settles_unhealthy() {
    converges(&UNREACHED).await;
}
