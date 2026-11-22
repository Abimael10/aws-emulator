// SPDX-License-Identifier: CC0-1.0

//! The two halves of the `EnforceWorkGroupConfiguration` contract, told apart by WHERE THE
//! RESULT BYTES LAND — across two buckets as well as across two prefixes of one.
//!
//! The rule under test: the output location one query runs under is resolved at SUBMIT time
//! out of the request's `ResultConfiguration` and the workgroup's own — the workgroup's
//! replacing the request's only where that workgroup enforces its configuration — and a run
//! that reported the statement done has MATERIALISED its results under the resolved location
//! by the time the execution first reports SUCCEEDED, keyed by the execution's own id. So a
//! listing of the resolved prefix holds that object, and a listing of the prefix the
//! resolution passed over holds nothing: a tenant can tell enforcement on from enforcement
//! off from the object store alone, whichever bucket each location names.
//!
//! What this suite adds to the ones beside it is the resolution reached through
//! `UpdateWorkGroup` — every workgroup here is created under the OPPOSITE setting and
//! switched before the query is submitted — with the two candidate locations in DIFFERENT
//! buckets, which is the shape a governed results bucket is set up in.
//!
//! The wire harness is the one `query_result_objects.rs` owns (Athena and Amazon S3 mounted
//! on ONE shared engine, each driven over its own production front door), so there is a
//! single copy of it.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    query_results::RESULT_METADATA_SUFFIX, RESULT_OBJECT_ROWS_SUFFIX,
};

use crate::query_result_objects::{
    athena, create_bucket, create_work_group, emulator, listed_keys, object, start, succeeded,
    update_work_group,
};

/// The bucket the workgroups of this suite keep their own results in: the governed
/// destination of an enforcing workgroup.
const WORK_GROUP_BUCKET: &str = "athena-enforced-results-bucket";

/// The bucket the requests of this suite name a results location in — a DIFFERENT bucket
/// from [`WORK_GROUP_BUCKET`], so the two halves of the contract are distinguishable at the
/// bucket and not only at the prefix.
const CLIENT_BUCKET: &str = "athena-client-chosen-bucket";

/// A bucket holding BOTH candidate prefixes, for the rows whose two locations differ only in
/// their prefix.
const SHARED_BUCKET: &str = "athena-shared-results-bucket";

/// One row of the table: the two candidate locations, the setting in force when the query is
/// submitted, and where the results have to be found afterwards.
struct Resolution {
    /// What the row is.
    case: &'static str,
    /// The workgroup this row's query runs in.
    work_group: &'static str,
    /// Whether that workgroup ENFORCES its configuration when the query is submitted. Every
    /// row reaches this setting through `UpdateWorkGroup`, from the opposite one at create.
    enforce: bool,
    /// The `OutputLocation` the workgroup's own configuration names.
    work_group_location: String,
    /// The `OutputLocation` the request names, as it writes it, or `None` for a request that
    /// names none.
    requested_location: Option<String>,
    /// The statement submitted.
    sql: &'static str,
    /// The bytes the saved results object must hold.
    csv: &'static str,
    /// The bucket the results object must land in.
    resolved_bucket: &'static str,
    /// The prefix under it, with neither a leading nor a trailing separator.
    resolved_prefix: &'static str,
    /// The bucket and prefix the resolution passed over, which must hold nothing at all.
    passed_over: Option<(&'static str, &'static str)>,
}

/// With enforcement OFF the request's own output location is the one the results are written
/// under, and with enforcement ON the workgroup's is — in either case the object is there by
/// the time the execution first reports SUCCEEDED, and the location the resolution passed
/// over is left empty.
///
/// The rows differ along what the rule generalises over: the enforcement setting in force at
/// submit time, whether the two candidate locations are in one bucket or in two, whether the
/// request names a location at all, the form the prefix was written in (with its trailing
/// separator, without it, and nested), and the values the statement produced.
#[tokio::test]
async fn the_enforcement_setting_decides_which_bucket_and_prefix_holds_the_results() {
    let router = emulator();
    for bucket in [WORK_GROUP_BUCKET, CLIENT_BUCKET, SHARED_BUCKET] {
        create_bucket(&router, bucket).await;
    }

    let cases: Vec<Resolution> = vec![
        Resolution {
            case: "enforcement turned off, the request's location in another bucket",
            work_group: "switched-off-across-buckets",
            enforce: false,
            work_group_location: format!("s3://{WORK_GROUP_BUCKET}/enforced/"),
            requested_location: Some(format!("s3://{CLIENT_BUCKET}/clientchosen/")),
            sql: "SELECT 3 AS a",
            csv: "\"a\"\n\"3\"\n",
            resolved_bucket: CLIENT_BUCKET,
            resolved_prefix: "clientchosen",
            passed_over: Some((WORK_GROUP_BUCKET, "enforced")),
        },
        Resolution {
            case: "enforcement turned on, the request's location in another bucket",
            work_group: "switched-on-across-buckets",
            enforce: true,
            work_group_location: format!("s3://{WORK_GROUP_BUCKET}/governed/"),
            requested_location: Some(format!("s3://{CLIENT_BUCKET}/overridden/")),
            sql: "SELECT 'governed' AS marker",
            csv: "\"marker\"\n\"governed\"\n",
            resolved_bucket: WORK_GROUP_BUCKET,
            resolved_prefix: "governed",
            passed_over: Some((CLIENT_BUCKET, "overridden")),
        },
        Resolution {
            case: "enforcement turned off, both locations in one bucket",
            work_group: "switched-off-one-bucket",
            enforce: false,
            work_group_location: format!("s3://{SHARED_BUCKET}/wgout/"),
            requested_location: Some(format!("s3://{SHARED_BUCKET}/clientout/")),
            sql: "SELECT 9 AS n",
            csv: "\"n\"\n\"9\"\n",
            resolved_bucket: SHARED_BUCKET,
            resolved_prefix: "clientout",
            passed_over: Some((SHARED_BUCKET, "wgout")),
        },
        Resolution {
            case: "enforcement turned off, a nested prefix written without its separator",
            work_group: "switched-off-nested-prefix",
            enforce: false,
            work_group_location: format!("s3://{WORK_GROUP_BUCKET}/unused/"),
            requested_location: Some(format!("s3://{CLIENT_BUCKET}/deep/nested/landing")),
            sql: "SELECT 'nested' AS marker",
            csv: "\"marker\"\n\"nested\"\n",
            resolved_bucket: CLIENT_BUCKET,
            resolved_prefix: "deep/nested/landing",
            passed_over: Some((WORK_GROUP_BUCKET, "unused")),
        },
        Resolution {
            case: "enforcement turned off, with no location of the request's own",
            work_group: "switched-off-without-request-location",
            enforce: false,
            work_group_location: format!("s3://{WORK_GROUP_BUCKET}/fallback/"),
            requested_location: None,
            sql: "SELECT 'fallback' AS marker",
            csv: "\"marker\"\n\"fallback\"\n",
            resolved_bucket: WORK_GROUP_BUCKET,
            resolved_prefix: "fallback",
            passed_over: None,
        },
    ];

    // Every workgroup is created under the OPPOSITE setting and switched to the row's own,
    // so what each query is judged by is the configuration in force at its submit.
    for resolution in &cases {
        create_work_group(
            &router,
            resolution.work_group,
            json!({
                "ResultConfiguration": { "OutputLocation": resolution.work_group_location },
                "EnforceWorkGroupConfiguration": !resolution.enforce,
            }),
        )
        .await;
        update_work_group(
            &router,
            resolution.work_group,
            json!({ "EnforceWorkGroupConfiguration": resolution.enforce }),
        )
        .await;
        // The premise of the row, read back rather than assumed: the switch really took.
        let (status, read) = athena(
            &router,
            "GetWorkGroup",
            &json!({ "WorkGroup": resolution.work_group }),
        )
        .await;
        let case = resolution.case;
        assert_eq!(
            status, 200,
            "case {case:?}: GetWorkGroup({}) must answer 200, got {status} with {read:?}",
            resolution.work_group
        );
        let configuration = &read["WorkGroup"]["Configuration"];
        assert_eq!(
            configuration["EnforceWorkGroupConfiguration"].as_bool(),
            Some(resolution.enforce),
            "case {case:?}: expected EnforceWorkGroupConfiguration {}, got {configuration:?}",
            resolution.enforce
        );
    }

    // Every query is started before any is read back, so the span a completion is owed after
    // elapses once for the whole table rather than once per row.
    let mut started: Vec<(&Resolution, String)> = Vec::new();
    for resolution in &cases {
        let mut request: Value =
            json!({ "QueryString": resolution.sql, "WorkGroup": resolution.work_group });
        if let Some(location) = resolution.requested_location.as_deref() {
            request["ResultConfiguration"] = json!({ "OutputLocation": location });
        }
        started.push((resolution, start(&router, resolution.case, &request).await));
    }

    for (resolution, id) in &started {
        let case = resolution.case;
        let bucket = resolution.resolved_bucket;
        let found = succeeded(&router, case, id).await;
        let key = format!(
            "{}/{id}{RESULT_OBJECT_ROWS_SUFFIX}",
            resolution.resolved_prefix
        );

        // The location the control plane NAMES is the object the data plane HOLDS.
        assert_eq!(
            found["ResultConfiguration"]["OutputLocation"].as_str(),
            Some(format!("s3://{bucket}/{key}").as_str()),
            "case {case:?}: expected the results object s3://{bucket}/{key}, got {found:?}"
        );
        // Listed under the resolved prefix as a FOLDER — with its separator — so a key that
        // merely starts with the same characters cannot be counted as one of this row's.
        let listed = listed_keys(
            &router,
            bucket,
            &format!("{}/", resolution.resolved_prefix),
        )
        .await;
        assert_eq!(
            listed,
            vec![key.clone(), format!("{key}{RESULT_METADATA_SUFFIX}")],
            "case {case:?}: expected s3://{bucket}/{} to hold {key} and its metadata, got \
             {listed:?}",
            resolution.resolved_prefix
        );
        let (status, body) = object(&router, bucket, &key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetObject(s3://{bucket}/{key}) must answer 200, got {status} with \
             {body:?}"
        );
        assert_eq!(
            body, resolution.csv,
            "case {case:?}: expected the rows the run produced ({:?}), got {body:?}",
            resolution.csv
        );

        // The location the resolution passed over took nothing at all, which is what tells
        // the two halves of the contract apart.
        if let Some((ignored_bucket, ignored_prefix)) = resolution.passed_over {
            let passed_over =
                listed_keys(&router, ignored_bucket, &format!("{ignored_prefix}/")).await;
            assert!(
                passed_over.is_empty(),
                "case {case:?}: expected s3://{ignored_bucket}/{ignored_prefix} to stay empty, \
                 got {passed_over:?}"
            );
        }
    }
}
