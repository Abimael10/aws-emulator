// SPDX-License-Identifier: CC0-1.0

//! The AGREEMENT between a finished export's manifest and the data it wrote, walked the
//! way a consumer of an export walks it.
//!
//! RULE: for a full export that reaches `ExportStatus` `COMPLETED`, the summary object
//! named by `ExportDescription$ExportManifest` reports in `itemCount` the number of items
//! the export wrote, and its `manifestFilesS3Key` names a newline-delimited file whose
//! per-data-file `itemCount` values partition that same total over data files that hold
//! exactly those items as GZIP-compressed DynamoDB JSON — so a consumer that sizes or
//! validates an export from its manifest alone reaches the data the table held.
//!
//! The path every assertion below takes is the consumer's: `DescribeExport` names the
//! summary object, the summary names the file list, the file list names each data file.
//! No key is built here, so a manifest that pointed at nothing would fail rather than be
//! stepped over.
//!
//! The rows differ along what the rule generalises over — how many items the export wrote
//! (none, one, a dozen) and which attribute TYPES those items carry (a plain string item,
//! one of every scalar and collection type `AttributeValue` declares, a nested document) —
//! because the count is read off the data rather than the request, and the data is
//! whatever the table holds.
//!
//! Each case drives DynamoDB over the wire through the same `MultiServiceRouter` the
//! emulator's front doors use, with Amazon S3 mounted beside it on ONE engine and read
//! through its OWN client: the objects an export writes exist only if that service serves
//! them.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{
    create_bucket, describe_when_active, emulator_with_s3, export_arn, ok_case, s3,
    with_sorted_sets, POLL_INTERVAL, REGION,
};

/// The destination bucket every case exports into; the export's own path keeps the cases
/// apart (`<S3Prefix>/AWSDynamoDB/<ExportId>/`).
const BUCKET: &str = "export-manifest-agreement";

/// The prefix the requests name, so the manifest the consumer reads is found under a path
/// of the caller's choosing rather than at the root of the bucket.
const PREFIX: &str = "exp";

/// How long a poll for the `IN_PROGRESS` -> `COMPLETED` transition the clock owes a
/// started export may take before this suite calls it a hang.
///
/// Generous slack over the emulator's own transition span ([`state::STATUS_TRANSITION`]):
/// a loaded machine is not a defect, and what the bound is for is that the wait ENDS.
const COMPLETED_DEADLINE: Duration = Duration::from_secs(30);

/// One row of the table below: the items the table holds when the export is taken.
struct Case {
    /// What this row is about, named in every assertion it raises.
    case: &'static str,
    /// The table the row's own items live in.
    table: String,
    /// The items, each a complete `PutItem` item map keyed on `Part`.
    items: Vec<Value>,
}

/// One item of nothing but the key, for a row about the COUNT rather than the attributes.
fn plain_item(id: usize) -> Value {
    json!({"Part": {"S": format!("item-{id}")}, "Payload": {"S": "x".repeat(24)}})
}

/// One item carrying one attribute of every type `AttributeValue` declares — the member
/// set of that shape (`smithy_query(query="shape", shape="AttributeValue")`: S, N, B, SS,
/// NS, BS, M, L, NULL, BOOL) — so the exported rendering is asserted over every type an
/// item may hold rather than over strings alone.
///
/// The numbers are written in the canonical form DynamoDB stores a number in, and the
/// sets are compared as sets ([`with_sorted_sets`]), because "Amazon DynamoDB does not
/// preserve the order of the members of a set".
fn every_type_item() -> Value {
    json!({
        "Part": {"S": "every-type"},
        "Text": {"S": "a string"},
        "Number": {"N": "7"},
        "Fraction": {"N": "-3.5"},
        "Bytes": {"B": "ZXhwb3J0ZWQ="},
        "Strings": {"SS": ["beta", "alpha"]},
        "Numbers": {"NS": ["2", "1"]},
        "Blobs": {"BS": ["Zm9v", "YmFy"]},
        "Document": {"M": {"Inner": {"S": "nested"}, "Count": {"N": "2"}}},
        "List": {"L": [{"S": "first"}, {"N": "1"}, {"BOOL": false}]},
        "Nothing": {"NULL": true},
        "Flag": {"BOOL": true},
    })
}

/// One item whose attributes nest a document inside a list inside a document, for the row
/// about a shape a flat item cannot show.
fn nested_item(id: usize) -> Value {
    json!({
        "Part": {"S": format!("nested-{id}")},
        "Tree": {"M": {
            "Branches": {"L": [
                {"M": {"Leaf": {"S": format!("leaf-{id}")}, "Weight": {"N": "1"}}},
                {"M": {"Leaf": {"S": "spare"}, "Weight": {"N": "0"}}},
            ]},
            "Label": {"S": "root"},
        }},
    })
}

/// Create one on-demand table keyed on `Part` and answer once it serves writes.
async fn create_table(router: &MultiServiceRouter, case: &str, name: &str) {
    ok_case(
        router,
        case,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
}

/// The bytes Amazon S3 serves for one key of the destination bucket — the read a consumer
/// of an export makes, so an object the export did not write fails the case here.
async fn object_bytes(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let read = s3(router)
        .get_object()
        .bucket(BUCKET)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: S3 serves {key:?}, got {error:?}"));
    read.body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: {key:?} is readable, got {error:?}"))
        .to_vec()
}

/// Poll `DescribeExport` until the export reports `COMPLETED`, answering the description
/// it reported then — the loop a client runs after starting an export, bounded by
/// [`COMPLETED_DEADLINE`] so an export that never settles fails naming its last
/// description rather than hanging.
async fn export_when_completed(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + COMPLETED_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let described = ok_case(router, case, "DescribeExport", json!({"ExportArn": arn})).await;
        last = described["ExportDescription"].clone();
        assert_ne!(
            last["ExportStatus"],
            json!("FAILED"),
            "case {case:?}: the export had somewhere to write, got {last}"
        );
        if last["ExportStatus"] == json!("COMPLETED") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the export never reported COMPLETED, last description {last}");
}

/// Every item one data file holds, as the `Item` member of each of its lines.
///
/// "the data files gzipped DynamoDB JSON": the object is GZIP, each line one JSON document
/// whose `Item` is the item in the same attribute-value form a `PutItem` wrote it in.
fn items_in_data_file(case: &str, data: &[u8]) -> Vec<Value> {
    use std::io::Read;

    let mut text = String::new();
    flate2::read::GzDecoder::new(data)
        .read_to_string(&mut text)
        .unwrap_or_else(|error| panic!("case {case:?}: the data file is GZIP, got {error:?}"));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let document: Value = serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case {case:?}: every data-file line is JSON, got {line:?}: {error:?}")
            });
            assert!(
                document["Item"].is_object(),
                "case {case:?}: a data-file line carries the item under Item, got {document}"
            );
            canonical(&document["Item"])
        })
        .collect()
}

/// The items of `items`, each as one canonical string, sorted — two renderings of one set
/// of items compared as SETS, since none of the data file's line order, an item's
/// attribute order or a set's member order is part of what was exported.
fn comparable(items: &[Value]) -> Vec<String> {
    let mut rendered: Vec<String> = items.iter().map(|item| canonical(item).to_string()).collect();
    rendered.sort();
    rendered
}

/// One item in a canonical form: every set's members sorted ([`with_sorted_sets`], since
/// "Amazon DynamoDB does not preserve the order of the members of a set") and every
/// attribute name in name order, because an item is a MAP of attributes — the order its
/// names happen to be rendered in is not part of the item.
fn canonical(item: &Value) -> Value {
    with_names_in_order(&with_sorted_sets(item))
}

/// The same value with the names of every object in it in name order.
fn with_names_in_order(value: &Value) -> Value {
    match value {
        Value::Object(entries) => {
            let mut names: Vec<String> = entries.keys().cloned().collect();
            names.sort();
            Value::Object(
                names
                    .into_iter()
                    .map(|name| {
                        let inner = entries.get(&name).map(with_names_in_order);
                        (name, inner.unwrap_or(Value::Null))
                    })
                    .collect(),
            )
        }
        Value::Array(members) => Value::Array(members.iter().map(with_names_in_order).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn a_completed_exports_manifest_counts_and_names_the_items_it_wrote() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;

    let rows: Vec<Case> = vec![
        Case {
            case: "a table holding no item",
            table: "ManifestEmpty".to_string(),
            items: Vec::new(),
        },
        Case {
            case: "one item of every attribute type",
            table: "ManifestEveryType".to_string(),
            items: vec![every_type_item()],
        },
        Case {
            case: "a dozen plain items",
            table: "ManifestDozen".to_string(),
            items: (0..12).map(plain_item).collect(),
        },
        Case {
            case: "items whose attributes nest",
            table: "ManifestNested".to_string(),
            items: (0..3).map(nested_item).collect(),
        },
    ];

    // Every table is created before any of them is waited on, so the span the clock owes a
    // new table is owed to all of them at once rather than one after another.
    for row in &rows {
        create_table(&router, row.case, &row.table).await;
    }
    let mut started = Vec::new();
    for row in &rows {
        describe_when_active(&router, row.case, &row.table).await;
        // "The table must have point in time recovery enabled" (the operation's doc).
        ok_case(
            &router,
            row.case,
            "UpdateContinuousBackups",
            json!({
                "TableName": &row.table,
                "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
            }),
        )
        .await;
        for item in &row.items {
            ok_case(
                &router,
                row.case,
                "PutItem",
                json!({"TableName": &row.table, "Item": item}),
            )
            .await;
        }
        let begun = ok_case(
            &router,
            row.case,
            "ExportTableToPointInTime",
            json!({
                "TableArn": state::table_arn(REGION, &row.table),
                "S3Bucket": BUCKET,
                "S3Prefix": PREFIX,
                "ExportFormat": "DYNAMODB_JSON",
            }),
        )
        .await;
        started.push(export_arn(&begun));
    }

    for (row, arn) in rows.iter().zip(started.iter()) {
        let case = row.case;
        let expected = row.items.len();
        let finished = export_when_completed(&router, case, arn).await;

        // The consumer's entry point: the summary object the description names.
        let summary_key = finished["ExportManifest"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !summary_key.is_empty(),
            "case {case:?}: a completed export names the manifest it wrote, got {finished}"
        );
        let summary: Value =
            serde_json::from_slice(&object_bytes(&router, case, &summary_key).await)
                .unwrap_or(Value::Null);

        // The total: what the export wrote, which is what the table held.
        assert_eq!(
            summary["itemCount"],
            json!(expected),
            "case {case:?}: expected the manifest summary to count {expected} items, got {summary}"
        );
        assert_eq!(
            finished["ItemCount"],
            json!(expected),
            "case {case:?}: expected the record to count {expected} items, got {finished}"
        );

        // The file list the summary names, one data file per line.
        let files_key = summary["manifestFilesS3Key"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !files_key.is_empty(),
            "case {case:?}: the summary names the list of data files, got {summary}"
        );
        let listing = String::from_utf8(object_bytes(&router, case, &files_key).await)
            .unwrap_or_else(|error| panic!("case {case:?}: the file list is text, got {error:?}"));
        let listed: Vec<Value> = listing
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("case {case:?}: every file-list line is JSON, got {line:?}: {error:?}")
                })
            })
            .collect();
        assert!(
            !listed.is_empty(),
            "case {case:?}: the file list names at least one data file, got {listing:?}"
        );

        // The partition: each line counts the items of its own data file, and the lines
        // together count the total the summary reports.
        let mut exported = Vec::new();
        let mut counted = 0_u64;
        for entry in &listed {
            let data_key = entry["dataFileS3Key"].as_str().unwrap_or_default().to_string();
            assert!(
                !data_key.is_empty(),
                "case {case:?}: a file-list line names its data file, got {entry}"
            );
            let held = items_in_data_file(case, &object_bytes(&router, case, &data_key).await);
            assert_eq!(
                entry["itemCount"],
                json!(held.len()),
                "case {case:?}: expected the file list to count the {} items of {data_key:?}, \
                 got {entry}",
                held.len()
            );
            counted += entry["itemCount"].as_u64().unwrap_or_default();
            exported.extend(held);
        }
        assert_eq!(
            counted,
            expected as u64,
            "case {case:?}: expected the per-file counts to sum to {expected}, got {listed:?}"
        );

        // And the data is the table's own: exactly the items that were written, nothing
        // synthesised and nothing dropped.
        assert_eq!(
            comparable(&exported),
            comparable(&row.items),
            "case {case:?}: expected the data files to hold exactly the items written, got \
             {exported:?}"
        );
    }
}
