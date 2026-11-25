// SPDX-License-Identifier: CC0-1.0

//! The CLUSTER-STATE gate on `GetOnClusterAppUIPresignedURL`, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: the
//! state a cluster has reached is read back through `DescribeCluster`, and every cluster
//! below is taken through its states by the spans its own launch and shutdown owe it.
//!
//! THE RULE: an on-cluster application user interface is served from the cluster's PRIMARY
//! NODE, so what this call answers is settled by whether the cluster the request names
//! still holds that node — a cluster that has been launched and not shut down is answered
//! the address of the interface it serves (or is coming up to serve) from the launch
//! onward, and a cluster that is giving its capacity up or has given it up is REFUSED,
//! under HTTP 400 `ValidationException` with the one sentence that states the rule and no
//! URL at all. The gate is the cluster's alone: it is settled before the interface the
//! request names, before the application it names, and before whether the call is a dry
//! run, so every form of the question is answered the same way for one cluster.
//!
//! The rows span what the rule generalises over: EVERY `ClusterState` the model declares,
//! each observed on a cluster the clock really took there — the two phases of coming up, a
//! cluster that is running its queue, one that is staying up for more work, one shutting
//! down because a caller said so, one shutting down because its queue ran out, and both
//! terminal states — against four forms of the question (no interface named, the interface
//! a console probes a cluster with, a dry run, and one application's own interface).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_where, kept_alive_request, minimal_request, Emr,
};
use crate::on_cluster_app_ui::assert_presigned_against_this_emulator;
use crate::refusal_messages::message;

/// The operation under test.
const OPERATION: &str = "GetOnClusterAppUIPresignedURL";

/// Every `ClusterState` the model declares, with whether a cluster in that state still
/// holds the primary node an on-cluster interface is served from.
///
/// SOURCE: the states are `smithy_query(query="shape", shape="ClusterState")` — `STARTING |
/// BOOTSTRAPPING | RUNNING | WAITING | TERMINATING | TERMINATED |
/// TERMINATED_WITH_ERRORS`. A cluster holds the capacity its launch asked for from the
/// launch up to and including `WAITING` — the states Amazon EMR reports a cluster as active
/// in, the `--active` cluster states of `ListClusters` — and from `TERMINATING` on it is
/// giving that capacity up or has given it up.
const CLUSTER_STATES: [(&str, bool); 7] = [
    ("STARTING", true),
    ("BOOTSTRAPPING", true),
    ("RUNNING", true),
    ("WAITING", true),
    ("TERMINATING", false),
    ("TERMINATED", false),
    ("TERMINATED_WITH_ERRORS", false),
];

/// The sentence a cluster that no longer holds its capacity is refused with, spelled out
/// here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make the cases below pass. It carries NO value of the request and
/// none of the cluster — not the id the request named and not the state the cluster is in —
/// which is what tells this refusal apart from the cluster-id one.
const INACTIVE_CLUSTER_REFUSAL: &str = "A presigned URL can only be created for an active cluster.";

/// The error code every refusal of this operation travels under.
const REFUSAL_CODE: &str = "ValidationException";

/// The applications every cluster below installs, so the interfaces the probes ask for have
/// an application to serve them whatever state the cluster is in.
const APPLICATIONS: [&str; 2] = ["Spark", "Hadoop"];

/// How long a walk through a cluster's states may take before it gives up: far above every
/// span this service owes a cluster, so a slow or loaded host delays a walk rather than
/// failing it.
const WALK_DEADLINE: Duration = Duration::from_secs(180);

/// One look at the cluster inside a walk.
const WALK_INTERVAL: Duration = Duration::from_millis(20);

/// Whether a cluster in `state` still holds the node an interface is served from. A state
/// the model does not declare fails the case rather than being read either way.
fn still_holds_its_primary_node(case: &str, state: &str) -> bool {
    CLUSTER_STATES
        .iter()
        .find(|(declared, _)| *declared == state)
        .map(|(_, active)| *active)
        .unwrap_or_else(|| {
            panic!("case {case:?}: {state:?} is no ClusterState the model declares")
        })
}

/// One form of the question, asked of whichever cluster the case holds.
struct Probe {
    /// What makes this row its own case.
    label: &'static str,
    /// The members the request carries beside `ClusterId`.
    members: Value,
    /// Whether an ANSWERED call hands back a URL: a dry run reports the readiness and mints
    /// nothing, so it answers one without.
    mints_a_url: bool,
}

/// The forms of the question every state below is asked in.
fn probes() -> Vec<Probe> {
    vec![
        Probe {
            label: "no interface named",
            members: json!({}),
            mints_a_url: true,
        },
        Probe {
            label: "the resource manager a console probes a cluster with",
            members: json!({ "OnClusterAppUIType": "ResourceManager" }),
            mints_a_url: true,
        },
        Probe {
            label: "a dry run for the Spark history server",
            members: json!({ "OnClusterAppUIType": "SparkHistoryServer", "DryRun": true }),
            mints_a_url: false,
        },
        Probe {
            label: "one application's own interface",
            members: json!({
                "OnClusterAppUIType": "ApplicationMaster",
                "ApplicationId": "application_1700000000000_0001",
            }),
            mints_a_url: true,
        },
    ]
}

/// A launch of `applications` that stays up when it has no steps to run, on a release the
/// catalog carries so each application name resolves.
fn kept_alive_launch(name: &str) -> Value {
    let mut request = kept_alive_request(name);
    request["ReleaseLabel"] = json!("emr-6.15.0");
    request["Applications"] = applications();
    request
}

/// A launch that does NOT stay up: it runs its queue and then stops itself, which is the
/// cluster that reports `RUNNING`.
fn transient_launch(name: &str, steps: Value) -> Value {
    let mut request = minimal_request(name);
    request["ReleaseLabel"] = json!("emr-6.15.0");
    request["Applications"] = applications();
    request["Steps"] = steps;
    request
}

fn applications() -> Value {
    json!(APPLICATIONS
        .iter()
        .map(|name| json!({ "Name": name }))
        .collect::<Vec<Value>>())
}

/// A queued step that runs and does not take its cluster down whatever its own work came
/// to, so the shutdown the cluster reaches is the one its finished QUEUE owes it.
fn queued_step(name: &str) -> Value {
    json!({
        "Name": name,
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": format!("s3://jars/{name}.jar") },
    })
}

/// The state the cluster REPORTS, read back through `DescribeCluster`.
async fn reported_state(emr: &Emr, cluster_id: &str) -> String {
    let cluster = emr.describe_cluster(cluster_id).await;
    at(&cluster, &["Status", "State"])
        .as_str()
        .unwrap_or_else(|| panic!("a cluster reports the state it is in, got {cluster}"))
        .to_string()
}

/// THE RULE, asserted on one answer: a cluster that still holds its primary node is served
/// the interface's readiness and its address, and one that does not is refused with the one
/// sentence and nothing else.
fn assert_gate(
    case: &str,
    state: &str,
    probe: &Probe,
    cluster_id: &str,
    status: u16,
    payload: &Value,
) {
    if still_holds_its_primary_node(case, state) {
        assert_eq!(
            status, 200,
            "case {case:?}: a cluster in {state} still serves its applications' interfaces, \
             got {status} with {payload}"
        );
        assert_eq!(
            at(payload, &["PresignedURLReady"]).as_bool(),
            Some(true),
            "case {case:?}: the interface asked for has an application of this cluster to \
             serve it, got {payload}"
        );
        if !probe.mints_a_url {
            assert!(
                at(payload, &["PresignedURL"]).is_null(),
                "case {case:?}: a dry run hands back no URL, got {payload}"
            );
            return;
        }
        let url = at(payload, &["PresignedURL"])
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no URL in {payload}"))
            .to_string();
        assert!(
            url.starts_with("https://") && url.contains(cluster_id),
            "case {case:?}: the URL is this cluster's own address on the https scheme, got \
             {url}"
        );
        assert_presigned_against_this_emulator(case, &url);
    } else {
        assert_refused(case, status, payload, REFUSAL_CODE);
        assert_eq!(
            message(case, payload),
            INACTIVE_CLUSTER_REFUSAL,
            "case {case:?}: a cluster that no longer holds its capacity is refused with the \
             sentence that states the rule, got {payload}"
        );
        assert!(
            at(payload, &["PresignedURL"]).is_null()
                && at(payload, &["PresignedURLReady"]).is_null(),
            "case {case:?}: a refusal carries neither a URL nor a readiness, got {payload}"
        );
    }
}

/// Ask every probe of the cluster and assert the gate for `state`, which the caller knows
/// the cluster to be in.
async fn assert_gate_in(emr: &Emr, cluster_id: &str, state: &str) {
    for probe in probes() {
        let case = format!("{OPERATION} against a cluster in {state}: {}", probe.label);
        let mut request = probe.members.clone();
        request["ClusterId"] = json!(cluster_id);
        let (status, payload) = emr.call(OPERATION, request).await;
        assert_gate(&case, state, &probe, cluster_id, status, &payload);
    }
}

/// Ask every probe of the cluster while it is reported in ONE state, and assert the gate's
/// answer for that state; answers the state observed, or `None` when the cluster moved on
/// while the probes were in flight — which is an observation of no state rather than of the
/// wrong one.
async fn assert_gate_in_one_state(emr: &Emr, cluster_id: &str) -> Option<String> {
    let before = reported_state(emr, cluster_id).await;
    let mut answers: Vec<(Probe, u16, Value)> = Vec::new();
    for probe in probes() {
        let mut request = probe.members.clone();
        request["ClusterId"] = json!(cluster_id);
        let (status, payload) = emr.call(OPERATION, request).await;
        answers.push((probe, status, payload));
    }
    if reported_state(emr, cluster_id).await != before {
        return None;
    }
    for (probe, status, payload) in &answers {
        let case = format!("{OPERATION} against a cluster in {before}: {}", probe.label);
        assert_gate(&case, &before, probe, cluster_id, *status, payload);
    }
    Some(before)
}

/// Walk the cluster until it reports a state `arrived` accepts, asserting THE RULE on every
/// state the cluster is observed in along the way and recording each one in `observed`.
///
/// Waits for the CONDITION rather than for a span: it looks again until the cluster has
/// arrived or a deadline far above every span this service owes a cluster passes, so a slow
/// or loaded host delays the walk rather than failing it.
async fn walk_until(
    emr: &Emr,
    cluster_id: &str,
    wanted: &str,
    arrived: impl Fn(&str) -> bool,
    observed: &mut BTreeSet<String>,
) {
    let deadline = Instant::now() + WALK_DEADLINE;
    loop {
        if let Some(state) = assert_gate_in_one_state(emr, cluster_id).await {
            observed.insert(state.clone());
            if arrived(&state) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the cluster never reached {wanted}, having been seen in {observed:?}"
        );
        tokio::time::sleep(WALK_INTERVAL).await;
    }
}

/// Fail the case unless the walk really saw the cluster in each state it was to pass
/// through, so a row that proves the rule for a state cannot silently go unasserted.
fn assert_seen(observed: &BTreeSet<String>, states: &[&str]) {
    for state in states {
        assert!(
            observed.contains(*state),
            "the walk never saw the cluster in {state}, having seen {observed:?}"
        );
    }
}

/// THE RULE over a cluster a CALLER takes down: it is served through both phases of coming
/// up and while it stays up for more work, and refused from the instant its shutdown
/// started — so the answer turns on the shutdown, not on the cluster being up.
#[tokio::test]
async fn a_cluster_is_served_from_its_launch_and_refused_from_its_shutdown() {
    let emr = Emr::new();
    let cluster_id = emr.run_job_flow(kept_alive_launch("app-ui-gate-up")).await;

    let mut observed = BTreeSet::new();
    walk_until(
        &emr,
        &cluster_id,
        "WAITING",
        |state| state == "WAITING",
        &mut observed,
    )
    .await;
    assert_seen(&observed, &["STARTING", "BOOTSTRAPPING", "WAITING"]);

    emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&cluster_id] }))
        .await;
    walk_until(
        &emr,
        &cluster_id,
        "TERMINATED",
        |state| state == "TERMINATED",
        &mut observed,
    )
    .await;
    assert_seen(&observed, &["TERMINATING", "TERMINATED"]);

    // The cluster is gone for good, so the refusal is what it keeps answering.
    assert_gate_in(&emr, &cluster_id, "TERMINATED").await;
}

/// THE RULE over a cluster that runs its QUEUE and then stops itself: it is served while it
/// is running steps — the state a cluster with work in hand reports — and refused once that
/// queue has run it out of capacity.
#[tokio::test]
async fn a_cluster_running_its_queue_is_served_until_that_queue_stops_it() {
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(transient_launch(
            "app-ui-gate-queue",
            json!([queued_step("first"), queued_step("second")]),
        ))
        .await;

    let mut observed = BTreeSet::new();
    walk_until(
        &emr,
        &cluster_id,
        "a terminal state",
        |state| !still_holds_its_primary_node("the walk down a transient cluster", state),
        &mut observed,
    )
    .await;
    assert_seen(&observed, &["STARTING", "BOOTSTRAPPING", "RUNNING"]);
    assert!(
        observed.contains("TERMINATING") || observed.contains("TERMINATED"),
        "a cluster that ran its queue gives its capacity up, having been seen in {observed:?}"
    );
}

/// THE RULE's other terminal state: a cluster whose shutdown carried errors is refused the
/// same way, with the same sentence — the gate is the capacity the cluster no longer holds,
/// not WHY it stopped holding it.
///
/// No object store is mounted beside Amazon EMR in this suite, so the bootstrap action this
/// launch carries names a script its nodes cannot fetch, which is what takes the cluster
/// down into the modelled failure state.
#[tokio::test]
async fn a_cluster_whose_shutdown_carried_errors_is_refused_with_the_same_sentence() {
    let emr = Emr::new();
    let mut request = kept_alive_launch("app-ui-gate-failed");
    request["BootstrapActions"] = json!([{
        "Name": "install",
        "ScriptBootstrapAction": { "Path": "s3://scripts/install.sh" },
    }]);
    let cluster_id = emr.run_job_flow(request).await;

    let case = "a cluster whose bootstrap phase failed";
    let cluster = await_cluster_where(&emr, &cluster_id, case, "a terminal state", |cluster| {
        at(cluster, &["Status", "State"])
            .as_str()
            .is_some_and(|state| !still_holds_its_primary_node(case, state))
    })
    .await;
    let state = at(&cluster, &["Status", "State"])
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a cluster reports its state, got {cluster}"))
        .to_string();
    // The failure state is reached through the shutdown, so wait the shutdown out: either
    // state is refused, and this is the one the launch's failure brings.
    let state = if state == "TERMINATING" {
        await_cluster_where(&emr, &cluster_id, case, "TERMINATED_WITH_ERRORS", |cluster| {
            at(cluster, &["Status", "State"]).as_str() == Some("TERMINATED_WITH_ERRORS")
        })
        .await;
        "TERMINATED_WITH_ERRORS".to_string()
    } else {
        state
    };
    assert_eq!(
        state, "TERMINATED_WITH_ERRORS",
        "case {case:?}: a bootstrap failure ends in the modelled failure state, got {cluster}"
    );
    assert_gate_in(&emr, &cluster_id, &state).await;
}
