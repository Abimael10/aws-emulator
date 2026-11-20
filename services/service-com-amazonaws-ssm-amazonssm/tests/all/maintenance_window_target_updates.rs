// SPDX-License-Identifier: CC0-1.0

//! Modifying a target already registered with a maintenance window:
//! `UpdateMaintenanceWindowTarget` edits what `RegisterTargetWithMaintenanceWindow`
//! persisted, and `DescribeMaintenanceWindowTargets` reports what the edit left behind.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **An update is a PATCH of the stored target.** "If a parameter is null, then the
//!    corresponding field isn't modified": every member the request supplies is applied and
//!    every member it leaves out keeps the value the target holds, and what the update
//!    reports is what a later read of that target reports.
//! 2. **A REPLACING update clears what it leaves out.** Under `Replace`, "optional fields
//!    that aren't specified are set to null", while the members
//!    `RegisterTargetWithMaintenanceWindow` requires are required here too — so a replacing
//!    update carrying no `Targets` is refused and writes nothing.
//! 3. **The target's identity does not move.** No input member carries the id, the window or
//!    the resource type, so an update leaves all three as the register persisted them, and
//!    leaves the order a listing of the window's targets walks them in.
//! 4. **Both ids are resolved against the Region's records.** A window this endpoint holds
//!    none of, and a target that is no target of the window named, are each refused
//!    `DoesNotExistException`, and a refused update writes nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a target is shown to be reachable only through the endpoint of the
/// Region its window lives in.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// A `WindowTargetId` of the form the modelled `MaintenanceWindowTargetId` pattern requires
/// that no register has minted.
const UNKNOWN_TARGET_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The labels a target carries beside its criteria, which an update may set and a replacing
/// update clears: the optional members of `UpdateMaintenanceWindowTargetRequest`
/// (`smithy_query(query="operation",operation="UpdateMaintenanceWindowTarget")`).
const LABELS: [&str; 3] = ["Name", "Description", "OwnerInformation"];

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
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the window every target of this case belongs to must be created, got \
         {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// Register a target of `window_id` with `body`'s members and answer the id minted for it.
async fn register(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    body: &Value,
) -> String {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    if request.get("ResourceType").is_none() {
        request["ResourceType"] = Value::String("INSTANCE".to_string());
    }
    let (status, response) = call_in_region(
        router,
        region,
        "RegisterTargetWithMaintenanceWindow",
        &request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the register {request} must succeed, got {status} {response}"
    );
    response["WindowTargetId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a register reports a WindowTargetId, got {response}")
        })
        .to_string()
}

/// `UpdateMaintenanceWindowTarget` at `region`'s endpoint with `body`.
async fn update(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "UpdateMaintenanceWindowTarget", body).await
}

/// The same update, for the target `window_target_id` of `window_id`, carrying `body`'s
/// members.
async fn update_for(
    router: &MultiServiceRouter,
    region: &str,
    window_id: &str,
    window_target_id: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    request["WindowTargetId"] = Value::String(window_target_id.to_string());
    update(router, region, &request).await
}

/// The targets one window holds, in the order the describe reports them.
async fn targets_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
) -> Vec<Value> {
    let (status, response) = call_in_region(
        router,
        region,
        "DescribeMaintenanceWindowTargets",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {window_id:?} must succeed, got {status} {response}"
    );
    response["Targets"].as_array().cloned().unwrap_or_default()
}

/// The one target `window_target_id` names among `window_id`'s, read back through the
/// describe.
async fn target(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    window_target_id: &str,
) -> Value {
    let held = targets_of(router, case, region, window_id).await;
    held.iter()
        .find(|target| target["WindowTargetId"].as_str() == Some(window_target_id))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the describe to report the target \
                 {window_target_id:?}, got {held:?}"
            )
        })
}

/// Assert that `reported` carries every member `expected` names, and nothing for a label
/// `expected` leaves out.
fn assert_members(case: &str, reported: &Value, expected: &Value) {
    for (member, value) in expected.as_object().expect("the expected members") {
        assert_eq!(
            &reported[member], value,
            "case {case:?}: expected {member} {value}, got {reported}"
        );
    }
    for label in LABELS {
        if expected.get(label).is_none() {
            assert!(
                reported.get(label).is_none_or(Value::is_null),
                "case {case:?}: expected no {label} to be carried, got {reported}"
            );
        }
    }
}

/// RULE 1: an update is a patch of the stored target — a member supplied is applied, a member
/// left out keeps what the target holds — and what the update reports is what a later read
/// reports.
///
/// The rows differ along what the rule generalises over: which members the update supplies
/// (one label, another label, every label, the criteria alone, nothing at all), which form
/// the new criteria take (node ids, a tag, two criteria at once, the empty list the modelled
/// length admits) and whether the update states `Replace` as false or leaves it out.
#[tokio::test]
async fn an_update_applies_what_it_carries_and_keeps_what_it_omits() {
    let router = router();
    let case = "the patch table";
    let window_id = window(&router, case, REGION, "target-updates").await;

    // What every row starts from: a register carrying criteria and all three labels.
    let registered = json!({
        "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        "Name": "registered-name",
        "Description": "the registered description",
        "OwnerInformation": "the registered owner",
    });

    // (case, the update's members, the members the target must carry afterwards)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "one label alone",
            json!({ "Name": "updated-name" }),
            json!({
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "Name": "updated-name",
                "Description": "the registered description",
                "OwnerInformation": "the registered owner",
            }),
        ),
        (
            "another label alone",
            json!({ "OwnerInformation": "the new owner" }),
            json!({
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "Name": "registered-name",
                "Description": "the registered description",
                "OwnerInformation": "the new owner",
            }),
        ),
        (
            "every label at once",
            json!({
                "Name": "renamed-target",
                "Description": "the new description",
                "OwnerInformation": "the new owner",
            }),
            json!({
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "Name": "renamed-target",
                "Description": "the new description",
                "OwnerInformation": "the new owner",
            }),
        ),
        (
            "a tag criterion in place of the node ids",
            json!({ "Targets": [{ "Key": "tag:Environment", "Values": ["production"] }] }),
            json!({
                "Targets": [{ "Key": "tag:Environment", "Values": ["production"] }],
                "Name": "registered-name",
                "Description": "the registered description",
                "OwnerInformation": "the registered owner",
            }),
        ),
        (
            "two criteria at once, stating Replace as false",
            json!({
                "Replace": false,
                "Targets": [
                    { "Key": "tag:Team", "Values": ["payments"] },
                    { "Key": "tag-key", "Values": ["Patch"] },
                ],
            }),
            json!({
                "Targets": [
                    { "Key": "tag:Team", "Values": ["payments"] },
                    { "Key": "tag-key", "Values": ["Patch"] },
                ],
                "Name": "registered-name",
                "Description": "the registered description",
                "OwnerInformation": "the registered owner",
            }),
        ),
        (
            "the empty criteria list the modelled length admits",
            json!({ "Targets": [] }),
            json!({
                "Targets": [],
                "Name": "registered-name",
                "Description": "the registered description",
                "OwnerInformation": "the registered owner",
            }),
        ),
        (
            "nothing at all, which modifies nothing",
            json!({}),
            json!({
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
                "Name": "registered-name",
                "Description": "the registered description",
                "OwnerInformation": "the registered owner",
            }),
        ),
    ];

    for (why, body, expected) in rows {
        let id = register(&router, why, REGION, &window_id, &registered).await;
        let (status, response) = update_for(&router, REGION, &window_id, &id, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the update {body} must be served, got {status} {response}"
        );
        assert_eq!(
            (
                response["WindowId"].as_str(),
                response["WindowTargetId"].as_str()
            ),
            (Some(window_id.as_str()), Some(id.as_str())),
            "case {why:?}: expected the update to report the ids it was asked about, got \
             {response}"
        );
        assert_members(why, &response, &expected);

        // The read back through the describe is the same answer: the update persisted it.
        let read = target(&router, why, REGION, &window_id, &id).await;
        assert_members(why, &read, &expected);
    }
}

/// RULE 2: a replacing update clears every label it leaves out, and is refused when it leaves
/// out the criteria `RegisterTargetWithMaintenanceWindow` requires — a refusal that writes
/// nothing.
///
/// The rows differ along which labels the replacing update keeps: all of them, one of them,
/// none of them.
#[tokio::test]
async fn a_replacing_update_clears_the_labels_it_leaves_out() {
    let router = router();
    let case = "the replacement table";
    let window_id = window(&router, case, REGION, "target-replacements").await;
    let registered = json!({
        "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        "Name": "registered-name",
        "Description": "the registered description",
        "OwnerInformation": "the registered owner",
    });

    // (case, the replacing update's members, the members the target must carry afterwards)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a replacement carrying every label",
            json!({
                "Replace": true,
                "Targets": [{ "Key": "tag:Team", "Values": ["payments"] }],
                "Name": "replaced-name",
                "Description": "the replacing description",
                "OwnerInformation": "the replacing owner",
            }),
            json!({
                "Targets": [{ "Key": "tag:Team", "Values": ["payments"] }],
                "Name": "replaced-name",
                "Description": "the replacing description",
                "OwnerInformation": "the replacing owner",
            }),
        ),
        (
            "a replacement carrying one label",
            json!({
                "Replace": true,
                "Targets": [{ "Key": "tag:Team", "Values": ["payments"] }],
                "Name": "replaced-name",
            }),
            json!({
                "Targets": [{ "Key": "tag:Team", "Values": ["payments"] }],
                "Name": "replaced-name",
            }),
        ),
        (
            "a replacement carrying no label at all",
            json!({
                "Replace": true,
                "Targets": [{ "Key": "resource-groups:Name", "Values": ["the-estate"] }],
            }),
            json!({ "Targets": [{ "Key": "resource-groups:Name", "Values": ["the-estate"] }] }),
        ),
    ];

    for (why, body, expected) in rows {
        let id = register(&router, why, REGION, &window_id, &registered).await;
        let (status, response) = update_for(&router, REGION, &window_id, &id, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the replacing update {body} must be served, got {status} {response}"
        );
        assert_members(why, &response, &expected);
        let read = target(&router, why, REGION, &window_id, &id).await;
        assert_members(why, &read, &expected);
    }

    // The criteria are required of a replacing update, because the register requires them.
    let id = register(&router, case, REGION, &window_id, &registered).await;
    let (status, response) =
        update_for(&router, REGION, &window_id, &id, &json!({ "Replace": true })).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "ValidationException"),
        "case \"a replacement carrying no criteria\": expected a refusal, got {status} {response}"
    );
    assert!(
        error_message(&response).contains("'targets'"),
        "case \"a replacement carrying no criteria\": expected the refusal to name the member it \
         wanted, got {response}"
    );
    let read = target(&router, case, REGION, &window_id, &id).await;
    assert_members(
        "a replacement carrying no criteria",
        &read,
        &json!({
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "Name": "registered-name",
            "Description": "the registered description",
            "OwnerInformation": "the registered owner",
        }),
    );
}

/// RULE 3: the target's identity does not move — an update leaves its minted id, its window
/// and its resource type as the register persisted them, leaves the other targets of that
/// window alone, and leaves the order a listing walks them in.
///
/// The rows differ along which resource type the register stated and which members the update
/// carries.
#[tokio::test]
async fn an_update_leaves_the_targets_identity_and_the_listing_order_alone() {
    let router = router();
    let case = "the identity case";
    let window_id = window(&router, case, REGION, "target-identity").await;

    // (case, the resource type the register states, the update's members)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a node target renamed",
            "INSTANCE",
            json!({ "Name": "renamed-node-target" }),
        ),
        (
            "a resource-group target replaced",
            "RESOURCE_GROUP",
            json!({
                "Replace": true,
                "Targets": [{ "Key": "resource-groups:Name", "Values": ["another-estate"] }],
            }),
        ),
    ];

    let mut registered: Vec<(&str, String, &str)> = Vec::new();
    for (why, resource_type, _) in &rows {
        let id = register(
            &router,
            why,
            REGION,
            &window_id,
            &json!({
                "ResourceType": resource_type,
                "Targets": [{ "Key": "tag:Team", "Values": ["payments"] }],
            }),
        )
        .await;
        registered.push((*why, id, *resource_type));
    }

    for (position, (why, id, resource_type)) in registered.iter().enumerate() {
        let body = &rows[position].2;
        let (status, response) = update_for(&router, REGION, &window_id, id, body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the update must be served, got {status} {response}"
        );
        let read = target(&router, why, REGION, &window_id, id).await;
        assert_eq!(
            read["ResourceType"].as_str(),
            Some(*resource_type),
            "case {why:?}: expected the resource type the register persisted, got {read}"
        );
        assert_eq!(
            read["WindowId"].as_str(),
            Some(window_id.as_str()),
            "case {why:?}: expected the window the register persisted, got {read}"
        );
    }

    let held = targets_of(&router, case, REGION, &window_id).await;
    let walked: Vec<&str> = held
        .iter()
        .map(|target| target["WindowTargetId"].as_str().unwrap_or_default())
        .collect();
    let minted: Vec<&str> = registered.iter().map(|(_, id, _)| id.as_str()).collect();
    assert_eq!(
        walked, minted,
        "case {case:?}: expected the updates to have minted no target and to have left the \
         order they were registered in"
    );
}

/// RULE 4: both ids are resolved against the Region's records — a window this endpoint holds
/// none of, and a target that is no target of the window named, are each refused, and a
/// refused update writes nothing.
#[tokio::test]
async fn an_update_resolves_both_ids_against_the_regions_records() {
    let router = router();
    let case = "the refusal table";
    let here = window(&router, case, REGION, "target-refusals").await;
    let elsewhere = window(&router, case, OTHER_REGION, "target-refusals").await;
    let other_window = window(&router, case, REGION, "another-window").await;
    let registered = json!({
        "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        "Name": "registered-name",
    });
    let id = register(&router, case, REGION, &here, &registered).await;
    let elsewhere_id = register(&router, case, OTHER_REGION, &elsewhere, &registered).await;

    // (case, the Region the update is signed for, the window it names, the target it names,
    //  the message the refusal must carry)
    let rows: Vec<(&str, &str, &str, &str, String)> = vec![
        (
            "a window id no create minted",
            REGION,
            UNKNOWN_WINDOW_ID,
            id.as_str(),
            format!("Maintenance window {UNKNOWN_WINDOW_ID} does not exist"),
        ),
        (
            "a window of another Region",
            REGION,
            elsewhere.as_str(),
            elsewhere_id.as_str(),
            format!("Maintenance window {elsewhere} does not exist"),
        ),
        (
            "a target id no register minted",
            REGION,
            here.as_str(),
            UNKNOWN_TARGET_ID,
            format!("Maintenance window target {UNKNOWN_TARGET_ID} does not exist"),
        ),
        (
            "a target of another window of this Region",
            REGION,
            other_window.as_str(),
            id.as_str(),
            format!("Maintenance window target {id} does not exist"),
        ),
        (
            "this Region's target at the other endpoint",
            OTHER_REGION,
            here.as_str(),
            id.as_str(),
            format!("Maintenance window {here} does not exist"),
        ),
    ];

    for (why, region, window_id, window_target_id, message) in rows {
        let (status, response) = update_for(
            &router,
            region,
            window_id,
            window_target_id,
            &json!({ "Name": "a-name-nothing-must-carry" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the update to be refused, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            message,
            "case {why:?}: expected the refusal to name what was asked for, got {response}"
        );
    }

    // None of the refused updates touched the target they named.
    let read = target(&router, case, REGION, &here, &id).await;
    assert_members(
        case,
        &read,
        &json!({
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "Name": "registered-name",
        }),
    );
}
