// SPDX-License-Identifier: CC0-1.0

//! How EMR's application-UI surface refuses a target it cannot resolve, end to end over
//! the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends.
//!
//! THE RULE: an operation refuses a target it resolves nothing for in ITS OWN surface's
//! words, under HTTP 400 — the sentence is the service's for that operation and that
//! class, with a value interpolated only where the service interpolates one:
//!
//! * the on-cluster presigned-URL read names a cluster by its `ClusterId`, so it quotes
//!   the id the request sent in the cluster-id sentence;
//! * the persistent-UI create names its target by ARN, so it states the same rule with NO
//!   value in it — while a value that is no usable target ARN at all is still refused as
//!   that value, which is what keeps the two classes apart;
//! * the persistent-UI read is served by the profiler backend behind the application-UI
//!   surface, so it states that no such interface exists inside that backend's own
//!   wrapper, naming the id the request sent and the request id of that one response.
//!
//! The rows span what the rule generalises over: several unresolvable values per
//! operation, and repeats where the per-response part of a sentence must differ.

use serde_json::json;

use crate::job_flow_lifecycle::{assert_refused, kept_alive_request, Emr};
use crate::refusal_messages::{message, wrapped_request_id};

/// Cluster ARNs of clusters this account holds none of: the form `RunJobFlow` mints an
/// ARN in, with ids and Regions that differ, so the sentence is asserted over the class
/// rather than over one value.
const UNKNOWN_CLUSTER_ARNS: &[&str] = &[
    "arn:aws:elasticmapreduce:us-east-1:123456789012:cluster/j-ZZZZZZZZZZZZZ",
    "arn:aws:elasticmapreduce:eu-west-1:000000000000:cluster/j-1ABCDEFGHIJKL",
    "arn:aws:elasticmapreduce:us-west-2:000000000000:cluster/j-NOSUCHCLUSTER",
];

/// `PersistentAppUIId`s no user interface is held under, spanning the form a minted id
/// has, a shorter and a longer body, a bare prefix and a value of another form entirely.
const UNKNOWN_UI_IDS: &[&str] = &[
    "p-ZZZZZZZZZZZZZ",
    "p-1",
    "p-000000000000000000000000",
    "p-",
    "not-a-ui-id",
];

/// THE RULE for the create: a target ARN that names a cluster this account does not hold
/// is refused with the cluster-id rule stated WITHOUT a value — the id read out of the ARN
/// is not echoed, and neither is the ARN — while a value that is no usable target ARN is
/// refused as the value it was.
#[tokio::test]
async fn the_create_states_the_cluster_rule_without_echoing_the_target() {
    let emr = Emr::new();
    // A cluster that DOES resolve, so each refusal below is a lookup rather than a blanket
    // refusal of the operation.
    let live = emr.ok("RunJobFlow", kept_alive_request("app-ui-target")).await;
    let live_arn = live["ClusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("RunJobFlow must answer a ClusterArn, got {live}"))
        .to_string();

    for target in UNKNOWN_CLUSTER_ARNS {
        let case = format!("CreatePersistentAppUI with the target {target:?}");
        let (status, payload) = emr
            .call(
                "CreatePersistentAppUI",
                json!({ "TargetResourceArn": target }),
            )
            .await;
        assert_refused(&case, status, &payload, "ValidationException");
        assert_eq!(
            message(&case, &payload),
            "Cluster id is not valid.",
            "case {case:?}: the create states the rule with no value in it, got {payload}"
        );
    }

    // A value that is no usable target ARN is a different class: it IS quoted.
    for target in [
        "a-target-resource-that-is-not-an-arn",
        "arn:aws:s3:::a-bucket-of-event-logs",
    ] {
        let case = format!("CreatePersistentAppUI with the target {target:?}");
        let (status, payload) = emr
            .call(
                "CreatePersistentAppUI",
                json!({ "TargetResourceArn": target }),
            )
            .await;
        assert_refused(&case, status, &payload, "ValidationException");
        assert_eq!(
            message(&case, &payload),
            format!("'{target}' is not valid."),
            "case {case:?}: an unusable target ARN is refused as the value sent, got {payload}"
        );
    }

    // The resolvable target is still served, so nothing above was a blanket refusal.
    let created = emr
        .ok(
            "CreatePersistentAppUI",
            json!({ "TargetResourceArn": &live_arn }),
        )
        .await;
    assert!(
        created["PersistentAppUIId"].as_str().is_some(),
        "a target this account holds is served, got {created}"
    );
}

/// THE RULE for the persistent-UI read: an id this account holds no interface under is
/// refused with the profiler backend's own wrapper around the statement that no such
/// interface exists — naming the id the request sent, and carrying a request id of its
/// own on every response.
#[tokio::test]
async fn the_persistent_ui_read_states_the_profiler_backends_own_refusal() {
    let emr = Emr::new();
    let mut seen_request_ids: Vec<String> = Vec::new();

    // Each unknown id is asked for twice: the sentence is the same, the request id is not.
    for unknown in UNKNOWN_UI_IDS {
        for repeat in 0..2 {
            let case = format!("DescribePersistentAppUI with {unknown:?} (call {repeat})");
            let (status, payload) = emr
                .call(
                    "DescribePersistentAppUI",
                    json!({ "PersistentAppUIId": unknown }),
                )
                .await;
            assert_refused(&case, status, &payload, "InvalidRequestException");
            let said = message(&case, &payload);
            let request_id = wrapped_request_id(&case, &said);
            assert!(
                !seen_request_ids.contains(&request_id),
                "case {case:?}: each refusal carries its own request id, got the \
                 already-reported {request_id:?}"
            );
            seen_request_ids.push(request_id.clone());
            assert_eq!(
                said,
                format!(
                    "PersistentAppUI with ID '{unknown}' does not exist (Service: \
                     EMRProfilers; Status Code: 400; Error Code: InvalidRequestException; \
                     Request ID: {request_id}; Proxy: null)"
                ),
                "case {case:?}: the profiler backend's own refusal, got {payload}"
            );
        }
    }
}
