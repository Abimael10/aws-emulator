// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CheckSchemaVersionValidity`: whether a `SchemaDefinition` is a
//! schema document of the `DataFormat` it was submitted for.
//!
//! The rules under test:
//!
//! 1. The answer is the READING of the definition by that data format's own parser: a
//!    definition the parser accepts is reported `Valid` true with no `Error`, and one it
//!    does not is reported `Valid` false carrying the parser's reading of where the text
//!    stops being a document of the format — on HTTP 200, because the model carries the
//!    outcome in `Valid`/`Error` rather than in an error
//!    (`smithy_query operation=CheckSchemaVersionValidity`). The reading keys on the
//!    `DataFormat` the request named, so one format's document submitted for another is not
//!    valid.
//! 2. It is the SAME reading a registry write applies to the definition it stores, so the
//!    `Error` this call reports for a definition is the reading the `Message` of the
//!    `InvalidInputException` a `CreateSchema` carrying that same definition is refused with
//!    states inside the registry's refusal frame.
//! 3. "This call has no side effects": a check writes nothing — no registry (not even the
//!    default one), no schema set and no version — whatever it answers.
//! 4. A protobuf definition's reading is the registry's own syntax-error clause: `Syntax
//!    error in :<line>:<column>: <what the document was required to carry>`, the name empty
//!    because a definition is submitted as text and no file, and the position counted from
//!    one and placed just past the character the parse stopped on.
//!
//! Suite 1 is a TABLE whose rows differ along what the rule generalises over: which of the
//! three modelled `DataFormat` values, whether the definition is a document of it, the class
//! of fault (text that is no document of the format at all, a document left open, a document
//! of ANOTHER format), the syntax level a protobuf document declares, and where in the
//! definition the fault sits. Every call goes over the wire through the shared [`crate::wire`]
//! harness.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use std::sync::Arc;

use service_com_amazonaws_glue_awsglue::state::ComAmazonawsGlueAwsglueState;

use crate::wire::{call, emulator, emulator_on, error_code, error_message, ok};

/// An Avro record document: a definition the `AVRO` format's parser accepts.
const AVRO: &str = r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"}]}"#;

/// A JSON Schema document: a definition the `JSON` format admits.
const JSON_SCHEMA: &str = r#"{"type":"object","properties":{"id":{"type":"string"}}}"#;

/// A proto3 message: a definition the `PROTOBUF` format's parser accepts.
const PROTOBUF: &str = "syntax = \"proto3\"; message User { string id = 1; }";

/// A proto2 message, for the row that shows the reading is not pinned to one syntax level.
const PROTOBUF_PROTO2: &str = "syntax = \"proto2\"; message User { required string id = 1; }";

/// The frame a registry WRITE states a definition fault in, around the `detail` the
/// format's own parser reported — the frame a refusal carries and this report does not.
///
/// Source: the wording a `CreateSchema` of a definition its format does not accept is
/// refused with (`tests/all/schema_definition_parse_refusal.rs`), asserted to state this
/// operation's own reading by suite 2.
fn invalid(data_format: &str, detail: &str) -> String {
    format!("Schema definition of {data_format} data format is invalid: {detail}")
}

/// Ask `CheckSchemaVersionValidity` about one definition and return the whole answer.
async fn check(router: &MultiServiceRouter, data_format: &str, definition: &str) -> Value {
    ok(
        router,
        "CheckSchemaVersionValidity",
        &json!({ "DataFormat": data_format, "SchemaDefinition": definition }),
    )
    .await
}

/// `Valid` as the answer reports it, which the model declares with a default of false, so an
/// answer that omits it reports an invalid definition.
fn valid(answer: &Value) -> bool {
    answer
        .get("Valid")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `Error` as the answer reports it, or `None` when it carries none.
fn reported_error(answer: &Value) -> Option<String> {
    answer
        .get("Error")
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Rule 1: the answer is the format's own reading of the definition
// ---------------------------------------------------------------------------

/// One row: what a failing assertion calls it, the `DataFormat` the request names, the
/// definition, and whether that format's parser accepts it.
type Row = (&'static str, &'static str, &'static str, bool);

/// The rows of rule 1, spanning the three formats and the fault classes each reads.
fn definitions() -> Vec<Row> {
    vec![
        ("an Avro record document", "AVRO", AVRO, true),
        (
            "an Avro enum document",
            "AVRO",
            r#"{"type":"enum","name":"Kind","symbols":["FULL","NONE"]}"#,
            true,
        ),
        (
            "an Avro document naming a type Avro does not have",
            "AVRO",
            r#"{"type":"record","name":"User","fields":[{"name":"id","type":"quasar"}]}"#,
            false,
        ),
        ("text that is no JSON at all, as AVRO", "AVRO", "not a schema", false),
        (
            "an Avro document left open",
            "AVRO",
            r#"{"type":"record","name":"User""#,
            false,
        ),
        ("a JSON Schema document", "JSON", JSON_SCHEMA, true),
        (
            "a JSON Schema document with a nested object",
            "JSON",
            r#"{"type":"object","properties":{"at":{"type":"object","properties":{}}}}"#,
            true,
        ),
        ("text that is no JSON at all, as JSON", "JSON", "nope", false),
        (
            "a JSON document whose member is left dangling",
            "JSON",
            r#"{"type":"#,
            false,
        ),
        ("a proto3 message", "PROTOBUF", PROTOBUF, true),
        ("a proto2 message", "PROTOBUF", PROTOBUF_PROTO2, true),
        (
            "a protobuf document carrying a package, an enum and a service",
            "PROTOBUF",
            "syntax = \"proto3\"; package com.example; enum Kind { UNKNOWN = 0; } message Ping \
             { Kind kind = 1; } service Pinger { rpc Send (Ping) returns (Ping); }",
            true,
        ),
        ("text that is no protobuf document", "PROTOBUF", "not a schema", false),
        (
            "a protobuf message left open",
            "PROTOBUF",
            "syntax = \"proto3\"; message User { string id = 1;",
            false,
        ),
        (
            "a protobuf field with no number",
            "PROTOBUF",
            "syntax = \"proto3\"; message User { string id = ; }",
            false,
        ),
        (
            "a protobuf document whose fault sits on its third line",
            "PROTOBUF",
            "syntax = \"proto3\";\nmessage User {\n  string id = ;\n}\n",
            false,
        ),
        // The reading keys on the format the request named: each of these documents is
        // accepted above for the format it is written in.
        ("an Avro document submitted as PROTOBUF", "PROTOBUF", AVRO, false),
        ("a protobuf document submitted as AVRO", "AVRO", PROTOBUF, false),
        ("a protobuf document submitted as JSON", "JSON", PROTOBUF, false),
        // An Avro document IS JSON, so the JSON format's reading admits it: what a JSON
        // document says about a schema is that format's own question and not this reading's.
        ("an Avro document submitted as JSON", "JSON", AVRO, true),
    ]
}

/// Rule 1: `Valid` follows the format's own parser, and an invalid definition carries the
/// parser's reading in `Error` while a valid one carries none.
#[tokio::test]
async fn a_check_reports_the_formats_own_reading_of_the_definition() {
    let router = emulator();

    for (case, data_format, definition, expected) in definitions() {
        let answer = check(&router, data_format, definition).await;
        let reported = valid(&answer);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected Valid {expected}, got {reported} in {answer}"
        );

        let error = reported_error(&answer);
        if expected {
            assert!(
                error.is_none(),
                "case {case:?}: a valid definition carries no Error, got {error:?}"
            );
            continue;
        }
        let error = error.unwrap_or_else(|| {
            panic!("case {case:?}: an invalid definition must carry an Error, got {answer}")
        });
        assert!(
            !error.is_empty(),
            "case {case:?}: the Error must carry the reading, got {error:?}"
        );
        // The modelled `SchemaValidationError` admits 1..5000 characters
        // (`smithy_query shape=SchemaValidationError`).
        assert!(
            (1..=5000).contains(&error.chars().count()),
            "case {case:?}: the Error must fit the modelled length, got {} characters",
            error.chars().count()
        );
        // A protobuf definition's reading is the registry's own syntax-error clause, whose
        // position is pinned row by row in suite 4.
        if data_format == "PROTOBUF" {
            assert!(
                error.starts_with("Syntax error in :"),
                "case {case:?}: expected the registry's syntax-error clause, got {error:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the same reading a registry write applies
// ---------------------------------------------------------------------------

/// Rule 2: for every definition a check reports invalid, the write that would have stored it
/// is refused with HTTP 400 `InvalidInputException` whose `Message` states the `Error` the
/// check reported inside the registry's refusal frame — one reading, worded once.
#[tokio::test]
async fn the_error_a_check_reports_is_the_message_a_write_of_it_is_refused_with() {
    let router = emulator();

    // Rows the check reports invalid, one per format, submitted to `CreateSchema`.
    let refused: Vec<(&str, &str, &str)> = vec![
        ("text that is no JSON at all", "AVRO", "not a schema"),
        (
            "an Avro document naming a type Avro does not have",
            "AVRO",
            r#"{"type":"record","name":"User","fields":[{"name":"id","type":"quasar"}]}"#,
        ),
        ("a JSON document left open", "JSON", r#"{"type":"object""#),
        ("text that is no protobuf document", "PROTOBUF", "not a schema"),
        (
            "a protobuf message left open",
            "PROTOBUF",
            "syntax = \"proto3\"; message User { string id = 1;",
        ),
        (
            "an Avro document submitted as PROTOBUF",
            "PROTOBUF",
            AVRO,
        ),
    ];

    for (index, (case, data_format, definition)) in refused.into_iter().enumerate() {
        let answer = check(&router, data_format, definition).await;
        assert!(
            !valid(&answer),
            "case {case:?}: expected Valid false, got {answer}"
        );
        let error = reported_error(&answer).unwrap_or_else(|| {
            panic!("case {case:?}: an invalid definition must carry an Error, got {answer}")
        });

        let schema = format!("validity-row-{index}");
        let (status, body) = call(
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
        assert_eq!(
            status, 400,
            "case {case:?}: a write of a definition the check refuses must be refused, got \
             {status} with {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {body}"
        );
        let message = error_message(&body);
        let expected = invalid(data_format, &error);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the write's Message to state the check's Error, \
             {expected:?}, got {message:?}"
        );
    }

    // And the other way: a definition the check reports valid is one a write stores, which
    // reads back through `GetSchemaVersion`.
    for (case, data_format, definition) in [
        ("an Avro record document", "AVRO", AVRO),
        ("a JSON Schema document", "JSON", JSON_SCHEMA),
        ("a proto3 message", "PROTOBUF", PROTOBUF),
        ("a proto2 message", "PROTOBUF", PROTOBUF_PROTO2),
    ] {
        let answer = check(&router, data_format, definition).await;
        assert!(
            valid(&answer),
            "case {case:?}: expected Valid true, got {answer}"
        );

        let schema = format!("validity-stored-{data_format}-{}", definition.len());
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
        let version_id = created
            .get("SchemaVersionId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the create must report a SchemaVersionId, got {created}"))
            .to_string();
        let read = ok(
            &router,
            "GetSchemaVersion",
            &json!({ "SchemaVersionId": version_id }),
        )
        .await;
        assert_eq!(
            read.get("SchemaDefinition").and_then(Value::as_str),
            Some(definition),
            "case {case:?}: the stored version must read back the definition, got {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the call has no side effects
// ---------------------------------------------------------------------------

/// Rule 3: a check writes nothing — a later caller sees the endpoint it would have seen had
/// the check never been made, including no default registry materialised by it.
#[tokio::test]
async fn a_check_writes_nothing() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    for (case, data_format, definition) in [
        ("a valid Avro document", "AVRO", AVRO),
        ("an invalid Avro document", "AVRO", "not a schema"),
        ("a valid protobuf document", "PROTOBUF", PROTOBUF),
        (
            "an invalid protobuf document",
            "PROTOBUF",
            "message User { string id = }",
        ),
        ("a valid JSON Schema document", "JSON", JSON_SCHEMA),
    ] {
        check(&router, data_format, definition).await;

        let registries = ok(&router, "ListRegistries", &json!({})).await;
        let listed = registries
            .get("Registries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: the listing must report Registries, got {registries}"));
        assert!(
            listed.is_empty(),
            "case {case:?}: a check must create no registry, got {registries}"
        );

        // The store itself, read behind the wire: no registry record at all, so no schema
        // set and no version either (both live inside a registry record).
        let held: Vec<String> = engine
            .state::<ComAmazonawsGlueAwsglueState>()
            .lock()
            .registries
            .keys()
            .cloned()
            .collect();
        assert!(
            held.is_empty(),
            "case {case:?}: a check must write nothing to the store, got {held:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: the registry's syntax-error clause for a protobuf definition
// ---------------------------------------------------------------------------

/// Rule 4: the whole `Error` a protobuf definition's fault is reported in, pinned over rows
/// that differ in WHERE the fault sits — which line of the definition, and which column of
/// that line — so the position reported tracks the definition and is no fixed value.
#[tokio::test]
async fn a_protobuf_definition_fault_is_reported_at_the_position_the_parse_stopped_at() {
    let router = emulator();

    // (case, the definition, the whole Error the answer carries)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "a field left without its terminator, the next field two lines down",
            "syntax = \"proto3\";\npackage com.example;\nmessage Order {\n  int64 id = 1\n  \
             string name = 2;\n}\n",
            "Syntax error in :5:4: expected ';'",
        ),
        (
            "the same fault one line higher",
            "syntax = \"proto3\";\nmessage Order {\n  int64 id = 1\n  string name = 2;\n}\n",
            "Syntax error in :4:4: expected ';'",
        ),
        (
            "the same fault indented one column further",
            "syntax = \"proto3\";\nmessage Order {\n   int64 id = 1\n   string name = 2;\n}\n",
            "Syntax error in :4:5: expected ';'",
        ),
        (
            "a field left without its terminator, all on one line",
            "syntax = \"proto3\"; message Order { int64 id = 1 string name = 2; }",
            "Syntax error in :1:50: expected ';'",
        ),
    ];

    for (case, definition, expected) in rows {
        let answer = check(&router, "PROTOBUF", definition).await;
        assert!(
            !valid(&answer),
            "case {case:?}: expected Valid false, got {answer}"
        );
        let error = reported_error(&answer);
        assert_eq!(
            error.as_deref(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {error:?}"
        );
    }
}
