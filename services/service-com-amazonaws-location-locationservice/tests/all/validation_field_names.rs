// SPDX-License-Identifier: CC0-1.0

//! What a `ValidationException`'s `fieldList` entry NAMES.
//!
//! RULE: a `fieldList` entry's `name` is the INPUT MEMBER the rejected value was
//! read from — the member path with no list subscript on it — while WHICH element of
//! that member was rejected is stated by the entry's `message` alone. Source:
//! `smithy_query(query="shape",shape="ValidationExceptionField")`, whose `Name` doc
//! reads "The field name where the invalid entry was detected" and whose `Message`
//! doc reads "A message with the reason for the validation exception error"; both are
//! `required`, and `ValidationException` carries them under `fieldList` beside its
//! own `message` and `reason`.
//!
//! So a geofence geometry refused for one linear ring of a `Polygon` or of one
//! component of a `MultiPolygon` is reported under `Geometry.Polygon` /
//! `Geometry.MultiPolygon`, with the ring's ordinal quoted in the message: a client
//! that groups validation failures by field name finds the member it sent, not a
//! member called `Geometry.Polygon[0]`, which the request has no such member for.
//!
//! The table spans what the rule generalises over: how deep the subscript sits (one
//! level under `Polygon`, two under `MultiPolygon`), which ordinal it carries (the
//! exterior ring, the first interior ring, a later interior ring), which check
//! produced the refusal (winding, ring closure, the coordinate domain, the
//! antimeridian), whether the sentence quotes a subject at all, and member paths that
//! carry NO subscript — those must come through unchanged, so the rule strips
//! subscripts rather than truncating paths.
//!
//! Every expectation is a LITERAL, so the wire text is pinned by text rather than
//! re-derived from the implementation's own template, and every call goes through the
//! shared harness (`crate::harness`) as a separate HTTP request.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, emulator, Answer};

/// One refused write: the geometry it carries, the member path the sentence quotes
/// (empty when the sentence quotes none), and the `fieldList` entry the service must
/// report — its `name` and its `message`.
struct FieldNameCase {
    case: &'static str,
    geometry: fn() -> Value,
    /// The subject the refusal's sentence quotes, as the table's `message` spells it,
    /// or `""` for a sentence that quotes no member.
    quoted: &'static str,
    /// The `fieldList` entry's `name`: the input member, with no subscript.
    name: &'static str,
    message: &'static str,
}

/// A closed, counter-clockwise ring — the winding an exterior ring requires, per the
/// `GeofenceGeometry$Polygon` doc: "The vertices for the exterior ring must be listed
/// in counter-clockwise sequence."
fn counter_clockwise_ring(longitude: f64, latitude: f64) -> Value {
    json!([
        [longitude, latitude],
        [longitude + 0.01, latitude],
        [longitude + 0.01, latitude + 0.01],
        [longitude, latitude],
    ])
}

/// The same ring listed the other way round: the winding an interior ring requires
/// ("Vertices for all interior rings must be listed in clockwise sequence") and the
/// one an exterior ring may not have.
fn clockwise_ring(longitude: f64, latitude: f64) -> Value {
    json!([
        [longitude, latitude],
        [longitude + 0.01, latitude + 0.01],
        [longitude + 0.01, latitude],
        [longitude, latitude],
    ])
}

/// The refused-write table.
fn field_name_cases() -> Vec<FieldNameCase> {
    vec![
        FieldNameCase {
            case: "a clockwise exterior ring, the polygon's ring 0",
            geometry: || json!({ "Polygon": [clockwise_ring(-122.4, 47.6)] }),
            quoted: "Geometry.Polygon[0]",
            name: "Geometry.Polygon",
            message: "'Geometry.Polygon[0]' must be oriented counter-clockwise.",
        },
        FieldNameCase {
            case: "a counter-clockwise interior ring, the polygon's ring 1",
            geometry: || {
                json!({
                    "Polygon": [
                        [[-0.02, -0.02], [0.02, -0.02], [0.02, 0.02], [-0.02, 0.02], [-0.02, -0.02]],
                        [[-0.01, -0.01], [0.01, -0.01], [0.01, 0.01], [-0.01, 0.01], [-0.01, -0.01]],
                    ]
                })
            },
            quoted: "Geometry.Polygon[1]",
            name: "Geometry.Polygon",
            message: "'Geometry.Polygon[1]' must be oriented clockwise.",
        },
        FieldNameCase {
            case: "a counter-clockwise interior ring behind a well-wound one, the polygon's ring 2",
            geometry: || {
                json!({
                    "Polygon": [
                        // Exterior, counter-clockwise, enclosing both interior rings.
                        [[-0.05, -0.05], [0.05, -0.05], [0.05, 0.05], [-0.05, 0.05], [-0.05, -0.05]],
                        // Interior, clockwise: the winding an interior ring requires.
                        [[-0.03, -0.03], [-0.03, -0.01], [-0.01, -0.01], [-0.01, -0.03], [-0.03, -0.03]],
                        // Interior, counter-clockwise: the one the service refuses.
                        [[0.01, 0.01], [0.03, 0.01], [0.03, 0.03], [0.01, 0.03], [0.01, 0.01]],
                    ]
                })
            },
            quoted: "Geometry.Polygon[2]",
            name: "Geometry.Polygon",
            message: "'Geometry.Polygon[2]' must be oriented clockwise.",
        },
        FieldNameCase {
            case: "a clockwise exterior ring in a multipolygon's second component",
            geometry: || {
                json!({
                    "MultiPolygon": [
                        [counter_clockwise_ring(10.0, 10.0)],
                        [clockwise_ring(-10.0, -10.0)],
                    ]
                })
            },
            quoted: "Geometry.MultiPolygon[1][0]",
            name: "Geometry.MultiPolygon",
            message: "'Geometry.MultiPolygon[1][0]' must be oriented counter-clockwise.",
        },
        FieldNameCase {
            case: "an unclosed ring in a multipolygon's second component",
            geometry: || {
                json!({
                    "MultiPolygon": [
                        [counter_clockwise_ring(10.0, 10.0)],
                        [[
                            [-10.0, -10.0],
                            [-9.99, -10.0],
                            [-9.99, -9.99],
                            [-9.98, -9.99],
                        ]],
                    ]
                })
            },
            quoted: "Geometry.MultiPolygon[1][0]",
            name: "Geometry.MultiPolygon",
            message: "'Geometry.MultiPolygon[1][0]' must be closed: its first and last vertex must be the same.",
        },
        FieldNameCase {
            case: "a polygon ring that crosses the antimeridian",
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
            quoted: "Geometry.Polygon[0]",
            name: "Geometry.Polygon",
            message: "'Geometry.Polygon[0]' must not cross the antimeridian.",
        },
        FieldNameCase {
            // The coordinate-domain sentence quotes no member at all, so the entry's
            // name can only come from the member path the value was read at.
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
            quoted: "",
            name: "Geometry.Polygon",
            message: "latitude must be between -90 and 90 degrees",
        },
        FieldNameCase {
            case: "a circle radius outside its bound, a member path with no subscript",
            geometry: || json!({ "Circle": { "Center": [-122.33, 47.60], "Radius": 0.0 } }),
            quoted: "Geometry.Circle.Radius",
            name: "Geometry.Circle.Radius",
            message: "'Geometry.Circle.Radius' must be greater than 0 and no larger than 100000.",
        },
        FieldNameCase {
            case: "a geometry naming no member, the geometry member itself",
            geometry: || json!({}),
            quoted: "Geometry",
            name: "Geometry",
            message: "'Geometry' must specify one of [Polygon, Circle, MultiPolygon, Geobuf].",
        },
        FieldNameCase {
            case: "an empty geobuf blob, a member path with no subscript",
            geometry: || json!({ "Geobuf": "" }),
            quoted: "Geometry.Geobuf",
            name: "Geometry.Geobuf",
            message: "'Geometry.Geobuf' must carry a Geobuf-encoded geometry.",
        },
    ]
}

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

/// One geofence written through `PutGeofence`
/// (`PUT /geofencing/v0/collections/{CollectionName}/geofences/{GeofenceId}`).
async fn put_geofence(
    router: &MultiServiceRouter,
    collection: &str,
    geofence: &str,
    geometry: Value,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/geofencing/v0/collections/{collection}/geofences/{geofence}"),
        Some(json!({ "Geometry": geometry })),
    )
    .await
}

/// The `fieldList` of a refusal body, as the pairs a client reads it as.
fn field_list(case: &str, body: &Value) -> Vec<(String, String)> {
    body.get("fieldList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a fieldList, got {body}"))
        .iter()
        .map(|entry| {
            let text = |member: &str| {
                entry
                    .get(member)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a fieldList entry must carry {member}, got {entry}")
                    })
                    .to_string()
            };
            (text("name"), text("message"))
        })
        .collect()
}

#[tokio::test]
async fn a_refused_geometry_names_the_input_member_in_field_list_and_keeps_the_index_in_the_message()
{
    let router = emulator();
    create_collection(&router, "fences").await;

    for (index, expected) in field_name_cases().into_iter().enumerate() {
        let refused = put_geofence(
            &router,
            "fences",
            &format!("field-name-{index}"),
            (expected.geometry)(),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {:?}: a refused geometry must be a 400, got {} {:?}",
            expected.case, refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "ValidationException",
            "case {:?}: expected ValidationException, got {:?}",
            expected.case,
            refused.body
        );
        let body = refused.json();
        assert_eq!(
            body.get("reason").and_then(Value::as_str),
            Some("FieldValidationFailed"),
            "case {:?}: expected reason FieldValidationFailed, got {body}",
            expected.case
        );
        assert_eq!(
            refused.message(),
            expected.message,
            "case {:?}: expected message {:?}, got {:?}",
            expected.case,
            expected.message,
            refused.message()
        );
        assert_eq!(
            field_list(expected.case, &body),
            vec![(
                expected.name.to_string(),
                expected.message.to_string()
            )],
            "case {:?}: expected fieldList [({:?}, {:?})], got {body}",
            expected.case,
            expected.name,
            expected.message
        );
    }
}

#[tokio::test]
async fn no_field_list_name_carries_a_list_subscript_and_the_quoted_subject_belongs_to_it() {
    let router = emulator();
    create_collection(&router, "fences").await;

    for (index, expected) in field_name_cases().into_iter().enumerate() {
        let refused = put_geofence(
            &router,
            "fences",
            &format!("subscript-{index}"),
            (expected.geometry)(),
        )
        .await;
        let body = refused.json();
        let entries = field_list(expected.case, &body);
        for (name, message) in &entries {
            // A field NAME is a member name: a subscript names an element of one.
            assert!(
                !name.contains('[') && !name.contains(']'),
                "case {:?}: a fieldList name must carry no list subscript, got {name:?}",
                expected.case
            );
            if !expected.quoted.is_empty() {
                // The sentence's subject reaches down to the element that was
                // rejected, so it starts at the member the entry is reported under.
                assert!(
                    message.contains(&format!("'{}'", expected.quoted)),
                    "case {:?}: the entry's message must quote {:?}, got {message:?}",
                    expected.case,
                    expected.quoted
                );
                assert!(
                    expected.quoted.starts_with(name.as_str()),
                    "case {:?}: the quoted subject {:?} must sit under the named member {name:?}",
                    expected.case,
                    expected.quoted
                );
            }
        }
        assert_eq!(
            entries.len(),
            1,
            "case {:?}: one rejected member is one entry, got {entries:?}",
            expected.case
        );
    }
}
