// SPDX-License-Identifier: CC0-1.0

//! The compliance a caller reports about its managed nodes, end to end: `PutComplianceItems`
//! writes the items and `ListComplianceItems` reads them back.
//!
//! RULE under test: **`PutComplianceItems` "registers a compliance type and other compliance
//! details on a designated resource", so the items a put carries become the compliance that
//! resource holds under the put's one `ComplianceType` — "this call overwrites existing
//! compliance information on the resource, so you must provide a full list of compliance
//! items each time that you send the request" — and what the put persisted is what
//! `ListComplianceItems` reports** (`smithy_query(query="operation",
//! operation="PutComplianceItems,ListComplianceItems")`). Around that write: `ManagedInstance`
//! is "currently the only supported resource type", so any other resource type is refused
//! `InvalidResourceType` naming the rejected type and the supported ones; a `ResourceId`
//! naming no managed node of the Region is refused `InvalidResourceId`; a resource may hold
//! reports of at most ten CUSTOM compliance types
//! (`ComplianceTypeCountLimitExceededException`: "you can specify a maximum of 10 different
//! types"); the caller's own identity-based policies decide the call before any of that; and
//! every refusal writes nothing.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call_in_region`], [`error_code`], [`error_message`],
//! [`router_with_engine`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. The ONE thing seeded through the engine is a registered
//! managed node, because no call of this service registers one (the agent's own call is not
//! in this model); every compliance item is written through `PutComplianceItems` itself.
//!
//! The rows span what the rules generalise over: how many items a put carries, which members
//! each item holds, which compliance type it is reported under, which resource and Region
//! holds it, the `UploadType` and `ItemContentHash` the put carries, the filter key and
//! comparison a read narrows by, the page size it asks for, and which principal signs the
//! call.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::just_in_time_node_access::{assumed_role, call_as};
use crate::parameter_store::{
    call_in_region, error_code, error_message, router_with_engine, ACCOUNT, REGION,
};

/// A second Region, to pin that compliance is written and read through the one Region its put
/// was signed for. The first is the harness's own [`crate::parameter_store::REGION`].
const OTHER_REGION: &str = "us-east-1";

/// The node every case reports compliance about: an EC2 node id of the 17-character form the
/// modelled `InstanceId` pattern admits (`^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const NODE: &str = "i-0123456789abcdef0";

/// A second registered node, so a case can pin that compliance is about ONE resource.
const OTHER_NODE: &str = "i-0abc1234";

/// A node id that is well formed and names no registered node.
const UNREGISTERED: &str = "i-0dead999";

/// The one compliance resource type Systems Manager supports: "`ManagedInstance` is currently
/// the only supported resource type"
/// (`smithy_query(query="doc", shape="PutComplianceItemsRequest", member="ResourceType")`).
const MANAGED_INSTANCE: &str = "ManagedInstance";

/// A compliance type of the caller's own namespace: "specify `Association` (for a State
/// Manager association), `Patch`, or `Custom:string`"
/// (`PutComplianceItemsRequest$ComplianceType`).
const CUSTOM_TYPE: &str = "Custom:Probe";

/// The instant every case reports its execution at, as epoch seconds — the form the
/// awsJson1_1 wire carries a timestamp in.
const EXECUTION_TIME: i64 = 1_700_000_000;

/// The same instant in the format the API fixes for it, which is what a read filtering on
/// `ExecutionTime` compares against: "a datetime object that is saved in the following
/// format: `yyyy-MM-dd'T'HH:mm:ss'Z'`" (`ComplianceExecutionSummary$ExecutionTime`).
const EXECUTION_TIME_RENDERED: &str = "2023-11-14T22:13:20Z";

/// The instant every seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// How this service words a `Filters` entry it cannot narrow by, which is the one sentence its
/// unknown-filter refusals carry
/// (`service_com_amazonaws_ssm_amazonssm::state::compliance_items::INVALID_COMPLIANCE_FILTER`).
const INVALID_FILTER: &str =
    service_com_amazonaws_ssm_amazonssm::state::compliance_items::INVALID_COMPLIANCE_FILTER;

/// The access key id the account's own published credential presents.
const ACCOUNT_KEY: &str = "AKIDTEST";

/// A router with just this service mounted, and the engine behind it — the handle a case
/// seeds the node store through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Register a node in `region` the way its own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, node_id: &str) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(node_id, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some("Linux".to_string());
    node.properties.instance_state = Some("Active".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// One compliance item as a put carries it, with only the members a row names.
fn item(id: Option<&str>, severity: &str, status: &str) -> Value {
    let mut entry = json!({ "Severity": severity, "Status": status });
    if let Some(id) = id {
        entry["Id"] = json!(id);
    }
    entry
}

/// The `ExecutionSummary` every put carries: the required execution time, plus the execution
/// that reported it where a case names one.
fn summary(execution_id: Option<&str>, execution_type: Option<&str>) -> Value {
    let mut summary = json!({ "ExecutionTime": EXECUTION_TIME });
    if let Some(id) = execution_id {
        summary["ExecutionId"] = json!(id);
    }
    if let Some(execution_type) = execution_type {
        summary["ExecutionType"] = json!(execution_type);
    }
    summary
}

/// `PutComplianceItems` of `items` about `resource_id`, signed for `region`.
async fn put(
    router: &MultiServiceRouter,
    region: &str,
    resource_type: &str,
    resource_id: &str,
    compliance_type: &str,
    items: Value,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "PutComplianceItems",
        &json!({
            "ResourceId": resource_id,
            "ResourceType": resource_type,
            "ComplianceType": compliance_type,
            "ExecutionSummary": summary(None, None),
            "Items": items,
        }),
    )
    .await
}

/// The same put, with the whole body in the case's hands — for the rows that carry an
/// `UploadType`, an `ItemContentHash` or an execution summary of their own.
async fn put_body(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "PutComplianceItems", body).await
}

/// The compliance items `ListComplianceItems` reports for `body`, which must be served.
async fn listed(router: &MultiServiceRouter, case: &str, region: &str, body: &Value) -> Vec<Value> {
    let (status, reported) = call_in_region(router, region, "ListComplianceItems", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must be served, got {status} with {reported}"
    );
    reported["ComplianceItems"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a read reports ComplianceItems, got {reported}"))
        .clone()
}

/// The items one node holds, read back the way a client does.
async fn items_of(router: &MultiServiceRouter, case: &str, region: &str, node: &str) -> Vec<Value> {
    listed(
        router,
        case,
        region,
        &json!({ "ResourceIds": [node], "ResourceTypes": [MANAGED_INSTANCE] }),
    )
    .await
}

/// The `Id`s a read reported, in the order it reported them.
fn ids(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: what a put persisted is what a later read reports — one item per entry the put
/// carried, in the order it carried them, with every member read back exactly as submitted and
/// the resource and compliance type the put addressed written onto each.
#[tokio::test]
async fn a_put_is_read_back_as_the_compliance_of_that_resource_and_type() {
    /// One row: the compliance type the put reports under and the items it carries.
    struct Row {
        case: &'static str,
        compliance_type: &'static str,
        items: Value,
    }

    let rows = [
        Row {
            case: "one item carrying only the required members",
            compliance_type: CUSTOM_TYPE,
            items: json!([{ "Severity": "HIGH", "Status": "NON_COMPLIANT" }]),
        },
        Row {
            case: "one item carrying every member",
            compliance_type: CUSTOM_TYPE,
            items: json!([{
                "Id": "item-1",
                "Title": "Security Update for Active Directory",
                "Severity": "CRITICAL",
                "Status": "COMPLIANT",
                "Details": { "PatchGroup": "prod", "PatchSeverity": "Critical" },
            }]),
        },
        Row {
            case: "several items under a predefined compliance type",
            compliance_type: "Patch",
            items: json!([
                item(Some("KB4010320"), "LOW", "COMPLIANT"),
                item(Some("KB4010321"), "MEDIUM", "NON_COMPLIANT"),
                item(Some("KB4010322"), "INFORMATIONAL", "COMPLIANT"),
            ]),
        },
        Row {
            case: "an association report whose items carry no id",
            compliance_type: "Association",
            items: json!([
                item(None, "UNSPECIFIED", "COMPLIANT"),
                item(None, "UNSPECIFIED", "NON_COMPLIANT"),
            ]),
        },
        Row {
            case: "a put whose ids descend, which the read keeps in submission order",
            compliance_type: CUSTOM_TYPE,
            items: json!([
                item(Some("item-9"), "LOW", "COMPLIANT"),
                item(Some("item-5"), "MEDIUM", "NON_COMPLIANT"),
                item(Some("item-1"), "HIGH", "NON_COMPLIANT"),
            ]),
        },
        Row {
            case: "a put carrying no item at all",
            compliance_type: CUSTOM_TYPE,
            items: json!([]),
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, engine) = service();
        register(&engine, REGION, NODE);

        let (status, body) = put(
            &router,
            REGION,
            MANAGED_INSTANCE,
            NODE,
            row.compliance_type,
            row.items.clone(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put must be served, got {status} with {body}"
        );

        let read = items_of(&router, case, REGION, NODE).await;
        let submitted = row.items.as_array().cloned().unwrap_or_default();
        assert_eq!(
            read.len(),
            submitted.len(),
            "case {case:?}: expected one reported item per item the put carried, got {read:?}"
        );
        for (reported, wrote) in read.iter().zip(submitted.iter()) {
            for member in ["Id", "Title", "Severity", "Status", "Details"] {
                assert_eq!(
                    reported.get(member),
                    wrote.get(member),
                    "case {case:?}: {member} is what the put supplied, got {reported}"
                );
            }
            assert_eq!(
                (
                    reported["ComplianceType"].as_str(),
                    reported["ResourceType"].as_str(),
                    reported["ResourceId"].as_str()
                ),
                (
                    Some(row.compliance_type),
                    Some(MANAGED_INSTANCE),
                    Some(NODE)
                ),
                "case {case:?}: expected the resource and type the put addressed, got {reported}"
            );
            assert_eq!(
                reported["ExecutionSummary"]["ExecutionTime"].as_f64(),
                Some(EXECUTION_TIME as f64),
                "case {case:?}: expected the execution time the put reported, got {reported}"
            );
        }
    }
}

/// RULE: the execution summary a put reports under is persisted with its items, and a put is
/// scoped to the Region it was signed for and to the resource it named.
#[tokio::test]
async fn compliance_is_held_per_region_resource_and_execution() {
    let case = "the scoping case";
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    register(&engine, REGION, OTHER_NODE);
    register(&engine, OTHER_REGION, NODE);

    for (region, node, execution) in [
        (REGION, NODE, "execution-here"),
        (REGION, OTHER_NODE, "execution-other-node"),
        (OTHER_REGION, NODE, "execution-elsewhere"),
    ] {
        let (status, body) = put_body(
            &router,
            region,
            &json!({
                "ResourceId": node,
                "ResourceType": MANAGED_INSTANCE,
                "ComplianceType": CUSTOM_TYPE,
                "ExecutionSummary": summary(Some(execution), Some("Command")),
                "Items": [item(Some("item-1"), "HIGH", "NON_COMPLIANT")],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the put for {node} at {region} must be served, got {status} with \
             {body}"
        );
    }

    for (region, node, execution) in [
        (REGION, NODE, "execution-here"),
        (REGION, OTHER_NODE, "execution-other-node"),
        (OTHER_REGION, NODE, "execution-elsewhere"),
    ] {
        let read = items_of(&router, case, region, node).await;
        assert_eq!(
            read.len(),
            1,
            "case {case:?}: expected {node} at {region} to hold the one item its put wrote, got \
             {read:?}"
        );
        assert_eq!(
            (
                read[0]["ExecutionSummary"]["ExecutionId"].as_str(),
                read[0]["ExecutionSummary"]["ExecutionType"].as_str()
            ),
            (Some(execution), Some("Command")),
            "case {case:?}: expected the execution {execution:?} the put reported, got {:?}",
            read[0]
        );
    }
}

/// RULE: a put overwrites the items the resource holds of ITS compliance type and leaves every
/// other compliance type standing; a `PARTIAL` put overwrites only the items whose `Id` it
/// names; and a put whose `ItemContentHash` matches the held one is ignored.
#[tokio::test]
async fn a_put_overwrites_its_own_compliance_type() {
    let case = "the overwrite case";
    let (router, engine) = service();
    register(&engine, REGION, NODE);

    // The full list, then a shorter one of the same compliance type: the second put is what
    // the resource holds of it.
    put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        CUSTOM_TYPE,
        json!([
            item(Some("item-1"), "HIGH", "NON_COMPLIANT"),
            item(Some("item-2"), "LOW", "COMPLIANT"),
        ]),
    )
    .await;
    put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        "Patch",
        json!([item(Some("KB1"), "CRITICAL", "COMPLIANT")]),
    )
    .await;
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        CUSTOM_TYPE,
        json!([item(Some("item-3"), "MEDIUM", "COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the overwriting put must be served, got {status} with {body}"
    );

    let custom = listed(
        &router,
        case,
        REGION,
        &json!({
            "ResourceIds": [NODE],
            "Filters": [{ "Key": "ComplianceType", "Values": [CUSTOM_TYPE] }],
        }),
    )
    .await;
    assert_eq!(
        ids(&custom),
        vec!["item-3"],
        "case {case:?}: expected the second put to replace the items of its own compliance \
         type, got {custom:?}"
    );
    let patches = listed(
        &router,
        case,
        REGION,
        &json!({
            "ResourceIds": [NODE],
            "Filters": [{ "Key": "ComplianceType", "Values": ["Patch"] }],
        }),
    )
    .await;
    assert_eq!(
        ids(&patches),
        vec!["KB1"],
        "case {case:?}: expected another compliance type to be left standing, got {patches:?}"
    );

    // PARTIAL: only the item the put names is replaced.
    put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        CUSTOM_TYPE,
        json!([
            item(Some("item-3"), "MEDIUM", "COMPLIANT"),
            item(Some("item-4"), "LOW", "COMPLIANT"),
        ]),
    )
    .await;
    let (status, body) = put_body(
        &router,
        REGION,
        &json!({
            "ResourceId": NODE,
            "ResourceType": MANAGED_INSTANCE,
            "ComplianceType": CUSTOM_TYPE,
            "UploadType": "PARTIAL",
            "ExecutionSummary": summary(None, None),
            "Items": [item(Some("item-4"), "CRITICAL", "NON_COMPLIANT")],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the partial put must be served, got {status} with {body}"
    );
    let after = listed(
        &router,
        case,
        REGION,
        &json!({
            "ResourceIds": [NODE],
            "Filters": [{ "Key": "ComplianceType", "Values": [CUSTOM_TYPE] }],
        }),
    )
    .await;
    assert_eq!(
        ids(&after),
        vec!["item-3", "item-4"],
        "case {case:?}: expected a partial put to keep the items it does not name, got {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .map(|held| held["Severity"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["MEDIUM", "CRITICAL"],
        "case {case:?}: expected the partial put to have replaced the item it names, got \
         {after:?}"
    );

    // The content hash: a put carrying the hash the resource already holds for that compliance
    // type is ignored, and the hash a LATER put recorded is the one a repeat is compared
    // against — so a put carrying the earlier hash is served again.
    let first_hash = "d41d8cd98f00b204e9800998ecf8427e";
    let second_hash = "ad0234829205b9033196ba818f7a872b";
    let hashed = |hash: &str, items: Value| {
        json!({
            "ResourceId": NODE,
            "ResourceType": MANAGED_INSTANCE,
            "ComplianceType": "Association",
            "ItemContentHash": hash,
            "ExecutionSummary": summary(None, None),
            "Items": items,
        })
    };
    async fn associations(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
        listed(
            router,
            case,
            REGION,
            &json!({
                "ResourceIds": [NODE],
                "Filters": [{ "Key": "ComplianceType", "Values": ["Association"] }],
            }),
        )
        .await
    }
    put_body(
        &router,
        REGION,
        &hashed(first_hash, json!([item(Some("assoc-1"), "HIGH", "COMPLIANT")])),
    )
    .await;
    let (status, body) = put_body(
        &router,
        REGION,
        &hashed(first_hash, json!([item(Some("assoc-2"), "HIGH", "COMPLIANT")])),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a put whose content hash matches is accepted, got {status} with {body}"
    );
    assert_eq!(
        ids(&associations(&router, case).await),
        vec!["assoc-1"],
        "case {case:?}: expected a put whose content hash matches to be ignored"
    );

    // A PARTIAL put under a NEW hash writes, and records its own hash: the items it kept are
    // the older ones, so the hash a repeat is compared against must be this put's rather than
    // the one those kept items arrived under.
    let mut partial_put = hashed(
        second_hash,
        json!([item(Some("assoc-2"), "LOW", "NON_COMPLIANT")]),
    );
    partial_put["UploadType"] = json!("PARTIAL");
    put_body(&router, REGION, &partial_put).await;
    assert_eq!(
        ids(&associations(&router, case).await),
        vec!["assoc-1", "assoc-2"],
        "case {case:?}: expected the partial put under a new hash to have been written"
    );
    put_body(
        &router,
        REGION,
        &hashed(second_hash, json!([item(Some("assoc-3"), "HIGH", "COMPLIANT")])),
    )
    .await;
    assert_eq!(
        ids(&associations(&router, case).await),
        vec!["assoc-1", "assoc-2"],
        "case {case:?}: expected a repeat of the LAST put's hash to be ignored"
    );
    put_body(
        &router,
        REGION,
        &hashed(first_hash, json!([item(Some("assoc-4"), "HIGH", "COMPLIANT")])),
    )
    .await;
    assert_eq!(
        ids(&associations(&router, case).await),
        vec!["assoc-4"],
        "case {case:?}: expected a put carrying an EARLIER hash to be written, since the hash \
         held is the last put's"
    );

    // A put that leaves the pair holding NO item takes its hash with it, so a later put
    // carrying that same hash is written rather than ignored.
    put_body(&router, REGION, &hashed(second_hash, json!([]))).await;
    assert!(
        associations(&router, case).await.is_empty(),
        "case {case:?}: expected a put carrying no item to leave the compliance type empty"
    );
    put_body(
        &router,
        REGION,
        &hashed(
            second_hash,
            json!([item(Some("assoc-5"), "HIGH", "COMPLIANT")]),
        ),
    )
    .await;
    assert_eq!(
        ids(&associations(&router, case).await),
        vec!["assoc-5"],
        "case {case:?}: expected the hash of a pair that holds nothing to be gone, so the next \
         put under it is written"
    );
}

/// RULE: `ManagedInstance` is the only supported compliance resource type, so a put or a read
/// naming any other is refused 400 `InvalidResourceType` — naming the rejected type and the
/// supported ones — and the refused put writes nothing.
#[tokio::test]
async fn a_resource_type_outside_the_supported_set_is_refused() {
    let case = "the resource type table";
    let (router, engine) = service();
    register(&engine, REGION, NODE);

    // The rows differ along the FORM of the unsupported type: prose, the supported type in
    // another case, a near miss, and other resource types this service does hold.
    let rejected = [
        "NotARealResourceType",
        "managedinstance",
        "ManagedInstances",
        "EC2Instance",
        "Document",
        "MaintenanceWindow",
    ];

    for resource_type in rejected {
        let (status, body) = put(
            &router,
            REGION,
            resource_type,
            NODE,
            CUSTOM_TYPE,
            json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidResourceType"),
            "case {case:?}: expected {resource_type:?} to be refused, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "Resource type {resource_type} is not supported, example resource type are \
                 [{MANAGED_INSTANCE}]"
            ),
            "case {case:?}: expected the refusal to name the rejected type, got {body}"
        );

        // The same type on the read side, which narrows by it.
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ListComplianceItems",
            &json!({ "ResourceIds": [NODE], "ResourceTypes": [resource_type] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidResourceType"),
            "case {case:?}: expected the read of {resource_type:?} to be refused, got {status} \
             with {body}"
        );
    }

    assert!(
        items_of(&router, case, REGION, NODE).await.is_empty(),
        "case {case:?}: expected a refused put to write nothing"
    );

    // The supported type is served, which is what makes the refusals above about the type.
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        CUSTOM_TYPE,
        json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {MANAGED_INSTANCE} to be served, got {status} with {body}"
    );
}

/// RULE: compliance is reported about a managed node, so a `ResourceId` naming none of the
/// Region's nodes is refused `InvalidResourceId` and writes nothing.
#[tokio::test]
async fn a_resource_id_naming_no_managed_node_is_refused() {
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    register(&engine, OTHER_REGION, OTHER_NODE);

    // (why, the Region the put is signed for, the resource id it names)
    let rows = [
        ("a node id no registration minted", REGION, UNREGISTERED),
        ("a node of another Region", REGION, OTHER_NODE),
        ("this Region's node at the other endpoint", OTHER_REGION, NODE),
    ];

    for (why, region, resource_id) in rows {
        let (status, body) = put(
            &router,
            region,
            MANAGED_INSTANCE,
            resource_id,
            CUSTOM_TYPE,
            json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidResourceId"),
            "case {why:?}: expected {resource_id:?} at {region} to be refused, got {status} \
             with {body}"
        );
        assert!(
            items_of(&router, why, region, resource_id).await.is_empty(),
            "case {why:?}: expected a refused put to write nothing"
        );
    }
}

/// RULE: a read narrows by the resources and resource types it names and by its filters, each
/// filter matched against the member of the item its key names, under the comparison its
/// `Type` asks for — and a key that names no member of a compliance item is refused
/// `InvalidFilter`.
#[tokio::test]
async fn a_read_narrows_by_resources_and_filters() {
    let case = "the filter table";
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    register(&engine, REGION, OTHER_NODE);

    put_body(
        &router,
        REGION,
        &json!({
            "ResourceId": NODE,
            "ResourceType": MANAGED_INSTANCE,
            "ComplianceType": CUSTOM_TYPE,
            "ExecutionSummary": summary(Some("execution-1"), Some("Command")),
            "Items": [
                { "Id": "alpha-1", "Title": "first", "Severity": "HIGH", "Status": "NON_COMPLIANT" },
                { "Id": "beta-2", "Title": "second", "Severity": "LOW", "Status": "COMPLIANT" },
            ],
        }),
    )
    .await;
    put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        OTHER_NODE,
        "Patch",
        json!([item(Some("KB1"), "CRITICAL", "COMPLIANT")]),
    )
    .await;

    // (why, the read body, the ids it must report)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "no narrowing at all",
            json!({}),
            vec!["alpha-1", "beta-2", "KB1"],
        ),
        (
            "one resource id",
            json!({ "ResourceIds": [NODE] }),
            vec!["alpha-1", "beta-2"],
        ),
        (
            "the supported resource type",
            json!({ "ResourceTypes": [MANAGED_INSTANCE] }),
            vec!["alpha-1", "beta-2", "KB1"],
        ),
        (
            "a compliance type",
            json!({ "Filters": [{ "Key": "ComplianceType", "Values": ["Patch"] }] }),
            vec!["KB1"],
        ),
        (
            "a status",
            json!({ "Filters": [{ "Key": "Status", "Values": ["COMPLIANT"] }] }),
            vec!["beta-2", "KB1"],
        ),
        (
            "two values of one key, which union",
            json!({ "Filters": [{ "Key": "Severity", "Values": ["HIGH", "CRITICAL"] }] }),
            vec!["alpha-1", "KB1"],
        ),
        (
            "an explicit EQUAL",
            json!({
                "Filters": [{ "Key": "Title", "Values": ["second"], "Type": "EQUAL" }],
            }),
            vec!["beta-2"],
        ),
        (
            "NOT_EQUAL, the complement of the equality",
            json!({
                "Filters": [{ "Key": "Severity", "Values": ["HIGH"], "Type": "NOT_EQUAL" }],
            }),
            vec!["beta-2", "KB1"],
        ),
        (
            "BEGIN_WITH on the item id",
            json!({
                "Filters": [{ "Key": "Id", "Values": ["alpha"], "Type": "BEGIN_WITH" }],
            }),
            vec!["alpha-1"],
        ),
        (
            "the execution that reported the items",
            json!({ "Filters": [{ "Key": "ExecutionId", "Values": ["execution-1"] }] }),
            vec!["alpha-1", "beta-2"],
        ),
        (
            "the execution type",
            json!({ "Filters": [{ "Key": "ExecutionType", "Values": ["Command"] }] }),
            vec!["alpha-1", "beta-2"],
        ),
        (
            "an execution time the items are after",
            json!({
                "Filters": [{
                    "Key": "ExecutionTime",
                    "Values": ["2000-01-01T00:00:00Z"],
                    "Type": "GREATER_THAN",
                }],
            }),
            vec!["alpha-1", "beta-2", "KB1"],
        ),
        (
            "an execution time the items are before",
            json!({
                "Filters": [{
                    "Key": "ExecutionTime",
                    "Values": [EXECUTION_TIME_RENDERED],
                    "Type": "LESS_THAN",
                }],
            }),
            vec![],
        ),
        (
            "two filters, which hold together",
            json!({
                "ResourceIds": [NODE],
                "Filters": [
                    { "Key": "Status", "Values": ["COMPLIANT"] },
                    { "Key": "Severity", "Values": ["LOW"] },
                ],
            }),
            vec!["beta-2"],
        ),
        (
            "a resource id nothing was reported about",
            json!({ "ResourceIds": [UNREGISTERED] }),
            vec![],
        ),
    ];

    for (why, body, expected) in rows {
        let read = listed(&router, why, REGION, &body).await;
        assert_eq!(
            ids(&read),
            expected,
            "case {why:?}: expected {expected:?} for {body}, got {read:?}"
        );
    }

    // A `Filters` entry this read cannot narrow by: a key that names no member of a compliance
    // item, and one that names a member but no value to search for. (A key shorter than the
    // modelled `ComplianceStringFilterKey`'s length is refused by the boundary walk before this
    // handler runs, so the keys here are all well-formed.)
    let rejected: Vec<(&str, Value)> = vec![
        ("a key naming no member", json!({ "Key": "NotAKey", "Values": ["anything"] })),
        ("a key in the wrong case", json!({ "Key": "complianceType", "Values": ["x"] })),
        ("a key naming a Details entry", json!({ "Key": "PatchGroup", "Values": ["prod"] })),
        ("a member with no value to search for", json!({ "Key": "Status", "Values": [""] })),
    ];
    for (why, filter) in rejected {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ListComplianceItems",
            &json!({ "Filters": [filter] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidFilter"),
            "case {why:?}: expected the filter to be refused, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            INVALID_FILTER,
            "case {why:?}: expected this service's unknown-filter sentence, got {body}"
        );
    }
    assert_eq!(
        case, "the filter table",
        "the case label is the one this suite reports"
    );
}

/// RULE: a read is paged — it serves at most `MaxResults` items and hands back a token that
/// resumes STRICTLY after the last item it served — and a token this read did not hand out is
/// refused `InvalidNextToken`.
#[tokio::test]
async fn a_read_pages_and_refuses_a_token_it_did_not_issue() {
    let case = "the paging case";
    let (router, engine) = service();
    register(&engine, REGION, NODE);

    let written: Vec<String> = (0..5).map(|index| format!("item-{index}")).collect();
    put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        CUSTOM_TYPE,
        Value::Array(
            written
                .iter()
                .map(|id| item(Some(id), "HIGH", "NON_COMPLIANT"))
                .collect(),
        ),
    )
    .await;

    for page_size in [1_usize, 2, 5, 50] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({ "ResourceIds": [NODE], "MaxResults": page_size });
            if let Some(token) = token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (status, reported) =
                call_in_region(&router, REGION, "ListComplianceItems", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?} at page size {page_size}: the read must be served, got {status} \
                 with {reported}"
            );
            let page = reported["ComplianceItems"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?} at page size {page_size}: a page holds at most what was asked \
                 for, got {page:?}"
            );
            walked.extend(ids(&page));
            pages += 1;
            assert!(
                pages <= written.len() + 1,
                "case {case:?} at page size {page_size}: the walk must terminate, got {walked:?}"
            );
            match reported["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, written,
            "case {case:?} at page size {page_size}: expected every item exactly once, in the \
             store's order"
        );
    }

    // Tokens nothing handed out: prose, a position in the clear, and a minted token with one
    // character changed.
    let (_, first) = call_in_region(
        &router,
        REGION,
        "ListComplianceItems",
        &json!({ "ResourceIds": [NODE], "MaxResults": 1 }),
    )
    .await;
    let minted = first["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a bounded page hands out a token, got {first}"))
        .to_string();
    let mut tampered = minted.clone();
    tampered.replace_range(0..1, if minted.starts_with('a') { "b" } else { "a" });
    for (why, token) in [
        ("prose", "not-a-real-token".to_string()),
        ("a position in the clear", format!("{MANAGED_INSTANCE}/{NODE}")),
        ("a minted token with a character changed", tampered),
    ] {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "ListComplianceItems",
            &json!({ "ResourceIds": [NODE], "NextToken": token }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidNextToken"),
            "case {why:?}: expected the token {token:?} to be refused, got {status} with {body}"
        );
    }

    // The token this read DID hand out still resumes it.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "ListComplianceItems",
        &json!({ "ResourceIds": [NODE], "NextToken": minted, "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the minted token must resume the read, got {status} with {body}"
    );
    assert_eq!(
        ids(&body["ComplianceItems"]
            .as_array()
            .cloned()
            .unwrap_or_default()),
        written[1..],
        "case {case:?}: expected the resume to continue strictly after the first item, got {body}"
    );
}

/// RULE: a read reports the compliance of BOTH sources together — the items Patch Manager
/// gathered on the node and the ones a put wrote — and the walk is total over the union, so an
/// item of one source is never dropped because an item of the other carries the same id.
#[tokio::test]
async fn a_read_reports_the_gathered_and_the_written_compliance_together() {
    use service_com_amazonaws_ssm_amazonssm::state::instance_patch_states::InstancePatchStateRecord;
    use service_com_amazonaws_ssm_amazonssm::state::instance_patches::PatchComplianceRow;

    let case = "the two sources";
    let (router, engine) = service();
    register(&engine, REGION, NODE);

    // The id both sources carry: the patch report names the patch by its operating-system id,
    // and the put below reports its own item under the very same id and compliance type.
    let shared_id = "KB5000001";
    let gathered = PatchComplianceRow {
        patch_id: "patch-1".to_string(),
        title: "Security Update".to_string(),
        kb_id: shared_id.to_string(),
        classification: "SecurityUpdates".to_string(),
        severity: "Critical".to_string(),
        state: "INSTALLED".to_string(),
        installed_time: SystemTime::UNIX_EPOCH + LAST_PING,
        compliance_level: Some("CRITICAL".to_string()),
    };
    let report = InstancePatchStateRecord::from_rows(
        NODE,
        "prod",
        "pb-0123456789abcdef0",
        "SCAN",
        SystemTime::UNIX_EPOCH + LAST_PING,
        SystemTime::UNIX_EPOCH + LAST_PING,
        vec![gathered],
    );
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_instance_patch_state(REGION, report),
        "a seeded patch report must carry the node id it is stored under"
    );

    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        "Patch",
        json!([item(Some(shared_id), "HIGH", "NON_COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put must be served, got {status} with {body}"
    );

    // Both items are reported, and the statuses show which source each came from.
    let read = items_of(&router, case, REGION, NODE).await;
    assert_eq!(
        read.len(),
        2,
        "case {case:?}: expected the gathered item and the written one, got {read:?}"
    );
    let statuses: Vec<&str> = read
        .iter()
        .map(|held| held["Status"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        statuses,
        vec!["COMPLIANT", "NON_COMPLIANT"],
        "case {case:?}: expected the gathered item before the written one, got {read:?}"
    );

    // And the paged walk reports each of them exactly once: the two share a resource, a
    // compliance type and an id, so a page boundary between them must still resume.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..=read.len() {
        let mut body = json!({ "ResourceIds": [NODE], "MaxResults": 1 });
        if let Some(token) = token.as_deref() {
            body["NextToken"] = json!(token);
        }
        let (status, page) = call_in_region(&router, REGION, "ListComplianceItems", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the paged read must be served, got {status} with {page}"
        );
        walked.extend(
            page["ComplianceItems"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|held| held["Status"].as_str().unwrap_or_default().to_string()),
        );
        match page["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        walked, statuses,
        "case {case:?}: expected the paged walk to report both items exactly once"
    );
}

/// RULE: a resource holds reports of at most ten CUSTOM compliance types — "you can specify a
/// maximum of 10 different types" — so the eleventh is refused
/// `ComplianceTypeCountLimitExceededException` and writes nothing, while a predefined type and
/// another resource's types are unaffected.
#[tokio::test]
async fn the_eleventh_custom_compliance_type_of_one_resource_is_refused() {
    let case = "the custom type ceiling";
    let (router, engine) = service();
    register(&engine, REGION, NODE);
    register(&engine, REGION, OTHER_NODE);

    // The published maximum: ten custom types per resource.
    let ceiling = 10;
    for index in 0..ceiling {
        let compliance_type = format!("Custom:Type{index}");
        let (status, body) = put(
            &router,
            REGION,
            MANAGED_INSTANCE,
            NODE,
            &compliance_type,
            json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: custom type {index} must be served, got {status} with {body}"
        );
    }

    // A put under a type the resource already holds is admitted at the ceiling.
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        "Custom:Type0",
        json!([item(Some("item-2"), "LOW", "COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a type the resource already holds must be served at the ceiling, got \
         {status} with {body}"
    );

    // A PREDEFINED type is no custom type, so the ceiling does not count it.
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        "Patch",
        json!([item(Some("KB1"), "CRITICAL", "COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a predefined compliance type must be served, got {status} with {body}"
    );

    // The eleventh custom type of THIS resource is refused.
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        NODE,
        "Custom:TooMany",
        json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ComplianceTypeCountLimitExceededException"),
        "case {case:?}: expected the eleventh custom type to be refused, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "You specified too many custom compliance types. You can specify a maximum of \
             {ceiling} different types."
        ),
        "case {case:?}: expected the refusal to state the published maximum, got {body}"
    );
    let refused = listed(
        &router,
        case,
        REGION,
        &json!({
            "ResourceIds": [NODE],
            "Filters": [{ "Key": "ComplianceType", "Values": ["Custom:TooMany"] }],
        }),
    )
    .await;
    assert!(
        refused.is_empty(),
        "case {case:?}: expected the refused put to write nothing, got {refused:?}"
    );

    // ANOTHER resource holds its own types, so the ceiling is per resource.
    let (status, body) = put(
        &router,
        REGION,
        MANAGED_INSTANCE,
        OTHER_NODE,
        "Custom:TooMany",
        json!([item(Some("item-1"), "HIGH", "NON_COMPLIANT")]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the ceiling to be per resource, got {status} with {body}"
    );
}

/// A router with this service beside the IAM and STS services the principals and their
/// policies are made with, all on ONE shared engine — and the engine, so the case can seed the
/// managed node compliance is reported about.
fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the caller's access key id and Region from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// RULE: the put is decided against the CALLER's own identity-based policies before anything
/// is resolved — the action is this operation's own and the resource is the EC2 instance ARN of
/// the node the request names — so a caller no readable document allows is refused
/// `AccessDeniedException` naming the principal, the action, the resource and the rule that
/// withheld the grant, and the refused put writes nothing.
#[tokio::test]
async fn a_caller_whose_policies_do_not_allow_the_put_is_refused() {
    let case = "the principal table";
    let (router, engine) = emulator_with_engine();
    register(&engine, REGION, NODE);
    let resource = format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/{NODE}");

    let allow_all = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#;
    let allow_elsewhere = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:GetParameter","Resource":"*"}]}"#;
    let allow_all_but_deny_the_put = r#"{"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":"ssm:*","Resource":"*"},
        {"Effect":"Deny","Action":"ssm:PutComplianceItems","Resource":"*"}]}"#;
    let allow_this_node = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:PutComplianceItems","Resource":"{resource}"}}]}}"#
    );
    let allow_another_node = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
        "Action":"ssm:PutComplianceItems","Resource":"arn:aws:ec2:{REGION}:{ACCOUNT}:instance/{OTHER_NODE}"}}]}}"#
    );

    // (why, the access key id the call is signed with, the principal a refusal must blame,
    //  the clause the refusal carries — `None` where the put is served)
    let mut rows: Vec<(&str, String, String, Option<String>)> = vec![(
        "the account's own published credential",
        ACCOUNT_KEY.to_string(),
        format!("arn:aws:iam::{ACCOUNT}:root"),
        None,
    )];
    for (why, role, policy, reason) in [
        (
            "a session carrying no policy at all",
            "compliance-noperm",
            None,
            Some("because no identity-based policy allows the ssm:PutComplianceItems action"),
        ),
        (
            "a session allowed only another action of this service",
            "compliance-elsewhere",
            Some(allow_elsewhere.to_string()),
            Some("because no identity-based policy allows the ssm:PutComplianceItems action"),
        ),
        (
            "a session allowed the put on ANOTHER node",
            "compliance-other-node",
            Some(allow_another_node.clone()),
            Some("because no identity-based policy allows the ssm:PutComplianceItems action"),
        ),
        (
            "a session whose policy denies the put explicitly",
            "compliance-denied",
            Some(allow_all_but_deny_the_put.to_string()),
            Some("with an explicit deny in an identity-based policy"),
        ),
        (
            "a session allowed everything",
            "compliance-admin",
            Some(allow_all.to_string()),
            None,
        ),
        (
            "a session allowed the put on THIS node",
            "compliance-this-node",
            Some(allow_this_node.clone()),
            None,
        ),
    ] {
        let principal = assumed_role(&router, role, policy.as_deref()).await;
        rows.push((
            why,
            principal.access_key_id,
            principal.arn,
            reason.map(str::to_string),
        ));
    }

    for (why, access_key_id, principal, reason) in rows {
        let body = json!({
            "ResourceId": NODE,
            "ResourceType": MANAGED_INSTANCE,
            "ComplianceType": "Custom:Authorized",
            "ExecutionSummary": summary(None, None),
            "Items": [item(Some("item-1"), "HIGH", "NON_COMPLIANT")],
        });
        let (status, answered) = call_as(
            &router,
            &access_key_id,
            REGION,
            "PutComplianceItems",
            &body,
        )
        .await;
        match reason {
            Some(reason) => {
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "AccessDeniedException"),
                    "case {why:?}: expected the put to be refused, got {status} with {answered}"
                );
                assert_eq!(
                    error_message(&answered),
                    format!(
                        "User: {principal} is not authorized to perform: ssm:PutComplianceItems \
                         on resource: {resource} {reason}"
                    ),
                    "case {why:?}: expected the refusal to name the principal, the action, the \
                     node's ARN and the rule, got {answered}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {why:?}: expected the put to be served, got {status} with {answered}"
                );
            }
        }
    }

    // Only the served puts wrote, and they wrote the one list they all carried.
    let read = listed(
        &router,
        case,
        REGION,
        &json!({ "ResourceIds": [NODE], "ResourceTypes": [MANAGED_INSTANCE] }),
    )
    .await;
    assert_eq!(
        ids(&read),
        vec!["item-1"],
        "case {case:?}: expected the served puts to have written the one list they carried, got \
         {read:?}"
    );
}
