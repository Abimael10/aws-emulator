// SPDX-License-Identifier: CC0-1.0

//! Athena's INTERACTIVE SESSION slice, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`): `StartSession` starts a
//! session in a Spark enabled workgroup and `GetSession`, `GetSessionStatus`,
//! `ListSessions`, `ListExecutors` and `TerminateSession` all address that same session.
//!
//! THE RULE UNDER TEST: a session is a resource of the Spark enabled workgroup it was
//! started in — `StartSession` mints its `SessionId` and persists every parameter the start
//! resolved, every later read answers out of that record, the state each read reports
//! follows the clock (`CREATING` until the session's resources are up, `IDLE` from then
//! until a caller asks it to shut down, `TERMINATING` while that shutdown is due), and
//! EVERY session-addressed operation refuses an id this account holds no session under with
//! HTTP 400 `InvalidRequestException` carrying Athena's fixed sentence for that class.
//!
//! Each test asserts that rule over a TABLE rather than one request: the rows differ along
//! what the rule generalises over — which operation is called, the shape and length of the
//! session id, the description and notebook version a start carries, the engine
//! configuration's sizes, the idle timeout, the tags, which workgroup the start names, the
//! page size of a listing and how far the clock has moved.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! the service for the state it is waiting on, under a deadline far above the span that
//! transition is due after, so a loaded host cannot turn a slow transition into a failure.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    self, page_token_malformed, EXECUTOR_STATE_CREATING, EXECUTOR_STATE_REGISTERED,
    EXECUTOR_TYPE_COORDINATOR, IDEMPOTENT_PARAMETERS_DO_NOT_MATCH, PYSPARK_ENGINE_VERSION,
    SESSION_NOT_FOUND, SESSION_STATE_CREATING, SESSION_STATE_IDLE, SESSION_STATE_TERMINATED,
    SESSION_STATE_TERMINATING, WORK_GROUP_NOT_FOUND,
};

use crate::harness::{call, create_work_group, error_code, message, request_token, router};

/// The wire error code every refusal of this slice must carry: the client error each of its
/// operations models (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The Spark enabled workgroup the sessions below are started in.
const SPARK_WORK_GROUP: &str = "spark-sessions";

/// The execution role a Spark workgroup accesses user resources through, inside the
/// modelled `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// Where the Spark workgroup's members write their results, which is also the working
/// directory a session of it reports.
const OUTPUT_LOCATION: &str = "s3://athena-spark-results/sessions/";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the spans those transitions are due after
/// (`state::SESSION_CREATION`, `state::SESSION_TERMINATION`), so the CONDITION is what is
/// waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// Create the Spark enabled workgroup every start below names: a PySpark engine version, an
/// execution role and an output location, which is the workgroup Athena documents sessions
/// run in.
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
            },
            "Tags": [{ "Key": "team", "Value": "analytics" }]
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// The engine configuration a start that is not about the configuration itself carries:
/// inside every modelled range, and naming all three sizes so no row says anything about a
/// default by accident.
fn some_engine_configuration() -> Value {
    json!({
        "CoordinatorDpuSize": 1,
        "MaxConcurrentDpus": 4,
        "DefaultExecutorDpuSize": 1
    })
}

/// Start one session in `work_group` and hand back the whole answer.
async fn start(router: &MultiServiceRouter, work_group: &str, extra: Value) -> (u16, Value) {
    let mut body = json!({
        "WorkGroup": work_group,
        "EngineConfiguration": some_engine_configuration()
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in extra {
            body.insert(member.clone(), value.clone());
        }
    }
    call(router, "StartSession", &body).await
}

/// Start one session and assert it was accepted, handing back its `SessionId`.
async fn given_session(router: &MultiServiceRouter, work_group: &str, extra: Value) -> String {
    let (status, started) = start(router, work_group, extra).await;
    assert_eq!(
        status, 200,
        "StartSession in {work_group} must be accepted, got {status} with {started:?}"
    );
    assert_eq!(
        started["State"].as_str(),
        Some(SESSION_STATE_CREATING),
        "a freshly started session is still acquiring its resources, got {started:?}"
    );
    started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string()
}

/// `GetSession`'s whole answer for `session_id`.
async fn read_session(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(router, "GetSession", &json!({ "SessionId": session_id })).await
}

/// The `Status.State` the session `session_id` reports, or `<status>` for a refusal.
async fn state_of(router: &MultiServiceRouter, session_id: &str) -> String {
    let (status, body) = call(
        router,
        "GetSessionStatus",
        &json!({ "SessionId": session_id }),
    )
    .await;
    if status != 200 {
        return format!("<{status}>");
    }
    body["Status"]["State"]
        .as_str()
        .unwrap_or("<no State>")
        .to_string()
}

/// Poll until the session `session_id` reports `wanted`, and hand back its status.
///
/// The CONDITION is what is waited on: every look asks the service what state the session is
/// in, and the deadline is far above the span the transition is due after.
async fn settled_in(router: &MultiServiceRouter, session_id: &str, wanted: &str) -> Value {
    for _ in 0..POLL_LOOKS {
        let (status, body) = call(
            router,
            "GetSessionStatus",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetSessionStatus({session_id}) must answer 200 while polling, got {status} with \
             {body:?}"
        );
        if body["Status"]["State"].as_str() == Some(wanted) {
            return body["Status"].clone();
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the session {session_id} never reached {wanted}");
}

/// The sessions `work_group` reports, as `(SessionId, Status.State)` pairs in the order the
/// listing gave them.
async fn listed(router: &MultiServiceRouter, body: &Value) -> (Value, Vec<(String, String)>) {
    let (status, listed) = call(router, "ListSessions", body).await;
    assert_eq!(
        status, 200,
        "ListSessions({body}) must answer 200, got {status} with {listed:?}"
    );
    let sessions = listed["Sessions"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|session| {
            (
                session["SessionId"].as_str().unwrap_or_default().to_string(),
                session["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    (listed, sessions)
}

/// One row of the start/read-back table.
struct StartCase {
    case: &'static str,
    /// The members the start carries beside `WorkGroup` and `EngineConfiguration`.
    extra: Value,
    /// The engine configuration the start carries, and what `GetSession` must then report
    /// for its three sizes — the configuration a session runs under is the one its start
    /// named, so a size the request left out is reported by neither.
    engine_configuration: Value,
    expected_dpus: (Option<i64>, Option<i64>, Option<i64>),
    /// What `GetSession` must report for the session's idle timeout, in minutes, or `None`
    /// where the start named none.
    expected_idle_minutes: Option<i64>,
    expected_description: Option<&'static str>,
}

/// The start table: a bare start and one naming every optional member the workgroup does
/// not supply, the engine configuration absent, at the modelled floor and well above it,
/// and the idle timeout at both ends of the modelled range and absent.
fn start_cases() -> Vec<StartCase> {
    vec![
        StartCase {
            case: "no optional member at all",
            extra: json!({}),
            engine_configuration: json!({}),
            // A start that names no size runs under a configuration that carries none.
            expected_dpus: (None, None, None),
            expected_idle_minutes: None,
            expected_description: None,
        },
        StartCase {
            case: "a description, the smallest idle timeout the model admits",
            extra: json!({ "Description": "a short-lived session", "SessionIdleTimeoutInMinutes": 1 }),
            engine_configuration: json!({
                "CoordinatorDpuSize": 1,
                "MaxConcurrentDpus": 2,
                "DefaultExecutorDpuSize": 1
            }),
            expected_dpus: (Some(1), Some(2), Some(1)),
            expected_idle_minutes: Some(1),
            expected_description: Some("a short-lived session"),
        },
        StartCase {
            case: "an execution role, the largest idle timeout the model admits, many DPUs",
            extra: json!({
                "Description": "a long-lived session",
                "SessionIdleTimeoutInMinutes": 480,
                "ExecutionRole": EXECUTION_ROLE
            }),
            engine_configuration: json!({
                "CoordinatorDpuSize": 1,
                "MaxConcurrentDpus": 5000,
                "DefaultExecutorDpuSize": 1
            }),
            expected_dpus: (Some(1), Some(5000), Some(1)),
            expected_idle_minutes: Some(480),
            expected_description: Some("a long-lived session"),
        },
    ]
}

/// RULE: a start persists every parameter the session runs under, and `GetSession` answers
/// out of that record — the workgroup it belongs to, the engine version that workgroup runs,
/// the engine configuration with the documented defaults filled in, the configuration
/// (execution role, working directory and idle timeout) and the status it is in.
#[tokio::test]
async fn a_start_persists_the_session_every_read_reports() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;

    for case in start_cases() {
        let mut body = json!({
            "WorkGroup": SPARK_WORK_GROUP,
            "EngineConfiguration": case.engine_configuration
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), case.extra.as_object()) {
            for (member, value) in extra {
                body.insert(member.clone(), value.clone());
            }
        }
        let (status, started) = call(&router, "StartSession", &body).await;
        assert_eq!(
            status, 200,
            "case {:?}: StartSession must be accepted, got {status} with {started:?}",
            case.case
        );
        let session_id = started["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no SessionId in {started:?}", case.case))
            .to_string();

        let (status, read) = read_session(&router, &session_id).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetSession({session_id}) must answer 200, got {status} with {read:?}",
            case.case
        );
        let found = [
            ("SessionId", read["SessionId"].as_str(), Some(&session_id[..])),
            (
                "WorkGroup",
                read["WorkGroup"].as_str(),
                Some(SPARK_WORK_GROUP),
            ),
            (
                "EngineVersion",
                read["EngineVersion"].as_str(),
                Some(PYSPARK_ENGINE_VERSION),
            ),
            (
                "Description",
                read["Description"].as_str(),
                case.expected_description,
            ),
            (
                "NotebookVersion",
                read["NotebookVersion"].as_str(),
                // None of these starts binds a notebook, and a notebook version without one
                // is refused, so no session here reports a version.
                None,
            ),
            (
                "SessionConfiguration.ExecutionRole",
                read["SessionConfiguration"]["ExecutionRole"].as_str(),
                Some(EXECUTION_ROLE),
            ),
            (
                "SessionConfiguration.WorkingDirectory",
                read["SessionConfiguration"]["WorkingDirectory"].as_str(),
                Some(OUTPUT_LOCATION),
            ),
        ];
        for (member, reported, expected) in found {
            assert_eq!(
                reported, expected,
                "case {:?}: expected {member} to be {expected:?}, got {reported:?}",
                case.case
            );
        }

        let configuration = &read["EngineConfiguration"];
        let sizes = (
            configuration["CoordinatorDpuSize"].as_i64(),
            configuration["MaxConcurrentDpus"].as_i64(),
            configuration["DefaultExecutorDpuSize"].as_i64(),
        );
        assert_eq!(
            sizes, case.expected_dpus,
            "case {:?}: expected the engine configuration's sizes to be {:?}, got {sizes:?}",
            case.case, case.expected_dpus
        );
        let timeout = (
            read["SessionConfiguration"]["SessionIdleTimeoutInMinutes"].as_i64(),
            read["SessionConfiguration"]["IdleTimeoutSeconds"].as_i64(),
        );
        assert_eq!(
            timeout,
            (
                case.expected_idle_minutes,
                case.expected_idle_minutes.map(|minutes| minutes * 60)
            ),
            "case {:?}: expected the idle timeout to be {:?} minutes in both units, got \
             {timeout:?}",
            case.case,
            case.expected_idle_minutes
        );
        // Nothing has run in the session, so there is no data processing unit time to report
        // for it: a zero would be a duration for work nothing did.
        assert!(
            read["Statistics"].is_null(),
            "case {:?}: expected no statistics for a session nothing has run in, got {read:?}",
            case.case
        );
        // The start answered with the state the session was in AT that instant — the
        // transient one, "the session is ready when it reaches an IDLE state" — and a read
        // afterwards reports whatever the clock has made of it since, which on a loaded host
        // may already be the ready state. Either way the session is ACTIVE and has not
        // ended, and the exact instant each state change lands at is pinned off a synthetic
        // clock in `state::sessions`' own tests rather than off this host's speed.
        assert_eq!(
            started["State"].as_str(),
            Some(SESSION_STATE_CREATING),
            "case {:?}: expected the start to answer with the transient state, got {started:?}",
            case.case
        );
        let state = read["Status"]["State"].as_str().unwrap_or_default();
        assert!(
            [SESSION_STATE_CREATING, SESSION_STATE_IDLE].contains(&state),
            "case {:?}: expected the session to be acquiring its resources or ready, got \
             {read:?}",
            case.case
        );
        assert!(
            read["Status"]["StartDateTime"].as_f64().is_some(),
            "case {:?}: expected the status to carry the instant the session started, got \
             {read:?}",
            case.case
        );
        assert!(
            read["Status"]["EndDateTime"].is_null(),
            "case {:?}: a session nothing has terminated reports no end instant, got {read:?}",
            case.case
        );
        assert_eq!(
            read["Status"]["IdleSinceDateTime"].is_null(),
            state == SESSION_STATE_CREATING,
            "case {:?}: the idle instant is reported exactly while the session is idle, got \
             {read:?}",
            case.case
        );
    }
}

/// RULE: the state a session reports follows the CLOCK — it is `CREATING` until its
/// resources are up and `IDLE` from then on, it is reported identically by `GetSession` and
/// `GetSessionStatus`, and its executors follow it.
#[tokio::test]
async fn a_session_reaches_idle_and_its_executors_register() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;

    // The coordinator the start provisioned, whose state follows its session's own: while
    // the session is acquiring its resources so is the coordinator, and once the session is
    // ready the coordinator is registered. Which of the two a read lands on depends on the
    // clock, so the executor's state is asserted AGAINST the session's as read in the same
    // moment, and the mapping between the two is pinned instant by instant off a synthetic
    // clock in `state::sessions`' own tests.
    let (status, executors) = call(
        &router,
        "ListExecutors",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListExecutors({session_id}) must answer 200, got {status} with {executors:?}"
    );
    assert_eq!(
        executors["SessionId"].as_str(),
        Some(&session_id[..]),
        "ListExecutors reports the session its executors belong to, got {executors:?}"
    );
    let summaries = executors["ExecutorsSummary"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        1,
        "case \"a session that has run nothing\": expected the coordinator alone, got \
         {summaries:?}"
    );
    let coordinator = &summaries[0];
    let reported = (
        coordinator["ExecutorType"].as_str(),
        coordinator["ExecutorSize"].as_i64(),
    );
    assert_eq!(
        reported,
        (Some(EXECUTOR_TYPE_COORDINATOR), Some(1)),
        "case \"the executor a start provisions\": expected a coordinator of one data \
         processing unit, got {reported:?}"
    );
    let executor_state = coordinator["ExecutorState"].as_str().unwrap_or_default();
    assert!(
        [EXECUTOR_STATE_CREATING, EXECUTOR_STATE_REGISTERED].contains(&executor_state),
        "case \"a live session's coordinator\": expected it to be acquiring resources or \
         registered, got {coordinator:?}"
    );
    assert!(
        coordinator["ExecutorId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            && coordinator["StartDateTime"].as_i64().is_some()
            && coordinator["TerminationDateTime"].is_null(),
        "case \"a live coordinator\": expected an id and a start instant and no termination \
         instant, got {coordinator:?}"
    );

    // The session is ready when it reaches IDLE, which the clock owes rather than a read.
    let settled = settled_in(&router, &session_id, SESSION_STATE_IDLE).await;
    assert!(
        settled["IdleSinceDateTime"].as_f64().is_some(),
        "an idle session reports the instant it became idle, got {settled:?}"
    );
    let (status, read) = read_session(&router, &session_id).await;
    assert_eq!(
        (status, read["Status"]["State"].as_str()),
        (200, Some(SESSION_STATE_IDLE)),
        "GetSession and GetSessionStatus report one state for one session, got {status} with \
         {read:?}"
    );
    // A ready session stays ready, so the coordinator of one is registered from here on.
    let (_, executors) = call(
        &router,
        "ListExecutors",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        executors["ExecutorsSummary"][0]["ExecutorState"].as_str(),
        Some(EXECUTOR_STATE_REGISTERED),
        "the coordinator of a running session is registered, got {executors:?}"
    );

    // `ExecutorStateFilter` narrows the listing to the state it names.
    for (case, filter, expected) in [
        ("the state the coordinator is in", EXECUTOR_STATE_REGISTERED, 1),
        ("a state it is not in", EXECUTOR_STATE_CREATING, 0),
    ] {
        let (status, filtered) = call(
            &router,
            "ListExecutors",
            &json!({ "SessionId": session_id, "ExecutorStateFilter": filter }),
        )
        .await;
        let found = filtered["ExecutorsSummary"]
            .as_array()
            .map_or(0, Vec::len);
        assert_eq!(
            (status, found),
            (200, expected),
            "case {case:?}: expected {expected} executor(s) for the filter {filter}, got \
             {status} with {filtered:?}"
        );
    }
}

/// The session-addressed operations of this slice, and the body each takes for one id.
fn session_addressed_operations(session_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("GetSession", json!({ "SessionId": session_id })),
        ("GetSessionStatus", json!({ "SessionId": session_id })),
        ("ListExecutors", json!({ "SessionId": session_id })),
        ("TerminateSession", json!({ "SessionId": session_id })),
    ]
}

/// RULE: every operation that addresses a session by `SessionId` refuses an id this account
/// holds no session under with HTTP 400 `InvalidRequestException` and Athena's fixed
/// sentence for that class, whatever the id looks like.
#[tokio::test]
async fn every_session_addressed_operation_refuses_an_unknown_id() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    // A real session exists throughout, so a refusal below is about the id the request
    // carried rather than about an account holding no sessions at all.
    let live = given_session(&router, SPARK_WORK_GROUP, json!({})).await;

    // The ids span the modelled `SessionId` (length 1 to 256, no pattern): the uuid form
    // Athena mints, the shortest id the model admits, one of another shape entirely, and the
    // longest the model admits.
    let unknown_ids = [
        ("a uuid no start minted", ::uuid::Uuid::new_v4().to_string()),
        ("the shortest id the model admits", "x".to_string()),
        (
            "an id of another shape",
            "session/not-one-of-ours_42".to_string(),
        ),
        ("the longest id the model admits", "s".repeat(256)),
    ];
    for (case, unknown) in unknown_ids {
        assert_ne!(
            unknown, live,
            "case {case:?}: the unknown id must not be the live session's"
        );
        for (operation, body) in session_addressed_operations(&unknown) {
            let (status, refused) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&refused), message(&refused)),
                (
                    400,
                    INVALID_REQUEST.to_string(),
                    SESSION_NOT_FOUND.to_string()
                ),
                "case {case:?}: expected {operation} to refuse with {INVALID_REQUEST} \
                 {SESSION_NOT_FOUND:?}, got {status} with {refused:?}"
            );
        }
    }

    // The same operations answer for the id that WAS minted, so the refusal is about the id
    // and not about the operation.
    for (operation, body) in session_addressed_operations(&live) {
        let (status, answered) = call(&router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case \"the live session\": expected {operation} to answer, got {status} with \
             {answered:?}"
        );
    }
}

/// RULE: a terminate asks the session to shut down and reports `TERMINATING` over the span
/// that teardown is due after; a terminate of a session that is already inactive succeeds
/// and has no effect; and a terminated session is still read by `GetSession`, no longer
/// listed by `ListSessions`, and has no executors to report.
#[tokio::test]
async fn a_terminate_is_idempotent_and_leaves_the_session_readable() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;

    let (status, terminated) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        (status, terminated["State"].as_str()),
        (200, Some(SESSION_STATE_TERMINATING)),
        "a live session is asked to shut down, got {status} with {terminated:?}"
    );
    // A second terminate neither refuses nor restarts the teardown: it reports the state the
    // session is in, which is still shutting down or already shut down depending on how much
    // of the teardown's span the clock has spent. That the repeat does not MOVE the end
    // instant is pinned instant by instant off a synthetic clock in `state::sessions`' own
    // tests.
    let (status, again) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await;
    let repeated = again["State"].as_str().unwrap_or_default();
    assert!(
        status == 200
            && [SESSION_STATE_TERMINATING, SESSION_STATE_TERMINATED].contains(&repeated),
        "a repeated terminate of a session already shutting down has no effect, got {status} \
         with {again:?}"
    );

    let settled = settled_in(&router, &session_id, SESSION_STATE_TERMINATED).await;
    assert!(
        settled["EndDateTime"].as_f64().is_some(),
        "a terminated session reports the instant it ended, got {settled:?}"
    );
    // And a terminate of the now-inactive session is a success answering that state.
    for case in ["the first terminate after it ended", "and the next one"] {
        let (status, idempotent) = call(
            &router,
            "TerminateSession",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            (status, idempotent["State"].as_str()),
            (200, Some(SESSION_STATE_TERMINATED)),
            "case {case:?}: expected a successful no-op, got {status} with {idempotent:?}"
        );
    }

    let (status, read) = read_session(&router, &session_id).await;
    assert_eq!(
        (status, read["Status"]["State"].as_str()),
        (200, Some(SESSION_STATE_TERMINATED)),
        "a terminated session is still read, got {status} with {read:?}"
    );
    let (_, sessions) = listed(&router, &json!({ "WorkGroup": SPARK_WORK_GROUP })).await;
    assert!(
        !sessions.iter().any(|(id, _)| id == &session_id),
        "a terminated session is not in the active listing, got {sessions:?}"
    );
    let (status, executors) = call(
        &router,
        "ListExecutors",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        (status, error_code(&executors), message(&executors)),
        (
            400,
            INVALID_REQUEST.to_string(),
            SESSION_NOT_FOUND.to_string()
        ),
        "the executors of a session whose resources are no longer running are not reported, \
         got {status} with {executors:?}"
    );
}

/// RULE: a workgroup holds sessions only where it runs Spark, and a start naming a workgroup
/// this account does not hold is refused as that miss — each with the sentence this service
/// states for the class.
#[tokio::test]
async fn a_start_refuses_a_workgroup_that_holds_no_sessions() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    create_work_group(&router, "sql-workgroup").await;

    let cases = [
        (
            "a workgroup this account does not hold",
            "no-such-workgroup".to_string(),
            WORK_GROUP_NOT_FOUND.to_string(),
        ),
        (
            "a workgroup created without any Spark member",
            "sql-workgroup".to_string(),
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
        (
            "the account's default workgroup, which runs Athena SQL",
            state::DEFAULT_WORK_GROUP.to_string(),
            state::WORK_GROUP_ENGINE_VERSION_NOT_SPARK.to_string(),
        ),
    ];
    for (case, work_group, stated) in cases {
        let (status, refused) = start(&router, &work_group, json!({})).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (400, INVALID_REQUEST.to_string(), stated.clone()),
            "case {case:?}: expected {INVALID_REQUEST} {stated:?}, got {status} with {refused:?}"
        );
    }

    // The Spark workgroup, by contrast, takes the identical start.
    let (status, started) = start(&router, SPARK_WORK_GROUP, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the Spark enabled workgroup\": expected the same start to be accepted, got \
         {status} with {started:?}"
    );

    // And a session listing of a workgroup this account does not hold is the same miss.
    let (status, refused) = call(
        &router,
        "ListSessions",
        &json!({ "WorkGroup": "no-such-workgroup" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused), message(&refused)),
        (
            400,
            INVALID_REQUEST.to_string(),
            WORK_GROUP_NOT_FOUND.to_string()
        ),
        "case \"a listing of a workgroup this account does not hold\": got {status} with \
         {refused:?}"
    );
}

/// RULE: a repeated `ClientRequestToken` answers with the session it already started and
/// starts no second one; the same token under a changed parameter is refused.
#[tokio::test]
async fn a_repeated_start_token_answers_the_same_session() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;

    let token = request_token("session-token");
    let first = given_session(
        &router,
        SPARK_WORK_GROUP,
        json!({ "ClientRequestToken": token, "Description": "the one session" }),
    )
    .await;

    // The same token and the same parameters: the same answer, and no second session.
    for case in ["the first retry", "and another"] {
        let (status, repeated) = start(
            &router,
            SPARK_WORK_GROUP,
            json!({ "ClientRequestToken": token, "Description": "the one session" }),
        )
        .await;
        assert_eq!(
            (status, repeated["SessionId"].as_str()),
            (200, Some(&first[..])),
            "case {case:?}: expected the same session back, got {status} with {repeated:?}"
        );
    }
    let (_, sessions) = listed(&router, &json!({ "WorkGroup": SPARK_WORK_GROUP })).await;
    assert_eq!(
        sessions.len(),
        1,
        "case \"a retried start\": expected one session in the workgroup, got {sessions:?}"
    );

    // The same token with a parameter changed is refused, one row per parameter.
    let changed = [
        (
            "another description",
            json!({ "ClientRequestToken": token, "Description": "another session" }),
        ),
        (
            "another idle timeout",
            json!({
                "ClientRequestToken": token,
                "Description": "the one session",
                "SessionIdleTimeoutInMinutes": 30
            }),
        ),
        (
            "another tag list",
            json!({
                "ClientRequestToken": token,
                "Description": "the one session",
                "Tags": [{ "Key": "owner", "Value": "analytics" }]
            }),
        ),
        (
            "asking for the workgroup's tags to be copied",
            json!({
                "ClientRequestToken": token,
                "Description": "the one session",
                "CopyWorkGroupTags": true
            }),
        ),
    ];
    for (case, body) in changed {
        let (status, refused) = start(&router, SPARK_WORK_GROUP, body).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (
                400,
                INVALID_REQUEST.to_string(),
                IDEMPOTENT_PARAMETERS_DO_NOT_MATCH.to_string()
            ),
            "case {case:?}: expected the reused token to be refused, got {status} with \
             {refused:?}"
        );
    }
    // A DIFFERENT token starts a second session, so the refusals above are about the token.
    let second = given_session(
        &router,
        SPARK_WORK_GROUP,
        json!({ "ClientRequestToken": request_token("another-token") }),
    )
    .await;
    assert_ne!(
        second, first,
        "case \"another token\": expected a second session, got the first one back"
    );
}

/// RULE: a session listing reports the workgroup's active sessions newest first, one page of
/// the requested size at a time, with a token that resumes exactly where the page ended —
/// and a token this service did not mint for that listing names no page.
#[tokio::test]
async fn sessions_are_listed_newest_first_and_paged() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    given_spark_work_group(&router, "other-spark-sessions").await;

    let mut started = Vec::new();
    for index in 0..3 {
        started.push(
            given_session(
                &router,
                SPARK_WORK_GROUP,
                json!({ "Description": format!("session {index}") }),
            )
            .await,
        );
    }
    // Another workgroup's session is never in this workgroup's listing.
    let elsewhere = given_session(&router, "other-spark-sessions", json!({})).await;

    let newest_first: Vec<String> = started.iter().rev().cloned().collect();
    let cases: Vec<(&str, Value, usize)> = vec![
        (
            "no page size at all",
            json!({ "WorkGroup": SPARK_WORK_GROUP }),
            3,
        ),
        (
            "one session per page",
            json!({ "WorkGroup": SPARK_WORK_GROUP, "MaxResults": 1 }),
            1,
        ),
        (
            "two sessions per page",
            json!({ "WorkGroup": SPARK_WORK_GROUP, "MaxResults": 2 }),
            2,
        ),
        (
            "a page wider than the workgroup holds",
            json!({ "WorkGroup": SPARK_WORK_GROUP, "MaxResults": 100 }),
            3,
        ),
    ];
    for (case, mut body, page_size) in cases {
        let mut walked = Vec::new();
        loop {
            let (answer, page) = listed(&router, &body).await;
            assert!(
                page.len() <= page_size,
                "case {case:?}: expected at most {page_size} per page, got {page:?}"
            );
            walked.extend(page.iter().map(|(id, _)| id.clone()));
            match answer["NextToken"].as_str() {
                Some(token) => {
                    body["NextToken"] = json!(token);
                }
                None => break,
            }
        }
        assert_eq!(
            walked, newest_first,
            "case {case:?}: expected the workgroup's sessions newest first, got {walked:?}"
        );
        assert!(
            !walked.contains(&elsewhere),
            "case {case:?}: another workgroup's session is not in this listing, got {walked:?}"
        );
    }

    // `StateFilter` narrows the listing to the state it names. A session's state follows the
    // clock, so what is asserted is what holds whatever the clock has spent: every one of
    // these sessions is in one of the two states it can be in, so filtering by the first and
    // then by the second reports all three between them, and a state none of them has been
    // moved to reports none. The states are queried in the order the clock moves through
    // them, so a session cannot slip past both filters.
    let mut filtered: Vec<String> = Vec::new();
    for state in [SESSION_STATE_CREATING, SESSION_STATE_IDLE] {
        let (_, page) = listed(
            &router,
            &json!({ "WorkGroup": SPARK_WORK_GROUP, "StateFilter": state }),
        )
        .await;
        for (id, reported) in page {
            assert_eq!(
                reported, state,
                "case {state:?}: a filtered listing reports only sessions in that state, got \
                 {reported:?} for {id}"
            );
            if !filtered.contains(&id) {
                filtered.push(id);
            }
        }
    }
    filtered.sort();
    let mut all: Vec<String> = started.clone();
    all.sort();
    assert_eq!(
        filtered, all,
        "case \"the two states a live session can be in\": expected every session of the \
         workgroup between the two filters, got {filtered:?}"
    );
    for state in [SESSION_STATE_TERMINATED, SESSION_STATE_TERMINATING] {
        let (_, page) = listed(
            &router,
            &json!({ "WorkGroup": SPARK_WORK_GROUP, "StateFilter": state }),
        )
        .await;
        assert!(
            page.is_empty(),
            "case {state:?}: nothing asked these sessions to shut down, so none is reported \
             for that filter, got {page:?}"
        );
    }

    // A token this service did not mint for THIS listing names no page of it.
    let (first_answer, _) = listed(
        &router,
        &json!({ "WorkGroup": SPARK_WORK_GROUP, "MaxResults": 1 }),
    )
    .await;
    let this_listings_token = first_answer["NextToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let bogus = [
        (
            "a token of no characters",
            SPARK_WORK_GROUP,
            String::new(),
        ),
        (
            "a token nothing minted",
            SPARK_WORK_GROUP,
            "not-a-token".to_string(),
        ),
        (
            "this listing's own token, offered to another workgroup's listing",
            "other-spark-sessions",
            this_listings_token,
        ),
    ];
    for (case, work_group, token) in bogus {
        let expected_message = page_token_malformed(&token);
        let (status, refused) = call(
            &router,
            "ListSessions",
            &json!({ "WorkGroup": work_group, "NextToken": token }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (400, INVALID_REQUEST.to_string(), expected_message),
            "case {case:?}: expected the token to name no page, got {status} with {refused:?}"
        );
    }
}

/// RULE: a session belongs to its workgroup, so deleting the workgroup takes its sessions
/// with it and no later read resolves them.
#[tokio::test]
async fn deleting_the_workgroup_takes_its_sessions_with_it() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let session_id = given_session(&router, SPARK_WORK_GROUP, json!({})).await;
    let before = state_of(&router, &session_id).await;
    assert!(
        [SESSION_STATE_CREATING, SESSION_STATE_IDLE].contains(&before.as_str()),
        "the session must be readable, and live, before its workgroup is deleted, got \
         {before:?}"
    );

    let (status, deleted) = call(
        &router,
        "DeleteWorkGroup",
        &json!({ "WorkGroup": SPARK_WORK_GROUP, "RecursiveDeleteOption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must be deleted, got {status} with {deleted:?}"
    );

    for (operation, body) in session_addressed_operations(&session_id) {
        let (status, refused) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (
                400,
                INVALID_REQUEST.to_string(),
                SESSION_NOT_FOUND.to_string()
            ),
            "case \"after the workgroup was deleted\": expected {operation} to resolve nothing, \
             got {status} with {refused:?}"
        );
    }
}
