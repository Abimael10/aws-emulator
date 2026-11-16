// SPDX-License-Identifier: CC0-1.0

//! What `EnterStandby` moves is the SET of instances its request names.
//!
//! The rule under test: `EnterStandby` parks exactly the DISTINCT instances its request
//! names — up to the 20 one call may name
//! (`smithy_query(query="doc",shape="EnterStandbyQuery",member="InstanceIds")`: "The IDs of
//! the instances. You can specify up to 20 instances.") — records one scaling activity of
//! the group per instance it parked and answers with those activities, leaves the group's
//! desired capacity decremented by that same count when the request asked for the
//! decrement and standing (so a replacement is launched for each parked instance) when it
//! did not, and moves nothing at all when the request names no instance. A request outside
//! that rule — more ids than one call may name, or an id of no instance of the group — is
//! refused and moves nothing.
//!
//! The tables below span what that rule generalises over: how many ids one call carries
//! (none, one, several, the published bound of 20, and one past it), whether an id is
//! REPEATED — a repeated id names one instance, so it is one move and one activity — whether
//! the group holds the instance named at all, and both settings of
//! `ShouldDecrementDesiredCapacity`. The activities each accepted row is answered with are
//! then read back through `DescribeScalingActivities`, so the answer is the group's own log
//! rather than a projection made for the call.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

// The group reader and the poll-until-the-condition helper of the suite that owns the standby
// transition's own table, and the refusal wording every instance-scoped write of this service
// answers an id the group does not hold with: one copy of each, reached from here.
use crate::group_state_and_standby_transitions::{describe, instances_in_state};
use crate::instance_scale_in_protection::not_in_group_message;
use crate::support::{borrowed, emulator, p, query, tag_value, tag_values, xml_unescape, REGION};

/// The launch template the groups here launch their instances from, one the account's own
/// Amazon EC2 store holds.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The largest capacity any group in this suite is given room for: the bound on how many
/// instances ONE `EnterStandby` call may name, so the row at that bound has an instance
/// for every id it carries.
///
/// SOURCE: `EnterStandbyQuery$InstanceIds` — "You can specify up to 20 instances."
const MAX_INSTANCE_IDS_PER_CALL: usize = 20;

/// Create one group holding `capacity` instances in the first Availability Zone Amazon
/// EC2's own catalog gives the Region.
async fn create_group(router: &MultiServiceRouter, case: &str, name: &str, capacity: usize) {
    let zones = ec2::availability_zones_in_region(REGION);
    assert!(
        !zones.is_empty(),
        "case {case:?}: the Region {REGION} must have an Availability Zone to place instances in"
    );
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", MAX_INSTANCE_IDS_PER_CALL.to_string()),
        p("DesiredCapacity", capacity.to_string()),
        p("AvailabilityZones.member.1", zones[0].zone_name.clone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// The scaling activities of one group, as a client reads them.
async fn group_activities(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
    );
    body
}

/// One row of the instance-set table.
struct Row {
    case: &'static str,
    /// How many instances the group holds before the call.
    capacity: usize,
    /// Which of those instances the request names, by position in the group's own
    /// in-service list — a position REPEATED here names the same instance twice in one
    /// request.
    names: &'static [usize],
    /// Whether the request asks for the desired capacity to be decremented.
    decrement: bool,
}

/// Every row of the table: the ids one call carries, and both settings of the decrement.
fn rows() -> Vec<Row> {
    // The row at the published bound names one id per instance the group holds, so it is
    // built from the bound itself rather than written out.
    static AT_THE_BOUND: [usize; MAX_INSTANCE_IDS_PER_CALL] = {
        let mut positions = [0usize; MAX_INSTANCE_IDS_PER_CALL];
        let mut position = 0;
        while position < MAX_INSTANCE_IDS_PER_CALL {
            positions[position] = position;
            position += 1;
        }
        positions
    };

    vec![
        Row {
            case: "no instance named: nothing moves",
            capacity: 2,
            names: &[],
            decrement: true,
        },
        Row {
            case: "one instance, decrementing",
            capacity: 2,
            names: &[0],
            decrement: true,
        },
        Row {
            case: "one instance named three times: one move",
            capacity: 2,
            names: &[0, 0, 0],
            decrement: true,
        },
        Row {
            case: "two instances, each named twice, not decrementing",
            capacity: 3,
            names: &[0, 1, 0, 1],
            decrement: false,
        },
        Row {
            case: "the whole group, not decrementing",
            capacity: 2,
            names: &[0, 1],
            decrement: false,
        },
        Row {
            case: "the 20 ids one call may name",
            capacity: MAX_INSTANCE_IDS_PER_CALL,
            names: &AT_THE_BOUND,
            decrement: true,
        },
    ]
}

/// The distinct positions of `names`, in the order they first appear: the instances one
/// request actually moves.
fn distinct(names: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for position in names {
        if !out.contains(position) {
            out.push(*position);
        }
    }
    out
}

#[tokio::test]
async fn enter_standby_moves_the_distinct_instances_the_request_names() {
    let (router, _engine) = emulator();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("standby-set-{index}");
        create_group(&router, case, &name, row.capacity).await;
        let in_service = instances_in_state(&router, &name, "InService", row.capacity).await;

        let moving: Vec<String> = distinct(row.names)
            .iter()
            .map(|position| {
                in_service
                    .get(*position)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the group must hold an instance at {position}")
                    })
                    .clone()
            })
            .collect();

        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p(
                "ShouldDecrementDesiredCapacity",
                if row.decrement { "true" } else { "false" },
            ),
        ];
        for (slot, position) in row.names.iter().enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", slot + 1),
                in_service[*position].clone(),
            ));
        }
        let (status, body) = query(&router, "EnterStandby", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: EnterStandby expected 200, got {status}: {body}"
        );

        // One activity per DISTINCT instance moved, each naming the instance it moved —
        // so a repeated id is one move, and a call naming no instance is answered with no
        // activity at all.
        let activity_ids = tag_values(&body, "ActivityId");
        let descriptions = tag_values(&body, "Description");
        assert_eq!(
            activity_ids.len(),
            moving.len(),
            "case {case:?}: expected {} activity/activities, got {activity_ids:?} in {body}",
            moving.len()
        );
        assert_eq!(
            descriptions.len(),
            moving.len(),
            "case {case:?}: expected one description per activity, got {descriptions:?} in {body}"
        );
        for instance_id in &moving {
            let naming: Vec<&String> = descriptions
                .iter()
                .filter(|description| description.contains(instance_id))
                .collect();
            assert_eq!(
                naming.len(),
                1,
                "case {case:?}: exactly one activity must name {instance_id}, got {naming:?} in \
                 {body}"
            );
        }

        // The activities the call is answered with are the group's OWN log: each one reads
        // back through `DescribeScalingActivities` with the same description.
        let reported = group_activities(&router, case, &name).await;
        let reported_ids = tag_values(&reported, "ActivityId");
        let reported_descriptions = tag_values(&reported, "Description");
        for (activity_id, description) in activity_ids.iter().zip(descriptions.iter()) {
            let position = reported_ids
                .iter()
                .position(|reported_id| reported_id == activity_id);
            let Some(position) = position else {
                panic!(
                    "case {case:?}: activity {activity_id} is not in the group's log, got \
                     {reported_ids:?} in {reported}"
                );
            };
            assert_eq!(
                reported_descriptions.get(position),
                Some(description),
                "case {case:?}: the log must report activity {activity_id} as the call did, got \
                 {reported_descriptions:?} in {reported}"
            );
        }

        // The parked instances read back `Standby`, and only those.
        let parked = instances_in_state(&router, &name, "Standby", moving.len()).await;
        for instance_id in &moving {
            assert!(
                parked.contains(instance_id),
                "case {case:?}: {instance_id} was not parked, got {parked:?}"
            );
        }

        // The capacity rule each form of the request documents: the decrement takes the
        // desired capacity down by the instances parked, and without it the capacity
        // stands and the group launches a replacement for each.
        let expected_capacity = if row.decrement {
            row.capacity - moving.len()
        } else {
            row.capacity
        };
        let read = describe(&router, &name).await;
        assert_eq!(
            tag_value(&read, "DesiredCapacity"),
            expected_capacity.to_string(),
            "case {case:?}: expected a desired capacity of {expected_capacity}, got {read}"
        );
        let serving = instances_in_state(&router, &name, "InService", expected_capacity).await;
        for instance_id in &moving {
            assert!(
                !serving.contains(instance_id),
                "case {case:?}: a parked instance does not meet the group's capacity, got \
                 {serving:?}"
            );
        }
    }
}

/// An instance id of no instance of the group, in the form Amazon EC2 gives one.
const ABSENT_INSTANCE_ID: &str = "i-00000000000000071";

#[tokio::test]
async fn a_request_outside_the_rule_is_refused_and_moves_nothing() {
    let (router, _engine) = emulator();
    let case = "a group holding one instance";
    let name = "standby-set-refused";
    create_group(&router, case, name, 1).await;
    let in_service = instances_in_state(&router, name, "InService", 1).await;

    // One id past the bound one call may name: the request carries one more id than the
    // operation's own documentation allows, and the refusal names the member and the bound.
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("ShouldDecrementDesiredCapacity", "false"),
    ];
    for slot in 1..=MAX_INSTANCE_IDS_PER_CALL + 1 {
        params.push(p(
            &format!("InstanceIds.member.{slot}"),
            format!("i-{slot:017}"),
        ));
    }
    let (status, body) = query(&router, "EnterStandby", &borrowed(&params)).await;
    assert_eq!(
        status, 400,
        "case {:?}: expected 400 for {} ids, got {status}: {body}",
        "one id past the bound",
        MAX_INSTANCE_IDS_PER_CALL + 1
    );
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert!(
        message.contains("'instanceIds'")
            && message.contains(&format!("less than or equal to {MAX_INSTANCE_IDS_PER_CALL}")),
        "case {:?}: the refusal must name the member and its bound, got {message:?}",
        "one id past the bound"
    );

    // An id of no instance of the group, refused with the wording every instance-scoped
    // write of this service answers one with.
    let (status, body) = query(
        &router,
        "EnterStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", ABSENT_INSTANCE_ID),
            ("ShouldDecrementDesiredCapacity", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {:?}: expected 400, got {status}: {body}",
        "an id the group does not hold"
    );
    assert_eq!(
        xml_unescape(&tag_value(&body, "Message")),
        not_in_group_message(ABSENT_INSTANCE_ID, name),
        "case {:?}: the refusal must name the instance and the group, got {body}",
        "an id the group does not hold"
    );

    // Both refusals left the group exactly as it was.
    assert_eq!(
        instances_in_state(&router, name, "InService", 1).await,
        in_service,
        "a refused request must leave the group's instances in service"
    );
    let read = describe(&router, name).await;
    assert_eq!(
        tag_value(&read, "DesiredCapacity"),
        "1",
        "a refused request must leave the group's desired capacity alone, got {read}"
    );
}
