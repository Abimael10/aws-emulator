// SPDX-License-Identifier: CC0-1.0

//! The whole post-mortem sequence a client runs against a cluster launched with a `LogUri`:
//! submit a step, wait for it to end, let the cluster settle, shut it down, and then read the
//! cluster's logs out of Amazon S3.
//!
//! THE RULE: a step submitted with a cluster reaches a terminal `StepState` — "Amazon EMR
//! executes each step in the order listed. For a step to be considered complete, the main
//! function must exit with a zero exit code" (`AddJobFlowSteps` doc) — a cluster that was asked
//! to stay up rests in `WAITING` once its queue has run and a cluster that was not shuts itself
//! down ("If the `JobFlowInstancesConfig` `KeepJobFlowAliveWhenNoSteps` parameter is set to
//! TRUE, the cluster transitions to the `WAITING` state rather than shutting down after the
//! steps have completed", `RunJobFlow` doc), and the log files written under the launch's
//! `LogUri` ("the location in Amazon S3 to write the log files of the job flow",
//! `RunJobFlow$LogUri` doc) are OBJECTS of the object store: they belong to the bucket and not
//! to the cluster, so they outlive the cluster that produced them — a client that terminates a
//! cluster and then lists `<LogUri>/<cluster-id>/` still finds every file, unchanged, however
//! many times it looks and whether or not anything is still reporting the cluster.
//!
//! The rows span what the rule generalises over: how the step's work ENDED (a clean exit and a
//! non-zero status), how many steps the launch carried, and WHAT ended the cluster — a caller's
//! `TerminateJobFlows` on a cluster that was resting in `WAITING`, or a transient cluster's own
//! shutdown once its queue had run.
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine — through the one wire harness this
//! crate's suites share ([`crate::spark_step_application`]) rather than a second one — and every
//! log file is read back through S3's own `ListObjectsV2` and `GetObject`, so what is asserted is
//! what a client's own reads find. Nothing here waits a fixed time: every phase of a cluster and
//! every step's span is a stored due instant, so each wait LOOKS AGAIN until the condition holds
//! or the generous [`DEADLINE`] passes.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::{
    command_failure_message, COMMAND_FAILED_REASON, STDERR_LOG_OBJECT, STEPS_SEGMENT,
    STEP_LOG_OBJECTS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::spark_step_application::{emr_ok, emulator, s3, BUCKET};

/// The key prefix the launches' `LogUri` names inside the suite's bucket.
const LOG_PREFIX: &str = "logs";

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// How long a poll waits for a condition: both provisioning phases of a cluster, its steps'
/// spans and the shutdown that follows, with room to spare on a loaded host.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a cluster never leaves (`com.amazonaws.emr#ClusterState`).
const TERMINAL_CLUSTER: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// The states a cluster that is still coming up reports (`com.amazonaws.emr#ClusterState`): a
/// client waiting for a cluster to settle waits for the state to LEAVE these.
const COMING_UP: [&str; 3] = ["STARTING", "BOOTSTRAPPING", "RUNNING"];

/// How many times a client re-reads the destination after the cluster is gone. CHOSEN: more
/// than one, so "the files are still there" is a repeated answer rather than a single look.
const PERSISTENCE_REREADS: usize = 4;

/// What a row's step prints, which is the whole of its work.
const MARKER: &str = "emr-log-persistence-marker";

/// The status the command of a row that gives up exits with. CHOSEN: a status of the command's
/// OWN, so the step's failure cannot be read as the status a session gives a command it could
/// not run at all.
const GIVING_UP_STATUS: i32 = 3;

/// The keys of every object under `prefix`, from `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> BTreeSet<String> {
    let (status, listing) = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
    )
    .await;
    let listing = String::from_utf8_lossy(&listing).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing s3://{BUCKET}/{prefix} must succeed, got {status} with {listing}"
    );
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</Key>").map(|(key, _)| key.to_string()))
        .collect()
}

/// One row: what the launch carried and how the cluster ends.
struct Case {
    label: &'static str,
    /// Whether the launch asked the cluster to stay up with no steps.
    keep_alive: bool,
    /// The command each step of the launch submits, and the state its exit status ends it in.
    steps: Vec<(&'static str, &'static str)>,
    /// The state the cluster rests in once its queue has run, for a launch that asked it to
    /// stay up: a cluster that did not is already shutting itself down, so it rests in none.
    resting_state: Option<&'static str>,
    /// Whether the caller is what ends the cluster (`TerminateJobFlows`), or the cluster's own
    /// shutdown is.
    terminated_by_caller: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a kept-alive cluster the caller terminates",
            keep_alive: true,
            steps: vec![(
                // A command whose work is a line of its own output, which exits zero.
                "echo",
                "COMPLETED",
            )],
            resting_state: Some("WAITING"),
            terminated_by_caller: true,
        },
        Case {
            label: "a kept-alive cluster whose step exited non-zero",
            keep_alive: true,
            // A command that exits with a status of its own fails its step, and a failed step
            // leaves its log directory exactly as a completed one does.
            steps: vec![("exit", "FAILED")],
            resting_state: Some("WAITING"),
            terminated_by_caller: true,
        },
        Case {
            label: "a kept-alive cluster carrying two steps",
            keep_alive: true,
            steps: vec![("echo", "COMPLETED"), ("exit", "FAILED")],
            resting_state: Some("WAITING"),
            terminated_by_caller: true,
        },
        Case {
            label: "a transient cluster that shut itself down",
            keep_alive: false,
            steps: vec![("echo", "COMPLETED")],
            // "After the steps complete, the cluster stops" (`RunJobFlow` doc): there is no
            // resting state to observe, the cluster goes down of its own accord.
            resting_state: None,
            terminated_by_caller: false,
        },
    ]
}

/// The command line one row's step submits, in the `command-runner.jar` form a client uses to
/// "run commands on your cluster".
fn command_of(kind: &str, ordinal: usize) -> Vec<String> {
    let program = match kind {
        "echo" => format!("echo {MARKER}-{ordinal}"),
        _ => format!("exit {GIVING_UP_STATUS}"),
    };
    vec!["bash".to_string(), "-c".to_string(), program]
}

/// The `StepConfig` one of a row's steps submits.
fn step_config(case: &Case, kind: &str, ordinal: usize) -> Value {
    json!({
        "Name": format!("{} #{ordinal}", case.label),
        // "CONTINUE — Continues to the next step in the queue"
        // (`StepConfig$ActionOnFailure` doc): a step that fails neither cancels the queue
        // behind it nor takes the cluster down, so the rows differ in the step's own status
        // alone.
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": {
            "Jar": "command-runner.jar",
            "Args": command_of(kind, ordinal),
        },
    })
}

/// Launch one row's cluster with the `LogUri` the rule is about, answering its id.
async fn launch(router: &MultiServiceRouter, case: &Case) -> String {
    let steps: Vec<Value> = case
        .steps
        .iter()
        .enumerate()
        .map(|(ordinal, (kind, _))| step_config(case, kind, ordinal))
        .collect();
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": case.label,
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": "Hadoop" }],
            "LogUri": format!("s3://{BUCKET}/{LOG_PREFIX}/"),
            "Steps": steps,
            "StepConcurrencyLevel": 1,
            "Instances": {
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": case.keep_alive,
            },
        }),
    )
    .await;
    launched["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering them in submission
/// order.
async fn settled_steps(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
        // "The list of steps for the cluster … in reverse order" (`ListSteps` doc): the
        // submission order is that listing read backwards.
        let mut steps: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
        steps.reverse();
        let states: Vec<String> = steps
            .iter()
            .map(|step| {
                step["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if !states.is_empty()
            && states
                .iter()
                .all(|state| TERMINAL_STEP.contains(&state.as_str()))
        {
            return steps;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The state `DescribeCluster` reports for `cluster_id` right now.
async fn cluster_state(router: &MultiServiceRouter, cluster_id: &str) -> String {
    let described = emr_ok(router, "DescribeCluster", json!({ "ClusterId": cluster_id })).await;
    described["Cluster"]["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Poll `DescribeCluster` until the cluster's state satisfies `settled`, answering that state.
async fn awaited_cluster_state<F: Fn(&str) -> bool>(
    case: &str,
    router: &MultiServiceRouter,
    cluster_id: &str,
    wanted: &str,
    settled: F,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = cluster_state(router, cluster_id).await;
        if settled(&state) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cluster to reach {wanted}, got {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `ListObjectsV2` until the cluster's log destination holds a file, answering every key
/// it holds then.
async fn delivered_keys(
    case: &str,
    router: &MultiServiceRouter,
    cluster_id: &str,
    prefix: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let keys = keys_under(router, case, prefix).await;
        if !keys.is_empty() {
            return keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected log files under {prefix}, got none"
        );
        // The delivery is owed from the instant a step ended, and whichever call reports the
        // cluster next is what drains it — which is what a client's own poll does.
        emr_ok(router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every row, end to end: the steps settle, the cluster settles, the log files land under the
/// launch's `LogUri`, and they are all still there — unchanged, read after read — once the
/// cluster has reached a state it never leaves.
#[tokio::test]
async fn a_terminated_cluster_leaves_its_log_files_in_amazon_s3() {
    let router = emulator();
    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );

    // Every row's cluster is launched FIRST, so the phases the clock owes them all run
    // together rather than one row's waiting standing in front of the next row's launch.
    let cases = cases();
    let mut launched: Vec<String> = Vec::new();
    for case in &cases {
        launched.push(launch(&router, case).await);
    }

    for (case, cluster_id) in cases.iter().zip(launched) {
        let label = case.label;
        let prefix = format!("{LOG_PREFIX}/{cluster_id}/");

        // (1) Every submitted step reaches the terminal state its own work answered.
        let settled = settled_steps(label, &router, &cluster_id).await;
        assert_eq!(
            settled.len(),
            case.steps.len(),
            "case {label:?}: expected one step per submitted step back, got {settled:?}"
        );
        for ((kind, expected), step) in case.steps.iter().zip(&settled) {
            assert_eq!(
                step["Status"]["State"].as_str(),
                Some(*expected),
                "case {label:?}: expected the {kind:?} step to end {expected}, got {step}"
            );
        }

        // (2) A cluster asked to stay up rests in WAITING once its queue has run.
        if let Some(resting) = case.resting_state {
            let state = awaited_cluster_state(label, &router, &cluster_id, resting, |state| {
                !COMING_UP.contains(&state)
            })
            .await;
            assert_eq!(
                state, resting,
                "case {label:?}: expected the cluster to rest in {resting}, got {state:?}"
            );
        }

        // (3) The log files are there while the cluster still is: one directory per step that
        // ran, holding exactly the files a step's run produces.
        let before = delivered_keys(label, &router, &cluster_id, &prefix).await;
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for step in &settled {
            let step_id = step["Id"].as_str().unwrap_or_default();
            for name in STEP_LOG_OBJECTS {
                expected.insert(format!("{prefix}{STEPS_SEGMENT}/{step_id}/{name}"));
            }
        }
        assert_eq!(
            before, expected,
            "case {label:?}: expected exactly {expected:?} under {prefix}, got {before:?}"
        );

        // A step whose command exited non-zero says so in the two members the model gives a
        // step failure, and points at the log file it really left — read back after the
        // delivery, which is when that path exists. A step that completed claims no failure.
        let reported = settled_steps(label, &router, &cluster_id).await;
        for ((kind, expected_state), step) in case.steps.iter().zip(&reported) {
            let step_id = step["Id"].as_str().unwrap_or_default();
            let details = &step["Status"]["FailureDetails"];
            if *expected_state != "FAILED" {
                assert!(
                    details.is_null(),
                    "case {label:?}: the {kind:?} step completed, so it reports no failure, \
                     got {step}"
                );
                continue;
            }
            let message = command_failure_message(GIVING_UP_STATUS);
            let log_file = format!("s3://{BUCKET}/{prefix}{STEPS_SEGMENT}/{step_id}/{STDERR_LOG_OBJECT}");
            assert_eq!(
                details["Reason"].as_str(),
                Some(COMMAND_FAILED_REASON),
                "case {label:?}: expected the {kind:?} step to fail on its command's status, \
                 got {step}"
            );
            assert_eq!(
                details["Message"].as_str(),
                Some(message.as_str()),
                "case {label:?}: expected the {kind:?} step to report the status its command \
                 exited with, got {step}"
            );
            assert_eq!(
                details["LogFile"].as_str(),
                Some(log_file.as_str()),
                "case {label:?}: expected the {kind:?} step to report the log file it left, \
                 got {step}"
            );
        }

        // (4) The cluster goes down — by the caller's request, or of its own accord — and
        // reaches the state it never leaves.
        if case.terminated_by_caller {
            emr_ok(
                &router,
                "TerminateJobFlows",
                json!({ "JobFlowIds": [cluster_id.clone()] }),
            )
            .await;
        }
        let ended = awaited_cluster_state(label, &router, &cluster_id, "TERMINATED", |state| {
            TERMINAL_CLUSTER.contains(&state)
        })
        .await;
        assert_eq!(
            ended, "TERMINATED",
            "case {label:?}: expected the cluster to end TERMINATED, got {ended:?}"
        );

        // (5) The files OUTLIVE the cluster: every re-read answers the same set, and each file
        // is still a gzip stream a reader can fetch — with nothing reporting the cluster in
        // between, because the objects belong to the bucket and not to the cluster.
        for round in 0..PERSISTENCE_REREADS {
            let after = keys_under(&router, label, &prefix).await;
            assert_eq!(
                after, before,
                "case {label:?}: re-read {round} after termination must answer the same files, \
                 expected {before:?}, got {after:?}"
            );
        }
        for key in &before {
            let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
            assert!(
                (200..300).contains(&status),
                "case {label:?}: {key} must still be served after termination, got {status}"
            );
            assert_eq!(
                body.get(..2),
                Some([0x1f, 0x8b].as_slice()),
                "case {label:?}: {key} must still be a gzip stream, got {:?}",
                body.get(..2)
            );
        }
        // And the cluster really is gone, read back through the operation a client polls.
        let state = cluster_state(&router, &cluster_id).await;
        assert_eq!(
            state, "TERMINATED",
            "case {label:?}: a terminated cluster stays terminated, got {state:?}"
        );
    }
}
