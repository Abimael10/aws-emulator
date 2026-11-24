// SPDX-License-Identifier: CC0-1.0

//! The per-group ceiling on the instances a WARM POOL prepares when it follows the group's
//! maximum — driven end to end over the wire this service speaks (`awsQuery`).
//!
//! RULE: a group's `MaxSize` is headroom and holds no instance of its own, EXCEPT where the
//! group holds a warm pool that names no prepared capacity: then "Amazon EC2 Auto Scaling
//! launches and maintains the difference between the group's maximum capacity and its desired
//! capacity" (`PutWarmPoolType$MaxGroupPreparedCapacity`), so the maximum IS a number of
//! instances. That number is held to the one ceiling this process holds a group to, by every
//! call that sets it: `PutWarmPool` refuses a pool size past it, and so does the update that
//! raises the group's maximum — with the same sentence, before the bound is applied, so a
//! refused update leaves the group and its pool exactly as they were.
//!
//! The rows differ along what decides the number of instances the maximum would buy: a pool
//! that follows the maximum, a pool that names its own prepared capacity (so the maximum buys
//! nothing), and a group with no pool at all (the plain headroom case, which must stay
//! served). Each row takes a maximum far past the ceiling, and the ceiling itself is read
//! from the service rather than written out here.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_value, xml_unescape};

/// The launch template the groups here launch from, seeded by the harness in the account's
/// own Amazon EC2 store.
const TEMPLATE: &str = "lt-1";

/// The maximum each row raises the group to: far past any per-group ceiling, so a pool that
/// followed it would prepare more instances than one group may hold.
const HUGE_MAXIMUM: i32 = 999_999_999;

/// One row: the case, the `MaxGroupPreparedCapacity` the group's warm pool names (`None` for
/// a pool that follows the group's maximum, and no pool at all when the group has none), and
/// whether the update that raises the maximum is served.
struct Row {
    case: &'static str,
    /// `Some(prepared)` puts a warm pool on the group; `None` leaves it without one.
    pool: Option<Option<i32>>,
    served: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group with no warm pool: the maximum is plain headroom",
        pool: None,
        served: true,
    },
    Row {
        case: "a warm pool naming its own prepared capacity: the maximum buys no instance",
        pool: Some(Some(2)),
        served: true,
    },
    Row {
        case: "a warm pool following the group's maximum",
        pool: Some(None),
        served: false,
    },
];

/// The `<Message>` of an answered refusal, decoded the way a client reads it.
fn message(body: &str) -> String {
    xml_unescape(&tag_value(body, "Message"))
}

/// The sentence a request whose warm pool would hold more instances than one group may is
/// refused with, spelled out here rather than imported so a wrong constant in the
/// implementation cannot make this suite pass.
fn too_large_warm_pool_message(size: i32) -> String {
    format!(
        "The warm pool size {size} exceeds the limit of {} instances per Auto Scaling group",
        autoscaling::state::MAX_GROUP_CAPACITY
    )
}

/// The group's `MaxSize` as `DescribeAutoScalingGroups` reports it.
async fn reported_maximum(router: &MultiServiceRouter, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups must answer 200, got {status}: {body}"
    );
    tag_value(&body, "MaxSize")
}

/// RULE: the maximum an update leaves a group with is held to the per-group ceiling through
/// the instances its warm pool would then prepare — and nowhere else, so plain headroom stays
/// served.
#[tokio::test]
async fn a_raised_maximum_is_held_to_the_ceiling_its_warm_pool_would_prepare() {
    for (index, row) in ROWS.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let group = format!("asg-warm-ceiling-{index}");
        create_group(&router, &group, &subnets[0].id).await;

        if let Some(prepared) = row.pool {
            let mut params = vec![p("AutoScalingGroupName", group.clone())];
            if let Some(prepared) = prepared {
                params.push(p("MaxGroupPreparedCapacity", prepared.to_string()));
            }
            let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {:?}: PutWarmPool must answer 200, got {status}: {body}",
                row.case
            );
        }

        let (status, body) = query(
            &router,
            "UpdateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("MaxSize", &HUGE_MAXIMUM.to_string()),
            ],
        )
        .await;
        let reported = reported_maximum(&router, &group).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {:?}: expected the update to be served, got {status}: {body}",
                row.case
            );
            assert_eq!(
                reported,
                HUGE_MAXIMUM.to_string(),
                "case {:?}: the maximum must round-trip verbatim, got {reported:?}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: expected the update to be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {:?}: expected ValidationError, got {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            too_large_warm_pool_message(HUGE_MAXIMUM),
            "case {:?}: expected the sentence for a warm pool past the ceiling",
            row.case
        );
        assert_eq!(
            reported, "3",
            "case {:?}: the refused update must leave the group's maximum alone, got \
             {reported:?}",
            row.case
        );
    }
}

/// A group of `subnet` that runs nothing and may grow to three instances.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", "3"),
        p("DesiredCapacity", "0"),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} must answer 200, got {status}: {body}"
    );
}
