// SPDX-License-Identifier: CC0-1.0

//! What a route metric of this service IS: the length of a path the road network offers,
//! and the time travelling that path takes at the speeds of the roads it runs on.
//!
//! `RouteMatrixEntry.Distance`/`Duration` and `RouteSummary.Distance`/`Duration` are "the
//! total distance of travel for the route" and "the expected duration of travel"
//! (`smithy_query(query="shape",shape="RouteMatrixEntry")`); the model fixes no routing
//! algorithm, so what these suites pin is the rule a road answer follows, over a class of
//! requests rather than one recorded pair:
//!
//! * a route travels ALONG roads, and the roads of this network run north-south or
//!   east-west, so no route is shorter than the rectilinear journey between its endpoints
//!   — strictly longer than the straight line for any pair that is not axis-aligned;
//! * a route's duration is the sum of its stretches at the pace of the road each one runs
//!   on — its posted speed, held down to the travel mode's own top speed, less what is
//!   given up at the junctions the grid puts every road spacing along it — so the speed a
//!   journey implies is a pace of the network's roads, it is higher on the roads that are
//!   posted higher and lose less at a crossing, and a journey long enough to be worth
//!   reaching the fast classes for implies more of a speed than a short one that cannot.
//!
//! The tables span what the rule generalises over: separation (hundreds of metres to
//! hundreds of kilometres), bearing (east-west, north-south, diagonal), hemisphere, the
//! class of road travelled, the travel mode and the routing objective.
//!
//! The numbered roads are the network's through-road grid, joined and left where they meet
//! one another rather than wherever a street crosses them, so a third rule follows: whether
//! a journey can travel them is decided by its own LENGTH against the spacing of those
//! access points, not by how near one its endpoints happen to fall. A journey shorter than
//! that spacing is paced by the local streets, and the connections OptimizeWaypoints reports
//! are read off the same traversal as every other metric of this service.
use serde_json::{json, Value};

use crate::routing::{great_circle_m, int, post, routes_service, MATRIX, OPTIMIZE, ROUTES};

/// The grid's road spacing in degrees, as `src/state.rs` holds it (`ROAD_SPACING_DEG`): a
/// submitted position is matched onto a road at most half of it away in each axis, which is
/// the only part of a reported distance that is not travel along the roads.
///
/// This module owns the copies of `src/state.rs`'s network parameters that the suites of this
/// target read, so a sibling suite reads them from here rather than keeping a second copy that
/// can drift from the source on its own.
pub(crate) const ROAD_SPACING_DEG: f64 = 0.000_5;

/// Posted speeds of the classes of road the network carries, in km/h, as `src/state.rs`
/// holds them (`MOTORWAY_SPEED_KPH`, `TRUNK_SPEED_KPH`, `STREET_SPEED_KPH`,
/// `FOOTPATH_SPEED_KPH`).
const MOTORWAY_SPEED_KPH: f64 = 110.0;
const TRUNK_SPEED_KPH: f64 = 85.0;
pub(crate) const STREET_SPEED_KPH: f64 = 50.0;
const FOOTPATH_SPEED_KPH: f64 = 5.0;

/// Metres per second in one km/h.
pub(crate) const KPH_TO_MPS: f64 = 1_000.0 / 3_600.0;

/// Decimal digits of a coordinate a response carries, as `src/state.rs` holds it
/// (`POSITION_PRECISION`): this API expresses a `Position` to a ten-millionth of a degree.
pub(crate) const POSITION_PRECISION: i32 = 7;

/// Seconds given up at one at-grade junction of a road of each class, as `src/state.rs`
/// holds them (`MOTORWAY_JUNCTION_DELAY_S`, `TRUNK_JUNCTION_DELAY_S`,
/// `STREET_JUNCTION_DELAY_S`): a motorway's crossings are grade-separated, a trunk road has
/// priority at its own, and a local street gives way at every block. A pedestrian queues at
/// none of them.
const MOTORWAY_JUNCTION_DELAY_S: f64 = 0.0;
const TRUNK_JUNCTION_DELAY_S: f64 = 1.5;
const STREET_JUNCTION_DELAY_S: f64 = 6.0;

/// The top speed of `mode` itself, in km/h
/// (`ComAmazonawsGeoroutesRoutesserviceState::travel_mode_speeds` in `src/state.rs`): a
/// pedestrian walks at the footpath pace on any road, a scooter and an intermodal journey
/// are held to 45 km/h, a truck to 90 and a transit vehicle to 80, and a car keeps up with
/// any road of the network.
fn top_speed_kph(mode: &str) -> f64 {
    match mode {
        "Pedestrian" => FOOTPATH_SPEED_KPH,
        "Scooter" | "Intermodal" => 45.0,
        "Transit" => 80.0,
        "Truck" => 90.0,
        _ => MOTORWAY_SPEED_KPH,
    }
}

/// The pace, in metres per second, that travel of `mode` makes along a road posted at
/// `posted_kph` whose junctions cost `delay_s` each, on an EAST-WEST run at `latitude` —
/// where the grid's crossings are closest together and so the pace is at its lowest.
fn pace_mps(posted_kph: f64, delay_s: f64, mode: &str, latitude: f64) -> f64 {
    let free_flow = (posted_kph * KPH_TO_MPS).min(top_speed_kph(mode) * KPH_TO_MPS);
    let block_m = block_m(latitude);
    block_m / (block_m / free_flow + delay_s)
}

/// Metres between two neighbouring crossings of an east-west road at `latitude`.
fn block_m(latitude: f64) -> f64 {
    ROAD_SPACING_DEG * 111_320.0 * latitude.to_radians().cos()
}

/// The slowest and the fastest pace, in metres per second, travel of `mode` can make on
/// this network at `latitude`.
///
/// A pedestrian may use the streets and the footpaths and queues at no junction, so it
/// walks at its own pace whatever road it is on; every other mode may use the streets and
/// the numbered roads, so its slowest pace is a street's — posted at
/// [`STREET_SPEED_KPH`] and giving way at every block — and its fastest a motorway's,
/// which loses nothing at a crossing.
fn speed_bounds_mps(mode: &str, latitude: f64) -> (f64, f64) {
    let top = top_speed_kph(mode) * KPH_TO_MPS;
    if mode == "Pedestrian" {
        return (top, top);
    }
    (
        pace_mps(STREET_SPEED_KPH, STREET_JUNCTION_DELAY_S, mode, latitude),
        (MOTORWAY_SPEED_KPH * KPH_TO_MPS).min(top),
    )
}

/// The rectilinear journey between two positions, in metres: the north-south leg and the
/// east-west leg a traveller on a grid of meridian and parallel roads has to cover. It is
/// measured at the latitude where the east-west leg is SHORTEST, so it is a lower bound on
/// any staircase between the two positions however the route splits its legs up.
fn rectilinear_m(from: [f64; 2], to: [f64; 2]) -> f64 {
    let north_south = great_circle_m([from[0], from[1]], [from[0], to[1]]);
    let east_west = great_circle_m([from[0], from[1]], [to[0], from[1]])
        .min(great_circle_m([from[0], to[1]], [to[0], to[1]]));
    north_south + east_west
}

/// One pair per row, differing in separation, bearing and hemisphere. Every position is in
/// mapped territory and every pair is connected by road, so each is routed.
const PAIRS: [(&str, [f64; 2], [f64; 2]); 7] = [
    ("a few hundred metres east", [-123.1017, 49.2823], [-123.0963, 49.2823]),
    ("a kilometre north", [-123.1017, 49.2823], [-123.1017, 49.2913]),
    ("a few kilometres diagonally", [-123.1017, 49.2823], [-123.1431, 49.2487]),
    ("tens of kilometres diagonally", [-122.3321, 47.6062], [-122.1037, 47.3129]),
    ("hundreds of kilometres north to south", [-122.3321, 47.6062], [-122.6784, 45.5152]),
    ("hundreds of kilometres west to east", [-122.3321, 47.6062], [-119.8743, 47.4221]),
    ("a diagonal pair in the southern hemisphere", [151.2093, -33.8688], [150.8931, -34.0522]),
];

/// No cell of a matrix is shorter than the rectilinear journey between its pair, and the
/// speed it implies is a speed of the roads the traveller is allowed on: the two members
/// are one traversal of the graph, not two numbers.
#[tokio::test]
async fn every_cell_is_a_path_of_the_road_graph_and_the_time_it_takes_on_it() {
    let router = routes_service();
    let modes = ["Car", "Truck", "Scooter", "Pedestrian"];
    // The matching hops at the two ends are the only part of a reported distance that is
    // not travel along a road, and a match moves a position at most half the grid spacing
    // in each axis.
    let slack_m = 4.0 * ROAD_SPACING_DEG * 111_320.0;

    for mode in modes {
        for objective in ["FastestRoute", "ShortestRoute"] {
            for (pair, origin, destination) in PAIRS {
                let case = format!("{mode}, {objective}, {pair}");
                let cell = cell(&router, origin, destination, mode, objective, &case).await;
                let distance = int(&cell, "Distance", &case);
                let duration = int(&cell, "Duration", &case);
                let rectilinear = rectilinear_m(origin, destination);
                assert!(
                    distance as f64 + slack_m >= rectilinear,
                    "case {case:?}: the roads run north-south and east-west, so the route covers \
                     the {rectilinear:.0} m rectilinear journey at least, got {distance}"
                );
                assert!(
                    duration > 0,
                    "case {case:?}: travelling {distance} m takes time, got {duration}"
                );
                let implied = distance as f64 / duration as f64;
                let (slowest, fastest) =
                    speed_bounds_mps(mode, origin[1].abs().max(destination[1].abs()));
                assert!(
                    implied >= slowest - 0.5 && implied <= fastest + 0.5,
                    "case {case:?}: {distance} m in {duration} s implies {implied:.2} m/s, outside \
                     the {slowest:.2}..{fastest:.2} m/s the roads this mode may travel carry it at"
                );
            }
        }
    }
}

/// The hierarchy reaches the answer: for a mode that can travel a road faster than a local
/// street carries it, a journey long enough to be worth joining the fast classes for
/// implies more of a speed than a short one that spends its length on the local roads. For
/// a mode held slower than every road of the network, the mode's own top speed is what
/// every journey implies, however long it is.
#[tokio::test]
async fn a_journey_worth_taking_the_fast_roads_implies_more_of_a_speed_than_a_local_one() {
    let router = routes_service();

    for mode in ["Car", "Truck", "Scooter", "Pedestrian"] {
        let (slowest, top) = speed_bounds_mps(mode, 49.3);
        let mut speeds: Vec<(&str, f64)> = Vec::new();
        for (pair, origin, destination) in [PAIRS[0], PAIRS[3], PAIRS[4]] {
            let case = format!("{mode}, {pair}");
            let cell = cell(&router, origin, destination, mode, "FastestRoute", &case).await;
            let (distance, duration) = (int(&cell, "Distance", &case), int(&cell, "Duration", &case));
            speeds.push((pair, distance as f64 / duration as f64));
        }
        if mode == "Pedestrian" {
            // Every road of the network is posted above a walker's own top speed and a
            // walker queues at no junction, so the mode is what paces any journey it
            // makes, whatever roads it travels.
            for (pair, speed) in &speeds {
                let case = format!("{mode}, {pair}");
                assert!(
                    (speed - top).abs() <= 0.2,
                    "case {case:?}: every road carries this mode at its own {top:.2} m/s, so that \
                     is the pace of any journey it makes, got {speed:.2} m/s"
                );
            }
            continue;
        }
        let (local_pair, local) = speeds[0];
        for (pair, speed) in &speeds[1..] {
            assert!(
                *speed > local,
                "case {mode:?}: {pair:?} travels the fast classes, so it must imply more of a \
                 speed than {local_pair:?}, got {speed:.2} m/s against {local:.2} m/s"
            );
        }
        // And a journey of hundreds of kilometres is not priced at a local-street pace,
        // which is the lowest pace any road this mode may use carries it at.
        let (long_pair, long) = speeds[speeds.len() - 1];
        assert!(
            long > slowest,
            "case {mode:?}: {long_pair:?} runs on the fast classes, so it must imply more than \
             the {slowest:.2} m/s a street carries travel at, got {long:.2} m/s"
        );
    }
}

/// The road travelled is what turns a distance into a duration: three runs of the same
/// length along a motorway, a trunk road and a street take times in the order of those
/// three roads' paces — each road's posted speed less what its own junctions cost — and the
/// same run on foot takes the footpath pace.
///
/// Each row is a run straight along one parallel road of the grid, taken as the shortest
/// route so that the path is that road: latitude 49.4 is a multiple of 400 grid steps,
/// where the network puts a motorway, 49.45 is a multiple of 100, where it puts a trunk
/// road, and 49.4505 is neither, so it carries a street (`class_of` in `src/state.rs`).
#[tokio::test]
async fn the_duration_of_a_run_is_its_length_at_the_pace_of_the_road_it_runs_on() {
    let router = routes_service();
    // name, the latitude the run follows, the speed the road is posted at, what one of its
    // junctions costs.
    let runs: [(&str, f64, f64, f64); 3] = [
        ("along a motorway", 49.4, MOTORWAY_SPEED_KPH, MOTORWAY_JUNCTION_DELAY_S),
        ("along a trunk road", 49.45, TRUNK_SPEED_KPH, TRUNK_JUNCTION_DELAY_S),
        ("along a street", 49.4505, STREET_SPEED_KPH, STREET_JUNCTION_DELAY_S),
    ];
    // A longitude between two meridian roads, so the run's endpoints match onto the
    // parallel road they sit on rather than onto a meridian crossing it.
    let (west, east) = (-123.097_23, -123.067_23);

    let mut measured: Vec<(&str, i64, i64, f64)> = Vec::new();
    for (case, latitude, posted_kph, delay_s) in runs {
        let cell = cell(
            &router,
            [west, latitude],
            [east, latitude],
            "Car",
            "ShortestRoute",
            case,
        )
        .await;
        let (distance, duration) = (int(&cell, "Distance", case), int(&cell, "Duration", case));
        let implied = distance as f64 / duration as f64;
        let pace = pace_mps(posted_kph, delay_s, "Car", latitude);
        assert!(
            (implied - pace).abs() <= 1.0,
            "case {case:?}: a run along this road is travelled at the {pace:.2} m/s it carries \
             travel at — {posted_kph} km/h less {delay_s} s at each of its junctions — got \
             {distance} m in {duration} s, i.e. {implied:.2} m/s"
        );
        measured.push((case, distance, duration, implied));
    }

    // The three runs cover the same ground to within the scale of their own latitudes, so
    // what differs between them is the road.
    let shortest = measured.iter().map(|run| run.1).min().unwrap_or_default();
    let longest = measured.iter().map(|run| run.1).max().unwrap_or_default();
    assert!(
        (longest - shortest) <= 10,
        "the three runs must cover the same distance for the comparison to be about the road, \
         got {measured:?}"
    );
    for pair in measured.windows(2) {
        let ((quicker, _, quicker_duration, _), (slower, _, slower_duration, _)) = (pair[0], pair[1]);
        assert!(
            quicker_duration < slower_duration,
            "case {slower:?}: a road posted lower takes longer over the same ground than \
             {quicker:?}, got {slower_duration} s against {quicker_duration} s"
        );
    }

    // The travel mode holds the speed down where the mode is the slower of the two: on foot
    // the same run takes the footpath pace whatever the road is posted at.
    for (case, latitude, _, _) in runs {
        let case = &format!("{case} on foot");
        let cell = cell(
            &router,
            [west, latitude],
            [east, latitude],
            "Pedestrian",
            "ShortestRoute",
            case,
        )
        .await;
        let (distance, duration) = (int(&cell, "Distance", case), int(&cell, "Duration", case));
        let implied = distance as f64 / duration as f64;
        assert!(
            (implied - FOOTPATH_SPEED_KPH * KPH_TO_MPS).abs() <= 0.2,
            "case {case:?}: a walker covers the ground at walking pace whatever the road is \
             posted at, got {distance} m in {duration} s, i.e. {implied:.2} m/s"
        );
    }
}

/// One traversal serves every operation that reports a route metric: the matrix cell for a
/// pair and the direct route for that pair are the same path, so they report the same
/// distance and the same duration.
#[tokio::test]
async fn the_matrix_cell_and_the_direct_route_report_the_same_traversal() {
    let router = routes_service();

    for mode in ["Car", "Pedestrian"] {
        for (pair, origin, destination) in PAIRS {
            let case = format!("{mode}, {pair}");
            let cell = cell(&router, origin, destination, mode, "FastestRoute", &case).await;
            let (status, answer, _) = post(
                &router,
                ROUTES,
                &json!({
                    "Origin": origin,
                    "Destination": destination,
                    "TravelMode": mode,
                    "OptimizeRoutingFor": "FastestRoute",
                }),
            )
            .await;
            assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
            let summary = &answer["Routes"][0]["Summary"];
            assert_eq!(
                (int(summary, "Distance", &case), int(summary, "Duration", &case)),
                (int(&cell, "Distance", &case), int(&cell, "Duration", &case)),
                "case {case:?}: the cell and the direct route are one traversal, got {summary:?} \
                 against {cell:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Reaching the numbered through-roads
// ---------------------------------------------------------------------------

/// How many grid roads apart the network's numbered through-roads sit, as `src/state.rs`
/// holds it (`TRUNK_EVERY`). It is also the spacing of the ACCESS POINTS travel joins and
/// leaves the numbered network at (`access_spacing_m` in `src/state/road_graph.rs`): a
/// numbered road is joined where it meets another of them, not wherever a street crosses it.
const TRUNK_EVERY: i64 = 100;

/// Metres per degree of latitude, as `src/state.rs` holds it (`METRES_PER_DEGREE_LAT`).
const METRES_PER_DEGREE_LAT: f64 = 110_574.0;

/// The shortest journey near `latitude` that can join the numbered network at all: the
/// closer together of the two orientations' access points, because a journey shorter than
/// that has no way onto a through-road at either of its ends whichever way it runs.
fn shortest_journey_that_joins_a_through_road_m(latitude: f64) -> f64 {
    let along_a_parallel = TRUNK_EVERY as f64 * block_m(latitude);
    let along_a_meridian = TRUNK_EVERY as f64 * ROAD_SPACING_DEG * METRES_PER_DEGREE_LAT;
    along_a_parallel.min(along_a_meridian)
}

/// The quickest pace, in metres per second, that travel of `mode` makes along a LOCAL
/// STREET: a north-south run, where the grid's crossings are furthest apart and so the least
/// is given up at them. Nothing confined to the streets covers ground faster than this.
fn fastest_street_pace_mps(mode: &str) -> f64 {
    let free_flow = (STREET_SPEED_KPH * KPH_TO_MPS).min(top_speed_kph(mode) * KPH_TO_MPS);
    let block = ROAD_SPACING_DEG * METRES_PER_DEGREE_LAT;
    block / (block / free_flow + STREET_JUNCTION_DELAY_S)
}

/// Whether `position` sits on LOCAL STREETS in both axes rather than on one of the numbered
/// through-roads. A journey from an end that already sits on a numbered road travels it from
/// where it sits and needs no access point, so such a pair would not exercise the rule.
fn is_on_local_streets(position: [f64; 2]) -> bool {
    position
        .iter()
        .all(|coordinate| (coordinate / ROAD_SPACING_DEG).round() as i64 % TRUNK_EVERY != 0)
}

/// Journeys between positions on local streets, spanning fractions and multiples of the
/// spacing of the numbered network's access points, the bearing the journey runs on and both
/// hemispheres. Each is a pair the network routes; the expectation per row is derived from
/// the row's own separation rather than written down, so the table says only what the
/// journeys ARE.
const JOURNEYS: [(&str, [f64; 2], [f64; 2]); 10] = [
    ("a third of the access spacing, diagonally", [-122.3327, 47.60615], [-122.322096, 47.613349]),
    ("two thirds of it, diagonally", [-122.3327, 47.60615], [-122.311487, 47.620552]),
    ("just under it, diagonally", [-122.3327, 47.60615], [-122.299116, 47.628949]),
    ("half of it, due east", [-122.3327, 47.60615], [-122.307708, 47.60615]),
    ("half of it, due north", [-122.3327, 47.60615], [-122.3327, 47.623116]),
    ("half again, diagonally", [-122.3327, 47.60615], [-122.279667, 47.642153]),
    ("four times it, diagonally", [-122.3327, 47.60615], [-122.191277, 47.702158]),
    ("twelve times it, diagonally", [-122.3327, 47.60615], [-121.90843, 47.894173]),
    ("under half of it, southern hemisphere", [151.2071, -33.8679], [151.22124, -33.85608]),
    ("six times it, southern hemisphere", [151.2071, -33.8679], [151.419242, -33.690565]),
];

/// A journey too short to reach the numbered network is paced by the LOCAL STREETS it is
/// confined to; one long enough to join it covers ground faster than any street carries
/// travel.
///
/// The numbered roads are joined where they meet one another, [`TRUNK_EVERY`] grid roads
/// apart, so what decides whether a journey can travel them is its own LENGTH — not how near
/// one of them its endpoints happen to fall. Each row is asked for through OptimizeWaypoints,
/// whose connection metrics are read off the same traversal, and the row's separation decides
/// which side of the access spacing it is on.
#[tokio::test]
async fn a_journey_travels_the_through_roads_only_once_it_is_long_enough_to_join_them() {
    let router = routes_service();
    let street_pace = fastest_street_pace_mps("Car");

    for (case, origin, destination) in JOURNEYS {
        assert!(
            is_on_local_streets(origin) && is_on_local_streets(destination),
            "case {case:?}: both ends must sit on local streets for the journey to have to \
             reach a numbered road, got {origin:?} and {destination:?}"
        );
        let connection = sole_connection(&router, origin, destination, "Car", "FastestRoute", case).await;
        let distance = int(&connection, "Distance", case);
        let duration = int(&connection, "TravelDuration", case);
        assert!(
            duration > 0 && distance > 0,
            "case {case:?}: a journey covers ground and takes time, got {connection:?}"
        );
        let rectilinear = rectilinear_m(origin, destination);
        // A match moves each end onto a road at most half the grid spacing away in each
        // axis: that is the only part of a reported distance that is not travel along a road.
        let slack_m = 4.0 * ROAD_SPACING_DEG * 111_320.0;
        assert!(
            distance as f64 + slack_m >= rectilinear,
            "case {case:?}: the roads run north-south and east-west, so the journey covers the \
             {rectilinear:.0} m rectilinear journey at least, got {distance}"
        );

        let implied = distance as f64 / duration as f64;
        let separation = great_circle_m(origin, destination);
        let access = shortest_journey_that_joins_a_through_road_m((origin[1] + destination[1]) / 2.0);
        if separation < access {
            assert!(
                implied <= street_pace + 0.1,
                "case {case:?}: a {separation:.0} m journey cannot reach the numbered network, \
                 whose access points are {access:.0} m apart, so it is paced by the streets at \
                 {street_pace:.2} m/s at most, got {distance} m in {duration} s, i.e. \
                 {implied:.2} m/s"
            );
        } else {
            assert!(
                implied > street_pace,
                "case {case:?}: a {separation:.0} m journey reaches the numbered network, whose \
                 access points are {access:.0} m apart, so it covers ground faster than the \
                 {street_pace:.2} m/s a street carries travel at, got {distance} m in \
                 {duration} s, i.e. {implied:.2} m/s"
            );
        }
    }
}

/// The single connection OptimizeWaypoints answers a one-stop journey with.
async fn sole_connection(
    router: &smithy_server_runtime::MultiServiceRouter,
    origin: [f64; 2],
    destination: [f64; 2],
    travel_mode: &str,
    objective: &str,
    case: &str,
) -> Value {
    let (status, answer, _) = post(
        router,
        OPTIMIZE,
        &json!({
            "Origin": origin,
            "OriginOptions": { "Id": "depot" },
            "Waypoints": [{ "Id": "stop", "Position": destination }],
            "TravelMode": travel_mode,
            "OptimizeSequencingFor": objective,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    let connections = answer["Connections"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Connections is required, got {answer:?}"));
    assert_eq!(
        connections.len(),
        1,
        "case {case:?}: an origin and one waypoint make one connection, got {connections:?}"
    );
    connections[0].clone()
}

/// The single cell of a 1x1 matrix for `origin` -> `destination`, which must be routed.
async fn cell(
    router: &smithy_server_runtime::MultiServiceRouter,
    origin: [f64; 2],
    destination: [f64; 2],
    travel_mode: &str,
    objective: &str,
    case: &str,
) -> Value {
    let (status, answer, _) = post(
        router,
        MATRIX,
        &json!({
            "Origins": [{ "Position": origin }],
            "Destinations": [{ "Position": destination }],
            "TravelMode": travel_mode,
            "OptimizeRoutingFor": objective,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    let cell = answer["RouteMatrix"][0][0].clone();
    assert!(
        cell["Error"].is_null(),
        "case {case:?}: the pair is on the road network, so it is routed, got {cell:?}"
    );
    cell
}
