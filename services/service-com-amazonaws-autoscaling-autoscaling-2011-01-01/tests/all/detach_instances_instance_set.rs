// SPDX-License-Identifier: CC0-1.0

//! What `DetachInstances` releases is the SET of instances its request names.
//!
//! RULE: `DetachInstances` releases exactly the DISTINCT instances its request names from the
//! group it names — "removes one or more instances from the specified Auto Scaling group.
//! After the instances are detached, you can manage them independent of the Auto Scaling
//! group. If you do not specify the option to decrement the desired capacity, Amazon EC2 Auto
//! Scaling launches instances to replace the ones that are detached"
//! (`smithy_query(query="doc",shape="DetachInstances")`) — records one scaling activity of the
//! group per instance it released and answers with those activities, leaves the group's
//! desired capacity down by the capacity those instances were providing when the request asked
//! for the decrement and standing (so a replacement is launched for each) when it did not, and
//! releases rather than terminates: the Amazon EC2 instance keeps running and stops carrying
//! the tag that marks a member of the group.
//!
//! The tables below span what that rule generalises over: how many ids one call carries (none,
//! one, several, the published bound of 20), whether an id is REPEATED — a repeated id names
//! one instance, so it is one detach and one activity — both settings of
//! `ShouldDecrementDesiredCapacity`, which of the two states a member is detachable in it is
//! released from (`InService`, and parked out of service in `Standby`), and the UNITS the
//! group's size is stated in, because a member of a group whose mixed instances policy weights
//! its instance types takes its own weight's worth of capacity with it rather than one.
//!
//! Every wait here looks again until the condition holds, never for a span: the lifecycle
//! transitions a detach starts are owed on a clock, so a fixed sleep would be a guess about how
//! fast the host runs.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

// The group reader of the suite that owns this service's group-state table, so a group is read
// here through that one reader rather than a second copy of it.
use crate::group_state_and_standby_transitions::{describe, instances_in_state};
use crate::support::{borrowed, ec2_query, emulator, p, query, tag_value, tag_values, REGION};

/// The launch template every group here launches from, one the harness puts in the account's
/// own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The bound on how many instances ONE call may name, and so the largest group here.
///
/// SOURCE: `DetachInstancesQuery$InstanceIds` — "The IDs of the instances. You can specify up
/// to 20 instances."
const MAX_INSTANCE_IDS_PER_CALL: usize = 20;

/// The two `LifecycleState` values a member of a group is detachable in, spelled as the model's
/// own `com.amazonaws.autoscaling#LifecycleState` enum spells them
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const LIFECYCLE_IN_SERVICE: &str = "InService";
const LIFECYCLE_STANDBY: &str = "Standby";

/// The `InstanceStateName` Amazon EC2 reports for an instance it is still running.
const EC2_STATE_RUNNING: &str = "running";

/// The tag Amazon EC2 Auto Scaling puts on every instance of a group, which is what tells an
/// instance a group manages from one a client manages itself — so a detached instance stops
/// carrying it.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// The instance type the weighted groups below override, so one override carries one weight.
const WEIGHTED_INSTANCE_TYPE: &str = "t3.micro";

/// How long a wait for a condition may take before a test gives up. Generous on purpose: what
/// is asserted is that the condition is reached at all, never how fast the host runs.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait between two looks while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The `Activity$Description` a detach carries, naming the instance it released from the
/// group — written out here rather than taken from the service, so the assertion is about the
/// wording and not about the implementation agreeing with itself.
fn detach_description(instance_id: &str) -> String {
    format!("Detaching EC2 instance: {instance_id}")
}

/// Create one group with the sizes `(min, max, desired)` in the first Availability Zone Amazon
/// EC2's own catalog gives the Region.
///
/// `weight` states the `WeightedCapacity` of the one instance type a mixed instances policy
/// overrides, for a group whose three size parameters are therefore in those units; `None`
/// launches from the launch template directly, where every member is worth one.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    sizes: (usize, usize, usize),
    weight: Option<&str>,
) {
    let zones = ec2::availability_zones_in_region(REGION);
    assert!(
        !zones.is_empty(),
        "case {case:?}: the Region {REGION} must have an Availability Zone to place instances in"
    );
    let (min, max, desired) = sizes;
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", min.to_string()),
        p("MaxSize", max.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", zones[0].zone_name.clone()),
    ];
    match weight {
        None => params.push(p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE)),
        Some(weight) => {
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.\
                 LaunchTemplateName",
                LAUNCH_TEMPLATE,
            ));
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
                WEIGHTED_INSTANCE_TYPE,
            ));
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.WeightedCapacity",
                weight,
            ));
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Post one `DetachInstances` naming `instance_ids` in the order given — a repeated id is sent
/// repeated — and answer its status and body.
async fn detach(
    router: &MultiServiceRouter,
    name: &str,
    instance_ids: &[String],
    decrement: bool,
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(
            "ShouldDecrementDesiredCapacity",
            if decrement { "true" } else { "false" },
        ),
    ];
    for (slot, instance_id) in instance_ids.iter().enumerate() {
        params.push(p(
            &format!("InstanceIds.member.{}", slot + 1),
            instance_id.clone(),
        ));
    }
    query(router, "DetachInstances", &borrowed(&params)).await
}

/// The scaling activities one group has recorded, as a client reads them.
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

/// Look again until the group reports `in_service` instances in service and holds none of
/// `released` at all, or [`DEADLINE`] passes. Answers the ids it last read in service.
///
/// The CONDITION, never a fixed wait: a released instance is reported in the transient state of
/// a member on its way out until the clock takes it out of the group, and a replacement the
/// detach asked for comes into service on that same clock.
async fn settled_without(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    in_service: usize,
    released: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let held = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let serving: Vec<String> = held
            .iter()
            .zip(states.iter())
            .filter(|(_, state)| state.as_str() == LIFECYCLE_IN_SERVICE)
            .map(|(id, _)| id.clone())
            .collect();
        if serving.len() == in_service && released.iter().all(|id| !held.contains(id)) {
            return serving;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to hold {in_service} instance(s) in service and none \
             of {released:?}, last read {held:?} in {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What Amazon EC2 reports for ONE instance: the state it is in, and the tags it carries.
///
/// The service that OWNS the instance is what says whether it is still running and whether it
/// is still marked a member of a group — which is what tells a detach from a termination.
async fn ec2_instance(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, Vec<(String, String)>) {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "instanceId"),
        instance_id,
        "case {case:?}: expected Amazon EC2 to report {instance_id} in {REGION}, got {body}"
    );
    // The state is read inside the `instanceState` element it belongs to, so the name of the
    // state is not confused with any other `name` the answer carries.
    let state = body
        .find("<instanceState")
        .map(|start| tag_value(&body[start..], "name"))
        .unwrap_or_default();
    let tags = tag_values(&body, "key")
        .into_iter()
        .zip(tag_values(&body, "value"))
        .collect();
    (state, tags)
}

/// Assert that `instance_id` is one Amazon EC2 still runs and no longer marks a member of
/// `group`: what "you can manage them independent of the Auto Scaling group" means.
async fn assert_released_not_terminated(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
) {
    let (state, tags) = ec2_instance(router, case, instance_id).await;
    assert_eq!(
        state, EC2_STATE_RUNNING,
        "case {case:?}: expected Amazon EC2 to still run the detached {instance_id}, got \
         {state:?}"
    );
    let marked = tags
        .iter()
        .find(|(key, _)| key == GROUP_NAME_TAG)
        .map(|(_, value)| value.clone());
    assert_ne!(
        marked.as_deref(),
        Some(group),
        "case {case:?}: expected the detached {instance_id} to stop carrying \
         {GROUP_NAME_TAG}={group:?}, got {tags:?}"
    );
}

/// One row of the instance-set table.
struct Row {
    case: &'static str,
    /// How many instances the group holds before the call.
    capacity: usize,
    /// Which of those instances the request names, by position in the group's own in-service
    /// list — a position REPEATED here names the same instance twice in one request.
    names: &'static [usize],
    /// Whether the request asks for the desired capacity to be decremented.
    decrement: bool,
}

/// Every row: the ids one call carries, and both settings of the decrement.
fn rows() -> Vec<Row> {
    // The row at the published bound names one id per instance the group holds, so it is built
    // from the bound itself rather than written out.
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
            case: "no instance named: nothing is released",
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
            case: "one instance, leaving the desired capacity standing",
            capacity: 2,
            names: &[0],
            decrement: false,
        },
        Row {
            case: "one instance named three times: one detach",
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
            case: "the whole group, decrementing",
            capacity: 2,
            names: &[0, 1],
            decrement: true,
        },
        Row {
            case: "the 20 ids one call may name",
            capacity: MAX_INSTANCE_IDS_PER_CALL,
            names: &AT_THE_BOUND,
            decrement: true,
        },
    ]
}

/// The distinct positions of `names`, in the order they first appear: the instances one request
/// actually releases.
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
async fn a_detach_releases_the_distinct_instances_the_request_names() {
    let (router, _engine) = emulator();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let name = format!("detach-set-{index}");
        create_group(
            &router,
            case,
            &name,
            (0, MAX_INSTANCE_IDS_PER_CALL, row.capacity),
            None,
        )
        .await;
        let in_service =
            instances_in_state(&router, &name, LIFECYCLE_IN_SERVICE, row.capacity).await;

        let releasing: Vec<String> = distinct(row.names)
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
        let named: Vec<String> = row
            .names
            .iter()
            .map(|position| in_service[*position].clone())
            .collect();

        let (status, body) = detach(&router, &name, &named, row.decrement).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DetachInstances expected 200, got {status}: {body}"
        );

        // One activity per DISTINCT instance released, each describing the detach of the
        // instance it released — so a repeated id is one detach, and a call naming no instance
        // is answered with no activity at all.
        let activity_ids = tag_values(&body, "ActivityId");
        let descriptions = tag_values(&body, "Description");
        assert_eq!(
            activity_ids.len(),
            releasing.len(),
            "case {case:?}: expected {} activity/activities, got {activity_ids:?} in {body}",
            releasing.len()
        );
        for instance_id in &releasing {
            let expected = detach_description(instance_id);
            let naming: Vec<&String> = descriptions
                .iter()
                .filter(|description| **description == expected)
                .collect();
            assert_eq!(
                naming.len(),
                1,
                "case {case:?}: exactly one activity must be described {expected:?}, got \
                 {descriptions:?} in {body}"
            );
        }

        // The activities the call is answered with are the group's OWN log: each one reads back
        // through `DescribeScalingActivities` with the same description.
        let reported = group_activities(&router, case, &name).await;
        let reported_ids = tag_values(&reported, "ActivityId");
        let reported_descriptions = tag_values(&reported, "Description");
        for (activity_id, description) in activity_ids.iter().zip(descriptions.iter()) {
            let Some(position) = reported_ids.iter().position(|held| held == activity_id) else {
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

        // The capacity rule each form of the request documents: the decrement takes the desired
        // capacity down by what the released instances were providing — one unit each in a group
        // that states no weights — and without it the capacity stands and the group launches a
        // replacement for each.
        let expected_capacity = if row.decrement {
            row.capacity - releasing.len()
        } else {
            row.capacity
        };
        let serving = settled_without(&router, case, &name, expected_capacity, &releasing).await;
        let read = describe(&router, &name).await;
        assert_eq!(
            tag_value(&read, "DesiredCapacity"),
            expected_capacity.to_string(),
            "case {case:?}: expected a desired capacity of {expected_capacity}, got {read}"
        );
        for instance_id in in_service
            .iter()
            .filter(|instance_id| !releasing.contains(instance_id))
        {
            assert!(
                serving.contains(instance_id),
                "case {case:?}: the group must keep the instance {instance_id} it was not asked \
                 to release, got {serving:?}"
            );
        }

        // Every released instance was RELEASED, not terminated.
        for instance_id in &releasing {
            assert_released_not_terminated(&router, case, &name, instance_id).await;
        }
    }
}

#[tokio::test]
async fn a_member_parked_in_standby_is_detachable() {
    let (router, _engine) = emulator();
    let case = "a member parked out of service before the detach";
    let name = "detach-standby";
    create_group(&router, case, name, (0, 4, 2), None).await;
    let in_service = instances_in_state(&router, name, LIFECYCLE_IN_SERVICE, 2).await;
    let parked = in_service[0].clone();
    let kept = in_service[1].clone();

    // Park one member out of service: `Standby` is the state a client puts a member in precisely
    // in order to manage it independently, which is the second state a detach may release from.
    let (status, body) = query(
        &router,
        "EnterStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", parked.as_str()),
            ("ShouldDecrementDesiredCapacity", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: EnterStandby expected 200, got {status}: {body}"
    );
    let standing_by = instances_in_state(&router, name, LIFECYCLE_STANDBY, 1).await;
    assert_eq!(
        standing_by,
        vec![parked.clone()],
        "case {case:?}: expected {parked} to be parked, got {standing_by:?}"
    );

    let released = vec![parked.clone()];
    let (status, body) = detach(&router, name, &released, false).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetachInstances of a parked member expected 200, got {status}: {body}"
    );
    let expected = detach_description(&parked);
    assert_eq!(
        tag_values(&body, "Description"),
        vec![expected.clone()],
        "case {case:?}: expected the one activity {expected:?}, got {body}"
    );

    // The group released it and kept the one it still holds, at the capacity the parking left it
    // with — the detach asked for no further decrement.
    let serving = settled_without(&router, case, name, 1, &released).await;
    assert_eq!(
        serving,
        vec![kept.clone()],
        "case {case:?}: expected the group to keep only {kept}, got {serving:?}"
    );
    let read = describe(&router, name).await;
    assert_eq!(
        tag_value(&read, "DesiredCapacity"),
        "1",
        "case {case:?}: expected the desired capacity the parking left, got {read}"
    );
    assert_released_not_terminated(&router, case, name, &parked).await;
}

/// One row of the weighted-capacity table: the weight each member of the group is worth, the
/// capacity the group is created with, how many members the request releases, and the capacity
/// the decrement leaves it at.
struct WeightRow {
    case: &'static str,
    /// The `WeightedCapacity` the group's single override carries, or `None` for a group that
    /// states no weights at all, where every member is worth one unit.
    weight: Option<&'static str>,
    /// The `DesiredCapacity` the group is created with, in those units.
    units: usize,
    /// How many members fulfil that capacity, and so how many the group holds.
    members: usize,
    /// How many of them the request releases.
    released: usize,
    /// The `DesiredCapacity` the group reports afterwards.
    units_after: usize,
    /// How many members the group then holds.
    members_after: usize,
}

#[tokio::test]
async fn the_decrement_is_the_capacity_the_released_instances_were_providing() {
    // The rows span the units a group's size can be stated in: no weights, where a member is
    // worth one, and two different weights, where it is worth its own.
    let rows = [
        WeightRow {
            case: "a group that states no weights",
            weight: None,
            units: 2,
            members: 2,
            released: 1,
            units_after: 1,
            members_after: 1,
        },
        WeightRow {
            case: "a member worth two capacity units",
            weight: Some("2"),
            units: 4,
            members: 2,
            released: 1,
            units_after: 2,
            members_after: 1,
        },
        WeightRow {
            case: "both members of a group worth five capacity units each",
            weight: Some("5"),
            units: 10,
            members: 2,
            released: 2,
            units_after: 0,
            members_after: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, _engine) = emulator();
        let case = row.case;
        let name = format!("detach-weighted-{index}");
        create_group(
            &router,
            case,
            &name,
            (0, row.units, row.units),
            row.weight,
        )
        .await;
        let in_service =
            instances_in_state(&router, &name, LIFECYCLE_IN_SERVICE, row.members).await;
        let released: Vec<String> = in_service.iter().take(row.released).cloned().collect();

        let (status, body) = detach(&router, &name, &released, true).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DetachInstances expected 200, got {status}: {body}"
        );

        // The decrement is in the units the group's size is stated in: each released member took
        // its own weight's worth of capacity with it.
        settled_without(&router, case, &name, row.members_after, &released).await;
        let read = describe(&router, &name).await;
        assert_eq!(
            tag_value(&read, "DesiredCapacity"),
            row.units_after.to_string(),
            "case {case:?}: expected a desired capacity of {} units, got {read}",
            row.units_after
        );
    }
}
