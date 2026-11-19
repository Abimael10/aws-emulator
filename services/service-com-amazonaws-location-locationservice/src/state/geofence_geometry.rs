// SPDX-License-Identifier: CC0-1.0

//! The checks a geofence WRITE makes on a `GeofenceGeometry` before the geometry is
//! stored, and the bounds those checks state.
//!
//! The model declares the shape of a geometry (`length` on the ring and polygon
//! lists, `length` on the Geobuf blob) and the request boundary refuses a violation
//! of those before a handler runs. What it cannot declare is the geometry's own
//! semantics — which member combinations name a geometry, whether a linear ring is
//! closed and wound the way its role requires, whether a radius is a usable one and
//! whether the whole geometry stays within its vertex budget — so those are the
//! service's own checks and live here, beside [`super::wgs84_position`], shared by
//! every operation that writes a geofence.
//!
//! Each refusal names the member path it concerns and states it, in single quotes,
//! then what that member has to satisfy — `'Geometry.Polygon[0]' must be oriented
//! counter-clockwise.` names the geometry's first linear ring. The one exception is a
//! coordinate outside its axis's WGS 84 domain, which keeps the fixed sentence this
//! service already refuses one with ([`super::wgs84_position`]), so a client matches
//! the same text wherever a coordinate was read. The only variable parts of a
//! sentence are the member path and the published bound it names: a `Position`, a
//! `Circle` and a `Geobuf` are `@sensitive` in the model, so no value read from a
//! request appears in a refusal.
//!
//! The intersection, containment and orientation requirements a polygon's rings have
//! to satisfy are read from the `cavalier_contours` crate's closed-polyline
//! predicates — a self-intersection scan, a polyline-to-polyline intersection search,
//! a winding number and a signed area — rather than hand-rolled here.

use cavalier_contours::core::math::Vector2;
use cavalier_contours::polyline::{PlineCreation, PlineSource, PlineSourceMut, Polyline};

use crate::ops::types::{Circle, GeofenceGeometry};

/// The member path of the geometry a geofence write carries, as a refusal states it.
pub const GEOMETRY_PATH: &str = "Geometry";

/// The `GeofenceGeometry` member a polygon geometry is given in.
const POLYGON: &str = "Polygon";

/// The member a circle geometry is given in.
const CIRCLE: &str = "Circle";

/// The member a multipolygon geometry is given in.
const MULTI_POLYGON: &str = "MultiPolygon";

/// The member a Geobuf-encoded geometry is given in.
const GEOBUF: &str = "Geobuf";

/// The `GeofenceGeometry` members that each describe a WHOLE geometry, spelled as
/// the model spells them and in the order a refusal lists them. Source:
/// `smithy_query(query="shape",shape="GeofenceGeometry")`, whose doc reads "A
/// geofence geometry can be a circle, a polygon, or a multipolygon. `Polygon` and
/// `MultiPolygon` geometries can be defined using their respective parameters, or
/// encoded in Geobuf format using the `Geobuf` parameter. Including multiple
/// geometry types in the same request will return a validation error."
pub const GEOMETRY_MEMBERS: [&str; 4] = [POLYGON, CIRCLE, MULTI_POLYGON, GEOBUF];

/// Largest radius a `Circle` geofence may carry, in metres. Source: the
/// `Circle$Radius` doc — "The radius of the circle in meters. Must be greater than
/// zero and no larger than 100,000 (100 kilometers)."
pub const MAX_CIRCLE_RADIUS_METRES: f64 = 100_000.0;

/// Most linear rings one `Polygon` may hold: "A `Polygon` is a list of up to 250
/// linear rings", and the member is `len 1..250`.
pub const MAX_RINGS_PER_POLYGON: usize = 250;

/// Most `Polygon` elements one `MultiPolygon` may hold: "A `MultiPolygon` is a list
/// of up to 250 `Polygon` elements", and the member is `len 1..250`.
pub const MAX_POLYGONS_PER_MULTI_POLYGON: usize = 250;

/// Fewest vertices a linear ring may hold: "A linear ring is an array of 4 or more
/// vertices, where the first and last vertex are the same", and `LinearRing` is
/// `len 4..`.
pub const MIN_RING_VERTICES: usize = 4;

/// Most vertices one geometry may hold, counted across every ring of every polygon.
/// Source: the `GeofenceGeometry$Polygon` doc — "The entire `Polygon` must consist
/// of no more than 1,000 vertices, including all vertices from the exterior ring
/// and all interior rings" — and the matching `MultiPolygon` sentence, which counts
/// "all vertices from all component `Polygons`".
pub const MAX_GEOMETRY_VERTICES: usize = 1_000;

/// What a geometry check refuses: the member path it concerns, and the sentence
/// about it.
///
/// Both, rather than only the text, because a refusal of one member is reported as
/// that pair — a per-entry `BatchItemError` states the sentence, and a
/// `ValidationException`'s `FieldList` entry names the member AND repeats the
/// sentence — so the member a check is about is carried out of the check rather
/// than re-derived from its wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeometryRefusal {
    /// The member path, as the sentence names it (`Geometry.Polygon[0]`).
    pub member: String,
    /// The sentence stating what that member has to satisfy.
    pub message: String,
}

impl GeometryRefusal {
    /// One refusal: the member path it concerns and the sentence about it.
    fn new(member: impl Into<String>, message: String) -> Self {
        Self {
            member: member.into(),
            message,
        }
    }
}

/// `Ok` when `geometry` names exactly one geometry and that geometry satisfies the
/// service's own constraints, otherwise the member the refusal concerns and the
/// sentence it states.
///
/// The caller decides how a refusal is reported: a batch write reports it per entry
/// inside a 200, a single write refuses the whole request.
pub fn validate_geofence_geometry(geometry: &GeofenceGeometry) -> Result<(), GeometryRefusal> {
    match named_members(geometry) {
        0 => return Err(must_name_one_geometry("must specify one of")),
        1 => {}
        _ => return Err(must_name_one_geometry("must specify only one of")),
    }

    // The vertex budget is the whole geometry's, so one counter is threaded through
    // every ring of every polygon rather than reset per polygon.
    let mut vertices = 0usize;
    if let Some(circle) = &geometry.circle {
        validate_circle(circle)?;
    }
    if let Some(polygon) = &geometry.polygon {
        let path = PolygonPath::of_polygon();
        validate_polygon(&path, polygon, &mut vertices)?;
    }
    if let Some(multi_polygon) = &geometry.multi_polygon {
        validate_multi_polygon(multi_polygon, &mut vertices)?;
    }
    if let Some(geobuf) = &geometry.geobuf {
        validate_geobuf(geobuf)?;
    }
    Ok(())
}

/// How many of [`GEOMETRY_MEMBERS`] the geometry carries.
fn named_members(geometry: &GeofenceGeometry) -> usize {
    [
        geometry.polygon.is_some(),
        geometry.circle.is_some(),
        geometry.multi_polygon.is_some(),
        geometry.geobuf.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count()
}

/// The sentence a geometry naming no member, or more than one, is refused with: the
/// member path, what it has to satisfy, and the members it may name — no value from
/// the request.
fn must_name_one_geometry(requirement: &str) -> GeometryRefusal {
    GeometryRefusal::new(
        GEOMETRY_PATH,
        format!(
            "'{GEOMETRY_PATH}' {requirement} [{}].",
            GEOMETRY_MEMBERS.join(", ")
        ),
    )
}

/// `Ok` when the circle's centre is a WGS 84 coordinate and its radius is a usable
/// one.
fn validate_circle(circle: &Circle) -> Result<(), GeometryRefusal> {
    super::wgs84_position(&circle.center).map_err(|why| {
        GeometryRefusal::new(format!("{GEOMETRY_PATH}.{CIRCLE}.Center"), why)
    })?;
    // Written as a refutation so a radius that is not a number (neither greater
    // than zero nor within the bound) is refused rather than silently stored.
    if !(circle.radius > 0.0 && circle.radius <= MAX_CIRCLE_RADIUS_METRES) {
        let member = format!("{GEOMETRY_PATH}.{CIRCLE}.Radius");
        return Err(GeometryRefusal::new(
            member.clone(),
            format!(
                "'{member}' must be greater than 0 and no larger than \
                 {MAX_CIRCLE_RADIUS_METRES}."
            ),
        ));
    }
    Ok(())
}

/// The member paths one polygon's refusals state: the geometry member the polygon
/// belongs to, and the polygon itself inside it.
///
/// They are the same path for a `Polygon` geometry and differ for a component of a
/// `MultiPolygon`, where the polygon carries its index — so a ring is always named
/// by the full path down to it (`Geometry.Polygon[0]`,
/// `Geometry.MultiPolygon[1][0]`).
struct PolygonPath {
    geometry: String,
    polygon: String,
}

impl PolygonPath {
    /// The paths of a geometry given as a `Polygon`.
    fn of_polygon() -> Self {
        let geometry = format!("{GEOMETRY_PATH}.{POLYGON}");
        Self {
            polygon: geometry.clone(),
            geometry,
        }
    }

    /// The paths of the `index`-th component polygon of a `MultiPolygon`.
    fn of_multi_polygon_component(index: usize) -> Self {
        let geometry = format!("{GEOMETRY_PATH}.{MULTI_POLYGON}");
        Self {
            polygon: format!("{geometry}[{index}]"),
            geometry,
        }
    }

    /// The path of the `index`-th linear ring of this polygon.
    fn ring(&self, index: usize) -> String {
        format!("{}[{index}]", self.polygon)
    }
}

/// `Ok` when every linear ring of one polygon is closed, long enough, made of WGS 84
/// coordinates, wound the way its role requires and free of the intersections a
/// polygon may not have, and the geometry's running vertex count stays within its
/// budget.
fn validate_polygon(
    path: &PolygonPath,
    rings: &[Vec<Vec<f64>>],
    vertices: &mut usize,
) -> Result<(), GeometryRefusal> {
    if rings.len() > MAX_RINGS_PER_POLYGON {
        return Err(GeometryRefusal::new(
            path.polygon.clone(),
            format!(
                "'{}' must have no more than {MAX_RINGS_PER_POLYGON} linear rings.",
                path.polygon
            ),
        ));
    }
    let mut checked: Vec<Polyline<f64>> = Vec::with_capacity(rings.len());
    for (index, ring) in rings.iter().enumerate() {
        if ring.len() < MIN_RING_VERTICES {
            return Err(GeometryRefusal::new(
                path.ring(index),
                format!(
                    "'{}' must have at least {MIN_RING_VERTICES} vertices.",
                    path.ring(index)
                ),
            ));
        }
        // The budget is checked BEFORE this ring's vertices are read, so the work
        // one request can ask for is bounded by the budget rather than by the
        // length of a ring the caller chose (`LinearRing` has no maximum).
        *vertices += ring.len();
        if *vertices > MAX_GEOMETRY_VERTICES {
            return Err(GeometryRefusal::new(
                path.geometry.clone(),
                format!(
                    "'{}' must have no more than {MAX_GEOMETRY_VERTICES} vertices.",
                    path.geometry
                ),
            ));
        }
        let mut coords = Vec::with_capacity(ring.len());
        for vertex in ring {
            let position = super::wgs84_position(vertex)
                .map_err(|why| GeometryRefusal::new(path.ring(index), why))?;
            coords.push(position);
        }
        // "the first and last vertex are the same (to form a closed boundary)".
        if coords.first() != coords.last() {
            return Err(GeometryRefusal::new(
                path.ring(index),
                format!(
                    "'{}' must be closed: its first and last vertex must be the same.",
                    path.ring(index)
                ),
            ));
        }
        validate_meridian_span(&path.ring(index), &coords)?;
        let closed = closed_ring(&coords);
        // A ring drawing fewer than three corners encloses no area, so it is not a
        // ring whose orientation, self-intersections or containment can be read at
        // all: it is refused here, with the same sentence a ring enclosing no area
        // is refused with below, rather than asked a question about a shape it does
        // not have.
        if closed.vertex_count() < MIN_RING_CORNERS {
            return Err(rings_must_not_intersect(path));
        }
        checked.push(closed);
    }

    // The exterior ring is the one listed first, "before any interior rings".
    let Some((exterior, interiors)) = checked.split_first() else {
        // Unreachable through the request boundary (`len 1..250`), stated rather
        // than assumed so nothing below reads an empty list.
        return Err(GeometryRefusal::new(
            path.polygon.clone(),
            format!(
                "'{}' must have at least one linear ring, the exterior ring.",
                path.polygon
            ),
        ));
    };

    validate_winding(&path.ring(0), exterior, EXTERIOR_WINDING, path)?;
    for (index, interior) in interiors.iter().enumerate() {
        // Interior rings are listed after the exterior one, so the first interior
        // ring is the geometry's ring 1.
        validate_winding(&path.ring(index + 1), interior, INTERIOR_WINDING, path)?;
    }

    // "No ring is permitted to intersect itself."
    if checked.iter().any(PlineSource::scan_for_self_intersect) {
        return Err(rings_must_not_intersect(path));
    }

    // "Rings must not touch or cross each other": two rings that share even a
    // single point are refused, which enclosed area alone would admit.
    for (index, ring) in checked.iter().enumerate() {
        if checked
            .iter()
            .skip(index + 1)
            .any(|other| rings_intersect(ring, other))
        {
            return Err(rings_must_not_intersect(path));
        }
    }

    // "All interior rings must be fully contained within the exterior ring" and
    // "Interior rings must not contain other interior rings". No two rings share a
    // point by here, so each one lies wholly inside or wholly outside another and a
    // single vertex of it places all of it.
    for (index, interior) in interiors.iter().enumerate() {
        let Some(inside) = first_vertex(interior) else {
            return Err(rings_must_not_intersect(path));
        };
        if !encloses(exterior, inside) {
            return Err(rings_must_not_intersect(path));
        }
        for other in interiors.iter().skip(index + 1) {
            let Some(other_inside) = first_vertex(other) else {
                return Err(rings_must_not_intersect(path));
            };
            if encloses(interior, other_inside) || encloses(other, inside) {
                return Err(rings_must_not_intersect(path));
            }
        }
    }
    Ok(())
}

/// Fewest corners a ring has to draw to enclose any area: three, a triangle. A ring
/// listing the same point four times, or two points back and forth, draws fewer.
const MIN_RING_CORNERS: usize = 3;

/// The bulge of a straight edge: a linear ring's edges are line segments, never arcs.
const BULGE_STRAIGHT: f64 = 0.0;

/// One linear ring as the predicates below read it: a CLOSED polyline over the ring's
/// vertices.
///
/// A closed polyline carries the edge from its last vertex back to its first one
/// implicitly, so the ring's repeated closing vertex is dropped; a vertex repeating
/// the one before it is dropped too, since a zero-length edge is no edge of the ring
/// and a vertex a request lists twice does not change the ring's shape.
fn closed_ring(vertices: &[[f64; 2]]) -> Polyline<f64> {
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
fn rings_intersect(ring: &Polyline<f64>, other: &Polyline<f64>) -> bool {
    let found = ring.find_intersects(other);
    !found.basic_intersects.is_empty() || !found.overlapping_intersects.is_empty()
}

/// The first vertex of a ring, as the point a containment test is asked about, or
/// `None` for a ring that draws no corner at all — which a ring reaching here cannot
/// be ([`MIN_RING_CORNERS`]), stated as a value rather than assumed.
fn first_vertex(ring: &Polyline<f64>) -> Option<Vector2<f64>> {
    ring.get(0).map(|vertex| Vector2::new(vertex.x, vertex.y))
}

/// Whether `ring` encloses `point`, whichever way the ring is wound.
fn encloses(ring: &Polyline<f64>, point: Vector2<f64>) -> bool {
    ring.winding_number(point) != 0
}

/// The sequence a ring's vertices are listed in, read from the sign of the area the
/// ring encloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindingOrder {
    /// The sequence an exterior ring is listed in: positive enclosed area.
    CounterClockwise,
    /// The sequence an interior ring is listed in: negative enclosed area.
    Clockwise,
}

/// The sequence the vertices of an exterior ring must be listed in, per the
/// `GeofenceGeometry$Polygon` doc: "The vertices for the exterior ring must be
/// listed in counter-clockwise sequence."
const EXTERIOR_WINDING: WindingOrder = WindingOrder::CounterClockwise;

/// The sequence the vertices of an interior ring must be listed in, per the same
/// doc: "Vertices for all interior rings must be listed in clockwise sequence."
const INTERIOR_WINDING: WindingOrder = WindingOrder::Clockwise;

/// The sentence a polygon whose rings intersect, touch, nest wrongly or enclose no
/// area at all is refused with: the member's note list in one sentence.
fn rings_must_not_intersect(path: &PolygonPath) -> GeometryRefusal {
    GeometryRefusal::new(
        path.polygon.clone(),
        format!(
            "'{}' rings must not intersect themselves, must not touch or cross each other, \
             and every interior ring must be fully contained within the exterior ring and \
             within no other interior ring.",
            path.polygon
        ),
    )
}

/// `Ok` when `ring` is wound the way its role requires.
///
/// A ring with no winding order at all encloses no area, so it is not a ring whose
/// orientation can be read: it is refused with the polygon's own ring requirements.
fn validate_winding(
    ring_path: &str,
    ring: &Polyline<f64>,
    required: WindingOrder,
    path: &PolygonPath,
) -> Result<(), GeometryRefusal> {
    match winding_order(ring) {
        Some(order) if order == required => Ok(()),
        Some(_) => Err(GeometryRefusal::new(
            ring_path,
            format!(
                "'{ring_path}' must be oriented {}.",
                winding_name(required)
            ),
        )),
        None => Err(rings_must_not_intersect(path)),
    }
}

/// The sequence a ring's vertices are listed in, or `None` when the ring encloses no
/// area at all and so has no sequence to read: the signed area a closed polyline
/// encloses is positive counter-clockwise and negative clockwise.
fn winding_order(ring: &Polyline<f64>) -> Option<WindingOrder> {
    let enclosed = ring.area();
    if enclosed > 0.0 {
        Some(WindingOrder::CounterClockwise)
    } else if enclosed < 0.0 {
        Some(WindingOrder::Clockwise)
    } else {
        None
    }
}

/// The sequence name a winding refusal states, spelled as the member doc spells it.
fn winding_name(order: WindingOrder) -> &'static str {
    match order {
        WindingOrder::CounterClockwise => "counter-clockwise",
        WindingOrder::Clockwise => "clockwise",
    }
}

/// The width of the WGS 84 longitude domain, in degrees.
const LONGITUDE_DOMAIN_WIDTH: f64 = 2.0 * super::MAX_LONGITUDE;

/// How far apart in longitude two consecutive vertices may be: an edge wider than
/// half the globe is the antimeridian being crossed rather than a wide polygon.
const MAX_EDGE_LONGITUDE_SPAN: f64 = LONGITUDE_DOMAIN_WIDTH / 2.0;

/// `Ok` when no edge of the ring crosses the antimeridian.
///
/// Source: the `GeofenceGeometry` doc's note — "Amazon Location doesn't currently
/// support polygons that cross the antimeridian."
fn validate_meridian_span(ring_path: &str, ring: &[[f64; 2]]) -> Result<(), GeometryRefusal> {
    let crosses = ring
        .windows(2)
        .any(|edge| (edge[1][0] - edge[0][0]).abs() > MAX_EDGE_LONGITUDE_SPAN);
    if crosses {
        return Err(GeometryRefusal::new(
            ring_path,
            format!("'{ring_path}' must not cross the antimeridian."),
        ));
    }
    Ok(())
}

/// `Ok` when every component polygon of a multipolygon is a valid polygon and the
/// whole geometry stays within its vertex budget.
///
/// Each component is checked on its own: the model bounds how much the components
/// of one multipolygon may intersect each other ("Each edge of a component
/// `Polygon` must intersect no more than 5 edges from other `Polygons`"), rather
/// than requiring them to be disjoint.
fn validate_multi_polygon(
    polygons: &[Vec<Vec<Vec<f64>>>],
    vertices: &mut usize,
) -> Result<(), GeometryRefusal> {
    if polygons.len() > MAX_POLYGONS_PER_MULTI_POLYGON {
        return Err(GeometryRefusal::new(
            format!("{GEOMETRY_PATH}.{MULTI_POLYGON}"),
            format!(
                "'{GEOMETRY_PATH}.{MULTI_POLYGON}' must have no more than \
                 {MAX_POLYGONS_PER_MULTI_POLYGON} polygons."
            ),
        ));
    }
    for (index, polygon) in polygons.iter().enumerate() {
        let path = PolygonPath::of_multi_polygon_component(index);
        validate_polygon(&path, polygon, vertices)?;
    }
    Ok(())
}

/// `Ok` when the Geobuf blob carries an encoding at all.
///
/// Its size bound (`Base64EncodedGeobuf` is `len ..700000`) is modelled, so the
/// request boundary refuses an oversized blob before any handler runs. The encoding
/// itself is NOT decoded here: the blob is stored and reported back byte for byte,
/// which is what `GetGeofence` promises ("The returned geometry will always match
/// the geometry format used when the geofence was created"), so nothing this service
/// answers depends on its contents. An EMPTY blob names no geometry, which is the
/// one thing a write can tell.
fn validate_geobuf(geobuf: &[u8]) -> Result<(), GeometryRefusal> {
    if geobuf.is_empty() {
        return Err(GeometryRefusal::new(
            format!("{GEOMETRY_PATH}.{GEOBUF}"),
            format!("'{GEOMETRY_PATH}.{GEOBUF}' must carry a Geobuf-encoded geometry."),
        ));
    }
    Ok(())
}
