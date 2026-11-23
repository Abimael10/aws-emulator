// SPDX-License-Identifier: CC0-1.0

//! The group metrics a group publishes while some of its members are parked out of service.
//!
//! The rule under test: every series a group collects reads the value the GROUP'S OWN STATE
//! implies at the instant the datapoint is published, so a member parked out of service by
//! `EnterStandby` ("Moves the specified instances into the standby state") leaves the
//! in-service family, joins the standby family, and stays in the total family the group's
//! membership is counted by — while the capacity the group is converging on falls by what the
//! parked members provide exactly when the request asks for it ("Indicates whether to
//! decrement the desired capacity of the Auto Scaling group by the number of instances moved
//! to `Standby` mode", `EnterStandbyQuery$ShouldDecrementDesiredCapacity`) and otherwise
//! stands, in which case "the Auto Scaling group launches new instances to replace the
//! instances on standby" (`EnterStandby`) and the group holds more members than before.
//!
//! Two subjects:
//!
//! 1. A TABLE over the park: how many of the group's members it moves, whether the request
//!    decrements the desired capacity, and the WEIGHT each member carries — so what is pinned
//!    is how the families move with the group's state rather than one recorded set of numbers.
//!    Each row reads the standby pair, the in-service pair and the total pair at ONE minute of
//!    the series, beside the desired capacity `DescribeAutoScalingGroups` then reports.
//! 2. A disable names METRICS, not families ("Identifies the metrics to disable",
//!    `DisableMetricsCollectionQuery$Metrics`): taking one half of the standby pair out of a
//!    collection stops that series alone, while the twin it was enabled beside keeps receiving
//!    datapoints.
//!
//! The harness, the group creation and the series readers are the ones the sibling suite over
//! group-metric publication states ([`crate::group_metric_publication`]), so both suites read
//! one definition of "the series this group published".

use std::collections::BTreeMap;
use std::time::Instant;

use smithy_server_runtime::MultiServiceRouter;

use crate::group_metric_publication::{
    create_group, disable_metrics, emulator, enable_metrics, enabled_metrics, indexed_metrics,
    published_count, series, shared_minute, Point, LOOK_AGAIN, POLL_DEADLINE,
};
use crate::group_state_and_standby_transitions::{describe, instances_in_state};
use crate::support::{borrowed, p, query, tag_value};

/// The `LifecycleState` of a member in service, and of one parked out of service, as the
/// model's own `LifecycleState` enum spells them.
const IN_SERVICE: &str = "InService";
const STANDBY: &str = "Standby";

/// One row of the parking table.
struct StandbyRow {
    case: &'static str,
    /// `LaunchTemplateOverrides$WeightedCapacity` for the group's single override, or `None`
    /// for an unweighted group, whose members are worth one unit each.
    weight: Option<&'static str>,
    min_size: i32,
    max_size: i32,
    /// The capacity the group is created with, in the units its size parameters are stated in.
    desired: i32,
    /// The members that fulfil that capacity.
    members: i32,
    /// How many of them the park moves.
    parking: i32,
    /// What the request says about the group's size.
    decrement: bool,
}

/// The rows: the park's SIZE, the decrement, and the weight each vary across them, and the
/// recorded single-member park of an unweighted group is the first of them.
fn standby_table() -> Vec<StandbyRow> {
    vec![
        StandbyRow {
            case: "one of three members parked, decrementing the desired capacity",
            weight: None,
            min_size: 1,
            max_size: 4,
            desired: 3,
            members: 3,
            parking: 1,
            decrement: true,
        },
        StandbyRow {
            case: "two of four members parked, decrementing the desired capacity",
            weight: None,
            min_size: 0,
            max_size: 6,
            desired: 4,
            members: 4,
            parking: 2,
            decrement: true,
        },
        StandbyRow {
            case: "the whole group parked down to its zero minimum",
            weight: None,
            min_size: 0,
            max_size: 4,
            desired: 2,
            members: 2,
            parking: 2,
            decrement: true,
        },
        StandbyRow {
            case: "one member parked without the decrement, so the group replaces it",
            weight: None,
            min_size: 0,
            max_size: 4,
            desired: 2,
            members: 2,
            parking: 1,
            decrement: false,
        },
        StandbyRow {
            case: "a weighted group: the decrement takes the units the parked member provides",
            weight: Some("2"),
            min_size: 0,
            max_size: 6,
            desired: 4,
            members: 2,
            parking: 1,
            decrement: true,
        },
        StandbyRow {
            case: "a weighted group parked without the decrement: the replacement's units \
                   count too",
            weight: Some("2"),
            min_size: 0,
            max_size: 8,
            desired: 4,
            members: 2,
            parking: 1,
            decrement: false,
        },
    ]
}

/// Park `parking` of the group's in-service members, answering the ids it moved.
async fn enter_standby(
    router: &MultiServiceRouter,
    group: &str,
    in_service: &[String],
    parking: usize,
    decrement: bool,
    case: &str,
) -> Vec<String> {
    let moving: Vec<String> = in_service.iter().take(parking).cloned().collect();
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p(
            "ShouldDecrementDesiredCapacity",
            if decrement { "true" } else { "false" },
        ),
    ];
    for (position, instance_id) in moving.iter().enumerate() {
        params.push(p(
            &format!("InstanceIds.member.{}", position + 1),
            instance_id.clone(),
        ));
    }
    let (status, body) = query(router, "EnterStandby", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EnterStandby expected 200, got {status}: {body}"
    );
    moving
}

/// The desired capacity `DescribeAutoScalingGroups` reports for `group`.
async fn reported_desired_capacity(router: &MultiServiceRouter, group: &str) -> String {
    tag_value(&describe(router, group).await, "DesiredCapacity")
}

/// SUBJECT 1: the parked members leave the in-service family, join the standby family and stay
/// in the total — and the desired capacity moves only when the request asks it to.
#[tokio::test]
async fn parking_members_moves_them_between_the_families_the_group_publishes() {
    let router = emulator();

    for (index, row) in standby_table().into_iter().enumerate() {
        let case = row.case;
        let group = format!("asg-standby-{index}");
        create_group(
            &router,
            &group,
            row.weight,
            row.min_size,
            row.max_size,
            row.desired,
        )
        .await;

        // The weight is the only thing separating a member from the units it provides.
        let weight = row
            .weight
            .map_or(1, |weight| weight.parse().expect("a row states a number"));
        let members = usize::try_from(row.members).expect("a row holds a few members");
        let parking = usize::try_from(row.parking).expect("a row parks a few members");
        let in_service = instances_in_state(&router, &group, IN_SERVICE, members).await;
        let parked = enter_standby(&router, &group, &in_service, parking, row.decrement, case).await;

        // What the group is then converging on, and what it holds: a park that decrements
        // takes the parked units out of the capacity and leaves the remaining members to meet
        // it, while a park that does not leaves the capacity standing, so the group launches
        // one replacement per parked member and ends up holding them both.
        let (desired_after, serving, held) = if row.decrement {
            (row.desired - row.parking * weight, row.members - row.parking, row.members)
        } else {
            (row.desired, row.members, row.members + row.parking)
        };
        let serving_count = usize::try_from(serving).expect("a row serves a few members");
        instances_in_state(&router, &group, IN_SERVICE, serving_count).await;
        let still_parked = instances_in_state(&router, &group, STANDBY, parking).await;
        assert_eq!(
            still_parked, parked,
            "case {case:?}: the members parked must be exactly the ones the request named"
        );
        assert_eq!(
            reported_desired_capacity(&router, &group).await,
            desired_after.to_string(),
            "case {case:?}: expected a desired capacity of {desired_after} after the park"
        );

        // Collected AFTER the park, so every minute the series opens with reports the state
        // the group is in rather than the one it was in before. A request naming no metric
        // collects the whole catalogue, which is what the parked group is read through here.
        enable_metrics(&router, &group, &[]).await;

        let expected = [
            // The size parameters as configured: parking a member changes neither bound.
            ("GroupMinSize", f64::from(row.min_size)),
            ("GroupMaxSize", f64::from(row.max_size)),
            ("GroupDesiredCapacity", f64::from(desired_after)),
            // The members still in service, and the units they provide.
            ("GroupInServiceInstances", f64::from(serving)),
            ("GroupInServiceCapacity", f64::from(serving * weight)),
            // The parked members, and the units they would provide were they in service.
            ("GroupStandbyInstances", f64::from(row.parking)),
            ("GroupStandbyCapacity", f64::from(row.parking * weight)),
            // The group holds both sets, so the total counts them together.
            ("GroupTotalInstances", f64::from(held)),
            ("GroupTotalCapacity", f64::from(held * weight)),
            // And a group that has settled is launching nothing and terminating nothing.
            ("GroupPendingInstances", 0.0),
            ("GroupPendingCapacity", 0.0),
            ("GroupTerminatingInstances", 0.0),
            ("GroupTerminatingCapacity", 0.0),
        ];
        let stamp = shared_minute(&router, &group, case, &expected).await;
        assert_eq!(
            stamp % 60,
            0,
            "case {case:?}: a one-minute datapoint is stamped with the start of its minute, \
             got {stamp}"
        );

        // The standby pair is a reading of the same members the total pair holds and the
        // in-service pair no longer does, at that one minute.
        let standby_instances = series(&router, &group, "GroupStandbyInstances").await;
        let in_service_instances = series(&router, &group, "GroupInServiceInstances").await;
        let total_instances = series(&router, &group, "GroupTotalInstances").await;
        let at = |points: &BTreeMap<i64, Point>| points.get(&stamp).map(|point| point.maximum);
        assert_eq!(
            at(&standby_instances)
                .zip(at(&in_service_instances))
                .zip(at(&total_instances))
                .map(|((standby, in_service), total)| standby + in_service - total),
            Some(0.0),
            "case {case:?}: at {stamp} the parked and the serving members must add up to the \
             group's total, got {standby_instances:?}, {in_service_instances:?} and \
             {total_instances:?}"
        );
    }
}

/// SUBJECT 2: a disable stops the series it names and leaves its twin publishing.
///
/// The two halves of the standby pair are enabled together and only the instance half is
/// disabled, so the capacity half is a series whose GROUP, whose family and whose minute
/// lattice are the same: what separates them is the metric the disable named.
#[tokio::test]
async fn disabling_one_metric_leaves_its_twin_publishing() {
    let router = emulator();
    let group = "asg-standby-disable";
    let case = "the instance half of the standby pair is disabled";
    create_group(&router, group, Some("2"), 0, 6, 4).await;
    let in_service = instances_in_state(&router, group, IN_SERVICE, 2).await;
    enter_standby(&router, group, &in_service, 1, true, case).await;
    instances_in_state(&router, group, STANDBY, 1).await;

    let dropped = "GroupStandbyInstances";
    let twin = "GroupStandbyCapacity";
    let kept = "GroupDesiredCapacity";
    enable_metrics(&router, group, &[dropped, twin, kept]).await;
    for metric in [dropped, twin, kept] {
        published_count(&router, group, metric, case).await;
    }

    disable_metrics(&router, group, &[dropped]).await;
    let dropped_at_disable = series(&router, group, dropped).await.len();
    let mut collected = enabled_metrics(&router, group).await;
    collected.sort();
    let mut left_with = vec![kept.to_string(), twin.to_string()];
    left_with.sort();
    assert_eq!(
        collected, left_with,
        "case {case:?}: the group must collect exactly the metrics the disable left it"
    );

    // The twin and the metric beside it keep RECEIVING datapoints: each is looked at again
    // until its series has grown past the length it had when the disable was served. The
    // CONDITION, never a fixed wait — a datapoint is owed on a clock nothing here sleeps for.
    for metric in [twin, kept] {
        let before = series(&router, group, metric).await.len();
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let now = series(&router, group, metric).await.len();
            if now > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the series {metric:?} must keep publishing, it stayed at \
                 {before} datapoint(s)"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    // And across those minutes the disabled series received nothing further, while the
    // datapoints it was published with stay readable — so it stays indexed beside the two.
    assert_eq!(
        series(&router, group, dropped).await.len(),
        dropped_at_disable,
        "case {case:?}: a metric the group no longer collects must receive no further datapoint"
    );
    let mut indexed = vec![dropped.to_string(), twin.to_string(), kept.to_string()];
    indexed.sort();
    assert_eq!(
        indexed_metrics(&router, group).await,
        indexed,
        "case {case:?}: ListMetrics must index the series this group published"
    );
}
