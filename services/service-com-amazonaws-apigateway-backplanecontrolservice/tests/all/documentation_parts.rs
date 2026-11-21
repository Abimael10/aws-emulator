// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the documentation-part family: `CreateDocumentationPart`,
//! `GetDocumentationPart`, `GetDocumentationParts` and `ImportDocumentationParts`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], [`Answer`], `create_api`,
//! `string_member`, `page_items` and `assert_failure` are this crate's one wire-test
//! harness, reused from `rest_api_lifecycle`.
//!
//! The tests assert RULES over sets of inputs, not one recorded call:
//!
//! * a location's valid fields, over the WHOLE `DocumentationPartType` catalog — each
//!   type's valid members echoed as given, each documented default filled in when the
//!   request sets none, and every member the type makes invalid refused;
//! * `GetDocumentationParts`'s filters, over a table of queries against one known
//!   collection, including the conjunction of several and a query that matches nothing;
//! * pagination, over a table of page sizes from one to past the collection's size;
//! * an import, over both `PutMode` values and a re-import, with the content map read
//!   back through both readers;
//! * a malformed import body, over a table of bodies that are not a documentation set,
//!   each asserted to change nothing, and a body whose entries are not all storable,
//!   which is refused in whole.

use serde_json::{json, Map, Value};

use crate::rest_api_lifecycle::{
    assert_failure, create_api, page_items, string_member, Answer, Gateway,
};

/// The modelled error codes these cases assert.
pub(crate) const BAD_REQUEST: &str = "BadRequestException";
const CONFLICT: &str = "ConflictException";
const NOT_FOUND: &str = "NotFoundException";

/// Every entity type a documentation part can target, with the location members that
/// type makes VALID.
///
/// Source: `DocumentationPartType`'s value set and each `DocumentationPartLocation`
/// member's own documentation (`smithy_query(query="shape",
/// shape="DocumentationPartLocation")`) — `path` is "a valid field for the API entity
/// types of RESOURCE, METHOD, PATH_PARAMETER, QUERY_PARAMETER, REQUEST_HEADER,
/// REQUEST_BODY, RESPONSE, RESPONSE_HEADER, and RESPONSE_BODY", `method` for the same
/// list without RESOURCE, `statusCode` for "RESPONSE, RESPONSE_HEADER, and
/// RESPONSE_BODY", and `name` for "AUTHORIZER, MODEL, PATH_PARAMETER, QUERY_PARAMETER,
/// REQUEST_HEADER, REQUEST_BODY and RESPONSE_HEADER".
const LOCATION_FIELDS: &[(&str, &[&str])] = &[
    ("API", &[]),
    ("AUTHORIZER", &["name"]),
    ("MODEL", &["name"]),
    ("RESOURCE", &["path"]),
    ("METHOD", &["path", "method"]),
    ("PATH_PARAMETER", &["path", "method", "name"]),
    ("QUERY_PARAMETER", &["path", "method", "name"]),
    ("REQUEST_HEADER", &["path", "method", "name"]),
    ("REQUEST_BODY", &["path", "method", "name"]),
    ("RESPONSE", &["path", "method", "statusCode"]),
    ("RESPONSE_HEADER", &["path", "method", "statusCode", "name"]),
    ("RESPONSE_BODY", &["path", "method", "statusCode"]),
];

/// The default each location member documents for a type that makes it valid: `path`
/// "The default value is / for the root resource", `method` "The default value is * for
/// any method", `statusCode` "The default value is * for any status code". `name`
/// documents none.
const LOCATION_DEFAULTS: &[(&str, &str)] = &[("path", "/"), ("method", "*"), ("statusCode", "*")];

/// One explicit value per location member, for the cases that set every valid member
/// rather than leaving it to its default. `statusCode` is a value its pattern
/// (`^([1-5]\d\d|\*|\s*)$`) admits.
const EXPLICIT_MEMBERS: &[(&str, &str)] = &[
    ("path", "/pets"),
    ("method", "GET"),
    ("statusCode", "404"),
    ("name", "petId"),
];

/// A content map, as the JSON string `DocumentationPart$properties` documents ("The map
/// must be encoded as a JSON string").
pub(crate) fn content_map(description: &str) -> String {
    json!({ "description": description }).to_string()
}

/// The text a content map an import body declared as a JSON OBJECT is stored and reported
/// as.
///
/// An entry's content map is a JSON object in the document, and `properties` is "a content
/// map ... encoded as a JSON string": the service writes that object out itself — one
/// member per line, indented by two spaces, with a space either side of the name/value
/// separator — so a read reports the service's own rendering rather than the bytes the body
/// carried. Spelled out here as the expected text, for the one-member `description` map
/// these cases import, rather than computed the way the service computes it.
pub(crate) fn imported_content_map(description: &str) -> String {
    format!("{{\n  \"description\" : {}\n}}", json!(description))
}

/// The location a request that sets only `type` must be STORED and reported with: every
/// member the type makes valid, at its documented default.
fn defaulted_location(entity_type: &str, valid: &[&str]) -> Value {
    let mut location = Map::new();
    location.insert("type".to_string(), json!(entity_type));
    for (member, default) in LOCATION_DEFAULTS {
        if valid.contains(member) {
            location.insert((*member).to_string(), json!(default));
        }
    }
    Value::Object(location)
}

/// The location a request that sets EVERY member the type makes valid must be stored and
/// reported with: the values as given.
fn explicit_location(entity_type: &str, valid: &[&str]) -> Value {
    let mut location = Map::new();
    location.insert("type".to_string(), json!(entity_type));
    for (member, value) in EXPLICIT_MEMBERS {
        if valid.contains(member) {
            location.insert((*member).to_string(), json!(value));
        }
    }
    Value::Object(location)
}

/// `CreateDocumentationPart` with `location` and `properties` sent as given.
pub(crate) async fn create_part(
    gateway: &Gateway,
    api: &str,
    location: Value,
    properties: &str,
) -> Answer {
    gateway
        .call(
            "POST",
            &format!("/restapis/{api}/documentation/parts"),
            Some(json!({ "location": location, "properties": properties })),
        )
        .await
}

/// `CreateDocumentationPart`, asserting the modelled 201 and returning the part's id.
pub(crate) async fn created_part_id(
    gateway: &Gateway,
    api: &str,
    location: Value,
    properties: &str,
    case: &str,
) -> String {
    let answer = create_part(gateway, api, location, properties).await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", case)
}

/// `GetDocumentationParts` with `query` appended verbatim (empty for no filter).
async fn get_parts(gateway: &Gateway, api: &str, query: &str) -> Answer {
    let uri = if query.is_empty() {
        format!("/restapis/{api}/documentation/parts")
    } else {
        format!("/restapis/{api}/documentation/parts?{query}")
    };
    gateway.call("GET", &uri, None).await
}

/// The ids `GetDocumentationParts` reports for `query`, asserting the modelled 200.
pub(crate) async fn listed_ids(
    gateway: &Gateway,
    api: &str,
    query: &str,
    case: &str,
) -> Vec<String> {
    let answer = get_parts(gateway, api, query).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    page_items(&answer.body, case)
        .iter()
        .map(|item| string_member(item, "id", case))
        .collect()
}

/// `GetDocumentationPart` for one part.
pub(crate) async fn get_part(gateway: &Gateway, api: &str, id: &str) -> Answer {
    gateway
        .call(
            "GET",
            &format!("/restapis/{api}/documentation/parts/{id}"),
            None,
        )
        .await
}

/// An import body: an OpenAPI document carrying the documentation set in API Gateway's
/// `x-amazon-apigateway-documentation` extension, each entry's content map written as the
/// JSON OBJECT an OpenAPI document writes it as.
pub(crate) fn import_body(entries: &[(Value, Value)]) -> String {
    let parts: Vec<Value> = entries
        .iter()
        .map(|(location, properties)| json!({ "location": location, "properties": properties }))
        .collect();
    json!({
        "openapi": "3.0.1",
        "info": { "title": "documentation", "version": "2026-01-01" },
        "x-amazon-apigateway-documentation": {
            "version": "1.0.0",
            "documentationParts": parts,
        },
    })
    .to_string()
}

/// `ImportDocumentationParts` with `body` sent verbatim as the payload.
pub(crate) async fn import_parts(
    gateway: &Gateway,
    api: &str,
    mode: Option<&str>,
    fail_on_warnings: bool,
    body: &str,
) -> Answer {
    let mut query: Vec<String> = Vec::new();
    if let Some(mode) = mode {
        query.push(format!("mode={mode}"));
    }
    if fail_on_warnings {
        query.push("failonwarnings=true".to_string());
    }
    let uri = if query.is_empty() {
        format!("/restapis/{api}/documentation/parts")
    } else {
        format!("/restapis/{api}/documentation/parts?{}", query.join("&"))
    };
    gateway.call_raw("PUT", &uri, body).await
}

/// The `(location, content map)` pairs `GetDocumentationParts` reports, so a case can
/// assert the documentation set an import left behind whatever ids it minted.
pub(crate) async fn listed_entities(
    gateway: &Gateway,
    api: &str,
    case: &str,
) -> Vec<(Value, Value)> {
    let answer = get_parts(gateway, api, "limit=500").await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    page_items(&answer.body, case)
        .iter()
        .map(|item| {
            let properties = string_member(item, "properties", case);
            let read: Value = serde_json::from_str(&properties).unwrap_or_else(|_| {
                panic!("case {case}: expected a JSON content map, got {properties:?}")
            });
            (item["location"].clone(), read)
        })
        .collect()
}

#[tokio::test]
async fn a_locations_valid_fields_are_defaulted_when_the_request_sets_none() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-defaults").await;

    let mut created: Vec<String> = Vec::new();
    for (entity_type, valid) in LOCATION_FIELDS {
        let case = format!("type={entity_type}, location sets only type");
        let properties = content_map(entity_type);
        let answer = create_part(&gateway, &api, json!({ "type": entity_type }), &properties).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201, got {} with {:?}",
            answer.status, answer.body
        );
        let expected = defaulted_location(entity_type, valid);
        assert_eq!(
            answer.body["location"], expected,
            "case {case}: expected location {expected:?}, got {:?}",
            answer.body["location"]
        );
        let id = string_member(&answer.body, "id", &case);

        // Round trip: the created part is read back member for member through the
        // implemented reader, not just reported by the writer.
        let read = get_part(&gateway, &api, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: GetDocumentationPart expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["location"], expected,
            "case {case}: GetDocumentationPart expected location {expected:?}, got {:?}",
            read.body["location"]
        );
        assert_eq!(
            read.body["properties"].as_str(),
            Some(properties.as_str()),
            "case {case}: expected properties {properties:?}, got {:?}",
            read.body["properties"]
        );
        assert_eq!(
            string_member(&read.body, "id", &case),
            id,
            "case {case}: GetDocumentationPart reported another id"
        );
        created.push(id);
    }

    // Every created part is listed. The collection's own order is the ADDRESS of the
    // entity each part documents (`collection_page_envelope` pins that order), not the
    // order the parts were created in, so what is compared here is which parts the API
    // holds.
    let mut expected = created.clone();
    expected.sort();
    let mut listed = listed_ids(&gateway, &api, "", "GetDocumentationParts, no filter").await;
    listed.sort();
    assert_eq!(
        listed, expected,
        "case GetDocumentationParts, no filter: expected {expected:?}, got {listed:?}"
    );
}

#[tokio::test]
async fn a_locations_valid_fields_are_stored_as_given_when_the_request_sets_them() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-explicit").await;

    for (entity_type, valid) in LOCATION_FIELDS {
        let case = format!("type={entity_type}, location sets every valid field");
        let expected = explicit_location(entity_type, valid);
        let id = created_part_id(
            &gateway,
            &api,
            expected.clone(),
            &content_map(entity_type),
            &case,
        )
        .await;
        let read = get_part(&gateway, &api, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["location"], expected,
            "case {case}: expected location {expected:?}, got {:?}",
            read.body["location"]
        );
    }
}

#[tokio::test]
async fn a_location_field_the_entity_type_makes_invalid_is_refused() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-invalid-fields").await;

    for (entity_type, valid) in LOCATION_FIELDS {
        for (member, value) in EXPLICIT_MEMBERS {
            if valid.contains(member) {
                continue;
            }
            let case = format!("type={entity_type}, location sets {member}");
            let mut location = Map::new();
            location.insert("type".to_string(), json!(entity_type));
            location.insert((*member).to_string(), json!(value));
            let answer = create_part(
                &gateway,
                &api,
                Value::Object(location),
                &content_map("invalid field"),
            )
            .await;
            assert_failure(&answer, 400, BAD_REQUEST, &case);
        }
    }

    // Nothing was stored by any of those refusals.
    let listed = listed_ids(&gateway, &api, "", "after invalid locations").await;
    assert!(
        listed.is_empty(),
        "case after invalid locations: expected no stored part, got {listed:?}"
    );
}

#[tokio::test]
async fn a_properties_that_is_not_a_json_content_map_is_refused() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-properties").await;

    // Each row is a `properties` that is not "a content map ... encoded as a JSON
    // string", differing along the JSON form it carries instead.
    let refused: &[&str] = &["", "not json", "{", "null", "42", "\"text\"", "[1, 2]"];
    for properties in refused {
        let case = format!("properties={properties:?}");
        let answer = create_part(
            &gateway,
            &api,
            json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
            properties,
        )
        .await;
        assert_failure(&answer, 400, BAD_REQUEST, &case);
    }

    // Each row IS a content map, differing along what the map holds.
    let accepted: &[&str] = &[
        "{}",
        "{\"description\": \"a pet\"}",
        "{\"description\": \"a pet\", \"summary\": \"pets\"}",
        "{\"schema\": {\"type\": \"object\"}, \"count\": 3}",
    ];
    for (index, properties) in accepted.iter().enumerate() {
        let case = format!("properties={properties:?}");
        let id = created_part_id(
            &gateway,
            &api,
            json!({ "type": "QUERY_PARAMETER", "path": "/pets", "method": "GET", "name": format!("q{index}") }),
            properties,
            &case,
        )
        .await;
        let read = get_part(&gateway, &api, &id).await;
        assert_eq!(
            read.body["properties"].as_str(),
            Some(*properties),
            "case {case}: expected the content map stored verbatim, got {:?}",
            read.body["properties"]
        );
    }
}

#[tokio::test]
async fn one_documentation_part_per_api_entity_and_a_second_is_a_conflict() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-conflict").await;

    // Each row is a location and the location that names the SAME entity through the
    // documented defaults, so the second create is a conflict however it is spelled.
    let rows: &[(&str, Value, Value)] = &[
        (
            "a repeated location",
            json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
            json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
        ),
        (
            "a defaulted method against the explicit wildcard",
            json!({ "type": "METHOD", "path": "/owners" }),
            json!({ "type": "METHOD", "path": "/owners", "method": "*" }),
        ),
        (
            "a defaulted path against the explicit root",
            json!({ "type": "RESOURCE" }),
            json!({ "type": "RESOURCE", "path": "/" }),
        ),
        (
            "a defaulted status code against the explicit wildcard",
            json!({ "type": "RESPONSE", "path": "/pets", "method": "GET" }),
            json!({ "type": "RESPONSE", "path": "/pets", "method": "GET", "statusCode": "*" }),
        ),
    ];
    for (case, first, again) in rows {
        created_part_id(&gateway, &api, first.clone(), &content_map(case), case).await;
        let answer = create_part(&gateway, &api, again.clone(), &content_map(case)).await;
        assert_failure(&answer, 409, CONFLICT, case);
    }

    // A location that names a DIFFERENT entity of the same type is not a conflict.
    let case = "another method of the same path";
    created_part_id(
        &gateway,
        &api,
        json!({ "type": "METHOD", "path": "/pets", "method": "POST" }),
        &content_map(case),
        case,
    )
    .await;
    let listed = listed_ids(&gateway, &api, "", "after the conflict cases").await;
    assert_eq!(
        listed.len(),
        rows.len() + 1,
        "case after the conflict cases: expected {} stored parts, got {listed:?}",
        rows.len() + 1
    );
}

#[tokio::test]
async fn get_documentation_parts_narrows_the_collection_by_every_filter() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-filters").await;

    // One known collection, spanning the entity types, paths, names and content states
    // the filters select on.
    let collection: &[(Value, &str)] = &[
        (
            json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
            "list pets",
        ),
        (
            json!({ "type": "METHOD", "path": "/pets", "method": "POST" }),
            "create a pet",
        ),
        (
            json!({ "type": "QUERY_PARAMETER", "path": "/pets", "method": "GET", "name": "page" }),
            "the page to read",
        ),
        // An entity with no content: the empty content map is what
        // `locationStatus=UNDOCUMENTED` reports.
        (json!({ "type": "MODEL", "name": "Pet" }), ""),
        (json!({ "type": "RESOURCE", "path": "/owners" }), "owners"),
    ];
    let mut ids: Vec<String> = Vec::new();
    for (location, description) in collection {
        let case = format!("create {location:?}");
        let properties = if description.is_empty() {
            "{}".to_string()
        } else {
            content_map(description)
        };
        ids.push(created_part_id(&gateway, &api, location.clone(), &properties, &case).await);
    }

    // Each row is a query and the indices of the collection it must report, in the order
    // the collection reports them — the order of the ADDRESSES of the API entities
    // documented, which is not the order these rows were created in: one filter, each
    // filter's other value, a conjunction of filters, and a query that matches nothing.
    let rows: &[(&str, &[usize])] = &[
        ("", &[0, 1, 3, 2, 4]),
        ("type=METHOD", &[0, 1]),
        ("type=QUERY_PARAMETER", &[2]),
        ("type=RESOURCE", &[4]),
        ("type=AUTHORIZER", &[]),
        ("name=page", &[2]),
        ("name=Pet", &[3]),
        ("name=absent", &[]),
        ("path=/pets", &[0, 1, 2]),
        ("path=/owners", &[4]),
        ("path=/nothing", &[]),
        ("locationStatus=DOCUMENTED", &[0, 1, 2, 4]),
        ("locationStatus=UNDOCUMENTED", &[3]),
        ("type=METHOD&path=/pets", &[0, 1]),
        ("type=METHOD&path=/pets&locationStatus=DOCUMENTED", &[0, 1]),
        ("type=METHOD&name=page", &[]),
        ("path=/pets&locationStatus=UNDOCUMENTED", &[]),
    ];
    for (query, expected) in rows {
        let case = format!("GetDocumentationParts?{query}");
        let listed = listed_ids(&gateway, &api, query, &case).await;
        let expected: Vec<String> = expected.iter().map(|index| ids[*index].clone()).collect();
        assert_eq!(
            listed, expected,
            "case {case}: expected {expected:?}, got {listed:?}"
        );
    }

    // Reading does not change what a later caller sees: the same query answers the same
    // collection.
    let whole: Vec<String> = [0, 1, 3, 2, 4].iter().map(|at| ids[*at].clone()).collect();
    let again = listed_ids(&gateway, &api, "", "GetDocumentationParts, read twice").await;
    assert_eq!(
        again, whole,
        "case GetDocumentationParts, read twice: expected {whole:?}, got {again:?}"
    );
}

#[tokio::test]
async fn get_documentation_parts_pages_through_creation_order_on_position_and_limit() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-pagination").await;

    let mut ids: Vec<String> = Vec::new();
    for index in 0..7 {
        let case = format!("create q{index}");
        ids.push(
            created_part_id(
                &gateway,
                &api,
                json!({
                    "type": "QUERY_PARAMETER",
                    "path": "/items",
                    "method": "GET",
                    "name": format!("q{index}"),
                }),
                &content_map(&case),
                &case,
            )
            .await,
        );
    }

    // Each row is a page size: below, at and past the collection's size, plus the
    // documented maximum.
    for limit in [1_usize, 2, 3, 7, 8, 500] {
        let case = format!("limit={limit}");
        let mut walked: Vec<String> = Vec::new();
        let mut position: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &position {
                None => format!("limit={limit}"),
                Some(cursor) => format!("limit={limit}&position={cursor}"),
            };
            let answer = get_parts(&gateway, &api, &query).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            let page = page_items(&answer.body, &case);
            assert!(
                page.len() <= limit,
                "case {case}: expected at most {limit} items, got {}",
                page.len()
            );
            walked.extend(
                page.iter()
                    .map(|item| string_member(item, "id", &case))
                    .collect::<Vec<String>>(),
            );
            pages += 1;
            assert!(
                pages <= ids.len() + 1,
                "case {case}: pagination did not terminate after {pages} pages"
            );
            match answer.body["position"].as_str() {
                Some(cursor) => position = Some(cursor.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, ids,
            "case {case}: expected the whole collection in the order of the entities it \
             documents, got {walked:?}"
        );
    }

    // A cursor is a POINT in the order the collection reports, so one the collection
    // holds no part at — here a text that names no entity type at all, which sorts past
    // every declared one — is the end of the collection: an empty page, not a refusal.
    let answer = get_parts(&gateway, &api, "position=nosuchpart").await;
    assert_eq!(
        answer.status, 200,
        "case position=nosuchpart: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        page_items(&answer.body, "position=nosuchpart").is_empty(),
        "case position=nosuchpart: expected the empty page, got {:?}",
        answer.body
    );
    assert!(
        answer.body["position"].is_null(),
        "case position=nosuchpart: the empty page hands back no cursor, got {:?}",
        answer.body["position"]
    );

    // An unknown API is a not found on both readers.
    let answer = get_parts(&gateway, "nosuchapi0", "").await;
    assert_failure(&answer, 404, NOT_FOUND, "GetDocumentationParts, unknown api");
    let answer = get_part(&gateway, &api, "nosuchpart").await;
    assert_failure(
        &answer,
        404,
        NOT_FOUND,
        "GetDocumentationPart, unknown part",
    );
}

#[tokio::test]
async fn an_import_writes_one_part_per_entity_and_the_mode_decides_what_survives() {
    // Each row is a `mode` the re-import runs in: the documented default (no `mode` at
    // all), `merge` spelled out, and `overwrite`.
    let rows: &[(&str, Option<&str>, bool)] = &[
        ("mode absent (merge by default)", None, false),
        ("mode=merge", Some("merge"), false),
        ("mode=overwrite", Some("overwrite"), true),
    ];
    for (case, mode, overwrites) in rows {
        let gateway = Gateway::new();
        let (api, _root) = create_api(&gateway, "documentation-import").await;

        let first: &[(Value, Value)] = &[
            (json!({ "type": "API" }), json!({ "info": { "description": "the pet store" } })),
            (
                json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
                json!({ "description": "list pets" }),
            ),
            (
                json!({ "type": "QUERY_PARAMETER", "path": "/pets", "method": "GET", "name": "page" }),
                json!({ "description": "the page to read" }),
            ),
        ];
        let answer = import_parts(&gateway, &api, None, false, &import_body(first)).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: the first import expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let created = imported_ids(&answer, case);
        assert_eq!(
            created.len(),
            first.len(),
            "case {case}: expected {} imported ids, got {created:?}",
            first.len()
        );
        assert!(
            answer.body["warnings"].is_null(),
            "case {case}: expected no warnings, got {:?}",
            answer.body["warnings"]
        );

        // Every entry is stored as a part: its location completed by the same rules a
        // created part's is — every entry here already sets every member its type makes
        // valid, so the stored location is the entry's own — and its content map as the
        // JSON text of the entry's object.
        let stored = listed_entities(&gateway, &api, case).await;
        assert_eq!(
            stored.len(),
            first.len(),
            "case {case}: expected {} stored parts, got {stored:?}",
            first.len()
        );
        for (location, properties) in first {
            assert!(
                stored
                    .iter()
                    .any(|(stored_location, stored_properties)| stored_location == location
                        && stored_properties == properties),
                "case {case}: expected a part at {location:?} with {properties:?}, got {stored:?}"
            );
        }

        // Each imported id reads back through `GetDocumentationPart`.
        for id in &created {
            let read = get_part(&gateway, &api, id).await;
            assert_eq!(
                read.status, 200,
                "case {case}: GetDocumentationPart({id}) expected 200, got {} with {:?}",
                read.status, read.body
            );
        }

        // The re-import changes one entity's content map and documents one more entity.
        let again: &[(Value, Value)] = &[
            (
                json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
                json!({ "description": "list every pet" }),
            ),
            (
                json!({ "type": "RESPONSE", "path": "/pets", "method": "GET", "statusCode": "200" }),
                json!({ "description": "the pets" }),
            ),
        ];
        let answer = import_parts(&gateway, &api, *mode, false, &import_body(again)).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: the re-import expected 200, got {} with {:?}",
            answer.status, answer.body
        );

        let stored = listed_entities(&gateway, &api, case).await;
        let expected_count = if *overwrites { again.len() } else { 4 };
        assert_eq!(
            stored.len(),
            expected_count,
            "case {case}: expected {expected_count} stored parts, got {stored:?}"
        );
        for (location, properties) in again {
            assert!(
                stored
                    .iter()
                    .any(|(stored_location, stored_properties)| stored_location == location
                        && stored_properties == properties),
                "case {case}: expected a part at {location:?} with {properties:?}, got {stored:?}"
            );
        }
        // What the first import documented and the second did not: kept by `merge`,
        // replaced by `overwrite`.
        let kept = stored
            .iter()
            .any(|(location, _)| location["type"] == json!("QUERY_PARAMETER"));
        assert_eq!(
            kept, !*overwrites,
            "case {case}: expected the earlier part to be kept={}, got {stored:?}",
            !*overwrites
        );

        // An entry lands on the part that already documents its entity, so that part keeps
        // its identifier whichever mode the import ran in: a client holding the id still
        // reads the entity's documentation, now carrying the re-imported content map.
        let first_method_id = created[1].clone();
        let read = get_part(&gateway, &api, &first_method_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the re-imported part to keep its id, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["properties"].as_str(),
            Some(imported_content_map("list every pet").as_str()),
            "case {case}: expected the re-imported content map, got {:?}",
            read.body["properties"]
        );
        // The part whose entity the second body does NOT document: `overwrite` drops it,
        // so its id names nothing; `merge` keeps it, under its own id.
        let query_parameter_id = created[2].clone();
        let read = get_part(&gateway, &api, &query_parameter_id).await;
        if *overwrites {
            assert_failure(&read, 404, NOT_FOUND, case);
        } else {
            assert_eq!(
                read.status, 200,
                "case {case}: expected the part the body left alone to be kept, got {} with {:?}",
                read.status, read.body
            );
        }
    }
}

/// The `ids` an accepted import answered with, asserting the modelled 200.
fn imported_ids(answer: &Answer, case: &str) -> Vec<String> {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["ids"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected an ids array, got {:?}", answer.body))
        .iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| panic!("case {case}: expected a string id, got {id:?}"))
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn every_entity_type_a_part_documents_is_kept_in_the_one_collection_and_filtered_from_it() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-location-types").await;

    // One part per entity type a documentation part can target — the whole
    // `DocumentationPartType` catalog ([`LOCATION_FIELDS`]), each at a location that sets
    // every member its type makes valid — so the rule is asserted over the entity types
    // the catalog names rather than over the one a report cites.
    let mut created: Vec<(&str, String, Value)> = Vec::new();
    for (entity_type, valid) in LOCATION_FIELDS {
        let case = format!("type={entity_type}");
        let location = explicit_location(entity_type, valid);
        let id =
            created_part_id(&gateway, &api, location.clone(), &content_map(&case), &case).await;
        created.push((entity_type, id, location));
    }

    // RETAINED: the API's one documentation set holds every part that was created, each
    // under the location it was created at and with the content map it carries — a part
    // documenting a method beside the ones documenting the API, a resource, a model and a
    // response.
    let listed = listed_entities(&gateway, &api, "the whole collection").await;
    assert_eq!(
        listed.len(),
        created.len(),
        "case the whole collection: expected {} parts, got {listed:?}",
        created.len()
    );
    for (entity_type, _, location) in &created {
        let case = format!("type={entity_type}");
        let expected = (location.clone(), json!({ "description": case }));
        assert!(
            listed.contains(&expected),
            "case {case}: expected the collection to hold {expected:?}, got {listed:?}"
        );
    }

    // SELECTED: a listing narrowed to one entity type reports the part at that type and
    // excludes the parts of every other type the API documents.
    for (entity_type, id, _) in &created {
        let case = format!("filter type={entity_type}");
        let selected = listed_ids(&gateway, &api, &format!("type={entity_type}"), &case).await;
        assert_eq!(
            selected,
            vec![id.clone()],
            "case {case}: expected only the part at that type, got {selected:?}"
        );
    }

    // And a filter on another location member selects by what the STORED location carries,
    // whatever entity type names it: every part whose type makes the member valid carries
    // the value these cases set ([`EXPLICIT_MEMBERS`]), and no other part does. Compared as
    // sets, because the collection's own order is `collection_page_envelope`'s subject.
    for member in ["path", "name"] {
        let value = EXPLICIT_MEMBERS
            .iter()
            .find(|(declared, _)| *declared == member)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| panic!("no explicit value for location.{member}"));
        let case = format!("filter {member}={value}");
        let query = match member {
            // `nameQuery` is bound to the `name` query parameter, `path` to `path`.
            "name" => format!("name={value}"),
            other => format!("{other}={value}"),
        };
        let mut selected = listed_ids(&gateway, &api, &query, &case).await;
        selected.sort();
        let mut expected: Vec<String> = created
            .iter()
            .filter(|(_, _, location)| location.get(member) == Some(&json!(value)))
            .map(|(_, id, _)| id.clone())
            .collect();
        expected.sort();
        assert!(
            !expected.is_empty(),
            "case {case}: the collection documents no entity at that member"
        );
        assert_eq!(
            selected, expected,
            "case {case}: expected {expected:?}, got {selected:?}"
        );
    }

    // A read leaves the collection as it was: the unfiltered listing still holds every
    // part the filtered reads selected from.
    let again = listed_entities(&gateway, &api, "the whole collection, read again").await;
    assert_eq!(
        again, listed,
        "case the whole collection, read again: expected {listed:?}, got {again:?}"
    );
}

#[tokio::test]
async fn a_merge_import_lands_on_the_part_a_create_made_for_that_entity() {
    // Each row is the entity a create documents FIRST and an import then re-documents,
    // beside an entity nothing documents yet — the dimension the identity rule generalises
    // over is the location form: no member at all, a path, a path and a verb, a status
    // code, a name.
    let rows: &[(&str, Value, Value)] = &[
        (
            "the api itself",
            json!({ "type": "API" }),
            json!({ "type": "MODEL", "name": "Pet" }),
        ),
        (
            "a resource",
            json!({ "type": "RESOURCE", "path": "/pets" }),
            json!({ "type": "RESOURCE", "path": "/owners" }),
        ),
        (
            "a method",
            json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
            json!({ "type": "METHOD", "path": "/pets", "method": "POST" }),
        ),
        (
            "a response",
            json!({ "type": "RESPONSE", "path": "/pets", "method": "GET", "statusCode": "200" }),
            json!({ "type": "RESPONSE", "path": "/pets", "method": "GET", "statusCode": "404" }),
        ),
        (
            "a named entity",
            json!({ "type": "MODEL", "name": "Pet" }),
            json!({ "type": "AUTHORIZER", "name": "petAuth" }),
        ),
    ];
    // And each row runs in every mode that KEEPS what the body does not document: the
    // documented default and `merge` spelled out.
    for (entity, documented, undocumented) in rows {
        for mode in [None, Some("merge")] {
            let case = format!("{entity} re-documented with mode {mode:?}");
            let gateway = Gateway::new();
            let (api, _root) = create_api(&gateway, "documentation-merge-identity").await;

            // The part that already documents the entity, created through
            // `CreateDocumentationPart`.
            let created = created_part_id(
                &gateway,
                &api,
                documented.clone(),
                &content_map("created"),
                &case,
            )
            .await;

            // An import documenting that same entity LANDS ON that part: it reports the
            // part's own identifier and the API still documents the entity once.
            let answer = import_parts(
                &gateway,
                &api,
                mode,
                false,
                &import_body(&[(documented.clone(), json!({ "description": "imported" }))]),
            )
            .await;
            let reported = imported_ids(&answer, &case);
            assert_eq!(
                reported,
                vec![created.clone()],
                "case {case}: expected the import to report the part that already documents \
                 the entity, got {reported:?}"
            );
            let stored = listed_entities(&gateway, &api, &case).await;
            assert_eq!(
                stored,
                vec![(documented.clone(), json!({ "description": "imported" }))],
                "case {case}: expected one part for the entity, got {stored:?}"
            );
            // Read back through the implemented reader under the id the create minted: the
            // content map is the re-imported one, written as the service writes a map an
            // entry declared as an object.
            let read = get_part(&gateway, &api, &created).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the part to keep its id, got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.body["properties"].as_str(),
                Some(imported_content_map("imported").as_str()),
                "case {case}: expected the re-imported content map, got {:?}",
                read.body["properties"]
            );

            // An entry for an entity the API does NOT document yet is a part of its own:
            // a different identifier, one more part, and the first part untouched.
            let answer = import_parts(
                &gateway,
                &api,
                mode,
                false,
                &import_body(&[(undocumented.clone(), json!({ "description": "another" }))]),
            )
            .await;
            let minted = imported_ids(&answer, &case);
            assert_eq!(
                minted.len(),
                1,
                "case {case}: expected one identifier for the one entity documented, got \
                 {minted:?}"
            );
            assert_ne!(
                minted[0], created,
                "case {case}: expected a part of its own for an entity nothing documented, \
                 got {minted:?}"
            );
            let read = get_part(&gateway, &api, &minted[0]).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the new part to read back, got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.body["location"], *undocumented,
                "case {case}: expected the new part at {undocumented:?}, got {:?}",
                read.body["location"]
            );
            let stored = listed_entities(&gateway, &api, &case).await;
            assert_eq!(
                stored.len(),
                2,
                "case {case}: expected the API to document both entities, got {stored:?}"
            );
            assert!(
                stored.contains(&(documented.clone(), json!({ "description": "imported" }))),
                "case {case}: expected the first part kept, got {stored:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_body_that_is_not_a_documentation_set_is_refused_and_changes_nothing() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "documentation-import-refusals").await;

    let case = "the part the refusals must not disturb";
    let kept = created_part_id(
        &gateway,
        &api,
        json!({ "type": "METHOD", "path": "/pets", "method": "GET" }),
        &content_map("list pets"),
        case,
    )
    .await;

    // Each row is a body that is not a documentation set, differing along how: carrying
    // nothing at all, not JSON at all, truncated, a JSON value that is not an object, an
    // object that declares no documentation parts, and a documentation set that is not a
    // list.
    let refused: &[&str] = &[
        "",
        "not json",
        "{\"documentationParts\": [",
        "[]",
        "42",
        "\"text\"",
        "{}",
        "{\"openapi\": \"3.0.1\"}",
        "{\"documentationParts\": {}}",
        "{\"x-amazon-apigateway-documentation\": {\"documentationParts\": \"nope\"}}",
    ];
    for body in refused {
        let case = format!("ImportDocumentationParts body={body:?}");
        let answer = import_parts(&gateway, &api, None, false, body).await;
        assert_failure(&answer, 400, BAD_REQUEST, &case);
    }

    // A mode outside the documented value set is refused too — by the CONSTRAINT layer,
    // which enforces the modelled `PutMode` enum before the handler runs and answers
    // under its own error class rather than the operation's `BadRequestException`.
    let answer = import_parts(
        &gateway,
        &api,
        Some("replace"),
        false,
        &import_body(&[(json!({ "type": "API" }), json!({ "info": {} }))]),
    )
    .await;
    assert_failure(&answer, 400, "ValidationException", "mode=replace");

    // An unknown API is a not found, not a bad request.
    let answer = import_parts(
        &gateway,
        "nosuchapi0",
        None,
        false,
        &import_body(&[(json!({ "type": "API" }), json!({ "info": {} }))]),
    )
    .await;
    assert_failure(&answer, 404, NOT_FOUND, "ImportDocumentationParts, unknown api");

    // Nothing any of those refusals touched the store.
    let listed = listed_ids(&gateway, &api, "", "after the import refusals").await;
    assert_eq!(
        listed,
        vec![kept],
        "case after the import refusals: expected the collection unchanged, got {listed:?}"
    );
}

#[tokio::test]
async fn a_documentation_set_is_read_with_or_without_the_openapi_envelope() {
    // Each row is one spelling of the SAME documentation set: inside API Gateway's
    // `x-amazon-apigateway-documentation` extension of an OpenAPI document, which is
    // where an export writes it, and as the bare `documentationParts` list a caller can
    // send on its own.
    let entry = json!({
        "location": { "type": "METHOD", "path": "/pets", "method": "GET" },
        "properties": { "description": "list pets" },
    });
    let rows: &[(&str, String)] = &[
        (
            "the x-amazon-apigateway-documentation envelope",
            json!({
                "openapi": "3.0.1",
                "x-amazon-apigateway-documentation": {
                    "version": "1.0.0",
                    "documentationParts": [entry.clone()],
                },
            })
            .to_string(),
        ),
        (
            "a bare documentationParts list",
            json!({ "documentationParts": [entry.clone()] }).to_string(),
        ),
    ];
    for (case, body) in rows {
        let gateway = Gateway::new();
        let (api, _root) = create_api(&gateway, "documentation-import-forms").await;
        let answer = import_parts(&gateway, &api, None, false, body).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let stored = listed_entities(&gateway, &api, case).await;
        assert_eq!(
            stored,
            vec![(entry["location"].clone(), entry["properties"].clone())],
            "case {case}: expected the imported part, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn an_entry_the_import_cannot_store_refuses_the_whole_import_and_writes_nothing() {
    // One body, three entries this service cannot store beside one it can, differing
    // along what is wrong with them. The import is ALL-OR-NOTHING, so the storable entry
    // is not written either.
    let body = {
        let parts = json!([
            {
                "location": { "type": "METHOD", "path": "/pets", "method": "GET" },
                "properties": { "description": "list pets" },
            },
            // `name` is an invalid field for the API entity type.
            {
                "location": { "type": "API", "name": "nope" },
                "properties": { "info": {} },
            },
            // A type outside the modelled enum.
            {
                "location": { "type": "NOT_AN_ENTITY" },
                "properties": {},
            },
            // No location at all.
            { "properties": { "description": "nowhere" } },
        ]);
        json!({
            "openapi": "3.0.1",
            "x-amazon-apigateway-documentation": { "documentationParts": parts },
        })
        .to_string()
    };

    // Whichever way `failonwarnings` is set: an entry that cannot be stored is not a
    // warning the caller may choose to tolerate, so both rows answer the modelled 400 and
    // leave the API's documentation set empty.
    for fail_on_warnings in [false, true] {
        let case = format!("failonwarnings={fail_on_warnings}");
        let gateway = Gateway::new();
        let (api, _root) = create_api(&gateway, "documentation-import-unstorable").await;
        let answer = import_parts(&gateway, &api, None, fail_on_warnings, &body).await;
        assert_failure(&answer, 400, BAD_REQUEST, &case);
        let listed = listed_ids(&gateway, &api, "", &format!("after {case}")).await;
        assert!(
            listed.is_empty(),
            "case after {case}: expected nothing written, got {listed:?}"
        );
    }
}
