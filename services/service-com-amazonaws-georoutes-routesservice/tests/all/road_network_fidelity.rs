// SPDX-License-Identifier: CC0-1.0

//! What a route answer of this service is DERIVED from: the road network in
//! `src/state.rs`, and nothing else.
//!
//! The model fixes no routing algorithm, so these suites pin the rules a road answer
//! follows rather than any one recorded number, each over a class of requests:
//!
//! * a connection of an optimized tour is the SAME traversal of the road graph that the
//!   route and the matrix report for its pair — `WaypointOptimizationConnection.Distance`
//!   and `.TravelDuration` are "the total distance/duration of travel for this connection",
//!   so the three operations cannot disagree about one pair of places (that the SEQUENCE
//!   those connections walk is the cheapest order of the stops is the sibling
//!   `all/success_response_synthesis.rs`'s rule, pinned there);
//! * travelling a road takes its length at the road's pace, and a road's pace includes what
//!   is given up at the junctions it crosses, so a journey on the local streets takes
//!   longer than its length at any street's posted limit while a run on the
//!   grade-separated class does not;
//! * every position a response reports — the point read off the network and the one the
//!   caller supplied alike — lies on the ten-millionth-of-a-degree grid this API expresses
//!   a coordinate on, so no response carries the artefacts of the arithmetic that located
//!   a point;
//! * `RoadSnapSnappedTracePoint.Confidence` ("Confidence value for the correctness of this
//!   point match") is read off how far the point had to move to reach its road: a point
//!   already on one is a certain match, and one as far off as this network allows is a poor
//!   one.

use serde_json::{json, Value};

// The network's own parameters — the grid spacing, the posted speed of a local street, the
// conversion to metres per second and the coordinate precision of this API — are the copies
// `all/road_graph_metrics.rs` keeps of `src/state.rs`, read from there rather than kept a
// second time here.
use crate::road_graph_metrics::{
    between_roads, on_a_road, KPH_TO_MPS, POSITION_PRECISION, ROAD_SPACING_DEG, STREET_SPEED_KPH,
};
use crate::routing::{great_circle_m, int, position, post, routes_service, MATRIX, OPTIMIZE, ROUTES, SNAP};

// ---------------------------------------------------------------------------
// One traversal behind every reported metric
// ---------------------------------------------------------------------------

/// One tour per row, differing in the stops it visits, whether it ends somewhere of its own,
/// the travel mode and the objective — what the rule below generalises over.
type Tour = (&'static str, [f64; 2], Vec<(&'static str, [f64; 2])>, Option<[f64; 2]>, &'static str, &'static str);

/// The tours the suites below drive. The stops of each are a few hundred metres to a few
/// kilometres apart, in three cities of two hemispheres, so every one of them is on the
/// network and no pair of them is trivially ordered.
fn tours() -> Vec<Tour> {
    vec![
        (
            "one stop, no destination",
            [-122.3321, 47.6062],
            vec![("market", [-122.3405, 47.6097])],
            None,
            "Car",
            "FastestRoute",
        ),
        (
            "two stops the request lists the far one first",
            [-122.3321, 47.6062],
            vec![("needle", [-122.3493, 47.6205]), ("square", [-122.3343, 47.6015])],
            None,
            "Car",
            "FastestRoute",
        ),
        (
            "two stops and a destination, shortest",
            [-123.1017, 49.2823],
            vec![("gastown", [-123.1067, 49.2834]), ("yaletown", [-123.1207, 49.2742])],
            Some([-123.1139, 49.2888]),
            "Car",
            "ShortestRoute",
        ),
        (
            "three stops by truck",
            [-123.1017, 49.2823],
            vec![
                ("east", [-123.0871, 49.2819]),
                ("south", [-123.1021, 49.2654]),
                ("west", [-123.1431, 49.2487]),
            ],
            None,
            "Truck",
            "FastestRoute",
        ),
        (
            "three stops in the southern hemisphere",
            [151.2093, -33.8688],
            vec![
                ("rocks", [151.2086, -33.8599]),
                ("surry", [151.2110, -33.8852]),
                ("pyrmont", [151.1957, -33.8696]),
            ],
            Some([151.2153, -33.8568]),
            "Car",
            "FastestRoute",
        ),
    ]
}

/// A connection of an optimized tour reports the road-graph traversal of ITS OWN PAIR: the
/// distance and the travel duration are the ones the matrix (and so the direct route)
/// reports for that ordered pair, whatever the tour's shape, mode or objective.
///
/// This is what makes the answer a road answer rather than an estimate beside one: a client
/// that routes a connection of the plan it was given gets the leg it was promised.
#[tokio::test]
async fn every_connection_reports_the_road_graph_traversal_of_its_own_pair() {
    let router = routes_service();

    for (case, origin, waypoints, destination, mode, objective) in tours() {
        let answer = optimize(&router, origin, &waypoints, destination, mode, objective, case).await;
        let places = named_places(origin, &waypoints, destination);
        let connections = answer["Connections"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: Connections is required, got {answer:?}"))
            .clone();
        let sequence = sequence_of(&answer, case);
        assert_eq!(
            connections.len(),
            sequence.len() - 1,
            "case {case:?}: one connection per consecutive pair of the sequence, got {connections:?}"
        );

        for (index, connection) in connections.iter().enumerate() {
            let case = &format!("{case} / connection {index}");
            let from = place_of(&places, &sequence[index], case);
            let to = place_of(&places, &sequence[index + 1], case);
            let (distance, duration) = traversal(&router, from, to, mode, objective, case).await;
            assert_eq!(
                int(connection, "Distance", case),
                distance,
                "case {case:?}: the connection covers the road distance between {from:?} and \
                 {to:?}, which the matrix reports as {distance} m, got {connection:?}"
            );
            assert_eq!(
                int(connection, "TravelDuration", case),
                duration,
                "case {case:?}: the connection takes what travelling that path takes, which the \
                 matrix reports as {duration} s, got {connection:?}"
            );
        }
    }
}

/// Travelling a road takes its length at the road's own pace, and the junctions the run
/// crosses are part of that pace: a journey on the local streets of this grid therefore
/// takes STRICTLY longer than its length at the fastest any street is posted at, while a run
/// along the grade-separated class — whose crossings cost nothing — takes exactly its length
/// at the posted speed.
///
/// Rows span the separation and the bearing of the journey, in two hemispheres; the
/// grade-separated run is one long straight along the motorway parallel of the grid.
#[tokio::test]
async fn a_journey_on_the_local_streets_takes_longer_than_its_length_at_the_street_limit() {
    let router = routes_service();
    let street_mps = STREET_SPEED_KPH * KPH_TO_MPS;
    // name, origin, destination: short journeys, which the graph answers on the local
    // streets throughout.
    let local: [(&str, [f64; 2], [f64; 2]); 4] = [
        ("a few hundred metres east", [-122.3321, 47.6062], [-122.3271, 47.6062]),
        ("a few hundred metres north", [-122.3321, 47.6062], [-122.3321, 47.6102]),
        ("a few hundred metres diagonally", [-123.1017, 49.2823], [-123.0977, 49.2793]),
        ("a few hundred metres south of the equator", [151.2093, -33.8688], [151.2043, -33.8718]),
    ];

    for (case, origin, destination) in local {
        let (distance, duration) = traversal(&router, origin, destination, "Car", "FastestRoute", case).await;
        let free_flow = distance as f64 / street_mps;
        assert!(
            (duration as f64) > free_flow + 1.0,
            "case {case:?}: the junctions crossed are part of the time, so {distance} m of local \
             streets takes more than the {free_flow:.0} s it would take at the {STREET_SPEED_KPH} \
             km/h a street is posted at, got {duration} s"
        );
    }

    // The parallel road nearest latitude 49.4 is at a multiple of 400 grid steps, where the
    // network puts a motorway: its crossings are grade-separated, so a run along it is
    // travelled at its posted speed and nothing is given up on the way. The run follows the
    // carriageway itself, midway between the meridian roads crossing it.
    let (distance, duration) = traversal(
        &router,
        [between_roads(-123.097_23), on_a_road(49.4)],
        [between_roads(-123.067_23), on_a_road(49.4)],
        "Car",
        "ShortestRoute",
        "along a motorway",
    )
    .await;
    let implied = distance as f64 / duration as f64;
    assert!(
        implied > street_mps,
        "case {:?}: a grade-separated run gives up nothing at a crossing, so it is quicker than \
         any street, got {distance} m in {duration} s, i.e. {implied:.2} m/s against the \
         {street_mps:.2} m/s a street is posted at",
        "along a motorway"
    );
}

// ---------------------------------------------------------------------------
// The coordinates a response carries
// ---------------------------------------------------------------------------

/// Every position a response reports lies on the ten-millionth-of-a-degree grid this API
/// expresses a coordinate on, and a position the CALLER supplied comes back as the caller
/// sent it: `OriginalPosition` is "the position provided in the request".
///
/// Rows span the precision of the submitted coordinate — fewer digits than the grid, exactly
/// as many, and more than the grid can carry — across the three operations that echo a
/// submitted position beside a computed one.
#[tokio::test]
async fn every_reported_position_is_on_the_coordinate_grid_of_the_api() {
    let router = routes_service();
    // name, the origin, the destination: the same place written to three precisions.
    let cases: [(&str, [f64; 2], [f64; 2]); 3] = [
        ("fewer digits than the grid", [-122.332, 47.606], [-122.309, 47.45]),
        (
            "exactly the digits of the grid",
            [-122.3410001, 47.6089001],
            [-122.3088001, 47.4501999],
        ),
        (
            "more digits than the grid can carry",
            [-122.33210000012345, 47.60620000098765],
            [-122.30880000054321, 47.45019990012345],
        ),
    ];

    for (case, origin, destination) in cases {
        for (operation, path, body) in [
            (
                "CalculateRoutes",
                ROUTES,
                json!({ "Origin": origin, "Destination": destination, "TravelMode": "Truck" }),
            ),
            (
                "SnapToRoads",
                SNAP,
                json!({ "TracePoints": [{ "Position": origin }, { "Position": destination }] }),
            ),
            (
                "OptimizeWaypoints",
                OPTIMIZE,
                json!({
                    "Origin": origin,
                    "Waypoints": [{ "Id": "stop", "Position": destination }],
                }),
            ),
        ] {
            let case = &format!("{case} / {operation}");
            let (status, answer, _) = post(&router, path, &body).await;
            assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");

            let reported = reported_positions(&answer);
            assert!(
                !reported.is_empty(),
                "case {case:?}: the answer reports positions, got {answer:?}"
            );
            for (member, value) in &reported {
                for coordinate in value {
                    let units = coordinate * 10_f64.powi(POSITION_PRECISION);
                    assert!(
                        (units - units.round()).abs() < 1e-3,
                        "case {case:?}: {member} is carried on the coordinate grid of this API, \
                         so it is a whole number of 1e-{POSITION_PRECISION} degrees, got \
                         {coordinate}"
                    );
                }
            }
            // An echoed position is the submitted one, on that same grid.
            for (member, value) in &reported {
                if !member.ends_with("OriginalPosition") {
                    continue;
                }
                let submitted = [origin, destination]
                    .into_iter()
                    .map(on_the_grid)
                    .find(|point| point == value);
                assert!(
                    submitted.is_some(),
                    "case {case:?}: {member} is the position provided in the request, got {value:?} \
                     for a request of {origin:?} and {destination:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The confidence of a snap
// ---------------------------------------------------------------------------

/// `Confidence` is read off how far the point had to move to reach its road: a point already
/// on a road is a certain match wherever in the world it is, a point off the road is less of
/// one, and the further off it is the less of one it is.
///
/// Rows span the place — three cities of two hemispheres, where the grid's blocks are of
/// different sizes — and the travel mode, which decides WHICH roads are candidates at all.
#[tokio::test]
async fn a_match_is_scored_by_how_far_the_point_moved_wherever_it_is() {
    let router = routes_service();
    // name, a position lying exactly on a parallel road of the network and midway between
    // two meridian roads (so the parallel road it sits on stays the nearest road for every
    // offset probed below). The carriageway is not at the round decimal the place is named
    // by, so the position is read off the network's own rule.
    let on_the_carriageway: [(&str, [f64; 2]); 3] = [
        ("Seattle", [between_roads(-122.332_25), on_a_road(47.606)]),
        ("Vancouver", [between_roads(-123.101_75), on_a_road(49.2825)]),
        ("Sydney", [between_roads(151.209_25), on_a_road(-33.869)]),
    ];

    for (place, point) in on_the_carriageway {
        for mode in ["Car", "Truck", "Pedestrian"] {
            let case = &format!("{place}, {mode}");
            let mut by_offset: Vec<(f64, f64)> = Vec::new();
            // Offsets along the axis the road runs across, from no offset at all to a fifth
            // of the grid spacing — short of the half-step the neighbouring meridian roads
            // sit at, so the road matched is the same one in every row.
            for steps in [0.0, 0.1, 0.2] {
                let probed = [point[0], point[1] + steps * ROAD_SPACING_DEG];
                let (confidence, snapped) = snap_one(&router, probed, mode, case).await;
                assert!(
                    (0.0..=1.0).contains(&confidence),
                    "case {case:?}: Confidence is in 0..1, got {confidence} at {steps} of the spacing"
                );
                let moved = great_circle_m(probed, snapped);
                by_offset.push((moved, confidence));
            }
            let (on_road_offset, certain) = by_offset[0];
            assert!(
                on_road_offset < 0.2 && certain >= 0.999,
                "case {case:?}: a point on a road was observed where the road is, so its match is \
                 certain, got {certain} for a match that moved it {on_road_offset:.3} m"
            );
            for pair in by_offset.windows(2) {
                let ((nearer, higher), (further, lower)) = (pair[0], pair[1]);
                assert!(
                    further > nearer && lower < higher,
                    "case {case:?}: a match that moved the point {further:.1} m is less certain \
                     than one that moved it {nearer:.1} m, got {lower} against {higher}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The ids of the sequence the answer visits, in order.
fn sequence_of(answer: &Value, case: &str) -> Vec<String> {
    answer["OptimizedWaypoints"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: OptimizedWaypoints is required, got {answer:?}"))
        .iter()
        .map(|waypoint| {
            waypoint["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: every optimized waypoint carries an Id, got {waypoint:?}"))
                .to_string()
        })
        .collect()
}

/// Every stop of a tour with the id the answer names it by, origin first and destination
/// last.
fn named_places(
    origin: [f64; 2],
    waypoints: &[(&'static str, [f64; 2])],
    destination: Option<[f64; 2]>,
) -> Vec<(String, [f64; 2])> {
    let mut places = vec![("origin".to_string(), origin)];
    places.extend(waypoints.iter().map(|(id, position)| ((*id).to_string(), *position)));
    if let Some(destination) = destination {
        places.push(("finish".to_string(), destination));
    }
    places
}

/// The position of the stop `id` names.
fn place_of(places: &[(String, [f64; 2])], id: &str, case: &str) -> [f64; 2] {
    places
        .iter()
        .find(|(name, _)| name == id)
        .map(|(_, position)| *position)
        .unwrap_or_else(|| panic!("case {case:?}: the answer names a stop {id:?} the request did not submit"))
}

/// An OptimizeWaypoints answer for one tour.
async fn optimize(
    router: &smithy_server_runtime::MultiServiceRouter,
    origin: [f64; 2],
    waypoints: &[(&'static str, [f64; 2])],
    destination: Option<[f64; 2]>,
    mode: &str,
    objective: &str,
    case: &str,
) -> Value {
    let mut body = json!({
        "Origin": origin,
        "OriginOptions": { "Id": "origin" },
        "Waypoints": waypoints
            .iter()
            .map(|(id, position)| json!({ "Id": id, "Position": position }))
            .collect::<Vec<Value>>(),
        "TravelMode": mode,
        "OptimizeSequencingFor": objective,
    });
    if let Some(destination) = destination {
        body["Destination"] = json!(destination);
        body["DestinationOptions"] = json!({ "Id": "finish" });
    }
    let (status, answer, _) = post(router, OPTIMIZE, &body).await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    answer
}

/// The road-graph traversal of one ordered pair, as the matrix reports it: its distance in
/// metres and what travelling it takes in seconds.
async fn traversal(
    router: &smithy_server_runtime::MultiServiceRouter,
    from: [f64; 2],
    to: [f64; 2],
    mode: &str,
    objective: &str,
    case: &str,
) -> (i64, i64) {
    let (status, answer, _) = post(
        router,
        MATRIX,
        &json!({
            "Origins": [{ "Position": from }],
            "Destinations": [{ "Position": to }],
            "TravelMode": mode,
            "OptimizeRoutingFor": objective,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    let cell = &answer["RouteMatrix"][0][0];
    assert!(
        cell["Error"].is_null(),
        "case {case:?}: the pair {from:?} -> {to:?} is on the network, so it is routed, got {cell:?}"
    );
    (int(cell, "Distance", case), int(cell, "Duration", case))
}

/// The confidence and the snapped position of the first trace point of a two-point trace.
async fn snap_one(
    router: &smithy_server_runtime::MultiServiceRouter,
    point: [f64; 2],
    mode: &str,
    case: &str,
) -> (f64, [f64; 2]) {
    let (status, answer, _) = post(
        router,
        SNAP,
        &json!({
            // A trace is two points at least; the second is far enough off not to be the
            // one under test.
            "TracePoints": [{ "Position": point }, { "Position": [point[0] + 0.004, point[1] + 0.004] }],
            "TravelMode": mode,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    let snapped = &answer["SnappedTracePoints"][0];
    (
        snapped["Confidence"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: Confidence is required, got {snapped:?}")),
        position(&snapped["SnappedPosition"], case),
    )
}

/// `point` on the coordinate grid this API expresses a position on.
fn on_the_grid(point: [f64; 2]) -> [f64; 2] {
    let scale = 10_f64.powi(POSITION_PRECISION);
    [
        (point[0] * scale).round() / scale,
        (point[1] * scale).round() / scale,
    ]
}

/// Every position the answer reports, as (the member path that carries it, the position).
fn reported_positions(answer: &Value) -> Vec<(String, [f64; 2])> {
    let mut found = Vec::new();
    collect_positions(answer, "$", &mut found);
    found
}

/// The member names that carry one position each.
const POSITION_MEMBERS: [&str; 4] = ["Position", "OriginalPosition", "SnappedPosition", "SnappedOrigin"];

fn collect_positions(value: &Value, path: &str, found: &mut Vec<(String, [f64; 2])>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                let path = format!("{path}.{name}");
                if POSITION_MEMBERS.contains(&name.as_str()) {
                    if let Some(point) = as_position(member) {
                        found.push((path.clone(), point));
                        continue;
                    }
                }
                if name == "LineString" {
                    for (index, vertex) in member.as_array().map(Vec::as_slice).unwrap_or_default().iter().enumerate() {
                        if let Some(point) = as_position(vertex) {
                            found.push((format!("{path}[{index}]"), point));
                        }
                    }
                    continue;
                }
                collect_positions(member, &path, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_positions(item, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

/// `value` as a `[longitude, latitude]` pair, when it is one.
fn as_position(value: &Value) -> Option<[f64; 2]> {
    let values = value.as_array()?;
    match values.as_slice() {
        [longitude, latitude] => Some([longitude.as_f64()?, latitude.as_f64()?]),
        _ => None,
    }
}
