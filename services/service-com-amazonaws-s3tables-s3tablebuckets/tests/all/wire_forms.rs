// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of the two server-derived value classes every S3 Tables read reports:
//! an instant and an identifier.
//!
//! RULE 1 — a timestamp member whose model declares `@timestampFormat("date-time")` goes
//! on the JSON wire as an RFC 3339 / ISO 8601 date-time STRING, never as the epoch-seconds
//! number the `restJson1` protocol defaults to. Every timestamp member of this model
//! declares it (`smithy_query(query="shape", shape="TableSummary,TableBucketSummary,
//! NamespaceSummary,GetTableResponse,GetTableBucketResponse,GetNamespaceResponse")` reports
//! `"timestampFormat": "date-time"` on each), so the rule covers every instant this service
//! reports.
//!
//! RULE 2 — every IDENTIFIER this service MINTS is in the canonical hyphenated UUID form
//! (8-4-4-4-12, lowercase): a `tableBucketId`, a `namespaceId` and the id segment after
//! `/table/` in a `tableARN`. The model constrains none of them beyond the `TableARN`
//! pattern (which admits the hyphens), so the form is recorded behaviour: the ids AWS mints
//! parse back to their own canonical spelling, and a client correlating a listing row with
//! an id it obtained elsewhere compares the two as strings.
//!
//! RULE 3 — a table's `versionToken` is NOT one of those identifiers. Nothing addresses a
//! table by it: it is the opaque optimistic-concurrency handle a write hands back, a caller
//! stores and hands straight back as a precondition, and the service mints it in its own
//! TOKEN form — one unbroken run of exactly [`VERSION_TOKEN_CHARACTERS`] lowercase
//! hexadecimal characters, which the model
//! (`smithy_query(query="shape", shape="VersionToken")`: a string with `@length(1, 2048)`
//! and no pattern) leaves to recorded behaviour. Every operation that mints one emits that
//! form, every read echoes the minted value unchanged, and a precondition compares the
//! token a caller holds against it.
//!
//! All three rules are asserted over a TABLE of fixtures — more than one region, namespace
//! and table name form and length, one, two and three tables per namespace, with and
//! without table metadata — and across every operation of this service that reports one of
//! these values, so a row the issue cited is one case among many rather than the case.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::OpenTableFormat;
use uuid::Uuid;

use crate::harness::{now_seconds, timestamp_seconds, Emulator};

/// One account's worth of S3 Tables resources, as a row of the table below.
#[derive(Debug, Clone, Copy)]
struct Fixture {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    namespace: &'static str,
    /// The tables created in that namespace, in request order.
    tables: &'static [&'static str],
    /// Whether the create describes the table's metadata, which is what gives a
    /// table a metadata file — a second place a minted id is laid out.
    metadata: bool,
}

/// The fixtures: they span the region an ARN is scoped to, namespace and table name
/// length (the modelled minimum of one character through a long name) and form (digits,
/// interior underscores), how many tables one namespace holds (one, two — the shape the
/// reported divergence had — and three) and whether the create carries metadata.
const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "one table, shortest names",
        region: "us-east-1",
        bucket: "wire-forms-single",
        namespace: "n",
        tables: &["t"],
        metadata: false,
    },
    Fixture {
        label: "two tables in one namespace",
        region: "us-west-2",
        bucket: "wire-forms-pair",
        namespace: "analytics_ns",
        tables: &["tbl_one", "tbl_two"],
        metadata: true,
    },
    Fixture {
        label: "three tables, digit and underscore names",
        region: "eu-west-1",
        bucket: "wire-forms-trio",
        namespace: "ns_2026_q1",
        tables: &["t0", "sales_2026", "a_very_long_table_name_0123456789"],
        metadata: false,
    },
];

/// `CreateTable`'s request body for `table`.
pub(crate) fn create_table_request(table: &str, metadata: bool) -> Value {
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

/// The TEXT a timestamp member carries, so two reads of one record can be compared on the
/// wire form itself and not only on the instant it decodes to.
///
/// The form is checked by the harness's ONE reading of a timestamp member
/// ([`timestamp_seconds`]), which accepts the modelled date-time string and refuses every
/// other wire form, so what counts as one is stated in a single place for every suite.
fn date_time_text(body: &Value, member: &str, case: &str) -> String {
    timestamp_seconds(body, member, case);
    body.get(member)
        .and_then(Value::as_str)
        .expect("the member was just read as a string")
        .to_string()
}

/// Assert `value` is an identifier in the canonical hyphenated UUID form: it parses as a
/// UUID AND is spelled the way that UUID renders itself, which a 32-character unhyphenated
/// digest parses as but is not spelled as.
pub(crate) fn assert_canonical_uuid(value: &str, what: &str, case: &str) {
    let canonical = Uuid::parse_str(value)
        .ok()
        .map(|uuid| uuid.hyphenated().to_string());
    assert!(
        canonical.as_deref() == Some(value),
        "case {case:?}: expected {what} to be a canonical hyphenated UUID, got {value:?}"
    );
}

/// What a table's `versionToken` is made of: the service's token form is a single run of
/// lowercase hexadecimal characters of exactly this width. Spelled out here rather than
/// read from the crate under test, so this suite pins the form itself.
pub(crate) const VERSION_TOKEN_CHARACTERS: usize = 20;

/// Assert `value` is a token in the form this service mints a `versionToken` in — a table's
/// own and a replication configuration's alike: exactly [`VERSION_TOKEN_CHARACTERS`]
/// lowercase hexadecimal characters, unbroken — no hyphen or other separator, no uppercase
/// character, and the same width whatever the token's value — and so neither a hyphenated
/// UUID nor a decimal number nor any other value carrying structure a client could read
/// something out of.
pub(crate) fn assert_table_version_token(value: &str, what: &str, case: &str) {
    let characters = value.chars().count();
    let lowercase_hex = !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase());
    assert!(
        lowercase_hex && characters == VERSION_TOKEN_CHARACTERS,
        "case {case:?}: expected {what} to be {VERSION_TOKEN_CHARACTERS} lowercase \
         hexadecimal characters, got {value:?} ({characters} characters)"
    );
}

/// The id segment of a table ARN: everything after `/table/`.
pub(crate) fn table_id_of(table_arn: &str, case: &str) -> String {
    table_arn
        .split_once("/table/")
        .unwrap_or_else(|| panic!("case {case:?}: {table_arn:?} carries no /table/ segment"))
        .1
        .to_string()
}

/// One table's row in a `ListTables` page.
pub(crate) fn summary_of<'a>(rows: &'a [Value], name: &str, case: &str) -> &'a Value {
    rows.iter()
        .find(|row| row.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: the listing carries no row for {name:?}"))
}

/// RULE 1: every instant this service reports — on a bucket, a namespace, a table, in a
/// describe and in a listing row — is an RFC 3339 date-time string, it decodes to the
/// instant the record was written at, and the two reads of one record report the same text.
#[tokio::test]
async fn every_reported_instant_is_an_rfc3339_date_time_string() {
    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();
        let before = now_seconds();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");
        let namespace = emulator
            .create_namespace(
                fixture.region,
                &bucket_arn,
                &json!({ "namespace": [fixture.namespace] }),
            )
            .await;
        assert_eq!(namespace.status, 200, "case {case:?}: CreateNamespace: {namespace:?}");
        for table in fixture.tables {
            let created = emulator
                .create_table(
                    fixture.region,
                    &bucket_arn,
                    fixture.namespace,
                    &create_table_request(table, fixture.metadata),
                )
                .await;
            assert_eq!(created.status, 200, "case {case:?}: CreateTable: {created:?}");
        }

        // The bucket's own instant, read back and in its listing row.
        let read_bucket = emulator.get_table_bucket(fixture.region, &bucket_arn).await;
        assert_eq!(read_bucket.status, 200, "case {case:?}: GetTableBucket: {read_bucket:?}");
        let bucket_created = date_time_text(&read_bucket.body, "createdAt", case);
        let listed_buckets = emulator.list_table_buckets(fixture.region, &[]).await;
        assert_eq!(
            listed_buckets.status, 200,
            "case {case:?}: ListTableBuckets: {listed_buckets:?}"
        );
        let bucket_rows = listed_buckets.list("tableBuckets");
        let bucket_row = summary_of(&bucket_rows, fixture.bucket, case);
        assert_eq!(
            date_time_text(bucket_row, "createdAt", case),
            bucket_created,
            "case {case:?}: the listing row and the describe report one bucket's createdAt \
             differently"
        );

        // The namespace's instant.
        let read_namespace = emulator
            .get_namespace(fixture.region, &bucket_arn, fixture.namespace)
            .await;
        assert_eq!(
            read_namespace.status, 200,
            "case {case:?}: GetNamespace: {read_namespace:?}"
        );
        let namespace_created = timestamp_seconds(&read_namespace.body, "createdAt", case);

        // Every table's two instants, in the describe and in the listing row.
        let listed_tables = emulator
            .list_tables(fixture.region, &bucket_arn, &[])
            .await;
        assert_eq!(listed_tables.status, 200, "case {case:?}: ListTables: {listed_tables:?}");
        let rows = listed_tables.list("tables");
        assert_eq!(
            rows.len(),
            fixture.tables.len(),
            "case {case:?}: expected {} listing rows, got {}",
            fixture.tables.len(),
            rows.len()
        );
        for table in fixture.tables {
            let read_table = emulator
                .get_table(fixture.region, &bucket_arn, fixture.namespace, table)
                .await;
            assert_eq!(read_table.status, 200, "case {case:?}: GetTable: {read_table:?}");
            let row = summary_of(&rows, table, case);
            for member in ["createdAt", "modifiedAt"] {
                let described = date_time_text(&read_table.body, member, case);
                assert_eq!(
                    date_time_text(row, member, case),
                    described,
                    "case {case:?}: table {table:?}: the listing row and the describe report \
                     {member} differently"
                );
            }
        }

        // Every instant is the one the record was written at: between the call that wrote
        // it and now, which is what tells a reported instant from an invented one.
        let after = now_seconds();
        for (what, seconds) in [
            ("the bucket's createdAt", timestamp_seconds(&read_bucket.body, "createdAt", case)),
            ("the namespace's createdAt", namespace_created),
            (
                "a table's createdAt",
                timestamp_seconds(summary_of(&rows, fixture.tables[0], case), "createdAt", case),
            ),
        ] {
            assert!(
                seconds >= before - 1.0 && seconds <= after + 1.0,
                "case {case:?}: {what} {seconds} is outside the window \
                 [{before}, {after}] the record was written in"
            );
        }
    }
}

/// RULE 2 (and RULE 3 for the token each create hands back): every identifier this service
/// mints is a canonical hyphenated UUID, it is the SAME id whichever operation reports it,
/// and no two resources share one.
#[tokio::test]
async fn every_minted_identifier_is_a_canonical_uuid() {
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");
        let namespace = emulator
            .create_namespace(
                fixture.region,
                &bucket_arn,
                &json!({ "namespace": [fixture.namespace] }),
            )
            .await;
        assert_eq!(namespace.status, 200, "case {case:?}: CreateNamespace: {namespace:?}");

        // `CreateTable` reports two minted values: the table's ARN, whose last segment is
        // the table id, and the table's version token.
        for table in fixture.tables {
            let created = emulator
                .create_table(
                    fixture.region,
                    &bucket_arn,
                    fixture.namespace,
                    &create_table_request(table, fixture.metadata),
                )
                .await;
            assert_eq!(created.status, 200, "case {case:?}: CreateTable: {created:?}");
            let table_arn = created.text("tableARN");
            let table_id = table_id_of(&table_arn, case);
            assert_canonical_uuid(&table_id, "the id segment of CreateTable's tableARN", case);
            assert_table_version_token(
                &created.text("versionToken"),
                "CreateTable's versionToken",
                case,
            );
            assert!(
                minted.insert(table_id.clone()),
                "case {case:?}: table id {table_id:?} was minted twice"
            );
        }

        // The bucket id, in the describe and in the listing row.
        let read_bucket = emulator.get_table_bucket(fixture.region, &bucket_arn).await;
        assert_eq!(read_bucket.status, 200, "case {case:?}: GetTableBucket: {read_bucket:?}");
        let bucket_id = read_bucket.text("tableBucketId");
        assert_canonical_uuid(&bucket_id, "GetTableBucket's tableBucketId", case);
        assert!(
            minted.insert(bucket_id.clone()),
            "case {case:?}: table bucket id {bucket_id:?} was minted twice"
        );
        let listed_buckets = emulator.list_table_buckets(fixture.region, &[]).await;
        let bucket_rows = listed_buckets.list("tableBuckets");
        let bucket_row = summary_of(&bucket_rows, fixture.bucket, case);
        assert_eq!(
            bucket_row.get("tableBucketId").and_then(Value::as_str),
            Some(bucket_id.as_str()),
            "case {case:?}: the listing row reports a different tableBucketId from the \
             describe: {bucket_row:?}"
        );

        // The namespace id, and the bucket id the namespace reports.
        let read_namespace = emulator
            .get_namespace(fixture.region, &bucket_arn, fixture.namespace)
            .await;
        assert_eq!(
            read_namespace.status, 200,
            "case {case:?}: GetNamespace: {read_namespace:?}"
        );
        let namespace_id = read_namespace.text("namespaceId");
        assert_canonical_uuid(&namespace_id, "GetNamespace's namespaceId", case);
        assert!(
            minted.insert(namespace_id.clone()),
            "case {case:?}: namespace id {namespace_id:?} was minted twice"
        );
        assert_eq!(
            read_namespace.text("tableBucketId"),
            bucket_id,
            "case {case:?}: GetNamespace reports a different tableBucketId from \
             GetTableBucket"
        );

        // Every table's ids, in the describe and in the listing row.
        let listed_tables = emulator
            .list_tables(fixture.region, &bucket_arn, &[])
            .await;
        assert_eq!(listed_tables.status, 200, "case {case:?}: ListTables: {listed_tables:?}");
        let rows = listed_tables.list("tables");
        for table in fixture.tables {
            let read_table = emulator
                .get_table(fixture.region, &bucket_arn, fixture.namespace, table)
                .await;
            assert_eq!(read_table.status, 200, "case {case:?}: GetTable: {read_table:?}");
            let table_arn = read_table.text("tableARN");
            assert_canonical_uuid(
                &table_id_of(&table_arn, case),
                "the id segment of GetTable's tableARN",
                case,
            );
            assert_table_version_token(
                &read_table.text("versionToken"),
                "GetTable's versionToken",
                case,
            );
            assert_eq!(
                read_table.text("namespaceId"),
                namespace_id,
                "case {case:?}: table {table:?} reports a different namespaceId from \
                 GetNamespace"
            );
            assert_eq!(
                read_table.text("tableBucketId"),
                bucket_id,
                "case {case:?}: table {table:?} reports a different tableBucketId from \
                 GetTableBucket"
            );

            let row = summary_of(&rows, table, case);
            let row_arn = row
                .get("tableARN")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: listing row {row:?} has no tableARN"));
            assert_canonical_uuid(
                &table_id_of(row_arn, case),
                "the id segment of a ListTables row's tableARN",
                case,
            );
            assert_eq!(
                row_arn, table_arn,
                "case {case:?}: table {table:?}: the listing row's tableARN differs from the \
                 describe's"
            );
            // A `ListTables` row reports the table's OWN identity, so neither container
            // id travels in it — the member set of a summary row is pinned by
            // `list_tables_summary_members.rs`; what this suite pins here is that the
            // ids this service mints are reported by the reads of the resources they
            // identify.
            for container_id in ["namespaceId", "tableBucketId"] {
                assert!(
                    row.get(container_id).is_none(),
                    "case {case:?}: table {table:?}: the listing row carries \
                     {container_id}, which only the reads of the namespace and the \
                     table bucket report: {row:?}"
                );
            }
        }
    }
}

/// RULE 3: a table's `versionToken` is an opaque token in the service's token form, never
/// an identifier — the create mints one, both reads echo exactly that value, no two tables
/// share one, it is nothing a value in the table's own identity is spelled as, and the
/// write that moves the table's metadata pointer both accepts it as a precondition and
/// answers with it, after which it is still the token the delete's precondition takes.
#[tokio::test]
async fn a_table_version_token_is_an_opaque_token_that_guards_the_table() {
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();

        let bucket = emulator
            .create_table_bucket(fixture.region, &json!({ "name": fixture.bucket }))
            .await;
        assert_eq!(bucket.status, 200, "case {case:?}: CreateTableBucket: {bucket:?}");
        let bucket_arn = bucket.text("arn");
        let namespace = emulator
            .create_namespace(
                fixture.region,
                &bucket_arn,
                &json!({ "namespace": [fixture.namespace] }),
            )
            .await;
        assert_eq!(namespace.status, 200, "case {case:?}: CreateNamespace: {namespace:?}");

        for table in fixture.tables {
            let created = emulator
                .create_table(
                    fixture.region,
                    &bucket_arn,
                    fixture.namespace,
                    &create_table_request(table, fixture.metadata),
                )
                .await;
            assert_eq!(created.status, 200, "case {case:?}: CreateTable: {created:?}");
            let token = created.text("versionToken");
            assert_table_version_token(&token, "CreateTable's versionToken", case);
            assert!(
                minted.insert(token.clone()),
                "case {case:?}: table {table:?}: the token {token:?} was already minted for \
                 another table"
            );

            // Both reads of the table echo the MINTED value, unchanged — that is what makes
            // a token a client stored usable later.
            let read = emulator
                .get_table(fixture.region, &bucket_arn, fixture.namespace, table)
                .await;
            assert_eq!(read.status, 200, "case {case:?}: GetTable: {read:?}");
            assert_eq!(
                read.text("versionToken"),
                token,
                "case {case:?}: table {table:?}: GetTable reports a token the create never \
                 minted: {read:?}"
            );
            let location = emulator
                .get_table_metadata_location(fixture.region, &bucket_arn, fixture.namespace, table)
                .await;
            assert_eq!(
                location.status, 200,
                "case {case:?}: GetTableMetadataLocation: {location:?}"
            );
            assert_eq!(
                location.text("versionToken"),
                token,
                "case {case:?}: table {table:?}: the metadata-location read reports a \
                 different token: {location:?}"
            );

            // The token identifies nothing: it is not the table's id, nor any other value
            // the table's identity is spelled with — those are the hyphenated UUIDs and the
            // names RULE 2 covers, which the form assertion above already rules this value
            // out of.
            let table_id = table_id_of(&read.text("tableARN"), case);
            let warehouse = read.text("warehouseLocation");
            for (what, identity) in [
                ("the table id", table_id.as_str()),
                ("the table's name", *table),
                ("the warehouse location", warehouse.as_str()),
            ] {
                assert!(
                    identity != token,
                    "case {case:?}: table {table:?}: {what} ({identity:?}) IS the version \
                     token {token:?}, so the token is an identifier rather than a handle"
                );
            }
        }

        // The precondition: the write that moves the table's metadata file accepts the
        // token the reads reported and answers with that same token, which a second write
        // under it is still honoured by.
        let table = fixture.tables[0];
        let before = emulator
            .get_table(fixture.region, &bucket_arn, fixture.namespace, table)
            .await;
        assert_eq!(before.status, 200, "case {case:?}: GetTable: {before:?}");
        let held = before.text("versionToken");
        let metadata_location = format!(
            "{warehouse}/metadata/00001-{table}.metadata.json",
            warehouse = before.text("warehouseLocation")
        );
        let updated = emulator
            .update_table_metadata_location(
                fixture.region,
                &bucket_arn,
                fixture.namespace,
                table,
                &json!({ "metadataLocation": metadata_location, "versionToken": held }),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: the write under the token the read reported: {updated:?}"
        );
        let settled = updated.text("versionToken");
        assert_table_version_token(&settled, "the versionToken a write answers with", case);
        assert_eq!(
            settled, held,
            "case {case:?}: the write answered with {settled:?} instead of the token \
             {held:?} the read reported, so a caller's view is spent by a write of its own"
        );
        let replayed = emulator
            .update_table_metadata_location(
                fixture.region,
                &bucket_arn,
                fixture.namespace,
                table,
                &json!({ "metadataLocation": metadata_location, "versionToken": held }),
            )
            .await;
        assert_eq!(
            replayed.status, 200,
            "case {case:?}: the token {held:?} must still be the table's: {replayed:?}"
        );

        // And it is still the one the delete's precondition takes.
        let deleted = emulator
            .delete_table(
                fixture.region,
                &bucket_arn,
                fixture.namespace,
                table,
                &[("versionToken", held.clone())],
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: the delete under the current token: {deleted:?}"
        );
    }
}
