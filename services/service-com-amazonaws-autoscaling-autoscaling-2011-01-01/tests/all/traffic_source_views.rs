// SPDX-License-Identifier: CC0-1.0

//! The ONE set of traffic sources an Auto Scaling group holds, read through both members that
//! report it, over the awsQuery wire.
//!
//! The rule under test: an Elastic Load Balancing v2 target group associated with an Auto
//! Scaling group is ONE association however the client stated it — under the group's own
//! `TargetGroupARNs` at create, through `AttachLoadBalancerTargetGroups`, or as an `elbv2`
//! traffic source through `AttachTrafficSources` — so the group's traffic-source view reports
//! every one of them exactly once, each `{Identifier: <the target group ARN>, Type: elbv2}`,
//! through `DescribeAutoScalingGroups`' `TrafficSources` member and through
//! `DescribeTrafficSources` alike; and the detach of a traffic source removes that one
//! association, so neither view reports it afterwards.
//!
//! The table spans what the rule generalises over — every way an association can be stated,
//! one stated twice, and more than one association at a time — rather than one recorded row,
//! and every assertion names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, create_target_group, emulator, list_members, p, query,
    seed_subnets, tag_value, vpc_of, SeededSubnet,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID"), which is
/// the only kind an Auto Scaling group may be associated with.
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` an Elastic Load Balancing v2 target group is a traffic
/// source under: "`elbv2` if `Identifier` is the ARN of an Application Load Balancer, Gateway
/// Load Balancer, or Network Load Balancer target group".
const TYPE_ELBV2: &str = "elbv2";

/// The port every target group of this suite listens on.
const TARGET_GROUP_PORT: i32 = 80;

/// The launch template every group here launches from.
const LAUNCH_TEMPLATE: &str = "lt-traffic-views";

/// How an association is STATED: the three calls that record one, and the two ways a traffic
/// source may be typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatedAs {
    /// `CreateAutoScalingGroup`'s own `TargetGroupARNs`.
    CreateTargetGroupArns,
    /// `AttachLoadBalancerTargetGroups`, under `TargetGroupARNs`.
    AttachTargetGroupArns,
    /// `AttachTrafficSources`, each identifier typed `elbv2`.
    AttachTrafficSourcesTyped,
    /// `AttachTrafficSources`, with the type left out, which the identifier's own ARN states.
    AttachTrafficSourcesUntyped,
    /// The same association stated BOTH ways: the legacy member and then the traffic source.
    BothWays,
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

/// Post `AttachLoadBalancerTargetGroups` for `arns` and require it to be served.
async fn attach_target_groups(router: &MultiServiceRouter, group: &str, arns: &[String]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            &format!("TargetGroupARNs.member.{}", index + 1),
            arn.clone(),
        ));
    }
    let (status, body) =
        query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "attaching {arns:?} to {group:?} must succeed, got {status} with {body:?}"
    );
}

/// Post `AttachTrafficSources` for `arns`, typed or not, and require it to be served.
async fn attach_traffic_sources(
    router: &MultiServiceRouter,
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
    let (status, body) = query(router, "AttachTrafficSources", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "attaching {arns:?} to {group:?} as traffic sources must succeed, got {status} with \
         {body:?}"
    );
}

/// The `(Identifier, Type)` pairs the group reports under its own `TrafficSources`, in the
/// order it reports them.
async fn group_traffic_sources(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
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
    list_members(&body, "TrafficSources")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "Identifier"),
                tag_value(member, "Type"),
            )
        })
        .collect()
}

/// The `(Identifier, Type)` pairs `DescribeTrafficSources` reports for the group, in the order
/// it reports them — the dedicated read of the same set.
async fn described_traffic_sources(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    selected_type: Option<&str>,
) -> Vec<(String, String)> {
    let mut params = vec![p("AutoScalingGroupName", group)];
    if let Some(wanted) = selected_type {
        params.push(p("TrafficSourceType", wanted));
    }
    let (status, body) = query(router, "DescribeTrafficSources", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTrafficSources must succeed, got {status} with {body:?}"
    );
    list_members(&body, "TrafficSources")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "Identifier"),
                tag_value(member, "Type"),
            )
        })
        .collect()
}

/// Post `DetachTrafficSources` for `arns` and answer its status.
async fn detach_traffic_sources(
    router: &MultiServiceRouter,
    group: &str,
    arns: &[String],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, arn) in arns.iter().enumerate() {
        let prefix = format!("TrafficSources.member.{}", index + 1);
        params.push(p(&format!("{prefix}.Identifier"), arn.clone()));
        params.push(p(&format!("{prefix}.Type"), TYPE_ELBV2));
    }
    let (status, body) = query(router, "DetachTrafficSources", &borrowed(&params)).await;
    (status, body)
}

/// RULE: however an association was stated, both reads report it once as an `elbv2` traffic
/// source, and the detach of it leaves neither read reporting it.
#[tokio::test]
async fn every_target_group_association_is_one_traffic_source_in_both_reads() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, LAUNCH_TEMPLATE).await;

    // Each row: how the association is stated, and how many target groups it names.
    let rows: Vec<(StatedAs, usize)> = vec![
        (StatedAs::CreateTargetGroupArns, 1),
        (StatedAs::AttachTargetGroupArns, 1),
        (StatedAs::AttachTrafficSourcesTyped, 1),
        (StatedAs::AttachTrafficSourcesUntyped, 1),
        (StatedAs::BothWays, 1),
        (StatedAs::CreateTargetGroupArns, 2),
        (StatedAs::AttachTargetGroupArns, 2),
        (StatedAs::AttachTrafficSourcesTyped, 2),
        (StatedAs::BothWays, 2),
    ];

    for (row, (stated, count)) in rows.into_iter().enumerate() {
        let case = format!("{count} target group(s) stated as {stated:?}");
        let mut arns: Vec<String> = Vec::new();
        for index in 0..count {
            arns.push(
                create_target_group(
                    &router,
                    &format!("tg-views-{row}-{index}"),
                    TARGET_GROUP_PORT,
                    TARGET_TYPE_INSTANCE,
                    &vpc_id,
                )
                .await,
            );
        }
        let group = format!("asg-views-{row}");
        let at_create: &[String] = match stated {
            StatedAs::CreateTargetGroupArns | StatedAs::BothWays => &arns,
            _ => &[],
        };
        given_group(&router, &group, &subnets[0], at_create).await;
        match stated {
            StatedAs::CreateTargetGroupArns => {}
            StatedAs::AttachTargetGroupArns => attach_target_groups(&router, &group, &arns).await,
            StatedAs::AttachTrafficSourcesTyped => {
                attach_traffic_sources(&router, &group, &arns, true).await
            }
            StatedAs::AttachTrafficSourcesUntyped => {
                attach_traffic_sources(&router, &group, &arns, false).await
            }
            // The same association again, under its other spelling: one attachment, so one
            // entry.
            StatedAs::BothWays => attach_traffic_sources(&router, &group, &arns, true).await,
        }

        let expected: Vec<(String, String)> = arns
            .iter()
            .map(|arn| (arn.clone(), TYPE_ELBV2.to_string()))
            .collect();
        let reported = group_traffic_sources(&router, &case, &group).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the group to report {expected:?}, got {reported:?}"
        );
        let described = described_traffic_sources(&router, &case, &group, None).await;
        assert_eq!(
            described, expected,
            "case {case:?}: expected DescribeTrafficSources to report {expected:?}, got \
             {described:?}"
        );
        // The read that asks for ONE kind of traffic source reports the same entries: an
        // association of a target group is of that kind whichever member recorded it.
        let selected = described_traffic_sources(&router, &case, &group, Some(TYPE_ELBV2)).await;
        assert_eq!(
            selected, expected,
            "case {case:?}: expected the elbv2-only read to report {expected:?}, got \
             {selected:?}"
        );

        // The detach of the traffic source removes the one association, so neither read
        // reports it and the legacy member it may have been written in is empty too.
        let (status, body) = detach_traffic_sources(&router, &group, &arns).await;
        assert_eq!(
            status, 200,
            "case {case:?}: detaching {arns:?} must be served, got {status} with {body:?}"
        );
        let reported = group_traffic_sources(&router, &case, &group).await;
        let described = described_traffic_sources(&router, &case, &group, None).await;
        assert_eq!(
            (reported.as_slice(), described.as_slice()),
            ([].as_slice(), [].as_slice()),
            "case {case:?}: expected the detach to leave the group attached to nothing, got \
             {reported:?} and {described:?}"
        );
    }
}
