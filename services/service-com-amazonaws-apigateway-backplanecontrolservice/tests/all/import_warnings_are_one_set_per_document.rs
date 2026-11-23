// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the warning SET an OpenAPI document earns, asserted to be the SAME
//! set — in the same order — whichever import of this service reads that document:
//! `ImportDocumentationParts` (`PUT /restapis/{id}/documentation/parts`) and `ImportRestApi`
//! (`POST /restapis?mode=import`).
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and both import
//! callers are this crate's existing harness.
//!
//! The RULE under test: an import reports every diagnostic the submitted document earns —
//! one line per PLACE an unresolvable model reference is resolved, then one line per media
//! type and method that reference left with no readable format — and both imports take "a
//! JSON object" of the one OpenAPI shape, so one document earns one list whichever of them
//! reads it. `failOnWarnings` turns that same list into the modelled 400 under API
//! Gateway's fixed lead, one tab-indented line per warning, before anything is written.
//!
//! The rows differ along what the rule generalises over: the spelling of the declaration
//! (OpenAPI 3's `content` map and Swagger 2.0's bare `schema`), where it sits (a response
//! and a request payload), the media type and the verb it is declared for, how many media
//! types of one declaration name the reference, how many methods carry one, a reference
//! pointing outside the document altogether, a reference the document does resolve, and a
//! document declaring no reference at all.

use serde_json::{json, Value};

use crate::documentation_part_import_warnings::document_with;
use crate::documentation_parts::{import_parts, listed_entities};
use crate::rest_api_create_from_import::import;
use crate::rest_api_import_model_reference_warnings::{refusal, unreadable, unresolved};
use crate::rest_api_lifecycle::{create_api, Gateway};

/// One row: the document both imports read, and the warnings it earns in order.
struct Row {
    case: &'static str,
    document: Value,
    warnings: Vec<String>,
}

/// The documents every case runs over.
fn table() -> Vec<Row> {
    vec![
        Row {
            case: "an OpenAPI 3 response schema keyed by one media type",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "paths": { "/pets": { "get": { "responses": { "200": {
                    "description": "ok",
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/DoesNotExist" },
                    } },
                } } } } },
                "components": { "schemas": {} },
            }),
            warnings: vec![
                unresolved("DoesNotExist"),
                unresolved("DoesNotExist"),
                unreadable("application/json", "GET"),
            ],
        },
        Row {
            case: "an OpenAPI 3 request payload of another media type and another verb",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "paths": { "/orders": { "put": {
                    "requestBody": { "content": { "application/xml": {
                        "schema": { "$ref": "#/components/schemas/Absent" },
                    } } },
                    "responses": { "200": { "description": "ok" } },
                } } },
            }),
            warnings: vec![
                unresolved("Absent"),
                unresolved("Absent"),
                unreadable("application/xml", "PUT"),
            ],
        },
        Row {
            case: "a Swagger 2.0 response schema, whose media type the document produces",
            document: json!({
                "swagger": "2.0",
                "info": { "title": "one-set", "version": "1.0" },
                "produces": ["text/plain"],
                "paths": { "/pets": { "head": { "responses": { "200": {
                    "description": "ok",
                    "schema": { "$ref": "#/definitions/Gone" },
                } } } } },
            }),
            warnings: vec![unresolved("Gone"), unreadable("text/plain", "HEAD")],
        },
        Row {
            case: "one reference named by two media types of one response",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "paths": { "/both": { "get": { "responses": { "200": {
                    "description": "ok",
                    "content": {
                        "application/json": { "schema": { "$ref": "#/components/schemas/Twice" } },
                        "application/xml": { "schema": { "$ref": "#/components/schemas/Twice" } },
                    },
                } } } } },
            }),
            warnings: vec![
                unresolved("Twice"),
                unresolved("Twice"),
                unresolved("Twice"),
                unreadable("application/json", "GET"),
                unreadable("application/xml", "GET"),
            ],
        },
        Row {
            case: "two methods, each declaring the same unresolvable model",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "paths": {
                    "/first": { "get": { "responses": { "200": {
                        "description": "ok",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/Shared" },
                        } },
                    } } } },
                    "/second": { "delete": { "responses": { "200": {
                        "description": "ok",
                        "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/Shared" },
                        } },
                    } } } },
                },
            }),
            warnings: vec![
                unresolved("Shared"),
                unresolved("Shared"),
                unresolved("Shared"),
                unreadable("application/json", "GET"),
                unreadable("application/json", "DELETE"),
            ],
        },
        Row {
            case: "a reference into another document, which an import has no access to",
            document: json!({
                "swagger": "2.0",
                "info": { "title": "one-set", "version": "1.0" },
                "consumes": ["application/x-www-form-urlencoded"],
                "paths": { "/elsewhere": { "post": {
                    "parameters": [{
                        "name": "body",
                        "in": "body",
                        "schema": { "$ref": "shared.json#/definitions/Thing" },
                    }],
                    "responses": { "200": { "description": "ok" } },
                } } },
            }),
            warnings: vec![
                unresolved("Thing"),
                unreadable("application/x-www-form-urlencoded", "POST"),
            ],
        },
        Row {
            case: "a reference the document does declare",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "components": { "schemas": { "Order": { "type": "object" } } },
                "paths": { "/orders": { "get": { "responses": { "200": {
                    "description": "ok",
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/Order" },
                    } },
                } } } } },
            }),
            warnings: Vec::new(),
        },
        Row {
            case: "a document declaring no model reference at all",
            document: json!({
                "openapi": "3.0.1",
                "info": { "title": "one-set", "version": "1.0" },
                "paths": { "/plain": { "get": { "responses": { "204": {
                    "description": "no content",
                } } } } },
            }),
            warnings: Vec::new(),
        },
    ]
}

/// The warnings an answered import reported, as plain strings; an absent member is no
/// warnings, because the service omits an empty list.
fn reported(body: &Value, case: &str) -> Vec<String> {
    match &body["warnings"] {
        Value::Null => Vec::new(),
        Value::Array(warnings) => warnings
            .iter()
            .map(|warning| {
                warning
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case}: expected a string warning, got {warning:?}")
                    })
                    .to_string()
            })
            .collect(),
        other => panic!("case {case}: expected warnings to be a list, got {other:?}"),
    }
}

/// One documentation entry, as the extension carries it.
fn entry(location: Value, description: &str) -> (Value, Value) {
    (location, json!({ "description": description }))
}

#[tokio::test]
async fn one_document_earns_one_warning_list_whichever_import_reads_it() {
    let gateway = Gateway::new();
    let documented = entry(json!({ "type": "API" }), "the api");
    for row in table() {
        // The documentation import, served: it reports exactly that list, in that order.
        let (api, _root) = create_api(&gateway, "import-warning-set").await;
        let body = document_with(&row.document, std::slice::from_ref(&documented));
        let answer = import_parts(&gateway, &api, None, false, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {}: ImportDocumentationParts expected 200, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        let parts_warnings = reported(&answer.body, row.case);
        assert_eq!(
            parts_warnings, row.warnings,
            "case {}: expected ImportDocumentationParts to report {:?}, got {parts_warnings:?}",
            row.case, row.warnings
        );

        // The definition import, served, over the SAME document: one document, one list.
        let created = import(&gateway, &row.document.to_string(), false, &[]).await;
        assert_eq!(
            created.status, 201,
            "case {}: ImportRestApi expected 201, got {} with {:?}",
            row.case, created.status, created.body
        );
        let definition_warnings = reported(&created.body, row.case);
        assert_eq!(
            definition_warnings, parts_warnings,
            "case {}: expected both imports to report one list {parts_warnings:?}, got \
             {definition_warnings:?} from ImportRestApi",
            row.case
        );
    }
}

#[tokio::test]
async fn fail_on_warnings_states_that_whole_list_and_writes_nothing() {
    let gateway = Gateway::new();
    let documented = entry(json!({ "type": "API" }), "the api");
    for row in table() {
        let (api, _root) = create_api(&gateway, "import-warning-refusal").await;
        // A documentation set the refused import must leave exactly as it was, so "nothing
        // was written" is asserted against a set that is not empty.
        let seeded = import_parts(
            &gateway,
            &api,
            None,
            false,
            &document_with(
                &json!({ "openapi": "3.0.1", "info": { "title": "seed", "version": "1.0" } }),
                std::slice::from_ref(&documented),
            ),
        )
        .await;
        assert_eq!(
            seeded.status, 200,
            "case {}: the seeding import expected 200, got {} with {:?}",
            row.case, seeded.status, seeded.body
        );

        let imported = entry(
            json!({ "type": "RESOURCE", "path": "/" }),
            "the root resource",
        );
        let body = document_with(&row.document, std::slice::from_ref(&imported));
        let answer = import_parts(&gateway, &api, None, true, &body).await;
        if row.warnings.is_empty() {
            // The rule's own boundary: a document that earns no warning is imported
            // whichever way the flag is set.
            assert_eq!(
                answer.status, 200,
                "case {}: a document that warns about nothing must import, got {} with {:?}",
                row.case, answer.status, answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 400,
            "case {}: expected the import to be refused, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        assert_eq!(
            answer.error.as_deref(),
            Some("BadRequestException"),
            "case {}: expected BadRequestException, got {:?}",
            row.case,
            answer.error
        );
        let want = refusal(&row.warnings);
        let stated = answer.body["message"].as_str().unwrap_or_default();
        assert_eq!(
            stated, want,
            "case {}: expected the refusal {want:?}, got {stated:?}",
            row.case
        );
        let after = listed_entities(&gateway, &api, row.case).await;
        assert_eq!(
            after,
            vec![documented.clone()],
            "case {}: expected the refused import to write nothing, got {after:?}",
            row.case
        );
    }
}
