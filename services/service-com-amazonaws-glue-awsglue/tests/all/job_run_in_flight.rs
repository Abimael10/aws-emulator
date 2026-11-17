// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a job run's IN-FLIGHT phase: what a client observes while the run's
//! script is running, and what the run reports once it has finished.
//!
//! The rule under test: a run of a job is one execution of the job's script, performed as the
//! RUN's own work rather than as part of the call that started it — `StartJobRun` "starts a job
//! run" and answers the id it minted, so the run is still in flight when that call returns —
//! and the run occupies each state of its lifecycle in turn: `STARTING`, then `RUNNING` for as
//! long as its script runs, and only then the state that execution came to. A WAIT the script
//! asks for keeps its value, so the run is reported running while its script waits, the
//! effects the script makes after waiting become observable then and not before the run was in
//! flight, and a script whose waits stay inside the span one run is given
//! ([`JOB_SCRIPT_RUN_CEILING`]) comes to rest `SUCCEEDED` rather than terminated for taking too
//! long. What a script that waits PAST that span reports is pinned beside it
//! ([`crate::job_script_bounds`]).
//!
//! The rows differ along how long the script waits before it makes its one effect — not at
//! all, one owed span, and two of them — which is what the rule generalises over: whether the
//! effect lands while the run is still starting or while it is running, the lifecycle a client
//! polling the run observes is the same.
//!
//! The rule is about the RUN and not about one read of it, so the second test here polls the
//! same lifecycle through EVERY operation that reports a job run — the read of one run, that
//! same read asking for the run's predecessors, and the read of the job's runs — because a run
//! is "reported by `GetJobRun`/`GetJobRuns`" in the state its own lifecycle has reached,
//! whichever of them a client keyed its wait loop on.
//!
//! Amazon S3 and Amazon SQS are mounted BESIDE Glue on the SAME engine, so the script is read
//! from the object store a test staged it in and the message it sends travels the real
//! in-process callback to a real queue — the effect asserted here is one a client can receive.
//! The queue itself and the one spelling of an SQS call are the event suite's
//! ([`crate::job_run_state_change_events`]), shared rather than written a second time. Every
//! wait is a poll for the CONDITION it waits on, against a generous deadline
//! ([`crate::job_runs::RUN_AT_REST_DEADLINE`]); nothing here sleeps for a fixed span and
//! nothing asserts on this host's clock.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_SETTLES_AFTER, JOB_RUN_STATE_RUNNING, JOB_RUN_STATE_STARTING,
    JOB_RUN_STATE_SUCCEEDED, JOB_SCRIPT_RUN_CEILING, TERMINAL_JOB_RUN_STATES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_run_state_change_events::{given_queue, sqs};
use crate::job_runs::{at_rest, get_run, start_run, RUN_AT_REST_DEADLINE, RUN_POLL_INTERVAL};
use crate::job_script_execution::{create_bucket, stage_script, ROLE_ARN, SCRIPTS};
use crate::wire::{emulator_on, ok};

/// The script every row runs: it waits for the seconds its `--WAIT_SECONDS` argument names and
/// then sends ONE message to the queue its `--QUEUE_URL` names, carrying the tag its
/// `--RUN_TAG` names.
///
/// The wait comes from the run's own arguments, so one script serves every row, and the
/// message is the row's only effect — so WHEN the queue holds it says when the script got
/// there.
const WAITS_THEN_SENDS: &str = r#"
import sys
import time

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
time.sleep(float(named["--WAIT_SECONDS"]))
boto3.client("sqs").send_message(
    QueueUrl=named["--QUEUE_URL"],
    MessageBody="completed:%s" % named["--RUN_TAG"],
)
"#;

/// An emulator with Glue, Amazon S3 AND Amazon SQS on one shared engine, plus that engine.
///
/// Amazon S3 is mounted because a run READS its script from it; Amazon SQS because the script's
/// own effect is a message on a queue, sent over the emulator's in-process callback.
fn emulator_with_a_queue() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = emulator_on(Arc::clone(&engine));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// How many messages the queue at `url` holds, WITHOUT taking any of them: the attribute SQS
/// reports for exactly that question, so reading it leaves the queue as it was.
async fn messages_held(router: &MultiServiceRouter, url: &str) -> u64 {
    let answer = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"] }),
    )
    .await;
    answer["Attributes"]["ApproximateNumberOfMessages"]
        .as_str()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0)
}

/// Every message body the queue at `url` holds, received until the queue answers with none.
async fn received_bodies(router: &MultiServiceRouter, case: &str, url: &str) -> BTreeSet<String> {
    let deadline = SystemTime::now() + RUN_AT_REST_DEADLINE;
    let mut bodies: BTreeSet<String> = BTreeSet::new();
    loop {
        let answer = sqs(
            router,
            "ReceiveMessage",
            json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answer["Messages"].as_array().cloned().unwrap_or_default();
        for message in &messages {
            if let Some(body) = message["Body"].as_str() {
                bodies.insert(body.to_string());
            }
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    "DeleteMessage",
                    json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
            }
        }
        if !bodies.is_empty() {
            return bodies;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the script's message must reach the queue, and it holds {bodies:?}"
        );
        tokio::time::sleep(RUN_POLL_INTERVAL).await;
    }
}

/// What polling one run through its lifecycle observed.
#[derive(Debug)]
struct Observed {
    /// Every `JobRunState` the poll saw, in the order it saw them.
    states: Vec<String>,
    /// How many messages the queue held the FIRST time the run was seen running, or `None`
    /// when the poll never saw it running.
    held_when_running: Option<u64>,
    /// The `JobRun` the run came to rest as.
    at_rest: Value,
}

/// WHICH read of a job run a client polls its lifecycle through.
///
/// Every one of these reports the same run, so each has to report the state that run's own
/// lifecycle has reached.
#[derive(Debug, Clone, Copy)]
enum Reader {
    /// `GetJobRun`, addressing the run by the `JobName`/`RunId` pair.
    OneRun,
    /// The same read, asking for the run's predecessors as well
    /// (`GetJobRunRequest$PredecessorsIncluded`).
    OneRunWithPredecessors,
    /// `GetJobRuns`, which reports every run of the job — the run under test among them.
    EveryRunOfTheJob,
}

/// Read the run `run_id` of `job_name` through `reader`, and answer the `JobRun` it reports.
async fn read_through(
    router: &MultiServiceRouter,
    reader: Reader,
    case: &str,
    job_name: &str,
    run_id: &str,
) -> Value {
    match reader {
        Reader::OneRun => get_run(router, job_name, run_id).await,
        Reader::OneRunWithPredecessors => {
            let body = ok(
                router,
                "GetJobRun",
                &json!({
                    "JobName": job_name,
                    "RunId": run_id,
                    "PredecessorsIncluded": true,
                }),
            )
            .await;
            body.get("JobRun").cloned().unwrap_or_else(|| {
                panic!("case {case:?}: GetJobRun must report a JobRun, got {body}")
            })
        }
        Reader::EveryRunOfTheJob => {
            let page = ok(router, "GetJobRuns", &json!({ "JobName": job_name })).await;
            page.get("JobRuns")
                .and_then(Value::as_array)
                .and_then(|runs| {
                    runs.iter()
                        .find(|run| run.get("Id").and_then(Value::as_str) == Some(run_id))
                        .cloned()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the runs of {job_name:?} must hold {run_id:?}, got {page}"
                    )
                })
        }
    }
}

/// Poll the run `run_id` of `job_name` THROUGH `reader` until it has come to rest, recording
/// every state it goes through and what the queue held the first time it was seen running.
///
/// Nothing is made due through the store: this suite is asking what the run's OWN lifecycle
/// does, so the states fall due on the service's own clock. The wait is a poll for the
/// condition — that the run has come to rest — against a generous deadline, and nothing is
/// asserted about how long any of it took. Every read is also asked the two questions a run
/// that has not finished answers the same way whichever read reports it.
async fn observe_run(
    router: &MultiServiceRouter,
    reader: Reader,
    case: &str,
    job_name: &str,
    run_id: &str,
    queue_url: &str,
) -> Observed {
    let deadline = SystemTime::now() + RUN_AT_REST_DEADLINE;
    let mut states: Vec<String> = Vec::new();
    let mut held_when_running: Option<u64> = None;
    loop {
        let run = read_through(router, reader, case, job_name, run_id).await;
        let state = run
            .get("JobRunState")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if states.last() != Some(&state) {
            if state == JOB_RUN_STATE_RUNNING && held_when_running.is_none() {
                held_when_running = Some(messages_held(router, queue_url).await);
            }
            states.push(state);
        }
        if at_rest(&run) {
            return Observed {
                states,
                held_when_running,
                at_rest: run,
            };
        }
        assert!(
            run.get("CompletedOn").is_none() && run.get("ErrorMessage").is_none(),
            "case {case:?}: a run read through {reader:?} while it is in flight reports neither \
             CompletedOn nor an ErrorMessage, got {run}"
        );
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: run {run_id:?} of {job_name:?}, read through {reader:?}, must come \
             to rest, and it reports {run}"
        );
        tokio::time::sleep(RUN_POLL_INTERVAL).await;
    }
}

/// Create the job for one row: a Python shell job whose script is the staged one.
async fn create_job(router: &MultiServiceRouter, name: &str, location: &str) {
    ok(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": ROLE_ARN,
            "Command": {
                "Name": "pythonshell",
                "PythonVersion": "3.9",
                "ScriptLocation": location,
            },
            "MaxCapacity": 0.0625,
        }),
    )
    .await;
}

#[tokio::test]
async fn a_run_is_observable_while_its_script_runs_and_its_effect_lands_then() {
    // (case, the seconds the script waits before it sends its message)
    let rows: [(&str, u64); 3] = [
        ("a script that waits for nothing at all", 0),
        (
            "a script that waits one of the spans a transition is owed after",
            JOB_RUN_SETTLES_AFTER.as_secs(),
        ),
        (
            "a script that waits two of them",
            JOB_RUN_SETTLES_AFTER.as_secs() * 2,
        ),
    ];

    let (router, _engine) = emulator_with_a_queue();
    create_bucket(&router).await;

    for (index, (case, waits)) in rows.into_iter().enumerate() {
        // Every row's wait has to stay inside the span one run of a script is given, or the
        // row would be asking for the state a terminated script reports instead.
        assert!(
            waits < JOB_SCRIPT_RUN_CEILING.as_secs(),
            "case {case:?}: a row of this suite waits {waits}s, inside the \
             {JOB_SCRIPT_RUN_CEILING:?} span one run of a script is given"
        );

        let name = format!("in-flight-{index}");
        let tag = format!("tag-{index}");
        let (queue_url, _arn) = given_queue(&router, &format!("in-flight-queue-{index}")).await;
        let location = stage_script(&router, &format!("{SCRIPTS}{name}.py"), WAITS_THEN_SENDS).await;
        create_job(&router, &name, &location).await;

        let started = ok(
            &router,
            "StartJobRun",
            &json!({
                "JobName": name,
                "Arguments": {
                    "--QUEUE_URL": queue_url,
                    "--RUN_TAG": tag,
                    "--WAIT_SECONDS": waits.to_string(),
                },
            }),
        )
        .await;
        let run_id = started
            .get("JobRunId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}"))
            .to_string();

        // The start ANSWERED while the run was still in flight: it records the run and hands
        // the script off, so the first read of the run finds it in the state a start puts it
        // in rather than in a terminal one.
        let straight_after = get_run(&router, &name, &run_id).await;
        assert_eq!(
            straight_after.get("JobRunState").and_then(Value::as_str),
            Some(JOB_RUN_STATE_STARTING),
            "case {case:?}: a caller reading straight after the start finds the run \
             {JOB_RUN_STATE_STARTING}, got {straight_after}"
        );
        assert!(
            straight_after.get("CompletedOn").is_none()
                && straight_after.get("ErrorMessage").is_none(),
            "case {case:?}: a run that has not finished reports neither CompletedOn nor an \
             ErrorMessage, got {straight_after}"
        );

        let observed =
            observe_run(&router, Reader::OneRun, case, &name, &run_id, &queue_url).await;

        // The run passed THROUGH the state a run whose script is running is in, before the
        // state it came to rest in: a client polling it observes the in-flight phase.
        assert!(
            observed.states.contains(&JOB_RUN_STATE_RUNNING.to_string()),
            "case {case:?}: the run must be observable in {JOB_RUN_STATE_RUNNING}, and the \
             poll saw {:?}",
            observed.states
        );
        assert_eq!(
            observed.states.first().map(String::as_str),
            Some(JOB_RUN_STATE_STARTING),
            "case {case:?}: the lifecycle begins in {JOB_RUN_STATE_STARTING}, and the poll \
             saw {:?}",
            observed.states
        );
        assert_eq!(
            observed.states.last().map(String::as_str),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: a script that exited cleanly brings its run to rest in \
             {JOB_RUN_STATE_SUCCEEDED}, and the poll saw {:?} ({})",
            observed.states,
            observed.at_rest
        );

        // The run came to rest on what its script did, with nothing it failed at.
        assert!(
            observed.at_rest.get("ErrorMessage").is_none(),
            "case {case:?}: a run whose script failed at nothing carries no ErrorMessage, got \
             {}",
            observed.at_rest
        );
        assert!(
            observed
                .at_rest
                .get("CompletedOn")
                .and_then(Value::as_f64)
                .is_some(),
            "case {case:?}: a run that has come to rest reports when it finished, got {}",
            observed.at_rest
        );

        // The WAIT the script asked for kept its value, so the run consumed at least that
        // many whole seconds: "the amount of time (in seconds) that the job run consumed
        // resources" (`JobRun$ExecutionTime`).
        let consumed = observed
            .at_rest
            .get("ExecutionTime")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        assert!(
            consumed >= i64::try_from(waits).unwrap_or(i64::MAX),
            "case {case:?}: a run whose script waited {waits}s must report an ExecutionTime of \
             at least that, got {consumed} in {}",
            observed.at_rest
        );

        // The script's own effect is there: the message it sent, received from the queue.
        let bodies = received_bodies(&router, case, &queue_url).await;
        assert_eq!(
            bodies,
            BTreeSet::from([format!("completed:{tag}")]),
            "case {case:?}: the run's script sends exactly the message its arguments name, \
             got {bodies:?}"
        );

        // And a script that waits longer than the span the start owes the run had NOT made
        // that effect yet when the run was first seen running: the effect lands while the run
        // is in flight rather than before the client could observe it at all.
        if waits > JOB_RUN_SETTLES_AFTER.as_secs() {
            assert_eq!(
                observed.held_when_running,
                Some(0),
                "case {case:?}: the script waits {waits}s, longer than the \
                 {JOB_RUN_SETTLES_AFTER:?} the start owes the run, so the queue must still be \
                 empty when the run is first seen in {JOB_RUN_STATE_RUNNING}, and it held {:?}",
                observed.held_when_running
            );
        }
    }
}

#[tokio::test]
async fn every_read_of_a_run_reports_the_lifecycle_it_has_reached() {
    // (case, the read the client polls through, the seconds the script waits before it sends)
    let rows: [(&str, Reader, u64); 4] = [
        ("polled through the read of one run", Reader::OneRun, 0),
        (
            "polled through the read of one run, asking for its predecessors",
            Reader::OneRunWithPredecessors,
            JOB_RUN_SETTLES_AFTER.as_secs(),
        ),
        (
            "polled through the read of the job's runs",
            Reader::EveryRunOfTheJob,
            0,
        ),
        (
            "polled through the read of the job's runs, whose script waits a span",
            Reader::EveryRunOfTheJob,
            JOB_RUN_SETTLES_AFTER.as_secs(),
        ),
    ];

    let (router, _engine) = emulator_with_a_queue();
    create_bucket(&router).await;

    for (index, (case, reader, waits)) in rows.into_iter().enumerate() {
        // As above: a row's wait has to stay inside the span one run of a script is given, or
        // the row would be asking for the state a terminated script reports instead.
        assert!(
            waits < JOB_SCRIPT_RUN_CEILING.as_secs(),
            "case {case:?}: a row of this suite waits {waits}s, inside the \
             {JOB_SCRIPT_RUN_CEILING:?} span one run of a script is given"
        );

        let name = format!("read-through-{index}");
        let tag = format!("read-through-tag-{index}");
        let (queue_url, _arn) = given_queue(&router, &format!("read-through-queue-{index}")).await;
        let location =
            stage_script(&router, &format!("{SCRIPTS}{name}.py"), WAITS_THEN_SENDS).await;
        create_job(&router, &name, &location).await;

        let run_id = start_run(
            &router,
            &json!({
                "JobName": name,
                "Arguments": {
                    "--QUEUE_URL": queue_url,
                    "--RUN_TAG": tag,
                    "--WAIT_SECONDS": waits.to_string(),
                },
            }),
        )
        .await;

        let observed = observe_run(&router, reader, case, &name, &run_id, &queue_url).await;
        let states = observed.states;

        // Whichever read the client polled through, it observed the whole lifecycle: the state
        // a start puts a run in, then the state a run whose script is running is in, then the
        // state that execution came to — and nothing outside it.
        assert_eq!(
            states.first().map(String::as_str),
            Some(JOB_RUN_STATE_STARTING),
            "case {case:?}: the lifecycle begins in {JOB_RUN_STATE_STARTING}, and {reader:?} \
             reported {states:?}"
        );
        assert_eq!(
            states.last().map(String::as_str),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: a script that exited cleanly brings its run to rest in \
             {JOB_RUN_STATE_SUCCEEDED}, and {reader:?} reported {states:?}"
        );
        // Checked, so a lifecycle with fewer states than that — the very collapse this suite
        // is about — is REPORTED rather than panicking on the arithmetic.
        assert_eq!(
            states.iter().position(|state| state == JOB_RUN_STATE_RUNNING),
            states.len().checked_sub(2),
            "case {case:?}: {reader:?} must report {JOB_RUN_STATE_RUNNING} as the state just \
             before the terminal one, and it reported {states:?}"
        );
        for state in &states {
            assert!(
                [
                    JOB_RUN_STATE_STARTING,
                    JOB_RUN_STATE_RUNNING,
                    JOB_RUN_STATE_SUCCEEDED,
                ]
                .contains(&state.as_str()),
                "case {case:?}: {reader:?} reported the unexpected state {state:?} in \
                 {states:?}"
            );
        }
        assert_eq!(
            states
                .iter()
                .filter(|state| TERMINAL_JOB_RUN_STATES.contains(&state.as_str()))
                .count(),
            1,
            "case {case:?}: a run leaves no terminal state again, so {reader:?} reports exactly \
             one of them, and it reported {states:?}"
        );
        assert!(
            observed
                .at_rest
                .get("CompletedOn")
                .and_then(Value::as_f64)
                .is_some()
                && observed.at_rest.get("ErrorMessage").is_none(),
            "case {case:?}: {reader:?} must report a finished run's CompletedOn and no \
             ErrorMessage for a script that failed at nothing, got {}",
            observed.at_rest
        );

        // And the run really ran: the message its script sends is on the queue a client reads.
        let bodies = received_bodies(&router, case, &queue_url).await;
        assert_eq!(
            bodies,
            BTreeSet::from([format!("completed:{tag}")]),
            "case {case:?}: the run's script sends exactly the message its arguments name, got \
             {bodies:?}"
        );
    }
}
