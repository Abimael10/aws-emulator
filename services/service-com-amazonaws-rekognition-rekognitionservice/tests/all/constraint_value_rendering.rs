// SPDX-License-Identifier: CC0-1.0

//! The VALUE a constraint-violation clause quotes back, end to end over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.<Operation>`), dispatched
//! through the same multi-service router the production emulator mounts.
//!
//! THE RULE UNDER TEST: a request refused at the boundary is answered `400
//! ValidationException` with the envelope `N validation error(s) detected: Value '<value>'
//! at '<member path>' failed to satisfy constraint: <requirement>`, and the value half is
//! written the way the service writes a value of the member's MODELLED NUMERIC TYPE — a
//! member the model types `float` carries a fractional part or an exponent (`101.0`, never
//! `101`; `-1.0E-6`, never `-0.000001`), while an INTEGRAL member's value, the member path
//! and the modelled bound are written exactly as the model declares them.
//!
//! The tables span what the rule generalises over: where the member sits (an operation
//! input's own member, one, two and three structures deep), which operation carries it,
//! which bound it broke (`0..100`, `50..100`, `0..1`), the submitted value's sign, whether
//! it is whole or fractional, its magnitude (inside the plain-decimal band and past its
//! upper end, far below it), and the member's modelled type — an integral member and a
//! string member are refused in the framework's own words.

use serde_json::{json, Value};

// The router harness, the dispatch helper and the reduced reply come from the
// collection-lifecycle suite rather than being built a second time here.
use crate::face_collections::{bytes_image, call, harness, one_face_png, Reply};

/// The published bounds of the members under test, read off the model:
/// `Percent` is `float, range 0..100`, `SegmentConfidence` `float, range 50..100`,
/// `MaxPixelThreshold` and `BoundingBoxHeight` `float, range 0..1`, and `MaxLabels`
/// (`UInteger`) `integer, range min 0`
/// (`smithy_query(query="shape",
/// shape="Percent,SegmentConfidence,MaxPixelThreshold,BoundingBoxHeight,UInteger")`).
const PERCENT_MAX: f64 = 100.0;
const SEGMENT_CONFIDENCE_MIN: f64 = 50.0;
const UNIT_MAX: f64 = 1.0;

/// A `FaceId` of the modelled form
/// (`pattern ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`).
const A_FACE_ID: &str = "deadbeef-0000-4000-8000-0123456789ab";

/// A `RoleArn` of the modelled form
/// (`pattern ^arn:aws:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
const A_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/rekognition-stream";

/// One row: the operation called, the body it is called with, and the clause the answer
/// must carry.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    clause: String,
}

/// The envelope one clause goes in.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// A clause that quotes the submitted value.
fn clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement a value above a `@range` maximum broke.
fn at_most(bound: &str) -> String {
    format!("Member must have value less than or equal to {bound}")
}

/// The requirement a value below a `@range` minimum broke.
fn at_least(bound: &str) -> String {
    format!("Member must have value greater than or equal to {bound}")
}

/// An `Image` the model admits without carrying any bytes, so every row below is refused
/// for the member it is about and nothing else: `Image`'s own members are optional.
fn empty_image() -> Value {
    json!({})
}

/// The rows about members the model types `float`: each quotes its value with a fractional
/// part or an exponent.
fn float_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a whole value one past the top of an input's own Percent member",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MinConfidence": PERCENT_MAX + 1.0 }),
            clause: clause("101.0", "minConfidence", &at_most("100")),
        },
        Row {
            case: "a fractional value just past the top of the same member",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MinConfidence": 100.5 }),
            clause: clause("100.5", "minConfidence", &at_most("100")),
        },
        Row {
            case: "a whole negative value below the bottom of the same member",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MinConfidence": -1.0 }),
            clause: clause("-1.0", "minConfidence", &at_least("0")),
        },
        Row {
            case: "a value far below the plain-decimal band, below the same bottom",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MinConfidence": -0.000001 }),
            clause: clause("-1.0E-6", "minConfidence", &at_least("0")),
        },
        Row {
            case: "a value at the top of the plain-decimal band, past the same top",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MinConfidence": 1e7 }),
            clause: clause("1.0E7", "minConfidence", &at_most("100")),
        },
        Row {
            case: "a threshold member of another operation",
            operation: "CompareFaces",
            body: json!({
                "SourceImage": empty_image(),
                "TargetImage": empty_image(),
                "SimilarityThreshold": 120.0,
            }),
            clause: clause("120.0", "similarityThreshold", &at_most("100")),
        },
        Row {
            case: "a threshold member of a collection search",
            operation: "SearchFaces",
            body: json!({
                "CollectionId": "a-collection",
                "FaceId": A_FACE_ID,
                "FaceMatchThreshold": 100.25,
            }),
            clause: clause("100.25", "faceMatchThreshold", &at_most("100")),
        },
        Row {
            case: "a threshold member of a video job",
            operation: "StartLabelDetection",
            body: json!({ "Video": json!({}), "MinConfidence": 101.0 }),
            clause: clause("101.0", "minConfidence", &at_most("100")),
        },
        Row {
            case: "a member one structure deep",
            operation: "DetectProtectiveEquipment",
            body: json!({
                "Image": empty_image(),
                "SummarizationAttributes": {
                    "MinConfidence": 101.0,
                    "RequiredEquipmentTypes": ["FACE_COVER"],
                },
            }),
            clause: clause("101.0", "summarizationAttributes.minConfidence", &at_most("100")),
        },
        Row {
            case: "a member two structures deep, under a 0..1 bound",
            operation: "DetectText",
            body: json!({
                "Image": empty_image(),
                "Filters": { "WordFilter": { "MinBoundingBoxHeight": 1.5 } },
            }),
            clause: clause(
                "1.5",
                "filters.wordFilter.minBoundingBoxHeight",
                &at_most("1"),
            ),
        },
        Row {
            case: "a member two structures deep, under a 50..100 bound",
            operation: "StartSegmentDetection",
            body: json!({
                "Video": json!({}),
                "SegmentTypes": ["SHOT"],
                "Filters": { "ShotFilter": { "MinSegmentConfidence": SEGMENT_CONFIDENCE_MIN - 1.0 } },
            }),
            clause: clause(
                "49.0",
                "filters.shotFilter.minSegmentConfidence",
                &at_least("50"),
            ),
        },
        Row {
            case: "a member three structures deep",
            operation: "StartSegmentDetection",
            body: json!({
                "Video": json!({}),
                "SegmentTypes": ["TECHNICAL_CUE"],
                "Filters": {
                    "TechnicalCueFilter": {
                        "BlackFrame": { "MaxPixelThreshold": UNIT_MAX + 1.0 },
                    },
                },
            }),
            clause: clause(
                "2.0",
                "filters.technicalCueFilter.blackFrame.maxPixelThreshold",
                &at_most("1"),
            ),
        },
        Row {
            case: "a stream processor's label-detection floor",
            operation: "CreateStreamProcessor",
            body: json!({
                "Input": json!({}),
                "Output": json!({}),
                "Name": "a-stream-processor",
                "RoleArn": A_ROLE_ARN,
                "Settings": {
                    "ConnectedHome": { "Labels": ["PERSON"], "MinConfidence": 101.0 },
                },
            }),
            clause: clause(
                "101.0",
                "settings.connectedHome.minConfidence",
                &at_most("100"),
            ),
        },
        Row {
            case: "the same floor on the update of one",
            operation: "UpdateStreamProcessor",
            body: json!({
                "Name": "a-stream-processor",
                "SettingsForUpdate": { "ConnectedHomeForUpdate": { "MinConfidence": -2.0 } },
            }),
            clause: clause(
                "-2.0",
                "settingsForUpdate.connectedHomeForUpdate.minConfidence",
                &at_least("0"),
            ),
        },
        Row {
            case: "a media-analysis job's moderation floor",
            operation: "StartMediaAnalysisJob",
            body: json!({
                "Input": { "S3Object": json!({}) },
                "OutputConfig": { "S3Bucket": "an-output-bucket" },
                "OperationsConfig": { "DetectModerationLabels": { "MinConfidence": 100.75 } },
            }),
            clause: clause(
                "100.75",
                "operationsConfig.detectModerationLabels.minConfidence",
                &at_most("100"),
            ),
        },
    ]
}

/// The rows about members the model does NOT type `float`: each keeps the framework's own
/// rendering of its value.
fn other_type_rows() -> Vec<Row> {
    vec![
        Row {
            case: "an integral member below its bound keeps its integer rendering",
            operation: "DetectLabels",
            body: json!({ "Image": empty_image(), "MaxLabels": -1 }),
            clause: clause("-1", "maxLabels", &at_least("0")),
        },
        Row {
            case: "another integral member of another operation",
            operation: "SearchFaces",
            body: json!({
                "CollectionId": "a-collection",
                "FaceId": A_FACE_ID,
                "MaxFaces": 0,
            }),
            clause: clause("0", "maxFaces", &at_least("1")),
        },
        Row {
            case: "a string member outside its pattern quotes the string it was sent",
            operation: "SearchFaces",
            body: json!({
                "CollectionId": "a collection!",
                "FaceId": A_FACE_ID,
            }),
            // `CollectionId` is `pattern ^[a-zA-Z0-9_.\-]+$`
            // (`smithy_query(query="operation", operation="SearchFaces")`). The quoted
            // expression is the one the service publishes, without the `^`/`$` pair the
            // model wraps it in to turn a find into a whole-value match.
            clause: clause(
                "a collection!",
                "collectionId",
                r"Member must satisfy regular expression pattern: [a-zA-Z0-9_.\-]+",
            ),
        },
        Row {
            case: "a doubly wrapped pattern keeps the inner pair and loses the outer one",
            operation: "DetectCustomLabels",
            body: json!({
                "Image": empty_image(),
                "ProjectVersionArn": "arn:aws:rekognition:us-east-1:123456789012:project/p/version/v/x",
            }),
            // `ProjectVersionArn` is modelled `^(^arn:…$)$`
            // (`smithy_query(query="operation", operation="DetectCustomLabels")`): the
            // service's own expression already anchors itself, so dropping the pair the
            // model added leaves that expression with its own anchors.
            clause: clause(
                "arn:aws:rekognition:us-east-1:123456789012:project/p/version/v/x",
                "projectVersionArn",
                r"Member must satisfy regular expression pattern: (^arn:[a-z\d-]+:rekognition:[a-z\d-]+:\d{12}:project\/[a-zA-Z0-9_.\-]{1,255}\/version\/[a-zA-Z0-9_.\-]{1,255}\/[0-9]+$)",
            ),
        },
    ]
}

/// RULE: the value half of a `@range` clause is written the way the service writes a value
/// of that member's modelled numeric type — with a fractional part or an exponent for a
/// `float` member, as the model declares it for an integral one — and the status, the error
/// code, the envelope, the member path and the bound are the same for both.
#[tokio::test]
async fn a_bounded_members_clause_quotes_its_value_in_the_members_modelled_type() {
    let router = harness();
    for Row {
        case,
        operation,
        body,
        clause,
    } in float_rows().into_iter().chain(other_type_rows())
    {
        let reply: Reply = call(&router, operation, body).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a value outside its bound is refused, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            reply.body
        );
        let message = reply.string("message").unwrap_or_default();
        assert_eq!(
            message,
            one_error(&clause),
            "case {case:?}: expected {:?}, got {message:?}",
            one_error(&clause)
        );
    }
}

/// RULE applied to a request wrong in two members: each clause states its own member in its
/// own member's rendering, inside the one envelope, whose count word counts them.
///
/// The clauses are asserted by CONTAINMENT rather than as one string: the order they are
/// listed in is the order the walk visits the input's members, which is the model's
/// declaration order and not something this rule is about.
#[tokio::test]
async fn two_members_of_one_request_each_keep_their_own_rendering() {
    let router = harness();
    let reply = call(
        &router,
        "DetectLabels",
        json!({
            "Image": empty_image(),
            "MaxLabels": -1,
            "MinConfidence": PERCENT_MAX + 1.0,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 400,
        "a request wrong in two members is refused, got {} with {:?}",
        reply.status, reply.body
    );
    let message = reply.string("message").unwrap_or_default();
    assert!(
        message.starts_with("2 validation errors detected: "),
        "expected the two-error envelope, got {message:?}"
    );
    for expected in [
        clause("101.0", "minConfidence", &at_most("100")),
        clause("-1", "maxLabels", &at_least("0")),
    ] {
        assert!(
            message.contains(&expected),
            "expected the clause {expected:?}, got {message:?}"
        );
    }
}

/// RULE: a value INSIDE a float member's bound is not refused at all — the rendering above
/// is about the clause a refusal states, and changes nothing about which requests are
/// refused. Each row sits at a published bound of `Percent`, submitted with an image this
/// service can read, so the whole call must succeed.
#[tokio::test]
async fn a_value_inside_a_float_members_bound_is_not_refused() {
    let router = harness();
    let image = bytes_image(&one_face_png(7));
    for (case, confidence) in [
        ("the bottom of the Percent range", 0.0),
        ("a fractional value inside it", 55.5),
        ("the top of the Percent range", PERCENT_MAX),
    ] {
        let reply: Reply = call(
            &router,
            "DetectLabels",
            json!({ "Image": image, "MinConfidence": confidence }),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a value inside the bound is served, got {} with {:?}",
            reply.status, reply.body
        );
        assert!(
            reply.field("Labels").is_some(),
            "case {case:?}: a served request answers with the labels measured, got {:?}",
            reply.body
        );
    }
}
