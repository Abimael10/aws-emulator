// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CANONICALIZATION of a `SchemaDefinition`: which submitted texts
//! the schema registry reads as the same schema document.
//!
//! The rules under test:
//!
//! 1. "The schema definition is sent to the Schema Registry, canonicalized, and hashed"
//!    (`smithy_query operation=GetSchemaByDefinition`), so a definition that differs from a
//!    stored one only in LAYOUT — the whitespace between its tokens, its indentation, its
//!    line breaks, a comment, the order an object's members are written in — is the same
//!    document: `RegisterSchemaVersion` adds no version and returns the id of the version
//!    already held ("if the same schema definition is already stored in Schema Registry as a
//!    version, the schema ID of the existing schema is returned to the caller"), and
//!    `GetSchemaByDefinition` resolves to that version.
//! 2. A definition that says something ELSE is another document, so it takes the schema
//!    set's next version number and an id of its own.
//!
//! Suite 1 is a TABLE whose rows differ along what the rule generalises over: the
//! `DataFormat` the schema set is written in, and which part of the layout the re-submitted
//! copy differs in. Every call goes over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, ok};

/// One schema set per `DataFormat`, with the definition its first version holds, the copy of
/// that definition which differs only in layout, and a definition that is another document.
///
/// (case, schema set, `DataFormat`, the first version's definition, a re-written copy of it,
/// another document)
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// The rows: one per format, each re-written in a way that format's own documents admit.
fn rows() -> Vec<Row> {
    vec![
        (
            "an Avro record re-spaced over several lines",
            "canonical-avro",
            "AVRO",
            r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"}]}"#,
            "{\n  \"type\" : \"record\",\n  \"name\" : \"User\",\n  \"fields\" : [\n    { \
             \"name\" : \"id\", \"type\" : \"string\" }\n  ]\n}\n",
            r#"{"type":"record","name":"User","fields":[{"name":"id","type":"long"}]}"#,
        ),
        (
            "a JSON Schema document re-spaced and its members re-ordered",
            "canonical-json",
            "JSON",
            r#"{"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}"#,
            "{\n  \"required\" : [ \"id\" ],\n  \"properties\" : { \"id\" : { \"type\" : \
             \"string\" } },\n  \"type\" : \"object\"\n}",
            r#"{"type":"object","properties":{"id":{"type":"number"}},"required":["id"]}"#,
        ),
        (
            "a protobuf message re-indented and commented",
            "canonical-protobuf",
            "PROTOBUF",
            "syntax = \"proto3\"; message User { string id = 1; int64 age = 2; }",
            "// the user record\nsyntax = \"proto3\";\n\nmessage User {\n    string  id  = 1;  \
             // its id\n    int64   age = 2;\n}\n",
            "syntax = \"proto3\"; message User { string id = 1; int64 age = 3; }",
        ),
    ]
}

/// The `VersionNumber`s `ListSchemaVersions` reports for `schema`, in the order it reports
/// them.
async fn version_numbers(router: &MultiServiceRouter, schema: &str) -> Vec<i64> {
    let listed = ok(
        router,
        "ListSchemaVersions",
        &json!({ "SchemaId": { "SchemaName": schema } }),
    )
    .await;
    listed
        .get("Schemas")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the listing must report Schemas, got {listed}"))
        .iter()
        .map(|entry| {
            entry
                .get("VersionNumber")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("every entry carries a VersionNumber, got {entry}"))
        })
        .collect()
}

/// The `SchemaVersionId` a response reports.
fn version_id(case: &str, body: &Value) -> String {
    body.get("SchemaVersionId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a SchemaVersionId, got {body}"))
        .to_string()
}

/// Rule 1: a copy differing only in layout is the SAME version — through the registration
/// and through the read by definition — and rule 2: another document is another version.
#[tokio::test]
async fn a_definition_re_written_in_the_same_document_resolves_to_the_same_version() {
    let router = emulator();

    for (case, schema, data_format, definition, re_written, other) in rows() {
        let created = ok(
            &router,
            "CreateSchema",
            &json!({
                "SchemaName": schema,
                "DataFormat": data_format,
                "Compatibility": "NONE",
                "SchemaDefinition": definition,
            }),
        )
        .await;
        let first = version_id(case, &created);

        // The registration of the re-written copy adds no version and reports the one held.
        let registered = ok(
            &router,
            "RegisterSchemaVersion",
            &json!({
                "SchemaId": { "SchemaName": schema },
                "SchemaDefinition": re_written,
            }),
        )
        .await;
        let reported = version_id(case, &registered);
        assert_eq!(
            reported, first,
            "case {case:?}: expected the version already held {first:?}, got {reported:?}"
        );
        assert_eq!(
            registered.get("VersionNumber").and_then(Value::as_i64),
            Some(1),
            "case {case:?}: expected the version already held, got {registered}"
        );
        let held = version_numbers(&router, schema).await;
        assert_eq!(
            held,
            vec![1],
            "case {case:?}: a re-written copy must add no version, got {held:?}"
        );

        // The read by definition resolves the same version from the re-written copy.
        let read = ok(
            &router,
            "GetSchemaByDefinition",
            &json!({
                "SchemaId": { "SchemaName": schema },
                "SchemaDefinition": re_written,
            }),
        )
        .await;
        let resolved = version_id(case, &read);
        assert_eq!(
            resolved, first,
            "case {case:?}: expected the read to resolve {first:?}, got {resolved:?}"
        );

        // Rule 2: a definition that says something else is another version, with an id of
        // its own, and the stored text of the first version is still the one it was
        // registered with.
        let another = ok(
            &router,
            "RegisterSchemaVersion",
            &json!({
                "SchemaId": { "SchemaName": schema },
                "SchemaDefinition": other,
            }),
        )
        .await;
        let second = version_id(case, &another);
        assert_ne!(
            second, first,
            "case {case:?}: another document must take an id of its own, got {second:?}"
        );
        assert_eq!(
            another.get("VersionNumber").and_then(Value::as_i64),
            Some(2),
            "case {case:?}: another document must take the next version number, got {another}"
        );

        let stored = ok(
            &router,
            "GetSchemaVersion",
            &json!({ "SchemaVersionId": first }),
        )
        .await;
        assert_eq!(
            stored.get("SchemaDefinition").and_then(Value::as_str),
            Some(definition),
            "case {case:?}: the version must keep the text it was registered with, got {stored}"
        );
    }
}

/// A definition no parser of the format reads is no document, so it is matched by text alone
/// — and a registry WRITE of one is refused outright, which is what leaves the registry
/// holding only documents it can canonicalize.
#[tokio::test]
async fn a_definition_that_is_no_document_of_the_format_is_refused_by_a_write() {
    let router = emulator();
    let schema = "canonical-refused";
    ok(
        &router,
        "CreateSchema",
        &json!({
            "SchemaName": schema,
            "DataFormat": "PROTOBUF",
            "Compatibility": "NONE",
            "SchemaDefinition": "syntax = \"proto3\"; message User { string id = 1; }",
        }),
    )
    .await;

    for (case, definition) in [
        ("prose", "not a protobuf document"),
        (
            "a message left open",
            "syntax = \"proto3\"; message User { string id = 1;",
        ),
    ] {
        let (status, refused) = call(
            &router,
            "RegisterSchemaVersion",
            &json!({
                "SchemaId": { "SchemaName": schema },
                "SchemaDefinition": definition,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {refused}"
        );
        assert!(
            error_code(&refused).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {refused}"
        );
    }

    let held = version_numbers(&router, schema).await;
    assert_eq!(
        held,
        vec![1],
        "a refused registration must add no version, got {held:?}"
    );
}
