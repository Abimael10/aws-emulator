// SPDX-License-Identifier: CC0-1.0

//! The node-addressed control plane of this service, end to end: `ListNodes` reports the
//! managed nodes the node-management experience holds, `DescribeInstancePatches` reports what
//! a patching operation reported about one node, `UpdateManagedInstanceRole` moves a
//! registered node to another IAM role and `DeregisterManagedInstance` removes its
//! registration.
//!
//! Three rules are asserted here, each over a table:
//!
//! 1. an operation that RESOLVES a node id refuses an id this Region holds no node under with
//!    `InvalidInstanceId` carrying NO message, and writes nothing
//!    (`smithy_query(query="operation", operation="DeregisterManagedInstance,UpdateManagedInstanceRole")`:
//!    `InvalidInstanceId` is the one client error either declares);
//! 2. `ListNodes` is part of the Systems Manager integrated experience, so an account that
//!    has not enabled it is refused `UnsupportedOperationException` before anything is read,
//!    and an account that has it is served the nodes its own Region holds, every member read
//!    out of the stored node (what a listing filters, aggregates and pages is the sibling
//!    `all/node_listing.rs`'s subject);
//! 3. `DescribeInstancePatches` reports the patches REPORTED for a node id — none, because no
//!    call of this model runs a patching operation — and refuses a `Filters` entry naming no
//!    property of a patch ("supported keys for `DescribeInstancePatches` include … 
//!    Classification, KBId, Severity, State", `smithy_query(query="doc",
//!    shape="DescribeInstancePatchesRequest", member="Filters")`).
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`call_in_region`], [`error_code`], [`error_message`],
//! [`router_with_engine`]). The ONE thing seeded through the engine is a registered managed
//! node, because no call of this service registers one (the agent's own call is not in this
//! model); the node-management experience is turned on through this service's own
//! `CreateResourceDataSync`.
//!
//! The rows span what each rule generalises over: the form of a node id and which Region
//! holds it, how many nodes a listing walks and how large a page it asks for, which filter
//! key and operator narrows it, and which patch filter key a read carries.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router_with_engine, ACCOUNT, REGION,
};

/// A second Region, to pin that a node is reached through the Region it registered in.
const OTHER_REGION: &str = "us-east-1";

/// A hybrid node id: `mi-` and 17 characters, the form `ManagedInstanceId` admits
/// (`^(^mi-[0-9a-f]{17}$)|(^eks_c:…)$`).
const HYBRID: &str = "mi-01234567890123456";

/// A second hybrid node id, so a case can pin that a write touches ONE node.
const OTHER_HYBRID: &str = "mi-abcdef01234567890";

/// A hybrid node id nothing registered.
const UNREGISTERED_HYBRID: &str = "mi-99999999999999999";

/// An EC2 node id of the 17-character form (`InstanceId`, pattern
/// `^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$`).
const EC2_NODE: &str = "i-0123456789abcdef0";

/// An EC2 node id of the 8-character form (same pattern).
const EC2_SHORT: &str = "i-0abc1234";

/// A well-formed node id nothing registered.
const UNREGISTERED: &str = "i-0dead999";

/// The instant every seeded node last reported, as a whole number of seconds.
const LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// The sentence `ListNodes` refuses an account that has not enabled the Systems Manager
/// integrated experience with, written out here independently of the service: fixed text,
/// with nothing of the request in it.
const NOT_ENABLED: &str = "This is an unsupported operation for this account. You must first \
                           enable the Systems Manager integrated experience in your account.";

/// A router with just this service mounted, and the engine behind it.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Register a node in `region` the way its own registration would, carrying the properties a
/// node reports about itself. Shared with the sibling suite that moves a registered node to
/// another IAM role ([`crate::managed_node_role_moves`]), which needs the same seeded node.
pub(crate) fn register(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    node_id: &str,
    resource_type: &str,
    platform: &str,
) {
    let at = SystemTime::UNIX_EPOCH + LAST_PING;
    let mut node = ManagedNodeRecord::registering(node_id, resource_type, at);
    node.properties.platform_type = Some(platform.to_string());
    node.properties.platform_name = Some(format!("{platform} Server"));
    node.properties.platform_version = Some("1.2".to_string());
    node.properties.agent_version = Some("3.3.2746.0".to_string());
    node.properties.instance_state = Some("Active".to_string());
    node.properties.computer_name = Some(format!("{node_id}.internal"));
    node.properties.ip_address = Some("10.0.1.17".to_string());
    node.properties.availability_zone = Some(format!("{region}a"));
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// The `IamRole` the node describes report for `node_id` in [`REGION`], read back through
/// this service's own `DescribeInstanceInformation`.
async fn reported_role(router: &MultiServiceRouter, node_id: &str) -> Option<String> {
    let (status, body) = call(
        router,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [node_id] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceInformation must be served, got {status} with {body}"
    );
    let reported = body["InstanceInformationList"]
        .as_array()
        .unwrap_or_else(|| panic!("a describe reports a list, got {body}"));
    reported
        .first()
        .and_then(|node| node["IamRole"].as_str())
        .map(str::to_string)
}

/// Whether the node describes report a node under `node_id` in `region`.
async fn is_registered(router: &MultiServiceRouter, region: &str, node_id: &str) -> bool {
    let (status, body) = call_in_region(
        router,
        region,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [node_id] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceInformation must be served, got {status} with {body}"
    );
    body["InstanceInformationList"]
        .as_array()
        .is_some_and(|reported| !reported.is_empty())
}

/// Turn the node-management experience on for `region` through this service's own
/// `CreateResourceDataSync`: enabling the Systems Manager integrated experience is what puts
/// the managed node sync in the account, and that sync is what a node listing reads
/// (`crate::state::node_listing::MANAGED_NODE_SYNC_NAME`).
async fn enable_node_management(router: &MultiServiceRouter, region: &str) {
    let (status, body) = call_in_region(
        router,
        region,
        "CreateResourceDataSync",
        &json!({
            "SyncName": service_com_amazonaws_ssm_amazonssm::state::node_listing::MANAGED_NODE_SYNC_NAME,
            "SyncType": "SyncFromSource",
            "SyncSource": {
                "SourceType": "SingleAccountMultiRegions",
                "SourceRegions": [region],
                "IncludeFutureRegions": false,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the sync must be created, got {status} with {body}"
    );
}

/// The node ids a `ListNodes` body reports, in the order it reports them.
fn listed_ids(body: &Value) -> Vec<String> {
    body["Nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing reports Nodes, got {body}"))
        .iter()
        .map(|node| node["Id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: an id this Region holds no node under names nothing to deregister or to move to
/// another role, which is `InvalidInstanceId` carrying no message — and the refused call
/// changes nothing.
#[tokio::test]
async fn a_node_write_is_refused_for_an_id_that_names_no_node() {
    // (case, the node id the request names)
    let rows: [(&str, &str); 3] = [
        ("a hybrid id nothing registered", UNREGISTERED_HYBRID),
        (
            "a hybrid id registered in another Region",
            OTHER_HYBRID,
        ),
        (
            "a hybrid id whose node was already deregistered",
            HYBRID,
        ),
    ];

    for (case, node_id) in rows {
        let (router, engine) = service();
        register(
            &engine,
            REGION,
            HYBRID,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
        register(
            &engine,
            OTHER_REGION,
            OTHER_HYBRID,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
        if node_id == HYBRID {
            // Deregister it first, so the id names a node that is no longer registered.
            let (status, body) = call(
                &router,
                "DeregisterManagedInstance",
                &json!({ "InstanceId": HYBRID }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the first deregister must be served, got {status} with {body}"
            );
        }

        for (operation, request) in [
            (
                "DeregisterManagedInstance",
                json!({ "InstanceId": node_id }),
            ),
            (
                "UpdateManagedInstanceRole",
                json!({ "InstanceId": node_id, "IamRole": "SSMServiceRole" }),
            ),
        ] {
            let (status, body) = call(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} under {operation}: must be refused, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidInstanceId",
                "case {case:?} under {operation}: expected InvalidInstanceId, got {body}"
            );
            assert_eq!(
                error_message(&body),
                "",
                "case {case:?} under {operation}: this refusal carries no message, got {body}"
            );
        }

        // The node the other Region holds is untouched by every refusal above.
        assert!(
            is_registered(&router, OTHER_REGION, OTHER_HYBRID).await,
            "case {case:?}: another Region's node is left alone"
        );
    }
}

/// RULE: a deregister removes the registration the id names, so the node describes report it
/// no more — and the other nodes the account holds are left where they are.
#[tokio::test]
async fn a_deregister_removes_the_registration_it_names() {
    let (router, engine) = service();
    for node_id in [HYBRID, OTHER_HYBRID] {
        register(
            &engine,
            REGION,
            node_id,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
    }
    register(&engine, OTHER_REGION, HYBRID, RESOURCE_TYPE_MANAGED_INSTANCE, "Linux");

    let (status, body) = call(
        &router,
        "DeregisterManagedInstance",
        &json!({ "InstanceId": HYBRID }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the deregister must be served, got {status} with {body}"
    );

    // (case, the Region, the node id, whether a node is still reported)
    let rows: [(&str, &str, &str, bool); 3] = [
        ("the node the deregister named", REGION, HYBRID, false),
        ("another node of the same Region", REGION, OTHER_HYBRID, true),
        (
            "the same id in another Region",
            OTHER_REGION,
            HYBRID,
            true,
        ),
    ];
    for (case, region, node_id, expected) in rows {
        assert_eq!(
            is_registered(&router, region, node_id).await,
            expected,
            "case {case:?}: expected registered == {expected}"
        );
    }
}

/// RULE: an update replaces the role the node is managed under and nothing else about it, and
/// the node describes report the role the last accepted update wrote.
#[tokio::test]
async fn an_update_replaces_the_role_the_node_is_managed_under() {
    let (router, engine) = service();
    register(
        &engine,
        REGION,
        HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "Linux",
    );
    register(
        &engine,
        REGION,
        OTHER_HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "Windows",
    );

    // The rows differ along the form of the role name: a plain name, a name under a path, a
    // name carrying the symbols `IamRole` admits, and the empty value the member's own
    // `length 0..64` allows.
    let rows: [(&str, &str); 4] = [
        ("a plain role name", "SSMServiceRole"),
        ("a role under a path", "service-role/AmazonSSMRole"),
        ("a name carrying symbols", "ssm_role-2+team=platform"),
        ("the empty role the member admits", ""),
    ];

    for (case, iam_role) in rows {
        let (status, body) = call(
            &router,
            "UpdateManagedInstanceRole",
            &json!({ "InstanceId": HYBRID, "IamRole": iam_role }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update must be served, got {status} with {body}"
        );
        assert_eq!(
            reported_role(&router, HYBRID).await.as_deref(),
            Some(iam_role),
            "case {case:?}: the node reports the role the update wrote"
        );
        assert_eq!(
            reported_role(&router, OTHER_HYBRID).await,
            None,
            "case {case:?}: another node is left as it was"
        );
    }
}

/// RULE: `ListNodes` is refused for an account that has not enabled the Systems Manager
/// integrated experience, whatever the request asks for — and nothing is read.
#[tokio::test]
async fn a_listing_is_refused_where_the_integrated_experience_is_not_enabled() {
    // (case, the request body)
    let rows: [(&str, Value); 4] = [
        ("a listing of everything", json!({})),
        (
            "a listing narrowed by a filter",
            json!({ "Filters": [{ "Key": "PlatformType", "Values": ["Linux"] }] }),
        ),
        ("a listing bounded by a page", json!({ "MaxResults": 1 })),
        (
            "a listing of a sync's own data",
            json!({ "SyncName": "explorer" }),
        ),
    ];

    for (case, request) in rows {
        let (router, engine) = service();
        // Nodes ARE held: the refusal is about the account's configuration rather than
        // about an empty store.
        register(&engine, REGION, EC2_NODE, RESOURCE_TYPE_EC2_INSTANCE, "Linux");

        let (status, body) = call(&router, "ListNodes", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the listing must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "UnsupportedOperationException",
            "case {case:?}: expected UnsupportedOperationException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NOT_ENABLED,
            "case {case:?}: expected {NOT_ENABLED:?}, got {:?}",
            error_message(&body)
        );
    }
}

/// RULE: once the experience is enabled, a listing reports the nodes the ENDPOINT's Region
/// holds, every member read out of the stored node, and a `SyncName` that names no sync a
/// node listing can read is refused.
#[tokio::test]
async fn a_listing_reports_the_nodes_of_its_own_region() {
    let (router, engine) = service();
    register(&engine, REGION, EC2_NODE, RESOURCE_TYPE_EC2_INSTANCE, "Linux");
    register(&engine, REGION, EC2_SHORT, RESOURCE_TYPE_EC2_INSTANCE, "Windows");
    register(
        &engine,
        REGION,
        HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "Linux",
    );
    register(
        &engine,
        OTHER_REGION,
        OTHER_HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "Linux",
    );
    enable_node_management(&router, REGION).await;

    let (status, body) = call(&router, "ListNodes", &json!({})).await;
    assert_eq!(
        status, 200,
        "the listing must be served, got {status} with {body}"
    );
    assert_eq!(
        listed_ids(&body),
        vec![
            EC2_NODE.to_string(),
            EC2_SHORT.to_string(),
            HYBRID.to_string()
        ],
        "every node the Region holds is reported, in node-id order, got {body}"
    );

    // Every member of one reported node is read out of the node the store holds.
    let reported = body["Nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["Id"] == json!(EC2_NODE)))
        .unwrap_or_else(|| panic!("the listing reports the node, got {body}"));
    assert_eq!(
        reported["Region"].as_str(),
        Some(REGION),
        "the node reports the Region it is held at, got {reported}"
    );
    assert_eq!(
        reported["Owner"]["AccountId"].as_str(),
        Some(ACCOUNT),
        "the node is this account's, got {reported}"
    );
    // (the member of the reported instance, the value the stored node holds for it)
    let projected: [(&str, Value); 8] = [
        ("AgentVersion", json!("3.3.2746.0")),
        ("ComputerName", json!(format!("{EC2_NODE}.internal"))),
        ("InstanceStatus", json!("Active")),
        ("IpAddress", json!("10.0.1.17")),
        ("PlatformType", json!("Linux")),
        ("PlatformName", json!("Linux Server")),
        ("PlatformVersion", json!("1.2")),
        ("ResourceType", json!(RESOURCE_TYPE_EC2_INSTANCE)),
    ];
    for (member, expected) in projected {
        assert_eq!(
            reported["NodeType"]["Instance"][member], expected,
            "case {member:?}: the listing reports what the node holds, got {reported}"
        );
    }

    // The other Region's endpoint has not enabled the experience at all.
    let (status, body) = call_in_region(&router, OTHER_REGION, "ListNodes", &json!({})).await;
    assert_eq!(
        status, 400,
        "the other Region has not enabled it, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "UnsupportedOperationException",
        "expected UnsupportedOperationException, got {body}"
    );

    // A sync name that names no sync a node listing reads cannot be served.
    let (status, body) = call(
        &router,
        "ListNodes",
        &json!({ "SyncName": "never-created" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "an unknown sync must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceDataSyncNotFoundException",
        "expected ResourceDataSyncNotFoundException, got {body}"
    );
}

/// RULE: `DescribeInstancePatches` refuses a `Filters` entry naming no property of a patch,
/// before any node is resolved, and otherwise reports the patches reported for the node id —
/// none, because nothing in this model runs a patching operation.
#[tokio::test]
async fn a_patch_read_refuses_a_filter_key_that_names_no_property_of_a_patch() {
    let (router, engine) = service();
    register(&engine, REGION, EC2_NODE, RESOURCE_TYPE_EC2_INSTANCE, "Linux");

    // The keys this read publishes, each accepted — and asserted on a key the issue did not
    // cite ("supported keys for DescribeInstancePatches include … Classification, KBId,
    // Severity, State").
    for key in ["Classification", "KBId", "Severity", "State"] {
        let (status, body) = call(
            &router,
            "DescribeInstancePatches",
            &json!({
                "InstanceId": EC2_NODE,
                "Filters": [{ "Key": key, "Values": ["anything"] }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {key:?}: a published filter key must be served, got {status} with {body}"
        );
        assert_eq!(
            body["Patches"],
            json!([]),
            "case {key:?}: no patching operation has reported for this node, got {body}"
        );
        assert!(
            body["NextToken"].is_null(),
            "case {key:?}: nothing is left for a next page, got {body}"
        );
    }

    // (case, the filters, the message the refusal carries)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a key of no patch property",
            json!([{ "Key": "NotAKey", "Values": ["x"] }]),
            "Unknown Filter Key: NotAKey".to_string(),
        ),
        (
            "a published key in another casing",
            json!([{ "Key": "severity", "Values": ["Critical"] }]),
            "Unknown Filter Key: severity".to_string(),
        ),
        (
            "a filter key naming a patch-state count",
            json!([{ "Key": "InstalledCount", "Values": ["0"] }]),
            "Unknown Filter Key: InstalledCount".to_string(),
        ),
        (
            "a filter carrying no key at all",
            json!([{ "Values": ["x"] }]),
            "A Filters entry carries no filter key to search on".to_string(),
        ),
        (
            "a published key carrying no value",
            json!([{ "Key": "State" }]),
            "Filter key 'State' carries no value to search for".to_string(),
        ),
        (
            "a bad key beside a good one",
            json!([
                { "Key": "State", "Values": ["Installed"] },
                { "Key": "NotAKey", "Values": ["x"] },
            ]),
            "Unknown Filter Key: NotAKey".to_string(),
        ),
    ];

    for (case, filters, expected) in rows {
        // The node id is a registered one here and an unregistered one below: the filter is
        // refused before the id is resolved either way.
        for node_id in [EC2_NODE, UNREGISTERED] {
            let (status, body) = call(
                &router,
                "DescribeInstancePatches",
                &json!({ "InstanceId": node_id, "Filters": filters }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?} for {node_id}: must be refused, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?} for {node_id}: expected ValidationException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?} for {node_id}: expected {expected:?}, got {:?}",
                error_message(&body)
            );
        }
    }

    // A read of a node id nothing reported about is served the nothing it holds, which is
    // what the read is ABOUT: the patches reported for that id.
    for node_id in [EC2_NODE, UNREGISTERED, HYBRID] {
        let (status, body) = call(
            &router,
            "DescribeInstancePatches",
            &json!({ "InstanceId": node_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {node_id:?}: the read must be served, got {status} with {body}"
        );
        assert_eq!(
            body["Patches"],
            json!([]),
            "case {node_id:?}: nothing has reported a patch for it, got {body}"
        );
    }
}
