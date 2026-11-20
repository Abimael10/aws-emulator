// SPDX-License-Identifier: CC0-1.0

//! An Auto Scaling group's whole delete-target-group lifecycle, through the WHOLE mounted
//! service set — the one `core/router/src/lib.rs` assembles for the binary — because it only
//! holds if Auto Scaling, Amazon EC2 and Elastic Load Balancing v2 agree over one engine.
//!
//! THE RULE: a group's `DesiredCapacity` is the capacity it MAINTAINS, so every phase of that
//! lifecycle reaches a state a client can read. Each instance the group launches is registered
//! as a target of the target group it is attached to — `CreateAutoScalingGroup$TargetGroupARNs`
//! is "the Amazon Resource Names (ARN) of the target groups to associate with the Auto Scaling
//! group. Instances are registered as targets in a target group" — so a scale-out shows up
//! both on the group and in that target group's own `DescribeTargetHealth`. A scale-out whose
//! attached target group has been DELETED out of band launches nothing and records a `Failed`
//! activity naming it, leaving the group at the capacity it had. And the capacity that launch
//! could not place stays OWED: once the client has repaired the attachments (detaching the
//! target group that is gone, attaching one that exists), the group launches it and registers
//! it there, without being asked for the capacity again.
//!
//! The rule's rows — what blocks a launch, what lifts it, and how large the shortfall is —
//! live in the owning crate's own suite
//! (`service-com-amazonaws-autoscaling-autoscaling-2011-01-01`,
//! `tests/all/owed_capacity_convergence.rs`); this is the one path through the assembled
//! emulator, where every id is minted by the service that owns it.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

const REGION: &str = "us-east-1";
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const ASG_VERSION: &str = "2011-01-01";
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
/// How long a phase waits for the clock-owed moves it depends on — the launch, the
/// registration, and the launch the group owes after the repair — and how often it looks. FAR
/// above the spans this emulator owes them, because the deadline is only here so a failure is
/// a failure rather than a hang: every wait below looks for its condition, so machine load
/// decides when a phase finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

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

async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

async fn asg(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

async fn registered(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    elements(&body, "Id")
}

async fn in_service(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    elements(&body, "LifecycleState")
}

#[tokio::test]
async fn every_phase_of_a_groups_target_group_lifecycle_reaches_an_observable_state() {
    let router = router();
    let vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.91.1.0/24"),
                ("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await,
        "subnetId",
    );
    ec2(
        &router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", "owed-template"),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    let home = element(
        &elbv2(
            &router,
            "CreateTargetGroup",
            &[
                ("Name", "owed-home"),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("VpcId", vpc.as_str()),
                ("TargetType", "instance"),
            ],
        )
        .await,
        "TargetGroupArn",
    );

    let group = "owed-group";
    asg(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "1"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "1"),
            ("LaunchTemplate.LaunchTemplateName", "owed-template"),
            ("VPCZoneIdentifier", subnet.as_str()),
            ("TargetGroupARNs.member.1", home.as_str()),
            ("HealthCheckType", "EC2"),
        ],
    )
    .await;

    // Phase 1: the group's one instance comes into service and is registered in the target
    // group it is attached to.
    wait_for(&router, group, &home, 1).await;
    // Phase 2: the control scale-out — the second instance registers there too.
    asg(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "2")],
    )
    .await;
    wait_for(&router, group, &home, 2).await;

    // Phase 3: the target group is deleted out of band and the scale-out is blocked.
    elbv2(&router, "DeleteTargetGroup", &[("TargetGroupArn", home.as_str())]).await;
    asg(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "3")],
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    let (statuses, messages) = loop {
        let body = asg(
            &router,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        let statuses = elements(&body, "StatusCode");
        let messages = elements(&body, "StatusMessage");
        if statuses.iter().any(|status| status == "Failed") || Instant::now() >= deadline {
            break (statuses, messages);
        }
        tokio::time::sleep(POLL).await;
    };
    assert!(
        statuses.iter().any(|status| status == "Failed"),
        "phase 3: the blocked scale-out must record a Failed activity, got {statuses:?} \
         {messages:?}"
    );
    assert!(
        messages.iter().any(|message| message.contains(&home)),
        "phase 3: the failed activity must name the deleted target group, got {messages:?}"
    );
    let held = in_service(&router, group).await;
    assert_eq!(
        held.len(),
        2,
        "phase 3: the blocked scale-out must launch nothing, got {held:?}"
    );

    // Phase 4: the client's own remediation — a fresh target group attached and the one that
    // is gone detached — lifts the block, and the capacity the blocked launch could not place
    // is launched and registered there, with no further capacity change.
    let fresh = element(
        &elbv2(
            &router,
            "CreateTargetGroup",
            &[
                ("Name", "owed-fresh"),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("VpcId", vpc.as_str()),
                ("TargetType", "instance"),
            ],
        )
        .await,
        "TargetGroupArn",
    );
    asg(
        &router,
        "AttachLoadBalancerTargetGroups",
        &[
            ("AutoScalingGroupName", group),
            ("TargetGroupARNs.member.1", fresh.as_str()),
        ],
    )
    .await;
    asg(
        &router,
        "DetachLoadBalancerTargetGroups",
        &[
            ("AutoScalingGroupName", group),
            ("TargetGroupARNs.member.1", home.as_str()),
        ],
    )
    .await;
    wait_for(&router, group, &fresh, 3).await;
}

/// Wait for the group to hold `count` instances in service, each registered in `arn`.
async fn wait_for(router: &MultiServiceRouter, group: &str, arn: &str, count: usize) {
    let deadline = Instant::now() + DEADLINE;
    let (held, targets) = loop {
        let held = in_service(router, group).await;
        let targets = registered(router, arn).await;
        let converged = held.len() == count
            && held.iter().all(|state| state == "InService")
            && targets.len() == count;
        if converged || Instant::now() >= deadline {
            break (held, targets);
        }
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(
        held.len(),
        count,
        "the group must hold {count} instances in service, got {held:?} with {targets:?} \
         registered"
    );
    assert!(
        held.iter().all(|state| state == "InService"),
        "every instance must be InService, got {held:?}"
    );
    assert_eq!(
        targets.len(),
        count,
        "the target group must hold the group's {count} instances, got {targets:?}"
    );
}
