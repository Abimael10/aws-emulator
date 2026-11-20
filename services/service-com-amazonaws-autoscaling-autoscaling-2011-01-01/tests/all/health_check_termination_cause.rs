// SPDX-License-Identifier: CC0-1.0

//! WHY a group took a member out of service, as the activity log words it.
//!
//! The rule under test: when a group's health check takes a member out of service, the
//! scaling activity recording that termination carries the fixed sentence Amazon EC2 Auto
//! Scaling words for THE CHECK THAT FAILED — `Activity$Cause` is "the reason the activity
//! began" (`smithy_query(query="shape",shape="Activity")`), and a client reads which check
//! condemned the member off that sentence — so each check has its own wording, reproduced
//! verbatim, article included, and no check's wording is re-worded into another's.
//!
//! The table spans what the rule generalises over: WHICH check reached the verdict, which is
//! the only thing that varies between these sentences — the load balancer's verdict on the
//! target the member occupies, a status a caller recorded with `SetInstanceHealth`, and the
//! Amazon EC2 state change that takes a member no longer running out of its group. Each row
//! is driven end to end over the wire, against the three services on one engine, and read
//! back through `DescribeScalingActivities`, where a client reads it.
//!
//! Every wait below looks again until the condition it needs holds, never for a fixed span.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

// The harness pieces this suite drives the three services through live in `support`, which
// is their one copy for every suite here.
use crate::support::{
    borrowed, create_launch_template, ec2_query, elbv2_query, emulator, list_members, p, query,
    seed_subnets, tag_value, vpc_of, xml_unescape, SeededSubnet,
};

/// The three sentences a termination the group's own health check performed is recorded with,
/// one per check that can reach the verdict.
///
/// Written out here rather than read from the crate: the activity log is what a client reads
/// the reason off, so the assertion states the wording itself — a test re-using the
/// implementation's own literal would assert nothing about it.
const TAKEN_OUT_BY_ELB_CHECK: &str =
    "an instance was taken out of service in response to an ELB system health check failure";
const TAKEN_OUT_BY_USER_CHECK: &str =
    "an instance was taken out of service in response to a user health-check";
const TAKEN_OUT_BY_EC2_CHECK: &str = "an instance was taken out of service in response to an \
                                      EC2 health check indicating it has been terminated or \
                                      stopped";

/// Every sentence the table asserts about, so a row can assert that its own is the one
/// recorded and the others are not.
const EVERY_SENTENCE: &[&str] = &[
    TAKEN_OUT_BY_ELB_CHECK,
    TAKEN_OUT_BY_USER_CHECK,
    TAKEN_OUT_BY_EC2_CHECK,
];

/// The `LifecycleState` of a member that is in service, and the `HealthStatus` a caller
/// records to have one taken out of service.
const LIFECYCLE_IN_SERVICE: &str = "InService";
const HEALTH_STATUS_UNHEALTHY: &str = "Unhealthy";

/// The two `HealthCheckType` values a group here is configured with: "`EC2`" and "`ELB` — Use
/// Elastic Load Balancing health checks" (`CreateAutoScalingGroupType$HealthCheckType`).
const HEALTH_CHECK_TYPE_EC2: &str = "EC2";
const HEALTH_CHECK_TYPE_ELB: &str = "ELB";

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the instances below receive traffic on, which their own security group admits the
/// load balancer's checks on, and a port it admits nothing on — so a health check pointed at
/// the latter is answered by nothing.
const ADMITTED_PORT: &str = "80";
const UNADMITTED_PORT: &str = "22";

/// The `HealthCheckIntervalSeconds` the target group below is checked at: a verdict takes
/// `UnhealthyThresholdCount` consecutive checks, each one interval after the last, so the row
/// that waits for one asks for the shortest interval the member admits ("the range is
/// 5-300").
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";

/// How long a row looks again for the condition it waits on. Generous on purpose — many times
/// the cadence a verdict takes and the spans this service moves an instance over — because
/// what is asserted is that it happens at all, never how fast the host runs.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// How often it looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which check of the group reaches the verdict that takes a member out of service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// The load balancer's verdict on the target the member occupies, which a group whose
    /// `HealthCheckType` names `ELB` is held to.
    LoadBalancerTargetHealth,
    /// The status a caller recorded for the member with `SetInstanceHealth`.
    CallerRecordedStatus,
    /// The Amazon EC2 state change that leaves the member no longer running.
    Ec2StateChange,
}

/// One row of the table: which check condemned the member, and the sentence the activity
/// recording its termination must carry.
struct Row {
    case: &'static str,
    check: Check,
    sentence: &'static str,
}

/// One scaling activity, as a client reads it: what happened and why.
#[derive(Debug, Clone)]
struct Activity {
    description: String,
    cause: String,
}

/// Every scaling activity the group `name` has recorded, in the order reported.
async fn activities(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Activity> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .map(|member| Activity {
            description: xml_unescape(&tag_value(member, "Description")),
            cause: xml_unescape(&tag_value(member, "Cause")),
        })
        .collect()
}

/// Every instance the group reports, as `(instance id, LifecycleState)`.
async fn instance_states(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Instances")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "LifecycleState"),
            )
        })
        .collect()
}

/// Look again until the group holds exactly one instance and it is in service, and answer its
/// id — the CONDITION, never a fixed wait.
async fn settled_instance(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = instance_states(router, case, name).await;
        let settled = held.len() == 1 && held[0].1 == LIFECYCLE_IN_SERVICE;
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected {name:?} to hold one instance in \
                 {LIFECYCLE_IN_SERVICE}, got {held:?}"
            );
            return held[0].0.clone();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until Elastic Load Balancing v2 reports `instance_id` among the targets of
/// `arn`, so the checks the listener starts are checks of a registration already in place.
async fn settled_registration(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    instance_id: &str,
) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let (status, body) =
            elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
        );
        let registered: Vec<String> = list_members(&body, "TargetHealthDescriptions")
            .into_iter()
            .map(|member| tag_value(member, "Id"))
            .collect();
        let holds = registered.iter().any(|id| id == instance_id);
        if holds || Instant::now() >= deadline {
            assert!(
                holds,
                "case {case:?}: expected {arn:?} to hold the target {instance_id}, got \
                 {registered:?}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the group has recorded the activity that terminates `instance_id`, and
/// answer it.
async fn termination_activity(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> Activity {
    let wanted = format!("Terminating EC2 instance: {instance_id}");
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let recorded = activities(router, case, name).await;
        let found = recorded
            .iter()
            .find(|activity| activity.description == wanted)
            .cloned();
        if found.is_some() || Instant::now() >= deadline {
            return found.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected {name:?} to record the activity {wanted:?}, got \
                     {recorded:?}"
                )
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one security group of `vpc_id` through Amazon EC2's own wire and answer its
/// `GroupId`.
async fn create_security_group(router: &MultiServiceRouter, name: &str, vpc_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateSecurityGroup",
        &[
            p("GroupName", name),
            p("GroupDescription", "a health-check cause suite group"),
            p("VpcId", vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateSecurityGroup for {name:?} expected 200, got {status}: {body}"
    );
    let group_id = tag_value(&body, "groupId");
    assert!(
        !group_id.is_empty(),
        "CreateSecurityGroup must report the group's id, got {body}"
    );
    group_id
}

/// Admit TCP `ADMITTED_PORT` into `group_id` from `source_group_id`: the one grant a load
/// balancer's check of a target governed by `group_id` needs, and the port the target group
/// below does NOT check.
async fn authorize_ingress(router: &MultiServiceRouter, group_id: &str, source_group_id: &str) {
    let (status, body) = ec2_query(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            p("GroupId", group_id),
            p("IpPermissions.1.IpProtocol", "tcp"),
            p("IpPermissions.1.FromPort", ADMITTED_PORT),
            p("IpPermissions.1.ToPort", ADMITTED_PORT),
            p("IpPermissions.1.Groups.1.GroupId", source_group_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "AuthorizeSecurityGroupIngress for {group_id:?} expected 200, got {status}: {body}"
    );
}

/// Create one group over the awsQuery wire: placed over `subnets`, holding one instance,
/// health checked the way `health_check_type` says and attached to `target_groups`.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    health_check_type: &str,
    target_groups: &[String],
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "1"),
        p("VPCZoneIdentifier", ids.join(",")),
        p("HealthCheckType", health_check_type),
        p("HealthCheckGracePeriod", "0"),
    ];
    for (index, arn) in target_groups.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Build the estate the ELB row needs — a target group the load balancer checks on a port the
/// instances admit nothing on — and answer the group name and the instance the verdict then
/// condemns.
async fn drive_load_balancer_verdict(
    router: &MultiServiceRouter,
    case: &str,
    subnets: &[SeededSubnet],
    index: usize,
) -> (String, String) {
    let vpc_id = vpc_of(router, &subnets[0]).await;

    // The two security groups the check is read against: the load balancer's own, which is
    // where a check comes FROM, and the instances', which admits it on one port alone — and
    // not on the port the target group's checks connect to.
    let balancer_group =
        create_security_group(router, &format!("hc-cause-lb-{index}"), &vpc_id).await;
    let instance_group =
        create_security_group(router, &format!("hc-cause-instance-{index}"), &vpc_id).await;
    authorize_ingress(router, &instance_group, &balancer_group).await;

    let template = format!("lt-hc-cause-{index}");
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", template.as_str()),
            p("LaunchTemplateData.ImageId", "ami-0123456789abcdef0"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
            p("LaunchTemplateData.SecurityGroupId.1", instance_group.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate expected 200, got {status}: {body}"
    );

    let (status, body) = elbv2_query(
        router,
        "CreateTargetGroup",
        &[
            p("Name", format!("tg-hc-cause-{index}")),
            p("Protocol", "HTTP"),
            p("Port", ADMITTED_PORT),
            p("VpcId", vpc_id.as_str()),
            p("TargetType", TARGET_TYPE_INSTANCE),
            p("HealthCheckPort", UNADMITTED_PORT),
            p("HealthCheckIntervalSeconds", HEALTH_CHECK_INTERVAL_SECONDS),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTargetGroup expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "TargetGroupArn");

    let name = format!("asg-hc-cause-{index}");
    create_group(
        router,
        case,
        &name,
        &template,
        subnets,
        HEALTH_CHECK_TYPE_ELB,
        std::slice::from_ref(&arn),
    )
    .await;
    let original = settled_instance(router, case, &name).await;
    settled_registration(router, case, &arn, &original).await;

    // The load balancer that CHECKS that target group: the listener forwarding to it is what
    // starts the checks, so they begin on a registration already in place.
    let mut params = vec![
        p("Name", format!("lb-hc-cause-{index}")),
        p("Scheme", "internal"),
        p("Type", "application"),
        p("SecurityGroups.member.1", balancer_group.clone()),
    ];
    for (position, subnet) in subnets.iter().enumerate() {
        params.push(p(
            format!("Subnets.member.{}", position + 1).as_str(),
            subnet.id.clone(),
        ));
    }
    let (status, body) = elbv2_query(router, "CreateLoadBalancer", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLoadBalancer expected 200, got {status}: {body}"
    );
    let load_balancer = tag_value(&body, "LoadBalancerArn");

    let (status, body) = elbv2_query(
        router,
        "CreateListener",
        &[
            p("LoadBalancerArn", load_balancer.as_str()),
            p("Protocol", "HTTP"),
            p("Port", ADMITTED_PORT),
            p("DefaultActions.member.1.Type", "forward"),
            p("DefaultActions.member.1.TargetGroupArn", arn.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateListener expected 200, got {status}: {body}"
    );

    (name, original)
}

/// Build `row`'s estate, have the check it names condemn the group's one member, and assert
/// the sentence the termination activity records.
async fn drive(row: &Row, index: usize) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);

    let (name, original) = match row.check {
        Check::LoadBalancerTargetHealth => {
            drive_load_balancer_verdict(&router, case, &subnets, index).await
        }
        Check::CallerRecordedStatus => {
            let template = format!("lt-hc-cause-{index}");
            create_launch_template(&router, &template).await;
            let name = format!("asg-hc-cause-{index}");
            create_group(
                &router,
                case,
                &name,
                &template,
                &subnets,
                HEALTH_CHECK_TYPE_EC2,
                &[],
            )
            .await;
            let original = settled_instance(&router, case, &name).await;
            let (status, body) = query(
                &router,
                "SetInstanceHealth",
                &[
                    ("InstanceId", original.as_str()),
                    ("HealthStatus", HEALTH_STATUS_UNHEALTHY),
                    ("ShouldRespectGracePeriod", "false"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: SetInstanceHealth expected 200, got {status}: {body}"
            );
            (name, original)
        }
        Check::Ec2StateChange => {
            let template = format!("lt-hc-cause-{index}");
            create_launch_template(&router, &template).await;
            let name = format!("asg-hc-cause-{index}");
            create_group(
                &router,
                case,
                &name,
                &template,
                &subnets,
                HEALTH_CHECK_TYPE_EC2,
                &[],
            )
            .await;
            let original = settled_instance(&router, case, &name).await;
            // Terminated OUTSIDE Auto Scaling, by a caller of Amazon EC2's own API: the
            // group's health check is what notices the member no longer runs.
            let (status, body) =
                ec2_query(&router, "TerminateInstances", &[p("InstanceId.1", original.clone())])
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}: TerminateInstances expected 200, got {status}: {body}"
            );
            (name, original)
        }
    };

    let recorded = termination_activity(&router, case, &name, &original).await;
    assert!(
        recorded.cause.contains(row.sentence),
        "case {case:?}: expected the termination of {original} to be recorded with the cause \
         {:?}, got {:?}",
        row.sentence,
        recorded.cause
    );
    // The sentence is the one the FAILED CHECK words, so no other check's wording stands in
    // for it: a client telling the checks apart by that sentence reads exactly one of them.
    for other in EVERY_SENTENCE
        .iter()
        .filter(|sentence| **sentence != row.sentence)
    {
        assert!(
            !recorded.cause.contains(other),
            "case {case:?}: expected the termination of {original} to carry no other check's \
             sentence, got {other:?} in {:?}",
            recorded.cause
        );
    }
}

/// RULE: the activity recording a termination the group's health check performed carries the
/// fixed sentence Amazon EC2 Auto Scaling words for the check that reached the verdict —
/// the load balancer's verdict on the target the member occupies, read here through a target
/// group whose checks the member's security group admits nothing from.
#[tokio::test]
async fn a_load_balancer_verdict_records_its_own_sentence() {
    drive(
        &Row {
            case: "the load balancer reports the target unhealthy on a port nothing admits it \
                   on",
            check: Check::LoadBalancerTargetHealth,
            sentence: TAKEN_OUT_BY_ELB_CHECK,
        },
        0,
    )
    .await;
}

/// The same rule for the status a CALLER records: a different check, so a different sentence.
#[tokio::test]
async fn a_caller_recorded_status_records_its_own_sentence() {
    drive(
        &Row {
            case: "a caller records the member Unhealthy",
            check: Check::CallerRecordedStatus,
            sentence: TAKEN_OUT_BY_USER_CHECK,
        },
        1,
    )
    .await;
}

/// And for the Amazon EC2 state change that leaves the member no longer running.
#[tokio::test]
async fn an_amazon_ec2_state_change_records_its_own_sentence() {
    drive(
        &Row {
            case: "Amazon EC2 no longer runs the member",
            check: Check::Ec2StateChange,
            sentence: TAKEN_OUT_BY_EC2_CHECK,
        },
        2,
    )
    .await;
}
