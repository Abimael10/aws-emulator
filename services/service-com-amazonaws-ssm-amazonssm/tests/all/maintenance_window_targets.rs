// SPDX-License-Identifier: CC0-1.0

//! The TARGETS of a maintenance window end to end:
//! `RegisterTargetWithMaintenanceWindow` registers one, `DescribeMaintenanceWindowTargets`
//! reports the ones a window holds, `DeregisterTargetFromMaintenanceWindow` removes one, and
//! `DeleteMaintenanceWindow` takes the window's targets with it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router. Nothing reaches
//! into the state struct, so a response that is not computed from persisted state cannot
//! pass: every read follows a write.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **A read reports what the register persisted.** For the window the request names, the
//!    describe reports exactly the members each register of that window supplied, under the
//!    id the service minted for it, in the order they were registered.
//! 2. **A target belongs to ONE window of ONE Region.** A register naming a window this
//!    endpoint holds none of is refused `DoesNotExistException` and writes nothing, and a
//!    target of one window is no target of another.
//! 3. **The describe narrows by the documented filter keys** — `Type`, `WindowTargetId` and
//!    `OwnerInformation` — which intersect, each filter's values union, and a key outside that
//!    set is refused before anything is read. A page holds `MaxResults` targets and the
//!    `NextToken` resumes at the target the page stopped at.
//! 4. **A removal removes it.** A deregistered target is no longer reported, a second removal
//!    of it is the no-op the state it asks for already is, and deleting the WINDOW removes the
//!    targets registered with it, because a target is only ever addressed through its window.
//! 5. **The store is bounded per window.** Past the ceiling one window's targets are held
//!    under, a register is refused `ResourceLimitExceededException` and writes nothing, while
//!    a removal frees a slot and another window is bounded on its own.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a window and its targets are shown to live at the endpoint of ONE
/// Region.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// A `WindowTargetId` of the form the modelled `MaintenanceWindowTargetId` pattern requires
/// that no register has minted.
const UNKNOWN_TARGET_ID: &str = "00000000-0000-0000-0000-000000000000";

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

/// `RegisterTargetWithMaintenanceWindow` at `region`'s endpoint with `body`.
async fn register(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "RegisterTargetWithMaintenanceWindow",
        body,
    )
    .await
}

/// The same write, asserting it succeeded, and answering the `WindowTargetId` it minted.
async fn register_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> String {
    let (status, response) = register(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the register {body} must succeed, got {status} {response}"
    );
    let id = response["WindowTargetId"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: a register reports a WindowTargetId, got {response}")
    });
    assert!(
        is_target_id(id),
        "case {case:?}: expected the 8-4-4-4-12 hexadecimal the model fixes, got {id:?}"
    );
    id.to_string()
}

/// `DescribeMaintenanceWindowTargets` at `region`'s endpoint with `body`.
async fn describe(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "DescribeMaintenanceWindowTargets", body).await
}

/// The targets one window holds, in the order the describe reports them.
async fn targets_of(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
) -> Vec<Value> {
    let (status, response) = describe(router, region, &json!({ "WindowId": window_id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {window_id:?} must succeed, got {status} {response}"
    );
    response["Targets"].as_array().cloned().unwrap_or_default()
}

/// Whether `id` is shaped like the `MaintenanceWindowTargetId` the model fixes: the 8-4-4-4-12
/// hexadecimal of its own pattern.
fn is_target_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, width)| {
                group.len() == width && group.chars().all(|digit| digit.is_ascii_hexdigit())
            })
}

/// RULE 1: the describe reports, for each target of the window the request names, exactly the
/// members its register supplied — under the id the service minted — and nothing for a member
/// the register left out.
///
/// The rows differ along what the rule generalises over: which `MaintenanceWindowResourceType`
/// the register states, which form its criteria take (node ids, a tag, a resource group,
/// several criteria at once, no criteria at all) and which of the three optional labels it
/// carries.
#[tokio::test]
async fn a_read_reports_what_the_register_persisted() {
    let router = router();
    let window_id = window(&router, "the register table", REGION, "target-table").await;

    // (case, the register body, the members the read must report)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "node ids and no labels",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            }),
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            }),
        ),
        (
            "a tag criterion with every label",
            json!({
                "ResourceType": "INSTANCE",
                "Name": "tagged-target",
                "Description": "the tagged fleet",
                "OwnerInformation": "platform",
                "Targets": [{ "Key": "tag:Environment", "Values": ["production", "staging"] }],
            }),
            json!({
                "ResourceType": "INSTANCE",
                "Name": "tagged-target",
                "Description": "the tagged fleet",
                "OwnerInformation": "platform",
                "Targets": [{ "Key": "tag:Environment", "Values": ["production", "staging"] }],
            }),
        ),
        (
            "a resource group of the other resource type",
            json!({
                "ResourceType": "RESOURCE_GROUP",
                "OwnerInformation": "the estate",
                "Targets": [{ "Key": "resource-groups:Name", "Values": ["the-estate"] }],
            }),
            json!({
                "ResourceType": "RESOURCE_GROUP",
                "OwnerInformation": "the estate",
                "Targets": [{ "Key": "resource-groups:Name", "Values": ["the-estate"] }],
            }),
        ),
        (
            "two criteria at once",
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [
                    { "Key": "tag:Team", "Values": ["payments"] },
                    { "Key": "tag-key", "Values": ["Patch"] },
                ],
            }),
            json!({
                "ResourceType": "INSTANCE",
                "Targets": [
                    { "Key": "tag:Team", "Values": ["payments"] },
                    { "Key": "tag-key", "Values": ["Patch"] },
                ],
            }),
        ),
        (
            "no criteria at all, which the modelled length admits",
            json!({ "ResourceType": "INSTANCE", "Targets": [] }),
            json!({ "ResourceType": "INSTANCE", "Targets": [] }),
        ),
    ];

    let mut registered: Vec<(&str, String, Value)> = Vec::new();
    for (case, mut body, expected) in rows {
        body["WindowId"] = Value::String(window_id.clone());
        let id = register_ok(&router, case, REGION, &body).await;
        registered.push((case, id, expected));
    }

    let held = targets_of(&router, "the register table", REGION, &window_id).await;
    assert_eq!(
        held.len(),
        registered.len(),
        "case \"the register table\": expected one target per register, got {held:?}"
    );
    for (position, (case, id, expected)) in registered.iter().enumerate() {
        // The order a describe reports them in is the order they were registered.
        let reported = &held[position];
        assert_eq!(
            reported["WindowTargetId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the target registered at position {position}, got \
             {reported}"
        );
        assert_eq!(
            reported["WindowId"].as_str(),
            Some(window_id.as_str()),
            "case {case:?}: expected the window it was registered with, got {reported}"
        );
        for (member, value) in expected.as_object().expect("the expected members") {
            assert_eq!(
                &reported[member], value,
                "case {case:?}: expected {member} {value}, got {reported}"
            );
        }
        for member in ["Name", "Description", "OwnerInformation"] {
            if expected.get(member).is_none() {
                assert!(
                    reported.get(member).is_none_or(Value::is_null),
                    "case {case:?}: expected no {member} for a register that carried none, got \
                     {reported}"
                );
            }
        }
    }
}

/// RULE 2: a target belongs to one window of one Region — a register naming a window this
/// endpoint holds none of is refused and writes nothing, and a window's describe reports its
/// own targets alone.
#[tokio::test]
async fn a_target_belongs_to_one_window_of_one_region() {
    let router = router();
    let case = "the window scope";
    let here = window(&router, case, REGION, "scoped-window").await;
    let elsewhere = window(&router, case, OTHER_REGION, "scoped-window").await;
    let body = |window_id: &str| {
        json!({
            "WindowId": window_id,
            "ResourceType": "INSTANCE",
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        })
    };
    let registered = register_ok(&router, case, REGION, &body(&here)).await;

    // (case, the Region the register is signed for, the window it names)
    let refused: Vec<(&str, &str, &str)> = vec![
        ("an id no create minted", REGION, UNKNOWN_WINDOW_ID),
        ("a window of another Region", REGION, elsewhere.as_str()),
        ("this Region's window at the other endpoint", OTHER_REGION, here.as_str()),
    ];
    for (why, region, window_id) in refused {
        let (status, response) = register(&router, region, &body(window_id)).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the register to be refused, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("Maintenance window {window_id} does not exist"),
            "case {why:?}: expected the refusal to name the window asked for, got {response}"
        );
    }

    let held = targets_of(&router, case, REGION, &here).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: the refused registers must have written nothing, got {held:?}"
    );
    assert_eq!(
        held[0]["WindowTargetId"].as_str(),
        Some(registered.as_str()),
        "case {case:?}: expected the one target this window holds, got {held:?}"
    );
    let (status, response) = describe(&router, REGION, &json!({ "WindowId": UNKNOWN_WINDOW_ID }))
        .await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "DoesNotExistException"),
        "case {case:?}: expected a describe of a window this endpoint holds none of to be \
         refused, got {status} {response}"
    );
    let none = targets_of(&router, case, OTHER_REGION, &elsewhere).await;
    assert!(
        none.is_empty(),
        "case {case:?}: expected the other Region's window to hold no target, got {none:?}"
    );
}

/// RULE 3: the describe narrows by the documented filter keys, which intersect while each
/// filter's values union — and a key outside that set is refused before anything is read.
#[tokio::test]
async fn the_describe_narrows_by_the_documented_filter_keys() {
    let router = router();
    let case = "the filter table";
    let window_id = window(&router, case, REGION, "filtered-window").await;
    let instance_target = register_ok(
        &router,
        case,
        REGION,
        &json!({
            "WindowId": window_id,
            "ResourceType": "INSTANCE",
            "OwnerInformation": "platform",
            "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
        }),
    )
    .await;
    let group_target = register_ok(
        &router,
        case,
        REGION,
        &json!({
            "WindowId": window_id,
            "ResourceType": "RESOURCE_GROUP",
            "OwnerInformation": "estate",
            "Targets": [{ "Key": "resource-groups:Name", "Values": ["the-estate"] }],
        }),
    )
    .await;

    // (case, the filters, the target ids the describe must report)
    let rows: Vec<(&str, Value, Vec<&String>)> = vec![
        (
            "the resource type",
            json!([{ "Key": "Type", "Values": ["INSTANCE"] }]),
            vec![&instance_target],
        ),
        (
            "either resource type, whose values union",
            json!([{ "Key": "Type", "Values": ["INSTANCE", "RESOURCE_GROUP"] }]),
            vec![&instance_target, &group_target],
        ),
        (
            "the target's own id",
            json!([{ "Key": "WindowTargetId", "Values": [group_target] }]),
            vec![&group_target],
        ),
        (
            "the owner information",
            json!([{ "Key": "OwnerInformation", "Values": ["platform"] }]),
            vec![&instance_target],
        ),
        (
            "two filters, which intersect",
            json!([
                { "Key": "Type", "Values": ["INSTANCE"] },
                { "Key": "OwnerInformation", "Values": ["estate"] },
            ]),
            vec![],
        ),
        (
            "a value no target carries",
            json!([{ "Key": "OwnerInformation", "Values": ["nobody"] }]),
            vec![],
        ),
    ];
    for (why, filters, expected) in rows {
        let (status, response) = describe(
            &router,
            REGION,
            &json!({ "WindowId": window_id, "Filters": filters }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {why:?}: the filtered describe must be served, got {status} {response}"
        );
        let reported: Vec<String> = response["Targets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|target| target["WindowTargetId"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {why:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // A key the operation does not narrow by names no property of a target.
    let (status, response) = describe(
        &router,
        REGION,
        &json!({
            "WindowId": window_id,
            "Filters": [{ "Key": "NotAKey", "Values": ["anything"] }],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "ValidationException"),
        "case \"a filter key the operation does not take\": expected a refusal, got {status} \
         {response}"
    );
    assert!(
        error_message(&response).contains("NotAKey"),
        "case \"a filter key the operation does not take\": expected the refusal to name the \
         key submitted, got {response}"
    );
}

/// RULE 3 (paging): a page holds `MaxResults` targets and the `NextToken` resumes the listing
/// at the target the page stopped at, so a walk over the pages reports each target once.
#[tokio::test]
async fn a_page_resumes_at_the_target_it_stopped_at() {
    let router = router();
    let case = "the paging case";
    let window_id = window(&router, case, REGION, "paged-window").await;
    // One more than the smallest page the modelled `MaintenanceWindowMaxResults` admits
    // (range 10..100), so the walk takes two pages.
    let page_size = 10;
    let mut minted: Vec<String> = Vec::new();
    for index in 0..=page_size {
        minted.push(
            register_ok(
                &router,
                case,
                REGION,
                &json!({
                    "WindowId": window_id,
                    "ResourceType": "INSTANCE",
                    "OwnerInformation": format!("target-{index}"),
                    "Targets": [{ "Key": "tag:Index", "Values": [index.to_string()] }],
                }),
            )
            .await,
        );
    }

    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "WindowId": window_id, "MaxResults": page_size });
        if let Some(next) = &token {
            body["NextToken"] = Value::String(next.clone());
        }
        let (status, response) = describe(&router, REGION, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: each page must be served, got {status} {response}"
        );
        let page = response["Targets"].as_array().cloned().unwrap_or_default();
        assert!(
            page.len() <= page_size as usize,
            "case {case:?}: a page holds at most MaxResults targets, got {}",
            page.len()
        );
        walked.extend(
            page.iter()
                .map(|target| target["WindowTargetId"].as_str().unwrap_or_default().to_string()),
        );
        match response["NextToken"].as_str() {
            Some(next) if !next.is_empty() => token = Some(next.to_string()),
            _ => break,
        }
    }
    assert_eq!(
        walked, minted,
        "case {case:?}: expected the walk to report every target once, in the order they were \
         registered"
    );

    // A token this read did not hand out is not a page of it: one that is not shaped like a
    // target id, and one that is but names no target of this result.
    for (why, token) in [
        ("a token this service did not mint", "not-a-token"),
        ("a target id that is no target of this window", UNKNOWN_TARGET_ID),
    ] {
        let (status, response) = describe(
            &router,
            REGION,
            &json!({ "WindowId": window_id, "NextToken": token }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {why:?}: expected a refusal, got {status} {response}"
        );
    }
}

/// RULE 4: a removal removes it — a deregistered target is no longer reported and a second
/// removal of it is the no-op the state it asks for already is, while deleting the WINDOW
/// takes the targets registered with it.
#[tokio::test]
async fn a_removal_removes_the_target() {
    let router = router();
    let case = "the removal case";
    let window_id = window(&router, case, REGION, "removal-window").await;
    let body = json!({
        "WindowId": window_id,
        "ResourceType": "INSTANCE",
        "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
    });
    let first = register_ok(&router, case, REGION, &body).await;
    let second = register_ok(&router, case, REGION, &body).await;

    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeregisterTargetFromMaintenanceWindow",
        &json!({ "WindowId": window_id, "WindowTargetId": first }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deregister must be served, got {status} {response}"
    );
    assert_eq!(
        (
            response["WindowId"].as_str(),
            response["WindowTargetId"].as_str()
        ),
        (Some(window_id.as_str()), Some(first.as_str())),
        "case {case:?}: expected the deregister to report what it removed, got {response}"
    );
    let held = targets_of(&router, case, REGION, &window_id).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected only the target that was not removed, got {held:?}"
    );
    assert_eq!(
        held[0]["WindowTargetId"].as_str(),
        Some(second.as_str()),
        "case {case:?}: expected the other target to be untouched, got {held:?}"
    );

    // A removal asks for a STATE: a target an existing window already holds none of is that
    // state, so the call answers 200 with the ids it was asked about and removes nothing.
    // (case, the target the deregister names)
    let already_gone: Vec<(&str, &str)> = vec![
        ("the target that is already gone", first.as_str()),
        ("a target id nothing minted", UNKNOWN_TARGET_ID),
    ];
    for (why, target) in already_gone {
        let (status, response) = call_in_region(
            &router,
            REGION,
            "DeregisterTargetFromMaintenanceWindow",
            &json!({ "WindowId": window_id, "WindowTargetId": target }),
        )
        .await;
        assert_eq!(
            (status, response["WindowTargetId"].as_str()),
            (200, Some(target)),
            "case {why:?}: expected the deregister to echo the ids it was asked about, got \
             {status} {response}"
        );
    }
    let still_held = targets_of(&router, case, REGION, &window_id).await;
    assert_eq!(
        still_held.len(),
        1,
        "case {case:?}: expected the repeated deregisters to have written nothing, got \
         {still_held:?}"
    );

    // The WINDOW is the resource half, and an id this endpoint holds no window for is still
    // refused.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeregisterTargetFromMaintenanceWindow",
        &json!({ "WindowId": UNKNOWN_WINDOW_ID, "WindowTargetId": second }),
    )
    .await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "DoesNotExistException"),
        "case \"a window this endpoint holds none of\": expected the deregister to be refused, \
         got {status} {response}"
    );

    // Deleting the window takes its remaining target with it: a target is only ever
    // addressed through its window.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeleteMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must be served, got {status} {response}"
    );
    assert_eq!(
        response["WindowId"].as_str(),
        Some(window_id.as_str()),
        "case {case:?}: expected the delete to report the window it was asked about, got \
         {response}"
    );
    let (status, response) = describe(&router, REGION, &json!({ "WindowId": window_id })).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "DoesNotExistException"),
        "case {case:?}: expected the deleted window to hold no targets to list, got {status} \
         {response}"
    );
    // The delete of a window that is already gone is the same answer: the operation models no
    // not-found error at all.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeleteMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a window already deleted\": expected the delete to be idempotent, got {status} \
         {response}"
    );
}

/// RULE: "user-provided idempotency token" — a retry of one register reports the target that
/// register already minted, and the same token carrying different members is another call
/// rather than a retry.
#[tokio::test]
async fn an_idempotency_token_makes_a_retried_register_the_same_target() {
    let router = router();
    let case = "the idempotency case";
    let window_id = window(&router, case, REGION, "token-window").await;
    let body = json!({
        "WindowId": window_id,
        "ResourceType": "INSTANCE",
        "ClientToken": "one-token",
        "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
    });
    let first = register_ok(&router, case, REGION, &body).await;
    let retried = register_ok(&router, case, REGION, &body).await;
    assert_eq!(
        retried, first,
        "case {case:?}: expected the retry to report the target the first register minted"
    );
    let held = targets_of(&router, case, REGION, &window_id).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the retry to have minted no second target, got {held:?}"
    );

    let mut different = body.clone();
    different["Targets"] = json!([{ "Key": "InstanceIds", "Values": ["i-00000000000000001"] }]);
    let (status, response) = register(&router, REGION, &different).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "IdempotentParameterMismatch"),
        "case {case:?}: expected one token carrying different members to be refused, got \
         {status} {response}"
    );
}

/// RULE 5: the store is BOUNDED by what ONE window holds — past that ceiling a register is
/// refused `ResourceLimitExceededException`, names the ceiling and the window asked about and
/// writes nothing; a removal frees a slot, and a second window of the same Region is bounded
/// on its own.
///
/// The ceiling is read from the crate rather than written here, so the case tests the bound
/// the handler enforces and not a copy of it.
#[tokio::test]
async fn a_register_past_the_ceiling_is_refused() {
    const MAX_TARGETS: usize = service_com_amazonaws_ssm_amazonssm::state::maintenance_window_targets::MAX_TARGETS_PER_WINDOW;

    let router = router();
    let case = "the ceiling case";
    let window_id = window(&router, case, REGION, "ceiling-window").await;
    // Each register states its own criteria, so no two of them are a retry of one another.
    let body = |window: &str, index: usize| {
        json!({
            "WindowId": window,
            "ResourceType": "INSTANCE",
            "OwnerInformation": format!("target-{index}"),
            "Targets": [{ "Key": "tag:Index", "Values": [index.to_string()] }],
        })
    };
    let mut minted: Vec<String> = Vec::new();
    for index in 0..MAX_TARGETS {
        minted.push(register_ok(&router, case, REGION, &body(&window_id, index)).await);
    }

    let (status, response) = register(&router, REGION, &body(&window_id, MAX_TARGETS)).await;
    assert_eq!(
        (status, error_code(&response).as_str()),
        (400, "ResourceLimitExceededException"),
        "case {case:?}: expected the register past the ceiling to be refused, got {status} \
         {response}"
    );
    assert_eq!(
        error_message(&response),
        format!(
            "This emulator holds at most {MAX_TARGETS} targets for the maintenance window \
             {window_id}"
        ),
        "case {case:?}: expected the refusal to name the ceiling and the window, got {response}"
    );

    // The refused register wrote nothing: the window still holds exactly the targets the
    // ceiling admitted, under the ids those registers minted.
    let reported: Vec<String> = targets_of(&router, case, REGION, &window_id)
        .await
        .iter()
        .map(|target| {
            target["WindowTargetId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        reported, minted,
        "case {case:?}: expected the targets the ceiling admitted and nothing more, got \
         {reported:?}"
    );

    // The bound is on what the window HOLDS, not on how many registers ran: deregistering one
    // frees the slot the next register takes.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeregisterTargetFromMaintenanceWindow",
        &json!({ "WindowId": window_id, "WindowTargetId": minted[0] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deregister that frees a slot must be served, got {status} {response}"
    );
    let refilled = register_ok(&router, case, REGION, &body(&window_id, MAX_TARGETS)).await;
    assert!(
        !minted.contains(&refilled),
        "case {case:?}: expected the register into the freed slot to mint its own id, got \
         {refilled:?}"
    );

    // A second window of the same Region is bounded on its own: the ceiling is per window.
    let other = window(&router, case, REGION, "other-ceiling-window").await;
    let elsewhere = register_ok(&router, case, REGION, &body(&other, 0)).await;
    let held = targets_of(&router, case, REGION, &other).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the other window to hold the one target registered with it, \
         got {held:?}"
    );
    assert_eq!(
        held[0]["WindowTargetId"].as_str(),
        Some(elsewhere.as_str()),
        "case {case:?}: expected the other window to hold its own target, got {held:?}"
    );
}
