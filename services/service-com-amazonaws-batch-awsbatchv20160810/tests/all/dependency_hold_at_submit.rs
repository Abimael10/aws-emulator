// SPDX-License-Identifier: CC0-1.0

//! The HOLD a submitted job's own `dependsOn` puts it in, end to end over the wire.
//!
//! RULE under test: `SubmitJob$dependsOn` is "a list of dependencies for the job. A job can
//! depend upon a maximum of 20 jobs" and `JobStatus$PENDING` is where a job "that resides in
//! the queue and isn't yet able to run due to a dependency on another job or resource" waits
//! (`smithy_query(query="shape", shape="JobStatus")`,
//! `smithy_query(query="doc", shape="SubmitJobRequest", member="dependsOn")`). The hold is
//! therefore decided by the SUBMIT, out of the dependencies the request itself names: a job
//! whose dependencies have not all finished when the submit is accepted is reported `PENDING`
//! from that submit onwards — the first read of it already says so — while a job every one of
//! whose dependencies has already finished is held by nothing and goes on to be scheduled.
//!
//! For an array submit the hold is per CHILD, because the array forms point a child's
//! dependency at a different job per index: "you can also specify an N_TO_N type dependency
//! with a job ID for array jobs. In that case, each index child of this job must wait for the
//! corresponding index child of each dependency", and "you can specify a SEQUENTIAL type
//! dependency without specifying a job ID for array jobs so that each child array job
//! completes sequentially, starting at index 0" — so index 0 of a `SEQUENTIAL` array waits for
//! nothing of its own while every later index waits for the one before it.
//!
//! The rows differ along what the rule generalises over: the FORM of the dependency (a plain
//! job id, `N_TO_N` against an array parent, `SEQUENTIAL` within the array itself), how many
//! dependencies one job names, whether they are finished or still live when the submit is
//! accepted, and which index of an array the read is about.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{describe_job, job_definition, job_queue, Batch};

/// `JobStatus` members this suite names, spelled out rather than imported. Source:
/// `smithy_query(query="shape", shape="JobStatus")`.
const PENDING: &str = "PENDING";
const SUCCEEDED: &str = "SUCCEEDED";

/// How long a job of this suite is given to settle, polled for the CONDITION rather than
/// waited out.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The program a job that must stay live for the whole test runs: a wait nothing in the
/// program will end, which is how a container stays up
/// (`job_settles_whatever_wait_its_container_states.rs`).
const LIVE_PROGRAM: &str = "echo up; sleep infinity";

/// The program a job that must finish runs.
const QUICK_PROGRAM: &str = "exit 0";

/// `containerProperties` running `program` under `sh -c`.
fn container_running(program: &str) -> Value {
    json!({
        "image": "busybox",
        "command": ["/bin/sh", "-c", program],
        "resourceRequirements": [
            {"type": "VCPU", "value": "1"},
            {"type": "MEMORY", "value": "512"},
        ],
    })
}

/// One emulator with a queue to submit into, and one definition per program.
struct Fixture {
    batch: Batch,
    queue_arn: String,
    live_definition: String,
    quick_definition: String,
}

async fn fixture(name: &str) -> Fixture {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, &format!("ce-{name}"), state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, &format!("q-{name}"), &environment).await;
    let (live_definition, _) =
        job_definition(&batch, &format!("jd-{name}-live"), container_running(LIVE_PROGRAM)).await;
    let (quick_definition, _) = job_definition(
        &batch,
        &format!("jd-{name}-quick"),
        container_running(QUICK_PROGRAM),
    )
    .await;
    Fixture {
        batch,
        queue_arn,
        live_definition,
        quick_definition,
    }
}

/// Submit one job and answer its id. `extra` carries whatever the row adds to the request
/// (`dependsOn`, `arrayProperties`).
async fn submit(fixture: &Fixture, case: &str, name: &str, definition: &str, extra: Value) -> String {
    let mut request = json!({
        "jobName": name,
        "jobQueue": &fixture.queue_arn,
        "jobDefinition": definition,
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    let answered = fixture.batch.ok("/v1/submitjob", request).await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a jobId in {answered}"))
        .to_string()
}

/// Look again until `job_id` reports `wanted`, and answer its detail.
async fn until_status(fixture: &Fixture, case: &str, job_id: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let detail = describe_job(&fixture.batch, job_id).await;
        if detail["status"].as_str() == Some(wanted) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected {job_id} to reach {wanted} inside {SETTLES_WITHIN:?}, \
             last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the FIRST read of a job whose `dependsOn` names a job that has not finished already
/// reports the hold — `PENDING` — whatever form that dependency takes and however many it
/// names.
#[tokio::test]
async fn the_first_read_of_a_job_its_dependencies_hold_reports_pending() {
    let fixture = fixture("dep-hold").await;
    // The jobs the rows depend on: one plain job and one array parent, both running a program
    // nothing will end, so no row's dependency can finish while the row is read.
    let live = submit(&fixture, "setup", "dep-live", &fixture.live_definition, json!({})).await;
    let live_other =
        submit(&fixture, "setup", "dep-live-other", &fixture.live_definition, json!({})).await;
    let live_array = submit(
        &fixture,
        "setup",
        "dep-live-array",
        &fixture.live_definition,
        json!({"arrayProperties": {"size": 3}}),
    )
    .await;

    // Each row: what the submit adds to the request, and which job id the hold is read on —
    // the submitted job itself, or one child of the array it spawned.
    let rows: Vec<(&str, Value, Option<i32>)> = vec![
        (
            "a plain dependency on a job that is still running",
            json!({"dependsOn": [{"jobId": &live}]}),
            None,
        ),
        (
            "two plain dependencies, neither finished",
            json!({"dependsOn": [{"jobId": &live}, {"jobId": &live_other}]}),
            None,
        ),
        (
            "a dependency on an array parent whose children are still running",
            json!({"dependsOn": [{"jobId": &live_array}]}),
            None,
        ),
        (
            "an N_TO_N child whose matching index has not finished",
            json!({
                "arrayProperties": {"size": 3},
                "dependsOn": [{"jobId": &live_array, "type": "N_TO_N"}],
            }),
            Some(0),
        ),
        (
            "the last N_TO_N child, whose matching index has not finished either",
            json!({
                "arrayProperties": {"size": 3},
                "dependsOn": [{"jobId": &live_array, "type": "N_TO_N"}],
            }),
            Some(2),
        ),
        (
            "a SEQUENTIAL child waiting for the index before it",
            json!({
                "arrayProperties": {"size": 3},
                "dependsOn": [{"type": "SEQUENTIAL"}],
            }),
            Some(1),
        ),
    ];

    for (index, (case, extra, child)) in rows.into_iter().enumerate() {
        let submitted = submit(
            &fixture,
            case,
            &format!("dep-held-{index}"),
            &fixture.live_definition,
            extra,
        )
        .await;
        let read = match child {
            Some(child_index) => state::array_child_job_id(&submitted, child_index),
            None => submitted.clone(),
        };
        let detail = describe_job(&fixture.batch, &read).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(PENDING),
            "case {case}: the first read of a job its dependencies hold reports {PENDING}, \
             got {detail}"
        );
        assert!(
            !detail["dependsOn"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .is_empty(),
            "case {case}: the held job echoes the dependencies holding it, got {detail}"
        );
        // And it STAYS held while the dependency runs: the hold is the dependency's, not a
        // span of this emulator's.
        let again = describe_job(&fixture.batch, &read).await;
        assert_eq!(
            again["status"].as_str(),
            Some(PENDING),
            "case {case}: a second read still reports the hold, got {again}"
        );
    }
}

/// RULE: a job whose dependencies have ALREADY finished is held by nothing — it is scheduled
/// like any other submitted job — and the index of a `SEQUENTIAL` array that waits for nothing
/// of its own is not held either.
#[tokio::test]
async fn a_job_nothing_holds_is_not_pending() {
    let fixture = fixture("dep-free").await;
    let finished = submit(
        &fixture,
        "setup",
        "dep-finished",
        &fixture.quick_definition,
        json!({}),
    )
    .await;
    until_status(&fixture, "setup", &finished, SUCCEEDED).await;

    // A job whose one dependency has already succeeded runs: it is never reported PENDING,
    // and it reaches the terminal status its own program decides.
    let released = submit(
        &fixture,
        "a dependency that has already succeeded",
        "dep-released",
        &fixture.quick_definition,
        json!({"dependsOn": [{"jobId": &finished}]}),
    )
    .await;
    let detail = until_status(
        &fixture,
        "a dependency that has already succeeded",
        &released,
        SUCCEEDED,
    )
    .await;
    assert_eq!(
        detail["container"]["exitCode"].as_i64(),
        Some(0),
        "case a dependency that has already succeeded: expected the job to run, got {detail}"
    );

    // Index 0 of a SEQUENTIAL array waits for nothing of its own, so nothing holds it.
    let ordered = submit(
        &fixture,
        "the first index of a SEQUENTIAL array",
        "dep-sequential",
        &fixture.quick_definition,
        json!({
            "arrayProperties": {"size": 2},
            "dependsOn": [{"type": "SEQUENTIAL"}],
        }),
    )
    .await;
    let first = state::array_child_job_id(&ordered, 0);
    let detail = describe_job(&fixture.batch, &first).await;
    assert_ne!(
        detail["status"].as_str(),
        Some(PENDING),
        "case the first index of a SEQUENTIAL array: index 0 waits for nothing of its own, \
         got {detail}"
    );
    // And the array still completes in order: index 1 waits for index 0 and then runs.
    until_status(
        &fixture,
        "the second index of a SEQUENTIAL array",
        &state::array_child_job_id(&ordered, 1),
        SUCCEEDED,
    )
    .await;
}
