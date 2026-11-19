// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the CONDITION an EC2 refusal names, driven over the wire the service
//! actually speaks — `ec2Query`, an `application/x-www-form-urlencoded` POST answered with
//! XML (README.md / OPERATIONS.md "Wire protocol"), through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! A client BRANCHES on the error code, so a refusal has to classify what went wrong rather
//! than merely say no: "fix the id you sent" and "the resource that id names is gone" are
//! different repairs, and so are "that parameter cannot be served" and "that resource does
//! not exist". Six rules are under test, each over a table of inputs so that no single value
//! is the specification:
//!
//! 1. A LAUNCH THAT TARGETS A CAPACITY RESERVATION IS HELD TO ITS ATTRIBUTES: a reservation
//!    serves an instance only when it "has matching attributes (instance type, platform,
//!    Availability Zone, and tenancy)" (`CapacityReservationPreference`), so a launch naming
//!    a type the targeted reservation did not reserve is `InvalidParameterValue` and nothing
//!    is minted, while the matching launch is served. An id EC2 could have issued that names
//!    no reservation stays `InvalidCapacityReservationId.NotFound`, and one it could never
//!    have issued `InvalidCapacityReservationId.Malformed`; a launch the reservation has no
//!    capacity for — another zone, or more instances than it holds — is
//!    `ReservationCapacityExceeded`, and one naming a preference beside the target is
//!    `InvalidParameterCombination`.
//! 2. AN INSTANCE WHOSE TYPE CANNOT SERVE THE OPERATION IS `InvalidInstanceType`: the
//!    instance resolved, so the id is not what the caller got wrong — its instance family
//!    cannot do what was asked, which is a different repair (launch another instance) from
//!    fixing an id.
//! 3. A ROUTE TO A NAT GATEWAY THE ACCOUNT DOES NOT HOLD is reported by the MEMBER that
//!    carried it, in the route family's `InvalidNatGatewayID.NotFound`, while the NAT gateway
//!    family's OWN operations keep their `NatGatewayNotFound` for the same id.
//! 4. A NETWORK THAT ALREADY HAS AN INTERNET GATEWAY refuses the `VpcId` it was given
//!    (`InvalidParameterValue`), where a GATEWAY that is already attached somewhere is the
//!    association conflict `Resource.AlreadyAssociated` naming the pair that holds.
//! 5. THE START OF A PATH ANALYSIS answers the refusal the path id's own FORM calls for: a
//!    value EC2 could never have issued a path id as is `InvalidParameterValue` naming the
//!    member, and an id in a form EC2 does issue is the path's own
//!    `InvalidNetworkInsightsPathId.NotFound` — which is also what the describes, which SELECT
//!    paths by a list of ids at the request boundary, keep for every value.
//! 6. AN ATTACHMENT ID OF THE PRINTED ID FORM IS LOOKED UP: one that names no attachment is
//!    `InvalidAttachmentID.NotFound`, so a caller cleaning up reads "already detached", and
//!    only a value OUTSIDE that form is
//!    `InvalidNetworkInterfaceAttachmentId.Malformed`.
//!
//! Every instance type here comes from the modelled `InstanceType` catalog
//! (`ec2::state::offered_instance_types`) and every Mac family split from the model's own
//! architecture reading, so nothing below is a list written by hand.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use ec2::ops::types::CAPACITY_RESERVATION_PREFERENCE_VALUES;
use ec2::state::{
    architecture_for, instance_family_of, is_mac_instance_family, offered_instance_types,
    ComAmazonawsEc2Amazonec2State as Ec2State, NetworkInsightsPathSpec, ARCHITECTURE_ARM64_MAC,
    CAPACITY_RESERVATION_PREFERENCE_ONLY, INSTANCE_TYPE_NOT_SUPPORTED_MESSAGE,
    INVALID_ATTACHMENT_ID_NOT_FOUND, INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
    INVALID_CAPACITY_RESERVATION_ID_NOT_FOUND, INVALID_INSTANCE_TYPE,
    INVALID_NAT_GATEWAY_ID_NOT_FOUND, INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
    INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED, INVALID_PARAMETER_COMBINATION,
    INVALID_PARAMETER_VALUE, NAT_GATEWAY_NOT_FOUND, RESERVATION_CAPACITY_EXCEEDED,
    RESOURCE_ALREADY_ASSOCIATED,
};

use super::ec2_query::{element, elements, error_code, filters, unescaped};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The Availability Zone of this Region the reservations below reserve capacity in.
const ZONE: &str = "us-west-2a";

/// An AMI id of the long form, for the launches these rows need.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The emulator the tests drive, with the engine beside it so a suite can seed a store a
/// handler reads.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The `<Message>` of a refusal body, with the XML escapes a sentence's quotes arrive in
/// read back.
fn error_message(body: &str) -> String {
    unescaped(&element(body, "Message").unwrap_or_default())
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The status and code of a call that has to be refused with HTTP 400.
async fn refused(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
    );
    body
}

// ---------------------------------------------------------------------------
// RULE 1: a launch that targets a Capacity Reservation
// ---------------------------------------------------------------------------

/// A Capacity Reservation for `instance_count` instances of `instance_type` in [`ZONE`], as
/// its id.
async fn reserve(
    case: &str,
    router: &MultiServiceRouter,
    instance_type: &str,
    instance_count: i32,
) -> String {
    reserve_in(case, router, instance_type, instance_count, ZONE).await
}

/// The same reservation in `zone` — a reservation reserves capacity in ONE Availability
/// Zone, so which zone it is in is a row of its own.
async fn reserve_in(
    case: &str,
    router: &MultiServiceRouter,
    instance_type: &str,
    instance_count: i32,
    zone: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateCapacityReservation",
        &[
            param("InstanceType", instance_type),
            param("InstancePlatform", "Linux/UNIX"),
            param("InstanceCount", &instance_count.to_string()),
            param("AvailabilityZone", zone),
        ],
    )
    .await;
    member(case, &body, "capacityReservationId")
}

/// The form fields of a one-instance launch of `instance_type` that targets
/// `capacity_reservation_id`.
fn targeted_launch(instance_type: &str, capacity_reservation_id: &str) -> Vec<Param> {
    vec![
        param("ImageId", IMAGE_ID),
        param("InstanceType", instance_type),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        param(
            "CapacityReservationSpecification.CapacityReservationTarget.CapacityReservationId",
            capacity_reservation_id,
        ),
    ]
}

/// How many instances the account holds, as `DescribeInstances` reports them.
async fn instance_count(case: &str, router: &MultiServiceRouter) -> usize {
    let body = ok(case, router, "DescribeInstances", &[]).await;
    elements(&body, "instanceId").len()
}

/// The `DescribeCapacityReservations` body for the one reservation
/// `capacity_reservation_id` names.
async fn described_reservation(
    case: &str,
    router: &MultiServiceRouter,
    capacity_reservation_id: &str,
) -> String {
    ok(
        case,
        router,
        "DescribeCapacityReservations",
        &[param("CapacityReservationId.1", capacity_reservation_id)],
    )
    .await
}

/// RULE 1's combination half: "you can only specify `CapacityReservationPreference` and
/// `CapacityReservationTarget` if the `CapacityReservationPreference` is
/// `capacity-reservations-only`" (`CapacityReservationSpecification`), so a launch naming a
/// target beside any OTHER preference names two placements at once and is refused
/// `InvalidParameterCombination` — before the target is resolved, so the refusal is about the
/// members rather than about the id they carried — while the one preference the pair is
/// documented for is served.
///
/// The rows are the preferences the model declares
/// (`smithy_query(query="shape",shape="CapacityReservationPreference")`), each against a
/// target, so every member of that catalog is covered rather than one of them.
#[tokio::test]
async fn a_launch_that_names_a_preference_beside_a_target_is_refused_as_a_combination() {
    let (router, _engine) = emulator();
    let case = "the reservation";
    let capacity_reservation_id = reserve(case, &router, "t3.micro", 2).await;

    for preference in CAPACITY_RESERVATION_PREFERENCE_VALUES {
        let mut params = targeted_launch("t3.micro", &capacity_reservation_id);
        params.push(param(
            "CapacityReservationSpecification.CapacityReservationPreference",
            preference,
        ));
        if *preference == CAPACITY_RESERVATION_PREFERENCE_ONLY {
            // The one preference the pair is documented for: the launch is served, and the
            // instance reports the preference it asked for.
            let served = ok(preference, &router, "RunInstances", &params).await;
            assert_eq!(
                elements(&served, "capacityReservationPreference"),
                vec![(*preference).to_string()],
                "case {preference:?}: expected the launch to be served, got {served}"
            );
            continue;
        }
        let body = refused(preference, &router, "RunInstances", &params).await;
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_COMBINATION,
            "case {preference:?}: expected {INVALID_PARAMETER_COMBINATION}, got {body}"
        );
        for named in ["capacityReservationPreference", "capacityReservationTarget"] {
            assert!(
                error_message(&body).contains(named),
                "case {preference:?}: expected the refusal to name {named}, got {}",
                error_message(&body)
            );
        }
    }

    // The combination is refused BEFORE the target is resolved: an id EC2 could never have
    // issued is answered for the combination rather than for the id.
    let mut params = targeted_launch("t3.micro", "cr-00000000000000000");
    params.push(param(
        "CapacityReservationSpecification.CapacityReservationPreference",
        "open",
    ));
    let body = refused("an unissuable id beside a preference", &router, "RunInstances", &params).await;
    assert_eq!(
        error_code(&body),
        INVALID_PARAMETER_COMBINATION,
        "the combination must be refused before the target is resolved, got {body}"
    );
}

/// RULE 1: a launch that explicitly targets a Capacity Reservation must ask for the instance
/// type that reservation holds; one that asks for another type is refused as a bad parameter
/// value, naming both types, and mints nothing. The launch the reservation does hold RUNS IN
/// it: the instance reports it, the documented filter selects the instance by it, the
/// reservation reports the capacity in use, and terminating the instance gives it back.
///
/// The rows differ in the reserved type and in the type the launch asks for, across instance
/// families and sizes, so no one pair is the specification; each row's matching launch is
/// served through the same target, which is what makes the refusal about the MISMATCH rather
/// than about targeting at all.
#[tokio::test]
async fn a_launch_into_a_targeted_capacity_reservation_must_match_its_instance_type() {
    let (router, _engine) = emulator();

    // (case, the type reserved, a DIFFERENT type the launch asks for)
    let cases = [
        ("one family, two sizes", "t3.micro", "t3.small"),
        ("two families, one size", "m5.large", "c5.large"),
        ("a bigger size of the reserved family", "m5.large", "m5.xlarge"),
    ];
    // How many instances each row's reservation holds capacity for: more than the one launch
    // each row makes, so the capacity it takes is visible as a DROP rather than as the whole
    // reservation.
    let reserved_count = 2;

    for (case, reserved, requested) in cases {
        let capacity_reservation_id = reserve(case, &router, reserved, reserved_count).await;
        let before = instance_count(case, &router).await;

        let body = refused(
            case,
            &router,
            "RunInstances",
            &targeted_launch(requested, &capacity_reservation_id),
        )
        .await;
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE} for a launch of {requested} \
             into a reservation of {reserved}, got {body}"
        );
        let message = error_message(&body);
        for named in [capacity_reservation_id.as_str(), reserved, requested] {
            assert!(
                message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {message:?}"
            );
        }
        assert_eq!(
            instance_count(case, &router).await,
            before,
            "case {case:?}: a refused launch must mint nothing"
        );

        // The same target, asking for the type the reservation holds, is served.
        let served = ok(
            case,
            &router,
            "RunInstances",
            &targeted_launch(reserved, &capacity_reservation_id),
        )
        .await;
        assert_eq!(
            elements(&served, "instanceType"),
            vec![reserved.to_string()],
            "case {case:?}: the matching launch must mint an instance of {reserved}, got {served}"
        );
        // The instance it minted RUNS IN the reservation it targeted: it reports that
        // reservation, and the documented filter selects it by it.
        let instance_id = member(case, &served, "instanceId");
        assert_eq!(
            elements(&served, "capacityReservationId"),
            vec![capacity_reservation_id.clone(), capacity_reservation_id.clone()],
            "case {case:?}: expected the instance to report the reservation it targeted, both \
             as its own member and as the target it named, got {served}"
        );
        let selected = ok(
            case,
            &router,
            "DescribeInstances",
            &filters(&[(
                "capacity-reservation-id",
                &[capacity_reservation_id.as_str()],
            )]),
        )
        .await;
        assert_eq!(
            elements(&selected, "instanceId"),
            vec![instance_id.clone()],
            "case {case:?}: expected the filter to select the instance in {capacity_reservation_id}, \
             got {selected}"
        );

        // Running in the reservation TAKES its capacity: the reservation reports one fewer
        // instance available, the `used` allocation for the one running in it, and the
        // owner's usage — and terminating the instance gives that capacity back.
        let reserved_total = reserved_count.to_string();
        let in_use = described_reservation(case, &router, &capacity_reservation_id).await;
        assert_eq!(
            (
                element(&in_use, "totalInstanceCount"),
                element(&in_use, "availableInstanceCount"),
                element(&in_use, "allocationType"),
                element(&in_use, "count"),
            ),
            (
                Some(reserved_total.clone()),
                Some((reserved_count - 1).to_string()),
                Some("used".to_string()),
                Some("1".to_string()),
            ),
            "case {case:?}: expected the reservation to report the instance running in it, \
             got {in_use}"
        );
        let usage = ok(
            case,
            &router,
            "GetCapacityReservationUsage",
            &[param("CapacityReservationId", &capacity_reservation_id)],
        )
        .await;
        assert_eq!(
            element(&usage, "usedInstanceCount"),
            Some("1".to_string()),
            "case {case:?}: expected the usage report to count the instance, got {usage}"
        );

        ok(
            case,
            &router,
            "TerminateInstances",
            &[param("InstanceId.1", &instance_id)],
        )
        .await;
        let freed = described_reservation(case, &router, &capacity_reservation_id).await;
        assert_eq!(
            (
                element(&freed, "availableInstanceCount"),
                element(&freed, "allocationType"),
            ),
            (Some(reserved_total.clone()), None),
            "case {case:?}: expected the terminated instance to free its capacity, got {freed}"
        );
    }
}

/// RULE 1's capacity half: the capacity a reservation holds is capacity in ONE Availability
/// Zone, so a launch of the reserved type placed in another zone, and one asking for more
/// instances than the reservation holds, are each refused `ReservationCapacityExceeded` and
/// mint nothing — where the launch placed in the reservation's own zone, for a count it
/// covers, is served.
///
/// The rows differ in the reserved count and in which of the two ways the request exceeds
/// what the reservation holds.
#[tokio::test]
async fn a_launch_a_targeted_capacity_reservation_cannot_hold_exceeds_its_capacity() {
    let (router, _engine) = emulator();
    // A second zone of the same Region, which the reservation does not reserve capacity in.
    let other_zone = "us-west-2b";

    // (case, the reserved type, how many instances are reserved, how many the launch asks
    // for, the zone the launch is placed in)
    let cases = [
        ("one instance too many", "t3.micro", 1, 2, None),
        ("three instances for a reservation of two", "m5.large", 2, 3, None),
        (
            "the reserved count, in another zone",
            "t3.small",
            2,
            2,
            Some(other_zone),
        ),
        (
            "one instance in another zone",
            "c5.large",
            3,
            1,
            Some(other_zone),
        ),
    ];

    for (case, reserved, reserved_count, requested_count, zone) in cases {
        let capacity_reservation_id = reserve(case, &router, reserved, reserved_count).await;
        let before = instance_count(case, &router).await;
        let mut params = targeted_launch(reserved, &capacity_reservation_id);
        params.retain(|(name, _)| name != "MaxCount" && name != "MinCount");
        params.push(param("MinCount", "1"));
        params.push(param("MaxCount", &requested_count.to_string()));
        if let Some(zone) = zone {
            params.push(param("Placement.AvailabilityZone", zone));
        }

        let body = refused(case, &router, "RunInstances", &params).await;
        assert_eq!(
            error_code(&body),
            RESERVATION_CAPACITY_EXCEEDED,
            "case {case:?}: expected {RESERVATION_CAPACITY_EXCEEDED}, got {body}"
        );
        assert!(
            error_message(&body).contains(&capacity_reservation_id),
            "case {case:?}: expected the refusal to name the reservation it targeted, got {}",
            error_message(&body)
        );
        assert_eq!(
            instance_count(case, &router).await,
            before,
            "case {case:?}: a refused launch must mint nothing"
        );

        // A launch the reservation CAN hold — its own zone, a count it covers — is served
        // through the same target.
        let mut served_params = targeted_launch(reserved, &capacity_reservation_id);
        served_params.push(param("Placement.AvailabilityZone", ZONE));
        let served = ok(case, &router, "RunInstances", &served_params).await;
        assert_eq!(
            elements(&served, "instanceType"),
            vec![reserved.to_string()],
            "case {case:?}: the launch the reservation covers must be served, got {served}"
        );
    }
}

/// RULE 1's placement half: a reservation reserves capacity in ONE Availability Zone
/// (`CapacityReservation$AvailabilityZone`), so a launch that explicitly targets one and
/// names NEITHER a `Placement.AvailabilityZone` nor a subnet runs in THAT reservation's zone
/// — the zone the caller left open is the reservation's — and takes its capacity, rather than
/// being placed in the Region's first zone and refused for a reservation it cannot reach.
///
/// The rows are zones of the Region other than the one every other row of this suite uses,
/// so no single zone is the specification.
#[tokio::test]
async fn a_targeted_launch_that_names_no_placement_runs_in_the_reservations_own_zone() {
    let (router, _engine) = emulator();

    // (the zone the reservation reserves in, an instance type that zone offers). The second
    // row is the Region's LAST zone, which offers only the previous-generation families —
    // the offering fact `DescribeInstanceTypeOfferings` reports — so the pair varies
    // together.
    for (zone, instance_type) in [("us-west-2b", "t3.micro"), ("us-west-2c", "m3.large")] {
        let capacity_reservation_id = reserve_in(zone, &router, instance_type, 2, zone).await;
        let served = ok(
            zone,
            &router,
            "RunInstances",
            &targeted_launch(instance_type, &capacity_reservation_id),
        )
        .await;
        assert_eq!(
            elements(&served, "availabilityZone"),
            vec![zone.to_string()],
            "case {zone:?}: expected the launch placed in the reservation's own zone, got \
             {served}"
        );
        assert_eq!(
            element(&served, "capacityReservationId").as_deref(),
            Some(capacity_reservation_id.as_str()),
            "case {zone:?}: expected the instance to run in the reservation it targeted, got \
             {served}"
        );
        let reported = described_reservation(zone, &router, &capacity_reservation_id).await;
        assert_eq!(
            (
                element(&reported, "availableInstanceCount"),
                element(&reported, "count"),
            ),
            (Some("1".to_string()), Some("1".to_string())),
            "case {zone:?}: expected the reservation to report the instance running in it, \
             got {reported}"
        );
    }
}

/// RULE 1's id half: a `CapacityReservationId` EC2 could have ISSUED that names no
/// reservation of the account is NOT FOUND, while a value it could never have issued — the
/// all-zero placeholder, a width it does not issue, or no id at all — is malformed before any
/// lookup; the launch asks neither question of the instance type, because there is no
/// reservation to compare it against.
#[tokio::test]
async fn a_launch_that_targets_an_unresolvable_reservation_is_refused_by_the_id() {
    let (router, _engine) = emulator();

    // (case, the targeted id, the code EC2 answers)
    let cases = [
        (
            "a long id nothing is reserved under",
            "cr-0123456789abcdef0",
            INVALID_CAPACITY_RESERVATION_ID_NOT_FOUND,
        ),
        (
            "another long-form payload",
            "cr-0fedcba987654321f",
            INVALID_CAPACITY_RESERVATION_ID_NOT_FOUND,
        ),
        (
            "the all-zero placeholder, which the mint can never reach",
            "cr-00000000000000000",
            INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
        ),
        (
            "the short id form, which EC2 does not issue for a reservation",
            "cr-0a1b2c3d",
            INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
        ),
        (
            "a long id whose leading digit is above zero",
            "cr-1234567890abcdef0",
            INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
        ),
        (
            "a value of no id shape at all",
            "cr-not-an-id",
            INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
        ),
        (
            "another resource's id",
            "igw-0123456789abcdef0",
            INVALID_CAPACITY_RESERVATION_ID_MALFORMED,
        ),
    ];

    for (case, targeted, expected) in cases {
        let body = refused(
            case,
            &router,
            "RunInstances",
            &targeted_launch("t3.micro", targeted),
        )
        .await;
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected} for the target {targeted}, got {body}"
        );
        assert!(
            error_message(&body).contains(targeted),
            "case {case:?}: expected the refusal to name the id the request sent, got {}",
            error_message(&body)
        );
    }
    assert_eq!(
        instance_count("the refusals", &router).await,
        0,
        "no refused launch may mint an instance"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: an instance whose TYPE cannot serve the operation
// ---------------------------------------------------------------------------

/// Credentials of the shape `MacCredentials` documents: the operation stores none of it and
/// reports none of it, so its only job is to be present.
const MAC_CREDENTIALS: &str = "{\"internalDiskPassword\":\"\",\"rootVolumeUsername\":\"ec2-user\"}";

/// The Mac instance types the model declares: every type this service offers whose instance
/// family runs macOS. Read off the model, so a family it adds or drops moves with it.
fn mac_instance_types() -> Vec<&'static str> {
    offered_instance_types()
        .iter()
        .copied()
        .filter(|instance_type| is_mac_instance_family(instance_family_of(instance_type)))
        .collect()
}

/// Launch one instance of `instance_type` and answer its id.
async fn launch(case: &str, router: &MultiServiceRouter, instance_type: &str) -> String {
    let body = ok(
        case,
        router,
        "RunInstances",
        &[
            param("ImageId", IMAGE_ID),
            param("InstanceType", instance_type),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    member(case, &body, "instanceId")
}

/// RULE 2: an instance the account really holds whose TYPE cannot serve the operation is
/// refused `InvalidInstanceType`, and the sentence names the instance and the type it is of
/// rather than reporting the id as the bad value.
///
/// The rows span both Mac task creates and both ways a type can be ineligible: a family that
/// runs no macOS at all (three of them, from the modelled catalog) and an Intel Mac, which is
/// a Mac the SIP create serves and the volume ownership delegation — "for an Apple silicon
/// Mac instance" — cannot. The eligible instance is served through the same handler, so the
/// refusal is about the type rather than about the operation.
#[tokio::test]
async fn an_instance_whose_type_cannot_serve_the_operation_is_refused_invalid_instance_type() {
    let (router, _engine) = emulator();
    let case = "the catalog";
    let apple_silicon = *mac_instance_types()
        .iter()
        .find(|instance_type| architecture_for(instance_type) == ARCHITECTURE_ARM64_MAC)
        .expect("the model must declare an Apple silicon Mac instance type");
    let intel_mac = *mac_instance_types()
        .iter()
        .find(|instance_type| architecture_for(instance_type) != ARCHITECTURE_ARM64_MAC)
        .expect("the model must declare an Intel Mac instance type");
    // Three types of no Mac family at all, taken from the modelled catalog in its own order.
    let non_mac: Vec<&'static str> = offered_instance_types()
        .iter()
        .copied()
        .filter(|instance_type| !is_mac_instance_family(instance_family_of(instance_type)))
        .take(3)
        .collect();
    assert_eq!(
        non_mac.len(),
        3,
        "case {case:?}: the model must declare at least three types of no Mac family"
    );

    // (case, the instance type, the operations whose type prerequisite it fails)
    let delegation = "CreateDelegateMacVolumeOwnershipTask";
    let sip = "CreateMacSystemIntegrityProtectionModificationTask";
    let mut rows: Vec<(String, &'static str, Vec<&'static str>)> = non_mac
        .iter()
        .map(|instance_type| {
            (
                format!("{instance_type}, which runs no macOS"),
                *instance_type,
                vec![sip, delegation],
            )
        })
        .collect();
    rows.push((
        format!("{intel_mac}, a Mac that is not Apple silicon"),
        intel_mac,
        vec![delegation],
    ));

    for (case, instance_type, actions) in rows {
        let instance_id = launch(&case, &router, instance_type).await;
        for action in actions {
            let params = if action == delegation {
                vec![
                    param("InstanceId", &instance_id),
                    param("MacCredentials", MAC_CREDENTIALS),
                ]
            } else {
                vec![
                    param("InstanceId", &instance_id),
                    param("MacSystemIntegrityProtectionStatus", "enabled"),
                ]
            };
            let body = refused(&case, &router, action, &params).await;
            assert_eq!(
                error_code(&body),
                INVALID_INSTANCE_TYPE,
                "case {case:?}: expected {INVALID_INSTANCE_TYPE} from {action}, got {body}"
            );
            assert_eq!(
                error_message(&body),
                INSTANCE_TYPE_NOT_SUPPORTED_MESSAGE,
                "case {case:?}: expected {action} to answer the service's one sentence for an \
                 ineligible instance type, got {body}"
            );
        }
    }

    // The eligible instance is served by both creates, and the task reads back.
    let eligible = launch(case, &router, apple_silicon).await;
    for (action, params) in [
        (
            delegation,
            vec![
                param("InstanceId", &eligible),
                param("MacCredentials", MAC_CREDENTIALS),
            ],
        ),
        (
            sip,
            vec![
                param("InstanceId", &eligible),
                param("MacSystemIntegrityProtectionStatus", "enabled"),
            ],
        ),
    ] {
        let body = ok(case, &router, action, &params).await;
        assert_eq!(
            element(&body, "instanceId").as_deref(),
            Some(eligible.as_str()),
            "case {case:?}: {action} must report the instance it ran on, got {body}"
        );
    }
    let reported = ok(case, &router, "DescribeMacModificationTasks", &[]).await;
    assert_eq!(
        elements(&reported, "instanceId"),
        vec![eligible.clone(), eligible.clone()],
        "case {case:?}: both tasks must read back for {eligible}, got {reported}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: a route to a NAT gateway the account does not hold
// ---------------------------------------------------------------------------

/// RULE 3: a route whose target names a NAT gateway the account holds none of is refused by
/// the member that carried the id — `InvalidNatGatewayID.NotFound` — and the table it would
/// have gone in is left with the local route alone, while the NAT gateway family's own
/// operations keep answering `NatGatewayNotFound` for the very same id.
///
/// The rows differ in the id's payload and width and in the route's destination, and cover
/// both operations that resolve a route target.
#[tokio::test]
async fn a_route_to_an_absent_nat_gateway_is_refused_by_the_member_that_named_it() {
    let (router, _engine) = emulator();
    let case = "the estate";
    let vpc_body = ok(case, &router, "CreateVpc", &[param("CidrBlock", "10.80.0.0/16")]).await;
    let vpc_id = member(case, &vpc_body, "vpcId");
    let table_body = ok(case, &router, "CreateRouteTable", &[param("VpcId", &vpc_id)]).await;
    let route_table_id = member(case, &table_body, "routeTableId");

    // (case, the NAT gateway id, the destination the route would have taken)
    let cases = [
        (
            "the long id form",
            "nat-0123456789abcdef0",
            "0.0.0.0/0",
        ),
        (
            "another long-form payload",
            "nat-0fedcba987654321f",
            "192.0.2.0/24",
        ),
        ("the short id form", "nat-0a1b2c3d", "198.51.100.0/24"),
    ];

    for (case, nat_gateway_id, destination) in cases {
        let body = refused(
            case,
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", destination),
                param("NatGatewayId", nat_gateway_id),
            ],
        )
        .await;
        assert_eq!(
            error_code(&body),
            INVALID_NAT_GATEWAY_ID_NOT_FOUND,
            "case {case:?}: expected {INVALID_NAT_GATEWAY_ID_NOT_FOUND}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The natGateway ID '{nat_gateway_id}' does not exist"),
            "case {case:?}: expected the member that carried the id to name it, got {body}"
        );

        // A replacement resolves its new target by the same rule.
        let replaced = refused(
            case,
            &router,
            "ReplaceRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", destination),
                param("NatGatewayId", nat_gateway_id),
            ],
        )
        .await;
        assert_eq!(
            error_code(&replaced),
            INVALID_NAT_GATEWAY_ID_NOT_FOUND,
            "case {case:?}: expected ReplaceRoute to answer \
             {INVALID_NAT_GATEWAY_ID_NOT_FOUND}, got {replaced}"
        );

        // The NAT gateway family's own operations report the same id their own way.
        for action in ["DeleteNatGateway", "DescribeNatGateways"] {
            let params = if action == "DeleteNatGateway" {
                vec![param("NatGatewayId", nat_gateway_id)]
            } else {
                vec![param("NatGatewayId.1", nat_gateway_id)]
            };
            let body = refused(case, &router, action, &params).await;
            assert_eq!(
                error_code(&body),
                NAT_GATEWAY_NOT_FOUND,
                "case {case:?}: expected {action} to keep {NAT_GATEWAY_NOT_FOUND}, got {body}"
            );
        }
    }

    // Nothing was added beside the local route the table came with.
    let read = ok(
        case,
        &router,
        "DescribeRouteTables",
        &[param("RouteTableId.1", &route_table_id)],
    )
    .await;
    assert_eq!(
        elements(&read, "destinationCidrBlock"),
        vec!["10.80.0.0/16".to_string()],
        "a refused route must add nothing, got {read}"
    );
}

// ---------------------------------------------------------------------------
// RULE 4: a network that already has an internet gateway
// ---------------------------------------------------------------------------

/// RULE 4: while an attachment holds, the VPC's side of it refuses the `VpcId` a second
/// attach was given (`InvalidParameterValue`, naming the network), and the GATEWAY's side
/// refuses the association conflict `Resource.AlreadyAssociated` naming the pair that holds
/// — two conditions a caller repairs differently, and nothing is attached beside the one
/// attachment either way.
///
/// The rows differ in the VPC's block value and size, and so in which pair holds.
#[tokio::test]
async fn a_network_that_already_has_an_internet_gateway_refuses_the_vpc_id() {
    let (router, _engine) = emulator();

    // (case, the VPC's block)
    let cases = [
        ("a /16", "10.81.0.0/16"),
        ("a /20", "10.82.0.0/20"),
        ("a /24 outside the private 10 block", "192.168.21.0/24"),
    ];

    for (case, cidr_block) in cases {
        let vpc_body = ok(case, &router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
        let vpc_id = member(case, &vpc_body, "vpcId");
        let free_vpc_body = ok(
            case,
            &router,
            "CreateVpc",
            &[param("CidrBlock", "172.16.0.0/16")],
        )
        .await;
        let free_vpc_id = member(case, &free_vpc_body, "vpcId");
        let held = member(
            case,
            &ok(case, &router, "CreateInternetGateway", &[]).await,
            "internetGatewayId",
        );
        let spare = member(
            case,
            &ok(case, &router, "CreateInternetGateway", &[]).await,
            "internetGatewayId",
        );
        let attach = |gateway: &str, vpc: &str| {
            vec![
                param("InternetGatewayId", gateway),
                param("VpcId", vpc),
            ]
        };
        ok(
            case,
            &router,
            "AttachInternetGateway",
            &attach(&held, &vpc_id),
        )
        .await;

        // The network's side: what the caller got wrong is the VpcId it passed.
        let body = refused(
            case,
            &router,
            "AttachInternetGateway",
            &attach(&spare, &vpc_id),
        )
        .await;
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE} for a second gateway, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("Network {vpc_id} already has an internet gateway attached"),
            "case {case:?}: expected the refusal to name the network, got {body}"
        );

        // The gateway's side: the gateway the request named is spoken for, and the refusal
        // names the pair that holds.
        let body = refused(
            case,
            &router,
            "AttachInternetGateway",
            &attach(&held, &free_vpc_id),
        )
        .await;
        assert_eq!(
            error_code(&body),
            RESOURCE_ALREADY_ASSOCIATED,
            "case {case:?}: expected {RESOURCE_ALREADY_ASSOCIATED} for a gateway already \
             attached, got {body}"
        );
        for named in [held.as_str(), vpc_id.as_str()] {
            assert!(
                error_message(&body).contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {}",
                error_message(&body)
            );
        }

        // Only the attachment that holds is reported, for either gateway.
        let reported = ok(
            case,
            &router,
            "DescribeInternetGateways",
            &[param("InternetGatewayId.1", &held)],
        )
        .await;
        assert_eq!(
            elements(&reported, "vpcId"),
            vec![vpc_id.clone()],
            "case {case:?}: expected the one attachment to survive, got {reported}"
        );
        let reported = ok(
            case,
            &router,
            "DescribeInternetGateways",
            &[param("InternetGatewayId.1", &spare)],
        )
        .await;
        assert!(
            elements(&reported, "vpcId").is_empty(),
            "case {case:?}: the refused gateway must be attached to nothing, got {reported}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 5: the start of a path analysis
// ---------------------------------------------------------------------------

/// A Reachability Analyzer path of the account, as its id — written through the store's own
/// one path-insertion seam, this suite being about what the START of an analysis refuses
/// rather than about what a create stores.
fn seed_path(engine: &engine::InMemoryEngine) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_network_insights_path(NetworkInsightsPathSpec {
            source: Some("igw-0123456789abcdef0".to_string()),
            destination: Some("i-0123456789abcdef0".to_string()),
            protocol: Some("tcp".to_string()),
            destination_port: Some(443),
            region: REGION.to_string(),
            ..NetworkInsightsPathSpec::default()
        })
        .expect("the store holds room for one path")
}

/// RULE 5: `StartNetworkInsightsAnalysis` answers the refusal the path id's own FORM calls
/// for: a value EC2 could never have issued a path id as is `InvalidParameterValue` stating
/// that the `NetworkInsightsPathId` it was given is not valid, while an id in a form EC2 does
/// issue names a path that is not there and is the path resource's own not-found code with
/// the id and the fixed text. The describes, which SELECT paths with a list of ids, keep that
/// not-found code for every value. The path the account does hold is analyzed, and the
/// analysis reads back.
///
/// The rows differ in the id's width and payload and in whether it is of the form EC2 issues
/// a path id in at all.
#[tokio::test]
async fn the_start_of_an_analysis_refuses_a_path_id_by_the_form_of_the_id() {
    let (router, engine) = emulator();
    let path_id = seed_path(&engine);

    // (case, a path id the account holds no path under, the code, the sentence)
    let cases = [
        (
            "the long id form",
            "nip-0123456789abcdef0",
            INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
            "nip-0123456789abcdef0 does not exist".to_string(),
        ),
        (
            "another long-form payload",
            "nip-0fedcba987654321f",
            INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
            "nip-0fedcba987654321f does not exist".to_string(),
        ),
        (
            "the short id form",
            "nip-0a1b2c3d",
            INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
            "nip-0a1b2c3d does not exist".to_string(),
        ),
        (
            "a long form whose leading digit no issued id carries",
            "nip-fffffffffffffffff",
            INVALID_PARAMETER_VALUE,
            "NetworkInsightsPathId (nip-fffffffffffffffff) is not valid".to_string(),
        ),
        (
            "the all-zero placeholder",
            "nip-00000000000000000",
            INVALID_PARAMETER_VALUE,
            "NetworkInsightsPathId (nip-00000000000000000) is not valid".to_string(),
        ),
        (
            "a value of no id form at all",
            "not-a-path-id",
            INVALID_PARAMETER_VALUE,
            "NetworkInsightsPathId (not-a-path-id) is not valid".to_string(),
        ),
    ];

    for (case, unknown, expected_code, expected_message) in cases {
        let body = refused(
            case,
            &router,
            "StartNetworkInsightsAnalysis",
            &[
                param("NetworkInsightsPathId", unknown),
                param("ClientToken", &format!("token-for-{unknown}")),
            ],
        )
        .await;
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code} from the start, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {case:?}: expected {expected_message:?}, got {body}"
        );

        // The describes select paths BY the id, and keep the path's own not-found code.
        let selected = refused(
            case,
            &router,
            "DescribeNetworkInsightsPaths",
            &[param("NetworkInsightsPathId.1", unknown)],
        )
        .await;
        assert_eq!(
            error_code(&selected),
            INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
            "case {case:?}: expected the describe to keep \
             {INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND}, got {selected}"
        );
    }

    // The path the account holds is analyzed, and the analysis reads back against it.
    let case = "the path the account holds";
    let started = ok(
        case,
        &router,
        "StartNetworkInsightsAnalysis",
        &[
            param("NetworkInsightsPathId", &path_id),
            param("ClientToken", "the-one-analysis"),
        ],
    )
    .await;
    let analysis_id = member(case, &started, "networkInsightsAnalysisId");
    let reported = ok(
        case,
        &router,
        "DescribeNetworkInsightsAnalyses",
        &[param("NetworkInsightsPathId", &path_id)],
    )
    .await;
    assert_eq!(
        elements(&reported, "networkInsightsAnalysisId"),
        vec![analysis_id],
        "case {case:?}: the analysis must read back for its path, got {reported}"
    );
}

// ---------------------------------------------------------------------------
// RULE 6: an attachment id of the form EC2 issues
// ---------------------------------------------------------------------------

/// RULE 6: a `NetworkInterfaceAttachmentId` of the PRINTED id form is LOOKED UP, so one that
/// names no attachment of the account is `InvalidAttachmentID.NotFound` — which is what a
/// caller cleaning up reads as "already detached" — while a value outside that form names no
/// attachment that could ever have existed and is refused as malformed before any lookup.
///
/// The rows differ in the id's payload and width and in how a value falls outside the form.
#[tokio::test]
async fn an_attachment_id_of_the_issued_form_is_looked_up_rather_than_called_malformed() {
    let (router, _engine) = emulator();

    // (case, the attachment id, the code EC2 answers)
    let cases = [
        (
            "a long id naming no attachment",
            "eni-attach-0123456789abcdef0",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "another long-form payload",
            "eni-attach-0fedcba987654321f",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a leading digit above zero",
            "eni-attach-1234567890abcdef0",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "the all-zero placeholder",
            "eni-attach-00000000000000000",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "the short id form",
            "eni-attach-0a1b2c3d",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a short id whose leading digit is above zero",
            "eni-attach-1234abcd",
            INVALID_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a value of no id form at all",
            "eni-attach-not-an-id",
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "upper-case digits",
            "eni-attach-0123456789ABCDEF0",
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "one digit too many",
            "eni-attach-000000000000000001",
            INVALID_NETWORK_INTERFACE_ATTACHMENT_ID_MALFORMED,
        ),
    ];

    for (case, attachment_id, expected) in cases {
        let body = refused(
            case,
            &router,
            "DetachNetworkInterface",
            &[param("AttachmentId", attachment_id)],
        )
        .await;
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected} for {attachment_id}, got {body}"
        );
        assert!(
            error_message(&body).contains(attachment_id),
            "case {case:?}: expected the refusal to name the id the request sent, got {}",
            error_message(&body)
        );
    }
}
