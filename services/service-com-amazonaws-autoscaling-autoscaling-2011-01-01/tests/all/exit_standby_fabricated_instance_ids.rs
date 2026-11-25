// SPDX-License-Identifier: CC0-1.0

//! Which refusal `ExitStandby` answers for an instance id a CALLER made up.
//!
//! The rule under test: an instance id is allocated to an instance by Amazon EC2, out of the
//! whole space its published form has room for, so an id a caller writes down for itself
//! names no instance of the account and none of a group. `ExitStandby` resolves MEMBERSHIP
//! first and the lifecycle state second, so such an id is refused "The instance
//! `<instance-id>` is not part of Auto Scaling group `<group-name>`." — the refusal for an
//! instance the group does not hold — and only an id the group really holds can reach the
//! refusal for one on the wrong side of the move, "The instance `<instance-id>` is not in
//! Standby."
//!
//! What the table spans: ids a client reaches for when it wants one that names nothing —
//! the first ids a sequence counted from one would hand out, a round number further along,
//! a value high in the space, and the retired short id form — each against a group whose own
//! members were launched by the service itself, so an id the allocator produced can never be
//! one of the table's. The members' own ids are then asserted to be in the published form,
//! distinct and outside the range a caller can enumerate, which is what makes the first half
//! of the table a refusal about membership rather than about a live member's state.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

// The group reader, the poll-until-the-condition helper and the membership refusal wording
// of the suites that own them: one copy of each, reached from here.
use crate::group_state_and_standby_transitions::instances_in_state;
use crate::instance_scale_in_protection::not_in_group_message;
use crate::support::{borrowed, emulator, p, query, tag_value, xml_unescape, REGION};

/// The launch template the groups here launch their instances from, one the harness puts in
/// the account's own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// How many instances a group of this suite holds: more than one, so the table's ids are
/// weighed against a group whose membership the service filled in itself.
const GROUP_CAPACITY: usize = 2;

/// The lowest identifier an id of this suite's table may carry and still be one the service
/// could have allocated.
///
/// An instance id carries a 64-bit identifier (`i-` and 17 hexadecimal digits of it), and an
/// allocated one is drawn across that space rather than counted up, so every id a caller can
/// reach by enumerating from one is below this floor. Stated as a bound rather than as a
/// value, because WHICH identifier an instance is allocated is the service's to choose.
const ENUMERABLE_IDENTIFIER_CEILING: u128 = 1 << 48;

/// Create one group holding [`GROUP_CAPACITY`] instances in the first Availability Zone
/// Amazon EC2's own catalog gives the Region, and answer the ids of the instances it
/// launched once they are in service.
async fn group_in_service(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let zones = ec2::availability_zones_in_region(REGION);
    assert!(
        !zones.is_empty(),
        "the Region {REGION} must have an Availability Zone to place instances in"
    );
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", GROUP_CAPACITY.to_string()),
        p("DesiredCapacity", GROUP_CAPACITY.to_string()),
        p("AvailabilityZones.member.1", zones[0].zone_name.clone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
    instances_in_state(router, name, "InService", GROUP_CAPACITY).await
}

/// One `ExitStandby` call naming one instance: the status, the error code and the message.
async fn exit_standby(
    router: &MultiServiceRouter,
    name: &str,
    instance_id: &str,
) -> (u16, String, String) {
    let (status, body) = query(
        router,
        "ExitStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", instance_id),
        ],
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The identifier the long form of `instance_id` carries, or `None` for the retired short
/// form — the eight-digit form EC2 reads but no longer issues.
fn long_form_identifier(instance_id: &str) -> Option<u128> {
    let digits = instance_id.strip_prefix("i-")?;
    if digits.len() != 17 {
        return None;
    }
    u128::from_str_radix(digits, 16).ok()
}

/// Every id a caller reaches for when it wants one that names nothing: the first ids a
/// sequence counted from one would hand out, a round number further along, a value high in
/// the space, and the retired short form.
const FABRICATED_IDS: &[(&str, &str)] = &[
    ("the first id a sequence from one would hand out", "i-00000000000000001"),
    ("the second such id", "i-00000000000000002"),
    ("a round number further along a sequence", "i-00000000000000064"),
    ("a value high in the space", "i-0fffffffffffffffe"),
    ("the retired short id form", "i-0abc1234"),
];

/// `ExitStandby` refuses every id a caller made up as an instance the group does not hold,
/// and a member of the group that is merely in service as one that is not in `Standby`.
#[tokio::test]
async fn exit_standby_refuses_a_fabricated_id_as_no_member_of_the_group() {
    let (router, _engine) = emulator();
    let name = "exit-standby-fabricated";
    let members = group_in_service(&router, name).await;

    for (case, instance_id) in FABRICATED_IDS {
        assert!(
            !members.contains(&(*instance_id).to_string()),
            "case {case:?}: the group's own members {members:?} must not hold the id a caller \
             made up, got {instance_id}"
        );
        let (status, code, message) = exit_standby(&router, name, instance_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {instance_id}, got {status} with {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the argument refusal code for {instance_id}, got {code:?}"
        );
        assert_eq!(
            message,
            not_in_group_message(instance_id, name),
            "case {case:?}: expected the refusal for an instance the group does not hold, got \
             {message:?}"
        );
    }

    // Only an id the group REALLY holds reaches the refusal about the side of the move the
    // instance is on, which is the refusal class a made-up id must not be answered with.
    for instance_id in &members {
        let (status, code, message) = exit_standby(&router, name, instance_id).await;
        assert_eq!(
            status, 400,
            "case {instance_id:?}: expected HTTP 400 for a member in service, got {status} with \
             {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {instance_id:?}: expected the argument refusal code, got {code:?}"
        );
        assert_eq!(
            message,
            format!("The instance {instance_id} is not in Standby."),
            "case {instance_id:?}: expected the refusal for a member outside Standby, got \
             {message:?}"
        );
    }
}

/// The ids the group's own instances were allocated are in the published long form,
/// distinct, and outside the range a caller can enumerate.
#[tokio::test]
async fn a_groups_member_ids_are_allocated_outside_the_range_a_caller_can_enumerate() {
    let (router, _engine) = emulator();
    let members = group_in_service(&router, "exit-standby-allocation").await;

    for instance_id in &members {
        let identifier = long_form_identifier(instance_id).unwrap_or_else(|| {
            panic!("a member id must be `i-` and 17 hexadecimal digits, got {instance_id:?}")
        });
        assert!(
            instance_id.starts_with("i-0"),
            "case {instance_id:?}: a 64-bit identifier never fills the seventeenth digit, so \
             the id must read `i-0…`"
        );
        assert!(
            identifier > ENUMERABLE_IDENTIFIER_CEILING,
            "case {instance_id:?}: an allocated identifier is drawn across the space, so it \
             must be above {ENUMERABLE_IDENTIFIER_CEILING}, got {identifier}"
        );
    }
    let distinct: std::collections::BTreeSet<&String> = members.iter().collect();
    assert_eq!(
        distinct.len(),
        members.len(),
        "every member must hold an id of its own, got {members:?}"
    );
}
