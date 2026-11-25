// SPDX-License-Identifier: CC0-1.0

//! `ListNodes` end to end: the managed nodes of the Systems Manager integrated experience,
//! narrowed by filters, aggregated by the sync a request names and served one page at a
//! time.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router_with_engine`], [`call_in_region`]) dispatches `POST /`
//! with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.
//!
//! The experience itself is turned on through this service's OWN operation
//! (`CreateResourceDataSync`, which puts the Amazon Web Services managed node sync in the
//! account), so what a listing answers follows a write a caller made rather than a flag a
//! test planted. The nodes are seeded through the service's state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store the node
//! describes read too), because a node enters it only by REGISTERING — an SSM Agent on an
//! instance, or a hybrid node activated with an activation code — and this suite registers
//! none: the same seam `all/managed_node_information.rs` uses for the sibling describe.
//!
//! The suites are tables: each row is one input along the axis the rule under test
//! generalises over (which member the request carries, the filter key, the filter operator,
//! the value count, the filter count, the node kind, the Region, the page size, the sync
//! name).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call_in_region, error_code, error_message, router_with_engine, ACCOUNT, REGION,
};

/// A second Region, to pin which Regions' nodes one listing reports.
const OTHER_REGION: &str = "us-east-1";

/// The name of the resource data sync the integrated experience puts in an account: "the
/// name of the supported resource data sync is AWS-QuickSetup-ManagedNode"
/// (`smithy_query(query="doc", shape="ListNodesRequest", member="SyncName")`).
const MANAGED_NODE_SYNC: &str = "AWS-QuickSetup-ManagedNode";

/// An EC2 node id whose suffix is 8 characters long (the modelled `NodeId` pattern
/// `^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$` admits both lengths).
const EC2_LINUX: &str = "i-0abc1234";

/// An EC2 node id whose suffix is 17 characters long (same pattern).
const EC2_WINDOWS: &str = "i-0123456789abcdef0";

/// A hybrid node id: `mi-` and 17 characters (same pattern).
const HYBRID: &str = "mi-01234567890123456";

/// The node seeded in [`OTHER_REGION`].
const REMOTE: &str = "i-0eeeeeee";

/// When every seeded node last reported, as its record holds it — which is the
/// `CaptureTime` a listing reports for it.
const CAPTURED_AT_SECS: u64 = 1_700_000_000;

/// The `UnsupportedOperationException` sentence an account without the experience is
/// answered with, as the service words it (corpus `simparity-behavioral-aws-v1`, check
/// `ssm.list-nodes-reflects-managed-instances`).
const NOT_ENABLED: &str = "This is an unsupported operation for this account. You must first \
                           enable the Systems Manager integrated experience in your account.";

/// Register `record` in `region` the way a node's own registration would.
fn register(engine: &Arc<engine::InMemoryEngine>, region: &str, record: ManagedNodeRecord) {
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// How many nodes the store holds, to pin that a listing wrote nothing.
fn registered_count(engine: &Arc<engine::InMemoryEngine>) -> usize {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .managed_nodes
        .len()
}

/// Three nodes in [`REGION`] and one in [`OTHER_REGION`], spanning node kind, platform,
/// agent version, address, zone, source and node status.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(CAPTURED_AT_SECS);

    let mut linux = ManagedNodeRecord::registering(EC2_LINUX, RESOURCE_TYPE_EC2_INSTANCE, at);
    linux.properties.platform_type = Some("Linux".to_string());
    linux.properties.platform_name = Some("Amazon Linux".to_string());
    linux.properties.platform_version = Some("2023".to_string());
    linux.properties.agent_version = Some("3.2.1234.0".to_string());
    linux.properties.ip_address = Some("10.0.1.17".to_string());
    linux.properties.computer_name = Some("ip-10-0-1-17.us-west-2.compute.internal".to_string());
    linux.properties.instance_state = Some("Active".to_string());
    linux.properties.availability_zone = Some("us-west-2a".to_string());
    linux.tags = BTreeMap::from([("env".to_string(), "dev".to_string())]);
    register(engine, REGION, linux);

    let mut windows = ManagedNodeRecord::registering(EC2_WINDOWS, RESOURCE_TYPE_EC2_INSTANCE, at);
    windows.properties.platform_type = Some("Windows".to_string());
    windows.properties.platform_name = Some("Microsoft Windows Server".to_string());
    windows.properties.platform_version = Some("2019".to_string());
    windows.properties.agent_version = Some("3.1.1.0".to_string());
    windows.properties.ip_address = Some("10.0.2.9".to_string());
    windows.properties.computer_name = Some("EC2AMAZ-WIN".to_string());
    windows.properties.instance_state = Some("Active".to_string());
    windows.properties.availability_zone = Some("us-west-2b".to_string());
    register(engine, REGION, windows);

    let mut hybrid = ManagedNodeRecord::registering(HYBRID, RESOURCE_TYPE_MANAGED_INSTANCE, at);
    hybrid.properties.platform_type = Some("MacOS".to_string());
    hybrid.properties.platform_name = Some("macOS".to_string());
    hybrid.properties.platform_version = Some("14.4".to_string());
    hybrid.properties.agent_version = Some("3.3.2746.0".to_string());
    hybrid.properties.name = Some("edge-1".to_string());
    hybrid.properties.source_type = Some("AWS::IoT::Thing".to_string());
    hybrid.properties.source_id = Some("thing-1".to_string());
    hybrid.properties.source_location = Some("edge-site-1".to_string());
    // A hybrid node reports no instance state and no zone of its own: nothing holds either,
    // so a listing reports neither and a filter on them selects this node for nothing.
    register(engine, REGION, hybrid);

    let mut remote = ManagedNodeRecord::registering(REMOTE, RESOURCE_TYPE_EC2_INSTANCE, at);
    remote.properties.platform_type = Some("Linux".to_string());
    remote.properties.agent_version = Some("3.2.1234.0".to_string());
    register(engine, OTHER_REGION, remote);
}

/// Turn the integrated experience on at `region`'s endpoint, through this service's own
/// `CreateResourceDataSync`: the managed node sync aggregating `source_regions`.
async fn enable_experience(
    router: &MultiServiceRouter,
    region: &str,
    source_regions: &[&str],
) {
    let (status, body) = call_in_region(
        router,
        region,
        "CreateResourceDataSync",
        &json!({
            "SyncName": MANAGED_NODE_SYNC,
            "SyncType": "SyncFromSource",
            "SyncSource": {
                "SourceType": "SingleAccountMultiRegions",
                "SourceRegions": source_regions,
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the managed node sync in {region} must succeed, got {status} with {body}"
    );
}

/// `ListNodes` at `region`'s endpoint.
async fn list_in_region(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    call_in_region(router, region, "ListNodes", body).await
}

/// `ListNodes` at [`REGION`]'s endpoint.
async fn list(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    list_in_region(router, REGION, body).await
}

/// The node ids a listing reports, in the order it reported them.
fn listed_ids(case: &str, body: &Value) -> Vec<String> {
    body["Nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a listing carries Nodes, got {body}"))
        .iter()
        .map(|node| {
            node["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: every node carries an Id, got {node}"))
                .to_string()
        })
        .collect()
}

/// A listing that must have succeeded, with the node ids it reported.
async fn listed(router: &MultiServiceRouter, case: &str, body: &Value) -> (Value, Vec<String>) {
    let (status, parsed) = list(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {parsed}"
    );
    let ids = listed_ids(case, &parsed);
    (parsed, ids)
}

/// Node ids in the order a listing reports them within one Region (ascending node id).
fn sorted(ids: &[&str]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| (*id).to_string()).collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn a_listing_is_refused_until_the_account_enables_the_integrated_experience() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);

    // The nodes ARE registered — the sibling describe of the same store reports them — so
    // the refusal below is the account's missing experience, not an empty store.
    let (status, described) =
        call_in_region(&router, REGION, "DescribeInstanceInformation", &json!({})).await;
    assert_eq!(
        status, 200,
        "the node describe must succeed, got {status} with {described}"
    );
    assert_eq!(
        described["InstanceInformationList"]
            .as_array()
            .map(Vec::len),
        Some(3),
        "the three seeded nodes of {REGION} must be registered, got {described}"
    );

    // One row per member a request may carry, including the two a listing would otherwise
    // refuse on (an unissued page token, a page size outside the modelled range): the
    // account has no node listing at all, so none of them is looked at. A `SyncName` naming
    // a sync no node listing reads is the one exception, answered below: that sync does not
    // exist whether or not the account has the experience, which is a different condition.
    let cases: &[(&str, Value)] = &[
        ("no member at all", json!({})),
        (
            "a filter",
            json!({ "Filters": [{ "Key": "InstanceId", "Values": [EC2_LINUX] }] }),
        ),
        ("a page size", json!({ "MaxResults": 1 })),
        ("the supported sync name", json!({ "SyncName": MANAGED_NODE_SYNC })),
        ("an unissued page token", json!({ "NextToken": "not-a-token" })),
    ];
    for (case, body) in cases {
        let (status, parsed) = list(&router, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "UnsupportedOperationException",
            "case {case:?}: expected UnsupportedOperationException, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            NOT_ENABLED,
            "case {case:?}: expected the not-enabled sentence, got {parsed}"
        );
    }

    // A `SyncName` naming a sync a node listing cannot read is reported as the sync that
    // does not exist, carrying the name the request asked for — the two refusals mean
    // different things, so the named sync is resolved before the account's experience is.
    let (status, parsed) = list(&router, &json!({ "SyncName": "inventory-sync" })).await;
    assert_eq!(
        status, 400,
        "case \"another sync name\": expected HTTP 400, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "ResourceDataSyncNotFoundException",
        "case \"another sync name\": expected the sync-not-found refusal, got {parsed}"
    );
    assert_eq!(
        parsed["SyncName"].as_str(),
        Some("inventory-sync"),
        "case \"another sync name\": the refusal echoes the sync the request named, got \
         {parsed}"
    );

    // The experience is per Region, like the sync it is read from: enabling it at one
    // endpoint leaves the other without a node listing.
    enable_experience(&router, OTHER_REGION, &[OTHER_REGION]).await;
    let (status, parsed) = list(&router, &json!({})).await;
    assert_eq!(
        status, 400,
        "case \"enabled in another Region\": expected HTTP 400, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "UnsupportedOperationException",
        "case \"enabled in another Region\": expected UnsupportedOperationException, got {parsed}"
    );
    let (_, ids) = {
        let (status, parsed) = list_in_region(&router, OTHER_REGION, &json!({})).await;
        assert_eq!(
            status, 200,
            "case \"the enabled Region\": expected HTTP 200, got {status} with {parsed}"
        );
        let ids = listed_ids("the enabled Region", &parsed);
        (parsed, ids)
    };
    assert_eq!(
        ids,
        sorted(&[REMOTE]),
        "case \"the enabled Region\": expected the node of {OTHER_REGION}, got {ids:?}"
    );
}

#[tokio::test]
async fn an_enabled_listing_reports_what_registering_persisted() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION]).await;
    let before = registered_count(&engine);

    let (body, ids) = listed(&router, "every node of the Region", &json!({})).await;
    assert_eq!(
        ids,
        sorted(&[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        "expected the three nodes of {REGION} in node-id order, got {ids:?}"
    );

    // The listing and the sibling describe read the ONE node store, so they report the same
    // nodes.
    let (status, described) =
        call_in_region(&router, REGION, "DescribeInstanceInformation", &json!({})).await;
    assert_eq!(
        status, 200,
        "the node describe must succeed, got {status} with {described}"
    );
    let described_ids: Vec<String> = described["InstanceInformationList"]
        .as_array()
        .expect("the describe carries InstanceInformationList")
        .iter()
        .map(|node| node["InstanceId"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        ids, described_ids,
        "the listing and the describe must report the same nodes, got {ids:?} and {described_ids:?}"
    );

    // One node, member by member: everything reported is what the record holds, and the two
    // attributes nothing records are reported by neither node.
    let nodes = body["Nodes"].as_array().expect("Nodes is a list");
    let hybrid = nodes
        .iter()
        .find(|node| node["Id"] == json!(HYBRID))
        .unwrap_or_else(|| panic!("the hybrid node must be reported, got {body}"));
    assert_eq!(
        hybrid["Region"],
        json!(REGION),
        "a node reports the Region it is held under, got {hybrid}"
    );
    assert_eq!(
        hybrid["Owner"]["AccountId"],
        json!(ACCOUNT),
        "a node reports the account that owns it, got {hybrid}"
    );
    assert_eq!(
        hybrid["Owner"]["OrganizationalUnitId"],
        Value::Null,
        "no organization record stands behind the account, got {hybrid}"
    );
    assert_eq!(
        hybrid["CaptureTime"].as_f64(),
        Some(CAPTURED_AT_SECS as f64),
        "a node reports when it last reported, got {hybrid}"
    );
    let instance = &hybrid["NodeType"]["Instance"];
    let expected: &[(&str, Value)] = &[
        ("AgentVersion", json!("3.3.2746.0")),
        ("ManagedStatus", json!("Managed")),
        ("Name", json!("edge-1")),
        ("PlatformType", json!("MacOS")),
        ("PlatformName", json!("macOS")),
        ("PlatformVersion", json!("14.4")),
        ("ResourceType", json!("ManagedInstance")),
        ("SourceType", json!("AWS::IoT::Thing")),
        ("SourceId", json!("thing-1")),
        ("SourceLocation", json!("edge-site-1")),
        // Nothing records which agent implementation reported, the zone id of a node, and —
        // for a hybrid node — its own instance status, address, host name or zone.
        ("AgentType", Value::Null),
        ("AvailabilityZone", Value::Null),
        ("AvailabilityZoneId", Value::Null),
        ("InstanceStatus", Value::Null),
        ("IpAddress", Value::Null),
        ("ComputerName", Value::Null),
    ];
    for (member, value) in expected {
        assert_eq!(
            &instance[member], value,
            "case {member:?}: expected {value}, got {instance}"
        );
    }

    // An EC2 node carries the members the instance behind it reported.
    let linux = nodes
        .iter()
        .find(|node| node["Id"] == json!(EC2_LINUX))
        .unwrap_or_else(|| panic!("the Linux node must be reported, got {body}"));
    let linux_instance = &linux["NodeType"]["Instance"];
    for (member, value) in [
        ("InstanceStatus", json!("Active")),
        ("IpAddress", json!("10.0.1.17")),
        ("ComputerName", json!("ip-10-0-1-17.us-west-2.compute.internal")),
        ("AvailabilityZone", json!("us-west-2a")),
        ("ResourceType", json!("EC2Instance")),
    ] {
        assert_eq!(
            linux_instance[member], value,
            "case {member:?}: expected {value}, got {linux_instance}"
        );
    }

    // A listing is a report: it leaves the store exactly as it found it, so a second one
    // answers the same.
    let (_, again) = listed(&router, "a second listing", &json!({})).await;
    assert_eq!(
        again, ids,
        "a second listing must report the same nodes, got {again:?}"
    );
    assert_eq!(
        registered_count(&engine),
        before,
        "a listing must register nothing, got {} nodes",
        registered_count(&engine)
    );
}

#[tokio::test]
async fn a_filter_selects_the_nodes_whose_attribute_it_names_matches() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION]).await;

    // One row per key of the modelled `NodeFilterKey` enum
    // (`smithy_query(query="shape", shape="NodeFilterKey")`: AgentType, AgentVersion,
    // ComputerName, InstanceId, InstanceStatus, IpAddress, ManagedStatus, PlatformName,
    // PlatformType, PlatformVersion, ResourceType, OrganizationalUnitId,
    // OrganizationalUnitPath, Region, AccountId, SourceType, SourceId, SourceLocation,
    // AvailabilityZone, AvailabilityZoneId), with the nodes seeded above that hold that
    // value — plus rows whose `Values` list unions and rows whose value no node holds.
    let cases: &[(&str, &[&str], &[&str])] = &[
        ("AccountId", &[ACCOUNT], &[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        ("AccountId", &["999999999999"], &[]),
        // Nothing records which agent implementation reported, so no node answers it.
        ("AgentType", &["amazon-ssm-agent"], &[]),
        ("AgentVersion", &["3.2.1234.0"], &[EC2_LINUX]),
        ("AgentVersion", &["3.1.1.0", "3.3.2746.0"], &[EC2_WINDOWS, HYBRID]),
        ("AgentVersion", &["9.9.9.9"], &[]),
        ("AvailabilityZone", &["us-west-2a"], &[EC2_LINUX]),
        // Nothing holds the id of the zone a node runs in.
        ("AvailabilityZoneId", &["usw2-az1"], &[]),
        ("ComputerName", &["EC2AMAZ-WIN"], &[EC2_WINDOWS]),
        ("InstanceId", &[HYBRID], &[HYBRID]),
        ("InstanceId", &[EC2_LINUX, HYBRID], &[EC2_LINUX, HYBRID]),
        ("InstanceId", &["i-0dead999"], &[]),
        ("InstanceStatus", &["Active"], &[EC2_LINUX, EC2_WINDOWS]),
        ("IpAddress", &["10.0.2.9"], &[EC2_WINDOWS]),
        // Every node this store holds is managed, and `All` covers managed and unmanaged
        // nodes alike (the modelled `ManagedStatus` enum).
        ("ManagedStatus", &["Managed"], &[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        ("ManagedStatus", &["All"], &[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        ("ManagedStatus", &["Unmanaged"], &[]),
        // No organization record stands behind the account.
        ("OrganizationalUnitId", &["ou-abcd-12345678"], &[]),
        ("OrganizationalUnitPath", &["r-abcd/ou-abcd-12345678"], &[]),
        ("PlatformName", &["Amazon Linux"], &[EC2_LINUX]),
        ("PlatformType", &["Linux"], &[EC2_LINUX]),
        ("PlatformType", &["Windows", "MacOS"], &[EC2_WINDOWS, HYBRID]),
        ("PlatformVersion", &["2019"], &[EC2_WINDOWS]),
        ("Region", &[REGION], &[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        ("Region", &[OTHER_REGION], &[]),
        ("ResourceType", &["EC2Instance"], &[EC2_LINUX, EC2_WINDOWS]),
        ("ResourceType", &["ManagedInstance"], &[HYBRID]),
        ("SourceId", &["thing-1"], &[HYBRID]),
        ("SourceLocation", &["edge-site-1"], &[HYBRID]),
        ("SourceType", &["AWS::IoT::Thing"], &[HYBRID]),
        ("SourceType", &["AWS::EC2::Instance"], &[]),
    ];
    for (key, values, expected) in cases {
        let case = format!("Key={key} Values={values:?}");
        let body = json!({ "Filters": [{ "Key": key, "Values": values }] });
        let (_, ids) = listed(&router, &case, &body).await;
        assert_eq!(
            ids,
            sorted(expected),
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
    }

    // One row per operator of the modelled `NodeFilterOperatorType` enum
    // (`smithy_query(query="shape", shape="NodeFilterOperatorType")`: Equal, NotEqual,
    // BeginWith), beside the relation a filter that names none states: the operator, the
    // key, the filter's values and the nodes it selects.
    type OperatorCase<'a> = (Option<&'a str>, &'a str, &'a [&'a str], &'a [&'a str]);
    let operators: &[OperatorCase] = &[
        (None, "PlatformType", &["Linux"], &[EC2_LINUX]),
        (Some("Equal"), "PlatformType", &["Linux"], &[EC2_LINUX]),
        (
            Some("NotEqual"),
            "PlatformType",
            &["Linux"],
            &[EC2_WINDOWS, HYBRID],
        ),
        (
            Some("BeginWith"),
            "AgentVersion",
            &["3.2"],
            &[EC2_LINUX],
        ),
        (
            Some("BeginWith"),
            "InstanceId",
            &["i-"],
            &[EC2_LINUX, EC2_WINDOWS],
        ),
        (Some("BeginWith"), "InstanceId", &["mi-"], &[HYBRID]),
    ];
    for (operator, key, values, expected) in operators {
        let case = format!("Type={operator:?} Key={key} Values={values:?}");
        let mut filter = json!({ "Key": key, "Values": values });
        if let Some(operator) = operator {
            filter["Type"] = json!(operator);
        }
        let (_, ids) = listed(&router, &case, &json!({ "Filters": [filter] })).await;
        assert_eq!(
            ids,
            sorted(expected),
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
    }

    // Filters INTERSECT: a node is reported only when every filter is satisfied. The
    // modelled `NodeFilterList` holds up to five.
    let combinations: &[(&str, Value, &[&str])] = &[
        (
            "two keys one node holds",
            json!([
                { "Key": "ResourceType", "Values": ["EC2Instance"] },
                { "Key": "PlatformType", "Values": ["Linux"] },
            ]),
            &[EC2_LINUX],
        ),
        (
            "two keys no node holds together",
            json!([
                { "Key": "ResourceType", "Values": ["ManagedInstance"] },
                { "Key": "PlatformType", "Values": ["Linux"] },
            ]),
            &[],
        ),
        (
            "five keys every EC2 node holds",
            json!([
                { "Key": "AccountId", "Values": [ACCOUNT] },
                { "Key": "Region", "Values": [REGION] },
                { "Key": "ManagedStatus", "Values": ["All"] },
                { "Key": "ResourceType", "Values": ["EC2Instance"] },
                { "Key": "InstanceStatus", "Values": ["Active"] },
            ]),
            &[EC2_LINUX, EC2_WINDOWS],
        ),
    ];
    for (case, filters, expected) in combinations {
        let (_, ids) = listed(&router, case, &json!({ "Filters": filters })).await;
        assert_eq!(
            ids,
            sorted(expected),
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn a_page_is_bounded_and_resumes_where_it_stopped() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION]).await;
    let all = sorted(&[EC2_LINUX, EC2_WINDOWS, HYBRID]);

    // The page sizes the modelled `MaxResults` admits (`range 1..50`): both published
    // bounds, the sizes around the node count, and a request naming none.
    for max_results in [None, Some(1), Some(2), Some(3), Some(4), Some(50)] {
        let case = format!("MaxResults={max_results:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({});
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = next_token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (page, ids) = listed(&router, &case, &body).await;
            if let Some(max_results) = max_results {
                assert!(
                    ids.len() <= max_results as usize,
                    "case {case:?}: a page holds at most {max_results} nodes, got {ids:?}"
                );
            }
            seen.extend(ids);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case {case:?}: a listing of {} nodes must not page forever, saw {seen:?}",
                all.len()
            );
            match page["NextToken"].as_str() {
                Some(token) => next_token = Some(token.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, all,
            "case {case:?}: the pages together must report every node exactly once, got {seen:?}"
        );
    }

    // A token this service did not hand out is refused, and the one it did resumes.
    let (page, first) = listed(&router, "the first page", &json!({ "MaxResults": 1 })).await;
    let token = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a bounded page hands out a NextToken, got {page}"))
        .to_string();
    let (_, resumed) = listed(
        &router,
        "the resumed page",
        &json!({ "NextToken": token.clone() }),
    )
    .await;
    assert_eq!(
        resumed,
        all[1..].to_vec(),
        "case \"the resumed page\": expected the nodes after {first:?}, got {resumed:?}"
    );
    for (case, candidate) in [
        ("prose", "not-a-token".to_string()),
        ("the node id alone", all[1].clone()),
        ("a Region with no node id", format!("{REGION}/")),
        ("a malformed node id", format!("{REGION}/i-zzz")),
        ("a token with no Region", format!("/{}", all[1])),
        ("the token with its Region mangled", token.replace(REGION, "us west 2")),
    ] {
        let (status, parsed) = list(&router, &json!({ "NextToken": candidate })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "InvalidNextToken",
            "case {case:?}: expected InvalidNextToken, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_named_sync_reports_the_nodes_of_the_regions_it_aggregates() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    // The managed node sync of this account aggregates both Regions' nodes.
    enable_experience(&router, REGION, &[OTHER_REGION, REGION]).await;

    let aggregated = {
        // The walk is in ascending `(region, node id)` order, so the nodes of
        // [`OTHER_REGION`] come before those of [`REGION`].
        let mut ids = vec![REMOTE.to_string()];
        ids.extend(sorted(&[EC2_LINUX, EC2_WINDOWS, HYBRID]));
        ids
    };
    let cases: &[(&str, Value, Vec<String>)] = &[
        (
            "no sync named: the Region the call was made at",
            json!({}),
            sorted(&[EC2_LINUX, EC2_WINDOWS, HYBRID]),
        ),
        (
            "the managed node sync: every Region it aggregates",
            json!({ "SyncName": MANAGED_NODE_SYNC }),
            aggregated.clone(),
        ),
        (
            "the managed node sync, narrowed to one of its Regions",
            json!({
                "SyncName": MANAGED_NODE_SYNC,
                "Filters": [{ "Key": "Region", "Values": [OTHER_REGION] }],
            }),
            sorted(&[REMOTE]),
        ),
    ];
    for (case, body, expected) in cases {
        let (listing, ids) = listed(&router, case, body).await;
        assert_eq!(
            &ids, expected,
            "case {case:?}: expected {expected:?}, got {ids:?}"
        );
        // Each node reports the Region it is held under, whichever Region the call was
        // made at.
        for node in listing["Nodes"].as_array().expect("Nodes is a list") {
            let expected_region = if node["Id"] == json!(REMOTE) {
                OTHER_REGION
            } else {
                REGION
            };
            assert_eq!(
                node["Region"],
                json!(expected_region),
                "case {case:?}: expected Region {expected_region}, got {node}"
            );
        }
    }

    // Paging walks the aggregated Regions in order and resumes across them.
    let mut seen: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut body = json!({ "SyncName": MANAGED_NODE_SYNC, "MaxResults": 1 });
        if let Some(token) = next_token.as_deref() {
            body["NextToken"] = json!(token);
        }
        let (page, ids) = listed(&router, "paging the aggregated listing", &body).await;
        seen.extend(ids);
        match page["NextToken"].as_str() {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
        assert!(
            seen.len() <= aggregated.len(),
            "paging the aggregated listing must not repeat a node, saw {seen:?}"
        );
    }
    assert_eq!(
        seen, aggregated,
        "the pages together must report every aggregated node exactly once, got {seen:?}"
    );

    // "The name of the supported resource data sync is AWS-QuickSetup-ManagedNode": any
    // other name names no sync a node listing can read, whether or not the account holds a
    // sync of that name.
    let (status, created) = call_in_region(
        &router,
        REGION,
        "CreateResourceDataSync",
        &json!({
            "SyncName": "inventory-sync",
            "S3Destination": { "BucketName": "nodes", "SyncFormat": "JsonSerDe", "Region": REGION },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "creating a second sync must succeed, got {status} with {created}"
    );
    for case in ["inventory-sync", "aws-quicksetup-managednode", "AWS-QuickSetup-ManagedNodes"] {
        let (status, parsed) = list(&router, &json!({ "SyncName": case })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceDataSyncNotFoundException",
            "case {case:?}: expected ResourceDataSyncNotFoundException, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            format!("ResourceDataSync with name {case} synctype SyncFromSource does not exists"),
            "case {case:?}: expected the sync-not-found sentence naming it, got {parsed}"
        );
        assert_eq!(
            parsed["SyncName"],
            json!(case),
            "case {case:?}: the refusal names the sync the request addressed, got {parsed}"
        );
    }
}
