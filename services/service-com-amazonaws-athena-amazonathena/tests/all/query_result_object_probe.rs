// SPDX-License-Identifier: CC0-1.0

//! PROBING for a query's results object, the way a client that only wants to know whether it
//! is there does: a `HEAD` of the address `GetQueryExecution` reported.
//!
//! The rule under test: by the time `GetQueryExecution` first reports a query SUCCEEDED, the
//! object `ResultConfiguration.OutputLocation` names EXISTS in the object store — so the
//! metadata read of that address (`HeadObject`, "useful if you're interested only in an
//! object's metadata") answers for it, announcing the media type it was saved as, and so does
//! the metadata information saved beside a result set of rows at `<that object>.metadata`;
//! while an address the resolution did NOT choose holds no such object, so the same read of it
//! is a 404. The effective location is the workgroup's wherever that workgroup's configuration
//! is enforced, whatever location the request named, and the override is silent.
//!
//! What this suite adds to the ones beside it is the EXISTENCE PROBE rather than a fetch: a
//! client driving a query to completion and then waiting on `HeadObject` — what the CLI's
//! waiters, the drivers and most pipeline glue do — is answered about the object, not told the
//! key is absent. The address probed is parsed out of what the control plane ANSWERED, because
//! that one address is all such a client has.
//!
//! The rows differ along what the rule generalises over: the class of statement submitted (one
//! whose results are rows, a definition and a utility statement, whose result is the engine's
//! text output), where the effective location came from (the request's own, a workgroup's
//! behind a request naming none, and an enforcing workgroup's in ANOTHER bucket over the one
//! the request named), and the form the prefix was written in (with its trailing separator,
//! without it, and nested).
//!
//! The wire harness is the one `query_result_objects.rs` owns (Athena and Amazon S3 mounted on
//! ONE shared engine, each driven over its own production front door), so there is a single
//! copy of it.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    query_results::RESULT_METADATA_SUFFIX, RESULT_OBJECT_ROWS_SUFFIX, RESULT_OBJECT_TEXT_SUFFIX,
};

use crate::query_result_objects::{
    create_bucket, create_work_group, emulator, listed_keys, parse_location, s3_served, start,
    succeeded, BUCKET, DECOY_BUCKET, GOVERNED_BUCKET,
};

/// The media types the two classes of results object are saved as: the CSV a result set of
/// rows is saved as, and the plain text the engine's own output is saved as. Written out here
/// rather than read off the implementation, so what this suite pins is the media type a reader
/// of the saved object is told its bytes are.
const CSV_MEDIA_TYPE: &str = "text/csv";
/// See [`CSV_MEDIA_TYPE`].
const TEXT_MEDIA_TYPE: &str = "text/plain";
/// See [`CSV_MEDIA_TYPE`]. The metadata information's encoding is this emulator's own — no
/// published format describes Athena's — so this is the type that document is served as.
const METADATA_MEDIA_TYPE: &str = "application/json";

/// A key no query of this suite wrote, probed in each row's own bucket: the probe of an
/// address nothing was saved at has to answer 404, which is what makes a 200 for the reported
/// address evidence rather than a store that answers for everything.
const ABSENT_KEY: &str = "probed/nothing-was-saved-here.csv";

/// `HEAD /<bucket>/<key>` — `HeadObject` — answering its status and the media type it
/// announced. A metadata read carries no body, so nothing is read out of one.
async fn probe(
    router: &smithy_server_runtime::MultiServiceRouter,
    bucket: &str,
    key: &str,
) -> (u16, Option<String>) {
    let (status, media_type, _) = s3_served(router, "HEAD", &format!("/{bucket}/{key}")).await;
    (status, media_type)
}

/// One row of the table below: what is submitted, and what the probe of its reported location
/// must find.
struct Probe {
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
    /// The extension the results object takes.
    extension: &'static str,
    /// The media type it must be announced as.
    media_type: &'static str,
    /// Whether metadata information is saved beside it: a statement whose results are rows
    /// saves it, and one whose result is text output has none to save.
    metadata: bool,
    /// Whether this row's request named [`DECOY_BUCKET`] as its own results location, which an
    /// enforcing workgroup then overrode: only such a row has an overridden address to probe.
    names_decoy: bool,
}

/// A client that drives a query to SUCCEEDED and then PROBES the location the control plane
/// reported is answered about the object: the metadata read of that address succeeds, naming
/// the media type the results were saved as, and so does the read of the metadata information
/// beside a result set of rows — while the same read of an address nothing was saved at, and of
/// the bucket an enforcing workgroup's location overrode, is a 404.
#[tokio::test]
async fn the_reported_results_location_answers_a_metadata_read() {
    let router = emulator();
    for bucket in [BUCKET, GOVERNED_BUCKET, DECOY_BUCKET] {
        create_bucket(&router, bucket).await;
    }

    // One workgroup that merely SUPPLIES a location to a request naming none, and one that
    // ENFORCES its own — in another bucket — over whatever a request names.
    create_work_group(
        &router,
        "probe-supplying",
        json!({
            "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/probe-supplied/") }
        }),
    )
    .await;
    create_work_group(
        &router,
        "probe-enforcing",
        json!({
            "ResultConfiguration": {
                "OutputLocation": format!("s3://{GOVERNED_BUCKET}/probe-governed/")
            },
            "EnforceWorkGroupConfiguration": true,
        }),
    )
    .await;

    let cases: Vec<Probe> = vec![
        Probe {
            case: "rows under the location the request named",
            sql: "SELECT 'north' AS marker",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/probed/") }
            }),
            bucket: BUCKET,
            prefix: "probed",
            extension: RESULT_OBJECT_ROWS_SUFFIX,
            media_type: CSV_MEDIA_TYPE,
            metadata: true,
            names_decoy: false,
        },
        Probe {
            case: "rows under a location written without its trailing separator",
            sql: "SELECT 'south' AS marker, 7 AS orders",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/probed-bare") }
            }),
            bucket: BUCKET,
            prefix: "probed-bare",
            extension: RESULT_OBJECT_ROWS_SUFFIX,
            media_type: CSV_MEDIA_TYPE,
            metadata: true,
            names_decoy: false,
        },
        Probe {
            case: "rows under the workgroup's location, behind a request naming none",
            sql: "SELECT 'east' AS marker UNION ALL SELECT 'west' AS marker",
            request: json!({ "WorkGroup": "probe-supplying" }),
            bucket: BUCKET,
            prefix: "probe-supplied",
            extension: RESULT_OBJECT_ROWS_SUFFIX,
            media_type: CSV_MEDIA_TYPE,
            metadata: true,
            names_decoy: false,
        },
        Probe {
            case: "rows under an enforcing workgroup's location in another bucket",
            sql: "SELECT 'central' AS marker",
            request: json!({
                "WorkGroup": "probe-enforcing",
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{DECOY_BUCKET}/probe-ignored/")
                },
            }),
            bucket: GOVERNED_BUCKET,
            prefix: "probe-governed",
            extension: RESULT_OBJECT_ROWS_SUFFIX,
            media_type: CSV_MEDIA_TYPE,
            metadata: true,
            names_decoy: true,
        },
        Probe {
            case: "a definition, whose result is text output, under a nested location",
            sql: "CREATE TABLE probed_rows (id integer)",
            request: json!({
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{BUCKET}/probed/year/2026/")
                }
            }),
            bucket: BUCKET,
            prefix: "probed/year/2026",
            extension: RESULT_OBJECT_TEXT_SUFFIX,
            media_type: TEXT_MEDIA_TYPE,
            metadata: false,
            names_decoy: false,
        },
        Probe {
            case: "a utility statement, whose result is text output",
            sql: "EXPLAIN SELECT 1",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/probed-plan/") }
            }),
            bucket: BUCKET,
            prefix: "probed-plan",
            extension: RESULT_OBJECT_TEXT_SUFFIX,
            media_type: TEXT_MEDIA_TYPE,
            metadata: false,
            names_decoy: false,
        },
        Probe {
            case: "text output under an enforcing workgroup's location in another bucket",
            sql: "CREATE TABLE probed_governed_rows (id integer)",
            request: json!({
                "WorkGroup": "probe-enforcing",
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{DECOY_BUCKET}/probe-also-ignored/")
                },
            }),
            bucket: GOVERNED_BUCKET,
            prefix: "probe-governed",
            extension: RESULT_OBJECT_TEXT_SUFFIX,
            media_type: TEXT_MEDIA_TYPE,
            metadata: false,
            names_decoy: true,
        },
    ];

    // Every row's query is started before any of them is read back, so the span a completion
    // is owed after elapses once for the whole table rather than once per row.
    let mut started: Vec<(&Probe, String)> = Vec::new();
    for case in &cases {
        let mut request = case.request.clone();
        request["QueryString"] = json!(case.sql);
        started.push((case, start(&router, case.case, &request).await));
    }

    for (probed, id) in &started {
        let case = probed.case;
        // Polled for the CONDITION and probed the instant it holds, so what follows pins the
        // order a waiting client depends on: the results object is already there when the
        // query first reports SUCCEEDED.
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

        let expected = format!(
            "s3://{}/{}/{id}{}",
            probed.bucket, probed.prefix, probed.extension
        );
        assert_eq!(
            location, expected,
            "case {case:?}: expected the results object {expected:?}, got {location:?}"
        );

        // The address the control plane NAMED answers a metadata read, as the media type the
        // results were saved as.
        let (bucket, key) = parse_location(case, &location);
        let (status, media_type) = probe(&router, &bucket, &key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: HeadObject({location}) must answer 200, got {status}"
        );
        assert_eq!(
            media_type.as_deref(),
            Some(probed.media_type),
            "case {case:?}: expected {location} to be announced as {}, got {media_type:?}",
            probed.media_type
        );

        // The metadata information beside a result set of rows answers one too; a statement
        // whose result is text output saved none, so nothing is there to answer.
        let metadata_key = format!("{key}{RESULT_METADATA_SUFFIX}");
        let (status, media_type) = probe(&router, &bucket, &metadata_key).await;
        let (expected_status, expected_media_type) = match probed.metadata {
            true => (200, Some(METADATA_MEDIA_TYPE)),
            false => (404, None),
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: HeadObject(s3://{bucket}/{metadata_key}) must answer \
             {expected_status}, got {status}"
        );
        if let Some(expected_media_type) = expected_media_type {
            assert_eq!(
                media_type.as_deref(),
                Some(expected_media_type),
                "case {case:?}: expected s3://{bucket}/{metadata_key} to be announced as \
                 {expected_media_type}, got {media_type:?}"
            );
        }

        // A key nothing was saved at, in that very bucket, is absent — so the 200 above is
        // about the object this run wrote and not about any address at all.
        let (status, _) = probe(&router, &bucket, ABSENT_KEY).await;
        assert_eq!(
            status, 404,
            "case {case:?}: HeadObject(s3://{bucket}/{ABSENT_KEY}) must answer 404, got {status}"
        );

        // And where this row's request named a location of its own that the workgroup overrode,
        // the object is not at that address either: the override sent the results elsewhere
        // silently and left nothing behind where the request asked for them.
        if probed.names_decoy {
            let (status, _) = probe(&router, DECOY_BUCKET, &key).await;
            assert_eq!(
                status, 404,
                "case {case:?}: HeadObject(s3://{DECOY_BUCKET}/{key}) must answer 404, got \
                 {status}"
            );
        }
    }

    // Nothing at all was written to the bucket an enforcing workgroup's location overrode, and
    // no submission naming it was refused.
    let decoy = listed_keys(&router, DECOY_BUCKET, "").await;
    assert!(
        decoy.is_empty(),
        "an overridden results location must stay empty, got {decoy:?}"
    );
}
