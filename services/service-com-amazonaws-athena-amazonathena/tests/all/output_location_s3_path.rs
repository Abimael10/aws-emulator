// SPDX-License-Identifier: CC0-1.0

//! The results location a submission resolves has to be an AMAZON S3 PATH.
//!
//! The rule under test: a query's results are written as Amazon S3 objects
//! (`ResultConfiguration$OutputLocation` — "The location in Amazon S3 where your query and
//! calculation results are stored, such as `s3://path/to/query/bucket/`"), so
//! `StartQueryExecution` validates the location it resolved for the results — the one the
//! request names, or the workgroup's where the request names none — BEFORE it queues the
//! query, and refuses one that names no `s3://` path at all with HTTP 400
//! `InvalidRequestException`, class code `INVALID_INPUT` and the wholly fixed sentence
//! `outputLocation is not a valid S3 path.`, minting no execution.
//!
//! A location of no characters is a different condition, and keeps its own sentence: the
//! request named NO location, which is what the missing-location refusal states.
//!
//! The suite is a TABLE whose rows differ along HOW the location fails to be an Amazon S3
//! path (no scheme, a filesystem path, another scheme, the scheme without a bucket), where
//! the location came from (the request, the workgroup behind a request that names none), and
//! — on the accepted rows — the depth of the prefix under the bucket.
//!
//! Every call goes over the wire the production front door uses.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
};

use crate::harness::{assert_refusal, call, history, router};

/// The sentence the refusal must carry word for word, pinned here rather than imported:
/// wholly fixed text that interpolates nothing from the request.
const NOT_A_VALID_S3_PATH: &str = "outputLocation is not a valid S3 path.";

/// The sentence a submission that names NO location at all is refused with, which is the
/// other condition and keeps its own words. Wholly fixed text, pinned here as the service
/// states it.
const NO_OUTPUT_LOCATION: &str = concat!(
    "No output location provided. You did not provide an output location for  your query ",
    "results. Either specify an S3 bucket location or enable Athena managed query results ",
    "in your workgroup settings."
);

/// The bucket the harness holds for this suite's results, which the accepted rows name.
const HELD_BUCKET: &str = "athena-results";

/// The workgroup whose OWN results location is no Amazon S3 path, for the row whose
/// location the workgroup supplies.
const WORK_GROUP_WITH_A_BAD_LOCATION: &str = "results-off-s3";

/// One row: the result location a submission resolves, and what the submission must answer.
struct Row {
    /// What this row is.
    case: &'static str,
    /// The `StartQueryExecution` request, less the statement.
    request: Value,
    /// The sentence the refusal must carry word for word, or `None` for a submission that
    /// must be accepted.
    refused: Option<&'static str>,
}

/// A submission whose resolved result location is no Amazon S3 path is refused with the
/// service's own fixed sentence and records no execution; one that names such a path is
/// accepted.
#[tokio::test]
async fn a_submission_whose_result_location_is_no_s3_path_is_refused() {
    let router = router();
    // A workgroup carrying a results location that is no Amazon S3 path, so the rule is seen
    // to hold on the location the submission RESOLVED and not only on the one it sent.
    let (created, body) = call(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP_WITH_A_BAD_LOCATION,
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": "/var/athena/results/" }
            },
        }),
    )
    .await;
    assert_eq!(
        created, 200,
        "setup: the workgroup must be created, got {created} with {body:?}"
    );

    let rows: Vec<Row> = vec![
        Row {
            case: "a location with no scheme at all",
            request: json!({
                "ResultConfiguration": { "OutputLocation": "athena-results/queries/" }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "an absolute filesystem path",
            request: json!({
                "ResultConfiguration": { "OutputLocation": "/var/athena/results/" }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "the bucket's https address",
            request: json!({
                "ResultConfiguration": {
                    "OutputLocation": "https://athena-results.s3.us-east-1.amazonaws.com/queries/"
                }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "a scheme that is not Amazon S3's",
            request: json!({
                "ResultConfiguration": { "OutputLocation": "file:///var/athena/results/" }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "the Amazon S3 scheme with no bucket after it",
            request: json!({ "ResultConfiguration": { "OutputLocation": "s3://" } }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "the Amazon S3 scheme followed by a prefix and no bucket",
            request: json!({ "ResultConfiguration": { "OutputLocation": "s3:///queries/" } }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "the Amazon S3 scheme written with one separator",
            request: json!({
                "ResultConfiguration": { "OutputLocation": "s3:/athena-results/queries/" }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "a location the workgroup supplies that is no Amazon S3 path",
            request: json!({ "WorkGroup": WORK_GROUP_WITH_A_BAD_LOCATION }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        // The boundary of the rule: no location at all is the OTHER condition, stated in its
        // own sentence rather than this one.
        Row {
            case: "a location of no characters",
            request: json!({ "ResultConfiguration": { "OutputLocation": "" } }),
            refused: Some(NO_OUTPUT_LOCATION),
        },
        Row {
            case: "an Amazon S3 path padded with whitespace",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!(" s3://{HELD_BUCKET}/padded/") }
            }),
            refused: Some(NOT_A_VALID_S3_PATH),
        },
        Row {
            case: "an Amazon S3 path under a prefix",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{HELD_BUCKET}/queries/") }
            }),
            refused: None,
        },
        Row {
            case: "an Amazon S3 path naming the bucket alone",
            request: json!({
                "ResultConfiguration": { "OutputLocation": format!("s3://{HELD_BUCKET}") }
            }),
            refused: None,
        },
        Row {
            case: "an Amazon S3 path under a deeper prefix",
            request: json!({
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{HELD_BUCKET}/year/2026/month/01/")
                }
            }),
            refused: None,
        },
    ];

    for row in &rows {
        let before = history(&router, DEFAULT_WORK_GROUP).await.len();
        let mut request = row.request.clone();
        request["QueryString"] = json!("SELECT 1 AS n");
        let (status, answer) = call(&router, "StartQueryExecution", &request).await;
        match row.refused {
            Some(expected) => {
                assert_refusal(
                    row.case,
                    status,
                    &answer,
                    ERROR_CODE_INVALID_INPUT,
                    expected,
                );
                let after = history(&router, DEFAULT_WORK_GROUP).await.len();
                assert_eq!(
                    after, before,
                    "case {:?}: a refused submission records no execution, got {after} where \
                     there were {before}",
                    row.case
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the submission must be accepted, got {status} with {answer:?}",
                    row.case
                );
                assert!(
                    answer["QueryExecutionId"].is_string(),
                    "case {:?}: an accepted submission answers with an id, got {answer:?}",
                    row.case
                );
            }
        }
    }
}
