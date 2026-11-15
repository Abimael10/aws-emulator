// SPDX-License-Identifier: CC0-1.0

//! The DIALECT this service refuses a request in — the one report every member's
//! constraint violation is stated in, pinned over a SET of inputs rather than over
//! the one request a recording happened to carry. Everything is driven over HTTP
//! through the same `MultiServiceRouter` the emulator boots with, so every
//! assertion is about the wire behaviour a real client sees.
//!
//! The rules these suites pin, each over the constraint CLASSES, the operations and
//! the request bindings they reach:
//!
//! 1. a clause names the member path and the requirement it broke and states NO
//!    rejected value — whatever class it is (a `@length` bound on either side, a
//!    `@range` bound, a `@pattern`, an enumerated value set), whatever binding
//!    carried the value (a URI label, a query parameter, a body member, a member of
//!    a list element) and however long the value was. A member that was not there at
//!    all is stated the same way: the member and the requirement, with no `null`
//!    standing in for the value it never carried;
//! 2. an enumerated type's value set is listed in the order the set's own hash table
//!    is walked, which is NOT the order the model declares its values in;
//! 3. a member that broke several of its requirements states them ALL, the
//!    expression it had to match first, and the envelope counts them;
//! 4. an expression written with look-around — every `CustomID` reference's is,
//!    to exclude the nil UUID — is applied like any other, so it is stated beside
//!    the member's other requirements rather than passed over;
//! 5. a requirement one of a COLLECTION's elements broke is stated at the collection
//!    MEMBER — no position and no `member` step — with the element shape's whole
//!    constraint set nested inside `Member must satisfy constraint: [ … ]`, while a
//!    member inside a STRUCTURE element keeps its own path.
//!
//! The expected strings are written out BY HAND from the model, as an independent
//! oracle: the suite calls none of the crate's message helpers, so a seam that
//! drifts into a dialect of its own cannot agree with itself.

use serde_json::{json, Value};

use crate::asset_lifecycle::{call, error_code, router};

/// A well-formed `CustomID` reference, so a row is wrong only in the member it is
/// about. Nothing in this suite stores it: every row below is refused by the
/// boundary, before any lookup.
const WELL_FORMED_ID: &str = "9b9b9b9b-1111-2222-3333-444444444444";

/// The code IoT SiteWise answers a request that broke one of the model's own rules
/// with: "The request isn't valid. This can occur if your request contains malformed
/// JSON or unsupported characters."
/// (`smithy_query(query="operation", operation="CreateAssetModel")`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The `@pattern` the model declares on `CustomID`, written out BY HAND from
/// `smithy_query(query="shape", shape="CustomID")` — less the `$` that closes it,
/// which is how this service publishes that expression.
const CUSTOM_ID_PATTERN: &str = r"^(?!00000000-0000-0000-0000-000000000000)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$|^externalId:[a-zA-Z0-9_][a-zA-Z_\-0-9.:]*[a-zA-Z0-9_]+";

/// The `@pattern` the model declares on `ExternalId`, verbatim
/// (`smithy_query(query="shape", shape="ExternalId")`): this service publishes it as
/// the model writes it, anchors and all.
const EXTERNAL_ID_PATTERN: &str = r"^[a-zA-Z0-9_][a-zA-Z_\-0-9.:]*[a-zA-Z0-9_]+$";

/// The `@pattern` the model declares on `AssetModelVersionFilter`, verbatim
/// (`smithy_query(query="shape", shape="AssetModelVersionFilter")`).
const VERSION_FILTER_PATTERN: &str = "^(LATEST|ACTIVE)$";

/// The `@pattern` the model declares on `ClientToken`, verbatim
/// (`smithy_query(query="shape", shape="ClientToken")`).
const CLIENT_TOKEN_PATTERN: &str = r"^\S{36,64}$";

/// The `@pattern` the model declares on `Description`, verbatim
/// (`smithy_query(query="shape", shape="Description")`): one character or more,
/// none of them a control character.
const DESCRIPTION_PATTERN: &str = r"^[^\u0000-\u001F\u007F]+$";

/// `PropertyDataType`'s values as a refusal LISTS them.
///
/// The model declares `STRING, INTEGER, DOUBLE, BOOLEAN, STRUCT, VIDEO, ANNOTATION,
/// JSON` (`smithy_query(query="shape", shape="PropertyDataType")`); the listing walks
/// the value set's own hash table, so a value is stated from the bucket its text
/// hashes to (the 31-polynomial over its code units, taken at the table's width of
/// 32) — `JSON` and `BOOLEAN` share bucket 8, `ANNOTATION` 15, `DOUBLE` and `STRING`
/// 17, `STRUCT` 21, `VIDEO` 27, `INTEGER` 30 — and two values sharing a bucket are
/// read from the head each was pushed onto, so the later-declared of the pair comes
/// first. Written out by hand here as the oracle.
const PROPERTY_DATA_TYPE_SET: &str =
    "JSON, BOOLEAN, ANNOTATION, DOUBLE, STRING, STRUCT, VIDEO, INTEGER";

/// The same for `AssetModelType`, declared `ASSET_MODEL, COMPONENT_MODEL, INTERFACE`
/// (`smithy_query(query="shape", shape="AssetModelType")`): buckets 26, 7 and 25, so
/// the listing runs `COMPONENT_MODEL, INTERFACE, ASSET_MODEL`. A second enumerated
/// type, so rule 2 is pinned on a type no recording of this issue cited.
const ASSET_MODEL_TYPE_SET: &str = "COMPONENT_MODEL, INTERFACE, ASSET_MODEL";

/// The requirement a clause stated at a COLLECTION member nests: the element shape's
/// whole declared constraint set, written out by hand.
fn element_set(requirements: &str) -> String {
    format!("Member must satisfy constraint: [{requirements}]")
}

/// One clause of the report, as AWS words it: the member path and the requirement,
/// and nothing of the value the caller sent.
fn clause(member_path: &str, requirement: &str) -> String {
    format!("Value at '{member_path}' failed to satisfy constraint: {requirement}")
}

/// The clause a member that was not there at all is stated with: the same form every
/// other clause takes, with no `null` standing in for the value it never carried.
fn absent_clause(member_path: &str) -> String {
    clause(member_path, "Member must not be null")
}

/// The whole report: the envelope counts the clauses and joins them with `; `.
fn report(clauses: &[String]) -> String {
    match clauses {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// The four requirement sentences the classes below state, written out by hand.
fn at_least(bound: usize) -> String {
    format!("Member must have length greater than or equal to {bound}")
}

fn at_most(bound: usize) -> String {
    format!("Member must have length less than or equal to {bound}")
}

fn value_at_least(bound: usize) -> String {
    format!("Member must have value greater than or equal to {bound}")
}

fn value_at_most(bound: usize) -> String {
    format!("Member must have value less than or equal to {bound}")
}

fn must_match(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

fn one_of(values: &str) -> String {
    format!("Member must satisfy enum value set: [{values}]")
}

/// One row: the case, the request, and the whole report it must be refused with.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    message: String,
}

/// A `CreateAssetModel` body carrying the one required member plus `members`.
fn create_body(members: Vec<(&str, Value)>) -> Value {
    let mut body = json!({"assetModelName": "Windfarm"});
    for (member, value) in members {
        body[member] = value;
    }
    body
}

/// A property definition the model accepts, except for the `dataType` given.
fn property(name: &str, data_type: &str) -> Value {
    json!({"name": name, "dataType": data_type, "type": {"measurement": {}}})
}

/// Every row of the dialect table: one per constraint class, per binding and per
/// combination of requirements one member can break at once.
fn rows() -> Vec<Row> {
    vec![
        // --- a `@range`, on a query parameter, at each published end ---
        Row {
            case: "maxResults under its published range",
            method: "GET",
            uri: "/asset-models?maxResults=0".to_string(),
            body: None,
            message: report(&[clause("maxResults", &value_at_least(1))]),
        },
        Row {
            case: "maxResults over its published range",
            method: "GET",
            uri: "/asset-models?maxResults=251".to_string(),
            body: None,
            message: report(&[clause("maxResults", &value_at_most(250))]),
        },
        // --- a `@pattern` the default engine compiles, on a query parameter ---
        Row {
            case: "an assetModelVersion outside its published expression",
            method: "GET",
            uri: format!("/asset-models/{WELL_FORMED_ID}?assetModelVersion=active"),
            body: None,
            message: report(&[clause(
                "assetModelVersion",
                &must_match(VERSION_FILTER_PATTERN),
            )]),
        },
        // --- an enumerated value set, on a body member and on a list element's ---
        Row {
            case: "an assetModelType outside its value set",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![("assetModelType", json!("COMPONENT"))])),
            message: report(&[clause(
                "assetModelType",
                &one_of(ASSET_MODEL_TYPE_SET),
            )]),
        },
        Row {
            case: "a property dataType outside its value set",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![(
                "assetModelProperties",
                json!([property("Temperature", "double")]),
            )])),
            message: report(&[clause(
                "assetModelProperties.1.member.dataType",
                &one_of(PROPERTY_DATA_TYPE_SET),
            )]),
        },
        Row {
            // The position a clause names is 1-based, and it is the element's own:
            // the SECOND definition is the second element.
            case: "a property dataType outside its value set on the second definition",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![(
                "assetModelProperties",
                json!([
                    property("Temperature", "DOUBLE"),
                    property("Pressure", "integer"),
                ]),
            )])),
            message: report(&[clause(
                "assetModelProperties.2.member.dataType",
                &one_of(PROPERTY_DATA_TYPE_SET),
            )]),
        },
        // --- a `@length`, on a body member, at each published end, with a value
        // --- the member's own expression admits, so the length is all that broke
        Row {
            case: "an assetModelName over its published length",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(json!({"assetModelName": "W".repeat(257)})),
            message: report(&[clause("assetModelName", &at_most(256))]),
        },
        Row {
            case: "an assetModelName far over its published length, so its value is elided",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(json!({"assetModelName": "W".repeat(900)})),
            message: report(&[clause("assetModelName", &at_most(256))]),
        },
        Row {
            // The empty string breaks BOTH of this member's requirements: its
            // expression admits one character or more, so the report states both.
            case: "an assetModelDescription under its published length",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![("assetModelDescription", json!(""))])),
            message: report(&[
                clause("assetModelDescription", &must_match(DESCRIPTION_PATTERN)),
                clause("assetModelDescription", &at_least(1)),
            ]),
        },
        // --- several requirements of ONE member, expression first ---
        Row {
            case: "an assetModelExternalId under its length and outside its expression",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![("assetModelExternalId", json!("_"))])),
            message: report(&[
                clause("assetModelExternalId", &must_match(EXTERNAL_ID_PATTERN)),
                clause("assetModelExternalId", &at_least(2)),
            ]),
        },
        Row {
            case: "a clientToken under its length and outside its expression",
            method: "POST",
            uri: format!("/assets/{WELL_FORMED_ID}/associate"),
            body: Some(json!({
                "hierarchyId": WELL_FORMED_ID,
                "childAssetId": WELL_FORMED_ID,
                "clientToken": "a b",
            })),
            message: report(&[
                clause("clientToken", &must_match(CLIENT_TOKEN_PATTERN)),
                clause("clientToken", &at_least(36)),
            ]),
        },
        // --- the same, for the expression written with look-around: a reference
        // --- that is both too short and no reference at all
        Row {
            case: "a hierarchyId under its length and outside its reference form",
            method: "POST",
            uri: format!("/assets/{WELL_FORMED_ID}/associate"),
            body: Some(json!({"hierarchyId": "Turbines", "childAssetId": WELL_FORMED_ID})),
            message: report(&[
                clause("hierarchyId", &must_match(CUSTOM_ID_PATTERN)),
                clause("hierarchyId", &at_least(13)),
            ]),
        },
        Row {
            case: "a childAssetId over its length and outside its reference form",
            method: "POST",
            uri: format!("/assets/{WELL_FORMED_ID}/associate"),
            body: Some(json!({
                "hierarchyId": WELL_FORMED_ID,
                "childAssetId": "N".repeat(140),
            })),
            message: report(&[
                clause("childAssetId", &must_match(CUSTOM_ID_PATTERN)),
                clause("childAssetId", &at_most(139)),
            ]),
        },
        // --- a requirement a COLLECTION's elements broke, stated at the collection
        Row {
            case: "an assetModelTypes element outside its value set",
            method: "GET",
            uri: "/asset-models?assetModelTypes=COMPONENT".to_string(),
            body: None,
            message: report(&[clause(
                "assetModelTypes",
                &element_set(&one_of(ASSET_MODEL_TYPE_SET)),
            )]),
        },
        Row {
            // Two offending elements of ONE collection are ONE clause: the member is
            // named once, however many of its elements were refused.
            case: "two assetModelTypes elements outside its value set",
            method: "GET",
            uri: "/asset-models?assetModelTypes=COMPONENT&assetModelTypes=MODEL".to_string(),
            body: None,
            message: report(&[clause(
                "assetModelTypes",
                &element_set(&one_of(ASSET_MODEL_TYPE_SET)),
            )]),
        },
        // --- a member that was not there at all, at the top level and inside a
        // --- list element, which is where the model requires most of its members
        Row {
            case: "an absent required member",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(json!({"assetModelDescription": "A farm"})),
            message: report(&[absent_clause("assetModelName")]),
        },
        Row {
            case: "an absent required member of a list element",
            method: "POST",
            uri: "/asset-models".to_string(),
            body: Some(create_body(vec![(
                "assetModelProperties",
                json!([{"name": "Temperature", "type": {"measurement": {}}}]),
            )])),
            message: report(&[absent_clause("assetModelProperties.1.member.dataType")]),
        },
    ]
}

#[tokio::test]
async fn every_constraint_class_is_refused_in_this_services_dialect() {
    let router = router();
    for row in rows() {
        let Row {
            case,
            method,
            uri,
            body,
            message: expected,
        } = row;
        let (status, answered) = call(&router, method, &uri, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            INVALID_REQUEST,
            "case {case:?}: unexpected error code in {answered}"
        );
        let stated = answered["message"].as_str().unwrap_or("");
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
        // Rule 1 restated as its own check, so a clause that grew a value half
        // some other way is caught by the row that carries that value.
        assert!(
            !stated.contains("Value '") && !stated.contains("Value null"),
            "case {case:?}: the report must quote no rejected value, got {stated:?}"
        );
    }
}

/// Rule 3 over a request that breaks TWO members at once: every member is reported,
/// in the order the walk reached them, with each member's own requirements together.
#[tokio::test]
async fn a_request_breaking_two_members_is_reported_over_both() {
    let router = router();
    let (status, answered) = call(
        &router,
        "POST",
        &format!("/assets/{WELL_FORMED_ID}/associate"),
        Some(json!({
            "hierarchyId": "Turbines",
            "childAssetId": WELL_FORMED_ID,
            "clientToken": "a b",
        })),
    )
    .await;
    assert_eq!(status, 400, "expected 400, got {status} {answered}");
    let stated = answered["message"].as_str().unwrap_or("");
    let expected = report(&[
        clause("hierarchyId", &must_match(CUSTOM_ID_PATTERN)),
        clause("hierarchyId", &at_least(13)),
        clause("clientToken", &must_match(CLIENT_TOKEN_PATTERN)),
        clause("clientToken", &at_least(36)),
    ]);
    assert_eq!(
        stated, expected,
        "expected {expected:?}, got {stated:?}"
    );
}

/// A value that satisfies every requirement of its member is not reported at all:
/// the rows above refuse the values they carry, and not the members themselves.
#[tokio::test]
async fn a_request_inside_every_bound_is_not_refused_by_the_boundary() {
    let router = router();
    let (status, answered) = call(
        &router,
        "POST",
        "/asset-models",
        Some(create_body(vec![
            ("assetModelType", json!("ASSET_MODEL")),
            ("assetModelExternalId", json!("windfarm_model")),
            ("assetModelDescription", json!("A farm")),
            (
                "assetModelProperties",
                json!([property("Temperature", "DOUBLE")]),
            ),
        ])),
    )
    .await;
    assert_eq!(
        status, 202,
        "a request inside every bound must be accepted, got {status} {answered}"
    );

    // And one the boundary passes whose reference form is still wrong is refused in
    // the same dialect by the operation that reads it: the clause the boundary
    // cannot state for a member nested inside a list element is stated there, in
    // these very words.
    let (status, answered) = call(
        &router,
        "POST",
        "/asset-models",
        Some(json!({
            "assetModelName": "Windfarm hierarchies",
            "assetModelHierarchies": [
                {"name": "turbines", "childAssetModelId": "WindTurbineModel"},
            ],
        })),
    )
    .await;
    assert_eq!(status, 400, "expected 400, got {status} {answered}");
    assert_eq!(
        error_code(&answered),
        INVALID_REQUEST,
        "unexpected error code in {answered}"
    );
    let stated = answered["message"].as_str().unwrap_or("");
    let expected = report(&[clause(
        "assetModelHierarchies.1.member.childAssetModelId",
        &must_match(CUSTOM_ID_PATTERN),
    )]);
    assert_eq!(stated, expected, "expected {expected:?}, got {stated:?}");
}
