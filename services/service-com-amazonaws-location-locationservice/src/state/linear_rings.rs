// SPDX-License-Identifier: CC0-1.0

//! What a geofence write checks about ONE LINEAR RING before any shape question is asked of
//! it, at each published bound and one step past it.
//!
//! The ring-geometry verdicts — winding, meeting, containment — are pinned by
//! [`super::ring_geometry`]. What this module pins is the other half of a ring's own checks,
//! the ones that are about the LIST a request wrote rather than the shape it draws: how few
//! vertices a ring may list, how many vertices one geometry may hold across all its rings,
//! and how far apart in longitude two consecutive vertices may be before the edge between
//! them is the antimeridian being crossed. Each is a published bound, so each is asked here
//! AT that bound and one step past it, which is the one place a bound can be seen to be the
//! bound it is: a row at the bound would pass against an off-by-one check and a row past it
//! would pass against a check that refuses everything.
//!
//! Beside the geometry rather than in `tests/`, for the reason [`super::ring_geometry`]
//! gives: every row is a question about one function of this module's sibling and needs no
//! request, no store and no router to ask.

#![cfg(test)]

use crate::ops::types::GeofenceGeometry;

use super::geofence_geometry::validate_geofence_geometry;

/// A `Polygon` geometry over the rings given as `(longitude, latitude)` vertex lists.
fn polygon(rings: &[Vec<(f64, f64)>]) -> GeofenceGeometry {
    GeofenceGeometry {
        polygon: Some(
            rings
                .iter()
                .map(|ring| {
                    ring.iter()
                        .map(|(longitude, latitude)| vec![*longitude, *latitude])
                        .collect()
                })
                .collect(),
        ),
        ..GeofenceGeometry::default()
    }
}

/// The member path and the sentence `validate_geofence_geometry` refused `geometry` with, or
/// `None` when it accepted it.
fn refusal(geometry: &GeofenceGeometry) -> Option<(String, String)> {
    validate_geofence_geometry(geometry)
        .err()
        .map(|refused| (refused.member, refused.message))
}

/// A closed, counter-clockwise ring of `corners` corners on a circle of [`CIRCLE_DEGREES`]
/// degrees about the origin: a ring whose vertex COUNT is what a row varies, valid in every
/// other respect — convex, so no two of its edges meet, inside both coordinate domains and
/// with no edge spanning any great width of longitude.
///
/// The ring it answers lists `corners + 1` vertices, the last repeating the first to close it.
fn circular_ring(corners: usize) -> Vec<(f64, f64)> {
    let step = std::f64::consts::TAU / corners as f64;
    let mut ring: Vec<(f64, f64)> = (0..corners)
        .map(|index| {
            let angle = step * index as f64;
            (CIRCLE_DEGREES * angle.cos(), CIRCLE_DEGREES * angle.sin())
        })
        .collect();
    ring.push(ring[0]);
    ring
}

/// How far from the origin the rings above are drawn, in degrees: large enough that even the
/// ring with the most corners a geometry may hold keeps its corners well apart, and small
/// enough that every one of them is inside both WGS 84 domains.
const CIRCLE_DEGREES: f64 = 80.0;

/// SOURCE: `LinearRing` is `len 4..` and the `GeofenceGeometry$Polygon` doc states "A linear
/// ring is an array of 4 or more vertices, where the first and last vertex are the same", so
/// a ring of four vertices (a triangle, closed) is the shortest a polygon may carry.
const MIN_RING_VERTICES: usize = 4;

#[test]
fn a_ring_is_checked_against_the_fewest_vertices_a_linear_ring_may_list() {
    // At the bound: the shortest ring a polygon may carry.
    let at_the_bound = polygon(&[circular_ring(MIN_RING_VERTICES - 1)]);
    assert_eq!(
        refusal(&at_the_bound),
        None,
        "case {:?}: a ring of {MIN_RING_VERTICES} vertices is the shortest the member admits",
        "a ring at the published minimum"
    );

    // One step past it: the same shape with one vertex fewer.
    let past_the_bound = polygon(&[circular_ring(MIN_RING_VERTICES - 2)]);
    assert_eq!(
        refusal(&past_the_bound),
        Some((
            "Geometry.Polygon[0]".to_string(),
            "'Geometry.Polygon[0]' must have at least 4 vertices.".to_string(),
        )),
        "case {:?}: a ring one vertex short of the minimum is refused, naming the ring and \
         the bound",
        "a ring one vertex below the published minimum"
    );
}

/// SOURCE: the `GeofenceGeometry$Polygon` doc — "The entire `Polygon` must consist of no more
/// than 1,000 vertices, including all vertices from the exterior ring and all interior rings".
const MAX_GEOMETRY_VERTICES: usize = 1_000;

#[test]
fn the_vertex_budget_is_the_whole_geometrys_and_is_checked_at_its_bound() {
    // At the bound: one ring listing exactly the budget.
    let at_the_bound = polygon(&[circular_ring(MAX_GEOMETRY_VERTICES - 1)]);
    assert_eq!(
        refusal(&at_the_bound),
        None,
        "case {:?}: a geometry of exactly {MAX_GEOMETRY_VERTICES} vertices is within its \
         budget",
        "a geometry at the published vertex budget"
    );

    let past_the_bound = polygon(&[circular_ring(MAX_GEOMETRY_VERTICES)]);
    let expected = Some((
        "Geometry.Polygon".to_string(),
        "'Geometry.Polygon' must have no more than 1000 vertices.".to_string(),
    ));
    assert_eq!(
        refusal(&past_the_bound),
        expected,
        "case {:?}: a geometry one vertex past the budget is refused, naming the geometry and \
         the bound",
        "a geometry one vertex past the published budget"
    );

    // The budget is the WHOLE geometry's, so the same count spread over two rings — an
    // exterior ring and an interior one inside it — is past it just the same.
    let spread = polygon(&[
        counter_clockwise_square(4.0),
        clockwise_square(1.0, (MAX_GEOMETRY_VERTICES + 2) - MIN_RING_VERTICES),
    ]);
    assert_eq!(
        refusal(&spread),
        expected,
        "case {:?}: the budget counts every ring of the geometry",
        "two rings whose vertices together pass the published budget"
    );
}

/// A closed, counter-clockwise square of side `2 * half` about `(0, 0)`.
fn counter_clockwise_square(half: f64) -> Vec<(f64, f64)> {
    vec![
        (-half, -half),
        (half, -half),
        (half, half),
        (-half, half),
        (-half, -half),
    ]
}

/// A closed, CLOCKWISE square of side `2 * half` about `(0, 0)` — the sequence an interior
/// ring is listed in — padded to `vertices` vertices by repeating its last corner, which
/// names the same boundary while listing as many vertices as a row asks for.
fn clockwise_square(half: f64, vertices: usize) -> Vec<(f64, f64)> {
    let mut ring = vec![
        (-half, -half),
        (-half, half),
        (half, half),
        (half, -half),
        (-half, -half),
    ];
    while ring.len() < vertices {
        let last = ring[ring.len() - 2];
        ring.insert(ring.len() - 1, last);
    }
    ring
}

/// SOURCE: the `GeofenceGeometry` doc's note — "Amazon Location doesn't currently support
/// polygons that cross the antimeridian" — read against the WGS 84 longitude domain, whose
/// width is 360 degrees: an edge wider than half of it is the shorter way round, which is the
/// antimeridian being crossed rather than a wide polygon.
const MAX_EDGE_LONGITUDE_SPAN: f64 = 180.0;

#[test]
fn an_edge_may_span_half_the_longitude_domain_but_no_more() {
    // At the bound: an edge spanning exactly half the domain is a wide polygon, not a
    // crossing.
    let at_the_bound = polygon(&[vec![
        (-MAX_EDGE_LONGITUDE_SPAN / 2.0, 0.0),
        (MAX_EDGE_LONGITUDE_SPAN / 2.0, 0.0),
        (0.0, 1.0),
        (-MAX_EDGE_LONGITUDE_SPAN / 2.0, 0.0),
    ]]);
    assert_eq!(
        refusal(&at_the_bound),
        None,
        "case {:?}: an edge spanning exactly {MAX_EDGE_LONGITUDE_SPAN} degrees crosses nothing",
        "an edge at the published span"
    );

    // One step past it: the same ring with that edge a little wider.
    let past_the_bound = polygon(&[vec![
        (-MAX_EDGE_LONGITUDE_SPAN / 2.0 - 0.5, 0.0),
        (MAX_EDGE_LONGITUDE_SPAN / 2.0, 0.0),
        (0.0, 1.0),
        (-MAX_EDGE_LONGITUDE_SPAN / 2.0 - 0.5, 0.0),
    ]]);
    assert_eq!(
        refusal(&past_the_bound),
        Some((
            "Geometry.Polygon[0]".to_string(),
            "'Geometry.Polygon[0]' must not cross the antimeridian.".to_string(),
        )),
        "case {:?}: an edge wider than half the domain is the antimeridian being crossed",
        "an edge past the published span"
    );
}
