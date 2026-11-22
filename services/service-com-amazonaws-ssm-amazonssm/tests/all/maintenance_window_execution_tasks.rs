// SPDX-License-Identifier: CC0-1.0

//! The maintenance-window EXECUTION-TASK read surface end to end:
//! `GetMaintenanceWindowExecutionTask` and `GetMaintenanceWindowExecutionTaskInvocation` read
//! ONE record, `DescribeMaintenanceWindowExecutionTasks` and
//! `DescribeMaintenanceWindowExecutionTaskInvocations` list the records of one run and of one
//! task execution.
//!
//! Two rules are asserted here, each over a table:
//!
//! 1. a READ of one record resolves its ids against the task executions (or invocations) this
//!    endpoint holds and refuses a set of ids it holds nothing under with the modelled
//!    `DoesNotExistException` (HTTP 400), whose sentence is worded for the data store the ids
//!    were looked for in and carries no part of the request — never an unmodelled fault and
//!    never a 200 for work nothing ran;
//! 2. a LISTING of the records under one id reports the ones this endpoint holds, in the order
//!    they started, narrowed by the one documented filter key and bounded by the modelled page
//!    size — and reports a run (or a task execution) holding none as an EMPTY identity list
//!    with HTTP 200 rather than refusing it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router_with_engine`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. The ONE thing seeded through the engine is a task
//! execution of a run, because nothing in this emulator picks up a maintenance window's tasks
//! — the same reading Run Command takes of a command's invocations — so no call of this
//! service records one.
//!
//! The rows span what each rule generalises over: the form of the ids asked for, the Region
//! asked through, which part of a composite key is wrong, how many records a listing walks,
//! how large a page it asks for, and which status it narrows by.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::maintenance_window_execution_task_invocations::MaintenanceWindowExecutionTaskInvocationRecord;
use service_com_amazonaws_ssm_amazonssm::state::maintenance_window_execution_tasks::MaintenanceWindowExecutionTaskRecord;
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router_with_engine, REGION,
};

/// The sentence this service refuses ids the data store holds nothing under with: FIXED text,
/// with no part of the request in it
/// (`service_com_amazonaws_ssm_amazonssm::state::maintenance_window_executions`).
const DOES_NOT_EXIST: &str = "Resource does not exist in the data store.";

/// A second Region, to pin that a run — and every record under it — is reached through the
/// endpoint of the Region its window lives in.
const OTHER_REGION: &str = "eu-central-1";

/// The run every seeded record below belongs to.
const RUN: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// The task execution every seeded invocation below belongs to.
const TASK: &str = "11111111-2222-3333-4444-555555555555";

/// An id nothing is ever recorded under, for the refusal rows.
const UNRECORDED: &str = "99999999-9999-9999-9999-999999999999";

/// The instant the first seeded record started, as a whole number of seconds since the epoch.
const FIRST_START: u64 = 1_893_456_000;

/// The page size the modelled `MaintenanceWindowMaxResults` admits at its low end
/// (`range 10..100`), which is the smallest page either listing serves.
const MIN_PAGE: usize = 10;

/// A clock reading `seconds` after the epoch.
fn epoch(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Record one task execution of `run` in `region`, the way a run of the window would.
///
/// Seeded through the engine because no call of this service records a run; every member a
/// read reports is set here, so a read that invents one is visible.
fn seed_task(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    run: &str,
    task_execution_id: &str,
    status: &str,
    started: u64,
) {
    let record = MaintenanceWindowExecutionTaskRecord {
        region: region.to_string(),
        window_execution_id: run.to_string(),
        task_execution_id: task_execution_id.to_string(),
        window_task_id: Some("abcdabcd-1234-5678-9abc-abcdabcdabcd".to_string()),
        task_arn: "AWS-RunShellScript".to_string(),
        task_type: "RUN_COMMAND".to_string(),
        service_role: Some("arn:aws:iam::123456789012:role/mw".to_string()),
        task_parameters: Vec::new(),
        priority: 3,
        max_concurrency: Some("10%".to_string()),
        max_errors: Some("1".to_string()),
        status: status.to_string(),
        status_details: Some(format!("status detail for {status}")),
        started_at: epoch(started),
        ended_at: Some(epoch(started + 60)),
        alarm_configuration: None,
        triggered_alarms: Vec::new(),
    };
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .insert_maintenance_window_execution_task(record);
}

/// Record one invocation of `task` of `run` in `region`.
fn seed_invocation(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    run: &str,
    task: &str,
    invocation_id: &str,
    status: &str,
    started: u64,
) {
    let record = MaintenanceWindowExecutionTaskInvocationRecord {
        region: region.to_string(),
        window_execution_id: run.to_string(),
        task_execution_id: task.to_string(),
        invocation_id: invocation_id.to_string(),
        execution_id: Some("c1d2e3f4-5a6b-7c8d-9e0f-1a2b3c4d5e6f".to_string()),
        task_type: "RUN_COMMAND".to_string(),
        parameters: Some("{\"commands\":[\"echo hello\"]}".to_string()),
        status: status.to_string(),
        status_details: Some(format!("status detail for {status}")),
        started_at: epoch(started),
        ended_at: Some(epoch(started + 30)),
        owner_information: Some("patch wave 1".to_string()),
        window_target_id: Some("22222222-3333-4444-5555-666666666666".to_string()),
    };
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .insert_maintenance_window_execution_task_invocation(record);
}

/// Assert that `response` is the modelled not-found refusal carrying this error class's fixed
/// sentence.
fn assert_does_not_exist(case: &str, status: u16, response: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the modelled refusal, got {status} {response}"
    );
    assert_eq!(
        error_code(response),
        "DoesNotExistException",
        "case {case:?}: expected the modelled not-found refusal, got {response}"
    );
    let message = error_message(response);
    assert_eq!(
        message, DOES_NOT_EXIST,
        "case {case:?}: expected {DOES_NOT_EXIST:?}, got {message:?}"
    );
}

/// The identities a task-execution listing reported, as `(TaskExecutionId, Status)` pairs.
fn listed_tasks(response: &Value) -> Vec<(String, String)> {
    response["WindowExecutionTaskIdentities"]
        .as_array()
        .unwrap_or_else(|| panic!("the listing reports an identity list, got {response}"))
        .iter()
        .map(|identity| {
            (
                identity["TaskExecutionId"].as_str().unwrap_or("").to_string(),
                identity["Status"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The identities an invocation listing reported, as `(InvocationId, Status)` pairs.
fn listed_invocations(response: &Value) -> Vec<(String, String)> {
    response["WindowExecutionTaskInvocationIdentities"]
        .as_array()
        .unwrap_or_else(|| panic!("the listing reports an identity list, got {response}"))
        .iter()
        .map(|identity| {
            (
                identity["InvocationId"].as_str().unwrap_or("").to_string(),
                identity["Status"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// One task-execution id, built so the rows below can seed as many as a page needs.
fn task_id(ordinal: usize) -> String {
    format!("{ordinal:08x}-0000-4000-8000-000000000000")
}

/// RULE: a set of ids this endpoint holds no record under is refused `DoesNotExistException`
/// with the sentence this error class carries — on BOTH reads of one record, whichever part of
/// the composite key names nothing.
#[tokio::test]
async fn ids_naming_no_task_execution_are_does_not_exist() {
    let (router, engine) = router_with_engine();
    seed_task(&engine, REGION, RUN, TASK, "SUCCESS", FIRST_START);
    seed_invocation(
        &engine,
        REGION,
        RUN,
        TASK,
        "66666666-7777-8888-9999-aaaaaaaaaaaa",
        "SUCCESS",
        FIRST_START,
    );

    // (case, Region asked through, run, task execution) — the read of one task execution.
    let task_rows: [(&str, &str, &str, &str); 6] = [
        ("a run nothing recorded", REGION, UNRECORDED, UNRECORDED),
        (
            "a held run, a task execution nothing recorded",
            REGION,
            RUN,
            UNRECORDED,
        ),
        ("a held task execution under another run", REGION, UNRECORDED, TASK),
        ("the two ids the other way round", REGION, TASK, RUN),
        ("a held pair at another Region's endpoint", OTHER_REGION, RUN, TASK),
        (
            "ids of all zeroes",
            REGION,
            "00000000-0000-0000-0000-000000000000",
            "00000000-0000-0000-0000-000000000000",
        ),
    ];

    for (case, region, run, task) in task_rows {
        let (status, response) = call_in_region(
            &router,
            region,
            "GetMaintenanceWindowExecutionTask",
            &json!({ "WindowExecutionId": run, "TaskId": task }),
        )
        .await;
        assert_does_not_exist(case, status, &response);
    }

    // (case, Region, run, task execution, invocation) — the read of one invocation.
    let invocation_rows: [(&str, &str, &str, &str, &str); 5] = [
        (
            "nothing recorded at all",
            REGION,
            UNRECORDED,
            UNRECORDED,
            UNRECORDED,
        ),
        (
            "a held task execution, an invocation nothing recorded",
            REGION,
            RUN,
            TASK,
            UNRECORDED,
        ),
        (
            "a held invocation under another task execution",
            REGION,
            RUN,
            UNRECORDED,
            "66666666-7777-8888-9999-aaaaaaaaaaaa",
        ),
        (
            "a held invocation under another run",
            REGION,
            UNRECORDED,
            TASK,
            "66666666-7777-8888-9999-aaaaaaaaaaaa",
        ),
        (
            "a held triple at another Region's endpoint",
            OTHER_REGION,
            RUN,
            TASK,
            "66666666-7777-8888-9999-aaaaaaaaaaaa",
        ),
    ];

    for (case, region, run, task, invocation) in invocation_rows {
        let (status, response) = call_in_region(
            &router,
            region,
            "GetMaintenanceWindowExecutionTaskInvocation",
            &json!({
                "WindowExecutionId": run,
                "TaskId": task,
                "InvocationId": invocation,
            }),
        )
        .await;
        assert_does_not_exist(case, status, &response);
    }
}

/// A value that is not shaped like one of these ids never reaches a read: every member's
/// length and hexadecimal-group pattern is MODELLED, so the boundary walk refuses it. The rows
/// differ along which part of the shape they break and which member carries it.
#[tokio::test]
async fn a_value_shaped_like_no_id_is_refused_by_the_boundary() {
    let router = router_with_engine().0;

    // (case, the value sent)
    let malformed: [(&str, &str); 5] = [
        ("a value of no groups", "notanid"),
        ("a value too short", "1111111-2222-3333-4444-555555555555"),
        ("a value too long", "111111111-2222-3333-4444-555555555555"),
        (
            "a value carrying a digit the pattern excludes",
            "zzzzzzzz-2222-3333-4444-555555555555",
        ),
        ("a window id", "mw-0123456789abcdef0"),
    ];

    for (case, value) in malformed {
        // The same value in each member of each operation of the surface.
        let requests = [
            (
                "GetMaintenanceWindowExecutionTask",
                json!({ "WindowExecutionId": value, "TaskId": TASK }),
            ),
            (
                "GetMaintenanceWindowExecutionTask",
                json!({ "WindowExecutionId": RUN, "TaskId": value }),
            ),
            (
                "GetMaintenanceWindowExecutionTaskInvocation",
                json!({ "WindowExecutionId": RUN, "TaskId": TASK, "InvocationId": value }),
            ),
            (
                "DescribeMaintenanceWindowExecutionTasks",
                json!({ "WindowExecutionId": value }),
            ),
            (
                "DescribeMaintenanceWindowExecutionTaskInvocations",
                json!({ "WindowExecutionId": RUN, "TaskId": value }),
            ),
        ];
        for (operation, body) in requests {
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: {value:?} is no id of {operation}, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected the boundary refusal from {operation}, got {response}"
            );
        }
    }
}

/// RULE: a listing reports the records this endpoint holds under the ids it names — so a run
/// (or task execution) holding none is reported as an EMPTY identity list with HTTP 200, never
/// refused. The rows differ along what the account holds, the ids asked for and the Region
/// asked through.
#[tokio::test]
async fn a_run_holding_no_record_is_listed_empty() {
    let (router, engine) = router_with_engine();

    // Before anything is held at all.
    for (case, run) in [("a fresh account", RUN), ("an id nothing recorded", UNRECORDED)] {
        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTasks",
            &json!({ "WindowExecutionId": run }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a listing of no task execution succeeds, got {status} {response}"
        );
        assert_eq!(
            listed_tasks(&response),
            Vec::<(String, String)>::new(),
            "case {case:?}: expected an empty identity list, got {response}"
        );
        assert!(
            response["NextToken"].is_null(),
            "case {case:?}: an empty page continues nowhere, got {response}"
        );
    }

    // Holding a maintenance window records no run of it, so the listing stays empty.
    let (status, created) = call(
        &router,
        "CreateMaintenanceWindow",
        &json!({
            "Name": "execution-task-listing-window",
            "Schedule": "rate(30 days)",
            "Duration": 1,
            "Cutoff": 0,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the window must be created, got {status} {created}"
    );
    let (status, response) = call(
        &router,
        "DescribeMaintenanceWindowExecutionTasks",
        &json!({ "WindowExecutionId": RUN }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a window records no run, got {status} {response}"
    );
    assert!(
        listed_tasks(&response).is_empty(),
        "expected an empty identity list once a window exists, got {response}"
    );

    // Now hold one record, and ask for the ids it is NOT held under.
    seed_task(&engine, REGION, RUN, TASK, "IN_PROGRESS", FIRST_START);
    seed_invocation(
        &engine,
        REGION,
        RUN,
        TASK,
        "66666666-7777-8888-9999-aaaaaaaaaaaa",
        "IN_PROGRESS",
        FIRST_START,
    );

    for (case, region, run) in [
        ("another run of the same Region", REGION, UNRECORDED),
        ("the held run at another Region's endpoint", OTHER_REGION, RUN),
    ] {
        let (status, response) = call_in_region(
            &router,
            region,
            "DescribeMaintenanceWindowExecutionTasks",
            &json!({ "WindowExecutionId": run }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an empty listing, got {status} {response}"
        );
        assert!(
            listed_tasks(&response).is_empty(),
            "case {case:?}: expected an empty identity list, got {response}"
        );
    }

    for (case, region, run, task) in [
        ("another task execution of the held run", REGION, RUN, UNRECORDED),
        ("another run of the same Region", REGION, UNRECORDED, TASK),
        (
            "the held task execution at another Region's endpoint",
            OTHER_REGION,
            RUN,
            TASK,
        ),
    ] {
        let (status, response) = call_in_region(
            &router,
            region,
            "DescribeMaintenanceWindowExecutionTaskInvocations",
            &json!({ "WindowExecutionId": run, "TaskId": task }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an empty listing, got {status} {response}"
        );
        assert!(
            listed_invocations(&response).is_empty(),
            "case {case:?}: expected an empty identity list, got {response}"
        );
    }
}

/// What a read reports is the record the endpoint holds, member by member — and the listing
/// reports the same values for the members its identity declares, so a caller that lists and
/// then reads sees ONE task execution. Reading twice reports the same thing: a read changes
/// nothing a later caller sees.
#[tokio::test]
async fn a_held_task_execution_reads_back_through_both_reads() {
    let (router, engine) = router_with_engine();
    seed_task(&engine, REGION, RUN, TASK, "SUCCESS", FIRST_START);
    let invocation_id = "66666666-7777-8888-9999-aaaaaaaaaaaa";
    seed_invocation(&engine, REGION, RUN, TASK, invocation_id, "SUCCESS", FIRST_START);

    for round in 1..=2 {
        let (status, read) = call(
            &router,
            "GetMaintenanceWindowExecutionTask",
            &json!({ "WindowExecutionId": RUN, "TaskId": TASK }),
        )
        .await;
        assert_eq!(
            status, 200,
            "round {round}: the held task execution must be readable, got {status} {read}"
        );
        // (member, reported, expected) — every member of the record the read reports.
        let expected: [(&str, &Value, Value); 10] = [
            ("WindowExecutionId", &read["WindowExecutionId"], json!(RUN)),
            ("TaskExecutionId", &read["TaskExecutionId"], json!(TASK)),
            ("TaskArn", &read["TaskArn"], json!("AWS-RunShellScript")),
            ("Type", &read["Type"], json!("RUN_COMMAND")),
            (
                "ServiceRole",
                &read["ServiceRole"],
                json!("arn:aws:iam::123456789012:role/mw"),
            ),
            ("Priority", &read["Priority"], json!(3)),
            ("MaxConcurrency", &read["MaxConcurrency"], json!("10%")),
            ("MaxErrors", &read["MaxErrors"], json!("1")),
            ("Status", &read["Status"], json!("SUCCESS")),
            (
                "StatusDetails",
                &read["StatusDetails"],
                json!("status detail for SUCCESS"),
            ),
        ];
        for (member, reported, wanted) in expected {
            assert_eq!(
                reported, &wanted,
                "round {round}: expected {member} = {wanted}, got {read}"
            );
        }
        assert_eq!(
            read["StartTime"].as_f64(),
            Some(FIRST_START as f64),
            "round {round}: expected the instant the record holds, got {read}"
        );
        assert_eq!(
            read["EndTime"].as_f64(),
            Some((FIRST_START + 60) as f64),
            "round {round}: expected the end the record holds, got {read}"
        );
        // Nothing invented for members the record holds nothing for.
        assert!(
            read["TaskParameters"].is_null() && read["TriggeredAlarms"].is_null(),
            "round {round}: a record holding neither reports neither, got {read}"
        );

        let (status, read) = call(
            &router,
            "GetMaintenanceWindowExecutionTaskInvocation",
            &json!({
                "WindowExecutionId": RUN,
                "TaskId": TASK,
                "InvocationId": invocation_id,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "round {round}: the held invocation must be readable, got {status} {read}"
        );
        let expected: [(&str, &Value, Value); 9] = [
            ("WindowExecutionId", &read["WindowExecutionId"], json!(RUN)),
            ("TaskExecutionId", &read["TaskExecutionId"], json!(TASK)),
            ("InvocationId", &read["InvocationId"], json!(invocation_id)),
            (
                "ExecutionId",
                &read["ExecutionId"],
                json!("c1d2e3f4-5a6b-7c8d-9e0f-1a2b3c4d5e6f"),
            ),
            ("TaskType", &read["TaskType"], json!("RUN_COMMAND")),
            (
                "Parameters",
                &read["Parameters"],
                json!("{\"commands\":[\"echo hello\"]}"),
            ),
            ("Status", &read["Status"], json!("SUCCESS")),
            (
                "OwnerInformation",
                &read["OwnerInformation"],
                json!("patch wave 1"),
            ),
            (
                "WindowTargetId",
                &read["WindowTargetId"],
                json!("22222222-3333-4444-5555-666666666666"),
            ),
        ];
        for (member, reported, wanted) in expected {
            assert_eq!(
                reported, &wanted,
                "round {round}: expected {member} = {wanted}, got {read}"
            );
        }
    }

    // The listings report the same records, with the identity members of each.
    let (status, listed) = call(
        &router,
        "DescribeMaintenanceWindowExecutionTasks",
        &json!({ "WindowExecutionId": RUN }),
    )
    .await;
    assert_eq!(status, 200, "the listing must succeed, got {status} {listed}");
    assert_eq!(
        listed_tasks(&listed),
        vec![(TASK.to_string(), "SUCCESS".to_string())],
        "expected the held task execution, got {listed}"
    );
    let identity = &listed["WindowExecutionTaskIdentities"][0];
    assert_eq!(
        identity["TaskArn"],
        json!("AWS-RunShellScript"),
        "expected the ARN the record holds, got {identity}"
    );
    assert_eq!(
        identity["TaskType"],
        json!("RUN_COMMAND"),
        "expected the task type the record holds, got {identity}"
    );
    assert_eq!(
        identity["StartTime"].as_f64(),
        Some(FIRST_START as f64),
        "expected the instant the record holds, got {identity}"
    );

    let (status, listed) = call(
        &router,
        "DescribeMaintenanceWindowExecutionTaskInvocations",
        &json!({ "WindowExecutionId": RUN, "TaskId": TASK }),
    )
    .await;
    assert_eq!(status, 200, "the listing must succeed, got {status} {listed}");
    assert_eq!(
        listed_invocations(&listed),
        vec![(invocation_id.to_string(), "SUCCESS".to_string())],
        "expected the held invocation, got {listed}"
    );
    assert_eq!(
        listed["WindowExecutionTaskInvocationIdentities"][0]["WindowTargetId"],
        json!("22222222-3333-4444-5555-666666666666"),
        "expected the target the record holds, got {listed}"
    );
}

/// RULE: a listing walks the records of the id it names, in the order they started, bounded by
/// the page size asked for and resumed at the id it handed back. The rows differ along the page
/// size and how far into the records the page starts.
#[tokio::test]
async fn a_listing_pages_by_the_id_it_resumes_at() {
    let (router, engine) = router_with_engine();

    // One more record than the smallest page the modelled range admits, so a page leaves a
    // remainder — seeded out of order, so the reported order is the records' own.
    let held = MIN_PAGE + 1;
    for ordinal in (0..held).rev() {
        seed_task(
            &engine,
            REGION,
            RUN,
            &task_id(ordinal),
            "SUCCESS",
            FIRST_START + ordinal as u64 * 60,
        );
    }

    // (case, MaxResults, how many identities the page carries, whether it continues)
    let rows: [(&str, Option<usize>, usize, bool); 3] = [
        ("no page size asked for", None, held, false),
        ("the smallest page the range admits", Some(MIN_PAGE), MIN_PAGE, true),
        ("a page larger than the records held", Some(50), held, false),
    ];

    for (case, max_results, expected, continues) in rows {
        let mut body = json!({ "WindowExecutionId": RUN });
        if let Some(max_results) = max_results {
            body["MaxResults"] = json!(max_results);
        }
        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTasks",
            &body,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must succeed, got {status} {response}"
        );
        let listed = listed_tasks(&response);
        assert_eq!(
            listed.len(),
            expected,
            "case {case:?}: expected {expected} identities, got {response}"
        );
        let wanted: Vec<String> = (0..listed.len()).map(task_id).collect();
        let reported: Vec<String> = listed.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected the records in the order they started, got {reported:?}"
        );
        assert_eq!(
            !response["NextToken"].is_null(),
            continues,
            "case {case:?}: expected continues={continues}, got {response}"
        );

        // A page that continues resumes at the id it handed back, and the two pages together
        // are every record held, each once.
        if continues {
            let token = response["NextToken"].as_str().unwrap_or_default().to_string();
            assert_eq!(
                token,
                task_id(expected),
                "case {case:?}: expected the id the next page starts at, got {response}"
            );
            let (status, rest) = call(
                &router,
                "DescribeMaintenanceWindowExecutionTasks",
                &json!({ "WindowExecutionId": RUN, "NextToken": token }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the resumed page must succeed, got {status} {rest}"
            );
            let resumed: Vec<String> = listed_tasks(&rest)
                .iter()
                .map(|(id, _)| id.clone())
                .collect();
            assert_eq!(
                resumed,
                (expected..held).map(task_id).collect::<Vec<String>>(),
                "case {case:?}: expected the records past the page, got {resumed:?}"
            );
        }
    }

    // A token naming no record of this result is not one this read handed out.
    for (case, token) in [
        ("an id nothing recorded", UNRECORDED),
        ("a value of no shape at all", "not-a-token"),
    ] {
        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTasks",
            &json!({ "WindowExecutionId": RUN, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the token refusal, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the token refusal, got {response}"
        );
    }

    // A page size outside the modelled range never reaches the read.
    for (case, max_results) in [("one below the minimum", 9), ("one above the maximum", 101)] {
        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTasks",
            &json!({ "WindowExecutionId": RUN, "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {max_results} is outside the modelled range, got {status} \
             {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the boundary refusal, got {response}"
        );
    }
}

/// RULE: the one documented filter key narrows a listing by the status the records hold, and a
/// filter's values UNION; a key outside the documented set names no property of a record, so
/// the request is refused rather than served everything. The rows differ along the statuses
/// filtered for and how many values the filter carries.
///
/// The statuses are the modelled `MaintenanceWindowExecutionStatus` values the two listings
/// document for this key (`smithy_query(query="doc",
/// shape="DescribeMaintenanceWindowExecutionTasksRequest", member="Filters")`: PENDING,
/// IN_PROGRESS, SUCCESS, FAILED, TIMED_OUT, CANCELLING, CANCELLED) — including TIMED_OUT, which
/// no recorded unit asked for.
#[tokio::test]
async fn a_listing_narrows_by_the_one_documented_filter_key() {
    let (router, engine) = router_with_engine();

    // (task execution ordinal, status) — one record per status, so a filter selects a known
    // subset.
    let seeded: [(usize, &str); 4] = [
        (0, "SUCCESS"),
        (1, "FAILED"),
        (2, "TIMED_OUT"),
        (3, "SUCCESS"),
    ];
    for (ordinal, status) in seeded {
        seed_task(
            &engine,
            REGION,
            RUN,
            &task_id(ordinal),
            status,
            FIRST_START + ordinal as u64 * 60,
        );
        seed_invocation(
            &engine,
            REGION,
            RUN,
            TASK,
            &task_id(ordinal),
            status,
            FIRST_START + ordinal as u64 * 60,
        );
    }

    // (case, the statuses filtered for, the ordinals selected)
    let rows: [(&str, &[&str], &[usize]); 5] = [
        ("one status two records hold", &["SUCCESS"], &[0, 3]),
        ("one status one record holds", &["FAILED"], &[1]),
        (
            "a status no recorded unit asked for",
            &["TIMED_OUT"],
            &[2],
        ),
        (
            "two statuses, which union",
            &["FAILED", "TIMED_OUT"],
            &[1, 2],
        ),
        ("a status no record holds", &["CANCELLED"], &[]),
    ];

    for (case, statuses, selected) in rows {
        let filters = json!([{ "Key": "STATUS", "Values": statuses }]);
        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTasks",
            &json!({ "WindowExecutionId": RUN, "Filters": filters }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must succeed, got {status} {response}"
        );
        let reported: Vec<String> = listed_tasks(&response)
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(
            reported,
            selected.iter().copied().map(task_id).collect::<Vec<String>>(),
            "case {case:?}: expected the records holding {statuses:?}, got {reported:?}"
        );

        let (status, response) = call(
            &router,
            "DescribeMaintenanceWindowExecutionTaskInvocations",
            &json!({ "WindowExecutionId": RUN, "TaskId": TASK, "Filters": filters }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation listing must succeed, got {status} {response}"
        );
        let reported: Vec<String> = listed_invocations(&response)
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(
            reported,
            selected.iter().copied().map(task_id).collect::<Vec<String>>(),
            "case {case:?}: expected the invocations holding {statuses:?}, got {reported:?}"
        );
    }

    // A key outside the documented set names no property of these records.
    for (case, key) in [
        ("the id of the record itself", "TaskExecutionId"),
        ("a key of another listing of this service", "TaskArn"),
        ("the documented key in another case", "Status"),
    ] {
        let filters = json!([{ "Key": key, "Values": ["SUCCESS"] }]);
        // Each listing is asked with the members IT declares, so the refusal is the filter's
        // and nothing else's.
        let requests = [
            (
                "DescribeMaintenanceWindowExecutionTasks",
                json!({ "WindowExecutionId": RUN, "Filters": filters }),
            ),
            (
                "DescribeMaintenanceWindowExecutionTaskInvocations",
                json!({ "WindowExecutionId": RUN, "TaskId": TASK, "Filters": filters }),
            ),
        ];
        for (operation, body) in requests {
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: {key} narrows nothing in {operation}, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected the filter refusal from {operation}, got {response}"
            );
            assert!(
                error_message(&response).contains(key),
                "case {case:?}: the refusal names the key it refused, got {response}"
            );
        }
    }
}
