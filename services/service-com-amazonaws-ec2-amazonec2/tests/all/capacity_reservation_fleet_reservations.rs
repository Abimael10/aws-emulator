// SPDX-License-Identifier: CC0-1.0

//! The Capacity Reservations a Capacity Reservation Fleet reserves its capacity THROUGH —
//! driven over the wire the service actually speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness).
//!
//! The rule under test: a Fleet reserves capacity through Capacity Reservations OF THE
//! ACCOUNT, so every reservation a Fleet reports is a reservation of the one Capacity
//! Reservation store — `DescribeCapacityReservations` resolves it under the id the Fleet
//! reports, and so does every other reader of a reservation id, including
//! `GetGroupsForCapacityReservation`. Each one carries what the Fleet reserved it with (its
//! instance type, platform and zone, the Fleet's tenancy and matching criteria, the Fleet's
//! tags and the Fleet's own id), holds the instances covering the target capacity, and is
//! cancelled when the Fleet is.
//!
//! Each table row is a distinct input along what the rule generalises over — how many capacity
//! units one instance provides, and so how many instances cover the target, and which of the
//! Fleet's instance type specifications the allocation strategy reserves on — so no row is the
//! specification.

use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;

use ec2::ops::types::FLEET_INSTANCE_MATCH_CRITERIA_VALUES;
use ec2::state::{
    CAPACITY_RESERVATION_FLEET_STATE_CANCELLING, CAPACITY_RESERVATION_STATE_ACTIVE,
    CAPACITY_RESERVATION_STATE_CANCELLED, RESOURCE_TYPE_CAPACITY_RESERVATION_FLEET,
};

use super::ec2_query::{call, elements, indexed};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a move the clock owes before it gives up: far longer than the
/// span the service gives one, so a loaded machine cannot fail the assertion, and the poll
/// stops as soon as the condition holds rather than after a fixed wait.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One of a Fleet's reported Capacity Reservations: the id it names, and the instances and
/// capacity it holds.
#[derive(Debug, Clone, PartialEq)]
struct FleetReservation {
    capacity_reservation_id: Option<String>,
    instance_type: String,
    instance_platform: String,
    availability_zone: String,
    total_instance_count: i32,
}

/// What `DescribeCapacityReservationFleets` reports for `fleet_id`: the Fleet's state and the
/// Capacity Reservations it reserves through, in the order it reports them.
async fn described_fleet(
    case: &str,
    router: &MultiServiceRouter,
    fleet_id: &str,
) -> (String, Vec<FleetReservation>) {
    let body = ok(
        case,
        router,
        "DescribeCapacityReservationFleets",
        &indexed("CapacityReservationFleetId", &[fleet_id]),
    )
    .await;
    let parsed = document(case, &body);
    let fleets = set_items(parsed.root_element(), "capacityReservationFleetSet");
    assert_eq!(
        fleets.len(),
        1,
        "case {case:?}: a describe narrowed to one Fleet reports it alone, got {body}"
    );
    let fleet = fleets[0];
    let state = child_text(fleet, "state").unwrap_or_default();
    let reservations = set_items(fleet, "instanceTypeSpecificationSet")
        .into_iter()
        .map(|item| FleetReservation {
            capacity_reservation_id: child_text(item, "capacityReservationId"),
            instance_type: child_text(item, "instanceType").unwrap_or_default(),
            instance_platform: child_text(item, "instancePlatform").unwrap_or_default(),
            availability_zone: child_text(item, "availabilityZone").unwrap_or_default(),
            total_instance_count: child_text(item, "totalInstanceCount")
                .unwrap_or_default()
                .parse()
                .unwrap_or_default(),
        })
        .collect();
    (state, reservations)
}

/// The body `DescribeCapacityReservations` answers for exactly one reservation id.
async fn described_reservation(
    case: &str,
    router: &MultiServiceRouter,
    capacity_reservation_id: &str,
) -> String {
    ok(
        case,
        router,
        "DescribeCapacityReservations",
        &indexed("CapacityReservationId", &[capacity_reservation_id]),
    )
    .await
}

/// The text of `member` in the one reservation `DescribeCapacityReservations` reports.
fn reported(case: &str, body: &str, member: &str) -> Option<String> {
    let parsed = document(case, body);
    let items = set_items(parsed.root_element(), "capacityReservationSet");
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: a describe narrowed to one reservation reports it alone, got {body}"
    );
    child_text(items[0], member)
}

/// Look again until `DescribeCapacityReservations` reports `state` for the reservation, or
/// fail once [`POLL_DEADLINE`] has passed. The move is one the CLOCK owes, so the wait is for
/// the condition rather than for a span.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    capacity_reservation_id: &str,
    state: &str,
) {
    let deadline = ::tokio::time::Instant::now() + POLL_DEADLINE;
    let mut last = String::new();
    while ::tokio::time::Instant::now() < deadline {
        let body = described_reservation(case, router, capacity_reservation_id).await;
        last = reported(case, &body, "state").unwrap_or_default();
        if last == state {
            return;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: expected {capacity_reservation_id} to reach state {state:?}, \
         last observed {last:?}"
    );
}

/// One Fleet's create parameters: two instance type specifications in ONE zone, the first of
/// them the one the `prioritized` strategy reserves on (the lower priority value), carrying
/// `weight` capacity units per instance, and one tag the Fleet assigns.
fn fleet_params(total_target_capacity: i32, weight: f64, chosen_priority: i32) -> Vec<Param> {
    vec![
        param("TotalTargetCapacity", &total_target_capacity.to_string()),
        // The instance matching criteria the create takes as a parameter of its own
        // (`$InstanceMatchCriteria`), in the one value that member publishes.
        param("InstanceMatchCriteria", "open"),
        param("InstanceTypeSpecification.1.InstanceType", "c5.large"),
        param("InstanceTypeSpecification.1.InstancePlatform", "Linux/UNIX"),
        param("InstanceTypeSpecification.1.AvailabilityZone", "us-west-2a"),
        param("InstanceTypeSpecification.1.Weight", &weight.to_string()),
        param(
            "InstanceTypeSpecification.1.Priority",
            &chosen_priority.to_string(),
        ),
        param("InstanceTypeSpecification.2.InstanceType", "m5.large"),
        param("InstanceTypeSpecification.2.InstancePlatform", "Windows"),
        param("InstanceTypeSpecification.2.AvailabilityZone", "us-west-2a"),
        param("InstanceTypeSpecification.2.Weight", "1"),
        param(
            "InstanceTypeSpecification.2.Priority",
            &(chosen_priority + 4).to_string(),
        ),
        param(
            "TagSpecification.1.ResourceType",
            RESOURCE_TYPE_CAPACITY_RESERVATION_FLEET,
        ),
        param("TagSpecification.1.Tag.1.Key", "workload"),
        param("TagSpecification.1.Tag.1.Value", "batch"),
    ]
}

/// The rule: the Capacity Reservation a Fleet reports is a reservation of the account, carrying
/// what the Fleet reserved it with — and it is cancelled when the Fleet is.
#[tokio::test]
async fn a_fleet_reserves_capacity_through_reservations_of_the_account() {
    // (case, total target capacity, capacity units one instance provides, the priority of the
    // specification the strategy reserves on, how many instances cover the target). The rows
    // differ in the weight the count follows from and in the priority ordering the strategy
    // resolves, so no row is the rule.
    let rows: &[(&str, i32, f64, i32, i32)] = &[
        ("one capacity unit per instance", 4, 1.0, 1, 4),
        ("two capacity units per instance", 5, 2.0, 0, 3),
        ("a fractional weight", 3, 1.5, 2, 2),
    ];
    for (case, total_target_capacity, weight, chosen_priority, expected_instances) in
        rows.iter().copied()
    {
        let (router, _engine) = super::ec2_query::emulator();
        let created = ok(
            case,
            &router,
            "CreateCapacityReservationFleet",
            &fleet_params(total_target_capacity, weight, chosen_priority),
        )
        .await;
        let fleet_id = super::ec2_query::member(&created, "capacityReservationFleetId")
            .unwrap_or_else(|| panic!("case {case:?}: no capacityReservationFleetId in {created}"));

        // The Fleet reports the reservation it reserved on, and reports none for the
        // specification the strategy did not reserve on.
        let (_, reservations) = described_fleet(case, &router, &fleet_id).await;
        assert_eq!(
            reservations.len(),
            2,
            "case {case:?}: the Fleet reports every specification, got {reservations:?}"
        );
        let chosen = reservations
            .iter()
            .find(|reservation| reservation.capacity_reservation_id.is_some())
            .unwrap_or_else(|| {
                panic!("case {case:?}: the Fleet reserved on no specification: {reservations:?}")
            });
        assert_eq!(
            reservations
                .iter()
                .filter(|reservation| reservation.capacity_reservation_id.is_some())
                .count(),
            1,
            "case {case:?}: the strategy reserves on one specification, got {reservations:?}"
        );
        assert_eq!(
            chosen.total_instance_count, expected_instances,
            "case {case:?}: expected the instances covering the target, got {chosen:?}"
        );
        let capacity_reservation_id = chosen
            .capacity_reservation_id
            .clone()
            .unwrap_or_default();

        // The id the Fleet reports is a reservation of the ACCOUNT: the store that owns
        // Capacity Reservations answers for it.
        let described = described_reservation(case, &router, &capacity_reservation_id).await;
        let member = |name: &str| reported(case, &described, name).unwrap_or_default();
        assert_eq!(
            member("capacityReservationFleetId"),
            fleet_id,
            "case {case:?}: the reservation names the Fleet it belongs to, got {described}"
        );
        assert_eq!(
            (
                member("instanceType"),
                member("instancePlatform"),
                member("availabilityZone"),
                member("totalInstanceCount"),
                member("availableInstanceCount"),
                member("instanceMatchCriteria"),
            ),
            (
                chosen.instance_type.clone(),
                chosen.instance_platform.clone(),
                chosen.availability_zone.clone(),
                expected_instances.to_string(),
                expected_instances.to_string(),
                // "All Capacity Reservations in the Fleet inherit this instance matching
                // criteria" (`CreateCapacityReservationFleetRequest$InstanceMatchCriteria`),
                // whose modelled catalog publishes one value.
                FLEET_INSTANCE_MATCH_CRITERIA_VALUES[0].to_string(),
            ),
            "case {case:?}: expected what the Fleet reserved the capacity with, got {described}"
        );
        // "The tags are automatically assigned to the Capacity Reservations in the Fleet"
        // (`CreateCapacityReservationFleetRequest$TagSpecifications`).
        assert_eq!(
            (
                elements(&described, "key"),
                elements(&described, "value")
            ),
            (
                vec!["workload".to_string()],
                vec!["batch".to_string()]
            ),
            "case {case:?}: expected the Fleet's tags on its reservation, got {described}"
        );

        // The capacity is provisioned on the CLOCK: the reservation is reported `pending`
        // until the move the create recorded falls due, and `active` from then on.
        poll_until_state(
            case,
            &router,
            &capacity_reservation_id,
            CAPACITY_RESERVATION_STATE_ACTIVE,
        )
        .await;

        // Every reader of a reservation id resolves it, including this one — which reports no
        // resource group, a Fleet's reservation being added to none.
        let groups = ok(
            case,
            &router,
            "GetGroupsForCapacityReservation",
            &[param("CapacityReservationId", &capacity_reservation_id)],
        )
        .await;
        assert!(
            elements(&groups, "groupArn").is_empty(),
            "case {case:?}: a Fleet's reservation is in no resource group, got {groups}"
        );

        // "All of the individual Capacity Reservations in the Fleet are cancelled"
        // (`CancelCapacityReservationFleets`).
        let cancelled = ok(
            case,
            &router,
            "CancelCapacityReservationFleets",
            &indexed("CapacityReservationFleetId", &[&fleet_id]),
        )
        .await;
        assert_eq!(
            elements(&cancelled, "currentFleetState"),
            vec![CAPACITY_RESERVATION_FLEET_STATE_CANCELLING.to_string()],
            "case {case:?}: expected the Fleet in the transient state, got {cancelled}"
        );
        let after = described_reservation(case, &router, &capacity_reservation_id).await;
        assert_eq!(
            reported(case, &after, "state").unwrap_or_default(),
            CAPACITY_RESERVATION_STATE_CANCELLED,
            "case {case:?}: the Fleet's reservation is cancelled with it, got {after}"
        );
    }
}

/// An end date in the future, as `ModifyCapacityReservationFleet` takes one: "The date and
/// time at which the Capacity Reservation Fleet expires"
/// (`ModifyCapacityReservationFleetRequest$EndDate`).
const LATER_END_DATE: &str = "2031-06-07T08:09:10Z";

/// The `EndDateType` a Capacity Reservation with an end date reports, and the one a
/// reservation with none reports — members of the modelled `EndDateType` enum.
const END_DATE_TYPE_LIMITED: &str = "limited";
const END_DATE_TYPE_UNLIMITED: &str = "unlimited";

/// The rule on the modification side: "the Fleet automatically creates new Capacity
/// Reservations, or modifies or cancels existing Capacity Reservations in the Fleet to meet
/// the new total target capacity" (`ModifyCapacityReservationFleet`) — so the capacity the
/// Fleet's own reservation holds follows the new target, the Fleet's end date is the date that
/// reservation expires on, and what the Fleet reports and what the Capacity Reservation store
/// holds never disagree.
#[tokio::test]
async fn modifying_a_fleet_modifies_the_capacity_reservations_it_reserves_through() {
    let case = "a fleet whose target capacity moves";
    let (router, _engine) = super::ec2_query::emulator();
    let created = ok(
        case,
        &router,
        "CreateCapacityReservationFleet",
        &fleet_params(4, 1.0, 1),
    )
    .await;
    let fleet_id = super::ec2_query::member(&created, "capacityReservationFleetId")
        .unwrap_or_else(|| panic!("case {case:?}: no capacityReservationFleetId in {created}"));
    let (_, reservations) = described_fleet(case, &router, &fleet_id).await;
    let capacity_reservation_id = reservations
        .iter()
        .find_map(|reservation| reservation.capacity_reservation_id.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the Fleet reserved on no specification"));

    // (case, the new target capacity, the instances covering it at the chosen weight of 1).
    // The rows differ in which direction the target moves — up, back down, and to the single
    // unit at the bottom of the range — and the SAME reservation holds the new capacity each
    // time, because "it keeps the id … it was reserved under".
    let rows: &[(&str, i32, i32)] = &[
        ("a wider target", 6, 6),
        ("a narrower target", 2, 2),
        ("the smallest target there is", 1, 1),
    ];
    for (case, total_target_capacity, expected_instances) in rows.iter().copied() {
        ok(
            case,
            &router,
            "ModifyCapacityReservationFleet",
            &[
                param("CapacityReservationFleetId", &fleet_id),
                param("TotalTargetCapacity", &total_target_capacity.to_string()),
            ],
        )
        .await;
        let (_, reserved) = described_fleet(case, &router, &fleet_id).await;
        assert_eq!(
            reserved
                .iter()
                .find_map(|reservation| reservation.capacity_reservation_id.clone()),
            Some(capacity_reservation_id.clone()),
            "case {case:?}: the Fleet keeps reserving through the same reservation, \
             got {reserved:?}"
        );
        let described = described_reservation(case, &router, &capacity_reservation_id).await;
        assert_eq!(
            (
                reported(case, &described, "totalInstanceCount").unwrap_or_default(),
                reported(case, &described, "availableInstanceCount").unwrap_or_default(),
            ),
            (
                expected_instances.to_string(),
                expected_instances.to_string()
            ),
            "case {case:?}: the reservation holds the instances covering the new target, \
             got {described}"
        );
    }

    // "When the Capacity Reservation Fleet expires … all of the Capacity Reservations in the
    // Fleet expire" (`$EndDate`), so the Fleet's end date is its reservation's end date — and
    // "if you remove the end date, the Capacity Reservation Fleet does not expire"
    // (`$RemoveEndDate`), which its reservation follows too.
    let case = "an end date set on the fleet";
    ok(
        case,
        &router,
        "ModifyCapacityReservationFleet",
        &[
            param("CapacityReservationFleetId", &fleet_id),
            param("EndDate", LATER_END_DATE),
        ],
    )
    .await;
    let described = described_reservation(case, &router, &capacity_reservation_id).await;
    assert_eq!(
        (
            reported(case, &described, "endDate").unwrap_or_default(),
            reported(case, &described, "endDateType").unwrap_or_default(),
        ),
        (
            LATER_END_DATE.to_string(),
            END_DATE_TYPE_LIMITED.to_string()
        ),
        "case {case:?}: the reservation expires with the Fleet, got {described}"
    );

    let case = "the end date removed again";
    ok(
        case,
        &router,
        "ModifyCapacityReservationFleet",
        &[
            param("CapacityReservationFleetId", &fleet_id),
            param("RemoveEndDate", "true"),
        ],
    )
    .await;
    let described = described_reservation(case, &router, &capacity_reservation_id).await;
    assert_eq!(
        (
            reported(case, &described, "endDate"),
            reported(case, &described, "endDateType").unwrap_or_default(),
        ),
        (None, END_DATE_TYPE_UNLIMITED.to_string()),
        "case {case:?}: a Fleet that does not expire holds a reservation that does not \
         either, got {described}"
    );
}
