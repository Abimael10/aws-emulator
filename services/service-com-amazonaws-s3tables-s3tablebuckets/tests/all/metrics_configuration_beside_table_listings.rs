// SPDX-License-Identifier: CC0-1.0

//! What a client sees when it walks a table bucket's METRICS CONFIGURATION and the bucket's
//! TABLE LISTING in ONE session, which is the shape the recorded S3 Tables traces take.
//!
//! RULE 3 below is what this suite is for, and it is the one no other suite states: the two
//! sub-surfaces of a table bucket are INDEPENDENT — a write to the bucket's metrics
//! configuration leaves every table record exactly as it was, and the configuration a read
//! reports does not vary with the namespaces and tables the bucket holds. It cannot be
//! stated without observing both surfaces at every phase of the configuration's lifecycle,
//! so the rules the other suites own are the observations it is stated over rather than
//! coverage of their own: `wire_forms.rs` and `list_tables_row_wire_forms.rs` own RULES 1
//! and 2 (and this file reuses their helpers), `table_bucket_metrics_configuration.rs` owns
//! RULE 4 and `metrics_configuration_not_found_message.rs` owns RULE 5.
//!
//! RULE 1 — a `ListTables` row reports its two instants as RFC 3339 date-time STRINGS,
//! never the epoch-seconds number `restJson1` defaults to:
//! `smithy_query(query="shape", shape="TableSummary")` declares
//! `"timestampFormat": "date-time"` on both `createdAt` and `modifiedAt`. The harness's one
//! reader of a timestamp member ([`timestamp_seconds`]) accepts that form and no other, and
//! the text a row carries is the text the read of the same table reports.
//!
//! RULE 2 — the id segment after `/table/` in a row's `tableARN` is the id this service
//! minted for that table: a canonical hyphenated UUID (8-4-4-4-12, lowercase), the same
//! value the create reported, for the life of the record. `TableARN`'s modelled pattern
//! constrains the charset only, so the form is recorded behaviour and is asserted through
//! the one [`assert_canonical_uuid`] every suite shares.
//!
//! RULE 3 — a write to the bucket's metrics configuration is not a write to the bucket's
//! table records: the listing a client reads before a put, between the put and the delete,
//! and after the delete is the same listing, row for row and member for member.
//!
//! RULE 4 — while a configuration is set, `GetTableBucketMetricsConfiguration` reports the
//! single whole-bucket configuration's FIXED id and the addressed bucket's own ARN. The
//! model types `id` as a plain `String`
//! (`smithy_query(query="operation", operation="GetTableBucketMetricsConfiguration")`), so
//! the value is recorded behaviour: it is bucket-independent, so it does not vary with the
//! bucket's name, region, tables or namespaces, nor with how often the `@idempotent` put was
//! repeated.
//!
//! RULE 5 — while no configuration is set — before any put, and once the delete has removed
//! the one a put set — the same read is refused with HTTP 404, the modelled
//! `NotFoundException` and the fixed sentence for that condition, which interpolates nothing
//! from the request or the store.
//!
//! The cases span what the rules generalise over: the region the resources are scoped to,
//! the published `TableBucketName` length bounds, one and two namespaces, one to three
//! tables per namespace, table names from the modelled one-character minimum to a long one,
//! whether the creates described the tables' metadata, and how many times the put is
//! repeated.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::harness::{timestamp_seconds, Emulator, Reply};
use crate::wire_forms::{assert_canonical_uuid, create_table_request, summary_of, table_id_of};

/// The id S3 Tables reports for the one metrics configuration a table bucket can have: the
/// configuration covers the WHOLE bucket, and this fixed text is how the service names it.
/// Spelled out here rather than read from the crate under test, so the suite pins the value.
const WHOLE_BUCKET_ID: &str = "EntireBucket";

/// The fixed sentence the read states a bucket that carries no metrics configuration with.
const NO_METRICS_CONFIGURATION: &str = "No such metric configuration exists";

/// One case: a table bucket, the namespaces it holds and the tables in each.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// Each namespace and the tables created in it, in request order.
    namespaces: &'static [(&'static str, &'static [&'static str])],
    /// Whether every create describes its table's metadata.
    metadata: bool,
    /// How many times `PutTableBucketMetricsConfiguration` is called, which exercises the
    /// operation's `@idempotent` trait.
    puts: usize,
}

impl Case {
    /// Every table of the case, with the namespace it sits in.
    fn tables(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.namespaces
            .iter()
            .flat_map(|(namespace, tables)| tables.iter().map(move |table| (*namespace, *table)))
    }
}

/// `TableBucketName` is `@length(3, 63)` and `@pattern(^[0-9a-z-]*$)`, and a namespace or
/// table name is `^[0-9a-z_]*$` with `@length(1, 255)`: the rows below take the shortest and
/// the longest bucket name those bounds publish, and names across the classes the patterns
/// allow.
const CASES: &[Case] = &[
    Case {
        label: "the shortest published bucket name, one table, one put",
        region: "us-east-1",
        bucket: "m3t",
        namespaces: &[("n", &["t"])],
        metadata: false,
        puts: 1,
    },
    Case {
        label: "two tables in one namespace, the put repeated",
        region: "us-west-2",
        bucket: "metrics-beside-listing-pair",
        namespaces: &[("analytics_ns", &["tbl_one", "tbl_two"])],
        metadata: true,
        puts: 2,
    },
    Case {
        label: "two namespaces, three tables, the put repeated three times",
        region: "eu-west-1",
        bucket: "metrics-beside-listing-trio",
        namespaces: &[
            ("ns_2026_q1", &["t0", "a_very_long_table_name_0123456789"]),
            ("reporting_ns", &["sales_2026"]),
        ],
        metadata: false,
        puts: 3,
    },
    Case {
        // 63 characters: `metrics-beside-listing-` (23) and 40 more.
        label: "the longest published bucket name",
        region: "ap-south-1",
        bucket: "metrics-beside-listing-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        namespaces: &[("ns_0", &["t_1", "t_2"])],
        metadata: true,
        puts: 1,
    },
    Case {
        label: "a bucket name of digits and hyphens",
        region: "us-east-1",
        bucket: "7-metrics-3",
        namespaces: &[("n0", &["t0"])],
        metadata: false,
        puts: 2,
    },
];

/// The bounds `TableBucketName` publishes (`@length(3, 63)`), which the cases above are
/// asserted to stay inside — the shortest and the longest among them sit on them.
const BUCKET_NAME_BOUNDS: (usize, usize) = (3, 63);

/// Assert `reply` carries the modelled 404 for a bucket whose metrics are off. The sentence
/// is compared whole, which is also what states that it took nothing from the request or the
/// store: any interpolated bucket name, ARN or id would make it differ.
fn assert_metrics_off(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected status 404, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected the modelled NotFoundException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        NO_METRICS_CONFIGURATION,
        "case {case:?}: expected the fixed sentence {NO_METRICS_CONFIGURATION:?}, got \
         {actual:?}",
        actual = reply.message()
    );
}

/// Every row of the bucket's listing, in the order the service reports them, following
/// `continuationToken` so a paged read reports the whole listing.
async fn listing(emulator: &Emulator, case: &Case, bucket_arn: &str, phase: &str) -> Vec<Value> {
    let label = case.label;
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    // One page per table, plus headroom for the empty page a cursor can end on.
    let pages = case.tables().count() + 2;
    for page in 0..pages {
        let mut query: Vec<(&str, String)> = vec![("maxTables", "1".to_string())];
        if let Some(token) = &token {
            query.push(("continuationToken", token.clone()));
        }
        let reply = emulator.list_tables(case.region, bucket_arn, &query).await;
        assert_eq!(
            reply.status, 200,
            "case {label:?}: ListTables ({phase}, page {page}): {status} {body}",
            status = reply.status,
            body = reply.body
        );
        collected.extend(reply.list("tables"));
        token = reply.optional_text("continuationToken");
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {label:?}: the {phase} listing never ran out of pages");
}

/// RULE 1 and RULE 2 over every row of `rows`: the instants are date-time strings carrying
/// the text the read of the same table reports, the modification never precedes the
/// creation, and the ARN is the minted one, its id segment a canonical hyphenated UUID.
async fn assert_rows(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    created_arns: &[(&str, &str, String)],
    rows: &[Value],
    phase: &str,
) {
    assert_eq!(
        rows.len(),
        created_arns.len(),
        "case {label:?} / {phase}: expected the listing to carry {expected} rows, got {rows:?}",
        label = case.label,
        expected = created_arns.len()
    );
    for (namespace, table, created_arn) in created_arns {
        let case_label = format!("{label} / {phase} / {table}", label = case.label);
        let row = summary_of(rows, table, &case_label);

        let described = emulator
            .get_table(case.region, bucket_arn, namespace, table)
            .await;
        assert_eq!(
            described.status, 200,
            "case {case_label:?}: GetTable: {status} {body}",
            status = described.status,
            body = described.body
        );

        // RULE 1.
        for member in ["createdAt", "modifiedAt"] {
            let actual = row.get(member);
            assert!(
                actual.is_some_and(Value::is_string),
                "case {case_label:?}: expected {member} to be an RFC 3339 date-time string, got \
                 {actual:?}"
            );
            let expected = described.text(member);
            assert_eq!(
                actual.and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case_label:?}: the row reports {member} as {actual:?}, not the {expected:?} \
                 the read of the same table reports"
            );
        }
        let created_at = timestamp_seconds(row, "createdAt", &case_label);
        let modified_at = timestamp_seconds(row, "modifiedAt", &case_label);
        assert!(
            modified_at >= created_at,
            "case {case_label:?}: the row was last modified at {modified_at}, before it was \
             created at {created_at}"
        );

        // RULE 2.
        let row_arn = row.get("tableARN").and_then(Value::as_str);
        assert_eq!(
            row_arn,
            Some(created_arn.as_str()),
            "case {case_label:?}: expected the row to carry the minted ARN {created_arn:?}, got \
             {row_arn:?}"
        );
        assert_canonical_uuid(
            &table_id_of(created_arn, &case_label),
            "the id segment of a listing row's tableARN",
            &case_label,
        );
    }
}

/// RULES 1 to 5, over every case: the listing is read at each phase of the metrics
/// configuration's lifecycle, and the configuration is read on a bucket that holds tables.
#[tokio::test]
async fn the_metrics_configuration_lifecycle_leaves_the_table_listing_as_it_was() {
    // The ids every case's served read reported: RULE 4 asks for one value across them all.
    let mut reported_ids: BTreeSet<String> = BTreeSet::new();

    for case in CASES {
        let label = case.label;
        let emulator = Emulator::new();

        let characters = case.bucket.chars().count();
        let (shortest, longest) = BUCKET_NAME_BOUNDS;
        assert!(
            (shortest..=longest).contains(&characters),
            "case {label:?}: the case's own bucket name must stay inside the published bounds \
             {BUCKET_NAME_BOUNDS:?}, but it is {characters} characters"
        );

        let bucket = emulator
            .create_table_bucket(case.region, &json!({ "name": case.bucket }))
            .await;
        assert_eq!(
            bucket.status, 200,
            "case {label:?}: CreateTableBucket: {status} {body}",
            status = bucket.status,
            body = bucket.body
        );
        let bucket_arn = bucket.text("arn");

        for (namespace, _) in case.namespaces {
            let created = emulator
                .create_namespace(case.region, &bucket_arn, &json!({ "namespace": [namespace] }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: CreateNamespace {namespace:?}: {status} {body}",
                status = created.status,
                body = created.body
            );
        }
        let mut created_arns: Vec<(&str, &str, String)> = Vec::new();
        for (namespace, table) in case.tables() {
            let created = emulator
                .create_table(
                    case.region,
                    &bucket_arn,
                    namespace,
                    &create_table_request(table, case.metadata),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: CreateTable {table:?}: {status} {body}",
                status = created.status,
                body = created.body
            );
            created_arns.push((namespace, table, created.text("tableARN")));
        }

        // Phase 1 — metrics were never turned on: the read is refused (RULE 5) and the
        // listing it leaves behind is the baseline every later phase is compared against.
        let read = emulator
            .get_table_bucket_metrics_configuration(case.region, &bucket_arn)
            .await;
        assert_metrics_off(&read, &format!("{label} / before any put"));
        let baseline = listing(&emulator, case, &bucket_arn, "before any put").await;
        assert_rows(
            &emulator,
            case,
            &bucket_arn,
            &created_arns,
            &baseline,
            "before any put",
        )
        .await;

        // Phase 2 — the put turns metrics on, however many times it is repeated, and the
        // read reports the whole-bucket configuration (RULE 4).
        for attempt in 1..=case.puts {
            let put = emulator
                .put_table_bucket_metrics_configuration(case.region, &bucket_arn)
                .await;
            assert_eq!(
                put.status, 204,
                "case {label:?}: put {attempt}: {status} {body}",
                status = put.status,
                body = put.body
            );
            let read = emulator
                .get_table_bucket_metrics_configuration(case.region, &bucket_arn)
                .await;
            assert_eq!(
                read.status, 200,
                "case {label:?}: the read after put {attempt} must be served, got {status} {body}",
                status = read.status,
                body = read.body
            );
            let id = read.optional_text("id");
            assert_eq!(
                id.as_deref(),
                Some(WHOLE_BUCKET_ID),
                "case {label:?}: after put {attempt}, expected id {WHOLE_BUCKET_ID:?}, got {id:?}"
            );
            assert_eq!(
                read.text("tableBucketARN"),
                bucket_arn,
                "case {label:?}: after put {attempt}, the read reports another bucket's ARN"
            );
            reported_ids.insert(read.text("id"));
        }

        // The configuration is the bucket's, not the tables': the listing is untouched
        // (RULE 3), and every row still carries the modelled forms (RULES 1 and 2).
        let after_put = listing(&emulator, case, &bucket_arn, "with metrics on").await;
        assert_eq!(
            after_put, baseline,
            "case {label:?}: turning metrics on changed the table listing"
        );
        assert_rows(
            &emulator,
            case,
            &bucket_arn,
            &created_arns,
            &after_put,
            "with metrics on",
        )
        .await;

        // Phase 3 — the delete removes the configuration, so the read is refused again with
        // the same fixed sentence (RULE 5), and the repeated delete is served and leaves it
        // refused.
        for attempt in 1..=2 {
            let deleted = emulator
                .delete_table_bucket_metrics_configuration(case.region, &bucket_arn)
                .await;
            assert_eq!(
                deleted.status, 204,
                "case {label:?}: delete {attempt}: {status} {body}",
                status = deleted.status,
                body = deleted.body
            );
            let read = emulator
                .get_table_bucket_metrics_configuration(case.region, &bucket_arn)
                .await;
            assert_metrics_off(&read, &format!("{label} / after delete {attempt}"));
        }

        // And the tables outlive the configuration, unchanged.
        let after_delete = listing(&emulator, case, &bucket_arn, "after the delete").await;
        assert_eq!(
            after_delete, baseline,
            "case {label:?}: turning metrics off changed the table listing"
        );
        assert_rows(
            &emulator,
            case,
            &bucket_arn,
            &created_arns,
            &after_delete,
            "after the delete",
        )
        .await;
    }

    // RULE 4: one fixed id across every bucket, region, table population and put count.
    assert_eq!(
        reported_ids,
        BTreeSet::from([WHOLE_BUCKET_ID.to_string()]),
        "expected every served read to report the one whole-bucket id \
         {WHOLE_BUCKET_ID:?}, got {reported_ids:?}"
    );
}
