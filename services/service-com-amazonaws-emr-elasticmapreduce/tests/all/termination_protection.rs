// SPDX-License-Identifier: CC0-1.0

//! `SetTerminationProtection`, end to end over the wire.
//!
//! THE RULE this suite asserts: `SetTerminationProtection` resolves every id in its
//! `JobFlowIds` batch against the service's own clusters, refuses the whole batch when
//! one id names no cluster, and otherwise writes the requested `TerminationProtected`
//! value on each named cluster — so a later `DescribeCluster` reports exactly that value
//! and `TerminateJobFlows` refuses to shut a locked cluster down until a later call
//! unlocks it.
//!
//! The last two cases carry that rule onto a cluster that is already UP: the write is a
//! mutation on live cluster state, so the FIRST `DescribeCluster` after the call returns
//! already reports it — a client polling for the lock it just wrote is satisfied by that
//! read — every later read answers the same, and the lock survives the transitions the
//! clock owes the cluster, so a locked cluster stays up and keeps its lock until a later
//! call clears it.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns. Nothing reaches
//! into the service's state struct: what was written is only ever read back through
//! `DescribeCluster` and through what `TerminateJobFlows` does next.

use std::time::Duration;

use serde_json::{json, Value};

use crate::emr_control_plane::{MAX_JOB_FLOW_IDS_PER_REQUEST, UNKNOWN_CLUSTER_IDS};
use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_state, kept_alive_request, minimal_request, Emr,
};
use crate::refusal_messages::message;
use crate::terminate_job_flows_protection::PROTECTED_SENTENCE;

/// Longer than the service's clock-owed transition span (500 ms), so a cluster that was
/// `TERMINATING` has reached `TERMINATED` by the time we look.
///
/// `pub(crate)` so a sibling suite waits out the same span through one constant rather than
/// its own copy.
pub(crate) const PAST_TRANSITION: Duration = Duration::from_millis(900);

/// The states a cluster that nothing terminated can be reported in: it was launched
/// `STARTING` and the clock owes it `WAITING` (it is kept alive with no steps) or
/// `RUNNING` (`com.amazonaws.emr#ClusterState`).
const NOT_SHUTTING_DOWN: [&str; 3] = ["STARTING", "WAITING", "RUNNING"];

/// A `RunJobFlow` request launching a cluster with `TerminationProtected` already set to
/// `protected`, so a row can assert the write both as a change and as a no-op.
///
/// `pub(crate)` so the sibling suite that spans the refusal launches a locked cluster
/// through this one helper rather than its own copy.
pub(crate) fn launched_with_protection(name: &str, protected: bool) -> serde_json::Value {
    json!({
        "Name": name,
        "JobFlowRole": crate::job_flow_lifecycle::INSTANCE_PROFILE, "ServiceRole": crate::job_flow_lifecycle::SERVICE_ROLE,
        "Instances": {
            "InstanceCount": 1,
            "MasterInstanceType": "m5.xlarge",
            "KeepJobFlowAliveWhenNoSteps": true,
            "TerminationProtected": protected,
        },
    })
}

/// `SetTerminationProtection` writes the lock on every cluster it names, and
/// `DescribeCluster` reports what it wrote. The table spans the value the cluster was
/// LAUNCHED with, the requested value, the batch size and a repeat of the same value.
#[tokio::test]
async fn set_termination_protection_writes_what_describe_cluster_reports() {
    let emr = Emr::new();
    let launched_off = emr.run_job_flow(launched_with_protection("off", false)).await;
    let launched_on = emr.run_job_flow(launched_with_protection("on", true)).await;
    // A cluster whose request names no `TerminationProtected` at all, and a transient
    // one, so the write is asserted on each launch form the operation can be called on.
    let defaulted = emr.run_job_flow(kept_alive_request("defaulted")).await;
    let transient = emr.run_job_flow(minimal_request("transient")).await;

    for (label, ids, requested) in [
        ("one cluster, locked", vec![launched_off.clone()], true),
        ("one cluster, unlocked", vec![launched_on.clone()], false),
        (
            "the same cluster, locked again",
            vec![launched_on.clone()],
            false,
        ),
        (
            "two clusters at once, both locked",
            vec![defaulted.clone(), transient.clone()],
            true,
        ),
        (
            "four clusters at once, all unlocked",
            vec![
                launched_off.clone(),
                launched_on.clone(),
                defaulted.clone(),
                transient.clone(),
            ],
            false,
        ),
        (
            "the same four, all locked",
            vec![
                launched_off.clone(),
                launched_on.clone(),
                defaulted.clone(),
                transient.clone(),
            ],
            true,
        ),
    ] {
        emr.ok(
            "SetTerminationProtection",
            json!({ "JobFlowIds": ids, "TerminationProtected": requested }),
        )
        .await;
        for id in &ids {
            let cluster = emr.describe_cluster(id).await;
            assert_eq!(
                cluster["TerminationProtected"].as_bool(),
                Some(requested),
                "case {label:?}: DescribeCluster must report the written value for {id}, \
                 got {cluster}"
            );
        }
    }

    // A cluster the request does not name keeps the value it holds: the last row above
    // locked all four, so locking only one of them leaves the others locked, and
    // unlocking only one leaves the others locked too.
    emr.ok(
        "SetTerminationProtection",
        json!({ "JobFlowIds": [&launched_off], "TerminationProtected": false }),
    )
    .await;
    for (id, expected) in [
        (&launched_off, false),
        (&launched_on, true),
        (&defaulted, true),
        (&transient, true),
    ] {
        let cluster = emr.describe_cluster(id).await;
        assert_eq!(
            cluster["TerminationProtected"].as_bool(),
            Some(expected),
            "case \"unnamed cluster\": {id} must still report {expected}, got {cluster}"
        );
    }
}

/// THE RULE's consequence: a locked cluster cannot be shut down by `TerminateJobFlows`,
/// and "to terminate a cluster that has been locked by setting SetTerminationProtection to
/// true, you must first unlock the job flow by a subsequent call to
/// SetTerminationProtection in which you set the value to false"
/// (`SetTerminationProtection` doc) — after which the same call shuts it down.
#[tokio::test]
async fn termination_protection_blocks_terminate_job_flows_until_it_is_unlocked() {
    let emr = Emr::new();

    // The rows differ in HOW the cluster came to be locked: by its launch request, or by
    // this operation on a cluster that launched unlocked.
    for (label, request, lock_first) in [
        ("locked at launch", launched_with_protection("at-launch", true), false),
        ("locked by the API call", kept_alive_request("by-api"), true),
    ] {
        let cluster_id = emr.run_job_flow(request).await;
        if lock_first {
            emr.ok(
                "SetTerminationProtection",
                json!({ "JobFlowIds": [&cluster_id], "TerminationProtected": true }),
            )
            .await;
        }

        let (status, payload) = emr
            .call("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
            .await;
        assert_refused(label, status, &payload, "ValidationException");
        tokio::time::sleep(PAST_TRANSITION).await;
        let locked = emr.describe_cluster(&cluster_id).await;
        let state = at(&locked, &["Status", "State"]).as_str().unwrap_or_default();
        assert!(
            NOT_SHUTTING_DOWN.contains(&state),
            "case {label:?}: a locked cluster must not be terminated by an API call, \
             got state {state:?} in {locked}"
        );
        assert!(
            at(&locked, &["Status", "StateChangeReason"]).is_null(),
            "case {label:?}: nothing changed a locked cluster's state, got {locked}"
        );
        assert!(
            at(&locked, &["Status", "Timeline", "EndDateTime"]).is_null(),
            "case {label:?}: a locked cluster has not ended, got {locked}"
        );

        // Unlocking it is what lets the same call through.
        emr.ok(
            "SetTerminationProtection",
            json!({ "JobFlowIds": [&cluster_id], "TerminationProtected": false }),
        )
        .await;
        emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
            .await;
        let shutting_down = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            at(&shutting_down, &["Status", "State"]).as_str(),
            Some("TERMINATING"),
            "case {label:?}: an unlocked cluster must start shutting down, got {shutting_down}"
        );
        assert_eq!(
            at(&shutting_down, &["Status", "StateChangeReason", "Code"]).as_str(),
            Some("USER_REQUEST"),
            "case {label:?}: the reason must be USER_REQUEST, got {shutting_down}"
        );

        tokio::time::sleep(PAST_TRANSITION).await;
        let gone = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            at(&gone, &["Status", "State"]).as_str(),
            Some("TERMINATED"),
            "case {label:?}: the unlocked cluster must reach TERMINATED, got {gone}"
        );
        assert!(
            !at(&gone, &["Status", "Timeline", "EndDateTime"]).is_null(),
            "case {label:?}: a terminated cluster carries an EndDateTime, got {gone}"
        );

        // Locking a cluster that is already gone changes neither its state nor the fact
        // that the lock is the cluster's own setting.
        emr.ok(
            "SetTerminationProtection",
            json!({ "JobFlowIds": [&cluster_id], "TerminationProtected": true }),
        )
        .await;
        let after = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            at(&after, &["Status", "State"]).as_str(),
            Some("TERMINATED"),
            "case {label:?}: locking a terminated cluster must not revive it, got {after}"
        );
        assert_eq!(
            after["TerminationProtected"].as_bool(),
            Some(true),
            "case {label:?}: the written lock must still be reported, got {after}"
        );
    }
}

/// An id no cluster carries refuses the WHOLE batch with the 400 `ValidationException`
/// channel (this operation's model declares no client error) and changes nothing,
/// however many known ids the same request carries.
#[tokio::test]
async fn set_termination_protection_refuses_an_unknown_job_flow_id() {
    let emr = Emr::new();
    let known = emr.run_job_flow(kept_alive_request("known")).await;

    for unknown in UNKNOWN_CLUSTER_IDS {
        let (status, payload) = emr
            .call(
                "SetTerminationProtection",
                json!({
                    "JobFlowIds": [&known, unknown],
                    "TerminationProtected": true,
                }),
            )
            .await;
        assert_refused(
            &format!("job flow id {unknown:?}"),
            status,
            &payload,
            "ValidationException",
        );
        let cluster = emr.describe_cluster(&known).await;
        assert_eq!(
            cluster["TerminationProtected"].as_bool(),
            Some(false),
            "case {unknown:?}: a refused request must change nothing, got {cluster}"
        );
    }
}

/// A batch of job flow ids is capped at the same number `TerminateJobFlows` publishes
/// for its identical member ("The maximum number of clusters allowed is 10"), since this
/// operation's own model publishes none: a batch AT the bound is accepted and one past it
/// is refused before any cluster is touched.
#[tokio::test]
async fn set_termination_protection_caps_the_batch_of_job_flow_ids() {
    let emr = Emr::new();
    let cluster_id = emr.run_job_flow(kept_alive_request("batched")).await;

    let at_the_bound: Vec<&String> = (0..MAX_JOB_FLOW_IDS_PER_REQUEST)
        .map(|_| &cluster_id)
        .collect();
    emr.ok(
        "SetTerminationProtection",
        json!({ "JobFlowIds": at_the_bound, "TerminationProtected": true }),
    )
    .await;
    let cluster = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        cluster["TerminationProtected"].as_bool(),
        Some(true),
        "case \"at the maximum\": the batch must be applied, got {cluster}"
    );

    let past_the_bound: Vec<&String> = (0..=MAX_JOB_FLOW_IDS_PER_REQUEST)
        .map(|_| &cluster_id)
        .collect();
    let (status, payload) = emr
        .call(
            "SetTerminationProtection",
            json!({ "JobFlowIds": past_the_bound, "TerminationProtected": false }),
        )
        .await;
    assert_refused(
        "one job flow id past the maximum",
        status,
        &payload,
        "ValidationException",
    );
    let cluster = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        cluster["TerminationProtected"].as_bool(),
        Some(true),
        "case \"past the maximum\": a refused batch must change nothing, got {cluster}"
    );
}

// ---------------------------------------------------------------------------
// The lock on a cluster that is already up
// ---------------------------------------------------------------------------

/// The states a live cluster nothing shut down is reported in: it is kept alive with no
/// steps, so the clock owes it `WAITING`, and one with work on it reports `RUNNING`
/// (`com.amazonaws.emr#ClusterState`).
const STILL_UP: [&str; 2] = ["WAITING", "RUNNING"];

/// How many times in a row each write below is read back. `DescribeCluster` is REPORTING,
/// so it may not change what the next caller sees: every read after a write answers the
/// same value, the FIRST one included, which is the read a client polling for the lock it
/// just wrote is satisfied by.
const REPEATED_READS: usize = 8;

/// A `RunJobFlow` request for a long-running cluster in the form a client launches one in:
/// instance GROUPS placed in a named subnet, on a release label the catalog offers, kept
/// alive with no steps and launched UNLOCKED — so the lock is a change this operation
/// makes rather than one the launch carried.
fn long_running_request(name: &str, release: &str, subnet: &str, core_instances: i64) -> Value {
    let mut groups = vec![json!({
        "Name": "Primary", "InstanceRole": "MASTER", "InstanceType": "m5.xlarge",
        "InstanceCount": 1, "Market": "ON_DEMAND",
    })];
    if core_instances > 0 {
        groups.push(json!({
            "Name": "Core", "InstanceRole": "CORE", "InstanceType": "m5.xlarge",
            "InstanceCount": core_instances, "Market": "ON_DEMAND",
        }));
    }
    json!({
        "Name": name,
        "ReleaseLabel": release,
        "Applications": [{ "Name": "Hadoop" }],
        "JobFlowRole": crate::job_flow_lifecycle::INSTANCE_PROFILE,
        "ServiceRole": crate::job_flow_lifecycle::SERVICE_ROLE,
        "Instances": {
            "InstanceGroups": groups,
            "Ec2SubnetId": subnet,
            "KeepJobFlowAliveWhenNoSteps": true,
            "TerminationProtected": false,
        },
    })
}

/// That `cluster` is a live cluster carrying the lock `locked`, with nothing of a shutdown
/// on it: it is in neither shutdown state, it records no reason for a state change and it
/// has not ended.
fn assert_up_and_locked(case: &str, cluster: &Value, locked: bool) {
    let state = at(cluster, &["Status", "State"]).as_str().unwrap_or_default();
    assert!(
        STILL_UP.contains(&state),
        "case {case:?}: the cluster must still be up, got state {state:?} in {cluster}"
    );
    assert_eq!(
        cluster["TerminationProtected"].as_bool(),
        Some(locked),
        "case {case:?}: the reported lock must be {locked}, got {cluster}"
    );
    assert!(
        at(cluster, &["Status", "StateChangeReason"]).is_null(),
        "case {case:?}: nothing changed the cluster's state, got {cluster}"
    );
    assert!(
        at(cluster, &["Status", "Timeline", "EndDateTime"]).is_null(),
        "case {case:?}: the cluster has not ended, got {cluster}"
    );
}

/// THE RULE on a cluster that has already come up: the write is a mutation on live cluster
/// state, so the value it sets is what the very next `DescribeCluster` reports — nothing
/// about it is eventual — and every further read answers the same, because a read may not
/// change what a later caller sees.
///
/// Rows span what the rule generalises over: the shape of the cluster the lock is written
/// on (one primary group, a primary beside a core group, the instance-count form), the
/// release it runs, whether its launch named a subnet, and the sequence of values written
/// on it — a repeat of the value it already holds and a reversal included.
#[tokio::test]
async fn the_lock_written_on_a_cluster_that_is_up_is_reported_by_the_very_next_read() {
    let emr = Emr::new();

    // Launched together, so the span the clock owes them all to come up is waited out once.
    let mut launched: Vec<(&str, String)> = Vec::new();
    for (label, request) in [
        (
            "one primary group, emr-6.15.0",
            long_running_request("primary-only", "emr-6.15.0", "subnet-0a1b2c3d4e5f60718", 0),
        ),
        (
            "a primary and a core group, emr-7.5.0",
            long_running_request("with-core", "emr-7.5.0", "subnet-1122334455667788a", 2),
        ),
        (
            "the instance-count form, no subnet named",
            kept_alive_request("counted"),
        ),
    ] {
        launched.push((label, emr.run_job_flow(request).await));
    }

    for (label, cluster_id) in &launched {
        // The lock is written on a cluster that is already serving, which is the state a
        // long-running cluster is locked in.
        let ready = await_cluster_state(&emr, cluster_id, label, "WAITING").await;
        assert_up_and_locked(label, &ready, false);

        for requested in [true, true, false, true, false] {
            emr.ok(
                "SetTerminationProtection",
                json!({ "JobFlowIds": [cluster_id], "TerminationProtected": requested }),
            )
            .await;
            let case = format!("{label}, {requested} written");
            for read in 0..REPEATED_READS {
                let cluster = emr.describe_cluster(cluster_id).await;
                assert_up_and_locked(&format!("{case}, read {read}"), &cluster, requested);
            }
        }
    }
}

/// THE RULE's whole path on a cluster that is up, in the order a client drives it: the
/// lock is written, read back, and while it is set every `TerminateJobFlows` naming the
/// cluster is refused in whole — HTTP 400 on the `ValidationException` channel with the one
/// constant sentence — leaving the cluster's state, its reason and its timeline exactly as
/// they stood, however many times the shutdown is asked for and however many reporting
/// calls look at it; then "you must first unlock the job flow by a subsequent call to
/// SetTerminationProtection in which you set the value to false"
/// (`SetTerminationProtection` doc) lets the identical call through, and the cluster
/// reaches `TERMINATED` carrying the `USER_REQUEST` reason.
///
/// The rows differ in the state the cluster was in when the lock was written — still
/// coming up, and already serving — because the lock is the cluster's own setting and
/// survives every transition the clock owes it either way.
#[tokio::test]
async fn a_locked_cluster_is_held_up_until_the_lock_is_cleared() {
    for (label, lock_once_up) in [
        ("locked while it was still coming up", false),
        ("locked once it was up", true),
    ] {
        let emr = Emr::new();
        let cluster_id = emr
            .run_job_flow(long_running_request(
                "held",
                "emr-6.15.0",
                "subnet-0a1b2c3d4e5f60718",
                1,
            ))
            .await;
        if lock_once_up {
            await_cluster_state(&emr, &cluster_id, label, "WAITING").await;
        }

        emr.ok(
            "SetTerminationProtection",
            json!({ "JobFlowIds": [&cluster_id], "TerminationProtected": true }),
        )
        .await;
        let written = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            written["TerminationProtected"].as_bool(),
            Some(true),
            "case {label:?}: the read after the call must report the lock, got {written}"
        );

        // The lock is still the cluster's setting once the clock has moved it on, and
        // nothing shut the cluster down on the way there.
        let up = await_cluster_state(&emr, &cluster_id, label, "WAITING").await;
        assert_up_and_locked(label, &up, true);

        // Refused in whole, as often as it is asked for, with the cluster untouched after
        // each refusal.
        for attempt in 0..3 {
            let (status, payload) = emr
                .call("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
                .await;
            let case = format!("{label}, shutdown attempt {attempt}");
            assert_refused(&case, status, &payload, "ValidationException");
            assert_eq!(
                message(&case, &payload),
                PROTECTED_SENTENCE,
                "case {case:?}: expected {PROTECTED_SENTENCE:?}, got {payload}"
            );
            for read in 0..REPEATED_READS {
                let cluster = emr.describe_cluster(&cluster_id).await;
                assert_up_and_locked(&format!("{case}, read {read}"), &cluster, true);
            }
        }

        // Cleared — and the identical call is served.
        emr.ok(
            "SetTerminationProtection",
            json!({ "JobFlowIds": [&cluster_id], "TerminationProtected": false }),
        )
        .await;
        emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
            .await;
        let shutting_down = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            at(&shutting_down, &["Status", "State"]).as_str(),
            Some("TERMINATING"),
            "case {label:?}: the unlocked cluster must start shutting down, got {shutting_down}"
        );
        assert_eq!(
            at(&shutting_down, &["Status", "StateChangeReason", "Code"]).as_str(),
            Some("USER_REQUEST"),
            "case {label:?}: the reason must be USER_REQUEST, got {shutting_down}"
        );
        assert_eq!(
            at(&shutting_down, &["Status", "StateChangeReason", "Message"]).as_str(),
            Some("Terminated by user request"),
            "case {label:?}: the reason's message must be the user-request one, got \
             {shutting_down}"
        );

        let gone = await_cluster_state(&emr, &cluster_id, label, "TERMINATED").await;
        assert!(
            !at(&gone, &["Status", "Timeline", "EndDateTime"]).is_null(),
            "case {label:?}: a terminated cluster carries an EndDateTime, got {gone}"
        );
        assert_eq!(
            gone["TerminationProtected"].as_bool(),
            Some(false),
            "case {label:?}: the cleared lock must still be reported, got {gone}"
        );
    }
}
