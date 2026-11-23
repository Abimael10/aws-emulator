// SPDX-License-Identifier: CC0-1.0

//! `ListNodesSummary` end to end: the counts the managed nodes of the Systems Manager
//! integrated experience are grouped into, narrowed by filters, aggregated by the sync a
//! request names and served one page at a time.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router_with_engine`], [`call_in_region`]) dispatches `POST /`
//! with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.
//!
//! The experience itself is turned on through this service's OWN operation
//! (`CreateResourceDataSync`, which puts the Amazon Web Services managed node sync in the
//! account), so what a summary answers follows a write a caller made rather than a flag a test
//! planted. The nodes are seeded through the service's state
//! (`state::ComAmazonawsSsmAmazonssmState::insert_managed_node`, the one node store `ListNodes`
//! and the node describes read too), because a node enters it only by REGISTERING and this
//! suite registers none — the same seam `all/node_listing.rs` uses for the sibling listing.
//!
//! RULES under test:
//!
//! 1. An account that has not enabled the integrated experience is refused
//!    `UnsupportedOperationException` with the sentence the service words that refusal with —
//!    the same refusal, and the same sentence, the sibling `ListNodes` answers such an account
//!    with, whatever the request asks for.
//! 2. A summary is the SAME node selection a listing makes, counted by the attribute an
//!    aggregator names: one row per value the selected nodes hold, ascending, carrying that
//!    value and the count of nodes under it — and a node that holds no value for the attribute
//!    is in no row of it.
//! 3. A nested aggregator refines each group, so a row then carries both attribute values
//!    beside the count.
//! 4. The rows are paged like every other read of this service: `MaxResults` rows per page and
//!    a `NextToken` that resumes after the row the page stopped at, with a token this service
//!    did not issue refused as `InvalidNextToken`.
//! 5. An aggregator member outside the closed enum the model declares for it is refused
//!    against that enum, an aggregation nested deeper than a summary row can carry is
//!    `InvalidAggregatorException`, and a `SyncName` other than the managed node sync's is
//!    `ResourceDataSyncNotFoundException`.
//!
//! The suites are tables: each row is one input along the axis the rule under test generalises
//! over (the attribute aggregated, the filter, the Region set, the page size, the aggregator
//! member that is wrong, the request members carried).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router_with_engine, REGION};

/// A second Region, to pin which Regions' nodes one summary counts.
const OTHER_REGION: &str = "us-east-1";

/// The name of the resource data sync the integrated experience puts in an account: "the name
/// of the supported resource data sync is AWS-QuickSetup-ManagedNode"
/// (`smithy_query(query="doc", shape="ListNodesSummaryRequest", member="SyncName")`).
const MANAGED_NODE_SYNC: &str = "AWS-QuickSetup-ManagedNode";

/// The `UnsupportedOperationException` sentence an account without the experience is answered
/// with, as the service words it — the sentence the sibling `ListNodes` answers with too.
const NOT_ENABLED: &str = "This is an unsupported operation for this account. You must first \
                           enable the Systems Manager integrated experience in your account.";

/// The key a summary row carries its count under.
const COUNT: &str = "Count";

/// The only `AggregatorType` the model declares (`smithy_query(query="shape",
/// shape="NodeAggregatorType")`: `Count`).
const AGGREGATOR_TYPE: &str = "Count";

/// The only `NodeTypeName` it declares (`smithy_query(query="shape", shape="NodeTypeName")`:
/// `Instance`).
const TYPE_NAME: &str = "Instance";

/// When every seeded node last reported, as its record holds it.
const CAPTURED_AT_SECS: u64 = 1_700_000_000;

/// One summary row as it travels on the wire: the modelled `NodeSummary` map, which carries
/// the attributes the row groups by and its count.
type SummaryRow = BTreeMap<String, String>;

/// One case of the aggregation table: its name, the attribute aggregated, and the rows it
/// expects.
type AggregationCase<'a> = (&'a str, &'a str, Vec<SummaryRow>);

/// One aggregator, as the request body writes it.
fn aggregator(attribute: &str) -> Value {
    json!({
        "AggregatorType": AGGREGATOR_TYPE,
        "TypeName": TYPE_NAME,
        "AttributeName": attribute,
    })
}

/// One aggregator with `nested` refining it.
fn nested_aggregator(attribute: &str, nested: &str) -> Value {
    let mut outer = aggregator(attribute);
    outer["Aggregators"] = json!([aggregator(nested)]);
    outer
}

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

/// How many nodes the store holds, to pin that a summary wrote nothing.
fn registered_count(engine: &Arc<engine::InMemoryEngine>) -> usize {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .managed_nodes
        .len()
}

/// Four nodes in [`REGION`] and one in [`OTHER_REGION`], spanning platform, agent version,
/// zone and node kind — including a hybrid node that reports no zone of its own, so an
/// aggregation by zone has a node with no value for it.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(CAPTURED_AT_SECS);

    let mut first = ManagedNodeRecord::registering("i-0abc1234", RESOURCE_TYPE_EC2_INSTANCE, at);
    first.properties.platform_type = Some("Linux".to_string());
    first.properties.agent_version = Some("3.2.0.0".to_string());
    first.properties.availability_zone = Some("us-west-2a".to_string());
    register(engine, REGION, first);

    let mut second = ManagedNodeRecord::registering("i-0def5678", RESOURCE_TYPE_EC2_INSTANCE, at);
    second.properties.platform_type = Some("Linux".to_string());
    second.properties.agent_version = Some("3.2.0.0".to_string());
    second.properties.availability_zone = Some("us-west-2b".to_string());
    register(engine, REGION, second);

    let mut windows = ManagedNodeRecord::registering("i-0999aaaa", RESOURCE_TYPE_EC2_INSTANCE, at);
    windows.properties.platform_type = Some("Windows".to_string());
    windows.properties.agent_version = Some("3.1.0.0".to_string());
    windows.properties.availability_zone = Some("us-west-2a".to_string());
    register(engine, REGION, windows);

    let mut hybrid =
        ManagedNodeRecord::registering("mi-01234567890123456", RESOURCE_TYPE_MANAGED_INSTANCE, at);
    hybrid.properties.platform_type = Some("MacOS".to_string());
    hybrid.properties.agent_version = Some("3.3.0.0".to_string());
    // A hybrid node reports no zone of its own: nothing holds one, so an aggregation by zone
    // counts it under none.
    register(engine, REGION, hybrid);

    let mut remote = ManagedNodeRecord::registering("i-0eeeeeee", RESOURCE_TYPE_EC2_INSTANCE, at);
    remote.properties.platform_type = Some("Linux".to_string());
    remote.properties.agent_version = Some("3.2.0.0".to_string());
    remote.properties.availability_zone = Some("us-east-1a".to_string());
    register(engine, OTHER_REGION, remote);
}

/// Turn the integrated experience on at `region`'s endpoint, through this service's own
/// `CreateResourceDataSync`: the managed node sync aggregating `source_regions`.
async fn enable_experience(router: &MultiServiceRouter, region: &str, source_regions: &[&str]) {
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

/// `ListNodesSummary` at [`REGION`]'s endpoint.
async fn summarise(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in_region(router, REGION, "ListNodesSummary", body).await
}

/// The rows a summary reported, in the order it reported them, each as its `(key, value)`
/// entries — asserting the call was served.
async fn rows(
    router: &MultiServiceRouter,
    case: &str,
    body: &Value,
) -> Vec<BTreeMap<String, String>> {
    let (status, reported) = summarise(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the summary to be served, got {status} with {reported}"
    );
    parsed_rows(&reported)
}

/// The rows one answered page carries, in the order it carries them.
fn parsed_rows(reported: &Value) -> Vec<BTreeMap<String, String>> {
    reported["Summary"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| {
            row.as_object()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|(key, value)| {
                            (key.clone(), value.as_str().unwrap_or_default().to_string())
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// One expected row: the `(attribute, value)` pairs it groups by, and the count.
fn row(grouped_by: &[(&str, &str)], count: usize) -> SummaryRow {
    let mut expected: BTreeMap<String, String> = grouped_by
        .iter()
        .map(|(attribute, value)| ((*attribute).to_string(), (*value).to_string()))
        .collect();
    expected.insert(COUNT.to_string(), count.to_string());
    expected
}

/// RULE 1: an account that has not enabled the integrated experience has no node summary at
/// all, whatever the request asks for — the refusal and the sentence the sibling listing
/// answers such an account with.
#[tokio::test]
async fn an_account_without_the_integrated_experience_is_refused() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    let before = registered_count(&engine);

    // (case, the request body) — spanning the members a summary takes.
    let cases: Vec<(&str, Value)> = vec![
        (
            "one aggregator and nothing else",
            json!({ "Aggregators": [aggregator("Region")] }),
        ),
        (
            "an aggregator and a page size",
            json!({ "Aggregators": [aggregator("Region")], "MaxResults": 50 }),
        ),
        (
            "two aggregators",
            json!({ "Aggregators": [aggregator("PlatformType"), aggregator("ResourceType")] }),
        ),
        (
            "a nested aggregator",
            json!({ "Aggregators": [nested_aggregator("Region", "PlatformType")] }),
        ),
        (
            "an aggregator beside a filter",
            json!({
                "Aggregators": [aggregator("PlatformType")],
                "Filters": [{ "Key": "PlatformType", "Values": ["Linux"] }],
            }),
        ),
        (
            "an aggregator beside the supported sync name",
            json!({ "Aggregators": [aggregator("Region")], "SyncName": MANAGED_NODE_SYNC }),
        ),
    ];

    for (case, body) in &cases {
        let (status, refused) = summarise(&router, body).await;
        assert_eq!(
            (status, error_code(&refused), error_message(&refused)),
            (
                400,
                "UnsupportedOperationException".to_string(),
                NOT_ENABLED.to_string()
            ),
            "case {case:?}: expected the not-enabled refusal, got {status} with {refused}"
        );
        // The sibling listing answers the same account identically, so the two operations
        // agree about an account that has not enabled the experience.
        let (listed_status, listed) =
            call_in_region(&router, REGION, "ListNodes", &json!({})).await;
        assert_eq!(
            (listed_status, error_code(&listed), error_message(&listed)),
            (status, error_code(&refused), error_message(&refused)),
            "case {case:?}: the summary and the listing must answer identically, got \
             {listed_status} with {listed}"
        );
    }

    assert_eq!(
        registered_count(&engine),
        before,
        "a refused summary must write nothing to the node store"
    );
}

/// RULE 2: a summary counts the nodes by the attribute the aggregator names — one row per
/// value held, ascending, and no row for a node that holds no value for it. RULE 5's sync
/// axis is here too: which Regions' nodes are counted is what `SyncName` decides.
#[tokio::test]
async fn a_summary_counts_the_nodes_by_the_attribute_it_aggregates() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION, OTHER_REGION]).await;
    let before = registered_count(&engine);

    // (case, the attribute aggregated, the rows the Region's own nodes make)
    let cases: Vec<AggregationCase<'_>> = vec![
        (
            "by platform type",
            "PlatformType",
            vec![
                row(&[("PlatformType", "Linux")], 2),
                row(&[("PlatformType", "MacOS")], 1),
                row(&[("PlatformType", "Windows")], 1),
            ],
        ),
        (
            "by node kind",
            "ResourceType",
            vec![
                row(&[("ResourceType", RESOURCE_TYPE_EC2_INSTANCE)], 3),
                row(&[("ResourceType", RESOURCE_TYPE_MANAGED_INSTANCE)], 1),
            ],
        ),
        (
            "by agent version",
            "AgentVersion",
            vec![
                row(&[("AgentVersion", "3.1.0.0")], 1),
                row(&[("AgentVersion", "3.2.0.0")], 2),
                row(&[("AgentVersion", "3.3.0.0")], 1),
            ],
        ),
        (
            // The hybrid node holds no zone, so it is in no row of this aggregation: the
            // counts sum to three of the Region's four nodes.
            "by availability zone, which one node holds none of",
            "AvailabilityZone",
            vec![
                row(&[("AvailabilityZone", "us-west-2a")], 2),
                row(&[("AvailabilityZone", "us-west-2b")], 1),
            ],
        ),
        (
            "by Region, which every node holds",
            "Region",
            vec![row(&[("Region", REGION)], 4)],
        ),
    ];

    for (case, attribute, expected) in &cases {
        let reported = rows(
            &router,
            case,
            &json!({ "Aggregators": [aggregator(attribute)] }),
        )
        .await;
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected the counts of {attribute} to be {expected:?}, got \
             {reported:?}"
        );
    }

    // A summary that names the managed node sync counts the nodes of every Region that sync
    // aggregates, so the same aggregation reports a row per Region.
    let aggregated = rows(
        &router,
        "the aggregated summary by Region",
        &json!({ "Aggregators": [aggregator("Region")], "SyncName": MANAGED_NODE_SYNC }),
    )
    .await;
    assert_eq!(
        aggregated,
        vec![
            row(&[("Region", OTHER_REGION)], 1),
            row(&[("Region", REGION)], 4),
        ],
        "expected the aggregated summary to count both Regions' nodes, got {aggregated:?}"
    );

    // A filter narrows the summary exactly as it narrows a listing: only the nodes it selects
    // are counted.
    let filtered = rows(
        &router,
        "a filtered summary",
        &json!({
            "Aggregators": [aggregator("AvailabilityZone")],
            "Filters": [{ "Key": "PlatformType", "Values": ["Linux"] }],
        }),
    )
    .await;
    assert_eq!(
        filtered,
        vec![
            row(&[("AvailabilityZone", "us-west-2a")], 1),
            row(&[("AvailabilityZone", "us-west-2b")], 1),
        ],
        "expected the filter to narrow the counts, got {filtered:?}"
    );

    // Two aggregators report the rows of each, in the order the request named them.
    let both = rows(
        &router,
        "two aggregators at once",
        &json!({ "Aggregators": [aggregator("ResourceType"), aggregator("PlatformType")] }),
    )
    .await;
    assert_eq!(
        both,
        vec![
            row(&[("ResourceType", RESOURCE_TYPE_EC2_INSTANCE)], 3),
            row(&[("ResourceType", RESOURCE_TYPE_MANAGED_INSTANCE)], 1),
            row(&[("PlatformType", "Linux")], 2),
            row(&[("PlatformType", "MacOS")], 1),
            row(&[("PlatformType", "Windows")], 1),
        ],
        "expected each aggregator's rows in the order the request named them, got {both:?}"
    );

    assert_eq!(
        registered_count(&engine),
        before,
        "a summary must not change what a later caller sees"
    );
}

/// RULE 3: a nested aggregator refines each group, so a row carries both attribute values
/// beside its count.
#[tokio::test]
async fn a_nested_aggregator_refines_each_group() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION, OTHER_REGION]).await;

    let refined = rows(
        &router,
        "Region refined by platform type",
        &json!({
            "Aggregators": [nested_aggregator("Region", "PlatformType")],
            "SyncName": MANAGED_NODE_SYNC,
        }),
    )
    .await;
    assert_eq!(
        refined,
        vec![
            row(&[("Region", OTHER_REGION), ("PlatformType", "Linux")], 1),
            row(&[("Region", REGION), ("PlatformType", "Linux")], 2),
            row(&[("Region", REGION), ("PlatformType", "MacOS")], 1),
            row(&[("Region", REGION), ("PlatformType", "Windows")], 1),
        ],
        "expected each Region's nodes counted by platform type, got {refined:?}"
    );
    // The refined counts of one Region sum to that Region's own count, so the refinement
    // partitions the group rather than inventing nodes.
    let unrefined = rows(
        &router,
        "the same aggregation unrefined",
        &json!({ "Aggregators": [aggregator("Region")], "SyncName": MANAGED_NODE_SYNC }),
    )
    .await;
    let total: usize = refined
        .iter()
        .filter(|reported| reported.get("Region").map(String::as_str) == Some(REGION))
        .filter_map(|reported| reported.get(COUNT))
        .filter_map(|count| count.parse::<usize>().ok())
        .sum();
    assert_eq!(
        Some(total.to_string()),
        unrefined
            .iter()
            .find(|reported| reported.get("Region").map(String::as_str) == Some(REGION))
            .and_then(|reported| reported.get(COUNT))
            .cloned(),
        "expected the refined counts of {REGION} to sum to its own count, got {refined:?} and \
         {unrefined:?}"
    );
}

/// RULE 4: the rows are paged — each page holds at most `MaxResults` of them, the pages
/// together report every row exactly once, and a token this service did not issue is refused.
#[tokio::test]
async fn the_rows_are_paged_and_an_unissued_token_is_refused() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION, OTHER_REGION]).await;

    let whole = rows(
        &router,
        "the whole summary",
        &json!({ "Aggregators": [nested_aggregator("Region", "PlatformType")], "SyncName": MANAGED_NODE_SYNC }),
    )
    .await;
    assert!(
        whole.len() > 1,
        "the summary must hold more than one row to page, got {whole:?}"
    );

    for page_size in 1..=whole.len() {
        let case = format!("paging {page_size} rows at a time");
        let mut seen: Vec<BTreeMap<String, String>> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut body = json!({
                "Aggregators": [nested_aggregator("Region", "PlatformType")],
                "SyncName": MANAGED_NODE_SYNC,
                "MaxResults": page_size,
            });
            if let Some(token) = next_token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (status, page) = summarise(&router, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a page to be served, got {status} with {page}"
            );
            let reported = parsed_rows(&page);
            assert!(
                reported.len() <= page_size,
                "case {case:?}: a page holds at most {page_size} rows, got {reported:?}"
            );
            seen.extend(reported);
            match page["NextToken"].as_str() {
                Some(token) => next_token = Some(token.to_string()),
                None => break,
            }
            assert!(
                seen.len() <= whole.len(),
                "case {case:?}: paging must not repeat a row, saw {seen:?}"
            );
        }
        assert_eq!(
            seen, whole,
            "case {case:?}: the pages together must report every row exactly once, got {seen:?}"
        );
    }

    // (case, a value that is no token this read issued)
    for (case, token) in [
        ("prose", "not-a-real-token".to_string()),
        ("a row position in the clear", "0\u{1f}us-west-2".to_string()),
        ("hex that carries no authenticator", "abcdef".to_string()),
    ] {
        let (status, refused) = summarise(
            &router,
            &json!({ "Aggregators": [aggregator("Region")], "NextToken": token }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidNextToken".to_string()),
            "case {case:?}: expected InvalidNextToken for {token:?}, got {status} with {refused}"
        );
    }
}

/// RULE 5: an aggregator naming an aggregation this summary cannot make is refused — each
/// member against the closed enum the model declares for it, and a nesting deeper than a
/// summary row can carry as an invalid aggregator — and a `SyncName` other than the managed
/// node sync's names no sync a summary can read.
#[tokio::test]
async fn an_aggregation_the_summary_cannot_make_is_refused() {
    let (router, engine) = router_with_engine();
    seed_nodes(&engine);
    enable_experience(&router, REGION, &[REGION]).await;

    // (case, the Aggregators the request carries) — one row per member that can name
    // something outside the enum the model declares for it, outermost and nested.
    let outside_the_enums: Vec<(&str, Value)> = vec![
        (
            "an aggregator type the model does not declare",
            json!([{ "AggregatorType": "Sum", "TypeName": TYPE_NAME, "AttributeName": "Region" }]),
        ),
        (
            "a data type name it does not declare",
            json!([{
                "AggregatorType": AGGREGATOR_TYPE,
                "TypeName": "Document",
                "AttributeName": "Region",
            }]),
        ),
        (
            "an attribute outside the ones a node summary groups by",
            json!([{
                "AggregatorType": AGGREGATOR_TYPE,
                "TypeName": TYPE_NAME,
                "AttributeName": "InstanceId",
            }]),
        ),
        (
            "a nested aggregator whose own member is outside its enum",
            json!([{
                "AggregatorType": AGGREGATOR_TYPE,
                "TypeName": TYPE_NAME,
                "AttributeName": "Region",
                "Aggregators": [{
                    "AggregatorType": "Sum",
                    "TypeName": TYPE_NAME,
                    "AttributeName": "PlatformType",
                }],
            }]),
        ),
    ];

    for (case, aggregators) in &outside_the_enums {
        let (status, refused) = summarise(&router, &json!({ "Aggregators": aggregators })).await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationException".to_string()),
            "case {case:?}: a member outside its enum is refused against that enum, got \
             {status} with {refused}"
        );
    }

    // A `NodeSummary` holds at most three entries, one of them the count, so a row can carry
    // at most two attribute values: an aggregation nested deeper than that is one no row could
    // report, which is the modelled invalid-aggregator refusal.
    let mut too_deep = nested_aggregator("Region", "PlatformType");
    too_deep["Aggregators"] = json!([nested_aggregator("PlatformType", "ResourceType")]);
    let (status, refused) = summarise(&router, &json!({ "Aggregators": [too_deep] })).await;
    assert_eq!(
        (status, error_code(&refused)),
        (400, "InvalidAggregatorException".to_string()),
        "case \"an aggregation nested deeper than a row can carry\": expected \
         InvalidAggregatorException, got {status} with {refused}"
    );

    // "The name of the supported resource data sync is AWS-QuickSetup-ManagedNode": any other
    // name names no sync whose nodes a summary could count.
    for case in ["inventory-sync", "aws-quicksetup-managednode"] {
        let (status, refused) = summarise(
            &router,
            &json!({ "Aggregators": [aggregator("Region")], "SyncName": case }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ResourceDataSyncNotFoundException".to_string()),
            "case {case:?}: expected ResourceDataSyncNotFoundException, got {status} with \
             {refused}"
        );
        assert_eq!(
            refused["SyncName"],
            json!(case),
            "case {case:?}: the refusal names the sync the request addressed, got {refused}"
        );
    }
}
