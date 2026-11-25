// SPDX-License-Identifier: CC0-1.0

//! `DeregisterTaskFromMaintenanceWindow` end to end, and the orderly teardown it unblocks:
//! once the task that references a registered target is gone, the SAFE deregistration of
//! that target is served.
//!
//! The rules under test, each over a TABLE whose rows differ along what the rule generalises
//! over rather than over one recorded request:
//!
//! 1. **A deregistration removes the task.** The call answers 200 echoing the `WindowId` and
//!    `WindowTaskId` it was given, and the window's own reads — a describe of its tasks and a
//!    get of that task — no longer report it, while the tasks beside it are untouched.
//! 2. **A removal asks for a STATE, so it converges.** A `WindowTaskId` an existing window
//!    holds no task for — one a removal already took, one belonging to a SIBLING window, one
//!    nothing ever minted — is that state: the call answers 200 with the ids the request
//!    carried and writes nothing, however many times it is repeated, and the sibling window
//!    keeps its own task. The WINDOW, on the other hand, is a resource the request addresses:
//!    an id this endpoint holds no window for is refused `DoesNotExistException`.
//! 3. **Deregistering the task releases the target it referenced.** A target a task names by
//!    `WindowTargetIds` is refused `TargetInUseException` while that task stands; after the
//!    task is deregistered the same safe deregistration is served 200 echoing `WindowId` and
//!    `WindowTargetId`, and the target is gone.
//!
//! Every call goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — and every task a row removes was registered by a
//! `RegisterTaskWithMaintenanceWindow` of this suite and read back through
//! `DescribeMaintenanceWindowTasks`, so nothing here can pass on a response that is not
//! computed from persisted state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a window and its tasks are shown to live at the endpoint of ONE
/// Region.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// A `WindowTaskId` of the 8-4-4-4-12 hexadecimal form the modelled
/// `MaintenanceWindowTaskId` pattern requires that no register has minted.
const UNKNOWN_TASK_ID: &str = "00000000-0000-0000-0000-000000000000";

/// How many times a row repeats one removal: more than twice, because the rule is about the
/// state the call asks for and not about the second call.
const REPEATS: usize = 4;

/// Create a window at `region`'s endpoint and answer the id it minted.
async fn window(router: &MultiServiceRouter, case: &str, region: &str, name: &str) -> String {
    let (status, response) = call_in_region(
        router,
        region,
        "CreateMaintenanceWindow",
        &json!({
            "Name": name,
            "Schedule": "cron(0 3 ? * SUN *)",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the window must be created first, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// Register one task with `window_id`, answering the `WindowTaskId` it minted. `targets` is
/// the task's `Targets` criteria, which a row uses to name a registered target.
async fn task(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    targets: Value,
) -> String {
    let (status, response) = call_in_region(
        router,
        region,
        "RegisterTaskWithMaintenanceWindow",
        &json!({
            "WindowId": window_id,
            "TaskType": "RUN_COMMAND",
            "TaskArn": "AWS-RunShellScript",
            "MaxConcurrency": "1",
            "MaxErrors": "1",
            "Targets": targets,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the task must be registered first, got {status} {response}"
    );
    response["WindowTaskId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a register reports a WindowTaskId, got {response}")
        })
        .to_string()
}

/// Register one target with `window_id` and answer the `WindowTargetId` it minted.
async fn target(router: &MultiServiceRouter, case: &str, window_id: &str) -> String {
    let (status, response) = call_in_region(
        router,
        REGION,
        "RegisterTargetWithMaintenanceWindow",
        &json!({
            "WindowId": window_id,
            "ResourceType": "INSTANCE",
            "Targets": [{ "Key": "tag:Name", "Values": ["probe"] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the target must be registered first, got {status} {response}"
    );
    response["WindowTargetId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a register reports a WindowTargetId, got {response}")
        })
        .to_string()
}

/// The `WindowTaskId`s `window_id` holds at `region`'s endpoint, in the order the describe
/// reports them.
async fn held_tasks(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
) -> Vec<String> {
    let (status, response) = call_in_region(
        router,
        region,
        "DescribeMaintenanceWindowTasks",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must be served, got {status} {response}"
    );
    response["Tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|held| held["WindowTaskId"].as_str().map(str::to_string))
        .collect()
}

/// The `WindowTargetId`s `window_id` holds, in the order the describe reports them.
async fn held_targets(router: &MultiServiceRouter, case: &str, window_id: &str) -> Vec<String> {
    let (status, response) = call_in_region(
        router,
        REGION,
        "DescribeMaintenanceWindowTargets",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must be served, got {status} {response}"
    );
    response["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|held| held["WindowTargetId"].as_str().map(str::to_string))
        .collect()
}

/// `DeregisterTaskFromMaintenanceWindow` at `region`'s endpoint.
async fn deregister_task(
    router: &MultiServiceRouter,
    region: &str,
    window_id: &str,
    window_task_id: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "DeregisterTaskFromMaintenanceWindow",
        &json!({ "WindowId": window_id, "WindowTaskId": window_task_id }),
    )
    .await
}

/// `DeregisterTargetFromMaintenanceWindow` at [`REGION`]'s endpoint, with `body`.
async fn deregister_target(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, "DeregisterTargetFromMaintenanceWindow", body).await
}

/// Assert a deregistration was served and echoed the two ids the request carried.
fn echoes(case: &str, status: u16, response: &Value, window_id: &str, window_task_id: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected the deregistration to be served, got {status} {response}"
    );
    assert_eq!(
        (
            response["WindowId"].as_str(),
            response["WindowTaskId"].as_str()
        ),
        (Some(window_id), Some(window_task_id)),
        "case {case:?}: expected the ids the request carried, got {response}"
    );
}

/// RULE 1: the deregistration removes the task the request names and nothing else, and the
/// window's own reads report that. The rows differ along how many tasks the window holds and
/// which of them is removed.
#[tokio::test]
async fn a_deregistration_removes_the_task_it_names() {
    let router = router();
    let case = "the removal case";
    let window_id = window(&router, case, REGION, "task-removal-window").await;

    // Three tasks of one window, so a row removes one of several rather than the only one.
    let mut registered: Vec<String> = Vec::new();
    for nth in 0..3 {
        registered.push(
            task(
                &router,
                case,
                REGION,
                &window_id,
                json!([{ "Key": "InstanceIds", "Values": [format!("i-00000000000000{nth:03}")] }]),
            )
            .await,
        );
    }
    let mut remaining = registered.clone();
    remaining.sort();
    let mut held = held_tasks(&router, case, REGION, &window_id).await;
    held.sort();
    assert_eq!(
        held, remaining,
        "case {case:?}: expected the three registered tasks, got {held:?}"
    );

    // (case, which of the registered tasks the row removes)
    let rows: [(&str, usize); 3] = [
        ("the task registered first", 0),
        ("the task registered last", 2),
        ("the one task left", 1),
    ];
    for (why, nth) in rows {
        let window_task_id = registered[nth].clone();
        let (status, response) = deregister_task(&router, REGION, &window_id, &window_task_id).await;
        echoes(why, status, &response, &window_id, &window_task_id);

        remaining.retain(|held| *held != window_task_id);
        let mut held = held_tasks(&router, why, REGION, &window_id).await;
        held.sort();
        assert_eq!(
            held, remaining,
            "case {why:?}: expected the tasks beside it to be untouched, got {held:?}"
        );

        // The single read of that task agrees with the listing: the window holds no such
        // task any more.
        let (status, read) = call_in_region(
            &router,
            REGION,
            "GetMaintenanceWindowTask",
            &json!({ "WindowId": window_id, "WindowTaskId": window_task_id }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the removed task to be unreadable, got {status} {read}"
        );
    }
    assert!(
        held_tasks(&router, case, REGION, &window_id).await.is_empty(),
        "case {case:?}: every registered task was removed"
    );
}

/// RULE 2: a `WindowTaskId` the window holds no task for is the state the call asks for, so
/// it is served and writes nothing; a `WindowId` this endpoint holds no window for is the
/// resource refusal. The rows differ along the way the task id misses and the way the window
/// id does.
#[tokio::test]
async fn a_task_the_window_holds_none_of_is_the_state_the_call_asks_for() {
    let router = router();
    let case = "the convergence case";
    let window_id = window(&router, case, REGION, "convergent-window").await;
    let sibling_id = window(&router, case, REGION, "sibling-window").await;
    let elsewhere_id = window(&router, case, OTHER_REGION, "window-of-another-region").await;
    let nodes = json!([{ "Key": "InstanceIds", "Values": ["i-00000000000000001"] }]);
    let removed = task(&router, case, REGION, &window_id, nodes.clone()).await;
    let sibling_task = task(&router, case, REGION, &sibling_id, nodes.clone()).await;
    let elsewhere_task = task(&router, case, OTHER_REGION, &elsewhere_id, nodes).await;

    // (case, the task id the removal names)
    let rows: [(&str, &str); 3] = [
        ("a task a removal already took", &removed),
        ("a task of a sibling window", &sibling_task),
        ("an id nothing ever minted", UNKNOWN_TASK_ID),
    ];
    for (why, window_task_id) in rows {
        // The first row's task is still registered, so the first call of it is the removal
        // and every call after it asks for a state already reached. The other rows are
        // already in that state.
        for repeat in 0..REPEATS {
            let (status, response) =
                deregister_task(&router, REGION, &window_id, window_task_id).await;
            echoes(
                &format!("{why} (call {repeat})"),
                status,
                &response,
                &window_id,
                window_task_id,
            );
        }
        assert!(
            held_tasks(&router, why, REGION, &window_id).await.is_empty(),
            "case {why:?}: the addressed window holds no task either way"
        );
    }

    // Nothing was removed from the windows the request did not address: a task id names a
    // task of ONE window, and of one Region.
    assert_eq!(
        held_tasks(&router, case, REGION, &sibling_id).await,
        vec![sibling_task.clone()],
        "case {case:?}: the sibling window keeps its own task"
    );
    assert_eq!(
        held_tasks(&router, case, OTHER_REGION, &elsewhere_id).await,
        vec![elsewhere_task.clone()],
        "case {case:?}: the other Region's window keeps its own task"
    );

    // (case, the Region addressed, the window id addressed)
    let refused: [(&str, &str, &str); 3] = [
        ("an id nothing created", REGION, UNKNOWN_WINDOW_ID),
        ("a window of another Region", REGION, &elsewhere_id),
        ("an id nothing created, elsewhere", OTHER_REGION, UNKNOWN_WINDOW_ID),
    ];
    for (why, region, addressed) in refused {
        let (status, response) = deregister_task(&router, region, addressed, &elsewhere_task).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the window refusal, got {status} {response}"
        );
        assert!(
            error_message(&response).contains(addressed),
            "case {why:?}: expected the refusal to name the window asked for, got {response}"
        );
    }
    // The refusals wrote nothing: the window of the other Region still holds its task.
    assert_eq!(
        held_tasks(&router, case, OTHER_REGION, &elsewhere_id).await,
        vec![elsewhere_task],
        "case {case:?}: a refused removal writes nothing"
    );
}

/// RULE 3: the safe deregistration of a target a task references is refused while that task
/// stands and served once it is deregistered — the orderly teardown. The rows differ along
/// how many tasks reference the target, so the target is released only when the LAST of them
/// is gone.
#[tokio::test]
async fn deregistering_the_task_releases_the_target_it_referenced() {
    let router = router();
    let case = "the teardown case";
    let window_id = window(&router, case, REGION, "teardown-window").await;
    let window_target_id = target(&router, case, &window_id).await;
    let referencing = json!([{ "Key": "WindowTargetIds", "Values": [window_target_id.clone()] }]);

    // Two tasks name the same registered target, so the removal of one leaves the interlock
    // standing and only the second releases it.
    let first = task(&router, case, REGION, &window_id, referencing.clone()).await;
    let second = task(&router, case, REGION, &window_id, referencing).await;
    let removal = json!({ "WindowId": window_id, "WindowTargetId": window_target_id });

    // (case, the task deregistered before the target removal is tried, which task the
    // refusal must name afterwards)
    let rows: [(&str, Option<&str>, Option<&str>); 3] = [
        ("no task deregistered yet", None, Some(&first)),
        ("the first of the two gone", Some(&first), Some(&second)),
        ("the last of the two gone", Some(&second), None),
    ];
    for (why, deregistered, still_referenced) in rows {
        if let Some(window_task_id) = deregistered {
            let (status, response) =
                deregister_task(&router, REGION, &window_id, window_task_id).await;
            echoes(why, status, &response, &window_id, window_task_id);
        }

        let (status, response) = deregister_target(&router, &removal).await;
        match still_referenced {
            Some(window_task_id) => {
                assert_eq!(
                    (status, error_code(&response).as_str()),
                    (400, "TargetInUseException"),
                    "case {why:?}: expected the target to be held, got {status} {response}"
                );
                assert_eq!(
                    error_message(&response),
                    format!(
                        "This Target cannot be deregistered because it is still referenced in \
                         Task: {window_task_id}"
                    ),
                    "case {why:?}: expected the refusal to name the referencing task, got \
                     {response}"
                );
                assert!(
                    held_targets(&router, why, &window_id)
                        .await
                        .contains(&window_target_id),
                    "case {why:?}: a refused removal leaves the target registered"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {why:?}: expected the orderly removal to be served, got {status} \
                     {response}"
                );
                assert_eq!(
                    (
                        response["WindowId"].as_str(),
                        response["WindowTargetId"].as_str()
                    ),
                    (Some(window_id.as_str()), Some(window_target_id.as_str())),
                    "case {why:?}: expected the ids the request carried, got {response}"
                );
                assert!(
                    !held_targets(&router, why, &window_id)
                        .await
                        .contains(&window_target_id),
                    "case {why:?}: the deregistered target is gone"
                );
            }
        }
    }
}
