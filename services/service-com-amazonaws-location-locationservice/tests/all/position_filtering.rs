// SPDX-License-Identifier: CC0-1.0

//! `PositionFiltering`: which position updates a tracker STORES, over the wire.
//!
//! RULE: a tracker's `PositionFiltering` mode decides whether an accepted update is
//! persisted at all, by comparing it against what that device already holds —
//! `TimeBased` keeps at most one sample per 30-second interval per device,
//! `DistanceBased` stores an update only once the device has moved at least 30 m from
//! its last known position, and `AccuracyBased` only once it has moved at least the sum
//! of the previous and the current update's horizontal accuracy (0 m when an update
//! carries no `Accuracy`); an absent `PositionFiltering` is `TimeBased`. A filtered
//! update is dropped SILENTLY: the call still answers HTTP 200 with an empty `Errors`
//! list, and the device's last known position and its history stay at the sample stored
//! before it.
//!
//! Sources: the `BatchUpdateDevicePosition` operation doc ("Position updates are handled
//! based on the PositionFiltering property of the tracker … stored at a maximum of one
//! position per 30 second interval … DistanceBased filtering, location data is stored …
//! only if the device has moved more than 30 m (98.4 ft) … uses the default value {
//! "Horizontal": 0} when accuracy is not provided") and the
//! `CreateTrackerRequest$PositionFiltering` member doc (the per-mode rules and "If not
//! specified, the default value is `TimeBased`").
//!
//! The tables span what the rule generalises over: the mode (each enum member and the
//! absent one), the quantity each mode measures (metres moved, milliseconds between
//! instants, metres against the summed accuracies) at zero, well under the threshold,
//! just under it, exactly ON the published threshold, just over and far over, whether
//! the comparison is against the last STORED sample or the last submitted one, how the
//! updates arrive (one per request and several in one batch), and whether two devices
//! under one tracker filter each other.
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so a sample that survives
//! from a write call to a read call survived because it was PERSISTED.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;

// The one HTTP harness this crate's suites share.
use crate::harness::{call, emulator, wire_time, Answer};

/// Mean Earth radius in metres, for this suite's OWN great-circle oracle: the arc the
/// rule's thresholds are measured along is the shortest path between two WGS 84
/// positions on a sphere of this radius.
const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// The position every case measures from: downtown Seattle, well inside the WGS 84
/// domain and far from a pole, so a metre of northward movement is a metre.
const BASE: [f64; 2] = [-122.3321, 47.6062];

/// The instant the first sample of a case carries; later samples are offset from it.
const BASE_TIME: &str = "2024-03-01T12:00:00Z";

/// This suite's own haversine oracle, written from the formula rather than read from
/// the crate, so a case says what distance it means independently of the service.
fn great_circle_m(from: [f64; 2], to: [f64; 2]) -> f64 {
    let from_latitude = from[1].to_radians();
    let to_latitude = to[1].to_radians();
    let delta_latitude = to_latitude - from_latitude;
    let delta_longitude = (to[0] - from[0]).to_radians();
    let chord = (delta_latitude / 2.0).sin().powi(2)
        + from_latitude.cos() * to_latitude.cos() * (delta_longitude / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * chord.clamp(0.0, 1.0).sqrt().asin()
}

/// The position `metres` due north of `from`: a displacement along a meridian, whose
/// great-circle length is exactly that latitude change's arc.
fn north_of(from: [f64; 2], metres: f64) -> [f64; 2] {
    let metres_per_degree = EARTH_RADIUS_M * std::f64::consts::PI / 180.0;
    [from[0], from[1] + metres / metres_per_degree]
}

/// An ISO 8601 instant `seconds` after [`BASE_TIME`], as the wire carries it.
fn at_offset(seconds: f64) -> String {
    let base = DateTime::from_str(BASE_TIME, Format::DateTime)
        .expect("the suite's base instant is ISO 8601")
        .to_millis()
        .expect("the suite's base instant fits in epoch millis");
    DateTime::from_millis(base + (seconds * 1_000.0).round() as i64)
        .fmt(Format::DateTime)
        .expect("an epoch-millis instant renders as ISO 8601")
}

/// Create a tracker, with `PositionFiltering` when the case names one and without the
/// member at all when it does not (the absent-member row).
async fn create_tracker(router: &MultiServiceRouter, name: &str, filtering: Option<&str>) {
    let mut body = json!({ "TrackerName": name });
    if let Some(mode) = filtering {
        body["PositionFiltering"] = json!(mode);
    }
    let answer = call(router, "POST", "/tracking/v0/trackers", Some(body)).await;
    assert_eq!(
        answer.status, 200,
        "CreateTracker {name:?} ({filtering:?}): expected 200, got {} {}",
        answer.status, answer.body
    );
}

/// One `DevicePositionUpdate` payload, with `Accuracy` only when the case names one.
fn update(device: &str, sample_time: &str, position: [f64; 2], accuracy: Option<f64>) -> Value {
    let mut payload = json!({
        "DeviceId": device,
        "SampleTime": wire_time(sample_time),
        "Position": [position[0], position[1]],
    });
    if let Some(horizontal) = accuracy {
        payload["Accuracy"] = json!({ "Horizontal": horizontal });
    }
    payload
}

/// Send one batch and assert the rule's silence: 200 with an empty `Errors` list,
/// whether or not the tracker stored what it carried.
async fn submit(router: &MultiServiceRouter, tracker: &str, case: &str, updates: Vec<Value>) {
    let answer = call(
        router,
        "POST",
        &format!("/tracking/v0/trackers/{tracker}/positions"),
        Some(json!({ "Updates": updates })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: BatchUpdateDevicePosition must answer 200, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.json().get("Errors").and_then(Value::as_array),
        Some(&vec![]),
        "case {case:?}: filtering is silent, so Errors must be empty, got {}",
        answer.body
    );
}

/// The `[longitude, latitude]` pairs the device's whole stored history reports, oldest
/// first.
async fn stored_positions(
    router: &MultiServiceRouter,
    tracker: &str,
    device: &str,
    case: &str,
) -> Vec<[f64; 2]> {
    let answer = call(
        router,
        "POST",
        &format!("/tracking/v0/trackers/{tracker}/devices/{device}/list-positions"),
        // A window wide enough to hold every instant this suite writes.
        Some(json!({
            "StartTimeInclusive": wire_time("2024-02-01T00:00:00Z"),
            "EndTimeExclusive": wire_time("2024-04-01T00:00:00Z"),
        })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetDevicePositionHistory must answer 200, got {} {}",
        answer.status, answer.body
    );
    positions_of(&answer, "DevicePositions", case)
}

/// The device's last known position, or `None` when the tracker holds no sample for it.
async fn latest_position(
    router: &MultiServiceRouter,
    tracker: &str,
    device: &str,
    case: &str,
) -> Option<[f64; 2]> {
    let answer = call(
        router,
        "GET",
        &format!("/tracking/v0/trackers/{tracker}/devices/{device}/positions/latest"),
        None,
    )
    .await;
    match answer.status {
        200 => Some(position_of(&answer.json(), case)),
        404 => None,
        status => panic!(
            "case {case:?}: GetDevicePosition must answer 200 or 404, got {status} {}",
            answer.body
        ),
    }
}

/// The `Position` of one reported `DevicePosition`.
fn position_of(reported: &Value, case: &str) -> [f64; 2] {
    let pair = reported
        .get("Position")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a reported sample must carry a Position, got {reported}"));
    assert_eq!(
        pair.len(),
        2,
        "case {case:?}: a Position is [longitude, latitude], got {reported}"
    );
    [
        pair[0]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: longitude must be a number, got {reported}")),
        pair[1]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: latitude must be a number, got {reported}")),
    ]
}

/// Every `Position` a list-shaped answer reports, in the order it reported them.
fn positions_of(answer: &Answer, member: &str, case: &str) -> Vec<[f64; 2]> {
    let body = answer.json();
    body.get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be a list, got {body}"))
        .iter()
        .map(|reported| position_of(reported, case))
        .collect()
}

/// How close two positions must be to count as the same one: far below the metre scale
/// every threshold in the rule is stated at.
const TOLERANCE_M: f64 = 0.001;

/// Assert the device holds exactly `expected`, oldest first.
fn assert_history(case: &str, held: &[[f64; 2]], expected: &[[f64; 2]]) {
    assert_eq!(
        held.len(),
        expected.len(),
        "case {case:?}: expected {} stored sample(s) {expected:?}, got {held:?}",
        expected.len()
    );
    for (index, (got, want)) in held.iter().zip(expected).enumerate() {
        let apart = great_circle_m(*got, *want);
        assert!(
            apart <= TOLERANCE_M,
            "case {case:?}: stored sample {index} must be {want:?}, got {got:?} ({apart} m away)"
        );
    }
}

// ---------------------------------------------------------------------------
// The oracle this suite's tables are written against
// ---------------------------------------------------------------------------

/// RULE (this suite's own helper): a row that says "n metres" submits a position that
/// really is n metres from the one before it, so a threshold case means what it says.
#[test]
fn the_offsets_this_suite_submits_are_the_distances_it_names() {
    for metres in [0.0, 1.5, 29.0, 29.99, 30.01, 150.0, 10_000.0] {
        let measured = great_circle_m(BASE, north_of(BASE, metres));
        assert!(
            (measured - metres).abs() <= TOLERANCE_M,
            "case {metres} m: the submitted offset must be that far from the base, got {measured} m"
        );
    }
}

// ---------------------------------------------------------------------------
// DistanceBased
// ---------------------------------------------------------------------------

/// One distance row: how far the second update moved, and whether the tracker keeps it.
struct DistanceCase {
    case: &'static str,
    metres: f64,
    stored: bool,
}

/// RULE: a `DistanceBased` tracker stores an update only once the device has moved at
/// least 30 m from its last known position.
///
/// Rows span the published threshold: no movement, GPS-jitter movement, a metre under
/// the bound, a centimetre under it, a centimetre over it, and far past it. A
/// great-circle distance is a floating-point quantity, so the bound is approached from
/// each side rather than asserted at a knife edge.
#[tokio::test]
async fn distance_based_filtering_stores_only_movement_past_the_published_threshold() {
    let router = emulator();
    create_tracker(&router, "distance", Some("DistanceBased")).await;

    let cases = [
        DistanceCase { case: "no movement at all", metres: 0.0, stored: false },
        DistanceCase { case: "GPS jitter, about a metre and a half", metres: 1.5, stored: false },
        DistanceCase { case: "a metre under the threshold", metres: 29.0, stored: false },
        DistanceCase { case: "a centimetre under the threshold", metres: 29.99, stored: false },
        DistanceCase { case: "a centimetre over the threshold", metres: 30.01, stored: true },
        DistanceCase { case: "a block away", metres: 150.0, stored: true },
        DistanceCase { case: "ten kilometres away", metres: 10_000.0, stored: true },
    ];

    for (index, DistanceCase { case, metres, stored }) in cases.into_iter().enumerate() {
        let device = format!("distance-{index}");
        let moved = north_of(BASE, metres);
        submit(
            &router,
            "distance",
            case,
            vec![update(&device, &at_offset(0.0), BASE, None)],
        )
        .await;
        submit(
            &router,
            "distance",
            case,
            vec![update(&device, &at_offset(60.0), moved, None)],
        )
        .await;

        let expected: Vec<[f64; 2]> = if stored {
            vec![BASE, moved]
        } else {
            vec![BASE]
        };
        let held = stored_positions(&router, "distance", &device, case).await;
        assert_history(case, &held, &expected);
        let last = latest_position(&router, "distance", &device, case).await;
        let want = *expected.last().expect("every row stores the first sample");
        assert!(
            last.is_some_and(|got| great_circle_m(got, want) <= TOLERANCE_M),
            "case {case:?}: the last known position must be {want:?}, got {last:?}"
        );
    }
}

/// RULE: the comparison is against the device's last STORED position, not against the
/// last update submitted — so a device creeping in sub-threshold steps stores nothing
/// until it has moved past the threshold from the sample the tracker kept.
#[tokio::test]
async fn distance_based_filtering_measures_from_the_last_stored_position() {
    let router = emulator();
    create_tracker(&router, "creeping", Some("DistanceBased")).await;
    let device = "creeper";
    let case = "four 20 m steps then one 40 m step";

    submit(
        &router,
        "creeping",
        case,
        vec![update(device, &at_offset(0.0), BASE, None)],
    )
    .await;
    // Each step is 20 m from the step before it and 20, 40, 60, 80 m from the base.
    // Were the predicate measured against the last SUBMITTED position, every one of
    // them would be dropped; measured against the last STORED one, the second step is
    // already past the threshold.
    for step in 1..=4 {
        submit(
            &router,
            "creeping",
            case,
            vec![update(
                device,
                &at_offset(60.0 * step as f64),
                north_of(BASE, 20.0 * step as f64),
                None,
            )],
        )
        .await;
    }

    let held = stored_positions(&router, "creeping", device, case).await;
    assert_history(
        case,
        &held,
        &[BASE, north_of(BASE, 40.0), north_of(BASE, 80.0)],
    );
}

// ---------------------------------------------------------------------------
// TimeBased, and the absent member
// ---------------------------------------------------------------------------

/// One time row: how long after the first sample the second one was taken, and whether
/// the tracker keeps it.
struct TimeCase {
    case: &'static str,
    seconds: f64,
    stored: bool,
}

/// RULE: a `TimeBased` tracker stores at most one position per 30-second interval for a
/// device, whatever the device's movement; and a tracker created WITHOUT
/// `PositionFiltering` is `TimeBased`, so both read the same.
///
/// Rows span the published interval: the same instant, a second later, a frequent
/// update, a hair under the interval, exactly ON it, just over it and well over it. The
/// two updates of a row report the SAME position, so only the instants can decide.
#[tokio::test]
async fn time_based_filtering_stores_at_most_one_sample_per_interval() {
    let cases = [
        TimeCase { case: "the same instant again", seconds: 0.0, stored: false },
        TimeCase { case: "one second later", seconds: 1.0, stored: false },
        TimeCase { case: "ten seconds later", seconds: 10.0, stored: false },
        TimeCase { case: "a millisecond under the interval", seconds: 29.999, stored: false },
        TimeCase { case: "exactly one interval later", seconds: 30.0, stored: true },
        TimeCase { case: "a millisecond over the interval", seconds: 30.001, stored: true },
        TimeCase { case: "a minute later", seconds: 60.0, stored: true },
    ];

    // The mode spelled out, and the member left absent: the documented default.
    for mode in [Some("TimeBased"), None] {
        let router = emulator();
        create_tracker(&router, "timed", mode).await;

        for (index, TimeCase { case, seconds, stored }) in cases.iter().enumerate() {
            let case = &format!("{mode:?}: {case}");
            let device = format!("timed-{index}");
            submit(
                &router,
                "timed",
                case,
                vec![update(&device, &at_offset(0.0), BASE, None)],
            )
            .await;
            submit(
                &router,
                "timed",
                case,
                vec![update(&device, &at_offset(*seconds), BASE, None)],
            )
            .await;

            let held = stored_positions(&router, "timed", &device, case).await;
            let expected = if *stored { 2 } else { 1 };
            assert_eq!(
                held.len(),
                expected,
                "case {case:?}: expected {expected} stored sample(s), got {held:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AccuracyBased
// ---------------------------------------------------------------------------

/// One accuracy row: the horizontal accuracy each of the two updates reported (absent
/// when `None`), how far the device moved between them, and whether the tracker keeps
/// the second one.
struct AccuracyCase {
    case: &'static str,
    first: Option<f64>,
    second: Option<f64>,
    metres: f64,
    stored: bool,
}

/// RULE: an `AccuracyBased` tracker stores an update only once the device has moved at
/// least the SUM of the previous and the current update's horizontal accuracy, and an
/// update that carries no `Accuracy` counts as 0 m.
///
/// The documented example is one row: "if two consecutive updates from a device have a
/// horizontal accuracy of 5 m and 10 m, the second update is ignored if the device has
/// moved less than 15 m". The others span the sum (absent on both sides, absent on one,
/// small, large) against movement under it, a centimetre either side of it and well over
/// it.
#[tokio::test]
async fn accuracy_based_filtering_requires_movement_past_the_summed_accuracies() {
    let router = emulator();
    create_tracker(&router, "accuracy", Some("AccuracyBased")).await;

    let cases = [
        AccuracyCase {
            case: "the documented 5 m and 10 m pair, moved under the sum",
            first: Some(5.0),
            second: Some(10.0),
            metres: 10.0,
            stored: false,
        },
        AccuracyCase {
            case: "the documented pair, a centimetre under the sum",
            first: Some(5.0),
            second: Some(10.0),
            metres: 14.99,
            stored: false,
        },
        AccuracyCase {
            case: "the documented pair, a centimetre over the sum",
            first: Some(5.0),
            second: Some(10.0),
            metres: 15.01,
            stored: true,
        },
        AccuracyCase {
            case: "the documented pair, moved past the sum",
            first: Some(5.0),
            second: Some(10.0),
            metres: 20.0,
            stored: true,
        },
        AccuracyCase {
            case: "no accuracy on either update: the sum is zero, so jitter is stored",
            first: None,
            second: None,
            metres: 0.5,
            stored: true,
        },
        AccuracyCase {
            case: "accuracy on the stored sample only, moved under it",
            first: Some(40.0),
            second: None,
            metres: 30.0,
            stored: false,
        },
        AccuracyCase {
            case: "accuracy on the new update only, moved past it",
            first: None,
            second: Some(25.0),
            metres: 40.0,
            stored: true,
        },
        AccuracyCase {
            case: "coarse accuracies, moved under the sum",
            first: Some(100.0),
            second: Some(100.0),
            metres: 150.0,
            stored: false,
        },
        AccuracyCase {
            case: "coarse accuracies, moved past the sum",
            first: Some(100.0),
            second: Some(100.0),
            metres: 250.0,
            stored: true,
        },
    ];

    for (index, row) in cases.into_iter().enumerate() {
        let AccuracyCase {
            case,
            first,
            second,
            metres,
            stored,
        } = row;
        let device = format!("accuracy-{index}");
        let moved = north_of(BASE, metres);
        submit(
            &router,
            "accuracy",
            case,
            vec![update(&device, &at_offset(0.0), BASE, first)],
        )
        .await;
        submit(
            &router,
            "accuracy",
            case,
            vec![update(&device, &at_offset(60.0), moved, second)],
        )
        .await;

        let expected: Vec<[f64; 2]> = if stored {
            vec![BASE, moved]
        } else {
            vec![BASE]
        };
        let held = stored_positions(&router, "accuracy", &device, case).await;
        assert_history(case, &held, &expected);
    }
}

// ---------------------------------------------------------------------------
// What a filtered update leaves behind
// ---------------------------------------------------------------------------

/// RULE: a filtered update changes nothing a later caller sees — the history keeps only
/// the samples that passed the predicate and the last known position stays at the newest
/// of them — and the updates of ONE batch are filtered against the samples stored before
/// them, so arriving together and arriving one per request store the same thing.
#[tokio::test]
async fn a_filtered_update_leaves_the_history_and_the_last_known_position_alone() {
    // Rows differ in how the three updates arrive: one per request, and all three in a
    // single batch.
    for (case, batched) in [("one update per request", false), ("one batch", true)] {
        let router = emulator();
        create_tracker(&router, "trip", Some("DistanceBased")).await;
        let device = "near-duplicate";

        // A first point, a near-duplicate a metre and a half away, and a point ten
        // kilometres away: the near-duplicate is under the threshold from the first.
        let near = north_of(BASE, 1.5);
        let far = north_of(BASE, 10_000.0);
        let updates = vec![
            update(device, &at_offset(0.0), BASE, None),
            update(device, &at_offset(5.0), near, None),
            update(device, &at_offset(10.0), far, None),
        ];

        if batched {
            submit(&router, "trip", case, updates).await;
        } else {
            for (step, payload) in updates.into_iter().enumerate() {
                submit(&router, "trip", case, vec![payload]).await;
                // After the near-duplicate, the device's last known position is still
                // the first point: a dropped update is not a new last known position.
                let want = if step == 2 { far } else { BASE };
                let last = latest_position(&router, "trip", device, case).await;
                assert!(
                    last.is_some_and(|got| great_circle_m(got, want) <= TOLERANCE_M),
                    "case {case:?} after update {step}: the last known position must be \
                     {want:?}, got {last:?}"
                );
            }
        }

        let held = stored_positions(&router, "trip", device, case).await;
        assert_history(case, &held, &[BASE, far]);
    }
}

/// RULE: the predicate compares an update against the SAME device's stored samples, so
/// one device's position never filters another's — including two devices standing at the
/// same spot under one tracker.
#[tokio::test]
async fn filtering_compares_a_device_against_its_own_samples_only() {
    let router = emulator();
    create_tracker(&router, "fleet", Some("DistanceBased")).await;
    let case = "two devices at the same spot";

    submit(
        &router,
        "fleet",
        case,
        vec![
            update("lead", &at_offset(0.0), BASE, None),
            // A metre and a half from the lead device's position: well under the
            // threshold, but this device's FIRST sample, so it is stored.
            update("follower", &at_offset(0.0), north_of(BASE, 1.5), None),
        ],
    )
    .await;

    for (device, expected) in [("lead", BASE), ("follower", north_of(BASE, 1.5))] {
        let held = stored_positions(&router, "fleet", device, case).await;
        assert_history(&format!("{case}: {device}"), &held, &[expected]);
    }
}
