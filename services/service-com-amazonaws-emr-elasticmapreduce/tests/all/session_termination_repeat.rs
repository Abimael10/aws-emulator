// SPDX-License-Identifier: CC0-1.0

//! Asking for the END of a Spark Connect session MORE THAN ONCE, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the harness
//! `tests/all/job_flow_lifecycle.rs` owns, and reads every session back through
//! `GetSession` and `ListSessions`, so these are evidence that the answers are computed
//! from persisted state rather than named by the handler.
//!
//! THE RULE: `TerminateSession` answers the state of the session RECORD it just moved —
//! "after you call this operation, the session enters the TERMINATING state and then
//! transitions to TERMINATED" (`TerminateSession` doc, Smithy model) — so the end is owed
//! to the clock ONCE: the first accepted ask arms it, every later ask reports where the
//! session has got to without arming a second end, moving the session back to a live state,
//! or recording a second session. A session that has ended stays readable, with the
//! `EndedAt` the first ask settled it on.
//!
//! The rows span what the rule generalises over: the live state the session was in when the
//! end was first asked for (still coming up, or up and idle), how many asks are made back to
//! back before the clock has settled the session (one, two, three), and how many more are made
//! after it has settled (one, two). The sibling suite `tests/all/session_termination.rs` pins
//! the single-ask half of the same rule and the refusals; EVERY row here carries what it does
//! not: asks made after the session has already settled.
//!
//! Recorded behavior for the state those late asks are answered with: a session this account
//! has already ended is reported ended rather than refused, so each one is a 200 whose `State`
//! is `TERMINATED` — which is also why the rows below assert that nothing of the settled
//! session moves when it is asked again.

use serde_json::{json, Value};

use crate::cluster_sessions::{await_session_state, launch_request, ready_cluster};
use crate::job_flow_lifecycle::{at, Emr};
use crate::session_termination::{
    get_session, ids_of, listed_sessions, start_session, ENDING_STATES,
};

/// The state the ask that ARMS the end moves a session into, and the state the clock then
/// settles it in: the two ending states, named here off the pair the sibling suite declares so
/// the assertions below read as the states they are rather than as positions in it.
const TERMINATING: &str = ENDING_STATES[0];
const TERMINATED: &str = ENDING_STATES[1];

/// How far along the end `state` is, or `None` for a state that is not an ending one.
fn ending_rank(state: &str) -> Option<usize> {
    ENDING_STATES.iter().position(|ending| *ending == state)
}

/// Ask for the end of one session and answer what the call reported.
async fn terminate(emr: &Emr, cluster_id: &str, session_id: &str) -> Value {
    emr.ok(
        "TerminateSession",
        json!({ "ClusterId": cluster_id, "SessionId": session_id }),
    )
    .await
}

/// How many of `cluster_id`'s listed sessions carry the id `session_id`, read through the
/// `ListSessions` call the sibling suite owns.
async fn times_listed(emr: &Emr, cluster_id: &str, session_id: &str) -> usize {
    ids_of(&listed_sessions(emr, cluster_id, None).await)
        .iter()
        .filter(|listed| *listed == session_id)
        .count()
}

/// THE RULE: every ask answers the session's own record, and only the FIRST one moves it —
/// the answer is never a live state again, never ahead of what `GetSession` reports, the
/// settled session is left byte for byte as it settled, and the cluster still carries that
/// one session.
#[tokio::test]
async fn asking_for_the_end_again_answers_the_record_and_arms_no_second_end() {
    struct Case {
        label: &'static str,
        /// Whether the session is waited to its ready state before the end is asked for.
        idle_first: bool,
        /// Asks made back to back, starting with the one that arms the end.
        asks_while_ending: usize,
        /// Asks made after the clock has settled the session in `TERMINATED`.
        asks_once_ended: usize,
    }

    let cases = [
        Case {
            label: "asked once while still coming up, then again once it had ended",
            idle_first: false,
            asks_while_ending: 1,
            asks_once_ended: 1,
        },
        Case {
            label: "asked once while idle, then again once it had ended",
            idle_first: true,
            asks_while_ending: 1,
            asks_once_ended: 1,
        },
        Case {
            label: "asked three times back to back while idle, then twice more once it had ended",
            idle_first: true,
            asks_while_ending: 3,
            asks_once_ended: 2,
        },
        Case {
            label: "asked twice while coming up and once more once it had ended",
            idle_first: false,
            asks_while_ending: 2,
            asks_once_ended: 1,
        },
    ];

    let emr = Emr::new();
    let cluster_id = ready_cluster(
        &emr,
        "the session-enabled cluster the rows run on",
        launch_request("repeated session ends", Some(true)),
    )
    .await;

    // Every row's session is started before any end is asked for, so the waits below overlap
    // instead of running one after another.
    let mut sessions: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        sessions.push((case, start_session(&emr, &cluster_id, case.label).await));
    }

    for (case, session_id) in &sessions {
        let label = case.label;
        if case.idle_first {
            await_session_state(&emr, &cluster_id, session_id, label, "IDLE").await;
        }

        for ask in 1..=case.asks_while_ending {
            let answer = terminate(&emr, &cluster_id, session_id).await;
            assert_eq!(
                answer["ClusterId"].as_str(),
                Some(cluster_id.as_str()),
                "case {label:?} ask {ask}: the answer names the session's cluster, got {answer}"
            );
            assert_eq!(
                answer["SessionId"].as_str(),
                Some(session_id.as_str()),
                "case {label:?} ask {ask}: the answer names the session, got {answer}"
            );
            let answered = answer["State"].as_str().unwrap_or_default().to_string();
            let answered_rank = ending_rank(&answered).unwrap_or_else(|| {
                panic!(
                    "case {label:?} ask {ask}: expected a state in {ENDING_STATES:?}, \
                     got {answered:?} in {answer}"
                )
            });
            // The ask that ARMS the end answers the state it just moved the session into;
            // a later one can only report the same state or the one the clock has since
            // made due, so the end is never re-armed from a live state.
            if ask == 1 {
                assert_eq!(
                    answered, TERMINATING,
                    "case {label:?} ask {ask}: expected the state the ask moves the session \
                     into, got {answered:?} in {answer}"
                );
            }
            // The answer is READ OFF the record, so the record is never behind it.
            let reported = get_session(&emr, &cluster_id, session_id).await;
            let reported_state = at(&reported, &["State"]).as_str().unwrap_or_default().to_string();
            let reported_rank = ending_rank(&reported_state).unwrap_or_else(|| {
                panic!(
                    "case {label:?} ask {ask}: expected the session to be ending, \
                     got {reported_state:?} in {reported}"
                )
            });
            assert!(
                reported_rank >= answered_rank,
                "case {label:?} ask {ask}: expected the session to report {answered:?} or \
                 later, got {reported_state:?} in {reported}"
            );
        }

        // The clock settles the session, whatever the number of asks was.
        let settled = await_session_state(&emr, &cluster_id, session_id, label, "TERMINATED").await;
        let created = at(&settled, &["CreatedAt"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: every session reports a CreatedAt, got {settled}")
        });
        let ended = at(&settled, &["EndedAt"]).as_f64().unwrap_or_else(|| {
            panic!("case {label:?}: a session that ended reports an EndedAt, got {settled}")
        });
        assert!(
            ended >= created,
            "case {label:?}: expected an EndedAt at or after CreatedAt {created}, \
             got {ended} in {settled}"
        );

        // Asking again once it has ended answers the state it settled in and changes NOTHING
        // the session reports — no fresh end, no later EndedAt, no touched UpdatedAt.
        for ask in 1..=case.asks_once_ended {
            let answer = terminate(&emr, &cluster_id, session_id).await;
            assert_eq!(
                answer["State"].as_str(),
                Some(TERMINATED),
                "case {label:?} late ask {ask}: expected the state the session settled in, \
                 got {answer}"
            );
            let again = get_session(&emr, &cluster_id, session_id).await;
            assert_eq!(
                again, settled,
                "case {label:?} late ask {ask}: expected the settled session unchanged \
                 ({settled}), got {again}"
            );
        }

        // Every ask named the session the cluster already held, so the cluster holds no
        // second copy of it.
        let listed = times_listed(&emr, &cluster_id, session_id).await;
        assert_eq!(
            listed, 1,
            "case {label:?}: expected the cluster to list the session exactly once, \
             got {listed} time(s)"
        );
    }
}
