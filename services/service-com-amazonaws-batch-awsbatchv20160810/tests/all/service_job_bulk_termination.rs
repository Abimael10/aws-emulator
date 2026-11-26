// SPDX-License-Identifier: CC0-1.0

//! The BULK service-job termination held against the SINGULAR one it is the bulk form of,
//! end to end over the wire.
//!
//! RULE under test: `TerminateServiceJobs` is "a bulk version of TerminateServiceJob" that
//! "reports the result for each service job individually in the response" — HTTP 200 with
//! every service job the request named in `successful`, an id no service job answers to
//! included ("Service jobs that can't be found are treated as successfully processed"), and
//! an empty `errors` — and what one such call DOES to this service's state for a list of ids
//! is exactly what one `TerminateServiceJob` call per id does, the capacity each ended job
//! was holding included: a service job resting for want of that capacity is placed once the
//! ids holding it have been ended, and not before.
//!
//! The rows span what the rule generalises over: the number of service jobs one request ends
//! (one, several), whether the list also names ids this service never minted or names one job
//! twice, the alphabet and size of the `reason`, and — the axis that makes this a differential
//! suite — WHICH form ends them, the bulk operation or one singular call per id. Each bulk row
//! is PAIRED with a singular row carrying the same jobs, the same list and the same reason, and
//! the two must leave every service job in the same state, member for member.
//!
//! Capacity is what makes the two forms comparable beyond the per-job report: a placed service
//! job holds the capacity its request asked for against the limits its queue's service
//! environments publish, so a second job asking for the whole published capacity RESTS in
//! `RUNNABLE` until the holders end. SageMaker AI is deliberately not mounted here (as in
//! `service_job_capacity_placement`), so a placed job's dispatch reaches nothing and it keeps
//! holding its capacity in `RUNNABLE` until a termination gives it back — which is exactly the
//! condition this suite needs.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{error_code, Batch};
use crate::service_job_capacity_placement::{
    environment_and_queue, is_placed, polled_until_placed, polled_until_status, submit,
};
use crate::service_job_termination::{describe, described, terminate, terminate_many};

/// An instance type a `ResourceConfig` can ask for: the example the `CapacityLimit$capacityUnit`
/// doc itself names.
const INSTANCE_TYPE: &str = "ml.m5.large";

/// The unit every environment below publishes its maximum in
/// (`CapacityLimit$capacityUnit`: "Otherwise, use `NUM_INSTANCES`").
const NUM_INSTANCES: &str = "NUM_INSTANCES";

/// How often a poll looks again. Only the loop's condition decides when it stops; the interval
/// is just how much work the wait costs, so it is kept coarse.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the resting waiters are watched for a move they must not make — several times the
/// span a service job's next move takes, so a move that was coming would have happened.
const RESTING_WINDOW: Duration = Duration::from_secs(5);

/// The members two differently-named service jobs in two differently-named queues cannot be
/// expected to agree on: the ids and names this service minted for each of them, and the
/// instants each was created and ended at. EVERYTHING else a `DescribeServiceJob` answer
/// carries must agree between a paired bulk row and singular row — that is the rule.
const PER_JOB_MEMBERS: &[&str] = &[
    "jobId",
    "jobArn",
    "jobName",
    "jobQueue",
    "createdAt",
    "scheduledAt",
    "startedAt",
    "stoppedAt",
];

/// Which operation ends a row's service jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// One `TerminateServiceJobs` call naming the whole list.
    Bulk,
    /// One `TerminateServiceJob` call per entry of that same list.
    Singular,
}

/// One row: a queue whose published capacity is fully held by `holders` service jobs, one more
/// job resting for want of it, and the list of ids a termination of that capacity names.
struct Row {
    /// What this row varies; named in every assertion.
    label: &'static str,
    /// The suffix this row's environment, queue and jobs are named with.
    suffix: &'static str,
    form: Form,
    /// How many service jobs hold the environment's whole published capacity between them.
    holders: usize,
    /// Ids this service never minted, which the request names beside the holders.
    unknown: &'static [&'static str],
    /// Whether the list names its first holder a second time.
    repeats_first: bool,
    reason: &'static str,
    /// The `label` of the row this one must end up identical to, when it has a pair.
    pair: Option<&'static str>,
}

/// The table. Each bulk row names the singular row it must match.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "one holder, ended by one singular call per id",
            suffix: "sing-one",
            form: Form::Singular,
            holders: 1,
            unknown: &["no-such-service-job", "00000000-0000-4000-8000-000000000000"],
            repeats_first: true,
            reason: "stop it",
            pair: None,
        },
        Row {
            label: "one holder, ended by one bulk call",
            suffix: "bulk-one",
            form: Form::Bulk,
            holders: 1,
            unknown: &["no-such-service-job", "00000000-0000-4000-8000-000000000000"],
            repeats_first: true,
            reason: "stop it",
            pair: Some("one holder, ended by one singular call per id"),
        },
        Row {
            label: "three holders, ended by one singular call per id",
            suffix: "sing-three",
            form: Form::Singular,
            holders: 3,
            unknown: &[],
            repeats_first: false,
            reason: "arrêt immédiat ✋ 停止",
            pair: None,
        },
        Row {
            label: "three holders, ended by one bulk call",
            suffix: "bulk-three",
            form: Form::Bulk,
            holders: 3,
            unknown: &[],
            repeats_first: false,
            reason: "arrêt immédiat ✋ 停止",
            pair: Some("three holders, ended by one singular call per id"),
        },
    ]
}

/// What a `SAGEMAKER_TRAINING` request asking for `instances` instances of one type looks
/// like: the `ResourceConfig` is the capacity a service job's placement is weighed against.
fn payload(instances: usize) -> String {
    json!({
        "ResourceConfig": {"InstanceType": INSTANCE_TYPE, "InstanceCount": instances},
    })
    .to_string()
}

/// The capacity limits an environment holding `max_capacity` instances publishes.
fn published_capacity(max_capacity: usize) -> Value {
    json!([{"maxCapacity": max_capacity, "capacityUnit": NUM_INSTANCES}])
}

/// `detail` with the members only one of two separately-minted jobs can carry removed
/// ([`PER_JOB_MEMBERS`]), so two of these compare everything else the answer reports.
///
/// Not the `untouched` projection of `service_job_termination`, which removes what a
/// termination CHANGES: here those members — the status, the reason and `isTerminated` — are
/// exactly what the two forms must agree on, and what differs is who each job is.
fn comparable(detail: &Value) -> Value {
    let mut copy = detail.clone();
    if let Some(object) = copy.as_object_mut() {
        for member in PER_JOB_MEMBERS {
            object.remove(*member);
        }
    }
    copy
}

/// Assert that `job_id` is NOT placed, now and for `window` longer — read through the service
/// each time, so the dispatch the clock owes is driven rather than waited out. A `window` of
/// zero reads it once.
async fn rests_unplaced(batch: &Batch, case: &str, job_id: &str, window: Duration) {
    let settled = Instant::now() + window;
    loop {
        let detail = described(batch, case, job_id).await;
        assert!(
            !is_placed(batch, job_id),
            "case {case}: the capacity it asked for is still held, so it must not be placed, \
             got {detail}"
        );
        if Instant::now() >= settled {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one row set up: the ids holding its capacity, the job resting for want of it, and the
/// list of ids its termination names.
struct Prepared {
    /// The queue the row's jobs were submitted to.
    queue: String,
    holders: Vec<String>,
    waiter: String,
    listed: Vec<String>,
    /// The ids `successful` must name, in order: every DISTINCT id the list carries.
    expected: Vec<String>,
}

/// RULE: one `TerminateServiceJobs` call over a list of service job ids does exactly what one
/// `TerminateServiceJob` call per id does — each id the store holds a live service job for
/// ends `FAILED`, terminated, carrying the caller's reason, and the capacity each ended job was
/// holding is given back, so a service job that was resting for want of it is placed — while
/// the call itself answers 200 reporting every distinct id it named in `successful` (an id no
/// service job answers to included) and an empty `errors`.
#[tokio::test]
async fn bulk_termination_does_what_one_singular_call_per_id_does() {
    let batch = Batch::new();
    let table = rows();

    // Every row's HOLDERS are submitted first, so the polls below cover them all at once. Each
    // row has its own environment and queue, so the capacity one row's jobs hold is not
    // capacity another row's jobs could have been placed on.
    let mut prepared: Vec<Prepared> = Vec::new();
    for row in &table {
        let queue =
            environment_and_queue(&batch, row.suffix, published_capacity(row.holders)).await;
        let mut holders: Vec<String> = Vec::new();
        for index in 0..row.holders {
            // One instance each, so the holders together hold exactly what the environment
            // publishes.
            let suffix = format!("{}-holder-{index}", row.suffix);
            holders.push(submit(&batch, &suffix, &queue, &payload(1)).await);
        }

        let mut listed: Vec<String> = holders.clone();
        listed.extend(row.unknown.iter().map(|id| (*id).to_string()));
        let expected = listed.clone();
        if row.repeats_first {
            listed.push(holders[0].clone());
        }
        prepared.push(Prepared {
            queue,
            holders,
            // Filled in once the holders have taken the capacity.
            waiter: String::new(),
            listed,
            expected,
        });
    }

    // The holders take the whole published capacity.
    for (row, row_state) in table.iter().zip(&prepared) {
        for holder in &row_state.holders {
            polled_until_placed(&batch, row.label, holder).await;
        }
    }

    // Only now is each row's waiter submitted: it asks for the whole published capacity, which
    // the holders already have, so what it is waiting for is unambiguous — nothing but a
    // termination can give it that capacity, and no scheduling order could have handed it the
    // capacity a holder was meant to take.
    for (row, row_state) in table.iter().zip(prepared.iter_mut()) {
        row_state.waiter = submit(
            &batch,
            &format!("{}-waiter", row.suffix),
            &row_state.queue,
            &payload(row.holders),
        )
        .await;
    }

    // Each waiter reaches the status a service job waits in, and rests there unplaced.
    for (row, row_state) in table.iter().zip(&prepared) {
        polled_until_status(
            &batch,
            row.label,
            &row_state.waiter,
            state::SERVICE_JOB_RUNNABLE,
        )
        .await;
    }
    let settled = Instant::now() + RESTING_WINDOW;
    while Instant::now() < settled {
        for (row, row_state) in table.iter().zip(&prepared) {
            let detail = described(&batch, row.label, &row_state.waiter).await;
            assert_eq!(
                detail["status"].as_str(),
                Some(state::SERVICE_JOB_RUNNABLE),
                "case {}: a job the published capacity cannot hold rests in RUNNABLE, got \
                 {detail}",
                row.label
            );
            assert!(
                !is_placed(&batch, &row_state.waiter),
                "case {}: the holders hold the whole published capacity, so the waiter must \
                 not be placed, got {detail}",
                row.label
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Now end each row's list, by the form the row names.
    for (row, row_state) in table.iter().zip(&prepared) {
        match row.form {
            Form::Bulk => {
                let (status, body) = terminate_many(&batch, &row_state.listed, row.reason).await;
                assert_eq!(
                    status, 200,
                    "case {}: the result is reported per service job, so the call itself \
                     answers 200, got {status} {body}",
                    row.label
                );
                assert_eq!(
                    body["successful"],
                    json!(row_state.expected),
                    "case {}: every distinct id the request named is successful, got {body}",
                    row.label
                );
                assert_eq!(
                    body["errors"],
                    json!([]),
                    "case {}: no id this service could address failed, got {body}",
                    row.label
                );
            }
            Form::Singular => {
                for (index, id) in row_state.listed.iter().enumerate() {
                    terminate(&batch, row.label, id, row.reason).await;
                    let ended = index + 1;
                    if ended >= row_state.holders.len() {
                        continue;
                    }
                    // The capacity comes back with the JOB that ended, not with the CALL that
                    // ended it: while another holder still holds part of it the waiter is not
                    // placeable at all. At the FIRST such point the waiter is watched for
                    // longer than the span a move of its own would take, which is what tells
                    // this apart from a give-back that armed the waiter at this call; the
                    // later points are read once, for the same fact.
                    let watch = if ended == 1 {
                        RESTING_WINDOW
                    } else {
                        Duration::ZERO
                    };
                    let case = format!(
                        "{}: {ended} of {} holders ended",
                        row.label,
                        row_state.holders.len()
                    );
                    rests_unplaced(&batch, &case, &row_state.waiter, watch).await;
                }
            }
        }
    }

    // Whichever form ended them, every row now reads the same way. Kept in the table's own
    // order, so a row's pair is read by position rather than searched for.
    let mut read_back: Vec<Vec<Value>> = Vec::new();
    for (row, row_state) in table.iter().zip(&prepared) {
        let mut holders_after: Vec<Value> = Vec::new();
        for holder in &row_state.holders {
            let detail = described(&batch, row.label, holder).await;
            assert_eq!(
                detail["status"].as_str(),
                Some(state::SERVICE_JOB_FAILED),
                "case {}: an ended service job is FAILED, got {detail}",
                row.label
            );
            assert_eq!(
                detail["statusReason"].as_str(),
                Some(row.reason),
                "case {}: the caller's own reason is what the job carries, got {detail}",
                row.label
            );
            assert_eq!(
                detail["isTerminated"].as_bool(),
                Some(true),
                "case {}: an ended service job reports isTerminated, got {detail}",
                row.label
            );
            holders_after.push(detail);
        }
        read_back.push(holders_after);

        // The capacity the ended jobs were holding has been given back, so the job that was
        // resting for want of it is placed.
        polled_until_placed(&batch, row.label, &row_state.waiter).await;
        let waiter = described(&batch, row.label, &row_state.waiter).await;
        assert_eq!(
            waiter["isTerminated"].as_bool(),
            Some(false),
            "case {}: a termination ends only what it named, got {waiter}",
            row.label
        );

        // An id this service never minted stays unresolvable: being reported as processed is
        // not a service job coming into existence.
        for id in row.unknown {
            let (status, body) = describe(&batch, id).await;
            assert_eq!(
                status, 400,
                "case {}: {id:?} names no service job, so it stays unresolvable, got {status} \
                 {body}",
                row.label
            );
            assert_eq!(
                error_code(&body),
                "ClientException",
                "case {}: expected the modelled ClientException for {id:?}, got {body}",
                row.label
            );
        }
    }

    // The differential assertion: a bulk row and the singular row it is paired with left their
    // service jobs in the same state, member for member.
    for (position, row) in table.iter().enumerate() {
        let Some(pair) = row.pair else {
            continue;
        };
        let paired_position = table
            .iter()
            .position(|other| other.label == pair)
            .unwrap_or_else(|| panic!("case {}: the paired row {pair:?} must exist", row.label));
        let mine = &read_back[position];
        let theirs = &read_back[paired_position];
        assert_eq!(
            mine.len(),
            theirs.len(),
            "case {}: the paired rows end the same number of service jobs",
            row.label
        );
        for (index, (ended, paired)) in mine.iter().zip(theirs).enumerate() {
            assert_eq!(
                comparable(ended),
                comparable(paired),
                "case {}: holder {index} must read exactly as the singular form left it, got \
                 {ended} against {paired}",
                row.label
            );
        }
    }
}
