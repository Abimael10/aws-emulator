// SPDX-License-Identifier: CC0-1.0

//! A capacity the group's bounds do not admit moves NO instance.
//!
//! RULE under test: `SetDesiredCapacity` "sets the size of the specified Auto Scaling
//! group" and the group then "launches or terminates instances to achieve the new capacity"
//! (`smithy_query(query="operation",operation="SetDesiredCapacity")`), while a group's size
//! is held inside the bounds it is configured with — so a capacity outside those bounds is
//! REFUSED, and a refused call is not half carried out: the group keeps the capacity it
//! had, and the instances it holds are exactly the instances it held before, neither
//! launched into nor taken out of. A capacity the bounds DO admit is served and moves the
//! instances it asks for.
//!
//! The rows span what that rule generalises over: which bound the capacity crosses (above
//! the maximum, below the minimum), how far past it the request reaches, which direction
//! the capacity a request CAN be served asks the group to move (up, down, and to the bound
//! itself), and the bounds the group is configured with.
//!
//! Nothing here waits to see that a refusal did nothing: after each refusal the test makes
//! a change the bounds DO admit and waits for THAT to be observable, so an instance a
//! refusal had launched or taken out would be counted when the barrier change settles.
//! Auto Scaling and Amazon EC2 are mounted on one engine, so the instances counted are the
//! account's own.

use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_value, tag_values,
    xml_unescape};

/// The launch source every group here launches from, created over Amazon EC2's own wire
/// because an instance is only launched from a template that can produce one.
const TEMPLATE: &str = "lt-capacity-intent";

/// The `LifecycleState` of an instance that is serving, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum does.
const IN_SERVICE: &str = "InService";

/// The wire error code a refused capacity carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// How long a wait waits for what it is waiting on, and how often it looks: driven off what
/// the group REPORTS rather than off a fixed sleep, and far above the work one capacity
/// change makes, so a loaded machine delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

/// Which bound the requested capacity crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crossed {
    /// Above the group's `MaxSize`.
    Max,
    /// Below its `MinSize`.
    Min,
}

impl Crossed {
    /// The whole sentence the service refuses that capacity with.
    ///
    /// Written out here rather than taken from the implementation — a test that re-used the
    /// service's own literal would assert nothing — as fixed text around the capacity the
    /// request asked for and the bound it crossed.
    fn sentence(self, requested: i32, bound: i32) -> String {
        let relation = match self {
            Self::Max => format!("above max value {bound}"),
            Self::Min => format!("below min value {bound}"),
        };
        format!(
            "New SetDesiredCapacity value {requested} is {relation} for the AutoScalingGroup."
        )
    }
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The bounds the group is configured with, and the capacity it is created at.
    min: i32,
    max: i32,
    created: i32,
    /// The capacity the row's own `SetDesiredCapacity` asks for.
    requested: i32,
    /// The bound it crosses, or `None` for a capacity the bounds admit.
    crossed: Option<Crossed>,
    /// The capacity asked for after a refusal, which the bounds DO admit: the barrier the
    /// assertions are made against. Ignored by a row that is not refused.
    barrier: i32,
}

const ROWS: &[Row] = &[
    Row {
        case: "one past the maximum",
        min: 0,
        max: 2,
        created: 1,
        requested: 3,
        crossed: Some(Crossed::Max),
        barrier: 2,
    },
    Row {
        case: "far past the maximum",
        min: 0,
        max: 2,
        created: 1,
        requested: 9,
        crossed: Some(Crossed::Max),
        barrier: 2,
    },
    Row {
        case: "below a minimum of one",
        min: 1,
        max: 3,
        created: 2,
        requested: 0,
        crossed: Some(Crossed::Min),
        barrier: 3,
    },
    Row {
        case: "below a minimum of two, which the group is sitting on",
        min: 2,
        max: 4,
        created: 2,
        requested: 1,
        crossed: Some(Crossed::Min),
        barrier: 4,
    },
    Row {
        case: "the maximum itself, which the group serves",
        min: 0,
        max: 2,
        created: 1,
        requested: 2,
        crossed: None,
        barrier: 2,
    },
    Row {
        case: "the minimum itself, which the group serves",
        min: 1,
        max: 3,
        created: 3,
        requested: 1,
        crossed: None,
        barrier: 1,
    },
];

/// The instances one group reports, each with the lifecycle state it is in.
async fn group_members(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
        .into_iter()
        .zip(tag_values(&body, "LifecycleState"))
        .collect()
}

/// The `DesiredCapacity` one group reports.
async fn reported_capacity(router: &MultiServiceRouter, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
    );
    tag_value(&body, "DesiredCapacity")
}

/// Look again until the group reports exactly `wanted` instances in service, and answer
/// their ids.
async fn poll_serving(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let members = group_members(router, group).await;
        let serving: Vec<String> = members
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .map(|(instance_id, _)| instance_id.clone())
            .collect();
        if serving.len() == wanted && members.len() == wanted {
            return serving;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected {group} to serve {wanted} instance(s), last saw {members:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// `SetDesiredCapacity` on one group, answering the status and body.
async fn set_capacity(
    router: &MultiServiceRouter,
    group: &str,
    desired: i32,
) -> (u16, String) {
    query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await
}

/// Every row: a capacity outside the bounds is refused and moves no instance, while one
/// inside them is served and moves exactly the instances it asks for.
async fn the_capacity_intent_rule_holds_for(index: usize, row: &Row) {
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    crate::support::create_launch_template(&router, TEMPLATE).await;
    let group = format!("asg-capacity-intent-{index}");

    let params = vec![
        p("AutoScalingGroupName", group.as_str()),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", row.min.to_string()),
        p("MaxSize", row.max.to_string()),
        p("DesiredCapacity", row.created.to_string()),
        p("VPCZoneIdentifier", subnet),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
        row.case
    );
    let held = usize::try_from(row.created).expect("a capacity is a count");
    let before = poll_serving(&router, row.case, &group, held).await;

    let (status, body) = set_capacity(&router, &group, row.requested).await;
    let Some(crossed) = row.crossed else {
        // A capacity the bounds admit is served, and the group moves to it: the instances it
        // then serves are the ones it kept plus the ones it launched, or the ones it kept
        // alone.
        assert_eq!(
            status, 200,
            "case {:?}: a capacity the bounds admit must be served, got {status}: {body}",
            row.case
        );
        let wanted = usize::try_from(row.requested).expect("a capacity is a count");
        let after = poll_serving(&router, row.case, &group, wanted).await;
        let kept = after.iter().filter(|id| before.contains(id)).count();
        assert_eq!(
            kept,
            wanted.min(held),
            "case {:?}: the group keeps the instances it does not have to move, got \
             {after:?} after {before:?}",
            row.case
        );
        return;
    };

    assert_eq!(
        status, 400,
        "case {:?}: a capacity outside the bounds must be refused, got {status}: {body}",
        row.case
    );
    let bound = match crossed {
        Crossed::Max => row.max,
        Crossed::Min => row.min,
    };
    assert_eq!(
        (
            tag_value(&body, "Code"),
            xml_unescape(&tag_value(&body, "Message"))
        ),
        (
            VALIDATION_ERROR.to_string(),
            crossed.sentence(row.requested, bound)
        ),
        "case {:?}: expected the refusal for {} against {bound}, got {body}",
        row.case,
        row.requested
    );

    // The group keeps the capacity it had, and the instances it holds are the instances it
    // held: the read that follows the refusal sees not one id more and not one fewer.
    let capacity = reported_capacity(&router, &group).await;
    assert_eq!(
        capacity,
        row.created.to_string(),
        "case {:?}: a refused capacity leaves the group holding the one it had, got \
         {capacity}",
        row.case
    );
    let members = group_members(&router, &group).await;
    let still_serving: Vec<String> = members
        .iter()
        .filter(|(_, state)| state == IN_SERVICE)
        .map(|(instance_id, _)| instance_id.clone())
        .collect();
    assert_eq!(
        still_serving, before,
        "case {:?}: a refused capacity moves no instance, got {members:?} after {before:?}",
        row.case
    );

    // And nothing was left OWED either: a capacity the bounds do admit is served next and
    // the group converges on exactly that many instances — so an instance the refusal had
    // launched, or one it had taken out, would be counted here.
    let (status, body) = set_capacity(&router, &group, row.barrier).await;
    assert_eq!(
        status, 200,
        "case {:?}: the capacity {} the bounds admit must be served, got {status}: {body}",
        row.case, row.barrier
    );
    let wanted = usize::try_from(row.barrier).expect("a capacity is a count");
    let after = poll_serving(&router, row.case, &group, wanted).await;
    let kept = after.iter().filter(|id| before.contains(id)).count();
    assert_eq!(
        kept,
        wanted.min(held),
        "case {:?}: the instances the refusal left alone are the ones the group goes on \
         serving, got {after:?} after {before:?}",
        row.case
    );
}

/// One test per row, so the rows run beside each other.
#[tokio::test]
async fn one_past_the_maximum_is_refused_and_moves_nothing() {
    the_capacity_intent_rule_holds_for(0, &ROWS[0]).await;
}

#[tokio::test]
async fn far_past_the_maximum_is_refused_and_moves_nothing() {
    the_capacity_intent_rule_holds_for(1, &ROWS[1]).await;
}

#[tokio::test]
async fn below_a_minimum_of_one_is_refused_and_moves_nothing() {
    the_capacity_intent_rule_holds_for(2, &ROWS[2]).await;
}

#[tokio::test]
async fn below_the_minimum_the_group_sits_on_is_refused_and_moves_nothing() {
    the_capacity_intent_rule_holds_for(3, &ROWS[3]).await;
}

#[tokio::test]
async fn the_maximum_itself_is_served_and_launches_what_it_asks_for() {
    the_capacity_intent_rule_holds_for(4, &ROWS[4]).await;
}

#[tokio::test]
async fn the_minimum_itself_is_served_and_takes_out_what_it_asks_for() {
    the_capacity_intent_rule_holds_for(5, &ROWS[5]).await;
}
