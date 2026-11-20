// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ASYNCHRONOUS teardown `DeleteWorkspaceInstance`
//! performs, driven over the wire this service really speaks (awsJson1_0, `POST /`,
//! `X-Amz-Target: EUCMIFrontendAPIService.<OperationName>`).
//!
//! The rule under test: deleting a resource this service tears down asynchronously
//! ACCEPTS the request and starts the teardown — it does not make the resource
//! disappear inside the call. The instance keeps being reported, by
//! `GetWorkspaceInstance` and by `ListWorkspaceInstances` (including under a
//! `ProvisionStates` filter naming the state it is in), while it moves through the
//! transient `DEALLOCATING` to the terminal `DEALLOCATED` — and it keeps being reported
//! THERE: `DEALLOCATED` is a modelled lifecycle value, not the removal of the record,
//! so everything read off it (its id, its managed EC2 instance, its billing
//! configuration, its tags) stays readable afterwards.
//!
//! Every row below creates a DIFFERENT instance — differing in the state it is in
//! when the delete arrives, in whether it carries a `ClientToken` or tags, and in the
//! shape of its configuration — because the rule is about the delete path, not about
//! one recorded request. Nothing here can pass against a canned answer: every
//! assertion reads the instance back out of the store through `Get` or `List`.
//!
//! The wire helpers come from the sibling `workspace_instance_lifecycle` suite.

use std::time::Instant;

use serde_json::{json, Value};
use service_com_amazonaws_workspacesinstances_eucmifrontendapiservice as wsinst;
use smithy_server_runtime::MultiServiceRouter;

use crate::workspace_instance_lifecycle::{
    call, create, harness, launchable_managed_instance, list_all_ids, minimal_create, ok,
    poll_until_provision_state, provision_state, DEFAULT_TEST_REGION, TEST_IMAGE_ID,
};

/// Every value `ProvisionStateEnum` declares, as the generated constant the model
/// drives (`smithy_query(query="shape",shape="ProvisionStateEnum")`), so this suite
/// cannot drift from the model's value list.
const EVERY_PROVISION_STATE: &[&str] = wsinst::ops::types::PROVISION_STATE_ENUM_VALUES;

/// One instance to delete. The rows differ along what the rule generalises over: the
/// `ProvisionState` the instance is in when the delete arrives, and the stored
/// record's shape (its idempotency token, its tags, its configuration).
struct DeleteRow {
    case: &'static str,
    body: Value,
    /// Whether to let the `ALLOCATING -> ALLOCATED` move fall due before deleting, so
    /// the table spans a delete of a transient instance and of a settled one.
    settle_first: bool,
}

fn delete_rows() -> Vec<DeleteRow> {
    vec![
        DeleteRow {
            case: "deleted while still ALLOCATING",
            body: minimal_create(),
            settle_first: false,
        },
        DeleteRow {
            case: "deleted once it reached ALLOCATED",
            body: minimal_create(),
            settle_first: true,
        },
        DeleteRow {
            case: "an instance created with an idempotency token",
            body: json!({
                "ClientToken": "tok-teardown",
                "ManagedInstance": launchable_managed_instance(),
            }),
            settle_first: true,
        },
        DeleteRow {
            case: "an instance carrying tags",
            body: json!({
                "ManagedInstance": launchable_managed_instance(),
                "Tags": [{ "Key": "env", "Value": "dev" }, { "Key": "owner", "Value": "alice" }]
            }),
            settle_first: false,
        },
        DeleteRow {
            case: "an instance with a nested configuration and monthly billing",
            body: json!({
                "ManagedInstance": {
                    "ImageId": TEST_IMAGE_ID,
                    "InstanceType": "m5.large",
                    "Placement": { "AvailabilityZone": "us-east-1a", "Tenancy": "default" },
                    "CpuOptions": { "CoreCount": 1, "ThreadsPerCore": 2 }
                },
                "BillingConfiguration": { "BillingMode": "MONTHLY" }
            }),
            settle_first: true,
        },
    ]
}

/// What the STORE holds for `id` right after a delete: the `ProvisionState` the
/// teardown put the record in, and the instant the `DEALLOCATING -> DEALLOCATED` move
/// falls due.
///
/// Read straight off the record rather than over the wire, because the wire cannot show
/// a deadline and because a read of the store does not settle anything: the transient
/// state the delete left is still there to be asserted however long this test takes,
/// where a reporting call would have moved it on and raced the clock.
fn stored_teardown(engine: &engine::InMemoryEngine, id: &str) -> (String, Instant) {
    let store = wsinst::state::service_state(engine).expect("the service's own store");
    let store = store.lock();
    let record = store
        .instances
        .get(id)
        .unwrap_or_else(|| panic!("the delete must have left the record for {id:?} stored"));
    let due = record
        .deallocation_due
        .unwrap_or_else(|| panic!("the delete must have stored a teardown deadline for {id:?}"));
    (record.provision_state.clone(), due)
}

/// `GetWorkspaceInstance`, as the (HTTP status, body) pair.
async fn get(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(
        router,
        "GetWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await
}

#[tokio::test]
async fn delete_starts_a_teardown_the_clock_carries_through_to_the_terminal_state() {
    for DeleteRow {
        case,
        body,
        settle_first,
    } in delete_rows()
    {
        let (router, engine) = harness();
        let id = create(&router, body).await;
        // A second instance nobody deletes: every "selects only the deleted one"
        // assertion below has to be about the deleted one, not about a store of one.
        let survivor = create(&router, minimal_create()).await;
        if settle_first {
            // Waited for by LOOKING, not by sleeping a span: the table spans a delete
            // of a transient instance and of a settled one.
            poll_until_provision_state(&router, &id, "ALLOCATED").await;
        }

        // 1. The delete is ACCEPTED, it leaves the record in the TRANSIENT
        //    `DEALLOCATING`, and the teardown it starts is never instantaneous.
        let before_delete = Instant::now();
        ok(
            &router,
            "DeleteWorkspaceInstance",
            json!({ "WorkspaceInstanceId": id }),
        )
        .await;
        let (stored_state, due) = stored_teardown(&engine, &id);
        assert_eq!(
            stored_state, "DEALLOCATING",
            "case {case:?}: expected the delete to leave the record in the transient DEALLOCATING, got {stored_state:?}"
        );
        assert!(
            due >= before_delete + wsinst::state::DEALLOCATION_SPAN,
            "case {case:?}: expected the DEALLOCATED move to fall due no sooner than one span after the delete, got a deadline {:?} earlier",
            (before_delete + wsinst::state::DEALLOCATION_SPAN).saturating_duration_since(due)
        );

        // 2. The instance is still reported over the wire while it tears down, in a
        //    deallocation state, and the `ProvisionStates` filter selects it on the
        //    state it is in rather than on the allocation states it has left. WHICH of
        //    the two deallocation states a read lands in is the clock's to say, so
        //    nothing here depends on how fast this host runs.
        let (status, payload) = get(&router, &id).await;
        let reported = payload["ProvisionState"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let filtered = list_all_ids(&router, json!({ "ProvisionStates": [&reported] })).await;
        let allocated_filter =
            list_all_ids(&router, json!({ "ProvisionStates": ["ALLOCATING", "ALLOCATED"] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the instance to still be reported while it tears down, got {status} with {payload}"
        );
        assert!(
            ["DEALLOCATING", "DEALLOCATED"].contains(&reported.as_str()),
            "case {case:?}: expected a deallocation state right after the delete, got {reported:?}"
        );
        assert_eq!(
            filtered,
            vec![id.clone()],
            "case {case:?}: expected ProvisionStates=[{reported}] to select only {id:?}, got {filtered:?}"
        );
        assert_eq!(
            allocated_filter,
            vec![survivor.clone()],
            "case {case:?}: expected the allocation-state filter to select only the instance nobody deleted, got {allocated_filter:?}"
        );

        // 3. The clock carries the teardown through to the terminal DEALLOCATED, and
        //    the filter follows it there.
        let settled = poll_until_provision_state(&router, &id, "DEALLOCATED").await;
        let filtered = list_all_ids(&router, json!({ "ProvisionStates": ["DEALLOCATED"] })).await;
        let still_deallocating =
            list_all_ids(&router, json!({ "ProvisionStates": ["DEALLOCATING"] })).await;
        assert_eq!(
            filtered,
            vec![id.clone()],
            "case {case:?}: expected ProvisionStates=[DEALLOCATED] to select only {id:?}, got {filtered:?}"
        );
        assert!(
            !still_deallocating.contains(&id),
            "case {case:?}: expected {id:?} to have left DEALLOCATING, got {still_deallocating:?}"
        );

        // 4. And DEALLOCATED is where the record STAYS: the terminal value is a state of
        //    the resource, not its removal, so every later read answers the SAME stored
        //    record — nothing drops it — and so does every view beside `Get`.
        for repeat in 1..=3 {
            let (status, again) = get(&router, &id).await;
            assert_eq!(
                (status, &again),
                (200, &settled),
                "case {case:?}: read {repeat} after the teardown expected the retained record {settled}, got {status} with {again}"
            );
        }
        let (status, tags) = call(
            &router,
            "ListTagsForResource",
            json!({ "WorkspaceInstanceId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the torn-down instance's tags to still be readable, got {status} with {tags}"
        );
        let mut listed = list_all_ids(&router, json!({})).await;
        listed.sort();
        let mut expected = vec![id.clone(), survivor.clone()];
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the torn-down instance to still be listed beside the one nobody deleted, got {listed:?}"
        );
        for state in EVERY_PROVISION_STATE {
            let selected = list_all_ids(&router, json!({ "ProvisionStates": [state] })).await;
            let selects = *state == "DEALLOCATED";
            assert_eq!(
                selected.contains(&id),
                selects,
                "case {case:?}: expected ProvisionStates=[{state}] to {} the torn-down {id:?}, got {selected:?}",
                if selects { "select" } else { "leave out" }
            );
        }
    }
}

#[tokio::test]
async fn a_repeated_delete_is_accepted_and_does_not_push_the_teardown_out() {
    // Delete is idempotent in effect: a repeat call names the end state the instance
    // is already heading to, so it is accepted and leaves the teardown deadline where
    // the FIRST delete put it — repeated calls cannot keep a resource alive.
    let (router, engine) = harness();
    let id = create(&router, minimal_create()).await;

    ok(
        &router,
        "DeleteWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;
    let (stored_state, first_due) = stored_teardown(&engine, &id);
    assert_eq!(
        stored_state, "DEALLOCATING",
        "case the first delete: expected the transient DEALLOCATING, got {stored_state:?}"
    );

    for repeat in 2..=4 {
        let (status, payload) = call(
            &router,
            "DeleteWorkspaceInstance",
            json!({ "WorkspaceInstanceId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case delete number {repeat}: expected HTTP 200 for an instance already tearing down, got {status} with {payload}"
        );
        let (_, due) = stored_teardown(&engine, &id);
        assert_eq!(
            due, first_due,
            "case delete number {repeat}: expected the teardown deadline to stay where the first delete put it, got a deadline moved by {:?}",
            due.saturating_duration_since(first_due)
        );
    }

    // A repeat once the teardown has FINISHED does not move the record back to the
    // transient state either, and does not take it out of the store: the instance is
    // already where the delete asks for.
    poll_until_provision_state(&router, &id, "DEALLOCATED").await;
    let (status, payload) = call(
        &router,
        "DeleteWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case delete a torn-down instance: expected HTTP 200, got {status} with {payload}"
    );
    let (status, payload) = get(&router, &id).await;
    let reported = payload["ProvisionState"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        (status, reported.as_str()),
        (200, "DEALLOCATED"),
        "case delete a torn-down instance: expected it to stay DEALLOCATED, got {status} with {payload}"
    );
    assert_eq!(
        stored_teardown(&engine, &id).1,
        first_due,
        "case delete a torn-down instance: expected the teardown deadline to stay where the first delete put it"
    );
}

#[tokio::test]
async fn an_instance_tearing_down_no_longer_holds_a_running_instance_slot() {
    // The quota this emulator enforces counts RUNNING instances (a chosen semantic,
    // documented on `state::MAX_INSTANCES`): a delete frees the slot as soon as it is
    // accepted, while the record it leaves behind is still reported.
    let (router, engine) = harness();
    let id = create(&router, minimal_create()).await;

    ok(
        &router,
        "DeleteWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;

    let store = wsinst::state::service_state(&*engine).expect("the service's own store");
    let (held, running) = {
        let state = store.lock();
        // The quota is counted per account per Region, so the count is asked for the
        // Region these calls were signed for.
        (
            state.instances.len(),
            state.running_instance_count(DEFAULT_TEST_REGION),
        )
    };
    assert_eq!(
        (held, running),
        (1, 0),
        "case right after the delete: expected the record to be held but not counted as running, got {held} held and {running} running"
    );

    // The record is still reported for the caller, which is the point: the slot is
    // free and the instance is visible.
    let state = provision_state(&router, &id).await;
    assert_eq!(
        state, "DEALLOCATING",
        "case right after the delete: expected the record to still report DEALLOCATING, got {state:?}"
    );
}

#[tokio::test]
async fn a_tag_write_and_a_tag_read_both_still_reach_an_instance_tearing_down() {
    // A resource that is tearing down still EXISTS, so the operations that resolve an
    // instance id keep resolving it — they do not answer not-found early.
    let (router, _engine) = harness();
    let id = create(
        &router,
        json!({
            "ManagedInstance": launchable_managed_instance(),
            "Tags": [{ "Key": "env", "Value": "dev" }],
        }),
    )
    .await;

    ok(
        &router,
        "DeleteWorkspaceInstance",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;

    let tags = ok(
        &router,
        "ListTagsForResource",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;
    assert_eq!(
        tags["Tags"][0]["Key"].as_str(),
        Some("env"),
        "case read the tags of an instance tearing down: expected the stored tag, got {tags}"
    );

    let (status, payload) = call(
        &router,
        "TagResource",
        json!({ "WorkspaceInstanceId": id, "Tags": [{ "Key": "owner", "Value": "alice" }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case tag an instance tearing down: expected HTTP 200, got {status} with {payload}"
    );
    let tags = ok(
        &router,
        "ListTagsForResource",
        json!({ "WorkspaceInstanceId": id }),
    )
    .await;
    let keys: Vec<&str> = tags["Tags"]
        .as_array()
        .unwrap_or_else(|| panic!("ListTagsForResource returned no Tags: {tags}"))
        .iter()
        .map(|tag| tag["Key"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        keys,
        vec!["env", "owner"],
        "case tag an instance tearing down: expected the merged tag set, got {keys:?}"
    );
}

/// One instance to drive to the END of its lifecycle, with the `BillingMode` the
/// create settles for it.
///
/// The rows differ along what the rule generalises over: the billing configuration
/// (named and defaulted), the shape of the launch configuration (flat, nested, with a
/// placement), whether the create carried an idempotency token, and whether the
/// instance carries tags — none of which the terminal read may lose.
struct RetainedRow {
    case: &'static str,
    body: Value,
    billing_mode: &'static str,
}

fn retained_rows() -> Vec<RetainedRow> {
    vec![
        RetainedRow {
            case: "a create that named no billing configuration",
            body: minimal_create(),
            // "Defaults to hourly billing if not specified"
            // (`CreateWorkspaceInstance$BillingConfiguration`).
            billing_mode: "HOURLY",
        },
        RetainedRow {
            case: "a create that asked for monthly billing, with a nested configuration",
            body: json!({
                "ManagedInstance": {
                    "ImageId": TEST_IMAGE_ID,
                    "InstanceType": "m5.large",
                    "Placement": { "AvailabilityZone": "us-east-1a", "Tenancy": "default" },
                    "CpuOptions": { "CoreCount": 1, "ThreadsPerCore": 2 }
                },
                "BillingConfiguration": { "BillingMode": "MONTHLY" }
            }),
            billing_mode: "MONTHLY",
        },
        RetainedRow {
            case: "a create carrying an idempotency token and tags",
            body: json!({
                "ClientToken": "tok-retained",
                "ManagedInstance": launchable_managed_instance(),
                "BillingConfiguration": { "BillingMode": "HOURLY" },
                "Tags": [{ "Key": "env", "Value": "dev" }, { "Key": "owner", "Value": "alice" }]
            }),
            billing_mode: "HOURLY",
        },
    ]
}

#[tokio::test]
async fn a_deallocated_instance_is_still_read_back_whole() {
    // The RULE: reaching the terminal `DEALLOCATED` changes the instance's state and
    // nothing else. Every other member `GetWorkspaceInstance` reports — the id, the
    // managed EC2 instance it was allocated, the active billing configuration — is read
    // out of the record that is still stored, so a caller that polls the end of the
    // deallocation lifecycle reads the whole resource there.
    for RetainedRow {
        case,
        body,
        billing_mode,
    } in retained_rows()
    {
        let (router, _engine) = harness();
        let id = create(&router, body).await;
        let allocated = poll_until_provision_state(&router, &id, "ALLOCATED").await;
        let managed_instance = allocated["EC2ManagedInstance"]["InstanceId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the allocated instance reported no managed EC2 instance: {allocated}")
            })
            .to_string();

        ok(
            &router,
            "DeleteWorkspaceInstance",
            json!({ "WorkspaceInstanceId": id }),
        )
        .await;
        let deallocated = poll_until_provision_state(&router, &id, "DEALLOCATED").await;

        assert_eq!(
            deallocated["WorkspaceInstanceId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the deallocated instance to report its own id, got {deallocated}"
        );
        assert_eq!(
            deallocated["EC2ManagedInstance"]["InstanceId"].as_str(),
            Some(managed_instance.as_str()),
            "case {case:?}: expected the deallocated instance to still name the managed EC2 instance {managed_instance:?}, got {deallocated}"
        );
        assert_eq!(
            deallocated["BillingConfiguration"]["BillingMode"].as_str(),
            Some(billing_mode),
            "case {case:?}: expected the billing configuration the instance was created with, got {deallocated}"
        );
        // Nothing else moved: the terminal state is the ONLY difference between the two
        // reads, which is what "the record is retained" means on the wire.
        let mut expected = allocated.clone();
        expected["ProvisionState"] = json!("DEALLOCATED");
        assert_eq!(
            deallocated, expected,
            "case {case:?}: expected the deallocation to change only the ProvisionState, got {deallocated} beside {allocated}"
        );
    }
}

