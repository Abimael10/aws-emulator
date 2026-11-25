// SPDX-License-Identifier: CC0-1.0

//! The two refusals `AttachInstances` states about an INSTANCE a request named: one in a zone
//! the group is not configured for, and one the account holds nothing for.
//!
//! The rules under test, each one sentence with fixed text and variable parts read off the
//! request and off what this service and Amazon EC2 hold:
//!
//! * an instance whose zone is not among the group's configured `AvailabilityZones` is refused
//!   with the group's WHOLE configured zone list, the group the request named, and the
//!   instance with the zone it occupies — so the list a caller has to place an instance in is
//!   in the message, however many zones the group is configured with and whichever way it
//!   states them (`AvailabilityZones` or `VPCZoneIdentifier`, both of which leave the group
//!   holding the zones it reports);
//! * an id the account holds no instance for is refused with Auto Scaling's own fixed text,
//!   naming the id the request carried — not the sentence Amazon EC2 states for an id of its
//!   own that resolves to nothing.
//!
//! The first table's rows differ along how many zones the group is configured with and how it
//! states them, so nothing in the sentence can be keyed to a one-zone group; the zone list
//! each row expects is read back from the group itself, so the message is asserted against
//! STORED state rather than against what the row sent. The second table's rows differ along
//! the id form (the seventeen-character form and the legacy eight-character one), how many ids
//! one call names, where the unknown one sits among them, and whether an attachable instance
//! is named beside it. Each table also carries a row the service must ACCEPT, so no row can
//! pass by refusing everything, and both close by reading the group back: nothing a refusal
//! named was attached.
//!
//! Every row is driven over the wire — awsQuery for Auto Scaling, ec2Query for Amazon EC2 —
//! against the services on ONE engine. Nothing here waits for a span: every wait looks AGAIN
//! until the condition the clock owes holds, or a generous deadline turns it into a failure.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_launch_template, ec2_query, element_body, emulator, p, query, seed_subnets,
    tag_value, tag_values, xml_unescape,
};

/// How long a case here waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every helper below looks again until the condition holds, so this
/// only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The AMI a standalone instance of this suite is launched from.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The launch template every group of this suite launches from.
const TEMPLATE: &str = "lt-attach-instance-wording";

/// The wire code every refusal here carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The wire code and the unescaped message of one refusal.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// Create one Auto Scaling group over the wire, placing its instances either by naming the
/// zones or by naming the subnets, and assert the create succeeded.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    max_size: i32,
    zones: &[String],
    subnets: Option<&[String]>,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", "0"),
    ];
    match subnets {
        Some(subnets) => params.push(p("VPCZoneIdentifier", subnets.join(","))),
        None => {
            for (index, zone) in zones.iter().enumerate() {
                params.push(p(
                    &format!("AvailabilityZones.member.{}", index + 1),
                    zone.clone(),
                ));
            }
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected HTTP 200, got {status} with {body:?}"
    );
}

/// What `DescribeAutoScalingGroups` answers for one group.
async fn describe(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected HTTP 200, got {status} with {body:?}"
    );
    body
}

/// The `AvailabilityZones` the group REPORTS, in the order it reports them: the stored list a
/// zone-mismatch refusal has to state.
async fn reported_zones(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = describe(router, name).await;
    let zones = element_body(&body, "AvailabilityZones")
        .map(|inner| tag_values(inner, "member"))
        .unwrap_or_default();
    assert!(
        !zones.is_empty(),
        "group {name} must report the zones it is configured with, got {body}"
    );
    zones
}

/// Launch one standalone Amazon EC2 instance into `subnet_id` over Amazon EC2's own wire and
/// answer its id, once Amazon EC2 reports it `running`.
async fn run_instance(router: &MultiServiceRouter, subnet_id: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "RunInstances",
        &[
            p("ImageId", IMAGE_ID),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "RunInstances expected HTTP 200, got {status} with {body:?}"
    );
    let instance_id = tag_value(&body, "instanceId");
    assert!(
        !instance_id.is_empty(),
        "RunInstances must report its instance, got {body}"
    );
    wait_for_running(router, &instance_id).await;
    instance_id
}

/// Look again until Amazon EC2 reports `instance_id` `running`.
async fn wait_for_running(router: &MultiServiceRouter, instance_id: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
        let state = if status == 200 {
            tag_value(&tag_value(&body, "instanceState"), "name")
        } else {
            String::new()
        };
        if state == "running" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected Amazon EC2 to report {instance_id} running, got {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Attach the instances `ids` names to `name` over the wire.
async fn attach(router: &MultiServiceRouter, name: &str, ids: &[String]) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, id) in ids.iter().enumerate() {
        params.push(p(&format!("InstanceIds.member.{}", index + 1), id.clone()));
    }
    query(router, "AttachInstances", &borrowed(&params)).await
}

/// Look again until the group reports exactly `wanted` instances, every one in service, and
/// answer their ids.
async fn settled_instances(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if ids.len() == wanted && states.iter().all(|state| state == "InService") {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: group {name} never reported {wanted} instance(s) in service; last \
             read {ids:?} in {states:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the zone-mismatch table: how the group states where its instances go, and how
/// many zones it is configured with.
struct ZoneRow {
    case: &'static str,
    group: &'static str,
    /// How many of the seeded subnets' zones the group is configured with. The instance the
    /// row attaches is launched into the NEXT seeded subnet, so its zone is outside the list.
    configured: usize,
    /// Whether the group states its placement as subnets rather than as zones.
    by_subnet: bool,
}

/// An instance in a zone the group is not configured for is refused with the group's whole
/// configured zone list, the group the request named, and the instance with the zone it
/// occupies.
#[tokio::test]
async fn a_zone_mismatch_states_the_groups_whole_zone_list() {
    let (router, engine) = emulator();
    // One subnet per zone of the Region, so a row configured with the first `n` of them has a
    // zone left over to launch an instance into.
    let subnets = seed_subnets(&engine, 4);
    let zones: Vec<String> = subnets.iter().map(|subnet| subnet.zone.clone()).collect();
    let ids: Vec<String> = subnets.iter().map(|subnet| subnet.id.clone()).collect();
    for (index, zone) in zones.iter().enumerate() {
        assert!(
            !zones[..index].contains(zone),
            "the seeded subnets must each be in a zone of their own, got {zones:?}"
        );
    }
    create_launch_template(&router, TEMPLATE).await;

    let rows = [
        ZoneRow {
            case: "a group configured with one zone",
            group: "asg-wording-zone-one",
            configured: 1,
            by_subnet: false,
        },
        ZoneRow {
            case: "a group configured with two zones",
            group: "asg-wording-zone-two",
            configured: 2,
            by_subnet: false,
        },
        ZoneRow {
            case: "a group configured with three zones",
            group: "asg-wording-zone-three",
            configured: 3,
            by_subnet: false,
        },
        ZoneRow {
            case: "a group that states two subnets rather than zones",
            group: "asg-wording-zone-subnets",
            configured: 2,
            by_subnet: true,
        },
    ];

    for row in &rows {
        let case = row.case;
        let configured = &zones[..row.configured];
        let subnet_ids = &ids[..row.configured];
        create_group(
            &router,
            row.group,
            5,
            configured,
            row.by_subnet.then_some(subnet_ids),
        )
        .await;
        // The row the service must ACCEPT: an instance of a zone the group IS configured for.
        let inside = run_instance(&router, &ids[0]).await;
        let (status, body) = attach(&router, row.group, std::slice::from_ref(&inside)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: attaching an instance of the group's own zone expected HTTP 200, \
             got {status} with {body:?}"
        );
        settled_instances(&router, case, row.group, 1).await;

        // The group's own list, read back from the group: the message has to state exactly
        // this, comma-separated, whichever way the group was told where to place instances.
        let stored = reported_zones(&router, row.group).await;
        let outside_zone = &zones[row.configured];
        assert!(
            !stored.contains(outside_zone),
            "case {case:?}: {outside_zone} must be outside the group's own zones {stored:?}"
        );
        let abroad = run_instance(&router, &ids[row.configured]).await;
        let (status, body) = attach(&router, row.group, std::slice::from_ref(&abroad)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let expected = format!(
            "The list of availability zones [{}] for the group {} does not contain the zone \
             occupied by the instance {abroad} [{outside_zone}]",
            stored.join(", "),
            row.group
        );
        let (code, message) = refusal(&body);
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected {VALIDATION_ERROR}, got {body:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the whole message {expected:?}, got {message:?}"
        );

        // The refused attach left the group holding only the accepted instance.
        let held = settled_instances(&router, case, row.group, 1).await;
        assert_eq!(
            held,
            vec![inside.clone()],
            "case {case:?}: the refused attach must leave the group holding only {inside}, got \
             {held:?}"
        );
    }
}

/// One row of the unknown-id table: the ids one call names, and which of them the refusal has
/// to name.
struct UnknownRow {
    case: &'static str,
    /// The ids the call names, `None` standing for the attachable instance the suite holds —
    /// a row that names it proves the unknown id is what the refusal reports.
    ids: Vec<Option<&'static str>>,
    /// The id the refusal names: the first one the account holds nothing for.
    reported: &'static str,
}

/// An id the account holds no instance for is refused with Auto Scaling's own fixed text,
/// naming the id the request carried.
#[tokio::test]
async fn an_id_the_account_holds_nothing_for_states_auto_scalings_own_text() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let home = subnets.first().expect("one seeded subnet").clone();
    create_launch_template(&router, TEMPLATE).await;
    let group = "asg-wording-unknown-id";
    create_group(&router, group, 5, std::slice::from_ref(&home.zone), None).await;

    // Ids of the two forms Amazon EC2 has issued — seventeen hex characters, and the legacy
    // eight — that this account holds no instance for.
    const LONG_FORM: &str = "i-0123456789abcdef0";
    const OTHER_LONG_FORM: &str = "i-0fedcba9876543210";
    const LEGACY_FORM: &str = "i-1a2b3c4d";

    // The row the service must ACCEPT, and the instance the mixed rows name beside an unknown
    // id: a running instance of the group's own zone.
    let attachable = run_instance(&router, &home.id).await;
    let (status, body) = attach(&router, group, std::slice::from_ref(&attachable)).await;
    assert_eq!(
        status, 200,
        "attaching a running instance of the group's own zone expected HTTP 200, got {status} \
         with {body:?}"
    );
    settled_instances(&router, "the accepted attach", group, 1).await;
    let spare = run_instance(&router, &home.id).await;

    let rows = [
        UnknownRow {
            case: "one id of the seventeen-character form",
            ids: vec![Some(LONG_FORM)],
            reported: LONG_FORM,
        },
        UnknownRow {
            case: "one id of the legacy eight-character form",
            ids: vec![Some(LEGACY_FORM)],
            reported: LEGACY_FORM,
        },
        UnknownRow {
            case: "one unknown id named twice",
            ids: vec![Some(OTHER_LONG_FORM), Some(OTHER_LONG_FORM)],
            reported: OTHER_LONG_FORM,
        },
        UnknownRow {
            case: "two unknown ids",
            ids: vec![Some(OTHER_LONG_FORM), Some(LEGACY_FORM)],
            reported: OTHER_LONG_FORM,
        },
        UnknownRow {
            case: "an attachable instance named before an unknown id",
            ids: vec![None, Some(LONG_FORM)],
            reported: LONG_FORM,
        },
        UnknownRow {
            case: "an unknown id named before an attachable instance",
            ids: vec![Some(LEGACY_FORM), None],
            reported: LEGACY_FORM,
        },
    ];

    for row in &rows {
        let case = row.case;
        let named: Vec<String> = row
            .ids
            .iter()
            .copied()
            .map(|id| id.map_or_else(|| spare.clone(), str::to_string))
            .collect();
        let (status, body) = attach(&router, group, &named).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let expected = format!("Instance [{}] is invalid.", row.reported);
        let (code, message) = refusal(&body);
        assert_eq!(
            code, VALIDATION_ERROR,
            "case {case:?}: expected {VALIDATION_ERROR}, got {body:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the whole message {expected:?}, got {message:?}"
        );
    }

    // Every refused call left the group exactly as the accepted one did, and the attachable
    // instance the mixed rows named is still standalone.
    let held = settled_instances(&router, "the group is unchanged", group, 1).await;
    assert_eq!(
        held,
        vec![attachable.clone()],
        "the refused calls must leave the group holding only {attachable}, got {held:?}"
    );
    let capacity = tag_value(&describe(&router, group).await, "DesiredCapacity");
    assert_eq!(
        capacity, "1",
        "the refused calls must leave the desired capacity at 1, got {capacity:?}"
    );
}
