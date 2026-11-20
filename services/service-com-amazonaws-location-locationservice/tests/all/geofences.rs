// SPDX-License-Identifier: CC0-1.0

//! Geofences: the writes and the read that proves what they stored.
//!
//! RULE: a geofence write — one geofence through `PutGeofence`, or a batch of them
//! through `BatchPutGeofence` — STORES every geofence whose `Geometry` satisfies the
//! service's own geometry constraints under its `GeofenceId` in the named
//! collection, so the write is readable afterwards by `GetGeofence` (its geometry
//! byte for byte, `Status` `ACTIVE`, the `CreateTime` it was first stored with and
//! the `UpdateTime` of the last write) and a put for an id the collection already
//! holds UPDATES that geofence (its `CreateTime` stays, its `UpdateTime` moves)
//! instead of adding a second one. A geometry that does not satisfy them stores
//! nothing and is refused where the write reports a failure: per entry as a
//! `BatchItemError` with `Code` `"ValidationError"` inside a 200 for the batch
//! write, and as a 400 `ValidationException` naming the member in its `fieldList`
//! for the single write, which has no per-entry channel. The geometry constraints
//! come from the model
//! (`smithy_query(query="shape",shape="GeofenceGeometry")`): exactly one of
//! `Polygon`, `Circle`, `MultiPolygon` or `Geobuf` names the geometry, every linear
//! ring is closed and holds at least 4 vertices, no geometry holds more than 1,000
//! vertices, every vertex is a WGS 84 coordinate, and a `Circle`'s radius is
//! greater than zero and no larger than 100,000 metres.
//!
//! The tables span what the rule generalises over: the write the geofence arrives
//! through (single or batch), the geometry member a geofence is defined with
//! (circle, single-ring polygon, polygon with an interior ring, multipolygon,
//! Geobuf blob), how many properties it carries (0..3, the modelled bound), which
//! constraint an entry violates, and where the violating entry sits in the batch.
//! The request the issue report replays — one valid entry beside one invalid one —
//! is one row of the partial-failure table.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine: a geometry the read reports
//! survived because it was PERSISTED, not because a handler echoed its input back.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one HTTP harness this crate's suites share, declared from `tests/all.rs`.
use crate::harness::{call, emulator, millis_of, Answer};

/// The `BatchItemError$Code` every per-entry refusal carries: the
/// `BatchItemErrorCode` member for "Input fails to satisfy the constraints specified
/// by the service".
const VALIDATION_ERROR: &str = "ValidationError";

async fn create_collection(router: &MultiServiceRouter, name: &str) {
    let answer = call(
        router,
        "POST",
        "/geofencing/v0/collections",
        Some(json!({ "CollectionName": name })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "CreateGeofenceCollection {name:?} must succeed, got {} {:?}",
        answer.status, answer.body
    );
}

async fn batch_put(router: &MultiServiceRouter, collection: &str, entries: Value) -> Answer {
    call(
        router,
        "POST",
        &format!("/geofencing/v0/collections/{collection}/put-geofences"),
        Some(json!({ "Entries": entries })),
    )
    .await
}

async fn get_geofence(router: &MultiServiceRouter, collection: &str, geofence: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/geofencing/v0/collections/{collection}/geofences/{geofence}"),
        None,
    )
    .await
}

/// One geofence written through `PutGeofence`: the single-write binding the model
/// declares (`PUT /geofencing/v0/collections/{CollectionName}/geofences/{GeofenceId}`,
/// with `Geometry` and `GeofenceProperties` in the body).
async fn put_geofence(
    router: &MultiServiceRouter,
    collection: &str,
    geofence: &str,
    geometry: Value,
    properties: Option<Value>,
) -> Answer {
    let mut body = json!({ "Geometry": geometry });
    if let Some(properties) = properties {
        body["GeofenceProperties"] = properties;
    }
    call(
        router,
        "PUT",
        &format!("/geofencing/v0/collections/{collection}/geofences/{geofence}"),
        Some(body),
    )
    .await
}

async fn geofence_count(router: &MultiServiceRouter, collection: &str) -> i64 {
    let described = call(
        router,
        "GET",
        &format!("/geofencing/v0/collections/{collection}"),
        None,
    )
    .await;
    assert_eq!(
        described.status, 200,
        "DescribeGeofenceCollection {collection:?} must succeed, got {} {:?}",
        described.status, described.body
    );
    described
        .json()
        .get("GeofenceCount")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("DescribeGeofenceCollection must report a GeofenceCount, got {:?}", described.body))
}

/// The same JSON with every number rendered as a float, so a table's `1000.0`
/// compares equal to a wire `1000` whichever way a serializer spelled it.
fn normalized(value: &Value) -> Value {
    match value {
        Value::Number(number) => json!(number.as_f64().unwrap_or_default()),
        Value::Array(items) => Value::Array(items.iter().map(normalized).collect()),
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(k, v)| (k.clone(), normalized(v)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}

/// The `Successes` / `Errors` list of a batch answer.
fn list(case: &str, body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} list, got {body}"))
        .clone()
}

/// A geometry made of one closed, counter-clockwise ring around a point.
fn square(longitude: f64, latitude: f64) -> Value {
    json!([[
        [longitude, latitude],
        [longitude + 0.01, latitude],
        [longitude + 0.01, latitude + 0.01],
        [longitude, latitude],
    ]])
}

/// One row of the write-then-read table: the geometry member the geofence is
/// defined with, and the properties it is put with.
struct GeofenceCase {
    case: &'static str,
    geofence_id: &'static str,
    geometry: fn() -> Value,
    properties: Option<Value>,
}

/// The write-then-read table. Rows differ along the geometry member (every one the
/// model declares) and the number of properties (0, 1 and the modelled maximum of
/// 3), so what the read reports comes from the store in each shape of geometry
/// rather than from one.
fn geofence_cases() -> Vec<GeofenceCase> {
    vec![
        GeofenceCase {
            case: "circle, no properties",
            geofence_id: "store-front",
            geometry: || json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } }),
            properties: None,
        },
        GeofenceCase {
            case: "circle at the largest radius the member admits",
            geofence_id: "metro.area-1",
            // `Circle$Radius`: "no larger than 100,000 (100 kilometers)" — the
            // published bound itself is accepted.
            geometry: || json!({ "Circle": { "Center": [0.0, 0.0], "Radius": 100_000.0 } }),
            properties: Some(json!({ "zone": "metro" })),
        },
        GeofenceCase {
            case: "single-ring polygon, three properties",
            geofence_id: "warehouse",
            geometry: || json!({ "Polygon": square(-122.4, 47.6) }),
            // `PropertyMap` is `len 0..3`: the modelled maximum.
            properties: Some(json!({ "team": "logistics", "env": "prod", "tier": "1" })),
        },
        GeofenceCase {
            case: "polygon with an interior ring",
            geofence_id: "campus_2",
            geometry: || {
                json!({
                    "Polygon": [
                        // Exterior ring, counter-clockwise, closed.
                        [[-0.02, -0.02], [0.02, -0.02], [0.02, 0.02], [-0.02, 0.02], [-0.02, -0.02]],
                        // Interior ring, clockwise, closed.
                        [[-0.01, -0.01], [-0.01, 0.01], [0.01, 0.01], [0.01, -0.01], [-0.01, -0.01]],
                    ]
                })
            },
            properties: None,
        },
        GeofenceCase {
            case: "multipolygon of two polygons",
            geofence_id: "two-sites",
            geometry: || json!({ "MultiPolygon": [square(10.0, 10.0), square(-10.0, -10.0)] }),
            properties: Some(json!({ "sites": "2" })),
        },
        GeofenceCase {
            case: "geobuf blob",
            geofence_id: "encoded.fence",
            // A blob is base64 on the restJson1 wire; the bytes are stored and
            // reported back as given, so any non-empty encoding round-trips.
            geometry: || json!({ "Geobuf": "Z2VvYnVm" }),
            properties: None,
        },
        GeofenceCase {
            case: "a ring against the northern and eastern domain bounds",
            geofence_id: "north-east",
            // The published bounds of each axis (±180 longitude, ±90 latitude) are
            // INSIDE the domain, so a ring may touch them.
            geometry: || {
                json!({
                    "Polygon": [[
                        [179.98, 89.98],
                        [180.0, 89.98],
                        [180.0, 90.0],
                        [179.98, 89.98],
                    ]]
                })
            },
            properties: None,
        },
        GeofenceCase {
            case: "a ring against the southern and western domain bounds",
            geofence_id: "south-west",
            geometry: || {
                json!({
                    "Polygon": [[
                        [-180.0, -90.0],
                        [-179.98, -90.0],
                        [-179.98, -89.98],
                        [-180.0, -90.0],
                    ]]
                })
            },
            properties: None,
        },
        GeofenceCase {
            case: "a ring with a vertex in the middle of one of its edges",
            geofence_id: "mid-edge-vertex",
            // Three vertices on one line in the same direction are a corner the ring
            // turns through by no angle at all: the same boundary as the square without
            // it, so the ring is a valid one.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.005, 0.0],
                        [0.01, 0.0],
                        [0.01, 0.01],
                        [0.0, 0.01],
                        [0.0, 0.0],
                    ]]
                })
            },
            properties: None,
        },
        GeofenceCase {
            case: "a ring that lists one vertex twice in a row",
            geofence_id: "repeated_vertex",
            // A vertex repeated in a row names an edge of no length, which names the
            // same boundary: the ring is valid and is stored as it was given.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.01, 0.0],
                        [0.01, 0.0],
                        [0.01, 0.01],
                        [0.0, 0.0],
                    ]]
                })
            },
            properties: None,
        },
    ]
}

/// The entry a row of the table sends.
fn entry(case: &GeofenceCase) -> Value {
    let mut value = json!({
        "GeofenceId": case.geofence_id,
        "Geometry": (case.geometry)(),
    });
    if let Some(properties) = &case.properties {
        value["GeofenceProperties"] = properties.clone();
    }
    value
}

#[tokio::test]
async fn batch_put_then_get_round_trips_every_geometry_kind() {
    let router = emulator();
    create_collection(&router, "fences").await;

    for case in geofence_cases() {
        let answered = batch_put(&router, "fences", json!([entry(&case)])).await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: BatchPutGeofence must answer 200, got {} {:?}",
            case.case, answered.status, answered.body
        );
        let body = answered.json();
        assert!(
            list(case.case, &body, "Errors").is_empty(),
            "case {:?}: a valid entry must not be reported, got {:?}",
            case.case,
            body
        );
        let successes = list(case.case, &body, "Successes");
        assert_eq!(
            successes.len(),
            1,
            "case {:?}: expected one success, got {successes:?}",
            case.case
        );
        assert_eq!(
            successes[0].get("GeofenceId").and_then(Value::as_str),
            Some(case.geofence_id),
            "case {:?}: the success must name the stored geofence, got {:?}",
            case.case,
            successes[0]
        );
        let put_create = millis_of(&successes[0], "CreateTime");
        let put_update = millis_of(&successes[0], "UpdateTime");
        assert_eq!(
            put_create, put_update,
            "case {:?}: a first put's CreateTime and UpdateTime must be the same instant, got {:?}",
            case.case, successes[0]
        );

        let read = get_geofence(&router, "fences", case.geofence_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the stored geofence must be readable, got {} {:?}",
            case.case, read.status, read.body
        );
        let read = read.json();
        assert_eq!(
            read.get("GeofenceId").and_then(Value::as_str),
            Some(case.geofence_id),
            "case {:?}: GetGeofence must report the stored id, got {read}",
            case.case
        );
        let expected_geometry = normalized(&(case.geometry)());
        let reported_geometry = normalized(
            read.get("Geometry")
                .unwrap_or_else(|| panic!("case {:?}: expected a Geometry, got {read}", case.case)),
        );
        assert_eq!(
            reported_geometry, expected_geometry,
            "case {:?}: expected the geometry it was put with {expected_geometry}, got {reported_geometry}",
            case.case
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {:?}: a stored geofence is one the system has indexed, got {read}",
            case.case
        );
        assert_eq!(
            read.get("GeofenceProperties").map(normalized),
            case.properties.as_ref().map(normalized),
            "case {:?}: expected GeofenceProperties {:?}, got {read}",
            case.case,
            case.properties
        );
        assert_eq!(
            millis_of(&read, "CreateTime"),
            put_create,
            "case {:?}: GetGeofence must report the CreateTime the put stored, got {read}",
            case.case
        );
        assert_eq!(
            millis_of(&read, "UpdateTime"),
            put_update,
            "case {:?}: GetGeofence must report the UpdateTime the put stored, got {read}",
            case.case
        );
    }

    // Every row is stored under its own id, so the collection holds them all.
    let expected = i64::try_from(geofence_cases().len()).expect("the table fits in an i64");
    assert_eq!(
        geofence_count(&router, "fences").await,
        expected,
        "the collection must report one geofence per stored row"
    );
}

#[tokio::test]
async fn one_batch_stores_every_entry_it_carries() {
    // The batch is the unit of the request, not of the write: every entry of one
    // request is readable afterwards.
    let router = emulator();
    create_collection(&router, "fleet").await;

    let cases = geofence_cases();
    let entries: Vec<Value> = cases.iter().map(entry).collect();
    let answered = batch_put(&router, "fleet", json!(entries)).await;
    assert_eq!(
        answered.status, 200,
        "BatchPutGeofence must answer 200, got {} {:?}",
        answered.status, answered.body
    );
    let body = answered.json();
    let successes = list("whole table", &body, "Successes");
    assert_eq!(
        successes.len(),
        cases.len(),
        "every entry of the batch must be reported a success, got {successes:?}"
    );
    for case in &cases {
        assert!(
            successes
                .iter()
                .any(|success| success.get("GeofenceId").and_then(Value::as_str)
                    == Some(case.geofence_id)),
            "case {:?}: the batch's success list must name it, got {successes:?}",
            case.case
        );
        assert_eq!(
            get_geofence(&router, "fleet", case.geofence_id).await.status,
            200,
            "case {:?}: every entry of the batch must be readable",
            case.case
        );
    }
}

/// One rejected entry: the geometry it carries, the member a refusal about it names
/// and the sentence the service refuses it with, the last two written out as
/// LITERALS so the wording is pinned by text rather than re-derived from the
/// implementation's own template.
struct RejectedCase {
    case: &'static str,
    geometry: fn() -> Value,
    /// The member path the refusal is about, as the request-level
    /// `ValidationException`'s `fieldList` entry names it.
    member: &'static str,
    message: &'static str,
}

/// The rejected-geometry table. Rows differ along which constraint the geometry
/// violates — the member combination, the radius bound in both directions, ring
/// closure, the vertex budget, the coordinate domain on either axis, ring winding in
/// both roles, ring self-intersection, rings touching, ring containment, the
/// antimeridian, and an encoding that carries nothing — so each constraint is
/// checked by the service rather than assumed. A ring shorter than 4 vertices is
/// NOT a row: `LinearRing` is modelled `len 4..`, so the request boundary refuses it
/// before any entry is looked at.
fn rejected_cases() -> Vec<RejectedCase> {
    vec![
        RejectedCase {
            case: "no geometry member at all",
            geometry: || json!({}),
            member: "Geometry",
            message: "'Geometry' must specify one of [Polygon, Circle, MultiPolygon, Geobuf].",
        },
        RejectedCase {
            case: "two geometry members at once",
            geometry: || {
                json!({
                    "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 },
                    "Polygon": square(-122.4, 47.6),
                })
            },
            member: "Geometry",
            message: "'Geometry' must specify only one of [Polygon, Circle, MultiPolygon, Geobuf].",
        },
        RejectedCase {
            case: "a radius of zero",
            geometry: || json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 0.0 } }),
            member: RADIUS_MEMBER,
            message: RADIUS_REQUIREMENT,
        },
        RejectedCase {
            case: "a negative radius",
            geometry: || json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": -1.0 } }),
            member: RADIUS_MEMBER,
            message: RADIUS_REQUIREMENT,
        },
        RejectedCase {
            case: "a radius one metre past the published bound",
            geometry: || json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 100_001.0 } }),
            member: RADIUS_MEMBER,
            message: RADIUS_REQUIREMENT,
        },
        RejectedCase {
            case: "a circle centred outside the coordinate domain",
            geometry: || json!({ "Circle": { "Center": [-200.0, 47.60], "Radius": 500.0 } }),
            member: "Geometry.Circle.Center",
            message: "longitude must be between -180 and 180 degrees",
        },
        RejectedCase {
            case: "an unclosed polygon ring",
            geometry: || {
                json!({
                    "Polygon": [[
                        [-122.4, 47.6],
                        [-122.3, 47.6],
                        [-122.3, 47.7],
                        [-122.4, 47.7],
                    ]]
                })
            },
            member: "Geometry.Polygon[0]",
            message:
                "'Geometry.Polygon[0]' must be closed: its first and last vertex must be the same.",
        },
        RejectedCase {
            case: "a polygon vertex outside the latitude domain",
            geometry: || {
                json!({
                    "Polygon": [[
                        [-122.4, 47.6],
                        [-122.3, 95.0],
                        [-122.3, 47.7],
                        [-122.4, 47.6],
                    ]]
                })
            },
            member: "Geometry.Polygon[0]",
            message: "latitude must be between -90 and 90 degrees",
        },
        RejectedCase {
            case: "a polygon past the vertex budget",
            geometry: || json!({ "Polygon": [too_many_vertices()] }),
            member: "Geometry.Polygon",
            message: "'Geometry.Polygon' must have no more than 1000 vertices.",
        },
        RejectedCase {
            case: "an unclosed ring inside a multipolygon",
            geometry: || {
                json!({
                    "MultiPolygon": [
                        square(10.0, 10.0),
                        [[
                            [-10.0, -10.0],
                            [-9.99, -10.0],
                            [-9.99, -9.99],
                            [-9.98, -9.99],
                        ]],
                    ]
                })
            },
            member: "Geometry.MultiPolygon[1][0]",
            message: "'Geometry.MultiPolygon[1][0]' must be closed: its first and last vertex must be the same.",
        },
        RejectedCase {
            case: "a multipolygon past the vertex budget",
            geometry: || json!({ "MultiPolygon": [[too_many_vertices()]] }),
            member: "Geometry.MultiPolygon",
            message: "'Geometry.MultiPolygon' must have no more than 1000 vertices.",
        },
        RejectedCase {
            case: "a clockwise exterior ring",
            // "The vertices for the exterior ring must be listed in
            // counter-clockwise sequence."
            geometry: || {
                json!({
                    "Polygon": [[
                        [-122.4, 47.6],
                        [-122.4, 47.7],
                        [-122.3, 47.7],
                        [-122.4, 47.6],
                    ]]
                })
            },
            member: "Geometry.Polygon[0]",
            message: "'Geometry.Polygon[0]' must be oriented counter-clockwise.",
        },
        RejectedCase {
            case: "a counter-clockwise interior ring",
            // "Vertices for all interior rings must be listed in clockwise
            // sequence."
            geometry: || {
                json!({
                    "Polygon": [
                        [[-0.02, -0.02], [0.02, -0.02], [0.02, 0.02], [-0.02, 0.02], [-0.02, -0.02]],
                        [[-0.01, -0.01], [0.01, -0.01], [0.01, 0.01], [-0.01, 0.01], [-0.01, -0.01]],
                    ]
                })
            },
            member: "Geometry.Polygon[1]",
            message: "'Geometry.Polygon[1]' must be oriented clockwise.",
        },
        RejectedCase {
            case: "a ring that intersects itself",
            // "No ring is permitted to intersect itself."
            geometry: || json!({ "Polygon": [[[0.0, 0.0], [0.01, 0.01], [0.01, 0.0], [0.0, 0.01], [0.0, 0.0]]] }),
            member: RING_MEMBER,
            message: RING_REQUIREMENT,
        },
        RejectedCase {
            case: "an interior ring touching the exterior ring at one point",
            // "Rings must not touch or cross each other." — a single shared point
            // is a touch, which plain geometric validity would admit.
            geometry: || {
                json!({
                    "Polygon": [
                        [[0.0, 0.0], [0.04, 0.0], [0.04, 0.04], [0.0, 0.04], [0.0, 0.0]],
                        [[0.02, 0.0], [0.01, 0.01], [0.03, 0.01], [0.02, 0.0]],
                    ]
                })
            },
            member: RING_MEMBER,
            message: RING_REQUIREMENT,
        },
        RejectedCase {
            case: "an interior ring outside the exterior ring",
            // "All interior rings must be fully contained within the exterior
            // ring."
            geometry: || {
                json!({
                    "Polygon": [
                        [[-0.02, -0.02], [0.02, -0.02], [0.02, 0.02], [-0.02, 0.02], [-0.02, -0.02]],
                        [[5.0, 5.0], [5.0, 5.01], [5.01, 5.01], [5.01, 5.0], [5.0, 5.0]],
                    ]
                })
            },
            member: RING_MEMBER,
            message: RING_REQUIREMENT,
        },
        RejectedCase {
            case: "a polygon that crosses the antimeridian",
            // "Amazon Location doesn't currently support polygons that cross the
            // antimeridian."
            geometry: || {
                json!({
                    "Polygon": [[
                        [170.0, 0.0],
                        [-170.0, 0.0],
                        [-170.0, 10.0],
                        [170.0, 0.0],
                    ]]
                })
            },
            member: "Geometry.Polygon[0]",
            message: "'Geometry.Polygon[0]' must not cross the antimeridian.",
        },
        RejectedCase {
            case: "an interior ring inside another interior ring",
            // "Interior rings must not contain other interior rings."
            geometry: || {
                json!({
                    "Polygon": [
                        [[-0.04, -0.04], [0.04, -0.04], [0.04, 0.04], [-0.04, 0.04], [-0.04, -0.04]],
                        [[-0.02, -0.02], [-0.02, 0.02], [0.02, 0.02], [0.02, -0.02], [-0.02, -0.02]],
                        [[-0.01, -0.01], [-0.01, 0.01], [0.01, 0.01], [0.01, -0.01], [-0.01, -0.01]],
                    ]
                })
            },
            member: RING_MEMBER,
            message: RING_REQUIREMENT,
        },
        RejectedCase {
            case: "a ring that doubles back along one of its edges",
            // "No ring is permitted to intersect itself": a ring that runs back down the
            // edge it just came up meets itself along that overlap, which a ring that
            // merely turns through a corner does not.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.02, 0.0],
                        [0.01, 0.0],
                        [0.01, 0.02],
                        [0.0, 0.0],
                    ]]
                })
            },
            member: RING_MEMBER,
            message: RING_REQUIREMENT,
        },
        RejectedCase {
            case: "an empty geobuf blob",
            geometry: || json!({ "Geobuf": "" }),
            member: "Geometry.Geobuf",
            message: "'Geometry.Geobuf' must carry a Geobuf-encoded geometry.",
        },
    ]
}

/// The sentence a polygon whose rings intersect, touch, nest wrongly or enclose no
/// area at all is refused with: the `GeofenceGeometry$Polygon` note list ("Rings
/// must not touch or cross each other", "All interior rings must be fully contained
/// within the exterior ring", "Interior rings must not contain other interior
/// rings", "No ring is permitted to intersect itself") in one sentence, written out
/// as a LITERAL so the wording is pinned by text.
const RING_REQUIREMENT: &str = "'Geometry.Polygon' rings must not intersect themselves, must not touch or cross each other, and every interior ring must be fully contained within the exterior ring and within no other interior ring.";

/// The member a ring requirement is about: the polygon as a whole, since the
/// requirement is about how its rings sit relative to each other.
const RING_MEMBER: &str = "Geometry.Polygon";

/// The sentence an unusable `Circle` radius is refused with: the member path and
/// the bound the `Circle$Radius` doc publishes ("Must be greater than zero and no
/// larger than 100,000 (100 kilometers)"), written out as a LITERAL.
const RADIUS_REQUIREMENT: &str =
    "'Geometry.Circle.Radius' must be greater than 0 and no larger than 100000.";

/// The member an unusable radius refusal is about.
const RADIUS_MEMBER: &str = "Geometry.Circle.Radius";

/// A closed ring of 1,002 in-domain vertices: one past the 1,000-vertex budget the
/// `Polygon` and `MultiPolygon` members document, with the ring itself closed so the
/// budget is the only thing it violates.
fn too_many_vertices() -> Value {
    let mut ring: Vec<Value> = (0..1_001)
        .map(|step| json!([-122.0 + f64::from(step) * 0.0001, 47.0]))
        .collect();
    ring.push(ring[0].clone());
    Value::Array(ring)
}

#[tokio::test]
async fn a_partial_batch_stores_the_valid_entry_and_reports_only_the_invalid_one() {
    // The issue's recorded request, generalised: one valid entry beside one
    // rejected one, the rejected one reported inside a 200 and storing nothing.
    for (index, rejected) in rejected_cases().into_iter().enumerate() {
        let router = emulator();
        create_collection(&router, "fences").await;

        let good = json!({
            "GeofenceId": "good",
            "Geometry": { "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } },
        });
        let bad = json!({ "GeofenceId": "bad", "Geometry": (rejected.geometry)() });
        // The rejected entry sits first in half the rows and last in the other
        // half: where it sits must not change what is stored.
        let entries = if index % 2 == 0 {
            json!([good, bad])
        } else {
            json!([bad, good])
        };

        let answered = batch_put(&router, "fences", entries).await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: a partial failure is still a 200, got {} {:?}",
            rejected.case, answered.status, answered.body
        );
        let body = answered.json();

        let successes = list(rejected.case, &body, "Successes");
        assert_eq!(
            successes
                .iter()
                .map(|success| success.get("GeofenceId").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            vec![Some("good")],
            "case {:?}: only the valid entry may be reported a success, got {successes:?}",
            rejected.case
        );

        let errors = list(rejected.case, &body, "Errors");
        assert_eq!(
            errors.len(),
            1,
            "case {:?}: only the invalid entry may be reported an error, got {errors:?}",
            rejected.case
        );
        assert_eq!(
            errors[0].get("GeofenceId").and_then(Value::as_str),
            Some("bad"),
            "case {:?}: the reported entry must name the refused geofence, got {:?}",
            rejected.case,
            errors[0]
        );
        let error = errors[0]
            .get("Error")
            .unwrap_or_else(|| panic!("case {:?}: expected an Error member, got {:?}", rejected.case, errors[0]));
        assert_eq!(
            error.get("Code").and_then(Value::as_str),
            Some(VALIDATION_ERROR),
            "case {:?}: expected Error.Code {VALIDATION_ERROR}, got {error}",
            rejected.case
        );
        assert_eq!(
            error.get("Message").and_then(Value::as_str),
            Some(rejected.message),
            "case {:?}: expected Error.Message {:?}, got {error}",
            rejected.case,
            rejected.message
        );

        assert_eq!(
            get_geofence(&router, "fences", "good").await.status,
            200,
            "case {:?}: the valid entry must be readable",
            rejected.case
        );
        let refused = get_geofence(&router, "fences", "bad").await;
        assert_eq!(
            refused.status, 404,
            "case {:?}: the refused entry must store nothing, got {} {:?}",
            rejected.case, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {:?}: expected ResourceNotFoundException, got {:?}",
            rejected.case,
            refused.body
        );
        assert_eq!(
            refused.message(),
            "Geofence not found: bad.",
            "case {:?}: expected the refusal to name the resource and the requested id, got {:?}",
            rejected.case,
            refused.message()
        );
        assert_eq!(
            geofence_count(&router, "fences").await,
            1,
            "case {:?}: only the valid entry may be counted by the collection",
            rejected.case
        );
    }
}

#[tokio::test]
async fn an_all_invalid_batch_stores_nothing() {
    let router = emulator();
    create_collection(&router, "fences").await;

    // `BatchPutGeofenceRequestEntryList` is `len 1..10`, so the table is sent in
    // chunks the model admits.
    let rejected = rejected_cases();
    for (chunk_index, chunk) in rejected.chunks(5).enumerate() {
        let entries: Vec<Value> = chunk
            .iter()
            .enumerate()
            .map(|(i, bad)| {
                json!({
                    "GeofenceId": format!("bad-{chunk_index}-{i}"),
                    "Geometry": (bad.geometry)(),
                })
            })
            .collect();
        let case = format!("chunk {chunk_index}");
        let answered = batch_put(&router, "fences", json!(entries)).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: BatchPutGeofence must answer 200, got {} {:?}",
            answered.status, answered.body
        );
        let body = answered.json();
        assert!(
            list(&case, &body, "Successes").is_empty(),
            "case {case:?}: no entry of this chunk may be reported a success, got {body}"
        );
        assert_eq!(
            list(&case, &body, "Errors").len(),
            chunk.len(),
            "case {case:?}: every entry of this chunk must be reported an error, got {body}"
        );
        for (i, bad) in chunk.iter().enumerate() {
            let id = format!("bad-{chunk_index}-{i}");
            let reported = list(&case, &body, "Errors")
                .into_iter()
                .find(|error| error.get("GeofenceId").and_then(Value::as_str) == Some(&id))
                .unwrap_or_else(|| panic!("case {case:?}: entry {id} must be reported, got {body}"));
            assert_eq!(
                reported
                    .get("Error")
                    .and_then(|error| error.get("Message"))
                    .and_then(Value::as_str),
                Some(bad.message),
                "case {case:?} entry {i}: expected Error.Message {:?}, got {reported}",
                bad.message
            );
        }
    }

    assert_eq!(
        geofence_count(&router, "fences").await,
        0,
        "a collection every entry was refused for must hold no geofence"
    );
}

#[tokio::test]
async fn re_putting_a_geofence_updates_it_and_keeps_its_create_time() {
    let router = emulator();
    create_collection(&router, "fences").await;

    let first = batch_put(
        &router,
        "fences",
        json!([{
            "GeofenceId": "moving",
            "Geometry": { "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } },
            "GeofenceProperties": { "generation": "first" },
        }]),
    )
    .await;
    assert_eq!(
        first.status, 200,
        "the first put must succeed, got {} {:?}",
        first.status, first.body
    );
    let stored_create = millis_of(&list("first put", &first.json(), "Successes")[0], "CreateTime");

    // The timestamps are millisecond-resolution, so the second put is separated
    // from the first by more than one tick.
    std::thread::sleep(std::time::Duration::from_millis(5));

    // Each row re-puts the SAME id with a different geometry member, so the update
    // replaces the geometry whatever it was defined with.
    for (case, geometry, properties) in [
        (
            "circle to polygon",
            json!({ "Polygon": square(-122.4, 47.6) }),
            json!({ "generation": "second" }),
        ),
        (
            "polygon to multipolygon",
            json!({ "MultiPolygon": [square(1.0, 1.0)] }),
            json!({ "generation": "third", "note": "updated" }),
        ),
    ] {
        let again = batch_put(
            &router,
            "fences",
            json!([{
                "GeofenceId": "moving",
                "Geometry": geometry,
                "GeofenceProperties": properties,
            }]),
        )
        .await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the update must succeed, got {} {:?}",
            again.status, again.body
        );
        let success = list(case, &again.json(), "Successes")[0].clone();
        assert_eq!(
            millis_of(&success, "CreateTime"),
            stored_create,
            "case {case:?}: an update must keep the CreateTime the geofence was first stored with, got {success}"
        );
        let updated = millis_of(&success, "UpdateTime");
        assert!(
            updated > stored_create,
            "case {case:?}: expected an UpdateTime later than the CreateTime {stored_create}, got {updated}"
        );

        let read = get_geofence(&router, "fences", "moving").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the updated geofence must be readable, got {} {:?}",
            read.status, read.body
        );
        let read = read.json();
        assert_eq!(
            read.get("Geometry").map(normalized),
            Some(normalized(&geometry)),
            "case {case:?}: GetGeofence must report the geometry the update stored, got {read}"
        );
        assert_eq!(
            read.get("GeofenceProperties").map(normalized),
            Some(normalized(&properties)),
            "case {case:?}: GetGeofence must report the properties the update stored, got {read}"
        );
        assert_eq!(
            millis_of(&read, "UpdateTime"),
            updated,
            "case {case:?}: GetGeofence must report the UpdateTime the update stored, got {read}"
        );
        assert_eq!(
            geofence_count(&router, "fences").await,
            1,
            "case {case:?}: re-putting an id must update the geofence, not add a second one"
        );
    }
}

#[tokio::test]
async fn putting_into_a_collection_that_was_never_created_is_not_found() {
    let router = emulator();
    // One collection exists; the names below satisfy `^[-._\w]+$` but were never
    // created, including case variants of the one that was.
    create_collection(&router, "Exists").await;

    for name in ["missing", "exists", "EXISTS", "Exists.2"] {
        let answered = batch_put(
            &router,
            name,
            json!([{
                "GeofenceId": "orphan",
                "Geometry": { "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } },
            }]),
        )
        .await;
        assert_eq!(
            answered.status, 404,
            "case {name:?}: an unknown collection must be 404, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.error_code(),
            "ResourceNotFoundException",
            "case {name:?}: expected ResourceNotFoundException, got {:?}",
            answered.body
        );
        let expected = format!("Geofence collection not found: {name}.");
        assert_eq!(
            answered.message(),
            expected,
            "case {name:?}: expected message {expected:?}, got {:?}",
            answered.message()
        );
    }

    // The refused writes stored nothing anywhere, including under the collection
    // that does exist.
    assert_eq!(
        geofence_count(&router, "Exists").await,
        0,
        "a write addressed to another name must not store a geofence here"
    );
    assert_eq!(
        get_geofence(&router, "Exists", "orphan").await.status,
        404,
        "a refused write must persist nothing"
    );
}

#[tokio::test]
async fn getting_a_geofence_the_collection_does_not_hold_is_not_found() {
    let router = emulator();
    create_collection(&router, "fences").await;
    let put = batch_put(
        &router,
        "fences",
        json!([{
            "GeofenceId": "Stored",
            "Geometry": { "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } },
        }]),
    )
    .await;
    assert_eq!(
        put.status, 200,
        "the positive control must be stored, got {} {:?}",
        put.status, put.body
    );

    // Ids that satisfy `^[-._\p{L}\p{N}]+$` but name no stored geofence, including
    // case variants of the one that is stored: the lookup is by exact id.
    for id in ["missing", "stored", "STORED", "Stored.2"] {
        let read = get_geofence(&router, "fences", id).await;
        assert_eq!(
            read.status, 404,
            "case {id:?}: an unknown geofence must be 404, got {} {:?}",
            read.status, read.body
        );
        let expected = format!("Geofence not found: {id}.");
        assert_eq!(
            read.message(),
            expected,
            "case {id:?}: expected message {expected:?}, got {:?}",
            read.message()
        );
    }
}

#[tokio::test]
async fn a_read_does_not_change_what_a_later_reader_sees() {
    let router = emulator();
    create_collection(&router, "fences").await;
    batch_put(
        &router,
        "fences",
        json!([{
            "GeofenceId": "stable",
            "Geometry": { "Polygon": square(-122.4, 47.6) },
            "GeofenceProperties": { "k": "v" },
        }]),
    )
    .await;

    let first = get_geofence(&router, "fences", "stable").await.json();
    for round in 1..=3 {
        let again = get_geofence(&router, "fences", "stable").await;
        assert_eq!(
            again.status, 200,
            "round {round}: the read must keep succeeding, got {} {:?}",
            again.status, again.body
        );
        assert_eq!(
            again.json(),
            first,
            "round {round}: GetGeofence must report the same geofence every time, got {:?}",
            again.body
        );
    }
    assert_eq!(
        geofence_count(&router, "fences").await,
        1,
        "the reads must not change what the collection holds"
    );
}

#[tokio::test]
async fn a_single_put_then_get_round_trips_every_geometry_kind() {
    // The issue's recorded workflow, generalised: PutGeofence for one geofence,
    // then GetGeofence for the same collection and id, over every geometry member
    // and property count the write-then-read table spans.
    let router = emulator();
    create_collection(&router, "fences").await;

    for case in geofence_cases() {
        let written = put_geofence(
            &router,
            "fences",
            case.geofence_id,
            (case.geometry)(),
            case.properties.clone(),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {:?}: PutGeofence must answer 200, got {} {:?}",
            case.case, written.status, written.body
        );
        let written = written.json();
        assert_eq!(
            written.get("GeofenceId").and_then(Value::as_str),
            Some(case.geofence_id),
            "case {:?}: the write must report the id it stored, got {written}",
            case.case
        );
        let put_create = millis_of(&written, "CreateTime");
        let put_update = millis_of(&written, "UpdateTime");
        assert_eq!(
            put_create, put_update,
            "case {:?}: a first put's CreateTime and UpdateTime must be the same instant, got {written}",
            case.case
        );

        let read = get_geofence(&router, "fences", case.geofence_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the geofence the put stored must be readable, got {} {:?}",
            case.case, read.status, read.body
        );
        let read = read.json();
        assert_eq!(
            read.get("GeofenceId").and_then(Value::as_str),
            Some(case.geofence_id),
            "case {:?}: GetGeofence must report the stored id, got {read}",
            case.case
        );
        let expected_geometry = normalized(&(case.geometry)());
        let reported_geometry = normalized(
            read.get("Geometry")
                .unwrap_or_else(|| panic!("case {:?}: expected a Geometry, got {read}", case.case)),
        );
        assert_eq!(
            reported_geometry, expected_geometry,
            "case {:?}: expected the geometry it was put with {expected_geometry}, got {reported_geometry}",
            case.case
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {:?}: a stored geofence is one the system has indexed, got {read}",
            case.case
        );
        assert_eq!(
            read.get("GeofenceProperties").map(normalized),
            case.properties.as_ref().map(normalized),
            "case {:?}: expected GeofenceProperties {:?}, got {read}",
            case.case,
            case.properties
        );
        assert_eq!(
            millis_of(&read, "CreateTime"),
            put_create,
            "case {:?}: GetGeofence must report the CreateTime the put stored, got {read}",
            case.case
        );
        assert_eq!(
            millis_of(&read, "UpdateTime"),
            put_update,
            "case {:?}: GetGeofence must report the UpdateTime the put stored, got {read}",
            case.case
        );
    }

    // Every row was stored under its own id, so the collection holds them all.
    let expected = i64::try_from(geofence_cases().len()).expect("the table fits in an i64");
    assert_eq!(
        geofence_count(&router, "fences").await,
        expected,
        "the collection must report one geofence per geofence the single write stored"
    );
}

#[tokio::test]
async fn a_single_put_with_a_rejected_geometry_refuses_the_request_and_stores_nothing() {
    // The single write has no per-entry channel, so the same geometry a batch
    // reports per entry inside a 200 refuses the WHOLE request here: a 400
    // `ValidationException` whose fieldList names the member the sentence is about.
    let router = emulator();
    create_collection(&router, "fences").await;

    for (index, rejected) in rejected_cases().into_iter().enumerate() {
        let geofence_id = format!("rejected-{index}");
        let refused = put_geofence(
            &router,
            "fences",
            &geofence_id,
            (rejected.geometry)(),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {:?}: a geometry the service refuses must be a 400, got {} {:?}",
            rejected.case, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {:?}: expected ValidationException, got {:?}",
            rejected.case,
            refused.body
        );
        assert_eq!(
            refused.message(),
            rejected.message,
            "case {:?}: expected message {:?}, got {:?}",
            rejected.case,
            rejected.message,
            refused.message()
        );
        let body = refused.json();
        assert_eq!(
            body.get("reason").and_then(Value::as_str),
            Some("FieldValidationFailed"),
            "case {:?}: expected reason FieldValidationFailed, got {body}",
            rejected.case
        );
        assert_eq!(
            body.get("fieldList")
                .and_then(Value::as_array)
                .map(|entries| entries
                    .iter()
                    .map(|entry| (
                        entry.get("name").and_then(Value::as_str).unwrap_or_default(),
                        entry
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                    ))
                    .collect::<Vec<_>>()),
            Some(vec![(rejected.member, rejected.message)]),
            "case {:?}: fieldList must name the member the sentence is about and repeat the sentence, got {body}",
            rejected.case
        );

        let read = get_geofence(&router, "fences", &geofence_id).await;
        assert_eq!(
            read.status, 404,
            "case {:?}: a refused write must store nothing, got {} {:?}",
            rejected.case, read.status, read.body
        );
        let expected = format!("Geofence not found: {geofence_id}.");
        assert_eq!(
            read.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            rejected.case,
            read.message()
        );
    }

    assert_eq!(
        geofence_count(&router, "fences").await,
        0,
        "a collection every write was refused for must hold no geofence"
    );
}

#[tokio::test]
async fn re_putting_one_geofence_replaces_it_and_keeps_its_create_time() {
    let router = emulator();
    create_collection(&router, "fences").await;

    let first = put_geofence(
        &router,
        "fences",
        "moving",
        json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } }),
        Some(json!({ "generation": "first" })),
    )
    .await;
    assert_eq!(
        first.status, 200,
        "the first put must succeed, got {} {:?}",
        first.status, first.body
    );
    let stored_create = millis_of(&first.json(), "CreateTime");

    // Each row re-puts the SAME id with a different geometry member and a
    // different number of properties, so the update replaces what is stored
    // whatever the geofence was defined with — including dropping the properties
    // when the new write carries none.
    for (case, geometry, properties) in [
        (
            "circle to polygon",
            json!({ "Polygon": square(-122.4, 47.6) }),
            Some(json!({ "generation": "second" })),
        ),
        (
            "polygon to multipolygon",
            json!({ "MultiPolygon": [square(1.0, 1.0)] }),
            Some(json!({ "generation": "third", "note": "updated" })),
        ),
        (
            "multipolygon to geobuf, no properties",
            json!({ "Geobuf": "Z2VvYnVm" }),
            None,
        ),
    ] {
        let again = put_geofence(&router, "fences", "moving", geometry.clone(), properties.clone())
            .await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the update must succeed, got {} {:?}",
            again.status, again.body
        );
        let answered = again.json();
        assert_eq!(
            millis_of(&answered, "CreateTime"),
            stored_create,
            "case {case:?}: an update must keep the CreateTime the geofence was first stored with, got {answered}"
        );
        let updated = millis_of(&answered, "UpdateTime");
        assert!(
            updated >= stored_create,
            "case {case:?}: expected an UpdateTime at or after the CreateTime {stored_create}, got {updated}"
        );

        let read = get_geofence(&router, "fences", "moving").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the updated geofence must be readable, got {} {:?}",
            read.status, read.body
        );
        let read = read.json();
        assert_eq!(
            read.get("Geometry").map(normalized),
            Some(normalized(&geometry)),
            "case {case:?}: GetGeofence must report the geometry the update stored, got {read}"
        );
        assert_eq!(
            read.get("GeofenceProperties").map(normalized),
            properties.as_ref().map(normalized),
            "case {case:?}: GetGeofence must report the properties the update stored, got {read}"
        );
        assert_eq!(
            millis_of(&read, "CreateTime"),
            stored_create,
            "case {case:?}: GetGeofence must keep reporting the first CreateTime, got {read}"
        );
        assert_eq!(
            millis_of(&read, "UpdateTime"),
            updated,
            "case {case:?}: GetGeofence must report the UpdateTime the update stored, got {read}"
        );
        assert_eq!(
            geofence_count(&router, "fences").await,
            1,
            "case {case:?}: re-putting an id must update the geofence, not add a second one"
        );
    }
}

#[tokio::test]
async fn a_single_put_into_a_collection_that_was_never_created_is_not_found() {
    let router = emulator();
    // One collection exists; the names below satisfy `^[-._\w]+$` but were never
    // created, including case variants of the one that was.
    create_collection(&router, "Exists").await;

    for name in ["missing", "exists", "EXISTS", "Exists.2"] {
        let answered = put_geofence(
            &router,
            name,
            "orphan",
            json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } }),
            None,
        )
        .await;
        assert_eq!(
            answered.status, 404,
            "case {name:?}: an unknown collection must be 404, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.error_code(),
            "ResourceNotFoundException",
            "case {name:?}: expected ResourceNotFoundException, got {:?}",
            answered.body
        );
        let expected = format!("Geofence collection not found: {name}.");
        assert_eq!(
            answered.message(),
            expected,
            "case {name:?}: expected message {expected:?}, got {:?}",
            answered.message()
        );
    }

    assert_eq!(
        geofence_count(&router, "Exists").await,
        0,
        "a write addressed to another name must not store a geofence here"
    );
    assert_eq!(
        get_geofence(&router, "Exists", "orphan").await.status,
        404,
        "a refused write must persist nothing"
    );
}

#[tokio::test]
async fn the_single_write_and_the_batch_write_store_into_one_keyspace() {
    // Both writes key a geofence by collection name and geofence id, so whichever
    // wrote it last is what the read reports, and the geofence the other write
    // created is UPDATED rather than duplicated.
    let router = emulator();
    create_collection(&router, "fences").await;

    let single = put_geofence(
        &router,
        "fences",
        "shared",
        json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 500.0 } }),
        Some(json!({ "written-by": "put" })),
    )
    .await;
    assert_eq!(
        single.status, 200,
        "the single write must succeed, got {} {:?}",
        single.status, single.body
    );
    let stored_create = millis_of(&single.json(), "CreateTime");

    let batched = batch_put(
        &router,
        "fences",
        json!([{
            "GeofenceId": "shared",
            "Geometry": { "Polygon": square(-122.4, 47.6) },
            "GeofenceProperties": { "written-by": "batch" },
        }]),
    )
    .await;
    assert_eq!(
        batched.status, 200,
        "the batch write must succeed, got {} {:?}",
        batched.status, batched.body
    );
    let success = list("batch over a single write", &batched.json(), "Successes")[0].clone();
    assert_eq!(
        millis_of(&success, "CreateTime"),
        stored_create,
        "the batch must keep the CreateTime the single write stored, got {success}"
    );

    let read = get_geofence(&router, "fences", "shared").await.json();
    assert_eq!(
        read.get("Geometry").map(normalized),
        Some(normalized(&json!({ "Polygon": square(-122.4, 47.6) }))),
        "the read must report the geometry the last write stored, got {read}"
    );
    assert_eq!(
        read.get("GeofenceProperties").map(normalized),
        Some(normalized(&json!({ "written-by": "batch" }))),
        "the read must report the properties the last write stored, got {read}"
    );

    // And the other way round: the single write updates what the batch stored.
    let again = put_geofence(
        &router,
        "fences",
        "shared",
        json!({ "MultiPolygon": [square(5.0, 5.0)] }),
        None,
    )
    .await;
    assert_eq!(
        again.status, 200,
        "the second single write must succeed, got {} {:?}",
        again.status, again.body
    );
    assert_eq!(
        millis_of(&again.json(), "CreateTime"),
        stored_create,
        "the single write must keep the CreateTime the geofence was first stored with, got {:?}",
        again.body
    );
    let read = get_geofence(&router, "fences", "shared").await.json();
    assert_eq!(
        read.get("Geometry").map(normalized),
        Some(normalized(&json!({ "MultiPolygon": [square(5.0, 5.0)] }))),
        "the read must report the geometry the last write stored, got {read}"
    );
    assert_eq!(
        geofence_count(&router, "fences").await,
        1,
        "one id written by both writes is one geofence"
    );
}

// --- how the rings of ONE polygon may sit relative to each other ---------------
//
// RULE: a polygon geometry is stored only when each of its linear rings is simple
// (it does not intersect itself), no two rings of that polygon share a point, and
// every interior ring lies wholly inside the exterior ring and inside no other
// interior ring; a polygon that breaks any of those is refused naming the polygon
// itself, because the requirement is about how its rings sit relative to each
// other. Source: the `GeofenceGeometry$Polygon` doc's note list ("No ring is
// permitted to intersect itself", "Rings must not touch or cross each other", "All
// interior rings must be fully contained within the exterior ring", "Interior rings
// must not contain other interior rings"), which the `MultiPolygon` member repeats
// for each of its component polygons.
//
// The tables below span what that rule generalises over: how many interior rings a
// polygon carries, whether the exterior ring is convex, whether a ring lists a
// vertex twice, how many corners a ring actually draws, which relation between two
// rings is broken (self-crossing, touching, crossing, coinciding, nesting, lying
// outside), and whether the polygon is the geometry itself or a component of a
// multipolygon — including a multipolygon whose COMPONENTS share a point, which is
// not a ring requirement.

/// The sentence a polygon whose rings sit wrongly relative to each other is refused
/// with, for the polygon reached by `member`: the note list in one sentence, with the
/// member path the only variable part.
fn ring_requirement(member: &str) -> String {
    format!(
        "'{member}' rings must not intersect themselves, must not touch or cross each other, \
         and every interior ring must be fully contained within the exterior ring and within \
         no other interior ring."
    )
}

/// A closed, counter-clockwise rectangle, the shape an exterior ring takes.
fn ccw_rectangle(west: f64, south: f64, east: f64, north: f64) -> Value {
    json!([
        [west, south],
        [east, south],
        [east, north],
        [west, north],
        [west, south],
    ])
}

/// A closed, CLOCKWISE rectangle, the shape an interior ring takes: the same corners
/// listed the other way round.
fn cw_rectangle(west: f64, south: f64, east: f64, north: f64) -> Value {
    json!([
        [west, south],
        [west, north],
        [east, north],
        [east, south],
        [west, south],
    ])
}

/// A closed ring that CROSSES itself away from its own vertices: its fourth edge
/// runs back across its first one. Wound counter-clockwise, so the only thing it
/// breaks is simplicity.
fn self_crossing_ring() -> Value {
    json!([[0.0, 0.0], [0.04, 0.0], [0.01, 0.03], [0.01, -0.01], [0.0, 0.0]])
}

/// One accepted ring-topology row: the polygon geometry a write carries.
struct AcceptedTopology {
    case: &'static str,
    geofence_id: &'static str,
    geometry: fn() -> Value,
}

/// The ring topologies a polygon MAY have.
fn accepted_topologies() -> Vec<AcceptedTopology> {
    vec![
        AcceptedTopology {
            case: "two interior rings side by side inside the exterior ring",
            geofence_id: "two-holes",
            geometry: || {
                json!({
                    "Polygon": [
                        ccw_rectangle(-0.1, -0.1, 0.1, 0.1),
                        cw_rectangle(-0.07, -0.02, -0.03, 0.02),
                        cw_rectangle(0.03, -0.02, 0.07, 0.02),
                    ]
                })
            },
        },
        AcceptedTopology {
            case: "a concave exterior ring",
            geofence_id: "l-shaped",
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.04, 0.0],
                        [0.04, 0.02],
                        [0.02, 0.02],
                        [0.02, 0.04],
                        [0.0, 0.04],
                        [0.0, 0.0],
                    ]]
                })
            },
        },
        AcceptedTopology {
            case: "a ring that lists one vertex twice in a row",
            geofence_id: "repeated.vertex",
            // A vertex listed twice draws a zero-length edge, which is no edge of
            // the ring: the ring it describes is the same simple one.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.02, 0.0],
                        [0.02, 0.0],
                        [0.02, 0.02],
                        [0.0, 0.0],
                    ]]
                })
            },
        },
        AcceptedTopology {
            case: "a multipolygon component carrying an interior ring",
            geofence_id: "component-hole",
            geometry: || {
                json!({
                    "MultiPolygon": [
                        [ccw_rectangle(10.0, 10.0, 10.2, 10.2)],
                        [
                            ccw_rectangle(-10.2, -10.2, -10.0, -10.0),
                            cw_rectangle(-10.15, -10.15, -10.05, -10.05),
                        ],
                    ]
                })
            },
        },
        AcceptedTopology {
            case: "two multipolygon components sharing a point",
            geofence_id: "touching-components",
            // The ring requirements are each polygon's own: the model bounds how
            // much the components of one multipolygon may intersect each other
            // ("Each edge of a component `Polygon` must intersect no more than 5
            // edges from other `Polygons`") rather than keeping them apart.
            geometry: || {
                json!({
                    "MultiPolygon": [
                        [[[0.0, 0.0], [0.02, 0.0], [0.02, 0.02], [0.0, 0.0]]],
                        [[[0.0, 0.0], [-0.02, -0.02], [0.0, -0.02], [0.0, 0.0]]],
                    ]
                })
            },
        },
    ]
}

/// One refused ring-topology row: the polygon geometry, and the member a refusal
/// about it names — the polygon the broken relation is between the rings of.
struct RefusedTopology {
    case: &'static str,
    geometry: fn() -> Value,
    member: &'static str,
}

/// The ring topologies a polygon may NOT have.
fn refused_topologies() -> Vec<RefusedTopology> {
    vec![
        RefusedTopology {
            case: "an exterior ring crossing itself away from its vertices",
            geometry: || json!({ "Polygon": [self_crossing_ring()] }),
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "an interior ring coinciding with the exterior ring",
            geometry: || {
                json!({
                    "Polygon": [
                        ccw_rectangle(0.0, 0.0, 0.04, 0.04),
                        cw_rectangle(0.0, 0.0, 0.04, 0.04),
                    ]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "an interior ring crossing the exterior ring",
            geometry: || {
                json!({
                    "Polygon": [
                        ccw_rectangle(0.0, 0.0, 0.04, 0.04),
                        cw_rectangle(0.03, 0.01, 0.05, 0.03),
                    ]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "two interior rings touching each other at one point",
            geometry: || {
                json!({
                    "Polygon": [
                        ccw_rectangle(-0.1, -0.1, 0.1, 0.1),
                        [[0.0, 0.0], [-0.04, -0.02], [-0.04, 0.02], [0.0, 0.0]],
                        [[0.0, 0.0], [0.04, 0.02], [0.04, -0.02], [0.0, 0.0]],
                    ]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "one interior ring nested inside another interior ring",
            geometry: || {
                json!({
                    "Polygon": [
                        ccw_rectangle(-0.1, -0.1, 0.1, 0.1),
                        cw_rectangle(-0.05, -0.05, 0.05, 0.05),
                        cw_rectangle(-0.02, -0.02, 0.02, 0.02),
                    ]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "an exterior ring enclosing no area at all",
            // Every vertex on one line: the ring encloses nothing, so it is not a
            // ring whose orientation can be read.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.01, 0.01],
                        [0.02, 0.02],
                        [0.0, 0.0],
                    ]]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "a ring listing one point four times",
            // Closed, and at the modelled minimum of four vertices, but it draws no
            // corner at all.
            geometry: || json!({ "Polygon": [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]] }),
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "a ring drawing a single line segment",
            // Two corners: a segment out and back, which encloses nothing.
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.0, 0.0],
                        [0.01, 0.01],
                        [0.0, 0.0],
                    ]]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "a ring listing two points back and forth",
            geometry: || {
                json!({
                    "Polygon": [[
                        [0.0, 0.0],
                        [0.01, 0.01],
                        [0.0, 0.0],
                        [0.01, 0.01],
                        [0.0, 0.0],
                    ]]
                })
            },
            member: RING_MEMBER,
        },
        RefusedTopology {
            case: "a self-crossing ring in the second component of a multipolygon",
            // Each component polygon is checked on its own, so the refusal names
            // the component the broken ring belongs to.
            geometry: || {
                json!({
                    "MultiPolygon": [
                        [ccw_rectangle(1.0, 1.0, 1.02, 1.02)],
                        [self_crossing_ring()],
                    ]
                })
            },
            member: "Geometry.MultiPolygon[1]",
        },
    ]
}

#[tokio::test]
async fn every_ring_topology_a_polygon_may_have_is_stored_and_read_back() {
    let router = emulator();
    create_collection(&router, "fences").await;

    for case in accepted_topologies() {
        let written = put_geofence(
            &router,
            "fences",
            case.geofence_id,
            (case.geometry)(),
            None,
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {:?}: PutGeofence must store a polygon whose rings sit legally, got {} {:?}",
            case.case, written.status, written.body
        );

        let read = get_geofence(&router, "fences", case.geofence_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the stored geofence must be readable, got {} {:?}",
            case.case, read.status, read.body
        );
        let read = read.json();
        let expected = normalized(&(case.geometry)());
        assert_eq!(
            read.get("Geometry").map(normalized),
            Some(expected.clone()),
            "case {:?}: expected the geometry it was put with {expected}, got {read}",
            case.case
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {:?}: a stored geofence is one the system has indexed, got {read}",
            case.case
        );
    }

    let expected = i64::try_from(accepted_topologies().len()).expect("the table fits in an i64");
    assert_eq!(
        geofence_count(&router, "fences").await,
        expected,
        "the collection must report one geofence per accepted ring topology"
    );
}

#[tokio::test]
async fn every_ring_topology_a_polygon_may_not_have_is_refused_by_both_writes() {
    // The sentence is the same one the single-ring rows are refused with, so the
    // table's literal and the member-path form cannot drift apart.
    assert_eq!(
        ring_requirement(RING_MEMBER),
        RING_REQUIREMENT,
        "the member-path form of the ring requirement must be the pinned sentence"
    );

    let router = emulator();
    create_collection(&router, "fences").await;

    for (index, refused) in refused_topologies().into_iter().enumerate() {
        let expected = ring_requirement(refused.member);
        let geofence_id = format!("refused-{index}");

        // The single write has no per-entry channel: the whole request is refused.
        let answered = put_geofence(
            &router,
            "fences",
            &geofence_id,
            (refused.geometry)(),
            None,
        )
        .await;
        assert_eq!(
            answered.status, 400,
            "case {:?}: a polygon whose rings sit illegally must be a 400, got {} {:?}",
            refused.case, answered.status, answered.body
        );
        assert_eq!(
            answered.error_code(),
            "ValidationException",
            "case {:?}: expected ValidationException, got {:?}",
            refused.case,
            answered.body
        );
        assert_eq!(
            answered.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            refused.case,
            answered.message()
        );
        let body = answered.json();
        assert_eq!(
            body.get("fieldList")
                .and_then(Value::as_array)
                .map(|entries| entries
                    .iter()
                    .map(|entry| (
                        entry
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        entry
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    ))
                    .collect::<Vec<_>>()),
            Some(vec![(refused.member.to_string(), expected.clone())]),
            "case {:?}: fieldList must name the polygon the refusal is about and repeat the sentence, got {body}",
            refused.case
        );

        // The batch write reports the same sentence per entry inside a 200.
        let batched = batch_put(
            &router,
            "fences",
            json!([{ "GeofenceId": geofence_id, "Geometry": (refused.geometry)() }]),
        )
        .await;
        assert_eq!(
            batched.status, 200,
            "case {:?}: a refused entry is still a 200, got {} {:?}",
            refused.case, batched.status, batched.body
        );
        let body = batched.json();
        assert!(
            list(refused.case, &body, "Successes").is_empty(),
            "case {:?}: the refused entry must not be reported a success, got {body}",
            refused.case
        );
        let errors = list(refused.case, &body, "Errors");
        assert_eq!(
            errors.len(),
            1,
            "case {:?}: the refused entry must be reported once, got {errors:?}",
            refused.case
        );
        let error = errors[0].get("Error").unwrap_or_else(|| {
            panic!(
                "case {:?}: expected an Error member, got {:?}",
                refused.case, errors[0]
            )
        });
        assert_eq!(
            error.get("Code").and_then(Value::as_str),
            Some(VALIDATION_ERROR),
            "case {:?}: expected Error.Code {VALIDATION_ERROR}, got {error}",
            refused.case
        );
        assert_eq!(
            error.get("Message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {:?}: expected Error.Message {expected:?}, got {error}",
            refused.case
        );

        // Neither write stored anything.
        let read = get_geofence(&router, "fences", &geofence_id).await;
        assert_eq!(
            read.status, 404,
            "case {:?}: a refused write must store nothing, got {} {:?}",
            refused.case, read.status, read.body
        );
    }

    assert_eq!(
        geofence_count(&router, "fences").await,
        0,
        "a collection every write was refused for must hold no geofence"
    );
}
