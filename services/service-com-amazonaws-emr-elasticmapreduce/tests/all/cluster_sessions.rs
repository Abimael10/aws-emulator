// SPDX-License-Identifier: CC0-1.0

//! The Spark Connect session surface of a cluster, end to end over the wire.
//!
//! THE RULE: the session surface exists only on a cluster whose LAUNCH enabled it — "set
//! `SessionEnabled` to true when creating the cluster" — so every operation of that surface
//! refuses a cluster without it, with the client error it models and Amazon EMR's own
//! sentence for the class, and `ListSessions` refuses rather than reporting an empty list.
//! On a cluster that does carry it, `StartSession` records a session — "when a session is
//! first created, it enters the `SUBMITTED` state" (`StartSessionOutput$State` doc, Smithy
//! model) — which `GetSession` and `ListSessions` then report, newest first, and which the
//! clock walks on to `STARTING`, `STARTED` and then `IDLE` — a session that is up and
//! running no work.
//!
//! THE IDENTITY HALF of that rule: each accepted `StartSession` records a session of its
//! OWN — its own minted id and its own ARN — and answers that recorded session, except
//! where the request repeats a `ClientRequestToken` an earlier accepted `StartSession` on
//! the same cluster carried, which answers the session that earlier request created and
//! records no second one ("if you retry a request that completed successfully using the
//! same client request token, the service returns the original response without performing
//! the operation again", `StartSessionInput$ClientRequestToken` doc, Smithy model — the
//! token being "a unique, case-sensitive identifier"). A start that names a cluster the
//! service resolves none of records nothing, and is refused through the channel the id's
//! own form settles: the cluster surface's modelled client error, under its discriminator,
//! for an id that could name a cluster; the request validator, naming the member, for one
//! that could not.
//!
//! The rows span what the rule generalises over: the operation of the surface that is called
//! and the request it carries, whether the cluster's launch enabled sessions (absent, false,
//! true), the state the cluster is in, the filters and page sizes a listing is given, how
//! many starts a cluster is asked for and whether they are sequential or in flight together,
//! whether a start carries a `ClientRequestToken` at all, whether that token repeats an
//! earlier one (and in which case), whether the rest of the request changed around a
//! repeated token, and the form of a cluster id that resolves nothing.
//! Every session is read back over the wire through `GetSession` and `ListSessions`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::{
    spark_release_labels, MAX_SESSIONS_PER_CLUSTER, SESSION_ID_PREFIX,
};

use crate::cluster_id_refusal_code::MALFORMED_CLUSTER_ID;
use crate::job_flow_lifecycle::{
    assert_minted_id, assert_refused, at, await_cluster_where, Emr, INSTANCE_PROFILE, SERVICE_ROLE,
};
use crate::refusal_messages::message;
use crate::resource_tags::error_code;

/// The sentence Amazon EMR refuses the session surface of a cluster without sessions with:
/// fixed text apart from the cluster id the request sent, quoted.
///
/// `pub(crate)` for the reason [`SPARK_RELEASE`] is.
pub(crate) fn not_enabled_sentence(cluster_id: &str) -> String {
    format!(
        "Sessions are not enabled on cluster '{cluster_id}'. Set SessionEnabled to true \
         when creating the cluster to enable sessions."
    )
}

/// The Amazon EMR Spark release the session surface is documented for: "this operation is
/// supported in Amazon EMR Spark 8.0.0 and later" (`StartSession` doc, Smithy model).
///
/// `pub(crate)` here and below so the sibling suite that drives a session's END states its
/// launch, waits for its cluster and polls its session through these helpers rather than
/// its own copies.
pub(crate) const SPARK_RELEASE: &str = "emr-spark-8.0.0";

/// A launch whose `SessionEnabled` is `enabled` when the row states one at all.
pub(crate) fn launch_request(name: &str, enabled: Option<bool>) -> Value {
    let mut request = json!({
        "Name": name,
        "ReleaseLabel": SPARK_RELEASE,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Spark" }],
        "Instances": {
            "InstanceCount": 2,
            "MasterInstanceType": "m5.xlarge",
            "SlaveInstanceType": "m5.xlarge",
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    });
    if let Some(enabled) = enabled {
        request["SessionEnabled"] = json!(enabled);
    }
    request
}

/// The states a cluster accepts a session in: "the cluster must be in the RUNNING or
/// WAITING state" (`StartSession` doc, Smithy model).
const SESSION_READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];

/// How long a wait for a session to reach the state the clock owes it may take before a
/// case gives up: far above every span this service owes a session, so a slow or loaded
/// host delays a wait rather than failing it ([`await_session_state`]).
const SESSION_WAIT_DEADLINE: Duration = Duration::from_secs(90);

/// One look at a session inside [`SESSION_WAIT_DEADLINE`].
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Launch the cluster `request` asks for and answer its id once it has reached a state
/// that accepts sessions.
///
/// The wait is on the CONDITION — the harness's own poll-until helper, whose deadline is far
/// above every span this service owes a cluster — so a slow or loaded host delays a case
/// rather than failing it.
pub(crate) async fn ready_cluster(emr: &Emr, case: &str, request: Value) -> String {
    let cluster_id = emr.run_job_flow(request).await;
    await_cluster_where(
        emr,
        &cluster_id,
        case,
        "a state that accepts sessions",
        |cluster| {
            SESSION_READY_STATES.contains(
                &at(cluster, &["Status", "State"]).as_str().unwrap_or_default(),
            )
        },
    )
    .await;
    cluster_id
}

/// Poll `GetSession` until the session reports `state`, and answer what it then reported.
///
/// Waits for the CONDITION rather than for a span: the deadline is far above every span a
/// session's start takes, so a slow host delays this rather than failing it.
pub(crate) async fn await_session_state(
    emr: &Emr,
    cluster_id: &str,
    session_id: &str,
    case: &str,
    state: &str,
) -> Value {
    let deadline = Instant::now() + SESSION_WAIT_DEADLINE;
    loop {
        let session = emr
            .ok(
                "GetSession",
                json!({ "ClusterId": cluster_id, "SessionId": session_id }),
            )
            .await["Session"]
            .clone();
        if session["State"].as_str() == Some(state) {
            return session;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the session never reached {state}, last {session}"
        );
        tokio::time::sleep(SESSION_POLL_INTERVAL).await;
    }
}

/// Assert that `answer` is `StartSession`'s answer for a session of `cluster_id` — a minted
/// id, the cluster the session runs on, its own ARN under that id, and the owning account —
/// and answer the session id it names.
fn assert_session_answer(case: &str, cluster_id: &str, answer: &Value) -> String {
    let id = answer["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartSession answers an Id, got {answer}"))
        .to_string();
    assert_minted_id(case, SESSION_ID_PREFIX, &id);
    assert_eq!(
        answer["ClusterId"].as_str(),
        Some(cluster_id),
        "case {case:?}: the answer names the cluster the session runs on, got {answer}"
    );
    let arn = answer["Arn"].as_str().unwrap_or_default();
    assert!(
        arn.starts_with("arn:aws:elasticmapreduce:") && arn.ends_with(&format!("session/{id}")),
        "case {case:?}: the answer names the session's own ARN, got {arn:?} in {answer}"
    );
    assert!(
        answer["AccountId"].as_str().is_some_and(|account| {
            account.len() == 12 && account.chars().all(|digit| digit.is_ascii_digit())
        }),
        "case {case:?}: the answer names the owning account, got {answer}"
    );
    id
}

/// The session ids `ListSessions` reports for `cluster_id`, in the order the pages report
/// them, asking for `max_results` per page where a case names one and paging until the
/// listing hands back no further token. Every page is held to the size the request asked
/// for.
///
/// The walk is BOUNDED: a cluster holds at most [`MAX_SESSIONS_PER_CLUSTER`] sessions, so a
/// listing that keeps handing back a token past that many pages is answering a token it
/// never advances, and the case says so rather than paging for ever.
async fn paged_session_ids(
    emr: &Emr,
    cluster_id: &str,
    max_results: Option<usize>,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=MAX_SESSIONS_PER_CLUSTER {
        let mut request = json!({ "ClusterId": cluster_id });
        if let Some(max) = max_results {
            request["MaxResults"] = json!(max);
        }
        if let Some(token) = token.as_ref() {
            request["NextToken"] = json!(token);
        }
        let page = emr.ok("ListSessions", request).await;
        let batch: Vec<String> = at(&page, &["Sessions"])
            .as_array()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter_map(|session| session["Id"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(max) = max_results {
            assert!(
                batch.len() <= max,
                "a page of {max} holds at most {max} sessions, got {page}"
            );
        }
        ids.extend(batch);
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            return ids;
        }
    }
    panic!(
        "ListSessions kept handing back the token {token:?} for cluster {cluster_id:?} past \
         {MAX_SESSIONS_PER_CLUSTER} pages, having reported {ids:?}"
    );
}

/// The sessions `ListSessions` reports for `cluster_id` as a SET, for a case about which
/// sessions a cluster holds rather than the order they are reported in.
async fn listed_session_ids(emr: &Emr, cluster_id: &str) -> BTreeSet<String> {
    paged_session_ids(emr, cluster_id, None)
        .await
        .into_iter()
        .collect()
}

/// THE RULE's refusal half: on a cluster whose launch did not enable sessions, BOTH
/// `StartSession` and `ListSessions` answer HTTP 400 `InvalidRequestException` with the
/// sentence above — the listing too, rather than an empty `Sessions` list.
#[tokio::test]
async fn the_session_surface_refuses_a_cluster_that_was_not_launched_with_sessions() {
    struct Case {
        label: &'static str,
        /// What the launch said about `SessionEnabled`.
        enabled: Option<bool>,
    }

    let cases = [
        Case {
            label: "a launch that said nothing about sessions",
            enabled: None,
        },
        Case {
            label: "a launch that disabled sessions explicitly",
            enabled: Some(false),
        },
    ];

    let emr = Emr::new();

    for case in cases {
        let label = case.label;
        let cluster_id = ready_cluster(&emr, label, launch_request(label, case.enabled)).await;

        // Every request shape of the surface, including a listing with filters and a page
        // size, is refused the same way: the gate is about the CLUSTER, not the request.
        let requests = [
            ("StartSession", json!({ "ClusterId": cluster_id.clone() })),
            (
                "StartSession",
                json!({ "ClusterId": cluster_id.clone(), "Name": "analysis" }),
            ),
            ("ListSessions", json!({ "ClusterId": cluster_id.clone() })),
            (
                "ListSessions",
                json!({
                    "ClusterId": cluster_id.clone(),
                    "SessionStates": ["STARTED"],
                    "MaxResults": 10,
                }),
            ),
        ];
        for (operation, body) in requests {
            let case = format!("{label}: {operation}");
            let (status, payload) = emr.call(operation, body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {payload}"
            );
            let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
            assert!(
                code.contains("InvalidRequestException"),
                "case {case:?}: expected InvalidRequestException, got {code:?} in {payload}"
            );
            assert_eq!(
                message(&case, &payload),
                not_enabled_sentence(&cluster_id),
                "case {case:?}: expected the sessions-not-enabled sentence, got {payload}"
            );
        }
    }
}

/// THE RULE's positive half: a cluster launched with sessions starts them, reports each one
/// through `GetSession` and `ListSessions` newest first, walks it from `SUBMITTED` to
/// `STARTED`, and answers a repeat of the same `ClientRequestToken` with the session the
/// first request created.
#[tokio::test]
async fn a_session_enabled_cluster_starts_sessions_and_reports_them() {
    let emr = Emr::new();
    let cluster_id = ready_cluster(&emr, "sessions", launch_request("sessions", Some(true))).await;

    // The requests span what `StartSession` accepts: a bare one, a named one, one with an
    // execution role, engine configuration overrides, an idle timeout, tags and a client
    // request token.
    let requests = vec![
        ("bare", json!({ "ClusterId": cluster_id.clone() })),
        (
            "named",
            json!({ "ClusterId": cluster_id.clone(), "Name": "analysis" }),
        ),
        (
            "configured",
            json!({
                "ClusterId": cluster_id.clone(),
                "Name": "configured",
                "ExecutionRoleArn": "arn:aws:iam::000000000000:role/EMR_Session_Role",
                "EngineConfigurations": [{
                    "Classification": "spark-defaults",
                    "Properties": { "spark.executor.memory": "2g" },
                }],
                "SessionIdleTimeoutInMinutes": 30,
                "Tags": [{ "Key": "team", "Value": "data" }],
                "ClientRequestToken": "token-configured",
            }),
        ),
    ];

    let mut started: Vec<(String, String)> = Vec::new();
    for (label, request) in &requests {
        let answer = emr.ok("StartSession", request.clone()).await;
        let id = assert_session_answer(label, &cluster_id, &answer);
        assert_eq!(
            answer["State"].as_str(),
            Some("SUBMITTED"),
            "case {label:?}: a session first created is SUBMITTED, got {answer}"
        );
        started.push(((*label).to_string(), id));
    }

    // Every session is read back through `GetSession`, with the configuration its own
    // request supplied and the cluster's release beside it.
    for (label, id) in &started {
        let session = emr
            .ok(
                "GetSession",
                json!({ "ClusterId": cluster_id.clone(), "SessionId": id.clone() }),
            )
            .await["Session"]
            .clone();
        assert_eq!(
            session["Id"].as_str(),
            Some(id.as_str()),
            "case {label:?}: GetSession reports the session that was started, got {session}"
        );
        assert_eq!(
            session["ReleaseLabel"].as_str(),
            Some(SPARK_RELEASE),
            "case {label:?}: a session reports the release its CLUSTER runs, got {session}"
        );
        assert!(
            session["CreatedAt"].as_f64().is_some(),
            "case {label:?}: a session reports when it was created, got {session}"
        );
        // No Spark Connect server is served for a session, so no URL is reported for one.
        assert!(
            session.get("ServerUrl").is_none_or(Value::is_null),
            "case {label:?}: no session reports a Spark Connect URL, got {session}"
        );
    }

    let configured = emr
        .ok(
            "GetSession",
            json!({
                "ClusterId": cluster_id.clone(),
                "SessionId": started[2].1.clone(),
            }),
        )
        .await["Session"]
        .clone();
    assert_eq!(
        configured["Name"].as_str(),
        Some("configured"),
        "the session reports the name its request gave it, got {configured}"
    );
    assert_eq!(
        configured["SessionIdleTimeoutInMinutes"].as_i64(),
        Some(30),
        "the session reports the idle timeout its request gave it, got {configured}"
    );
    assert_eq!(
        configured["EngineConfigurations"][0]["Classification"].as_str(),
        requests[2].1["EngineConfigurations"][0]["Classification"].as_str(),
        "the session reports the engine configuration its request gave it, got {configured}"
    );
    assert_eq!(
        configured["Tags"][0]["Key"].as_str(),
        Some("team"),
        "the session reports the tags its request gave it, got {configured}"
    );

    // "Newer sessions are returned first." (`ListSessions` doc.)
    let listed = emr
        .ok("ListSessions", json!({ "ClusterId": cluster_id.clone() }))
        .await;
    let ids: Vec<&str> = at(&listed, &["Sessions"])
        .as_array()
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| session["Id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let newest_first: Vec<&str> = started
        .iter()
        .rev()
        .map(|(_, id)| id.as_str())
        .collect();
    assert_eq!(
        ids, newest_first,
        "ListSessions reports every session newest first, got {listed}"
    );

    // One page per session: the token is the first session the page did not report.
    let paged = paged_session_ids(&emr, &cluster_id, Some(1)).await;
    assert_eq!(
        paged,
        newest_first
            .iter()
            .map(|id| (*id).to_string())
            .collect::<Vec<String>>(),
        "paging reports every session exactly once, in the same order"
    );

    // A token the listing did not mint is refused.
    let (status, payload) = emr
        .call(
            "ListSessions",
            json!({ "ClusterId": cluster_id.clone(), "NextToken": "is-NOTATOKEN" }),
        )
        .await;
    assert_eq!(
        status, 400,
        "an unminted NextToken is refused, got {status} with {payload}"
    );

    // The same `ClientRequestToken` answers the session the first request created, and
    // starts no second one.
    let repeated = emr.ok("StartSession", requests[2].1.clone()).await;
    assert_eq!(
        repeated["Id"].as_str(),
        Some(started[2].1.as_str()),
        "a repeated ClientRequestToken answers the first session, got {repeated}"
    );
    let listed = emr
        .ok("ListSessions", json!({ "ClusterId": cluster_id.clone() }))
        .await;
    assert_eq!(
        at(&listed, &["Sessions"]).as_array().map(Vec::len),
        Some(started.len()),
        "a repeated ClientRequestToken starts no second session, got {listed}"
    );

    // The clock walks a session on: `STARTING` after one span, `STARTED` after two, and
    // `IDLE` — up and running no work, where the walk settles — after three, polled for
    // rather than waited out. The state filter then partitions the sessions by the state
    // they have reached.
    for (label, id) in &started {
        let session = await_session_state(&emr, &cluster_id, id, label, "IDLE").await;
        let started_at = session["StartedAt"].as_f64().unwrap_or_else(|| {
            panic!(
                "case {label:?}: a session that has started reports when it entered that \
                 state, got {session}"
            )
        });
        // "The date and time that the session entered the STARTED state"
        // (`Session$StartedAt` doc): the state it was created in came first, so the instant
        // it started follows the instant it was created.
        let created_at = session["CreatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: a session reports when it was created, got {session}")
        });
        assert!(
            started_at > created_at,
            "case {label:?}: StartedAt {started_at} must follow CreatedAt {created_at}, got \
             {session}"
        );
    }
    for (states, expected) in [
        (json!(["IDLE"]), started.len()),
        (json!(["SUBMITTED"]), 0),
        (json!(["STARTED"]), 0),
        (json!(["SUBMITTED", "IDLE"]), started.len()),
        (json!(["TERMINATED"]), 0),
        // A filter that names no state narrows nothing, the way an absent one does.
        (json!([]), started.len()),
    ] {
        let filtered = emr
            .ok(
                "ListSessions",
                json!({ "ClusterId": cluster_id.clone(), "SessionStates": states.clone() }),
            )
            .await;
        assert_eq!(
            at(&filtered, &["Sessions"]).as_array().map(Vec::len),
            Some(expected),
            "case {states:?}: expected {expected} sessions, got {filtered}"
        );
    }
}

/// The release family the session surface needs: a cluster launches on an Amazon EMR Spark
/// release, and `DescribeReleaseLabel` reports what one bundles. The rows span the releases
/// the family publishes and the spellings around them, so a label the family does not
/// publish is refused rather than launched.
#[tokio::test]
async fn a_cluster_launches_on_an_amazon_emr_spark_release() {
    let emr = Emr::new();

    // `(the label, whether a launch may name it)`. The offered releases come from the
    // service's own family (`state::spark_release_labels`), newest first, and the rows
    // around them are the spellings next to them: another patch, another line, another
    // case.
    let offered = spark_release_labels();
    assert_eq!(
        offered,
        vec!["emr-spark-8.1.0".to_string(), SPARK_RELEASE.to_string()],
        "the Spark family publishes its releases newest first, including the one the \
         session surface needs, got {offered:?}"
    );
    let rows: Vec<(&str, bool)> = vec![
        (SPARK_RELEASE, true),
        ("emr-spark-8.1.0", true),
        ("emr-spark-8.0.1", false),
        ("emr-spark-8.2.0", false),
        ("emr-spark-7.0.0", false),
        ("emr-spark-8.0", false),
        ("emr-SPARK-8.0.0", false),
        ("emr-spark-8.0.0.0", false),
        // The series that is not this family is unaffected: it resolves as it always did.
        ("emr-7.1.0", true),
    ];

    for (label, launchable) in rows {
        let mut request = launch_request(label, Some(true));
        request["ReleaseLabel"] = json!(label);
        let (status, payload) = emr.call("RunJobFlow", request).await;
        if !launchable {
            assert_eq!(
                status, 400,
                "case {label:?}: a label the catalog does not offer launches nothing, got \
                 {status} with {payload}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {label:?}: expected the launch to be accepted, got {status} with {payload}"
        );
        let cluster_id = payload["JobFlowId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: RunJobFlow answers an id"))
            .to_string();
        let described = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            described["ReleaseLabel"].as_str(),
            Some(label),
            "case {label:?}: the cluster reports the release it was launched on, got \
             {described}"
        );
    }

    // What the family's release bundles, read back through the operation that reports it.
    let described = emr
        .ok(
            "DescribeReleaseLabel",
            json!({ "ReleaseLabel": SPARK_RELEASE }),
        )
        .await;
    assert_eq!(
        described["ReleaseLabel"].as_str(),
        Some(SPARK_RELEASE),
        "the description names the release asked for, got {described}"
    );
    let applications: Vec<&str> = described["Applications"]
        .as_array()
        .map(|applications| {
            applications
                .iter()
                .filter_map(|application| application["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        applications,
        vec!["Spark"],
        "an Amazon EMR Spark release bundles Spark, got {described}"
    );
}

/// The sentence Amazon EMR refuses a start on a cluster that is not up with: fixed text
/// around two variable parts, the cluster the request named and the state that cluster is in
/// at the call.
fn not_running_sentence(cluster_id: &str, state: &str) -> String {
    format!(
        "StartSession is not supported for cluster '{cluster_id}' in state {state}. \
         The cluster must be in RUNNING or WAITING state."
    )
}

/// A cluster that carries the surface but is not up: "the cluster must be in the RUNNING or
/// WAITING state" (`StartSession` doc), so a session cannot be started on one that has not
/// come up yet or that is shutting down — and the refusal names the cluster and the state it
/// is in.
///
/// The rows span the sides of the cluster's own walk a start can land on: one that has not
/// reached a state accepting sessions yet, and one that is on its way out — each held to the
/// sentence for every state it could be in when the call arrived, since a row's cluster is
/// walked by the clock and not by the case.
#[tokio::test]
async fn a_session_is_not_started_on_a_cluster_that_is_not_running() {
    /// Hold a start on `cluster_id` to the refusal, naming the cluster and whichever of
    /// `states` the cluster is in when the call arrives — a row's cluster is walked by the
    /// clock, not by the case, so every state it could be in is a sentence the row accepts.
    async fn assert_start_refused(emr: &Emr, case: &str, cluster_id: &str, states: &[&str]) {
        let (status, payload) = emr
            .call("StartSession", json!({ "ClusterId": cluster_id }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a cluster that is not up starts no session, got {status} with \
             {payload}"
        );
        let said = message(case, &payload);
        let wanted: Vec<String> = states
            .iter()
            .map(|state| not_running_sentence(cluster_id, state))
            .collect();
        assert!(
            wanted.contains(&said),
            "case {case:?}: expected one of {wanted:?}, got {said:?}"
        );

        // The listing still answers for that cluster: its sessions are a list, and it has
        // none.
        let listed = emr
            .ok("ListSessions", json!({ "ClusterId": cluster_id }))
            .await;
        assert_eq!(
            at(&listed, &["Sessions"]).as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a session-enabled cluster with no sessions lists none, got \
             {listed}"
        );
    }

    let emr = Emr::new();

    // Coming up: the launch has been accepted and nothing has waited for it, so the cluster
    // is in one of the phases it passes through on the way to a state that accepts sessions.
    let coming_up = emr
        .run_job_flow(launch_request("coming up", Some(true)))
        .await;
    assert_start_refused(
        &emr,
        "a cluster still coming up",
        &coming_up,
        &["STARTING", "BOOTSTRAPPING"],
    )
    .await;

    // On its way out: a cluster that came up and was then terminated.
    let going_down = ready_cluster(
        &emr,
        "terminating",
        launch_request("terminating", Some(true)),
    )
    .await;
    emr.ok(
        "TerminateJobFlows",
        json!({ "JobFlowIds": [going_down.clone()] }),
    )
    .await;
    assert_start_refused(
        &emr,
        "a cluster on its way out",
        &going_down,
        &["TERMINATING", "TERMINATED"],
    )
    .await;
}

// ---------------------------------------------------------------------------
// What a start RECORDS: one session per accepted start, and one per token
// ---------------------------------------------------------------------------

/// One `StartSession` the identity table drives.
struct Start {
    /// What makes this row its own case, and the key a later row names it by.
    label: &'static str,
    /// The `ClientRequestToken` the request carries, if any.
    token: Option<&'static str>,
    /// The `Name` the request carries, if any.
    name: Option<&'static str>,
    /// The row whose session this one must answer, for a repeat of a request that already
    /// succeeded; `None` means the row must record a session of its own.
    answers: Option<&'static str>,
}

/// THE IDENTITY HALF of the rule, sequentially: every start that repeats no accepted token
/// records a session of its own, and a repeat answers the session the first request created
/// without recording a second one.
#[tokio::test]
async fn every_start_that_repeats_no_accepted_token_records_its_own_session() {
    // Rows differ along: no token at all (twice, and once with the same `Name` as another
    // row, so a name is not what identifies a session), two distinct tokens, a repeat of one
    // of them, a repeat of it carrying a different `Name`, and the same token spelled in
    // another case — "a unique, case-sensitive identifier"
    // (`StartSessionInput$ClientRequestToken` doc, Smithy model).
    let rows = [
        Start {
            label: "no token",
            token: None,
            name: None,
            answers: None,
        },
        Start {
            label: "no token, named",
            token: None,
            name: Some("analysis"),
            answers: None,
        },
        Start {
            label: "no token, named the same as the row before",
            token: None,
            name: Some("analysis"),
            answers: None,
        },
        Start {
            label: "token a",
            token: Some("start-a"),
            name: None,
            answers: None,
        },
        Start {
            label: "token b",
            token: Some("start-b"),
            name: Some("beta"),
            answers: None,
        },
        Start {
            label: "token a repeated",
            token: Some("start-a"),
            name: None,
            answers: Some("token a"),
        },
        Start {
            label: "token a repeated with another name",
            token: Some("start-a"),
            name: Some("renamed"),
            answers: Some("token a"),
        },
        Start {
            label: "token a in another case",
            token: Some("START-A"),
            name: None,
            answers: None,
        },
    ];

    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "sequential starts",
        launch_request("sequential starts", Some(true)),
    )
    .await;

    let mut by_label: Vec<(&str, String)> = Vec::new();
    let mut recorded: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let case = row.label;
        let mut request = json!({ "ClusterId": cluster_id.clone() });
        if let Some(token) = row.token {
            request["ClientRequestToken"] = json!(token);
        }
        if let Some(name) = row.name {
            request["Name"] = json!(name);
        }
        let answer = emr.ok("StartSession", request).await;
        let id = assert_session_answer(case, &cluster_id, &answer);

        match row.answers {
            None => {
                assert!(
                    recorded.insert(id.clone()),
                    "case {case:?}: a start that repeats no accepted token records a session \
                     of its own, got the already-recorded {id:?}"
                );
                assert_eq!(
                    answer["State"].as_str(),
                    Some("SUBMITTED"),
                    "case {case:?}: a session first created is SUBMITTED, got {answer}"
                );
            }
            Some(first) => {
                let expected = by_label
                    .iter()
                    .find(|(label, _)| *label == first)
                    .map(|(_, id)| id.clone())
                    .unwrap_or_else(|| panic!("case {case:?}: no row labelled {first:?}"));
                assert_eq!(
                    id, expected,
                    "case {case:?}: a repeated ClientRequestToken answers the session {first:?} \
                     created, got {id:?}"
                );
            }
        }
        by_label.push((case, id));
    }

    // The cluster holds exactly the sessions the rows that were not repeats recorded, each
    // one once — so a repeat performed the operation again on nothing.
    let listed = listed_session_ids(&emr, &cluster_id).await;
    assert_eq!(
        listed, recorded,
        "the cluster holds exactly the sessions the non-repeat rows recorded"
    );

    // Each one reads back through `GetSession` as the session that was started.
    for id in &recorded {
        let session = emr
            .ok(
                "GetSession",
                json!({ "ClusterId": cluster_id.clone(), "SessionId": id.clone() }),
            )
            .await["Session"]
            .clone();
        assert_eq!(
            session["Id"].as_str(),
            Some(id.as_str()),
            "case {id:?}: GetSession reports the session that was started, got {session}"
        );
        assert_eq!(
            session["ClusterId"].as_str(),
            Some(cluster_id.as_str()),
            "case {id:?}: the session names the cluster it was started on, got {session}"
        );
    }
}

/// THE SAME HALF with the starts IN FLIGHT TOGETHER: being concurrent changes neither side
/// of it, so the untokened starts still record a session each and the two carrying one token
/// still share the single session that token names.
#[tokio::test]
async fn concurrent_starts_on_one_cluster_record_one_session_each() {
    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "concurrent starts",
        launch_request("concurrent starts", Some(true)),
    )
    .await;

    let untokened = json!({ "ClusterId": cluster_id.clone() });
    let tokened = json!({
        "ClusterId": cluster_id.clone(),
        "ClientRequestToken": "start-together",
    });
    let (first, second, third, fourth, token_first, token_second) = tokio::join!(
        emr.ok("StartSession", untokened.clone()),
        emr.ok("StartSession", untokened.clone()),
        emr.ok("StartSession", untokened.clone()),
        emr.ok("StartSession", untokened.clone()),
        emr.ok("StartSession", tokened.clone()),
        emr.ok("StartSession", tokened.clone()),
    );

    let mut distinct: BTreeSet<String> = BTreeSet::new();
    for (case, answer) in [
        ("concurrent 1", &first),
        ("concurrent 2", &second),
        ("concurrent 3", &third),
        ("concurrent 4", &fourth),
    ] {
        let id = assert_session_answer(case, &cluster_id, answer);
        assert!(
            distinct.insert(id.clone()),
            "case {case:?}: each start in flight records a session of its own, got the \
             already-recorded {id:?}"
        );
    }

    let shared = assert_session_answer("concurrent token, first", &cluster_id, &token_first);
    let shared_again = assert_session_answer("concurrent token, second", &cluster_id, &token_second);
    assert_eq!(
        shared, shared_again,
        "two starts carrying one token answer one session, got {shared:?} and {shared_again:?}"
    );
    assert!(
        distinct.insert(shared.clone()),
        "case \"concurrent token\": the tokened start records a session of its own, got the \
         already-recorded {shared:?}"
    );

    let listed = listed_session_ids(&emr, &cluster_id).await;
    assert_eq!(
        listed, distinct,
        "the cluster holds exactly one session per accepted start"
    );
}

/// THE RULE's lookup half: a `ClusterId` the service resolves no cluster for records nothing,
/// and is refused through the channel the id's own form settles — the cluster surface's
/// modelled client error, under its discriminator, for an id that could name a cluster; the
/// request validator, naming the member, for one that could not.
#[tokio::test]
async fn a_start_naming_no_cluster_records_nothing() {
    // Cluster ids no cluster carries but a cluster COULD: the shape a real id has, a shorter
    // and a longer body, a bare prefix, the empty string (`ClusterId` is `len 0..256`, so the
    // boundary validator hands it to the handler) and another all-alphanumeric form.
    let unresolvable = [
        "j-ZZZZZZZZZZZZZ",
        "j-1",
        "j-000000000000000000000000",
        "j-",
        "",
        "not-a-cluster-id",
    ];
    // Values no cluster id is drawn from at all: an ARN, a value with a space, a path, a
    // query string and a non-ASCII body.
    let malformed = [
        "arn:aws:elasticmapreduce:us-east-1:000000000000:cluster/j-ABCDEFGHIJKLM",
        "j ABCDEFGHIJKLM",
        "j-ABCDEFGHIJKLM/session",
        "clusters?id=j-ABCDEFGHIJKLM",
        "j-ÅBCDEFGHIJKLM",
    ];

    let emr = Emr::new();
    // A session-enabled cluster carrying a session EXISTS throughout, so every refusal below
    // is a lookup of persisted state rather than a blanket refusal of the operation.
    let cluster_id = ready_cluster(
        &emr,
        "unresolvable ids",
        launch_request("unresolvable ids", Some(true)),
    )
    .await;
    let held = emr
        .ok("StartSession", json!({ "ClusterId": cluster_id.clone() }))
        .await;
    let held_id = assert_session_answer("the held session", &cluster_id, &held);

    for unknown in unresolvable {
        let case = format!("StartSession with the unresolvable cluster id {unknown:?}");
        let (status, payload) = emr
            .call("StartSession", json!({ "ClusterId": unknown }))
            .await;
        assert_refused(&case, status, &payload, "InvalidRequestException");
        assert_eq!(
            error_code(&payload).as_deref(),
            Some(MALFORMED_CLUSTER_ID),
            "case {case:?}: expected ErrorCode {MALFORMED_CLUSTER_ID}, got {payload}"
        );
        assert_eq!(
            message(&case, &payload),
            format!("Cluster id '{unknown}' is not valid."),
            "case {case:?}: expected the cluster-id sentence, got {payload}"
        );
    }

    for bad in malformed {
        let case = format!("StartSession with the malformed cluster id {bad:?}");
        let (status, payload) = emr.call("StartSession", json!({ "ClusterId": bad })).await;
        assert_refused(&case, status, &payload, "ValidationException");
        assert_eq!(
            message(&case, &payload),
            format!("clusterId '{bad}' is not valid."),
            "case {case:?}: expected the member sentence of the request validator, got {payload}"
        );
    }

    // Nothing was recorded for any of them: the cluster still holds the one session that was
    // started, and it still reads back.
    let listed = listed_session_ids(&emr, &cluster_id).await;
    assert_eq!(
        listed,
        BTreeSet::from([held_id.clone()]),
        "a refused start records nothing, so the cluster holds only the started session"
    );
    let session = emr
        .ok(
            "GetSession",
            json!({ "ClusterId": cluster_id.clone(), "SessionId": held_id.clone() }),
        )
        .await["Session"]
        .clone();
    assert_eq!(
        session["Id"].as_str(),
        Some(held_id.as_str()),
        "the started session still reads back, got {session}"
    );
}

/// THE RULE's bound: a cluster holds as many sessions as one carries and no more, so the
/// start AT the ceiling is accepted, the one PAST it is refused, and the cluster still
/// reports exactly the sessions it accepted.
#[tokio::test]
async fn a_cluster_holds_no_more_sessions_than_one_carries() {
    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "the session ceiling",
        launch_request("the session ceiling", Some(true)),
    )
    .await;

    let mut recorded: BTreeSet<String> = BTreeSet::new();
    for ordinal in 1..=MAX_SESSIONS_PER_CLUSTER {
        let case = format!("session {ordinal} of {MAX_SESSIONS_PER_CLUSTER}");
        let answer = emr
            .ok("StartSession", json!({ "ClusterId": cluster_id.clone() }))
            .await;
        let id = assert_session_answer(&case, &cluster_id, &answer);
        assert!(
            recorded.insert(id.clone()),
            "case {case:?}: every start up to the ceiling records its own session, got the \
             already-recorded {id:?}"
        );
    }

    let case = format!("the start past {MAX_SESSIONS_PER_CLUSTER} sessions");
    let (status, payload) = emr
        .call("StartSession", json!({ "ClusterId": cluster_id.clone() }))
        .await;
    assert_refused(&case, status, &payload, "InvalidRequestException");
    // The cluster the request named and the maximum it carries, both read off the request and
    // the service's own ceiling rather than written out as values.
    assert_eq!(
        message(&case, &payload),
        format!(
            "Cluster '{cluster_id}' already carries {MAX_SESSIONS_PER_CLUSTER} sessions, which \
             is the maximum."
        ),
        "case {case:?}: the refusal states the cluster and the maximum it carries, got {payload}"
    );

    let listed = listed_session_ids(&emr, &cluster_id).await;
    assert_eq!(
        listed, recorded,
        "the cluster holds exactly the sessions it accepted"
    );
    assert_eq!(
        listed.len(),
        MAX_SESSIONS_PER_CLUSTER,
        "the cluster holds the ceiling's worth of sessions, got {}",
        listed.len()
    );
}
