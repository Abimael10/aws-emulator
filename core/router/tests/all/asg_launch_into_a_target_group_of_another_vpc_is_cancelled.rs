// SPDX-License-Identifier: CC0-1.0

//! What an Auto Scaling group does with a launch it would have to register in a target group
//! of ANOTHER virtual private cloud, through the WHOLE mounted service set — the one
//! `core/router/src/lib.rs` assembles for the binary — because the rule only holds if Auto
//! Scaling, Amazon EC2 and Elastic Load Balancing v2 agree over one engine.
//!
//! THE RULE: registering the instance is part of bringing it into service
//! (`CreateAutoScalingGroup$TargetGroupARNs` is "the Amazon Resource Names (ARN) of the
//! target groups to associate with the Auto Scaling group. Instances are registered as
//! targets in a target group"), and Elastic Load Balancing v2 takes no target its target
//! group's own VPC does not hold ("the specified target does not exist, is not in the same
//! VPC as the target group, or has an unsupported instance type", its `InvalidTarget`). So a
//! launch into a group attached to such a target group reaches no service: the group records a
//! scaling activity whose `StatusCode` is `Cancelled` and whose `StatusMessage` names that
//! target group after the fixed text `Failed to update target group `, nothing is registered
//! there, and the attachment stays exactly as the client made it — which is what lets the
//! client's own remediation (detach that target group, attach one of the group's own VPC)
//! land, after which the capacity the cancelled launch could not place is launched and
//! registered.
//!
//! The rows below span what the rule generalises over: which VPC each attachment's target
//! group is in, how many attachments the group holds, in which order the mismatched one sits
//! among them, and which member the client stated the attachment in — the group's own
//! `TargetGroupARNs` or an `elbv2` traffic source, the two spellings of one association
//! (`AttachTrafficSources`: "use the DescribeTrafficSources API to return details about the
//! state of the attachments between traffic sources and your Auto Scaling group").

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, members_of};

const REGION: &str = "us-east-1";
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const ASG_VERSION: &str = "2011-01-01";
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
/// The fixed text Amazon EC2 Auto Scaling opens the activity of a launch it could not bring
/// into a target group with, which is what a client matches the activity on.
const FAILED_UPDATE: &str = "Failed to update target group";
/// The terminal `ScalingActivityStatusCode` such a launch ends in.
const CANCELLED: &str = "Cancelled";
/// How long a phase waits for the clock-owed moves it depends on — the launch, the
/// registration, and the launch the group owes once the client has repaired its attachments —
/// and how often it looks. FAR above the spans this emulator owes them, because the deadline
/// is only here so a failure is a failure rather than a hang: every wait below looks for its
/// condition, so machine load decides when a phase finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// Which member the client stated an attachment in. Either one associates the target group
/// with the group, so the rule holds for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// The group's own `TargetGroupARNs`.
    TargetGroupArns,
    /// An `elbv2` traffic source, whose `Identifier` is the target group's ARN.
    TrafficSources,
}

/// One row of the rule: the attachments a group is created with, and where each one's target
/// group lives.
#[derive(Debug, Clone, Copy)]
struct Row {
    /// Names the case, and the resources it creates.
    slug: &'static str,
    /// Whether each attachment's target group is in the group's OWN VPC, in attachment order.
    own_vpc: &'static [bool],
    /// Which member the attachments are stated in.
    spelling: Spelling,
}

impl Row {
    /// Whether this group's launch has an attachment it could never be registered in.
    fn expects_cancellation(self) -> bool {
        self.own_vpc.iter().any(|own| !own)
    }
}

const ROWS: &[Row] = &[
    // The controls: one attachment of the group's own VPC, in each spelling. The launch comes
    // into service and is registered there.
    Row {
        slug: "arns-own-vpc",
        own_vpc: &[true],
        spelling: Spelling::TargetGroupArns,
    },
    Row {
        slug: "sources-own-vpc",
        own_vpc: &[true],
        spelling: Spelling::TrafficSources,
    },
    // One attachment of ANOTHER VPC, in each spelling.
    Row {
        slug: "arns-other-vpc",
        own_vpc: &[false],
        spelling: Spelling::TargetGroupArns,
    },
    Row {
        slug: "sources-other-vpc",
        own_vpc: &[false],
        spelling: Spelling::TrafficSources,
    },
    // And a group holding several attachments, with the mismatched one in either position: one
    // attachment that can take no registration is enough to cancel the launch, so nothing is
    // registered in the attachment that would have taken it either.
    Row {
        slug: "arns-own-then-other",
        own_vpc: &[true, false],
        spelling: Spelling::TargetGroupArns,
    },
    Row {
        slug: "arns-other-then-own",
        own_vpc: &[false, true],
        spelling: Spelling::TargetGroupArns,
    },
];

fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
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

/// The parameters of one call, as owned pairs, so a row can build them from the ids the
/// services minted for it.
fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

async fn ec2(router: &MultiServiceRouter, action: &str, pairs: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, &params(pairs))).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

async fn elbv2(router: &MultiServiceRouter, action: &str, pairs: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, &params(pairs)),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

async fn asg(router: &MultiServiceRouter, action: &str, pairs: &[(String, String)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, pairs)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The target ids Elastic Load Balancing v2 holds in the target group `arn`.
async fn registered(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    elements(&body, "Id")
}

/// The `LifecycleState` of every instance the group holds.
async fn held(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &params(&[("AutoScalingGroupNames.member.1", group)]),
    )
    .await;
    elements(&body, "LifecycleState")
}

/// Every scaling activity of the group, as `(StatusCode, StatusMessage)` — each pair read from
/// the activity that carries it, so an activity carrying no message cannot misalign the rest.
async fn activities(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let body = asg(
        router,
        "DescribeScalingActivities",
        &params(&[("AutoScalingGroupName", group)]),
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter_map(|activity| {
            let code = elements(activity, "StatusCode").into_iter().next()?;
            let message = elements(activity, "StatusMessage")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some((code, message))
        })
        .collect()
}

/// A VPC with one subnet in it, as Amazon EC2 mints them: `(vpcId, subnetId)`.
async fn network(router: &MultiServiceRouter, cidr: &str, subnet_cidr: &str) -> (String, String) {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", cidr)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", subnet_cidr),
                ("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await,
        "subnetId",
    );
    (vpc, subnet)
}

async fn target_group(router: &MultiServiceRouter, name: &str, vpc: &str) -> String {
    element(
        &elbv2(
            router,
            "CreateTargetGroup",
            &[
                ("Name", name),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("VpcId", vpc),
                ("TargetType", "instance"),
            ],
        )
        .await,
        "TargetGroupArn",
    )
}

/// The create parameters of the group `group`, attached to `arns` in the row's spelling.
fn create_parameters(
    group: &str,
    subnet: &str,
    template: &str,
    arns: &[String],
    spelling: Spelling,
) -> Vec<(String, String)> {
    let mut pairs = params(&[
        ("AutoScalingGroupName", group),
        ("MinSize", "1"),
        ("MaxSize", "1"),
        ("DesiredCapacity", "1"),
        ("LaunchTemplate.LaunchTemplateName", template),
        ("VPCZoneIdentifier", subnet),
        ("HealthCheckType", "EC2"),
    ]);
    for (position, arn) in arns.iter().enumerate() {
        let index = position + 1;
        match spelling {
            Spelling::TargetGroupArns => {
                pairs.push((format!("TargetGroupARNs.member.{index}"), arn.clone()));
            }
            Spelling::TrafficSources => {
                pairs.push((
                    format!("TrafficSources.member.{index}.Identifier"),
                    arn.clone(),
                ));
                pairs.push((
                    format!("TrafficSources.member.{index}.Type"),
                    "elbv2".to_string(),
                ));
            }
        }
    }
    pairs
}

#[tokio::test]
async fn a_launch_that_cannot_be_registered_in_an_attachment_is_cancelled_naming_it() {
    let router = router();
    // The group's own network, and the one its instances are NOT in.
    let (own_vpc, subnet) = network(&router, "10.60.0.0/16", "10.60.1.0/24").await;
    let (other_vpc, _) = network(&router, "10.61.0.0/16", "10.61.1.0/24").await;
    let template = "vpc-mismatch-template";
    ec2(
        &router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;

    for row in ROWS {
        let case = row.slug;
        // One target group per attachment of this row, in the VPC the row places it in, so no
        // two rows read one another's registrations.
        let mut arns: Vec<String> = Vec::new();
        for (position, own) in row.own_vpc.iter().enumerate() {
            let vpc = if *own { &own_vpc } else { &other_vpc };
            arns.push(target_group(&router, &format!("{case}-{position}"), vpc).await);
        }
        let group = format!("{case}-group");
        asg(
            &router,
            "CreateAutoScalingGroup",
            &create_parameters(&group, &subnet, template, &arns, row.spelling),
        )
        .await;

        if row.expects_cancellation() {
            // The attachment that can take no registration, in attachment order: the one the
            // activity has to name.
            let refused = row
                .own_vpc
                .iter()
                .zip(&arns)
                .find_map(|(own, arn)| (!own).then_some(arn))
                .expect("the row holds a mismatched attachment");
            let expected = format!("{FAILED_UPDATE} {refused}");
            let deadline = Instant::now() + DEADLINE;
            let recorded = loop {
                let recorded = activities(&router, &group).await;
                let found = recorded
                    .iter()
                    .any(|(code, message)| code == CANCELLED && message.contains(&expected));
                if found || Instant::now() >= deadline {
                    break recorded;
                }
                tokio::time::sleep(POLL).await;
            };
            assert!(
                recorded
                    .iter()
                    .any(|(code, message)| code == CANCELLED && message.contains(&expected)),
                "case {case:?}: expected a {CANCELLED} activity whose message carries \
                 {expected:?}, got {recorded:?}"
            );
            let instances = held(&router, &group).await;
            assert!(
                instances.is_empty(),
                "case {case:?}: expected the cancelled launch to bring no instance into the \
                 group, got {instances:?}"
            );
            for arn in &arns {
                let targets = registered(&router, arn).await;
                assert!(
                    targets.is_empty(),
                    "case {case:?}: expected no target in {arn}, got {targets:?}"
                );
            }
        } else {
            let arn = &arns[0];
            let deadline = Instant::now() + DEADLINE;
            let (instances, targets) = loop {
                let instances = held(&router, &group).await;
                let targets = registered(&router, arn).await;
                let converged = instances.iter().any(|state| state == "InService")
                    && targets.len() == 1;
                if converged || Instant::now() >= deadline {
                    break (instances, targets);
                }
                tokio::time::sleep(POLL).await;
            };
            assert!(
                instances.iter().any(|state| state == "InService"),
                "case {case:?}: expected the launch to bring an instance into service, got \
                 {instances:?}"
            );
            assert_eq!(
                targets.len(),
                1,
                "case {case:?}: expected the group's instance registered in {arn}, got \
                 {targets:?}"
            );
            let recorded = activities(&router, &group).await;
            assert!(
                !recorded.iter().any(|(code, _)| code == CANCELLED),
                "case {case:?}: expected no cancelled launch, got {recorded:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_capacity_a_cancelled_launch_could_not_place_lands_once_the_client_repairs_it() {
    let router = router();
    let (own_vpc, subnet) = network(&router, "10.62.0.0/16", "10.62.1.0/24").await;
    let (other_vpc, _) = network(&router, "10.63.0.0/16", "10.63.1.0/24").await;
    let template = "vpc-repair-template";
    ec2(
        &router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    let elsewhere = target_group(&router, "repair-elsewhere", &other_vpc).await;
    let home = target_group(&router, "repair-home", &own_vpc).await;
    let group = "repair-group";
    asg(
        &router,
        "CreateAutoScalingGroup",
        &create_parameters(
            group,
            &subnet,
            template,
            std::slice::from_ref(&elsewhere),
            Spelling::TargetGroupArns,
        ),
    )
    .await;

    // The launch is cancelled, naming the attachment it could not be registered in.
    let expected = format!("{FAILED_UPDATE} {elsewhere}");
    let deadline = Instant::now() + DEADLINE;
    let recorded = loop {
        let recorded = activities(&router, group).await;
        let found = recorded
            .iter()
            .any(|(code, message)| code == CANCELLED && message.contains(&expected));
        if found || Instant::now() >= deadline {
            break recorded;
        }
        tokio::time::sleep(POLL).await;
    };
    assert!(
        recorded
            .iter()
            .any(|(code, message)| code == CANCELLED && message.contains(&expected)),
        "the blocked launch must record a {CANCELLED} activity carrying {expected:?}, got \
         {recorded:?}"
    );
    // The attachment is the CLIENT's: the group still reports it, so the client's own detach
    // of it is accepted.
    let attached = asg(
        &router,
        "DescribeLoadBalancerTargetGroups",
        &params(&[("AutoScalingGroupName", group)]),
    )
    .await;
    assert!(
        elements(&attached, "LoadBalancerTargetGroupARN").contains(&elsewhere),
        "the group must go on reporting the attachment the client made, got {attached}"
    );

    // The client's remediation: the mismatched target group off, one of the group's own VPC on.
    asg(
        &router,
        "AttachLoadBalancerTargetGroups",
        &params(&[
            ("AutoScalingGroupName", group),
            ("TargetGroupARNs.member.1", home.as_str()),
        ]),
    )
    .await;
    asg(
        &router,
        "DetachLoadBalancerTargetGroups",
        &params(&[
            ("AutoScalingGroupName", group),
            ("TargetGroupARNs.member.1", elsewhere.as_str()),
        ]),
    )
    .await;

    // And the capacity the cancelled launch could not place is launched and registered there,
    // with no further capacity change from anybody.
    let deadline = Instant::now() + DEADLINE;
    let (instances, targets) = loop {
        let instances = held(&router, group).await;
        let targets = registered(&router, &home).await;
        if (instances.iter().any(|state| state == "InService") && targets.len() == 1)
            || Instant::now() >= deadline
        {
            break (instances, targets);
        }
        tokio::time::sleep(POLL).await;
    };
    assert!(
        instances.iter().any(|state| state == "InService"),
        "the repaired group must bring its owed capacity into service, got {instances:?}"
    );
    assert_eq!(
        targets.len(),
        1,
        "the repaired attachment must hold the group's instance, got {targets:?}"
    );
}
