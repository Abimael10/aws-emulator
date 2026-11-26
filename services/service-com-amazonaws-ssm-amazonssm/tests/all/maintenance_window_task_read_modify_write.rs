// SPDX-License-Identifier: CC0-1.0

//! The READ of a registered maintenance window task as a COMPLETE description of it: what
//! `GetMaintenanceWindowTask` reports is everything a client needs to re-submit the task
//! unchanged, so feeding the read's own projection back through
//! `UpdateMaintenanceWindowTask` with `Replace` leaves the task exactly as it was.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **A read reports a value for every member the service materialises for the task.** The
//!    identity the task runs as ("if you do not specify a service role ARN, Systems Manager
//!    uses a service-linked role in your account", `MaintenanceWindowTask$ServiceRoleArn`) and
//!    the parameter map the task carries are materialised for every registered task, so a read
//!    reports both whatever the register stated — the role as a resolved ARN and the map
//!    present, empty where the task holds no parameters. A role is an IAM resource and IAM is
//!    global, so the resolved ARN is the same at every Region's endpoint.
//! 2. **The read ROUND-TRIPS.** A read-modify-write that re-submits the members the read
//!    reported through a replacing update persists the same task: the update answers the same
//!    role and map, and a later read reports the same projection, member for member. A member
//!    a read left out is one a replacement clears to the same value, so the projection is
//!    stable under feedback rather than losing the role binding.
//! 3. **A read changes nothing.** Two reads of one task, with no write between them, report
//!    the same body.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::maintenance_window_task_service_role::service_linked_role_arn;
use crate::parameter_store::{call_in_region, router, REGION};

/// A second Region, so the rules are shown to hold at whichever endpoint a task is registered
/// through — and the resolved role ARN, being IAM's and so global, is shown to carry no Region.
const OTHER_REGION: &str = "eu-central-1";

/// The members `UpdateMaintenanceWindowTask` takes that a read of the same task also reports,
/// which is therefore exactly what a read-modify-write feeds back
/// (`smithy_query(query="operation",operation="UpdateMaintenanceWindowTask")`: its input carries
/// these beside `WindowId`, `WindowTaskId` and `Replace`, and nothing for the task's type,
/// which "you can't change").
const FED_BACK_MEMBERS: &[&str] = &[
    "Targets",
    "TaskArn",
    "ServiceRoleArn",
    "TaskParameters",
    "TaskInvocationParameters",
    "Priority",
    "MaxConcurrency",
    "MaxErrors",
    "LoggingInfo",
    "Name",
    "Description",
    "CutoffBehavior",
    "AlarmConfiguration",
];

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
        "case {case:?}: the window every task of this case belongs to must be created, got \
         {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// Register `body`'s task with `window_id` at `region`'s endpoint and answer the id minted.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    body: &Value,
) -> String {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    let (status, response) =
        call_in_region(router, region, "RegisterTaskWithMaintenanceWindow", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the register {request} must succeed, got {status} {response}"
    );
    response["WindowTaskId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a register reports a WindowTaskId, got {response}")
        })
        .to_string()
}

/// `GetMaintenanceWindowTask` for one task at `region`'s endpoint, asserting it was served.
async fn read(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    window_task_id: &str,
) -> Value {
    let (status, response) = call_in_region(
        router,
        region,
        "GetMaintenanceWindowTask",
        &json!({ "WindowId": window_id, "WindowTaskId": window_task_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read of {window_task_id:?} must succeed, got {status} {response}"
    );
    response
}

/// Re-submit the task through a REPLACING update carrying every member `reported` reported that
/// the update takes — the read-modify-write a client performs — and answer what it answered.
async fn feed_back(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    window_task_id: &str,
    reported: &Value,
) -> Value {
    let mut request = json!({
        "WindowId": window_id,
        "WindowTaskId": window_task_id,
        "Replace": true,
    });
    for member in FED_BACK_MEMBERS {
        if let Some(value) = reported.get(*member) {
            request[*member] = value.clone();
        }
    }
    let (status, response) =
        call_in_region(router, region, "UpdateMaintenanceWindowTask", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read-modify-write {request} must be served, got {status} {response}"
    );
    response
}

/// One row of the table: what a failing assertion calls it, the members the register states
/// beside the window id, the role every read must then report, and the parameter map it must.
struct Row {
    why: &'static str,
    registration: Value,
    role: String,
    parameters: Value,
}

/// The registrations the rules are asserted over, differing along what they generalise over:
/// whether the register names a role at all and which account's role it names, whether it
/// states a parameter map and how many entries and values that map holds, whether the task has
/// targets and of which kind, which task type it is, and which of the optional members
/// (`Name`, `Description`, `LoggingInfo`, `AlarmConfiguration`, `TaskInvocationParameters`,
/// the two bounds, the priority and the cutoff behaviour) it carries.
fn rows() -> Vec<Row> {
    let linked = service_linked_role_arn();
    vec![
        Row {
            why: "a targetless task stating neither a role nor a map",
            registration: json!({
                "TaskType": "RUN_COMMAND",
                "TaskArn": "AWS-RunShellScript",
            }),
            role: linked.clone(),
            parameters: json!({}),
        },
        Row {
            why: "a node-targeted task stating neither a role nor a map",
            registration: json!({
                "TaskType": "RUN_COMMAND",
                "TaskArn": "AWS-RunPatchBaseline",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "MaxConcurrency": "2",
                "MaxErrors": "1",
            }),
            role: linked.clone(),
            parameters: json!({}),
        },
        Row {
            why: "a tag-targeted task stating the empty map and no role",
            registration: json!({
                "TaskType": "AUTOMATION",
                "TaskArn": "AWS-RestartEC2Instance",
                "Targets": [{ "Key": "tag:Environment", "Values": ["production", "staging"] }],
                "MaxConcurrency": "50%",
                "MaxErrors": "0",
                "TaskParameters": {},
                "Priority": 3,
                "CutoffBehavior": "CANCEL_TASK",
                "Name": "restart-the-fleet",
                "Description": "the weekly restart",
            }),
            role: linked.clone(),
            parameters: json!({}),
        },
        Row {
            why: "a task stating a role of this account and a one-entry map",
            registration: json!({
                "TaskType": "RUN_COMMAND",
                "TaskArn": "AWS-RunPatchBaseline",
                "Targets": [{ "Key": "tag-key", "Values": ["Patch"] }],
                "MaxConcurrency": "1",
                "MaxErrors": "1",
                "ServiceRoleArn": "arn:aws:iam::123456789012:role/mw-task-role",
                "TaskParameters": { "Operation": { "Values": ["Install"] } },
                "TaskInvocationParameters": {
                    "RunCommand": { "Comment": "the weekly run", "TimeoutSeconds": 600 },
                },
            }),
            role: "arn:aws:iam::123456789012:role/mw-task-role".to_string(),
            parameters: json!({ "Operation": { "Values": ["Install"] } }),
        },
        Row {
            why: "a task stating a role carrying a path and a two-entry map, one multi-valued",
            registration: json!({
                "TaskType": "RUN_COMMAND",
                "TaskArn": "AWS-RunShellScript",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "MaxConcurrency": "100%",
                "MaxErrors": "100%",
                "ServiceRoleArn": "arn:aws:iam::123456789012:role/service-role/mw-task-role",
                "TaskParameters": {
                    "Operation": { "Values": ["Install", "Scan"] },
                    "RebootOption": { "Values": ["NoReboot"] },
                },
                "LoggingInfo": {
                    "S3BucketName": "maintenance-logs",
                    "S3KeyPrefix": "window/tasks",
                    "S3Region": "us-west-2",
                },
                "AlarmConfiguration": {
                    "IgnorePollAlarmFailure": true,
                    "Alarms": [{ "Name": "maintenance-window-alarm" }],
                },
            }),
            role: "arn:aws:iam::123456789012:role/service-role/mw-task-role".to_string(),
            parameters: json!({
                "Operation": { "Values": ["Install", "Scan"] },
                "RebootOption": { "Values": ["NoReboot"] },
            }),
        },
        Row {
            why: "a targetless LAMBDA task stating a role of another account",
            registration: json!({
                "TaskType": "LAMBDA",
                "TaskArn": "arn:aws:lambda:us-west-2:123456789012:function:maintenance",
                "Targets": [],
                "ServiceRoleArn": "arn:aws:iam::210987654321:role/shared-mw-task-role",
                "TaskInvocationParameters": {
                    "Lambda": { "ClientContext": "eyJrIjoidiJ9" },
                },
                "Priority": 10,
            }),
            role: "arn:aws:iam::210987654321:role/shared-mw-task-role".to_string(),
            parameters: json!({}),
        },
    ]
}

/// RULES 1 and 3: every read of a registered task reports the resolved role it runs as and its
/// parameter map — present, and empty where the task holds none — at whichever Region's
/// endpoint the task was registered through, and reading it again reports the same body.
#[tokio::test]
async fn a_read_reports_the_resolved_role_and_the_parameter_map() {
    let router = router();
    for region in [REGION, OTHER_REGION] {
        let window_id = window(&router, region, region, "task-read-projection").await;
        for row in rows() {
            let why = format!("{} at {region}", row.why);
            let id = register(&router, &why, region, &window_id, &row.registration).await;
            let reported = read(&router, &why, region, &window_id, &id).await;

            assert_eq!(
                reported["ServiceRoleArn"].as_str(),
                Some(row.role.as_str()),
                "case {why:?}: expected the read to report ServiceRoleArn {:?}, got {reported}",
                row.role
            );
            assert!(
                reported["TaskParameters"].is_object(),
                "case {why:?}: expected the read to report TaskParameters as a map, got \
                 {reported}"
            );
            assert_eq!(
                reported["TaskParameters"], row.parameters,
                "case {why:?}: expected the read to report TaskParameters {}, got {reported}",
                row.parameters
            );

            // RULE 3: the read reports, so a second read with no write between them answers
            // the same body.
            let again = read(&router, &why, region, &window_id, &id).await;
            assert_eq!(
                again, reported,
                "case {why:?}: expected a second read to report {reported}, got {again}"
            );
        }
    }
}

/// RULE 2: the read's own projection, fed back through a REPLACING update, persists the same
/// task — so a read-modify-write keeps the identity the task runs as and the parameters it
/// passes instead of silently dropping them.
#[tokio::test]
async fn a_read_modify_write_persists_the_same_task() {
    let router = router();
    for region in [REGION, OTHER_REGION] {
        let window_id = window(&router, region, region, "task-round-trip").await;
        for row in rows() {
            let why = format!("{} at {region}", row.why);
            let id = register(&router, &why, region, &window_id, &row.registration).await;
            let before = read(&router, &why, region, &window_id, &id).await;

            let answered = feed_back(&router, &why, region, &window_id, &id, &before).await;
            assert_eq!(
                answered["ServiceRoleArn"].as_str(),
                Some(row.role.as_str()),
                "case {why:?}: expected the read-modify-write to answer ServiceRoleArn {:?}, \
                 got {answered}",
                row.role
            );
            assert_eq!(
                answered["TaskParameters"], row.parameters,
                "case {why:?}: expected the read-modify-write to answer TaskParameters {}, got \
                 {answered}",
                row.parameters
            );

            // Member for member, the task a later read reports is the task the first read
            // described: the projection is stable under its own feedback.
            let after = read(&router, &why, region, &window_id, &id).await;
            assert_eq!(
                after, before,
                "case {why:?}: expected the read after the read-modify-write to report \
                 {before}, got {after}"
            );
        }
    }
}
