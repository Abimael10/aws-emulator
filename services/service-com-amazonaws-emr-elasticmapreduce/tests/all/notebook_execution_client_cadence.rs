// SPDX-License-Identifier: CC0-1.0

//! A notebook execution a client POLLS at its own cadence is still an execution it can act
//! on, end to end over the wire.
//!
//! THE RULE: a notebook execution is consumed by polling — `DescribeNotebookExecution` is
//! the only way to read one — so every status on the way to the end is a phase a client
//! really reaches, not an instant between two reads: "`STARTING` indicates that the
//! execution is starting on the cluster", "`RUNNING` indicates that the execution is being
//! processed by the cluster", "`FINISHING` indicates that execution processing is in the
//! final stages" (`NotebookExecution$Status` doc). A caller that starts an execution and
//! then reads it at the cadence a cluster-side status is polled at therefore finds it being
//! processed rather than already ended, and the `StopNotebookExecution` it sends from there
//! takes it through "`STOP_PENDING` … the cluster has received a `StopNotebookExecution`
//! request and the stop is pending" and "`STOPPING` …" to "`STOPPED` indicates that the
//! execution stopped because of a `StopNotebookExecution` request" — whatever its notebook
//! was going to come to.
//!
//! The rows differ along what the rule generalises over: HOW OFTEN the client looks before
//! it decides to stop (a read a second apart, and reads several seconds apart, which is what
//! a client waiting on an AWS status does), how many reads it takes before the stop, and what
//! the notebook it stopped would have come to (cells that would all have run, a cell that
//! raises, a key Amazon S3 holds nothing under). The sibling suite
//! [`crate::zz_probe_notebook_lifecycle`] pins the same rule for a caller that reads as fast
//! as it can; this one pins it for a caller that reads as a client does.
//!
//! The harness is [`crate::notebook_execution_run`]'s — Amazon EMR and Amazon S3 on one
//! engine, every notebook written through S3's own `PutObject` — so nothing here reaches into
//! the service's state struct.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::notebook_execution_run::{
    code_cell, described, emr_ok, emulator, notebook, notebook_cluster, s3_ok, BUCKET,
    NOTEBOOK_ROLE, TERMINAL,
};

/// How long any wait of this suite looks for the condition it waits on before giving up.
///
/// Deliberately far longer than the walks it follows take: every wait here is on a STATUS,
/// so a loaded host delays a row rather than failing it, and this only bounds a status that
/// never arrives.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long a wait that is not pacing itself like a client sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One row: the cadence this row's client reads at, how many reads past `START_PENDING` it
/// takes before it stops the execution, and the notebook the execution names.
struct Row {
    case: &'static str,
    /// How long this row's client waits between reads — the cadence a client polling an AWS
    /// status keeps.
    cadence: Duration,
    /// How many reads of a status past `START_PENDING` this client takes before it asks for
    /// the stop: a client that acts on the first one it sees, and one that looks again first.
    reads_before_stopping: usize,
    /// The notebook object written under this row's key, or `None` for a key the bucket
    /// holds nothing under.
    object: Option<Vec<u8>>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client reading a second apart",
            cadence: Duration::from_secs(1),
            reads_before_stopping: 1,
            object: Some(notebook(json!([code_cell("total = sum(range(10))")]), None)),
        },
        Row {
            case: "a client reading five seconds apart",
            cadence: Duration::from_secs(5),
            reads_before_stopping: 1,
            object: Some(notebook(json!([code_cell("answer = 6 * 7")]), None)),
        },
        Row {
            case: "a client reading ten seconds apart",
            cadence: Duration::from_secs(10),
            reads_before_stopping: 1,
            object: Some(notebook(json!([code_cell("print(1 + 1)")]), None)),
        },
        Row {
            case: "a client that looks twice before stopping",
            cadence: Duration::from_secs(5),
            reads_before_stopping: 2,
            object: Some(notebook(
                json!([code_cell("raise ValueError('the notebook said no')")]),
                None,
            )),
        },
        Row {
            case: "a key Amazon S3 holds nothing under",
            cadence: Duration::from_secs(5),
            reads_before_stopping: 1,
            object: None,
        },
    ]
}

/// `StartNotebookExecution` for one row, answering the id Amazon EMR minted.
async fn start(router: &MultiServiceRouter, case: &str, engine_id: &str, key: &str) -> String {
    let answer = emr_ok(
        router,
        "StartNotebookExecution",
        json!({
            "ExecutionEngine": { "Id": engine_id, "Type": "EMR" },
            "ServiceRole": NOTEBOOK_ROLE,
            "NotebookExecutionName": case,
            "NotebookS3Location": { "Bucket": BUCKET, "Key": key },
        }),
    )
    .await;
    answer["NotebookExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartNotebookExecution answers an id, got {answer}"))
        .to_string()
}

/// Read the execution `id` the way a client does — one read, then a wait of `cadence`, until
/// it reports a status past `START_PENDING` — and answer the `reads` statuses it read from
/// there on, each one `cadence` after the one before it.
///
/// The wait is on the STATUS, not on the clock: it looks again until the execution has left
/// `START_PENDING` or [`DEADLINE`] has passed.
async fn read_at_a_cadence(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    cadence: Duration,
    reads: usize,
) -> Vec<String> {
    let began = Instant::now();
    loop {
        let execution = described(router, id).await;
        let status = execution["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: an execution reports a Status, got {execution}"))
            .to_string();
        if status != "START_PENDING" {
            let mut read = vec![status];
            while read.len() < reads {
                tokio::time::sleep(cadence).await;
                let again = described(router, id).await;
                read.push(
                    again["Status"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: an execution reports a Status, got {again}")
                        })
                        .to_string(),
                );
            }
            return read;
        }
        assert!(
            began.elapsed() < DEADLINE,
            "case {case:?}: expected the execution to leave START_PENDING, got {execution}"
        );
        tokio::time::sleep(cadence).await;
    }
}

/// The execution `id` once it has reached a status it never leaves, and every status it was
/// read in on the way, in order and without repeats.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> (Vec<String>, Value) {
    let began = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let execution = described(router, id).await;
        let status = execution["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: an execution reports a Status, got {execution}"))
            .to_string();
        if seen.last() != Some(&status) {
            seen.push(status.clone());
        }
        if TERMINAL.contains(&status.as_str()) {
            return (seen, execution);
        }
        assert!(
            began.elapsed() < DEADLINE,
            "case {case:?}: expected the execution to settle, saw {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The key one row's notebook is written under.
fn key_of(index: usize) -> String {
    format!("client-cadence-{index}.ipynb")
}

/// THE RULE: an execution a client reads at its own cadence is read while it is being
/// PROCESSED — not as one that has already ended — and the stop that client then sends takes
/// it to `STOPPED`.
#[tokio::test]
async fn an_execution_read_at_a_clients_cadence_is_still_stoppable() {
    let router = emulator();
    let cluster_id = notebook_cluster(&router).await;
    s3_ok(&router, "bucket", &format!("/{BUCKET}"), &[]).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let key = key_of(index);
        if let Some(object) = row.object.as_ref() {
            s3_ok(&router, case, &format!("/{BUCKET}/{key}"), object).await;
        }
        let id = start(&router, case, &cluster_id, &key).await;

        let read = read_at_a_cadence(&router, case, &id, row.cadence, row.reads_before_stopping)
            .await;
        for status in &read {
            assert!(
                !TERMINAL.contains(&status.as_str()),
                "case {case:?}: an execution this client has only just started is still being \
                 processed, read {read:?}"
            );
        }

        emr_ok(
            &router,
            "StopNotebookExecution",
            json!({ "NotebookExecutionId": id.clone() }),
        )
        .await;

        let (seen, execution) = settled(&router, case, &id).await;
        assert_eq!(
            execution["Status"].as_str(),
            Some("STOPPED"),
            "case {case:?}: an execution stopped from {read:?} reaches STOPPED, saw {seen:?}"
        );
        assert_eq!(
            execution["NotebookExecutionId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the stopped execution is the one that was started, got {execution}"
        );
        for stopping in ["STOP_PENDING", "STOPPING"] {
            assert!(
                seen.iter().any(|status| status == stopping),
                "case {case:?}: the stop walk passes through {stopping}, saw {seen:?}"
            );
        }
    }
}

/// The other half of the same rule: an execution NOBODY stops still ends in what its own run
/// answered, so the window a client acts in does not hold an execution open.
#[tokio::test]
async fn an_execution_nobody_stops_still_ends_in_what_its_notebook_answered() {
    let router = emulator();
    let cluster_id = notebook_cluster(&router).await;
    s3_ok(&router, "bucket", &format!("/{BUCKET}"), &[]).await;

    let ends: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "every cell ran",
            notebook(json!([code_cell("answer = 6 * 7")]), None),
            "FINISHED",
        ),
        (
            "a cell raised",
            notebook(json!([code_cell("raise ValueError('no')")]), None),
            "FAILED",
        ),
    ];

    // Both are started before either is read, so the clock walks them together.
    let mut started: Vec<String> = Vec::new();
    for (index, (case, object, _)) in ends.iter().enumerate() {
        let key = format!("unstopped-{index}.ipynb");
        s3_ok(&router, case, &format!("/{BUCKET}/{key}"), object).await;
        started.push(start(&router, case, &cluster_id, &key).await);
    }

    // One read of each per pass, so each walk is followed from the instant it was started
    // rather than after the one before it had ended.
    let began = Instant::now();
    let mut seen: Vec<Vec<String>> = vec![Vec::new(); started.len()];
    let mut ended: Vec<Value> = vec![Value::Null; started.len()];
    loop {
        let mut still_walking = false;
        for (index, id) in started.iter().enumerate() {
            let execution = described(&router, id).await;
            let status = execution["Status"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: an execution reports a Status, got {execution}",
                        ends[index].0
                    )
                })
                .to_string();
            if seen[index].last() != Some(&status) {
                seen[index].push(status.clone());
            }
            if TERMINAL.contains(&status.as_str()) {
                ended[index] = execution;
            } else {
                still_walking = true;
            }
        }
        if !still_walking {
            break;
        }
        assert!(
            began.elapsed() < DEADLINE,
            "every execution must settle, saw {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, (case, _, expected)) in ends.iter().enumerate() {
        let walk = &seen[index];
        assert_eq!(
            ended[index]["Status"].as_str(),
            Some(*expected),
            "case {case:?}: expected {expected}, saw {walk:?}"
        );
        assert!(
            walk.iter().any(|status| status == "RUNNING"),
            "case {case:?}: an execution is processed by the cluster before it ends, saw \
             {walk:?}"
        );
    }
}
