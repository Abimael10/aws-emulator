// SPDX-License-Identifier: CC0-1.0

//! What a table reports about having been MODIFIED.
//!
//! The rule under test: `GetTable`'s `modifiedBy` is the account that last
//! modified the table, and the response carries it only once a modification has
//! been recorded — a table that still stands as its create left it comes back
//! WITHOUT the member, whatever its `modifiedAt` says, and every operation that
//! changes the table writes it from that point on. A client that tells
//! "untouched since creation" from "modified" by the member's presence depends
//! on it: with the member always set, every freshly created table reads as
//! already modified.
//!
//! The rows span what the rule generalises over: how the table was created (by a
//! caller's `CreateTable`, and by the in-process create another emulated service
//! reaches), which operation modifies it (a rename, a metadata-location update,
//! an Iceberg catalog commit), and reading it twice over — a read is not a
//! modification, so it never makes the member appear.

use serde_json::json;

use crate::harness::{timestamp_seconds, Emulator, Reply, ACCOUNT};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The namespace every case's table lives in.
const NAMESPACE: &str = "analytics";

/// The metadata a create carries when the case needs the table to have a
/// metadata document to commit to.
fn iceberg_metadata() -> serde_json::Value {
    json!({ "iceberg": { "schema": { "fields": [
        { "name": "id", "type": "long", "required": true },
        { "name": "payload", "type": "string", "required": false },
    ]}}})
}

/// One table, created for a case.
struct Created {
    bucket_arn: String,
    table_arn: String,
    name: String,
    version_token: String,
    warehouse_location: String,
}

/// Create a bucket, a namespace and one table named `table`.
async fn created(
    emulator: &Emulator,
    bucket: &str,
    table: &str,
    metadata: bool,
    in_process: bool,
) -> Created {
    let made = emulator
        .create_table_bucket(REGION, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        made.status, 200,
        "creating the bucket {bucket:?} must succeed, got {status} {body}",
        status = made.status,
        body = made.body
    );
    let bucket_arn = made.text("arn");
    let namespace = emulator
        .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    assert_eq!(
        namespace.status, 200,
        "creating the namespace must succeed, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );
    let mut request = json!({ "name": table, "format": "ICEBERG" });
    if metadata {
        request
            .as_object_mut()
            .expect("the create request is an object")
            .insert("metadata".to_string(), iceberg_metadata());
    }
    let table_created = if in_process {
        emulator
            .create_table_in_process(REGION, &bucket_arn, NAMESPACE, &request)
            .await
    } else {
        emulator
            .create_table(REGION, &bucket_arn, NAMESPACE, &request)
            .await
    };
    assert_eq!(
        table_created.status, 200,
        "creating the table {table:?} must succeed, got {status} {body}",
        status = table_created.status,
        body = table_created.body
    );
    let read = emulator.get_table(REGION, &bucket_arn, NAMESPACE, table).await;
    assert_eq!(
        read.status, 200,
        "reading the table {table:?} must succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
    Created {
        bucket_arn,
        table_arn: read.text("tableARN"),
        name: table.to_string(),
        version_token: read.text("versionToken"),
        warehouse_location: read.text("warehouseLocation"),
    }
}

/// Assert `read` reports a table nothing has modified: no modifying account, and
/// a last-modified instant that is still the instant it was created.
fn assert_unmodified(read: &Reply, case: &str) {
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read must succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert!(
        read.optional_text("modifiedBy").is_none(),
        "case {case:?}: a table nothing has modified must report no modifying account, got \
         {actual:?}",
        actual = read.optional_text("modifiedBy")
    );
    assert_eq!(
        timestamp_seconds(&read.body, "modifiedAt", case),
        timestamp_seconds(&read.body, "createdAt", case),
        "case {case:?}: a table nothing has modified was last modified when it was created"
    );
}

/// Assert `read` reports a table that HAS been modified: the modifying account,
/// and a last-modified instant at or after the create's.
fn assert_modified(read: &Reply, case: &str) {
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read must succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.optional_text("modifiedBy").as_deref(),
        Some(ACCOUNT),
        "case {case:?}: a modified table must report the modifying account, got {actual:?}",
        actual = read.optional_text("modifiedBy")
    );
    assert!(
        timestamp_seconds(&read.body, "modifiedAt", case)
            >= timestamp_seconds(&read.body, "createdAt", case),
        "case {case:?}: a modified table was last modified at or after it was created"
    );
}

#[tokio::test]
async fn a_table_nothing_has_modified_reports_no_modifying_account() {
    let emulator = Emulator::new();

    // (case, whether the create describes the table's metadata, whether the
    // create arrives over the in-process path another emulated service uses)
    let rows = [
        ("a create with no metadata", false, false),
        ("a create describing the table's metadata", true, false),
        ("an in-process create", false, true),
    ];

    for (index, (case, metadata, in_process)) in rows.into_iter().enumerate() {
        let table = created(
            &emulator,
            &format!("unmodified-table-{index}"),
            "user_events",
            metadata,
            in_process,
        )
        .await;

        // Both addressing modes of the read, and the read twice over: reporting
        // a table is not modifying it, so the member never appears.
        for attempt in 0..2 {
            let by_triple = emulator
                .get_table(REGION, &table.bucket_arn, NAMESPACE, &table.name)
                .await;
            assert_unmodified(&by_triple, &format!("{case}, read {attempt} by the triple"));
            let by_arn = emulator.get_table_by_arn(REGION, &table.table_arn).await;
            assert_unmodified(&by_arn, &format!("{case}, read {attempt} by the table ARN"));
        }
    }
}

#[tokio::test]
async fn the_first_modification_is_what_records_the_modifying_account() {
    let emulator = Emulator::new();

    // A rename moves the table's coordinates.
    let renamed = created(&emulator, "modified-by-rename", "user_events", false, false).await;
    assert_unmodified(
        &emulator
            .get_table(REGION, &renamed.bucket_arn, NAMESPACE, &renamed.name)
            .await,
        "before the rename",
    );
    let moved = emulator
        .rename_table(
            REGION,
            &renamed.bucket_arn,
            NAMESPACE,
            &renamed.name,
            &json!({ "newName": "user_events_renamed" }),
        )
        .await;
    assert_eq!(
        moved.status, 204,
        "the rename must succeed, got {status} {body}",
        status = moved.status,
        body = moved.body
    );
    assert_modified(
        &emulator.get_table_by_arn(REGION, &renamed.table_arn).await,
        "after the rename",
    );

    // A metadata-location update moves the table's metadata pointer.
    let updated = created(&emulator, "modified-by-update", "user_events", false, false).await;
    assert_unmodified(
        &emulator
            .get_table(REGION, &updated.bucket_arn, NAMESPACE, &updated.name)
            .await,
        "before the metadata-location update",
    );
    let location = format!(
        "{warehouse}/metadata/00001-a4b0c1d2e3f40516273849506172838a.metadata.json",
        warehouse = updated.warehouse_location
    );
    let pointed = emulator
        .update_table_metadata_location(
            REGION,
            &updated.bucket_arn,
            NAMESPACE,
            &updated.name,
            &json!({
                "metadataLocation": location,
                "versionToken": updated.version_token,
            }),
        )
        .await;
    assert_eq!(
        pointed.status, 200,
        "the metadata-location update must succeed, got {status} {body}",
        status = pointed.status,
        body = pointed.body
    );
    assert_modified(
        &emulator.get_table_by_arn(REGION, &updated.table_arn).await,
        "after the metadata-location update",
    );

    // An Iceberg catalog commit changes the table's metadata document.
    let committed = created(&emulator, "modified-by-commit", "user_events", true, false).await;
    assert_unmodified(
        &emulator
            .get_table(REGION, &committed.bucket_arn, NAMESPACE, &committed.name)
            .await,
        "before the commit",
    );
    let commit = emulator
        .commit_table(
            REGION,
            &committed.bucket_arn,
            NAMESPACE,
            &committed.name,
            &json!({
                "requirements": [],
                "updates": [
                    { "action": "set-properties", "updates": { "owner": "analytics" } },
                ],
            }),
        )
        .await;
    assert_eq!(
        commit.status, 200,
        "the commit must succeed, got {status} {body}",
        status = commit.status,
        body = commit.body
    );
    assert_modified(
        &emulator.get_table_by_arn(REGION, &committed.table_arn).await,
        "after the commit",
    );
}
