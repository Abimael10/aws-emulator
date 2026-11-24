// SPDX-License-Identifier: CC0-1.0

//! The identity a maintenance window task RUNS AS, and the parameter map every registered task
//! carries, across the three projections of one task: the register's own read
//! (`GetMaintenanceWindowTask`), the window's listing (`DescribeMaintenanceWindowTasks`) and the
//! edit's own answer (`UpdateMaintenanceWindowTask`).
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response that
//! is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **A registered task always names the role it runs as.** "If you do not specify a service
//!    role ARN, Systems Manager uses a service-linked role in your account"
//!    (`MaintenanceWindowTask$ServiceRoleArn`), so a task whose request named a role runs as
//!    that role and a task whose request named none runs as the account's service-linked role —
//!    and every projection of the task reports it.
//! 2. **That service-linked role is one IAM holds.** The ARN the projections report is the ARN
//!    IAM answers for that role, so a caller reading the role back resolves it.
//! 3. **A registered task always carries its parameter map.** `TaskParameters` is a map the
//!    service materialises for the task, so every projection reports it — present and EMPTY for
//!    a task that holds no parameters, rather than left out.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, router_with_engine, ACCOUNT, REGION};

/// The name IAM publishes for the service-linked role of the Systems Manager service principal
/// `ssm.amazonaws.com` ("Using service-linked roles for Systems Manager"; the same pairing the
/// emulated IAM service's own published registry holds), under the path IAM reserves for such a
/// role (`/aws-service-role/<service principal>/`). A role is an IAM resource and IAM is
/// global, so the ARN names IAM and carries no Region.
pub(crate) const SERVICE_LINKED_ROLE_NAME: &str = "AWSServiceRoleForAmazonSSM";

/// The ARN of [`SERVICE_LINKED_ROLE_NAME`] in the account every emulator ARN carries. Only the
/// account-id segment varies.
///
/// The suites about the register, the listing and the update read the role a task that named
/// none runs as through this one spelling of it.
pub(crate) fn service_linked_role_arn() -> String {
    format!(
        "arn:aws:iam::{ACCOUNT}:role/aws-service-role/ssm.amazonaws.com/{SERVICE_LINKED_ROLE_NAME}"
    )
}

/// A router with this service and IAM mounted on ONE engine, so the role the task resolves to
/// can be read back through IAM's own front door.
fn router_with_iam() -> MultiServiceRouter {
    let (mut router, engine) = router_with_engine();
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, role_name: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(format!(
            "Action={action}&Version=2010-05-08&RoleName={role_name}"
        )))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, body)
}

/// Create a maintenance window and answer the id it minted.
async fn window(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, response) = call_in_region(
        router,
        REGION,
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

/// Register a task of `window_id` with `body`'s members and answer the id minted for it.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    body: &Value,
) -> String {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    request["TaskType"] = json!("RUN_COMMAND");
    request["TaskArn"] = json!("AWS-RunShellScript");
    let (status, response) =
        call_in_region(router, REGION, "RegisterTaskWithMaintenanceWindow", &request).await;
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

/// `GetMaintenanceWindowTask` for one task, asserting it was served.
async fn read(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    window_task_id: &str,
) -> Value {
    let (status, response) = call_in_region(
        router,
        REGION,
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

/// The entry `DescribeMaintenanceWindowTasks` reports for one task of `window_id`.
async fn listed(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    window_task_id: &str,
) -> Value {
    let (status, response) = call_in_region(
        router,
        REGION,
        "DescribeMaintenanceWindowTasks",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing of {window_id:?}'s tasks must succeed, got {status} \
         {response}"
    );
    response["Tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|task| task["WindowTaskId"].as_str() == Some(window_task_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must hold {window_task_id:?}, got {response}")
        })
}

/// `UpdateMaintenanceWindowTask` for one task with `body`'s members, asserting it was served.
async fn update(
    router: &MultiServiceRouter,
    case: &str,
    window_id: &str,
    window_task_id: &str,
    body: &Value,
) -> Value {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    request["WindowTaskId"] = Value::String(window_task_id.to_string());
    let (status, response) =
        call_in_region(router, REGION, "UpdateMaintenanceWindowTask", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update {request} must be served, got {status} {response}"
    );
    response
}

/// RULE 1: every projection of a registered task names the role it runs as — the role the
/// request stated, or the account's service-linked role where it stated none.
///
/// The rows differ along what the rule generalises over: whether the register names a role at
/// all, and which form the named role takes (a role of this account, one carrying a path, one
/// of another account — `ServiceRole` carries no modelled length or pattern, so the value is
/// held as stated).
#[tokio::test]
async fn every_projection_of_a_task_names_the_role_it_runs_as() {
    let router = router_with_iam();
    let case = "the role table";
    let window_id = window(&router, case, "task-service-roles").await;
    let linked = service_linked_role_arn();

    // (case, the role the register states, the role every projection must report)
    let rows: Vec<(&str, Option<&str>, String)> = vec![
        ("a register naming no role at all", None, linked.clone()),
        (
            "a role of this account",
            Some("arn:aws:iam::123456789012:role/mw-task-role"),
            "arn:aws:iam::123456789012:role/mw-task-role".to_string(),
        ),
        (
            "a role carrying a path",
            Some("arn:aws:iam::123456789012:role/service-role/mw-task-role"),
            "arn:aws:iam::123456789012:role/service-role/mw-task-role".to_string(),
        ),
        (
            "a role of another account",
            Some("arn:aws:iam::210987654321:role/shared-mw-task-role"),
            "arn:aws:iam::210987654321:role/shared-mw-task-role".to_string(),
        ),
    ];

    for (why, stated, expected) in rows {
        let mut body = json!({
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "MaxConcurrency": "1",
            "MaxErrors": "1",
        });
        if let Some(role) = stated {
            body["ServiceRoleArn"] = json!(role);
        }
        let id = register(&router, why, &window_id, &body).await;

        for (projection, reported) in [
            ("the read", read(&router, why, &window_id, &id).await),
            ("the listing", listed(&router, why, &window_id, &id).await),
            (
                "the update's own answer",
                update(&router, why, &window_id, &id, &json!({ "Priority": 4 })).await,
            ),
        ] {
            assert_eq!(
                reported["ServiceRoleArn"].as_str(),
                Some(expected.as_str()),
                "case {why:?}: expected {projection} to report ServiceRoleArn \
                 {expected:?}, got {reported}"
            );
        }
    }

    // RULE 2: the role a task that named none runs as is a role IAM holds — the ARN reported is
    // the one IAM's own read answers for it, so a caller that reads the role back resolves it.
    let (status, held) = iam(&router, "GetRole", SERVICE_LINKED_ROLE_NAME).await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM must report the service-linked role a task runs as, got {status} \
         {held}"
    );
    assert!(
        held.contains(&linked),
        "case {case:?}: expected IAM to report the role under {linked:?}, got {held}"
    );
}

/// RULE 1, on the UPDATE: an update that states a role rewrites which identity the task runs
/// as, one that states none keeps it, and a REPLACING update that states none leaves the task
/// running as the account's service-linked role — "optional fields that aren't specified are
/// set to null", and a task's role is never nothing.
///
/// The rows differ along which role the register stated, whether the update states one, and
/// whether the update replaces the task.
#[tokio::test]
async fn an_update_resolves_the_role_the_task_runs_as() {
    let router = router_with_iam();
    let case = "the update table";
    let window_id = window(&router, case, "task-role-updates").await;
    let linked = service_linked_role_arn();
    let registered = "arn:aws:iam::123456789012:role/mw-task-role";

    // (case, the role the register states, the update's members, the role the task then runs as)
    let rows: Vec<(&str, Option<&str>, Value, String)> = vec![
        (
            "a patch naming another role",
            Some(registered),
            json!({ "ServiceRoleArn": "arn:aws:iam::123456789012:role/another-task-role" }),
            "arn:aws:iam::123456789012:role/another-task-role".to_string(),
        ),
        (
            "a patch naming no role, which modifies nothing",
            Some(registered),
            json!({ "Priority": 2 }),
            registered.to_string(),
        ),
        (
            "a patch naming no role on a task that named none either",
            None,
            json!({ "Priority": 2 }),
            linked.clone(),
        ),
        (
            "a replacement naming no role, which clears what was stated",
            Some(registered),
            json!({ "Replace": true, "TaskArn": "AWS-RunPatchBaseline" }),
            linked.clone(),
        ),
        (
            "a replacement naming a role",
            None,
            json!({
                "Replace": true,
                "TaskArn": "AWS-RunPatchBaseline",
                "ServiceRoleArn": "arn:aws:iam::123456789012:role/replacing-role",
            }),
            "arn:aws:iam::123456789012:role/replacing-role".to_string(),
        ),
    ];

    for (why, stated, body, expected) in rows {
        let mut registration = json!({
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "MaxConcurrency": "1",
            "MaxErrors": "1",
        });
        if let Some(role) = stated {
            registration["ServiceRoleArn"] = json!(role);
        }
        let id = register(&router, why, &window_id, &registration).await;
        let answered = update(&router, why, &window_id, &id, &body).await;
        assert_eq!(
            answered["ServiceRoleArn"].as_str(),
            Some(expected.as_str()),
            "case {why:?}: expected the update to answer ServiceRoleArn {expected:?}, got \
             {answered}"
        );
        // What the update answered is what it persisted.
        let reported = read(&router, why, &window_id, &id).await;
        assert_eq!(
            reported["ServiceRoleArn"].as_str(),
            Some(expected.as_str()),
            "case {why:?}: expected the read to report ServiceRoleArn {expected:?}, got \
             {reported}"
        );
    }
}

/// RULE 3: every projection of a registered task carries its parameter map, present and EMPTY
/// where the task holds no parameters.
///
/// The rows differ along what the rule generalises over: whether the register states the map at
/// all, how many entries it holds, how many values one entry's expression carries, and whether
/// an update rewrites or clears the map.
#[tokio::test]
async fn every_projection_of_a_task_carries_its_parameter_map() {
    let router = router_with_iam();
    let case = "the parameter table";
    let window_id = window(&router, case, "task-parameters").await;

    // (case, the map the register states, the update's members, the map every projection must
    //  report afterwards)
    let rows: Vec<(&str, Option<Value>, Value, Value)> = vec![
        (
            "a register stating no map at all",
            None,
            json!({ "Priority": 1 }),
            json!({}),
        ),
        (
            "a register stating the empty map",
            Some(json!({})),
            json!({ "Priority": 1 }),
            json!({}),
        ),
        (
            "a register stating one entry",
            Some(json!({ "Operation": { "Values": ["Install"] } })),
            json!({ "Priority": 1 }),
            json!({ "Operation": { "Values": ["Install"] } }),
        ),
        (
            "a register stating two entries, one multi-valued",
            Some(json!({
                "Operation": { "Values": ["Install", "Scan"] },
                "RebootOption": { "Values": ["NoReboot"] },
            })),
            json!({ "Priority": 1 }),
            json!({
                "Operation": { "Values": ["Install", "Scan"] },
                "RebootOption": { "Values": ["NoReboot"] },
            }),
        ),
        (
            "an update rewriting the map wholesale",
            Some(json!({ "Operation": { "Values": ["Install"] } })),
            json!({ "TaskParameters": { "RebootOption": { "Values": ["NoReboot"] } } }),
            json!({ "RebootOption": { "Values": ["NoReboot"] } }),
        ),
        (
            "a replacement stating no map, which clears it to the empty map",
            Some(json!({ "Operation": { "Values": ["Install"] } })),
            json!({ "Replace": true, "TaskArn": "AWS-RunPatchBaseline" }),
            json!({}),
        ),
    ];

    for (why, stated, body, expected) in rows {
        let mut registration = json!({
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "MaxConcurrency": "1",
            "MaxErrors": "1",
        });
        if let Some(map) = stated {
            registration["TaskParameters"] = map;
        }
        let id = register(&router, why, &window_id, &registration).await;
        let answered = update(&router, why, &window_id, &id, &body).await;

        for (projection, reported) in [
            ("the update's own answer", answered),
            ("the read", read(&router, why, &window_id, &id).await),
            ("the listing", listed(&router, why, &window_id, &id).await),
        ] {
            assert_eq!(
                reported["TaskParameters"], expected,
                "case {why:?}: expected {projection} to report TaskParameters {expected}, got \
                 {reported}"
            );
        }
    }
}
