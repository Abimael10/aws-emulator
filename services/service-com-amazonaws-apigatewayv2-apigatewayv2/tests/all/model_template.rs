// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ApiGatewayV2 `GetModelTemplate`.
//!
//! The rule these cases assert: **`GetModelTemplate` GENERATES the sample mapping template
//! of the model the api holds under the requested id** — the
//! `#set($inputRoot = $input.path('$'))` preamble followed by an EXAMPLE of the payload the
//! model's JSON Schema declares, in which every declared property carries a sample value of
//! the JSON type declared for that property (a `string` a sample string, an `integer` an
//! integer, a `number` a fractional number, a `boolean` a boolean, an `object` an object of
//! its own declared properties), and where the schema itself states the value (an `enum`
//! entry, a `default`) that value is the sample. A declared ARRAY is the one value whose
//! members the schema does not state — how many elements it has is the incoming payload's
//! to say — so what is generated for it is the Velocity LOOP that maps it, around one
//! sample element. It reads the store and writes nothing, so two calls on an unchanged
//! model answer the same template; a model the api does not hold is 404 `NotFoundException`.
//!
//! The rows vary what the rule generalises over: the JSON type the schema declares
//! (`object`, `array`, `string`, `integer`, `number`, `boolean`, `null`, a union of types,
//! a type implied by `properties`/`items`/`enum`, a type stated by a composition, none at
//! all), how deeply the declaration nests, how many properties it declares, how a property
//! is named, and whether the schema is reached directly or through a `$ref`. The last three
//! cases are the array half of the rule on its own: WHERE the mapped array sits, what its
//! elements are, and how it is declared. The recorded cases that filed the issue — one
//! `GetModelTemplate` on a JSON Schema model declaring a single string property, and one on
//! a model whose schema declares an array at its root — are rows of the first table and of
//! the array table.

use serde_json::{json, Value};

use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// The line a generated template opens with: it binds `$inputRoot` to the root of the
/// incoming payload, which is what a caller maps from once they edit the sample.
const PREAMBLE: &str = "#set($inputRoot = $input.path('$'))";

/// A whole template: the preamble, then `body`.
fn template(body: &str) -> String {
    format!("{PREAMBLE}\n{body}")
}

/// The comment every generated loop is preceded by — the instruction to point it at the
/// array the incoming payload carries — which is also how a case tells a template that maps
/// an array from one that writes its members out directly, and the marker a caller greps
/// for to find the one spot a generated template has to be edited at.
const FOREACH_TODO: &str = "##TODO: Update this foreach loop to reference array from input json";

/// The loop a generated template iterates the mapped array with.
const FOREACH_OPEN: &str = "#foreach($elem in $inputRoot.TODO)";

/// What that loop writes BETWEEN elements and not after the last one: the mapped array is
/// JSON, whose elements are comma-separated.
const FOREACH_SEPARATOR: &str = "#if($foreach.hasNext),#end";

/// The directive that closes a Velocity block.
const BLOCK_END: &str = "#end";

/// The template an array whose element renders as `element` is generated as: the loop that
/// maps the array the incoming payload carries, around one sample element.
fn array_loop(element: &str) -> String {
    [
        "[".to_string(),
        FOREACH_TODO.to_string(),
        FOREACH_OPEN.to_string(),
        format!(" {element} "),
        FOREACH_SEPARATOR.to_string(),
        BLOCK_END.to_string(),
        "]".to_string(),
    ]
    .join("\n")
}

/// The generated template's body — everything after the preamble — parsed as the JSON
/// document the sample payload is. Only a template that maps no array is one: a loop is
/// Velocity, not JSON.
fn parsed_sample(value: &str, case: &str) -> Value {
    let body = value
        .split_once('\n')
        .map(|(_, body)| body)
        .unwrap_or_else(|| panic!("case {case:?}: expected a preamble and a body, got {value:?}"));
    serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("case {case:?}: expected the sample to be JSON, got {body:?}"))
}

/// One row: a model's schema, and the template `GetModelTemplate` must generate from it.
struct Row {
    case: &'static str,
    /// The `ContentType` the model is created with, if any.
    content_type: Option<&'static str>,
    schema: Value,
    expected_body: String,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an object declaring one property of each JSON scalar type",
            content_type: Some("application/json"),
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "title": "Order",
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" },
                    "paid": { "type": "boolean" },
                    "quantity": { "type": "integer" },
                    "total": { "type": "number" }
                }
            }),
            expected_body: [
                "{",
                "  \"id\" : \"foo\",",
                "  \"paid\" : true,",
                "  \"quantity\" : 0,",
                "  \"total\" : 0.0",
                "}",
            ]
            .join("\n"),
        },
        Row {
            case: "a draft-04 object declaring no properties (the Empty model's schema)",
            content_type: Some("application/json"),
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "title": "Empty Schema",
                "type": "object"
            }),
            // Nothing is declared, so there is no member to map.
            expected_body: "{ }".to_string(),
        },
        Row {
            case: "an object nesting an object",
            content_type: None,
            schema: json!({
                "type": "object",
                "properties": {
                    "customer": {
                        "type": "object",
                        "properties": { "name": { "type": "string" } }
                    },
                    "total": { "type": "number" }
                }
            }),
            expected_body: [
                "{",
                "  \"customer\" : {",
                "    \"name\" : \"foo\"",
                "  },",
                "  \"total\" : 0.0",
                "}",
            ]
            .join("\n"),
        },
        Row {
            case: "an array of objects",
            content_type: Some("application/json"),
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "title": "PetsModelNoFlatten",
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "class": { "type": "string" },
                        "number": { "type": "integer" }
                    }
                }
            }),
            expected_body: array_loop(
                &["{", "  \"class\" : \"foo\",", "  \"number\" : 0", "}"].join("\n"),
            ),
        },
        Row {
            case: "an array of strings",
            content_type: Some("application/json"),
            schema: json!({ "type": "array", "items": { "type": "string" } }),
            expected_body: array_loop("\"foo\""),
        },
        Row {
            // One sample element, whatever the element itself is: an array of arrays
            // describes one array of one integer, so the loop that maps the outer array
            // carries the loop that maps the inner one.
            case: "an array of arrays",
            content_type: Some("application/json"),
            schema: json!({
                "type": "array",
                "items": { "type": "array", "items": { "type": "integer" } }
            }),
            expected_body: array_loop(&array_loop("0")),
        },
        Row {
            case: "an object whose property is an array of objects",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": { "sku": { "type": "string" } }
                        }
                    }
                }
            }),
            expected_body: [
                "{".to_string(),
                format!(
                    "  \"items\" : {}",
                    array_loop(&["{", "    \"sku\" : \"foo\"", "  }"].join("\n"))
                ),
                "}".to_string(),
            ]
            .join("\n"),
        },
        Row {
            // JSON Schema draft-04 section 7: a reference into the same document is a
            // JSON Pointer fragment, which resolves to the schema it names.
            case: "a property whose schema is a $ref into the document's definitions",
            content_type: Some("application/json"),
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "properties": { "pet": { "$ref": "#/definitions/Pet" } },
                "definitions": {
                    "Pet": {
                        "type": "object",
                        "properties": { "name": { "type": "string" } }
                    }
                }
            }),
            expected_body: [
                "{",
                "  \"pet\" : {",
                "    \"name\" : \"foo\"",
                "  }",
                "}",
            ]
            .join("\n"),
        },
        Row {
            // Draft-04 section 5.5.2 allows a union of types; the first type that is not
            // `null` is the one the sample carries.
            case: "properties declaring a union of types",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": {
                    "count": { "type": ["integer", "null"] },
                    "note": { "type": ["string", "null"] }
                }
            }),
            expected_body: ["{", "  \"count\" : 0,", "  \"note\" : \"foo\"", "}"].join("\n"),
        },
        Row {
            // A schema that RESTRICTS its instance to a set of values (draft-04 section
            // 5.5.1) has already stated the value it means, so the sample is the first of
            // them — whatever JSON type the entries carry, and with no `type` keyword
            // needed to say so.
            case: "properties typed only by their enum values",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": {
                    "retries": { "enum": [2, 3] },
                    "status": { "enum": ["OPEN", "CLOSED"] }
                }
            }),
            expected_body: ["{", "  \"retries\" : 2,", "  \"status\" : \"OPEN\"", "}"]
                .join("\n"),
        },
        Row {
            // And a schema that supplies a `default` for its instance (section 6.2) has
            // stated one too.
            case: "a property carrying a default",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "region": { "type": "string", "default": "us-east-1" } }
            }),
            expected_body: ["{", "  \"region\" : \"us-east-1\"", "}"].join("\n"),
        },
        Row {
            // draft-04 section 5.5.3: an instance satisfying `allOf` satisfies EVERY
            // subschema, so the sample describes what all of them declare together.
            case: "an object composed with allOf",
            content_type: Some("application/json"),
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "allOf": [{ "$ref": "#/definitions/Audited" }],
                "properties": { "sku": { "type": "string" } },
                "definitions": {
                    "Audited": {
                        "type": "object",
                        "properties": { "createdAt": { "type": "integer" } }
                    }
                }
            }),
            expected_body: [
                "{",
                "  \"createdAt\" : 0,",
                "  \"sku\" : \"foo\"",
                "}",
            ]
            .join("\n"),
        },
        Row {
            // Sections 5.5.4 and 5.5.5: an instance satisfying `anyOf`/`oneOf` satisfies
            // one branch, so the sample describes the first of them — which is also what
            // types the value where the schema states no type of its own.
            case: "a property composed with oneOf",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": {
                    "amount": { "oneOf": [{ "type": "number" }, { "type": "string" }] },
                    "tag": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
                }
            }),
            expected_body: ["{", "  \"amount\" : 0.0,", "  \"tag\" : \"foo\"", "}"]
                .join("\n"),
        },
        Row {
            // A property name is a JSON member name whatever it carries, and the sample's
            // own structure is the generator's, never a name's.
            case: "a property name that is no identifier",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "order id": { "type": "string" } }
            }),
            expected_body: ["{", "  \"order id\" : \"foo\"", "}"].join("\n"),
        },
        Row {
            // A name carrying a quote of either kind must not close the JSON literal the
            // member name is rendered inside.
            case: "a property name carrying quotes",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "o'rder \"x\"": { "type": "string" } }
            }),
            expected_body: ["{", r#"  "o'rder \"x\"" : "foo""#, "}"].join("\n"),
        },
        Row {
            case: "a property name carrying a newline and a backslash",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "a\nb\\c": { "type": "string" } }
            }),
            expected_body: ["{", r#"  "a\nb\\c" : "foo""#, "}"].join("\n"),
        },
        Row {
            case: "a property declared null",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "deletedAt": { "type": "null" } }
            }),
            expected_body: ["{", "  \"deletedAt\" : null", "}"].join("\n"),
        },
        Row {
            // An object is what `properties` applies to (draft-04 section 5.4.4), even
            // where no `type` says so.
            case: "an object typed only by declaring properties",
            content_type: Some("application/json"),
            schema: json!({ "properties": { "name": { "type": "string" } } }),
            expected_body: ["{", "  \"name\" : \"foo\"", "}"].join("\n"),
        },
        Row {
            // And an array is what `items` applies to (section 5.3.1).
            case: "an array typed only by declaring items",
            content_type: Some("application/json"),
            schema: json!({ "items": { "type": "number" } }),
            expected_body: array_loop("0.0"),
        },
        Row {
            // An array declaring no items constrains no element, so there is none to
            // describe.
            case: "an array declaring no items, whose elements are of unknown shape",
            content_type: Some("application/json"),
            schema: json!({ "type": "array" }),
            expected_body: "[ ]".to_string(),
        },
        Row {
            // A value the schema declares nothing about is rendered as the JSON value that
            // says nothing about it.
            case: "a property whose schema declares no type at all",
            content_type: Some("application/json"),
            schema: json!({
                "type": "object",
                "properties": { "payload": { "description": "anything at all" } }
            }),
            expected_body: ["{", "  \"payload\" : null", "}"].join("\n"),
        },
        Row {
            case: "the empty schema, which matches every document",
            content_type: Some("application/json"),
            schema: json!({}),
            expected_body: "null".to_string(),
        },
        Row {
            // A `+json` media type (RFC 6839) names JSON Schema as its schema language
            // too, so a template is generated for it.
            case: "a +json suffixed media type carrying a parameter",
            content_type: Some("application/vnd.example.order+json; charset=UTF-8"),
            schema: json!({
                "type": "object",
                "properties": { "sku": { "type": "string" } }
            }),
            expected_body: ["{", "  \"sku\" : \"foo\"", "}"].join("\n"),
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

/// Create one model under `api_id` and answer its `ModelId`.
async fn create_model(
    emulator: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    api_id: &str,
    name: &str,
    content_type: Option<&str>,
    schema: &str,
) -> String {
    let mut request = json!({ "name": name, "schema": schema });
    if let Some(content_type) = content_type {
        request["contentType"] = json!(content_type);
    }
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

#[tokio::test]
async fn get_model_template_generates_the_template_the_models_schema_declares() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the templates api", "model-template-api").await;

    for (index, row) in rows().into_iter().enumerate() {
        let schema = row.schema.to_string();
        let model_id = create_model(
            &emulator,
            row.case,
            &api_id,
            &format!("Template{index}"),
            row.content_type,
            &schema,
        )
        .await;

        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}/template"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetModelTemplate, got {status} {body}",
            row.case
        );
        let value = as_str(&body, "value");
        assert_eq!(
            value,
            template(&row.expected_body),
            "case {:?}: expected the sample its schema declares, got {value:?}",
            row.case
        );
        // Every template opens with the preamble that binds `$inputRoot`.
        assert!(
            value.starts_with(PREAMBLE),
            "case {:?}: expected the template to open with {PREAMBLE:?}, got {value:?}",
            row.case
        );
        // A template that maps no array writes its members out directly, so its body is
        // itself a JSON document: each value slot carries a sample value, never a reference
        // into the payload. One that maps an array carries the Velocity loop instead, which
        // the array cases below assert.
        if !row.expected_body.contains(FOREACH_TODO) {
            parsed_sample(&value, row.case);
        }
    }
}

#[tokio::test]
async fn every_sample_value_carries_the_json_type_its_schema_declares() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the typed api", "model-template-types").await;

    // One property per declared type, each checked against what the parsed sample must
    // carry for it. The rows span every type a JSON Schema declares a VALUE of, plus the
    // two forms that state a value outright and the union spelling. The remaining declared
    // type, an `array`, generates the loop that maps it rather than a value — so it is no
    // member of this JSON document, and the array cases below are where it is asserted.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, Value, fn(&Value) -> bool)> = vec![
        ("a string", json!({ "type": "string" }), |value| {
            value.is_string()
        }),
        ("an integer", json!({ "type": "integer" }), |value| {
            value.is_i64()
        }),
        ("a number", json!({ "type": "number" }), |value| {
            value.is_f64() && !value.is_i64()
        }),
        ("a boolean", json!({ "type": "boolean" }), |value| {
            value.is_boolean()
        }),
        ("a null", json!({ "type": "null" }), |value| value.is_null()),
        (
            "an object",
            json!({ "type": "object", "properties": { "inner": { "type": "string" } } }),
            |value| value.is_object(),
        ),
        ("an integer enum", json!({ "enum": [7, 8] }), |value| {
            value.is_i64()
        }),
        (
            "a string default",
            json!({ "type": "string", "default": "set" }),
            |value| value.is_string(),
        ),
        (
            "a union of a number and null",
            json!({ "type": ["number", "null"] }),
            |value| value.is_f64(),
        ),
    ];

    let properties: serde_json::Map<String, Value> = rows
        .iter()
        .enumerate()
        .map(|(index, (_, schema, _))| (format!("member{index}"), schema.clone()))
        .collect();
    let case = "one model declaring every type";
    let model_id = create_model(
        &emulator,
        case,
        &api_id,
        "EveryType",
        Some("application/json"),
        &json!({ "type": "object", "properties": properties }).to_string(),
    )
    .await;

    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}/template"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetModelTemplate, got {status} {body}"
    );
    let sample = parsed_sample(&as_str(&body, "value"), case);
    for (index, (declared, _, carries)) in rows.iter().enumerate() {
        let member = format!("member{index}");
        let example = &sample[&member];
        assert!(
            carries(example),
            "case {declared}: expected {member} to carry the declared type, got {example:?}"
        );
    }
}

#[tokio::test]
async fn generating_a_template_changes_nothing_a_later_caller_sees() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the read-only api", "model-template-read-api").await;

    let schema = json!({
        "$schema": "http://json-schema.org/draft-04/schema#",
        "type": "object",
        "properties": { "id": { "type": "string" } }
    })
    .to_string();
    let model_id = create_model(
        &emulator,
        "a read model",
        &api_id,
        "ReadOnly",
        Some("application/json"),
        &schema,
    )
    .await;
    let uri = format!("/v2/apis/{api_id}/models/{model_id}/template");

    let mut answers = Vec::new();
    for attempt in 0..3 {
        let (status, body) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "attempt {attempt}: expected 200 from GetModelTemplate, got {status} {body}"
        );
        answers.push(as_str(&body, "value"));
    }
    for (attempt, answer) in answers.iter().enumerate() {
        assert_eq!(
            answer, &answers[0],
            "attempt {attempt}: expected the same template every call, got {answer:?} \
             after {:?}",
            answers[0]
        );
    }

    // The model itself is untouched: its schema is still the document the create stored.
    let (status, fetched) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetModel after the template reads, got {status} {fetched}"
    );
    assert_eq!(
        fetched.get("schema"),
        Some(&json!(schema)),
        "expected the stored schema to survive the template reads, got {fetched}"
    );
    assert_eq!(
        as_str(&fetched, "name"),
        "ReadOnly",
        "expected the stored name to survive the template reads, got {fetched}"
    );
}

#[tokio::test]
async fn a_template_is_asked_of_the_one_api_that_owns_the_model() {
    let emulator = emulator();
    let first = create_api(&emulator, "the owning api", "model-template-owner").await;
    let second = create_api(&emulator, "another api", "model-template-other").await;

    let model_id = create_model(
        &emulator,
        "the owned model",
        &first,
        "Owned",
        Some("application/json"),
        "{\"type\":\"object\",\"properties\":{\"id\":{\"type\":\"string\"}}}",
    )
    .await;

    // An HTTP api holds no models at all: "Currently, Models are not permitted for APIs
    // with a protocol type of HTTP".
    let (status, http_api) = call(
        &emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": "model-template-http", "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi for the http api, got {status} {http_api}"
    );
    let http_api_id = as_str(&http_api, "apiId");

    for (case, uri, named) in [
        (
            "a model of another api",
            format!("/v2/apis/{second}/models/{model_id}/template"),
            model_id.clone(),
        ),
        (
            "a model id nothing minted",
            format!("/v2/apis/{first}/models/nosuchmdl0/template"),
            "nosuchmdl0".to_string(),
        ),
        (
            "an api that does not exist",
            format!("/v2/apis/nosuchapi00/models/{model_id}/template"),
            model_id.clone(),
        ),
        (
            "an api whose protocol holds no models",
            format!("/v2/apis/{http_api_id}/models/{model_id}/template"),
            model_id.clone(),
        ),
    ] {
        let (status, body) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from GetModelTemplate, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
        // The refusal names the identifier the REQUEST supplied: that is what the caller
        // can act on.
        assert!(
            body.get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains(&named)),
            "case {case:?}: expected the refusal to name {named:?}, got {body}"
        );
    }
}

#[tokio::test]
async fn a_model_whose_schema_is_no_json_schema_has_no_generated_template() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the other languages api", "model-template-lang").await;

    // A model of a non-JSON content type carries its schema in that content type's own
    // language, which this service has no reader for — whatever that document happens to
    // look like, so a schema that parses as JSON under an XML content type is refused
    // too, rather than read as a JSON Schema it was never declared to be.
    for (index, (case, content_type, schema)) in [
        (
            "an XML schema under an XML content type",
            "application/xml",
            "<xs:schema><xs:element name=\"order\"/></xs:schema>",
        ),
        (
            "a JSON document under an XML content type",
            "application/xml",
            "{\"type\":\"object\"}",
        ),
        (
            "a text content type",
            "text/plain",
            "an order, one per line",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let model_id = create_model(
            &emulator,
            case,
            &api_id,
            &format!("Lang{index}"),
            Some(content_type),
            schema,
        )
        .await;
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}/template"),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from GetModelTemplate, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert!(
            body.get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| message.contains(content_type)),
            "case {case:?}: expected the refusal to name the content type, got {body}"
        );
    }
}

#[tokio::test]
async fn a_schema_that_refers_to_itself_still_answers_one_template() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the recursive api", "model-template-recursive").await;

    // A node whose child is the same schema: the descent is bounded, so the generation
    // terminates and the deepest value is described as one of unknown shape.
    let schema = json!({
        "$schema": "http://json-schema.org/draft-04/schema#",
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "child": { "$ref": "#" }
        }
    })
    .to_string();
    let model_id = create_model(
        &emulator,
        "a recursive model",
        &api_id,
        "Recursive",
        Some("application/json"),
        &schema,
    )
    .await;

    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}/template"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetModelTemplate for a recursive schema, got {status} {body}"
    );
    let value = as_str(&body, "value");
    assert!(
        value.starts_with(PREAMBLE),
        "expected the template to open with {PREAMBLE:?}, got {value:?}"
    );
    // The recursion descended and stopped: the self-referring member is described at every
    // level reached, and the descent is bounded well under the schema's infinite depth.
    let depth = value.matches("\"child\" : ").count();
    assert!(
        (1..=12).contains(&depth),
        "expected the descent into a self-referring schema to be bounded, got {depth} \
         levels in {value:?}"
    );
    assert!(
        value.len() <= MAX_TEMPLATE_BYTES,
        "expected the template to fit the output bound of {MAX_TEMPLATE_BYTES} bytes, \
         got {} bytes",
        value.len()
    );
}

/// The output bound the service generates a template within, read from the service rather
/// than copied, so a test cannot drift from the bound it asserts.
const MAX_TEMPLATE_BYTES: usize =
    service_com_amazonaws_apigatewayv2_apigatewayv2::ops::get_model_template::MAX_TEMPLATE_BYTES;

/// The step bound the service generates a template within.
const MAX_STEPS: usize =
    service_com_amazonaws_apigatewayv2_apigatewayv2::ops::get_model_template::MAX_STEPS;

#[tokio::test]
async fn a_schema_whose_template_would_not_fit_the_bounds_is_refused_not_expanded() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the bounded api", "model-template-bounded").await;

    // Each row is a small schema whose sample, expanded without a bound, would be
    // enormous: the rows vary HOW it grows — a branching self-reference (exponential in
    // the branching factor), a branching reference into a definition, and arrays whose
    // items refer back to the whole document.
    let mut branching = serde_json::Map::new();
    for index in 0..10 {
        branching.insert(format!("child{index}"), json!({ "$ref": "#" }));
    }
    let mut fanning = serde_json::Map::new();
    for index in 0..8 {
        fanning.insert(format!("node{index}"), json!({ "$ref": "#/definitions/Node" }));
    }
    let mut nesting = serde_json::Map::new();
    for index in 0..10 {
        nesting.insert(
            format!("list{index}"),
            json!({ "type": "array", "items": { "$ref": "#" } }),
        );
    }
    let rows: Vec<(&str, Value)> = vec![
        (
            "ten properties that each refer back to the whole document",
            json!({ "type": "object", "properties": Value::Object(branching) }),
        ),
        (
            "a definition whose properties refer back to that definition",
            json!({
                "type": "object",
                "properties": { "root": { "$ref": "#/definitions/Node" } },
                "definitions": {
                    "Node": {
                        "type": "object",
                        "properties": Value::Object(fanning.clone())
                    }
                }
            }),
        ),
        (
            // The same growth through arrays: each element is one sample, so an array
            // whose items refer back to the document re-expands it once per property.
            "ten arrays whose items refer back to the whole document",
            json!({ "type": "object", "properties": Value::Object(nesting) }),
        ),
    ];

    for (index, (case, schema)) in rows.into_iter().enumerate() {
        let model_id = create_model(
            &emulator,
            case,
            &api_id,
            &format!("Bounded{index}"),
            Some("application/json"),
            &schema.to_string(),
        )
        .await;
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}/template"),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from GetModelTemplate, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        // The refusal names the bound it hit, so the caller can see which one — the steps
        // a generation may take, or the bytes of template it may build.
        let message = as_str(&body, "message");
        assert!(
            message.contains(&format!("{MAX_STEPS} steps"))
                || message.contains(&format!("{MAX_TEMPLATE_BYTES} bytes of template")),
            "case {case:?}: expected the refusal to name the bound it hit, got {message:?}"
        );
    }

    // The service is still serving after the refusals: a bounded refusal costs the api
    // nothing, and a model that fits the bounds is still answered.
    let model_id = create_model(
        &emulator,
        "a model that fits",
        &api_id,
        "Fits",
        Some("application/json"),
        "{\"type\":\"object\",\"properties\":{\"id\":{\"type\":\"string\"}}}",
    )
    .await;
    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}/template"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 after the refusals, got {status} {body}"
    );
    let value = as_str(&body, "value");
    assert_eq!(
        value,
        template(&["{", "  \"id\" : \"foo\"", "}"].join("\n")),
        "expected the sample its schema declares, got {value:?}"
    );
}

/// One array case: a schema declaring at least one array, the text of the sample element the
/// innermost loop wraps, and how many arrays the schema declares.
struct ArrayRow {
    case: &'static str,
    schema: Value,
    /// The sample element of the INNERMOST loop, which is what that array's `items` schema
    /// declares every one of its elements to look like.
    element: String,
    /// How many arrays the generated template maps — one loop each.
    loops: usize,
}

/// The cases the array half of the rule is asserted over. They vary WHERE the array sits
/// (the document's root, a property of an object, a property two levels down, an element of
/// another array, two beside each other), what its elements are (a string, an integer, a
/// number, a boolean, an object, an array) and HOW it is declared (a `type` keyword, a union
/// naming the array type, `items` alone, a tuple of item schemas, an item schema reached
/// through a `$ref`).
fn array_rows() -> Vec<ArrayRow> {
    vec![
        ArrayRow {
            // The recorded shape: a model whose JSON Schema declares an array at its root.
            case: "an array of strings at the document's root",
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "title": "Tags",
                "type": "array",
                "items": { "type": "string" }
            }),
            element: "\"foo\"".to_string(),
            loops: 1,
        },
        ArrayRow {
            case: "an array of integers",
            schema: json!({ "type": "array", "items": { "type": "integer" } }),
            element: "0".to_string(),
            loops: 1,
        },
        ArrayRow {
            case: "an array of numbers",
            schema: json!({ "type": "array", "items": { "type": "number" } }),
            element: "0.0".to_string(),
            loops: 1,
        },
        ArrayRow {
            case: "an array of booleans",
            schema: json!({ "type": "array", "items": { "type": "boolean" } }),
            element: "true".to_string(),
            loops: 1,
        },
        ArrayRow {
            case: "an array of objects",
            schema: json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "sku": { "type": "string" } }
                }
            }),
            element: ["{", "  \"sku\" : \"foo\"", "}"].join("\n"),
            loops: 1,
        },
        ArrayRow {
            case: "an array of arrays",
            schema: json!({
                "type": "array",
                "items": { "type": "array", "items": { "type": "string" } }
            }),
            element: "\"foo\"".to_string(),
            loops: 2,
        },
        ArrayRow {
            case: "an array declared as a property of an object",
            schema: json!({
                "type": "object",
                "properties": { "lines": { "type": "array", "items": { "type": "string" } } }
            }),
            element: "\"foo\"".to_string(),
            loops: 1,
        },
        ArrayRow {
            case: "an array declared two levels down",
            schema: json!({
                "type": "object",
                "properties": {
                    "order": {
                        "type": "object",
                        "properties": {
                            "lines": { "type": "array", "items": { "type": "integer" } }
                        }
                    }
                }
            }),
            element: "0".to_string(),
            loops: 1,
        },
        ArrayRow {
            // Two arrays beside each other: each one is mapped by its own loop.
            case: "two arrays declared beside each other",
            schema: json!({
                "type": "object",
                "properties": {
                    "first": { "type": "array", "items": { "type": "string" } },
                    "second": { "type": "array", "items": { "type": "string" } }
                }
            }),
            element: "\"foo\"".to_string(),
            loops: 2,
        },
        ArrayRow {
            // JSON Schema draft-04 section 5.5.2 allows a union of type names; the first
            // that is not `null` is the one the template is generated for.
            case: "an array declared by a union naming the array type",
            schema: json!({ "type": ["null", "array"], "items": { "type": "string" } }),
            element: "\"foo\"".to_string(),
            loops: 1,
        },
        ArrayRow {
            // Section 5.3.1: `items` only applies to an array, so it declares one on its
            // own.
            case: "an array typed only by declaring items",
            schema: json!({ "items": { "type": "boolean" } }),
            element: "true".to_string(),
            loops: 1,
        },
        ArrayRow {
            // And `items` written as a TUPLE of schemas (section 5.3.1), of which the first
            // describes the element the template samples.
            case: "an array whose items are declared as a tuple",
            schema: json!({ "type": "array", "items": [{ "type": "integer" }] }),
            element: "0".to_string(),
            loops: 1,
        },
        ArrayRow {
            // Section 7: an in-document reference states the schema of the element.
            case: "an array whose items are stated by a reference",
            schema: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "array",
                "items": { "$ref": "#/definitions/Leg" },
                "definitions": {
                    "Leg": {
                        "type": "object",
                        "properties": { "length": { "type": "number" } }
                    }
                }
            }),
            element: ["{", "  \"length\" : 0.0", "}"].join("\n"),
            loops: 1,
        },
    ]
}

/// Where a schema declares an ARRAY, what is generated for it is the Velocity skeleton that
/// MAPS that array — the `##TODO:` comment, the `#foreach` over the array the incoming
/// payload carries, one sample element inside the loop, the separator that commas the
/// elements and the `#end` that closes it — and never a collapsed list of one element: how
/// many elements the mapped array has is the payload's to say, so a template that writes one
/// element out maps every payload onto a one-element array.
#[tokio::test]
async fn every_declared_array_is_generated_as_the_loop_that_maps_it() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the array templates api", "model-template-arrays").await;

    for (index, row) in array_rows().into_iter().enumerate() {
        let model_id = create_model(
            &emulator,
            row.case,
            &api_id,
            &format!("ArrayTemplate{index}"),
            Some("application/json"),
            &row.schema.to_string(),
        )
        .await;
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}/template"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetModelTemplate, got {status} {body}",
            row.case
        );
        let value = as_str(&body, "value");
        assert!(
            value.starts_with(PREAMBLE),
            "case {:?}: expected the template to open with {PREAMBLE:?}, got {value:?}",
            row.case
        );
        // One loop per declared array, and one separator inside each of them.
        for (part, expected) in [
            (FOREACH_TODO, row.loops),
            (FOREACH_OPEN, row.loops),
            (FOREACH_SEPARATOR, row.loops),
        ] {
            let counted = value.matches(part).count();
            assert_eq!(
                counted, expected,
                "case {:?}: expected {expected} occurrence(s) of {part:?}, got {counted} in \
                 {value:?}",
                row.case
            );
        }
        // The innermost loop wraps the sample element its `items` schema declares, with the
        // pieces around it in the order a Velocity reader needs them.
        let loop_text = array_loop(&row.element);
        assert!(
            value.contains(&loop_text),
            "case {:?}: expected the template to carry the loop {loop_text:?}, got {value:?}",
            row.case
        );
        // And nothing of the collapsed form a template that writes one element out has.
        let collapsed = format!("[ {} ]", row.element);
        assert!(
            !value.contains(&collapsed),
            "case {:?}: expected no collapsed array {collapsed:?}, got {value:?}",
            row.case
        );
        // The generated document is a Velocity template, so it is one the language reads:
        // this is the parser the gateways read a caller's own mapping template with.
        service_common::velocity_template::parse(&value).unwrap_or_else(|error| {
            panic!(
                "case {:?}: expected the generated template to parse as Velocity, got \
                 {error:?} for {value:?}",
                row.case
            )
        });
    }
}

/// The recorded case as whole text: a model whose schema declares an array at its root is
/// answered with the preamble and the skeleton, line for line.
#[tokio::test]
async fn a_root_array_is_generated_as_the_whole_skeleton() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the skeleton api", "model-template-skeleton").await;

    let case = "an array of strings at the document's root";
    let model_id = create_model(
        &emulator,
        case,
        &api_id,
        "RootArray",
        Some("application/json"),
        &json!({
            "$schema": "http://json-schema.org/draft-04/schema#",
            "title": "Tags",
            "type": "array",
            "items": { "type": "string" }
        })
        .to_string(),
    )
    .await;
    let (status, body) = call(
        &emulator,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}/template"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetModelTemplate, got {status} {body}"
    );
    let value = as_str(&body, "value");
    let expected = [
        PREAMBLE,
        "[",
        FOREACH_TODO,
        FOREACH_OPEN,
        " \"foo\" ",
        FOREACH_SEPARATOR,
        BLOCK_END,
        "]",
    ]
    .join("\n");
    assert_eq!(
        value, expected,
        "case {case:?}: expected the generated skeleton, got {value:?}"
    );
}

/// An array declaring no `items` constrains no element, so there is no element expression
/// for a loop to wrap and the sample is the empty array.
#[tokio::test]
async fn an_array_that_constrains_no_element_has_no_loop_to_generate() {
    let emulator = emulator();
    let api_id = create_api(&emulator, "the unconstrained api", "model-template-open").await;

    for (index, (case, schema)) in [
        ("an array declaring no items", json!({ "type": "array" })),
        (
            "an array property declaring no items",
            json!({ "type": "object", "properties": { "lines": { "type": "array" } } }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let model_id = create_model(
            &emulator,
            case,
            &api_id,
            &format!("Open{index}"),
            Some("application/json"),
            &schema.to_string(),
        )
        .await;
        let (status, body) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}/template"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetModelTemplate, got {status} {body}"
        );
        let value = as_str(&body, "value");
        assert!(
            value.contains("[ ]"),
            "case {case:?}: expected the empty array, got {value:?}"
        );
        assert!(
            !value.contains(FOREACH_OPEN),
            "case {case:?}: expected no loop over elements nothing is declared about, got \
             {value:?}"
        );
    }
}
