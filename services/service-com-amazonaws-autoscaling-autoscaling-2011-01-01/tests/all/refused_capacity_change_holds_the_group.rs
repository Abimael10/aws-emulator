// SPDX-License-Identifier: CC0-1.0

//! What a capacity change the service REFUSES does to the group: nothing, for as long as a
//! client watches.
//!
//! THE RULE: `SetDesiredCapacity` either changes a group's size or is refused, and a refused
//! one starts no scaling activity at all — so the group goes on reporting the capacity it was
//! configured for and the very instances it already held, every one of them in service, and
//! no activity is appended to the log a client reads the group's scaling history off. Nothing
//! was started, so nothing falls due later either: the group is in the same state one span,
//! one launch-retry instant and any number of reads after the refusal as it was before it,
//! and a capacity inside the group's own bounds is still served afterwards.
//!
//! The rows span what the rule generalises over — WHICH refusal the request earned (below the
//! group's minimum, above its maximum, or not a capacity at all), how far outside the bound it
//! reaches, the bounds the group is configured with, and how many instances it holds while the
//! refusal is answered. Each row's refusal sentence is written out from that row's own numbers,
//! so a message built from some other request's values fails the row rather than passing it.
//!
//! The sibling suites own the OTHER half of each of those facts:
//! `capacity_sign_and_headroom_bounds` and `set_desired_capacity_on_subnet_placed_groups` own
//! the refusal wording and status, and `owed_capacity_convergence` owns the capacity a SERVED
//! change could not place. What is under test here is the group a refusal leaves behind,
//! watched over a window rather than read once.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, emulator, p, query, seed_subnets, tag_value, tag_values,
};

/// How long a row watches the group the refusal left behind.
///
/// Long enough to cover every instant this service owes a group of its own accord: the span a
/// lifecycle move takes to settle ([`autoscaling::state::LIFECYCLE_TRANSITION`], several times
/// over) and the instant at which a group that is owed capacity attempts the launch again
/// ([`autoscaling::state::WITHDRAWN_LAUNCH_RETRY`]). A refusal starts neither, so a group that
/// moves inside this window moved for no reason a client asked for. The window is a watch, not
/// a wait: the row looks again until it has passed, so machine load decides how many reads it
/// makes rather than whether it passes.
fn watch_window() -> Duration {
    autoscaling::state::WITHDRAWN_LAUNCH_RETRY + autoscaling::state::LIFECYCLE_TRANSITION * 4
}

/// How long a row waits for the instances the group was CONFIGURED for before it asks for
/// anything else, and how often it looks: far above the spans this service owes them, because
/// the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// Which refusal a row's capacity earns, and the sentence the service states it in — built
/// from the row's own numbers rather than read off the crate, so an implementation that worded
/// it with another request's values fails the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The capacity is below the group's `MinSize`.
    BelowMinimum,
    /// It is above its `MaxSize`.
    AboveMaximum,
    /// It is not a count of instances at all.
    Negative,
}

impl Refusal {
    /// The whole `Message` the refusal carries, for a group bounded by `min_size`/`max_size`
    /// and a request asking for `asked`.
    fn message(self, asked: i32, min_size: i32, max_size: i32) -> String {
        match self {
            Self::BelowMinimum => format!(
                "New SetDesiredCapacity value {asked} is below min value {min_size} for the \
                 AutoScalingGroup."
            ),
            Self::AboveMaximum => format!(
                "New SetDesiredCapacity value {asked} is above max value {max_size} for the \
                 AutoScalingGroup."
            ),
            Self::Negative => format!("New SetDesiredCapacity value {asked} is negative."),
        }
    }
}

/// One row of the refusal table.
struct Row {
    case: &'static str,
    /// The bounds the group is configured with.
    min_size: i32,
    max_size: i32,
    /// The capacity it holds — and goes on holding — while the refusal is answered.
    held: i32,
    /// The capacity the refused request asks for.
    asked: i32,
    refusal: Refusal,
}

/// ROW 1: one below the group's minimum, with the group holding its minimum.
#[tokio::test]
async fn a_capacity_one_below_the_minimum_leaves_the_group_as_it_was() {
    refused_change_holds_the_group(
        1,
        Row {
            case: "one below the group's minimum",
            min_size: 1,
            max_size: 3,
            held: 1,
            asked: 0,
            refusal: Refusal::BelowMinimum,
        },
    )
    .await;
}

/// ROW 2: several below a HIGHER minimum, with the group holding more than that minimum — so
/// the refusal is about the bound rather than about the instances the group happens to hold.
#[tokio::test]
async fn a_capacity_well_below_a_higher_minimum_leaves_the_group_as_it_was() {
    refused_change_holds_the_group(
        2,
        Row {
            case: "two below a minimum of two, holding three",
            min_size: 2,
            max_size: 4,
            held: 3,
            asked: 1,
            refusal: Refusal::BelowMinimum,
        },
    )
    .await;
}

/// ROW 3: one above the group's maximum, with the group holding that maximum.
#[tokio::test]
async fn a_capacity_one_above_the_maximum_leaves_the_group_as_it_was() {
    refused_change_holds_the_group(
        3,
        Row {
            case: "one above the group's maximum",
            min_size: 1,
            max_size: 2,
            held: 2,
            asked: 3,
            refusal: Refusal::AboveMaximum,
        },
    )
    .await;
}

/// ROW 4: far above the maximum, from a group holding one instance under it.
#[tokio::test]
async fn a_capacity_far_above_the_maximum_leaves_the_group_as_it_was() {
    refused_change_holds_the_group(
        4,
        Row {
            case: "five above the group's maximum",
            min_size: 0,
            max_size: 2,
            held: 1,
            asked: 7,
            refusal: Refusal::AboveMaximum,
        },
    )
    .await;
}

/// ROW 5: a capacity that is no count at all, refused for its sign rather than against either
/// bound — a group whose minimum is zero has no minimum for it to be below.
#[tokio::test]
async fn a_negative_capacity_leaves_the_group_as_it_was() {
    refused_change_holds_the_group(
        5,
        Row {
            case: "a negative capacity",
            min_size: 0,
            max_size: 3,
            held: 2,
            asked: -1,
            refusal: Refusal::Negative,
        },
    )
    .await;
}

/// Drive one row: the group holds what it was configured for, the refusal is answered, the
/// group is unmoved for the whole watch window, and a capacity inside its bounds is still
/// served.
async fn refused_change_holds_the_group(index: usize, row: Row) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let template = format!("lt-refused-{index}");
    create_launch_template(&router, &template).await;

    let group = format!("asg-refused-{index}");
    let params = vec![
        p("AutoScalingGroupName", &group),
        p("LaunchTemplate.LaunchTemplateName", &template),
        p("MinSize", row.min_size.to_string()),
        p("MaxSize", row.max_size.to_string()),
        p("DesiredCapacity", row.held.to_string()),
        p("VPCZoneIdentifier", subnets[0].id.clone()),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be created, got {status} with {body:?}"
    );
    let before = settled(&router, case, &group, row.held).await;
    let activities_before = activity_count(&router, case, &group).await;

    // The refusal: the status, the code and the whole sentence, built from this row's numbers.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group.as_str()),
            ("DesiredCapacity", row.asked.to_string().as_str()),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the capacity {} must be refused, got {status} with {body:?}",
        row.asked
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "ValidationError",
        "case {case:?}: expected a ValidationError, got {body:?}"
    );
    let wanted = row.refusal.message(row.asked, row.min_size, row.max_size);
    assert_eq!(
        tag_value(&body, "Message"),
        wanted,
        "case {case:?}: expected the message {wanted:?}, got {body:?}"
    );

    // And the whole of what the refusal did: nothing, for as long as the group is watched.
    let until = Instant::now() + watch_window();
    while Instant::now() < until {
        let (held, states, capacity) = group_state(&router, case, &group).await;
        assert_eq!(
            held, before,
            "case {case:?}: a refused capacity change must leave the group's instances as they \
             were ({before:?}), got {held:?}"
        );
        assert!(
            states.iter().all(|state| state == "InService"),
            "case {case:?}: every instance must stay in service, got {states:?}"
        );
        assert_eq!(
            capacity,
            row.held.to_string(),
            "case {case:?}: the group must keep the capacity it was configured for ({}), got \
             {capacity:?}",
            row.held
        );
        let activities = activity_count(&router, case, &group).await;
        assert_eq!(
            activities, activities_before,
            "case {case:?}: a refused capacity change must record no scaling activity, got \
             {activities} where the group had {activities_before}"
        );
        tokio::time::sleep(POLL).await;
    }

    // The refusal did not wedge the group either: a capacity inside its own bounds is served
    // and converged on.
    let inside = if row.held < row.max_size {
        row.held + 1
    } else {
        row.held - 1
    };
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group.as_str()),
            ("DesiredCapacity", inside.to_string().as_str()),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the capacity {inside} is inside the group's bounds and must be served, \
         got {status} with {body:?}"
    );
    let after = settled(&router, case, &group, inside).await;
    assert_eq!(
        after.len(),
        usize::try_from(inside).expect("a capacity inside the group's bounds is not negative"),
        "case {case:?}: the served change must leave the group holding {inside} instance(s), got \
         {after:?}"
    );
}

/// The instances the group reports, their lifecycle states, and the `DesiredCapacity` it
/// reports.
async fn group_state(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> (Vec<String>, Vec<String>, String) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be readable, got {status} with {body:?}"
    );
    (
        tag_values(&body, "InstanceId"),
        tag_values(&body, "LifecycleState"),
        tag_value(&body, "DesiredCapacity"),
    )
}

/// How many scaling activities the group's log holds.
async fn activity_count(router: &MultiServiceRouter, case: &str, group: &str) -> usize {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the activities of {group:?} must be readable, got {status} with {body:?}"
    );
    tag_values(&body, "ActivityId").len()
}

/// Look at the group until it holds `count` instances, every one of them `InService`, or
/// [`DEADLINE`] passes; answer their ids in the order it reports them.
async fn settled(router: &MultiServiceRouter, case: &str, group: &str, count: i32) -> Vec<String> {
    let wanted = usize::try_from(count).expect("a configured capacity is not negative");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (ids, states, _) = group_state(router, case, group).await;
        if ids.len() == wanted && states.iter().all(|state| state == "InService") {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must hold {wanted} instance(s) in service, got {ids:?} in \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}
