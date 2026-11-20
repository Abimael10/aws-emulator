// SPDX-License-Identifier: CC0-1.0

//! What a read of an Auto Scaling group reports is the state the preceding mutations
//! actually produced.
//!
//! Five rules are under test here, each of them about the same thing: a read answers from
//! what the group IS, and a mutation either produces that state or is refused.
//!
//! 1. A group reports every member it CARRIES, including the ones the service decides when
//!    the request leaves them out: both spellings of its Availability Zones (the names and
//!    the ids Amazon EC2's own catalog gives them), its Capacity Reservation preference,
//!    the capacity distribution strategy across its zones and its instance lifecycle
//!    policy, each at the default its own model documentation states
//!    (`CapacityDistributionStrategy`: "the default is `balanced-best-effort`";
//!    `RetentionTriggers$TerminateHookAbandon`: "set to `terminate` for default termination
//!    behavior").
//! 2. "If you specify `VPCZoneIdentifier` with `AvailabilityZones`, the subnets that you
//!    specify must reside in those Availability Zones"
//!    (`smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="VPCZoneIdentifier")`),
//!    so a request whose two placements disagree is refused and nothing is stored.
//! 3. A `HealthCheckGracePeriod` is a wait, so no group holds a negative one: the request
//!    stating one is refused and the group keeps the value it had.
//! 4. `EnterStandby` parks the instances it names out of service and `ExitStandby` returns
//!    them, with the capacity rules both operations document; a parked instance reads back
//!    `Standby`, and every instance reports the zone ID it was launched in and the launch
//!    template VERSION it actually ran, never the `$Latest` word its group floats on.
//! 5. A running instance refresh reports the preferences IN FORCE for it — each member the
//!    request left out at its documented default, and `AutoRollback` at the effective value
//!    rather than the requested one, since "a rollback is not supported" where the refresh
//!    carries no desired configuration.
//!
//! Every table below spans what its rule generalises over, and every row is driven over
//! the wire — awsQuery for Auto Scaling and IAM, ec2Query for Amazon EC2 — against the
//! three services on ONE engine, so the zones, templates, profiles and instances these
//! assertions resolve against are the account's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam_service;

use crate::support::{
    borrowed, ec2_query, iam_query, p, query, seed_subnets, tag_value, tag_values, xml_unescape,
    REGION,
};

/// How long a suite here waits for a condition the clock owes before calling it a failure.
///
/// A BACKSTOP, not a wait: every helper below looks AGAIN until the condition it needs
/// holds, so this only turns a state that never arrives into a failure instead of a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The image and instance type every launch template in this suite carries.
const IMAGE_ID: &str = "ami-00000000000000031";
const INSTANCE_TYPE: &str = "t3.micro";

/// One emulator with the three services this suite needs on ONE engine: the group, its
/// launch templates and the instance profile they name belong to one account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// The Availability Zones of [`REGION`], read from Amazon EC2's own catalog rather than
/// written out here.
fn zones() -> Vec<ec2::AvailabilityZoneRecord> {
    let zones = ec2::availability_zones_in_region(REGION);
    assert!(
        zones.len() >= 2,
        "the Region {REGION} must have at least two Availability Zones for these tables"
    );
    zones
}

/// Create one Amazon EC2 launch template over that service's own wire, carrying the
/// parameters `extra` adds, and answer its name.
async fn create_template(
    router: &MultiServiceRouter,
    name: &str,
    extra: &[(String, String)],
) -> String {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    params.extend(extra.iter().cloned());
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate for {name} expected 200, got {status}: {body}"
    );
    name.to_string()
}

/// Create one Auto Scaling group over the wire and answer the status and body it answered.
async fn create_group(
    router: &MultiServiceRouter,
    params: &[(String, String)],
) -> (u16, String) {
    query(router, "CreateAutoScalingGroup", &borrowed(params)).await
}

/// What `DescribeAutoScalingGroups` answers for one group.
///
/// `pub(crate)` so the sibling suite over `EnterStandby`'s instance set reads a group through
/// this one reader rather than a second copy of it.
pub(crate) async fn describe(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected 200, got {status}: {body}"
    );
    body
}

/// Look again until the group reports exactly `wanted` instances in `state`, or
/// [`DEADLINE`] passes. Answers the instance ids it last read, in document order.
///
/// `pub(crate)` for the same reason as [`describe`]: one poll-until-the-condition helper for
/// every suite here that waits on a lifecycle state.
pub(crate) async fn instances_in_state(
    router: &MultiServiceRouter,
    name: &str,
    state: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = describe(router, name).await;
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let matching: Vec<String> = ids
            .iter()
            .zip(states.iter())
            .filter(|(_, held)| held.as_str() == state)
            .map(|(id, _)| id.clone())
            .collect();
        if matching.len() == wanted {
            return matching;
        }
        assert!(
            Instant::now() < deadline,
            "group {name} never reported {wanted} instance(s) in {state}; last read {states:?} \
             in {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the member-projection table: how the request spells the group's placement,
/// and what the group must then report for both spellings.
struct PlacementRow {
    case: &'static str,
    /// Whether the request states the zones by NAME, by ID, or only by subnet.
    spelling: Spelling,
    /// Which of the Region's zones the group must end up in, by catalog index.
    zones_of: Vec<usize>,
}

/// How a request spells where the group places its instances.
#[derive(Debug, Clone, Copy)]
enum Spelling {
    /// `AvailabilityZones`, by zone name.
    Names,
    /// `AvailabilityZoneIds`, by zone id.
    Ids,
    /// `VPCZoneIdentifier` alone: the zones are the zones of those subnets.
    Subnets,
}

/// A group reports both spellings of its zones and every option set it carries, each at
/// the default its own documentation states when the request left it out.
#[tokio::test]
async fn a_group_reports_both_zone_spellings_and_the_option_sets_it_carries() {
    let (router, engine) = emulator();
    let catalog = zones();
    let subnets = seed_subnets(&engine, catalog.len());
    create_template(&router, "lt-members", &[]).await;

    let rows = [
        PlacementRow {
            case: "one zone named by name",
            spelling: Spelling::Names,
            zones_of: vec![0],
        },
        PlacementRow {
            case: "two zones named by name",
            spelling: Spelling::Names,
            zones_of: vec![0, 1],
        },
        PlacementRow {
            case: "one zone named by id",
            spelling: Spelling::Ids,
            zones_of: vec![1],
        },
        PlacementRow {
            case: "two zones named by id",
            spelling: Spelling::Ids,
            zones_of: vec![0, 1],
        },
        PlacementRow {
            case: "no zone named at all: the zones of the named subnets",
            spelling: Spelling::Subnets,
            zones_of: vec![0, 1],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("members-{index}");
        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-members"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
        ];
        match row.spelling {
            Spelling::Names => {
                for (position, zone) in row.zones_of.iter().enumerate() {
                    params.push(p(
                        &format!("AvailabilityZones.member.{}", position + 1),
                        catalog[*zone].zone_name.clone(),
                    ));
                }
            }
            Spelling::Ids => {
                for (position, zone) in row.zones_of.iter().enumerate() {
                    params.push(p(
                        &format!("AvailabilityZoneIds.member.{}", position + 1),
                        catalog[*zone].zone_id.clone(),
                    ));
                }
            }
            Spelling::Subnets => {
                let identifier = row
                    .zones_of
                    .iter()
                    .map(|zone| subnets[*zone].id.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                params.push(p("VPCZoneIdentifier", identifier));
            }
        }
        let (status, body) = create_group(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        let body = describe(&router, &name).await;
        let expected_names: String = row
            .zones_of
            .iter()
            .map(|zone| format!("<member>{}</member>", catalog[*zone].zone_name))
            .collect();
        let expected_ids: String = row
            .zones_of
            .iter()
            .map(|zone| format!("<member>{}</member>", catalog[*zone].zone_id))
            .collect();
        assert_eq!(
            tag_value(&body, "AvailabilityZones"),
            expected_names,
            "case {:?}: expected AvailabilityZones {expected_names}, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "AvailabilityZoneIds"),
            expected_ids,
            "case {:?}: expected AvailabilityZoneIds {expected_ids}, got {body}",
            row.case
        );
        // The option sets every group carries, each at its documented default.
        assert_eq!(
            tag_value(&body, "CapacityReservationPreference"),
            "default",
            "case {:?}: expected the default Capacity Reservation preference, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "CapacityDistributionStrategy"),
            "balanced-best-effort",
            "case {:?}: expected the documented default distribution strategy, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "TerminateHookAbandon"),
            "terminate",
            "case {:?}: expected the documented default retention trigger, got {body}",
            row.case
        );
    }
}

/// A request that STATES those option sets keeps exactly what it stated: the default is a
/// floor under a request that omits the member, never an override of one that states it.
#[tokio::test]
async fn stated_option_sets_are_kept_as_written() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-stated", &[]).await;

    // Each value is one the model's own enum admits and is NOT this service's default, so
    // a row that read back the default would be indistinguishable from a dropped member.
    let cases = [
        ("capacity-reservations-only", "balanced-only", "retain"),
        (
            "capacity-reservations-first",
            "reservations-then-balanced",
            "retain",
        ),
        ("none", "balanced-only", "retain"),
    ];
    for (index, (preference, strategy, trigger)) in cases.iter().enumerate() {
        let name = format!("stated-{index}");
        let params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-stated"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
            p(
                "CapacityReservationSpecification.CapacityReservationPreference",
                *preference,
            ),
            p(
                "AvailabilityZoneDistribution.CapacityDistributionStrategy",
                *strategy,
            ),
            p(
                "InstanceLifecyclePolicy.RetentionTriggers.TerminateHookAbandon",
                *trigger,
            ),
        ];
        let (status, body) = create_group(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {preference}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );
        let body = describe(&router, &name).await;
        assert_eq!(
            tag_value(&body, "CapacityReservationPreference"),
            *preference,
            "case {preference}: the stated preference must be kept, got {body}"
        );
        assert_eq!(
            tag_value(&body, "CapacityDistributionStrategy"),
            *strategy,
            "case {preference}: the stated strategy must be kept, got {body}"
        );
        assert_eq!(
            tag_value(&body, "TerminateHookAbandon"),
            *trigger,
            "case {preference}: the stated retention trigger must be kept, got {body}"
        );
    }
}

/// The sentence a request whose subnets are not in the zones it names is refused with.
const ZONE_MISMATCH: &str =
    "The availability zones of the specified subnets and the Auto Scaling group do not match";

/// One row of the placement-agreement table.
struct AgreementRow {
    case: &'static str,
    /// The zones the request states, by catalog index.
    zones_of: Vec<usize>,
    /// Whether it states them by id rather than by name.
    by_id: bool,
    /// The subnets it names, by seeded index — each subnet is in the zone of that index.
    subnets_of: Vec<usize>,
    /// Whether the request must be SERVED.
    accepted: bool,
}

/// A request stating both the zones and the subnets has to state them consistently, on the
/// create and on the update alike; a refused one leaves the group exactly as it was.
#[tokio::test]
async fn subnets_must_reside_in_the_named_zones() {
    let (router, engine) = emulator();
    let catalog = zones();
    let subnets = seed_subnets(&engine, catalog.len());
    create_template(&router, "lt-agreement", &[]).await;

    let rows = [
        AgreementRow {
            case: "one zone and a subnet in it",
            zones_of: vec![0],
            by_id: false,
            subnets_of: vec![0],
            accepted: true,
        },
        AgreementRow {
            case: "one zone and a subnet in another zone",
            zones_of: vec![0],
            by_id: false,
            subnets_of: vec![1],
            accepted: false,
        },
        AgreementRow {
            case: "two zones and a subnet in each of them",
            zones_of: vec![0, 1],
            by_id: false,
            subnets_of: vec![0, 1],
            accepted: true,
        },
        AgreementRow {
            case: "one zone and two subnets, one of them elsewhere",
            zones_of: vec![0],
            by_id: false,
            subnets_of: vec![0, 1],
            accepted: false,
        },
        AgreementRow {
            case: "two zones and a subnet in the first of them",
            zones_of: vec![0, 1],
            by_id: false,
            subnets_of: vec![0],
            accepted: true,
        },
        AgreementRow {
            case: "the zone stated by id, and a subnet in another zone",
            zones_of: vec![0],
            by_id: true,
            subnets_of: vec![1],
            accepted: false,
        },
        AgreementRow {
            case: "the zone stated by id, and a subnet in it",
            zones_of: vec![0],
            by_id: true,
            subnets_of: vec![0],
            accepted: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("agreement-{index}");
        let identifier = row
            .subnets_of
            .iter()
            .map(|subnet| subnets[*subnet].id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-agreement"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("VPCZoneIdentifier", &identifier),
        ];
        for (position, zone) in row.zones_of.iter().enumerate() {
            let (member, value) = if row.by_id {
                (
                    format!("AvailabilityZoneIds.member.{}", position + 1),
                    catalog[*zone].zone_id.clone(),
                )
            } else {
                (
                    format!("AvailabilityZones.member.{}", position + 1),
                    catalog[*zone].zone_name.clone(),
                )
            };
            params.push(p(&member, value));
        }
        let (status, body) = create_group(&router, &params).await;
        if row.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: CreateAutoScalingGroup expected 400, got {status}: {body}",
            row.case
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            ZONE_MISMATCH,
            "case {:?}: expected the placement-mismatch refusal, got {body}",
            row.case
        );
        let read = describe(&router, &name).await;
        assert!(
            tag_values(&read, "AutoScalingGroupName").is_empty(),
            "case {:?}: a refused create must leave no group behind, got {read}",
            row.case
        );
    }

    // The same rule on the UPDATE, which writes the same group: a refused update leaves
    // the group's zones exactly as they were.
    let name = "agreement-update";
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", "lt-agreement"),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
    ];
    let (status, body) = create_group(&router, &params).await;
    assert_eq!(status, 200, "the update row's group: got {status}: {body}");

    let (status, body) = query(
        &router,
        "UpdateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("AvailabilityZones.member.1", catalog[0].zone_name.as_str()),
            ("VPCZoneIdentifier", subnets[1].id.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "an update whose subnet is in another zone: got {status}: {body}"
    );
    assert_eq!(
        xml_unescape(&tag_value(&body, "Message")),
        ZONE_MISMATCH,
        "the update must answer the placement-mismatch refusal, got {body}"
    );
    let read = describe(&router, name).await;
    assert_eq!(
        tag_value(&read, "AvailabilityZones"),
        format!("<member>{}</member>", catalog[0].zone_name),
        "a refused update must leave the group's zones alone, got {read}"
    );
    assert_eq!(
        tag_value(&read, "VPCZoneIdentifier"),
        "",
        "a refused update must store none of its subnets, got {read}"
    );

    // And the consistent update is served, moving the group to the subnet it named.
    let (status, body) = query(
        &router,
        "UpdateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("AvailabilityZones.member.1", catalog[0].zone_name.as_str()),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "a consistent update: got {status}: {body}");
    let read = describe(&router, name).await;
    assert_eq!(
        tag_value(&read, "VPCZoneIdentifier"),
        subnets[0].id,
        "a served update stores the subnets it named, got {read}"
    );
}

/// The sentence a request stating a negative `HealthCheckGracePeriod` is refused with:
/// fixed text, with no part of the request in it.
const GRACE_PERIOD_REFUSAL: &str = "Grace period must be a positive integer.";

/// A `HealthCheckGracePeriod` below zero is refused, on the create and on the update, and
/// the group keeps the value it had.
#[tokio::test]
async fn a_negative_health_check_grace_period_is_refused() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-grace", &[]).await;

    // Both published bounds of the rule and values either side of each: zero is the least
    // value a group may hold, and everything under it is refused however far under.
    let cases: [(i32, bool); 6] = [
        (-1, false),
        (-2, false),
        (-300, false),
        (0, true),
        (1, true),
        (300, true),
    ];

    for (index, (value, accepted)) in cases.iter().enumerate() {
        let name = format!("grace-{index}");
        let params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-grace"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
            p("HealthCheckGracePeriod", value.to_string()),
        ];
        let (status, body) = create_group(&router, &params).await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {value}: CreateAutoScalingGroup expected 200, got {status}: {body}"
            );
            let read = describe(&router, &name).await;
            assert_eq!(
                tag_value(&read, "HealthCheckGracePeriod"),
                value.to_string(),
                "case {value}: the group must report the grace period it was created with, \
                 got {read}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {value}: CreateAutoScalingGroup expected 400, got {status}: {body}"
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            GRACE_PERIOD_REFUSAL,
            "case {value}: expected the bound refusal, got {body}"
        );
        let read = describe(&router, &name).await;
        assert!(
            tag_values(&read, "AutoScalingGroupName").is_empty(),
            "case {value}: a refused create must leave no group behind, got {read}"
        );
    }

    // The update writes the same member of the same group, so the same bound holds — and a
    // refused update leaves the value the group already had.
    let name = "grace-update";
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", "lt-grace"),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
        p("HealthCheckGracePeriod", "120"),
    ];
    let (status, body) = create_group(&router, &params).await;
    assert_eq!(status, 200, "the update row's group: got {status}: {body}");

    for (value, accepted) in cases {
        let (status, body) = query(
            &router,
            "UpdateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", name),
                ("HealthCheckGracePeriod", value.to_string().as_str()),
            ],
        )
        .await;
        let read = describe(&router, name).await;
        if accepted {
            assert_eq!(
                status, 200,
                "update case {value}: expected 200, got {status}: {body}"
            );
            assert_eq!(
                tag_value(&read, "HealthCheckGracePeriod"),
                value.to_string(),
                "update case {value}: the group must report the value it was updated to, \
                 got {read}"
            );
            // Put it back, so the next refused row has a non-default value to keep.
            let (status, body) = query(
                &router,
                "UpdateAutoScalingGroup",
                &[
                    ("AutoScalingGroupName", name),
                    ("HealthCheckGracePeriod", "120"),
                ],
            )
            .await;
            assert_eq!(status, 200, "restoring the grace period: got {status}: {body}");
            continue;
        }
        assert_eq!(
            status, 400,
            "update case {value}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            GRACE_PERIOD_REFUSAL,
            "update case {value}: expected the bound refusal, got {body}"
        );
        assert_eq!(
            tag_value(&read, "HealthCheckGracePeriod"),
            "120",
            "update case {value}: a refused update must leave the stored value alone, got {read}"
        );
    }
}

/// One row of the standby table.
struct StandbyRow {
    case: &'static str,
    /// How many instances the group holds before the park.
    capacity: i32,
    /// How many of them the request parks.
    parking: usize,
    /// Whether the request asks for the desired capacity to be decremented.
    decrement: bool,
}

/// `EnterStandby` parks the instances it names and `ExitStandby` returns them, with the
/// capacity each operation documents — and a parked instance reads back `Standby`.
#[tokio::test]
async fn standby_parks_instances_and_exit_returns_them() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-standby", &[]).await;

    let rows = [
        StandbyRow {
            case: "one of one, decrementing",
            capacity: 1,
            parking: 1,
            decrement: true,
        },
        StandbyRow {
            case: "one of one, not decrementing: a replacement is launched",
            capacity: 1,
            parking: 1,
            decrement: false,
        },
        StandbyRow {
            case: "one of three, decrementing",
            capacity: 3,
            parking: 1,
            decrement: true,
        },
        StandbyRow {
            case: "two of three, not decrementing",
            capacity: 3,
            parking: 2,
            decrement: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("standby-{index}");
        let params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-standby"),
            // The group floats on the launch template's latest version, so the version an
            // instance reports can only come from resolving it at the launch.
            p("LaunchTemplate.Version", "$Latest"),
            p("MinSize", "0"),
            p("MaxSize", "10"),
            p("DesiredCapacity", row.capacity.to_string()),
            p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
        ];
        let (status, body) = create_group(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );
        let in_service = instances_in_state(
            &router,
            &name,
            "InService",
            usize::try_from(row.capacity).expect("a small capacity"),
        )
        .await;
        let parking: Vec<String> = in_service.iter().take(row.parking).cloned().collect();

        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p(
                "ShouldDecrementDesiredCapacity",
                if row.decrement { "true" } else { "false" },
            ),
        ];
        for (position, instance_id) in parking.iter().enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", position + 1),
                instance_id.clone(),
            ));
        }
        let (status, body) = query(&router, "EnterStandby", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: EnterStandby expected 200, got {status}: {body}",
            row.case
        );
        // One activity per instance parked, each naming the instance it moved.
        let descriptions = tag_values(&body, "Description");
        assert_eq!(
            descriptions.len(),
            row.parking,
            "case {:?}: expected one activity per parked instance, got {body}",
            row.case
        );
        for instance_id in &parking {
            assert!(
                descriptions
                    .iter()
                    .any(|description| description.contains(instance_id)),
                "case {:?}: no activity names {instance_id}, got {body}",
                row.case
            );
        }

        let parked = instances_in_state(&router, &name, "Standby", row.parking).await;
        assert_eq!(
            parked.len(),
            row.parking,
            "case {:?}: expected {} parked instance(s), got {parked:?}",
            row.case,
            row.parking
        );
        for instance_id in &parking {
            assert!(
                parked.contains(instance_id),
                "case {:?}: {instance_id} was not parked, got {parked:?}",
                row.case
            );
        }
        // The capacity rule each form of the request documents: the decrement takes it
        // down by the instances parked, and without it the group launches replacements and
        // holds its capacity in service.
        let expected_capacity = if row.decrement {
            row.capacity - i32::try_from(row.parking).expect("a small count")
        } else {
            row.capacity
        };
        let body = describe(&router, &name).await;
        assert_eq!(
            tag_value(&body, "DesiredCapacity"),
            expected_capacity.to_string(),
            "case {:?}: expected a desired capacity of {expected_capacity}, got {body}",
            row.case
        );
        let still_serving = instances_in_state(
            &router,
            &name,
            "InService",
            usize::try_from(expected_capacity).expect("a small capacity"),
        )
        .await;
        assert_eq!(
            still_serving.len(),
            usize::try_from(expected_capacity).expect("a small capacity"),
            "case {:?}: a parked instance does not meet the group's capacity, got \
             {still_serving:?}",
            row.case
        );

        // Every instance reports the zone ID of the zone it is in, and the launch-template
        // VERSION it ran — never the `$Latest` word the group floats on. The group's own
        // specification is the first `Version` of the document; the instances' follow it.
        let body = describe(&router, &name).await;
        let zone_ids = tag_values(&body, "AvailabilityZoneId");
        assert!(
            !zone_ids.is_empty() && zone_ids.iter().all(|id| *id == catalog[0].zone_id),
            "case {:?}: every instance must report the zone id {}, got {zone_ids:?} in {body}",
            row.case,
            catalog[0].zone_id
        );
        let versions = tag_values(&body, "Version");
        assert_eq!(
            versions.first().map(String::as_str),
            Some("$Latest"),
            "case {:?}: the group reports the specification it was given, got {body}",
            row.case
        );
        for version in versions.iter().skip(1) {
            assert!(
                version.parse::<i64>().is_ok(),
                "case {:?}: an instance must report the version it launched from, got \
                 {version:?} in {body}",
                row.case
            );
        }

        // And the return: the parked instances come back into service and the capacity
        // rises with them.
        let mut params = vec![p("AutoScalingGroupName", &name)];
        for (position, instance_id) in parking.iter().enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", position + 1),
                instance_id.clone(),
            ));
        }
        let (status, body) = query(&router, "ExitStandby", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: ExitStandby expected 200, got {status}: {body}",
            row.case
        );
        let returned = expected_capacity + i32::try_from(row.parking).expect("a small count");
        let serving = instances_in_state(
            &router,
            &name,
            "InService",
            usize::try_from(returned).expect("a small capacity"),
        )
        .await;
        for instance_id in &parking {
            assert!(
                serving.contains(instance_id),
                "case {:?}: {instance_id} did not come back into service, got {serving:?}",
                row.case
            );
        }
        let body = describe(&router, &name).await;
        assert_eq!(
            tag_value(&body, "DesiredCapacity"),
            returned.to_string(),
            "case {:?}: the capacity rises with the instances returned, got {body}",
            row.case
        );
        assert!(
            instances_in_state(&router, &name, "Standby", 0).await.is_empty(),
            "case {:?}: no instance stays parked after the return, got {body}",
            row.case
        );
    }
}

/// A standby move the group cannot make is refused, and every instance stays where it was.
#[tokio::test]
async fn a_standby_move_the_group_cannot_make_is_refused() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-standby-refusal", &[]).await;

    let name = "standby-refusal";
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", "lt-standby-refusal"),
        p("MinSize", "1"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "1"),
        p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
    ];
    let (status, body) = create_group(&router, &params).await;
    assert_eq!(status, 200, "the refusal row's group: got {status}: {body}");
    let in_service = instances_in_state(&router, name, "InService", 1).await;
    let instance_id = in_service[0].clone();

    // An id the group does not hold.
    let (status, body) = query(
        &router,
        "EnterStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", "i-00000000000000099"),
            ("ShouldDecrementDesiredCapacity", "false"),
        ],
    )
    .await;
    assert_eq!(status, 400, "an id of no instance of the group: got {status}: {body}");
    assert_eq!(
        xml_unescape(&tag_value(&body, "Message")),
        format!("The instance i-00000000000000099 is not part of Auto Scaling group {name}."),
        "the refusal must name the instance and the group, got {body}"
    );

    // The decrement would take the capacity below the group's own MinSize.
    let (status, body) = query(
        &router,
        "EnterStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", instance_id.as_str()),
            ("ShouldDecrementDesiredCapacity", "true"),
        ],
    )
    .await;
    assert_eq!(status, 400, "a decrement below MinSize: got {status}: {body}");
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert!(
        message.contains("below the minimum capacity of the group (1)"),
        "the refusal must state the bound it crossed, got {message:?}"
    );

    // An instance not in Standby cannot be returned from it.
    let (status, body) = query(
        &router,
        "ExitStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", instance_id.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 400, "a return of an in-service instance: got {status}: {body}");
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert_eq!(
        message,
        format!("The instance {instance_id} is not in Standby."),
        "the refusal must state the state the return requires, got {message:?}"
    );

    // More instance ids than one call may name.
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("ShouldDecrementDesiredCapacity", "false"),
    ];
    for position in 1..=21 {
        params.push(p(
            &format!("InstanceIds.member.{position}"),
            format!("i-{position:017}"),
        ));
    }
    let (status, body) = query(&router, "EnterStandby", &borrowed(&params)).await;
    assert_eq!(status, 400, "21 instance ids: got {status}: {body}");
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert!(
        message.contains("'instanceIds'") && message.contains("less than or equal to 20"),
        "the refusal must name the member and its bound, got {message:?}"
    );

    // Every refusal left the one instance of the group exactly where it was.
    let serving = instances_in_state(&router, name, "InService", 1).await;
    assert_eq!(
        serving,
        vec![instance_id],
        "a refused standby move must leave the group's instances alone, got {serving:?}"
    );
}

/// One row of the refresh-preference table.
struct PreferenceRow {
    case: &'static str,
    /// Group members that decide a documented fallback.
    group_params: Vec<(&'static str, &'static str)>,
    /// The `Preferences.*` members the request states.
    preferences: Vec<(&'static str, &'static str)>,
    /// Whether the request carries a desired configuration, which is what makes a rollback
    /// supported at all.
    desired_configuration: bool,
    expected_min_healthy: &'static str,
    expected_warmup: &'static str,
    expected_skip_matching: &'static str,
    expected_auto_rollback: &'static str,
}

/// A running instance refresh reports the preferences IN FORCE for it: each member the
/// request left out at its documented default, and `AutoRollback` at the effective value.
#[tokio::test]
async fn an_instance_refresh_reports_its_effective_preferences() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-preferences", &[]).await;
    create_template(&router, "lt-preferences-next", &[]).await;

    let rows = [
        PreferenceRow {
            case: "no preferences at all: every documented default",
            group_params: Vec::new(),
            preferences: Vec::new(),
            desired_configuration: false,
            expected_min_healthy: "90",
            expected_warmup: "0",
            expected_skip_matching: "false",
            expected_auto_rollback: "false",
        },
        PreferenceRow {
            case: "the group's own instance maintenance policy sets the minimum",
            group_params: vec![
                ("InstanceMaintenancePolicy.MinHealthyPercentage", "50"),
                ("InstanceMaintenancePolicy.MaxHealthyPercentage", "150"),
            ],
            preferences: Vec::new(),
            desired_configuration: false,
            expected_min_healthy: "50",
            expected_warmup: "0",
            expected_skip_matching: "false",
            expected_auto_rollback: "false",
        },
        PreferenceRow {
            case: "the group's DefaultInstanceWarmup is the warmup",
            group_params: vec![("DefaultInstanceWarmup", "120")],
            preferences: Vec::new(),
            desired_configuration: false,
            expected_min_healthy: "90",
            expected_warmup: "120",
            expected_skip_matching: "false",
            expected_auto_rollback: "false",
        },
        PreferenceRow {
            case: "with no DefaultInstanceWarmup, the HealthCheckGracePeriod is",
            group_params: vec![("HealthCheckGracePeriod", "300")],
            preferences: Vec::new(),
            desired_configuration: false,
            expected_min_healthy: "90",
            expected_warmup: "300",
            expected_skip_matching: "false",
            expected_auto_rollback: "false",
        },
        PreferenceRow {
            case: "stated preferences are kept as written",
            group_params: Vec::new(),
            preferences: vec![
                ("Preferences.MinHealthyPercentage", "75"),
                ("Preferences.InstanceWarmup", "45"),
                ("Preferences.SkipMatching", "true"),
            ],
            desired_configuration: false,
            expected_min_healthy: "75",
            expected_warmup: "45",
            expected_skip_matching: "true",
            expected_auto_rollback: "false",
        },
        PreferenceRow {
            case: "AutoRollback asked for with a desired configuration IS in force",
            group_params: Vec::new(),
            preferences: vec![("Preferences.AutoRollback", "true")],
            desired_configuration: true,
            expected_min_healthy: "90",
            expected_warmup: "0",
            expected_skip_matching: "false",
            expected_auto_rollback: "true",
        },
        PreferenceRow {
            case: "AutoRollback left out with a desired configuration is still false",
            group_params: Vec::new(),
            preferences: Vec::new(),
            desired_configuration: true,
            expected_min_healthy: "90",
            expected_warmup: "0",
            expected_skip_matching: "false",
            expected_auto_rollback: "false",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("preferences-{index}");
        let mut params = vec![
            p("AutoScalingGroupName", &name),
            p("LaunchTemplate.LaunchTemplateName", "lt-preferences"),
            // A pinned version, so a rollback is not refused for a floating one.
            p("LaunchTemplate.Version", "1"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
        ];
        for (member, value) in &row.group_params {
            params.push(p(member, *value));
        }
        let (status, body) = create_group(&router, &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        let mut params = vec![p("AutoScalingGroupName", &name)];
        for (member, value) in &row.preferences {
            params.push(p(member, *value));
        }
        if row.desired_configuration {
            params.push(p(
                "DesiredConfiguration.LaunchTemplate.LaunchTemplateName",
                "lt-preferences-next",
            ));
            params.push(p("DesiredConfiguration.LaunchTemplate.Version", "1"));
        }
        let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: StartInstanceRefresh expected 200, got {status}: {body}",
            row.case
        );

        let (status, body) = query(
            &router,
            "DescribeInstanceRefreshes",
            &[("AutoScalingGroupName", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeInstanceRefreshes expected 200, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "MinHealthyPercentage"),
            row.expected_min_healthy,
            "case {:?}: expected MinHealthyPercentage {}, got {body}",
            row.case,
            row.expected_min_healthy
        );
        assert_eq!(
            tag_value(&body, "InstanceWarmup"),
            row.expected_warmup,
            "case {:?}: expected InstanceWarmup {}, got {body}",
            row.case,
            row.expected_warmup
        );
        assert_eq!(
            tag_value(&body, "SkipMatching"),
            row.expected_skip_matching,
            "case {:?}: expected SkipMatching {}, got {body}",
            row.case,
            row.expected_skip_matching
        );
        assert_eq!(
            tag_value(&body, "AutoRollback"),
            row.expected_auto_rollback,
            "case {:?}: expected AutoRollback {}, got {body}",
            row.case,
            row.expected_auto_rollback
        );
        // The alarms the refresh fails on: an empty specification for a refresh that
        // watches none, never an absent member.
        assert!(
            body.contains("<AlarmSpecification/>") || body.contains("<AlarmSpecification>"),
            "case {:?}: the refresh must report an AlarmSpecification, got {body}",
            row.case
        );
    }
}

/// A rollback the service cannot perform is not a preference it honours: `AutoRollback`
/// asked for with no desired configuration is REFUSED, and no refresh is started.
#[tokio::test]
async fn auto_rollback_without_a_desired_configuration_is_refused() {
    let (router, _engine) = emulator();
    let catalog = zones();
    create_template(&router, "lt-auto-rollback", &[]).await;

    let name = "auto-rollback";
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", "lt-auto-rollback"),
        p("LaunchTemplate.Version", "1"),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
    ];
    let (status, body) = create_group(&router, &params).await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");

    let (status, body) = query(
        &router,
        "StartInstanceRefresh",
        &[
            ("AutoScalingGroupName", name),
            ("Preferences.AutoRollback", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "AutoRollback with no desired configuration: got {status}: {body}"
    );
    assert_eq!(
        xml_unescape(&tag_value(&body, "Message")),
        "The request isn\u{2019}t valid. The AutoRollback parameter cannot be set to true when \
         the DesiredConfiguration parameter is empty. Set AutoRollback to false or specify a \
         desired configuration and try again.",
        "expected the AutoRollback refusal, got {body}"
    );

    // Nothing was started: a refused request leaves the group with no refresh.
    let (status, body) = query(
        &router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(status, 200, "DescribeInstanceRefreshes: got {status}: {body}");
    assert!(
        tag_values(&body, "InstanceRefreshId").is_empty(),
        "a refused StartInstanceRefresh must start no refresh, got {body}"
    );

    // And asking for it WITH a desired configuration is served, with the preference in
    // force exactly as asked.
    let (status, body) = query(
        &router,
        "StartInstanceRefresh",
        &[
            ("AutoScalingGroupName", name),
            ("Preferences.AutoRollback", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "AutoRollback false with no desired configuration: got {status}: {body}"
    );
}

/// An instance profile no row creates, so no account in this suite holds one.
const ABSENT_PROFILE: &str = "asg-launch-profile-never-created";

/// A launch is made only from a configuration that can produce an instance: a group
/// floating on its launch template's latest version launches from whatever that version
/// then says, so a version naming an instance profile the account does not hold FAILS the
/// launch instead of giving the group an instance that could not exist.
#[tokio::test]
async fn a_launch_from_an_unsatisfiable_configuration_places_nothing() {
    let (router, _engine) = emulator();
    let catalog = zones();

    // The account holds this profile, so the first version of the template resolves.
    let (status, body) = iam_query(
        &router,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", "asg-launch-profile")],
    )
    .await;
    assert_eq!(status, 200, "CreateInstanceProfile: got {status}: {body}");
    create_template(
        &router,
        "lt-launch-failure",
        &[p(
            "LaunchTemplateData.IamInstanceProfile.Name",
            "asg-launch-profile",
        )],
    )
    .await;

    let name = "launch-failure";
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", "lt-launch-failure"),
        p("LaunchTemplate.Version", "$Latest"),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "1"),
        p("AvailabilityZones.member.1", catalog[0].zone_name.clone()),
    ];
    let (status, body) = create_group(&router, &params).await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");
    let first = instances_in_state(&router, name, "InService", 1).await;

    // A NEW version of the same template, naming a profile the account does not hold. The
    // group floats on `$Latest`, so this is what its next launch reads.
    let (status, body) = ec2_query(
        &router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateName", "lt-launch-failure"),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            p("LaunchTemplateData.IamInstanceProfile.Name", ABSENT_PROFILE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplateVersion: got {status}: {body}"
    );

    // The group is asked for one more unit of capacity: the launch it owes reads the new
    // version, which it cannot launch from.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", name),
            ("DesiredCapacity", "2"),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(status, 200, "SetDesiredCapacity: got {status}: {body}");

    // The group holds the instance it already had and no second one: nothing was placed.
    let body = describe(&router, name).await;
    assert_eq!(
        tag_value(&body, "DesiredCapacity"),
        "2",
        "the group records the capacity it was asked for, got {body}"
    );
    assert_eq!(
        tag_values(&body, "InstanceId"),
        first,
        "a failed launch must give the group no instance, got {body}"
    );

    // And the failed launch says WHY, on the activity a client reads it off.
    let (status, body) = query(
        &router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(status, 200, "DescribeScalingActivities: got {status}: {body}");
    let status_codes = tag_values(&body, "StatusCode");
    assert!(
        status_codes.iter().any(|code| code == "Failed"),
        "the failed launch must be recorded as a failed activity, got {status_codes:?} in {body}"
    );
    let messages: Vec<String> = tag_values(&body, "StatusMessage")
        .iter()
        .map(|message| xml_unescape(message))
        .collect();
    assert!(
        messages.iter().any(|message| {
            message == "Invalid IAM Instance Profile name. Launching EC2 instance failed."
        }),
        "the activity must state the reason the launch failed, got {messages:?} in {body}"
    );
}
