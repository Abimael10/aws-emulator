// SPDX-License-Identifier: CC0-1.0

//! WHEN a table's `versionToken` changes, across every write of this service that touches
//! a table.
//!
//! The rule under test: a table's `versionToken` names the metadata document THIS SERVICE
//! holds for the table, so it is minted when the table is created and re-minted only when
//! the service writes a new metadata document for it (a catalog commit); a write that only
//! records a pointer, a coordinate, a configuration or a tag the CALLER supplied — under
//! the caller's token as its precondition, where the operation takes one — leaves the token
//! exactly as it was and answers with it, so the view a caller holds stays current and its
//! next conditional call is honoured rather than refused.
//!
//! The model leaves this to recorded behaviour: `versionToken` is a plain
//! `@length(1, 2048)` string documented only as "The version token of the table"
//! (`smithy_query(query="operation", operation="UpdateTableMetadataLocation,RenameTable,
//! DeleteTable,GetTable")`), so nothing in the model says what rotates it.
//!
//! The rows span what the rule generalises over: which write is made (the metadata pointer,
//! the table's coordinates, a maintenance configuration, tags added and removed, a catalog
//! commit that writes a metadata document and one that writes none), the form of the
//! metadata location written (the metadata directory, a deeper prefix, the Gzip-compressed
//! suffix), whether the write presents the caller's token at all, and whether the table was
//! created with a metadata document of its own.
//!
//! The second test pins the WIDTH of the minted token over many mints: the token is one
//! unbroken run of lowercase hexadecimal characters of a fixed width, so a form whose
//! spelling shrinks with the value it encodes (a number) is ruled out however small a value
//! one mint happens to draw.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, TableMaintenanceType, TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{Emulator, Reply};
use crate::wire_forms::{assert_table_version_token, VERSION_TOKEN_CHARACTERS};

/// The region every case works in; the assertions follow the values the creates report, so
/// the region is one choice and not a rule.
const REGION: &str = "us-east-1";

/// The namespace every case's table lives in, and the name the table is created under.
/// Both satisfy the modelled `^[0-9a-z_]*$`.
const NAMESPACE: &str = "token_ns";
const TABLE: &str = "token_tbl";

/// The write one row makes to the table, after which the row reads the token back.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `UpdateTableMetadataLocation`, pointing the table at the file named by the
    /// table's own warehouse location plus this suffix.
    MetadataLocation(&'static str),
    /// `RenameTable` to another name in the same namespace, presenting the caller's
    /// token when `with_token` is set (the member is optional on this operation).
    Rename {
        new_name: &'static str,
        with_token: bool,
    },
    /// `PutTableMaintenanceConfiguration`: service configuration of the table.
    Maintenance,
    /// `TagResource` on the table's ARN.
    Tag,
    /// `UntagResource` of a key the row attached first.
    Untag,
    /// An Iceberg catalog commit carrying an update, which is the service writing a new
    /// metadata document for the table.
    Commit,
    /// A catalog commit that asks for no update at all, so the service writes none.
    CommitWithoutUpdates,
}

/// One row: the write it makes, and whether the service writes the table's metadata
/// document while making it.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    write: Write,
    /// Whether the table's create describes its Iceberg metadata, which is what gives the
    /// table a metadata document to begin with.
    created_with_metadata: bool,
    /// Whether the service writes a NEW metadata document for the table while making this
    /// write, which is the one thing that re-mints the token.
    writes_metadata_document: bool,
}

/// The rows.
const CASES: &[Case] = &[
    Case {
        label: "the next metadata file in the metadata directory",
        write: Write::MetadataLocation("/metadata/00001-0f1e2d3c4b5a69788796a5b4c3d2e1f0.metadata.json"),
        created_with_metadata: true,
        writes_metadata_document: false,
    },
    Case {
        label: "the first metadata file of a table created without metadata",
        write: Write::MetadataLocation("/metadata/00000-112233445566778899aabbccddeeff00.metadata.json"),
        created_with_metadata: false,
        writes_metadata_document: false,
    },
    Case {
        label: "a Gzip-compressed metadata file under a deeper prefix",
        write: Write::MetadataLocation("/metadata/snapshots/2026/v7.metadata.json.gz"),
        created_with_metadata: true,
        writes_metadata_document: false,
    },
    Case {
        label: "a rename presenting the caller's token",
        write: Write::Rename {
            new_name: "token_tbl_moved",
            with_token: true,
        },
        created_with_metadata: true,
        writes_metadata_document: false,
    },
    Case {
        label: "a rename presenting no token at all",
        write: Write::Rename {
            new_name: "token_tbl_moved_again",
            with_token: false,
        },
        created_with_metadata: false,
        writes_metadata_document: false,
    },
    Case {
        label: "a maintenance configuration put on the table",
        write: Write::Maintenance,
        created_with_metadata: true,
        writes_metadata_document: false,
    },
    Case {
        label: "tags attached to the table",
        write: Write::Tag,
        created_with_metadata: false,
        writes_metadata_document: false,
    },
    Case {
        label: "a tag removed from the table",
        write: Write::Untag,
        created_with_metadata: true,
        writes_metadata_document: false,
    },
    Case {
        label: "a catalog commit that writes a new metadata document",
        write: Write::Commit,
        created_with_metadata: true,
        writes_metadata_document: true,
    },
    Case {
        label: "a catalog commit that asks for no update",
        write: Write::CommitWithoutUpdates,
        created_with_metadata: true,
        writes_metadata_document: false,
    },
];

/// One table bucket holding one namespace with one table in it, which every row starts
/// from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
    table_arn: String,
    /// The table's current name, which a rename moves.
    name: String,
    warehouse_location: String,
}

impl Fixture {
    async fn new(bucket: &str, metadata: bool, case: &str) -> Self {
        let emulator = Emulator::new();
        let created = emulator
            .create_table_bucket(REGION, &json!({ "name": bucket }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTableBucket: {created:?}"
        );
        let bucket_arn = created.text("arn");
        let namespace = emulator
            .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [NAMESPACE] }))
            .await;
        assert_eq!(
            namespace.status, 200,
            "case {case:?}: CreateNamespace: {namespace:?}"
        );
        // `format` is required and `ICEBERG` is the only value the modelled
        // `OpenTableFormat` enum declares.
        let mut request = json!({ "name": TABLE, "format": OpenTableFormat::Iceberg.as_wire() });
        if metadata {
            request["metadata"] = json!({ "iceberg": { "schema": { "fields": [
                { "name": "id", "type": "long", "required": true },
                { "name": "label", "type": "string" },
            ]}}});
        }
        let table = emulator
            .create_table(REGION, &bucket_arn, NAMESPACE, &request)
            .await;
        assert_eq!(table.status, 200, "case {case:?}: CreateTable: {table:?}");
        let read = emulator
            .get_table(REGION, &bucket_arn, NAMESPACE, TABLE)
            .await;
        assert_eq!(read.status, 200, "case {case:?}: GetTable: {read:?}");
        Self {
            emulator,
            bucket_arn,
            table_arn: read.text("tableARN"),
            name: TABLE.to_string(),
            warehouse_location: read.text("warehouseLocation"),
        }
    }

    /// `GetTable` for the table at its current name.
    async fn describe(&self) -> Reply {
        self.emulator
            .get_table(REGION, &self.bucket_arn, NAMESPACE, &self.name)
            .await
    }

    /// `GetTableMetadataLocation` for the table at its current name.
    async fn read_location(&self) -> Reply {
        self.emulator
            .get_table_metadata_location(REGION, &self.bucket_arn, NAMESPACE, &self.name)
            .await
    }

    /// `UpdateTableMetadataLocation` presenting `token`, which is how a row checks whether
    /// a token a caller holds is still the table's.
    async fn update_under(&self, token: &str, file: &str) -> Reply {
        let location = format!("{warehouse}{file}", warehouse = self.warehouse_location);
        self.emulator
            .update_table_metadata_location(
                REGION,
                &self.bucket_arn,
                NAMESPACE,
                &self.name,
                &json!({ "metadataLocation": location, "versionToken": token }),
            )
            .await
    }

    /// Make the row's write, which may move the table's name.
    async fn write(&mut self, case: &Case, held: &str) {
        let label = case.label;
        match case.write {
            Write::MetadataLocation(file) => {
                let updated = self.update_under(held, file).await;
                assert_eq!(
                    updated.status, 200,
                    "case {label:?}: UpdateTableMetadataLocation: {updated:?}"
                );
                assert_eq!(
                    updated.text("versionToken"),
                    held,
                    "case {label:?}: the write answered with a token the caller never \
                     presented: {updated:?}"
                );
            }
            Write::Rename {
                new_name,
                with_token,
            } => {
                let mut request = json!({ "newName": new_name });
                if with_token {
                    request["versionToken"] = Value::from(held);
                }
                let renamed = self
                    .emulator
                    .rename_table(REGION, &self.bucket_arn, NAMESPACE, &self.name, &request)
                    .await;
                assert_eq!(
                    renamed.status, 204,
                    "case {label:?}: RenameTable: {renamed:?}"
                );
                self.name = new_name.to_string();
            }
            Write::Maintenance => {
                let put = self
                    .emulator
                    .put_table_maintenance_configuration(
                        REGION,
                        &self.bucket_arn,
                        NAMESPACE,
                        &self.name,
                        TableMaintenanceType::IcebergCompaction.as_wire(),
                        &json!({
                            "status": "disabled",
                            "settings": {
                                "icebergCompaction": {
                                    "targetFileSizeMB": 128,
                                    "strategy": "binpack",
                                },
                            },
                        }),
                    )
                    .await;
                assert_eq!(
                    put.status, 204,
                    "case {label:?}: PutTableMaintenanceConfiguration: {put:?}"
                );
            }
            Write::Tag => {
                let tagged = self
                    .emulator
                    .tag_resource(REGION, &self.table_arn, &json!({ "stage": "probe" }))
                    .await;
                assert_eq!(tagged.status, 200, "case {label:?}: TagResource: {tagged:?}");
            }
            Write::Untag => {
                let removed = self
                    .emulator
                    .untag_resource(REGION, &self.table_arn, &["stage"])
                    .await;
                assert_eq!(
                    removed.status, 204,
                    "case {label:?}: UntagResource: {removed:?}"
                );
            }
            Write::Commit | Write::CommitWithoutUpdates => {
                let updates = if matches!(case.write, Write::Commit) {
                    json!([{ "action": "set-properties", "updates": { "owner": "analytics" } }])
                } else {
                    json!([])
                };
                let commit = self
                    .emulator
                    .commit_table(
                        REGION,
                        &self.bucket_arn,
                        NAMESPACE,
                        &self.name,
                        &json!({ "requirements": [], "updates": updates }),
                    )
                    .await;
                assert_eq!(
                    commit.status, 200,
                    "case {label:?}: the catalog commit: {commit:?}"
                );
            }
        }
    }
}

/// The write a row makes before the one under test, where the row needs state to work on.
async fn prepare(fixture: &Fixture, case: &Case) {
    if matches!(case.write, Write::Untag) {
        let tagged = fixture
            .emulator
            .tag_resource(
                REGION,
                &fixture.table_arn,
                &json!({ "stage": "probe", "owner": "analytics" }),
            )
            .await;
        assert_eq!(
            tagged.status, 200,
            "case {label:?}: tagging the table before the removal: {tagged:?}",
            label = case.label
        );
    }
}

/// The rule, over every row: the token after the write, and whether the token the caller
/// held is still the table's.
#[tokio::test]
async fn only_a_metadata_document_the_service_writes_re_mints_a_tables_version_token() {
    for (index, case) in CASES.iter().enumerate() {
        let label = case.label;
        let mut fixture = Fixture::new(
            &format!("token-lifecycle-{index}"),
            case.created_with_metadata,
            label,
        )
        .await;

        // The token the caller holds: what every read of the table reports before the
        // write, in the form this service mints a table's token in.
        let before = fixture.describe().await;
        assert_eq!(before.status, 200, "case {label:?}: GetTable: {before:?}");
        let held = before.text("versionToken");
        assert_table_version_token(&held, "the token the create minted", label);
        assert_eq!(
            fixture.read_location().await.text("versionToken"),
            held,
            "case {label:?}: the two reads of the table report different tokens"
        );

        prepare(&fixture, case).await;
        fixture.write(case, &held).await;

        // What the reads report afterwards: one token, in the service's token form, and
        // re-minted only where the service wrote the table's metadata document.
        let after = fixture.describe().await;
        assert_eq!(
            after.status, 200,
            "case {label:?}: GetTable after the write: {after:?}"
        );
        let reported = after.text("versionToken");
        assert_table_version_token(&reported, "the token after the write", label);
        assert_eq!(
            fixture.read_location().await.text("versionToken"),
            reported,
            "case {label:?}: the two reads report different tokens after the write"
        );
        if case.writes_metadata_document {
            assert_ne!(
                reported, held,
                "case {label:?}: the service wrote a new metadata document and still \
                 reports the token {held:?}, so a caller cannot tell the document moved"
            );
        } else {
            assert_eq!(
                reported, held,
                "case {label:?}: expected the table to keep the token {held:?}, got \
                 {reported:?}"
            );
        }

        // And what that means for the caller: the token it holds is still the table's
        // precondition, unless the service minted a new one — in which case the stale one
        // is the modelled conflict and the current one is honoured.
        let probe = fixture
            .update_under(&held, "/metadata/00042-probe.metadata.json")
            .await;
        if case.writes_metadata_document {
            assert_eq!(
                probe.status, 409,
                "case {label:?}: the superseded token {held:?} must be refused as a \
                 conflict, got {probe:?}"
            );
            assert_eq!(
                probe.message(),
                TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
                "case {label:?}: unexpected message on the stale-token refusal: {probe:?}"
            );
            let current = fixture
                .update_under(&reported, "/metadata/00042-probe.metadata.json")
                .await;
            assert_eq!(
                current.status, 200,
                "case {label:?}: the token the commit minted must be the table's: {current:?}"
            );
        } else {
            assert_eq!(
                probe.status, 200,
                "case {label:?}: the token {held:?} the caller holds must still be the \
                 table's, got {probe:?}"
            );
            assert_eq!(
                probe.text("versionToken"),
                held,
                "case {label:?}: the conditional write answered with another token: {probe:?}"
            );
        }
    }
}

/// How many tables the width test mints tokens for. One mint says nothing about the width
/// of the form: a token that spells a NUMBER is as wide as the value it drew, so only a run
/// of mints tells a fixed-width form from a value-dependent one.
const MINTS: usize = 24;

/// The token's FORM, over a run of mints: every one is the same unbroken run of lowercase
/// hexadecimal characters, no two tables share one, and each read of a table echoes exactly
/// the value its create minted.
#[tokio::test]
async fn every_minted_table_version_token_is_one_fixed_width_run_of_lowercase_hex() {
    let emulator = Emulator::new();
    let bucket = emulator
        .create_table_bucket(REGION, &json!({ "name": "token-form-mints" }))
        .await;
    assert_eq!(bucket.status, 200, "CreateTableBucket: {bucket:?}");
    let bucket_arn = bucket.text("arn");
    let namespace = emulator
        .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    assert_eq!(namespace.status, 200, "CreateNamespace: {namespace:?}");

    let mut minted: Vec<(String, String)> = Vec::new();
    for mint in 0..MINTS {
        // The names span both create forms and stay inside the modelled `^[0-9a-z_]*$`.
        let table = format!("tbl_{mint:02}");
        let created = emulator
            .create_table(
                REGION,
                &bucket_arn,
                NAMESPACE,
                &json!({
                    "name": table,
                    "format": OpenTableFormat::Iceberg.as_wire(),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {table:?}: CreateTable: {created:?}"
        );
        let token = created.text("versionToken");
        assert_table_version_token(&token, "CreateTable's versionToken", &table);
        let read = emulator
            .get_table(REGION, &bucket_arn, NAMESPACE, &table)
            .await;
        assert_eq!(read.status, 200, "case {table:?}: GetTable: {read:?}");
        assert_eq!(
            read.text("versionToken"),
            token,
            "case {table:?}: GetTable reports a token the create never minted: {read:?}"
        );
        minted.push((table, token));
    }

    for (table, token) in &minted {
        assert_eq!(
            token.chars().count(),
            VERSION_TOKEN_CHARACTERS,
            "case {table:?}: the token {token:?} is not {VERSION_TOKEN_CHARACTERS} characters \
             wide, so the form's width follows the value it encodes"
        );
        let shared: Vec<&String> = minted
            .iter()
            .filter(|(other, value)| other != table && value == token)
            .map(|(other, _)| other)
            .collect();
        assert!(
            shared.is_empty(),
            "case {table:?}: the token {token:?} was minted for {shared:?} as well"
        );
    }
}
