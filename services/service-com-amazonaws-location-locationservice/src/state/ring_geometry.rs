// SPDX-License-Identifier: CC0-1.0

//! The RING-GEOMETRY verdicts a geofence write owes, asserted through the checks that make
//! them ([`super::geofence_geometry::validate_geofence_geometry`]).
//!
//! The requirements a polygon's rings have to satisfy are stated about the rings and decided
//! by a maintained crate's closed-polyline predicates, so what this module pins is the
//! VERDICT each ring form earns — which way a ring has to wind, that no two rings may share a
//! point, that no ring may cross itself, that every interior ring has to lie inside the
//! exterior one, and that a ring enclosing no area is not a ring at all. A change of predicate
//! crate, or of how a ring is handed to one, keeps those verdicts or it is a change in what a
//! geofence write accepts.
//!
//! Beside the geometry rather than in `tests/`, because every row here is a question about one
//! function of this module's sibling and needs no request, no store and no router to ask.

#![cfg(test)]

use crate::ops::types::GeofenceGeometry;

use super::geofence_geometry::validate_geofence_geometry;

/// A `Polygon` geometry over the rings given as `(x, y)` vertex lists.
fn polygon(rings: &[&[(f64, f64)]]) -> GeofenceGeometry {
    GeofenceGeometry {
        polygon: Some(
            rings
                .iter()
                .map(|ring| ring.iter().map(|(x, y)| vec![*x, *y]).collect())
                .collect(),
        ),
        ..GeofenceGeometry::default()
    }
}

/// A counter-clockwise square of side `2 * half` about `(0, 0)`, closed — the exterior ring
/// every row below that needs a valid one uses.
fn exterior(half: f64) -> Vec<(f64, f64)> {
    vec![
        (-half, -half),
        (half, -half),
        (half, half),
        (-half, half),
        (-half, -half),
    ]
}

/// A CLOCKWISE square of side `2 * half` about `(x, y)`, closed — the sequence an interior
/// ring is required to be listed in.
fn interior(x: f64, y: f64, half: f64) -> Vec<(f64, f64)> {
    vec![
        (x - half, y - half),
        (x - half, y + half),
        (x + half, y + half),
        (x + half, y - half),
        (x - half, y - half),
    ]
}

/// The sentence a polygon whose rings intersect, touch, nest wrongly or enclose no area at all
/// is refused with, and the member it is about: the polygon as a whole, since every one of
/// those requirements is about how its rings sit relative to each other.
const RING_MEMBER: &str = "Geometry.Polygon";

/// What a row expects of a geometry: acceptance, or the member a refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Accepted,
    /// Refused about the polygon's rings as a whole.
    RefusedAboutTheRings,
    /// Refused about ONE ring, named by its path.
    RefusedAboutRing(usize),
}

/// Every ring form a polygon geometry can take, against the verdict it earns.
///
/// The rows differ along what the ring requirements generalise over: which way each ring is
/// wound, how many rings the polygon holds, whether two of them cross, touch at a single point
/// or share a vertex, whether a ring crosses itself, where an interior ring sits relative to
/// the exterior one and to another interior one, and whether a ring encloses any area at all.
#[test]
fn every_ring_form_earns_its_documented_verdict() {
    let outer = exterior(4.0);
    let rows: Vec<(&str, GeofenceGeometry, Verdict)> = vec![
        (
            "a counter-clockwise exterior ring alone",
            polygon(&[&outer]),
            Verdict::Accepted,
        ),
        (
            "a counter-clockwise triangle",
            polygon(&[&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]]),
            Verdict::Accepted,
        ),
        (
            "a concave exterior ring",
            polygon(&[&[
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 4.0),
                (2.0, 2.0),
                (0.0, 4.0),
                (0.0, 0.0),
            ]]),
            Verdict::Accepted,
        ),
        (
            "an exterior ring listed clockwise",
            polygon(&[&[(0.0, 0.0), (0.0, 4.0), (4.0, 4.0), (4.0, 0.0), (0.0, 0.0)]]),
            Verdict::RefusedAboutRing(0),
        ),
        (
            "one clockwise interior ring inside it",
            polygon(&[&outer, &interior(0.0, 0.0, 1.0)]),
            Verdict::Accepted,
        ),
        (
            "two clockwise interior rings inside it, beside each other",
            polygon(&[&outer, &interior(-2.0, 0.0, 0.5), &interior(2.0, 0.0, 0.5)]),
            Verdict::Accepted,
        ),
        (
            "an interior ring listed counter-clockwise",
            polygon(&[&outer, &exterior(1.0)]),
            Verdict::RefusedAboutRing(1),
        ),
        (
            "an interior ring outside the exterior one",
            polygon(&[&outer, &interior(50.0, 50.0, 1.0)]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an interior ring crossing the exterior one",
            polygon(&[&outer, &interior(4.0, 0.0, 1.0)]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an interior ring touching the exterior one at a single point",
            polygon(&[&outer, &[(0.0, -4.0), (-1.0, -3.0), (1.0, -3.0), (0.0, -4.0)]]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an interior ring inside another interior ring",
            polygon(&[&outer, &interior(0.0, 0.0, 2.0), &interior(0.0, 0.0, 0.5)]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an exterior ring that crosses itself",
            polygon(&[&[(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an exterior ring whose vertices are collinear",
            polygon(&[&[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (0.0, 0.0)]]),
            Verdict::RefusedAboutTheRings,
        ),
        (
            "an exterior ring listing one point four times",
            polygon(&[&[(0.0, 0.0), (0.0, 0.0), (0.0, 0.0), (0.0, 0.0)]]),
            Verdict::RefusedAboutTheRings,
        ),
    ];

    for (case, geometry, expected) in rows {
        let answered = validate_geofence_geometry(&geometry);
        match (expected, &answered) {
            (Verdict::Accepted, _) => assert!(
                answered.is_ok(),
                "case {case:?}: expected the geometry to be accepted, got {answered:?}"
            ),
            (Verdict::RefusedAboutTheRings, Err(refusal)) => assert_eq!(
                refusal.member, RING_MEMBER,
                "case {case:?}: expected a refusal about {RING_MEMBER}, got {refusal:?}"
            ),
            (Verdict::RefusedAboutRing(index), Err(refusal)) => assert_eq!(
                refusal.member,
                format!("{RING_MEMBER}[{index}]"),
                "case {case:?}: expected a refusal about ring {index}, got {refusal:?}"
            ),
            (_, Ok(())) => panic!("case {case:?}: expected {expected:?}, got an acceptance"),
        }
    }
}
