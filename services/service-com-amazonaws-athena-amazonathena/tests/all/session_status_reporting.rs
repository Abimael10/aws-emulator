// SPDX-License-Identifier: CC0-1.0

//! Athena's `GetSessionStatus`, driven end to end over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.GetSessionStatus`).
//!
//! THE RULE UNDER TEST: a status read REPORTS the session the request names out of the
//! record its start persisted — it echoes that record's `SessionId`, reports the
//! `SessionState` the clock puts the record in, and reports the instants that state follows
//! from (`StartDateTime` the start, `LastModifiedDateTime` the transition being observed,
//! `IdleSinceDateTime` only while the session is idle, `EndDateTime` only once the session
//! has ended). It is a REPORTING operation, so reading a session changes nothing a later
//! caller sees: repeated reads answer identically, and the status is the same projection
//! `GetSession` and `ListSessions` report for that session at that instant. An id this
//! account holds no session under is refused with HTTP 400 `InvalidRequestException`
//! carrying Athena's fixed sentence for that class, and a `SessionId` outside the modelled
//! `@length` is refused as the broken input constraint it is.
//!
//! Each test asserts that rule over a TABLE rather than one request: the rows differ along
//! what the rule generalises over — the parameters the start carried (description, idle
//! timeout at both ends of the modelled range, execution role, request token), which Spark
//! workgroup the session belongs to, how far the clock has moved, and the shape and length
//! of the id a refused read names.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! the service for the state it is waiting on, under a deadline far above the span that
//! transition is due after, so a loaded host cannot turn a slow transition into a failure.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    ERROR_CODE_INVALID_INPUT, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND, SESSION_STATE_CREATING,
    SESSION_STATE_IDLE, SESSION_STATE_TERMINATED, SESSION_STATE_TERMINATING,
};

use crate::harness::{assert_refusal, call, request_token, router};

/// The Spark enabled workgroup the sessions below are started in.
const SPARK_WORK_GROUP: &str = "status-sessions";

/// A SECOND Spark enabled workgroup: a status read addresses a session by id, so which
/// workgroup holds it must make no difference to what the read reports.
const OTHER_WORK_GROUP: &str = "status-sessions-two";

/// The execution role a Spark workgroup accesses user resources through, inside the
/// modelled `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// Where the Spark workgroups' members write their results.
const OUTPUT_LOCATION: &str = "s3://athena-spark-results/status/";

/// How often a poll for a transition the clock owes looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many times such a poll looks before it gives up: together with [`POLL_INTERVAL`] a
/// deadline far above the spans those transitions are due after, so the CONDITION is what is
/// waited on rather than a span of the host's own speed.
const POLL_LOOKS: u32 = 2_400;

/// Create a Spark enabled workgroup: a PySpark engine version, an execution role and an
/// output location, which is the workgroup Athena documents sessions run in.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": EXECUTION_ROLE,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Start one session in `work_group` with the members `extra` carries beside the required
/// ones, and hand back the `SessionId` it minted.
async fn given_session(router: &MultiServiceRouter, work_group: &str, extra: Value) -> String {
    let mut body = json!({
        "WorkGroup": work_group,
        "EngineConfiguration": { "MaxConcurrentDpus": 4 }
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in extra {
            body.insert(member.clone(), value.clone());
        }
    }
    let (status, started) = call(router, "StartSession", &body).await;
    assert_eq!(
        status, 200,
        "StartSession in {work_group} must be accepted, got {status} with {started:?}"
    );
    started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string()
}

/// `GetSessionStatus`'s whole answer for `session_id`.
async fn read_status(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(router, "GetSessionStatus", &json!({ "SessionId": session_id })).await
}

/// The `Status` a status read reports for `session_id`, asserting the read was served.
async fn status_of(case: &str, router: &MultiServiceRouter, session_id: &str) -> Value {
    let (code, body) = read_status(router, session_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetSessionStatus({session_id}) must answer 200, got {code} with {body:?}"
    );
    assert_eq!(
        body["SessionId"].as_str(),
        Some(session_id),
        "case {case:?}: a status read echoes the id it was asked about, got {body:?}"
    );
    body["Status"].clone()
}

/// Poll until `session_id` reports `wanted`, and hand back the status that reported it.
///
/// The CONDITION is what is waited on: every look asks the service what state the session is
/// in, and the deadline is far above the span the transition is due after.
async fn settled_in(
    case: &str,
    router: &MultiServiceRouter,
    session_id: &str,
    wanted: &str,
) -> Value {
    for _ in 0..POLL_LOOKS {
        let status = status_of(case, router, session_id).await;
        if status["State"].as_str() == Some(wanted) {
            return status;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the session {session_id} never reached {wanted}");
}

/// One instant a status reports, as the epoch-seconds value the awsJson protocols put a
/// timestamp on the wire as.
fn instant(case: &str, status: &Value, member: &str) -> f64 {
    status[member].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: the status must report {member}, got {status:?}")
    })
}

/// Assert `member` is reported by NO value: a status states an instant only where the state
/// it reports has one.
fn assert_absent(case: &str, status: &Value, member: &str) {
    assert_eq!(
        status[member],
        Value::Null,
        "case {case:?}: {member} must not be reported here, got {status:?}"
    );
}

/// One row of the start table: the same rule read back off sessions whose starts differ.
struct SessionCase {
    case: &'static str,
    work_group: &'static str,
    /// The members the start carries beside `WorkGroup` and `EngineConfiguration`.
    extra: Value,
}

/// The start table: a bare start, the idle timeout at both ends of the range
/// `StartSessionRequest$SessionIdleTimeoutInMinutes` publishes (1 to 480), an execution role
/// of the session's own, a request token, and a second workgroup.
fn session_cases() -> Vec<SessionCase> {
    vec![
        SessionCase {
            case: "a bare start",
            work_group: SPARK_WORK_GROUP,
            extra: json!({}),
        },
        SessionCase {
            case: "a described session at the smallest idle timeout the model admits",
            work_group: SPARK_WORK_GROUP,
            extra: json!({
                "Description": "a short-lived session",
                "SessionIdleTimeoutInMinutes": 1
            }),
        },
        SessionCase {
            case: "the largest idle timeout the model admits, under a role of its own",
            work_group: SPARK_WORK_GROUP,
            extra: json!({
                "Description": "a long-lived session",
                "SessionIdleTimeoutInMinutes": 480,
                "ExecutionRole": EXECUTION_ROLE
            }),
        },
        SessionCase {
            case: "a start in a second workgroup, carrying a request token",
            work_group: OTHER_WORK_GROUP,
            extra: json!({ "ClientRequestToken": request_token("status-token") }),
        },
    ]
}

/// RULE: a status read reports the state the CLOCK puts the session in — `CREATING` until
/// its resources are up and `IDLE` from then on — with the instants that state follows from:
/// the start, the transition being observed, and the instant it became idle once it is.
#[tokio::test]
async fn a_status_read_reports_the_state_the_clock_puts_the_session_in() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;

    for row in session_cases() {
        let case = row.case;
        let session_id = given_session(&router, row.work_group, row.extra).await;

        // A session's resources are not up the instant it is started, so the documented
        // CREATING state is what a caller reading straight back observes.
        let creating = status_of(case, &router, &session_id).await;
        assert_eq!(
            creating["State"].as_str(),
            Some(SESSION_STATE_CREATING),
            "case {case:?}: a session still acquiring its resources reports CREATING, got \
             {creating:?}"
        );
        let started_at = instant(case, &creating, "StartDateTime");
        assert_eq!(
            instant(case, &creating, "LastModifiedDateTime"),
            started_at,
            "case {case:?}: the session last changed when it was started, got {creating:?}"
        );
        // Not idle, not ended, and in a state the caller's own start asked for.
        assert_absent(case, &creating, "IdleSinceDateTime");
        assert_absent(case, &creating, "EndDateTime");
        assert_absent(case, &creating, "StateChangeReason");

        let idle = settled_in(case, &router, &session_id, SESSION_STATE_IDLE).await;
        let idle_since = instant(case, &idle, "IdleSinceDateTime");
        assert!(
            idle_since >= started_at,
            "case {case:?}: a session becomes idle no earlier than it was started, got \
             {idle_since} against {started_at} in {idle:?}"
        );
        assert_eq!(
            instant(case, &idle, "LastModifiedDateTime"),
            idle_since,
            "case {case:?}: the session last changed when it became idle, got {idle:?}"
        );
        assert_eq!(
            instant(case, &idle, "StartDateTime"),
            started_at,
            "case {case:?}: the instant a session started never moves, got {idle:?}"
        );
        assert_absent(case, &idle, "EndDateTime");
    }
}

/// RULE: a status read REPORTS — it changes nothing a later caller sees, and the status it
/// reports is the one projection every read of that session reports at that instant.
#[tokio::test]
async fn reading_the_status_changes_nothing_a_later_caller_sees() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;
    let case = "a session read many times over";
    let settled = settled_in(case, &router, &session_id, SESSION_STATE_IDLE).await;

    // Reading a session neither moves it on nor holds it back: every later read answers with
    // the same state and the same instants as the first.
    for look in 1..=5 {
        let again = status_of(case, &router, &session_id).await;
        assert_eq!(
            again, settled,
            "case {case:?}: read {look} must report the status read {:?} reported, got {again:?}",
            settled
        );
    }

    let (code, read) = call(&router, "GetSession", &json!({ "SessionId": session_id })).await;
    assert_eq!(
        (code, &read["Status"]),
        (200, &settled),
        "case {case:?}: GetSession reports the same status, got {code} with {read:?}"
    );

    let (code, listed) = call(
        &router,
        "ListSessions",
        &json!({ "WorkGroup": SPARK_WORK_GROUP }),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListSessions must answer 200, got {code} with {listed:?}"
    );
    let empty = Vec::new();
    let summary = listed["Sessions"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .find(|session| session["SessionId"].as_str() == Some(session_id.as_str()))
        .unwrap_or_else(|| panic!("case {case:?}: the session must be listed, got {listed:?}"));
    assert_eq!(
        summary["Status"], settled,
        "case {case:?}: a listing reports the same status a status read does, got {summary:?}"
    );
}

/// RULE: a status read follows the shutdown a caller asked for — `TERMINATING` while that
/// teardown is due, `TERMINATED` once it is done, and the end instant reported only once the
/// session has ended.
#[tokio::test]
async fn a_status_read_follows_a_terminate_through_to_terminated() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;
    let case = "a session asked to shut down";
    let idle = settled_in(case, &router, &session_id, SESSION_STATE_IDLE).await;
    let started_at = instant(case, &idle, "StartDateTime");
    let idle_since = instant(case, &idle, "IdleSinceDateTime");

    let (code, stopping) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: TerminateSession must be accepted, got {code} with {stopping:?}"
    );

    let terminating = status_of(case, &router, &session_id).await;
    assert_eq!(
        terminating["State"].as_str(),
        Some(SESSION_STATE_TERMINATING),
        "case {case:?}: a session whose teardown is still due reports TERMINATING, got \
         {terminating:?}"
    );
    // A session shutting down is no longer idle, and it has not ended yet.
    assert_absent(case, &terminating, "IdleSinceDateTime");
    assert_absent(case, &terminating, "EndDateTime");
    let asked_at = instant(case, &terminating, "LastModifiedDateTime");
    assert!(
        asked_at >= idle_since,
        "case {case:?}: the shutdown was asked for no earlier than the session became idle, \
         got {asked_at} against {idle_since} in {terminating:?}"
    );

    let terminated = settled_in(case, &router, &session_id, SESSION_STATE_TERMINATED).await;
    let ended_at = instant(case, &terminated, "EndDateTime");
    assert!(
        ended_at >= asked_at,
        "case {case:?}: a session ends no earlier than its shutdown was asked for, got \
         {ended_at} against {asked_at} in {terminated:?}"
    );
    assert_eq!(
        instant(case, &terminated, "LastModifiedDateTime"),
        ended_at,
        "case {case:?}: the session last changed when it ended, got {terminated:?}"
    );
    assert_eq!(
        instant(case, &terminated, "StartDateTime"),
        started_at,
        "case {case:?}: the instant a session started never moves, got {terminated:?}"
    );
    assert_absent(case, &terminated, "IdleSinceDateTime");

    // An ended session stays ended: the read reports the record, so it reports the same
    // instants however often it is asked.
    let again = status_of(case, &router, &session_id).await;
    assert_eq!(
        again, terminated,
        "case {case:?}: a terminated session reports one status, got {again:?}"
    );
}

/// RULE: a status read naming an id this account holds no session under is refused with the
/// one sentence this service states for that class, whatever the id looks like — and an id
/// outside the modelled `SessionId` length is refused as the broken constraint it is.
#[tokio::test]
async fn a_status_read_refuses_an_id_no_session_is_held_under() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    // A real session exists throughout, so each refusal below is about the id the read named
    // rather than about an account holding no sessions at all.
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;
    let case = "a session that does exist";
    // Read once the session has settled in the state it stays in, so what the comparison at
    // the end of this test observes is the effect of the refused reads rather than a
    // transition the clock owed in between.
    let served = settled_in(case, &router, &session_id, SESSION_STATE_IDLE).await;

    // `GetSessionStatusRequest$SessionId` targets `SessionId`, 1 to 256 characters: the rows
    // sit at both published bounds, past the upper one, below the lower one, and at the
    // shapes an id can take in between.
    let shortest = "x".to_string();
    let longest = "s".repeat(256);
    let overlong = "s".repeat(257);
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "an id at the shortest the model admits",
            json!({ "SessionId": shortest }),
            SESSION_NOT_FOUND.to_string(),
        ),
        (
            "an id at the longest the model admits",
            json!({ "SessionId": longest }),
            SESSION_NOT_FOUND.to_string(),
        ),
        (
            "an id shaped like one this service mints",
            json!({ "SessionId": "6f1c9f2a-0000-4000-8000-0f1c9f2a0000" }),
            SESSION_NOT_FOUND.to_string(),
        ),
        (
            "the id of another session with one character changed",
            json!({ "SessionId": format!("{session_id}z") }),
            SESSION_NOT_FOUND.to_string(),
        ),
        (
            "an id one character past the longest the model admits",
            json!({ "SessionId": overlong }),
            "1 validation error detected: Value at 'sessionId' failed to satisfy constraint: \
             Member must have length less than or equal to 256"
                .to_string(),
        ),
        (
            "an id one character below the shortest the model admits",
            json!({ "SessionId": "" }),
            "1 validation error detected: Value at 'sessionId' failed to satisfy constraint: \
             Member must have length greater than or equal to 1"
                .to_string(),
        ),
        (
            "no id at all",
            json!({}),
            "1 validation error detected: Value null at 'sessionId' failed to satisfy \
             constraint: Member must not be null"
                .to_string(),
        ),
    ];

    for (case, request, expected) in rows {
        let (code, body) = call(&router, "GetSessionStatus", &request).await;
        assert_refusal(case, code, &body, ERROR_CODE_INVALID_INPUT, &expected);
    }

    // The refusals changed nothing: the session that does exist still reports what it did.
    let after = status_of(case, &router, &session_id).await;
    assert_eq!(
        after, served,
        "case {case:?}: a refused read leaves the session it did not name alone, got {after:?}"
    );
}
