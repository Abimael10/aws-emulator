// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WARNINGS a definition import reports for a model reference
//! the posted document does not resolve, and for the `failOnWarnings` refusal that states
//! them: `ImportRestApi` served (`warnings`) and refused (`BadRequestException`).
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the import
//! helpers are this crate's existing harness.
//!
//! The RULE under test: an import reports an unresolvable model reference ONCE PER PLACE
//! it resolves it — a schema keyed BY MEDIA TYPE (OpenAPI 3's `content` map) is resolved
//! both for the model that media type names and for the binding of that model to the
//! method, while Swagger 2.0's bare `schema`, whose media type comes from
//! `produces`/`consumes`, is resolved once — and then reports the FORMAT each method is
//! left without, once per media type and method. `failOnWarnings` states exactly that
//! list, in that order, under API Gateway's fixed lead and one tab-indented line per
//! warning, and creates no API.
//!
//! The rows differ along what the rule generalises over: the spelling of the declaration
//! (`content` and the bare `schema`), where it sits (a response and a request payload),
//! how many media types of one declaration name the reference, the media type and the verb
//! it is declared for, how many methods carry one, and a reference the document does
//! resolve. A reference the document repeats is resolved once where it is carried, so a
//! repeat adds its own media-type binding and nothing else.

use crate::rest_api_create_from_import::{api_count, import};
use crate::rest_api_lifecycle::{assert_failure, Gateway};

/// The sentence an import states for a model reference it could not resolve, by the
/// MODEL's own name: fixed text with the name of the model the reference pointed at.
///
/// `pub(crate)` so the sibling `refused_control_plane_writes` suite, which drives this same
/// refusal as one row of its table, states the sentence the one way rather than keeping a
/// second copy of it.
pub(crate) fn unresolved(model: &str) -> String {
    format!("Reference to model '{model}' not found. Ignoring.")
}

/// The sentence an import states for the FORMAT a method is left without once the model
/// of one of its media types resolved to nothing: fixed text with the media type and the
/// method's verb.
///
/// `pub(crate)` for the reason [`unresolved`] is.
pub(crate) fn unreadable(media_type: &str, verb: &str) -> String {
    format!("Invalid format for model {media_type} for method {verb}")
}

/// The `failOnWarnings` refusal for `warnings`: API Gateway's fixed lead and one
/// tab-indented line per warning, in the order the served import reports them.
///
/// `pub(crate)` for the reason [`unresolved`] is.
pub(crate) fn refusal(warnings: &[String]) -> String {
    let mut message = String::from("Warnings found during import:");
    for warning in warnings {
        message.push_str("\n\t");
        message.push_str(warning);
    }
    message
}

/// One row: the document posted, and the warnings it earns in order.
struct Row {
    case: &'static str,
    document: String,
    warnings: Vec<String>,
}

#[tokio::test]
async fn an_unresolvable_model_reference_is_reported_once_per_place_the_import_resolves_it() {
    let rows = vec![
        Row {
            case: "an OpenAPI 3 response schema keyed by one media type",
            document: r##"{"openapi": "3.0.1", "info": {"title": "warn", "version": "1.0"},
                "paths": {"/imported": {"post": {"responses": {"200": {"description": "ok",
                    "content": {"application/json": {"schema":
                        {"$ref": "#/components/schemas/Item"}}}}},
                    "x-amazon-apigateway-integration": {"type": "mock",
                        "requestTemplates": {"application/json": "{\"statusCode\":200}"}}}}},
                "components": {"schemas": {}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Item"),
                unresolved("Item"),
                unreadable("application/json", "POST"),
            ],
        },
        Row {
            case: "a Swagger 2.0 response schema, whose media type the document produces",
            document: r##"{"swagger": "2.0", "info": {"title": "warn", "version": "1.0"},
                "produces": ["application/json"],
                "paths": {"/imported": {"get": {"responses": {"200": {"description": "ok",
                    "schema": {"$ref": "#/definitions/Gone"}}}}}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Gone"),
                unreadable("application/json", "GET"),
            ],
        },
        Row {
            case: "an OpenAPI 3 request payload of another media type and another verb",
            document: r##"{"openapi": "3.0.1", "info": {"title": "warn", "version": "1.0"},
                "paths": {"/orders": {"put": {"requestBody": {"content":
                    {"application/xml": {"schema": {"$ref": "#/components/schemas/Absent"}}}},
                    "responses": {"200": {"description": "ok"}}}}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Absent"),
                unresolved("Absent"),
                unreadable("application/xml", "PUT"),
            ],
        },
        Row {
            case: "one reference named by two media types of one response",
            document: r##"{"openapi": "3.0.1", "info": {"title": "warn", "version": "1.0"},
                "paths": {"/both": {"get": {"responses": {"200": {"description": "ok",
                    "content": {
                        "application/json": {"schema": {"$ref": "#/components/schemas/Twice"}},
                        "application/xml": {"schema": {"$ref": "#/components/schemas/Twice"}}}}}}}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Twice"),
                unresolved("Twice"),
                unresolved("Twice"),
                unreadable("application/json", "GET"),
                unreadable("application/xml", "GET"),
            ],
        },
        Row {
            case: "a reference into another document, which an import has no access to",
            document: r##"{"swagger": "2.0", "info": {"title": "warn", "version": "1.0"},
                "consumes": ["text/plain"],
                "paths": {"/elsewhere": {"post": {"parameters": [{"name": "body",
                    "in": "body", "schema": {"$ref": "shared.json#/definitions/Thing"}}],
                    "responses": {"200": {"description": "ok"}}}}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Thing"),
                unreadable("text/plain", "POST"),
            ],
        },
        Row {
            case: "two methods, each declaring the same unresolvable model",
            document: r##"{"openapi": "3.0.1", "info": {"title": "warn", "version": "1.0"},
                "paths": {
                    "/first": {"get": {"responses": {"200": {"description": "ok",
                        "content": {"application/json": {"schema":
                            {"$ref": "#/components/schemas/Shared"}}}}}}},
                    "/second": {"delete": {"responses": {"200": {"description": "ok",
                        "content": {"application/json": {"schema":
                            {"$ref": "#/components/schemas/Shared"}}}}}}}}}"##
                .to_string(),
            warnings: vec![
                unresolved("Shared"),
                unresolved("Shared"),
                unresolved("Shared"),
                unreadable("application/json", "GET"),
                unreadable("application/json", "DELETE"),
            ],
        },
        Row {
            case: "a reference the document does declare",
            document: r##"{"openapi": "3.0.1", "info": {"title": "warn", "version": "1.0"},
                "components": {"schemas": {"Order": {"type": "object"}}},
                "paths": {"/orders": {"get": {"responses": {"200": {"description": "ok",
                    "content": {"application/json": {"schema":
                        {"$ref": "#/components/schemas/Order"}}}}}}}}}"##
                .to_string(),
            warnings: Vec::new(),
        },
    ];

    let gateway = Gateway::new();
    for row in &rows {
        // Served, the import reports exactly that list, in that order.
        let created = import(&gateway, &row.document, false, &[]).await;
        assert_eq!(
            created.status, 201,
            "case {}: ImportRestApi expected 201, got {} with {:?}",
            row.case, created.status, created.body
        );
        let reported: Vec<String> = created.body["warnings"]
            .as_array()
            .map(|warnings| {
                warnings
                    .iter()
                    .filter_map(|warning| warning.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, row.warnings,
            "case {}: expected the warnings {:?}, got {reported:?}",
            row.case, row.warnings
        );

        // Refused, it states the same list under the fixed lead — and creates no API. A
        // document that earns no warning is imported whichever way the flag is set, which
        // is the rule's own boundary.
        let before = api_count(&gateway, row.case).await;
        let answer = import(&gateway, &row.document, true, &[]).await;
        if row.warnings.is_empty() {
            assert_eq!(
                answer.status, 201,
                "case {}: a document that warns about nothing must import, got {} with {:?}",
                row.case, answer.status, answer.body
            );
            continue;
        }
        assert_failure(
            &answer,
            400,
            "BadRequestException",
            &format!("{} with failonwarnings", row.case),
        );
        let want = refusal(&row.warnings);
        let stated = answer.body["message"].as_str().unwrap_or_default();
        assert_eq!(
            stated, want,
            "case {}: expected the refusal {want:?}, got {stated:?}",
            row.case
        );
        let after = api_count(&gateway, row.case).await;
        assert_eq!(
            after, before,
            "case {}: the refused import must create no API, expected {before} APIs, got \
             {after}",
            row.case
        );
    }
}
