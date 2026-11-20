// SPDX-License-Identifier: CC0-1.0

//! The metadata CHAIN of a table served over the Iceberg REST catalog: what each
//! new metadata document records about the one it replaces, and the properties a
//! table starts life with.
//!
//! Two rules are pinned here, both of the table-metadata document the catalog's
//! table endpoint (`/iceberg/v1/{prefix}/namespaces/{namespace}/tables/{table}`)
//! answers with:
//!
//! * every commit that writes a new metadata file for a table appends the file it
//!   superseded to `metadata-log` — the location the control plane reported for
//!   the table before the commit, beside the instant that description was last
//!   updated (`last-updated-ms` of the document that lived in it) — so the log
//!   holds one entry per superseded file in commit order, oldest first, and the
//!   table's metadata location advances through the numbered metadata-file
//!   sequence under its warehouse while the control plane rotates its version
//!   token. A commit that writes no new metadata file (one that asks for no
//!   update, one whose requirement the table does not satisfy, one this catalog
//!   will not apply) appends nothing.
//! * a table the control plane creates starts with the table properties S3 Tables
//!   settles on every managed table — the Parquet compression codec a writer
//!   encodes its data files with — and the create's own property document is
//!   applied OVER them, so a create that states a property of its own keeps it.
//!
//! The tables below span what each rule generalises over: the first over the kind
//! and the count of updates one commit carries (a sort order, a property set, a
//! property removal, a schema, a partition spec, several updates at once) and
//! over how a commit fails to write a file; the second over the schema forms a
//! create may declare and over a create that states no property, one that states
//! another property and one that states the settled property itself.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::OpenTableFormat;

use super::harness::{Emulator, Reply};

/// One entry of a `metadata-log`, as this suite compares it: the metadata file
/// the table was described by, and when that description was last updated.
type LoggedFile = (String, i64);

/// A table created for the catalog to serve, with what the control plane reports
/// about it.
struct Created {
    bucket_arn: String,
    table_arn: String,
    warehouse_location: String,
    metadata_location: String,
    version_token: String,
}

/// Create a table bucket, a namespace in it and one Iceberg table whose create
/// carries `metadata`, and read the table's control-plane record back.
async fn create_table(
    emulator: &Emulator,
    region: &str,
    bucket: &str,
    namespace: &str,
    table: &str,
    metadata: Value,
) -> Created {
    let created = emulator
        .create_table_bucket(region, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let bucket_arn = created.text("arn");
    let namespace_created = emulator
        .create_namespace(region, &bucket_arn, &json!({ "namespace": [namespace] }))
        .await;
    assert_eq!(
        namespace_created.status, 200,
        "creating the namespace {namespace:?} must succeed, got {status} {body}",
        status = namespace_created.status,
        body = namespace_created.body
    );
    let table_created = emulator
        .create_table(
            region,
            &bucket_arn,
            namespace,
            &json!({
                "name": table,
                "format": OpenTableFormat::Iceberg.as_wire(),
                "metadata": metadata,
            }),
        )
        .await;
    assert_eq!(
        table_created.status, 200,
        "creating the table {table:?} must succeed, got {status} {body}",
        status = table_created.status,
        body = table_created.body
    );
    let read = emulator
        .get_table(region, &bucket_arn, namespace, table)
        .await;
    assert_eq!(
        read.status, 200,
        "reading the table {table:?} must succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
    Created {
        bucket_arn,
        table_arn: read.text("tableARN"),
        warehouse_location: read.text("warehouseLocation"),
        metadata_location: read
            .optional_text("metadataLocation")
            .unwrap_or_else(|| panic!("the table must carry a metadata location: {}", read.body)),
        version_token: read.text("versionToken"),
    }
}

/// The `metadata` document a load or a commit answered with.
fn metadata_of(reply: &Reply, case: &str) -> Value {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the catalog call must succeed, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    reply
        .body
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the reply carries no metadata: {}", reply.body))
}

/// The `metadata-location` a load or a commit reports.
fn reported_location(reply: &Reply, case: &str) -> String {
    reply
        .body
        .get("metadata-location")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the reply names no metadata location: {}",
                reply.body
            )
        })
        .to_string()
}

/// The document's `metadata-log`, oldest entry first.
fn logged_files(document: &Value, case: &str) -> Vec<LoggedFile> {
    document
        .get("metadata-log")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the document carries no metadata log: {document}"))
        .iter()
        .map(|entry| {
            (
                entry
                    .get("metadata-file")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("timestamp-ms")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// The instant the document states the table's description was last updated.
fn last_updated_ms(document: &Value, case: &str) -> i64 {
    document
        .get("last-updated-ms")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the document states no last-updated-ms: {document}")
        })
}

/// The table's UUID, as its ARN carries it: the last segment of the ARN is the
/// table's server-minted id.
fn table_uuid(table_arn: &str) -> String {
    table_arn
        .rsplit('/')
        .next()
        .expect("a table ARN ends in the table's id")
        .to_string()
}

/// The schema a table of this suite is created with: four columns, none of them
/// stating a field id.
fn declared_schema() -> Value {
    json!({ "iceberg": { "schema": { "fields": [
        { "name": "user_id", "type": "string", "required": true },
        { "name": "event_type", "type": "string", "required": true },
        { "name": "event_timestamp", "type": "long", "required": true },
        { "name": "session_id", "type": "string", "required": false },
    ]}}})
}

#[tokio::test]
async fn each_commit_logs_the_metadata_file_it_superseded() {
    let emulator = Emulator::new();
    let region = "us-west-2";
    let namespace = "events";
    let table = "user_events_with_sort_compaction";
    let created = create_table(
        &emulator,
        region,
        "catalog-metadata-log",
        namespace,
        table,
        declared_schema(),
    )
    .await;
    let uuid = table_uuid(&created.table_arn);

    // The file the create wrote is the table's first, so the table was described
    // by no earlier one.
    let first = metadata_of(
        &emulator
            .load_table(region, &created.bucket_arn, namespace, table)
            .await,
        "the created table",
    );
    assert!(
        logged_files(&first, "the created table").is_empty(),
        "case {case:?}: a table whose first metadata file is still its current one must log no \
         superseded file, got {actual:?}",
        case = "the created table",
        actual = logged_files(&first, "the created table")
    );

    // (the case, the updates one commit asks for) — the kinds of update this
    // catalog applies, each writing a new metadata file for the table.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a commit that adds a sort order and makes it the default",
            json!([
                { "action": "add-sort-order", "sort-order": { "order-id": 1, "fields": [
                    {
                        "source-id": 1,
                        "transform": "identity",
                        "direction": "asc",
                        "null-order": "nulls-first",
                    },
                ]}},
                { "action": "set-default-sort-order", "sort-order-id": -1 },
            ]),
        ),
        (
            "a commit that sets one property",
            json!([{ "action": "set-properties", "updates": { "owner": "analytics" } }]),
        ),
        (
            "a commit that removes the property the commit before it set",
            json!([{ "action": "remove-properties", "removals": ["owner"] }]),
        ),
        (
            "a commit that adds a schema and makes it current",
            json!([
                { "action": "add-schema", "schema": { "fields": [
                    { "id": 1, "name": "user_id", "type": "string", "required": true },
                    { "id": 2, "name": "event_region", "type": "string", "required": false },
                ]}},
                { "action": "set-current-schema", "schema-id": -1 },
            ]),
        ),
        (
            "a commit that adds a partition spec",
            json!([
                { "action": "add-spec", "spec": { "fields": [
                    { "source-id": 3, "transform": "day", "name": "event_day" },
                ]}},
            ]),
        ),
        (
            "a commit that states several updates at once",
            json!([
                { "action": "assign-uuid", "uuid": uuid },
                { "action": "set-properties", "updates": {
                    "owner": "analytics",
                    "write.metadata.delete-after-commit.enabled": "true",
                }},
                { "action": "set-location", "location": created.warehouse_location },
            ]),
        ),
    ];

    // What the log must hold, grown by the file each commit supersedes.
    let mut expected_log: Vec<LoggedFile> = Vec::new();
    let mut previous_location = created.metadata_location.clone();
    let mut previous_token = created.version_token.clone();
    let mut previous_updated_ms = last_updated_ms(&first, "the created table");

    for (index, (case, updates)) in rows.into_iter().enumerate() {
        let committed = emulator
            .commit_table(
                region,
                &created.bucket_arn,
                namespace,
                table,
                &json!({
                    "requirements": [{ "type": "assert-table-uuid", "uuid": uuid }],
                    "updates": updates,
                }),
            )
            .await;
        let document = metadata_of(&committed, case);

        expected_log.push((previous_location.clone(), previous_updated_ms));
        let actual_log = logged_files(&document, case);
        assert_eq!(
            actual_log, expected_log,
            "case {case:?}: expected the metadata log {expected_log:?} — one entry per superseded \
             metadata file, oldest first — got {actual_log:?}"
        );

        // The new file is the next one of the table's numbered metadata sequence,
        // under the table's own warehouse location.
        let location = reported_location(&committed, case);
        let version = index + 1;
        let expected_prefix = format!(
            "{warehouse}/metadata/{version:05}-",
            warehouse = created.warehouse_location
        );
        assert!(
            location.starts_with(&expected_prefix),
            "case {case:?}: expected a metadata file under {expected_prefix:?}, got {location:?}"
        );
        assert_ne!(
            location, previous_location,
            "case {case:?}: the commit must write a metadata file other than the one it \
             superseded, got {location:?}"
        );

        // The control plane reports the file the commit wrote, through both of
        // its readers, under a rotated version token.
        let read = emulator
            .get_table(region, &created.bucket_arn, namespace, table)
            .await;
        assert_eq!(
            read.optional_text("metadataLocation").as_deref(),
            Some(location.as_str()),
            "case {case:?}: expected GetTable to report {location:?}, got {actual:?}",
            actual = read.optional_text("metadataLocation")
        );
        let pointed = emulator
            .get_table_metadata_location(region, &created.bucket_arn, namespace, table)
            .await;
        assert_eq!(
            pointed.optional_text("metadataLocation").as_deref(),
            Some(location.as_str()),
            "case {case:?}: expected GetTableMetadataLocation to report {location:?}, got \
             {actual:?}",
            actual = pointed.optional_text("metadataLocation")
        );
        let token = read.text("versionToken");
        assert_ne!(
            token, previous_token,
            "case {case:?}: the commit must rotate the table's version token, got {token:?}"
        );

        // A load answers the document the commit published, log included.
        let loaded = metadata_of(
            &emulator
                .load_table(region, &created.bucket_arn, namespace, table)
                .await,
            case,
        );
        assert_eq!(
            loaded, document,
            "case {case:?}: a load after the commit must answer the committed document"
        );

        previous_location = location;
        previous_token = token;
        previous_updated_ms = last_updated_ms(&document, case);
    }
}

#[tokio::test]
async fn a_commit_that_writes_no_metadata_file_logs_nothing() {
    let emulator = Emulator::new();
    let region = "eu-west-1";
    let namespace = "events";
    let table = "user_events";
    let created = create_table(
        &emulator,
        region,
        "catalog-metadata-log-refusals",
        namespace,
        table,
        declared_schema(),
    )
    .await;
    let uuid = table_uuid(&created.table_arn);
    let before = metadata_of(
        &emulator
            .load_table(region, &created.bucket_arn, namespace, table)
            .await,
        "the created table",
    );
    let before_updated_ms = last_updated_ms(&before, "the created table");

    // One commit that DOES write a file, so the log the calls below must leave
    // alone is not the empty one.
    let committed = emulator
        .commit_table(
            region,
            &created.bucket_arn,
            namespace,
            table,
            &json!({
                "requirements": [{ "type": "assert-table-uuid", "uuid": uuid }],
                "updates": [{ "action": "set-properties", "updates": { "owner": "analytics" } }],
            }),
        )
        .await;
    let published = metadata_of(&committed, "the first commit");
    let settled_log = logged_files(&published, "the first commit");
    let settled_location = reported_location(&committed, "the first commit");
    let expected_log: Vec<LoggedFile> =
        vec![(created.metadata_location.clone(), before_updated_ms)];
    assert_eq!(
        settled_log, expected_log,
        "case {case:?}: the first commit must log the file the create wrote as {expected_log:?}, \
         got {settled_log:?}",
        case = "the first commit"
    );

    // (the case, the commit body, the status it is answered with)
    let rows: Vec<(&str, Value, u16)> = vec![
        (
            "a commit that asks for no update",
            json!({ "requirements": [{ "type": "assert-table-uuid", "uuid": uuid }] }),
            200,
        ),
        (
            "a commit whose requirement the table does not satisfy",
            json!({
                "requirements": [
                    { "type": "assert-table-uuid", "uuid": "00000000-0000-4000-8000-000000000000" },
                ],
                "updates": [{ "action": "set-properties", "updates": { "owner": "ops" } }],
            }),
            409,
        ),
        (
            "a commit naming an update action the catalog does not know",
            json!({
                "updates": [{ "action": "rewrite-the-table", "updates": {} }],
            }),
            400,
        ),
        (
            "a commit asking for an update this catalog does not serve",
            json!({
                "updates": [{ "action": "add-snapshot", "snapshot": { "snapshot-id": 1 } }],
            }),
            501,
        ),
        (
            "a commit that would move the table's files",
            json!({
                "updates": [{ "action": "set-location", "location": "s3://somewhere-else" }],
            }),
            400,
        ),
    ];

    for (case, body, expected_status) in rows {
        let reply = emulator
            .commit_table(region, &created.bucket_arn, namespace, table, &body)
            .await;
        assert_eq!(
            reply.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} {actual}",
            status = reply.status,
            actual = reply.body
        );
        let loaded = metadata_of(
            &emulator
                .load_table(region, &created.bucket_arn, namespace, table)
                .await,
            case,
        );
        let actual_log = logged_files(&loaded, case);
        assert_eq!(
            actual_log, settled_log,
            "case {case:?}: a commit that writes no metadata file must leave the log \
             {settled_log:?}, got {actual_log:?}"
        );
        let read = emulator
            .get_table(region, &created.bucket_arn, namespace, table)
            .await;
        assert_eq!(
            read.optional_text("metadataLocation").as_deref(),
            Some(settled_location.as_str()),
            "case {case:?}: expected the table to still point at {settled_location:?}, got \
             {actual:?}",
            actual = read.optional_text("metadataLocation")
        );
    }
}

#[tokio::test]
async fn a_created_table_carries_the_properties_the_service_settles() {
    let emulator = Emulator::new();
    let region = "ap-south-1";
    let namespace = "events";
    let table = "user_events";

    // `write.parquet.compression-codec: zstd` below is the one property S3 Tables
    // settles on every managed table, as a catalog load of a freshly created
    // table reports it: the codec a writer compresses the table's Parquet data
    // files with.
    //
    // (the case, the `metadata` the create carries, the properties the load must
    // report)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a create that states a schema and no property",
            declared_schema(),
            json!({ "write.parquet.compression-codec": "zstd" }),
        ),
        (
            "a create that states a property of its own",
            json!({ "iceberg": {
                "schema": { "fields": [
                    { "name": "id", "type": "long", "required": true },
                ]},
                "properties": { "write.format.default": "parquet" },
            }}),
            json!({
                "write.parquet.compression-codec": "zstd",
                "write.format.default": "parquet",
            }),
        ),
        (
            "a create that states the settled property itself, with another value",
            json!({ "iceberg": {
                "schema": { "fields": [
                    { "name": "id", "type": "long", "required": true },
                ]},
                "properties": { "write.parquet.compression-codec": "snappy" },
            }}),
            json!({ "write.parquet.compression-codec": "snappy" }),
        ),
        (
            "a create that declares a version 2 schema with a nested column type",
            json!({ "iceberg": { "schemaV2": {
                "type": "struct",
                "schema-id": 0,
                "fields": [
                    { "id": 1, "name": "id", "type": "long", "required": true },
                    {
                        "id": 2,
                        "name": "payload",
                        "required": false,
                        "type": {
                            "type": "list",
                            "element-id": 3,
                            "element": "string",
                            "element-required": false,
                        },
                    },
                ],
            }}}),
            json!({ "write.parquet.compression-codec": "zstd" }),
        ),
    ];

    for (index, (case, metadata, expected_properties)) in rows.into_iter().enumerate() {
        let created = create_table(
            &emulator,
            region,
            &format!("catalog-created-properties-{index}"),
            namespace,
            table,
            metadata,
        )
        .await;
        let loaded = emulator
            .load_table(region, &created.bucket_arn, namespace, table)
            .await;
        let document = metadata_of(&loaded, case);
        assert_eq!(
            document.get("properties"),
            Some(&expected_properties),
            "case {case:?}: expected the properties {expected_properties}, got {actual:?}",
            actual = document.get("properties")
        );
        assert!(
            logged_files(&document, case).is_empty(),
            "case {case:?}: a freshly created table has superseded no metadata file, got \
             {actual:?}",
            actual = logged_files(&document, case)
        );
        assert_eq!(
            reported_location(&loaded, case),
            created.metadata_location,
            "case {case:?}: expected the load to name the file the create wrote"
        );
    }
}
