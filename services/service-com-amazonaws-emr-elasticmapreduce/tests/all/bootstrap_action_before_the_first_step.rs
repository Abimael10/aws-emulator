// SPDX-License-Identifier: CC0-1.0

//! A cluster's BOOTSTRAP ACTIONS have run by the time its FIRST STEP runs, and that step's
//! own command line is what decides how the step ends.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/node_hadoop_filesystem.rs` owns, with Amazon S3
//! mounted beside Amazon EMR on ONE engine: every script a row runs is a real object a
//! `PutObject` wrote, and the bytes a step uploaded are read back through `GetObject`.
//!
//! THE RULE: bootstrap actions "run before Hadoop starts on the cluster nodes"
//! (`RunJobFlow$BootstrapActions`) and a step's work is "considered complete [when] the main
//! function [exits] with a zero exit code" (`AddJobFlowSteps` doc) — so a step dispatched on a
//! cluster whose launch carried bootstrap actions RUNS ITS OWN WORK, against a node that
//! already holds everything those actions left on it, and ends in `StepState` `COMPLETED`
//! (with `Status.Timeline.StartDateTime` and `EndDateTime`, no `FailureDetails` and no reason
//! blaming the service) exactly when that work exits zero, and `FAILED` carrying the
//! command's OWN failure when it does not. The ordering is observable from the answers
//! themselves: such a step never starts before the instant the cluster reports as ready, which
//! is the instant its bootstrap phase ended.
//!
//! The rows span what the rule generalises over: how many actions the launch carried (one,
//! three), where on the node an action left its bytes (the temporary directory, a directory
//! the script created), how the step reads them (a bare `cat`, a `cat` piped to a transfer, a
//! `cat` of several files, a test of the file), and — as the control that makes the completions
//! mean something — a step of the same kind of cluster whose command names a file no action
//! wrote, which exits non-zero and fails.

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::STEP_NOT_EXECUTED_REASON;

use crate::node_hadoop_filesystem::{
    create_bucket, drained, emr_ok, emulator, launch, launch_request, object, put_object,
    shell, step_config, BUCKET,
};

/// One row: the scripts its launch's bootstrap actions run, in launch order, the command line
/// its one step submits, the state that step must reach, and what the object store must hold
/// once it has.
struct Row {
    case: &'static str,
    scripts: Vec<String>,
    program: String,
    /// The `StepState` the step's own command line brings it to.
    ends: &'static str,
    /// `(key, the text the object must hold, trimmed)` once the step has ended.
    holds: Vec<(String, String)>,
}

/// The key one row's bootstrap script is put under.
fn script_key(ordinal: usize, position: usize) -> String {
    format!("scripts/{ordinal}-{position}.sh")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The shape the recorded client submits: the action writes a token file under the
            // node's temporary directory and the step merely reads it, so the step completes
            // only because the file was already there when its turn came.
            case: "a bare read of the file the action wrote",
            scripts: vec![
                "#!/bin/bash\nprintf '%s' 'token-one' > /tmp/bootstrap-token-one\n".to_string(),
            ],
            program: "cat /tmp/bootstrap-token-one".to_string(),
            ends: "COMPLETED",
            holds: Vec::new(),
        },
        Row {
            // The same ordering, with the BYTES as the evidence: what the step read is a real
            // object of the store afterwards.
            case: "the bytes the action wrote, carried off the node by the step",
            scripts: vec![
                "#!/bin/bash\nprintf '%s' 'token-two' > /tmp/bootstrap-token-two\n".to_string(),
            ],
            program: format!(
                "set -eu; cat /tmp/bootstrap-token-two | aws s3 cp - s3://{BUCKET}/out/two.txt"
            ),
            ends: "COMPLETED",
            holds: vec![("out/two.txt".to_string(), "token-two".to_string())],
        },
        Row {
            // A directory the SCRIPT created, outside the temporary one, tested rather than
            // read: the node's own storage is what the step looks at.
            case: "a file under a directory the action created, tested by the step",
            scripts: vec![concat!(
                "#!/bin/bash\n",
                "mkdir -p /mnt/var/lib/bootstrap-ordering\n",
                "printf '%s\\n' 'token-three' > /mnt/var/lib/bootstrap-ordering/token\n"
            )
            .to_string()],
            program: "test -f /mnt/var/lib/bootstrap-ordering/token".to_string(),
            ends: "COMPLETED",
            holds: Vec::new(),
        },
        Row {
            // THREE actions: the step reads what each of them left, so the cluster ran the
            // whole list before dispatching it.
            case: "three actions, the step reading what each one left",
            scripts: vec![
                "#!/bin/bash\nprintf '%s\\n' 'first' > /tmp/bootstrap-first\n".to_string(),
                "#!/bin/bash\nprintf '%s\\n' 'second' > /tmp/bootstrap-second\n".to_string(),
                "#!/bin/bash\nprintf '%s\\n' 'third' > /tmp/bootstrap-third\n".to_string(),
            ],
            program: format!(
                "set -eu; cat /tmp/bootstrap-first /tmp/bootstrap-second /tmp/bootstrap-third \
                 | aws s3 cp - s3://{BUCKET}/out/three.txt"
            ),
            ends: "COMPLETED",
            holds: vec![(
                "out/three.txt".to_string(),
                "first\nsecond\nthird".to_string(),
            )],
        },
        Row {
            // The CONTROL: the same cluster, and a command that names a file no action of it
            // wrote. It runs, exits non-zero and fails the step — so the completions above are
            // the commands' own answers rather than a state the lifecycle hands out.
            case: "a read of a file no action wrote",
            scripts: vec![
                "#!/bin/bash\nprintf '%s' 'token-written' > /tmp/bootstrap-written\n".to_string(),
            ],
            program: "cat /tmp/bootstrap-never-written".to_string(),
            ends: "FAILED",
            holds: Vec::new(),
        },
    ]
}

/// The `BootstrapActions` one row's launch carries: one entry per script, each naming the
/// object this suite put that script in.
fn bootstrap_actions(ordinal: usize, row: &Row) -> Value {
    json!((1..=row.scripts.len())
        .map(|position| json!({
            "Name": format!("action-{position}"),
            "ScriptBootstrapAction": { "Path": format!("s3://{BUCKET}/{}", script_key(ordinal, position)) },
        }))
        .collect::<Vec<Value>>())
}

/// A served instant, in the epoch seconds the service reports it in.
fn instant(case: &str, what: &str, value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {what} to be reported, got {value}"))
}

/// THE RULE over the whole table: each row's step ends in what its own command line answered,
/// and every completion is one the node's state — left there by the launch's bootstrap
/// actions — made possible.
#[tokio::test]
async fn a_step_runs_its_command_against_what_the_bootstrap_actions_left() {
    let router = emulator();
    let rows = rows();

    create_bucket(&router).await;
    for (ordinal, row) in rows.iter().enumerate() {
        for (index, script) in row.scripts.iter().enumerate() {
            put_object(&router, &script_key(ordinal, index + 1), script.as_bytes()).await;
        }
    }

    // One cluster per row, all launched before any is polled, so the rows' bootstrap phases
    // run together rather than one after another.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let mut request = launch_request(row.case, json!([step_config(row.case, shell(&row.program))]));
        request["BootstrapActions"] = bootstrap_actions(ordinal, row);
        launched.push(launch(&router, row.case, request).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let steps = drained(case, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        assert_eq!(
            step["Status"]["State"].as_str(),
            Some(row.ends),
            "case {case:?}: the step ends in what its command line answered, got {step}"
        );

        // Whatever it ended as, the step RAN: it has a span of its own, and the failure of the
        // control row is the command's rather than the one that says nothing ran.
        let started = instant(case, "the step's start", &step["Status"]["Timeline"]["StartDateTime"]);
        let ended = instant(case, "the step's end", &step["Status"]["Timeline"]["EndDateTime"]);
        assert!(
            ended >= started,
            "case {case:?}: a step ends no earlier than it started, got {step}"
        );
        assert_ne!(
            step["Status"]["FailureDetails"]["Reason"].as_str(),
            Some(STEP_NOT_EXECUTED_REASON),
            "case {case:?}: the step's application ran, so no failure says it did not, got {step}"
        );

        if row.ends == "COMPLETED" {
            assert!(
                step["Status"]["FailureDetails"].is_null(),
                "case {case:?}: a step that completed reports no failure, got {step}"
            );
            assert!(
                step["Status"]["StateChangeReason"]["Message"].is_null(),
                "case {case:?}: a step that completed carries no reason at all, got {step}"
            );
        } else {
            assert!(
                step["Status"]["FailureDetails"]["Reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "case {case:?}: a command that exited non-zero says why its step failed, got \
                 {step}"
            );
        }

        // THE RULE's ordering half, read off the answers: the cluster reports the instant its
        // bootstrap phase ended (`Status.Timeline.ReadyDateTime`, "when the cluster was ready
        // to run steps"), and the step started no earlier than that.
        let cluster = emr_ok(&router, "DescribeCluster", json!({ "ClusterId": cluster_id })).await
            ["Cluster"]
            .clone();
        let ready = instant(
            case,
            "the cluster's ready instant",
            &cluster["Status"]["Timeline"]["ReadyDateTime"],
        );
        assert!(
            started >= ready,
            "case {case:?}: a step runs after the cluster was ready to run steps ({ready}), got \
             {started} in {step}"
        );

        for (key, expected) in &row.holds {
            let held = object(&router, key).await;
            assert_eq!(
                held.as_deref().map(str::trim),
                Some(expected.as_str()),
                "case {case:?}: expected {key} to hold {expected:?}, got {held:?}"
            );
        }
    }
}
