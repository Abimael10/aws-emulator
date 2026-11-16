// SPDX-License-Identifier: CC0-1.0

//! The application user interfaces served ON a cluster, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: every
//! answer below follows from what `RunJobFlow` persisted and from the state the cluster
//! has reached, read back through `DescribeCluster`.
//!
//! THE RULE: `GetOnClusterAppUIPresignedURL` answers from the cluster the request names.
//! A `ClusterId` the service resolves no cluster for is a bad REQUEST — HTTP 400
//! `ValidationException` carrying the cluster-id sentence — while a cluster it DOES
//! resolve is never refused that way: one that is not up yet is refused through the
//! client error the operation models, naming the state it is in, so a caller can tell an
//! id that can never work from one that works once the cluster is up. For a cluster that
//! is up, `PresignedURLReady` says whether there is an interface to open — the cluster
//! runs the application that serves the interface asked for — and the URL is that
//! cluster's own address, presigned afresh per call; a dry run reports the readiness and
//! hands back no URL.
//!
//! The rows span what the rule generalises over: every `OnClusterAppUIType` the model
//! declares, clusters whose launches installed different applications, every form of
//! unresolvable id, and the cluster states on the way up and down.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_state, kept_alive_request, Emr,
};
use crate::refusal_messages::message;
use crate::refusal_wording::UNKNOWN_CLUSTER_IDS;

/// The operation under test.
const OPERATION: &str = "GetOnClusterAppUIPresignedURL";

/// Every `OnClusterAppUIType` the model declares, each with the application that serves
/// that interface on the cluster. SOURCE: the types are
/// `smithy_query(query="shape", shape="OnClusterAppUIType")` — `SparkHistoryServer |
/// YarnTimelineService | TezUI | ApplicationMaster | JobHistoryServer | ResourceManager`;
/// which application serves each is the Amazon EMR Management Guide's "View web
/// interfaces hosted on Amazon EMR clusters".
const UI_TYPES: [(&str, &str); 6] = [
    ("SparkHistoryServer", "Spark"),
    ("YarnTimelineService", "Hadoop"),
    ("TezUI", "Tez"),
    ("ApplicationMaster", "Hadoop"),
    ("JobHistoryServer", "Hadoop"),
    ("ResourceManager", "Hadoop"),
];

/// The interface a request that names no `OnClusterAppUIType` is answered for.
const DEFAULT_UI_TYPE: &str = "SparkHistoryServer";

/// A kept-alive launch that installs `applications`, on a release the catalog carries so
/// each name resolves to the version that release ships. A launch handed no application
/// names none at all, which is a cluster running nothing that serves a web interface.
fn launch_running(name: &str, applications: &[&str]) -> Value {
    let mut request = kept_alive_request(name);
    request["ReleaseLabel"] = json!("emr-6.15.0");
    if !applications.is_empty() {
        request["Applications"] = json!(applications
            .iter()
            .map(|name| json!({ "Name": name }))
            .collect::<Vec<Value>>());
    }
    request
}

/// A cluster of `applications`, waited up to the state it stays in with no steps to run.
async fn running_cluster(emr: &Emr, name: &'static str, applications: &[&str]) -> String {
    let cluster_id = emr.run_job_flow(launch_running(name, applications)).await;
    await_cluster_state(emr, &cluster_id, name, "WAITING").await;
    cluster_id
}

/// The interface a presigned URL opens: everything before the token it carries.
fn opened_interface(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// THE RULE's unresolvable half: an id the service resolves no cluster for is refused as a
/// bad request — HTTP 400 `ValidationException` with the cluster-id sentence, quoting the
/// value the request sent — for every form of such an id, and while a real cluster exists.
#[tokio::test]
async fn an_unresolvable_cluster_id_is_refused_as_an_invalid_request() {
    let emr = Emr::new();
    let live = running_cluster(&emr, "app-ui-live", &["Spark"]).await;

    for unknown in UNKNOWN_CLUSTER_IDS {
        for ui_type in [None, Some("SparkHistoryServer"), Some("ResourceManager")] {
            let case = format!("{OPERATION} with cluster id {unknown:?} / {ui_type:?}");
            let mut request = json!({ "ClusterId": unknown });
            if let Some(ui_type) = ui_type {
                request["OnClusterAppUIType"] = json!(ui_type);
            }
            let (status, payload) = emr.call(OPERATION, request).await;
            assert_refused(&case, status, &payload, "ValidationException");
            assert_eq!(
                message(&case, &payload),
                format!("Cluster id '{unknown}' is not valid."),
                "case {case:?}: the cluster-id sentence quotes what the request sent, got \
                 {payload}"
            );
        }
    }

    // The live cluster is untouched and still answers, so each refusal above was a lookup.
    let out = emr.ok(OPERATION, json!({ "ClusterId": &live })).await;
    assert_eq!(
        at(&out, &["PresignedURLReady"]).as_bool(),
        Some(true),
        "a live Spark cluster serves its Spark history server, got {out}"
    );
}

/// THE RULE's resolvable half: a cluster the service DOES resolve is never refused as an
/// unresolvable id, whatever state it is in. A cluster that is not up serves no interface,
/// so the call is refused through the client error the operation models, naming the state
/// the cluster is in and the states that do serve one.
///
/// Rows span the states a cluster is in while it cannot serve: one on the way up — the
/// state a caller acting straight after the launch sees, since the span the first phase
/// takes is never zero — and one that has been terminated. Both are asserted with and
/// without a dry run, because a dry run is still a question about this cluster.
#[tokio::test]
async fn a_cluster_that_is_not_up_is_refused_as_a_wrong_state_not_as_a_bad_id() {
    let emr = Emr::new();

    // Asked about straight away, while the cluster is still coming up.
    let starting = emr
        .run_job_flow(launch_running("app-ui-starting", &["Spark"]))
        .await;
    assert_wrong_state(&emr, "a starting cluster", &starting).await;

    // And a cluster whose primary node is gone for good, which no span can change.
    let gone = running_cluster(&emr, "app-ui-gone", &["Spark"]).await;
    emr.ok("TerminateJobFlows", json!({ "JobFlowIds": [&gone] }))
        .await;
    await_cluster_state(&emr, &gone, "app-ui-gone", "TERMINATED").await;
    assert_wrong_state(&emr, "a terminated cluster", &gone).await;
}

/// Every dry-run form of one call against `cluster_id`, each refused for the cluster's
/// STATE: the modelled client error, naming the cluster and the states that serve an
/// interface, and never the sentence an id that resolves nothing answers.
async fn assert_wrong_state(emr: &Emr, label: &str, cluster_id: &str) {
    for dry_run in [None, Some(true), Some(false)] {
        let case = format!("{OPERATION} against {label} / DryRun {dry_run:?}");
        let mut request = json!({ "ClusterId": cluster_id });
        if let Some(dry_run) = dry_run {
            request["DryRun"] = json!(dry_run);
        }
        let (status, payload) = emr.call(OPERATION, request).await;
        assert_refused(&case, status, &payload, "InvalidRequestException");
        let said = message(&case, &payload);
        assert!(
            said.contains(cluster_id)
                && said.contains(
                    "an application user interface is only served by a cluster in the RUNNING \
                     or WAITING state."
                ),
            "case {case:?}: the refusal names the cluster and the states that serve, got \
             {payload}"
        );
        assert!(
            !said.contains("is not valid"),
            "case {case:?}: a cluster that RESOLVES is not refused as a bad id, got {payload}"
        );
    }
}

/// THE RULE's readiness half: for a cluster that is up, `PresignedURLReady` follows the
/// applications the launch installed — an interface served by an application the cluster
/// runs is ready and answered with that cluster's own URL, and one served by an
/// application it does not run is reported not ready with no URL at all.
///
/// Rows span every `OnClusterAppUIType` the model declares against three launches: one
/// running Spark alone, one running Hadoop and Tez, and one running neither.
#[tokio::test]
async fn readiness_follows_the_applications_the_cluster_runs() {
    let emr = Emr::new();
    let launches: [(&'static str, &[&str]); 3] = [
        ("spark only", &["Spark"]),
        ("hadoop and tez", &["Hadoop", "Tez"]),
        ("no applications", &[]),
    ];

    for (label, applications) in launches {
        let cluster_id = running_cluster(&emr, label, applications).await;
        for (ui_type, server) in UI_TYPES {
            let case = format!("{label} / {ui_type}");
            let serves = applications
                .iter()
                .any(|installed| installed.eq_ignore_ascii_case(server));
            let out = emr
                .ok(
                    OPERATION,
                    json!({ "ClusterId": &cluster_id, "OnClusterAppUIType": ui_type }),
                )
                .await;
            assert_eq!(
                at(&out, &["PresignedURLReady"]).as_bool(),
                Some(serves),
                "case {case:?}: {ui_type} is served by {server}, which this cluster \
                 {} run, got {out}",
                if serves { "does" } else { "does not" }
            );
            if serves {
                let url = at(&out, &["PresignedURL"])
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no URL in {out}"))
                    .to_string();
                let interface = opened_interface(&url);
                assert!(
                    interface.starts_with("https://")
                        && interface.contains(cluster_id.as_str())
                        && interface.contains(&ui_type.to_lowercase()),
                    "case {case:?}: the URL is this cluster's own {ui_type} address, got {url}"
                );
            } else {
                assert!(
                    at(&out, &["PresignedURL"]).is_null(),
                    "case {case:?}: no URL for an interface nothing serves, got {out}"
                );
            }
        }

        // A request that names no interface is answered for the default one, which is the
        // same answer naming it gives.
        let named = emr
            .ok(
                OPERATION,
                json!({ "ClusterId": &cluster_id, "OnClusterAppUIType": DEFAULT_UI_TYPE }),
            )
            .await;
        let unnamed = emr.ok(OPERATION, json!({ "ClusterId": &cluster_id })).await;
        assert_eq!(
            at(&unnamed, &["PresignedURLReady"]).as_bool(),
            at(&named, &["PresignedURLReady"]).as_bool(),
            "case {label:?}: a request naming no interface asks for {DEFAULT_UI_TYPE}, got \
             {unnamed} against {named}"
        );
        assert_eq!(
            at(&unnamed, &["PresignedURL"])
                .as_str()
                .map(opened_interface),
            at(&named, &["PresignedURL"]).as_str().map(opened_interface),
            "case {label:?}: both open the same interface, got {unnamed} against {named}"
        );
    }
}

/// THE RULE's dry-run and freshness half: a dry run reports the readiness and mints
/// nothing, while a call that is not a dry run presigns the URL AFRESH — the interface it
/// opens is the cluster's own, stable across calls, and the token in it is not.
///
/// Rows span the application the request names (none, one) and the repeats, so the token's
/// freshness is asserted over the whole table rather than within one row.
#[tokio::test]
async fn a_dry_run_mints_nothing_and_every_other_call_presigns_afresh() {
    let emr = Emr::new();
    let cluster_id = running_cluster(&emr, "app-ui-fresh", &["Spark", "Hadoop"]).await;

    let mut every_token: BTreeSet<String> = BTreeSet::new();
    for application_id in [None, Some("application_1700000000000_0001")] {
        for (ui_type, _) in UI_TYPES
            .iter()
            .copied()
            .filter(|(_, server)| *server != "Tez")
        {
            let case = format!("{ui_type} / application {application_id:?}");
            let mut request = json!({ "ClusterId": &cluster_id, "OnClusterAppUIType": ui_type });
            if let Some(application_id) = application_id {
                request["ApplicationId"] = json!(application_id);
            }

            // A dry run: the readiness, and no URL — so probing an interface vends no
            // credential.
            let mut dry = request.clone();
            dry["DryRun"] = json!(true);
            let probed = emr.ok(OPERATION, dry).await;
            assert_eq!(
                at(&probed, &["PresignedURLReady"]).as_bool(),
                Some(true),
                "case {case:?}: a served interface is ready for a dry run too, got {probed}"
            );
            assert!(
                at(&probed, &["PresignedURL"]).is_null(),
                "case {case:?}: a dry run hands back no URL, got {probed}"
            );

            let mut interfaces: BTreeSet<String> = BTreeSet::new();
            let mut tokens: Vec<String> = Vec::new();
            for repeat in 0..3 {
                let out = emr.ok(OPERATION, request.clone()).await;
                let url = at(&out, &["PresignedURL"])
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?} (call {repeat}): no URL in {out}"))
                    .to_string();
                match application_id {
                    Some(application_id) => assert!(
                        url.contains(application_id),
                        "case {case:?}: the application named is part of the interface, got \
                         {url}"
                    ),
                    None => assert!(
                        !url.contains("history/"),
                        "case {case:?}: no application was named, got {url}"
                    ),
                }
                interfaces.insert(opened_interface(&url).to_string());
                let token = url
                    .split_once('?')
                    .and_then(|(_, query)| query.split_once('='))
                    .map(|(_, token)| token.to_string())
                    .unwrap_or_else(|| panic!("case {case:?}: no token in {url}"));
                assert!(
                    !token.is_empty()
                        && token
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')),
                    "case {case:?}: the token travels in a URL query as itself, got {token:?}"
                );
                tokens.push(token);
            }
            assert_eq!(
                interfaces.len(),
                1,
                "case {case:?}: every call opens the same interface, got {interfaces:?}"
            );
            for token in tokens {
                assert!(
                    every_token.insert(token.clone()),
                    "case {case:?}: a token was handed out twice: {token:?}"
                );
            }
        }
    }
}

/// An `ExecutionRoleArn` naming no IAM role of this account names nothing to assume, so it
/// is refused as a bad request rather than ignored — on a cluster that would otherwise be
/// answered a URL, so the refusal is the member's and not the cluster's.
///
/// Rows span the forms such a value takes inside the member's `len 20..2048`: another
/// account's role, another service's resource, and a value that is no ARN at all.
#[tokio::test]
async fn an_execution_role_arn_naming_no_role_of_this_account_is_refused() {
    let emr = Emr::new();
    let cluster_id = running_cluster(&emr, "app-ui-role", &["Spark"]).await;

    let bad_arns = [
        "arn:aws:iam::999999999999:role/EMRRuntime",
        "arn:aws:s3:::a-bucket-that-is-not-a-role",
        "not-an-arn-at-all-but-long-enough",
    ];
    for role_arn in bad_arns {
        let case = format!("{OPERATION} with ExecutionRoleArn {role_arn:?}");
        let (status, payload) = emr
            .call(
                OPERATION,
                json!({ "ClusterId": &cluster_id, "ExecutionRoleArn": role_arn }),
            )
            .await;
        assert_refused(&case, status, &payload, "ValidationException");
        let said = message(&case, &payload);
        assert!(
            said.contains("ExecutionRoleArn") && said.contains(role_arn),
            "case {case:?}: the refusal names the member and the value sent, got {payload}"
        );
    }

    // The same cluster, with no execution role named, is still served.
    let out = emr.ok(OPERATION, json!({ "ClusterId": &cluster_id })).await;
    assert_eq!(
        at(&out, &["PresignedURLReady"]).as_bool(),
        Some(true),
        "the cluster itself is served, got {out}"
    );
}
