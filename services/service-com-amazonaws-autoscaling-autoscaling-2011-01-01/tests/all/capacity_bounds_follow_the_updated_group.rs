// SPDX-License-Identifier: CC0-1.0

//! Which bounds a capacity change is weighed against, after the group's own bounds have MOVED.
//!
//! RULE: `SetDesiredCapacity` "sets the size of the specified Auto Scaling group"
//! (`smithy_query(query="operation",operation="SetDesiredCapacity")`) and a group's size is
//! bounded by the `MinSize` and `MaxSize` the group CURRENTLY holds, which
//! `UpdateAutoScalingGroup` is what moves — so every capacity change is weighed against the
//! bounds the group reads back at that moment, and a capacity outside them is refused HTTP 400
//! `ValidationError` with the one sentence this service words for that class: `New
//! SetDesiredCapacity value <capacity> is above max value <MaxSize> for the AutoScalingGroup.`
//! (or `below min value <MinSize>`). Its only variable parts are the capacity the request named
//! and the bound of the group it crossed; the sentence ENDS at the period after
//! `AutoScalingGroup`, so it carries neither the group's name nor any other value, and a
//! refused change leaves the capacity the group was holding exactly where it was.
//!
//! The table spans what the rule generalises over: WHICH bound moved and WHICH WAY (a maximum
//! lowered, lowered twice, lowered and raised back, raised above the one the group was created
//! with; a minimum raised and lowered), where the capacity sits relative to the bound the group
//! now holds (one past it, several past it, at the bound itself), and the group's own name —
//! which no refusal carries, however long or however punctuated it is. The bound each
//! assertion expects is the one the group READS BACK after its updates, never a value written
//! out beside them, and every row is driven end to end over the awsQuery wire.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, emulator, p, query, seed_subnets, tag_value, xml_unescape,
    SeededSubnet,
};

/// The error code every argument refusal of this service answers with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The fixed text of the capacity-bound refusal, with the capacity the request named and the
/// bound of the group it crossed interpolated — and nothing else.
///
/// Written out here rather than taken from the service's own builder: the sentence is what a
/// client classifies the refusal by, so the assertion states the wording itself.
fn expected_sentence(capacity: i32, relation: &str, bound: &str) -> String {
    format!(
        "New SetDesiredCapacity value {capacity} is {relation} value {bound} for the \
         AutoScalingGroup."
    )
}

/// Which bound of the group the capacity crossed, as the sentence words it.
const ABOVE_MAX: &str = "above max";
const BELOW_MIN: &str = "below min";

/// What the service does with the capacity a row asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The group's current bounds admit it, so the change is served.
    Served,
    /// They do not, so it is refused against the bound named — whose VALUE comes from the group
    /// the row has just updated.
    Refused(&'static str),
}

/// One row: the group, the bounds it is created with, the updates applied to it in order, the
/// capacity the change then asks for, and what the service does with it.
struct Row {
    case: &'static str,
    group: &'static str,
    /// `(MinSize, MaxSize, DesiredCapacity)` the group is created with.
    created: (i32, i32, i32),
    /// The `(member, value)` pairs each `UpdateAutoScalingGroup` carries, applied in order.
    updates: &'static [(&'static str, i32)],
    capacity: i32,
    expect: Expect,
}

/// The three sizes one group reads back as, read over the awsQuery wire where a client reads
/// them.
async fn sizes(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String, String) {
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
    (
        tag_value(&body, "MinSize"),
        tag_value(&body, "DesiredCapacity"),
        tag_value(&body, "MaxSize"),
    )
}

/// Create one group placed over `subnets`, with the bounds the row states.
async fn create_group(
    router: &MultiServiceRouter,
    row: &Row,
    template: &str,
    subnets: &[SeededSubnet],
) {
    let case = row.case;
    let (min_size, max_size, desired_capacity) = row.created;
    let placement: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", row.group),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", min_size.to_string()),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired_capacity.to_string()),
        p("VPCZoneIdentifier", placement.join(",")),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {:?} expected 200, got {status}: {body}",
        row.group
    );
}

/// Apply one `UpdateAutoScalingGroup` moving the single bound `member` to `value`.
async fn update_bound(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    member: &str,
    value: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(member, value.to_string()),
    ];
    let (status, body) = query(router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateAutoScalingGroup({member}={value}) for {name:?} expected 200, got \
         {status}: {body}"
    );
}

/// The `(code, message)` of a refused awsQuery answer.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// RULE: a capacity change is weighed against the bounds the group holds AT THAT MOMENT, so the
/// refusal and the acceptance both follow an update that moved one — and the refusal is the one
/// sentence for the bound crossed, carrying the requested capacity and that bound and nothing
/// else.
#[tokio::test]
async fn a_capacity_change_follows_the_bounds_the_group_now_holds() {
    let rows = [
        Row {
            case: "the maximum lowered below a capacity it once admitted",
            group: "bound-moved-lowered-max",
            created: (0, 3, 0),
            updates: &[("MaxSize", 2)],
            capacity: 3,
            expect: Expect::Refused(ABOVE_MAX),
        },
        Row {
            case: "the maximum lowered twice, the capacity well past it",
            group: "bound-moved-twice",
            created: (0, 5, 0),
            updates: &[("MaxSize", 3), ("MaxSize", 1)],
            capacity: 4,
            expect: Expect::Refused(ABOVE_MAX),
        },
        Row {
            case: "the capacity at the lowered maximum itself",
            group: "bound-moved-at-max",
            created: (0, 3, 0),
            updates: &[("MaxSize", 2)],
            capacity: 2,
            expect: Expect::Served,
        },
        Row {
            case: "the maximum lowered and raised back",
            group: "bound-moved-raised-back",
            created: (0, 3, 0),
            updates: &[("MaxSize", 2), ("MaxSize", 3)],
            capacity: 3,
            expect: Expect::Served,
        },
        Row {
            case: "the maximum raised above the one the group was created with",
            group: "bound-moved-raised-max",
            created: (0, 1, 0),
            updates: &[("MaxSize", 4)],
            capacity: 3,
            expect: Expect::Served,
        },
        Row {
            case: "the minimum raised above a capacity it once admitted",
            group: "bound-moved-raised-min",
            created: (0, 3, 2),
            updates: &[("MinSize", 2)],
            capacity: 1,
            expect: Expect::Refused(BELOW_MIN),
        },
        Row {
            case: "the minimum lowered to admit a smaller capacity",
            group: "bound-moved-lowered-min",
            created: (2, 4, 2),
            updates: &[("MinSize", 0)],
            capacity: 0,
            expect: Expect::Served,
        },
        Row {
            case: "a group whose name is long and punctuated, which the sentence never carries",
            group: "bound.moved-a-group-whose-name-is-considerably-longer-than-the-others",
            created: (0, 2, 0),
            updates: &[("MaxSize", 1)],
            capacity: 2,
            expect: Expect::Refused(ABOVE_MAX),
        },
    ];

    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = "lt-bound-moved";
    create_launch_template(&router, template).await;

    for row in &rows {
        let case = row.case;
        create_group(&router, row, template, &subnets).await;
        for (member, value) in row.updates {
            update_bound(&router, case, row.group, member, *value).await;
        }

        // The bounds the group NOW holds, which are what the change is weighed against — read
        // back from the group rather than written out beside the row.
        let (min_size, before, max_size) = sizes(&router, case, row.group).await;
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", row.group),
                ("DesiredCapacity", row.capacity.to_string().as_str()),
            ],
        )
        .await;

        match row.expect {
            Expect::Served => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the bounds {min_size}..{max_size} admit {}, so the change \
                     must be served, got {status}: {body}",
                    row.capacity
                );
                let (_, after, _) = sizes(&router, case, row.group).await;
                assert_eq!(
                    after,
                    row.capacity.to_string(),
                    "case {case:?}: expected {:?} to hold the capacity it was set to, got \
                     {after}",
                    row.group
                );
            }
            Expect::Refused(relation) => {
                let bound = if relation == ABOVE_MAX {
                    max_size.clone()
                } else {
                    min_size.clone()
                };
                assert_eq!(
                    status, 400,
                    "case {case:?}: the bounds {min_size}..{max_size} refuse {}, got {status}: \
                     {body}",
                    row.capacity
                );
                assert_eq!(
                    refusal(&body),
                    (
                        VALIDATION_ERROR.to_string(),
                        expected_sentence(row.capacity, relation, &bound)
                    ),
                    "case {case:?}: expected the capacity-bound sentence against the bound the \
                     group now holds, got {body}"
                );
                assert!(
                    !xml_unescape(&tag_value(&body, "Message")).contains(row.group),
                    "case {case:?}: the sentence names no group, got {body}"
                );
                // A refused change writes nothing: the group keeps the capacity it held.
                let (_, after, _) = sizes(&router, case, row.group).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: expected {:?} to keep the capacity it was holding \
                     ({before}), got {after}",
                    row.group
                );
            }
        }
    }
}
