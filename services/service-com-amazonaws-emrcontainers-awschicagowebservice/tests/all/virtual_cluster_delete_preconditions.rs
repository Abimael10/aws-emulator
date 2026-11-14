// SPDX-License-Identifier: CC0-1.0

//! What `DeleteVirtualCluster` requires of the cluster before it tears it down.
//!
//! Every call goes through the production path: the service is mounted in the real
//! `MultiServiceRouter` by the harness [`crate::virtual_cluster_job_run_loop::Emr`] and
//! driven with restJson1 HTTP requests, so routing, deserialization, constraint validation,
//! the handler and serialization all run exactly as they do for an AWS SDK client. Every
//! request helper is the one the suites beside this one drive, so there is one request path
//! under test rather than a copy per suite.
//!
//! The rule under test, asserted over a TABLE of clusters rather than one request: a virtual
//! cluster is deleted only when every resource it holds has reached a terminal state, so
//! while any job run or managed endpoint of the cluster has not, the delete is refused with
//! the modelled `ValidationException` (HTTP 400) and ONE fixed sentence, and the cluster is
//! left exactly as it was; once the caller has ended them, the same delete goes through.
//!
//! The rows differ along what the rule generalises over: which KIND of resource the cluster
//! holds (a job run, a managed endpoint, both), which non-terminal state it is in, how many
//! of them there are, and whether a resource that has already ended still blocks anything.

use serde_json::Value;
use service_com_amazonaws_emrcontainers_awschicagowebservice::state;

use crate::managed_endpoint_delete::delete_endpoint;
use crate::managed_endpoint_listing::{create_endpoint, endpoint_body, endpoint_state};
use crate::virtual_cluster_job_run_loop::{field, text, Emr};
use crate::virtual_cluster_scheduler_status::{
    cancel_run, cluster_id, run_id, run_state, wait_for_the_clock,
};

/// AWS's sentence for a `DeleteVirtualCluster` whose cluster still holds resources that
/// have not finished, written here as the service emits it: it is FIXED text — no cluster
/// id, no resource id, no count — so the expectation is the sentence itself rather than a
/// format string.
const ACTIVE_RESOURCES: &str = "VirtualCluster has active resources";

/// What one fixture cluster holds when the delete arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holding {
    /// A job run the clock has started, so it is `RUNNING` (or `PENDING` behind a limit).
    LiveRun,
    /// A job run whose cancellation has been accepted but has not come due:
    /// `CANCEL_PENDING`, which is not a state a run has finished in.
    CancellingRun,
    /// A job run that has reached `CANCELLED`: over, so it holds nothing.
    EndedRun,
    /// A managed endpoint the clock has settled to `ACTIVE`.
    ActiveEndpoint,
    /// A managed endpoint still `CREATING`.
    CreatingEndpoint,
    /// A managed endpoint whose teardown has been accepted but has not come due:
    /// `TERMINATING`.
    TerminatingEndpoint,
    /// A managed endpoint that has reached `TERMINATED`: over, so it holds nothing.
    EndedEndpoint,
}

impl Holding {
    /// Whether this is a managed endpoint rather than a job run.
    fn is_endpoint(self) -> bool {
        matches!(
            self,
            Holding::ActiveEndpoint
                | Holding::CreatingEndpoint
                | Holding::TerminatingEndpoint
                | Holding::EndedEndpoint
        )
    }

    /// Whether a cluster holding this still has work in flight, so the delete is refused.
    fn is_live(self) -> bool {
        !matches!(self, Holding::EndedRun | Holding::EndedEndpoint)
    }

    /// The state the resource must be reported in once the row is set up.
    fn expected_state(self) -> &'static [&'static str] {
        match self {
            Holding::LiveRun => &[state::JOB_RUNNING, state::JOB_PENDING],
            Holding::CancellingRun => &[state::JOB_CANCEL_PENDING],
            Holding::EndedRun => &[state::JOB_CANCELLED],
            Holding::ActiveEndpoint => &[state::ENDPOINT_ACTIVE],
            Holding::CreatingEndpoint => &[state::ENDPOINT_CREATING],
            Holding::TerminatingEndpoint => &[state::ENDPOINT_TERMINATING],
            Holding::EndedEndpoint => &[state::ENDPOINT_TERMINATED],
        }
    }
}

/// The `state` `DescribeVirtualCluster` reports for one cluster.
async fn cluster_state(emr: &Emr, case: &str, cluster: &str) -> String {
    let answer = emr.describe_cluster(cluster).await;
    let described = field(answer.ok(case), "/virtualCluster", case).clone();
    text(&described, "/state", case)
}

/// The whole `VirtualCluster` record a describe reports, so a refused delete can be shown
/// to have changed nothing about it at all.
async fn cluster_record(emr: &Emr, case: &str, cluster: &str) -> Value {
    let answer = emr.describe_cluster(cluster).await;
    field(answer.ok(case), "/virtualCluster", case).clone()
}

/// The `message` a refused call carries.
fn refusal_message(body: &Value, case: &str) -> String {
    text(body, "/message", case)
}

/// Create the cluster of one row and put each resource it holds in the state the row names.
/// Answers the cluster id and the resources beside the ids they were minted under.
async fn set_up(emr: &Emr, label: &str, index: usize, holds: &[Holding]) -> (String, Vec<(Holding, String)>) {
    let cluster = cluster_id(
        emr,
        label,
        &format!("delete-precondition-{index}"),
        &format!("tok-delete-precondition-{index}"),
        None,
    )
    .await;

    let mut held: Vec<(Holding, String)> = Vec::new();
    // Everything the clock has to act on first: a run has to be started and an endpoint has
    // to reach ACTIVE before either can be cancelled or settled.
    for (slot, holding) in holds.iter().enumerate() {
        let id = if holding.is_endpoint() {
            if *holding == Holding::CreatingEndpoint {
                // Created last, so nothing waited for it and it is still CREATING.
                continue;
            }
            let created = create_endpoint(
                emr,
                &cluster,
                endpoint_body(
                    &format!("precondition-{index}-{slot}"),
                    &format!("ep-tok-precondition-{index}-{slot}"),
                    "JUPYTER_ENTERPRISE_GATEWAY",
                ),
            )
            .await;
            text(created.ok(label), "/id", label)
        } else {
            run_id(
                emr,
                label,
                &cluster,
                &format!("run-tok-precondition-{index}-{slot}"),
            )
            .await
        };
        held.push((*holding, id));
    }
    if !held.is_empty() {
        wait_for_the_clock();
    }

    // The resources the row wants already over: ended before anything else, so the span
    // they need has passed by the time the delete arrives.
    let mut any_ended = false;
    for (holding, id) in &held {
        match holding {
            Holding::EndedRun => {
                cancel_run(emr, &cluster, id).await.ok(label);
                any_ended = true;
            }
            Holding::EndedEndpoint => {
                delete_endpoint(emr, &cluster, id).await.ok(label);
                any_ended = true;
            }
            _ => {}
        }
    }
    if any_ended {
        wait_for_the_clock();
    }

    // And the ones that must still be in flight when the delete arrives.
    for (holding, id) in &held {
        match holding {
            Holding::CancellingRun => cancel_run(emr, &cluster, id).await.ok(label),
            Holding::TerminatingEndpoint => delete_endpoint(emr, &cluster, id).await.ok(label),
            _ => continue,
        };
    }
    for (slot, holding) in holds.iter().enumerate() {
        if *holding == Holding::CreatingEndpoint {
            let created = create_endpoint(
                emr,
                &cluster,
                endpoint_body(
                    &format!("precondition-{index}-{slot}"),
                    &format!("ep-tok-precondition-{index}-{slot}"),
                    "JUPYTER_ENTERPRISE_GATEWAY",
                ),
            )
            .await;
            held.push((*holding, text(created.ok(label), "/id", label)));
        }
    }

    (cluster, held)
}

/// The state each resource of a row is reported in, so the row's premise is pinned before
/// the delete is sent.
async fn assert_premise(emr: &Emr, label: &str, cluster: &str, held: &[(Holding, String)]) {
    for (holding, id) in held {
        let reported = if holding.is_endpoint() {
            endpoint_state(emr, cluster, id, label).await
        } else {
            run_state(emr, label, cluster, id).await
        };
        let expected = holding.expected_state();
        assert!(
            expected.contains(&reported.as_str()),
            "case {label:?}: {holding:?} {id} must be in one of {expected:?} before the \
             delete, got {reported:?}"
        );
    }
}

/// THE RULE: a cluster holding a resource that has not finished is not deleted, and the
/// same delete goes through once the resources it holds are over.
#[tokio::test]
async fn a_virtual_cluster_is_deleted_only_once_the_resources_it_holds_have_finished() {
    let rows: Vec<(&str, Vec<Holding>)> = vec![
        ("a cluster holding nothing", Vec::new()),
        ("one live job run", vec![Holding::LiveRun]),
        (
            "three live job runs",
            vec![Holding::LiveRun, Holding::LiveRun, Holding::LiveRun],
        ),
        (
            "a cancellation still in flight",
            vec![Holding::CancellingRun],
        ),
        ("an active managed endpoint", vec![Holding::ActiveEndpoint]),
        (
            "a managed endpoint still being created",
            vec![Holding::CreatingEndpoint],
        ),
        (
            "a managed endpoint still tearing down",
            vec![Holding::TerminatingEndpoint],
        ),
        (
            "a live run beside a live endpoint",
            vec![Holding::LiveRun, Holding::ActiveEndpoint],
        ),
        (
            "a run that has ended and one that has not",
            vec![Holding::EndedRun, Holding::LiveRun],
        ),
        (
            "only resources that have ended",
            vec![Holding::EndedRun, Holding::EndedEndpoint],
        ),
    ];

    for (index, (label, holds)) in rows.iter().enumerate() {
        let emr = Emr::new();
        let (cluster, held) = set_up(&emr, label, index, holds).await;
        assert_premise(&emr, label, &cluster, &held).await;

        let blocking: Vec<&(Holding, String)> = held
            .iter()
            .filter(|(holding, _)| holding.is_live())
            .collect();
        if !blocking.is_empty() {
            let before = cluster_record(&emr, label, &cluster).await;
            // Repeated, because a refusal must not be a one-shot: the state it reads is the
            // same on every attempt.
            for attempt in 0..2 {
                let answer = emr.delete_cluster(&cluster).await;
                answer.refused(label, 400, "ValidationException");
                let message = refusal_message(&answer.body, label);
                assert_eq!(
                    message, ACTIVE_RESOURCES,
                    "case {label:?}: attempt {attempt} must carry the one sentence a cluster \
                     holding live resources is refused with, got {message:?}"
                );
            }
            // The refused delete changed NOTHING about the cluster: not its state, not any
            // other member a read reports.
            let after = cluster_record(&emr, label, &cluster).await;
            assert_eq!(
                after, before,
                "case {label:?}: a refused delete must leave the cluster exactly as it was"
            );
            let reported = cluster_state(&emr, label, &cluster).await;
            assert_eq!(
                reported,
                state::CLUSTER_RUNNING,
                "case {label:?}: a refused delete must leave the cluster RUNNING, got \
                 {reported:?}"
            );

            // The caller ends what the cluster holds, each through its own operation.
            for (holding, id) in &blocking {
                if holding.is_endpoint() {
                    // A teardown already in flight needs no second delete.
                    if *holding != Holding::TerminatingEndpoint {
                        delete_endpoint(&emr, &cluster, id).await.ok(label);
                    }
                } else if *holding != Holding::CancellingRun {
                    cancel_run(&emr, &cluster, id).await.ok(label);
                }
            }
            wait_for_the_clock();
        }

        // With everything it held over, the delete goes through and starts the teardown.
        let answer = emr.delete_cluster(&cluster).await;
        let body = answer.ok(label).clone();
        assert_eq!(
            text(&body, "/id", label),
            cluster,
            "case {label:?}: the delete must answer the cluster it acted on: {body}"
        );
        let reported = cluster_state(&emr, label, &cluster).await;
        assert_eq!(
            reported,
            state::CLUSTER_TERMINATING,
            "case {label:?}: the accepted delete must leave the cluster TERMINATING, got \
             {reported:?}"
        );
        wait_for_the_clock();
        let reported = cluster_state(&emr, label, &cluster).await;
        assert_eq!(
            reported,
            state::CLUSTER_TERMINATED,
            "case {label:?}: the clock owed this cluster TERMINATED, got {reported:?}"
        );
    }
}

/// The refusal is about the resources the cluster the request NAMES holds: another
/// cluster's live job run blocks nothing, and a live run of a cluster in another Region
/// blocks nothing either.
#[tokio::test]
async fn only_the_named_clusters_own_resources_block_its_delete() {
    let emr = Emr::new();
    let case = "a neighbour's live job run";
    let quiet = cluster_id(&emr, case, "delete-quiet", "tok-delete-quiet", None).await;
    let busy = cluster_id(&emr, case, "delete-busy", "tok-delete-busy", None).await;
    let run = run_id(&emr, case, &busy, "run-tok-delete-busy").await;
    wait_for_the_clock();

    // The cluster that holds nothing is deleted while its neighbour's run is live.
    emr.delete_cluster(&quiet).await.ok(case);
    let reported = cluster_state(&emr, case, &quiet).await;
    assert_eq!(
        reported,
        state::CLUSTER_TERMINATING,
        "case {case:?}: a cluster holding nothing must be torn down whatever another \
         cluster holds, got {reported:?}"
    );

    // And the busy one is still refused, so the run really is live.
    let answer = emr.delete_cluster(&busy).await;
    answer.refused(case, 400, "ValidationException");
    assert_eq!(
        refusal_message(&answer.body, case),
        ACTIVE_RESOURCES,
        "case {case:?}: the cluster that holds the run must be refused with the one sentence"
    );
    let reported = run_state(&emr, case, &busy, &run).await;
    assert!(
        !state::job_state_is_terminal(&reported),
        "case {case:?}: the fixture run must still be live, got {reported:?}"
    );
}
