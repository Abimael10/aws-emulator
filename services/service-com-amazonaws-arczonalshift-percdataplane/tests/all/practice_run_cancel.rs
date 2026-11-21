// SPDX-License-Identifier: CC0-1.0

//! Cancelling an in-progress practice run, driven end to end over the wire.
//!
//! Every case here builds the production `MultiServiceRouter`, registers this service on
//! one shared engine and dispatches real `restJson1` requests, so what is pinned is the
//! HTTP behaviour rather than a handler called directly.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one request:
//!
//! * cancelling a practice run moves the stored shift to `CANCELED`, so the cancel's own
//!   answer and every later read report it that way and the weight it moved goes back to
//!   1 — a canceled shift is no longer one of the resource's shifts in progress;
//! * the only shift this operation takes a target of is one of the `PRACTICE_RUN` type
//!   ("Cancel an in-progress practice run zonal shift"), so a shift of any other type is
//!   refused 400 `ValidationException` with the `ValidationExceptionReason` the model
//!   names for the mismatch, nothing is written, and that same shift is still canceled
//!   through `CancelZonalShift`, which documents every kind of shift;
//! * a practice run that is no longer `ACTIVE` — already canceled, or whose run the clock
//!   has ended — has nothing left to cancel, so it is refused 409 `ConflictException`;
//! * the target is authorized before it is resolved, so a `zonalShiftId` ARC holds no
//!   shift for is refused 403 `AccessDeniedException` that discloses nothing about it.
//!
//! The rows differ along what those rules generalise over: WHICH resource the run is for
//! (each resource this store is seeded with, and one the service that owns it
//! registered), WHICH of its Availability Zones traffic moves away from, and — for the
//! wrong-type rule — the length of time and the zone the customer-initiated shift was
//! started with, and the form and size of the `zonalShiftId` for the unheld-target rule.

use std::time::{Duration, SystemTime};

use serde_json::Value;

use service_com_amazonaws_arczonalshift_percdataplane::ops::types::{
    CONFLICT_EXCEPTION_REASON_VALUES, VALIDATION_EXCEPTION_REASON_VALUES, ZONAL_SHIFT_STATUS_VALUES,
};
use service_com_amazonaws_arczonalshift_percdataplane::state::{
    ShiftType, PRACTICE_RUN_DURATION, UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_MESSAGE,
    UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_REASON, ZONAL_SHIFT_NOT_ACTIVE_MESSAGE,
    ZONAL_SHIFT_NOT_ACTIVE_REASON,
};
use service_com_amazonaws_arczonalshift_percdataplane::ComAmazonawsArczonalshiftPercdataplaneState as ZonalShiftStore;
use smithy_server_runtime::MultiServiceRouter;

use super::access_denied_precedence::UNKNOWN_ZONAL_SHIFT_IDS;
use super::practice_run_configuration::get_managed_resource;
use super::practice_run_start::{given_configuration, start_practice_run};
use super::registered_managed_resources::register;
use super::zonal_shift_lifecycle::{call, emulator, error_code, seeded, start_body};

/// `CancelPracticeRun` over the wire: the `DELETE /practiceruns/{zonalShiftId}` binding
/// the model gives the operation.
pub(crate) async fn cancel_practice_run(
    router: &MultiServiceRouter,
    zonal_shift_id: &str,
) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/practiceruns/{zonal_shift_id}"),
        "",
    )
    .await
}

/// `CancelZonalShift` over the wire, for the boundary of the wrong-type rule.
async fn cancel_zonal_shift(router: &MultiServiceRouter, zonal_shift_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/zonalshifts/{zonal_shift_id}"), "").await
}

/// The `zonalShifts` entry a resource view reports for one shift id.
fn reported_shift(resource_view: &Value, zonal_shift_id: &str) -> Option<Value> {
    resource_view["zonalShifts"]
        .as_array()?
        .iter()
        .find(|shift| shift["zonalShiftId"].as_str() == Some(zonal_shift_id))
        .cloned()
}

/// The `ListZonalShifts` entry for one shift id.
async fn listed_shift(router: &MultiServiceRouter, zonal_shift_id: &str) -> Value {
    let (status, listed) = call(router, "GET", "/zonalshifts", "").await;
    assert_eq!(status, 200, "ListZonalShifts must answer, got {status} {listed}");
    listed["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .find(|item| item["zonalShiftId"].as_str() == Some(zonal_shift_id))
        .cloned()
        .unwrap_or_else(|| panic!("expected {zonal_shift_id} listed, got {listed}"))
}

/// A resource the service that owns it registered with ARC, named for the case using it so
/// every case acts on a resource of its own.
fn registered(engine: &engine::InMemoryEngine, case: &str, zones: &[&str]) -> String {
    let arn = format!("arn:aws:eks:us-east-1:000000000000:cluster/cancel-{case}");
    register(engine, &arn, &format!("cancel-{case}"), zones);
    arn
}

/// One row of the cancel table: the resource the run is for, the zone traffic moves away
/// from, and the zones that resource is deployed in.
struct Row {
    resource: String,
    away_from: String,
    zones: Vec<String>,
}

/// Every resource a cancel is exercised against: each resource the store seeds, with the
/// first and the last of its zones taken in turn, plus one registered resource.
fn rows(engine: &engine::InMemoryEngine) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (index, (resource, zones)) in seeded(engine).into_iter().enumerate() {
        let away_from = if index % 2 == 0 {
            zones[0].clone()
        } else {
            zones[zones.len() - 1].clone()
        };
        rows.push(Row {
            resource,
            away_from,
            zones,
        });
    }
    let registered_zones = ["use1-az1", "use1-az2", "use1-az3"];
    rows.push(Row {
        resource: registered(engine, "canceled", &registered_zones),
        away_from: "use1-az2".to_string(),
        zones: registered_zones.iter().map(|z| (*z).to_string()).collect(),
    });
    rows
}

/// RULE: cancelling a practice run moves the stored shift to `CANCELED`, so the cancel's
/// answer and every later read report it that way, and the weight of the zone it moved
/// traffic away from goes back to 1.
#[tokio::test]
async fn a_practice_run_a_caller_cancels_is_canceled_and_stops_moving_weight() {
    let (router, engine) = emulator();
    assert!(
        ZONAL_SHIFT_STATUS_VALUES.contains(&"CANCELED"),
        "CANCELED is not a ZonalShiftStatus the model declares"
    );
    for row in rows(&engine) {
        let (resource, away_from, zones) = (&row.resource, &row.away_from, &row.zones);
        let case = format!("{resource} away from {away_from}");
        given_configuration(&router, resource, &[]).await;
        let (status, started) =
            start_practice_run(&router, resource, away_from, "a run to cancel").await;
        assert_eq!(
            status, 200,
            "case {case:?}: the practice run starts first, got {status} {started}"
        );
        let zonal_shift_id = started["zonalShiftId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: zonalShiftId is required, got {started}"))
            .to_string();

        let (status, canceled) = cancel_practice_run(&router, &zonal_shift_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: cancelling the run answers 200, got {status} {canceled}"
        );
        assert_eq!(
            (
                canceled["zonalShiftId"].as_str(),
                canceled["resourceIdentifier"].as_str(),
                canceled["awayFrom"].as_str(),
                canceled["comment"].as_str(),
                canceled["status"].as_str(),
            ),
            (
                Some(zonal_shift_id.as_str()),
                Some(resource.as_str()),
                Some(away_from.as_str()),
                Some("a run to cancel"),
                Some("CANCELED"),
            ),
            "case {case:?}: expected the run the caller started, CANCELED, got {canceled}"
        );
        assert_eq!(
            canceled["startTime"], started["startTime"],
            "case {case:?}: the cancel reports the start the run began at, got {canceled}"
        );

        // Read back through the resource's own view: the run is no longer a shift in
        // progress, so every zone the resource is deployed in carries weight 1 again.
        let (status, view) = get_managed_resource(&router, resource).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetManagedResource answers 200, got {status} {view}"
        );
        assert!(
            reported_shift(&view, &zonal_shift_id).is_none(),
            "case {case:?}: a canceled run is no shift in progress, got {view}"
        );
        for zone in zones {
            assert_eq!(
                view["appliedWeights"][zone].as_f64(),
                Some(1.0),
                "case {case:?}: expected weight 1 for {zone} once the run is canceled, got {view}"
            );
        }

        // And through the list of shifts, which reports completed shifts as well.
        let listed = listed_shift(&router, &zonal_shift_id).await;
        assert_eq!(
            (
                listed["status"].as_str(),
                listed["shiftType"].as_str(),
                listed["resourceIdentifier"].as_str(),
            ),
            (
                Some("CANCELED"),
                Some("PRACTICE_RUN"),
                Some(resource.as_str()),
            ),
            "case {case:?}: expected the canceled practice run listed, got {listed}"
        );
    }
}

/// The kinds of shift this operation does not take a target of, and the name ARC states
/// each by in the refusal.
///
/// ARC states a CUSTOMER-INITIATED shift as `MANUAL` — the shift a customer started by
/// hand, as against the ones ARC runs itself — and the shifts ARC runs by the tokens the
/// model's `ShiftType` gives them (`smithy_query(query="shape", shape="ShiftType")`).
const OTHER_SHIFT_TYPES: &[(ShiftType, &str, &str)] = &[
    (ShiftType::ZonalShift, "ZONAL_SHIFT", "MANUAL"),
    (ShiftType::FisExperiment, "FIS_EXPERIMENT", "FIS_EXPERIMENT"),
    (
        ShiftType::ZonalAutoshift,
        "ZONAL_AUTOSHIFT",
        "ZONAL_AUTOSHIFT",
    ),
];

/// RULE: the only shift a practice-run cancel takes a target of is one of the
/// `PRACTICE_RUN` type, so a shift of any other type is refused 400 `ValidationException`
/// with the reason the model names for the mismatch and a message naming the kind of
/// shift it turned out to be, and nothing is written.
///
/// Rows differ in the KIND of shift the call named — every type that is not a practice
/// run, which is what the one variable part of the refusal generalises over — and in the
/// resource the shift is for, the zone it moves traffic away from and the length of time
/// it was started for. The boundary is asserted too: the same shift IS canceled through
/// `CancelZonalShift`, which documents every kind of zonal shift.
#[tokio::test]
async fn cancelling_a_shift_that_is_not_a_practice_run_is_refused_400() {
    let (router, engine) = emulator();
    assert!(
        VALIDATION_EXCEPTION_REASON_VALUES.contains(&UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_REASON),
        "{UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_REASON} is not a ValidationExceptionReason the \
         model declares"
    );
    // The lengths of time come from the model's own form for `expiresIn`: whole minutes
    // and whole hours (`StartZonalShiftRequest$expiresIn`).
    let expiries = ["15m", "1h", "4320m"];
    let rows = rows(&engine);
    for (index, row) in rows.iter().enumerate() {
        for (shift_type, wire_type, refusal_name) in OTHER_SHIFT_TYPES {
            let expires_in = expiries[index % expiries.len()];
            let case = format!(
                "a {wire_type} shift for {} away from {} for {expires_in}",
                row.resource, row.away_from
            );
            let (status, started) = call(
                &router,
                "POST",
                "/zonalshifts",
                &start_body(&row.resource, &row.away_from, expires_in, "not a practice run"),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: the shift starts first, got {status} {started}"
            );
            let zonal_shift_id = started["zonalShiftId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: zonalShiftId is required, got {started}"))
                .to_string();
            if *shift_type != ShiftType::ZonalShift {
                // ARC starts the shifts of these types itself and models no operation a
                // caller starts one with, so the stored shift is given the type directly
                // — the same seam the clock-driven cases use.
                let state = engine.state::<ZonalShiftStore>();
                let mut store = state.lock();
                let shift = store
                    .zonal_shifts
                    .get_mut(&zonal_shift_id)
                    .unwrap_or_else(|| panic!("case {case:?}: the shift is stored"));
                shift.shift_type = *shift_type;
            }

            let (status, refused) = cancel_practice_run(&router, &zonal_shift_id).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a shift that is not a practice run is refused 400, got {status} \
                 {refused}"
            );
            assert_eq!(
                (
                    error_code(&refused).as_str(),
                    refused["reason"].as_str(),
                    refused["message"].as_str(),
                ),
                (
                    "ValidationException",
                    Some(UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_REASON),
                    Some(
                        format!("{UNSUPPORTED_PRACTICE_CANCEL_SHIFT_TYPE_MESSAGE}{refusal_name}")
                            .as_str()
                    ),
                ),
                "case {case:?}: expected the sentence naming the kind of shift, and the reason \
                 for the class, got {refused}"
            );

            // The refusal wrote nothing: the shift is still in progress and still moves
            // the weight of the zone it shifts away from.
            let (_, view) = get_managed_resource(&router, &row.resource).await;
            let still_held = reported_shift(&view, &zonal_shift_id).unwrap_or_else(|| {
                panic!("case {case:?}: the refused shift is still in progress, got {view}")
            });
            assert_eq!(
                still_held["shiftType"].as_str(),
                Some(*wire_type),
                "case {case:?}: the shift kept its type, got {still_held}"
            );
            assert_eq!(
                view["appliedWeights"][&row.away_from].as_f64(),
                Some(0.0),
                "case {case:?}: the refused cancel left the shifted weight in place, got {view}"
            );

            // The boundary: the operation that documents every kind of shift cancels it.
            let (status, canceled) = cancel_zonal_shift(&router, &zonal_shift_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: CancelZonalShift cancels the same shift, got {status} {canceled}"
            );
            assert_eq!(
                canceled["status"].as_str(),
                Some("CANCELED"),
                "case {case:?}: expected the shift canceled, got {canceled}"
            );
        }
    }
}

/// RULE: a practice run that is no longer `ACTIVE` has nothing left to cancel, so it is
/// refused 409 `ConflictException` with the reason the model names for that precondition.
///
/// The two ways a run leaves `ACTIVE` are both rows: a caller canceled it, and the clock
/// ended its run.
#[tokio::test]
async fn cancelling_a_practice_run_that_is_no_longer_active_conflicts_409() {
    let (router, engine) = emulator();
    assert!(
        CONFLICT_EXCEPTION_REASON_VALUES.contains(&ZONAL_SHIFT_NOT_ACTIVE_REASON),
        "{ZONAL_SHIFT_NOT_ACTIVE_REASON} is not a ConflictExceptionReason the model declares"
    );
    for row in rows(&engine) {
        given_configuration(&router, &row.resource, &[]).await;
        for already_over in ["canceled by a caller", "ended by the clock"] {
            let case = format!("{} {already_over}", row.resource);
            let (status, started) =
                start_practice_run(&router, &row.resource, &row.away_from, "a run that ends").await;
            assert_eq!(
                status, 200,
                "case {case:?}: the practice run starts first, got {status} {started}"
            );
            let zonal_shift_id = started["zonalShiftId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: zonalShiftId is required, got {started}"))
                .to_string();

            if already_over == "canceled by a caller" {
                let (status, canceled) = cancel_practice_run(&router, &zonal_shift_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the first cancel answers 200, got {status} {canceled}"
                );
            } else {
                // No modelled operation moves the clock, so the stored run is started far
                // enough in the past that its 30 minutes are up — the same seam the
                // clock-driven cases of the sibling suites use.
                let over = SystemTime::now()
                    .checked_sub(PRACTICE_RUN_DURATION + Duration::from_secs(60))
                    .expect("an instant half an hour ago is representable");
                let state = engine.state::<ZonalShiftStore>();
                let mut store = state.lock();
                let shift = store
                    .zonal_shifts
                    .get_mut(&zonal_shift_id)
                    .unwrap_or_else(|| panic!("case {case:?}: the run is stored"));
                shift.start_time = over;
                shift.expiry_time = over;
            }

            let (status, refused) = cancel_practice_run(&router, &zonal_shift_id).await;
            assert_eq!(
                status, 409,
                "case {case:?}: a run that is over conflicts, got {status} {refused}"
            );
            assert_eq!(
                (
                    error_code(&refused).as_str(),
                    refused["reason"].as_str(),
                    refused["zonalShiftId"].as_str(),
                    refused["message"].as_str(),
                ),
                (
                    "ConflictException",
                    Some(ZONAL_SHIFT_NOT_ACTIVE_REASON),
                    Some(zonal_shift_id.as_str()),
                    Some(ZONAL_SHIFT_NOT_ACTIVE_MESSAGE),
                ),
                "case {case:?}: expected the status precondition's refusal, got {refused}"
            );
        }
    }
}

/// RULE: the target is authorized before it is resolved, so a `zonalShiftId` ARC holds no
/// shift for is refused 403 `AccessDeniedException`, which discloses nothing about whether
/// the identifier names anything — the table of forms and sizes it spans is the one
/// `access_denied_precedence.rs` states the rule over.
#[tokio::test]
async fn cancelling_a_run_arc_holds_no_shift_for_is_refused_403() {
    let (router, _engine) = emulator();
    for (id, class) in UNKNOWN_ZONAL_SHIFT_IDS {
        let (status, answered, refused) = super::zonal_shift_lifecycle::refused(
            &router,
            "DELETE",
            &format!("/practiceruns/{id}"),
            "",
        )
        .await;
        assert_eq!(
            status, 403,
            "case {class:?}: an unauthorized target is refused 403, got {status} {refused}"
        );
        assert_eq!(
            answered, "AccessDeniedException",
            "case {class:?}: expected AccessDeniedException, got {refused}"
        );
        assert!(
            refused["Message"].is_null() || refused["Message"].as_str() == Some(""),
            "case {class:?}: a denial on a shift id states no reason, got {refused}"
        );
        assert!(
            !refused.to_string().contains(*id),
            "case {class:?}: the refusal must not disclose the target {id:?}, got {refused}"
        );
    }
}
