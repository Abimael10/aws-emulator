// SPDX-License-Identifier: CC0-1.0

//! The ONE Elastic Load Balancing v2 attachment surface of an Auto Scaling group, read and
//! removed through BOTH of the API pairs that carry it.
//!
//! The rule under test: an Elastic Load Balancing v2 target group associated with an Auto
//! Scaling group is ONE attachment however the client stated it — under the group's own
//! `TargetGroupARNs` at create, through `AttachLoadBalancerTargetGroups`, or as an `elbv2`
//! traffic source through `AttachTrafficSources` — so EVERY reader of that association
//! reports it exactly once (`DescribeAutoScalingGroups`' `TargetGroupARNs` and
//! `TrafficSources`, `DescribeLoadBalancerTargetGroups` and `DescribeTrafficSources`), and
//! one removal through EITHER remover (`DetachLoadBalancerTargetGroups`,
//! `DetachTrafficSources`) takes it out of all four. The legacy pair and the traffic-source
//! pair are two views of one set, never two sets: "you can use both the original
//! `AttachLoadBalancerTargetGroups` API operation and `AttachTrafficSources` on the same
//! Auto Scaling group" (`AttachLoadBalancerTargetGroups`), and a group that reports an
//! attachment cannot refuse the call that removes it.
//!
//! The table spans what the rule generalises over — every call that can state an
//! association, one stated through both calls at once, one stated through each call for a
//! different target group, how many target groups are named, and which of the two removers
//! takes them off — rather than one recorded row. A view is compared as the SET of what it
//! names, each entry once: WHICH attachments a reader reports is this suite's concern, while
//! the sequence each reader lists them in is pinned by the suites that own that reader.
//!
//! Every assertion names its case and the values actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, create_target_group, emulator, list_members, p, query,
    seed_subnets, tag_value, vpc_of, SeededSubnet,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID"), the only
/// kind an Auto Scaling group may be associated with.
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` an Elastic Load Balancing v2 target group is a traffic
/// source under: "`elbv2` if `Identifier` is the ARN of an Application Load Balancer, Gateway
/// Load Balancer, or Network Load Balancer target group".
const TYPE_ELBV2: &str = "elbv2";

/// The port every target group of this suite listens on.
const TARGET_GROUP_PORT: i32 = 80;

/// The launch template every group here launches from.
const LAUNCH_TEMPLATE: &str = "lt-one-surface";

/// How an association is STATED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatedAs {
    /// `CreateAutoScalingGroup`'s own `TargetGroupARNs`.
    AtCreate,
    /// `AttachLoadBalancerTargetGroups`, under `TargetGroupARNs`.
    LegacyAttach,
    /// `AttachTrafficSources`, each identifier typed `elbv2`.
    TrafficSourceAttachTyped,
    /// `AttachTrafficSources`, with the type left out, which the identifier's own ARN states.
    TrafficSourceAttachUntyped,
    /// The SAME target groups stated through both calls: one attachment each.
    BothCallsSameTargetGroups,
    /// One target group through the legacy call and the next one as a traffic source, so the
    /// group holds an attachment stated each way at once.
    OneThroughEachCall,
}

/// Which call takes the attachments off again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachedBy {
    /// `DetachLoadBalancerTargetGroups`, under `TargetGroupARNs`.
    LegacyDetach,
    /// `DetachTrafficSources`, each identifier typed `elbv2`.
    TrafficSourceDetach,
}

/// The four views of one group's Elastic Load Balancing v2 attachments, each as the SET of
/// target group ARNs it names, sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Views {
    /// `DescribeAutoScalingGroups` -> `AutoScalingGroups[].TargetGroupARNs`.
    group_target_group_arns: Vec<String>,
    /// `DescribeAutoScalingGroups` -> `AutoScalingGroups[].TrafficSources[].Identifier`, of
    /// the `elbv2` entries.
    group_traffic_sources: Vec<String>,
    /// `DescribeLoadBalancerTargetGroups` -> `LoadBalancerTargetGroups[].LoadBalancerTargetGroupARN`.
    described_target_groups: Vec<String>,
    /// `DescribeTrafficSources(TrafficSourceType=elbv2)` -> `TrafficSources[].Identifier`.
    described_traffic_sources: Vec<String>,
}

impl Views {
    /// The one answer every view owes when the group holds exactly `arns`.
    fn holding(arns: &[String]) -> Self {
        let expected = sorted(arns.to_vec());
        Self {
            group_target_group_arns: expected.clone(),
            group_traffic_sources: expected.clone(),
            described_target_groups: expected.clone(),
            described_traffic_sources: expected,
        }
    }
}

/// `values`, sorted, so a view is compared as the set of what it names.
fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

/// Create the group `name` in `subnet`, with `arns` named under `TargetGroupARNs` when the
/// case states its association at create time.
async fn given_group(
    router: &MultiServiceRouter,
    name: &str,
    subnet: &SeededSubnet,
    arns: &[String],
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnet.id.clone()),
    ];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            &format!("TargetGroupARNs.member.{}", index + 1),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Post `AttachLoadBalancerTargetGroups` (or its detach) for `arns` and require it to be
/// served.
async fn legacy_call(router: &MultiServiceRouter, action: &str, group: &str, arns: &[String]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            &format!("TargetGroupARNs.member.{}", index + 1),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "{action} of {arns:?} on {group:?} must be served, got {status} with {body:?}"
    );
}

/// Post `AttachTrafficSources` (or its detach) for `arns`, typed or not, and require it to be
/// served.
async fn traffic_source_call(
    router: &MultiServiceRouter,
    action: &str,
    group: &str,
    arns: &[String],
    typed: bool,
) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, arn) in arns.iter().enumerate() {
        let prefix = format!("TrafficSources.member.{}", index + 1);
        params.push(p(&format!("{prefix}.Identifier"), arn.clone()));
        if typed {
            params.push(p(&format!("{prefix}.Type"), TYPE_ELBV2));
        }
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "{action} of {arns:?} on {group:?} must be served, got {status} with {body:?}"
    );
}

/// Read all four views of what `group` is attached to.
async fn views(router: &MultiServiceRouter, case: &str, group: &str) -> Views {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group's own read must succeed, got {status} with {body:?}"
    );
    let group_target_group_arns = sorted(
        list_members(&body, "TargetGroupARNs")
            .into_iter()
            .map(str::to_string)
            .collect(),
    );
    let group_traffic_sources = sorted(
        list_members(&body, "TrafficSources")
            .into_iter()
            .filter(|member| tag_value(member, "Type") == TYPE_ELBV2)
            .map(|member| tag_value(member, "Identifier"))
            .collect(),
    );

    let (status, body) = query(
        router,
        "DescribeLoadBalancerTargetGroups",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLoadBalancerTargetGroups must succeed, got {status} with \
         {body:?}"
    );
    let described_target_groups = sorted(
        list_members(&body, "LoadBalancerTargetGroups")
            .into_iter()
            .map(|member| tag_value(member, "LoadBalancerTargetGroupARN"))
            .collect(),
    );

    // The read that asks for ONE kind of traffic source: an association of a target group is
    // of that kind whichever call recorded it.
    let (status, body) = query(
        router,
        "DescribeTrafficSources",
        &[
            ("AutoScalingGroupName", group),
            ("TrafficSourceType", TYPE_ELBV2),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTrafficSources must succeed, got {status} with {body:?}"
    );
    let described_traffic_sources = sorted(
        list_members(&body, "TrafficSources")
            .into_iter()
            .map(|member| tag_value(member, "Identifier"))
            .collect(),
    );

    Views {
        group_target_group_arns,
        group_traffic_sources,
        described_target_groups,
        described_traffic_sources,
    }
}

/// RULE: however an association was stated, every one of the four readers reports it once,
/// and one removal through either remover leaves every reader reporting nothing.
#[tokio::test]
async fn one_attachment_is_reported_and_removed_through_both_api_pairs() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, LAUNCH_TEMPLATE).await;

    // Each row: how the association is stated, how many target groups it names, and which
    // call takes them off again.
    let rows: Vec<(StatedAs, usize, DetachedBy)> = vec![
        (StatedAs::AtCreate, 1, DetachedBy::LegacyDetach),
        (StatedAs::AtCreate, 2, DetachedBy::TrafficSourceDetach),
        (StatedAs::LegacyAttach, 1, DetachedBy::TrafficSourceDetach),
        (StatedAs::LegacyAttach, 2, DetachedBy::LegacyDetach),
        (
            StatedAs::TrafficSourceAttachTyped,
            1,
            DetachedBy::LegacyDetach,
        ),
        (
            StatedAs::TrafficSourceAttachTyped,
            2,
            DetachedBy::TrafficSourceDetach,
        ),
        (
            StatedAs::TrafficSourceAttachUntyped,
            1,
            DetachedBy::LegacyDetach,
        ),
        (
            StatedAs::BothCallsSameTargetGroups,
            1,
            DetachedBy::LegacyDetach,
        ),
        (
            StatedAs::BothCallsSameTargetGroups,
            2,
            DetachedBy::TrafficSourceDetach,
        ),
        (StatedAs::OneThroughEachCall, 2, DetachedBy::LegacyDetach),
        (
            StatedAs::OneThroughEachCall,
            2,
            DetachedBy::TrafficSourceDetach,
        ),
    ];

    for (row, (stated, count, detached_by)) in rows.into_iter().enumerate() {
        let case = format!("{count} target group(s) stated as {stated:?}, removed by \
             {detached_by:?}");
        let mut arns: Vec<String> = Vec::new();
        for index in 0..count {
            arns.push(
                create_target_group(
                    &router,
                    &format!("tg-surface-{row}-{index}"),
                    TARGET_GROUP_PORT,
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }
        let group = format!("asg-surface-{row}");
        let at_create: &[String] = match stated {
            StatedAs::AtCreate => &arns,
            _ => &[],
        };
        given_group(&router, &group, &subnets[0], at_create).await;
        match stated {
            StatedAs::AtCreate => {}
            StatedAs::LegacyAttach => {
                legacy_call(&router, "AttachLoadBalancerTargetGroups", &group, &arns).await;
            }
            StatedAs::TrafficSourceAttachTyped => {
                traffic_source_call(&router, "AttachTrafficSources", &group, &arns, true).await;
            }
            StatedAs::TrafficSourceAttachUntyped => {
                traffic_source_call(&router, "AttachTrafficSources", &group, &arns, false).await;
            }
            // The same attachments again, under their other spelling: one attachment each.
            StatedAs::BothCallsSameTargetGroups => {
                legacy_call(&router, "AttachLoadBalancerTargetGroups", &group, &arns).await;
                traffic_source_call(&router, "AttachTrafficSources", &group, &arns, true).await;
            }
            // Each call states a DIFFERENT target group, so the group holds one attachment
            // from each.
            StatedAs::OneThroughEachCall => {
                let (first, rest) = arns.split_at(1);
                legacy_call(&router, "AttachLoadBalancerTargetGroups", &group, first).await;
                traffic_source_call(&router, "AttachTrafficSources", &group, rest, true).await;
            }
        }

        let reported = views(&router, &case, &group).await;
        assert_eq!(
            reported,
            Views::holding(&arns),
            "case {case:?}: expected every view to report {arns:?} once, got {reported:?}"
        );

        match detached_by {
            DetachedBy::LegacyDetach => {
                legacy_call(&router, "DetachLoadBalancerTargetGroups", &group, &arns).await;
            }
            DetachedBy::TrafficSourceDetach => {
                traffic_source_call(&router, "DetachTrafficSources", &group, &arns, true).await;
            }
        }
        let reported = views(&router, &case, &group).await;
        assert_eq!(
            reported,
            Views::holding(&[]),
            "case {case:?}: expected the removal to leave the group attached to nothing, got \
             {reported:?}"
        );
    }
}
