// SPDX-License-Identifier: CC0-1.0

//! A session Athena RECLAIMS when it has been idle its whole window, read back over the same
//! router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a session is started with an idle window
//! (`StartSessionRequest$SessionIdleTimeoutInMinutes`, "the idle timeout in minutes for the
//! session") and a session left idle for that whole window shuts itself down — IDLE through
//! TERMINATING to TERMINATED, with no `TerminateSession` call — and not before the window has
//! elapsed. The window runs from the instant the session's resources came up, or from the last
//! work submitted into it, so a session being worked is not reclaimed under its caller. The
//! transition is the CLOCK's: every read derives the state from the instants the record holds,
//! so a session nobody polls is reclaimed exactly when one that is polled is, and `GetSession`,
//! `GetSessionStatus`, `ListSessions` and `ListExecutors` all report it.
//!
//! Each rule is asserted over a TABLE rather than one session: the rows differ along the window
//! the start configured (both published bounds of the modelled `@range`, a middling value, and
//! none at all), along how far into that window the clock has moved, and along whether work was
//! submitted into the session.
//!
//! Nothing here waits on the wall clock: a session that must be at a given point of its idle
//! window is SEEDED with the instants that put it there — which is what lets a row place it at
//! an exact point, where waiting out a one-minute window could not — and every read is made
//! over the wire.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, SessionRecord, EXECUTOR_STATE_REGISTERED, EXECUTOR_STATE_TERMINATED,
    MAX_SESSION_IDLE_TIMEOUT_MINUTES, PYSPARK_ENGINE_VERSION, SESSION_CREATION,
    SESSION_STATE_IDLE, SESSION_STATE_TERMINATED, SESSION_STATE_TERMINATING, SESSION_TERMINATION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router};

/// The Spark enabled workgroup the sessions below run in.
const WORK_GROUP: &str = "spark-idle";

/// Seconds in a minute, the unit the configured window is stated in.
const SECONDS_PER_MINUTE: u64 = 60;

/// Create the Spark enabled workgroup a session is started in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
}

/// Seed one session into `engine`'s store, started `started_ago` ago under the idle window
/// `configured` (in minutes, `None` for a start that named none), with `worked_ago` as how long
/// ago work was last submitted into it, and hand back its `SessionId`.
fn given_session(
    engine: &Arc<engine::InMemoryEngine>,
    configured: Option<i32>,
    started_ago: Duration,
    worked_ago: Option<Duration>,
) -> String {
    let now = SystemTime::now();
    let started_at = now
        .checked_sub(started_ago)
        .expect("an instant inside the clock's range");
    let id = state::new_session_id();
    let mut record = SessionRecord::started(id.clone(), WORK_GROUP.to_string(), None, started_at);
    record.configuration.session_idle_timeout_in_minutes = configured;
    record.last_activity_at = worked_ago.map(|ago| {
        now.checked_sub(ago)
            .expect("an instant inside the clock's range")
    });
    let store = state::service_state(&**engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// The window `configured` minutes is, as a span — and the window a start that named none gets.
fn window(configured: Option<i32>) -> Duration {
    let minutes = configured.unwrap_or(MAX_SESSION_IDLE_TIMEOUT_MINUTES);
    Duration::from_secs(u64::from(minutes.unsigned_abs()) * SECONDS_PER_MINUTE)
}

/// The `State` a status read reports for `session_id`.
async fn state_of(router: &MultiServiceRouter, session_id: &str) -> String {
    let (status, body) = call(
        router,
        "GetSessionStatus",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetSessionStatus({session_id}) must answer 200, got {status} with {body:?}"
    );
    body["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The whole `SessionStatus` `GetSession` reports for `session_id`.
async fn session_status(router: &MultiServiceRouter, session_id: &str) -> Value {
    let (status, body) = call(router, "GetSession", &json!({ "SessionId": session_id })).await;
    assert_eq!(
        status, 200,
        "GetSession({session_id}) must answer 200, got {status} with {body:?}"
    );
    body["Status"].clone()
}

/// Whether the workgroup's own listing — the one that reports its ACTIVE sessions — holds
/// `session_id`.
async fn in_active_listing(router: &MultiServiceRouter, session_id: &str) -> bool {
    let (status, body) = call(
        router,
        "ListSessions",
        &json!({ "WorkGroup": WORK_GROUP, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup's sessions must be listable, got {status} with {body:?}"
    );
    body["Sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|summary| summary["SessionId"].as_str() == Some(session_id))
}

/// RULE: a session reaches TERMINATED on its own exactly one idle window — and then one
/// shutdown span — after its resources came up, and not before.
#[tokio::test]
async fn a_session_left_idle_its_whole_window_is_reclaimed() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;

    // (case, the window the start configured) — both published bounds of the modelled
    // `@range` (1 to 480), a middling value, and a start that configured none.
    let windows: [(&str, Option<i32>); 4] = [
        ("the smallest window the model admits", Some(1)),
        ("a middling window", Some(5)),
        ("the largest window the model admits", Some(MAX_SESSION_IDLE_TIMEOUT_MINUTES)),
        ("no window configured", None),
    ];

    for (case, configured) in windows {
        let configured_window = window(configured);
        // (what the row is about, how long ago the session was started, the state it is in)
        let rows: [(&str, Duration, &str); 4] = [
            (
                "halfway through the window",
                SESSION_CREATION + configured_window / 2,
                SESSION_STATE_IDLE,
            ),
            (
                "the instant the window runs out",
                SESSION_CREATION + configured_window,
                SESSION_STATE_TERMINATING,
            ),
            (
                "while the shutdown it owes is due",
                SESSION_CREATION + configured_window + SESSION_TERMINATION / 2,
                SESSION_STATE_TERMINATING,
            ),
            (
                "once that shutdown is complete",
                SESSION_CREATION + configured_window + SESSION_TERMINATION,
                SESSION_STATE_TERMINATED,
            ),
        ];
        for (point, started_ago, expected) in rows {
            let id = given_session(&engine, configured, started_ago, None);
            let reported = state_of(&router, &id).await;
            assert_eq!(
                reported, expected,
                "case {case:?} {point:?}: expected {expected}, got {reported}"
            );
            // The two reads of one session agree, because both project the same record.
            assert_eq!(
                session_status(&router, &id).await["State"].as_str(),
                Some(expected),
                "case {case:?} {point:?}: GetSession and GetSessionStatus must agree"
            );
            // A reclaimed session is in no ACTIVE listing, and reports when it ended.
            let status = session_status(&router, &id).await;
            if expected == SESSION_STATE_TERMINATED {
                assert!(
                    status["EndDateTime"].as_f64().is_some(),
                    "case {case:?} {point:?}: a reclaimed session reports when it ended, got \
                     {status:?}"
                );
            }
            assert_eq!(
                in_active_listing(&router, &id).await,
                expected == SESSION_STATE_IDLE,
                "case {case:?} {point:?}: the active listing holds exactly the live session"
            );
        }
    }
}

/// RULE: a read never MOVES a session — two reads at the same point report the same state, and
/// the session the clock has not reclaimed is still there afterwards.
#[tokio::test]
async fn reading_a_session_neither_reclaims_it_nor_holds_it_back() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;

    // Rows differ along how much of the window is left, including none at all.
    for (case, started_ago, expected) in [
        (
            "a session with its whole window left",
            SESSION_CREATION,
            SESSION_STATE_IDLE,
        ),
        (
            "a session one span into a five-minute window",
            SESSION_CREATION + SESSION_TERMINATION,
            SESSION_STATE_IDLE,
        ),
        (
            "a session whose window ran out long ago",
            SESSION_CREATION + window(Some(5)) + SESSION_TERMINATION * 10,
            SESSION_STATE_TERMINATED,
        ),
    ] {
        let id = given_session(&engine, Some(5), started_ago, None);
        let first = state_of(&router, &id).await;
        let second = state_of(&router, &id).await;
        assert_eq!(
            (first.as_str(), second.as_str()),
            (expected, expected),
            "case {case:?}: two reads must report {expected}, got {first} then {second}"
        );
    }
}

/// RULE: the window runs from the LAST work submitted into the session, so a session being
/// worked is not reclaimed — and one whose work is a whole window old is.
#[tokio::test]
async fn work_submitted_into_a_session_restarts_its_idle_window() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;

    let configured = Some(5);
    let configured_window = window(configured);
    // (case, how long ago the session was started, how long ago work was last submitted, the
    // state it is in) — every row's session was started long enough ago that its window would
    // have run out had nothing been submitted into it.
    let rows: [(&str, Duration, Option<Duration>, &str); 4] = [
        (
            "work submitted just now",
            SESSION_CREATION + configured_window * 3,
            Some(Duration::ZERO),
            SESSION_STATE_IDLE,
        ),
        (
            "work submitted halfway through a window ago",
            SESSION_CREATION + configured_window * 3,
            Some(configured_window / 2),
            SESSION_STATE_IDLE,
        ),
        (
            "work submitted a whole window ago",
            SESSION_CREATION + configured_window * 3,
            Some(configured_window + SESSION_TERMINATION),
            SESSION_STATE_TERMINATED,
        ),
        (
            "no work submitted at all",
            SESSION_CREATION + configured_window * 3,
            None,
            SESSION_STATE_TERMINATED,
        ),
    ];

    for (case, started_ago, worked_ago, expected) in rows {
        let id = given_session(&engine, configured, started_ago, worked_ago);
        let reported = state_of(&router, &id).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected}, got {reported}"
        );
        // The session's executors follow it: registered while it runs, gone once it is
        // reclaimed.
        let (status, executors) =
            call(&router, "ListExecutors", &json!({ "SessionId": id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the roster must be reported, got {status} with {executors:?}"
        );
        let states: Vec<String> = executors["ExecutorsSummary"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|summary| {
                summary["ExecutorState"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        let expected_executor = if expected == SESSION_STATE_IDLE {
            EXECUTOR_STATE_REGISTERED
        } else {
            EXECUTOR_STATE_TERMINATED
        };
        assert_eq!(
            states,
            vec![expected_executor.to_string()],
            "case {case:?}: expected the coordinator in {expected_executor}, got {executors:?}"
        );
    }
}

/// RULE: the window a session runs under is the one its START asked for, reported back by the
/// session's own configuration — so a client reads the window the reclamation follows.
#[tokio::test]
async fn a_start_reports_the_idle_window_it_asked_for() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router).await;

    // Rows differ along the window the start asks for, including both published bounds.
    for configured in [1, 5, MAX_SESSION_IDLE_TIMEOUT_MINUTES] {
        let (status, started) = call(
            &router,
            "StartSession",
            &json!({
                "WorkGroup": WORK_GROUP,
                "EngineConfiguration": { "CoordinatorDpuSize": 1, "MaxConcurrentDpus": 4 },
                "SessionIdleTimeoutInMinutes": configured
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {configured}: the session must start, got {status} with {started:?}"
        );
        let session_id = started["SessionId"].as_str().unwrap_or_default().to_string();
        let (status, read) = call(
            &router,
            "GetSession",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {configured}: the session must be readable, got {status} with {read:?}"
        );
        assert_eq!(
            read["SessionConfiguration"]["SessionIdleTimeoutInMinutes"].as_i64(),
            Some(i64::from(configured)),
            "case {configured}: the session reports the window its start asked for, got {read:?}"
        );
        assert_eq!(
            read["SessionConfiguration"]["IdleTimeoutSeconds"].as_i64(),
            Some(i64::from(configured) * 60),
            "case {configured}: the same window in seconds, got {read:?}"
        );
    }
}
