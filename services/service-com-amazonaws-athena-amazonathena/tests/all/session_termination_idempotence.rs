// SPDX-License-Identifier: CC0-1.0

//! `TerminateSession` asked REPEATEDLY, and asked for an id that never named a session,
//! driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a terminate of a session this account holds is a SUCCESS whatever
//! state that session is in and however many times it is asked — it answers 200 with the
//! state the session is then in, it does not move the shutdown already under way (so the
//! instant the session ended stays the instant the FIRST ask put it at), and the session
//! stays resolvable by a read afterwards; an id this account holds no session under is
//! refused with HTTP 400 `InvalidRequestException` carrying Athena's fixed sentence for that
//! class, with nothing of the request in it.
//!
//! Each test drives that rule over a TABLE rather than one request: the rows differ along
//! the state the first terminate is asked from (resources still being acquired, or the
//! session idle), how many repeats follow it, and — for the refusal — the shape and length
//! of the id the request carries.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! for the state it is waiting on under a deadline far above the span that transition is due
//! after, so a loaded host cannot turn a slow transition into a failure.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    ERROR_CODE_UNAUTHORIZED, PYSPARK_ENGINE_VERSION, SESSION_STATE_CREATING, SESSION_STATE_IDLE,
    SESSION_STATE_TERMINATED, SESSION_STATE_TERMINATING,
};

use crate::harness::{assert_refusal, call, router};

/// The sentence Athena answers a session-addressed request whose id named no session of this
/// account with. Entirely FIXED text for that class: the id the request carried is
/// deliberately not in it, so the same sentence is expected for every row of the table below.
const UNKNOWN_SESSION_MESSAGE: &str = "Not authorized to make this request.";

/// The Spark enabled workgroup the sessions below are started in.
const SPARK_WORK_GROUP: &str = "spark-teardown";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkTeardown";

/// Where that workgroup's members write their results.
const OUTPUT_LOCATION: &str = "s3://athena-spark-results/teardown/";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the spans those transitions are due after, so
/// the CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// Create the Spark enabled workgroup every start below names: a PySpark engine version, an
/// execution role and an output location, which is the workgroup Athena documents sessions
/// run in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": SPARK_WORK_GROUP,
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
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
}

/// Start one session in that workgroup and hand back the `SessionId` it minted.
async fn given_session(router: &MultiServiceRouter) -> String {
    let (status, started) = call(
        router,
        "StartSession",
        &json!({
            "WorkGroup": SPARK_WORK_GROUP,
            "EngineConfiguration": {
                "CoordinatorDpuSize": 1,
                "MaxConcurrentDpus": 2,
                "DefaultExecutorDpuSize": 1
            },
            "ExecutionRole": EXECUTION_ROLE,
            "SessionIdleTimeoutInMinutes": 15
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession must be accepted, got {status} with {started:?}"
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

/// Ask the session `session_id` to shut down, and hand back the whole answer.
async fn terminate(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(
        router,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await
}

/// The `SessionStatus` `GetSession` reports for `session_id`, asserting the read resolves.
async fn read_status(router: &MultiServiceRouter, case: &str, session_id: &str) -> Value {
    let (status, read) = call(router, "GetSession", &json!({ "SessionId": session_id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSession({session_id}) must resolve the session, got {status} with \
         {read:?}"
    );
    read["Status"].clone()
}

/// Poll until the session `session_id` reports `wanted`, and hand back the status that
/// reported it.
///
/// The CONDITION is what is waited on: every look asks the service what state the session is
/// in, and the deadline is far above the span the transition is due after.
async fn settled_in(
    router: &MultiServiceRouter,
    case: &str,
    session_id: &str,
    wanted: &str,
) -> Value {
    for _ in 0..POLL_LOOKS {
        let status = read_status(router, case, session_id).await;
        if status["State"].as_str() == Some(wanted) {
            return status;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the session {session_id} never reached {wanted}");
}

/// One row of the repeated-terminate table: the state the FIRST terminate is asked from, and
/// how many repeats follow the shutdown once it is complete.
struct RepeatCase {
    case: &'static str,
    /// The state to wait for before asking the first terminate, or `None` to ask straight
    /// after the start — while the session's resources are still being acquired.
    ask_from: Option<&'static str>,
    repeats: usize,
}

/// The table: a terminate asked while the session is still coming up and one asked once it is
/// idle, each followed by a different number of repeats — one, and several.
fn repeat_cases() -> Vec<RepeatCase> {
    vec![
        RepeatCase {
            case: "asked while the session's resources are still being acquired",
            ask_from: None,
            repeats: 1,
        },
        RepeatCase {
            case: "asked once the session is idle",
            ask_from: Some(SESSION_STATE_IDLE),
            repeats: 3,
        },
    ]
}

/// RULE: a terminate of a session this account holds is a success whatever state that session
/// is in and however often it is asked — it answers the state the session is then in, a
/// repeat of it neither refuses nor moves the instant the session ended, and the session is
/// still read back afterwards.
#[tokio::test]
async fn a_repeated_terminate_is_a_successful_no_op_reporting_terminated() {
    let router = router();
    given_spark_work_group(&router).await;

    for RepeatCase {
        case,
        ask_from,
        repeats,
    } in repeat_cases()
    {
        let session_id = given_session(&router).await;
        if let Some(wanted) = ask_from {
            settled_in(&router, case, &session_id, wanted).await;
        }

        let (status, asked) = terminate(&router, &session_id).await;
        let first = asked["State"].as_str().unwrap_or_default().to_string();
        assert!(
            status == 200
                && [SESSION_STATE_TERMINATING, SESSION_STATE_TERMINATED].contains(&first.as_str()),
            "case {case:?}: the first terminate must be a success reporting the shutdown, got \
             {status} with {asked:?}"
        );

        // The shutdown is complete once the clock says so, whether or not anybody asks again.
        let settled = settled_in(&router, case, &session_id, SESSION_STATE_TERMINATED).await;
        let ended_at = settled["EndDateTime"].clone();
        assert!(
            ended_at.as_f64().is_some(),
            "case {case:?}: a session whose shutdown is complete reports when it ended, got \
             {settled:?}"
        );

        // Every repeat is a success answering TERMINATED — never a refusal — and has NO
        // effect: the instant the session ended is still the one the first ask put it at.
        for repeat in 1..=repeats {
            let (status, again) = terminate(&router, &session_id).await;
            assert_eq!(
                (status, again["State"].as_str()),
                (200, Some(SESSION_STATE_TERMINATED)),
                "case {case:?}: repeat {repeat} must be a successful no-op, got {status} with \
                 {again:?}"
            );
            let after = read_status(&router, case, &session_id).await;
            assert_eq!(
                (after["State"].as_str(), after["EndDateTime"].clone()),
                (Some(SESSION_STATE_TERMINATED), ended_at.clone()),
                "case {case:?}: repeat {repeat} must leave the ended session exactly as it was, \
                 got {after:?}"
            );
        }
    }
}

/// RULE: a terminate carrying a `SessionId` this account holds no session under is refused
/// with HTTP 400 `InvalidRequestException` and Athena's fixed sentence for that class,
/// whatever the id looks like — while a session that WAS minted is terminated successfully.
#[tokio::test]
async fn a_terminate_of_an_id_that_never_named_a_session_is_refused() {
    let router = router();
    given_spark_work_group(&router).await;
    // A real session exists throughout, so each refusal below is about the id the request
    // carried rather than about an account holding no sessions at all.
    let live = given_session(&router).await;

    // The ids span the modelled `SessionId` (length 1 to 256, no pattern — see
    // smithy_query(query="shape",shape="SessionId")): the uuid form a start mints, the
    // shortest and the longest the model admits, one of another shape entirely, and the live
    // session's own id with one character changed.
    let unknown_ids = [
        ("a uuid no start minted", ::uuid::Uuid::new_v4().to_string()),
        ("the shortest id the model admits", "x".to_string()),
        ("the longest id the model admits", "s".repeat(256)),
        (
            "an id of another shape",
            "sessions/none-of-ours_42".to_string(),
        ),
        (
            "one character off a live session's id",
            format!("{live}-x"),
        ),
    ];
    for (case, unknown) in unknown_ids {
        assert_ne!(
            unknown, live,
            "case {case:?}: the unknown id must not be the live session's"
        );
        let (status, refused) = terminate(&router, &unknown).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            UNKNOWN_SESSION_MESSAGE,
        );
    }

    // The positive control in the same account: the id a start DID mint is terminated.
    let (status, terminated) = terminate(&router, &live).await;
    let state = terminated["State"].as_str().unwrap_or_default().to_string();
    assert!(
        status == 200
            && [SESSION_STATE_TERMINATING, SESSION_STATE_TERMINATED].contains(&state.as_str()),
        "case \"the live session\": a real session is terminated, got {status} with \
         {terminated:?}"
    );
}
