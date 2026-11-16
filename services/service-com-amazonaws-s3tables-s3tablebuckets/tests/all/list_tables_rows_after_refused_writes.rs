// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of a `ListTables` summary row over the calls that do NOT write the record
//! a row stands for: a read, and every class of refusal a table takes.
//!
//! RULE 1 — a `TableSummary`'s two instants go on the JSON wire as RFC 3339 date-time
//! STRINGS, never as the epoch-seconds number the `restJson1` protocol defaults to:
//! `smithy_query(query="shape", shape="TableSummary")` reports
//! `"timestampFormat": "date-time"` on both `createdAt` and `modifiedAt`, and both are
//! `@required`.
//!
//! RULE 2 — the id segment after `/table/` in a row's `tableARN` is the id this service
//! MINTED for the table: a canonical hyphenated UUID (8-4-4-4-12, lowercase), the same id
//! the create of that table reported. The model constrains the charset only (`TableARN`'s
//! pattern admits the hyphens), so the form is recorded behaviour, asserted through the one
//! [`assert_canonical_uuid`] every suite shares.
//!
//! RULE 3 — a call the service does not carry out writes NOTHING a later listing reports:
//! a read leaves the record as it found it, and a request refused as its class
//! (`BadRequestException` 400, `ConflictException` 409, `NotFoundException` 404) leaves it
//! too, so the listing a client re-reads afterwards is the listing it read before — row for
//! row, member for member, including the two instants and the minted id of RULES 1 and 2.
//!
//! `wire_forms.rs` states RULES 1 and 2 over the create-and-read paths and
//! `list_tables_row_wire_forms.rs` over the ways a record is written and changed. What this
//! suite adds is the complement of that dimension: the calls that change NOTHING. The cases
//! span the reads of the listing, of the table and of its metadata location, crossed with
//! every class of refusal a table takes — a create of an occupied name, a rename onto an
//! occupied name, a rename describing no move, a rename and a read of a table nothing holds,
//! a metadata-location write and a delete presenting another table's version token, a
//! maintenance put naming a status that is neither of the two, and a tag write naming a
//! reserved key — over three regions.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    INVALID_CONFIGURATION_STATUS_MESSAGE, NOTHING_TO_RENAME_MESSAGE, RESERVED_TAG_KEY_MESSAGE,
    TABLE_ALREADY_EXISTS_MESSAGE, TABLE_NOT_FOUND_MESSAGE, TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{timestamp_seconds, Emulator, Reply};
use crate::wire_forms::{assert_canonical_uuid, create_table_request, table_id_of};

/// The namespace every case's tables are created in. Satisfies `NamespaceName`'s
/// `^[0-9a-z_]*$`.
const NAMESPACE: &str = "refused_ns";

/// The table a case's interfering call is aimed at.
const SUBJECT: &str = "tbl_one";

/// The tables created beside it, so a listing carries more than the one row: the modelled
/// one-character minimum and a long name, both `^[0-9a-z_]*$`.
const SIBLINGS: [&str; 2] = ["t", "a_very_long_table_name_0123456789"];

/// A name no case's namespace holds, which the two "absent table" rows address.
const ABSENT_TABLE: &str = "never_created";

/// The regions the cases are spread over, so no case's ARNs are the only ones asserted.
const REGIONS: [&str; 3] = ["us-east-1", "us-west-2", "eu-west-1"];

/// WHICH call stands between the two listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interfering {
    /// The listing itself, read a second time.
    ListedAgain,
    /// `GetTable` on the subject.
    TableRead,
    /// `GetTableMetadataLocation` on the subject.
    MetadataLocationRead,
    /// `CreateTable` naming the subject's own name.
    CreateOfAnOccupiedName,
    /// `RenameTable` moving the subject onto a sibling's name.
    RenameOntoAnOccupiedName,
    /// `RenameTable` naming neither new coordinate, so describing no move.
    RenameDescribingNoMove,
    /// `RenameTable` of a table the namespace does not hold.
    RenameOfAnAbsentTable,
    /// `UpdateTableMetadataLocation` presenting a sibling's version token: a real token of
    /// this service's minting that is not the subject's.
    MetadataWriteOnAStaleView,
    /// `DeleteTable` presenting that same token.
    DeleteOnAStaleView,
    /// `PutTableMaintenanceConfiguration` naming a status that is neither of the two a
    /// configuration takes.
    MaintenancePutOnAnInvalidStatus,
    /// `TagResource` on the subject naming a key reserved for system use.
    TagWriteOnAReservedKey,
    /// `GetTable` on a table the namespace does not hold.
    ReadOfAnAbsentTable,
}

/// One case: the call, and the answer it is owed.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    interfering: Interfering,
    status: u16,
    /// The error code the refusal carries, or `None` for a call that is carried out.
    code: Option<&'static str>,
    /// The fixed sentence the refusal states, or `None` for a call that is carried out.
    message: Option<&'static str>,
}

/// The cases: the three reads, then one row per class of refusal a table takes, across the
/// three statuses this service refuses a table-addressed request with.
const CASES: &[Case] = &[
    Case {
        label: "the listing read a second time",
        interfering: Interfering::ListedAgain,
        status: 200,
        code: None,
        message: None,
    },
    Case {
        label: "the table read by name",
        interfering: Interfering::TableRead,
        status: 200,
        code: None,
        message: None,
    },
    Case {
        label: "the table's metadata location read",
        interfering: Interfering::MetadataLocationRead,
        status: 200,
        code: None,
        message: None,
    },
    Case {
        label: "a create naming the name the subject already occupies",
        interfering: Interfering::CreateOfAnOccupiedName,
        status: 409,
        code: Some("ConflictException"),
        message: Some(TABLE_ALREADY_EXISTS_MESSAGE),
    },
    Case {
        label: "a rename onto a name a sibling already occupies",
        interfering: Interfering::RenameOntoAnOccupiedName,
        status: 409,
        code: Some("ConflictException"),
        message: Some(TABLE_ALREADY_EXISTS_MESSAGE),
    },
    Case {
        label: "a rename describing no move at all",
        interfering: Interfering::RenameDescribingNoMove,
        status: 400,
        code: Some("BadRequestException"),
        message: Some(NOTHING_TO_RENAME_MESSAGE),
    },
    Case {
        label: "a rename of a table the namespace does not hold",
        interfering: Interfering::RenameOfAnAbsentTable,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(TABLE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a metadata-location write presenting another table's version token",
        interfering: Interfering::MetadataWriteOnAStaleView,
        status: 409,
        code: Some("ConflictException"),
        message: Some(TABLE_VERSION_TOKEN_MISMATCH_MESSAGE),
    },
    Case {
        label: "a delete presenting another table's version token",
        interfering: Interfering::DeleteOnAStaleView,
        status: 409,
        code: Some("ConflictException"),
        message: Some(TABLE_VERSION_TOKEN_MISMATCH_MESSAGE),
    },
    Case {
        label: "a maintenance put naming a status that is neither of the two",
        interfering: Interfering::MaintenancePutOnAnInvalidStatus,
        status: 400,
        code: Some("BadRequestException"),
        message: Some(INVALID_CONFIGURATION_STATUS_MESSAGE),
    },
    Case {
        label: "a tag write naming a key reserved for system use",
        interfering: Interfering::TagWriteOnAReservedKey,
        status: 400,
        code: Some("BadRequestException"),
        message: Some(RESERVED_TAG_KEY_MESSAGE),
    },
    Case {
        label: "a read of a table the namespace does not hold",
        interfering: Interfering::ReadOfAnAbsentTable,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(TABLE_NOT_FOUND_MESSAGE),
    },
];

/// The resources one case runs against: a bucket, one namespace, the subject table and its
/// two siblings, with the ARN each create reported.
struct Fixture {
    emulator: Emulator,
    region: &'static str,
    bucket_arn: String,
    /// Each table's name mapped to the `tableARN` its create reported — the ARN every later
    /// read and every listing row of that table must carry.
    created_arns: BTreeMap<String, String>,
}

impl Fixture {
    async fn new(region: &'static str, bucket: &str, case: &str) -> Self {
        let emulator = Emulator::new();
        let created = emulator
            .create_table_bucket(region, &json!({ "name": bucket }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTableBucket: {created:?}"
        );
        let bucket_arn = created.text("arn");
        let namespace = emulator
            .create_namespace(region, &bucket_arn, &json!({ "namespace": [NAMESPACE] }))
            .await;
        assert_eq!(
            namespace.status, 200,
            "case {case:?}: CreateNamespace: {namespace:?}"
        );

        let mut created_arns = BTreeMap::new();
        // The subject carries metadata, which is what gives it a metadata file and so a
        // metadata location to read and to try to write.
        for (table, metadata) in [
            (SUBJECT, true),
            (SIBLINGS[0], false),
            (SIBLINGS[1], false),
        ] {
            let reply = emulator
                .create_table(
                    region,
                    &bucket_arn,
                    NAMESPACE,
                    &create_table_request(table, metadata),
                )
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: CreateTable {table:?}: {reply:?}"
            );
            created_arns.insert(table.to_string(), reply.text("tableARN"));
        }

        Self {
            emulator,
            region,
            bucket_arn,
            created_arns,
        }
    }

    /// Every row of the whole bucket's listing, keyed by the table's name.
    async fn rows(&self, case: &str) -> BTreeMap<String, Value> {
        let listed = self
            .emulator
            .list_tables(self.region, &self.bucket_arn, &[])
            .await;
        assert_eq!(listed.status, 200, "case {case:?}: ListTables: {listed:?}");
        assert!(
            listed.optional_text("continuationToken").is_none(),
            "case {case:?}: the bucket's three tables fit one page, got a continuation \
             token: {listed:?}"
        );
        listed
            .list("tables")
            .into_iter()
            .map(|row| {
                let name = row
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: listing row {row:?} has no name"))
                    .to_string();
                (name, row)
            })
            .collect()
    }

    /// One table's current `versionToken`, as `GetTable` reports it.
    async fn version_token(&self, table: &str, case: &str) -> String {
        let read = self
            .emulator
            .get_table(self.region, &self.bucket_arn, NAMESPACE, table)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTable {table:?}: {read:?}"
        );
        read.text("versionToken")
    }

    /// The subject's warehouse location, which a metadata location must sit under.
    async fn warehouse_location(&self, case: &str) -> String {
        let read = self
            .emulator
            .get_table_metadata_location(self.region, &self.bucket_arn, NAMESPACE, SUBJECT)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTableMetadataLocation: {read:?}"
        );
        read.text("warehouseLocation")
    }
}

/// Make the case's one interfering call.
async fn interfere(fixture: &Fixture, case: &Case) -> Reply {
    let label = case.label;
    let emulator = &fixture.emulator;
    let region = fixture.region;
    let bucket_arn = &fixture.bucket_arn;
    match case.interfering {
        Interfering::ListedAgain => emulator.list_tables(region, bucket_arn, &[]).await,
        Interfering::TableRead => {
            emulator
                .get_table(region, bucket_arn, NAMESPACE, SUBJECT)
                .await
        }
        Interfering::MetadataLocationRead => {
            emulator
                .get_table_metadata_location(region, bucket_arn, NAMESPACE, SUBJECT)
                .await
        }
        Interfering::CreateOfAnOccupiedName => {
            emulator
                .create_table(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    &create_table_request(SUBJECT, false),
                )
                .await
        }
        Interfering::RenameOntoAnOccupiedName => {
            let subject_token = fixture.version_token(SUBJECT, label).await;
            emulator
                .rename_table(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    SUBJECT,
                    &json!({ "newName": SIBLINGS[0], "versionToken": subject_token }),
                )
                .await
        }
        Interfering::RenameDescribingNoMove => {
            emulator
                .rename_table(region, bucket_arn, NAMESPACE, SUBJECT, &json!({}))
                .await
        }
        Interfering::RenameOfAnAbsentTable => {
            emulator
                .rename_table(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    ABSENT_TABLE,
                    &json!({ "newName": "free_name" }),
                )
                .await
        }
        Interfering::MetadataWriteOnAStaleView => {
            // A sibling's token: a real token of this service's own minting that is not
            // the subject's, which is what a stale view of the table presents.
            let sibling_token = fixture.version_token(SIBLINGS[0], label).await;
            let warehouse = fixture.warehouse_location(label).await;
            emulator
                .update_table_metadata_location(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    SUBJECT,
                    &json!({
                        // A location this service would otherwise accept: under the
                        // table's own warehouse and naming an Iceberg metadata file, so
                        // the version token is the only thing the request gets wrong.
                        "metadataLocation":
                            format!("{warehouse}/metadata/00002-stale-view.metadata.json"),
                        "versionToken": sibling_token,
                    }),
                )
                .await
        }
        Interfering::DeleteOnAStaleView => {
            let sibling_token = fixture.version_token(SIBLINGS[0], label).await;
            emulator
                .delete_table(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    SUBJECT,
                    &[("versionToken", sibling_token)],
                )
                .await
        }
        Interfering::MaintenancePutOnAnInvalidStatus => {
            emulator
                .put_table_maintenance_configuration(
                    region,
                    bucket_arn,
                    NAMESPACE,
                    SUBJECT,
                    "icebergCompaction",
                    &json!({
                        // Neither of the two values the modelled `MaintenanceStatus` enum
                        // declares, with settings the put would otherwise store.
                        "status": "on",
                        "settings": {
                            "icebergCompaction": {
                                "targetFileSizeMB": 128,
                                "strategy": "binpack",
                            }
                        },
                    }),
                )
                .await
        }
        Interfering::TagWriteOnAReservedKey => {
            let table_arn = fixture
                .created_arns
                .get(SUBJECT)
                .expect("the subject's create reported an ARN");
            emulator
                .tag_resource(region, table_arn, &json!({ "aws:owner": "someone" }))
                .await
        }
        Interfering::ReadOfAnAbsentTable => {
            emulator
                .get_table(region, bucket_arn, NAMESPACE, ABSENT_TABLE)
                .await
        }
    }
}

/// RULES 1 and 2 on every row of `rows`, against the ARNs the creates reported.
fn assert_row_wire_forms(rows: &BTreeMap<String, Value>, fixture: &Fixture, case: &str) {
    assert_eq!(
        rows.keys().cloned().collect::<Vec<String>>(),
        fixture.created_arns.keys().cloned().collect::<Vec<String>>(),
        "case {case:?}: expected a row for each created table, got {rows:?}"
    );
    for (name, row) in rows {
        let row_case = format!("{case} / row {name:?}");

        // RULE 1: both instants are JSON STRINGS in the modelled date-time format —
        // `timestamp_seconds` accepts that form and no other — and the instant the record
        // was last modified at is never before the one it was created at.
        for member in ["createdAt", "modifiedAt"] {
            let actual = row.get(member);
            assert!(
                actual.is_some_and(Value::is_string),
                "case {row_case:?}: expected {member} to be an RFC 3339 date-time string, \
                 got {actual:?}"
            );
        }
        let created_at = timestamp_seconds(row, "createdAt", &row_case);
        let modified_at = timestamp_seconds(row, "modifiedAt", &row_case);
        assert!(
            modified_at >= created_at,
            "case {row_case:?}: the row was last modified at {modified_at}, before it was \
             created at {created_at}"
        );

        // RULE 2: the ARN is the one the create minted, and its id segment is a canonical
        // hyphenated UUID.
        let expected_arn = fixture
            .created_arns
            .get(name)
            .map(String::as_str)
            .expect("the row names a created table");
        let row_arn = row.get("tableARN").and_then(Value::as_str);
        assert_eq!(
            row_arn,
            Some(expected_arn),
            "case {row_case:?}: expected the row to carry the minted ARN {expected_arn:?}, \
             got {row_arn:?}"
        );
        assert_canonical_uuid(
            &table_id_of(row_arn.expect("the ARN was just compared"), &row_case),
            "the id segment of a listing row's tableARN",
            &row_case,
        );
    }
}

/// RULES 1, 2 and 3: every class of call that does not write the record is answered as its
/// class, and the listing a client re-reads afterwards is the one it read before — in the
/// modelled wire forms, row for row.
#[tokio::test]
async fn a_call_that_writes_nothing_leaves_every_summary_row_as_it_was() {
    for (index, case) in CASES.iter().enumerate() {
        let label = case.label;
        let region = REGIONS[index % REGIONS.len()];
        let fixture = Fixture::new(region, &format!("lt-unwritten-{index}"), label).await;

        let before = fixture.rows(label).await;
        assert_row_wire_forms(&before, &fixture, &format!("{label} / before the call"));

        let reply = interfere(&fixture, case).await;
        assert_eq!(
            reply.status, case.status,
            "case {label:?}: expected status {}, got {reply:?}",
            case.status
        );
        assert_eq!(
            reply.error_code.as_deref(),
            case.code,
            "case {label:?}: expected error code {:?}, got {reply:?}",
            case.code
        );
        if let Some(message) = case.message {
            assert_eq!(
                reply.message(),
                message,
                "case {label:?}: expected message {message:?}, got {actual:?}",
                actual = reply.message()
            );
        }

        // RULE 3: the whole listing is unchanged — the same rows carrying the same members
        // with the same text, which is RULES 1 and 2 held across the call as well.
        let after = fixture.rows(label).await;
        assert_eq!(
            after, before,
            "case {label:?}: the listing changed across a call that writes nothing"
        );
        assert_row_wire_forms(&after, &fixture, &format!("{label} / after the call"));
    }
}
