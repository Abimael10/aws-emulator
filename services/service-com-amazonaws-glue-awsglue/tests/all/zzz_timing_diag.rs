// SPDX-License-Identifier: CC0-1.0

//! A job run's script is performed by the SERVICE, and its lifecycle advances on the service's
//! OWN CLOCK — so the `Glue Job State Change` its terminal transition publishes reaches its
//! rule's target with NO further Glue call, however many runs are in flight.
//!
//! The rule under test: **the ONE execution a run is owed is owed from the instant the run is
//! recorded, and the service performs it; the bound on how many job scripts this emulator runs
//! AT ONCE ([`MAX_CONCURRENT_JOB_SCRIPT_RUNS`]) bounds how many run at a time and not whether a
//! run is performed at all — so every run started comes to rest in the state its script reached
//! and publishes that transition, with no request of the client's driving it.** That is what a
//! client reacting to a job run through a rule DOES: it starts the run (or a trigger does) and
//! waits on the rule's target, never polling `GetJobRun`, so a run that is performed only by the
//! next read is a run that is never performed, and a publication only a later read produces is
//! never published. And the read that follows must report the SAME state the event carried,
//! since the run came to rest before anything read it.
//!
//! The rows of the first table differ along the terminal state the run's own script reaches — a
//! script that exits cleanly, and one that raises — because the publication is owed by the
//! transition and not by the outcome. The second table is the same rule under CONTENTION: more
//! runs started together than this process runs scripts at once, each script holding its
//! interpreter while it waits, so the runs past the bound are the ones no interpreter was free
//! for when they were started.
//!
//! Nothing here moves a due instant through the store and nothing sleeps for one: the only wait
//! is a poll for the CONDITION — the event on the target queue — against a generous deadline,
//! and no assertion reads elapsed time. A script's own `time.sleep` is the script's work, which
//! a run is reported `RUNNING` for the length of.
//!
//! [`crate::job_run_state_change_events`] pins what the published envelope carries over every
//! terminal state, and [`crate::job_run_event_pump_liveness`] pins that a claim no pump holds
//! does not park the publication; both read the run back through `GetJobRun` while they wait,
//! which is the one thing this suite must not do.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED, MAX_CONCURRENT_JOB_SCRIPT_RUNS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_run_state_change_events::{
    drain, emulator_with_a_bus, given_queue, given_rule, grant_delivery,
};
use crate::job_script_execution::{create_bucket, stage_script, SCRIPTS};
use crate::wire::ok;

/// The `source` and `detail-type` Glue publishes a job run's terminal state under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make this suite
/// pass.
const EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";

/// The role the jobs here name. No IAM is mounted in this suite, so nothing can be shown about
/// it and the run is performed; which role a run is performed as is pinned by the suites that
/// mount IAM.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/glue-job-clock";

/// A script that runs to its end, which is what a succeeding job does.
const EXITS_CLEANLY: &str = r#"
total = sum(index for index in range(4))
assert total == 6, total
"#;

/// A script that raises, so its run comes to rest in the failure state instead.
const RAISES: &str = r#"
raise ValueError("the input partition is malformed")
"#;

/// A script that WAITS before it ends: an ordinary Glue script waiting for work to settle. A
/// run is reported `RUNNING` for as long as its script waits, and the script holds the
/// interpreter it runs in for that span — which is what makes the runs started behind it find
/// none free.
const WAITS: &str = r#"
import time

time.sleep(2)
"#;

/// How long a delivery is waited for, and how often the queue is looked at. Generous: what is
/// asserted is that the event arrives at all, never how fast the host runs — and the contended
/// table waits for runs that are performed one interpreter at a time.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// One started run: what it was started with, and where its event is delivered.
struct Started {
    /// The case name every assertion of the row reports.
    case: String,
    /// The terminal `JobRunState` the row's script reaches.
    state: &'static str,
    /// The job the row runs, and the run it started.
    job: String,
    run_id: String,
    /// The queue the row's rule delivers to.
    queue_url: String,
}

/// Start a run of a job on `script`, with its own rule and target queue, and answer what waits
/// for its event.
///
/// The rule matches this job in EITHER terminal state: what is asserted is that the transition
/// was published, so the state is read off the event rather than selected by the rule.
async fn start_a_run(
    router: &MultiServiceRouter,
    name: &str,
    case: String,
    script: &str,
    state: &'static str,
) -> Started {
    let job = format!("job_run_clock_{name}");
    let (queue_url, queue_arn) = given_queue(router, &format!("glue-job-clock-{name}")).await;
    grant_delivery(router, &queue_url, &queue_arn).await;
    given_rule(
        router,
        &format!("glue-job-clock-{name}"),
        json!({
            "source": [EVENT_SOURCE],
            "detail-type": [JOB_STATE_CHANGE],
            "detail": { "jobName": [&job] },
        }),
        &[queue_arn],
    )
    .await;

    let location = stage_script(router, &format!("{SCRIPTS}{job}.py"), script).await;
    ok(
        router,
        "CreateJob",
        &json!({
            "Name": &job,
            "Role": ROLE_ARN,
            "Command": {
                "Name": "pythonshell",
                "PythonVersion": "3.9",
                "ScriptLocation": location,
            },
            "MaxCapacity": 0.0625,
            "Timeout": 5,
        }),
    )
    .await;
    let started = ok(router, "StartJobRun", &json!({ "JobName": &job })).await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
        })
        .to_string();
    Started {
        case,
        state,
        job,
        run_id,
        queue_url,
    }
}

/// Wait until every started run's event is on its own queue, calling NOTHING of Glue while
/// waiting, and answer what each queue held.
async fn delivered_events(router: &MultiServiceRouter, started: &[Started]) -> Vec<Vec<Value>> {
    let mut held: Vec<Vec<Value>> = vec![Vec::new(); started.len()];
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        for (index, run) in started.iter().enumerate() {
            held[index].extend(drain(router, &run.queue_url).await);
        }
        let outstanding: Vec<&str> = started
            .iter()
            .enumerate()
            .filter(|(index, _)| held[*index].is_empty())
            .map(|(_, run)| run.case.as_str())
            .collect();
        if outstanding.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "no event reached the target queue of {outstanding:?} before the deadline; the \
             queues hold {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Assert that each started run published exactly one state change carrying its own state, and
/// that the `GetJobRun` made AFTERWARDS reports that same state.
async fn assert_each_run_published_its_own_state(
    router: &MultiServiceRouter,
    started: &[Started],
    held: &[Vec<Value>],
) {
    for (index, run) in started.iter().enumerate() {
        let case = &run.case;
        let events = &held[index];
        assert_eq!(
            events.len(),
            1,
            "case {case:?}: expected exactly one published state change, got {events:?}"
        );
        let event = &events[0];
        for (pointer, expected) in [
            ("/source", json!(EVENT_SOURCE)),
            ("/detail-type", json!(JOB_STATE_CHANGE)),
            ("/detail/jobName", json!(&run.job)),
            ("/detail/jobRunId", json!(&run.run_id)),
            ("/detail/state", json!(run.state)),
        ] {
            let actual = event.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {event}"
            );
        }

        // Only now is the run read: it must report the state its event already carried, which
        // is what says the clock moved it rather than this read.
        let reported = ok(
            router,
            "GetJobRun",
            &json!({ "JobName": &run.job, "RunId": &run.run_id }),
        )
        .await;
        let reported_run = &reported["JobRun"];
        assert_eq!(
            reported_run["JobRunState"].as_str(),
            Some(run.state),
            "case {case:?}: the run must report the state its event carried, got {reported_run}"
        );
    }
}

/// RULE: the terminal transition a run is owed is materialised and published by the service
/// itself, so its `Glue Job State Change` reaches the rule's target with no Glue call after the
/// start — whichever state the run's own script reached.
#[tokio::test]
async fn a_runs_terminal_state_change_arrives_without_a_single_further_glue_call() {
    let (router, _engine) = emulator_with_a_bus();
    create_bucket(&router).await;

    // The rows: the terminal states a run's own script reaches, which the publication does not
    // turn on.
    let rows: [(&str, &str, &str); 2] = [
        ("a script that exits cleanly", EXITS_CLEANLY, JOB_RUN_STATE_SUCCEEDED),
        ("a script that raises", RAISES, JOB_RUN_STATE_FAILED),
    ];

    let mut started: Vec<Started> = Vec::new();
    for (index, (case, script, state)) in rows.into_iter().enumerate() {
        started.push(
            start_a_run(
                &router,
                &format!("state_{index}"),
                case.to_string(),
                script,
                state,
            )
            .await,
        );
    }

    let held = delivered_events(&router, &started).await;
    assert_each_run_published_its_own_state(&router, &started, &held).await;
}

/// RULE: the bound on how many job scripts this emulator runs AT ONCE bounds how many run at a
/// time, not whether a run is performed — so every run of a burst larger than that bound, each
/// of whose scripts holds its interpreter while it waits, is still performed and still publishes
/// its terminal transition, with no Glue call after the start.
#[tokio::test]
async fn a_run_started_with_no_free_interpreter_is_still_performed_and_still_published() {
    let (router, _engine) = emulator_with_a_bus();
    create_bucket(&router).await;

    // Two more runs than this process runs scripts at once, so the last of them are started
    // while every interpreter is held by a script that is still waiting.
    let burst = MAX_CONCURRENT_JOB_SCRIPT_RUNS + 2;
    let mut started: Vec<Started> = Vec::new();
    for index in 0..burst {
        started.push(
            start_a_run(
                &router,
                &format!("burst_{index}"),
                format!("run {index} of a burst of {burst} waiting scripts"),
                WAITS,
                JOB_RUN_STATE_SUCCEEDED,
            )
            .await,
        );
    }

    let held = delivered_events(&router, &started).await;
    assert_each_run_published_its_own_state(&router, &started, &held).await;
}
