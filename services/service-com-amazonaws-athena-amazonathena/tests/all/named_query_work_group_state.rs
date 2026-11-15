// SPDX-License-Identifier: CC0-1.0

//! Saving a named query into a workgroup whose state forbids it, driven end to end over the
//! same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so
//! every assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: a workgroup in state `DISABLED` may not be used by its members —
//! neither to run a query in it nor to save a new named query to it — so `CreateNamedQuery`
//! and `StartQueryExecution` addressing such a workgroup are refused with
//! `InvalidRequestException` (HTTP 400) naming that workgroup, and the refused create
//! persists nothing; a workgroup whose stored state is `ENABLED`, including one that was
//! disabled and enabled again, takes both.
//!
//! Each test asserts that rule over a TABLE rather than one request: the rows differ along
//! the sequence of states the workgroup was put through, the members the create carries, and
//! which operation the rule is driven through — the create of a saved query and the
//! submission of a query, the two the rule covers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    self, DEFAULT_WORK_GROUP, WORK_GROUP_STATE_DISABLED, WORK_GROUP_STATE_ENABLED,
};

use crate::harness::{call, create_work_group, error_code, message, request_token, router};

/// The wire error code a refused request must carry: the client error both operations model
/// (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// Where the submitted queries below write their results. A query needs a results location,
/// and this suite asserts nothing about the objects themselves.
const OUTPUT_LOCATION: &str = "s3://athena-results/named-query-work-group-state/";

/// Put `work_group` into `state`, the way a client does.
async fn set_state(router: &MultiServiceRouter, work_group: &str, work_group_state: &str) {
    let (status, answer) = call(
        router,
        "UpdateWorkGroup",
        &json!({ "WorkGroup": work_group, "State": work_group_state }),
    )
    .await;
    assert_eq!(
        status, 200,
        "{work_group} must be moved to {work_group_state}, got {status} with {answer:?}"
    );
}

/// Create `work_group` and put it through `states` in order, leaving it in the last one.
async fn given_work_group(router: &MultiServiceRouter, work_group: &str, states: &[&str]) {
    create_work_group(router, work_group).await;
    for work_group_state in states {
        set_state(router, work_group, work_group_state).await;
    }
}

/// A `CreateNamedQuery` body for `work_group`, carrying whatever `extra` members the case
/// adds beside the required ones.
fn create_request(work_group: Option<&str>, name: &str, extra: &Value) -> Value {
    let mut request = json!({
        "Name": name,
        "Database": "logs",
        "QueryString": "SELECT * FROM requests WHERE status >= 500",
    });
    if let Some(work_group) = work_group {
        request["WorkGroup"] = json!(work_group);
    }
    for (member, value) in extra.as_object().into_iter().flatten() {
        request[member] = value.clone();
    }
    request
}

/// Assert that `body` is the refusal a disabled `work_group` answers with.
fn assert_disabled_refusal(case: &str, work_group: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the disabled workgroup {work_group} to be refused with 400, \
         got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        INVALID_REQUEST,
        "case {case:?}: expected the refusal to be {INVALID_REQUEST}, got {body:?}"
    );
    // Asserted against the wording itself, not only against the helper the handler words it
    // with, so a change to that wording fails here rather than agreeing with itself: the
    // sentence is fixed and the workgroup the request named is its one variable part.
    assert_eq!(
        message(body),
        format!("WorkGroup {work_group} is disabled."),
        "case {case:?}: expected the disabled-workgroup wording naming {work_group}, got {body:?}"
    );
    assert_eq!(
        message(body),
        state::work_group_disabled(work_group),
        "case {case:?}: expected the wording every operation refusing a disabled workgroup \
         answers with, got {body:?}"
    );
}

/// One row of the state table: the states a workgroup is put through before the create, what
/// the create carries, and whether it must be taken.
struct StateCase {
    case: &'static str,
    /// The `State` values the workgroup is moved through, in order; the last one is the state
    /// the create is judged by.
    states: Vec<&'static str>,
    /// Members the create carries beside `Name`, `Database`, `QueryString` and `WorkGroup`.
    extra: Value,
    /// Whether the create must be accepted.
    accepted: bool,
}

/// The rows: the state the workgroup is left in is what decides the create, whichever path it
/// reached that state by and whatever optional members the create carries.
fn state_cases() -> Vec<StateCase> {
    vec![
        StateCase {
            case: "a workgroup no update has touched, which is enabled",
            states: vec![],
            extra: json!({}),
            accepted: true,
        },
        StateCase {
            case: "a workgroup explicitly enabled",
            states: vec![WORK_GROUP_STATE_ENABLED],
            extra: json!({}),
            accepted: true,
        },
        StateCase {
            case: "a disabled workgroup",
            states: vec![WORK_GROUP_STATE_DISABLED],
            extra: json!({}),
            accepted: false,
        },
        StateCase {
            case: "a disabled workgroup, and a create carrying a description and a token",
            states: vec![WORK_GROUP_STATE_DISABLED],
            extra: json!({
                "Description": "the day's failed requests",
                "ClientRequestToken": request_token("disabled-with-token"),
            }),
            accepted: false,
        },
        StateCase {
            case: "a workgroup disabled and then enabled again",
            states: vec![WORK_GROUP_STATE_DISABLED, WORK_GROUP_STATE_ENABLED],
            extra: json!({ "Description": "re-enabled" }),
            accepted: true,
        },
        StateCase {
            case: "a workgroup enabled, then disabled twice",
            states: vec![
                WORK_GROUP_STATE_ENABLED,
                WORK_GROUP_STATE_DISABLED,
                WORK_GROUP_STATE_DISABLED,
            ],
            extra: json!({}),
            accepted: false,
        },
    ]
}

/// Whether a named query may be saved into a workgroup is decided by the state that
/// workgroup is in, and a create into a disabled one is refused with its own wording.
#[tokio::test]
async fn saving_a_named_query_follows_the_work_group_state() {
    let router = router();

    for (position, case) in state_cases().iter().enumerate() {
        let work_group = format!("wg-state-{position}");
        given_work_group(&router, &work_group, &case.states).await;

        let request = create_request(Some(&work_group), "daily-errors", &case.extra);
        let (status, body) = call(&router, "CreateNamedQuery", &request).await;

        if !case.accepted {
            assert_disabled_refusal(case.case, &work_group, status, &body);
            continue;
        }

        assert_eq!(
            status, 200,
            "case {:?}: expected the create in {work_group} to answer 200, got {status} with \
             {body:?}",
            case.case
        );
        let id = body["NamedQueryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: expected an id, got {body:?}", case.case))
            .to_string();

        // The saved query is real: it reads back through `GetNamedQuery`, reporting the
        // workgroup it was saved into.
        let (read_status, read_body) =
            call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        assert_eq!(
            read_status, 200,
            "case {:?}: expected GetNamedQuery to answer 200, got {read_status} with \
             {read_body:?}",
            case.case
        );
        assert_eq!(
            read_body["NamedQuery"]["WorkGroup"].as_str(),
            Some(work_group.as_str()),
            "case {:?}: expected the saved query to belong to {work_group}, got {read_body:?}",
            case.case
        );
    }
}

/// The disabled workgroup refuses BOTH of the things its members may no longer do — running a
/// query in it and saving a new named query to it — and no other workgroup is affected.
#[tokio::test]
async fn a_disabled_work_group_refuses_queries_and_saved_queries_alike() {
    let router = router();
    let disabled = "wg-disabled-both";
    let enabled = "wg-enabled-beside";
    given_work_group(&router, disabled, &[WORK_GROUP_STATE_DISABLED]).await;
    given_work_group(&router, enabled, &[]).await;

    // The rows differ along the operation the rule is driven through: the create of a saved
    // query and the submission of a query.
    let requests: Vec<(&str, &str, Value)> = vec![
        (
            "saving a named query",
            "CreateNamedQuery",
            create_request(Some(disabled), "errors", &json!({})),
        ),
        (
            "submitting a query",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "WorkGroup": disabled,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
        ),
    ];
    for (case, operation, request) in &requests {
        let (status, body) = call(&router, operation, request).await;
        assert_disabled_refusal(case, disabled, status, &body);
    }

    // One workgroup's state governs only that workgroup: a sibling workgroup and the
    // account's default workgroup, which no update put into a state of its own, both still
    // take a saved query.
    let elsewhere: Vec<(&str, Option<&str>, &str)> = vec![
        ("a sibling workgroup", Some(enabled), enabled),
        ("the account's default workgroup", None, DEFAULT_WORK_GROUP),
    ];
    for (case, named, expected) in elsewhere {
        let request = create_request(named, "elsewhere", &json!({}));
        let (status, body) = call(&router, "CreateNamedQuery", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to answer 200, got {status} with {body:?}"
        );
        let id = body["NamedQueryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {body:?}"))
            .to_string();
        let (read_status, read_body) =
            call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected GetNamedQuery to answer 200, got {read_status} with \
             {read_body:?}"
        );
        assert_eq!(
            read_body["NamedQuery"]["WorkGroup"].as_str(),
            Some(expected),
            "case {case:?}: expected the saved query to belong to {expected}, got {read_body:?}"
        );
    }
}

/// A create refused by the workgroup's state persists NOTHING: the workgroup still holds no
/// contents, and the token the refused call carried has minted no query.
#[tokio::test]
async fn a_create_refused_by_the_state_saves_nothing() {
    let router = router();

    // A workgroup that only ever refused a create holds no contents, so a delete that did
    // not ask for its contents takes it.
    let emptied = "wg-refused-then-deleted";
    given_work_group(&router, emptied, &[WORK_GROUP_STATE_DISABLED]).await;
    let token = request_token("refused-create");
    let request = create_request(
        Some(emptied),
        "errors",
        &json!({ "ClientRequestToken": token }),
    );
    let (status, body) = call(&router, "CreateNamedQuery", &request).await;
    assert_disabled_refusal("the refused create", emptied, status, &body);

    let (delete_status, delete_body) =
        call(&router, "DeleteWorkGroup", &json!({ "WorkGroup": emptied })).await;
    assert_eq!(
        delete_status, 200,
        "expected {emptied} to hold no contents after the refused create, so a delete that \
         did not ask for them takes it, got {delete_status} with {delete_body:?}"
    );

    // The token the refused create carried minted nothing either: once the workgroup is
    // enabled, the SAME token and the SAME parameters save a query rather than answering
    // with an id from the refused call.
    let reopened = "wg-refused-then-enabled";
    given_work_group(&router, reopened, &[WORK_GROUP_STATE_DISABLED]).await;
    let token = request_token("refused-then-retried");
    let request = create_request(
        Some(reopened),
        "errors",
        &json!({ "ClientRequestToken": token }),
    );
    let (status, body) = call(&router, "CreateNamedQuery", &request).await;
    assert_disabled_refusal("the retried create", reopened, status, &body);

    set_state(&router, reopened, WORK_GROUP_STATE_ENABLED).await;
    let (status, body) = call(&router, "CreateNamedQuery", &request).await;
    assert_eq!(
        status, 200,
        "expected the same create to be taken once {reopened} is enabled, got {status} with \
         {body:?}"
    );
    let id = body["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an id, got {body:?}"))
        .to_string();
    let (read_status, read_body) =
        call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_eq!(
        read_status, 200,
        "expected the query saved after the workgroup was enabled to read back, got \
         {read_status} with {read_body:?}"
    );
    assert_eq!(
        read_body["NamedQuery"]["WorkGroup"].as_str(),
        Some(reopened),
        "expected the saved query to belong to {reopened}, got {read_body:?}"
    );
}
