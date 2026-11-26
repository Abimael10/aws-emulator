// SPDX-License-Identifier: CC0-1.0

//! What a successful `UpdateTableMetadataLocation` does to the table's optimistic-
//! concurrency version, and what that makes of the token the caller presented.
//!
//! The rule under test: a write that moves a table to another metadata document ADVANCES
//! the table's `versionToken` — the write answers with the table's new token, that token is
//! the table's current one for every later read and every later precondition, and the token
//! the write was given is superseded, so presenting it again is the conflict with a
//! previous write the modelled `ConflictException` (HTTP 409) names, which changes nothing.
//!
//! The model leaves the token's lifecycle to recorded behaviour: `versionToken` is a plain
//! string with `@length(1, 2048)` and no pattern, required on this operation's input and
//! its output, documented only as "The version token of the table"
//! (`smithy_query(query="operation", operation="UpdateTableMetadataLocation")`).
//!
//! The rows span what the rule generalises over: how many successful writes a run makes,
//! the form of each location written (the metadata directory the service writes into, a
//! deeper prefix, the Gzip-compressed suffix the operation's documentation names, the
//! shortest file name a suffix admits), and whether the table's create described its
//! metadata, so the run either moves an existing pointer or sets the table's first one.

use serde_json::json;
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{Emulator, Reply};
use crate::wire_forms::assert_table_version_token;

/// The region every row works in; every assertion follows the values the creates report,
/// so the region is one choice and not a rule.
const REGION: &str = "us-west-2";

/// The namespace and the table every row works on. Both satisfy the modelled
/// `^[0-9a-z_]*$`.
const NAMESPACE: &str = "advance_ns";
const TABLE: &str = "advance_tbl";

/// One row: a run of metadata files the table is pointed at in turn.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    /// Whether the table's create describes its Iceberg metadata, which is what gives the
    /// table a metadata file to begin with.
    created_with_metadata: bool,
    /// Each entry is appended to the table's own warehouse location, which only the create
    /// knows.
    files: &'static [&'static str],
}

/// The rows.
const CASES: &[Case] = &[
    Case {
        label: "one write, moving the pointer of a table created with metadata",
        created_with_metadata: true,
        files: &["/metadata/00001-5c6d7e8f90a1b2c3d4e5f60718293a4b.metadata.json"],
    },
    Case {
        label: "one write, naming the first metadata file of a table created without it",
        created_with_metadata: false,
        files: &["/metadata/00000-0a1b2c3d4e5f60718293a4b5c6d7e8f9.metadata.json"],
    },
    Case {
        label: "three writes in a row, each under the token the one before it settled",
        created_with_metadata: true,
        files: &[
            "/metadata/00001-112233445566778899aabbccddeeff00.metadata.json",
            "/metadata/snapshots/2026/00002-ffeeddccbbaa99887766554433221100.metadata.json",
            "/metadata/00003-a1b2c3d4e5f6071829304a5b6c7d8e9f.metadata.json.gz",
        ],
    },
    Case {
        label: "two writes, the Gzip-compressed form then the shortest file name",
        created_with_metadata: false,
        files: &[
            "/metadata/00001-99aabbccddeeff00112233445566778.metadata.json.gz",
            "/0.metadata.json",
        ],
    },
];

/// One table bucket holding one namespace with one table in it: what every row starts
/// from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
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
            warehouse_location: read.text("warehouseLocation"),
        }
    }

    async fn read(&self) -> Reply {
        self.emulator
            .get_table_metadata_location(REGION, &self.bucket_arn, NAMESPACE, TABLE)
            .await
    }

    async fn describe(&self) -> Reply {
        self.emulator
            .get_table(REGION, &self.bucket_arn, NAMESPACE, TABLE)
            .await
    }

    /// `UpdateTableMetadataLocation` pointing the table at `file` under its warehouse,
    /// presenting `token`.
    async fn update(&self, token: &str, file: &str) -> Reply {
        let location = format!("{warehouse}{file}", warehouse = self.warehouse_location);
        self.emulator
            .update_table_metadata_location(
                REGION,
                &self.bucket_arn,
                NAMESPACE,
                TABLE,
                &json!({ "metadataLocation": location, "versionToken": token }),
            )
            .await
    }
}

/// The rule over every row: each accepted write answers with a token the table never
/// carried before, the reads report that token beside the location written, and the
/// superseded token is refused and moves nothing.
#[tokio::test]
async fn every_accepted_metadata_location_write_advances_the_tables_version_token() {
    for (index, case) in CASES.iter().enumerate() {
        let label = case.label;
        let fixture = Fixture::new(
            &format!("token-advance-{index}"),
            case.created_with_metadata,
            label,
        )
        .await;

        let created = fixture.read().await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: GetTableMetadataLocation: {created:?}"
        );
        // Every token the table has carried in this run, so a write that answers with one
        // the table already held is caught as a version that did not move.
        let mut carried = vec![created.text("versionToken")];
        let mut held = created.text("versionToken");

        for file in case.files {
            let step = format!("{label} / writing {file}");
            let location = format!("{warehouse}{file}", warehouse = fixture.warehouse_location);

            let updated = fixture.update(&held, file).await;
            assert_eq!(
                updated.status, 200,
                "case {step:?}: expected 200, got {updated:?}"
            );
            let settled = updated.text("versionToken");
            assert_table_version_token(&settled, "the versionToken the write settled", &step);
            assert!(
                !carried.contains(&settled),
                "case {step:?}: the write answered with {settled:?}, a token the table \
                 already carried ({carried:?}), so its version did not move"
            );
            assert_eq!(
                updated.text("metadataLocation"),
                location,
                "case {step:?}: the write reports another location: {updated:?}"
            );

            // Both reads report the token the write settled, beside the location it wrote.
            let read = fixture.read().await;
            assert_eq!(
                read.text("versionToken"),
                settled,
                "case {step:?}: GetTableMetadataLocation reports another token: {read:?}"
            );
            assert_eq!(
                read.optional_text("metadataLocation"),
                Some(location.clone()),
                "case {step:?}: GetTableMetadataLocation reports another location: {read:?}"
            );
            let described = fixture.describe().await;
            assert_eq!(
                described.text("versionToken"),
                settled,
                "case {step:?}: GetTable reports another token: {described:?}"
            );
            assert_eq!(
                described.optional_text("metadataLocation"),
                Some(location.clone()),
                "case {step:?}: GetTable reports another location: {described:?}"
            );

            // The token the write was given is spent: a second writer holding it is the
            // lost update the precondition exists to catch, refused by its fixed sentence
            // and leaving the record at the values the accepted write settled.
            let superseded = fixture
                .update(&held, "/metadata/00500-lost-update.metadata.json")
                .await;
            assert_eq!(
                superseded.status, 409,
                "case {step:?}: the superseded token {held:?}: expected 409, got \
                 {superseded:?}"
            );
            assert_eq!(
                superseded.error_code.as_deref(),
                Some("ConflictException"),
                "case {step:?}: unexpected error code on the superseded token: {superseded:?}"
            );
            assert_eq!(
                superseded.message(),
                TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
                "case {step:?}: unexpected message on the superseded token: {superseded:?}"
            );
            let after = fixture.read().await;
            assert_eq!(
                after.optional_text("metadataLocation"),
                Some(location),
                "case {step:?}: the refused write moved the location: {after:?}"
            );
            assert_eq!(
                after.text("versionToken"),
                settled,
                "case {step:?}: the refused write changed the token: {after:?}"
            );

            carried.push(settled.clone());
            held = settled;
        }

        // The token the run settled on is the table's precondition everywhere, not only on
        // this operation: the conditional delete takes it, and the superseded one does not.
        let stale = carried[carried.len() - 2].clone();
        let refused = fixture
            .emulator
            .delete_table(
                REGION,
                &fixture.bucket_arn,
                NAMESPACE,
                TABLE,
                &[("versionToken", stale.clone())],
            )
            .await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: DeleteTable presenting the superseded token {stale:?}: expected \
             409, got {refused:?}"
        );
        let deleted = fixture
            .emulator
            .delete_table(
                REGION,
                &fixture.bucket_arn,
                NAMESPACE,
                TABLE,
                &[("versionToken", held.clone())],
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {label:?}: DeleteTable presenting the token {held:?} the run settled: \
             expected 204, got {deleted:?}"
        );
    }
}

/// Two writers that read the same table version: the first write lands, the second is
/// refused as the conflict, and the table holds exactly what the first one wrote — which is
/// what a token that advances buys a client over one that never moves.
#[tokio::test]
async fn two_writers_holding_one_table_version_cannot_both_commit() {
    let case = "two writers on one version";
    let fixture = Fixture::new("token-advance-lost-update", true, case).await;

    let read = fixture.read().await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetTableMetadataLocation: {read:?}"
    );
    // Both writers read the table once, so both hold the same version.
    let first_view = read.text("versionToken");
    let second_view = read.text("versionToken");

    let first_file = "/metadata/00001-first-writer.metadata.json";
    let first = fixture.update(&first_view, first_file).await;
    assert_eq!(
        first.status, 200,
        "case {case:?}: the first writer: expected 200, got {first:?}"
    );
    let current = first.text("versionToken");

    let second = fixture
        .update(&second_view, "/metadata/00001-second-writer.metadata.json")
        .await;
    assert_eq!(
        second.status, 409,
        "case {case:?}: the second writer presenting the same version: expected 409, got \
         {second:?}"
    );
    assert_eq!(
        second.message(),
        TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
        "case {case:?}: unexpected message on the second writer: {second:?}"
    );

    let after = fixture.read().await;
    assert_eq!(
        after.optional_text("metadataLocation"),
        Some(format!(
            "{warehouse}{first_file}",
            warehouse = fixture.warehouse_location
        )),
        "case {case:?}: the table must hold what the first writer wrote: {after:?}"
    );
    assert_eq!(
        after.text("versionToken"),
        current,
        "case {case:?}: the table must carry the token the first write settled: {after:?}"
    );

    // And the refused writer re-reads the table and retries under the version it reports.
    let retried = fixture
        .update(&current, "/metadata/00002-second-writer.metadata.json")
        .await;
    assert_eq!(
        retried.status, 200,
        "case {case:?}: the retry under the current version: expected 200, got {retried:?}"
    );
    assert_ne!(
        retried.text("versionToken"),
        current,
        "case {case:?}: the retry answered with the token it was given: {retried:?}"
    );
}
