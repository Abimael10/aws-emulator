// SPDX-License-Identifier: CC0-1.0

//! How the successful answers of this service are SYNTHESIZED, over the classes of request
//! each rule covers.
//!
//! The rules under test here are:
//!
//! * `PricingBucket` is the tier the OPERATION's queries are charged in: one value per
//!   operation, the same for every request that operation serves;
//! * a position the service RESOLVED against the road network is reported at the
//!   coordinate precision this API expresses a position in, while a position the CALLER
//!   supplied is reported as submitted;
//! * `SnapToRoads` scores a match on the match itself — how far the point had to move to
//!   reach its road — and never on the `SnapRadius` the
//!   caller allowed;
//! * `SnappedGeometry` runs ALONG the roads the trace matched onto, so every vertex is on
//!   a road and every segment follows one;
//! * `OptimizeWaypoints` answers the order whose WHOLE journey is cheapest, by the same
//!   cost table `CalculateRouteMatrix` reports for those stops;
//! * a road of the network is identified by a designation a client can render, never by an
//!   internal signed index.
//!
//! Each table spans what its rule generalises over (operation, request shape, coordinate
//! precision, hemisphere, travel mode, objective, stop count, radius) rather than
//! restating one recorded request.

use serde_json::{json, Value};

use crate::road_graph_metrics::{
    between_roads, on_a_road, on_a_road_line, POSITION_PRECISION, ROAD_SPACING_DEG,
};
use crate::routing::{
    great_circle_m, int, position, post, routes_service, MATRIX, OPTIMIZE, ROUTES, SNAP,
};

/// The path `CalculateIsolines` is bound to.
const ISOLINES: &str = "/v2/isolines";

/// Whether `value` carries no more than the coordinate precision this API expresses a
/// position in (`POSITION_PRECISION`, a ten-millionth of a degree), which is what a
/// position the service resolved against the network must be reported at: rounding it to
/// that precision leaves it unchanged.
///
/// It is the API's own precision and not the coarser one the polyline FORMAT encodes a
/// vertex at: a resolved position is reported in a `Position` member, a list of `Double`,
/// and AWS answers one with as many as seven decimals.
fn at_the_apis_position_precision(value: f64) -> bool {
    let scale = 10_f64.powi(POSITION_PRECISION);
    (value * scale).round() / scale == value
}

/// The farthest a match may move a position inside the network's coverage, in metres: half
/// the grid spacing, in the one axis the match moves it along, at the most metres a degree
/// of this grid is worth anywhere on Earth (~111.3 km per degree at the equator).
const MAX_MATCH_OFFSET_M: f64 = ROAD_SPACING_DEG / 2.0 * 111_320.0;

// ---------------------------------------------------------------------------
// The pricing tier of an operation
// ---------------------------------------------------------------------------

/// `PricingBucket` ("the pricing bucket for which the query is charged at", `@required`
/// and bound to `x-amz-geo-pricing-bucket`) names the tier the OPERATION is charged in:
/// the route and matrix queries in the base tier, the isoline, waypoint-optimization and
/// road-snapping queries in the advanced one. The model enumerates no values, so the two
/// tokens come from recorded behavior; what this table pins is that each operation
/// reports ITS OWN tier and reports it for every request it serves.
#[tokio::test]
async fn each_operation_is_charged_in_its_own_pricing_tier() {
    let router = routes_service();
    let core = service_com_amazonaws_georoutes_routesservice::state::PRICING_BUCKET_CORE;
    let advanced = service_com_amazonaws_georoutes_routesservice::state::PRICING_BUCKET_ADVANCED;
    let near = [-123.1, 49.28];
    let far = [-123.12, 49.26];
    let trace = json!([
        { "Position": [-123.100_25, 49.280_25] },
        { "Position": [-123.100_75, 49.280_75] }
    ]);

    // Rows: every implemented operation, twice, with requests differing in what the
    // response varies over (travel mode, waypoints, matrix size, radius, geometry format).
    let cases: [(&str, &str, &str, Value); 10] = [
        ("a direct route", ROUTES, core, json!({ "Origin": near, "Destination": far })),
        (
            "a walking route through a waypoint",
            ROUTES,
            core,
            json!({
                "Origin": near,
                "Destination": far,
                "Waypoints": [{ "Position": [-123.11, 49.27] }],
                "TravelMode": "Pedestrian",
            }),
        ),
        (
            "a 1x1 matrix",
            MATRIX,
            core,
            json!({ "Origins": [{ "Position": near }], "Destinations": [{ "Position": far }] }),
        ),
        (
            "a 2x1 truck matrix",
            MATRIX,
            core,
            json!({
                "Origins": [{ "Position": near }, { "Position": [-123.2, 49.3] }],
                "Destinations": [{ "Position": far }],
                "TravelMode": "Truck",
            }),
        ),
        (
            "a snap at the default radius",
            SNAP,
            advanced,
            json!({ "TracePoints": trace, "SnapRadius": 0 }),
        ),
        (
            "a snap at 10 km, as a line string",
            SNAP,
            advanced,
            json!({ "TracePoints": trace, "SnapRadius": 10_000, "SnappedGeometryFormat": "Simple" }),
        ),
        (
            "an optimization with no waypoints",
            OPTIMIZE,
            advanced,
            json!({ "Origin": near, "Destination": far }),
        ),
        (
            "an optimization over two waypoints, shortest first",
            OPTIMIZE,
            advanced,
            json!({
                "Origin": near,
                "Waypoints": [
                    { "Id": "alpha", "Position": [-123.11, 49.27] },
                    { "Id": "bravo", "Position": [-123.05, 49.25] },
                ],
                "OptimizeSequencingFor": "ShortestRoute",
            }),
        ),
        (
            "an isoline bounded by a distance",
            ISOLINES,
            advanced,
            json!({
                "Origin": near,
                "Thresholds": { "Distance": [2_000] },
                "IsolineGranularity": { "MaxPoints": 31 },
            }),
        ),
        (
            "isolines bounded by two walking times, towards a destination",
            ISOLINES,
            advanced,
            json!({
                "Destination": far,
                "Thresholds": { "Time": [300, 600] },
                "TravelMode": "Pedestrian",
                "IsolineGranularity": { "MaxPoints": 31 },
            }),
        ),
    ];

    assert_ne!(
        core, advanced,
        "the two tiers of this service must be distinct tokens, got {core:?} twice"
    );
    for (case, path, expected, body) in cases {
        let (status, answer, bucket) = post(&router, path, &body).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
        assert_eq!(
            bucket.as_deref(),
            Some(expected),
            "case {case:?}: expected the {expected:?} pricing tier, got {bucket:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Positions: the caller's, and the network's
// ---------------------------------------------------------------------------

/// A position the service RESOLVED against the road network is reported at the coordinate
/// precision this API expresses a position in and lies on a road of the network; a position
/// the CALLER supplied is echoed exactly as it was submitted, however many digits it
/// carried.
///
/// A matched position is the submitted one moved onto the network, so it "may differ from
/// the requested" position — and does differ whenever the submitted position lay on no road
/// of the network, by no more than the farthest a position inside the network's coverage can
/// be from the nearest road.
///
/// Rows span the precision of the submitted coordinates (4 to 7 decimals, and coordinates
/// already on a road line), both hemispheres and both signs of longitude, and the three
/// operations that report a matched position for a position the request supplied.
#[tokio::test]
async fn a_position_of_the_network_is_reported_at_the_apis_position_precision() {
    let router = routes_service();
    // name, the two positions submitted.
    let cases: [(&str, [f64; 2], [f64; 2]); 5] = [
        ("four decimals, off the roads", [-123.1003, 49.2803], [-123.1203, 49.2603]),
        (
            "seven decimals, off the roads",
            [-123.100_180_7, 49.280_180_7],
            [-123.120_220_3, 49.260_220_3],
        ),
        (
            "on a road line in both coordinates",
            [on_a_road(-123.1), on_a_road(49.28)],
            [on_a_road(-123.12), on_a_road(49.26)],
        ),
        (
            "the southern hemisphere, six decimals",
            [151.210_183, -33.870_183],
            [151.200_227, -33.860_227],
        ),
        (
            "east of the prime meridian, seven decimals",
            [2.294_481_3, 48.858_370_1],
            [2.300_000_7, 48.860_000_3],
        ),
    ];

    for (case, first, second) in cases {
        let (status, snapped, _) = post(
            &router,
            SNAP,
            &json!({
                "TracePoints": [{ "Position": first }, { "Position": second }],
                "SnapRadius": 10_000,
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {snapped:?}");
        let points = snapped["SnappedTracePoints"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: SnappedTracePoints is required, got {snapped:?}"))
            .clone();
        for (index, submitted) in [first, second].into_iter().enumerate() {
            let case = &format!("{case} / trace point {index}");
            let point = &points[index];
            assert_eq!(
                position(&point["OriginalPosition"], case),
                submitted,
                "case {case:?}: OriginalPosition is the submitted position, verbatim, got {point:?}"
            );
            let matched = position(&point["SnappedPosition"], case);
            assert!(
                matched.iter().copied().all(at_the_apis_position_precision),
                "case {case:?}: a matched position carries this API's position precision, got \
                 {matched:?}"
            );
            assert!(
                matched.iter().copied().any(on_a_road_line),
                "case {case:?}: a matched position lies on a road of the network, got {matched:?}"
            );
        }

        let (status, route, _) = post(
            &router,
            ROUTES,
            &json!({ "Origin": first, "Destination": second }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {route:?}");
        let details = &route["Routes"][0]["Legs"][0]["VehicleLegDetails"];
        for (which, submitted) in [("Departure", first), ("Arrival", second)] {
            let case = &format!("{case} / {which}");
            let place = &details[which]["Place"];
            assert_eq!(
                position(&place["OriginalPosition"], case),
                submitted,
                "case {case:?}: OriginalPosition is the submitted position, verbatim, got {place:?}"
            );
            let reported = position(&place["Position"], case);
            assert!(
                reported.iter().copied().all(at_the_apis_position_precision),
                "case {case:?}: Position is the point the network resolved, so it carries this \
                 API's position precision, got {reported:?}"
            );
            assert!(
                reported.iter().copied().any(on_a_road_line),
                "case {case:?}: Position lies on a road of the network, got {reported:?}"
            );
        }

        // An isoline is grown from the anchor matched onto the network, and reports that
        // matched point for whichever of `Origin` and `Destination` the request set: "the
        // actual point on the road network used for calculations, which may differ from the
        // requested origin if Origin was not directly on a road".
        for (member, reported_member, submitted) in [
            ("Origin", "SnappedOrigin", first),
            ("Destination", "SnappedDestination", second),
        ] {
            let case = &format!("{case} / an isoline from {member}");
            let (status, answer, _) = post(
                &router,
                ISOLINES,
                &json!({
                    member: submitted,
                    "Thresholds": { "Distance": [1_000] },
                    "IsolineGranularity": { "MaxPoints": 31 },
                }),
            )
            .await;
            assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
            let matched = position(&answer[reported_member], case);
            assert!(
                matched.iter().copied().all(at_the_apis_position_precision),
                "case {case:?}: {reported_member} is the point the network resolved, so it \
                 carries this API's position precision, got {matched:?}"
            );
            assert!(
                matched.iter().copied().any(on_a_road_line),
                "case {case:?}: {reported_member} lies on a road of the network, got {matched:?}"
            );
            let offset_m = great_circle_m(submitted, matched);
            assert!(
                offset_m <= MAX_MATCH_OFFSET_M,
                "case {case:?}: the anchor is matched onto a road near it, so the match moved it \
                 at most {MAX_MATCH_OFFSET_M} m, got {offset_m} m to {matched:?}"
            );
            if submitted.iter().copied().any(on_a_road_line) {
                assert_eq!(
                    matched, submitted,
                    "case {case:?}: the submitted position is already on a road, so the point \
                     used for calculations is that position, got {matched:?}"
                );
            } else {
                assert_ne!(
                    matched, submitted,
                    "case {case:?}: the submitted position lies on no road, so the point used for \
                     calculations differs from it, got {matched:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A matched position is a coordinate of the road network
// ---------------------------------------------------------------------------

/// Positions written the way a caller writes one: round decimals, including coordinates that
/// are whole multiples of the network's road spacing, in four places of two hemispheres and
/// both signs of longitude. None of them is a coordinate of the network's own geometry, which
/// every row below checks for itself before it reads the answer.
const ROUND_DECIMALS: [(&str, [f64; 2]); 6] = [
    ("four decimals", [-122.3421, 47.6097]),
    (
        "four decimals, both a whole multiple of the road spacing",
        [-122.342, 47.6095],
    ),
    ("three decimals", [-123.101, 49.282]),
    ("five decimals", [-123.101_25, 49.282_25]),
    ("five decimals, southern hemisphere", [151.209_25, -33.868_75]),
    ("four decimals, east of the prime meridian", [2.2945, 48.8584]),
];

/// `coordinate` rounded to `digits` decimal places.
fn round_to(coordinate: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (coordinate * scale).round() / scale
}

/// The position each operation of this service reports as the one it used for its
/// calculation, for a request built around `submitted`: the isoline's `SnappedOrigin` and
/// `SnappedDestination`, a leg's departure `Place.Position`, and a trace point's
/// `SnappedPosition`.
async fn matched_positions(
    router: &smithy_server_runtime::MultiServiceRouter,
    submitted: [f64; 2],
    case: &str,
) -> Vec<(&'static str, [f64; 2])> {
    // A second position for the requests that need two, far enough off not to be the one
    // under test.
    let other = [submitted[0] + 0.004, submitted[1] + 0.004];
    let mut reported = Vec::new();

    for (member, reported_member) in [("Origin", "SnappedOrigin"), ("Destination", "SnappedDestination")] {
        let (status, answer, _) = post(
            router,
            ISOLINES,
            &json!({
                member: submitted,
                "Thresholds": { "Distance": [500] },
                "IsolineGranularity": { "MaxPoints": 31 },
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
        reported.push((reported_member, position(&answer[reported_member], case)));
    }

    let (status, answer, _) = post(
        router,
        ROUTES,
        &json!({ "Origin": submitted, "Destination": other }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    reported.push((
        "CalculateRoutes Departure Position",
        position(
            &answer["Routes"][0]["Legs"][0]["VehicleLegDetails"]["Departure"]["Place"]["Position"],
            case,
        ),
    ));

    let (status, answer, _) = post(
        router,
        SNAP,
        &json!({
            "TracePoints": [{ "Position": submitted }, { "Position": other }],
            "SnapRadius": 300,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    reported.push((
        "SnapToRoads SnappedPosition",
        position(&answer["SnappedTracePoints"][0]["SnappedPosition"], case),
    ));

    reported
}

/// The position an answer reports as the one used for its calculation — `SnappedOrigin` /
/// `SnappedDestination` ("the actual point on the road network used for calculations, which
/// may differ from the requested origin if Origin was not directly on a road"), a leg's
/// `Place.Position`, a trace point's `SnappedPosition` — is a coordinate of the ROAD
/// NETWORK's own geometry, so a position the caller wrote in round decimals comes back MOVED
/// onto the carriageway nearest it and never as that position re-emitted at some precision.
///
/// The carriageways of this network sit off the decimal grid a coordinate is written on
/// (`CARRIAGEWAY_OFFSET_DEG` in `src/state.rs`), so no coordinate of four or five decimals is
/// a coordinate of a road: every row is matched onto a road beside it, by the distance to
/// that road and by no more than the farthest a position inside the network's coverage can be
/// from one.
///
/// Rows span how many decimals the caller wrote (three to five, and coordinates that are
/// whole multiples of the road spacing), the place and the hemisphere, and every operation
/// that reports a matched position.
#[tokio::test]
async fn a_matched_position_is_a_coordinate_of_the_network_and_not_the_submitted_one() {
    let router = routes_service();

    for (case, submitted) in ROUND_DECIMALS {
        assert!(
            !submitted.iter().copied().any(on_a_road_line),
            "case {case:?}: no coordinate written in round decimals is a coordinate of a road \
             of this network, so the row must lie on none, got {submitted:?}"
        );
        for (member, matched) in matched_positions(&router, submitted, case).await {
            let case = &format!("{case} / {member}");
            assert!(
                matched.iter().copied().any(on_a_road_line),
                "case {case:?}: the position used for the calculation is a point of the road \
                 network, so one of its coordinates is the one a road of the network runs at, \
                 got {matched:?}"
            );
            let moved = great_circle_m(submitted, matched);
            assert!(
                moved > 1.0 && moved <= MAX_MATCH_OFFSET_M,
                "case {case:?}: {submitted:?} lies on no carriageway, so it is matched onto the \
                 one nearest it — more than a metre and at most {MAX_MATCH_OFFSET_M:.1} m away — \
                 got {moved:.3} m to {matched:?}"
            );
            assert!(
                matched.iter().copied().all(at_the_apis_position_precision),
                "case {case:?}: the point is the one the network resolved, so it carries this \
                 API's position precision, got {matched:?}"
            );
            for digits in 3..=7 {
                let quantized = [round_to(submitted[0], digits), round_to(submitted[1], digits)];
                assert_ne!(
                    matched, quantized,
                    "case {case:?}: the point is resolved against the road network, so it is not \
                     the submitted position at {digits} decimals, got {matched:?}"
                );
            }
        }
    }
}

/// A position read off the network is a position OF the network: submitted back, it is
/// already on a road, so the match reports it unchanged and moves it nothing at all.
///
/// That round trip is what a client does with `SnappedOrigin` — feeding it into a follow-up
/// call as a road-anchored point — and what makes the "was my position on a road?" check
/// worth making: it answers no for a coordinate the caller wrote and yes for the point the
/// service answered with.
///
/// Rows are the same span of written coordinates, through both isoline anchors.
#[tokio::test]
async fn a_position_read_off_the_network_is_matched_to_itself() {
    let router = routes_service();

    for (case, submitted) in ROUND_DECIMALS {
        for (member, reported_member) in [("Origin", "SnappedOrigin"), ("Destination", "SnappedDestination")] {
            let case = &format!("{case} / {reported_member}");
            let matched = isoline_anchor(&router, member, reported_member, submitted, case).await;
            let again = isoline_anchor(&router, member, reported_member, matched, case).await;
            assert_eq!(
                again, matched,
                "case {case:?}: {matched:?} is a point of the road network, so submitting it is \
                 submitting a position already on a road: the match reports that same point and \
                 moves it nothing at all, got {again:?}"
            );
            assert!(
                matched.iter().copied().any(on_a_road_line),
                "case {case:?}: the point reported is a coordinate of the network, got {matched:?}"
            );
        }
    }
}

/// The point `CalculateIsolines` reports for an anchor submitted under `member`.
async fn isoline_anchor(
    router: &smithy_server_runtime::MultiServiceRouter,
    member: &str,
    reported_member: &str,
    anchor: [f64; 2],
    case: &str,
) -> [f64; 2] {
    let (status, answer, _) = post(
        router,
        ISOLINES,
        &json!({
            member: anchor,
            "Thresholds": { "Distance": [500] },
            "IsolineGranularity": { "MaxPoints": 31 },
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    position(&answer[reported_member], case)
}

// ---------------------------------------------------------------------------
// The confidence of a match
// ---------------------------------------------------------------------------

/// One `Confidence` of a snapped trace point, as the service reports it.
async fn confidence_of(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    point: [f64; 2],
    radius: i64,
) -> f64 {
    let (status, answer, _) = post(
        router,
        SNAP,
        &json!({
            // A trace is two points at least; the second is far enough away not to be the
            // one under test.
            "TracePoints": [{ "Position": point }, { "Position": [point[0] + 0.004, point[1] + 0.004] }],
            "SnapRadius": radius,
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
    answer["SnappedTracePoints"][0]["Confidence"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: Confidence is required, got {answer:?}"))
}

/// `Confidence` ("Confidence value for the correctness of this point match", `@range 0..1`)
/// is scored on the MATCH, and on one thing about it: how far the point had to move to
/// reach its road. A point already on a road was observed where the road is, so its match
/// is certain and scores the range's maximum; a point as far from every road as a position
/// on this network can be barely has that road as an explanation at all, so it scores near
/// the range's minimum; and of two points the one further off always scores less. The
/// radius the request allowed says which points may be matched at all, so it cannot move
/// the score of a match that was made.
///
/// Rows span the offset from the matched road, at every radius the service accepts.
#[tokio::test]
async fn a_match_is_scored_on_the_match_and_not_on_the_requested_radius() {
    let router = routes_service();
    // The meridian road nearest longitude -123.095 is at an odd multiple of ten grid steps,
    // where the network puts a footpath no car may travel, so the nearest road a car can be
    // matched to in that axis is a grid step and a half away and these rows differ only in
    // the LATITUDE offset from the parallel road nearest 49.28. The offsets run from none to
    // a quarter of the grid spacing, which is as far as a point on this network can be from
    // the road nearest it.
    let on_a_carriageway = [on_a_road(-123.095), on_a_road(49.28)];
    let cases: [(&str, f64); 4] = [
        ("on the road", 0.0),
        ("a tenth of the spacing off the road", 0.000_05),
        ("a fifth of the spacing off the road", 0.000_1),
        ("as far off the road as a point can be", 0.000_2),
    ];

    let mut by_offset: Vec<(&str, f64)> = Vec::new();
    for (case, offset_deg) in cases {
        let point = [on_a_carriageway[0], on_a_carriageway[1] + offset_deg];
        let mut radii: Vec<(i64, f64)> = Vec::new();
        for radius in [50_i64, 300, 1_000, 10_000] {
            let confidence = confidence_of(&router, case, point, radius).await;
            assert!(
                (0.0..=1.0).contains(&confidence),
                "case {case:?} / radius {radius}: Confidence must be in 0..1, got {confidence}"
            );
            assert!(
                confidence > 0.0,
                "case {case:?} / radius {radius}: a matched point carries some confidence, got {confidence}"
            );
            radii.push((radius, confidence));
        }
        for (radius, confidence) in &radii[1..] {
            assert_eq!(
                *confidence, radii[0].1,
                "case {case:?}: the same match must score the same at {radius} m as at {} m, \
                 got {confidence} against {}",
                radii[0].0, radii[0].1
            );
        }
        by_offset.push((case, radii[0].1));
    }

    // The further the point had to move to reach its road, the less certain the match.
    for pair in by_offset.windows(2) {
        let ((nearer, higher), (further, lower)) = (pair[0], pair[1]);
        assert!(
            higher > lower,
            "case {further:?}: a point further from its road than {nearer:?} is matched less \
             certainly, got {lower} against {higher}"
        );
    }

    // A point ON a road was observed where the road is, so nothing about the match is in
    // doubt and it scores the top of the modeled range; the point as far off as one can be
    // scores near the bottom of it, which is what makes the value worth thresholding on.
    let (on_the_road, certain) = by_offset[0];
    assert!(
        certain >= 0.999,
        "case {on_the_road:?}: a point on a road was observed where the road is, so its match is \
         certain, got {certain}"
    );
    let (furthest, poorest) = by_offset[by_offset.len() - 1];
    assert!(
        poorest < 0.2,
        "case {furthest:?}: a point as far from every road as this network allows is a poor \
         match, so it scores near the bottom of the 0..1 range, got {poorest}"
    );

    // The meridian road nearest longitude -123.0945 is a street, so a point a metre east of
    // its carriageway sits at a crossing of two roads a car may travel. WHICH of the two
    // carried it is not what this value reports: the point lies on the network either way, so
    // the match is as certain as any other on-road match.
    let at_a_crossing = confidence_of(
        &router,
        "at a crossing",
        [on_a_road(-123.094_5) + 0.000_01, on_a_road(49.28)],
        300,
    )
    .await;
    assert!(
        at_a_crossing > 0.9,
        "a point at a crossing of two roads lies on the network whichever of them carried it, so \
         its match is certain, got {at_a_crossing} against {certain} for a point on one road"
    );
}

// ---------------------------------------------------------------------------
// The snapped geometry
// ---------------------------------------------------------------------------

/// `SnappedGeometry` is the geometry of the ROADS the trace was matched onto: every vertex
/// of it lies on a road of the network and every segment of it follows one road, so a
/// client drawing the line stays on the network instead of cutting across the blocks
/// between the matched points.
///
/// Rows span the ways two consecutive matched points can sit on the network — the same
/// road, two crossing roads, two roads that never meet — and the length of the trace.
#[tokio::test]
async fn the_snapped_geometry_runs_along_the_roads_the_trace_matched() {
    let router = routes_service();
    // name, the trace, whether consecutive matched points lie on different roads (so the
    // line has to turn, and carries more vertices than there are matched points).
    let cases: [(&str, Vec<[f64; 2]>, bool); 5] = [
        (
            // Both on the parallel road nearest 49.28, ~70 m apart along it, each midway
            // between the meridian roads crossing it.
            "two points on one road",
            vec![
                [between_roads(-123.1), on_a_road(49.28)],
                [between_roads(-123.099), on_a_road(49.28)],
            ],
            false,
        ),
        (
            // One on a parallel road, one on a meridian road: they cross.
            "two points on crossing roads",
            vec![
                [between_roads(-123.1), on_a_road(49.28)],
                [on_a_road(-123.1), between_roads(49.2805)],
            ],
            true,
        ),
        (
            // Both on meridian roads of different longitude: they never meet.
            "two points on roads that never meet",
            vec![
                [on_a_road(-123.1), between_roads(49.2805)],
                [on_a_road(-123.101), between_roads(49.2815)],
            ],
            true,
        ),
        (
            "a five-point trace across the grid",
            vec![
                [-123.100_18, 49.280_18],
                [-123.100_92, 49.280_93],
                [-123.101_67, 49.281_71],
                [-123.102_44, 49.282_39],
                [-123.103_11, 49.283_05],
            ],
            true,
        ),
        (
            "a trace in the southern hemisphere",
            vec![[151.210_18, -33.870_18], [151.211_12, -33.869_21], [151.212_07, -33.868_33]],
            true,
        ),
    ];

    for (case, trace, turns) in cases {
        let body = json!({
            "TracePoints": trace.iter().map(|position| json!({ "Position": position })).collect::<Vec<Value>>(),
            "SnapRadius": 300,
            "SnappedGeometryFormat": "Simple",
        });
        let (status, answer, _) = post(&router, SNAP, &body).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");
        let matched: Vec<[f64; 2]> = answer["SnappedTracePoints"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: SnappedTracePoints is required, got {answer:?}"))
            .iter()
            .map(|point| position(&point["SnappedPosition"], case))
            .collect();
        let line: Vec<[f64; 2]> = answer["SnappedGeometry"]["LineString"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: the snapped line string is required, got {answer:?}"))
            .iter()
            .map(|vertex| position(vertex, case))
            .collect();

        assert_eq!(
            line.first().copied(),
            matched.first().copied(),
            "case {case:?}: the line starts at the first matched point, got {line:?}"
        );
        assert_eq!(
            line.last().copied(),
            matched.last().copied(),
            "case {case:?}: the line ends at the last matched point, got {line:?}"
        );
        for vertex in &line {
            assert!(
                vertex.iter().copied().any(on_a_road_line),
                "case {case:?}: every vertex of the line is on a road of the network, got {vertex:?}"
            );
        }
        // One road of this network runs at a constant longitude or at a constant latitude,
        // so a segment that follows one road moves in exactly one coordinate.
        for pair in line.windows(2) {
            let moved: Vec<usize> = (0..2)
                .filter(|axis| (pair[0][*axis] - pair[1][*axis]).abs() > 1e-9)
                .collect();
            assert_eq!(
                moved.len(), 1,
                "case {case:?}: every segment of the line follows one road, so it moves in one \
                 coordinate only, got {pair:?}"
            );
        }
        if turns {
            assert!(
                line.len() > matched.len(),
                "case {case:?}: the matched points are on different roads, so the line turns \
                 between them, got {} vertices for {} matched points",
                line.len(),
                matched.len()
            );
        }
        // Following the roads is never shorter than the straight line between the points.
        let along_roads: f64 = line.windows(2).map(|pair| great_circle_m(pair[0], pair[1])).sum();
        let straight: f64 = matched.windows(2).map(|pair| great_circle_m(pair[0], pair[1])).sum();
        assert!(
            along_roads >= straight - 0.5,
            "case {case:?}: the line along the roads ({along_roads:.1} m) cannot be shorter than \
             the chords between the matched points ({straight:.1} m)"
        );
    }
}

// ---------------------------------------------------------------------------
// The optimized order
// ---------------------------------------------------------------------------

/// Every order of `stops`, as lists of indexes into it.
fn permutations(stops: &[usize]) -> Vec<Vec<usize>> {
    if stops.len() <= 1 {
        return vec![stops.to_vec()];
    }
    let mut orders = Vec::new();
    for (index, stop) in stops.iter().enumerate() {
        let mut rest: Vec<usize> = stops.to_vec();
        rest.remove(index);
        for mut order in permutations(&rest) {
            order.insert(0, *stop);
            orders.push(order);
        }
    }
    orders
}

/// `OptimizeWaypoints` "calculates the optimal order to travel between a set of waypoints
/// to minimize either the travel time or the distance travelled during the journey", so the
/// order it answers with is the one whose WHOLE journey — origin, every waypoint, and the
/// destination where the request names one — costs least in what the objective minimizes.
/// Nothing cheaper exists, which this test checks against the service's own cost table:
/// `CalculateRouteMatrix` over the same stops, which reports the cost of every hop.
///
/// Rows span the number of waypoints, both objectives, whether the journey ends at a
/// destination (the case where the cheapest first hop is the wrong one), and the travel
/// mode.
#[tokio::test]
async fn the_optimized_order_is_the_cheapest_order_of_the_whole_journey() {
    let router = routes_service();
    // name, origin, waypoints, destination, travel mode, objective.
    type OrderCase = (
        &'static str,
        [f64; 2],
        Vec<[f64; 2]>,
        Option<[f64; 2]>,
        &'static str,
        &'static str,
    );
    let cases: [OrderCase; 5] = [
        (
            "three waypoints, fastest, open-ended",
            [-123.1, 49.28],
            vec![[-123.112, 49.272], [-123.085, 49.262], [-123.104, 49.291]],
            None,
            "Car",
            "FastestRoute",
        ),
        (
            // The nearest waypoint lies the wrong way for the journey: taking it first is
            // the cheapest hop and leaves the dearest remainder.
            "two waypoints and a destination, fastest",
            [-123.1, 49.28],
            vec![[-123.091, 49.28], [-123.111, 49.28]],
            Some([-123.03, 49.28]),
            "Car",
            "FastestRoute",
        ),
        (
            "four waypoints, shortest, open-ended",
            [-123.1, 49.28],
            vec![
                [-123.112, 49.272],
                [-123.085, 49.262],
                [-123.104, 49.291],
                [-123.093, 49.286],
            ],
            None,
            "Car",
            "ShortestRoute",
        ),
        (
            "four waypoints and a destination, shortest, by truck",
            [-123.1, 49.28],
            vec![
                [-123.112, 49.272],
                [-123.085, 49.262],
                [-123.104, 49.291],
                [-123.093, 49.286],
            ],
            Some([-123.07, 49.3]),
            "Truck",
            "ShortestRoute",
        ),
        (
            "three waypoints and a destination in the southern hemisphere, fastest",
            [151.21, -33.87],
            vec![[151.222, -33.862], [151.196, -33.879], [151.204, -33.855]],
            Some([151.24, -33.85]),
            "Scooter",
            "FastestRoute",
        ),
    ];

    for (case, origin, waypoints, destination, travel_mode, objective) in cases {
        // Stop 0 is the origin, 1..=n the waypoints, and the last the destination where
        // there is one: the indexing the cost table below is read with.
        let stops: Vec<[f64; 2]> = std::iter::once(origin)
            .chain(waypoints.iter().copied())
            .chain(destination)
            .collect();
        let endpoints: Vec<Value> = stops.iter().map(|position| json!({ "Position": position })).collect();
        let (status, matrix, _) = post(
            &router,
            MATRIX,
            &json!({
                "Origins": endpoints,
                "Destinations": endpoints,
                "TravelMode": travel_mode,
                "OptimizeRoutingFor": objective,
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200 from the matrix, got {status} with {matrix:?}");
        // What this objective minimizes, hop by hop.
        let member = if objective == "ShortestRoute" { "Distance" } else { "Duration" };
        let cost = |from: usize, to: usize| -> i64 {
            let entry = &matrix["RouteMatrix"][from][to];
            assert!(
                entry["Error"].is_null(),
                "case {case:?}: the hop {from} -> {to} must be routable, got {entry:?}"
            );
            int(entry, member, case)
        };

        let mut body = json!({
            "Origin": origin,
            "OriginOptions": { "Id": "origin" },
            "Waypoints": waypoints
                .iter()
                .enumerate()
                .map(|(index, position)| json!({ "Id": format!("w{index}"), "Position": position }))
                .collect::<Vec<Value>>(),
            "TravelMode": travel_mode,
            "OptimizeSequencingFor": objective,
        });
        if let Some(destination) = destination {
            body["Destination"] = json!(destination);
            body["DestinationOptions"] = json!({ "Id": "destination" });
        }
        let (status, answer, _) = post(&router, OPTIMIZE, &body).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");

        // The cheapest journey any order of the waypoints makes, by the same cost table.
        let last = stops.len() - 1;
        let total_of = |order: &[usize]| -> i64 {
            let mut total = 0;
            let mut from = 0;
            for &stop in order {
                total += cost(from, stop);
                from = stop;
            }
            if destination.is_some() {
                total += cost(from, last);
            }
            total
        };
        let orders = permutations(&(1..=waypoints.len()).collect::<Vec<usize>>());
        let cheapest = orders
            .iter()
            .map(|order| total_of(order))
            .min()
            .unwrap_or_else(|| panic!("case {case:?}: there is always at least one order"));

        // The order the service answered with, as stop indexes.
        let visited: Vec<String> = answer["OptimizedWaypoints"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: OptimizedWaypoints is required, got {answer:?}"))
            .iter()
            .map(|waypoint| waypoint["Id"].as_str().unwrap_or_default().to_string())
            .collect();
        let answered: Vec<usize> = visited
            .iter()
            .filter_map(|id| id.strip_prefix('w'))
            .filter_map(|index| index.parse::<usize>().ok())
            .map(|index| index + 1)
            .collect();
        assert_eq!(
            answered.len(),
            waypoints.len(),
            "case {case:?}: every waypoint must be sequenced once, got {visited:?}"
        );
        assert_eq!(
            total_of(&answered),
            cheapest,
            "case {case:?}: the answered order {visited:?} costs {} where the cheapest order of \
             the journey costs {cheapest}",
            total_of(&answered)
        );

        // And the totals the answer reports are that same journey: the objective's total
        // over the connections it carries.
        let reported = if objective == "ShortestRoute" {
            int(&answer, "Distance", case)
        } else {
            int(&answer["TimeBreakdown"], "TravelDuration", case)
        };
        assert_eq!(
            reported, cheapest,
            "case {case:?}: the reported {member} of the journey must be the cheapest order's \
             {cheapest}, got {reported}"
        );
    }
}

// ---------------------------------------------------------------------------
// How a road is identified
// ---------------------------------------------------------------------------

/// Every text this service reports a road by is a designation a client can render or match
/// on: the road's number and the side of the origin it runs on. The grid index the network
/// identifies a road by internally is signed, and a negative numeral is not part of any
/// road's name, so no reported text carries one.
///
/// Rows span the four quadrants of the world — both signs of longitude and of latitude —
/// because the sign of the index is exactly what used to reach the text, and the travel
/// modes that decide which classes of road are travelled.
#[tokio::test]
async fn a_road_is_reported_by_a_designation_and_never_by_a_signed_index() {
    let router = routes_service();
    // name, origin, destination, travel mode.
    let cases: [(&str, [f64; 2], [f64; 2], &str); 5] = [
        ("west and north of the origin", [-123.1, 49.28], [-123.08, 49.29], "Car"),
        ("west and south of the origin", [-70.65, -33.45], [-70.63, -33.44], "Car"),
        ("east and north of the origin", [2.294, 48.858], [2.314, 48.868], "Truck"),
        ("east and south of the origin", [151.21, -33.87], [151.23, -33.86], "Scooter"),
        ("on foot, west and north", [-123.1, 49.28], [-123.097, 49.283], "Pedestrian"),
    ];

    for (case, origin, destination, travel_mode) in cases {
        let (status, answer, _) = post(
            &router,
            ROUTES,
            &json!({
                "Origin": origin,
                "Destination": destination,
                "TravelMode": travel_mode,
                "TravelStepType": "TurnByTurn",
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status} with {answer:?}");

        let mut texts: Vec<String> = Vec::new();
        for label in answer["Routes"][0]["MajorRoadLabels"].as_array().unwrap_or(&Vec::new()) {
            for member in ["RoadName", "RouteNumber"] {
                if let Some(text) = label[member]["Value"].as_str() {
                    texts.push(text.to_string());
                }
            }
        }
        let details = &answer["Routes"][0]["Legs"][0]["VehicleLegDetails"]["TravelSteps"];
        let steps = details
            .as_array()
            .cloned()
            .or_else(|| {
                answer["Routes"][0]["Legs"][0]["PedestrianLegDetails"]["TravelSteps"]
                    .as_array()
                    .cloned()
            })
            .unwrap_or_default();
        for step in &steps {
            for road in ["CurrentRoad", "NextRoad"] {
                for name in step[road]["RoadName"].as_array().unwrap_or(&Vec::new()) {
                    if let Some(text) = name["Value"].as_str() {
                        texts.push(text.to_string());
                    }
                }
            }
        }

        assert!(
            !texts.is_empty(),
            "case {case:?}: a route over the grid travels named roads, got {answer:?}"
        );
        for text in &texts {
            assert!(
                !text.contains('-'),
                "case {case:?}: a road designation carries no negative numeral, got {text:?}"
            );
            assert!(
                text.ends_with(['N', 'S', 'E', 'W']),
                "case {case:?}: a road designation names the side of the origin it runs on, got {text:?}"
            );
        }
    }
}
