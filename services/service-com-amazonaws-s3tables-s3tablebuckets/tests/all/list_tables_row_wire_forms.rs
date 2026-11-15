// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of a `ListTables` summary row, over how the listed table's record came to
//! be and what has happened to it since.
//!
//! RULE 1 — a `TableSummary`'s two instants go on the JSON wire as RFC 3339 date-time
//! STRINGS, never as the epoch-seconds number the `restJson1` protocol defaults to:
//! `smithy_query(query="shape", shape="TableSummary")` reports
//! `"timestampFormat": "date-time"` on both `createdAt` and `modifiedAt`, and both are
//! `@required`. The row reports the stored record's instants, so its text is the text the
//! read of the same table reports, and the instant the record was last modified at is never
//! before the one it was created at.
//!
//! RULE 2 — the id segment after `/table/` in a row's `tableARN` is the id this service
//! MINTED for the table: a canonical hyphenated UUID (8-4-4-4-12, lowercase), the same id
//! for the life of the record, so a row's ARN is the ARN the create and the read of that
//! table report. The model constrains the charset only (`TableARN`'s pattern admits the
//! hyphens), so the form is recorded behaviour, asserted through the one
//! [`assert_canonical_uuid`] every suite shares.
//!
//! `wire_forms.rs` states both rules over the create-and-read paths and `rename_table.rs`
//! over the listing a rename leaves behind. What this suite adds is the dimension neither
//! walks: WHICH WRITE last touched the record a row stands for, and WHO wrote it. The cases
//! span the ways a table's record is written — a caller's `CreateTable`, the in-process
//! create another emulated service reaches, and the create S3 Tables performs for a service
//! it manages a table for — crossed with the ways one is changed afterwards: left as the
//! create made it, renamed in place, moved to another namespace, pointed at a new metadata
//! file, and given a maintenance configuration. Every case is then read through four views
//! of the listing (the whole bucket, narrowed by namespace, narrowed by prefix, and walked
//! one row per page through `continuationToken`), so no single row is "the" case.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{service_state, OpenTableFormat};

use crate::harness::{timestamp_seconds, Emulator};
use crate::wire_forms::{assert_canonical_uuid, summary_of, table_id_of};

/// The second namespace every case's bucket holds: where a cross-namespace move goes.
/// Satisfies `NamespaceName`'s `^[0-9a-z_]*$`.
const TARGET_NAMESPACE: &str = "lt_forms_target";

/// The service a managed table is created for, in the `<service>.amazonaws.com` form the
/// `managedByService` member carries.
const MANAGING_SERVICE: &str = "kinesis.amazonaws.com";

/// How the listed table's record was WRITTEN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// A caller's `CreateTable` over the wire.
    Caller,
    /// The same create reached over the in-process callback another emulated service
    /// provisions a table through.
    InProcess,
    /// The create S3 Tables performs itself for a service it manages a table for, which is
    /// the one path that gives a row a `managedByService` and a table of type `aws`.
    ServiceManaged,
}

/// What CHANGED the record after its create — the write a row's `modifiedAt` stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// Nothing: the record still stands as its create left it.
    None,
    /// A rename inside the same namespace, to the name carried here.
    Renamed(&'static str),
    /// A move to [`TARGET_NAMESPACE`], keeping the table's name.
    Moved,
    /// A metadata file written for the table, at the path carried here under the table's
    /// own warehouse location.
    MetadataLocation(&'static str),
    /// A maintenance configuration put on the table.
    MaintenancePut,
}

/// One case: a bucket holding one subject table and its neighbours.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// The namespace the subject table is created in.
    namespace: &'static str,
    /// The subject table, the row every assertion below is about.
    table: &'static str,
    /// Tables a caller creates beside it in the same namespace, so a listing carries more
    /// than the one row.
    siblings: &'static [&'static str],
    /// Whether the subject's create describes its metadata, which is what gives the table a
    /// metadata document before anything else writes one.
    metadata: bool,
    provenance: Provenance,
    mutation: Mutation,
}

/// The cases: every provenance crossed with the mutation classes, over three regions,
/// one, two and three tables in a namespace, and name lengths from the modelled
/// one-character minimum to a long name.
const CASES: &[Case] = &[
    Case {
        label: "a caller's table, unchanged since its create",
        region: "us-east-1",
        bucket: "lt-forms-fresh",
        namespace: "n",
        table: "t",
        siblings: &[],
        metadata: false,
        provenance: Provenance::Caller,
        mutation: Mutation::None,
    },
    Case {
        label: "a caller's table renamed in place",
        region: "us-west-2",
        bucket: "lt-forms-renamed",
        namespace: "analytics_ns",
        table: "tbl_one",
        siblings: &["tbl_two"],
        metadata: true,
        provenance: Provenance::Caller,
        mutation: Mutation::Renamed("tbl_one_moved"),
    },
    Case {
        label: "a caller's table moved to another namespace",
        region: "eu-west-1",
        bucket: "lt-forms-moved",
        namespace: "ns_2026_q1",
        table: "sales_2026",
        siblings: &["t0"],
        metadata: false,
        provenance: Provenance::Caller,
        mutation: Mutation::Moved,
    },
    Case {
        label: "a table pointed at a new metadata file",
        region: "us-west-2",
        bucket: "lt-forms-metadata",
        namespace: "ns_a",
        table: "a_very_long_table_name_0123456789",
        siblings: &["a0", "a_1"],
        metadata: true,
        provenance: Provenance::Caller,
        mutation: Mutation::MetadataLocation(
            "/metadata/00001-9d8a7d2f4f5a4a0a8f1b2c3d4e5f6071.metadata.json",
        ),
    },
    Case {
        label: "a table carrying a maintenance configuration",
        region: "eu-west-1",
        bucket: "lt-forms-maintenance",
        namespace: "maint_ns",
        table: "tbl_3",
        siblings: &[],
        metadata: false,
        provenance: Provenance::Caller,
        mutation: Mutation::MaintenancePut,
    },
    Case {
        label: "a table another emulated service created in-process",
        region: "us-east-1",
        bucket: "lt-forms-in-process",
        namespace: "svc_ns",
        table: "customer_tbl",
        siblings: &["other_tbl"],
        metadata: true,
        provenance: Provenance::InProcess,
        mutation: Mutation::None,
    },
    Case {
        label: "a table another service's create was then moved",
        region: "us-west-2",
        bucket: "lt-forms-in-process-moved",
        namespace: "svc_moved_ns",
        table: "events_tbl",
        siblings: &[],
        metadata: false,
        provenance: Provenance::InProcess,
        mutation: Mutation::Moved,
    },
    Case {
        label: "a table S3 Tables manages for another service",
        region: "eu-west-1",
        bucket: "lt-forms-managed",
        namespace: "managed_ns",
        table: "managed_tbl",
        siblings: &["customer_tbl"],
        metadata: false,
        provenance: Provenance::ServiceManaged,
        mutation: Mutation::None,
    },
];

/// `CreateTable`'s request body for `table`.
fn create_request(table: &str, metadata: bool) -> Value {
    let mut request = json!({
        "name": table,
        // `format` is required and `ICEBERG` is the only value the modelled
        // `OpenTableFormat` enum declares.
        "format": OpenTableFormat::Iceberg.as_wire(),
    });
    if metadata {
        request["metadata"] = json!({
            "iceberg": {
                "schema": {
                    "fields": [
                        { "name": "id", "type": "long", "required": true },
                        { "name": "label", "type": "string" },
                    ]
                }
            }
        });
    }
    request
}

/// The coordinates a table is addressed by, which a rename moves.
#[derive(Debug, Clone)]
struct Coordinates {
    namespace: String,
    name: String,
}

/// Create the case's bucket, its two namespaces, the subject table and its siblings, and
/// answer the bucket's ARN and the subject's `tableARN`.
async fn set_up(emulator: &Emulator, case: &Case) -> (String, String) {
    let label = case.label;
    let bucket = emulator
        .create_table_bucket(case.region, &json!({ "name": case.bucket }))
        .await;
    assert_eq!(
        bucket.status, 200,
        "case {label:?}: CreateTableBucket: {bucket:?}"
    );
    let bucket_arn = bucket.text("arn");
    for namespace in [case.namespace, TARGET_NAMESPACE] {
        let created = emulator
            .create_namespace(case.region, &bucket_arn, &json!({ "namespace": [namespace] }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateNamespace {namespace:?}: {created:?}"
        );
    }

    let table_arn = match case.provenance {
        Provenance::Caller => {
            let created = emulator
                .create_table(
                    case.region,
                    &bucket_arn,
                    case.namespace,
                    &create_request(case.table, case.metadata),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: CreateTable: {created:?}"
            );
            created.text("tableARN")
        }
        Provenance::InProcess => {
            let created = emulator
                .create_table_in_process(
                    case.region,
                    &bucket_arn,
                    case.namespace,
                    &create_request(case.table, case.metadata),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: in-process CreateTable: {created:?}"
            );
            created.text("tableARN")
        }
        // The one path that is not an operation: the store's own create for a service it
        // provisions a table on behalf of.
        Provenance::ServiceManaged => {
            let state =
                service_state(&*emulator.engine).expect("the S3 Tables store is mounted");
            let minted = state
                .lock()
                .create_service_managed_table(
                    &bucket_arn,
                    case.namespace,
                    case.table,
                    MANAGING_SERVICE,
                    SystemTime::now(),
                )
                .unwrap_or_else(|refusal| {
                    panic!("case {label:?}: the managed create was refused: {refusal:?}")
                });
            minted
        }
    };

    for sibling in case.siblings {
        let created = emulator
            .create_table(
                case.region,
                &bucket_arn,
                case.namespace,
                &create_request(sibling, case.metadata),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateTable {sibling:?}: {created:?}"
        );
    }

    (bucket_arn, table_arn)
}

/// Apply the case's mutation and answer the coordinates the subject table is addressed by
/// afterwards.
async fn mutate(emulator: &Emulator, case: &Case, bucket_arn: &str) -> Coordinates {
    let label = case.label;
    let mut at = Coordinates {
        namespace: case.namespace.to_string(),
        name: case.table.to_string(),
    };
    match case.mutation {
        Mutation::None => {}
        Mutation::Renamed(new_name) => {
            let token = version_token(emulator, case, bucket_arn, &at).await;
            let renamed = emulator
                .rename_table(
                    case.region,
                    bucket_arn,
                    &at.namespace,
                    &at.name,
                    &json!({ "newName": new_name, "versionToken": token }),
                )
                .await;
            assert_eq!(
                renamed.status, 204,
                "case {label:?}: RenameTable: {renamed:?}"
            );
            at.name = new_name.to_string();
        }
        Mutation::Moved => {
            let token = version_token(emulator, case, bucket_arn, &at).await;
            let moved = emulator
                .rename_table(
                    case.region,
                    bucket_arn,
                    &at.namespace,
                    &at.name,
                    &json!({
                        "newNamespaceName": TARGET_NAMESPACE,
                        "versionToken": token,
                    }),
                )
                .await;
            assert_eq!(moved.status, 204, "case {label:?}: RenameTable: {moved:?}");
            at.namespace = TARGET_NAMESPACE.to_string();
        }
        Mutation::MetadataLocation(under_warehouse) => {
            let read = emulator
                .get_table_metadata_location(case.region, bucket_arn, &at.namespace, &at.name)
                .await;
            assert_eq!(
                read.status, 200,
                "case {label:?}: GetTableMetadataLocation: {read:?}"
            );
            let location = format!("{}{under_warehouse}", read.text("warehouseLocation"));
            let updated = emulator
                .update_table_metadata_location(
                    case.region,
                    bucket_arn,
                    &at.namespace,
                    &at.name,
                    &json!({
                        "metadataLocation": location,
                        "versionToken": read.text("versionToken"),
                    }),
                )
                .await;
            assert_eq!(
                updated.status, 200,
                "case {label:?}: UpdateTableMetadataLocation: {updated:?}"
            );
        }
        Mutation::MaintenancePut => {
            let put = emulator
                .put_table_maintenance_configuration(
                    case.region,
                    bucket_arn,
                    &at.namespace,
                    &at.name,
                    "icebergCompaction",
                    &json!({
                        "status": "enabled",
                        "settings": {
                            "icebergCompaction": { "targetFileSizeMB": 128, "strategy": "binpack" }
                        }
                    }),
                )
                .await;
            assert_eq!(
                put.status, 204,
                "case {label:?}: PutTableMaintenanceConfiguration: {put:?}"
            );
        }
    }
    at
}

/// The subject table's current `versionToken`, which a rename's precondition names.
async fn version_token(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    at: &Coordinates,
) -> String {
    let label = case.label;
    let described = emulator
        .get_table(case.region, bucket_arn, &at.namespace, &at.name)
        .await;
    assert_eq!(
        described.status, 200,
        "case {label:?}: GetTable: {described:?}"
    );
    described.text("versionToken")
}

/// Every row of the listing `query` asks for, following `continuationToken` to the end so a
/// paged view reports the whole listing.
async fn rows(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    query: &[(&str, String)],
    view: &str,
) -> Vec<Value> {
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    // One page per table of the bucket is the worst case; the extra pages are headroom for
    // the last, empty page a cursor can end on.
    let pages = case.siblings.len() + 3;
    for page in 0..pages {
        let mut asked: Vec<(&str, String)> = query.to_vec();
        if let Some(token) = &token {
            asked.push(("continuationToken", token.clone()));
        }
        let reply = emulator
            .list_tables(case.region, bucket_arn, &asked)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: ListTables ({view}, page {page}): {reply:?}",
            case = case.label
        );
        collected.extend(reply.list("tables"));
        token = reply.optional_text("continuationToken");
        if token.is_none() {
            return collected;
        }
    }
    panic!(
        "case {case:?}: the {view} listing never ran out of pages",
        case = case.label
    );
}

/// RULE 1 and RULE 2 over every case and every view of the listing.
#[tokio::test]
async fn every_list_tables_row_reports_date_time_instants_and_the_minted_table_id() {
    for case in CASES {
        let label = case.label;
        let emulator = Emulator::new();
        let (bucket_arn, created_arn) = set_up(&emulator, case).await;
        assert_canonical_uuid(
            &table_id_of(&created_arn, label),
            "the id segment of the ARN the create reported",
            label,
        );

        let at = mutate(&emulator, case, &bucket_arn).await;

        // The read of the same record: its text is what every listing row must carry, and
        // its ARN is the one the create reported — a write to the record never re-mints the
        // table's id.
        let described = emulator
            .get_table(case.region, &bucket_arn, &at.namespace, &at.name)
            .await;
        assert_eq!(described.status, 200, "case {label:?}: GetTable: {described:?}");
        assert_eq!(
            described.text("tableARN"),
            created_arn,
            "case {label:?}: GetTable reports a different tableARN from the create"
        );
        let described_created = described.text("createdAt");
        let described_modified = described.text("modifiedAt");

        // Four views of the same listing: the whole bucket, narrowed by the namespace the
        // table sits in now, narrowed by a prefix of its name, and walked one row per page.
        let prefix = at.name[..1].to_string();
        let views: Vec<(String, Vec<(&str, String)>)> = vec![
            ("the whole bucket".to_string(), Vec::new()),
            (
                format!("namespace {:?}", at.namespace),
                vec![("namespace", at.namespace.clone())],
            ),
            (
                format!("prefix {prefix:?}"),
                vec![("prefix", prefix.clone())],
            ),
            (
                "one row per page".to_string(),
                vec![("maxTables", "1".to_string())],
            ),
        ];

        for (view, query) in views {
            let listed = rows(&emulator, case, &bucket_arn, &query, &view).await;
            let case_label = format!("{label} / {view}");
            let row = summary_of(&listed, &at.name, &case_label);

            // RULE 1: both instants are JSON STRINGS in the modelled date-time format —
            // `timestamp_seconds` accepts that form and no other — and they are the text
            // the read of the same record reports.
            for (member, described) in [
                ("createdAt", &described_created),
                ("modifiedAt", &described_modified),
            ] {
                let actual = row.get(member);
                assert!(
                    actual.is_some_and(Value::is_string),
                    "case {case_label:?}: expected {member} to be an RFC 3339 date-time \
                     string, got {actual:?}"
                );
                assert_eq!(
                    actual.and_then(Value::as_str),
                    Some(described.as_str()),
                    "case {case_label:?}: the row reports {member} as {actual:?}, not the \
                     {described:?} the read of the same table reports"
                );
            }
            let created_at = timestamp_seconds(row, "createdAt", &case_label);
            let modified_at = timestamp_seconds(row, "modifiedAt", &case_label);
            assert!(
                modified_at >= created_at,
                "case {case_label:?}: the row was last modified at {modified_at}, before it \
                 was created at {created_at}"
            );

            // RULE 2: the ARN is the one the create minted, and its id segment is a
            // canonical hyphenated UUID.
            let row_arn = row.get("tableARN").and_then(Value::as_str);
            assert_eq!(
                row_arn,
                Some(created_arn.as_str()),
                "case {case_label:?}: expected the row to carry the minted ARN \
                 {created_arn:?}, got {row_arn:?}"
            );
            assert_canonical_uuid(
                &table_id_of(row_arn.expect("the ARN was just compared"), &case_label),
                "the id segment of a listing row's tableARN",
                &case_label,
            );

            // The row stands for the table at the coordinates the mutation left it at, and
            // reports the service managing it only for a table a service manages.
            assert_eq!(
                row.get("namespace").and_then(Value::as_array),
                Some(&vec![Value::from(at.namespace.clone())]),
                "case {case_label:?}: expected the row's namespace to be [{:?}], got {row}",
                at.namespace
            );
            let expected_service = (case.provenance == Provenance::ServiceManaged)
                .then_some(MANAGING_SERVICE);
            assert_eq!(
                row.get("managedByService").and_then(Value::as_str),
                expected_service,
                "case {case_label:?}: expected managedByService {expected_service:?}, got \
                 {row}"
            );
        }
    }
}
