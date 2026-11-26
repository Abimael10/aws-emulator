// SPDX-License-Identifier: CC0-1.0

//! The sentence `DetachInstances` refuses a decrement its group's sizes do not admit with.
//!
//! RULE: a request that MOVES named instances of an Auto Scaling group and would leave the
//! group's desired capacity outside the sizes the group is configured with — `MinSize` and
//! `MaxSize`, "the minimum size of the group" and "the maximum size of the group"
//! (`smithy_query(query="shape",shape="AutoScalingGroup")`) — is refused whole, HTTP 400
//! `ValidationError`, with the one sentence that refusal class states: the group the request
//! named, the three sizes the group holds now, the number of instances the request named and
//! the remedy of updating the group's sizes. Nothing of the request's own prose, and nothing
//! about the floor alone: the refusal is about the group's CONFIGURATION, which is what the
//! caller has to change for the same request to be admitted.
//!
//! The table below spans what the rule generalises over: whether the desired capacity sits AT
//! the minimum or above it, a maximum equal to the minimum and one above it, how many
//! instances one request names (one, two, three), the UNITS the group's size is stated in — a
//! member of a group whose mixed instances policy weights its instance types takes its own
//! weight's worth of capacity with it — and the two settings of
//! `ShouldDecrementDesiredCapacity`, because a request that asks for no decrement moves the
//! capacity nowhere and so crosses nothing. The rows that are ADMITTED pin the bound itself:
//! a decrement landing exactly ON the minimum is admitted, one unit under it is refused.
//!
//! Every wait here looks again until the condition holds, never for a span: the instances a
//! group launches come into service on a clock, so a fixed sleep would be a guess about how
//! fast the host runs.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

// The group reader and the poll-until-the-condition helper of the suite that owns this
// service's group-state table, so a group is read here through those rather than through a
// second copy of them.
use crate::group_state_and_standby_transitions::{describe, instances_in_state};
use crate::support::{borrowed, emulator, p, query, tag_value, tag_values, xml_unescape, REGION};

/// The launch template every group here launches from, one the harness puts in the account's
/// own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The `LifecycleState` a member serving traffic is reported in, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum spells it
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const LIFECYCLE_IN_SERVICE: &str = "InService";

/// The wire error code Amazon EC2 Auto Scaling refuses a request its own values make invalid
/// with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The instance type the weighted groups below override, so one override carries one weight.
const WEIGHTED_INSTANCE_TYPE: &str = "t3.micro";

/// How long a wait for a condition may take before a test gives up. Generous on purpose: what
/// is asserted is that the condition is reached at all, never how fast the host runs.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait between two looks while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The sentence the refusal states, written out here rather than taken from the service's own
/// builder — a test that re-used the implementation's literal would assert nothing.
///
/// Fixed text around four variable parts: the group the request named, the three sizes the
/// group holds, and how many instances the request asked to detach.
fn sizes_refuse_detach_message(
    group_name: &str,
    min_size: usize,
    max_size: usize,
    desired_capacity: usize,
    detaching: usize,
) -> String {
    format!(
        "AutoScalingGroup {group_name} has min-size={min_size}, max-size={max_size}, and \
         desired-size={desired_capacity}. To detach {detaching} instance, please update the \
         AutoScalingGroup sizes appropriately."
    )
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
    weight: Option<usize>,
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
                weight.to_string(),
            ));
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
}

/// Post one `DetachInstances` and answer its status, wire error code and message.
async fn detach(
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
    for (slot, instance_id) in instance_ids.iter().enumerate() {
        params.push(p(
            &format!("InstanceIds.member.{}", slot + 1),
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

/// Look again until the group reports `wanted` capacity units as its `DesiredCapacity`, or
/// [`DEADLINE`] passes.
///
/// The CONDITION, never a fixed wait: an admitted detach leaves the group converging, so the
/// capacity it reports is read until it holds.
async fn capacity_reaching(router: &MultiServiceRouter, case: &str, name: &str, wanted: usize) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let reported = tag_value(&body, "DesiredCapacity");
        if reported == wanted.to_string() || Instant::now() >= deadline {
            assert_eq!(
                reported,
                wanted.to_string(),
                "case {case:?}: expected a desired capacity of {wanted}, got {body}"
            );
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the table: the group's configuration, what the request asks for, and whether the
/// group's sizes admit it.
struct Row {
    case: &'static str,
    /// The `MinSize`, `MaxSize` and `DesiredCapacity` the group is created with, in the units
    /// its `weight` states.
    sizes: (usize, usize, usize),
    /// The `WeightedCapacity` one member of the group is worth, or `None` where it is worth
    /// one unit.
    weight: Option<usize>,
    /// How many of the group's members the request names.
    detaching: usize,
    /// Whether the request asks for the desired capacity to be decremented.
    decrement: bool,
    /// The `DesiredCapacity` the group holds once the call is answered: unchanged for a
    /// refusal, down by what the released members were providing for an admitted decrement.
    capacity_after: usize,
    /// Whether the group's sizes refuse the request.
    refused: bool,
}

/// How many members fulfil `units` capacity units in a group whose members are each worth
/// `weight`.
fn members_of(units: usize, weight: Option<usize>) -> usize {
    units / weight.unwrap_or(1)
}

#[tokio::test]
async fn a_detach_the_group_sizes_do_not_admit_states_the_group_and_its_sizes() {
    let rows = [
        Row {
            case: "the desired capacity already at a minimum equal to the maximum",
            sizes: (1, 1, 1),
            weight: None,
            detaching: 1,
            decrement: true,
            capacity_after: 1,
            refused: true,
        },
        Row {
            case: "the desired capacity at the minimum, with room above it to the maximum",
            sizes: (2, 5, 2),
            weight: None,
            detaching: 1,
            decrement: true,
            capacity_after: 2,
            refused: true,
        },
        Row {
            case: "three instances crossing a minimum below the desired capacity",
            sizes: (1, 4, 3),
            weight: None,
            detaching: 3,
            decrement: true,
            capacity_after: 3,
            refused: true,
        },
        Row {
            case: "one member worth two capacity units, which crosses the minimum by itself",
            sizes: (3, 6, 4),
            weight: Some(2),
            detaching: 1,
            decrement: true,
            capacity_after: 4,
            refused: true,
        },
        Row {
            case: "the decrement landing exactly on the minimum",
            sizes: (1, 4, 2),
            weight: None,
            detaching: 1,
            decrement: true,
            capacity_after: 1,
            refused: false,
        },
        Row {
            case: "a decrement to a minimum of none, releasing the whole group",
            sizes: (0, 4, 2),
            weight: None,
            detaching: 2,
            decrement: true,
            capacity_after: 0,
            refused: false,
        },
        Row {
            case: "the same group and instance with no decrement asked for",
            sizes: (1, 1, 1),
            weight: None,
            detaching: 1,
            decrement: false,
            capacity_after: 1,
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, _engine) = emulator();
        let case = row.case;
        let name = format!("asg-detach-sizes-{index}");
        create_group(&router, case, &name, row.sizes, row.weight).await;
        let members = members_of(row.sizes.2, row.weight);
        let held = instances_in_state(&router, &name, LIFECYCLE_IN_SERVICE, members).await;
        let named: Vec<String> = held.iter().take(row.detaching).cloned().collect();
        assert_eq!(
            named.len(),
            row.detaching,
            "case {case:?}: the group must hold {} member(s) to name, got {held:?}",
            row.detaching
        );

        let (status, code, message) = detach(&router, &name, &named, row.decrement).await;
        if row.refused {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {code:?} {message:?}"
            );
            assert_eq!(
                code, VALIDATION_ERROR,
                "case {case:?}: expected the {VALIDATION_ERROR} code, got {code:?} {message:?}"
            );
            let (min, max, desired) = row.sizes;
            let expected = sizes_refuse_detach_message(&name, min, max, desired, row.detaching);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );

            // The refusal changed nothing: the group holds the same members, each still in
            // service, at the capacity it had.
            let after = instances_in_state(&router, &name, LIFECYCLE_IN_SERVICE, members).await;
            assert_eq!(
                after, held,
                "case {case:?}: a refused detach must leave the group's members as they were, \
                 got {after:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} {code:?} {message:?}"
            );
        }

        capacity_reaching(&router, case, &name, row.capacity_after).await;
    }
}

/// RULE (the same one, over the instances the group KEEPS): a refusal is for the request as a
/// whole, so a call that named several members of a group releases none of them.
#[tokio::test]
async fn a_refused_detach_releases_no_instance_of_the_group() {
    let (router, _engine) = emulator();
    let case = "a group of three with a minimum of two, asked to release two";
    let name = "asg-detach-sizes-whole-request";
    create_group(&router, case, name, (2, 4, 3), None).await;
    let held = instances_in_state(&router, name, LIFECYCLE_IN_SERVICE, 3).await;
    let named: Vec<String> = held.iter().take(2).cloned().collect();

    let (status, code, message) = detach(&router, name, &named, true).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} {code:?} {message:?}"
    );
    assert_eq!(
        code, VALIDATION_ERROR,
        "case {case:?}: expected the {VALIDATION_ERROR} code, got {code:?} {message:?}"
    );
    let expected = sizes_refuse_detach_message(name, 2, 4, 3, 2);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );

    // Every member the call named is still a member of the group, in service, and the group
    // still reports the capacity it was created with.
    let after = describe(&router, name).await;
    let reported = tag_values(&after, "InstanceId");
    for instance_id in &held {
        assert!(
            reported.contains(instance_id),
            "case {case:?}: expected the group to keep {instance_id}, got {reported:?}"
        );
    }
    assert_eq!(
        tag_value(&after, "DesiredCapacity"),
        "3",
        "case {case:?}: a refused detach must leave the desired capacity where it was, got \
         {after}"
    );
}
