// SPDX-License-Identifier: CC0-1.0

//! Fetching a query's results the way a CONSUMER does: at the location the API reported.
//!
//! The rule under test: a query whose run reported the statement done has, by the time
//! `GetQueryExecution` first reports it SUCCEEDED, saved its result set at the object
//! `ResultConfiguration.OutputLocation` names — `<effective location><QueryExecutionId>.csv`
//! for a statement whose results are rows, with its metadata information beside it at
//! `<that object>.metadata` — and the effective location is the workgroup's wherever that
//! workgroup's configuration is enforced, whatever location the request named, with no
//! refusal of the override. So a client that reads the reported location off the control
//! plane finds the object there, served as the media type it was saved as, and a bucket the
//! resolution passed over holds nothing at all.
//!
//! Every address asserted here is parsed out of what `GetQueryExecution` ANSWERED rather than
//! rebuilt from the prefix, because that one address is all a consumer of a query's results —
//! the console, the CLI, a job picking the object up — ever has.
//!
//! The wire harness is the one `query_result_objects.rs` owns (Athena and Amazon S3 mounted on
//! ONE shared engine, each driven over its own production front door), so there is a single
//! copy of it; what this suite adds is the consumer's reading of the reported location.

use serde_json::{json, Value};

use crate::query_result_objects::{
    create_bucket, create_work_group, emulator, listed_keys, parse_location, s3_served, start,
    succeeded, BUCKET, DECOY_BUCKET, GOVERNED_BUCKET,
};

/// The media type the results of a statement whose results are rows are saved as: Athena
/// saves those results as the CSV the object's own extension announces.
const CSV_MEDIA_TYPE: &str = "text/csv";

/// The extension the results object of a statement whose results are ROWS takes, and the one
/// its metadata sibling takes: `<query-result-location>/<QueryExecutionId>.csv` and the
/// metadata information saved beside it. Written out here rather than read off the
/// implementation, so the layout this suite pins is the published one.
const ROWS_EXTENSION: &str = ".csv";
/// See [`ROWS_EXTENSION`].
const METADATA_EXTENSION: &str = ".csv.metadata";

/// One row of the table below: what is submitted, and where its results must be fetchable
/// from afterwards.
struct Fetch {
    /// What the row is.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `StartQueryExecution` request, less the statement.
    request: Value,
    /// The bucket the results object must land in.
    bucket: &'static str,
    /// The prefix under it, without a trailing separator.
    prefix: &'static str,
    /// The CSV the results object must hold.
    csv: &'static str,
    /// The column names the metadata information beside it must describe, in projection
    /// order.
    labels: &'static [&'static str],
}

/// A consumer that reads back the location `GetQueryExecution` REPORTS finds the query's
/// result set there: the `.csv` object the run produced with its `.csv.metadata` sibling, each
/// served as the media type it was saved as — and a bucket an enforcing workgroup's location
/// overrode holds nothing, with no refusal of the override.
///
/// The rows differ along where the effective location came from (the request's own, the
/// workgroup's behind a request that names none, an enforcing workgroup's in ANOTHER bucket,
/// and an enforcing workgroup's nested prefix) and along the result set the statement
/// produced.
#[tokio::test]
async fn the_reported_results_location_holds_the_result_set_and_its_metadata() {
    let router = emulator();
    for bucket in [BUCKET, GOVERNED_BUCKET, DECOY_BUCKET] {
        create_bucket(&router, bucket).await;
    }

    // One workgroup that merely SUPPLIES a location to a request naming none, and two that
    // ENFORCE their own over whatever a request names.
    create_work_group(
        &router,
        "fetch-supplying",
        json!({
            "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/fetch-supplied/") }
        }),
    )
    .await;
    create_work_group(
        &router,
        "fetch-enforcing",
        json!({
            "ResultConfiguration": {
                "OutputLocation": format!("s3://{GOVERNED_BUCKET}/governed/")
            },
            "EnforceWorkGroupConfiguration": true,
        }),
    )
    .await;
    create_work_group(
        &router,
        "fetch-enforcing-nested",
        json!({
            "ResultConfiguration": {
                "OutputLocation": format!("s3://{GOVERNED_BUCKET}/year/2026/results/")
            },
            "EnforceWorkGroupConfiguration": true,
        }),
    )
    .await;

    let cases: Vec<Fetch> = vec![
        Fetch {
            case: "the location the request named",
            sql: "SELECT 'north' AS region",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/fetched/") }
            }),
            bucket: BUCKET,
            prefix: "fetched",
            csv: "\"region\"\n\"north\"\n",
            labels: &["region"],
        },
        Fetch {
            case: "the workgroup's location, behind a request that names none",
            sql: "SELECT 'desk' AS item, 1 AS id UNION ALL SELECT 'lamp' AS item, 2 AS id",
            request: json!({ "WorkGroup": "fetch-supplying" }),
            bucket: BUCKET,
            prefix: "fetch-supplied",
            csv: "\"item\",\"id\"\n\"desk\",\"1\"\n\"lamp\",\"2\"\n",
            labels: &["item", "id"],
        },
        Fetch {
            case: "an enforcing workgroup's location in another bucket",
            sql: "SELECT NULL AS absent, '' AS blank",
            request: json!({
                "WorkGroup": "fetch-enforcing",
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{DECOY_BUCKET}/ignored/")
                },
            }),
            bucket: GOVERNED_BUCKET,
            prefix: "governed",
            csv: "\"absent\",\"blank\"\n,\"\"\n",
            labels: &["absent", "blank"],
        },
        Fetch {
            case: "an enforcing workgroup's nested location",
            sql: "SELECT 2 + 3 AS computed",
            request: json!({
                "WorkGroup": "fetch-enforcing-nested",
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{DECOY_BUCKET}/also-ignored/")
                },
            }),
            bucket: GOVERNED_BUCKET,
            prefix: "year/2026/results",
            csv: "\"computed\"\n\"5\"\n",
            labels: &["computed"],
        },
    ];

    // Every row's query is started before any of them is read back, so the span a completion
    // is owed after elapses once for the whole table rather than once per row.
    let mut started: Vec<(&Fetch, String)> = Vec::new();
    for fetch in &cases {
        let mut request = fetch.request.clone();
        request["QueryString"] = json!(fetch.sql);
        started.push((fetch, start(&router, fetch.case, &request).await));
    }

    for (fetch, id) in &started {
        let case = fetch.case;
        // Polled for the CONDITION and read back the instant it holds, so what follows also
        // pins the order every consumer depends on: the results are already saved by the time
        // the query first reports SUCCEEDED.
        let found = succeeded(&router, case, id).await;
        let location = found["ResultConfiguration"]["OutputLocation"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a SUCCEEDED query must report the object its results were \
                     saved at, got {found:?}"
                )
            })
            .to_string();

        // The reported location is the results object of THIS execution, keyed by its own id
        // under the effective location.
        let expected = format!(
            "s3://{}/{}/{id}{ROWS_EXTENSION}",
            fetch.bucket, fetch.prefix
        );
        assert_eq!(
            location, expected,
            "case {case:?}: expected the results object {expected:?}, got {location:?}"
        );

        // And the address it names holds the result set, fetched exactly where the control
        // plane said it would be.
        let (bucket, key) = parse_location(case, &location);
        let (status, media_type, body) =
            s3_served(&router, "GET", &format!("/{bucket}/{key}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject({location}) must answer 200, got {status} with {body:?}"
        );
        assert_eq!(
            body, fetch.csv,
            "case {case:?}: expected {location} to hold {:?}, got {body:?}",
            fetch.csv
        );
        assert_eq!(
            media_type.as_deref(),
            Some(CSV_MEDIA_TYPE),
            "case {case:?}: expected {location} to be served as {CSV_MEDIA_TYPE}, got \
             {media_type:?}"
        );

        // The metadata information is the sibling of that same object, and describes the
        // result set saved beside it. Its encoding is this emulator's own — no published
        // format describes Athena's — so what is pinned here is the schema it has to carry:
        // one entry per column of the projection, under the `Name` the modelled `ColumnInfo`
        // requires, in projection order.
        let metadata_key = format!("{}/{id}{METADATA_EXTENSION}", fetch.prefix);
        let (status, _, metadata) =
            s3_served(&router, "GET", &format!("/{bucket}/{metadata_key}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject(s3://{bucket}/{metadata_key}) must answer 200, got \
             {status} with {metadata:?}"
        );
        let document: Value = serde_json::from_str(&metadata).unwrap_or(Value::Null);
        let columns = document["ColumnInfo"].as_array().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the metadata to describe the result set's columns, got \
                 {metadata:?}"
            )
        });
        let described: Vec<String> = columns
            .iter()
            .map(|column| {
                column["Name"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected every described column to carry its Name, \
                             got {column:?}"
                        )
                    })
                    .to_string()
            })
            .collect();
        assert_eq!(
            described, *fetch.labels,
            "case {case:?}: expected the metadata to describe the columns {:?}, got \
             {metadata:?}",
            fetch.labels
        );

        // The effective location holds exactly that pair and nothing else.
        let mut expected_keys = vec![key.clone(), metadata_key.clone()];
        expected_keys.sort();
        let mut listed = listed_keys(&router, fetch.bucket, &format!("{}/", fetch.prefix)).await;
        listed.sort();
        assert_eq!(
            listed, expected_keys,
            "case {case:?}: expected s3://{}/{} to hold {expected_keys:?}, got {listed:?}",
            fetch.bucket, fetch.prefix
        );
    }

    // Nothing was written to the bucket the enforcing workgroups' overrides passed over, and
    // no submission naming it was refused.
    let decoy = listed_keys(&router, DECOY_BUCKET, "").await;
    assert!(
        decoy.is_empty(),
        "an overridden results location must stay empty, got {decoy:?}"
    );
}
