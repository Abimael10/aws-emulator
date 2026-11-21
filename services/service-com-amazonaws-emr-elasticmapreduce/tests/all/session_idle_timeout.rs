// SPDX-License-Identifier: CC0-1.0

//! A session's `SessionIdleTimeoutInMinutes` is a TIMER: a session left alone ends itself.
//!
//! THE RULE under test: "the idle timeout, in minutes. If the session is idle for this
//! duration, Amazon EMR automatically terminates it."
//! (`StartSessionInput$SessionIdleTimeoutInMinutes` doc, Smithy model, mirrored on
//! `Session$SessionIdleTimeoutInMinutes`.) So a session started with that member reaches a
//! ready state, records the instant it fell idle — "the date and time that the session last
//! entered the `IDLE` state" (`Session$IdleSince` doc) — and then reaches `TERMINATED` with
//! NOBODY calling `TerminateSession`, no sooner than that many minutes after that instant.
//! A session started without the member asked for no such timer and stays up.
//!
//! The rows span what the rule generalises over: a session that named no timeout, one that
//! named a span of no minutes at all, one that named the smallest span there is, and one
//! that named a span far past the life of this case. Every row's session is started on ONE
//! cluster before any wait, so the suite spends one idle timeout rather than one per row.
//!
//! The timeout waited out is the smallest a request can state, and every wait waits for the
//! CONDITION — `GetSession` reporting the state — under a deadline far above it, so a slow
//! host delays the suite rather than failing it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cluster_sessions::{await_session_state, launch_request, ready_cluster};
use crate::job_flow_lifecycle::{at, Emr};

/// The idle timeout the waiting row states: one minute, the smallest span the member can
/// state in the unit it is stated in (`SessionIdleTimeoutInMinutes` is a `Long` with no
/// range of its own).
const IDLE_TIMEOUT_MINUTES: u64 = 1;

/// The same span in seconds, which is what the timestamps a session reports are compared in.
const IDLE_TIMEOUT_SECONDS: f64 = (IDLE_TIMEOUT_MINUTES * 60) as f64;

/// How long a wait for the end the timer owes may take: far above the idle timeout and the
/// spans this service walks a cluster and a session through, so the deadline is never what
/// decides a case.
const DEADLINE: Duration = Duration::from_secs(4 * 60 + IDLE_TIMEOUT_MINUTES * 180);

/// One look at a session inside [`DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The states a session is in once its end has begun ("the session enters the TERMINATING
/// state and then transitions to TERMINATED", `TerminateSession` doc, Smithy model).
const ENDING_STATES: [&str; 2] = ["TERMINATING", "TERMINATED"];

/// The state a session that is up and running no work rests in.
const IDLE: &str = "IDLE";

/// Start a session on `cluster_id`, stating `timeout` as its idle timeout where the row
/// names one, and answer its id.
async fn start_session(emr: &Emr, cluster_id: &str, name: &str, timeout: Option<i64>) -> String {
    let mut request = json!({ "ClusterId": cluster_id, "Name": name });
    if let Some(timeout) = timeout {
        request["SessionIdleTimeoutInMinutes"] = json!(timeout);
    }
    let answer = emr.ok("StartSession", request).await;
    answer["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: StartSession must answer an Id, got {answer}"))
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

/// Poll `GetSession` until the session reports `TERMINATED`, and answer what it then
/// reported.
async fn await_terminated(emr: &Emr, case: &str, cluster_id: &str, session_id: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let session = get_session(emr, cluster_id, session_id).await;
        if at(&session, &["State"]).as_str() == Some("TERMINATED") {
            return session;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the session never ended on its own, last {session}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: a session started with the idle timeout it names.
struct Row {
    /// What makes this row its own case.
    label: &'static str,
    /// The `SessionIdleTimeoutInMinutes` the start states, where it states one.
    timeout: Option<i64>,
    /// Whether the timer that member arms ends this session.
    ends: bool,
}

#[tokio::test]
async fn an_idle_session_terminates_itself_after_its_own_idle_timeout() {
    let rows = [
        Row {
            label: "a session that named no idle timeout",
            timeout: None,
            ends: false,
        },
        Row {
            // A span of no minutes at all is no span: it arms no timer, the way a cluster's
            // auto-termination policy stating no idle time arms none.
            label: "a session that named a timeout of no minutes",
            timeout: Some(0),
            ends: false,
        },
        Row {
            label: "a session that named the smallest timeout there is",
            timeout: Some(IDLE_TIMEOUT_MINUTES as i64),
            ends: true,
        },
        Row {
            label: "a session that named a timeout far past this case",
            timeout: Some(30),
            ends: false,
        },
    ];

    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "the session-enabled cluster the rows run on",
        launch_request("idle sessions", Some(true)),
    )
    .await;

    // Every row's session is started before any wait, so the rows wait out ONE idle timeout
    // together.
    let mut sessions: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        sessions.push((
            row,
            start_session(&emr, &cluster_id, row.label, row.timeout).await,
        ));
    }

    // Each session reaches the state the walk settles in — up, and running no work — and
    // records the instant it entered it, which is what the timer counts from.
    let mut idle_since: Vec<f64> = Vec::new();
    for (row, session_id) in &sessions {
        let label = row.label;
        let session = await_session_state(&emr, &cluster_id, session_id, label, IDLE).await;
        assert_eq!(
            at(&session, &["SessionIdleTimeoutInMinutes"]).as_i64(),
            row.timeout,
            "case {label:?}: the session reports the idle timeout its request stated, \
             got {session}"
        );
        let entered = at(&session, &["IdleSince"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: an idle session reports an IdleSince, got {session}")
        });
        let started_at = at(&session, &["StartedAt"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: a session that started reports a StartedAt, got {session}")
        });
        assert!(
            entered >= started_at,
            "case {label:?}: expected an IdleSince at or after StartedAt {started_at}, \
             got {entered} in {session}"
        );
        assert!(
            at(&session, &["EndedAt"]).is_null(),
            "case {label:?}: a session that is up reports no EndedAt, got {session}"
        );
        idle_since.push(entered);
    }

    // NOBODY calls TerminateSession: the row whose timer is armed ends itself, no sooner
    // than its own timeout after the instant it fell idle.
    for (index, (row, session_id)) in sessions.iter().enumerate() {
        if !row.ends {
            continue;
        }
        let label = row.label;
        let ended_session = await_terminated(&emr, label, &cluster_id, session_id).await;
        let ended = at(&ended_session, &["EndedAt"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: a session that ended reports an EndedAt, got {ended_session}")
        });
        let idle_for = ended - idle_since[index];
        assert!(
            idle_for >= IDLE_TIMEOUT_SECONDS,
            "case {label:?}: expected at least {IDLE_TIMEOUT_SECONDS}s of idle time before \
             the end, got {idle_for} in {ended_session}"
        );
        // The end says why the session went down, which is what tells it apart from one a
        // caller asked for.
        assert!(
            at(&ended_session, &["StateChangeReason"])
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "case {label:?}: the end reports why the session went down, got {ended_session}"
        );
    }

    // And every row that armed no timer is still up, having been idle the whole of that same
    // span.
    for (row, session_id) in &sessions {
        if row.ends {
            continue;
        }
        let label = row.label;
        let session = get_session(&emr, &cluster_id, session_id).await;
        let state = at(&session, &["State"]).as_str().unwrap_or_default();
        assert!(
            !ENDING_STATES.contains(&state),
            "case {label:?}: a session that armed no timer stays up, got {state:?} in {session}"
        );
        assert!(
            at(&session, &["EndedAt"]).is_null(),
            "case {label:?}: a session that stayed up reports no EndedAt, got {session}"
        );
    }

    // The listing partitions the cluster's sessions the same way: exactly the row whose
    // timer fired is reported as ended.
    let listed = emr
        .ok(
            "ListSessions",
            json!({ "ClusterId": cluster_id.clone(), "SessionStates": ["TERMINATED"] }),
        )
        .await;
    let ended_ids: Vec<&str> = at(&listed, &["Sessions"])
        .as_array()
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| session["Id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let expected: Vec<&str> = sessions
        .iter()
        .filter(|(row, _)| row.ends)
        .map(|(_, session_id)| session_id.as_str())
        .collect();
    assert_eq!(
        ended_ids, expected,
        "the TERMINATED filter reports exactly the sessions whose idle timer fired, \
         got {listed}"
    );
}
