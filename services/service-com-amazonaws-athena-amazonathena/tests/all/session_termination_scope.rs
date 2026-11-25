// SPDX-License-Identifier: CC0-1.0

//! `TerminateSession` driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`), asked for ONE session of an account that holds
//! several.
//!
//! THE RULE UNDER TEST: a terminate addresses exactly the session its `SessionId` names — it
//! answers 200 with the state that one session is then in, that session's own reads report
//! the same shutdown (so it leaves the workgroup's active listing while `GetSession` still
//! resolves it), and every OTHER session the account holds keeps the state it had, whether it
//! runs in the same workgroup or another one, and is shut down only when it is itself asked.
//!
//! Each test drives that rule over a TABLE rather than one request: the rows differ along the
//! state the addressed session is asked from (its resources still being acquired, or the
//! session idle), how many sessions stand beside it in the same workgroup, and how many stand
//! in a second Spark workgroup of the same account.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! for the state it is waiting on under a deadline far above the span that transition is due
//! after, so a loaded host cannot turn a slow transition into a failure.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    ACTIVE_SESSION_STATES, PYSPARK_ENGINE_VERSION, SESSION_STATE_IDLE, SESSION_STATE_TERMINATED,
    SESSION_STATE_TERMINATING,
};

use crate::harness::{call, router};

/// The two Spark enabled workgroups the sessions below run in: one the terminate addresses a
/// session of, and a second holding sessions of the same account that no request addresses.
const ADDRESSED_WORK_GROUP: &str = "spark-scope-addressed";
/// See [`ADDRESSED_WORK_GROUP`].
const OTHER_WORK_GROUP: &str = "spark-scope-elsewhere";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkScope";

/// Where the members of those workgroups write their results.
const OUTPUT_LOCATION: &str = "s3://athena-spark-results/scope/";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the spans those transitions are due after, so the
/// CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// The states a session counts as ACTIVE in, which is what `ListSessions` lists and what a
/// session no terminate has addressed is in.
fn is_active(state: &str) -> bool {
    ACTIVE_SESSION_STATES.contains(&state)
}

/// The states a session that has been asked to shut down is in: shutting down, or already
/// gone.
fn is_shutting_down(state: &str) -> bool {
    [SESSION_STATE_TERMINATING, SESSION_STATE_TERMINATED].contains(&state)
}

/// Create one Spark enabled workgroup: a PySpark engine version, an execution role and an
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

/// Start one session in `work_group` and hand back the `SessionId` it minted.
async fn given_session(router: &MultiServiceRouter, work_group: &str) -> String {
    let (status, started) = call(
        router,
        "StartSession",
        &json!({
            "WorkGroup": work_group,
            "EngineConfiguration": {
                "CoordinatorDpuSize": 1,
                "MaxConcurrentDpus": 2,
                "DefaultExecutorDpuSize": 1
            },
            "ExecutionRole": EXECUTION_ROLE,
            "SessionIdleTimeoutInMinutes": 60
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession({work_group}) must be accepted, got {status} with {started:?}"
    );
    // A start answers with the state the session is in — one of the ACTIVE states, since the
    // session is running for the caller from here on. Which one it is depends on how much of
    // the creation span has already run, so this asserts the property the rule under test
    // needs rather than the host's speed.
    let state = started["State"].as_str().unwrap_or_default();
    assert!(
        ACTIVE_SESSION_STATES.contains(&state),
        "a freshly started session is active, got {state:?} in {started:?}"
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

/// The state `GetSessionStatus` reports for `session_id`, asserting the read resolves.
async fn reported_state(router: &MultiServiceRouter, case: &str, session_id: &str) -> String {
    let (status, read) = call(
        router,
        "GetSessionStatus",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSessionStatus({session_id}) must resolve the session, got {status} \
         with {read:?}"
    );
    read["Status"]["State"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a reported state, got {read:?}"))
        .to_string()
}

/// Poll until the session `session_id` reports `wanted`.
///
/// The CONDITION is what is waited on: every look asks the service what state the session is
/// in, and the deadline is far above the span the transition is due after.
async fn settled_in(router: &MultiServiceRouter, case: &str, session_id: &str, wanted: &str) {
    for _ in 0..POLL_LOOKS {
        if reported_state(router, case, session_id).await == wanted {
            return;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the session {session_id} never reached {wanted}");
}

/// The session ids `ListSessions` reports for `work_group` — the workgroup's ACTIVE sessions.
async fn listed(router: &MultiServiceRouter, case: &str, work_group: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "ListSessions",
        &json!({ "WorkGroup": work_group, "MaxResults": 100 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListSessions({work_group}) must answer 200, got {status} with {body:?}"
    );
    body["Sessions"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|summary| summary["SessionId"].as_str().map(str::to_string))
        .collect()
}

/// One row of the table: the state the addressed session is asked from, and how many sessions
/// stand beside it in its own workgroup and in a second one.
struct ScopeCase {
    case: &'static str,
    /// The state to wait for before asking the terminate, or `None` to ask straight after the
    /// start — while the session's resources are still being acquired.
    ask_from: Option<&'static str>,
    beside: usize,
    elsewhere: usize,
}

/// The table: a terminate asked while the addressed session is still coming up and one asked
/// once it is idle, each with a different number of sessions standing beside it in its own
/// workgroup and in another workgroup of the same account.
fn scope_cases() -> Vec<ScopeCase> {
    vec![
        ScopeCase {
            case: "asked while the addressed session's resources are still being acquired",
            ask_from: None,
            beside: 1,
            elsewhere: 1,
        },
        ScopeCase {
            case: "asked once the addressed session is idle",
            ask_from: Some(SESSION_STATE_IDLE),
            beside: 2,
            elsewhere: 1,
        },
    ]
}

/// RULE: a terminate addresses exactly the session it names — that session reports the
/// shutdown and leaves its workgroup's active listing while a read still resolves it, and
/// every other session of the account, in the same workgroup or another, keeps the state it
/// had.
#[tokio::test]
async fn a_terminate_shuts_down_only_the_session_it_names() {
    let router = router();
    given_spark_work_group(&router, ADDRESSED_WORK_GROUP).await;
    given_spark_work_group(&router, OTHER_WORK_GROUP).await;

    for ScopeCase {
        case,
        ask_from,
        beside,
        elsewhere,
    } in scope_cases()
    {
        let addressed = given_session(&router, ADDRESSED_WORK_GROUP).await;
        let mut siblings = Vec::new();
        for _ in 0..beside {
            siblings.push(given_session(&router, ADDRESSED_WORK_GROUP).await);
        }
        let mut others = Vec::new();
        for _ in 0..elsewhere {
            others.push(given_session(&router, OTHER_WORK_GROUP).await);
        }
        if let Some(wanted) = ask_from {
            settled_in(&router, case, &addressed, wanted).await;
        }

        let (status, asked) = terminate(&router, &addressed).await;
        let answered = asked["State"].as_str().unwrap_or_default().to_string();
        assert!(
            status == 200 && is_shutting_down(&answered),
            "case {case:?}: the terminate must answer the shutdown of the session it names, got \
             {status} with {asked:?}"
        );

        // The addressed session's OWN reads report that same shutdown.
        let reported = reported_state(&router, case, &addressed).await;
        assert!(
            is_shutting_down(&reported),
            "case {case:?}: the addressed session must report the shutdown, got {reported:?}"
        );

        // Every other session of the account is untouched — whichever workgroup it runs in.
        for (where_it_runs, sessions) in [
            ("the same workgroup", &siblings),
            ("another workgroup", &others),
        ] {
            for other in sessions {
                let state = reported_state(&router, case, other).await;
                assert!(
                    is_active(&state),
                    "case {case:?}: the session {other} in {where_it_runs} must keep an active \
                     state, got {state:?}"
                );
            }
        }

        // The listing of a workgroup's active sessions follows: the addressed session has
        // left it, those beside it are still in it, and the second workgroup's listing is
        // untouched. A read of the addressed session still resolves it.
        let active_here = listed(&router, case, ADDRESSED_WORK_GROUP).await;
        assert!(
            !active_here.contains(&addressed),
            "case {case:?}: the terminated session {addressed} must leave the active listing, \
             got {active_here:?}"
        );
        for sibling in &siblings {
            assert!(
                active_here.contains(sibling),
                "case {case:?}: the session {sibling} beside it must still be listed, got \
                 {active_here:?}"
            );
        }
        let active_elsewhere = listed(&router, case, OTHER_WORK_GROUP).await;
        for other in &others {
            assert!(
                active_elsewhere.contains(other),
                "case {case:?}: the session {other} in another workgroup must still be listed, \
                 got {active_elsewhere:?}"
            );
        }
        let (status, read) = call(
            &router,
            "GetSession",
            &json!({ "SessionId": addressed.clone() }),
        )
        .await;
        assert_eq!(
            (status, read["SessionId"].as_str()),
            (200, Some(addressed.as_str())),
            "case {case:?}: a read must still resolve the terminated session, got {status} with \
             {read:?}"
        );

        // Each of those other sessions shuts down only when it is ITSELF asked, so the scope
        // of a terminate is the session and not the workgroup or the account.
        for other in siblings.iter().chain(others.iter()) {
            let (status, asked) = terminate(&router, other).await;
            let answered = asked["State"].as_str().unwrap_or_default().to_string();
            assert!(
                status == 200 && is_shutting_down(&answered),
                "case {case:?}: the session {other} must shut down when asked itself, got \
                 {status} with {asked:?}"
            );
        }
        for work_group in [ADDRESSED_WORK_GROUP, OTHER_WORK_GROUP] {
            let still_active = listed(&router, case, work_group).await;
            assert!(
                still_active.is_empty(),
                "case {case:?}: every session of {work_group} was asked to shut down, so none is \
                 active, got {still_active:?}"
            );
        }
    }
}
