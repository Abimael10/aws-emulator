// SPDX-License-Identifier: CC0-1.0

//! The planar questions a geofence polygon's requirements are decided by: how a
//! linear ring is read as a shape at all, which sequence its vertices are listed in,
//! whether two rings of one polygon share any point, and whether one ring encloses a
//! point of another.
//!
//! They live here, beside [`super::geofence_geometry`], because the requirements that
//! use them are stated about a POLYGON ("No ring is permitted to intersect itself",
//! "Rings must not touch or cross each other", "All interior rings must be fully
//! contained within the exterior ring") while each one is answered about a ring or a
//! pair of them: that module states the requirements and this one answers the
//! questions, so the sentences a refusal carries stay in one place and the geometry
//! in another.
//!
//! Every answer is read from the `cavalier_contours` crate's closed-polyline
//! predicates — a polyline-to-polyline intersection search, a winding number and a
//! signed area — rather than derived here.

use cavalier_contours::core::math::Vector2;
use cavalier_contours::polyline::{PlineCreation, PlineSource, PlineSourceMut, Polyline};

/// Fewest corners a ring has to draw to enclose any area: three, a triangle. A ring
/// listing the same point four times, or two points back and forth, draws fewer.
pub(super) const MIN_RING_CORNERS: usize = 3;

/// The bulge of a straight edge: a linear ring's edges are line segments, never arcs.
const BULGE_STRAIGHT: f64 = 0.0;

/// One linear ring as the predicates below read it: a CLOSED polyline over the ring's
/// vertices.
///
/// A closed polyline carries the edge from its last vertex back to its first one
/// implicitly, so the ring's repeated closing vertex is dropped; a vertex repeating
/// the one before it is dropped too, since a zero-length edge is no edge of the ring
/// and a vertex a request lists twice does not change the ring's shape.
pub(super) fn closed_ring(vertices: &[[f64; 2]]) -> Polyline<f64> {
    let mut ring: Polyline<f64> = Polyline::with_capacity(vertices.len(), true);
    let mut previous: Option<[f64; 2]> = None;
    // The ring is closed by here, so its last vertex repeats its first one.
    for vertex in vertices.iter().take(vertices.len().saturating_sub(1)) {
        if previous == Some(*vertex) {
            continue;
        }
        ring.add(vertex[0], vertex[1], BULGE_STRAIGHT);
        previous = Some(*vertex);
    }
    ring
}

/// Whether two rings share any point at all — crossing at one, touching at one, or
/// running along each other.
pub(super) fn rings_intersect(ring: &Polyline<f64>, other: &Polyline<f64>) -> bool {
    let found = ring.find_intersects(other);
    !found.basic_intersects.is_empty() || !found.overlapping_intersects.is_empty()
}

/// The first vertex of a ring, as the point a containment test is asked about, or
/// `None` for a ring that draws no corner at all — which a ring reaching here cannot
/// be ([`MIN_RING_CORNERS`]), stated as a value rather than assumed.
pub(super) fn first_vertex(ring: &Polyline<f64>) -> Option<Vector2<f64>> {
    ring.get(0).map(|vertex| Vector2::new(vertex.x, vertex.y))
}

/// Whether `ring` encloses `point`, whichever way the ring is wound.
pub(super) fn encloses(ring: &Polyline<f64>, point: Vector2<f64>) -> bool {
    ring.winding_number(point) != 0
}

/// The sequence a ring's vertices are listed in, read from the sign of the area the
/// ring encloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindingOrder {
    /// The sequence an exterior ring is listed in: positive enclosed area.
    CounterClockwise,
    /// The sequence an interior ring is listed in: negative enclosed area.
    Clockwise,
}

/// The sequence a ring's vertices are listed in, or `None` when the ring encloses no
/// area at all and so has no sequence to read: the signed area a closed polyline
/// encloses is positive counter-clockwise and negative clockwise.
pub(super) fn winding_order(ring: &Polyline<f64>) -> Option<WindingOrder> {
    let enclosed = ring.area();
    if enclosed > 0.0 {
        Some(WindingOrder::CounterClockwise)
    } else if enclosed < 0.0 {
        Some(WindingOrder::Clockwise)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring as a geofence request lists it: the vertices, closed by a repeat of the
    /// first one.
    fn listed(vertices: &[[f64; 2]]) -> Vec<[f64; 2]> {
        let mut closed = vertices.to_vec();
        if let Some(first) = vertices.first().copied() {
            closed.push(first);
        }
        closed
    }

    /// The ring those vertices draw.
    fn ring(vertices: &[[f64; 2]]) -> Polyline<f64> {
        closed_ring(&listed(vertices))
    }

    /// RULE: a ring is read as the corners it draws — the closing repeat and a vertex
    /// repeating the one before it are the same ring, and a ring drawing fewer than
    /// three corners draws no shape to ask a question about.
    #[test]
    fn a_ring_is_read_as_the_corners_it_draws() {
        for (case, vertices, corners) in [
            ("a triangle", vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]], 3),
            (
                "a square",
                vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                4,
            ),
            (
                "a square listing one of its corners twice",
                vec![
                    [0.0, 0.0],
                    [1.0, 0.0],
                    [1.0, 0.0],
                    [1.0, 1.0],
                    [0.0, 1.0],
                ],
                4,
            ),
            (
                "two points listed back and forth",
                vec![[0.0, 0.0], [1.0, 0.0]],
                2,
            ),
            (
                "the same point listed again and again",
                vec![[2.0, 2.0], [2.0, 2.0], [2.0, 2.0]],
                1,
            ),
        ] {
            let found = ring(&vertices).vertex_count();
            assert_eq!(
                found, corners,
                "case {case:?}: expected the ring to draw {corners} corners, got {found}"
            );
            let encloses_area = winding_order(&ring(&vertices)).is_some();
            let draws_a_shape = corners >= MIN_RING_CORNERS;
            assert_eq!(
                encloses_area, draws_a_shape,
                "case {case:?}: a ring of {corners} corners encloses an area only when it \
                 draws at least {MIN_RING_CORNERS}, got {encloses_area}"
            );
        }
    }

    /// RULE: the winding order is the sequence the vertices are listed in, whatever
    /// the ring's size, position or shape, and a ring enclosing no area has none.
    #[test]
    fn a_rings_winding_order_is_the_sequence_its_vertices_are_listed_in() {
        for (case, vertices, expected) in [
            (
                "a counter-clockwise square",
                vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                Some(WindingOrder::CounterClockwise),
            ),
            (
                "the same square listed the other way",
                vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
                Some(WindingOrder::Clockwise),
            ),
            (
                "a counter-clockwise triangle in the southern hemisphere",
                vec![[-30.0, -40.0], [-29.5, -40.0], [-30.0, -39.5]],
                Some(WindingOrder::CounterClockwise),
            ),
            (
                "a concave ring listed counter-clockwise",
                vec![
                    [0.0, 0.0],
                    [4.0, 0.0],
                    [4.0, 4.0],
                    [2.0, 1.0],
                    [0.0, 4.0],
                ],
                Some(WindingOrder::CounterClockwise),
            ),
            (
                "three collinear vertices, which enclose no area",
                vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]],
                None,
            ),
            (
                "one vertex listed three times",
                vec![[5.0, 5.0], [5.0, 5.0], [5.0, 5.0]],
                None,
            ),
        ] {
            let found = winding_order(&ring(&vertices));
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }

    /// RULE: two rings share a point whether they cross, touch at a single point or
    /// run along each other — and a ring wholly inside or outside another shares
    /// none.
    #[test]
    fn two_rings_share_a_point_when_they_cross_and_when_they_only_touch() {
        let outer = ring(&[[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]]);
        for (case, vertices, expected) in [
            (
                "a ring well inside it",
                vec![[1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 1.0]],
                false,
            ),
            (
                "a ring well outside it",
                vec![[10.0, 10.0], [10.0, 11.0], [11.0, 11.0], [11.0, 10.0]],
                false,
            ),
            (
                "a ring crossing its edge",
                vec![[3.0, 3.0], [3.0, 5.0], [5.0, 5.0], [5.0, 3.0]],
                true,
            ),
            (
                "a ring touching its edge at one point",
                vec![[2.0, 0.0], [1.0, 1.0], [3.0, 1.0]],
                true,
            ),
            (
                "a ring sharing one of its corners",
                vec![[0.0, 0.0], [1.0, 1.0], [2.0, 1.0]],
                true,
            ),
            (
                "a ring with an edge running along its edge",
                vec![[1.0, 0.0], [3.0, 0.0], [2.0, 1.0]],
                true,
            ),
        ] {
            let other = ring(&vertices);
            let found = rings_intersect(&outer, &other);
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
            // The question is about the pair, so it is answered the same either way
            // round.
            let found = rings_intersect(&other, &outer);
            assert_eq!(
                found, expected,
                "case {case:?} reversed: expected {expected:?}, got {found:?}"
            );
        }
    }

    /// RULE: a ring encloses the points inside it whichever way it is wound and
    /// however concave it is, and encloses nothing outside it.
    #[test]
    fn a_ring_encloses_the_points_inside_it_whichever_way_it_is_wound() {
        let square = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]];
        let reversed = vec![[0.0, 0.0], [0.0, 4.0], [4.0, 4.0], [4.0, 0.0]];
        let notched = vec![
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 4.0],
            [2.0, 1.0],
            [0.0, 4.0],
        ];
        for (case, vertices, point, expected) in [
            ("the centre of a square", &square, [2.0, 2.0], true),
            ("a point beyond that square", &square, [6.0, 2.0], false),
            (
                "a point level with that square but to its left",
                &square,
                [-1.0, 2.0],
                false,
            ),
            (
                "the centre of the same square wound the other way",
                &reversed,
                [2.0, 2.0],
                true,
            ),
            (
                "a point in the notch of a concave ring",
                &notched,
                [2.0, 3.0],
                false,
            ),
            (
                "a point in the body of that concave ring",
                &notched,
                [2.0, 0.5],
                true,
            ),
        ] {
            let found = encloses(&ring(vertices), Vector2::new(point[0], point[1]));
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?} for {point:?}, got {found:?}"
            );
        }
    }
}
