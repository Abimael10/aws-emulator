// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ApiGatewayV2 `Model` lifecycle.
//!
//! The rules these cases assert:
//!
//! * **`CreateModel` persists a model under its api and answers 201 with the `ModelId`
//!   it minted**, reporting the `Name`, `ContentType`, `Description` and `Schema` the
//!   record holds; `GetModel` reports that same record, member for member, with the
//!   schema document verbatim. A model belongs to the ONE api that owns it.
//! * **A `Schema` that is not a schema document of the model's content type is refused
//!   400 `BadRequestException`**, and nothing is persisted. For a JSON model — one whose
//!   content type is a JSON media type, or that names none — the schema is a JSON Schema
//!   document ("For application/json models, this should be JSON schema draft 4 model",
//!   `CreateModel$Schema`), and "A JSON Schema is a JSON document, and that document
//!   MUST be an object" (JSON Schema draft-04, section 3.2). A model of another content
//!   type carries its schema in that content type's own language, which this service has
//!   no reader for, so such a schema is stored verbatim.
//! * **A `Name` that is not alphanumeric is refused 400** ("The name of the model. Must
//!   be alphanumeric", `CreateModel$Name`), and a name another model of the same api
//!   already carries is refused 409 `ConflictException`.
//! * **An api holds at most `MAX_MODELS_PER_API` models**, after which a create is
//!   refused 429 `TooManyRequestsException`; the bound is per api.
//! * **`UpdateModel` is a PATCH of the stored record**: a member the request carries
//!   replaces the stored one, a member it omits leaves the stored value exactly as it was,
//!   the `ModelId` is never patched, and the record it leaves behind is what every later
//!   `GetModel`/`GetModels` reports. The patch is weighed on the record it would LEAVE
//!   BEHIND, held to the same name, schema and uniqueness rules as the create, and an
//!   accepted patch is answered `201` — the status API Gateway answers every PATCH that
//!   rewrites an api or one of its children with, not the `200` the model's `@http` trait
//!   declares.
//!
//! The rows vary what each rule generalises over: the content type (absent, JSON, a
//! `+json` suffixed media type, one with parameters, a non-JSON one), the schema's JSON
//! type and well-formedness, which optional members the request carries, the name's
//! alphabet, and — for the patch — WHICH member the patch carries, whether it was already
//! set, and the value's own shape. The recorded cases that filed the issues — a malformed
//! schema refused with `BadRequestException`, and a `Description` patched onto a model that
//! had none — are one row of each table.

use serde_json::{json, Value};

use crate::api_lifecycle::{as_str, assert_minted_id, call, emulator, error_code};

/// A draft-04 document, the schema language `application/json` models use.
fn draft4_schema() -> String {
    json!({
        "$schema": "http://json-schema.org/draft-04/schema#",
        "title": "Order",
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": { "type": "string" },
            "quantity": { "type": "integer" }
        }
    })
    .to_string()
}

/// One accepted-create row: the request, and the members the stored model must report.
struct Row {
    case: &'static str,
    request: Value,
    expected: Vec<(&'static str, Value)>,
}

fn accepted_rows() -> Vec<Row> {
    vec![
        Row {
            case: "a draft-04 model with every optional member set",
            request: json!({
                "name": "Order",
                "contentType": "application/json",
                "description": "the body of an order request",
                "schema": draft4_schema()
            }),
            expected: vec![
                ("name", json!("Order")),
                ("contentType", json!("application/json")),
                ("description", json!("the body of an order request")),
                ("schema", json!(draft4_schema())),
            ],
        },
        Row {
            case: "only the required members: no content type and no description",
            request: json!({
                "name": "Empty",
                "schema": "{}"
            }),
            expected: vec![
                ("name", json!("Empty")),
                // Nothing is invented for a member the request left out.
                ("schema", json!("{}")),
            ],
        },
        Row {
            case: "a JSON media type carrying a charset parameter",
            request: json!({
                "name": "Parameterised",
                "contentType": "application/json; charset=UTF-8",
                "schema": "{\"type\":\"object\"}"
            }),
            expected: vec![
                ("contentType", json!("application/json; charset=UTF-8")),
                ("schema", json!("{\"type\":\"object\"}")),
            ],
        },
        Row {
            case: "a +json structured-syntax-suffix media type (RFC 6839)",
            request: json!({
                "name": "SuffixedJson",
                "contentType": "application/vnd.example.order+json",
                "schema": "{\"type\":\"array\"}"
            }),
            expected: vec![
                ("contentType", json!("application/vnd.example.order+json")),
                ("schema", json!("{\"type\":\"array\"}")),
            ],
        },
        Row {
            case: "a non-JSON content type, whose schema language is not JSON Schema",
            request: json!({
                "name": "OrderXml",
                "contentType": "application/xml",
                // Not JSON at all, and not refused: an XML model's schema is XML.
                "schema": "<xs:schema><xs:element name=\"order\"/></xs:schema>"
            }),
            expected: vec![
                ("contentType", json!("application/xml")),
                (
                    "schema",
                    json!("<xs:schema><xs:element name=\"order\"/></xs:schema>"),
                ),
            ],
        },
        Row {
            case: "an alphanumeric name mixing letters and digits",
            request: json!({
                "name": "Order2Items3",
                "contentType": "application/json",
                "schema": "{\"type\":\"object\",\"properties\":{}}"
            }),
            expected: vec![("name", json!("Order2Items3"))],
        },
        Row {
            case: "an empty JSON Schema object, which matches every document",
            request: json!({
                "name": "Anything",
                "contentType": "application/json",
                "schema": "{ }"
            }),
            // Stored verbatim, not reserialized: the whitespace survives.
            expected: vec![("schema", json!("{ }"))],
        },
    ]
}

/// Create one WebSocket api — the protocol type whose routes reference models — and
/// answer its `ApiId`.
async fn create_api(
    emulator: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

#[tokio::test]
async fn create_model_persists_the_record_and_get_model_reports_it_back() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the models api", "models-api").await;

    let mut minted = Vec::new();
    for row in accepted_rows() {
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/models"),
            Some(row.request.clone()),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateModel, got {status} {created}",
            row.case
        );
        let model_id = as_str(&created, "modelId");
        assert_minted_id(row.case, "ModelId", &model_id);
        minted.push(model_id.clone());

        // The same record, read back through the api that owns it.
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetModel, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            as_str(&fetched, "modelId"),
            model_id,
            "case {:?}: expected the stored model's own id, got {fetched}",
            row.case
        );

        for (member, value) in &row.expected {
            assert_eq!(
                created.get(member),
                Some(value),
                "case {:?}: expected CreateModel to report {member}={value}, got {created}",
                row.case
            );
            assert_eq!(
                fetched.get(member),
                Some(value),
                "case {:?}: expected GetModel to report {member}={value}, got {fetched}",
                row.case
            );
        }

        // A member the request did not carry is reported by neither reader.
        for member in ["contentType", "description"] {
            if row.request.get(member).is_none() {
                assert!(
                    fetched.get(member).is_none_or(Value::is_null),
                    "case {:?}: expected GetModel to report no {member}, got {fetched}",
                    row.case
                );
            }
        }
    }

    // Every create minted a distinct id within the api.
    for (index, id) in minted.iter().enumerate() {
        assert_eq!(
            minted.iter().filter(|other| *other == id).count(),
            1,
            "model {index}: expected distinct ids, got {minted:?}"
        );
    }
}

#[tokio::test]
async fn a_schema_that_is_no_schema_document_of_its_content_type_is_refused() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the refusals api", "model-refusals-api").await;

    // Each row is a `Schema` that is not a JSON Schema document, under a content type
    // whose schema language IS JSON Schema (named, suffixed, or left to the default).
    let rows: Vec<(&str, Value)> = vec![
        (
            "a truncated JSON document",
            json!({ "name": "Truncated", "contentType": "application/json",
                    "schema": "{\"type\":" }),
        ),
        (
            "a document with trailing garbage",
            json!({ "name": "Trailing", "contentType": "application/json",
                    "schema": "{} }" }),
        ),
        (
            "prose that is not JSON at all",
            json!({ "name": "Prose", "contentType": "application/json",
                    "schema": "this is not a schema" }),
        ),
        (
            "an empty schema, which parses as no document",
            json!({ "name": "Blank", "contentType": "application/json", "schema": "" }),
        ),
        (
            "a JSON array, not an object",
            json!({ "name": "Array", "contentType": "application/json", "schema": "[]" }),
        ),
        (
            "a JSON number, not an object",
            json!({ "name": "Number", "contentType": "application/json", "schema": "42" }),
        ),
        (
            "a JSON string, not an object",
            json!({ "name": "Str", "contentType": "application/json",
                    "schema": "\"an object, please\"" }),
        ),
        (
            "a JSON boolean, not an object",
            json!({ "name": "Boolean", "contentType": "application/json", "schema": "true" }),
        ),
        (
            "a JSON null, not an object",
            json!({ "name": "Null", "contentType": "application/json", "schema": "null" }),
        ),
        (
            "a malformed document under a +json media type",
            json!({ "name": "SuffixedBad", "contentType": "application/vnd.example+json",
                    "schema": "{\"type\": }" }),
        ),
        (
            "a malformed document under no content type at all",
            json!({ "name": "DefaultBad", "schema": "{oops}" }),
        ),
    ];

    for (case, request) in rows {
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/models"),
            Some(request),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateModel, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
    }

    // Every create above was refused, so the api persisted nothing: the one model it then
    // accepts is the ONLY model it holds, with nothing a refused create left behind.
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/models"),
        Some(json!({
            "name": "WellFormed",
            "contentType": "application/json",
            "schema": draft4_schema()
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for a well-formed model after the refusals, got {status} {created}"
    );
    let after_refusals = as_str(&created, "modelId");
    assert_minted_id("after the refusals", "modelId", &after_refusals);

    let (status, listed) = call(&emulator, "GET", &format!("/v2/apis/{api_id}/models"), None).await;
    assert_eq!(
        status, 200,
        "expected 200 from GetModels after the refusals, got {status} {listed}"
    );
    let held: Vec<String> = listed
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(|item| as_str(item, "modelId")).collect())
        .unwrap_or_default();
    assert_eq!(
        held,
        vec![after_refusals.clone()],
        "expected the refused creates to have persisted nothing, so the api holds only the \
         model it accepted ({after_refusals:?}), got {listed}"
    );

    // And that one model is the ONLY model the api holds: an id nothing minted is not
    // found under it.
    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/nosuchmdl0"),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "expected no model under an id nothing minted, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "NotFoundException",
        "expected NotFoundException, got {body}"
    );
}

#[tokio::test]
async fn a_name_that_is_not_alphanumeric_or_is_already_taken_is_refused() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the names api", "model-names-api").await;

    // "The name of the model. Must be alphanumeric" (`CreateModel$Name`): the rows vary
    // the non-alphanumeric character and where in the name it falls.
    for (case, name) in [
        ("a hyphenated name", "order-body"),
        ("an underscored name", "order_body"),
        ("a name carrying a space", "order body"),
        ("a dotted name", "order.body"),
        ("a name carrying a dollar", "$default"),
        ("a name outside ASCII", "ordré"),
    ] {
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/models"),
            Some(json!({
                "name": name,
                "contentType": "application/json",
                "schema": "{}"
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from CreateModel, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
    }

    // A name is unique within its api.
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/models"),
        Some(json!({ "name": "Taken", "contentType": "application/json", "schema": "{}" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from the first CreateModel, got {status} {created}"
    );
    let (status, body) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/models"),
        Some(json!({
            "name": "Taken",
            "contentType": "application/json",
            // A different schema, the same name: the NAME is what conflicts.
            "schema": draft4_schema()
        })),
    )
    .await;
    assert_eq!(
        status, 409,
        "expected 409 from a second CreateModel of the same name, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "ConflictException",
        "expected ConflictException, got {body}"
    );

    // The conflict left the first model as it was.
    let model_id = as_str(&created, "modelId");
    let (status, fetched) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetModel after the conflict, got {status} {fetched}"
    );
    assert_eq!(
        fetched.get("schema"),
        Some(&json!("{}")),
        "expected the first model's schema to survive the conflict, got {fetched}"
    );

    // The same name under ANOTHER api is a different model: uniqueness is per api.
    let other = create_api(&emulator, "another api", "model-names-other-api").await;
    let (status, created_elsewhere) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{other}/models"),
        Some(json!({ "name": "Taken", "contentType": "application/json", "schema": "{}" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 for the same name under another api, got {status} {created_elsewhere}"
    );
}

#[tokio::test]
async fn an_api_holds_at_most_the_models_its_quota_allows() {
    // The per-api bound the service enforces, read from the service itself rather than
    // copied: the create past it is refused with the modelled `TooManyRequestsException`
    // ("a limit has been exceeded").
    const LIMIT: usize =
        service_com_amazonaws_apigatewayv2_apigatewayv2::state::MAX_MODELS_PER_API;

    let emulator = emulator();
    let api_id = create_api(&emulator, "the quota api", "model-quota-api").await;

    for index in 0..LIMIT {
        let (status, created) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/models"),
            Some(json!({
                "name": format!("Model{index}"),
                "contentType": "application/json",
                "schema": "{}"
            })),
        )
        .await;
        assert_eq!(
            status, 201,
            "model {index} of {LIMIT}: expected 201 from CreateModel, got {status} {created}"
        );
    }

    let (status, body) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/models"),
        Some(json!({
            "name": "OneTooMany",
            "contentType": "application/json",
            "schema": "{}"
        })),
    )
    .await;
    assert_eq!(
        status, 429,
        "expected 429 from the model past the limit of {LIMIT}, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "TooManyRequestsException",
        "expected TooManyRequestsException, got {body}"
    );

    // Another api is bounded on its own: the full api does not exhaust it.
    let other = create_api(&emulator, "another api", "model-quota-other-api").await;
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{other}/models"),
        Some(json!({ "name": "Room", "contentType": "application/json", "schema": "{}" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 under another api, got {status} {created}"
    );
}

#[tokio::test]
async fn a_model_belongs_to_the_one_api_that_owns_it() {
    let emulator = emulator();
    let first = create_api(&emulator, "the owning api", "model-owner-api").await;
    let second = create_api(&emulator, "another api", "model-other-api").await;

    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{first}/models"),
        Some(json!({
            "name": "Owned",
            "contentType": "application/json",
            "schema": draft4_schema()
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateModel, got {status} {created}"
    );
    let model_id = as_str(&created, "modelId");

    for (case, uri) in [
        (
            "a model of another api",
            format!("/v2/apis/{second}/models/{model_id}"),
        ),
        (
            "a model id nothing minted",
            format!("/v2/apis/{first}/models/nosuchmdl0"),
        ),
        (
            "an api that does not exist",
            format!("/v2/apis/nosuchapi00/models/{model_id}"),
        ),
    ] {
        let (status, body) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from GetModel, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
    }

    // Creating under an api that does not exist creates nothing.
    let (status, body) = call(
        &emulator,
        "POST",
        "/v2/apis/nosuchapi00/models",
        Some(json!({ "name": "Nowhere", "contentType": "application/json", "schema": "{}" })),
    )
    .await;
    assert_eq!(
        status, 404,
        "expected 404 from CreateModel on an unknown api, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "NotFoundException",
        "expected NotFoundException, got {body}"
    );
}

/// The status API Gateway answers a PATCH that rewrites a child of an api with — not the
/// `200` the model's `@http` trait declares for `UpdateModel`.
const PATCHED: u16 = 201;

/// Create one model under `api_id` and answer its `ModelId`.
async fn create_model(
    emulator: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    api_id: &str,
    request: Value,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/models"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateModel, got {status} {created}"
    );
    as_str(&created, "modelId")
}

/// Read one model back through the api that owns it.
async fn read_model(
    emulator: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    api_id: &str,
    model_id: &str,
) -> Value {
    let (status, fetched) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetModel, got {status} {fetched}"
    );
    fetched
}

/// Assert every member of `expected` on `document`, where `None` is a member the record has
/// no value for and so reports not at all.
fn assert_members(case: &str, reader: &str, document: &Value, expected: &[(&str, Option<Value>)]) {
    for (member, value) in expected {
        assert_eq!(
            document.get(*member),
            value.as_ref(),
            "case {case:?}: expected {reader} to report {member}={value:?}, got {document}"
        );
    }
}

/// One `UpdateModel` row: the patch body, and what the record must report afterwards.
struct PatchRow {
    case: &'static str,
    patch: Value,
    expected: Vec<(&'static str, Option<Value>)>,
}

/// The rows the patch rule is asserted over: they differ along WHICH member the patch
/// carries (each of `Name`, `ContentType`, `Description`, `Schema`, several at once, and
/// none at all), whether that member was already set, and the value's own shape.
fn patch_rows() -> Vec<PatchRow> {
    vec![
        PatchRow {
            // The recorded case: a description onto a model created without one.
            case: "a description onto a model that had none",
            patch: json!({ "description": "an updated probe model" }),
            expected: vec![
                ("description", Some(json!("an updated probe model"))),
                // Everything the patch did not name is untouched.
                ("name", Some(json!("Probe"))),
                ("contentType", Some(json!("application/json"))),
                ("schema", Some(json!("{}"))),
            ],
        },
        PatchRow {
            case: "a second description, replacing the first",
            patch: json!({ "description": "a further description" }),
            expected: vec![
                ("description", Some(json!("a further description"))),
                ("name", Some(json!("Probe"))),
            ],
        },
        PatchRow {
            case: "the name alone",
            patch: json!({ "name": "Renamed" }),
            expected: vec![
                ("name", Some(json!("Renamed"))),
                ("description", Some(json!("a further description"))),
                ("schema", Some(json!("{}"))),
            ],
        },
        PatchRow {
            case: "the schema alone, which is stored verbatim",
            patch: json!({ "schema": draft4_schema() }),
            expected: vec![
                ("schema", Some(json!(draft4_schema()))),
                ("name", Some(json!("Renamed"))),
                ("contentType", Some(json!("application/json"))),
            ],
        },
        PatchRow {
            case: "a content type and the schema of its own language, at once",
            patch: json!({
                "contentType": "application/xml",
                "schema": "<xs:schema><xs:element name=\"order\"/></xs:schema>"
            }),
            expected: vec![
                ("contentType", Some(json!("application/xml"))),
                (
                    "schema",
                    Some(json!("<xs:schema><xs:element name=\"order\"/></xs:schema>")),
                ),
                ("name", Some(json!("Renamed"))),
            ],
        },
        PatchRow {
            case: "a description emptied to the shortest its bound allows",
            patch: json!({ "description": "" }),
            expected: vec![
                ("description", Some(json!(""))),
                ("contentType", Some(json!("application/xml"))),
            ],
        },
        PatchRow {
            case: "a patch that carries no member at all, which changes nothing",
            patch: json!({}),
            expected: vec![
                ("description", Some(json!(""))),
                ("name", Some(json!("Renamed"))),
                ("contentType", Some(json!("application/xml"))),
                (
                    "schema",
                    Some(json!("<xs:schema><xs:element name=\"order\"/></xs:schema>")),
                ),
            ],
        },
    ]
}

/// `UpdateModel` replaces what its request carries and leaves what it omits, the patches
/// compose, and every reader reports the record the last patch left behind.
#[tokio::test]
async fn update_model_replaces_what_it_carries_and_every_reader_reports_the_result() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the patched models api", "model-patch-api").await;
    let model_id = create_model(
        &emulator,
        "the seed",
        &api_id,
        json!({ "name": "Probe", "contentType": "application/json", "schema": "{}" }),
    )
    .await;

    for row in patch_rows() {
        let case = row.case;
        let (status, patched) = call(
            &emulator,
            "PATCH",
            &format!("/v2/apis/{api_id}/models/{model_id}"),
            Some(row.patch.clone()),
        )
        .await;
        assert_eq!(
            status, PATCHED,
            "case {case:?}: expected {PATCHED} from UpdateModel, got {status} {patched}"
        );
        // The identity is the resource's and is never patched.
        assert_eq!(
            as_str(&patched, "modelId"),
            model_id,
            "case {case:?}: expected the patch to answer the same ModelId, got {patched}"
        );
        assert_members(case, "UpdateModel", &patched, &row.expected);

        // The round trip: the single reader reports what the patch stored.
        let fetched = read_model(&emulator, case, &api_id, &model_id).await;
        assert_members(case, "GetModel", &fetched, &row.expected);
        assert_eq!(
            fetched, patched,
            "case {case:?}: expected GetModel to report the patched record, got {fetched}"
        );

        // And so does the collection reader.
        let (status, page) =
            call(&emulator, "GET", &format!("/v2/apis/{api_id}/models"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetModels, got {status} {page}"
        );
        let listed = page["items"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| as_str(item, "modelId") == model_id)
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: GetModels must list the model, got {page}"));
        assert_members(case, "GetModels", &listed, &row.expected);
    }
}

/// A patch is weighed on the record it would LEAVE BEHIND, so one that would store a record
/// the create could not is refused and writes nothing.
#[tokio::test]
async fn an_update_that_would_store_an_invalid_model_is_refused_and_writes_nothing() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the refusals api", "model-patch-refusal-api").await;
    let model_id = create_model(
        &emulator,
        "the seed",
        &api_id,
        json!({
            "name": "Order",
            "contentType": "application/json",
            "description": "the seeded description",
            "schema": draft4_schema()
        }),
    )
    .await;
    // A sibling, so a patch can collide with a name the api already holds.
    create_model(
        &emulator,
        "the sibling",
        &api_id,
        json!({ "name": "Sibling", "contentType": "application/json", "schema": "{}" }),
    )
    .await;
    let before = read_model(&emulator, "the seed", &api_id, &model_id).await;

    // (case, the patch, the status, the error code)
    let refusals: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a hyphenated name",
            json!({ "name": "order-body" }),
            400,
            "BadRequestException",
        ),
        (
            "a name carrying a space",
            json!({ "name": "order body" }),
            400,
            "BadRequestException",
        ),
        (
            "a name outside ASCII",
            json!({ "name": "ordré" }),
            400,
            "BadRequestException",
        ),
        (
            "a schema that is not JSON at all, under a JSON content type",
            json!({ "schema": "not a document" }),
            400,
            "BadRequestException",
        ),
        (
            "a schema that is JSON but not an object",
            json!({ "schema": "[1, 2, 3]" }),
            400,
            "BadRequestException",
        ),
        (
            // The content type MOVES and the stored schema stays: the PAIR the record
            // would carry is what is weighed, so a JSON content type over a non-JSON
            // schema is refused even though neither member alone is new.
            "a JSON content type over a schema that is no JSON document",
            json!({ "contentType": "application/json", "schema": "<order/>" }),
            400,
            "BadRequestException",
        ),
        (
            "a name the sibling already holds",
            json!({ "name": "Sibling" }),
            409,
            "ConflictException",
        ),
    ];

    for (case, patch, expected_status, code) in refusals {
        let (status, body) = call(
            &emulator,
            "PATCH",
            &format!("/v2/apis/{api_id}/models/{model_id}"),
            Some(patch.clone()),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected {expected_status} from UpdateModel, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected {code}, got {body}"
        );
        // The refused patch wrote nothing.
        let after = read_model(&emulator, case, &api_id, &model_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused patch to leave the record alone, got {after}"
        );
    }

    // Re-sending the model's OWN name conflicts with nothing.
    let (status, patched) = call(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        Some(json!({ "name": "Order", "description": "kept its own name" })),
    )
    .await;
    assert_eq!(
        status, PATCHED,
        "expected {PATCHED} when a patch re-sends the model's own name, got {status} \
         {patched}"
    );
    assert_eq!(
        patched.get("description"),
        Some(&json!("kept its own name")),
        "expected the rest of the patch to be applied, got {patched}"
    );
}

/// A model is addressed by the api that owns it AND its own id, so a patch naming either
/// wrongly is 404 and writes nothing; and a `Model` is WebSocket-only, so an api of another
/// protocol has none to patch.
#[tokio::test]
async fn an_update_naming_no_model_of_this_api_is_refused() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the owning api", "model-patch-owner-api").await;
    let other_api = create_api(&emulator, "another api", "model-patch-other-api").await;
    let model_id = create_model(
        &emulator,
        "the seed",
        &api_id,
        json!({ "name": "Owned", "contentType": "application/json", "schema": "{}" }),
    )
    .await;

    for (case, uri) in [
        (
            "an api nothing minted",
            format!("/v2/apis/nosuchapi00/models/{model_id}"),
        ),
        (
            "a model id nothing minted",
            format!("/v2/apis/{api_id}/models/absentmodel"),
        ),
        (
            "a model of ANOTHER api",
            format!("/v2/apis/{other_api}/models/{model_id}"),
        ),
    ] {
        let (status, body) = call(
            &emulator,
            "PATCH",
            &uri,
            Some(json!({ "description": "should not land" })),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from UpdateModel, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
    }

    // None of them wrote anything: the model still has no description.
    let fetched = read_model(&emulator, "after the refusals", &api_id, &model_id).await;
    assert_eq!(
        fetched.get("description"),
        None,
        "expected the refused patches to have written nothing, got {fetched}"
    );

    // An HTTP api has no models at all, so there is nothing there to patch.
    let case = "an HTTP api, whose protocol has no models";
    let (status, created) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "model-patch-http-api", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    let http_api_id = as_str(&created, "apiId");
    let (status, body) = call(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{http_api_id}/models/anymodelid"),
        Some(json!({ "description": "should not land" })),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from UpdateModel, got {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "BadRequestException",
        "case {case:?}: expected BadRequestException, got {body}"
    );
}
