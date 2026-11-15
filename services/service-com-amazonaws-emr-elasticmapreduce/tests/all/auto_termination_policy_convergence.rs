// SPDX-License-Identifier: CC0-1.0

//! `PutAutoTerminationPolicy` under RACING callers, and the exactness of the idle time it
//! attaches.
//!
//! THE RULE: an attachment is a WRITE of the submitted policy to the named cluster, so
//! every accepted attachment answers 200 and the cluster afterwards reports exactly one of
//! the submitted policies — whole, byte for byte, with its `IdleTimeout` the same integer
//! the request carried — however many callers attach at once, and a write to one cluster
//! is no write to another. Nothing merges two submissions, nothing rounds an idle time,
//! and reading the attachment never changes it.
//!
//! The rows span what the rule generalises over: HOW MANY callers attach at once (2, 4 and
//! 8), the FORM of what they attach (the empty policy, an idle timeout, a removal), WHICH
//! cluster each attaches to, and the VALUE of the attached idle time across the window its
//! member documents.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, and a policy is only ever read back through `GetAutoTerminationPolicy`, so
//! these are evidence the response is computed from persisted state.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::auto_termination_policy::{policy, put, IDLE_TIMEOUT_BOUNDS};
use crate::job_flow_lifecycle::{minimal_request, Emr};

/// How many callers race in each row — the rule holds whatever the count is, so the rows
/// differ in it.
const RACER_COUNTS: [usize; 3] = [2, 4, 8];

/// One `PutAutoTerminationPolicy` body: the policy member when the row carries one, and no
/// member at all when it does not (the member is optional, and a request carrying none
/// attaches none).
fn put_body(cluster_id: &str, attached: &Value) -> Value {
    if attached.is_null() {
        json!({ "ClusterId": cluster_id })
    } else {
        json!({ "ClusterId": cluster_id, "AutoTerminationPolicy": attached })
    }
}

/// Distinct policies for `racers` racing callers: one empty policy, the rest naming idle
/// times spread across the documented window, so the policy a cluster converges on names
/// exactly one racer.
///
/// Every row this suite races carries at least two callers, and the policies are asserted
/// pairwise distinct here, so a future row cannot silently put two racers on one policy and
/// make a convergence case unable to tell them apart.
fn racing_policies(racers: usize) -> Vec<Value> {
    let [minimum, maximum] = IDLE_TIMEOUT_BOUNDS;
    assert!(
        racers >= 2,
        "case {racers} racers: a race needs at least two callers"
    );
    // Spread from the documented minimum across the window, so every racer that names an
    // idle time names a different one and all of them are inside the window.
    let step = (maximum - minimum) / racers as i64;
    let policies: Vec<Value> = (0..racers)
        .map(|index| {
            if index == 0 {
                json!({})
            } else {
                json!({ "IdleTimeout": minimum + step * (index as i64 - 1) })
            }
        })
        .collect();
    for (index, policy) in policies.iter().enumerate() {
        assert!(
            !policies[..index].contains(policy),
            "case {racers} racers: racer {index}'s policy {policy} must differ from every \
             earlier racer's, got {policies:?}"
        );
    }
    policies
}

/// Spawn one `PutAutoTerminationPolicy` per `(cluster id, policy)` entry of `attachments`,
/// every one of them in flight before any is awaited, and answer each racer's
/// `(status, payload)` in order.
///
/// The cluster is part of the entry, so one call races attachments to the SAME cluster and
/// another races one attachment per cluster.
async fn race_puts(emr: &Arc<Emr>, attachments: &[(String, Value)]) -> Vec<(u16, Value)> {
    let mut racing = Vec::with_capacity(attachments.len());
    for (cluster_id, attached) in attachments {
        let emr = Arc::clone(emr);
        let body = put_body(cluster_id, attached);
        racing.push(tokio::spawn(async move {
            emr.call("PutAutoTerminationPolicy", body).await
        }));
    }
    let mut answers = Vec::with_capacity(attachments.len());
    for racer in racing {
        answers.push(racer.await.expect("the racer ran to completion"));
    }
    answers
}

/// Every entry of `attachments` aimed at the one `cluster_id`, for a row that races
/// submissions to a single cluster.
fn all_against(cluster_id: &str, attachments: &[Value]) -> Vec<(String, Value)> {
    attachments
        .iter()
        .map(|attached| (cluster_id.to_string(), attached.clone()))
        .collect()
}

/// Racing attachments to ONE cluster all succeed and converge: the cluster afterwards
/// reports exactly one of the submitted policies, unmerged, and keeps reporting it.
// A runtime with several worker threads, so the racers spawned below really are in flight
// together rather than taking turns on one thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_puts_converge_on_one_submitted_policy() {
    for racers in RACER_COUNTS {
        let case = format!("{racers} racing attachments to one cluster");
        let emr = Arc::new(Emr::new());
        let cluster = emr.run_job_flow(minimal_request("raced attachment")).await;
        let attachments = racing_policies(racers);

        let answers = race_puts(&emr, &all_against(&cluster, &attachments)).await;
        for (index, (status, payload)) in answers.iter().enumerate() {
            assert_eq!(
                *status, 200,
                "case {case:?}: racer {index} must answer HTTP 200, got {status} with \
                 {payload}"
            );
        }

        // The converged attachment is one racer's submission as submitted: not a merge of
        // two of them, and not an invented one.
        let converged = policy(&emr, &case, &cluster).await;
        assert!(
            attachments.contains(&converged),
            "case {case:?}: the reported policy must be one of the submitted {attachments:?}, \
             got {converged}"
        );
        // And it STAYS converged: reading it changes nothing a later caller sees.
        for round in 1..=3 {
            let again = policy(&emr, &case, &cluster).await;
            assert_eq!(
                again, converged,
                "case {case:?}: read {round} must report the converged policy {converged}, \
                 got {again}"
            );
        }

        // A later single attachment settles the cluster deterministically, so the race
        // left the cluster writable rather than stuck on the policy it converged on.
        let settled = json!({ "IdleTimeout": 3_600 });
        put(&emr, &case, &cluster, &settled).await;
        let got = policy(&emr, &case, &cluster).await;
        assert_eq!(
            got, settled,
            "case {case:?}: a later attachment must replace the converged policy, got {got}"
        );
    }
}

/// Racing callers that all attach the SAME policy converge on exactly that policy: the
/// attachment is a write of what was submitted, so agreeing racers lose nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_identical_puts_attach_that_policy() {
    for (label, attached) in [
        ("the empty policy", json!({})),
        (
            "the documented minimum idle timeout",
            json!({ "IdleTimeout": IDLE_TIMEOUT_BOUNDS[0] }),
        ),
        (
            "the documented maximum idle timeout",
            json!({ "IdleTimeout": IDLE_TIMEOUT_BOUNDS[1] }),
        ),
    ] {
        for racers in RACER_COUNTS {
            let case = format!("{racers} racers attaching {label}");
            let emr = Arc::new(Emr::new());
            let cluster = emr.run_job_flow(minimal_request("agreed attachment")).await;
            let attachments = vec![attached.clone(); racers];

            let answers = race_puts(&emr, &all_against(&cluster, &attachments)).await;
            for (index, (status, payload)) in answers.iter().enumerate() {
                assert_eq!(
                    *status, 200,
                    "case {case:?}: racer {index} must answer HTTP 200, got {status} with \
                     {payload}"
                );
            }

            let got = policy(&emr, &case, &cluster).await;
            assert_eq!(
                got, attached,
                "case {case:?}: the cluster must report the agreed policy, got {got}"
            );
        }
    }
}

/// Racing attachments to DIFFERENT clusters are independent writes: each cluster reports
/// its own racer's policy, and a cluster nothing attached to reports none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_puts_to_distinct_clusters_do_not_cross_talk() {
    for racers in RACER_COUNTS {
        let case = format!("{racers} racing attachments, one per cluster");
        let emr = Arc::new(Emr::new());
        let attachments = racing_policies(racers);

        let mut clusters = Vec::with_capacity(racers);
        for index in 0..racers {
            clusters.push(
                emr.run_job_flow(minimal_request(&format!("raced cluster {index}")))
                    .await,
            );
        }
        // A cluster no racer touches, to show the writes reached only their own clusters.
        let untouched = emr.run_job_flow(minimal_request("untouched")).await;

        // One racer per cluster, each attaching its own policy: the same racing helper, with
        // the cluster varying across the entries rather than fixed.
        let requests: Vec<(String, Value)> = clusters
            .iter()
            .cloned()
            .zip(attachments.iter().cloned())
            .collect();
        for (index, (status, payload)) in race_puts(&emr, &requests).await.iter().enumerate() {
            assert_eq!(
                *status, 200,
                "case {case:?}: racer {index} must answer HTTP 200, got {status} with \
                 {payload}"
            );
        }

        for (index, (cluster, attached)) in clusters.iter().zip(&attachments).enumerate() {
            let got = policy(&emr, &case, cluster).await;
            assert_eq!(
                &got, attached,
                "case {case:?}: cluster {index} must report its own policy {attached}, got \
                 {got}"
            );
        }
        let got = policy(&emr, &case, &untouched).await;
        assert!(
            got.is_null(),
            "case {case:?}: a cluster no racer attached to must report no policy, got {got}"
        );
    }
}

/// An attachment racing a removal converges on ONE of the two outcomes — the submitted
/// policy or no policy at all — and never on a third: the cluster is never left with a
/// policy nobody submitted, and it stays readable and writable afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_racing_put_and_remove_converge_to_one_outcome() {
    for (label, attached) in [
        ("the empty policy", json!({})),
        (
            "an idle timeout inside the window",
            json!({ "IdleTimeout": 3_600 }),
        ),
    ] {
        let case = format!("a removal racing an attachment of {label}");
        let emr = Arc::new(Emr::new());
        let cluster = emr.run_job_flow(minimal_request("raced removal")).await;

        let putter = {
            let emr = Arc::clone(&emr);
            let body = put_body(&cluster, &attached);
            tokio::spawn(async move { emr.call("PutAutoTerminationPolicy", body).await })
        };
        let remover = {
            let emr = Arc::clone(&emr);
            let body = json!({ "ClusterId": cluster });
            tokio::spawn(async move { emr.call("RemoveAutoTerminationPolicy", body).await })
        };
        for (operation, racer) in [
            ("PutAutoTerminationPolicy", putter),
            ("RemoveAutoTerminationPolicy", remover),
        ] {
            let (status, payload) = racer.await.expect("the racer ran to completion");
            assert_eq!(
                status, 200,
                "case {case:?}: {operation} must answer HTTP 200, got {status} with {payload}"
            );
        }

        let converged = policy(&emr, &case, &cluster).await;
        assert!(
            converged == attached || converged.is_null(),
            "case {case:?}: the cluster must report either {attached} or no policy, got \
             {converged}"
        );
        // Whichever side won, the cluster takes an attachment afterwards.
        let settled = json!({ "IdleTimeout": IDLE_TIMEOUT_BOUNDS[1] });
        put(&emr, &case, &cluster, &settled).await;
        let got = policy(&emr, &case, &cluster).await;
        assert_eq!(
            got, settled,
            "case {case:?}: an attachment after the race must be reported, got {got}"
        );
    }
}

/// `IdleTimeout` values spanning the documented window, including the ones a fractional or
/// 32-bit path would mangle: each documented bound, one step inside each, and values whose
/// digits are all distinct.
fn exact_idle_timeouts() -> Vec<i64> {
    let [minimum, maximum] = IDLE_TIMEOUT_BOUNDS;
    vec![
        minimum,
        minimum + 1,
        61,
        3_601,
        86_399,
        123_457,
        543_210,
        maximum - 1,
        maximum,
    ]
}

/// The attached idle time is reported as the EXACT integer the request carried: the same
/// value, as a JSON integer, with no fractional or exponent rendering — the attachment
/// stores a `Long`, so nothing about it is approximate.
#[tokio::test]
async fn an_attached_idle_timeout_is_reported_exactly() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(minimal_request("exact idle time")).await;

    for idle_timeout in exact_idle_timeouts() {
        let case = format!("an idle timeout of {idle_timeout} seconds");
        put(
            &emr,
            &case,
            &cluster,
            &json!({ "IdleTimeout": idle_timeout }),
        )
        .await;

        let got = policy(&emr, &case, &cluster).await;
        let reported = got.get("IdleTimeout").cloned().unwrap_or(Value::Null);
        assert!(
            reported.is_i64(),
            "case {case:?}: IdleTimeout must be reported as an integer, got {reported}"
        );
        assert_eq!(
            reported.as_i64(),
            Some(idle_timeout),
            "case {case:?}: expected {idle_timeout}, got {reported}"
        );
        let rendered = reported.to_string();
        assert_eq!(
            rendered,
            idle_timeout.to_string(),
            "case {case:?}: expected the integer {idle_timeout} on the wire, got {rendered}"
        );
    }
}
