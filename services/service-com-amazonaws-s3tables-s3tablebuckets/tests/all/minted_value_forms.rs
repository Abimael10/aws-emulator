// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of the server-derived values the CONFIGURATION reads and the table's
//! locations report — the second half of the two rules `wire_forms.rs` states, over the
//! members that suite does not reach.
//!
//! RULE 1 — a timestamp member whose model declares `@timestampFormat("date-time")` goes
//! on the JSON wire as an RFC 3339 / ISO 8601 date-time STRING, never as the epoch-seconds
//! number the `restJson1` protocol defaults to. `NamespaceSummary.createdAt` declares it
//! (`smithy_query(query="shape", shape="NamespaceSummary")` reports
//! `"timestampFormat": "date-time"` on `createdAt`), so a `ListNamespaces` row is one of
//! the places the rule holds, and the row must report the same instant, in the same text,
//! as the read of the namespace it stands for.
//!
//! RULE 2 — every identifier this service MINTS is in the canonical hyphenated UUID form
//! (8-4-4-4-12, lowercase). The model constrains none of these values beyond a length, so
//! the form is recorded behaviour, stated once in [`assert_canonical_uuid`]. The values
//! asserted here are the ones no other suite pins to that form:
//!
//! * a `ListNamespaces` row's `namespaceId`;
//! * the table id the table's `warehouseLocation` is named for, and the metadata-file id
//!   inside its `metadataLocation`, as `GetTable` and `GetTableMetadataLocation` report
//!   them.
//!
//! A VERSION TOKEN is NOT one of them — neither a table's own nor a replication
//! configuration's: it is an opaque token rather than an identifier, minted in the token
//! form `wire_forms.rs` states ([`assert_table_version_token`]), which the read of the
//! table's metadata location and the two replication puts below are pinned to.
//!
//! The `id` of a table bucket's metrics configuration is NOT one of them: that
//! configuration covers the whole bucket and the service names it with a fixed
//! identifier, which `table_bucket_metrics_configuration.rs` and
//! `metrics_configuration_fixed_id.rs` pin.
//!
//! Both rules are asserted over a TABLE of fixtures spanning the region, the bucket-name
//! length and form, one, two and three namespaces per bucket, a table created with and
//! without metadata (so a metadata file exists and does not), zero, one and two
//! replication destinations and one and two metrics puts — so no row is "the" case.

use serde_json::{json, Value};

use crate::harness::{expected_bucket_arn, timestamp_seconds, Emulator};
use crate::wire_forms::{
    assert_canonical_uuid, assert_table_version_token, create_table_request, table_id_of,
};

/// One account's worth of resources, as a row of the table below.
#[derive(Debug, Clone, Copy)]
struct Fixture {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// The namespaces created in the bucket, in request order.
    namespaces: &'static [&'static str],
    table: &'static str,
    /// Whether the create describes the table's metadata, which is what gives the
    /// table a metadata file — and so a `metadataLocation` carrying a minted id.
    metadata: bool,
    /// The table buckets the replication configuration replicates to. Empty means
    /// the row configures no replication at all.
    destinations: &'static [&'static str],
    /// How many times the row turns metrics on, so a repeated put is covered.
    metrics_puts: usize,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "one namespace, no metadata, no replication",
        region: "us-east-1",
        bucket: "mvf1",
        namespaces: &["n"],
        table: "t",
        metadata: false,
        destinations: &[],
        metrics_puts: 1,
    },
    Fixture {
        label: "two namespaces, metadata, one destination",
        region: "us-west-2",
        bucket: "minted-value-forms-pair",
        namespaces: &["ns_one", "ns_two"],
        table: "tbl_0",
        metadata: true,
        destinations: &["minted-value-forms-pair-dst"],
        metrics_puts: 2,
    },
    Fixture {
        label: "three namespaces, metadata, two destinations, long name",
        region: "eu-west-1",
        bucket: "minted-value-forms-on-a-rather-long-table-bucket-name-here",
        namespaces: &["a_2026_q1", "b_2026_q2", "c"],
        table: "a_very_long_table_name_0123456789",
        metadata: true,
        destinations: &[
            "minted-value-forms-dst-one",
            "minted-value-forms-dst-two",
        ],
        metrics_puts: 1,
    },
];

/// A `TableBucketReplicationConfiguration` replicating to `destinations`.
fn replication_configuration(destinations: &[String]) -> Value {
    json!({
        "role": "arn:aws:iam::000000000000:role/minted-value-forms",
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<Value>>(),
        }],
    })
}

/// The table id a `warehouseLocation` names the table's files by: the location is
/// `s3://<table id>--table-s3`, so the id is what stands before the suffix.
fn warehouse_table_id(warehouse_location: &str, case: &str) -> String {
    let rest = warehouse_location
        .strip_prefix("s3://")
        .unwrap_or_else(|| panic!("case {case:?}: {warehouse_location:?} is not an S3 URI"));
    let (table_id, _suffix) = rest
        .split_once("--")
        .unwrap_or_else(|| panic!("case {case:?}: {warehouse_location:?} names no table id"));
    table_id.to_string()
}

/// The id of the metadata FILE a `metadataLocation` points at: the file is named
/// `<five-digit version>-<id>.metadata.json` under the table's warehouse, so the id is
/// what stands between the version prefix and the suffix.
fn metadata_file_id(metadata_location: &str, case: &str) -> String {
    let file = metadata_location
        .rsplit('/')
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: {metadata_location:?} names no file"));
    let stem = file.strip_suffix(".metadata.json").unwrap_or_else(|| {
        panic!("case {case:?}: {metadata_location:?} is not an Iceberg metadata file")
    });
    let (version, id) = stem.split_once('-').unwrap_or_else(|| {
        panic!("case {case:?}: {file:?} carries no <version>-<id> file name")
    });
    assert!(
        version.len() == 5 && version.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected the metadata file name to open with a five-digit version, \
         got {version:?}"
    );
    id.to_string()
}

/// RULE 1: a `ListNamespaces` row reports its namespace's `createdAt` as an RFC 3339
/// date-time string, in the same text the read of that namespace reports, and its
/// `namespaceId` is the canonical UUID that read reports (RULE 2).
#[tokio::test]
async fn a_namespace_listing_row_reports_the_instant_and_the_id_of_its_namespace() {
    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");
        for namespace in fixture.namespaces {
            let created = emulator
                .create_namespace(fixture.region, &bucket_arn, &json!({ "namespace": [namespace] }))
                .await;
            assert_eq!(created.status, 200, "case {case:?}: CreateNamespace: {created:?}");
        }

        let listed = emulator
            .list_namespaces(fixture.region, &bucket_arn, &[])
            .await;
        assert_eq!(listed.status, 200, "case {case:?}: ListNamespaces: {listed:?}");
        let rows = listed.list("namespaces");
        assert_eq!(
            rows.len(),
            fixture.namespaces.len(),
            "case {case:?}: expected {} listing rows, got {}: {rows:?}",
            fixture.namespaces.len(),
            rows.len()
        );

        for namespace in fixture.namespaces {
            let read = emulator
                .get_namespace(fixture.region, &bucket_arn, namespace)
                .await;
            assert_eq!(read.status, 200, "case {case:?}: GetNamespace: {read:?}");
            // Reading the member through the ONE reader of a timestamp member is what
            // refuses every wire form but the modelled date-time string.
            timestamp_seconds(&read.body, "createdAt", case);
            let read_created = read.text("createdAt");
            let namespace_id = read.text("namespaceId");
            assert_canonical_uuid(&namespace_id, "GetNamespace's namespaceId", case);

            let row = rows
                .iter()
                .find(|row| {
                    row.get("namespace")
                        .and_then(Value::as_array)
                        .and_then(|parts| parts.first())
                        .and_then(Value::as_str)
                        == Some(*namespace)
                })
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the listing carries no row for {namespace:?}: {rows:?}")
                });
            timestamp_seconds(row, "createdAt", case);
            assert_eq!(
                row.get("createdAt").and_then(Value::as_str),
                Some(read_created.as_str()),
                "case {case:?}: namespace {namespace:?}: the listing row reports a different \
                 createdAt from the read: {row:?}"
            );
            let row_id = row.get("namespaceId").and_then(Value::as_str);
            assert_canonical_uuid(
                row_id.unwrap_or_else(|| {
                    panic!("case {case:?}: the listing row carries no namespaceId: {row:?}")
                }),
                "a ListNamespaces row's namespaceId",
                case,
            );
            assert_eq!(
                row_id,
                Some(namespace_id.as_str()),
                "case {case:?}: namespace {namespace:?}: the listing row reports a different \
                 namespaceId from the read: {row:?}"
            );
        }
    }
}

/// RULE 2: the ids inside a table's locations — the table id its `warehouseLocation` is
/// named for and the metadata-file id its `metadataLocation` points at — are canonical
/// UUIDs, and both reads of the table report the same locations.
#[tokio::test]
async fn a_table_s_locations_carry_canonical_uuid_ids() {
    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");
        let namespace = fixture.namespaces[0];
        let created_namespace = emulator
            .create_namespace(fixture.region, &bucket_arn, &json!({ "namespace": [namespace] }))
            .await;
        assert_eq!(
            created_namespace.status, 200,
            "case {case:?}: CreateNamespace: {created_namespace:?}"
        );
        let created = emulator
            .create_table(
                fixture.region,
                &bucket_arn,
                namespace,
                &create_table_request(fixture.table, fixture.metadata),
            )
            .await;
        assert_eq!(created.status, 200, "case {case:?}: CreateTable: {created:?}");
        let table_id = table_id_of(&created.text("tableARN"), case);

        let read = emulator
            .get_table(fixture.region, &bucket_arn, namespace, fixture.table)
            .await;
        assert_eq!(read.status, 200, "case {case:?}: GetTable: {read:?}");
        let warehouse = read.text("warehouseLocation");
        let warehouse_id = warehouse_table_id(&warehouse, case);
        assert_canonical_uuid(&warehouse_id, "the table id in GetTable's warehouseLocation", case);
        assert_eq!(
            warehouse_id, table_id,
            "case {case:?}: the warehouse location names {warehouse_id:?} where the table's \
             ARN carries {table_id:?}"
        );

        let location = emulator
            .get_table_metadata_location(fixture.region, &bucket_arn, namespace, fixture.table)
            .await;
        assert_eq!(
            location.status, 200,
            "case {case:?}: GetTableMetadataLocation: {location:?}"
        );
        assert_eq!(
            location.text("warehouseLocation"),
            warehouse,
            "case {case:?}: the two reads report different warehouse locations"
        );
        assert_table_version_token(
            &location.text("versionToken"),
            "GetTableMetadataLocation's versionToken",
            case,
        );

        // A table carries a metadata file only once one exists for it: the create writes
        // the first one when the request describes the table's metadata, and only then is
        // there a metadata-file id to report.
        match fixture.metadata {
            false => {
                for (what, body) in [("GetTable", &read.body), ("the location read", &location.body)]
                {
                    assert!(
                        body.get("metadataLocation").is_none(),
                        "case {case:?}: a table created without metadata has no metadata file, \
                         but {what} reports one: {body}"
                    );
                }
            }
            true => {
                let metadata_location = read.text("metadataLocation");
                assert!(
                    metadata_location.starts_with(&format!("{warehouse}/metadata/")),
                    "case {case:?}: expected the metadata file under the table's warehouse \
                     {warehouse:?}, got {metadata_location:?}"
                );
                assert_canonical_uuid(
                    &metadata_file_id(&metadata_location, case),
                    "the metadata-file id in GetTable's metadataLocation",
                    case,
                );
                assert_eq!(
                    location.text("metadataLocation"),
                    metadata_location,
                    "case {case:?}: the two reads report different metadata locations"
                );
            }
        }
    }
}

/// RULE 2: the `versionToken` a table bucket's replication configuration carries is a
/// canonical UUID, the read reports the one the put minted, and a conditional re-put
/// advances the token to another canonical UUID. The metrics puts each row performs are
/// driven too, so the id they settle is observed to be stable across them — its VALUE is
/// the fixed whole-bucket id the metrics suites pin, not a minted one.
#[tokio::test]
async fn a_configuration_read_reports_canonical_uuid_identifiers() {
    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");

        let mut first_id: Option<String> = None;
        for put in 0..fixture.metrics_puts {
            let turned_on = emulator
                .put_table_bucket_metrics_configuration(fixture.region, &bucket_arn)
                .await;
            assert_eq!(
                turned_on.status, 204,
                "case {case:?}: put {put} of the metrics configuration: {turned_on:?}"
            );
            // Read between the puts, so a REPEATED put is observed not to re-mint the id
            // the first one settled.
            let after_put = emulator
                .get_table_bucket_metrics_configuration(fixture.region, &bucket_arn)
                .await;
            assert_eq!(
                after_put.status, 200,
                "case {case:?}: the read after put {put}: {after_put:?}"
            );
            let id = after_put.text("id");
            match &first_id {
                None => first_id = Some(id),
                Some(settled) => assert_eq!(
                    &id, settled,
                    "case {case:?}: put {put} re-minted the metrics configuration id"
                ),
            }
        }
        let metrics = emulator
            .get_table_bucket_metrics_configuration(fixture.region, &bucket_arn)
            .await;
        assert_eq!(
            metrics.status, 200,
            "case {case:?}: GetTableBucketMetricsConfiguration: {metrics:?}"
        );
        let metrics_id = metrics.text("id");
        let again = emulator
            .get_table_bucket_metrics_configuration(fixture.region, &bucket_arn)
            .await;
        assert_eq!(
            again.text("id"),
            metrics_id,
            "case {case:?}: two reads report different metrics configuration ids"
        );

        // A row with no destination configures no replication, so there is no
        // replication version token for it to report.
        if fixture.destinations.is_empty() {
            continue;
        }
        let destinations: Vec<String> = fixture
            .destinations
            .iter()
            .map(|name| expected_bucket_arn(fixture.region, name))
            .collect();
        for name in fixture.destinations {
            let created = emulator
                .create_table_bucket(fixture.region, &json!({ "name": name }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: creating the destination {name:?}: {created:?}"
            );
        }
        let configuration = replication_configuration(&destinations);

        let put = emulator
            .put_table_bucket_replication(
                fixture.region,
                &[("tableBucketARN", bucket_arn.clone())],
                &configuration,
            )
            .await;
        assert_eq!(put.status, 200, "case {case:?}: PutTableBucketReplication: {put:?}");
        let token = put.text("versionToken");
        assert_table_version_token(&token, "a replication configuration's versionToken", case);
        let read = emulator
            .get_table_bucket_replication(fixture.region, &bucket_arn)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTableBucketReplication: {read:?}"
        );
        assert_eq!(
            read.text("versionToken"),
            token,
            "case {case:?}: the read reports a different versionToken from the put"
        );

        // A re-put presenting the configuration's current token succeeds and advances the
        // token, which is minted in the same form as the first.
        let advanced = emulator
            .put_table_bucket_replication(
                fixture.region,
                &[
                    ("tableBucketARN", bucket_arn.clone()),
                    ("versionToken", token.clone()),
                ],
                &configuration,
            )
            .await;
        assert_eq!(
            advanced.status, 200,
            "case {case:?}: the conditional PutTableBucketReplication: {advanced:?}"
        );
        let advanced_token = advanced.text("versionToken");
        assert_table_version_token(
            &advanced_token,
            "the versionToken a conditional replication put advances to",
            case,
        );
        assert_ne!(
            advanced_token, token,
            "case {case:?}: the re-put left the version token at {token:?}"
        );
    }
}
