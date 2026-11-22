// SPDX-License-Identifier: CC0-1.0

//! The `Details` of a compliance item, from the put that wrote it to the read that reports it.
//!
//! RULE under test: **`ComplianceItem$Details` is "a 'Key': 'Value' tag combination for the
//! compliance item" — a MAP — so `ListComplianceItems` reports the member for every item it
//! reports, carrying exactly the pairs the item holds, and an item whose put carried no
//! `Details` (or an empty one) is reported with the EMPTY map rather than with the member left
//! out** (`smithy_query(query="shape", shape="ComplianceItem,ComplianceItemEntry")`). A caller
//! therefore never has to tell "this item has no details" from "this response does not say".
//!
//! The rows span what the rule generalises over: how many pairs an item carries (none, an
//! empty map, one, several), whether the put's other items carry any, which compliance type
//! the report is under, and how many items the put carries.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — and the one thing seeded through the engine is a
//! registered managed node, because no call of this service registers one.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, router_with_engine, REGION};

/// The node every case reports compliance about: an EC2 node id of the 17-character form the
/// modelled `InstanceId` pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// "`ManagedInstance` is currently the only supported resource type"
/// (`smithy_query(query="doc", shape="ComplianceItem", member="ResourceType")`).
const MANAGED_INSTANCE: &str = "ManagedInstance";

/// A compliance type of the caller's own namespace: "specify `Association` (for a State
/// Manager association), `Patch`, or `Custom:string`"
/// (`PutComplianceItemsRequest$ComplianceType`).
const CUSTOM_TYPE: &str = "Custom:DetailsProbe";

/// The instant every case reports its execution at, as epoch seconds — the form the awsJson1_1
/// wire carries a timestamp in.
const EXECUTION_TIME: i64 = 1_700_000_000;

/// The instant the seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// Register a node in `REGION` the way its own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, node_id: &str) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(node_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some("Linux".to_string());
    node.properties.instance_state = Some("Active".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// `PutComplianceItems` of `items` about the seeded node, under `compliance_type`.
async fn put(
    router: &MultiServiceRouter,
    compliance_type: &str,
    items: &Value,
) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "PutComplianceItems",
        &json!({
            "ResourceId": NODE,
            "ResourceType": MANAGED_INSTANCE,
            "ComplianceType": compliance_type,
            "ExecutionSummary": { "ExecutionTime": EXECUTION_TIME },
            "Items": items,
        }),
    )
    .await
}

/// The items the seeded node holds, read back the way a client does.
async fn items_of(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, reported) = call_in_region(
        router,
        REGION,
        "ListComplianceItems",
        &json!({ "ResourceIds": [NODE], "ResourceTypes": [MANAGED_INSTANCE] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {reported}"
    );
    reported["ComplianceItems"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a read reports ComplianceItems, got {reported}"))
        .clone()
}

/// RULE: a reported compliance item always carries `Details`, as the map of pairs its put
/// supplied — the empty map for an item that supplied none and for one that supplied an empty
/// map alike.
#[tokio::test]
async fn details_are_reported_as_the_map_the_put_supplied() {
    /// One row: the items a put carries, and the `Details` map each one must be reported with.
    struct Row {
        case: &'static str,
        compliance_type: &'static str,
        items: Value,
        expected: Vec<Value>,
    }

    let rows = [
        Row {
            case: "an item whose put carried no Details member at all",
            compliance_type: CUSTOM_TYPE,
            items: json!([{ "Id": "absent", "Severity": "HIGH", "Status": "NON_COMPLIANT" }]),
            expected: vec![json!({})],
        },
        Row {
            case: "an item whose put carried an empty Details map",
            compliance_type: CUSTOM_TYPE,
            items: json!([{
                "Id": "empty",
                "Severity": "HIGH",
                "Status": "NON_COMPLIANT",
                "Details": {},
            }]),
            expected: vec![json!({})],
        },
        Row {
            case: "an item carrying one pair",
            compliance_type: CUSTOM_TYPE,
            items: json!([{
                "Id": "one-pair",
                "Severity": "LOW",
                "Status": "COMPLIANT",
                "Details": { "PatchGroup": "prod" },
            }]),
            expected: vec![json!({ "PatchGroup": "prod" })],
        },
        Row {
            case: "an item carrying several pairs, one of them an empty value",
            compliance_type: "Association",
            items: json!([{
                "Id": "many-pairs",
                "Severity": "CRITICAL",
                "Status": "NON_COMPLIANT",
                "Details": {
                    "DocumentName": "AWS-RunPatchBaseline",
                    "DocumentVersion": "4",
                    "PatchBaselineId": "pb-0123456789abcdef0",
                    "Classification": "",
                },
            }]),
            expected: vec![json!({
                "DocumentName": "AWS-RunPatchBaseline",
                "DocumentVersion": "4",
                "PatchBaselineId": "pb-0123456789abcdef0",
                "Classification": "",
            })],
        },
        Row {
            case: "one put mixing items with and without Details, in submission order",
            compliance_type: "Patch",
            items: json!([
                { "Id": "KB4010320", "Severity": "LOW", "Status": "COMPLIANT" },
                {
                    "Id": "KB4010321",
                    "Severity": "MEDIUM",
                    "Status": "NON_COMPLIANT",
                    "Details": { "PatchState": "Missing" },
                },
                {
                    "Id": "KB4010322",
                    "Severity": "HIGH",
                    "Status": "COMPLIANT",
                    "Details": {},
                },
            ]),
            expected: vec![
                json!({}),
                json!({ "PatchState": "Missing" }),
                json!({}),
            ],
        },
        Row {
            case: "an item carrying no id and no Details",
            compliance_type: CUSTOM_TYPE,
            items: json!([{ "Severity": "UNSPECIFIED", "Status": "COMPLIANT" }]),
            expected: vec![json!({})],
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = router_with_engine();
        register(&engine, NODE);

        let (status, body) = put(&router, row.compliance_type, &row.items).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be served, got {status} with {body}"
        );

        let reported = items_of(&router, case).await;
        assert_eq!(
            reported.len(),
            row.expected.len(),
            "case {case:?}: expected one reported item per item the put carried, got {reported:?}"
        );
        for (item, expected) in reported.iter().zip(row.expected.iter()) {
            assert_eq!(
                item.get("Details"),
                Some(expected),
                "case {case:?}: expected Details {expected}, got {item}"
            );
        }
    }
}

/// RULE: the map is held per item and per compliance type, so a later put of one type replaces
/// its own items' details and leaves another type's standing — and an overwrite that drops the
/// pairs reports the empty map, not the pairs it replaced.
#[tokio::test]
async fn an_overwrite_reports_the_details_of_the_last_put() {
    let case = "overwriting the details of one compliance type";
    let (router, engine) = router_with_engine();
    register(&engine, NODE);

    let (status, body) = put(
        &router,
        "Association",
        &json!([{
            "Id": "assoc-1",
            "Severity": "HIGH",
            "Status": "NON_COMPLIANT",
            "Details": { "DocumentName": "AWS-RunPatchBaseline" },
        }]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first put must be served, got {status} with {body}"
    );

    let (status, body) = put(
        &router,
        CUSTOM_TYPE,
        &json!([{ "Id": "probe-1", "Severity": "LOW", "Status": "COMPLIANT" }]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the second put must be served, got {status} with {body}"
    );

    // The same association item, re-reported with no details at all: the overwrite is what the
    // resource holds of that type afterwards, so its details are the empty map.
    let (status, body) = put(
        &router,
        "Association",
        &json!([{ "Id": "assoc-1", "Severity": "HIGH", "Status": "COMPLIANT" }]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the overwrite must be served, got {status} with {body}"
    );

    let reported = items_of(&router, case).await;
    let details: Vec<(Option<&str>, Option<&Value>)> = reported
        .iter()
        .map(|item| (item["Id"].as_str(), item.get("Details")))
        .collect();
    let empty = json!({});
    assert_eq!(
        details,
        vec![
            (Some("assoc-1"), Some(&empty)),
            (Some("probe-1"), Some(&empty)),
        ],
        "case {case:?}: expected both items reported with the empty Details map, got {reported:?}"
    );
}
