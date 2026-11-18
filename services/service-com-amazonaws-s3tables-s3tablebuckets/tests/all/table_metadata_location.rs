// SPDX-License-Identifier: CC0-1.0

//! Where a table's metadata file is: the read that reports it, and the write that moves
//! it.
//!
//! RULE 1 — `GetTableMetadataLocation` REPORTS the one stored table record and changes
//! nothing: the table's current `versionToken` and its `warehouseLocation` always, and
//! `metadataLocation` only once something has written a metadata file for the table
//! (`smithy_query(query="operation", operation="GetTableMetadataLocation")`: GET
//! `/tables/{tableBucketARN}/{namespace}/{name}/metadata-location`, 200, `versionToken`
//! and `warehouseLocation` required, `metadataLocation` optional, `@readonly`).
//!
//! RULE 2 — `UpdateTableMetadataLocation` POINTS the table at the metadata file the
//! request names, under the precondition the caller's `versionToken` states, and advances
//! that token: the token guards the table's open-table metadata and this call is what
//! moves it, so every later read reports the new location and the new token, and the token
//! the caller presented is spent
//! (`smithy_query(query="operation", operation="UpdateTableMetadataLocation")`: PUT on the
//! same path, 200, required `versionToken` + `metadataLocation` in, and `name`/`tableARN`/
//! `namespace`/`versionToken`/`metadataLocation` out).
//!
//! RULE 3 — a write the store rules out changes nothing and is refused as its class: a
//! `metadataLocation` that is not an S3 URI under the table's warehouse location naming a
//! metadata file of the table's format is a `BadRequestException` (HTTP 400) — the
//! operation's documentation states both halves of that rule ("The metadata location of a
//! table must be an S3 URI that begins with the table's warehouse location. The metadata
//! location for an Apache Iceberg table must end with .metadata.json, or if the metadata
//! file is Gzip-compressed, .metadata.json.gz."); a `versionToken` that is not the table's
//! current one is the conflict with a previous write `ConflictException` (HTTP 409) names;
//! a bucket, namespace or table the store does not hold is a `NotFoundException` (HTTP
//! 404).
//!
//! Each rule is asserted over a TABLE of cases spanning what it generalises over: whether
//! the table was created with metadata, the form of the location written (the metadata
//! directory, a deeper prefix, the Gzip-compressed suffix, the shortest file name), and
//! every class of refusal.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, BUCKET_NOT_FOUND_MESSAGE, INVALID_METADATA_LOCATION_MESSAGE,
    NAMESPACE_NOT_FOUND_MESSAGE, TABLE_NOT_FOUND_MESSAGE, TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
};

use crate::harness::{expected_bucket_arn, Emulator, Reply};
use crate::wire_forms::assert_table_version_token;

/// The region every case in this suite is scoped to; the assertions follow the values the
/// create reports, so the region is one choice and not a rule.
const REGION: &str = "us-west-2";

/// The namespace and the table every case works on. Both satisfy the modelled
/// `^[0-9a-z_]*$`.
const NAMESPACE: &str = "metadata_ns";
const TABLE: &str = "metadata_tbl";

/// One table bucket holding one namespace with one table in it: the state every case
/// below starts from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
}

impl Fixture {
    /// `metadata` says whether the create describes the table's Iceberg metadata, which
    /// is what gives the table a metadata file of its own to begin with.
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
        let mut request = json!({
            "name": TABLE,
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
        let table = emulator
            .create_table(REGION, &bucket_arn, NAMESPACE, &request)
            .await;
        assert_eq!(table.status, 200, "case {case:?}: CreateTable: {table:?}");
        Self {
            emulator,
            bucket_arn,
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

    async fn update(&self, request: &Value) -> Reply {
        self.emulator
            .update_table_metadata_location(REGION, &self.bucket_arn, NAMESPACE, TABLE, request)
            .await
    }
}

/// RULE 1: the read answers out of the stored record, agrees with `GetTable` on every
/// member they share, and leaves the record as it was.
#[tokio::test]
async fn the_metadata_location_read_reports_the_stored_table() {
    // Both create forms: a table whose create described its metadata already has a
    // metadata file, one whose create did not has none yet.
    for (index, metadata) in [false, true].into_iter().enumerate() {
        let case = if metadata {
            "a table created with metadata"
        } else {
            "a table created without metadata"
        };
        let fixture = Fixture::new(&format!("metadata-read-{index}"), metadata, case).await;

        let read = fixture.read().await;
        assert_eq!(read.status, 200, "case {case:?}: expected 200, got {read:?}");
        let described = fixture.describe().await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: GetTable: {described:?}"
        );
        assert_eq!(
            read.text("versionToken"),
            described.text("versionToken"),
            "case {case:?}: the read reports a different versionToken from GetTable"
        );
        assert_eq!(
            read.text("warehouseLocation"),
            described.text("warehouseLocation"),
            "case {case:?}: the read reports a different warehouseLocation from GetTable"
        );
        assert_eq!(
            read.optional_text("metadataLocation"),
            described.optional_text("metadataLocation"),
            "case {case:?}: the read reports a different metadataLocation from GetTable"
        );
        assert_eq!(
            read.optional_text("metadataLocation").is_some(),
            metadata,
            "case {case:?}: a metadata file is reported only for a table created with \
             metadata, got {read:?}"
        );

        // A report changes nothing a later caller sees: the second read is the first.
        let again = fixture.read().await;
        assert_eq!(
            again.body, read.body,
            "case {case:?}: a second read answered differently: {again:?}"
        );
    }
}

/// One accepted write: the path under the table's warehouse location the request names.
#[derive(Debug, Clone, Copy)]
struct Accepted {
    label: &'static str,
    /// Appended to the table's own warehouse location, which only the create knows.
    under_warehouse: &'static str,
    /// Whether the table's create described its metadata, so the write moves an existing
    /// metadata file rather than naming the table's first one.
    created_with_metadata: bool,
}

/// The accepted writes: they span the metadata directory the service itself writes into,
/// a deeper prefix, the Gzip-compressed suffix the operation's documentation names, the
/// shortest file name either suffix admits, and both create forms.
const ACCEPTED: &[Accepted] = &[
    Accepted {
        label: "the next metadata file in the metadata directory",
        under_warehouse: "/metadata/00001-9d8a7d2f4f5a4a0a8f1b2c3d4e5f6071.metadata.json",
        created_with_metadata: true,
    },
    Accepted {
        label: "the first metadata file of a table created without metadata",
        under_warehouse: "/metadata/00000-6f1c4b2a2d7e4f3b9a0c5d6e7f801122.metadata.json",
        created_with_metadata: false,
    },
    Accepted {
        label: "a Gzip-compressed metadata file",
        under_warehouse: "/metadata/00002-aa11bb22cc33dd44ee55ff6677889900.metadata.json.gz",
        created_with_metadata: true,
    },
    Accepted {
        label: "a deeper prefix under the warehouse",
        under_warehouse: "/metadata/snapshots/2026/v3.metadata.json",
        created_with_metadata: true,
    },
    Accepted {
        label: "the shortest file name the suffix admits",
        under_warehouse: "/0.metadata.json",
        created_with_metadata: false,
    },
];

/// RULE 2: the write points the table at the file it names, advances the token, and every
/// read afterwards agrees.
#[tokio::test]
async fn an_update_points_the_table_at_the_metadata_file_it_names() {
    for (index, case) in ACCEPTED.iter().enumerate() {
        let label = case.label;
        let fixture = Fixture::new(
            &format!("metadata-update-{index}"),
            case.created_with_metadata,
            label,
        )
        .await;

        let before = fixture.read().await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: GetTableMetadataLocation: {before:?}"
        );
        let warehouse = before.text("warehouseLocation");
        let token = before.text("versionToken");
        let held = before.optional_text("metadataLocation");
        let location = format!("{warehouse}{under}", under = case.under_warehouse);

        let updated = fixture
            .update(&json!({ "metadataLocation": location, "versionToken": token }))
            .await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected 200, got {updated:?}"
        );
        assert_eq!(
            updated.text("metadataLocation"),
            location,
            "case {label:?}: the write reports a different location: {updated:?}"
        );
        assert_eq!(
            updated.text("name"),
            TABLE,
            "case {label:?}: the write reports a different name: {updated:?}"
        );
        assert_eq!(
            updated.body.get("namespace"),
            Some(&json!([NAMESPACE])),
            "case {label:?}: the write reports a different namespace: {updated:?}"
        );
        assert_eq!(
            updated.text("tableARN"),
            fixture.describe().await.text("tableARN"),
            "case {label:?}: the write reports a different tableARN from the describe"
        );
        let advanced = updated.text("versionToken");
        assert!(
            advanced != token,
            "case {label:?}: the write left the version token at {token:?}, so a caller \
             cannot tell the metadata moved"
        );
        assert_table_version_token(&advanced, "the versionToken an update settles", label);
        assert!(
            held.as_deref() != Some(location.as_str()),
            "case {label:?}: the case does not move the location at all (it was already \
             {held:?})"
        );

        // The reads: the location and the token the write settled, out of the store.
        let read = fixture.read().await;
        assert_eq!(
            read.optional_text("metadataLocation"),
            Some(location.clone()),
            "case {label:?}: GetTableMetadataLocation after the write: {read:?}"
        );
        assert_eq!(
            read.text("versionToken"),
            advanced,
            "case {label:?}: GetTableMetadataLocation reports a different token: {read:?}"
        );
        assert_eq!(
            read.text("warehouseLocation"),
            warehouse,
            "case {label:?}: the write changed the table's warehouse location"
        );
        let described = fixture.describe().await;
        assert_eq!(
            described.optional_text("metadataLocation"),
            Some(location.clone()),
            "case {label:?}: GetTable after the write: {described:?}"
        );
        assert_eq!(
            described.text("versionToken"),
            advanced,
            "case {label:?}: GetTable reports a different token: {described:?}"
        );

        // The presented token is spent: the same write again is the stale-token conflict,
        // and the location stays where this write put it.
        let replayed = fixture
            .update(&json!({ "metadataLocation": location, "versionToken": token }))
            .await;
        assert_eq!(
            replayed.status, 409,
            "case {label:?}: re-presenting the spent token: expected 409, got {replayed:?}"
        );
        assert_eq!(
            replayed.message(),
            TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
            "case {label:?}: unexpected message on the stale-token refusal: {replayed:?}"
        );
        assert_eq!(
            fixture.read().await.optional_text("metadataLocation"),
            Some(location),
            "case {label:?}: the refused write moved the metadata location"
        );
    }
}

/// One refused write: how it addresses the table, and what it asks for.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    label: &'static str,
    /// The namespace path label, or `None` for the namespace the table lives in.
    namespace: Option<&'static str>,
    /// The name path label, or `None` for the table the fixture created.
    name: Option<&'static str>,
    /// Whether the bucket ARN names a bucket the store does not hold.
    absent_bucket: bool,
    /// The location, appended to the table's warehouse location when
    /// `under_warehouse` is set, or sent as written when it is not.
    under_warehouse: Option<&'static str>,
    absolute_location: Option<&'static str>,
    /// Whether the request presents a token that is not the table's current one.
    stale_version_token: bool,
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// A location a refusal case sends as written: it is a well-formed S3 URI naming a
/// metadata file, but not under the table's warehouse.
const OTHER_WAREHOUSE_LOCATION: &str =
    "s3://11111111-2222-3333-4444-555555555555--table-s3/metadata/00001-x.metadata.json";

/// The refusals: one per class the rule names, spanning both halves of the documented
/// location rule (outside the warehouse, and not naming a metadata file) and every
/// resource the request addresses.
const REFUSALS: &[Refusal] = &[
    Refusal {
        label: "a location under another table's warehouse",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: None,
        absolute_location: Some(OTHER_WAREHOUSE_LOCATION),
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: INVALID_METADATA_LOCATION_MESSAGE,
    },
    Refusal {
        label: "a location that is not an S3 URI at all",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: None,
        absolute_location: Some("/tmp/metadata/00001-x.metadata.json"),
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: INVALID_METADATA_LOCATION_MESSAGE,
    },
    Refusal {
        label: "a warehouse name the table's warehouse is only a prefix of",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: Some("-other/metadata/00001-x.metadata.json"),
        absolute_location: None,
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: INVALID_METADATA_LOCATION_MESSAGE,
    },
    Refusal {
        label: "a file that is not a metadata file",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: Some("/data/00000-0-abc.parquet"),
        absolute_location: None,
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: INVALID_METADATA_LOCATION_MESSAGE,
    },
    Refusal {
        label: "the warehouse location itself",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: Some(""),
        absolute_location: None,
        stale_version_token: false,
        status: 400,
        code: "BadRequestException",
        message: INVALID_METADATA_LOCATION_MESSAGE,
    },
    Refusal {
        label: "a version token that is not the table's",
        namespace: None,
        name: None,
        absent_bucket: false,
        under_warehouse: Some("/metadata/00001-x.metadata.json"),
        absolute_location: None,
        stale_version_token: true,
        status: 409,
        code: "ConflictException",
        message: TABLE_VERSION_TOKEN_MISMATCH_MESSAGE,
    },
    Refusal {
        label: "a table the namespace does not hold",
        namespace: None,
        name: Some("absent_table"),
        absent_bucket: false,
        under_warehouse: Some("/metadata/00001-x.metadata.json"),
        absolute_location: None,
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: TABLE_NOT_FOUND_MESSAGE,
    },
    Refusal {
        label: "a namespace the bucket does not hold",
        namespace: Some("absent_namespace"),
        name: None,
        absent_bucket: false,
        under_warehouse: Some("/metadata/00001-x.metadata.json"),
        absolute_location: None,
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: NAMESPACE_NOT_FOUND_MESSAGE,
    },
    Refusal {
        label: "a table bucket the store does not hold",
        namespace: None,
        name: None,
        absent_bucket: true,
        under_warehouse: Some("/metadata/00001-x.metadata.json"),
        absolute_location: None,
        stale_version_token: false,
        status: 404,
        code: "NotFoundException",
        message: BUCKET_NOT_FOUND_MESSAGE,
    },
];

/// RULE 3 over every class: the refusal the class names, and a record the write left
/// exactly as it was.
#[tokio::test]
async fn an_update_the_store_rules_out_states_its_class_and_changes_nothing() {
    for (index, refusal) in REFUSALS.iter().enumerate() {
        let case = refusal.label;
        let fixture = Fixture::new(&format!("metadata-refusals-{index}"), true, case).await;

        let before = fixture.read().await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: GetTableMetadataLocation: {before:?}"
        );
        let warehouse = before.text("warehouseLocation");
        let token = before.text("versionToken");
        let held = before.optional_text("metadataLocation");

        let location = match (refusal.under_warehouse, refusal.absolute_location) {
            (Some(under), _) => format!("{warehouse}{under}"),
            (None, Some(absolute)) => absolute.to_string(),
            (None, None) => unreachable!("every refusal case names a location"),
        };
        // A token of this service's own minting that is not this table's: the one the
        // bucket's own read reports is a token of another record entirely.
        let presented = if refusal.stale_version_token {
            fixture
                .emulator
                .get_table_bucket(REGION, &fixture.bucket_arn)
                .await
                .text("tableBucketId")
        } else {
            token.clone()
        };
        let bucket_arn = if refusal.absent_bucket {
            expected_bucket_arn(REGION, "metadata-absent-bucket")
        } else {
            fixture.bucket_arn.clone()
        };
        let refused = fixture
            .emulator
            .update_table_metadata_location(
                REGION,
                &bucket_arn,
                refusal.namespace.unwrap_or(NAMESPACE),
                refusal.name.unwrap_or(TABLE),
                &json!({ "metadataLocation": location, "versionToken": presented }),
            )
            .await;
        assert_eq!(
            refused.status, refusal.status,
            "case {case:?}: expected status {expected}, got {refused:?}",
            expected = refusal.status
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(refusal.code),
            "case {case:?}: expected error type {code}, got {refused:?}",
            code = refusal.code
        );
        assert_eq!(
            refused.message(),
            refusal.message,
            "case {case:?}: unexpected message: {refused:?}"
        );

        // Nothing moved: the record still reports the location and the token it did.
        let after = fixture.read().await;
        assert_eq!(
            after.optional_text("metadataLocation"),
            held,
            "case {case:?}: the refused write moved the metadata location: {after:?}"
        );
        assert_eq!(
            after.text("versionToken"),
            token,
            "case {case:?}: the refused write advanced the version token: {after:?}"
        );
    }
}
