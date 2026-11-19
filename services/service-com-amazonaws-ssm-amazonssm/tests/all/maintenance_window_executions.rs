// SPDX-License-Identifier: CC0-1.0

//! `GetMaintenanceWindowExecution` end to end: how a `WindowExecutionId` is resolved, and
//! what a read answers for an id this endpoint holds no execution under.
//!
//! "Retrieves details about a specific a maintenance window execution"
//! (`smithy_query(query="doc",shape="GetMaintenanceWindowExecution")`): the id is resolved
//! against the RUNS of a window this service holds, which is its own id space — a window's
//! `WindowId` names no execution, and an id no run was recorded under names none either.
//! The operation models ONE client error for that, `DoesNotExistException` (HTTP 400), whose
//! sentence is worded for the data store the id was looked for in and carries no part of the
//! request.
//!
//! Nothing in this emulator picks up a maintenance window's tasks — the same reading Run
//! Command takes of a command's invocations, which nothing picks up either — so no run of a
//! window is recorded and nothing is invented for one: creating a window, registering a task
//! with it and reading it back leave the execution store exactly as empty as it was, and
//! every id a caller can name is refused. The records themselves, and what a read reports
//! from one, are driven in the owning module's own tests
//! (`state::maintenance_window_executions`).
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`call_in_region`], [`error_code`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service
//! router.
//!
//! The suites are tables: the rows differ along the form of the id asked for (the case of its
//! hexadecimal digits, the groups it carries), the Region it is asked through and what the
//! account holds when it is asked — rather than the single id a recorded unit happened to
//! read.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, error_message, router, REGION};

/// The sentence this service refuses an id no execution is held under with: FIXED text,
/// with no part of the request in it
/// (`service_com_amazonaws_ssm_amazonssm::state::maintenance_window_executions`).
const DOES_NOT_EXIST: &str = "Resource does not exist in the data store.";

/// A second Region, to pin that an execution is reached through the endpoint of the Region
/// its window lives in.
const OTHER_REGION: &str = "eu-central-1";

/// Read one execution at `region`'s endpoint.
async fn get_in(
    router: &MultiServiceRouter,
    region: &str,
    window_execution_id: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetMaintenanceWindowExecution",
        &json!({ "WindowExecutionId": window_execution_id }),
    )
    .await
}

/// Assert that `window_execution_id` is refused as naming no execution, at `region`.
async fn refused(router: &MultiServiceRouter, case: &str, region: &str, window_execution_id: &str) {
    let (status, response) = get_in(router, region, window_execution_id).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {window_execution_id} names no execution, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "DoesNotExistException",
        "case {case:?}: expected the modelled not-found refusal, got {response}"
    );
    let message = error_message(&response);
    assert_eq!(
        message, DOES_NOT_EXIST,
        "case {case:?}: expected {DOES_NOT_EXIST:?}, got {message:?}"
    );
}

/// RULE: an id this endpoint holds no execution under is refused `DoesNotExistException`
/// with the sentence this error class carries — never an unmodelled fault, and never a
/// 200 with an execution nothing ran.
#[tokio::test]
async fn an_id_naming_no_execution_is_does_not_exist() {
    let router = router();

    // (case, the id asked for, the Region asked through)
    let rows: [(&str, &str, &str); 6] = [
        (
            "an id nothing recorded",
            "11111111-2222-3333-4444-555555555555",
            REGION,
        ),
        (
            "an id of all zeroes",
            "00000000-0000-0000-0000-000000000000",
            REGION,
        ),
        (
            "an id of all f",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            REGION,
        ),
        (
            "an id in upper case, which the modelled pattern admits",
            "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
            REGION,
        ),
        (
            "an id in mixed case",
            "AaBbCcDd-1234-abcd-5678-9aBcDeF01234",
            REGION,
        ),
        (
            "an id asked through another Region's endpoint",
            "11111111-2222-3333-4444-555555555555",
            OTHER_REGION,
        ),
    ];

    for (case, window_execution_id, region) in rows {
        refused(&router, case, region, window_execution_id).await;
    }
}

/// A value that is not shaped like a `WindowExecutionId` at all never reaches the read: the
/// member's length and hexadecimal-group pattern are MODELLED, so the boundary walk refuses
/// it. The rows differ along which part of the shape they break.
#[tokio::test]
async fn a_value_shaped_like_no_execution_id_is_refused_by_the_boundary() {
    let router = router();

    // (case, the value sent)
    let rows: [(&str, &str); 5] = [
        ("a value of no groups", "notanexecutionid"),
        ("a value too short", "1111111-2222-3333-4444-555555555555"),
        ("a value too long", "111111111-2222-3333-4444-555555555555"),
        (
            "a value carrying a digit the pattern excludes",
            "zzzzzzzz-2222-3333-4444-555555555555",
        ),
        ("a window id", "mw-0123456789abcdef0"),
    ];

    for (case, value) in rows {
        let (status, response) = get_in(&router, REGION, value).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {value:?} is no execution id, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the boundary refusal, got {response}"
        );
    }
}

/// Holding a maintenance window — tasks and all — records no run of it: a window is a
/// schedule, and the executions this read resolves are the runs something picked up. So the
/// refusal is the same before and after a window exists, and the window's own reads are
/// unaffected by the refused ones.
#[tokio::test]
async fn a_window_and_its_tasks_record_no_execution() {
    let router = router();
    let probe = "11111111-2222-3333-4444-555555555555";
    refused(&router, "before any window exists", REGION, probe).await;

    let (status, created) = call(
        &router,
        "CreateMaintenanceWindow",
        &json!({
            "Name": "execution-lookup-window",
            "Schedule": "cron(0/15 * * * ? *)",
            "Duration": 3,
            "Cutoff": 1,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must be created, got {status} {created}"
    );
    let window_id = created["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a WindowId, got {created}"))
        .to_string();

    let (status, registered) = call(
        &router,
        "RegisterTaskWithMaintenanceWindow",
        &json!({
            "WindowId": window_id,
            "TaskArn": "AWS-RunShellScript",
            "TaskType": "RUN_COMMAND",
            "Priority": 1,
            "MaxConcurrency": "1",
            "MaxErrors": "1",
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the task must be registered, got {status} {registered}"
    );
    let window_task_id = registered["WindowTaskId"]
        .as_str()
        .unwrap_or_else(|| panic!("the register reports a WindowTaskId, got {registered}"))
        .to_string();

    // (case, the id asked for) — neither the window's own id space nor the task's is the
    // execution id space, so an id out of either names no execution.
    for (case, id) in [
        ("an id nothing recorded, now that a window exists", probe),
        ("the id of the window's own task", window_task_id.as_str()),
    ] {
        refused(&router, case, REGION, id).await;
    }

    // The window itself still reads back as the create left it: a refused execution read
    // changed nothing.
    let (status, read) = call(
        &router,
        "GetMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must still be readable, got {status} {read}"
    );
    assert_eq!(
        read["WindowId"].as_str(),
        Some(window_id.as_str()),
        "expected the window the create minted, got {read}"
    );
}
