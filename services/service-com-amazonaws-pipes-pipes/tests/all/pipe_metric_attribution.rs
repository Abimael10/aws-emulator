// SPDX-License-Identifier: CC0-1.0

//! WHICH pipe a datapoint in `AWS/EventBridge/Pipes` belongs to, with several pipes of the
//! same account running at the same time — end to end over the wire, on the harness of
//! `tests/all/pipe_metrics.rs` (IAM, CloudWatch, Amazon SQS and Step Functions beside
//! pipes on ONE engine), reused rather than copied.
//!
//! `tests/all/pipe_metrics.rs` asserts WHAT a single running pipe publishes about itself.
//! The rule here is the other half, and it cannot be seen with one pipe in the account: a
//! per-pipe series is dimensioned on the pipe's name ALONE, so every datapoint is
//! attributed to the pipe whose execution produced it — a counter reads its own pipe's
//! executions, and reads 0 rather than nothing for a kind of execution that pipe never
//! had, however many other pipes are having them at that very moment.
//!
//! That is what tells two pipes on identical wiring apart: the one whose role may not
//! reach its target accumulates its stage counter while its twin's same counter exists and
//! holds at 0, and the two targets look identical (one holds the record, the other holds
//! nothing) only because the counters say which happened.
//!
//! Every wait here LOOKS AGAIN until the condition it needs holds, on a generous deadline,
//! and every step that depends on an earlier one waits for the EVIDENCE of that step rather
//! than for a span it is assumed to fit in: a counter that moved past what it read before
//! the record was sent is the pipe having taken a pass over it, and a refused poll is a
//! narrowed role having reached the pipe. A target expected to stay empty and a counter
//! expected to stay quiet are invariants, so each is looked at repeatedly across its span
//! instead of once at the end of one. Nothing here is asserted about elapsed time, so what
//! a row reports is what the pipes did and never how fast the host they ran on happened to
//! be.
//!
//! The rows differ along what the rule generalises over — which stage of the row's own pipe
//! fails (none, the target, the enrichment, the source) and how its role bars that stage
//! (a grant that omits the action, an explicit deny, a grant naming only the other
//! queue, a grant narrowed once the pipe is running) — so every counter of the set is moved
//! by at least one pipe of the table and quiet for at least one other.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use service_com_amazonaws_pipes_pipes::state::metrics::{
    COUNTER_UNIT, ENRICHMENT_STAGE_FAILED_METRIC, EVENT_COUNT_METRIC, EXECUTION_FAILED_METRIC,
    PIPE_COUNTERS, PIPE_NAME_DIMENSION, TARGET_STAGE_FAILED_METRIC,
};

use crate::pipe_data_plane::{
    collect_at_least, collect_during, given_express_workflow, given_queue, send, QUIET,
};
use crate::pipe_lifecycle::{call, REGION};
use crate::pipe_metrics::{
    datapoints, emulator, given_role_stating, listed_dimension_sets, quiet_counters_hold, series,
    wait_for_every_counter, wait_for_sum, withdraw_from_role,
};

/// How long a wait for something a pipe has to do — a delivery, or a counter moving — is
/// given before the row is a failure.
///
/// Every wait below LOOKS AGAIN until the condition it needs holds, so this bounds only how
/// long a row whose pipe never did the work takes to fail, and a row whose work the pipe did
/// spends none of it. It is therefore generous rather than tight: this table stands five
/// pipes up on one emulator, and a budget tuned to an idle host is what makes a test report
/// the host's load as the emulator's defect.
const BUDGET: Duration = Duration::from_secs(60);

/// An enrichment that hands back what it was given, for the row whose pipe carries one its
/// role may not invoke.
const PASS_THROUGH_DEFINITION: &str = r#"{
  "StartAt": "PassThrough",
  "States": {
    "PassThrough": {
      "Type": "Pass",
      "End": true
    }
  }
}"#;

/// One row: one pipe of the account, with its own queues, its own role and its own
/// expectation of what its counters read.
struct Row {
    /// The pipe's name, which is also the value of the one dimension its series carries.
    case: &'static str,
    /// The statements of the pipe's role, written against that pipe's OWN source and
    /// target.
    policy: fn(&str, &str) -> Value,
    /// Whether this pipe carries an enrichment between its source and its target.
    enrichment: bool,
    /// What this pipe's role is left allowing once the pipe is RUNNING, for a row whose
    /// stage must fail on a call the role allowed while the pipe was being brought up.
    withdrawn_to: Option<&'static [&'static str]>,
    /// Whether this pipe's target ends up holding the record.
    delivered: bool,
    /// The counters of THIS pipe expected to read at least 1. Every other counter of this
    /// pipe has to exist and read exactly 0, whatever the other pipes publish.
    moved: &'static [&'static str],
}

/// Every Amazon SQS call a pipe with an SQS source and an SQS target makes, allowed on
/// exactly the resource each is made on.
fn exact_sqs_grant(source: &str, target: &str) -> Value {
    json!([
        {
            "Effect": "Allow",
            "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage"],
            "Resource": source,
        },
        { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target },
    ])
}

/// The `CreatePipe` body of one row: the row's own queues, its own role, one record per
/// batch collected with no batching window, and the enrichment when the row has one.
fn pipe_body(
    source_arn: &str,
    target_arn: &str,
    role_arn: &str,
    enrichment: Option<&str>,
) -> Value {
    let mut body = json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": role_arn,
        "DesiredState": "RUNNING",
        "SourceParameters": {
            "SqsQueueParameters": { "BatchSize": 1, "MaximumBatchingWindowInSeconds": 0 },
        },
    });
    if let Some(enrichment) = enrichment {
        body["Enrichment"] = json!(enrichment);
    }
    body
}

/// What one row's pipe was given, kept so the assertions can read its target and look for
/// its own record.
struct Prepared<'row> {
    row: &'row Row,
    target_url: String,
    marker: String,
    /// What each counter of [`Row::moved`] read just BEFORE this pipe's source was written
    /// to, in that order — 0 for every row but the one whose role was narrowed while it
    /// ran, whose failure counter the narrowing itself has already driven. The waits below
    /// are for the pipe moving each counter FURTHER than this, so no row's wait is a
    /// condition that already held.
    before: Vec<f64>,
}

/// RULE: every datapoint a pipe publishes to `AWS/EventBridge/Pipes` is dimensioned on the
/// name of the pipe whose execution produced it and on nothing else, so each counter
/// accounts for its own pipe's executions only — and exists and reads 0 for a kind of
/// execution that pipe never had, while other pipes of the account are accumulating that
/// very counter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_pipes_counters_account_for_its_own_executions_only() {
    let rows = [
        Row {
            case: "attribution-every-stage-allowed",
            policy: exact_sqs_grant,
            enrichment: false,
            withdrawn_to: None,
            delivered: true,
            moved: &[EVENT_COUNT_METRIC],
        },
        Row {
            // The twin of the row above on identical wiring, whose role simply does not
            // state the delivery: the pipe runs, the target stage is refused, and the
            // record never arrives.
            case: "attribution-target-send-not-stated",
            policy: |source, _target| {
                json!([
                    {
                        "Effect": "Allow",
                        "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage"],
                        "Resource": source,
                    },
                ])
            },
            enrichment: false,
            withdrawn_to: None,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                TARGET_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            case: "attribution-target-send-denied",
            policy: |_source, target| {
                json!([
                    { "Effect": "Allow", "Action": "sqs:*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "sqs:SendMessage", "Resource": target },
                ])
            },
            enrichment: false,
            withdrawn_to: None,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                TARGET_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            // The enrichment is handed the records before the target is, so a role that
            // may not invoke it fails the pipe at THAT stage: the enrichment counter
            // moves and the target counter stays at 0.
            case: "attribution-enrichment-invoke-not-stated",
            policy: exact_sqs_grant,
            enrichment: true,
            withdrawn_to: None,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                ENRICHMENT_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            // The source poll is the first call the pipe makes, so a role that may not
            // make it takes no record at all: the execution failed before a stage of its
            // own, and this pipe's event counter stays at 0 while its twins' rise. The
            // call is withdrawn from the role once the pipe is RUNNING, because a pipe
            // whose role may not take its source's records when it is brought up is never
            // brought up (`tests/all/pipe_bring_up.rs`) and publishes nothing at all.
            case: "attribution-source-receive-withdrawn-while-running",
            policy: |source, target| {
                json!([
                    { "Effect": "Allow", "Action": "sqs:*", "Resource": source },
                    { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target },
                ])
            },
            enrichment: false,
            withdrawn_to: Some(&["sqs:SendMessage"]),
            delivered: false,
            moved: &[EXECUTION_FAILED_METRIC],
        },
    ];

    // Every row's pipe is created on ONE emulator and every source then written to, so all
    // of them are running and publishing at the same time — which is what makes "this
    // counter is 0" a statement about attribution rather than about an idle account.
    let router = emulator();
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for row in &rows {
        let (source_url, source_arn) = given_queue(&router, &format!("{}-source", row.case)).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{}-target", row.case)).await;
        let role_arn = given_role_stating(
            &router,
            &format!("{}-role", row.case),
            SERVICE_PRINCIPAL,
            (row.policy)(&source_arn, &target_arn),
        )
        .await;
        let enrichment = if row.enrichment {
            Some(
                given_express_workflow(
                    &router,
                    &format!("{}-enrichment", row.case),
                    PASS_THROUGH_DEFINITION,
                )
                .await,
            )
        } else {
            None
        };
        let created = call(
            &router,
            "POST",
            &format!("/v1/pipes/{}", row.case),
            Some(pipe_body(
                &source_arn,
                &target_arn,
                &role_arn,
                enrichment.as_deref(),
            )),
            REGION,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe answered {} {}",
            row.case, created.status, created.body
        );
        // A row whose stage must fail on a call its role allowed while the pipe was being
        // brought up waits for the pipe to be RUNNING and then narrows the role, so the
        // refusal reaches a pipe that is already running rather than its creation.
        if let Some(allowed) = row.withdrawn_to {
            let reported = crate::pipe_execution_role::wait_for_state(
                &router,
                row.case,
                service_com_amazonaws_pipes_pipes::state::pipe_state::RUNNING,
                BUDGET,
            )
            .await;
            assert_eq!(
                reported,
                service_com_amazonaws_pipes_pipes::state::pipe_state::RUNNING,
                "case {:?}: expected the pipe RUNNING before its role is narrowed, it \
                 reports {reported}",
                row.case
            );
            withdraw_from_role(&router, row.case, &format!("{}-role", row.case), allowed).await;
            // And the record is sent only once the pipe has ALREADY been refused a poll
            // under the narrowed role, which its own failure counter is the report of. The
            // poller takes one pass at a time, so a pass that was refused is a pass no
            // earlier poll of that source outlived: the record cannot then be taken by a
            // receive the wider role authorized. Waiting for that refusal — rather than
            // sending straight away and trusting the narrowing to land first — is what
            // keeps the row from turning on how fast the host runs.
            let refused = wait_for_sum(&router, row.case, EXECUTION_FAILED_METRIC, 1.0).await;
            assert!(
                refused >= 1.0,
                "case {:?}: expected the pipe to be refused a source poll once its role no \
                 longer allows one, its {EXECUTION_FAILED_METRIC} reads {refused}",
                row.case
            );
        }
        // What each counter this row expects to move reads before its source is written to,
        // so the waits below are for the pipe moving them FURTHER than that.
        let mut before: Vec<f64> = Vec::with_capacity(row.moved.len());
        for metric in row.moved {
            let (_, sum) = series(&router, row.case, metric).await;
            before.push(sum);
        }
        let marker = format!("{}-probe", row.case);
        send(&router, &source_url, std::slice::from_ref(&marker)).await;
        prepared.push(Prepared {
            row,
            target_url,
            marker,
            before,
        });
    }

    // First every counter the rows expect to MOVE: each is waited for past what it read
    // before that pipe's source was written to, which is the EVIDENCE that the pipe took a
    // pass over the record it was sent. Every read below is then about pipes that have done
    // their work rather than about ones the host has not got round to — and each "reads 0"
    // is asserted at an instant when other pipes of the same account have already
    // accumulated that same counter.
    for entry in &prepared {
        for (metric, read_before) in entry.row.moved.iter().zip(&entry.before) {
            let want = read_before + 1.0;
            let sum = wait_for_sum(&router, entry.row.case, metric, want).await;
            assert!(
                sum >= want,
                "case {:?}: expected {metric} to read at least {want} once the pipe has taken \
                 a pass over the record it was sent (it read {read_before} before that), got \
                 {sum}",
                entry.row.case
            );
        }
    }

    // What a client sees at each target: its own record, or nothing at all. A pipe whose
    // stage was refused has already published that refusal above, so its target is watched
    // for the whole of a quiet window — looked at again and again across it — rather than
    // read once after a span the delivery was assumed to fit in.
    for entry in &prepared {
        if entry.row.delivered {
            let delivered = collect_at_least(&router, &entry.target_url, 1, BUDGET).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {:?}: expected the target to hold the one record this pipe moved, got {delivered:?}",
                entry.row.case
            );
            assert!(
                delivered[0].contains(&entry.marker),
                "case {:?}: expected the delivered record to carry this pipe's own probe {:?}, got {:?}",
                entry.row.case,
                entry.marker,
                delivered[0]
            );
        } else {
            let delivered = collect_during(&router, &entry.target_url, QUIET).await;
            assert!(
                delivered.is_empty(),
                "case {:?}: expected the target of a pipe whose stage was refused to hold \
                 nothing, got {delivered:?}",
                entry.row.case
            );
        }
    }

    // Then every counter of every pipe: the ones this pipe never had EXIST and read
    // exactly 0, and each datapoint of each counter carries the unit the namespace
    // publishes in. A counter exists once the pipe has opened a window, which is waited
    // for rather than assumed to have happened by now.
    for entry in &prepared {
        let absent = wait_for_every_counter(&router, entry.row.case).await;
        assert!(
            absent.is_none(),
            "case {:?}: expected every counter of a running pipe to have a datapoint, \
             {absent:?} had none",
            entry.row.case
        );
        for metric in PIPE_COUNTERS {
            let points = datapoints(&router, entry.row.case, metric).await;
            assert!(
                !points.is_empty(),
                "case {:?}: expected the counter {metric} of a running pipe to have a \
                 datapoint, it had none",
                entry.row.case
            );
            for point in &points {
                assert_eq!(
                    point["Unit"],
                    json!(COUNTER_UNIT),
                    "case {:?}: expected {metric} to be published in {COUNTER_UNIT}, got {point}",
                    entry.row.case
                );
            }
            if entry.row.moved.contains(metric) {
                continue;
            }
            let sum = points
                .iter()
                .filter_map(|point| point["Sum"].as_f64())
                .sum::<f64>();
            assert_eq!(
                sum, 0.0,
                "case {:?}: this pipe had no {metric} of its own, so its series reads 0 \
                 whatever the other pipes publish, got {sum}",
                entry.row.case
            );
        }
    }

    // And it HOLDS at 0 for the whole window, while the pipes that are failing go on
    // failing: a counter of one pipe never takes another pipe's execution. That is an
    // invariant, so each quiet counter is LOOKED AT again and again across the span
    // instead of once at the end of a sleep — a counter that moves anywhere in the span
    // fails its row.
    for entry in &prepared {
        quiet_counters_hold(&router, entry.row.case, entry.row.moved, QUIET).await;
    }

    // The catalogue a dashboard builds from: one series per pipe per counter, each
    // dimensioned on the pipe's name and nothing else.
    for metric in PIPE_COUNTERS {
        let sets = listed_dimension_sets(&router, metric).await;
        for set in &sets {
            let dimensions = set.as_array().cloned().unwrap_or_default();
            assert_eq!(
                dimensions.len(),
                1,
                "metric {metric}: expected every per-pipe series to carry the one \
                 {PIPE_NAME_DIMENSION} dimension, got {set}"
            );
            assert_eq!(
                dimensions[0]["Name"],
                json!(PIPE_NAME_DIMENSION),
                "metric {metric}: expected the one dimension to be {PIPE_NAME_DIMENSION}, \
                 got {set}"
            );
        }
        for row in &rows {
            let want = json!([{ "Name": PIPE_NAME_DIMENSION, "Value": row.case }]);
            let found = sets.iter().filter(|set| **set == want).count();
            assert_eq!(
                found, 1,
                "case {:?}: expected ListMetrics to catalogue exactly one {metric} series \
                 for this pipe, found {found} among {sets:?}",
                row.case
            );
        }
    }
}
