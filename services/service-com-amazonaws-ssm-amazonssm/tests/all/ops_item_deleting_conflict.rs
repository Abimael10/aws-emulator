// SPDX-License-Identifier: CC0-1.0

//! The refusal every write of an OpsItem whose deletion has not settled yet carries.
//!
//! RULE: a write held against an OpsItem whose delete has been asked for and has not
//! settled is refused `OpsItemConflictException` (`smithy_query(query="operation",
//! operation="UpdateOpsItem")`: the exception the model declares for that state, awsQueryError
//! httpResponseCode 409) with the sentence Systems Manager words that class with, followed
//! by the envelope this service appends to a refusal it words itself — the service the
//! refusal came from, the status and the error code the class carries, the serving
//! response's own request id, and the closing proxy field. Every part of that message is
//! fixed except the request id, which is minted per call, and the refused write changes
//! nothing about the OpsItem.
//!
//! The rows are the operations of this service that refuse a write for that reason, each
//! driven through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`error_code`]).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, router_with_engine, REGION};

/// The error code the class is reported under, which is the modelled exception's own shape
/// name. Spelled out here rather than read from the crate, so the wording itself is what
/// the assertions check.
const CONFLICT_CODE: &str = "OpsItemConflictException";

/// The status the class carries, which is the one the model declares for it
/// (`OpsItemConflictException`, awsQueryError httpResponseCode 409).
const CONFLICT_STATUS: u16 = 409;

/// The sentence that refusal opens with: fixed text for the class, interpolating nothing
/// from the request — neither the OpsItem id nor the member the write carried.
const CONFLICT_BODY: &str = "Deletion of the provided OpsItem is already in progress";

/// The service the envelope names, which is the service shape the request targets
/// (`AmazonSSM.<Operation>`, this service's `X-Amz-Target`).
const ENVELOPE_SERVICE: &str = "AmazonSSM";

/// How long the delete of the case's OpsItem is held pending: long enough that it cannot
/// settle while the case runs, so nothing sleeps and nothing depends on how fast the host
/// runs.
const DELETE_HELD_PENDING: Duration = Duration::from_secs(3600);

/// Hold the delete of `ops_item_id` in [`REGION`] pending for the rest of the case.
fn hold_delete_pending(engine: &Arc<engine::InMemoryEngine>, ops_item_id: &str) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let held = store
        .ops_item_mut(REGION, ops_item_id)
        .unwrap_or_else(|| panic!("the deleting OpsItem {ops_item_id} must still be held"));
    held.delete_due = Some(SystemTime::now() + DELETE_HELD_PENDING);
}

/// The request id the refusal's envelope names, after holding the whole message against the
/// wording the class carries: the fixed sentence, then the service, the status and the error
/// code, then the request id, then the closing field.
///
/// Everything but the request id is fixed, so the request id is what this hands back for the
/// caller to hold against another refusal's.
fn request_id_of(case: &str, response: &Value) -> String {
    let message = response["Message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries a Message, got {response}"));
    let expected = format!(
        "{CONFLICT_BODY} (Service: {ENVELOPE_SERVICE}; Status Code: {CONFLICT_STATUS}; Error \
         Code: {CONFLICT_CODE}; Request ID: "
    );
    let request_id = message
        .strip_prefix(&expected)
        .and_then(|rest| rest.strip_suffix("; Proxy: null)"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {expected:?}<request id>\"; Proxy: null)\", got \
                 {message:?}"
            )
        });
    // A request id is minted per call as a hyphenated UUID, so the field carries one of
    // that form rather than anything of the request.
    let groups: Vec<&str> = request_id.split('-').collect();
    assert!(
        groups.len() == 5
            && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
            && request_id
                .chars()
                .all(|character| character == '-' || character.is_ascii_hexdigit()),
        "case {case:?}: expected a hyphenated UUID request id, got {request_id:?}"
    );
    request_id.to_string()
}

/// Create an OpsItem with one related item, answering `(OpsItemId, AssociationId)`.
async fn ops_item_with_related_item(router: &MultiServiceRouter) -> (String, String) {
    let (status, created) = call(
        router,
        "CreateOpsItem",
        &json!({
            "Title": "the work item whose delete is settling",
            "Description": "what the OpsItem is about",
            "Source": "emulator-tests",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateOpsItem must succeed, got {status} {created}"
    );
    let ops_item_id = created["OpsItemId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateOpsItem reports an OpsItemId, got {created}"))
        .to_string();

    let (status, associated) = call(
        router,
        "AssociateOpsItemRelatedItem",
        &json!({
            "OpsItemId": ops_item_id,
            "AssociationType": "RelatesTo",
            "ResourceType": "AWS::SSM::Document",
            "ResourceUri": format!("arn:aws:ssm:{REGION}:123456789012:document/AWS-RunShellScript"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "AssociateOpsItemRelatedItem must succeed, got {status} {associated}"
    );
    let association_id = associated["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("the association reports an AssociationId, got {associated}"))
        .to_string();
    (ops_item_id, association_id)
}

/// The `Status` the OpsItem reports, which is what a refused edit must leave alone.
async fn reported_status(router: &MultiServiceRouter, ops_item_id: &str) -> String {
    let (status, read) = call(router, "GetOpsItem", &json!({ "OpsItemId": ops_item_id })).await;
    assert_eq!(status, 200, "GetOpsItem must succeed, got {status} {read}");
    read["OpsItem"]["Status"]
        .as_str()
        .unwrap_or_else(|| panic!("a read reports the OpsItem's Status, got {read}"))
        .to_string()
}

/// RULE: every write refused because the OpsItem's deletion is already in progress carries
/// the one message for that class — the fixed sentence plus the envelope, whose only
/// variable part is the serving response's own request id — and changes nothing.
#[tokio::test]
async fn every_write_of_a_deleting_ops_item_is_refused_with_the_one_wording() {
    let (router, engine) = router_with_engine();
    let (ops_item_id, association_id) = ops_item_with_related_item(&router).await;
    let before = reported_status(&router, &ops_item_id).await;

    let (status, deleted) = call(
        &router,
        "DeleteOpsItem",
        &json!({ "OpsItemId": ops_item_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteOpsItem must succeed, got {status} {deleted}"
    );
    hold_delete_pending(&engine, &ops_item_id);

    // Each row is one write this service refuses for that reason: the two operations that
    // edit an OpsItem, the first with two different patches so the refusal is held against
    // more than one member.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "UpdateOpsItem moving the status",
            "UpdateOpsItem",
            json!({ "OpsItemId": ops_item_id, "Status": "Resolved" }),
        ),
        (
            "UpdateOpsItem retitling the work item",
            "UpdateOpsItem",
            json!({ "OpsItemId": ops_item_id, "Title": "a new title" }),
        ),
        (
            "UpdateOpsItem adding operational data",
            "UpdateOpsItem",
            json!({
                "OpsItemId": ops_item_id,
                "OperationalData": { "team": { "Value": "platform" } },
            }),
        ),
        (
            "DisassociateOpsItemRelatedItem dropping the related item",
            "DisassociateOpsItemRelatedItem",
            json!({ "OpsItemId": ops_item_id, "AssociationId": association_id }),
        ),
    ];

    let mut request_ids: Vec<String> = Vec::new();
    for (case, operation, body) in rows {
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the write must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            CONFLICT_CODE,
            "case {case:?}: expected the conflict refusal, got {response}"
        );
        request_ids.push(request_id_of(case, &response));
    }

    // The request id is the SERVING response's own, so no two refusals report the same one.
    for (index, request_id) in request_ids.iter().enumerate() {
        assert!(
            !request_ids[..index].contains(request_id),
            "the request id is minted per call, so {request_id:?} must not repeat in \
             {request_ids:?}"
        );
    }

    // Nothing the refused writes asked for happened.
    assert_eq!(
        reported_status(&router, &ops_item_id).await,
        before,
        "a refused edit leaves the OpsItem's status as it was"
    );
    let (status, listed) = call(
        &router,
        "ListOpsItemRelatedItems",
        &json!({ "OpsItemId": ops_item_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListOpsItemRelatedItems must succeed, got {status} {listed}"
    );
    let reported: Vec<String> = listed["Summaries"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing reports Summaries, got {listed}"))
        .iter()
        .map(|summary| {
            summary["AssociationId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        reported,
        vec![association_id],
        "a refused disassociation deletes nothing"
    );
}
