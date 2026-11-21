// SPDX-License-Identifier: CC0-1.0

//! The END of a Spark Connect session and the ENDPOINT a client reaches a live one at, end
//! to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, and every session
//! is read back through `GetSession` and `ListSessions`, so these are evidence that the
//! answers are computed from persisted state.
//!
//! THE RULE: a session's state is a record the clock walks, and every session-scoped
//! operation answers from it.
//!
//! * `TerminateSession` ends ONE live session of the cluster: "after you call this
//!   operation, the session enters the TERMINATING state and then transitions to
//!   TERMINATED" (`TerminateSession` doc, Smithy model), so the call answers `TERMINATING`
//!   and the session reaches `TERMINATED` on its own, carrying `EndedAt` — "the date and
//!   time that the session was terminated or failed" (`Session$EndedAt` doc) — while
//!   staying readable, and every other session of the cluster is left where it was. That
//!   holds whatever live state the session was in when the request came, and asking twice
//!   settles it once.
//! * A session does not outlive the cluster it was started on, so a cluster that terminates
//!   leaves none of its sessions live.
//! * `GetSessionEndpoint` reports how to REACH a session that is up — "use the endpoint and
//!   token to connect a PySpark client to the session. Call this operation again when the
//!   token expires to obtain a new one" (its doc) — which is an address under the base this
//!   emulator is served at, naming the session, plus a token minted for that one call; a
//!   session that has not reached a ready state (`STARTED`/`IDLE`/`BUSY`) has no endpoint to
//!   connect to and the call is refused for the state the session is actually in.
//!
//! The rows span what the rule generalises over: the live state a session is terminated in,
//! how many times it is terminated, how many sessions the cluster holds beside it, the
//! state a session is in when its endpoint is asked for (coming up, ready, terminating,
//! ended), whether the cluster ended the session or the caller did, and — for the refusals
//! — whether the launch enabled sessions, whether the cluster id resolves, and whether the
//! session id resolves or could name a session at all.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::cluster_sessions::{
    await_session_state, launch_request, not_enabled_sentence, ready_cluster,
};
use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_state, epoch_secs, kept_alive_request, Emr,
};
use crate::refusal_messages::message;

/// The error the whole session surface models for a bad request
/// (`smithy_query(query="operation", operation="TerminateSession")`).
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The envelope the request validator refuses a member it cannot resolve a resource from
/// with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The states a session can be in before it is ready: "when a session is first created, it
/// enters the SUBMITTED state" (`StartSessionOutput$State` doc, Smithy model) and it is
/// `STARTING` while the cluster starts it, so a read that lands in either is the same case.
const NOT_YET_READY_STATES: [&str; 2] = ["SUBMITTED", "STARTING"];

/// The states a session is in once the end has been asked for: `TERMINATING` while it is
/// being torn down and `TERMINATED` after ("the session enters the TERMINATING state and
/// then transitions to TERMINATED", `TerminateSession` doc).
const ENDING_STATES: [&str; 2] = ["TERMINATING", "TERMINATED"];

/// Start a session on `cluster_id` and answer its id.
async fn start_session(emr: &Emr, cluster_id: &str, name: &str) -> String {
    let answer = emr
        .ok(
            "StartSession",
            json!({ "ClusterId": cluster_id, "Name": name }),
        )
        .await;
    answer["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer an Id, got {answer}"))
        .to_string()
}

/// `GetSession` for one session, as the wire reports it.
async fn get_session(emr: &Emr, cluster_id: &str, session_id: &str) -> Value {
    emr.ok(
        "GetSession",
        json!({ "ClusterId": cluster_id, "SessionId": session_id }),
    )
    .await["Session"]
        .clone()
}

/// The sessions `ListSessions` reports for `cluster_id`, keeping only the states `states`
/// names where a case names any.
async fn listed_sessions(emr: &Emr, cluster_id: &str, states: Option<&[&str]>) -> Vec<Value> {
    let mut request = json!({ "ClusterId": cluster_id });
    if let Some(states) = states {
        request["SessionStates"] = json!(states);
    }
    emr.ok("ListSessions", request).await["Sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The ids in a `ListSessions` page.
fn ids_of(sessions: &[Value]) -> Vec<String> {
    sessions
        .iter()
        .filter_map(|session| session["Id"].as_str())
        .map(str::to_string)
        .collect()
}

/// THE RULE's terminate half: `TerminateSession` answers `TERMINATING` for the session it
/// names, the clock settles that session in `TERMINATED` with an `EndedAt`, the session
/// stays readable through `GetSession` and `ListSessions`, and no other session of the
/// cluster moves.
///
/// The rows differ along the live state the session was in when the end was asked for and
/// how many times it was asked for; one row is left alone, so the "every other session is
/// untouched" half is read off the same cluster.
#[tokio::test]
async fn terminating_a_session_settles_it_terminated_and_leaves_the_others_alone() {
    struct Case {
        label: &'static str,
        /// Whether the session is waited for its ready state before the end is asked for.
        started_first: bool,
        /// How many `TerminateSession` calls the row makes.
        terminates: usize,
    }

    let cases = [
        Case {
            label: "a session terminated while it is still coming up",
            started_first: false,
            terminates: 1,
        },
        Case {
            label: "a session terminated once it has started",
            started_first: true,
            terminates: 1,
        },
        Case {
            label: "a session terminated twice",
            started_first: true,
            terminates: 2,
        },
    ];

    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "the session-enabled cluster the rows run on",
        launch_request("terminating sessions", Some(true)),
    )
    .await;

    // Every row's session, plus one the rows never touch, started before any end is asked
    // for: the waits below then overlap instead of running one after another.
    let mut sessions: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        sessions.push((case, start_session(&emr, &cluster_id, case.label).await));
    }
    let untouched = start_session(&emr, &cluster_id, "a session nobody ends").await;

    for (case, session_id) in &sessions {
        let label = case.label;
        if case.started_first {
            await_session_state(&emr, &cluster_id, session_id, label, "STARTED").await;
        }
        for attempt in 1..=case.terminates {
            let answer = emr
                .ok(
                    "TerminateSession",
                    json!({ "ClusterId": cluster_id, "SessionId": session_id }),
                )
                .await;
            assert_eq!(
                answer["ClusterId"].as_str(),
                Some(cluster_id.as_str()),
                "case {label:?} attempt {attempt}: the answer names the session's cluster, \
                 got {answer}"
            );
            assert_eq!(
                answer["SessionId"].as_str(),
                Some(session_id.as_str()),
                "case {label:?} attempt {attempt}: the answer names the session, got {answer}"
            );
            let state = answer["State"].as_str().unwrap_or_default();
            // The FIRST ask moves a live session into TERMINATING; a later one reports the
            // state the first ask already settled it on, and never a live state again.
            let expected: &[&str] = if attempt == 1 {
                &["TERMINATING"]
            } else {
                &ENDING_STATES
            };
            assert!(
                expected.contains(&state),
                "case {label:?} attempt {attempt}: expected a state in {expected:?}, \
                 got {state:?} in {answer}"
            );
        }
    }

    for (case, session_id) in &sessions {
        let label = case.label;
        let settled = await_session_state(&emr, &cluster_id, session_id, label, "TERMINATED").await;
        let created = at(&settled, &["CreatedAt"]).as_f64().unwrap_or_default();
        let ended = at(&settled, &["EndedAt"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: a terminated session reports an EndedAt, got {settled}")
        });
        assert!(
            ended >= created,
            "case {label:?}: expected an EndedAt at or after CreatedAt {created}, \
             got {ended} in {settled}"
        );
        assert_eq!(
            at(&settled, &["Id"]).as_str(),
            Some(session_id.as_str()),
            "case {label:?}: the ended session is still read back under its own id, \
             got {settled}"
        );
        assert!(
            at(&settled, &["Arn"]).as_str().is_some_and(|arn| arn
                .ends_with(&format!("session/{session_id}"))),
            "case {label:?}: the ended session still reports its own ARN, got {settled}"
        );
        // "The date and time that the session entered the STARTED state"
        // (`Session$StartedAt` doc): a session that got there reports one.
        if case.started_first {
            assert!(
                !at(&settled, &["StartedAt"]).is_null(),
                "case {label:?}: a session that started reports a StartedAt, got {settled}"
            );
        }
    }

    // Every ended session is still listed, and the state filter reports exactly them.
    let listed = ids_of(&listed_sessions(&emr, &cluster_id, None).await);
    for (case, session_id) in &sessions {
        assert!(
            listed.contains(session_id),
            "case {:?}: an ended session is still listed, got {listed:?}",
            case.label
        );
    }
    let terminated = ids_of(&listed_sessions(&emr, &cluster_id, Some(&["TERMINATED"])).await);
    let mut expected: Vec<String> = sessions
        .iter()
        .map(|(_, session_id)| session_id.clone())
        .collect();
    expected.sort();
    let mut reported = terminated.clone();
    reported.sort();
    assert_eq!(
        reported, expected,
        "the TERMINATED filter reports exactly the sessions that were ended, \
         got {terminated:?}"
    );

    // The session no row touched is untouched: it is not ended, and it is still listed.
    let other = get_session(&emr, &cluster_id, &untouched).await;
    assert!(
        !ENDING_STATES.contains(&at(&other, &["State"]).as_str().unwrap_or_default()),
        "a session nobody ended is still live, got {other}"
    );
    assert!(
        at(&other, &["EndedAt"]).is_null(),
        "a session nobody ended reports no EndedAt, got {other}"
    );
    assert!(
        listed.contains(&untouched),
        "a session nobody ended is still listed, got {listed:?}"
    );
}

/// THE RULE's cluster half: a session is started "on the specified cluster", so a cluster
/// that terminates leaves none of its sessions live — each reports `TERMINATED` with an
/// `EndedAt`, with no `TerminateSession` call at all.
#[tokio::test]
async fn a_terminated_cluster_leaves_none_of_its_sessions_live() {
    let emr = Emr::new();
    let case = "a cluster terminated under its sessions";
    let cluster_id = ready_cluster(
        &emr,
        case,
        launch_request("sessions of a dying cluster", Some(true)),
    )
    .await;

    // Rows: one session still coming up and one that has started, so the rule is read on
    // both sides of the start walk.
    let coming_up = start_session(&emr, &cluster_id, "still coming up").await;
    let started = start_session(&emr, &cluster_id, "already started").await;
    await_session_state(&emr, &cluster_id, &started, case, "STARTED").await;

    emr.ok(
        "TerminateJobFlows",
        json!({ "JobFlowIds": [cluster_id.clone()] }),
    )
    .await;
    await_cluster_state(&emr, &cluster_id, case, "TERMINATED").await;

    for (label, session_id) in [("a session still coming up", &coming_up), ("a started session", &started)] {
        let session = get_session(&emr, &cluster_id, session_id).await;
        assert_eq!(
            at(&session, &["State"]).as_str(),
            Some("TERMINATED"),
            "case {label:?}: a session of a terminated cluster has ended, got {session}"
        );
        assert!(
            !at(&session, &["EndedAt"]).is_null(),
            "case {label:?}: a session that ended with its cluster reports an EndedAt, \
             got {session}"
        );
    }
}

/// THE RULE's endpoint half: `GetSessionEndpoint` reports an address under the base this
/// emulator is served at, naming the session, with a freshly minted token and the instant
/// it expires at — and refuses a session that is not up, naming the state it is in.
///
/// The rows span the states a session passes through: still coming up, ready, being torn
/// down, ended.
#[tokio::test]
async fn the_session_endpoint_is_reported_for_a_ready_session_and_refused_otherwise() {
    let emr = Emr::new();
    let case = "the endpoint of a session";
    let cluster_id = ready_cluster(
        &emr,
        case,
        launch_request("session endpoints", Some(true)),
    )
    .await;
    let session_id = start_session(&emr, &cluster_id, "an endpoint to reach").await;

    // Coming up: no endpoint to connect to, and the refusal names the state the session is
    // actually in — either of the two states a session that has not started is in.
    let (status, payload) = emr
        .call(
            "GetSessionEndpoint",
            json!({ "ClusterId": cluster_id, "SessionId": session_id }),
        )
        .await;
    assert_refused(case, status, &payload, INVALID_REQUEST_EXCEPTION);
    let said = message(case, &payload);
    let wanted: Vec<String> = NOT_YET_READY_STATES
        .iter()
        .map(|state| not_ready_sentence(&session_id, state))
        .collect();
    assert!(
        wanted.contains(&said),
        "case {case:?}: expected one of {wanted:?}, got {said:?}"
    );

    // Ready: the endpoint and the token.
    await_session_state(&emr, &cluster_id, &session_id, case, "STARTED").await;
    let before = epoch_secs(SystemTime::now());
    let first = emr
        .ok(
            "GetSessionEndpoint",
            json!({ "ClusterId": cluster_id, "SessionId": session_id }),
        )
        .await;
    let endpoint = at(&first, &["Endpoint"]).as_str().unwrap_or_default().to_string();
    assert!(
        endpoint.starts_with("https://"),
        "case {case:?}: expected an https endpoint, got {endpoint:?} in {first}"
    );
    assert!(
        endpoint.ends_with(&format!("/clusters/{cluster_id}/sessions/{session_id}")),
        "case {case:?}: expected the session's own path under the served base, \
         got {endpoint:?} in {first}"
    );
    let token = at(&first, &["AuthToken"]).as_str().unwrap_or_default().to_string();
    assert!(
        !token.is_empty(),
        "case {case:?}: expected an authentication token, got {first}"
    );
    let expires = at(&first, &["AuthTokenExpirationTime"])
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected an expiry, got {first}"));
    assert!(
        expires >= before + MINIMUM_TOKEN_VALIDITY.as_secs_f64(),
        "case {case:?}: expected an expiry at least {MINIMUM_TOKEN_VALIDITY:?} after the \
         call at {before}, got {expires} in {first}"
    );

    // "Call this operation again when the token expires to obtain a new one": the next call
    // reports the SAME endpoint and a token of its own.
    let second = emr
        .ok(
            "GetSessionEndpoint",
            json!({ "ClusterId": cluster_id, "SessionId": session_id }),
        )
        .await;
    assert_eq!(
        at(&second, &["Endpoint"]).as_str(),
        Some(endpoint.as_str()),
        "case {case:?}: every call about one session reports the same endpoint, got {second}"
    );
    assert_ne!(
        at(&second, &["AuthToken"]).as_str(),
        Some(token.as_str()),
        "case {case:?}: each call mints a token of its own, got {second}"
    );
    // The pair for a direct Spark Connect connection belongs to a server no session of this
    // emulator offers, so no session reports it.
    assert!(
        at(&second, &["Credentials"]).is_null(),
        "case {case:?}: no credentials are reported for a connection nothing serves, \
         got {second}"
    );

    // Being torn down, and then ended: no endpoint either way.
    emr.ok(
        "TerminateSession",
        json!({ "ClusterId": cluster_id, "SessionId": session_id }),
    )
    .await;
    for label in ["a session being torn down", "a session that has ended"] {
        if label == "a session that has ended" {
            await_session_state(&emr, &cluster_id, &session_id, label, "TERMINATED").await;
        }
        let (status, payload) = emr
            .call(
                "GetSessionEndpoint",
                json!({ "ClusterId": cluster_id, "SessionId": session_id }),
            )
            .await;
        assert_refused(label, status, &payload, INVALID_REQUEST_EXCEPTION);
        let said = message(label, &payload);
        let wanted: Vec<String> = ENDING_STATES
            .iter()
            .map(|state| not_ready_sentence(&session_id, state))
            .collect();
        assert!(
            wanted.contains(&said),
            "case {label:?}: expected one of {wanted:?}, got {said:?}"
        );
    }
}

/// The sentence `GetSessionEndpoint` refuses a session that is not up with: fixed text
/// around the session the request named and the state that session is in.
fn not_ready_sentence(session_id: &str, state: &str) -> String {
    format!("GetSessionEndpoint is not supported because Session {session_id} is in {state} state.")
}

/// The floor the token's validity is held to: how long a `GetSessionEndpoint` token is good
/// for is this service's own choice, so a case reads the expiry it reports rather than
/// pinning a value and holds it only to being in the future by a margin — which is what
/// "time-limited" means for a token a client is told to use and then renew.
const MINIMUM_TOKEN_VALIDITY: Duration = Duration::from_secs(1);

/// THE RULE's refusal half, for both operations at once: the session surface refuses a
/// cluster whose launch did not enable sessions with Amazon EMR's sentence for that class,
/// a cluster id it resolves nothing for through the cluster surface's own refusal, and a
/// `SessionId` it can resolve no session from — one of a form no session is minted in, and
/// one that simply names none — on the channel each operation words that class on. No
/// refused call moves a session.
#[tokio::test]
async fn the_session_end_and_endpoint_refuse_what_they_cannot_resolve() {
    let emr = Emr::new();
    let with_sessions = ready_cluster(
        &emr,
        "the cluster the session rows run on",
        launch_request("session refusals", Some(true)),
    )
    .await;
    let live = start_session(&emr, &with_sessions, "a session nothing below touches").await;
    // A cluster of the same account whose launch asked for no sessions at all.
    let without_sessions = emr.run_job_flow(kept_alive_request("no sessions here")).await;

    // Ids no session is ever minted under: `SessionId` is `len 0..256` with no pattern, so
    // the boundary validator passes every one of these to the handler, which refuses the
    // SHAPE before any session is resolved.
    let malformed: [(&str, &str); 4] = [
        ("another resource's prefix", "j-ZZZZZZZZZZZZZ"),
        ("a bare prefix", "is-"),
        ("the empty string", ""),
        ("a value of another form", "session/1"),
    ];

    /// A well-formed session id the cluster carries no session for.
    const UNKNOWN_SESSION_ID: &str = "is-ZZZZZZZZZZZZ";

    for operation in ["TerminateSession", "GetSessionEndpoint"] {
        let case = format!("{operation} on a cluster launched without sessions");
        let (status, payload) = emr
            .call(
                operation,
                json!({ "ClusterId": without_sessions, "SessionId": live }),
            )
            .await;
        assert_refused(&case, status, &payload, INVALID_REQUEST_EXCEPTION);
        assert_eq!(
            message(&case, &payload),
            not_enabled_sentence(&without_sessions),
            "case {case:?}: expected the sessions-not-enabled sentence, got {payload}"
        );

        let case = format!("{operation} on a cluster id that resolves nothing");
        let unknown = "j-ZZZZZZZZZZZZZ";
        let (status, payload) = emr
            .call(
                operation,
                json!({ "ClusterId": unknown, "SessionId": live }),
            )
            .await;
        assert_refused(&case, status, &payload, INVALID_REQUEST_EXCEPTION);
        assert_eq!(
            message(&case, &payload),
            format!("Cluster id '{unknown}' is not valid."),
            "case {case:?}: expected the cluster-id sentence, got {payload}"
        );

        let case = format!("{operation} with a malformed cluster id");
        // A value carrying characters no cluster id is drawn from — a slash, here — names
        // no cluster the service could hold, so it is refused on its shape.
        let malformed_cluster = "cluster/j-1";
        let (status, payload) = emr
            .call(
                operation,
                json!({ "ClusterId": malformed_cluster, "SessionId": live }),
            )
            .await;
        assert_refused(&case, status, &payload, VALIDATION_EXCEPTION);
        assert_eq!(
            message(&case, &payload),
            format!("clusterId '{malformed_cluster}' is not valid."),
            "case {case:?}: expected the member sentence of the request validator, got {payload}"
        );

        for (label, session_id) in malformed {
            let case = format!("{operation} with {label}");
            let (status, payload) = emr
                .call(
                    operation,
                    json!({ "ClusterId": with_sessions, "SessionId": session_id }),
                )
                .await;
            assert_refused(&case, status, &payload, VALIDATION_EXCEPTION);
            assert_eq!(
                message(&case, &payload),
                format!("sessionId '{with_sessions}/session/{session_id}' is not valid."),
                "case {case:?}: expected the qualified session path, got {payload}"
            );
        }

        // A well-formed id the cluster simply carries no session for is refused on the
        // channel each operation words that class on: recorded behavior gives
        // `TerminateSession` the request validator's sentence naming the session's
        // qualified path, while a session READ states the session-id sentence under the
        // modelled client error, the way `GetSession` does.
        let case = format!("{operation} with a well-formed id no session carries");
        let (status, payload) = emr
            .call(
                operation,
                json!({ "ClusterId": with_sessions, "SessionId": UNKNOWN_SESSION_ID }),
            )
            .await;
        let (expected_code, expected_message) = if operation == "TerminateSession" {
            (
                VALIDATION_EXCEPTION,
                format!("sessionId '{with_sessions}/session/{UNKNOWN_SESSION_ID}' is not valid."),
            )
        } else {
            (
                INVALID_REQUEST_EXCEPTION,
                format!("Session id '{UNKNOWN_SESSION_ID}' is not valid."),
            )
        };
        assert_refused(&case, status, &payload, expected_code);
        assert_eq!(
            message(&case, &payload),
            expected_message,
            "case {case:?}: expected the sentence this operation words that class with, \
             got {payload}"
        );
    }

    // Nothing above moved the session that does exist: it is still live and still the only
    // session the cluster holds.
    let session = get_session(&emr, &with_sessions, &live).await;
    assert!(
        !ENDING_STATES.contains(&at(&session, &["State"]).as_str().unwrap_or_default()),
        "a refused call ends no session, got {session}"
    );
    assert_eq!(
        ids_of(&listed_sessions(&emr, &with_sessions, None).await),
        vec![live],
        "a refused call records and removes no session"
    );
}

