// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ExportEBSVolumeRecommendations`.
//!
//! Every call goes through the same `MultiServiceRouter` dispatch a client uses, so what is
//! asserted is what a client sees. The RULES asserted here are the ones this export shares
//! with every other `Export*Recommendations` operation of the service — it RECORDS one job
//! for the Region its endpoint serves, answers that job's id and the destination keys derived
//! from the request's bucket and key prefix, the account and the resource type, the job's
//! `JobStatus` then walks the lifecycle the clock drives, and only one volume export job runs
//! per Region at a time — plus the ones specific to it: the job carries the `EbsVolume`
//! resource type, and the request is refused for every input rule the operation documents,
//! over the same `EBSFilters` vocabulary `GetEBSVolumeRecommendations` selects with. Each is
//! checked over a TABLE of inputs rather than one call.
//!
//! What the exported FILE holds is asserted here too
//! ([`the_export_file_states_what_the_reader_reports_for_every_volume`]): every cell of a
//! volume's row is the value `GetEBSVolumeRecommendations` reports for the member that column
//! names, under the look-back period in force, so a file and a read of the same analysis cannot
//! disagree. The reader is the ORACLE there, so nothing restates a volume type, a figure or a
//! tag this analysis happens to hold.
//!
//! The reader of what these calls record, `DescribeRecommendationExportJobs`, has its own
//! suite (`recommendation_export_jobs.rs`); it is used here only to read a created job back.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::ops::types::{
    EXPORTABLE_VOLUME_FIELD_VALUES, LOOK_BACK_PERIOD_PREFERENCE_VALUES,
};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::{
    EXPORT_JOB_STEP, JOB_STATUS_COMPLETE, JOB_STATUS_IN_PROGRESS, JOB_STATUS_QUEUED,
    RESOURCE_TYPE_EBS_VOLUME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    analysed_emulator, assert_figure, call, call_in_region, cell, create_bucket, emulator,
    error_code, exported_recommendation_rows, metrics, opt_in, put_preference, ACCOUNT,
};

/// The operation under test.
const EXPORT: &str = "ExportEBSVolumeRecommendations";

/// The destination bucket these fixtures export to. Compute Optimizer does not create it;
/// the request just names it.
const BUCKET: &str = "ebs-recommendation-exports";

/// Enough slack over one lifecycle step for the clock to have made it due whatever the test
/// scheduler is doing; the step itself is the service's own ([`EXPORT_JOB_STEP`]).
const SLACK: Duration = Duration::from_millis(120);

/// An emulator with the account opted in, which every export call requires.
async fn enrolled_emulator() -> MultiServiceRouter {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    opt_in(&router).await;
    router
}

/// Export the account's Amazon EBS volume recommendations, asserting the call was accepted,
/// and answer the whole response body.
async fn export_ok(router: &MultiServiceRouter, case: &str, request: Value) -> Value {
    let (status, body) = call(router, EXPORT, request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {request} to be accepted, got {status}: {body}"
    );
    body
}

/// Export with the default destination and answer the job id.
async fn export_job_id(router: &MultiServiceRouter, case: &str) -> String {
    let body = export_ok(
        router,
        case,
        json!({"s3DestinationConfig": {"bucket": BUCKET}}),
    )
    .await;
    body["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an accepted export answers a jobId, got {body}"))
        .to_string()
}

/// Read the export jobs of the request's Region back, asserting the read was accepted.
async fn describe_ok(router: &MultiServiceRouter, case: &str, request: Value) -> Value {
    let (status, body) = call(router, "DescribeRecommendationExportJobs", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {request} to be accepted, got {status}: {body}"
    );
    body
}

/// The job ids a `DescribeRecommendationExportJobs` body reports, in the order it reported
/// them.
fn reported_ids(body: &Value) -> Vec<String> {
    body["recommendationExportJobs"]
        .as_array()
        .unwrap_or_else(|| panic!("the reader reports a job list, got {body}"))
        .iter()
        .map(|job| job["jobId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The one job the reader reports for `job_id`.
fn reported_job(case: &str, body: &Value, job_id: &str) -> Value {
    body["recommendationExportJobs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the reader reports a job list, got {body}"))
        .iter()
        .find(|job| job["jobId"] == json!(job_id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no job reported for {job_id}, got {body}"))
}

/// The destination keys an export answers are derived from the bucket and key prefix the
/// request named, the account the export is for, the resource type and the job id — one row
/// per form a key prefix can take, including none at all — and the job the call recorded
/// names exactly the objects the call answered.
#[tokio::test]
async fn the_destination_is_derived_from_the_request_and_the_minted_job_id() {
    let account = engine::emulator_account_id();

    // (case, the `keyPrefix` the request carries, the prefix the keys are expected under)
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        ("no key prefix at all", None, ""),
        ("an empty key prefix", Some(""), ""),
        ("a single-segment prefix", Some("exports"), "exports/"),
        (
            "a prefix with a trailing slash",
            Some("exports/"),
            "exports/",
        ),
        (
            "a prefix with leading and trailing slashes",
            Some("/nested/volume-exports/"),
            "nested/volume-exports/",
        ),
        ("a multi-segment prefix", Some("a/b/c"), "a/b/c/"),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, key_prefix, expected_prefix) in rows {
        // A fresh emulator per row: one volume export job may be in progress per Region at a
        // time, so each row gets its own store rather than waiting on the previous row's job.
        let router = enrolled_emulator().await;
        let mut destination_config = json!({"bucket": BUCKET});
        if let Some(key_prefix) = key_prefix {
            destination_config["keyPrefix"] = json!(key_prefix);
        }
        let body = export_ok(
            &router,
            case,
            json!({"s3DestinationConfig": destination_config}),
        )
        .await;

        let job_id = body["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a jobId, got {body}"))
            .to_string();
        // A `JobId` is an opaque string the service mints; this emulator hands out a
        // UUID-shaped one, which is what AWS's own job ids look like.
        assert_eq!(
            job_id.len(),
            36,
            "case {case:?}: expected a 36-character job id, got {job_id:?}"
        );
        assert!(
            job_id
                .char_indices()
                .all(|(at, character)| if matches!(at, 8 | 13 | 18 | 23) {
                    character == '-'
                } else {
                    character.is_ascii_hexdigit()
                }),
            "case {case:?}: expected a UUID-shaped job id, got {job_id:?}"
        );
        assert!(
            !minted.contains(&job_id),
            "case {case:?}: every export job gets its own id, got {job_id} twice"
        );
        minted.push(job_id.clone());

        let stem = format!(
            "{expected_prefix}{account}/ComputeOptimizer/{RESOURCE_TYPE_EBS_VOLUME}/{job_id}"
        );
        assert_eq!(
            body["s3Destination"],
            json!({
                "bucket": BUCKET,
                "key": format!("{stem}.csv"),
                "metadataKey": format!("{stem}-metadata.json"),
            }),
            "case {case:?}: unexpected destination, got {body}"
        );

        // The job the call recorded names exactly the objects the call answered, and carries
        // the resource type whose recommendations were exported.
        let listed = describe_ok(&router, case, json!({})).await;
        assert_eq!(
            reported_ids(&listed),
            vec![job_id.clone()],
            "case {case:?}: the export records exactly one job, got {listed}"
        );
        let job = reported_job(case, &listed, &job_id);
        assert_eq!(
            job["destination"],
            json!({"s3": body["s3Destination"].clone()}),
            "case {case:?}: the reported destination must be the one the export answered, \
             got {job}"
        );
        assert_eq!(
            job["resourceType"].as_str(),
            Some(RESOURCE_TYPE_EBS_VOLUME),
            "case {case:?}: a volume recommendations export is of {RESOURCE_TYPE_EBS_VOLUME}, \
             got {job}"
        );
        assert!(
            job["failureReason"].is_null(),
            "case {case:?}: a job that did not fail carries no failure reason, got {job}"
        );
    }
}

/// The job a volume export records walks the lifecycle the clock owes: `Queued` while the
/// caller can still see it queued, then `InProgress`, then the terminal `Complete`, which
/// never moves again. Reading it never moves it.
#[tokio::test]
async fn the_recorded_job_status_walks_the_lifecycle() {
    let router = enrolled_emulator().await;
    let job_id = export_job_id(&router, "a job to follow").await;

    let created = reported_job(
        "straight after the export",
        &describe_ok(&router, "straight after the export", json!({})).await,
        &job_id,
    );
    assert_eq!(
        created["status"].as_str(),
        Some(JOB_STATUS_QUEUED),
        "a job the clock has not moved yet is Queued, got {created}"
    );
    let created_at = created["creationTimestamp"]
        .as_f64()
        .unwrap_or_else(|| panic!("a reported job carries a creation timestamp, got {created}"));
    assert_eq!(
        created["lastUpdatedTimestamp"].as_f64(),
        Some(created_at),
        "a job that has not changed yet was last updated when it was created, got {created}"
    );

    // (case, how long to wait before reading, the status expected then)
    let steps: Vec<(&str, Duration, &str)> = vec![
        (
            "one step in",
            EXPORT_JOB_STEP + SLACK,
            JOB_STATUS_IN_PROGRESS,
        ),
        ("two steps in", EXPORT_JOB_STEP, JOB_STATUS_COMPLETE),
        (
            "well past the last step",
            2 * EXPORT_JOB_STEP,
            JOB_STATUS_COMPLETE,
        ),
    ];
    for (case, wait, expected) in steps {
        tokio::time::sleep(wait).await;
        let job = reported_job(case, &describe_ok(&router, case, json!({})).await, &job_id);
        assert_eq!(
            job["status"].as_str(),
            Some(expected),
            "case {case:?}: expected status {expected}, got {job}"
        );
        assert_eq!(
            job["creationTimestamp"].as_f64(),
            Some(created_at),
            "case {case:?}: the creation timestamp never changes, got {job}"
        );
        // A read reports the status the clock has made due and writes nothing, so the same
        // read twice answers the same job.
        let again = reported_job(case, &describe_ok(&router, case, json!({})).await, &job_id);
        assert_eq!(
            job, again,
            "case {case:?}: a read must not change what a later caller sees"
        );
    }
}

/// "You can have only one Amazon EBS volume export job in progress per Amazon Web Services
/// Region": a second export is refused while the first is running, admitted once it has
/// completed, and never blocked by a job of another Region.
#[tokio::test]
async fn only_one_volume_export_job_runs_per_region_at_a_time() {
    let router = enrolled_emulator().await;
    let first = export_job_id(&router, "the first export").await;

    let (status, body) = call(
        &router,
        EXPORT,
        json!({"s3DestinationConfig": {"bucket": BUCKET}}),
    )
    .await;
    assert_eq!(
        status, 400,
        "a second export while one is in progress is refused, got {status}: {body}"
    );
    assert!(
        error_code(&body).contains("LimitExceededException"),
        "expected LimitExceededException, got {body}"
    );
    assert_eq!(
        reported_ids(&describe_ok(&router, "after the refusal", json!({})).await),
        vec![first.clone()],
        "a refused export records no job"
    );

    // Another Region's endpoint has its own in-progress job: the limit is per Region.
    let (status, body) = call_in_region(
        &router,
        "eu-west-1",
        EXPORT,
        json!({"s3DestinationConfig": {"bucket": BUCKET}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "an export at another Region's endpoint is not blocked, got {status}: {body}"
    );
    assert_eq!(
        reported_ids(&describe_ok(&router, "the home Region", json!({})).await),
        vec![first.clone()],
        "a Region reports only its own export jobs"
    );

    // Once the first job reaches its terminal status it is no longer in progress, so the
    // next export is admitted.
    tokio::time::sleep(2 * EXPORT_JOB_STEP + SLACK).await;
    let second = export_job_id(&router, "after the first completed").await;
    assert_ne!(
        second, first,
        "every export job gets its own id, got {second} twice"
    );
    assert_eq!(
        reported_ids(&describe_ok(&router, "both jobs", json!({})).await),
        vec![first, second],
        "the reader reports the jobs in creation order"
    );
}

/// An export job of ANOTHER resource type never blocks a volume export, and the two are
/// reported side by side: the one-job-in-progress limit counts the jobs of the resource type
/// being exported.
#[tokio::test]
async fn a_job_of_another_resource_type_does_not_block_a_volume_export() {
    let router = enrolled_emulator().await;
    let (status, body) = call(
        &router,
        "ExportRDSDatabaseRecommendations",
        json!({"s3DestinationConfig": {"bucket": BUCKET}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a database export is accepted, got {status}: {body}"
    );
    let database_job = body["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("an accepted export answers a jobId, got {body}"))
        .to_string();

    let volume_job = export_job_id(&router, "a volume export beside a database one").await;
    assert_eq!(
        reported_ids(&describe_ok(&router, "both resource types", json!({})).await),
        vec![database_job, volume_job.clone()],
        "a job of another resource type does not block a volume export"
    );

    // And the volume job is the one a `ResourceType` filter for volumes selects.
    let listed = describe_ok(
        &router,
        "filtered to volumes",
        json!({"filters": [{"name": "ResourceType", "values": [RESOURCE_TYPE_EBS_VOLUME]}]}),
    )
    .await;
    assert_eq!(
        reported_ids(&listed),
        vec![volume_job],
        "only the volume export job carries {RESOURCE_TYPE_EBS_VOLUME}, got {listed}"
    );
}

/// An export request outside the service's documented input rules is refused with HTTP 400
/// and records no job — one row per rule, each on a value the rule rejects — and a request
/// that carries every member at a value the model declares is accepted.
#[tokio::test]
async fn an_invalid_export_request_is_refused() {
    let router = enrolled_emulator().await;

    // (case, request, the wire error code expected)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            // "You must create the destination Amazon S3 bucket ... Compute Optimizer does
            // not create the S3 bucket for you."
            "a destination naming no bucket",
            json!({"s3DestinationConfig": {}}),
            "InvalidParameterValueException",
        ),
        (
            "a destination whose bucket is blank",
            json!({"s3DestinationConfig": {"bucket": "   "}}),
            "InvalidParameterValueException",
        ),
        (
            // "This parameter cannot be specified together with the include member accounts
            // parameter. The parameters are mutually exclusive."
            "accountIds together with includeMemberAccounts",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "accountIds": [ACCOUNT],
                "includeMemberAccounts": true,
            }),
            "InvalidParameterValueException",
        ),
        (
            "an account id that is not 12 digits",
            json!({"s3DestinationConfig": {"bucket": BUCKET}, "accountIds": ["12345"]}),
            "InvalidParameterValueException",
        ),
        (
            "an account id that is not digits at all",
            json!({"s3DestinationConfig": {"bucket": BUCKET}, "accountIds": ["abcdefghijkl"]}),
            "InvalidParameterValueException",
        ),
        (
            // "The valid values are `Optimized`, or `NotOptimized`" (`EBSFilter$values`).
            "a finding outside the EBSFinding value set",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "filters": [{"name": "Finding", "values": ["Underprovisioned"]}],
            }),
            "InvalidParameterValueException",
        ),
        (
            "one valid finding beside one the model does not declare",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "filters": [{"name": "Finding", "values": ["Optimized", "Enormous"]}],
            }),
            "InvalidParameterValueException",
        ),
        (
            "a filter carrying no name",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "filters": [{"values": ["Optimized"]}],
            }),
            "InvalidParameterValueException",
        ),
        (
            // `EBSFilterName` is a modelled enum whose only value is Finding, so the request
            // boundary refuses every other name — including the `tag:key` and `tag-key`
            // names `EBSFilter$name`'s prose mentions — before the handler runs.
            "a filter name the model does not declare",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "filters": [{"name": "tag:Owner", "values": ["TeamA"]}],
            }),
            "InvalidParameterValueException",
        ),
        (
            // `FileFormat` is a modelled enum whose only value is Csv.
            "a file format the model does not declare",
            json!({"s3DestinationConfig": {"bucket": BUCKET}, "fileFormat": "Parquet"}),
            "InvalidParameterValueException",
        ),
        (
            // Likewise `ExportableVolumeField`.
            "a field to export the model does not declare",
            json!({
                "s3DestinationConfig": {"bucket": BUCKET},
                "fieldsToExport": ["VolumeArn", "NotAField"],
            }),
            "InvalidParameterValueException",
        ),
    ];

    for (case, request, wanted_code) in rows {
        let (status, body) = call(&router, EXPORT, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {request} to be refused, got {status}: {body}"
        );
        assert!(
            error_code(&body).contains(wanted_code),
            "case {case:?}: expected {wanted_code}, got {body}"
        );
        assert!(
            body["jobId"].is_null(),
            "case {case:?}: a refused export answers no job id, got {body}"
        );
        assert_eq!(
            reported_ids(&describe_ok(&router, case, json!({})).await),
            Vec::<String>::new(),
            "case {case:?}: a refused export records no job"
        );
    }

    // Every field a valid request may carry together, accepted: the rows above refuse a
    // VALUE, never the member. The exportable fields are `ExportableVolumeField` values the
    // model declares (`smithy_query query="shape" shape="ExportableVolumeField"`), including
    // ones no row above cites.
    let body = export_ok(
        &router,
        "every member set to a value the model declares",
        json!({
            "s3DestinationConfig": {"bucket": BUCKET, "keyPrefix": "quarterly"},
            "accountIds": [ACCOUNT],
            "includeMemberAccounts": false,
            "fileFormat": "Csv",
            "fieldsToExport": ["AccountId", "VolumeArn", "Finding",
                               "CurrentConfigurationVolumeType", "LookbackPeriodInDays",
                               "RecommendationOptionsMonthlyPrice", "RootVolume", "Tags"],
            "filters": [{"name": "Finding", "values": ["Optimized", "NotOptimized"]}],
        }),
    )
    .await;
    assert!(
        body["jobId"].is_string(),
        "an accepted export answers a job id, got {body}"
    );
}

/// The enrollment gate: "The account is not opted in to Compute Optimizer" is what the export
/// answers while the account is not enrolled, before and after opting back out, and no job is
/// recorded either time.
#[tokio::test]
async fn an_export_is_refused_while_the_account_is_not_opted_in() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    let request = json!({"s3DestinationConfig": {"bucket": BUCKET}});

    for case in ["before enrolling", "after opting back out"] {
        let (status, body) = call(&router, EXPORT, request.clone()).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected 403 while not opted in, got {status}: {body}"
        );
        assert!(
            error_code(&body).contains("OptInRequiredException"),
            "case {case:?}: expected OptInRequiredException, got {body}"
        );

        if case == "before enrolling" {
            // Enrolling is what admits the export; the store the refused call left alone is
            // still empty, and the export then records its one job.
            opt_in(&router).await;
            assert_eq!(
                reported_ids(&describe_ok(&router, case, json!({})).await),
                Vec::<String>::new(),
                "case {case:?}: a refused export records no job"
            );
            let job_id = export_job_id(&router, case).await;
            assert_eq!(
                reported_ids(&describe_ok(&router, case, json!({})).await),
                vec![job_id],
                "case {case:?}: the export recorded its job once the account was enrolled"
            );

            let (status, body) = call(
                &router,
                "UpdateEnrollmentStatus",
                json!({"status": "Inactive", "includeMemberAccounts": false}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: opting back out: got {status}: {body}"
            );
        }
    }
}

/// The same `EBSFilters` vocabulary gates the READER and the EXPORT: a filter either
/// operation admits, the other admits too, and one either refuses, the other refuses too.
#[tokio::test]
async fn the_export_and_the_reader_admit_the_same_filters() {
    // (case, the filters, whether both operations accept them)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("no filters at all", json!([]), true),
        (
            "the one filter name the model declares",
            json!([{"name": "Finding", "values": ["NotOptimized"]}]),
            true,
        ),
        (
            "both findings at once",
            json!([{"name": "Finding", "values": ["Optimized", "NotOptimized"]}]),
            true,
        ),
        (
            "a filter carrying no value, which selects nothing",
            json!([{"name": "Finding", "values": []}]),
            true,
        ),
        (
            "two filters of the same name",
            json!([{"name": "Finding", "values": ["Optimized"]},
                   {"name": "Finding", "values": ["NotOptimized"]}]),
            true,
        ),
        (
            "a finding the model does not declare",
            json!([{"name": "Finding", "values": ["Overprovisioned"]}]),
            false,
        ),
        (
            "a filter carrying no name",
            json!([{"values": ["Optimized"]}]),
            false,
        ),
    ];

    for (case, filters, accepted) in rows {
        // A fresh emulator per row: only one volume export job runs per Region at a time.
        let router = enrolled_emulator().await;
        let (read_status, read_body) = call(
            &router,
            "GetEBSVolumeRecommendations",
            json!({"filters": filters.clone()}),
        )
        .await;
        let (export_status, export_body) = call(
            &router,
            EXPORT,
            json!({"s3DestinationConfig": {"bucket": BUCKET}, "filters": filters.clone()}),
        )
        .await;
        let wanted = if accepted { 200 } else { 400 };
        assert_eq!(
            read_status, wanted,
            "case {case:?}: the reader answered {read_status} for {filters}: {read_body}"
        );
        assert_eq!(
            export_status, wanted,
            "case {case:?}: the export answered {export_status} for {filters}: {export_body}"
        );
        if !accepted {
            assert!(
                error_code(&read_body).contains("InvalidParameterValueException"),
                "case {case:?}: the reader refuses with InvalidParameterValueException, \
                 got {read_body}"
            );
            assert!(
                error_code(&export_body).contains("InvalidParameterValueException"),
                "case {case:?}: the export refuses with InvalidParameterValueException, \
                 got {export_body}"
            );
        }
    }
}

/// How a cell holding several values separates them, so one exportable field is still one
/// column.
const LIST_SEPARATOR: &str = "|";

/// The column a volume's own ARN lands in (`ExportableVolumeField$VolumeArn`).
const ARN_COLUMN: &str = "VolumeArn";

/// The column naming the baseline IOPS the exported recommendation option would provision,
/// which is what shows the look-back period in force reached the file: a longer period reveals
/// a higher peak, and the option has to carry it.
const OPTION_IOPS_COLUMN: &str = "RecommendationOptionsConfigurationVolumeBaselineIOPS";

/// Each member of a `VolumeConfiguration` the reader reports, with the `ExportableVolumeField`
/// suffix the column naming it carries and whether its value is a figure
/// (`smithy_query query="shape" shape="VolumeConfiguration,ExportableVolumeField"`). The
/// prefixes are the two the field set declares a configuration under:
/// `CurrentConfiguration…` for the volume's own and `RecommendationOptionsConfiguration…` for
/// the one its option would move it to.
const CONFIGURATION_MEMBERS: &[(&str, &str, bool)] = &[
    ("volumeType", "VolumeType", false),
    ("volumeSize", "VolumeSize", true),
    ("volumeBaselineIOPS", "VolumeBaselineIOPS", true),
    ("volumeBurstIOPS", "VolumeBurstIOPS", true),
    ("volumeBaselineThroughput", "VolumeBaselineThroughput", true),
    ("volumeBurstThroughput", "VolumeBurstThroughput", true),
];

/// The figure an export cell states for a value the reader reports unrounded: every derived
/// figure in a file is rounded to two decimal places, so a comparison against the reader rounds
/// its value the same way.
fn rounded(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The volume recommendations the READER reports, keyed by volume ARN — the oracle every
/// assertion about the exported file is made against.
async fn reported_volumes(
    router: &MultiServiceRouter,
    case: &str,
) -> BTreeMap<String, Value> {
    let request = json!({"maxResults": 100});
    let (status, body) = call(router, "GetEBSVolumeRecommendations", request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {request} answered {status}: {body}"
    );
    let recommendations = body["volumeRecommendations"].as_array().unwrap_or_else(|| {
        panic!("case {case:?}: the reader answers a recommendation list, got {body}")
    });
    assert!(
        !recommendations.is_empty(),
        "case {case:?}: the analysis holds volume recommendations, or the comparison proves \
         nothing, got {body}"
    );
    recommendations
        .iter()
        .map(|recommendation| {
            let arn = recommendation["volumeArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every recommendation carries volumeArn, got {recommendation}")
                })
                .to_string();
            (arn, recommendation.clone())
        })
        .collect()
}

/// Assert the cells of one configuration — the volume's own or its option's — against the
/// members the reader reports for it.
fn assert_configuration(
    case: &str,
    header: &[String],
    row: &[String],
    prefix: &str,
    reported: &Value,
) {
    for (member, suffix, is_figure) in CONFIGURATION_MEMBERS {
        let column = format!("{prefix}{suffix}");
        let value = &reported[*member];
        if *is_figure {
            let expected = value.as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: the reader reports {member} as a figure, got {reported}")
            });
            assert_figure(case, header, row, &column, expected);
        } else {
            let expected = value.as_str().unwrap_or_default();
            assert_eq!(
                cell(header, row, &column),
                expected,
                "case {case:?}: {column} must state the {member} the reader reports, got {row:?}"
            );
        }
    }
}

/// Every cell of a volume's exported row is the value `GetEBSVolumeRecommendations` reports for
/// the member that column names — the volume's own configuration, the utilization observed on
/// it, the one option the analysis has with its configuration, risk and savings, its tags, the
/// preferences the recommendation was generated under and when it was generated — so a file and
/// a read of the same analysis cannot disagree.
///
/// The table spans the look-back preference the rule generalises over: none at all, and each
/// `LookBackPeriodPreference` the model declares. A longer period analyses samples a shorter one
/// left out, so it changes the option a volume gets — which is what proves the exported row is
/// computed under the period in force rather than frozen at the default.
#[tokio::test]
async fn the_export_file_states_what_the_reader_reports_for_every_volume() {
    // (case, the `lookBackPeriod` the preference asks for, or none at all)
    let mut rows: Vec<(String, Option<&str>)> = vec![("no preference at all".to_string(), None)];
    rows.extend(
        LOOK_BACK_PERIOD_PREFERENCE_VALUES
            .iter()
            .map(|period| (format!("a {period} look-back preference"), Some(*period))),
    );

    // What each case exported for one volume's option, so the table is shown to prove that the
    // preference reaches the file.
    let mut option_iops: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for (case, look_back_period) in rows {
        // A fresh emulator per row: one volume export job runs per Region at a time, so each row
        // gets its own store rather than waiting on the previous row's job.
        let router = analysed_emulator().await;
        create_bucket(&router, BUCKET).await;
        if let Some(look_back_period) = look_back_period {
            put_preference(
                &router,
                &json!({"resourceType": RESOURCE_TYPE_EBS_VOLUME,
                        "lookBackPeriod": look_back_period}),
            )
            .await;
        }

        let reported = reported_volumes(&router, &case).await;
        let exported = exported_recommendation_rows(
            &router,
            &case,
            EXPORT,
            BUCKET,
            EXPORTABLE_VOLUME_FIELD_VALUES,
        )
        .await;
        let (header, data) = exported
            .split_first()
            .unwrap_or_else(|| panic!("case {case:?}: the export file carries a header row"));
        assert_eq!(
            header.len(),
            EXPORTABLE_VOLUME_FIELD_VALUES.len(),
            "case {case:?}: the header holds the field the request asked for, got {header:?}"
        );

        // The file names exactly the volumes the reader reports, one row each: this analysis
        // holds one recommendation option per volume, and each `RecommendationOptions*` field
        // names ONE option.
        let exported_arns: BTreeSet<&str> =
            data.iter().map(|row| cell(header, row, ARN_COLUMN)).collect();
        let reported_arns: BTreeSet<&str> = reported.keys().map(String::as_str).collect();
        assert_eq!(
            exported_arns, reported_arns,
            "case {case:?}: the export names the volumes the reader reports, got {exported:?}"
        );
        assert_eq!(
            data.len(),
            reported.len(),
            "case {case:?}: one row per reported volume, got {exported:?}"
        );

        for (arn, recommendation) in &reported {
            let row = data
                .iter()
                .find(|row| cell(header, row, ARN_COLUMN) == arn)
                .unwrap_or_else(|| panic!("case {case:?}: {arn} has a row, got {exported:?}"));

            for (column, reported_member) in [
                ("AccountId", "accountId"),
                ("Finding", "finding"),
                ("CurrentPerformanceRisk", "currentPerformanceRisk"),
            ] {
                let expected = recommendation[reported_member].as_str().unwrap_or_default();
                assert_eq!(
                    cell(header, row, column),
                    expected,
                    "case {case:?}: {column} of {arn} must state the {reported_member} the \
                     reader reports, got {row:?}"
                );
            }

            // The period analysed, and the preferences the recommendation was generated under.
            assert_figure(
                &case,
                header,
                row,
                "LookbackPeriodInDays",
                recommendation["lookBackPeriodInDays"]
                    .as_f64()
                    .unwrap_or_default(),
            );
            let preferences = &recommendation["effectiveRecommendationPreferences"];
            assert_eq!(
                cell(header, row, "EffectiveRecommendationPreferencesLookBackPeriod"),
                preferences["lookBackPeriod"].as_str().unwrap_or_default(),
                "case {case:?}: the row of {arn} states the look-back period the reader \
                 reports, got {row:?}"
            );
            assert_eq!(
                cell(
                    header,
                    row,
                    "EffectiveRecommendationPreferencesSavingsEstimationMode"
                ),
                preferences["savingsEstimationMode"]["source"]
                    .as_str()
                    .unwrap_or_default(),
                "case {case:?}: the row of {arn} states the savings estimation mode the reader \
                 reports, got {row:?}"
            );

            // The volume's own configuration, and whether it is a root volume — which the field
            // set names both beside that configuration and on its own.
            let current = &recommendation["currentConfiguration"];
            assert_configuration(&case, header, row, "CurrentConfiguration", current);
            let root_volume = current["rootVolume"].as_bool().unwrap_or_else(|| {
                panic!("case {case:?}: the reader reports rootVolume for {arn}, got {current}")
            });
            for column in ["RootVolume", "CurrentConfigurationRootVolume"] {
                assert_eq!(
                    cell(header, row, column),
                    root_volume.to_string(),
                    "case {case:?}: {column} of {arn} must state whether the reader reports it a \
                     root volume, got {row:?}"
                );
            }

            // The utilization observed on the volume: each metric under the field naming it and
            // the one statistic this API reports metrics with.
            let observed = metrics(&recommendation["utilizationMetrics"]);
            assert!(
                !observed.is_empty(),
                "case {case:?}: the reader reports utilization for {arn}, got {recommendation}"
            );
            for (metric, value) in &observed {
                let column = format!("UtilizationMetrics{metric}Maximum");
                assert!(
                    EXPORTABLE_VOLUME_FIELD_VALUES.contains(&column.as_str()),
                    "case {case:?}: {column} must be a field the model declares"
                );
                assert_figure(&case, header, row, &column, *value);
            }

            // The one option the analysis has: its configuration, the risk it runs and what
            // adopting it saves.
            let option = &recommendation["volumeRecommendationOptions"][0];
            assert!(
                option.is_object(),
                "case {case:?}: the reader reports an option for {arn}, got {recommendation}"
            );
            assert_configuration(
                &case,
                header,
                row,
                "RecommendationOptionsConfiguration",
                &option["configuration"],
            );
            assert_figure(
                &case,
                header,
                row,
                "RecommendationOptionsPerformanceRisk",
                rounded(option["performanceRisk"].as_f64().unwrap_or_default()),
            );
            let savings = &option["savingsOpportunity"];
            let percentage = savings["savingsOpportunityPercentage"]
                .as_f64()
                .unwrap_or_default();
            let value = savings["estimatedMonthlySavings"]["value"]
                .as_f64()
                .unwrap_or_default();
            let currency = savings["estimatedMonthlySavings"]["currency"]
                .as_str()
                .unwrap_or_default();
            let after = &option["savingsOpportunityAfterDiscounts"];
            for (column, expected) in [
                ("RecommendationOptionsSavingsOpportunityPercentage", percentage),
                ("RecommendationOptionsEstimatedMonthlySavingsValue", value),
                (
                    "RecommendationOptionsSavingsOpportunityAfterDiscountsPercentage",
                    after["savingsOpportunityPercentage"].as_f64().unwrap_or_default(),
                ),
                (
                    "RecommendationOptionsEstimatedMonthlySavingsValueAfterDiscounts",
                    after["estimatedMonthlySavings"]["value"].as_f64().unwrap_or_default(),
                ),
            ] {
                assert_figure(&case, header, row, column, expected);
            }
            for (column, expected) in [
                ("RecommendationOptionsEstimatedMonthlySavingsCurrency", currency),
                (
                    "RecommendationOptionsEstimatedMonthlySavingsCurrencyAfterDiscounts",
                    after["estimatedMonthlySavings"]["currency"]
                        .as_str()
                        .unwrap_or_default(),
                ),
            ] {
                assert_eq!(
                    cell(header, row, column),
                    expected,
                    "case {case:?}: {column} of {arn} must state the currency the reader \
                     reports, got {row:?}"
                );
            }

            // The two price columns name no member of a `VolumeRecommendation`, so they are
            // asserted against the saving the reader DOES report: what the volume costs today
            // less what its option costs is exactly that saving, and that saving is that
            // percentage of today's cost.
            let price = |column: &str| -> f64 {
                let held = cell(header, row, column);
                held.parse().unwrap_or_else(|_| {
                    panic!("case {case:?}: {column} of {arn} holds a price, got {held:?} in {row:?}")
                })
            };
            let current_price = price("CurrentMonthlyPrice");
            let option_price = price("RecommendationOptionsMonthlyPrice");
            assert!(
                current_price > 0.0 && option_price > 0.0,
                "case {case:?}: both price columns of {arn} state a cost, got {current_price} \
                 and {option_price} in {row:?}"
            );
            assert!(
                ((current_price - option_price).max(0.0) - value).abs() < 0.011,
                "case {case:?}: the prices of {arn} must agree with the {value} saving the \
                 reader reports, got {current_price} - {option_price} in {row:?}"
            );
            assert!(
                (rounded((current_price - option_price).max(0.0) / current_price * 100.0)
                    - percentage)
                    .abs()
                    < 0.011,
                "case {case:?}: the prices of {arn} must agree with the {percentage}% the \
                 reader reports, got {current_price} - {option_price} in {row:?}"
            );

            // The volume's tags, as the reader reports them, in one cell.
            let tags: Vec<String> = recommendation["tags"]
                .as_array()
                .map(|tags| {
                    tags.iter()
                        .map(|tag| {
                            format!(
                                "{}={}",
                                tag["key"].as_str().unwrap_or_default(),
                                tag["value"].as_str().unwrap_or_default()
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                cell(header, row, "Tags"),
                tags.join(LIST_SEPARATOR),
                "case {case:?}: the Tags cell of {arn} must state the tags the reader reports, \
                 got {row:?}"
            );

            // When the recommendation was generated, as the reader reports it. Compared as the
            // INSTANT each side states rather than as the text of it: the file states the
            // instant in RFC 3339 while `VolumeRecommendation$lastRefreshTimestamp` crosses the
            // wire as a JSON NUMBER, and an `f64` of a present-day instant carries less than a
            // microsecond, so the two renderings of ONE instant can land on neighbouring
            // microseconds. Both sides are therefore compared at the millisecond resolution an
            // AWS JSON timestamp states, as the sibling export suite of this crate compares the
            // same column ([`crate::idle_recommendations_export`]) — close enough that any
            // other instant, or no instant at all, still fails, and far enough that the
            // comparison is about the instant rather than about a rounding step neither end
            // chose.
            let refreshed = recommendation["lastRefreshTimestamp"]
                .as_f64()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the reader reports a refresh instant, got {recommendation}")
                });
            let held = cell(header, row, "LastRefreshTimestamp");
            let stated = DateTime::from_str(held, Format::DateTime).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: LastRefreshTimestamp of {arn} must state an instant, got \
                     {held:?} in {row:?}: {error}"
                )
            });
            assert!(
                (stated.as_secs_f64() - refreshed).abs() < 0.001,
                "case {case:?}: LastRefreshTimestamp of {arn} must state the instant the reader \
                 reports ({refreshed}), got {row:?}"
            );

            option_iops
                .entry(arn.clone())
                .or_default()
                .insert(case.clone(), cell(header, row, OPTION_IOPS_COLUMN).to_string());
        }
    }

    // The table has to span a volume whose option CHANGED with the look-back period, or it
    // proves nothing about the period reaching the file.
    assert!(
        option_iops.values().any(|per_case| {
            per_case.values().collect::<BTreeSet<&String>>().len() > 1
        }),
        "a longer look-back preference has to change the option a volume's row states, got \
         {option_iops:?}"
    );
}
