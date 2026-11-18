// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `Model` family: `CreateModel` and `GetModel`.
//!
//! The `CreateModel`/`GetModel` wire helpers below are this crate's `Model` harness and
//! are shared with the `delete_model` suite, so every suite of the family drives the one
//! wire path.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. [`Gateway`], `create_api`, `string_member` and
//! `assert_failure` are this crate's one wire-test harness, reused from
//! `rest_api_lifecycle`.
//!
//! The RULES these cases assert, over SETS of inputs rather than one recorded call:
//!
//! * a create stores the model its request describes and a read reports that record back
//!   member for member, over a table spanning the content-type families a payload can
//!   carry, which optional members the request sets, and the schema forms a document
//!   takes;
//! * a request that does not DESCRIBE a payload is refused with the modelled 400 and
//!   stores nothing — a `name` that is not alphanumeric, a `contentType` that is not a
//!   media type, a `schema` that is not a schema document for a model typed as JSON, and
//!   a schema one character past the published 400 KB maximum;
//! * a model is identified by its NAME under the API that holds it, so a second create
//!   under one name is the modelled 409 and two APIs may each hold a model of one name;
//! * that 409 carries the ONE fixed sentence for the condition, which varies neither with
//!   the name that collides, nor with what the second request says about the payload, nor
//!   with the API the name is held under;
//! * a schema is VALIDATED before it is stored, so a schema whose reference resolves to
//!   nothing — a fragment the document does not hold, or a model the API does not hold — is
//!   the modelled 400 and stores nothing;
//! * `flatten` resolves the model references a schema carries from stored state.

use serde_json::{json, Value};

use crate::rest_api_lifecycle::{assert_failure, create_api, string_member, Answer, Gateway};

/// The modelled error codes these cases assert.
const BAD_REQUEST: &str = "BadRequestException";
const CONFLICT: &str = "ConflictException";
const NOT_FOUND: &str = "NotFoundException";

/// From `CreateModel$schema`'s own documentation: "The maximum size of the model is 400
/// KB." Written out here rather than read from the service, so the test checks the bound
/// instead of restating it.
///
/// `pub(crate)` so a sibling suite of the family asserts the one published bound rather
/// than keeping its own copy (the `update_model` suite does).
pub(crate) const MAX_SCHEMA_CHARS: usize = 400 * 1024;

/// `CreateModel` on `api`, with `request` as its body.
pub async fn create_model(gateway: &Gateway, api: &str, request: Value) -> Answer {
    gateway
        .call("POST", &format!("/restapis/{api}/models"), Some(request))
        .await
}

/// `CreateModel`, asserting the modelled 201 and answering the body it reported.
pub async fn created_model(gateway: &Gateway, api: &str, case: &str, request: Value) -> Value {
    let answer = create_model(gateway, api, request).await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected 201 from CreateModel, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// `GetModel` for `name` on `api`, resolving model references when `flatten` is set.
pub async fn get_model(gateway: &Gateway, api: &str, name: &str, flatten: bool) -> Answer {
    let uri = if flatten {
        format!("/restapis/{api}/models/{name}?flatten=true")
    } else {
        format!("/restapis/{api}/models/{name}")
    };
    gateway.call("GET", &uri, None).await
}

/// The names of the models API Gateway creates every REST API WITH, in creation order,
/// read FROM the service rather than copied, so a suite listing an API's models cannot
/// drift from what a create seeds.
///
/// `pub(crate)` because every suite that asserts a `GetModels` page has to account for
/// them: they are stored models of the API from the moment it exists, so they precede
/// whatever the suite created.
pub(crate) fn default_model_names() -> Vec<String> {
    service_com_amazonaws_apigateway_backplanecontrolservice::state::default_models::default_model_names()
}

/// `created` prefixed by [`default_model_names`]: the model names an API holds in creation
/// order when a suite has created `created` on it.
pub(crate) fn with_default_models<T: AsRef<str>>(created: &[T]) -> Vec<String> {
    let mut names = default_model_names();
    names.extend(created.iter().map(|name| name.as_ref().to_string()));
    names
}

/// A JSON Schema document of `properties` named properties, as a caller writes one.
///
/// `pub(crate)` for the reason [`MAX_SCHEMA_CHARS`] is: one spelling of a schema document
/// for every suite of the `Model` family.
pub(crate) fn json_schema(title: &str, properties: usize) -> String {
    let members: serde_json::Map<String, Value> = (0..properties)
        .map(|index| (format!("member{index}"), json!({ "type": "string" })))
        .collect();
    json!({
        "$schema": "http://json-schema.org/draft-04/schema#",
        "title": title,
        "type": "object",
        "properties": members,
    })
    .to_string()
}

/// A JSON Schema document whose text is exactly `chars` characters long, so a row can sit
/// on the published maximum and one character past it.
///
/// `pub(crate)` for the reason [`MAX_SCHEMA_CHARS`] is.
pub(crate) fn schema_of_length(chars: usize) -> String {
    let envelope = json!({ "type": "object", "title": "" }).to_string();
    let padding = chars.saturating_sub(envelope.chars().count());
    json!({ "type": "object", "title": "x".repeat(padding) }).to_string()
}

/// One accepted create: the case, the request, and the `Model` the service must report.
struct Accepted {
    case: &'static str,
    request: Value,
    reported: Value,
}

/// The table every accepted create runs over. Rows differ along what the rule
/// generalises over: the content-type family of the payload the model describes
/// (`application/json`, a `+json` structured syntax suffix, a media type with a
/// parameter, XML, plain text), which optional members the request sets (`description`,
/// `schema`, neither), and the form the schema document takes (an object, a nested
/// object, a document that is not JSON at all under a content type that is not JSON).
fn accepted_table() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "a JSON model with every member set",
            request: json!({
                "name": "Pet",
                "contentType": "application/json",
                "description": "a pet",
                "schema": json_schema("Pet", 2),
            }),
            reported: json!({
                "name": "Pet",
                "contentType": "application/json",
                "description": "a pet",
                "schema": json_schema("Pet", 2),
            }),
        },
        Accepted {
            case: "a JSON model with only its required members",
            request: json!({ "name": "Bare", "contentType": "application/json" }),
            reported: json!({ "name": "Bare", "contentType": "application/json" }),
        },
        Accepted {
            case: "a model whose content type carries the +json suffix",
            request: json!({
                "name": "Suffixed",
                "contentType": "application/vnd.pet+json",
                "schema": json_schema("Suffixed", 1),
            }),
            reported: json!({
                "name": "Suffixed",
                "contentType": "application/vnd.pet+json",
                "schema": json_schema("Suffixed", 1),
            }),
        },
        Accepted {
            case: "a model whose content type carries a parameter",
            request: json!({
                "name": "Parameterized",
                "contentType": "application/json; charset=utf-8",
                "schema": json_schema("Parameterized", 0),
            }),
            reported: json!({
                "name": "Parameterized",
                "contentType": "application/json; charset=utf-8",
                "schema": json_schema("Parameterized", 0),
            }),
        },
        Accepted {
            // The JSON Schema sentence is scoped to models typed as JSON, so a model of
            // another content type keeps the body it was given whatever its syntax.
            case: "an XML model whose schema is not JSON",
            request: json!({
                "name": "PetXml",
                "contentType": "application/xml",
                "schema": "<xs:schema><xs:element name=\"pet\"/></xs:schema>",
                "description": "a pet, in XML",
            }),
            reported: json!({
                "name": "PetXml",
                "contentType": "application/xml",
                "schema": "<xs:schema><xs:element name=\"pet\"/></xs:schema>",
                "description": "a pet, in XML",
            }),
        },
        Accepted {
            case: "a plain-text model with no schema and a description",
            request: json!({
                "name": "Plain",
                "contentType": "text/plain",
                "description": "an opaque payload",
            }),
            reported: json!({
                "name": "Plain",
                "contentType": "text/plain",
                "description": "an opaque payload",
            }),
        },
        Accepted {
            case: "a name of digits and letters at the published maximum schema size",
            request: json!({
                "name": "Model2",
                "contentType": "application/json",
                "schema": schema_of_length(MAX_SCHEMA_CHARS),
            }),
            reported: json!({
                "name": "Model2",
                "contentType": "application/json",
                "schema": schema_of_length(MAX_SCHEMA_CHARS),
            }),
        },
    ]
}

/// `body` with the service-minted `id` dropped, so a case compares the members the
/// request decides.
fn without_id(body: &Value) -> Value {
    crate::rest_api_lifecycle::without(body, &["id"])
}

#[tokio::test]
async fn a_created_model_reads_back_member_for_member() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models").await;

    // Every row is created on ONE API, so each read runs against a store holding models
    // of every other shape beside the one it names.
    let mut created: Vec<(&str, String, Value)> = Vec::new();
    for row in accepted_table() {
        let body = created_model(&gateway, &api, row.case, row.request).await;
        assert_eq!(
            without_id(&body),
            row.reported,
            "case {}: expected the created model {:?}, got {:?}",
            row.case,
            row.reported,
            without_id(&body)
        );
        let id = string_member(&body, "id", row.case);
        assert!(
            !id.is_empty(),
            "case {}: expected a minted id, got {id:?}",
            row.case
        );
        created.push((row.case, id, body));
    }

    // Every id is the model's own.
    for (case, id, _) in &created {
        let clashing = created
            .iter()
            .filter(|(_, other, _)| other == id)
            .count();
        assert_eq!(
            clashing, 1,
            "case {case}: expected the id {id:?} to name one model, got {clashing}"
        );
    }

    // The read reports the stored record, and reading it twice reports the same record:
    // a read changes nothing.
    for (case, _, body) in &created {
        let name = string_member(body, "name", case);
        for round in 0..2 {
            let read = get_model(&gateway, &api, &name, false).await;
            assert_eq!(
                read.status, 200,
                "case GetModel({case}) round {round}: expected 200, got {} with {:?}",
                read.status, read.body
            );
            assert_eq!(
                &read.body, body,
                "case GetModel({case}) round {round}: expected the created model {body:?}, \
                 got {:?}",
                read.body
            );
        }
    }
}

#[tokio::test]
async fn a_request_that_does_not_describe_a_payload_is_refused_and_stores_nothing() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models-refused").await;

    // Rows differ along WHICH member does not describe a payload and HOW: a name outside
    // the documented alphanumeric form (a separator, a space, an underscore, a non-ASCII
    // letter, empty), a contentType that is not a media type (too many parts, no
    // subtype, an empty subtype, a space inside a token, empty), a schema that is not a
    // schema document under a JSON content type (truncated, not JSON at all, a JSON
    // value that is not an object) — including under a `+json` content type, which is
    // the same rule one level more general — and a schema one character past the
    // published maximum.
    let rows: Vec<(String, Value)> = vec![
        ("a name with a separator", json!({ "name": "Pet-Model", "contentType": "application/json" })),
        ("a name with a space", json!({ "name": "Pet Model", "contentType": "application/json" })),
        ("a name with an underscore", json!({ "name": "Pet_Model", "contentType": "application/json" })),
        ("a non-ASCII name", json!({ "name": "Pét", "contentType": "application/json" })),
        ("an empty name", json!({ "name": "", "contentType": "application/json" })),
        (
            "a content type of three parts",
            json!({ "name": "Pet", "contentType": "not/a/media/type" }),
        ),
        (
            "a content type with no subtype",
            json!({ "name": "Pet", "contentType": "application" }),
        ),
        (
            "a content type with an empty subtype",
            json!({ "name": "Pet", "contentType": "application/" }),
        ),
        (
            "a content type with a space in its type",
            json!({ "name": "Pet", "contentType": "appli cation/json" }),
        ),
        ("an empty content type", json!({ "name": "Pet", "contentType": "" })),
        (
            "a truncated JSON schema",
            json!({ "name": "Pet", "contentType": "application/json", "schema": "{" }),
        ),
        (
            "a schema that is not JSON",
            json!({ "name": "Pet", "contentType": "application/json", "schema": "not a schema" }),
        ),
        (
            "a schema that is a JSON array",
            json!({ "name": "Pet", "contentType": "application/json", "schema": "[{\"type\":\"object\"}]" }),
        ),
        (
            "a schema that is a JSON string",
            json!({ "name": "Pet", "contentType": "application/json", "schema": "\"object\"" }),
        ),
        (
            // The same schema rule under a content type that is JSON by its structured
            // syntax suffix rather than by its subtype.
            "a truncated schema under a +json content type",
            json!({ "name": "Pet", "contentType": "application/vnd.pet+json", "schema": "{" }),
        ),
        (
            "a schema one character past the published maximum",
            json!({
                "name": "Pet",
                "contentType": "application/json",
                "schema": schema_of_length(MAX_SCHEMA_CHARS + 1),
            }),
        ),
    ]
    .into_iter()
    .map(|(case, request)| (case.to_string(), request))
    .collect();

    for (case, request) in &rows {
        let answer = create_model(&gateway, &api, request.clone()).await;
        assert_failure(&answer, 400, BAD_REQUEST, case);
        assert!(
            answer.body["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "case {case}: expected a message naming what to fix, got {:?}",
            answer.body
        );
    }

    // None of the refusals stored a model: the name every row used reads back as missing,
    // so a later create under it is still free.
    for name in ["Pet", "PetModel"] {
        let read = get_model(&gateway, &api, name, false).await;
        assert_failure(
            &read,
            404,
            NOT_FOUND,
            &format!("GetModel({name:?}) after the refusals"),
        );
    }
}

#[tokio::test]
async fn a_model_is_identified_by_its_name_under_the_api_that_holds_it() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models-identity").await;
    let (other, _other_root) = create_api(&gateway, "models-identity-other").await;

    let stored = created_model(
        &gateway,
        &api,
        "the first create",
        json!({
            "name": "Pet",
            "contentType": "application/json",
            "schema": json_schema("Pet", 1),
        }),
    )
    .await;

    // A second create under the same name is the modelled conflict, whatever the rest of
    // the request says — the name is the identity, not the body.
    for (case, request) in [
        (
            "the same request again",
            json!({
                "name": "Pet",
                "contentType": "application/json",
                "schema": json_schema("Pet", 1),
            }),
        ),
        (
            "the same name, a different content type and schema",
            json!({ "name": "Pet", "contentType": "text/plain", "description": "second" }),
        ),
    ] {
        let answer = create_model(&gateway, &api, request).await;
        assert_failure(&answer, 409, CONFLICT, case);
    }

    // The conflict changed nothing: the model the first create stored still reads back.
    let read = get_model(&gateway, &api, "Pet", false).await;
    assert_eq!(
        read.body, stored,
        "case GetModel after the conflicts: expected {stored:?}, got {:?}",
        read.body
    );

    // A model belongs to ONE API: the other API may hold its own model of that name, and
    // its record is its own.
    let elsewhere = created_model(
        &gateway,
        &other,
        "the other API's model of the same name",
        json!({ "name": "Pet", "contentType": "application/xml" }),
    )
    .await;
    assert_ne!(
        string_member(&elsewhere, "id", "the other API's model"),
        string_member(&stored, "id", "the first create"),
        "case the other API's model: expected an id of its own"
    );
    assert_eq!(
        string_member(&elsewhere, "contentType", "the other API's model"),
        "application/xml",
        "case the other API's model: expected its own content type, got {elsewhere:?}"
    );

    // Identifiers that name no record, one unknown at a time.
    let missing = "zzzzzzzzzz";
    for (case, target_api, model) in [
        ("GetModel on an unknown API", missing, "Pet"),
        ("GetModel for an unknown model", api.as_str(), "Nope"),
        ("GetModel on an unknown API and model", missing, "Nope"),
    ] {
        assert_failure(
            &get_model(&gateway, target_api, model, false).await,
            404,
            NOT_FOUND,
            case,
        );
    }
    assert_failure(
        &create_model(
            &gateway,
            missing,
            json!({ "name": "Pet", "contentType": "application/json" }),
        )
        .await,
        404,
        NOT_FOUND,
        "CreateModel on an unknown API",
    );
}

/// The one sentence API Gateway refuses a second model under one name with: the resource
/// class and the scope a model name is unique within, and no value from the request.
///
/// `pub(crate)` so a sibling suite asserts the one sentence rather than keeping a second
/// copy of it.
pub(crate) const NAME_ALREADY_EXISTS: &str = "Model name already exists for this REST API";

#[tokio::test]
async fn a_duplicate_model_name_is_refused_with_one_fixed_sentence() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models-conflict-sentence").await;
    let (other, _other_root) = create_api(&gateway, "models-conflict-sentence-other").await;

    // Rows differ along what the sentence must NOT vary with: the form of the name that
    // collides (letters, digits, mixed case, one character, a long name), what the second
    // request says about the payload (its content type, its schema, its description), and
    // which API holds the model.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a name of letters, repeated verbatim",
            "Pet",
            json!({ "name": "Pet", "contentType": "application/json" }),
        ),
        (
            "a name of digits, with another content type",
            "12345",
            json!({ "name": "12345", "contentType": "application/xml" }),
        ),
        (
            "a mixed-case name, with a schema the first create did not carry",
            "PetOrder2",
            json!({
                "name": "PetOrder2",
                "contentType": "application/json",
                "schema": json_schema("PetOrder2", 3),
            }),
        ),
        (
            "a one-character name, with a description",
            "A",
            json!({ "name": "A", "contentType": "text/plain", "description": "second" }),
        ),
        (
            "a long name, under a +json content type",
            "M0123456789012345678901234567890123456789",
            json!({
                "name": "M0123456789012345678901234567890123456789",
                "contentType": "application/vnd.pet+json",
            }),
        ),
    ];

    for (case, name, second) in rows {
        // Both APIs hold a model of the name, so each refusal is decided inside the API
        // the request names and the sentence is the same in either scope.
        for scope in [api.as_str(), other.as_str()] {
            created_model(
                &gateway,
                scope,
                case,
                json!({ "name": name, "contentType": "application/json" }),
            )
            .await;
            let answer = create_model(&gateway, scope, second.clone()).await;
            assert_failure(&answer, 409, CONFLICT, case);
            assert_eq!(
                answer.body["message"].as_str(),
                Some(NAME_ALREADY_EXISTS),
                "case {case}: expected the message {NAME_ALREADY_EXISTS:?}, got {:?}",
                answer.body["message"]
            );
        }
    }
}

#[tokio::test]
async fn flatten_resolves_the_model_references_a_schema_carries() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models-flatten").await;

    // The reference form API Gateway addresses another model of the same API by: the
    // model's own URL under the API that holds it.
    let reference = |model: &str| format!("https://apigateway.amazonaws.com/restapis/{api}/models/{model}");

    let child_schema = json!({ "type": "object", "properties": { "leg": { "type": "string" } } });
    created_model(
        &gateway,
        &api,
        "the referenced model",
        json!({
            "name": "Leg",
            "contentType": "application/json",
            "schema": child_schema.to_string(),
        }),
    )
    .await;

    // Rows differ along WHERE the reference sits in the document and how many there are:
    // a referenced property, two of them, a reference inside a list, and a schema with no
    // reference at all.
    let rows: Vec<(&str, Value, Option<Value>)> = vec![
        (
            "one referenced property",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": reference("Leg") } },
            }),
            Some(json!({
                "type": "object",
                "properties": { "front": child_schema.clone() },
            })),
        ),
        (
            "two referenced properties",
            json!({
                "type": "object",
                "properties": {
                    "front": { "$ref": reference("Leg") },
                    "back": { "$ref": reference("Leg") },
                },
            }),
            Some(json!({
                "type": "object",
                "properties": {
                    "front": child_schema.clone(),
                    "back": child_schema.clone(),
                },
            })),
        ),
        (
            "a reference inside a list",
            json!({
                "type": "object",
                "properties": { "legs": { "type": "array", "items": { "$ref": reference("Leg") } } },
            }),
            Some(json!({
                "type": "object",
                "properties": { "legs": { "type": "array", "items": child_schema.clone() } },
            })),
        ),
        (
            // A reference that is no MODEL reference names no model of this API, so the
            // document is reported as stored: a JSON Pointer into the document itself,
            // which the document resolves on its own.
            "a schema whose only reference is internal",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": "#/definitions/leg" } },
                "definitions": { "leg": { "type": "object" } },
            }),
            None,
        ),
        (
            "a schema with no reference at all",
            json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
            None,
        ),
    ];

    for (index, (case, schema, flattened)) in rows.into_iter().enumerate() {
        let name = format!("Parent{index}");
        let stored = schema.to_string();
        created_model(
            &gateway,
            &api,
            case,
            json!({
                "name": name.clone(),
                "contentType": "application/json",
                "schema": stored.clone(),
            }),
        )
        .await;

        // Without `flatten` the schema is reported exactly as it was stored.
        let read = get_model(&gateway, &api, &name, false).await;
        assert_eq!(
            string_member(&read.body, "schema", case),
            stored,
            "case {case} unflattened: expected the stored schema, got {:?}",
            read.body["schema"]
        );

        // With `flatten` the references this API can resolve are inlined from stored
        // state, and a schema with none to resolve is reported as stored.
        let read = get_model(&gateway, &api, &name, true).await;
        let reported = string_member(&read.body, "schema", case);
        match flattened {
            Some(expected) => {
                let read_schema: Value = serde_json::from_str(&reported).unwrap_or_else(|_| {
                    panic!("case {case} flattened: expected a JSON schema, got {reported:?}")
                });
                assert_eq!(
                    read_schema, expected,
                    "case {case} flattened: expected {expected:?}, got {read_schema:?}"
                );
            }
            None => assert_eq!(
                reported, stored,
                "case {case} flattened: expected the stored schema, got {reported:?}"
            ),
        }
    }

    // A schema that references ITSELF is expanded once and then left as it stands, so the
    // read terminates and still reports a schema.
    let self_reference = json!({
        "type": "object",
        "properties": { "parent": { "$ref": reference("Cycle") } },
    })
    .to_string();
    created_model(
        &gateway,
        &api,
        "a self-referencing model",
        json!({
            "name": "Cycle",
            "contentType": "application/json",
            "schema": self_reference.clone(),
        }),
    )
    .await;
    let read = get_model(&gateway, &api, "Cycle", true).await;
    let reported = string_member(&read.body, "schema", "a self-referencing model");
    assert!(
        serde_json::from_str::<Value>(&reported).is_ok(),
        "case a self-referencing model: expected a JSON schema, got {reported:?}"
    );
}

/// The sentence API Gateway refuses a model whose schema does not VALIDATE with: the
/// validation result, whose one error names the model the schema's unresolved reference
/// stood for. Written out here rather than read from the service, so the test checks the
/// text instead of restating it.
fn invalid_model_specified(model_name: &str) -> String {
    format!(
        "Invalid model specified: Validation Result: warnings : [], errors : [Invalid model \
         specified: {model_name}]"
    )
}

/// `GetModels` on `api`, as the list a refused create must not appear in.
async fn model_names(gateway: &Gateway, api: &str) -> Vec<String> {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/models"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case GetModels: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["item"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|model| model["name"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_schema_whose_reference_resolves_to_nothing_is_refused() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "models-schema-validation").await;
    let (other, _other_root) = create_api(&gateway, "models-schema-validation-other").await;

    // The reference form API Gateway addresses another model of the same API by: the
    // model's own URL under the API that holds it.
    let reference = |scope: &str, model: &str| {
        format!("https://apigateway.amazonaws.com/restapis/{scope}/models/{model}")
    };

    created_model(
        &gateway,
        &api,
        "the referenced model",
        json!({
            "name": "Leg",
            "contentType": "application/json",
            "schema": json!({ "type": "object", "properties": { "length": { "type": "number" } } })
                .to_string(),
        }),
    )
    .await;
    // A model of the OTHER api, under a name THIS api does not hold: a reference naming it
    // names no model of the api the schema belongs to.
    created_model(
        &gateway,
        &other,
        "a model of another api",
        json!({
            "name": "Elsewhere",
            "contentType": "application/json",
            "schema": json!({ "type": "object" }).to_string(),
        }),
    )
    .await;

    // Rows differ along WHICH KIND of reference the schema states — a fragment of the
    // document itself, a model of this api — along WHERE it sits in the document (a
    // property, an array's items, a composition, the document root), along whether what it
    // names is there, and along the forms that name no schema this service addresses (a
    // model of another api, a URL of something else), which are stored. `true` is a create
    // that is refused, and names the model its unresolved reference stood for.
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "a property referencing a model the api holds",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": reference(&api, "Leg") } },
            }),
            None,
        ),
        (
            "a property referencing a model the api does not hold",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": reference(&api, "Absent") } },
            }),
            Some("Absent"),
        ),
        (
            "an array whose items reference a model the api does not hold",
            json!({
                "type": "object",
                "properties": {
                    "legs": { "type": "array", "items": { "$ref": reference(&api, "Gone") } },
                },
            }),
            Some("Gone"),
        ),
        (
            "a composition naming a model the api does not hold",
            json!({
                "type": "object",
                "allOf": [
                    { "$ref": reference(&api, "Leg") },
                    { "$ref": reference(&api, "Nope") },
                ],
            }),
            Some("Nope"),
        ),
        (
            "a document whose root is the reference",
            json!({ "$ref": reference(&api, "Missing") }),
            Some("Missing"),
        ),
        (
            "a fragment reference the document resolves",
            json!({
                "type": "object",
                "properties": { "pet": { "$ref": "#/definitions/Pet" } },
                "definitions": { "Pet": { "type": "object" } },
            }),
            None,
        ),
        (
            "a fragment reference the document does not hold",
            json!({
                "type": "object",
                "properties": { "pet": { "$ref": "#/definitions/Pet" } },
            }),
            Some("Pet"),
        ),
        (
            "a fragment reference naming a definition beside the ones declared",
            json!({
                "type": "object",
                "properties": { "pet": { "$ref": "#/definitions/Absent" } },
                "definitions": { "Pet": { "type": "object" } },
            }),
            Some("Absent"),
        ),
        (
            "a reference to the document itself",
            json!({
                "type": "object",
                "properties": { "child": { "$ref": "#" } },
            }),
            None,
        ),
        (
            "a reference to a model of ANOTHER api",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": reference(&other, "Elsewhere") } },
            }),
            None,
        ),
        (
            "a reference that is no model URL at all",
            json!({
                "type": "object",
                "properties": { "front": { "$ref": "urn:example:order" } },
            }),
            None,
        ),
        (
            "a schema carrying no reference at all",
            json!({ "type": "object", "properties": { "id": { "type": "string" } } }),
            None,
        ),
    ];

    for (index, (case, schema, named)) in rows.into_iter().enumerate() {
        let name = format!("Checked{index}");
        let answer = create_model(
            &gateway,
            &api,
            json!({
                "name": name.clone(),
                "contentType": "application/json",
                "schema": schema.to_string(),
            }),
        )
        .await;
        if let Some(named) = named {
            assert_failure(&answer, 400, BAD_REQUEST, case);
            // The refusal's one variable part is the model the unresolved reference STOOD
            // FOR — the name the document declares it under — rather than the pointer that
            // failed or the record being written.
            let expected = invalid_model_specified(named);
            assert_eq!(
                answer.body["message"].as_str(),
                Some(expected.as_str()),
                "case {case}: expected the message {expected:?}, got {:?}",
                answer.body["message"]
            );
            // Nothing was stored: the model is in neither the read nor the list.
            let read = get_model(&gateway, &api, &name, false).await;
            assert_failure(&read, 404, NOT_FOUND, case);
            assert!(
                !model_names(&gateway, &api).await.contains(&name),
                "case {case}: expected the refused model to be out of the collection"
            );
        } else {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected 201 from CreateModel, got {} with {:?}",
                answer.status, answer.body
            );
        }
    }

    // A schema referring to the very model being created resolves: the model is one of the
    // API's own by the time the reference is read.
    created_model(
        &gateway,
        &api,
        "a schema referring to itself by its own URL",
        json!({
            "name": "Recursive",
            "contentType": "application/json",
            "schema": json!({
                "type": "object",
                "properties": { "parent": { "$ref": reference(&api, "Recursive") } },
            })
            .to_string(),
        }),
    )
    .await;

    // The refusal is decided from STATE, not from the schema's text: the very document
    // refused above is accepted once the API holds the model it names.
    let schema = json!({
        "type": "object",
        "properties": { "front": { "$ref": reference(&api, "Absent") } },
    })
    .to_string();
    created_model(
        &gateway,
        &api,
        "the model the reference names",
        json!({
            "name": "Absent",
            "contentType": "application/json",
            "schema": json!({ "type": "object" }).to_string(),
        }),
    )
    .await;
    created_model(
        &gateway,
        &api,
        "the once-refused schema, now resolvable",
        json!({
            "name": "Resolvable",
            "contentType": "application/json",
            "schema": schema.clone(),
        }),
    )
    .await;
    let read = get_model(&gateway, &api, "Resolvable", false).await;
    assert_eq!(
        string_member(&read.body, "schema", "the once-refused schema"),
        schema,
        "case the once-refused schema: expected the stored document back, got {:?}",
        read.body["schema"]
    );

    // A model typed as something other than JSON carries its schema in that content type's
    // own language, which states no JSON Schema reference to resolve — whatever its text
    // happens to parse as.
    for (case, name, schema) in [
        (
            "an XML model naming a model the api does not hold",
            "Xml",
            "<xs:schema><xs:include schemaLocation=\"Nowhere\"/></xs:schema>".to_string(),
        ),
        (
            "an XML model whose schema text parses as a JSON reference",
            "XmlJson",
            json!({ "$ref": "#/definitions/Nowhere" }).to_string(),
        ),
    ] {
        created_model(
            &gateway,
            &api,
            case,
            json!({
                "name": name,
                "contentType": "application/xml",
                "schema": schema,
            }),
        )
        .await;
    }
}
