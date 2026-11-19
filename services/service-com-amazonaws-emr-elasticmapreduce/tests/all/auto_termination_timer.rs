// SPDX-License-Identifier: CC0-1.0

//! An auto-termination policy is a TIMER: an idle cluster carrying one shuts itself down.
//!
//! THE RULE under test: "an auto-termination policy defines the amount of idle time in seconds
//! after which a cluster automatically terminates" (`AutoTerminationPolicy` doc), and
//! "`IdleTimeout` specifies the amount of idle time in seconds after which the cluster
//! automatically terminates. You can specify a minimum of 60 seconds and a maximum of 604800
//! seconds (seven days)" (`AutoTerminationPolicy$IdleTimeout` doc). So a cluster carrying a
//! policy and running nothing of its own reaches the terminal `TERMINATED` with NOBODY calling
//! `TerminateJobFlows`, no sooner than that many seconds after it fell idle — the end of its
//! last step, or the instant it became ready when it ran none — and the shutdown is an orderly
//! one, not a failure of the cluster. A cluster carrying no policy, or one whose policy
//! `RemoveAutoTerminationPolicy` took off, stays up.
//!
//! The rows span what the rule generalises over: a cluster idle from the instant it came up, a
//! cluster idle from the end of a step it ran, a cluster whose policy was attached and then
//! removed, and a cluster that never carried one. Every row runs on ONE emulator and is
//! launched before any wait, so the suite spends one idle timeout rather than one per row.
//!
//! The timeout itself is the DOCUMENTED minimum, which is the shortest a policy may state, and
//! every wait waits for the CONDITION — `DescribeCluster` reporting a terminal state — under a
//! deadline far above it, so a slow host delays the suite rather than failing it.

use std::time::Duration;

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, kept_alive_request, step_config, Emr};

/// The policy's own idle timeout: "a minimum of 60 seconds"
/// (`AutoTerminationPolicy$IdleTimeout` doc), which is the shortest span a cluster of this
/// suite can be asked to wait out.
const IDLE_TIMEOUT: u64 = 60;

/// How long a row waits for the shutdown the timer owes: far above the idle timeout, the
/// launch spans and the step span this service walks a cluster through, so the deadline is
/// never what decides a row.
const DEADLINE: Duration = Duration::from_secs(4 * 60 + IDLE_TIMEOUT * 3);

/// One poll of `DescribeCluster` inside [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The state a cluster that is up and taking no work rests in.
const WAITING: &str = "WAITING";

/// The terminal state an orderly shutdown ends in.
const TERMINATED: &str = "TERMINATED";

/// The state-change reason codes that report a FAILURE of the cluster
/// (`smithy_query(query="shape", shape="ClusterStateChangeReasonCode")`): the idle shutdown is
/// not one of them.
const FAILURE_CODES: [&str; 5] = [
    "INTERNAL_ERROR",
    "VALIDATION_ERROR",
    "INSTANCE_FAILURE",
    "INSTANCE_FLEET_TIMEOUT",
    "BOOTSTRAP_FAILURE",
];

/// A kept-alive launch carrying `policy` as its `AutoTerminationPolicy`, and `steps` steps.
fn launch(name: &str, idle_timeout: Option<u64>, steps: Vec<Value>) -> Value {
    let mut request = kept_alive_request(name);
    if let Some(idle_timeout) = idle_timeout {
        request["AutoTerminationPolicy"] = json!({ "IdleTimeout": idle_timeout });
    }
    if !steps.is_empty() {
        request["Steps"] = Value::Array(steps);
    }
    request
}

/// Poll `DescribeCluster` until the cluster reports a terminal state, and answer what it then
/// reported.
async fn await_terminal(emr: &Emr, case: &str, cluster_id: &str) -> Value {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let cluster = emr.describe_cluster(cluster_id).await;
        let state = at(&cluster, &["Status", "State"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == TERMINATED || state == "TERMINATED_WITH_ERRORS" {
            return cluster;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the cluster never reached a terminal state, last {cluster}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `ListSteps` until every step of the cluster has ended, answering the latest
/// `EndDateTime` among them.
async fn await_steps_ended(emr: &Emr, case: &str, cluster_id: &str) -> f64 {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
        let rows = at(&listed, &["Steps"]).as_array().cloned().unwrap_or_default();
        let ends: Vec<f64> = rows
            .iter()
            .filter_map(|step| at(step, &["Status", "Timeline", "EndDateTime"]).as_f64())
            .collect();
        if !rows.is_empty() && ends.len() == rows.len() {
            return ends.into_iter().fold(f64::MIN, f64::max);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the cluster's steps never ended, last {listed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The state of a cluster right now.
async fn state_of(emr: &Emr, cluster_id: &str) -> String {
    let cluster = emr.describe_cluster(cluster_id).await;
    at(&cluster, &["Status", "State"])
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn an_idle_cluster_carrying_an_auto_termination_policy_terminates_itself() {
    let emr = Emr::new();

    // Idle from the instant it came up: the policy is attached by the launch.
    let idle = emr
        .run_job_flow(launch("idle", Some(IDLE_TIMEOUT), Vec::new()))
        .await;
    // Idle from the end of the step it ran: the same policy, and one step to run first.
    let after_step = emr
        .run_job_flow(launch(
            "after-step",
            Some(IDLE_TIMEOUT),
            vec![step_config(
                "noop",
                "s3://emr/jars/noop.jar",
                Value::Null,
                Value::Null,
            )],
        ))
        .await;
    // A policy attached after the launch and then taken off again: no timer is left armed.
    let removed = emr.run_job_flow(launch("removed", None, Vec::new())).await;
    emr.ok(
        "PutAutoTerminationPolicy",
        json!({
            "ClusterId": &removed,
            "AutoTerminationPolicy": { "IdleTimeout": IDLE_TIMEOUT },
        }),
    )
    .await;
    emr.ok(
        "RemoveAutoTerminationPolicy",
        json!({ "ClusterId": &removed }),
    )
    .await;
    // And a cluster that never carried one.
    let unattached = emr
        .run_job_flow(launch("unattached", None, Vec::new()))
        .await;

    // The policy a launch attached is the policy the cluster carries, which is what arms the
    // timer below.
    let attached = emr
        .ok("GetAutoTerminationPolicy", json!({ "ClusterId": &idle }))
        .await;
    assert_eq!(
        at(&attached, &["AutoTerminationPolicy", "IdleTimeout"]).as_u64(),
        Some(IDLE_TIMEOUT),
        "case {:?}: expected the launched policy to be reported, got {attached}",
        "idle"
    );
    let cleared = emr
        .ok("GetAutoTerminationPolicy", json!({ "ClusterId": &removed }))
        .await;
    assert!(
        at(&cleared, &["AutoTerminationPolicy"]).is_null(),
        "case {:?}: expected no policy after the removal, got {cleared}",
        "removed"
    );

    // The step of the second cluster runs first, so the instant that cluster fell idle is the
    // instant its step ended.
    let step_ended = await_steps_ended(&emr, "after-step", &after_step).await;

    // NOBODY calls TerminateJobFlows: the idle cluster shuts itself down.
    for (case, cluster_id, idle_since) in [("idle", &idle, None), ("after-step", &after_step, Some(step_ended))] {
        let terminal = await_terminal(&emr, case, cluster_id).await;
        assert_eq!(
            at(&terminal, &["Status", "State"]).as_str(),
            Some(TERMINATED),
            "case {case:?}: an orderly shutdown ends TERMINATED, got {terminal}"
        );
        let code = at(&terminal, &["Status", "StateChangeReason", "Code"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !FAILURE_CODES.contains(&code.as_str()),
            "case {case:?}: the shutdown must not report a failure code, got {code:?} in \
             {terminal}"
        );
        let ready = at(&terminal, &["Status", "Timeline", "ReadyDateTime"])
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: no ReadyDateTime in {terminal}"));
        let ended = at(&terminal, &["Status", "Timeline", "EndDateTime"])
            .as_f64()
            .unwrap_or_else(|| panic!("case {case:?}: no EndDateTime in {terminal}"));
        let idle_since = idle_since.unwrap_or(ready);
        let idle_for = ended - idle_since;
        assert!(
            idle_for >= IDLE_TIMEOUT as f64,
            "case {case:?}: expected at least {IDLE_TIMEOUT}s of idle time before the \
             shutdown, got {idle_for} in {terminal}"
        );
        assert!(
            ended >= ready,
            "case {case:?}: EndDateTime {ended} must follow ReadyDateTime {ready}"
        );
    }

    // And the clusters carrying no policy are still up, having been idle the whole of that
    // same span.
    for (case, cluster_id) in [("removed", &removed), ("unattached", &unattached)] {
        let state = state_of(&emr, cluster_id).await;
        assert_eq!(
            state, WAITING,
            "case {case:?}: a cluster carrying no policy stays up, got {state:?}"
        );
    }
}
