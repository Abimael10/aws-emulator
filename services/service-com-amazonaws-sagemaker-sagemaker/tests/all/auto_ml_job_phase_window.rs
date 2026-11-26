// SPDX-License-Identifier: CC0-1.0

//! WHAT A CLIENT SEES of an AutoML job V2's work: whether the `AutoMLJobSecondaryStatus` stages
//! the job walks while it is `InProgress` are each in effect for a window of their own for a poll
//! loop to land inside.
//!
//! RULE — the same one [`crate::training_job_phase_window`] holds for a training job, applied to
//! the other lifecycle of this service whose model documents a non-terminal status as an ORDERED
//! SEQUENCE of sub-stages: each stage is revealed for a whole span of its own, and those spans
//! are SHARES of the one compressed span the clock owes the whole walk rather than spans sized on
//! their own. `DescribeAutoMLJobV2` reports `AutoMLJobSecondaryStatus` so that a client can tell
//! how far an Autopilot job has got — the data analysed, the features engineered, the model
//! tuned, the candidate definitions written — and the only way it finds that out is by POLLING,
//! so no stage may be in effect for no time at all; and a client chains this lifecycle with the
//! ones it feeds inside one budget of its own, so the walk of those stages is over inside a small
//! part of such a budget ([`crate::deploy_chain_settles_for_a_client`] holds that ceiling).
//!
//! The table's rows differ along what the rule generalises over: how the channel's location is
//! addressed and whether the request names its problem type and objective or leaves the run to
//! resolve both from the data — so the walk is asserted over runs that do different amounts of
//! work before each stage falls due.
//!
//! Nothing here waits for a fixed time outside a poll loop and nothing asserts on elapsed wall
//! clock: the poll looks again until the jobs are terminal, under a deadline far longer than any
//! span this state gives an AutoML job's work.
//!
//! Driven over the emulator's own Amazon S3 and IAM mounted beside SageMaker on ONE shared
//! engine, on the harness [`crate::training_job_execution`] builds for this target.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{csv_records, read_and_write, str_field, Emulator, BUCKET};

/// The cadence the poller below reads `DescribeAutoMLJobV2` on.
///
/// A fraction of the span the state gives ONE stage of an AutoML job's work, read from that span
/// rather than restated here, so a poller looks several times inside every stage the clock lays
/// out. It is a CADENCE, not a timeout: nothing below fails because a span elapsed, only because
/// the service answered the wrong thing.
const POLL_CADENCE: Duration = match STAGE_SPAN.checked_div(4) {
    Some(cadence) => cadence,
    None => STAGE_SPAN,
};

/// How long the clock keeps an AutoML job in each stage of its work, out of the crate's own state
/// rather than restated here.
const STAGE_SPAN: Duration =
    service_com_amazonaws_sagemaker_sagemaker::state::AUTO_ML_JOB_PHASE_SPAN;

/// How long the poll loop keeps looking before the suite calls the emulator broken. Far longer
/// than the whole walk of a job's work (`state::AUTO_ML_JOB_TERMINAL_AFTER`), so it bounds a
/// lifecycle that never settles rather than the condition a row waits for.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The `AutoMLJobSecondaryStatus` members that mean the job's own WORK is under way — the
/// stages a client polls for to learn how far an Autopilot job has got
/// (smithy_query query=shape shape=AutoMLJobSecondaryStatus, the members between `Starting` and
/// the terminal ones).
const WORK_IN_PROGRESS: [&str; 4] = [
    "AnalyzingData",
    "FeatureEngineering",
    "ModelTuning",
    "CandidateDefinitionsGenerated",
];

/// The `AutoMLJobStatus` members nothing leaves on its own
/// (smithy_query query=shape shape=AutoMLJobStatus).
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// One row of the table: the job's name, the object its channel reads and whether the request
/// names its problem type and objective.
struct Row {
    case: &'static str,
    name: &'static str,
    /// (key, body) of the object the row's channel is staged from.
    object: (String, String),
    /// The `S3DataType` the channel addresses its location as.
    data_type: &'static str,
    /// Whether the request names `ProblemType` and `AutoMLJobObjective` itself.
    named: bool,
}

/// The table of the RULE: every stage of the walk has a window of its own whatever the run's own
/// work before it is.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one object, the problem type and objective named",
            name: "automlwindownamed",
            object: (
                "automl-window/named/data.csv".to_string(),
                csv_records(40, 3),
            ),
            data_type: "S3Prefix",
            named: true,
        },
        Row {
            case: "a prefix of records, the problem type resolved from the data",
            name: "automlwindowresolved",
            object: (
                "automl-window/resolved/data.csv".to_string(),
                csv_records(24, 2),
            ),
            data_type: "S3Prefix",
            named: false,
        },
    ]
}

/// A `CreateAutoMLJobV2` body for `row`: one tabular training channel over the object it staged.
fn create_body(row: &Row, role_arn: &str) -> Value {
    let mut tabular = json!({ "TargetAttributeName": "y" });
    if row.named {
        tabular["ProblemType"] = json!("Regression");
    }
    let mut body = json!({
        "AutoMLJobName": row.name,
        "AutoMLJobInputDataConfig": [{
            "ChannelType": "training",
            "ContentType": "text/csv;header=present",
            "DataSource": {
                "S3DataSource": {
                    "S3DataType": row.data_type,
                    "S3Uri": format!("s3://{BUCKET}/{}", row.object.0),
                },
            },
        }],
        "OutputDataConfig": { "S3OutputPath": format!("s3://{BUCKET}/automl-window/{}/out/", row.name) },
        "AutoMLProblemTypeConfig": { "TabularJobConfig": tabular },
        "RoleArn": role_arn,
    });
    if row.named {
        body["AutoMLJobObjective"] = json!({ "MetricName": "MSE" });
    }
    body
}

/// RULE: a client polling inside a stage's span observes one of the stages of the job's work
/// while the job is still `InProgress`.
#[tokio::test]
async fn a_poller_on_a_client_cadence_observes_a_stage_of_the_work() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role_arn = emulator
        .given_role("automl-window-role", Some(&read_and_write()))
        .await;

    let rows = rows();
    // Every job is accepted before anything is polled, so one poll loop at a client's cadence
    // walks all of them: the rows share the loop instead of each paying its own wait.
    for row in &rows {
        emulator
            .given_object(BUCKET, &row.object.0, &row.object.1)
            .await;
        emulator
            .sagemaker_ok("CreateAutoMLJobV2", &create_body(row, &role_arn))
            .await;
    }

    let mut seen: Vec<Vec<(String, String)>> = rows.iter().map(|_| Vec::new()).collect();
    let mut settled: Vec<Option<Value>> = rows.iter().map(|_| None).collect();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    while settled.iter().any(Option::is_none) {
        for (index, row) in rows.iter().enumerate() {
            if settled[index].is_some() {
                continue;
            }
            let read = emulator
                .sagemaker_ok("DescribeAutoMLJobV2", &json!({ "AutoMLJobName": row.name }))
                .await;
            let status = str_field(&read, "AutoMLJobStatus");
            seen[index].push((status.clone(), str_field(&read, "AutoMLJobSecondaryStatus")));
            if TERMINAL.contains(&status.as_str()) {
                settled[index] = Some(read);
            }
        }
        if settled.iter().all(Option::is_some) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"the poll loop\": every job must reach a terminal status, having seen {seen:?}"
        );
        tokio::time::sleep(POLL_CADENCE).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let reads = &seen[index];
        let read = settled[index]
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the poll loop left no terminal read"));
        assert!(
            reads.iter().any(|(status, secondary)| {
                status == "InProgress" && WORK_IN_PROGRESS.contains(&secondary.as_str())
            }),
            "case {case:?}: a client polling every {POLL_CADENCE:?} must find the job in one of \
             {WORK_IN_PROGRESS:?} while it is still InProgress, got the reads {reads:?} and the \
             terminal read {read}"
        );
        assert_eq!(
            (
                str_field(&read, "AutoMLJobStatus"),
                str_field(&read, "AutoMLJobSecondaryStatus"),
            ),
            ("Completed".to_string(), "Completed".to_string()),
            "case {case:?}: a job whose work succeeded ends Completed/Completed, got {read}"
        );
    }
}
