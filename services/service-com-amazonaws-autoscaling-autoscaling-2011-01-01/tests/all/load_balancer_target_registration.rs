// SPDX-License-Identifier: CC0-1.0

//! The Elastic Load Balancing v2 TARGETS an Auto Scaling group's instances occupy in the
//! target groups it is attached to.
//!
//! The rule under test: an instance of a group that is attached to a target group IS a
//! target of that target group — `CreateAutoScalingGroup$TargetGroupARNs` is "the Amazon
//! Resource Names (ARN) of the target groups to associate with the Auto Scaling group.
//! Instances are registered as targets in a target group, and traffic is routed to the
//! target group", and `AttachLoadBalancerTargetGroups` adds the same association later
//! ("this operation is additive and does not detach existing target groups"), while a
//! detach "enters the Removing state while deregistering the instances in the group". So
//! every instance the group holds is registered with every target group it is attached to,
//! at that target group's own port and in the Availability Zone the instance runs in; an
//! instance that leaves the group, and an attachment that is removed, deregister; and
//! `DescribeAutoScalingGroups` reports the group's collections — its attachments included —
//! as lists, empty when the group holds nothing in them, rather than leaving the member out.
//!
//! The registrations are asserted where a client sees them: Elastic Load Balancing v2's own
//! `DescribeTargetHealth`, over its own wire, with all three services (Auto Scaling, Amazon
//! EC2 and Elastic Load Balancing v2) mounted on ONE engine — so a target asserted here is
//! one the owning service returns from its own read, and the instance ids are the account's
//! own Amazon EC2 instances.
//!
//! Registering the instance is PART of bringing it into service, so the same rule decides
//! what a launch does when an attachment can take no registration at all: a target group
//! that has been DELETED in Elastic Load Balancing v2 since it was attached stops the
//! launch that would have to register there — the group launches nothing and records a
//! failed scaling activity whose `StatusMessage` names the target groups that are gone
//! ("if an attempt to launch instances failed, the StatusCode value is Failed or Cancelled
//! and the StatusMessage element in the response indicates the cause of the failure",
//! `DescribeScalingActivities`). The attachment itself is the CLIENT's: the group keeps
//! reporting it, and keeps refusing the launch, until the client detaches it — so the
//! remediation a client performs (detach the target group that is gone, attach one that
//! exists, ask for the capacity again) is what lifts the block.
//!
//! The tables below span what the rule generalises over: how many target groups a group is
//! attached to, how many instances it holds, how many zones they are spread across, the
//! target group's own port, whether the attachment came from the create or from a later
//! attach, whether the attachment is named as `TargetGroupARNs` or as an `elbv2` traffic
//! source, the target group's `TargetType`, the documented per-call bound on an attach, and
//! how many of a group's attachments have been deleted out of band.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

// The harness pieces this suite drives the three services through — the one engine they
// share, their wires, and the resources a group is built from — live in `support`, which is
// their one copy for every suite here.
use crate::support::{
    borrowed, create_launch_template, create_target_group, ec2_query, elbv2_query, emulator, p,
    query, seed_subnets, tag_value, tag_values, vpc_of, xml_unescape, SeededSubnet,
};

/// The most target groups ONE attach may name, from the model's own documentation for
/// `AttachLoadBalancerTargetGroupsType$TargetGroupARNs`: "You can specify up to 10 target
/// groups."
const MAX_TARGET_GROUPS_PER_CALL: usize = 10;

/// The most load-balancer attachments ONE group may HOLD, however many calls put them
/// there — the ceiling this process holds a group to, read from the crate rather than
/// written out here so the rows below cannot drift from the handlers.
const MAX_TARGET_GROUPS_PER_GROUP: usize = autoscaling::state::MAX_TARGET_GROUPS_PER_GROUP;

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` of a traffic source whose `Identifier` is an Elastic
/// Load Balancing v2 target group ARN — "`elbv2` if `Identifier` is the ARN of an
/// Application Load Balancer, Gateway Load Balancer, or Network Load Balancer target group"
/// (`smithy_query(query="doc",shape="TrafficSourceIdentifier",member="Type")`).
const TRAFFIC_SOURCE_TYPE_ELBV2: &str = "elbv2";

/// The CLASSIC Load Balancers a group is attached to, in their own file beside this one: the
/// same rule — an instance of an attached group is registered with what it is attached to —
/// through the other load balancer kind's own operations, which is this suite's sibling
/// concern. `tests/all.rs` declares one module per suite FILE, and this is that file's
/// module; the `#[path]` points it into this suite's own directory, as that file's own
/// declaration points into `tests/all/`.
#[path = "load_balancer_target_registration/classic_load_balancers.rs"]
mod classic_load_balancers;

/// How a group names the target groups it is created attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedAs {
    /// `TargetGroupARNs`, the member `AttachLoadBalancerTargetGroups` also writes.
    TargetGroupArns,
    /// `TrafficSources`, each entry an `elbv2` traffic source whose `Identifier` is the
    /// target group's ARN.
    Elbv2TrafficSources,
    /// Attached by a later `AttachLoadBalancerTargetGroups` call rather than by the create.
    LaterAttach,
}

/// Create one group over the awsQuery wire: placed by `VPCZoneIdentifier` over `subnets`,
/// holding `desired` instances, attached to `target_groups` the way `attached_as` says.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    desired: usize,
    target_groups: &[String],
    attached_as: AttachedAs,
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", ids.join(",")),
    ];
    for (index, arn) in target_groups.iter().enumerate() {
        match attached_as {
            AttachedAs::TargetGroupArns => params.push(p(
                format!("TargetGroupARNs.member.{}", index + 1).as_str(),
                arn.clone(),
            )),
            AttachedAs::Elbv2TrafficSources => {
                params.push(p(
                    format!("TrafficSources.member.{}.Identifier", index + 1).as_str(),
                    arn.clone(),
                ));
                params.push(p(
                    format!("TrafficSources.member.{}.Type", index + 1).as_str(),
                    "elbv2",
                ));
            }
            AttachedAs::LaterAttach => {}
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
    if attached_as == AttachedAs::LaterAttach && !target_groups.is_empty() {
        let (status, code, message) = attach(router, name, target_groups).await;
        assert_eq!(
            status, 200,
            "attaching {} target group(s) to {name:?} must succeed, got {status} with \
             {code:?} {message:?}",
            target_groups.len()
        );
    }
}

/// Post one attach and answer its status, wire error code and message.
async fn attach(
    router: &MultiServiceRouter,
    name: &str,
    arns: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    (status, tag_value(&body, "Code"), tag_value(&body, "Message"))
}

/// Post one detach and answer its status.
async fn detach(router: &MultiServiceRouter, name: &str, arns: &[String]) -> u16 {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, _) = query(router, "DetachLoadBalancerTargetGroups", &borrowed(&params)).await;
    status
}

/// One `DescribeAutoScalingGroups` body for the named group.
async fn described(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    body
}

/// The XML inside the first element named `tag`, or `None` when the element is absent.
///
/// Attribute-tolerant, because the envelope stamps the document's namespace onto the first
/// element of a list.
fn element_inner<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = xml.find(&open)?;
    let after = &xml[start + open.len()..];
    if !after.starts_with('>') && !after.starts_with(char::is_whitespace) {
        return None;
    }
    let inner_start = after.find('>')? + 1;
    let inner = &after[inner_start..];
    let end = inner.find(&close)?;
    Some(&inner[..end])
}

/// Whether the body names the element `tag` at all — present and empty included, which is
/// how an empty list crosses the wire (`<Tag></Tag>`).
fn names_element(xml: &str, tag: &str) -> bool {
    [format!("<{tag}>"), format!("<{tag} "), format!("<{tag}/>")]
        .iter()
        .any(|needle| xml.contains(needle.as_str()))
}

/// Each `<member>…</member>` of `xml`, in document order. The lists these are read out of
/// hold no nested member list.
fn members(xml: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<member") {
        let after = &rest[start..];
        let Some(inner_start) = after.find('>').map(|at| at + 1) else {
            break;
        };
        let inner = &after[inner_start..];
        let Some(end) = inner.find("</member>") else {
            break;
        };
        found.push(&inner[..end]);
        rest = &inner[end + "</member>".len()..];
    }
    found
}

/// The target groups the group reports under its own `TargetGroupARNs`, in the order it
/// holds them.
async fn attached_target_groups(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = described(router, name).await;
    assert!(
        names_element(&body, "TargetGroupARNs"),
        "a described group must report its TargetGroupARNs, got {body}"
    );
    let inner = element_inner(&body, "TargetGroupARNs").unwrap_or_default();
    tag_values(inner, "member")
}

/// Every instance the group reports, as `(instance id, Availability Zone)` in the order
/// reported.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let body = described(router, name).await;
    let inner = element_inner(&body, "Instances").unwrap_or_default();
    members(inner)
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "AvailabilityZone"),
            )
        })
        .collect()
}

/// One target Elastic Load Balancing v2 reports as registered with a target group: its id,
/// the port it receives traffic on and the zone it is in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReportedTarget {
    id: String,
    port: String,
    availability_zone: String,
}

/// The targets Elastic Load Balancing v2 reports as registered with `arn`, sorted, read
/// through its own `DescribeTargetHealth`.
async fn registered_targets(router: &MultiServiceRouter, arn: &str) -> Vec<ReportedTarget> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    let inner = element_inner(&body, "TargetHealthDescriptions").unwrap_or_default();
    let mut targets: Vec<ReportedTarget> = members(inner)
        .into_iter()
        .map(|member| ReportedTarget {
            id: tag_value(member, "Id"),
            port: tag_value(member, "Port"),
            availability_zone: tag_value(member, "AvailabilityZone"),
        })
        .collect();
    targets.sort();
    targets
}

/// What the group's instances must be reported as, in the target group on `port`.
fn expected_targets(instances: &[(String, String)], port: i32) -> Vec<ReportedTarget> {
    let mut expected: Vec<ReportedTarget> = instances
        .iter()
        .map(|(id, zone)| ReportedTarget {
            id: id.clone(),
            port: port.to_string(),
            availability_zone: zone.clone(),
        })
        .collect();
    expected.sort();
    expected
}

/// One row of the registration table: how many subnets the group is placed over, how many
/// instances it holds, how many target groups it is attached to, those target groups' port,
/// and how the attachment was made.
struct RegistrationRow {
    case: &'static str,
    subnets: usize,
    desired: usize,
    target_groups: usize,
    port: i32,
    attached_as: AttachedAs,
}

fn registration_rows() -> Vec<RegistrationRow> {
    vec![
        RegistrationRow {
            case: "one target group, one instance, one subnet",
            subnets: 1,
            desired: 1,
            target_groups: 1,
            port: 80,
            attached_as: AttachedAs::TargetGroupArns,
        },
        RegistrationRow {
            case: "two target groups, two instances across two subnets",
            subnets: 2,
            desired: 2,
            target_groups: 2,
            port: 8080,
            attached_as: AttachedAs::TargetGroupArns,
        },
        RegistrationRow {
            case: "three target groups, three instances, attached by a later attach",
            subnets: 2,
            desired: 3,
            target_groups: 3,
            port: 443,
            attached_as: AttachedAs::LaterAttach,
        },
        RegistrationRow {
            case: "one target group named as an elbv2 traffic source",
            subnets: 1,
            desired: 2,
            target_groups: 1,
            port: 9000,
            attached_as: AttachedAs::Elbv2TrafficSources,
        },
        RegistrationRow {
            case: "a target group of a group that holds no instance",
            subnets: 1,
            desired: 0,
            target_groups: 1,
            port: 80,
            attached_as: AttachedAs::TargetGroupArns,
        },
        RegistrationRow {
            case: "the documented per-call maximum of target groups at once",
            subnets: 2,
            desired: 1,
            target_groups: MAX_TARGET_GROUPS_PER_CALL,
            port: 1,
            attached_as: AttachedAs::LaterAttach,
        },
    ]
}

/// Every row: each of the group's instances is a target of EVERY target group it is attached
/// to, at that target group's own port and in the zone the instance runs in.
#[tokio::test]
async fn the_instances_of_an_attached_group_are_targets_of_its_target_groups() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-registration").await;

    for (row, case) in registration_rows().into_iter().enumerate() {
        let name = format!("asg-registration-{row}");
        let mut arns = Vec::new();
        for index in 0..case.target_groups {
            arns.push(
                create_target_group(
                    &router,
                    &format!("tg-{row}-{index}"),
                    case.port,
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }
        create_group(
            &router,
            &name,
            "lt-registration",
            &subnets[..case.subnets],
            case.desired,
            &arns,
            case.attached_as,
        )
        .await;

        let instances = group_instances(&router, &name).await;
        assert_eq!(
            instances.len(),
            case.desired,
            "case {:?}: expected the group to hold {} instance(s), got {instances:?}",
            case.case,
            case.desired
        );
        let expected = expected_targets(&instances, case.port);
        for arn in &arns {
            let actual = registered_targets(&router, arn).await;
            assert_eq!(
                actual, expected,
                "case {:?}: expected {arn:?} to hold the targets {expected:?}, got {actual:?}",
                case.case
            );
        }
    }
}

/// A target group a group is attached to and then detached from deregisters the group's
/// instances, and the attachment itself is gone from what the group reports — while a target
/// group it is still attached to keeps every one of them.
#[tokio::test]
async fn a_detached_target_group_deregisters_the_groups_instances() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-detach").await;

    let kept = create_target_group(&router, "tg-kept", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    let dropped =
        create_target_group(&router, "tg-dropped", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    create_group(
        &router,
        "asg-detach",
        "lt-detach",
        &subnets,
        2,
        &[kept.clone(), dropped.clone()],
        AttachedAs::TargetGroupArns,
    )
    .await;

    let instances = group_instances(&router, "asg-detach").await;
    let expected = expected_targets(&instances, 80);
    for arn in [&kept, &dropped] {
        let actual = registered_targets(&router, arn).await;
        assert_eq!(
            actual, expected,
            "before the detach, {arn:?} must hold {expected:?}, got {actual:?}"
        );
    }

    let status = detach(&router, "asg-detach", std::slice::from_ref(&dropped)).await;
    assert_eq!(status, 200, "the detach must succeed, got {status}");

    let actual = registered_targets(&router, &dropped).await;
    assert!(
        actual.is_empty(),
        "the detached target group must hold no target of the group, got {actual:?}"
    );
    let actual = registered_targets(&router, &kept).await;
    assert_eq!(
        actual, expected,
        "the target group still attached must keep {expected:?}, got {actual:?}"
    );
    let attached = attached_target_groups(&router, "asg-detach").await;
    assert_eq!(
        attached,
        vec![kept.clone()],
        "the group must report only the attachment it still holds, got {attached:?}"
    );
}

/// An instance that leaves the group is no longer one of its targets: a scale-in takes the
/// instances it terminated out of every target group the group is attached to, and leaves
/// the ones still in service registered.
#[tokio::test]
async fn a_scale_in_deregisters_the_instances_that_left_the_group() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-scale-in").await;

    let arn = create_target_group(&router, "tg-scale-in", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    create_group(
        &router,
        "asg-scale-in",
        "lt-scale-in",
        &subnets,
        3,
        std::slice::from_ref(&arn),
        AttachedAs::TargetGroupArns,
    )
    .await;

    let launched = group_instances(&router, "asg-scale-in").await;
    let actual = registered_targets(&router, &arn).await;
    assert_eq!(
        actual,
        expected_targets(&launched, 80),
        "every launched instance must be a target, got {actual:?}"
    );

    for remaining in [2usize, 1, 0] {
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", "asg-scale-in"),
                ("DesiredCapacity", &remaining.to_string()),
                ("HonorCooldown", "false"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "scaling to {remaining} must succeed, got {status} with {body:?}"
        );

        let actual = registered_targets(&router, &arn).await;
        assert_eq!(
            actual.len(),
            remaining,
            "case {remaining} instance(s) left: expected that many targets, got {actual:?}"
        );
        for target in &actual {
            assert!(
                launched.iter().any(|(id, _)| *id == target.id),
                "case {remaining} instance(s) left: {target:?} is not one of the instances \
                 this group launched ({launched:?})"
            );
        }
    }
}

/// A target group whose targets are NOT instances receives none of the group's instances:
/// an Amazon EC2 instance is a target of an `instance` target group alone, so attaching an
/// `ip` target group is served and registers nothing in it.
#[tokio::test]
async fn a_target_group_whose_targets_are_not_instances_receives_none() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-target-type").await;

    // The `TargetType` values this table spans, from the model's own
    // `TargetTypeEnum` (`smithy_query(query="shape",shape="TargetTypeEnum")` of the
    // Elastic Load Balancing v2 model): the instance type takes the group's instances, and
    // the ones addressed otherwise take none.
    let rows = [
        (TARGET_TYPE_INSTANCE, true),
        ("ip", false),
        ("lambda", false),
    ];
    for (row, (target_type, takes_instances)) in rows.into_iter().enumerate() {
        let name = format!("asg-target-type-{row}");
        let arn = create_target_group(
            &router,
            &format!("tg-type-{row}"),
            80,
            target_type,
            &vpc_id,
        )
        .await;
        create_group(
            &router,
            &name,
            "lt-target-type",
            &subnets,
            1,
            std::slice::from_ref(&arn),
            AttachedAs::TargetGroupArns,
        )
        .await;

        let instances = group_instances(&router, &name).await;
        let expected = if takes_instances {
            expected_targets(&instances, 80)
        } else {
            Vec::new()
        };
        let actual = registered_targets(&router, &arn).await;
        assert_eq!(
            actual, expected,
            "case {target_type:?}: expected the target group to hold {expected:?}, got \
             {actual:?}"
        );
        // Whatever the target type, the attachment itself is the group's own and is
        // reported.
        let attached = attached_target_groups(&router, &name).await;
        assert_eq!(
            attached,
            vec![arn],
            "case {target_type:?}: expected the attachment to be reported, got {attached:?}"
        );
    }
}

/// An attach is ADDITIVE and holds each target group once: the ARNs it names join the ones
/// the group already holds, an ARN already attached changes nothing, and the group's
/// instances are targets of every one of them.
#[tokio::test]
async fn an_attach_is_additive_and_holds_each_target_group_once() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-additive").await;

    let mut arns = Vec::new();
    for index in 0..3 {
        arns.push(
            create_target_group(
                &router,
                &format!("tg-additive-{index}"),
                80,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }
    create_group(
        &router,
        "asg-additive",
        "lt-additive",
        &subnets,
        2,
        &arns[..1],
        AttachedAs::TargetGroupArns,
    )
    .await;
    let instances = group_instances(&router, "asg-additive").await;
    let expected = expected_targets(&instances, 80);

    // Each row: what the attach names, and what the group must then hold.
    let rows: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        (
            "one more target group",
            vec![arns[1].clone()],
            arns[..2].to_vec(),
        ),
        (
            "the two it already holds, again",
            arns[..2].to_vec(),
            arns[..2].to_vec(),
        ),
        (
            "one it holds beside one it does not",
            vec![arns[1].clone(), arns[2].clone()],
            arns.clone(),
        ),
        (
            "the same new target group twice in one call",
            vec![arns[2].clone(), arns[2].clone()],
            arns.clone(),
        ),
    ];
    for (case, named, held) in rows {
        let (status, code, message) = attach(&router, "asg-additive", &named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );
        let attached = attached_target_groups(&router, "asg-additive").await;
        assert_eq!(
            attached, held,
            "case {case:?}: expected the group to hold {held:?}, got {attached:?}"
        );
        for arn in &attached {
            let actual = registered_targets(&router, arn).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {arn:?} to hold {expected:?}, got {actual:?}"
            );
        }
    }
}

/// The documented per-call bound: an attach naming the maximum is served, and one naming a
/// single target group more is refused `ValidationError` without attaching anything.
#[tokio::test]
async fn an_attach_past_the_documented_per_call_maximum_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-bound").await;

    let mut arns = Vec::new();
    for index in 0..=MAX_TARGET_GROUPS_PER_CALL {
        arns.push(
            create_target_group(
                &router,
                &format!("tg-bound-{index}"),
                80,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }
    create_group(
        &router,
        "asg-bound",
        "lt-bound",
        &subnets,
        1,
        &[],
        AttachedAs::TargetGroupArns,
    )
    .await;

    // One past the bound: refused, and nothing is attached.
    let (status, code, message) = attach(&router, "asg-bound", &arns).await;
    assert_eq!(
        status, 400,
        "an attach of {} target groups must be refused, got {status} with {message:?}",
        arns.len()
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code past the bound, got {code:?} {message:?}"
    );
    let expected = format!(
        "Trying to update too many Target Groups at once. The limit is \
         {MAX_TARGET_GROUPS_PER_CALL}"
    );
    assert_eq!(
        message, expected,
        "expected the message {expected:?} past the bound, got {message:?}"
    );
    let attached = attached_target_groups(&router, "asg-bound").await;
    assert!(
        attached.is_empty(),
        "the refusal must attach nothing, got {attached:?}"
    );

    // AT the bound: served, and every named target group is attached.
    let at_bound: Vec<String> = arns[..MAX_TARGET_GROUPS_PER_CALL].to_vec();
    let (status, code, message) = attach(&router, "asg-bound", &at_bound).await;
    assert_eq!(
        status, 200,
        "an attach of {MAX_TARGET_GROUPS_PER_CALL} target groups must succeed, got {status} \
         with {code:?} {message:?}"
    );
    let attached = attached_target_groups(&router, "asg-bound").await;
    assert_eq!(
        attached, at_bound,
        "expected every named target group to be attached, got {attached:?}"
    );
}

/// An attach naming a group the account does not hold is refused like every other
/// group-scoped mutation of this service, and names the group that is missing — and the
/// group that does exist keeps its attachments.
#[tokio::test]
async fn an_attach_of_an_absent_group_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-absent").await;

    let arn = create_target_group(&router, "tg-absent", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    create_group(
        &router,
        "asg-present",
        "lt-absent",
        &subnets,
        1,
        std::slice::from_ref(&arn),
        AttachedAs::TargetGroupArns,
    )
    .await;

    for name in ["asg-absent", "ASG-Present", "asg-present-but-longer"] {
        let (status, code, message) = attach(&router, name, std::slice::from_ref(&arn)).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {name:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        // The absent-group refusal this operation answers with: the fixed sentence whose
        // detail field is left unfilled, so it travels as the literal token `null` and no
        // part of the request appears in it.
        let expected = "AutoScalingGroup name not found - null";
        assert_eq!(
            message, expected,
            "case {name:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    let attached = attached_target_groups(&router, "asg-present").await;
    assert_eq!(
        attached,
        vec![arn],
        "the group that exists must keep its attachment, got {attached:?}"
    );
}

/// A described group reports its collections as LISTS: empty for a group that holds nothing
/// in them, and carrying exactly what it holds otherwise. A client reads
/// `AutoScalingGroups[].TargetGroupARNs` off the group it describes, so the member is there
/// whether or not the group is attached to anything.
#[tokio::test]
async fn a_described_group_reports_its_collections_as_lists() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-collections").await;

    let arn = create_target_group(&router, "tg-collections", 80, TARGET_TYPE_INSTANCE, &vpc_id)
        .await;

    // The collection members of `AutoScalingGroup` a described group carries
    // (`smithy_query(query="shape",shape="AutoScalingGroup")`): the attachments, the
    // suspended processes, the enabled metrics, the traffic sources, the termination
    // policies, the zones and the tags.
    let collections = [
        "TargetGroupARNs",
        "LoadBalancerNames",
        "SuspendedProcesses",
        "EnabledMetrics",
        "TrafficSources",
        "TerminationPolicies",
        "AvailabilityZones",
        "Tags",
    ];

    for (case, attachments) in [
        ("a group attached to nothing", Vec::new()),
        ("a group attached to one target group", vec![arn.clone()]),
    ] {
        let name = format!("asg-collections-{}", attachments.len());
        create_group(
            &router,
            &name,
            "lt-collections",
            &subnets,
            0,
            &attachments,
            AttachedAs::TargetGroupArns,
        )
        .await;
        let body = described(&router, &name).await;
        for collection in collections {
            assert!(
                names_element(&body, collection),
                "case {case:?}: expected the described group to report {collection}, got {body}"
            );
        }
        let attached = attached_target_groups(&router, &name).await;
        assert_eq!(
            attached, attachments,
            "case {case:?}: expected TargetGroupARNs to carry {attachments:?}, got {attached:?}"
        );
    }
}

/// The ARNs of the target groups the ceiling rows name, created in Elastic Load Balancing
/// v2 itself: `MAX_TARGET_GROUPS_PER_GROUP` of them plus the one past the ceiling, since an
/// attach RESOLVES every ARN it names against that service's own target groups, so the
/// ceiling is walked over target groups the account really holds.
async fn bulk_target_groups(
    router: &MultiServiceRouter,
    vpc_id: &str,
    count: usize,
) -> Vec<String> {
    let mut arns = Vec::with_capacity(count);
    for index in 0..count {
        arns.push(
            create_target_group(
                router,
                &format!("tg-bulk-{index}"),
                80,
                TARGET_TYPE_INSTANCE,
                vpc_id,
            )
            .await,
        );
    }
    arns
}

/// The ceiling on what ONE group may HOLD, whatever call put the attachments there: a run of
/// attaches is served up to it and refused past it, a create naming more than it is refused,
/// and the group is left holding exactly what was accepted.
///
/// The rows walk the ceiling itself and the first count past it from both directions — a
/// create that names them outright, and a sequence of attaches that accumulates to them —
/// because the per-call bound says nothing about a run of calls.
#[tokio::test]
async fn one_group_holds_at_most_the_ceiling_of_attachments() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-ceiling").await;
    // Every target group the rows below name, created in Elastic Load Balancing v2 itself:
    // the ceiling plus the first one past it.
    let bulk = bulk_target_groups(&router, &vpc_id, MAX_TARGET_GROUPS_PER_GROUP + 1).await;

    // The message both handlers refuse past the ceiling with, read from the crate so the
    // rows cannot drift from it.
    let refusal = autoscaling::state::too_many_attached_target_groups_message();

    // A run of attaches: served while the group would hold at most the ceiling, refused as
    // soon as it would hold one more.
    create_group(
        &router,
        "asg-ceiling",
        "lt-ceiling",
        &subnets,
        1,
        &[],
        AttachedAs::TargetGroupArns,
    )
    .await;
    let mut held = 0usize;
    while held < MAX_TARGET_GROUPS_PER_GROUP {
        let batch: Vec<String> = (held..(held + MAX_TARGET_GROUPS_PER_CALL)
            .min(MAX_TARGET_GROUPS_PER_GROUP))
            .map(|index| bulk[index].clone())
            .collect();
        let (status, code, message) = attach(&router, "asg-ceiling", &batch).await;
        assert_eq!(
            status, 200,
            "case {held} already held: attaching {} more must be served, got {status} with \
             {code:?} {message:?}",
            batch.len()
        );
        held += batch.len();
    }
    let attached = attached_target_groups(&router, "asg-ceiling").await;
    assert_eq!(
        attached.len(),
        MAX_TARGET_GROUPS_PER_GROUP,
        "expected the group to hold the ceiling of attachments, got {} of them",
        attached.len()
    );

    let (status, code, message) = attach(
        &router,
        "asg-ceiling",
        std::slice::from_ref(&bulk[MAX_TARGET_GROUPS_PER_GROUP]),
    )
    .await;
    assert_eq!(
        status, 400,
        "one attachment past the ceiling must be refused, got {status} with {code:?} \
         {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code past the ceiling, got {code:?} {message:?}"
    );
    assert_eq!(
        message, refusal,
        "expected the message {refusal:?} past the ceiling, got {message:?}"
    );
    let attached = attached_target_groups(&router, "asg-ceiling").await;
    assert_eq!(
        attached.len(),
        MAX_TARGET_GROUPS_PER_GROUP,
        "the refusal must attach nothing, got {} attachments",
        attached.len()
    );

    // A create naming them outright: the ceiling is served and one more is refused with the
    // modelled limit error, under either collection the attachment is named in.
    let rows: Vec<(&str, usize, AttachedAs, u16, &str)> = vec![
        (
            "a create naming the ceiling as TargetGroupARNs",
            MAX_TARGET_GROUPS_PER_GROUP,
            AttachedAs::TargetGroupArns,
            200,
            "",
        ),
        (
            "a create naming one more as TargetGroupARNs",
            MAX_TARGET_GROUPS_PER_GROUP + 1,
            AttachedAs::TargetGroupArns,
            400,
            "LimitExceeded",
        ),
        (
            "a create naming one more as elbv2 traffic sources",
            MAX_TARGET_GROUPS_PER_GROUP + 1,
            AttachedAs::Elbv2TrafficSources,
            400,
            "LimitExceeded",
        ),
    ];
    for (row, (case, count, attached_as, expected_status, expected_code)) in
        rows.into_iter().enumerate()
    {
        let name = format!("asg-ceiling-create-{row}");
        let arns: Vec<String> = bulk[..count].to_vec();
        let mut params = vec![
            p("AutoScalingGroupName", name.clone()),
            p("LaunchTemplate.LaunchTemplateName", "lt-ceiling"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
        ];
        for (index, arn) in arns.iter().enumerate() {
            match attached_as {
                AttachedAs::Elbv2TrafficSources => {
                    params.push(p(
                        format!("TrafficSources.member.{}.Identifier", index + 1).as_str(),
                        arn.clone(),
                    ));
                    params.push(p(
                        format!("TrafficSources.member.{}.Type", index + 1).as_str(),
                        "elbv2",
                    ));
                }
                _ => params.push(p(
                    format!("TargetGroupARNs.member.{}", index + 1).as_str(),
                    arn.clone(),
                )),
            }
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {body:?}"
        );
        if expected_status == 200 {
            let attached = attached_target_groups(&router, &name).await;
            let expected = if attached_as == AttachedAs::TargetGroupArns {
                count
            } else {
                0
            };
            assert_eq!(
                attached.len(),
                expected,
                "case {case:?}: expected {expected} attachment(s) reported, got {}",
                attached.len()
            );
            continue;
        }
        let code = tag_value(&body, "Code");
        let message = tag_value(&body, "Message");
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected the {expected_code} code, got {code:?} {message:?}"
        );
        assert_eq!(
            message, refusal,
            "case {case:?}: expected the message {refusal:?}, got {message:?}"
        );
    }
}

/// How often a poll re-reads the group.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span this service owes a
/// transition, with ample margin for a loaded test machine. Read from the service's own span
/// rather than written out, so the two cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(10)
}

/// The `LifecycleState` of an instance that is in service, from the model's own
/// `LifecycleState` enum (`smithy_query(query="shape",shape="LifecycleState")`).
const LIFECYCLE_IN_SERVICE: &str = "InService";

/// The `ScalingActivityStatusCode` of an activity that did not place what it set out to,
/// from the model's own `ScalingActivityStatusCode` enum
/// (`smithy_query(query="shape",shape="ScalingActivityStatusCode")`).
const ACTIVITY_FAILED: &str = "Failed";

/// Post one `SetDesiredCapacity` and answer its status, wire error code and message.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    name: &str,
    desired: usize,
) -> (u16, String, String) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", name),
            ("DesiredCapacity", &desired.to_string()),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Delete one target group through Elastic Load Balancing v2's own wire — the service that
/// owns it is what takes it away, so the attachment the Auto Scaling group still holds is
/// left naming a target group that no longer exists.
async fn delete_target_group(router: &MultiServiceRouter, arn: &str) {
    let (status, body) =
        elbv2_query(router, "DeleteTargetGroup", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DeleteTargetGroup for {arn:?} expected 200, got {status}: {body}"
    );
}

/// One scaling activity as a client reads it: what it set out to do, its status code and
/// the message a failed one carries.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedActivity {
    description: String,
    status_code: String,
    status_message: String,
}

/// Every scaling activity the group reports, through `DescribeScalingActivities`.
async fn scaling_activities(router: &MultiServiceRouter, name: &str) -> Vec<ReportedActivity> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeScalingActivities for {name:?} expected 200, got {status}: {body}"
    );
    let inner = element_inner(&body, "Activities").unwrap_or_default();
    members(inner)
        .into_iter()
        .map(|member| ReportedActivity {
            description: xml_unescape(&tag_value(member, "Description")),
            status_code: tag_value(member, "StatusCode"),
            status_message: xml_unescape(&tag_value(member, "StatusMessage")),
        })
        .collect()
}

/// The activities of the group that FAILED, in the order it reports them.
async fn failed_activities(router: &MultiServiceRouter, name: &str) -> Vec<ReportedActivity> {
    scaling_activities(router, name)
        .await
        .into_iter()
        .filter(|activity| activity.status_code == ACTIVITY_FAILED)
        .collect()
}

/// Every instance the group reports, as `(instance id, LifecycleState)` in the order
/// reported.
async fn instance_states(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let body = described(router, name).await;
    let inner = element_inner(&body, "Instances").unwrap_or_default();
    members(inner)
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "LifecycleState"),
            )
        })
        .collect()
}

/// Read the group until it holds `wanted` instances and every one of them is in service, or
/// a generous deadline passes — the CONDITION, never a fixed wait. Answers what it last read.
async fn settled_instances(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = instance_states(router, name).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|(_, state)| state == LIFECYCLE_IN_SERVICE);
        if settled || Instant::now() >= deadline {
            assert_eq!(
                held.len(),
                wanted,
                "case {case:?}: expected {name:?} to hold {wanted} instance(s), got {held:?}"
            );
            for (instance_id, state) in &held {
                assert_eq!(
                    state, LIFECYCLE_IN_SERVICE,
                    "case {case:?}: expected {instance_id} of {name:?} to reach \
                     {LIFECYCLE_IN_SERVICE}, got {held:?}"
                );
            }
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ids of the targets Elastic Load Balancing v2 reports in `arn`, sorted — read through
/// its own `DescribeTargetHealth`.
async fn registered_ids(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let mut ids: Vec<String> = registered_targets(router, arn)
        .await
        .into_iter()
        .map(|target| target.id)
        .collect();
    ids.sort();
    ids
}

/// The ids of the instances the group reports, sorted — what every target group it is
/// attached to must hold.
fn instance_ids(instances: &[(String, String)]) -> Vec<String> {
    let mut ids: Vec<String> = instances.iter().map(|(id, _)| id.clone()).collect();
    ids.sort();
    ids
}

/// The target groups the group holds an attachment to, in EITHER form the attachment takes
/// — its own `TargetGroupARNs` and an `elbv2` traffic source — which is what a registration
/// of its instances follows.
async fn attached_identifiers(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = described(router, name).await;
    let mut held = tag_values(
        element_inner(&body, "TargetGroupARNs").unwrap_or_default(),
        "member",
    );
    for source in members(element_inner(&body, "TrafficSources").unwrap_or_default()) {
        if tag_value(source, "Type") == TRAFFIC_SOURCE_TYPE_ELBV2 {
            held.push(tag_value(source, "Identifier"));
        }
    }
    held
}

/// The `DesiredCapacity` the group reports.
async fn reported_desired_capacity(router: &MultiServiceRouter, name: &str) -> String {
    tag_value(&described(router, name).await, "DesiredCapacity")
}

/// The `StatusMessage` the failed activity of a launch blocked by deleted target groups
/// carries: Amazon EC2 Auto Scaling's own fixed text for a registration it could not make
/// ('Failed to update target group ') naming the target groups it could not update, then
/// Elastic Load Balancing's own `TargetGroupNotFound` refusal for the identifiers a call
/// names — one identifier, or several, under the plural noun, with the service, status and
/// code an AWS SDK renders beside it — followed by the launch Amazon EC2 Auto Scaling
/// therefore did not make.
fn blocked_message(unresolved: &[String]) -> String {
    let named = unresolved.join(", ");
    format!(
        "Failed to update target group {named}: Target groups '{named}' not found (Service: \
         ElasticLoadBalancingV2, Status Code: 400, Error Code: TargetGroupNotFound). Launching \
         EC2 instance failed."
    )
}

/// What the activity of that launch DESCRIBES: the launch it set out to make and the
/// status reason it failed with — the two members `DescribeScalingActivities` reports
/// apart, where the activity of a launch that succeeded names the instance it launched.
fn blocked_description(unresolved: &[String]) -> String {
    format!(
        "Launching a new EC2 instance. Status Reason: {}",
        blocked_message(unresolved)
    )
}

/// One row of the deleted-attachment table: how many target groups the group is attached to,
/// which of them are deleted out of band, how the attachment was made, how many instances the
/// group holds before the scale-out, and the capacity the scale-out asks for.
struct DeletedAttachmentRow {
    case: &'static str,
    target_groups: usize,
    /// The indexes, into the row's own target groups, of the ones deleted before the
    /// scale-out. Empty for a row whose every attachment still resolves.
    deleted: &'static [usize],
    attached_as: AttachedAs,
    held: usize,
    desired: usize,
}

fn deleted_attachment_rows() -> Vec<DeletedAttachmentRow> {
    vec![
        DeletedAttachmentRow {
            case: "the only attached target group is deleted",
            target_groups: 1,
            deleted: &[0],
            attached_as: AttachedAs::TargetGroupArns,
            held: 1,
            desired: 2,
        },
        DeletedAttachmentRow {
            case: "one of two attached target groups is deleted",
            target_groups: 2,
            deleted: &[1],
            attached_as: AttachedAs::TargetGroupArns,
            held: 1,
            desired: 2,
        },
        DeletedAttachmentRow {
            case: "both attached target groups are deleted, scaling out by two",
            target_groups: 2,
            deleted: &[0, 1],
            attached_as: AttachedAs::TargetGroupArns,
            held: 2,
            desired: 4,
        },
        DeletedAttachmentRow {
            case: "the deleted target group is named as an elbv2 traffic source",
            target_groups: 1,
            deleted: &[0],
            attached_as: AttachedAs::Elbv2TrafficSources,
            held: 1,
            desired: 2,
        },
        DeletedAttachmentRow {
            case: "the deleted target group was attached by a later attach",
            target_groups: 1,
            deleted: &[0],
            attached_as: AttachedAs::LaterAttach,
            held: 1,
            desired: 3,
        },
        DeletedAttachmentRow {
            case: "every attached target group still exists",
            target_groups: 2,
            deleted: &[],
            attached_as: AttachedAs::TargetGroupArns,
            held: 1,
            desired: 2,
        },
        DeletedAttachmentRow {
            case: "the group is attached to nothing at all",
            target_groups: 0,
            deleted: &[],
            attached_as: AttachedAs::TargetGroupArns,
            held: 1,
            desired: 3,
        },
    ]
}

/// Every row: a scale-out of a group holding an attachment whose target group is GONE
/// launches nothing and records one failed activity per instance it was asked for, naming
/// the target groups that are missing — while the group's stored capacity still moves, the
/// instances it already holds stay in service and registered, and the attachment that can
/// never resolve again is no longer one the group holds. A row whose every attachment
/// resolves scales out as it always did.
#[tokio::test]
async fn a_scale_out_is_refused_while_an_attached_target_group_is_gone() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-deleted").await;

    for (row, case) in deleted_attachment_rows().into_iter().enumerate() {
        let name = format!("asg-deleted-{row}");
        let mut arns = Vec::new();
        for index in 0..case.target_groups {
            arns.push(
                create_target_group(
                    &router,
                    &format!("tg-deleted-{row}-{index}"),
                    80,
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }
        create_group(
            &router,
            &name,
            "lt-deleted",
            &subnets,
            case.held,
            &arns,
            case.attached_as,
        )
        .await;
        let before = settled_instances(&router, case.case, &name, case.held).await;
        // Every attachment holds the instances the group launched before anything was
        // deleted, so what follows is a change the delete made and not a group that never
        // registered.
        for arn in &arns {
            let actual = registered_ids(&router, arn).await;
            assert_eq!(
                actual,
                instance_ids(&before),
                "case {:?}: expected {arn:?} to hold the group's instances before the delete, \
                 got {actual:?}",
                case.case
            );
        }

        let gone: Vec<String> = case
            .deleted
            .iter()
            .map(|index| arns[*index].clone())
            .collect();
        for arn in &gone {
            delete_target_group(&router, arn).await;
        }
        let kept: Vec<String> = arns
            .iter()
            .filter(|arn| !gone.contains(arn))
            .cloned()
            .collect();

        let (status, code, message) = set_desired_capacity(&router, &name, case.desired).await;
        assert_eq!(
            status, 200,
            "case {:?}: the capacity change itself must be served, got {status} with {code:?} \
             {message:?}",
            case.case
        );
        // The capacity the group was asked for is the capacity it holds, whether or not the
        // launch it owes could be made: the request was accepted.
        let reported = reported_desired_capacity(&router, &name).await;
        assert_eq!(
            reported,
            case.desired.to_string(),
            "case {:?}: expected the group to report DesiredCapacity {}, got {reported:?}",
            case.case,
            case.desired
        );

        let failed = failed_activities(&router, &name).await;
        if gone.is_empty() {
            // Nothing blocks the launch, so the scale-out happens and nothing failed.
            let after = settled_instances(&router, case.case, &name, case.desired).await;
            assert!(
                failed.is_empty(),
                "case {:?}: expected no failed activity, got {failed:?}",
                case.case
            );
            for arn in &kept {
                let actual = registered_ids(&router, arn).await;
                assert_eq!(
                    actual,
                    instance_ids(&after),
                    "case {:?}: expected {arn:?} to hold every instance after the scale-out, got \
                     {actual:?}",
                    case.case
                );
            }
            continue;
        }

        // Blocked: nothing was launched, and the instances the group already holds are
        // exactly the ones it still reports, still in service.
        let after = instance_states(&router, &name).await;
        assert_eq!(
            after, before,
            "case {:?}: expected the group to still hold {before:?} and launch nothing, got \
             {after:?}",
            case.case
        );
        let wanted = blocked_message(&gone);
        let wanted_description = blocked_description(&gone);
        assert_eq!(
            failed.len(),
            case.desired - case.held,
            "case {:?}: expected one failed activity per instance the change asked for, got \
             {failed:?}",
            case.case
        );
        for activity in &failed {
            assert_eq!(
                activity.status_message, wanted,
                "case {:?}: expected the failed activity to report {wanted:?}, got {activity:?}",
                case.case
            );
            assert_eq!(
                activity.description, wanted_description,
                "case {:?}: expected the failed activity to describe {wanted_description:?}, got \
                 {activity:?}",
                case.case
            );
        }
        // The attachments that DO resolve are untouched by the refusal: they hold the
        // instances the group already had, and nothing was registered for a launch that
        // never happened.
        for arn in &kept {
            let actual = registered_ids(&router, arn).await;
            assert_eq!(
                actual,
                instance_ids(&before),
                "case {:?}: expected {arn:?} to hold only the instances the group already had, \
                 got {actual:?}",
                case.case
            );
        }
        // The attachment is the CLIENT's: the group goes on reporting every target group it
        // was attached to, the one that is gone included, because only a detach takes an
        // attachment off a group.
        let attached = attached_identifiers(&router, &name).await;
        for arn in arns.iter() {
            assert!(
                attached.contains(arn),
                "case {:?}: expected the group to keep its attachment to {arn:?}, got \
                 {attached:?}",
                case.case
            );
        }
        // And the client's own detach of the target group that is gone is served: a group
        // that reports an attachment cannot refuse the call that removes it. (A traffic
        // source is removed by `DetachTrafficSources` instead, which this suite's detach
        // helper does not post, so the rows naming one stop at the assertion above.)
        if !gone.is_empty() && case.attached_as != AttachedAs::Elbv2TrafficSources {
            let status = detach(&router, &name, &gone).await;
            assert_eq!(
                status, 200,
                "case {:?}: detaching the deleted target group(s) {gone:?} must be served, got \
                 {status}",
                case.case
            );
            let attached = attached_identifiers(&router, &name).await;
            for arn in &gone {
                assert!(
                    !attached.contains(arn),
                    "case {:?}: expected the detached {arn:?} to be gone from the group, got \
                     {attached:?}",
                    case.case
                );
            }
            for arn in &kept {
                assert!(
                    attached.contains(arn),
                    "case {:?}: expected the detach to leave {arn:?} attached, got {attached:?}",
                    case.case
                );
            }
        }
    }
}

/// The refusal stands until the CLIENT clears it: while the group holds an attachment
/// Elastic Load Balancing v2 cannot resolve, every scale-out launches nothing and records
/// its own refusal, and once the client has detached that attachment — which the group,
/// still reporting it, serves — and attached a fresh target group, the same scale-out
/// launches the instance and registers it there.
#[tokio::test]
async fn a_scale_out_resumes_once_no_attachment_is_missing() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-resumes").await;

    let gone = create_target_group(&router, "tg-resumes", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    create_group(
        &router,
        "asg-resumes",
        "lt-resumes",
        &subnets,
        1,
        std::slice::from_ref(&gone),
        AttachedAs::TargetGroupArns,
    )
    .await;
    let before = settled_instances(&router, "before the delete", "asg-resumes", 1).await;

    delete_target_group(&router, &gone).await;
    let (status, code, message) = set_desired_capacity(&router, "asg-resumes", 2).await;
    assert_eq!(
        status, 200,
        "the blocked capacity change must be served, got {status} with {code:?} {message:?}"
    );
    let blocked = failed_activities(&router, "asg-resumes").await;
    let wanted = blocked_message(std::slice::from_ref(&gone));
    assert_eq!(
        blocked.len(),
        1,
        "the blocked scale-out must record one failed activity, got {blocked:?}"
    );
    assert_eq!(
        blocked[0].status_message, wanted,
        "expected the failed activity to report {wanted:?}, got {:?}",
        blocked[0]
    );
    let held = instance_states(&router, "asg-resumes").await;
    assert_eq!(
        held, before,
        "the blocked scale-out must launch nothing, got {held:?}"
    );
    // Nothing launches for that change afterwards either: the refusal was the whole of what
    // it did, so the group stays exactly as it left it and reports the refusal once. Watched
    // over a budget several times the span this service owes a transition, so a launch the
    // clock could still have made falls inside it.
    let quiet = Instant::now() + autoscaling::state::LIFECYCLE_TRANSITION * 8;
    while Instant::now() < quiet {
        tokio::time::sleep(POLL_INTERVAL).await;
        let held = instance_states(&router, "asg-resumes").await;
        assert_eq!(
            held, before,
            "the refused scale-out must launch nothing afterwards either, got {held:?}"
        );
        let still = failed_activities(&router, "asg-resumes").await;
        assert_eq!(
            still, blocked,
            "the refused scale-out must report its refusal once, got {still:?}"
        );
    }
    // The attachment the launch could not register into is STILL the group's: only the
    // client takes an attachment off a group.
    let attached = attached_identifiers(&router, "asg-resumes").await;
    assert_eq!(
        attached,
        vec![gone.clone()],
        "expected the group to still hold its attachment to the deleted target group, got \
         {attached:?}"
    );
    // While it is there the block stands, so the SAME capacity change is refused again and
    // nothing is launched for it.
    let (status, code, message) = set_desired_capacity(&router, "asg-resumes", 2).await;
    assert_eq!(
        status, 200,
        "the repeated blocked capacity change must be served, got {status} with {code:?} \
         {message:?}"
    );
    let again = failed_activities(&router, "asg-resumes").await;
    assert_eq!(
        again.len(),
        2,
        "each blocked scale-out must record its own refusal, got {again:?}"
    );
    assert!(
        again.iter().all(|activity| activity.status_message == wanted),
        "every refusal must report {wanted:?}, got {again:?}"
    );
    let held = instance_states(&router, "asg-resumes").await;
    assert_eq!(
        held, before,
        "the repeated blocked scale-out must launch nothing either, got {held:?}"
    );

    // The client's own remediation: detach the target group that is gone — the group reports
    // it, so the detach is served — and attach a fresh one in its place. Nothing is in the
    // way now, so the same capacity change launches the instance and registers it there.
    let status = detach(&router, "asg-resumes", std::slice::from_ref(&gone)).await;
    assert_eq!(
        status, 200,
        "detaching the deleted target group must be served, got {status}"
    );
    let attached = attached_identifiers(&router, "asg-resumes").await;
    assert!(
        attached.is_empty(),
        "expected the detach to leave the group with no attachment, got {attached:?}"
    );
    let fresh = create_target_group(&router, "tg-fresh", 80, TARGET_TYPE_INSTANCE, &vpc_id).await;
    let (status, code, message) = attach(&router, "asg-resumes", std::slice::from_ref(&fresh)).await;
    assert_eq!(
        status, 200,
        "attaching the fresh target group must succeed, got {status} with {code:?} {message:?}"
    );
    let (status, code, message) = set_desired_capacity(&router, "asg-resumes", 2).await;
    assert_eq!(
        status, 200,
        "the capacity change asked for again must be served, got {status} with {code:?} \
         {message:?}"
    );

    let after = settled_instances(&router, "after the fresh attachment", "asg-resumes", 2).await;
    let actual = registered_ids(&router, &fresh).await;
    assert_eq!(
        actual,
        instance_ids(&after),
        "expected the fresh target group to hold every instance of the group, got {actual:?}"
    );
    let failed = failed_activities(&router, "asg-resumes").await;
    assert_eq!(
        failed, again,
        "the resumed scale-out must record no further failure, got {failed:?}"
    );
}

/// The fixed text Amazon EC2 Auto Scaling opens the activity of a launch it could not bring
/// into a target group with, and the whole of what a client matches such an activity on:
/// recorded behaviour, where the message reads `Failed to update target group ` and then the
/// target group's own ARN.
const FAILED_TARGET_GROUP_UPDATE: &str = "Failed to update target group";

/// One row of the blocked-launch message table: the attachments the group holds, which of
/// them are deleted out of band, how they were attached, the target group's own port,
/// whether the group was scaled out once while every attachment still resolved, how many
/// instances it holds before the blocked change, and the capacity that change asks for.
struct BlockedMessageRow {
    case: &'static str,
    target_groups: usize,
    /// The indexes, into the row's own target groups, of the ones deleted before the
    /// scale-out.
    deleted: &'static [usize],
    attached_as: AttachedAs,
    port: i32,
    /// Whether one scale-out is made while every attachment still resolves, before the
    /// delete — a launch that must register, and therefore record no failure.
    positive_control: bool,
    held: usize,
    desired: usize,
}

fn blocked_message_rows() -> Vec<BlockedMessageRow> {
    vec![
        BlockedMessageRow {
            case: "the only attached target group is deleted after a scale-out that worked",
            target_groups: 1,
            deleted: &[0],
            attached_as: AttachedAs::TargetGroupArns,
            port: 80,
            positive_control: true,
            held: 1,
            desired: 3,
        },
        BlockedMessageRow {
            case: "both attached target groups are deleted, scaling out by two",
            target_groups: 2,
            deleted: &[0, 1],
            attached_as: AttachedAs::TargetGroupArns,
            port: 8080,
            positive_control: false,
            held: 1,
            desired: 3,
        },
        BlockedMessageRow {
            case: "one of two attached target groups is deleted, named as a traffic source",
            target_groups: 2,
            deleted: &[1],
            attached_as: AttachedAs::Elbv2TrafficSources,
            port: 80,
            positive_control: false,
            held: 1,
            desired: 2,
        },
        BlockedMessageRow {
            case: "the deleted target group was attached by a later attach",
            target_groups: 1,
            deleted: &[0],
            attached_as: AttachedAs::LaterAttach,
            port: 8080,
            positive_control: false,
            held: 2,
            desired: 3,
        },
    ]
}

/// Every row: the activity a client polls for after a scale-out that could not be brought
/// into its target groups is found by the text Amazon EC2 Auto Scaling words that failure
/// with — a `Failed` activity whose message opens with [`FAILED_TARGET_GROUP_UPDATE`] and
/// names, by ARN, each target group the launch could not be put into, carries the refusal
/// the load-balancing service answered for it, and ends with the launch that was therefore
/// not made. A launch every attachment resolved for records no such activity.
#[tokio::test]
async fn a_blocked_launch_names_the_target_group_it_could_not_update() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-blocked-message").await;

    for (row, case) in blocked_message_rows().into_iter().enumerate() {
        let name = format!("asg-blocked-message-{row}");
        let mut arns = Vec::new();
        for index in 0..case.target_groups {
            arns.push(
                create_target_group(
                    &router,
                    &format!("tg-blocked-message-{row}-{index}"),
                    case.port,
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }
        create_group(
            &router,
            &name,
            "lt-blocked-message",
            &subnets,
            case.held,
            &arns,
            case.attached_as,
        )
        .await;
        let mut held = case.held;
        settled_instances(&router, case.case, &name, held).await;
        // The control: while every attachment resolves the scale-out launches and registers,
        // so nothing before the delete has recorded a failure for a client to find.
        if case.positive_control {
            let (status, code, message) = set_desired_capacity(&router, &name, held + 1).await;
            assert_eq!(
                status, 200,
                "case {:?}: the control scale-out must be served, got {status} with {code:?} \
                 {message:?}",
                case.case
            );
            held += 1;
            let settled = settled_instances(&router, case.case, &name, held).await;
            for arn in &arns {
                let actual = registered_ids(&router, arn).await;
                assert_eq!(
                    actual,
                    instance_ids(&settled),
                    "case {:?}: expected {arn:?} to hold the group's instances before the \
                     delete, got {actual:?}",
                    case.case
                );
            }
            let failed = failed_activities(&router, &name).await;
            assert!(
                failed.is_empty(),
                "case {:?}: the control scale-out must record no failure, got {failed:?}",
                case.case
            );
        }

        let gone: Vec<String> = case
            .deleted
            .iter()
            .map(|index| arns[*index].clone())
            .collect();
        for arn in &gone {
            delete_target_group(&router, arn).await;
        }
        let (status, code, message) = set_desired_capacity(&router, &name, case.desired).await;
        assert_eq!(
            status, 200,
            "case {:?}: the blocked capacity change must be served, got {status} with {code:?} \
             {message:?}",
            case.case
        );

        // What a client does with the activity log: it polls it for the failure AWS words a
        // blocked registration with, and reads the target group out of the message it finds.
        let wanted = case.desired - held;
        let deadline = Instant::now() + poll_budget();
        let found = loop {
            let found: Vec<ReportedActivity> = failed_activities(&router, &name)
                .await
                .into_iter()
                .filter(|activity| {
                    activity.status_message.contains(FAILED_TARGET_GROUP_UPDATE)
                        || activity.description.contains(FAILED_TARGET_GROUP_UPDATE)
                })
                .collect();
            if found.len() == wanted || Instant::now() >= deadline {
                break found;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            found.len(),
            wanted,
            "case {:?}: expected {wanted} activity(ies) a client finds by {:?}, got {found:?}",
            case.case,
            FAILED_TARGET_GROUP_UPDATE
        );
        let expected_message = blocked_message(&gone);
        let expected_description = blocked_description(&gone);
        for activity in &found {
            assert_eq!(
                activity.status_code, ACTIVITY_FAILED,
                "case {:?}: expected the blocked launch to report {ACTIVITY_FAILED}, got \
                 {activity:?}",
                case.case
            );
            assert_eq!(
                activity.status_message, expected_message,
                "case {:?}: expected the activity to report {expected_message:?}, got \
                 {activity:?}",
                case.case
            );
            assert_eq!(
                activity.description, expected_description,
                "case {:?}: expected the activity to describe {expected_description:?}, got \
                 {activity:?}",
                case.case
            );
            // The target groups the launch could not be put into are named by the ARN the
            // request carried, which is what a client repoints the group with.
            for arn in &gone {
                assert!(
                    activity.status_message.contains(arn.as_str()),
                    "case {:?}: expected the activity to name {arn:?}, got {activity:?}",
                    case.case
                );
            }
            // An attachment that still resolves took no part in the failure, so the message
            // names none of them.
            for arn in arns.iter().filter(|arn| !gone.contains(arn)) {
                assert!(
                    !activity.status_message.contains(arn.as_str()),
                    "case {:?}: expected the activity to name no resolvable attachment, got \
                     {activity:?} naming {arn:?}",
                    case.case
                );
            }
        }
        // Nothing else failed: every failed activity of the group is one of these.
        let failed = failed_activities(&router, &name).await;
        assert_eq!(
            failed, found,
            "case {:?}: expected the group's only failures to be the blocked launches, got \
             {failed:?}",
            case.case
        );
    }
}

// ---------------------------------------------------------------------------------------
// `DetachInstances`: releasing a member of a group WITHOUT terminating it, and the
// registrations that leave the target groups with it.
// ---------------------------------------------------------------------------------------

/// Post one `DetachInstances` and answer its status, wire error code and message.
async fn detach_instances(
    router: &MultiServiceRouter,
    name: &str,
    instance_ids: &[String],
    should_decrement: bool,
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(
            "ShouldDecrementDesiredCapacity",
            if should_decrement { "true" } else { "false" },
        ),
    ];
    for (index, instance_id) in instance_ids.iter().enumerate() {
        params.push(p(
            format!("InstanceIds.member.{}", index + 1).as_str(),
            instance_id.clone(),
        ));
    }
    let (status, body) = query(router, "DetachInstances", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The `instanceState` Amazon EC2 reports for `instance_id` — the service that OWNS the
/// instance is what says whether it is still running, which is what tells a detach from a
/// termination.
async fn ec2_instance_state(router: &MultiServiceRouter, instance_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstances for {instance_id:?} expected 200, got {status}: {body}"
    );
    let state = element_inner(&body, "instanceState").unwrap_or_default();
    tag_value(state, "name")
}

/// The `InstanceStateName` Amazon EC2 reports for an instance it is running.
const EC2_STATE_RUNNING: &str = "running";

/// Read the target group until it registers exactly `wanted`, or a generous deadline passes
/// — the CONDITION, never a fixed wait. Answers what it last read.
async fn settled_registrations(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = registered_ids(router, arn).await;
        if held == wanted || Instant::now() >= deadline {
            assert_eq!(
                held, wanted,
                "case {case:?}: expected {arn:?} to register {wanted:?}, got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the detach table: how many instances the group starts with, how many the
/// request releases, whether it asks for the decrement, and what the group is left holding.
struct DetachRow {
    case: &'static str,
    /// The `DesiredCapacity` the group is created with, and so how many instances it holds.
    desired: usize,
    /// How many of them the request detaches.
    detached: usize,
    /// Whether `ShouldDecrementDesiredCapacity` is set.
    decrement: bool,
    /// The `DesiredCapacity` the group reports afterwards.
    capacity_after: usize,
}

/// The rows: the two answers `ShouldDecrementDesiredCapacity` can take, and a call that
/// releases SEVERAL of a group's instances at once rather than one.
fn detach_rows() -> Vec<DetachRow> {
    vec![
        DetachRow {
            case: "one of two instances, decrementing the desired capacity",
            desired: 2,
            detached: 1,
            decrement: true,
            capacity_after: 1,
        },
        DetachRow {
            case: "one of two instances, leaving the desired capacity standing",
            desired: 2,
            detached: 1,
            decrement: false,
            capacity_after: 2,
        },
        DetachRow {
            case: "two of three instances, decrementing the desired capacity",
            desired: 3,
            detached: 2,
            decrement: true,
            capacity_after: 1,
        },
    ]
}

/// RULE: `DetachInstances` takes exactly the instances a request names out of the group
/// without terminating them — "removes one or more instances from the specified Auto Scaling
/// group. After the instances are detached, you can manage them independent of the Auto
/// Scaling group … If there are target groups attached to the Auto Scaling group, the
/// instances are deregistered from the target groups" — and "if you do not specify the option
/// to decrement the desired capacity, Amazon EC2 Auto Scaling launches instances to replace
/// the ones that are detached".
///
/// So each row asserts the four facts one detach is: the named instances stop being instances
/// of the group, exactly those registrations leave the target group while the group's
/// remaining instances stay registered, the desired capacity falls by what the request asked
/// for (or stands, and the group launches a replacement for each), and the detached Amazon EC2
/// instances are still RUNNING where they live.
#[tokio::test]
async fn a_detach_releases_its_instances_and_deregisters_exactly_them() {
    for (index, row) in detach_rows().into_iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let vpc_id = vpc_of(&router, &subnets[0]).await;
        let template = format!("lt-detach-instances-{index}");
        create_launch_template(&router, &template).await;
        let arn = create_target_group(
            &router,
            &format!("tg-detach-instances-{index}"),
            80,
            TARGET_TYPE_INSTANCE,
            &vpc_id,
        )
        .await;
        let name = format!("asg-detach-instances-{index}");
        create_group(
            &router,
            &name,
            &template,
            &subnets,
            row.desired,
            std::slice::from_ref(&arn),
            AttachedAs::TargetGroupArns,
        )
        .await;

        let before = settled_instances(&router, row.case, &name, row.desired).await;
        settled_registrations(&router, row.case, &arn, &instance_ids(&before)).await;

        let detached: Vec<String> = before
            .iter()
            .take(row.detached)
            .map(|(id, _)| id.clone())
            .collect();
        let kept: Vec<String> = before
            .iter()
            .skip(row.detached)
            .map(|(id, _)| id.clone())
            .collect();
        let (status, code, message) =
            detach_instances(&router, &name, &detached, row.decrement).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the detach to be served, got {status} {code:?} {message:?}",
            row.case
        );

        // The group holds the capacity the request left it with, and none of the instances it
        // released: with the decrement that is the instances it kept, and without it the ones
        // it kept plus a replacement for each released instance.
        let after = settled_instances(&router, row.case, &name, row.capacity_after).await;
        let held = instance_ids(&after);
        for instance_id in &detached {
            assert!(
                !held.contains(instance_id),
                "case {:?}: expected {instance_id} to be no instance of {name:?}, got {held:?}",
                row.case
            );
        }
        for instance_id in &kept {
            assert!(
                held.contains(instance_id),
                "case {:?}: expected {name:?} to keep {instance_id}, got {held:?}",
                row.case
            );
        }
        let capacity = reported_desired_capacity(&router, &name).await;
        assert_eq!(
            capacity,
            row.capacity_after.to_string(),
            "case {:?}: expected {name:?} to report a desired capacity of {}, got {capacity:?}",
            row.case,
            row.capacity_after
        );

        // Exactly the detached instances left the target group: the ones the group still
        // holds are still registered, and the released ones are not.
        settled_registrations(&router, row.case, &arn, &held).await;

        // A detached instance was RELEASED, not terminated: Amazon EC2 still runs it, which
        // is what "you can manage them independent of the Auto Scaling group" means.
        for instance_id in &detached {
            let state = ec2_instance_state(&router, instance_id).await;
            assert_eq!(
                state, EC2_STATE_RUNNING,
                "case {:?}: expected Amazon EC2 to still run the detached {instance_id}, got \
                 {state:?}",
                row.case
            );
        }
    }
}

/// RULE: a detach moves a member of THE GROUP, so a request naming an instance the group does
/// not hold is refused whole — nothing is detached and the group's capacity does not move —
/// and so is one naming more instances than the member admits ("you can specify up to 20
/// instances").
#[tokio::test]
async fn a_detach_naming_what_the_group_does_not_hold_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-detach-refusal").await;
    let arn = create_target_group(
        &router,
        "tg-detach-refusal",
        80,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let name = "asg-detach-refusal";
    create_group(
        &router,
        name,
        "lt-detach-refusal",
        &subnets,
        1,
        std::slice::from_ref(&arn),
        AttachedAs::TargetGroupArns,
    )
    .await;
    let before = settled_instances(&router, "the group before any detach", name, 1).await;
    let held = instance_ids(&before);

    // An id of an instance of no group at all, and one of the account's own instances the
    // group does not hold: both are instances this group is not holding, which is the one
    // thing the refusal is about.
    let foreign = "i-0123456789abcdef0";
    let too_many: Vec<String> = (0..21).map(|index| format!("i-{index:017x}")).collect();
    for (case, ids, expected) in [
        (
            "an instance of no group",
            vec![foreign.to_string()],
            format!("The instance {foreign} is not part of Auto Scaling group {name}."),
        ),
        (
            "an instance of the group beside one it does not hold",
            vec![held[0].clone(), foreign.to_string()],
            format!("The instance {foreign} is not part of Auto Scaling group {name}."),
        ),
        (
            "one more instance than the member admits",
            too_many.clone(),
            autoscaling::state::too_many_moved_instance_ids_message(too_many.len()),
        ),
    ] {
        let (status, code, message) = detach_instances(&router, name, &ids, true).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the detach to be refused, got {status} {code:?} {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // Nothing moved: the group holds the instance it held, still registered, at the capacity
    // it was created with.
    let after = settled_instances(&router, "the group after every refusal", name, 1).await;
    assert_eq!(
        instance_ids(&after),
        held,
        "a refused detach must leave the group's instances exactly as they were"
    );
    settled_registrations(&router, "the group after every refusal", &arn, &held).await;
    let capacity = reported_desired_capacity(&router, name).await;
    assert_eq!(
        capacity, "1",
        "a refused detach must leave the group's desired capacity where it was, got \
         {capacity:?}"
    );
}

/// Create one group over the awsQuery wire with the sizes `min`/`max`/`desired`, placed over
/// `subnets` and attached to `target_group`.
async fn create_sized_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    sizes: (usize, usize, usize),
    target_group: &str,
) {
    let (min, max, desired) = sizes;
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", min.to_string()),
        p("MaxSize", max.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", ids.join(",")),
        p("TargetGroupARNs.member.1", target_group),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// RULE: the decrement a detach asks for is a change to the group's SIZE, and a group never
/// holds a desired capacity below its own `MinSize` — so a request that would take it under
/// is refused whole rather than decrementing by less than it asked for.
#[tokio::test]
async fn a_detach_that_would_take_the_group_under_its_minimum_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-detach-min-size").await;
    // The two forms the refusal takes: a group whose desired capacity is AT its minimum (the
    // sentence's singular form, which one detached instance is enough to reach) and one
    // above it that a detach of several would cross. Each row has a target group of its own,
    // so what one group registers is the whole of what the row reads.
    for (case, sizes, detached, expected) in [
        (
            "the desired capacity already at the group's minimum",
            (1usize, 4usize, 1usize),
            1usize,
            autoscaling::state::min_size_decrement_breach_message("Detaching", 1, 1, 1),
        ),
        (
            "a detach of several crossing a minimum below the desired capacity",
            (2usize, 4usize, 3usize),
            2usize,
            autoscaling::state::min_size_decrement_breach_message("Detaching", 3, 2, 2),
        ),
    ] {
        let name = format!("asg-detach-min-size-{}", sizes.2);
        let arn = create_target_group(
            &router,
            &format!("tg-detach-min-size-{}", sizes.2),
            80,
            TARGET_TYPE_INSTANCE,
            &vpc_id,
        )
        .await;
        let template = "lt-detach-min-size";
        create_sized_group(&router, &name, template, &subnets, sizes, &arn).await;
        let held = settled_instances(&router, case, &name, sizes.2).await;
        let ids = instance_ids(&held);
        let asked: Vec<String> = ids.iter().take(detached).cloned().collect();

        let (status, code, message) = detach_instances(&router, &name, &asked, true).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the detach to be refused, got {status} {code:?} \
             {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        // Nothing moved: the same members, still registered, at the capacity the group was
        // created with.
        let after = settled_instances(&router, case, &name, sizes.2).await;
        assert_eq!(
            instance_ids(&after),
            ids,
            "case {case:?}: a refused detach must leave the group's instances as they were"
        );
        settled_registrations(&router, case, &arn, &ids).await;
        let capacity = reported_desired_capacity(&router, &name).await;
        assert_eq!(
            capacity,
            sizes.2.to_string(),
            "case {case:?}: a refused detach must leave the desired capacity where it was, \
             got {capacity:?}"
        );
    }
}

/// The `LifecycleState` an instance a TERMINATION lifecycle hook is holding waits in, from
/// the model's own `LifecycleState` enum — read from the crate so the two cannot drift.
const LIFECYCLE_TERMINATING_WAIT: &str = autoscaling::state::LIFECYCLE_TERMINATING_WAIT;

/// Read the group until `instance_id` is reported in `wanted`, or a generous deadline passes.
async fn instance_reaching(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = instance_states(router, name).await;
        let reported = held
            .iter()
            .find(|(id, _)| id == instance_id)
            .map(|(_, state)| state.clone())
            .unwrap_or_default();
        if reported == wanted || Instant::now() >= deadline {
            assert_eq!(
                reported, wanted,
                "case {case:?}: expected {instance_id} of {name:?} to be reported {wanted}, \
                 got {held:?}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a detach releases a member in one of the two states a member of a group is
/// detachable in — `InService`, or parked out of service in `Standby` — so a request naming
/// one that is in neither, such as an instance a termination lifecycle hook is holding on its
/// way out of the group, is refused whole and the member stays exactly where it was.
#[tokio::test]
async fn a_detach_of_an_instance_in_neither_detachable_state_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-detach-wrong-state").await;
    let arn = create_target_group(
        &router,
        "tg-detach-wrong-state",
        80,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let name = "asg-detach-wrong-state";
    create_group(
        &router,
        name,
        "lt-detach-wrong-state",
        &subnets,
        1,
        std::slice::from_ref(&arn),
        AttachedAs::TargetGroupArns,
    )
    .await;
    let held = settled_instances(&router, "a member on its way out", name, 1).await;
    let original = held[0].0.clone();

    // A termination hook of the group holds a member it is taking out of service in the
    // transition's wait state, which is neither state a detach may release it from — and it
    // waits there until a client resolves the hold, so the row is not racing a clock.
    let (status, body) = query(
        &router,
        "PutLifecycleHook",
        &[
            ("AutoScalingGroupName", name),
            ("LifecycleHookName", "hold-terminating"),
            (
                "LifecycleTransition",
                autoscaling::state::TRANSITION_TERMINATING,
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutLifecycleHook expected 200, got {status}: {body}"
    );
    let (status, code, message) = set_desired_capacity(&router, name, 0).await;
    assert_eq!(
        status, 200,
        "the scale-in must be served, got {status} {code:?} {message:?}"
    );
    instance_reaching(
        &router,
        "a member on its way out",
        name,
        &original,
        LIFECYCLE_TERMINATING_WAIT,
    )
    .await;

    let expected = format!(
        "Instance {original} is in {LIFECYCLE_TERMINATING_WAIT} state, not in InService or \
         Standby state, and cannot be detached."
    );
    let (status, code, message) =
        detach_instances(&router, name, std::slice::from_ref(&original), false).await;
    assert_eq!(
        status, 400,
        "expected the detach to be refused, got {status} {code:?} {message:?}"
    );
    assert_eq!(
        message, expected,
        "expected the message {expected:?}, got {message:?}"
    );
    // The refused detach moved nothing: the member is still held where the hook holds it.
    instance_reaching(
        &router,
        "a member on its way out, after the refusal",
        name,
        &original,
        LIFECYCLE_TERMINATING_WAIT,
    )
    .await;
}

// ---------------------------------------------------------------------------------------
// `HealthCheckType` `ELB`: the load balancer's verdict on an instance's target, folded into
// the group's own per-instance health.
// ---------------------------------------------------------------------------------------
/// The `HealthStatus` values a group reports an instance in.
const HEALTH_STATUS_HEALTHY: &str = "Healthy";
const HEALTH_STATUS_UNHEALTHY: &str = "Unhealthy";

/// The two `TargetHealthStateEnum` values a check of a registered target reaches, from the
/// model's own enum
/// (`smithy_query(query="shape",shape="TargetHealthStateEnum",
/// model_path="services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/smithy-model/com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10.json")`).
const TARGET_STATE_HEALTHY: &str = "healthy";
const TARGET_STATE_UNHEALTHY: &str = "unhealthy";

/// How long a row waits for a verdict the load balancer's own health checks reach, and the
/// group's reaction to it. Generous on purpose — many times the cadence a row configures
/// times the threshold a verdict takes — because what is asserted is that it happens at all,
/// never how fast the host runs.
const HEALTH_POLL_BUDGET: Duration = Duration::from_secs(120);

/// The `HealthCheckIntervalSeconds` the target groups below are checked at: a verdict takes
/// `UnhealthyThresholdCount` consecutive checks, each one interval after the last, so a row
/// that waits for one asks for the shortest interval the member admits ("the range is
/// 5-300").
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";

/// The port the instances below receive traffic on, which their own security group admits the
/// load balancer's checks on.
const ADMITTED_PORT: &str = "80";

/// A port it admits nothing on, so a health check pointed at it is answered by nothing.
const UNADMITTED_PORT: &str = "22";

/// The `TargetHealth$State` of every target the target group `arn` holds, by target id.
async fn target_health_states(router: &MultiServiceRouter, arn: &str) -> Vec<(String, String)> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    let inner = element_inner(&body, "TargetHealthDescriptions").unwrap_or_default();
    members(inner)
        .into_iter()
        .map(|member| (tag_value(member, "Id"), tag_value(member, "State")))
        .collect()
}

/// The `HealthStatus` the group reports for each of its instances, by instance id.
async fn instance_health_statuses(
    router: &MultiServiceRouter,
    name: &str,
) -> Vec<(String, String)> {
    let body = described(router, name).await;
    let inner = element_inner(&body, "Instances").unwrap_or_default();
    members(inner)
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "HealthStatus"),
            )
        })
        .collect()
}

/// Create one security group of `vpc_id` through Amazon EC2's own wire and answer its
/// `GroupId`.
async fn create_security_group(
    router: &MultiServiceRouter,
    name: &str,
    vpc_id: &str,
) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateSecurityGroup",
        &[
            p("GroupName", name),
            p("GroupDescription", "an Auto Scaling health-check suite group"),
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

/// Admit TCP `port` into `group_id` from the security group `source_group_id`: the one grant a
/// load balancer's health check of a target governed by `group_id` needs.
async fn authorize_ingress(
    router: &MultiServiceRouter,
    group_id: &str,
    source_group_id: &str,
    port: &str,
) {
    let (status, body) = ec2_query(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            p("GroupId", group_id),
            p("IpPermissions.1.IpProtocol", "tcp"),
            p("IpPermissions.1.FromPort", port),
            p("IpPermissions.1.ToPort", port),
            p("IpPermissions.1.Groups.1.GroupId", source_group_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "AuthorizeSecurityGroupIngress for {group_id:?} expected 200, got {status}: {body}"
    );
}

/// Create one internal Application Load Balancer over the subnets `subnets`, associated with
/// `security_group`, and answer its ARN.
async fn create_load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[SeededSubnet],
    security_group: &str,
) -> String {
    let mut params = vec![
        p("Name", name),
        p("Scheme", "internal"),
        p("Type", "application"),
        p("SecurityGroups.member.1", security_group),
    ];
    for (index, subnet) in subnets.iter().enumerate() {
        params.push(p(
            format!("Subnets.member.{}", index + 1).as_str(),
            subnet.id.clone(),
        ));
    }
    let (status, body) = elbv2_query(router, "CreateLoadBalancer", &params).await;
    assert_eq!(
        status, 200,
        "CreateLoadBalancer for {name:?} expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "LoadBalancerArn");
    assert!(
        !arn.is_empty(),
        "CreateLoadBalancer must report the load balancer's ARN, got {body}"
    );
    arn
}

/// Forward a listener of `load_balancer` to the target group `target_group`: what makes that
/// group one the load balancer CHECKS.
async fn create_listener(router: &MultiServiceRouter, load_balancer: &str, target_group: &str) {
    let (status, body) = elbv2_query(
        router,
        "CreateListener",
        &[
            p("LoadBalancerArn", load_balancer),
            p("Protocol", "HTTP"),
            p("Port", ADMITTED_PORT),
            p("DefaultActions.member.1.Type", "forward"),
            p("DefaultActions.member.1.TargetGroupArn", target_group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateListener for {load_balancer:?} expected 200, got {status}: {body}"
    );
}

/// One row of the health-check table: which port the target group's checks connect to — one
/// the instances' own security group admits and one it does not — and which health check type
/// the group is configured with, which is what decides whether the load balancer's verdict is
/// the group's.
struct HealthRow {
    case: &'static str,
    /// `HealthCheckPort` of the target group.
    check_port: &'static str,
    /// `HealthCheckType` of the Auto Scaling group.
    health_check_type: &'static str,
    /// The `TargetHealth$State` the registration reaches.
    target_state: &'static str,
    /// Whether the group reports its instance `Unhealthy` and replaces it.
    replaced: bool,
}

/// Create one group placed over `subnets`, holding one instance, attached to `target_group`
/// and health checked the way `health_check_type` says.
async fn create_health_checked_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    target_group: &str,
    health_check_type: &str,
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "1"),
        p("VPCZoneIdentifier", ids.join(",")),
        p("TargetGroupARNs.member.1", target_group),
        p("HealthCheckType", health_check_type),
        p("HealthCheckGracePeriod", "0"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Build `row`'s estate and assert the rule over it.
///
/// The estate is built in the order the rule needs: the group registers its instance with the
/// target group FIRST, and the listener that makes the load balancer check that group is
/// created after — so the periodic checks the listener starts are checks of a registration
/// that is already there.
async fn drive_health_row(row: &HealthRow, index: usize) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;

    // The two security groups the check is read against: the load balancer's own, which is
    // where a check comes FROM, and the instances', which admits it on one port alone.
    let balancer_group =
        create_security_group(&router, &format!("elb-health-lb-{index}"), &vpc_id).await;
    let instance_group =
        create_security_group(&router, &format!("elb-health-instance-{index}"), &vpc_id).await;
    authorize_ingress(&router, &instance_group, &balancer_group, ADMITTED_PORT).await;

    // The launch template the group launches from names that instance security group, so the
    // instances it launches are governed by it.
    let template = format!("lt-elb-health-{index}");
    let (status, body) = ec2_query(
        &router,
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
        &router,
        "CreateTargetGroup",
        &[
            p("Name", format!("tg-elb-health-{index}")),
            p("Protocol", "HTTP"),
            p("Port", ADMITTED_PORT),
            p("VpcId", vpc_id.as_str()),
            p("TargetType", TARGET_TYPE_INSTANCE),
            p("HealthCheckPort", row.check_port),
            p("HealthCheckIntervalSeconds", HEALTH_CHECK_INTERVAL_SECONDS),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTargetGroup expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "TargetGroupArn");

    let name = format!("asg-elb-health-{index}");
    create_health_checked_group(
        &router,
        &name,
        &template,
        &subnets,
        &arn,
        row.health_check_type,
    )
    .await;
    let launched = settled_instances(&router, case, &name, 1).await;
    let original = launched[0].0.clone();
    settled_registrations(&router, case, &arn, std::slice::from_ref(&original)).await;

    // The load balancer that CHECKS that target group: the listener forwarding to it is what
    // starts the checks, so they begin on a registration already in place.
    let load_balancer = create_load_balancer(
        &router,
        &format!("elb-health-{index}"),
        &subnets,
        &balancer_group,
    )
    .await;
    create_listener(&router, &load_balancer, &arn).await;

    // The verdict the load balancer's own checks reach for that registration, which is the
    // CONDITION everything below is read after.
    let deadline = Instant::now() + HEALTH_POLL_BUDGET;
    // Every per-instance health the group reported meanwhile, so the step the rule is about —
    // the instance reported `Unhealthy` before it is replaced — is observed rather than
    // raced for.
    let mut seen: Vec<(String, String)> = Vec::new();
    let reached = loop {
        let reported = target_health_states(&router, &arn).await;
        let reached = reported
            .iter()
            .find(|(id, _)| *id == original)
            .map(|(_, state)| state.clone())
            .unwrap_or_default();
        for observed in instance_health_statuses(&router, &name).await {
            if !seen.contains(&observed) {
                seen.push(observed);
            }
        }
        if reached == row.target_state || Instant::now() >= deadline {
            assert_eq!(
                reached, row.target_state,
                "case {case:?}: expected the load balancer to report {original} \
                 {:?}, got {reported:?}",
                row.target_state
            );
            break reached;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        reached, row.target_state,
        "case {case:?}: expected the registration to reach {:?}",
        row.target_state
    );

    if !row.replaced {
        // The verdict is in and the group is not held to it — either because the load
        // balancer found the target healthy, or because the group is health checked by
        // Amazon EC2 rather than by the load balancer — so it keeps the instance it
        // launched, reported healthy.
        let held = instance_health_statuses(&router, &name).await;
        assert_eq!(
            held,
            vec![(original.clone(), HEALTH_STATUS_HEALTHY.to_string())],
            "case {case:?}: expected {name:?} to keep reporting {original} \
             {HEALTH_STATUS_HEALTHY}, got {held:?}"
        );
        return;
    }

    // The group IS held to it: the instance is reported `Unhealthy`, and that is what makes
    // the replacement owed — "Amazon EC2 Auto Scaling terminates and replaces the unhealthy
    // instance".
    let deadline = Instant::now() + HEALTH_POLL_BUDGET;
    let replacement = loop {
        for observed in instance_health_statuses(&router, &name).await {
            if !seen.contains(&observed) {
                seen.push(observed);
            }
        }
        let held = instance_states(&router, &name).await;
        let replacement = held
            .iter()
            .find(|(id, state)| *id != original && state == LIFECYCLE_IN_SERVICE)
            .map(|(id, _)| id.clone());
        if replacement.is_some() || Instant::now() >= deadline {
            assert!(
                replacement.is_some(),
                "case {case:?}: expected {name:?} to replace the unhealthy {original}, got \
                 {held:?} having reported {seen:?}"
            );
            break replacement.unwrap_or_default();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert!(
        seen.contains(&(original.clone(), HEALTH_STATUS_UNHEALTHY.to_string())),
        "case {case:?}: expected {name:?} to report {original} {HEALTH_STATUS_UNHEALTHY} \
         before replacing it, got {seen:?}"
    );
    assert_ne!(
        replacement, original,
        "case {case:?}: expected the replacement to be an instance of its own"
    );
    // The replacement is a target of the same target group, which is what keeps the group
    // serving traffic through the replacement it just made.
    let registered = registered_ids(&router, &arn).await;
    assert!(
        registered.contains(&replacement),
        "case {case:?}: expected the replacement {replacement} to be registered with {arn:?}, \
         got {registered:?}"
    );
}

/// The rows of the health-check table, each driven by its own test so each gets its own
/// budget.
///
/// RULE: a group whose `HealthCheckType` names `ELB` takes the load balancer's verdict on the
/// target each of its instances occupies as that instance's OWN health — "`ELB` — Use Elastic
/// Load Balancing health checks", and with them enabled Amazon EC2 Auto Scaling "considers an
/// instance unhealthy if it fails either the EC2 status checks or the load balancer target
/// health checks" — so an instance the load balancer reports `unhealthy` is reported
/// `Unhealthy` by the group and then terminated and replaced, while one it reports `healthy`
/// stays `Healthy`. A group health checked by Amazon EC2 alone is held to no such verdict.
#[tokio::test]
async fn an_elb_health_checked_group_keeps_an_instance_the_balancer_reaches() {
    drive_health_row(
        &HealthRow {
            case: "ELB health checks, on a port the instances' security group admits",
            check_port: ADMITTED_PORT,
            health_check_type: "ELB",
            target_state: TARGET_STATE_HEALTHY,
            replaced: false,
        },
        0,
    )
    .await;
}

#[tokio::test]
async fn an_elb_health_checked_group_replaces_an_instance_the_balancer_cannot_reach() {
    drive_health_row(
        &HealthRow {
            case: "ELB health checks, on a port the instances' security group admits nothing on",
            check_port: UNADMITTED_PORT,
            health_check_type: "ELB",
            target_state: TARGET_STATE_UNHEALTHY,
            replaced: true,
        },
        1,
    )
    .await;
}

#[tokio::test]
async fn an_ec2_health_checked_group_is_held_to_no_balancer_verdict() {
    drive_health_row(
        &HealthRow {
            case: "EC2 health checks, with the same unreachable health-check port",
            check_port: UNADMITTED_PORT,
            health_check_type: "EC2",
            target_state: TARGET_STATE_UNHEALTHY,
            replaced: false,
        },
        2,
    )
    .await;
}
