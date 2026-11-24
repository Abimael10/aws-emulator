// SPDX-License-Identifier: CC0-1.0

//! `SetKeepJobFlowAliveWhenNoSteps`: the setting a caller writes on a cluster (job flow),
//! and what the cluster then DOES about it.
//!
//! THE RULE every table below asserts: the call writes `KeepJobFlowAliveWhenNoSteps` on
//! every cluster its `JobFlowIds` resolves — resolving all of them before it writes any, so
//! a batch carrying an id the service holds no cluster under writes nothing and is refused —
//! and the cluster's self-shutdown follows the value it wrote: a cluster that no longer
//! stays up shuts itself down once its step queue has run, and one that now stays up does
//! not. `Cluster$AutoTerminate` ("specifies whether the cluster should terminate after
//! completing all steps") is the complement of what was written, so a described cluster is
//! where the write is read back.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: what
//! was written is only ever read back through `DescribeCluster` and `ListSteps`.
//!
//! Every wait waits for the CONDITION a `DescribeCluster` reports, under a deadline far
//! above every span this service owes a cluster, so a slow or loaded host delays a case
//! rather than failing it.

use serde_json::{json, Value};

use crate::emr_control_plane::{MAX_JOB_FLOW_IDS_PER_REQUEST, UNKNOWN_CLUSTER_IDS};
use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_state, await_cluster_where, kept_alive_request,
    minimal_request, step_config, Emr, INSTANCE_PROFILE, SERVICE_ROLE,
};
use crate::refusal_messages::message;
use crate::refusal_wording::JOB_FLOW_SENTENCE;

/// The operation under test.
const OPERATION: &str = "SetKeepJobFlowAliveWhenNoSteps";

/// The states a cluster that is UP reports, either of which a cluster asked to stay up may
/// rest in (`smithy_query(query="shape", shape="com.amazonaws.emr#ClusterState")`).
const UP: [&str; 2] = ["RUNNING", "WAITING"];

/// The states a step of this service can have ENDED in
/// (`smithy_query(query="shape", shape="com.amazonaws.emr#StepState")`).
const ENDED_STEP_STATES: [&str; 4] = ["COMPLETED", "FAILED", "CANCELLED", "INTERRUPTED"];

/// A launch that asks NOT to stay up when it has no steps — the transient form of
/// `JobFlowInstancesConfig$KeepJobFlowAliveWhenNoSteps`, which
/// `tests/all/job_flow_lifecycle.rs` has no helper for (its own is the kept-alive one).
fn transient_request(name: &str) -> Value {
    json!({
        "Name": name,
        "JobFlowRole": INSTANCE_PROFILE, "ServiceRole": SERVICE_ROLE,
        "Instances": {
            "InstanceCount": 1,
            "MasterInstanceType": "m5.xlarge",
            "KeepJobFlowAliveWhenNoSteps": false,
        },
    })
}

/// One step naming a jar and nothing else: what the step DOES is another suite's concern
/// (`tests/all/cluster_step_execution.rs`), and what this one needs is a queue the shutdown
/// has to wait behind.
fn step(name: &str) -> Value {
    step_config(name, "s3://emr/jars/noop.jar", Value::Null, Value::Null)
}

/// `AutoTerminate` as a described cluster reports it.
fn auto_terminate(cluster: &Value) -> Option<bool> {
    cluster["AutoTerminate"].as_bool()
}

/// THE RULE on the write itself: whatever this call wrote is what a described cluster
/// reports the complement of, whatever the launch asked for and however often the value is
/// written.
///
/// Rows differ along what the rule generalises over: what the LAUNCH asked (to stay up, not
/// to, and nothing at all — the documented default), the value written, and whether that
/// value CHANGES the setting or asks again for what the cluster is already doing.
#[tokio::test]
async fn the_value_this_call_writes_is_what_a_described_cluster_reports() {
    let emr = Emr::new();

    let launches: [(&str, Value); 3] = [
        ("a launch that asked to stay up", kept_alive_request("kept")),
        ("a launch that asked not to", transient_request("transient")),
        ("a launch that asked for neither", minimal_request("default")),
    ];

    for (label, request) in launches {
        let cluster_id = emr.run_job_flow(request).await;
        // The launch's own value first, so each row below is read against what preceded it.
        let launched = emr.describe_cluster(&cluster_id).await;
        assert!(
            auto_terminate(&launched).is_some(),
            "case {label:?}: a described cluster states AutoTerminate, got {launched}"
        );

        for (index, keep_alive) in [true, true, false, false, true].into_iter().enumerate() {
            let case = format!("{label}: write {keep_alive} (call {index})");
            emr.ok(
                OPERATION,
                json!({
                    "JobFlowIds": [&cluster_id],
                    "KeepJobFlowAliveWhenNoSteps": keep_alive,
                }),
            )
            .await;

            let cluster = emr.describe_cluster(&cluster_id).await;
            assert_eq!(
                auto_terminate(&cluster),
                Some(!keep_alive),
                "case {case:?}: expected AutoTerminate {}, got {cluster}",
                !keep_alive
            );
        }
    }
}

/// THE RULE over the BATCH: one call writes the value on every cluster its `JobFlowIds`
/// names, whatever the size or shape of the batch.
///
/// Rows differ along the batch: one cluster, two, a batch at the cap the member takes, and
/// one naming the same cluster twice.
#[tokio::test]
async fn one_call_writes_the_value_on_every_cluster_the_batch_names() {
    let emr = Emr::new();

    for size in [1usize, 2, MAX_JOB_FLOW_IDS_PER_REQUEST] {
        let mut ids: Vec<String> = Vec::new();
        for index in 0..size {
            ids.push(
                emr.run_job_flow(kept_alive_request(&format!("batch-{size}-{index}")))
                    .await,
            );
        }
        // A batch naming one of its clusters twice is the same batch of clusters — asserted
        // where the repeat still fits under the cap the member takes.
        let repeated: Vec<String> = ids.iter().chain(ids.first()).cloned().collect();
        let mut variants: Vec<(&str, &Vec<String>)> = vec![("a batch", &ids)];
        if repeated.len() <= MAX_JOB_FLOW_IDS_PER_REQUEST {
            variants.push(("a batch with a repeated id", &repeated));
        }

        for (label, batch) in variants {
            for keep_alive in [false, true] {
                let case = format!("{label} of {size} cluster(s), writing {keep_alive}");
                emr.ok(
                    OPERATION,
                    json!({
                        "JobFlowIds": batch,
                        "KeepJobFlowAliveWhenNoSteps": keep_alive,
                    }),
                )
                .await;

                for id in &ids {
                    let cluster = emr.describe_cluster(id).await;
                    assert_eq!(
                        auto_terminate(&cluster),
                        Some(!keep_alive),
                        "case {case:?}: cluster {id} must report AutoTerminate {}, got \
                         {cluster}",
                        !keep_alive
                    );
                }
            }
        }
    }
}

/// THE RULE on what the cluster DOES once it was asked to stop staying up: it shuts itself
/// down — reporting the modelled reason for a cluster that has run its queue and the instant
/// it ended — and never before the steps it had in hand have ended.
///
/// Rows differ along the queue the cluster carries when the call lands: none at all, and one
/// step submitted just before it, which the shutdown has to wait behind.
#[tokio::test]
async fn a_cluster_asked_to_stop_staying_up_shuts_itself_down_once_its_queue_has_run() {
    for queued in [0usize, 1] {
        let case = format!("a cluster asked to stop staying up with {queued} queued step(s)");
        let emr = Emr::new();
        let cluster_id = emr.run_job_flow(kept_alive_request("asked-to-stop")).await;

        // The cluster first comes to rest where a cluster asked to stay up rests, so the
        // shutdown below is this call's doing and not the launch's.
        let waiting = await_cluster_state(&emr, &cluster_id, &case, "WAITING").await;
        assert_eq!(
            auto_terminate(&waiting),
            Some(false),
            "case {case:?}: a cluster launched to stay up reports AutoTerminate false, got \
             {waiting}"
        );

        for index in 0..queued {
            let added = emr
                .ok(
                    "AddJobFlowSteps",
                    json!({
                        "JobFlowId": &cluster_id,
                        "Steps": [step(&format!("step-{index}"))],
                    }),
                )
                .await;
            assert_eq!(
                added["StepIds"].as_array().map(Vec::len),
                Some(1),
                "case {case:?}: the step must be accepted, got {added}"
            );
        }

        emr.ok(
            OPERATION,
            json!({
                "JobFlowIds": [&cluster_id],
                "KeepJobFlowAliveWhenNoSteps": false,
            }),
        )
        .await;
        let asked = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            auto_terminate(&asked),
            Some(true),
            "case {case:?}: the write must be reported back, got {asked}"
        );

        // Nothing but the clock is called from here on: no TerminateJobFlows.
        let ended = await_cluster_state(&emr, &cluster_id, &case, "TERMINATED").await;
        assert_eq!(
            at(&ended, &["Status", "StateChangeReason", "Code"]).as_str(),
            Some("ALL_STEPS_COMPLETED"),
            "case {case:?}: the self-shutdown names the modelled reason, got {ended}"
        );
        let cluster_end = at(&ended, &["Status", "Timeline", "EndDateTime"])
            .as_f64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a terminated cluster records EndDateTime, got {ended}")
            });

        // Every step it had in hand ended, and the shutdown never fell before one of them.
        let listed = emr.list_steps(json!({ "ClusterId": &cluster_id })).await;
        let steps = listed["Steps"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            steps.len(),
            queued,
            "case {case:?}: the cluster keeps every step submitted to it, got {listed}"
        );
        for step in &steps {
            let state = at(step, &["Status", "State"]).as_str().unwrap_or_default();
            assert!(
                ENDED_STEP_STATES.contains(&state),
                "case {case:?}: every step of a terminated cluster has ended, got {step}"
            );
            if let Some(step_end) = at(step, &["Status", "Timeline", "EndDateTime"]).as_f64() {
                assert!(
                    cluster_end >= step_end,
                    "case {case:?}: the shutdown at {cluster_end} must not fall before the \
                     step that ended at {step_end}, got {step}"
                );
            }
        }
    }
}

/// THE RULE the other way round: a cluster asked to stay up stops shutting itself down, so
/// the shutdown its launch had it owed never happens.
///
/// The evidence is a CONTROL launched the same way and left alone: the case waits for that
/// one to shut itself down, and by the instant it has, the cluster this call touched has had
/// at least as long to do the same — so what tells the two apart is this call and nothing
/// else.
#[tokio::test]
async fn a_cluster_asked_to_stay_up_stops_shutting_itself_down() {
    let emr = Emr::new();
    let control = emr.run_job_flow(transient_request("control")).await;
    let subject = emr.run_job_flow(transient_request("asked-to-stay")).await;

    emr.ok(
        OPERATION,
        json!({
            "JobFlowIds": [&subject],
            "KeepJobFlowAliveWhenNoSteps": true,
        }),
    )
    .await;

    let ended = await_cluster_state(&emr, &control, "the control cluster", "TERMINATED").await;
    assert!(
        at(&ended, &["Status", "Timeline", "EndDateTime"])
            .as_f64()
            .is_some(),
        "the control cluster must record EndDateTime, got {ended}"
    );
    assert_eq!(
        auto_terminate(&ended),
        Some(true),
        "the control cluster was never asked to stay up, got {ended}"
    );

    let kept = await_cluster_where(
        &emr,
        &subject,
        "the cluster asked to stay up",
        "a reported ReadyDateTime",
        |cluster| !at(cluster, &["Status", "Timeline", "ReadyDateTime"]).is_null(),
    )
    .await;
    assert_eq!(
        auto_terminate(&kept),
        Some(false),
        "the write must be reported back, got {kept}"
    );
    let state = at(&kept, &["Status", "State"]).as_str().unwrap_or_default();
    assert!(
        UP.contains(&state),
        "a cluster asked to stay up is still up, got {kept}"
    );
    assert!(
        at(&kept, &["Status", "Timeline", "EndDateTime"]).is_null(),
        "a cluster asked to stay up has not ended, got {kept}"
    );
}

/// `JobFlowIds` batches that cannot all be resolved, built around one id the service holds a
/// cluster under (`known`) and one it does not (`unknown`).
///
/// Rows differ along the size of the batch, where the unresolvable id sits in it, how many
/// unresolvable ids it carries and whether the same one repeats.
fn unresolvable_batches(known: &str, unknown: &str) -> Vec<(String, Vec<String>)> {
    let mut at_the_cap: Vec<String> = (1..MAX_JOB_FLOW_IDS_PER_REQUEST)
        .map(|_| known.to_string())
        .collect();
    at_the_cap.push(unknown.to_string());

    vec![
        ("the unresolvable id alone".to_string(), vec![unknown.to_string()]),
        (
            "the unresolvable id after a known one".to_string(),
            vec![known.to_string(), unknown.to_string()],
        ),
        (
            "the unresolvable id before a known one".to_string(),
            vec![unknown.to_string(), known.to_string()],
        ),
        (
            "the unresolvable id between two known ones".to_string(),
            vec![known.to_string(), unknown.to_string(), known.to_string()],
        ),
        (
            "the same unresolvable id twice".to_string(),
            vec![unknown.to_string(), unknown.to_string()],
        ),
        (
            "a batch at the cap, one id unresolvable".to_string(),
            at_the_cap,
        ),
    ]
}

/// THE RULE on a batch this service cannot resolve: it is refused with HTTP 400 and this
/// legacy job-flow surface's own sentence — which is CONSTANT, naming no id — and it writes
/// NOTHING, not even on the clusters of the batch it did resolve.
///
/// Rows span the unresolvable ids, the shapes of batch they can sit in, and both values the
/// call can ask to write; a batch PAST the cap the member takes is refused the same way, and
/// the control at the end shows every refusal above was a lookup.
#[tokio::test]
async fn a_batch_this_service_cannot_resolve_is_refused_and_writes_nothing() {
    let emr = Emr::new();
    // Launched to stay up, so the value a refused call must not write is the other one.
    let known = emr.run_job_flow(kept_alive_request("known")).await;

    for unknown in UNKNOWN_CLUSTER_IDS {
        for (label, ids) in unresolvable_batches(&known, unknown) {
            // Both values the call can write: the sentence is about the id it could not
            // resolve, so what the request asked to write is no part of it.
            for keep_alive in [true, false] {
                let case = format!("{OPERATION}({keep_alive}) with {label} ({unknown:?})");
                let (status, payload) = emr
                    .call(
                        OPERATION,
                        json!({
                            "JobFlowIds": &ids,
                            "KeepJobFlowAliveWhenNoSteps": keep_alive,
                        }),
                    )
                    .await;
                assert_refused(&case, status, &payload, "ValidationException");
                let said = message(&case, &payload);
                assert_eq!(
                    said, JOB_FLOW_SENTENCE,
                    "case {case:?}: expected {JOB_FLOW_SENTENCE:?}, got {payload}"
                );
                assert!(
                    unknown.is_empty() || !said.contains(unknown),
                    "case {case:?}: the job-flow sentence names no id, got {said:?}"
                );

                let cluster = emr.describe_cluster(&known).await;
                assert_eq!(
                    auto_terminate(&cluster),
                    Some(false),
                    "case {case:?}: a refused batch must write nothing, got {cluster}"
                );
            }
        }
    }

    // A batch one id PAST the cap, every id of it resolvable: refused, and nothing written.
    let over_the_cap: Vec<String> = (0..=MAX_JOB_FLOW_IDS_PER_REQUEST)
        .map(|_| known.clone())
        .collect();
    let case = format!("a batch of {} ids, one past the cap", over_the_cap.len());
    let (status, payload) = emr
        .call(
            OPERATION,
            json!({
                "JobFlowIds": &over_the_cap,
                "KeepJobFlowAliveWhenNoSteps": false,
            }),
        )
        .await;
    assert_refused(&case, status, &payload, "ValidationException");
    let cluster = emr.describe_cluster(&known).await;
    assert_eq!(
        auto_terminate(&cluster),
        Some(false),
        "case {case:?}: a refused batch must write nothing, got {cluster}"
    );

    // The control: the resolvable id alone is served and the value is written.
    emr.ok(
        OPERATION,
        json!({
            "JobFlowIds": [&known],
            "KeepJobFlowAliveWhenNoSteps": false,
        }),
    )
    .await;
    let cluster = emr.describe_cluster(&known).await;
    assert_eq!(
        auto_terminate(&cluster),
        Some(true),
        "case \"the resolvable id\": the write must be reported back, got {cluster}"
    );
}
