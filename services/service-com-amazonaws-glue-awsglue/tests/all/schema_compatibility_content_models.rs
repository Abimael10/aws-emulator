// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the compatibility verdict a registration earns in the `JSON` and
//! `PROTOBUF` data formats — the formats whose documents are a JSON Schema and a `.proto`
//! source rather than an Avro schema (whose own rows are in
//! [`crate::schema_version_compatibility`]).
//!
//! The rule under test: a `RegisterSchemaVersion` against a schema set whose recorded
//! `Compatibility` asks for a check is compared with the version the set holds in the
//! direction the mode names — `BACKWARD` makes the registered definition the READER of the
//! held version's data, `FORWARD` the WRITER of it, `FULL` both — and the version is stored
//! with the `SchemaVersionStatus` that comparison gives it: `AVAILABLE` when a reader admits
//! every instance the writer writes, `FAILURE` when it does not. The call itself stays a 200
//! either way, consumes the next `VersionNumber`, and leaves the submitted bytes resolvable
//! — the status is the ONLY channel the service reports a failed evolution through
//! (`smithy_query operation=RegisterSchemaVersion`: "You can call the GetSchemaVersion API
//! with the SchemaVersionId to check compatibility modes").
//!
//! What a reader admits is the FORMAT's own question:
//!
//! * `JSON` — containment of one JSON Schema document in another. The CONTENT MODEL decides
//!   what a difference means: a document with `additionalProperties: true` admits an instance
//!   carrying any property at all, so a property the reader ADDS is one such an instance can
//!   carry with a value the reader does not admit, while a closed document
//!   (`additionalProperties: false`) can never carry it — and a closed READER forbids a
//!   property an open writer may carry.
//! * `PROTOBUF` — the protobuf language's own field rules. A field the reader declares
//!   `required` must be in the bytes, which the writer writes only if the writer requires it
//!   too, so adding a `required` field is backward incompatible and removing one is forward
//!   incompatible, while `optional` fields are never required of the bytes.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: the data
//! format, the `Compatibility` mode, the content model of each document, the difference
//! between the two definitions (a property or field added, dropped, made required, retyped)
//! and the direction the mode compares in. Every call goes over the wire through the shared
//! [`crate::wire`] harness.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    SCHEMA_VERSION_STATUS_AVAILABLE, SCHEMA_VERSION_STATUS_FAILURE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{emulator, ok};

/// Shorthands for the two statuses a registration can give a version.
const AVAILABLE: &str = SCHEMA_VERSION_STATUS_AVAILABLE;
const FAILURE: &str = SCHEMA_VERSION_STATUS_FAILURE;

/// A JSON Schema document of one object: `open` says whether it admits properties it does
/// not declare, `properties` are the declared ones as `(name, type)` pairs, and `required`
/// names those an instance must carry.
fn json_schema(open: bool, properties: &[(&str, &str)], required: &[&str]) -> String {
    let declared: Vec<String> = properties
        .iter()
        .map(|(name, kind)| format!(r#""{name}":{{"type":"{kind}"}}"#))
        .collect();
    let names: Vec<String> = required.iter().map(|name| format!("\"{name}\"")).collect();
    format!(
        r#"{{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","additionalProperties":{open},"properties":{{{}}},"required":[{}]}}"#,
        declared.join(","),
        names.join(",")
    )
}

/// A proto2 document of one message carrying `fields`.
fn proto2(fields: &[&str]) -> String {
    format!(
        "syntax = \"proto2\";\npackage com.example;\nmessage User {{\n{}\n}}\n",
        fields.join("\n")
    )
}

/// A string member of a response body.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: {member} must be a string, got {body}"))
        .to_string()
}

/// Create the schema set `schema` of `data_format` under `mode`, holding `definition` as its
/// first version — which is never compatibility-checked, there being nothing to compare it
/// with.
async fn given_schema(
    router: &MultiServiceRouter,
    case: &str,
    schema: &str,
    data_format: &str,
    mode: &str,
    definition: &str,
) {
    let created = ok(
        router,
        "CreateSchema",
        &json!({
            "SchemaName": schema,
            "DataFormat": data_format,
            "Compatibility": mode,
            "SchemaDefinition": definition,
        }),
    )
    .await;
    assert_eq!(
        text(case, &created, "SchemaVersionStatus"),
        AVAILABLE,
        "case {case}: the version a create registers is never compatibility-checked, got \
         {created}"
    );
}

/// One row of the table below: a schema set of one data format and mode holding one
/// definition, against which another is registered.
struct Row {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The `DataFormat` both definitions are written in.
    data_format: &'static str,
    /// The `Compatibility` the schema set is created under.
    mode: &'static str,
    /// The definition the create registers as version 1.
    held: String,
    /// The definition the registration submits as version 2.
    registered: String,
    /// The `SchemaVersionStatus` that registration must settle at.
    status: &'static str,
}

/// Drive one row end to end: the registration's own answer, the version read back by its
/// id, and the version the definition resolves to.
async fn assert_row(row: &Row, index: usize) {
    let router = emulator();
    let schema = format!("content-model-schema-{index}");
    let case = row.case;
    given_schema(
        &router,
        case,
        &schema,
        row.data_format,
        row.mode,
        &row.held,
    )
    .await;

    let registered = ok(
        &router,
        "RegisterSchemaVersion",
        &json!({
            "SchemaId": { "SchemaName": schema },
            "SchemaDefinition": row.registered,
        }),
    )
    .await;
    assert_eq!(
        text(case, &registered, "Status"),
        row.status,
        "case {case}: expected the registration to settle {}, got {registered}",
        row.status
    );
    // The registration stays a 200 that consumed the next version number, whatever the
    // verdict.
    assert_eq!(
        registered.get("VersionNumber"),
        Some(&json!(2)),
        "case {case}: the registration must consume the next version number, got {registered}"
    );

    // The verdict is a property of the stored version: the read by its own id reports it, and
    // so does the lookup by the bytes that were registered — which also proves the bytes stay
    // resolvable when the verdict is a failure.
    let version_id = text(case, &registered, "SchemaVersionId");
    let read = ok(
        &router,
        "GetSchemaVersion",
        &json!({ "SchemaVersionId": version_id }),
    )
    .await;
    assert_eq!(
        text(case, &read, "Status"),
        row.status,
        "case {case}: the stored version must report the verdict, got {read}"
    );
    assert_eq!(
        text(case, &read, "SchemaDefinition"),
        row.registered,
        "case {case}: the stored version must carry the bytes registered, got {read}"
    );
    let by_definition = ok(
        &router,
        "GetSchemaByDefinition",
        &json!({
            "SchemaId": { "SchemaName": schema },
            "SchemaDefinition": row.registered,
        }),
    )
    .await;
    assert_eq!(
        text(case, &by_definition, "Status"),
        row.status,
        "case {case}: the lookup by definition must report the same verdict, got {by_definition}"
    );
    assert_eq!(
        text(case, &by_definition, "SchemaVersionId"),
        version_id,
        "case {case}: the lookup by definition must resolve the version registered, got \
         {by_definition}"
    );
}

/// A `JSON` registration settles by JSON Schema containment, which the CONTENT MODEL of the
/// two documents decides.
#[tokio::test]
async fn a_json_registration_settles_by_the_content_models_of_the_two_documents() {
    let open_one = json_schema(true, &[("id", "string")], &["id"]);
    let open_two = json_schema(true, &[("id", "string"), ("email", "string")], &["id"]);
    let open_two_required = json_schema(
        true,
        &[("id", "string"), ("email", "string")],
        &["id", "email"],
    );
    let closed_one = json_schema(false, &[("id", "string")], &["id"]);
    let closed_two = json_schema(false, &[("id", "string"), ("email", "string")], &["id"]);
    let closed_retyped = json_schema(false, &[("id", "integer")], &["id"]);

    let rows: Vec<Row> = vec![
        // An OPEN document admits an instance carrying the added property with any value, so
        // the reader's constraint on it is one such an instance need not satisfy.
        Row {
            case: "an optional property added to an open content model under BACKWARD",
            data_format: "JSON",
            mode: "BACKWARD",
            held: open_one.clone(),
            registered: open_two.clone(),
            status: FAILURE,
        },
        // The CLOSED sibling of the same difference: an instance of the held version can
        // never carry the added property.
        Row {
            case: "an optional property added to a closed content model under BACKWARD",
            data_format: "JSON",
            mode: "BACKWARD",
            held: closed_one.clone(),
            registered: closed_two.clone(),
            status: AVAILABLE,
        },
        Row {
            case: "a required property added to a closed content model under BACKWARD",
            data_format: "JSON",
            mode: "BACKWARD",
            held: closed_one.clone(),
            registered: json_schema(
                false,
                &[("id", "string"), ("email", "string")],
                &["id", "email"],
            ),
            status: FAILURE,
        },
        Row {
            case: "a required property added to an open content model under BACKWARD",
            data_format: "JSON",
            mode: "BACKWARD",
            held: open_one.clone(),
            registered: open_two_required.clone(),
            status: FAILURE,
        },
        // Under FORWARD the registered definition is the WRITER, so the open add is the
        // compatible half and the closed add is not: a closed reader forbids the property the
        // new writer may carry.
        Row {
            case: "an optional property added to an open content model under FORWARD",
            data_format: "JSON",
            mode: "FORWARD",
            held: open_one.clone(),
            registered: open_two.clone(),
            status: AVAILABLE,
        },
        Row {
            case: "an optional property added to a closed content model under FORWARD",
            data_format: "JSON",
            mode: "FORWARD",
            held: closed_one.clone(),
            registered: closed_two.clone(),
            status: FAILURE,
        },
        // A property DROPPED by a closed reader is the mirror image of the add.
        Row {
            case: "a property dropped from a closed content model under BACKWARD",
            data_format: "JSON",
            mode: "BACKWARD",
            held: closed_two.clone(),
            registered: closed_one.clone(),
            status: FAILURE,
        },
        Row {
            case: "a property dropped from a closed content model under FORWARD",
            data_format: "JSON",
            mode: "FORWARD",
            held: closed_two.clone(),
            registered: closed_one.clone(),
            status: AVAILABLE,
        },
        // A property's TYPE changed is incompatible in either direction, so `FULL` reports it
        // too.
        Row {
            case: "a property's type changed under FULL",
            data_format: "JSON",
            mode: "FULL",
            held: closed_one.clone(),
            registered: closed_retyped.clone(),
            status: FAILURE,
        },
        // `NONE` asks for no comparison at all, so the same difference settles available.
        Row {
            case: "an optional property added to an open content model under NONE",
            data_format: "JSON",
            mode: "NONE",
            held: open_one.clone(),
            registered: open_two.clone(),
            status: AVAILABLE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        assert_row(row, index).await;
    }
}

/// A `PROTOBUF` registration settles by the protobuf language's own field rules, which the
/// LABEL of the field that changed decides.
#[tokio::test]
async fn a_protobuf_registration_settles_by_the_labels_of_the_fields_that_changed() {
    let one = proto2(&["  required string id = 1;"]);
    let add_required = proto2(&[
        "  required string id = 1;",
        "  required string email = 2;",
    ]);
    let add_optional = proto2(&[
        "  required string id = 1;",
        "  optional string email = 2;",
    ]);
    let retyped = proto2(&["  required int64 id = 1;"]);

    let rows: Vec<Row> = vec![
        // A reader requiring a field the writer does not require cannot parse the writer's
        // messages.
        Row {
            case: "a required field added under BACKWARD",
            data_format: "PROTOBUF",
            mode: "BACKWARD",
            held: one.clone(),
            registered: add_required.clone(),
            status: FAILURE,
        },
        Row {
            case: "an optional field added under BACKWARD",
            data_format: "PROTOBUF",
            mode: "BACKWARD",
            held: one.clone(),
            registered: add_optional.clone(),
            status: AVAILABLE,
        },
        // Under FORWARD the version HELD is the reader, so dropping a field it requires is
        // what fails.
        Row {
            case: "a required field removed under FORWARD",
            data_format: "PROTOBUF",
            mode: "FORWARD",
            held: add_required.clone(),
            registered: one.clone(),
            status: FAILURE,
        },
        Row {
            case: "an optional field removed under FORWARD",
            data_format: "PROTOBUF",
            mode: "FORWARD",
            held: add_optional.clone(),
            registered: one.clone(),
            status: AVAILABLE,
        },
        // The same two differences in the other direction: dropping a required field is
        // backward compatible, and adding one is forward compatible.
        Row {
            case: "a required field removed under BACKWARD",
            data_format: "PROTOBUF",
            mode: "BACKWARD",
            held: add_required.clone(),
            registered: one.clone(),
            status: AVAILABLE,
        },
        Row {
            case: "a required field added under FORWARD",
            data_format: "PROTOBUF",
            mode: "FORWARD",
            held: one.clone(),
            registered: add_required.clone(),
            status: AVAILABLE,
        },
        // `FULL` asks for both, so either direction's failure settles the version failed.
        Row {
            case: "a required field added under FULL",
            data_format: "PROTOBUF",
            mode: "FULL",
            held: one.clone(),
            registered: add_required.clone(),
            status: FAILURE,
        },
        // A field number read against another type is another reading of the same bytes.
        Row {
            case: "a field's type changed under FULL",
            data_format: "PROTOBUF",
            mode: "FULL",
            held: one.clone(),
            registered: retyped.clone(),
            status: FAILURE,
        },
        Row {
            case: "a required field added under NONE",
            data_format: "PROTOBUF",
            mode: "NONE",
            held: one.clone(),
            registered: add_required.clone(),
            status: AVAILABLE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        assert_row(row, index).await;
    }
}
