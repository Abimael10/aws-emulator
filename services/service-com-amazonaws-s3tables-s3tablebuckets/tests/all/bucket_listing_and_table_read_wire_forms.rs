// SPDX-License-Identifier: CC0-1.0

//! The WIRE FORM of the server-derived values a table-bucket LISTING row and a table READ
//! report, over the dimensions that decide WHICH call produced the row: the view the
//! listing is asked for, and the form the table is addressed by.
//!
//! RULE 1 — a timestamp member whose model declares `@timestampFormat("date-time")` goes on
//! the JSON wire as an RFC 3339 date-time STRING, never as the epoch-seconds number the
//! `restJson1` protocol defaults to. `TableBucketSummary.createdAt` and
//! `GetTableResponse.createdAt`/`.modifiedAt` each declare it
//! (`smithy_query(query="shape", shape="TableBucketSummary,GetTableResponse")` reports
//! `"timestampFormat": "date-time"` on all three), and the value is the stored record's
//! instant, so the text a row carries is the text the read of the same record carries —
//! whichever view produced the row, and whichever form addressed the read.
//!
//! RULE 2 — every IDENTIFIER this service MINTS is a canonical hyphenated UUID (8-4-4-4-12,
//! lowercase): a `tableBucketId`, a `namespaceId` and the id segment after `/table/` in a
//! `tableARN`. The model constrains none of them beyond a length (and the `TableARN`
//! pattern, which admits the hyphens), so the form is recorded behaviour, asserted through
//! the one [`assert_canonical_uuid`] every suite of this crate shares; and one record has
//! ONE id, so every view and every addressing form reports the same value for it.
//!
//! RULE 3 — a `versionToken` is not such an identifier but the service's opaque token form
//! ([`assert_table_version_token`]), so a read answers with it unchanged however the table
//! was addressed.
//!
//! `wire_forms.rs` states the three rules over the create-and-read paths and
//! `list_tables_row_wire_forms.rs` over a `ListTables` row's provenance. What this suite
//! adds is the dimension neither walks:
//!
//! * a `ListTableBuckets` row as reported by each VIEW of the listing — the whole region,
//!   narrowed by a name prefix, narrowed by the bucket type, and walked one and two rows per
//!   page through `continuationToken` (the members `ListTableBucketsRequest` declares:
//!   `prefix`, `type`, `maxBuckets`, `continuationToken`);
//! * a `GetTable` response as reached by BOTH whole addressing forms — the table's own ARN
//!   alone, and the bucket ARN, namespace and name together.
//!
//! Both are asserted over a TABLE of fixtures spanning the region an ARN is scoped to, one,
//! three and four buckets in a region, the modelled extremes of `TableBucketName`'s length
//! (3 and 63 characters — `smithy_query(query="shape", shape="TableBucketName")` reports
//! `@length(3, 63)`) and both ends of `TableName`'s, and a table created with and without
//! metadata, so no single row is "the" case.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets::state::TableBucketType;

use crate::harness::{expected_bucket_arn, timestamp_seconds, Emulator};
use crate::wire_forms::{
    assert_canonical_uuid, assert_table_version_token, create_table_request, summary_of,
    table_id_of,
};

/// The modelled bounds on a table bucket's name, which the fixtures below span:
/// `TableBucketName` is `@length(3, 63)` with the charset `^[0-9a-z-]*$`.
const BUCKET_NAME_LENGTH: (usize, usize) = (3, 63);

/// One region's worth of resources, as a row of the table below.
#[derive(Debug, Clone, Copy)]
struct Fixture {
    label: &'static str,
    region: &'static str,
    /// The table buckets created in that region, in request order — which is NOT the
    /// stored order, so a listing that echoed the creates would be visible.
    buckets: &'static [&'static str],
    /// A name prefix the listing is narrowed by; it selects some of `buckets` and none of
    /// the rest.
    prefix: &'static str,
    /// The namespace created in the FIRST bucket, and the table created in it.
    namespace: &'static str,
    table: &'static str,
    /// Whether the table's create describes its metadata, which gives the table a metadata
    /// document before anything else writes one.
    metadata: bool,
}

/// The fixtures: three regions; one, four and three buckets per region; bucket names at the
/// modelled minimum (3 characters) and maximum (63) length; table and namespace names at
/// `TableName`'s one-character minimum through a long name; metadata present and absent.
const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "one bucket, shortest names",
        region: "us-east-1",
        buckets: &["ltb"],
        prefix: "l",
        namespace: "n",
        table: "t",
        metadata: false,
    },
    Fixture {
        label: "four buckets, two sharing a prefix",
        region: "us-west-2",
        buckets: &[
            "ltb-forms-pair-two",
            "zzz-ltb-forms-other",
            "ltb-forms-pair-one",
            "mid-ltb-forms",
        ],
        prefix: "ltb-forms-pair-",
        namespace: "analytics_ns",
        table: "tbl_one",
        metadata: true,
    },
    Fixture {
        label: "three buckets, one at the maximum name length",
        region: "eu-west-1",
        buckets: &[
            "ltb-forms-maximum-length-bucket-name-0123456789-abcdefghij-0123",
            "ltb-forms-trio-2",
            "ltb-forms-trio-1",
        ],
        prefix: "ltb-forms-trio-",
        namespace: "ns_2026_q1",
        table: "a_very_long_table_name_0123456789",
        metadata: false,
    },
];

/// One VIEW of the bucket listing: how it is asked for, and which buckets it must report.
#[derive(Debug)]
struct View {
    label: String,
    /// The query members the view narrows or pages the listing by.
    query: Vec<(&'static str, String)>,
    /// The names of the buckets the view reports, sorted.
    expected: Vec<String>,
}

/// Every row of the bucket listing `query` asks for, following `continuationToken` to the
/// end so a paged view reports the whole listing.
async fn listed_rows(
    emulator: &Emulator,
    region: &str,
    query: &[(&str, String)],
    case: &str,
    pages: usize,
) -> Vec<Value> {
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..pages {
        let mut asked: Vec<(&str, String)> = query.to_vec();
        if let Some(token) = &token {
            asked.push(("continuationToken", token.clone()));
        }
        let reply = emulator.list_table_buckets(region, &asked).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: ListTableBuckets (page {page}) must be served, got {status} {body}",
            status = reply.status,
            body = reply.body
        );
        collected.extend(reply.list("tableBuckets"));
        token = reply.optional_text("continuationToken");
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {case:?}: the listing never ran out of pages");
}

/// The `name` member of every row, sorted, so a view is compared on WHICH buckets it
/// reports without restating the order another suite pins.
fn sorted_names(rows: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = rows
        .iter()
        .map(|row| {
            row.get("name")
                .and_then(Value::as_str)
                .unwrap_or("<no name>")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

/// Create the fixture's buckets, and the namespace and table the first one holds. Answers
/// the first bucket's ARN and the table's ARN.
async fn set_up(emulator: &Emulator, fixture: &Fixture) -> (String, String) {
    let case = fixture.label;
    let mut first_arn: Option<String> = None;
    for name in fixture.buckets {
        assert!(
            (BUCKET_NAME_LENGTH.0..=BUCKET_NAME_LENGTH.1).contains(&name.chars().count()),
            "case {case:?}: the fixture name {name:?} is outside the modelled length bound \
             {BUCKET_NAME_LENGTH:?}"
        );
        let created = emulator
            .create_table_bucket(fixture.region, &json!({ "name": name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTableBucket {name:?}: {created:?}"
        );
        let arn = created.text("arn");
        assert_eq!(
            arn,
            expected_bucket_arn(fixture.region, name),
            "case {case:?}: CreateTableBucket {name:?} minted an unexpected ARN"
        );
        if first_arn.is_none() {
            first_arn = Some(arn);
        }
    }
    let bucket_arn = first_arn.expect("every fixture creates at least one bucket");

    let namespace = emulator
        .create_namespace(
            fixture.region,
            &bucket_arn,
            &json!({ "namespace": [fixture.namespace] }),
        )
        .await;
    assert_eq!(
        namespace.status, 200,
        "case {case:?}: CreateNamespace: {namespace:?}"
    );
    let table = emulator
        .create_table(
            fixture.region,
            &bucket_arn,
            fixture.namespace,
            &create_table_request(fixture.table, fixture.metadata),
        )
        .await;
    assert_eq!(table.status, 200, "case {case:?}: CreateTable: {table:?}");

    (bucket_arn, table.text("tableARN"))
}

/// RULE 1 and RULE 2 over every view of a table-bucket listing: each row's `createdAt` is
/// the date-time STRING the read of that bucket reports, and its `tableBucketId` is the
/// canonical UUID that read reports.
#[tokio::test]
async fn every_view_of_a_bucket_listing_reports_date_time_instants_and_the_minted_bucket_id() {
    for fixture in FIXTURES {
        let emulator = Emulator::new();
        let (_bucket_arn, _table_arn) = set_up(&emulator, fixture).await;

        let every_name = {
            let mut names: Vec<String> = fixture.buckets.iter().map(|n| n.to_string()).collect();
            names.sort();
            names
        };
        let prefixed: Vec<String> = every_name
            .iter()
            .filter(|name| name.starts_with(fixture.prefix))
            .cloned()
            .collect();
        assert!(
            (!prefixed.is_empty() && prefixed.len() < every_name.len())
                || every_name.len() == 1,
            "case {case:?}: the fixture's prefix {prefix:?} must narrow the listing, it \
             selects {prefixed:?} of {every_name:?}",
            case = fixture.label,
            prefix = fixture.prefix
        );

        // Every view `ListTableBucketsRequest` offers, each with the buckets it must report.
        let views = vec![
            View {
                label: "the whole region".to_string(),
                query: Vec::new(),
                expected: every_name.clone(),
            },
            View {
                label: format!("prefix {:?}", fixture.prefix),
                query: vec![("prefix", fixture.prefix.to_string())],
                expected: prefixed,
            },
            View {
                label: "the type a caller's buckets carry".to_string(),
                query: vec![("type", TableBucketType::Customer.as_wire().to_string())],
                expected: every_name.clone(),
            },
            View {
                label: "one row per page".to_string(),
                query: vec![("maxBuckets", "1".to_string())],
                expected: every_name.clone(),
            },
            View {
                label: "two rows per page".to_string(),
                query: vec![("maxBuckets", "2".to_string())],
                expected: every_name.clone(),
            },
        ];

        for View {
            label: view,
            query,
            expected,
        } in views
        {
            let case = format!("{label} / {view}", label = fixture.label);
            // One page per bucket is the worst case; the rest is headroom for the last,
            // empty page a cursor can end on.
            let rows = listed_rows(
                &emulator,
                fixture.region,
                &query,
                &case,
                fixture.buckets.len() + 3,
            )
            .await;
            assert_eq!(
                sorted_names(&rows),
                expected,
                "case {case:?}: expected the view to report {expected:?}, got {rows:?}"
            );

            for name in &expected {
                let row = summary_of(&rows, name, &case);
                let arn = expected_bucket_arn(fixture.region, name);
                let read = emulator.get_table_bucket(fixture.region, &arn).await;
                assert_eq!(read.status, 200, "case {case:?}: GetTableBucket: {read:?}");

                // RULE 1: the row's instant is a JSON string in the modelled date-time
                // format — `timestamp_seconds` accepts that form and no other — and it is
                // the text the read of the same bucket reports.
                let actual = row.get("createdAt");
                assert!(
                    actual.is_some_and(Value::is_string),
                    "case {case:?}: bucket {name:?}: expected createdAt to be an RFC 3339 \
                     date-time string, got {actual:?}"
                );
                timestamp_seconds(row, "createdAt", &case);
                let described = read.text("createdAt");
                assert_eq!(
                    actual.and_then(Value::as_str),
                    Some(described.as_str()),
                    "case {case:?}: bucket {name:?}: the row reports createdAt as {actual:?}, \
                     not the {described:?} the read of the same bucket reports"
                );

                // RULE 2: the row's bucket id is the canonical UUID the read reports.
                let described_id = read.text("tableBucketId");
                assert_canonical_uuid(&described_id, "GetTableBucket's tableBucketId", &case);
                let row_id = row.get("tableBucketId").and_then(Value::as_str);
                assert_canonical_uuid(
                    row_id.unwrap_or_else(|| {
                        panic!("case {case:?}: bucket {name:?}: the row carries no tableBucketId: {row:?}")
                    }),
                    "a ListTableBuckets row's tableBucketId",
                    &case,
                );
                assert_eq!(
                    row_id,
                    Some(described_id.as_str()),
                    "case {case:?}: bucket {name:?}: the row reports a different \
                     tableBucketId from the read: {row:?}"
                );
                assert_eq!(
                    row.get("arn").and_then(Value::as_str),
                    Some(arn.as_str()),
                    "case {case:?}: bucket {name:?}: the row reports an unexpected ARN: {row:?}"
                );
            }
        }
    }
}

/// RULES 1, 2 and 3 over both whole forms `GetTable` is addressed by: the table's own ARN
/// alone, and the bucket ARN, namespace and name together. The two reads resolve one stored
/// record, so each reports the same date-time text, the same minted ids and the same opaque
/// token — and the ids are the ones the reads of the containing namespace and bucket report.
#[tokio::test]
async fn both_addressing_forms_of_a_table_read_report_the_same_wire_forms() {
    for fixture in FIXTURES {
        let case = fixture.label;
        let emulator = Emulator::new();
        let (bucket_arn, table_arn) = set_up(&emulator, fixture).await;

        let bucket = emulator.get_table_bucket(fixture.region, &bucket_arn).await;
        assert_eq!(bucket.status, 200, "case {case:?}: GetTableBucket: {bucket:?}");
        let bucket_id = bucket.text("tableBucketId");
        let namespace = emulator
            .get_namespace(fixture.region, &bucket_arn, fixture.namespace)
            .await;
        assert_eq!(namespace.status, 200, "case {case:?}: GetNamespace: {namespace:?}");
        let namespace_id = namespace.text("namespaceId");

        let by_triple = emulator
            .get_table(fixture.region, &bucket_arn, fixture.namespace, fixture.table)
            .await;
        assert_eq!(by_triple.status, 200, "case {case:?}: GetTable by the triple: {by_triple:?}");
        let by_arn = emulator.get_table_by_arn(fixture.region, &table_arn).await;
        assert_eq!(by_arn.status, 200, "case {case:?}: GetTable by the ARN: {by_arn:?}");

        let mut instants: Vec<(String, String)> = Vec::new();
        for (form, read) in [("the triple", &by_triple), ("the table ARN", &by_arn)] {
            let case = format!("{case} / addressed by {form}");

            // RULE 1: both instants are JSON strings in the modelled date-time format, and
            // the record was last modified no earlier than it was created.
            for member in ["createdAt", "modifiedAt"] {
                let actual = read.body.get(member);
                assert!(
                    actual.is_some_and(Value::is_string),
                    "case {case:?}: expected {member} to be an RFC 3339 date-time string, got \
                     {actual:?}"
                );
            }
            let created_at = timestamp_seconds(&read.body, "createdAt", &case);
            let modified_at = timestamp_seconds(&read.body, "modifiedAt", &case);
            assert!(
                modified_at >= created_at,
                "case {case:?}: the table was last modified at {modified_at}, before it was \
                 created at {created_at}"
            );
            instants.push((read.text("createdAt"), read.text("modifiedAt")));

            // RULE 2: the three minted ids the read reports are canonical UUIDs, and the
            // two container ids are the ones the reads of those containers report.
            assert_canonical_uuid(
                &table_id_of(&read.text("tableARN"), &case),
                "the id segment of GetTable's tableARN",
                &case,
            );
            assert_eq!(
                read.text("tableARN"),
                table_arn,
                "case {case:?}: the read reports a different tableARN from the create"
            );
            for (what, actual, expected) in [
                ("namespaceId", read.text("namespaceId"), &namespace_id),
                ("tableBucketId", read.text("tableBucketId"), &bucket_id),
            ] {
                assert_canonical_uuid(&actual, &format!("GetTable's {what}"), &case);
                assert_eq!(
                    &actual, expected,
                    "case {case:?}: the read reports {what} {actual:?}, not the {expected:?} \
                     the read of that resource reports"
                );
            }

            // RULE 3: the version token is the service's opaque token, not an identifier.
            assert_table_version_token(
                &read.text("versionToken"),
                "GetTable's versionToken",
                &case,
            );
        }

        // One stored record: the two forms report it identically, down to the text of its
        // instants.
        assert_eq!(
            instants[0], instants[1],
            "case {case:?}: the two addressing forms report different instants"
        );
        assert_eq!(
            by_arn.body, by_triple.body,
            "case {case:?}: the two addressing forms report different records"
        );
    }
}
