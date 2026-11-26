// SPDX-License-Identifier: CC0-1.0

//! What a cluster an auto-termination policy's idle timer is armed on reports when a
//! termination LOCK held that shutdown off, end to end over the wire.
//!
//! THE RULE this suite asserts: the idle time an auto-termination policy counts is the idle
//! time during which that policy was free to shut the cluster down. "An auto-termination
//! policy defines the amount of idle time in seconds after which a cluster automatically
//! terminates" (`AutoTerminationPolicy` doc), while termination protection "prevent[s] the
//! Amazon EC2 instances from being terminated by an API call or user intervention"
//! (`Cluster$TerminationProtected` doc) — so a locked cluster is held steady however long its
//! idle timeout has been elapsed, every shutdown asked of it is still refused, and the window
//! the policy counts begins AGAIN when the lock is cleared. A caller that clears the lock and
//! asks for the shutdown itself therefore gets the shutdown it asked for — `USER_REQUEST`
//! with "Terminated by user request" — rather than one the policy took in front of it, and a
//! cluster left alone after the unlock is still shut down by that same policy one idle timeout
//! later: clearing a lock hands the cluster back to its timer rather than disarming it.
//!
//! The rows differ along what the rule generalises over: what happens to the lock while the
//! idle timeout elapses — never locked, locked then unlocked and shut down by the caller,
//! locked then unlocked and left alone, and locked throughout. Every cluster is launched with
//! the same policy on ONE emulator before any wait, so the suite spends the documented
//! minimum idle timeout twice rather than once per row, and each wait waits for the CONDITION
//! another cluster's read reports rather than for a span of the host's clock.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns. Nothing reaches into
//! the service's state struct: every effect is read back through `DescribeCluster`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, at, await_cluster_state, kept_alive_request, Emr};
use crate::refusal_messages::message;
use crate::terminate_job_flows_protection::PROTECTED_SENTENCE;

/// The idle timeout every cluster below carries: "a minimum of 60 seconds"
/// (`AutoTerminationPolicy$IdleTimeout` doc), which is the shortest span a policy may state
/// and so the shortest one a case can be asked to wait out.
const IDLE_TIMEOUT: u64 = 60;

/// How long a wait for a shutdown the policy owes may take before a case gives up: far above
/// the two idle timeouts and the launch spans this suite waits out, so a slow or loaded host
/// delays it rather than failing it.
const DEADLINE: Duration = Duration::from_secs(10 * 60);

/// One look at a cluster inside [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// `ClusterStatus$StateChangeReason.Message` of the shutdown a caller asked for with
/// `TerminateJobFlows`. Recorded behaviour: fixed text, with nothing of the request in it.
const USER_REQUEST_MESSAGE: &str = "Terminated by user request";

/// The modelled `ClusterStateChangeReasonCode` a shutdown that was ASKED FOR carries
/// (`smithy_query(query="shape", shape="ClusterStateChangeReasonCode")`).
const USER_REQUEST_CODE: &str = "USER_REQUEST";

/// The state-change reason codes that report a FAILURE of the cluster, from the same shape:
/// neither the policy's shutdown nor the caller's is one of them.
const FAILURE_CODES: [&str; 5] = [
    "INTERNAL_ERROR",
    "VALIDATION_ERROR",
    "INSTANCE_FAILURE",
    "INSTANCE_FLEET_TIMEOUT",
    "BOOTSTRAP_FAILURE",
];

/// A kept-alive launch carrying the policy under test, so the cluster is idle from the instant
/// it comes up and stays up unless something shuts it down.
fn launch_with_policy(name: &str) -> Value {
    let mut request = kept_alive_request(name);
    request["AutoTerminationPolicy"] = json!({ "IdleTimeout": IDLE_TIMEOUT });
    request
}

/// Poll `DescribeCluster` until the cluster reports a terminal state, and answer what it then
/// reported.
async fn await_terminal(emr: &Emr, case: &str, cluster_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let cluster = emr.describe_cluster(cluster_id).await;
        let state = at(&cluster, &["Status", "State"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "TERMINATED" || state == "TERMINATED_WITH_ERRORS" {
            return cluster;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never reached a terminal state, last {cluster}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// That `cluster` is up, resting, locked, and that nothing has shut it down: the state a
/// locked cluster keeps however long its idle timeout has been elapsed.
fn assert_still_resting_and_locked(case: &str, cluster: &Value) {
    assert_eq!(
        at(cluster, &["Status", "State"]).as_str(),
        Some("WAITING"),
        "case {case:?}: a locked cluster must stay up and resting, got {cluster}"
    );
    assert_eq!(
        cluster["TerminationProtected"].as_bool(),
        Some(true),
        "case {case:?}: the lock must still be reported, got {cluster}"
    );
    assert!(
        at(cluster, &["Status", "StateChangeReason"]).is_null(),
        "case {case:?}: nothing changed a locked cluster's state, got {cluster}"
    );
    assert!(
        at(cluster, &["Status", "Timeline", "EndDateTime"]).is_null(),
        "case {case:?}: a locked cluster has not ended, got {cluster}"
    );
}

/// The `EndDateTime` of a cluster that has ended, in the epoch seconds it is served in.
fn ended_at(case: &str, cluster: &Value) -> f64 {
    at(cluster, &["Status", "Timeline", "EndDateTime"])
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: a terminated cluster carries an EndDateTime, got {cluster}"))
}

/// The whole rule, in the order a client drives it.
#[tokio::test]
async fn a_lock_holds_the_idle_shutdown_off_and_the_window_begins_again_when_it_is_cleared() {
    let emr = Emr::new();

    // Launched together and carrying the SAME policy, so one span of idle time is waited out
    // for all four and the only difference between them is what happens to their lock.
    let unlocked = emr.run_job_flow(launch_with_policy("never-locked")).await;
    let terminated_by_caller = emr.run_job_flow(launch_with_policy("unlocked-then-asked")).await;
    let left_alone = emr.run_job_flow(launch_with_policy("unlocked-then-left")).await;
    let locked_throughout = emr.run_job_flow(launch_with_policy("locked-throughout")).await;
    let locked = [&terminated_by_caller, &left_alone, &locked_throughout];

    // Each cluster is idle from the instant it comes up: it stays up with no steps to run.
    for (case, cluster_id) in [
        ("never locked", &unlocked),
        ("unlocked then asked", &terminated_by_caller),
        ("unlocked then left", &left_alone),
        ("locked throughout", &locked_throughout),
    ] {
        await_cluster_state(&emr, cluster_id, case, "WAITING").await;
    }

    // Three of the four are locked BEFORE their idle timeout elapses, and the lock is the
    // value the very next read reports.
    emr.ok(
        "SetTerminationProtection",
        json!({ "JobFlowIds": locked, "TerminationProtected": true }),
    )
    .await;
    for cluster_id in locked {
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_still_resting_and_locked("the lock is written", &cluster);
    }

    // The CONDITION the rest of the case waits on: the cluster nothing locked is shut down by
    // its policy, which is what proves the idle timeout every one of the four carries has
    // elapsed. Nobody called `TerminateJobFlows` on it.
    let policy_shutdown = await_terminal(&emr, "never locked", &unlocked).await;
    assert_eq!(
        at(&policy_shutdown, &["Status", "State"]).as_str(),
        Some("TERMINATED"),
        "case \"never locked\": the policy's shutdown is an orderly one, got {policy_shutdown}"
    );
    let policy_code = at(&policy_shutdown, &["Status", "StateChangeReason", "Code"])
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !FAILURE_CODES.contains(&policy_code.as_str()),
        "case \"never locked\": the policy's shutdown reports no failure code, got \
         {policy_code:?} in {policy_shutdown}"
    );
    assert_ne!(
        at(&policy_shutdown, &["Status", "StateChangeReason", "Message"]).as_str(),
        Some(USER_REQUEST_MESSAGE),
        "case \"never locked\": nobody asked for this shutdown, got {policy_shutdown}"
    );
    let ready = at(&policy_shutdown, &["Status", "Timeline", "ReadyDateTime"])
        .as_f64()
        .unwrap_or_else(|| panic!("case \"never locked\": no ReadyDateTime in {policy_shutdown}"));
    let policy_ended = ended_at("never locked", &policy_shutdown);
    assert!(
        policy_ended - ready >= IDLE_TIMEOUT as f64,
        "case \"never locked\": expected at least {IDLE_TIMEOUT}s of idle time before the \
         shutdown, got {} in {policy_shutdown}",
        policy_ended - ready
    );

    // The same idle timeout has elapsed for all three locked clusters, and the lock held every
    // one of them steady: none is shutting down, none records a reason, none has ended — and a
    // shutdown asked of one is still refused with the one constant sentence rather than
    // answered for a cluster the policy took away.
    for cluster_id in locked {
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_still_resting_and_locked("the idle timeout elapsed under the lock", &cluster);
    }
    let (status, payload) = emr
        .call(
            "TerminateJobFlows",
            json!({ "JobFlowIds": [&terminated_by_caller] }),
        )
        .await;
    let case = "a shutdown asked of a long-idle locked cluster";
    assert_refused(case, status, &payload, "ValidationException");
    assert_eq!(
        message(case, &payload),
        PROTECTED_SENTENCE,
        "case {case:?}: expected {PROTECTED_SENTENCE:?}, got {payload}"
    );
    assert_still_resting_and_locked(case, &emr.describe_cluster(&terminated_by_caller).await);

    // Two of them are unlocked. The read that follows the unlock still finds each up: the
    // window the policy counts begins again here, so the unlock does not shut a long-idle
    // cluster down behind the caller that asked for it.
    emr.ok(
        "SetTerminationProtection",
        json!({
            "JobFlowIds": [&terminated_by_caller, &left_alone],
            "TerminationProtected": false,
        }),
    )
    .await;
    for (case, cluster_id) in [
        ("unlocked then asked", &terminated_by_caller),
        ("unlocked then left", &left_alone),
    ] {
        let cluster = emr.describe_cluster(cluster_id).await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some("WAITING"),
            "case {case:?}: the unlocked cluster must still be up, got {cluster}"
        );
        assert_eq!(
            cluster["TerminationProtected"].as_bool(),
            Some(false),
            "case {case:?}: the cleared lock must be reported, got {cluster}"
        );
        assert!(
            at(&cluster, &["Status", "StateChangeReason"]).is_null(),
            "case {case:?}: clearing the lock must not shut the cluster down, got {cluster}"
        );
    }

    // So the shutdown the caller now asks for is the shutdown the cluster records.
    emr.ok(
        "TerminateJobFlows",
        json!({ "JobFlowIds": [&terminated_by_caller] }),
    )
    .await;
    let asked = emr.describe_cluster(&terminated_by_caller).await;
    assert_eq!(
        at(&asked, &["Status", "State"]).as_str(),
        Some("TERMINATING"),
        "case \"unlocked then asked\": the unlocked cluster must start shutting down, got {asked}"
    );
    for (member, expected) in [
        ("Code", USER_REQUEST_CODE),
        ("Message", USER_REQUEST_MESSAGE),
    ] {
        assert_eq!(
            at(&asked, &["Status", "StateChangeReason", member]).as_str(),
            Some(expected),
            "case \"unlocked then asked\": the reason's {member} must be {expected:?}, got {asked}"
        );
    }
    let gone = await_terminal(&emr, "unlocked then asked", &terminated_by_caller).await;
    assert_eq!(
        at(&gone, &["Status", "State"]).as_str(),
        Some("TERMINATED"),
        "case \"unlocked then asked\": the cluster must reach TERMINATED, got {gone}"
    );
    for (member, expected) in [
        ("Code", USER_REQUEST_CODE),
        ("Message", USER_REQUEST_MESSAGE),
    ] {
        assert_eq!(
            at(&gone, &["Status", "StateChangeReason", member]).as_str(),
            Some(expected),
            "case \"unlocked then asked\": the terminal reason's {member} must be \
             {expected:?}, got {gone}"
        );
    }
    let _ = ended_at("unlocked then asked", &gone);

    // And the one left alone after the unlock is still shut down by its policy: the lock handed
    // the cluster back to the timer rather than disarming it, and the window that timer counted
    // began at the unlock — which is already past the instant the cluster nothing locked ended
    // at, so this one ends a whole further idle timeout after that.
    let left = await_terminal(&emr, "unlocked then left", &left_alone).await;
    assert_eq!(
        at(&left, &["Status", "State"]).as_str(),
        Some("TERMINATED"),
        "case \"unlocked then left\": the policy's shutdown is an orderly one, got {left}"
    );
    assert_ne!(
        at(&left, &["Status", "StateChangeReason", "Message"]).as_str(),
        Some(USER_REQUEST_MESSAGE),
        "case \"unlocked then left\": nobody asked for this shutdown, got {left}"
    );
    let left_ended = ended_at("unlocked then left", &left);
    assert!(
        left_ended - policy_ended >= IDLE_TIMEOUT as f64,
        "case \"unlocked then left\": expected the shutdown at least {IDLE_TIMEOUT}s after \
         {policy_ended} (the unlock came after that instant), got {left_ended} in {left}"
    );

    // The cluster whose lock was never cleared is still up, having been idle through both of
    // those spans: the lock is what holds it, and it holds for as long as it is set.
    assert_still_resting_and_locked(
        "locked throughout",
        &emr.describe_cluster(&locked_throughout).await,
    );
}
