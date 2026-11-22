// SPDX-License-Identifier: CC0-1.0

//! `GetOpsSummary` end to end: a summary of this account's operations metadata (OpsData),
//! narrowed by OpsData filters, counted by aggregators and served one page at a time.
//!
//! The RULE asserted here: OpsData is a PROJECTION of the resources this service holds —
//! "OpsData can include information about […] OpsItems as well as information about any
//! Amazon Web Services resource or service configured to report OpsData" (`GetOpsSummary`)
//! — under the data types the model publishes ("Valid value: `AWS:OpsItem`,
//! `AWS:EC2InstanceInformation`, `AWS:OpsItemTrendline`, or `AWS:ComplianceSummary`",
//! `OpsResultAttribute$TypeName`). So a summary reports, for the Regions it reads, one entity
//! per item of the requested type whose attributes satisfy every filter, or the counts of
//! those items per distinct value of the attribute an aggregator names — and a request naming
//! a type, a filter key or an aggregator the service cannot serve is refused with the modelled
//! error for it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — and every OpsItem a summary reports was opened by a
//! `CreateOpsItem` of the same suite, so a response that is not computed from persisted state
//! cannot pass.
//!
//! The tables are rows along what the rule generalises over: the ATTRIBUTE (text, number,
//! time, custom data), the OPERATOR (each of the modelled `OpsFilterOperatorType` values), how
//! many values one filter offers, how many filters narrow together, the OpsData TYPE, the
//! nesting of aggregators, the page size and the Region.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router_with_engine, REGION,
};

/// The operation under test.
const OPERATION: &str = "GetOpsSummary";

/// A second Region of the same account, to pin which Regions' OpsData a summary reads.
const OTHER_REGION: &str = "eu-west-1";

/// The OpsData type whose items are OpsCenter's OpsItems, and the type a summary that names
/// none reports.
const TYPE_OPS_ITEM: &str = "AWS:OpsItem";

/// The OpsData type whose items are the managed nodes of a Region.
const TYPE_EC2: &str = "AWS:EC2InstanceInformation";

/// A published OpsData type this emulator has collected nothing for.
const TYPE_COMPLIANCE: &str = "AWS:ComplianceSummary";

/// The other published OpsData type nothing has reported for.
const TYPE_TRENDLINE: &str = "AWS:OpsItemTrendline";

/// The sync type that aggregates OpsData "from multiple Amazon Web Services Regions"
/// (`CreateResourceDataSync`'s `SyncType` doc).
const FROM_SOURCE: &str = "SyncFromSource";

/// The sentence `InvalidAggregatorException` carries, which is that error class's own
/// documentation.
const INVALID_AGGREGATOR: &str = "The specified aggregator isn't valid for the group type. \
                                  Verify that the aggregator you provided is supported.";

/// The sentence `InvalidFilter` carries, which is that error class's own documentation.
const INVALID_FILTER: &str =
    "The filter name isn't valid. Verify that you entered the correct name and try again.";

/// The sentence `InvalidTypeNameException` carries, which is that error class's own
/// documentation.
const INVALID_TYPE_NAME: &str = "The parameter type name isn't valid.";

/// The instant the seeded node last reported, as a whole number of seconds.
const NODE_LAST_PING: Duration = Duration::from_secs(1_700_000_000);

/// [`NODE_LAST_PING`] as the modelled `OpsEntityItemCaptureTime` spells it (pattern
/// `^(20)[0-9][0-9]-(0[1-9]|1[012])-([12][0-9]|3[01]|0[1-9])(T)…(Z)$`).
const NODE_LAST_PING_RENDERED: &str = "2023-11-14T22:13:20Z";

/// A filter key over one OpsData type: the dotted `<TypeName>.<AttributeName>` form.
fn key(type_name: &str, attribute: &str) -> String {
    format!("{type_name}.{attribute}")
}

/// One OpsData filter, with the operator it names.
fn filter(type_name: &str, attribute: &str, values: &[&str], operator: &str) -> Value {
    json!({ "Key": key(type_name, attribute), "Values": values, "Type": operator })
}

/// One counting aggregator over an attribute of an OpsData type.
fn aggregator(type_name: &str, attribute: &str) -> Value {
    json!({ "AggregatorType": "count", "TypeName": type_name, "AttributeName": attribute })
}

/// A router with just this service mounted, and the engine it was mounted on — the handle the
/// node cases seed the node store through.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    router_with_engine()
}

/// Open one OpsItem through the front door, answering its id.
async fn open(router: &MultiServiceRouter, region: &str, body: &Value) -> String {
    let (status, response) = call_in_region(router, region, "CreateOpsItem", body).await;
    assert_eq!(
        status, 200,
        "CreateOpsItem {body} must succeed, got {status} {response}"
    );
    response["OpsItemId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateOpsItem {body} must report an OpsItemId, got {response}"))
        .to_string()
}

/// The OpsItems every table runs against: three in the harness's Region spanning status,
/// source, severity, category, priority, type and custom data, and one in [`OTHER_REGION`].
async fn seed_ops_items(router: &MultiServiceRouter) {
    for body in [
        json!({
            "Title": "disk is filling up",
            "Description": "the data volume is at 91%",
            "Source": "fleet-monitoring",
            "Severity": "2",
            "Category": "Availability",
            "Priority": 2,
            "OperationalData": {
                "team": {"Value": "platform", "Type": "SearchableString"},
            },
        }),
        json!({
            "Title": "latency regression in checkout",
            "Description": "p99 doubled after the deploy",
            "Source": "fleet-monitoring",
            "Severity": "3",
            "Category": "Performance",
            "Priority": 4,
        }),
        json!({
            "Title": "duplicate work items about one node",
            "Description": "several OpsItems name the same node",
            "Source": "opscenter-insights",
            "Severity": "4",
            "Category": "Cost",
            "Priority": 5,
            "OpsItemType": "/aws/insight",
        }),
    ] {
        open(router, REGION, &body).await;
    }
    open(
        router,
        OTHER_REGION,
        &json!({
            "Title": "disk is filling up elsewhere",
            "Description": "another Region's volume is at 91%",
            "Source": "fleet-monitoring",
            "Severity": "2",
            "Category": "Availability",
            "Priority": 2,
        }),
    )
    .await;
}

/// Register one managed node in `region`, the way its own registration would.
fn register_node(engine: &Arc<engine::InMemoryEngine>, region: &str, id: &str, platform: &str) {
    let at = SystemTime::UNIX_EPOCH + NODE_LAST_PING;
    let mut node = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    node.properties.platform_type = Some(platform.to_string());
    node.properties.platform_name = Some(format!("{platform} image"));
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, node),
        "a seeded node must carry the node id it is stored under"
    );
}

/// How many OpsItems the store holds, to pin that a summary wrote nothing.
fn held_ops_items(engine: &Arc<engine::InMemoryEngine>) -> usize {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .ops_items
        .len()
}

/// `GetOpsSummary` with `body`, asserting it succeeded.
async fn summary(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, OPERATION, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// The entities of a successful summary.
fn entities(body: &Value) -> &Vec<Value> {
    body["Entities"].as_array().expect("a summary carries Entities")
}

/// The titles a summary reported, sorted, so a case compares a SET of OpsItems.
async fn reported_titles(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<String> {
    let parsed = summary(router, case, body).await;
    let mut titles: Vec<String> = entities(&parsed)
        .iter()
        .map(|entity| {
            entity["Data"][TYPE_OPS_ITEM]["Content"][0]["Title"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every reported OpsItem carries a Title, got {parsed}")
                })
                .to_string()
        })
        .collect();
    titles.sort();
    titles
}

/// `want` sorted, the order [`reported_titles`] compares against.
fn sorted(want: &[&str]) -> Vec<String> {
    let mut want: Vec<String> = want.iter().map(|title| (*title).to_string()).collect();
    want.sort();
    want
}

/// One aggregated content entry, as an ordered map so a row is compared whatever order the
/// attributes serialise in.
type AggregatedRow = BTreeMap<String, String>;

/// One expected aggregated row.
fn row(pairs: &[(&str, &str)]) -> AggregatedRow {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// The content entries of the one aggregated entity, each as an ordered map.
async fn aggregated_rows(
    router: &MultiServiceRouter,
    case: &str,
    body: &Value,
    type_name: &str,
) -> Vec<AggregatedRow> {
    let parsed = summary(router, case, body).await;
    let reported = entities(&parsed);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: counts are reported as one entity, got {parsed}"
    );
    assert_eq!(
        reported[0]["Id"].as_str(),
        Some(""),
        "case {case:?}: a count belongs to no resource, got {parsed}"
    );
    reported[0]["Data"][type_name]["Content"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the item carries Content, got {parsed}"))
        .iter()
        .map(|entry| {
            entry
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: every entry is a map, got {parsed}"))
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        value
                            .as_str()
                            .unwrap_or_else(|| {
                                panic!("case {case:?}: every entry value is a string, got {parsed}")
                            })
                            .to_string(),
                    )
                })
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn an_ops_item_is_reported_as_the_ops_data_its_writes_persisted() {
    let (router, engine) = service();
    seed_ops_items(&router).await;
    let before = held_ops_items(&engine);

    let parsed = summary(&router, "the default type", &json!({})).await;
    let reported = entities(&parsed);
    assert_eq!(
        reported.len(),
        3,
        "one entity per OpsItem of the Region, got {parsed}"
    );

    let opened = reported
        .iter()
        .find(|entity| {
            entity["Data"][TYPE_OPS_ITEM]["Content"][0]["Title"].as_str()
                == Some("disk is filling up")
        })
        .unwrap_or_else(|| panic!("the OpsItem that was opened is reported, got {parsed}"));
    let content = &opened["Data"][TYPE_OPS_ITEM]["Content"];
    assert_eq!(
        content.as_array().map(Vec::len),
        Some(1),
        "one entry per item, got {parsed}"
    );
    let entry = &content[0];
    assert_eq!(
        entry["Status"].as_str(),
        Some("Open"),
        "the entry reports the status the create persisted, got {entry}"
    );
    assert_eq!(
        entry["Source"].as_str(),
        Some("fleet-monitoring"),
        "the entry reports the source the create persisted, got {entry}"
    );
    assert_eq!(
        entry["Priority"].as_str(),
        Some("2"),
        "a number is reported as the text of that number, got {entry}"
    );
    assert_eq!(
        entry["Severity"].as_str(),
        Some("2"),
        "the entry reports the severity the create persisted, got {entry}"
    );
    assert_eq!(
        entry["Category"].as_str(),
        Some("Availability"),
        "the entry reports the category the create persisted, got {entry}"
    );
    assert_eq!(
        entry["OpsItemType"].as_str(),
        Some("/aws/issue"),
        "the entry reports the type the create defaulted to, got {entry}"
    );
    assert_eq!(
        entry["OpsItemId"].as_str(),
        opened["Id"].as_str(),
        "the entity is identified by the resource its OpsData is about, got {parsed}"
    );
    assert!(
        entry["OperationalData"]
            .as_str()
            .is_some_and(|data| data.contains("platform")),
        "the custom data the create supplied is reported as one value, got {entry}"
    );
    // An attribute the OpsItem holds no value for has no value to report.
    assert!(
        entry.get("ActualStartTime").is_none(),
        "no entry for an attribute the OpsItem holds nothing for, got {entry}"
    );

    // Every instant is reported as the modelled `OpsEntityItemCaptureTime` spells it.
    for member in ["CreatedTime", "LastModifiedTime"] {
        let rendered = entry[member].as_str().unwrap_or_default().to_string();
        assert!(
            rendered.len() == "2024-01-01T00:00:00Z".len() && rendered.ends_with('Z'),
            "case {member:?}: expected whole seconds ending in Z, got {rendered:?}"
        );
    }
    assert_eq!(
        opened["Data"][TYPE_OPS_ITEM]["CaptureTime"].as_str(),
        entry["LastModifiedTime"].as_str(),
        "the OpsData was captured when the OpsItem last changed, got {parsed}"
    );

    assert_eq!(
        held_ops_items(&engine),
        before,
        "a summary is a read: it wrote nothing"
    );
}

#[tokio::test]
async fn an_edit_of_an_ops_item_is_what_the_next_summary_reports() {
    let (router, _engine) = service();
    let id = open(
        &router,
        REGION,
        &json!({
            "Title": "disk is filling up",
            "Description": "the data volume is at 91%",
            "Source": "fleet-monitoring",
            "Priority": 2,
        }),
    )
    .await;

    for (case, update, want_status, want_priority) in [
        ("the status is worked", json!({"Status": "InProgress"}), "InProgress", "2"),
        (
            "the priority is raised",
            json!({"Priority": 1}),
            "InProgress",
            "1",
        ),
    ] {
        let mut body = update;
        body["OpsItemId"] = json!(id);
        let (status, response) = call(&router, "UpdateOpsItem", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateOpsItem {body} must succeed, got {status} {response}"
        );

        let parsed = summary(&router, case, &json!({})).await;
        let entry = &entities(&parsed)[0]["Data"][TYPE_OPS_ITEM]["Content"][0];
        assert_eq!(
            entry["Status"].as_str(),
            Some(want_status),
            "case {case:?}: expected Status {want_status:?}, got {entry}"
        );
        assert_eq!(
            entry["Priority"].as_str(),
            Some(want_priority),
            "case {case:?}: expected Priority {want_priority:?}, got {entry}"
        );
    }
}

#[tokio::test]
async fn filters_select_the_ops_data_whose_attributes_satisfy_every_one_of_them() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    // One row per axis the rule generalises over: the attribute's kind (text, number,
    // custom data), each modelled operator, how many values one filter offers, how many
    // filters narrow together, and a filter about ANOTHER OpsData type.
    let table: Vec<(&str, Vec<Value>, Vec<&str>)> = vec![
        (
            "Equal over text",
            vec![filter(TYPE_OPS_ITEM, "Source", &["opscenter-insights"], "Equal")],
            vec!["duplicate work items about one node"],
        ),
        (
            "an operator a filter does not name defaults to Equal",
            vec![json!({
                "Key": key(TYPE_OPS_ITEM, "Category"),
                "Values": ["Performance"],
            })],
            vec!["latency regression in checkout"],
        ),
        (
            "NotEqual over text",
            vec![filter(TYPE_OPS_ITEM, "Source", &["fleet-monitoring"], "NotEqual")],
            vec!["duplicate work items about one node"],
        ),
        (
            "BeginWith over text",
            vec![filter(TYPE_OPS_ITEM, "Title", &["disk"], "BeginWith")],
            vec!["disk is filling up"],
        ),
        (
            "LessThan over a number",
            vec![filter(TYPE_OPS_ITEM, "Priority", &["4"], "LessThan")],
            vec!["disk is filling up"],
        ),
        (
            "GreaterThan over a number",
            vec![filter(TYPE_OPS_ITEM, "Priority", &["4"], "GreaterThan")],
            vec!["duplicate work items about one node"],
        ),
        (
            "Exists asks only whether the attribute is reported",
            // The modelled `OpsFilterValueList` is `len 1..40`, so a filter carries a value
            // even where the operator compares none: this one holds against no OpsItem's
            // custom data, and the OpsItem that reports any is selected regardless.
            vec![filter(
                TYPE_OPS_ITEM,
                "OperationalData",
                &["no OpsItem holds this"],
                "Exists",
            )],
            vec!["disk is filling up"],
        ),
        (
            "a filter's values union",
            vec![filter(
                TYPE_OPS_ITEM,
                "Category",
                &["Availability", "Cost"],
                "Equal",
            )],
            vec![
                "disk is filling up",
                "duplicate work items about one node",
            ],
        ),
        (
            "filters intersect",
            vec![
                filter(TYPE_OPS_ITEM, "Source", &["fleet-monitoring"], "Equal"),
                filter(TYPE_OPS_ITEM, "Severity", &["3"], "Equal"),
            ],
            vec!["latency regression in checkout"],
        ),
        (
            "a filter no OpsItem satisfies selects nothing",
            vec![filter(TYPE_OPS_ITEM, "Status", &["Resolved"], "Equal")],
            vec![],
        ),
        (
            "a filter about another OpsData type is about other OpsData",
            vec![filter(TYPE_EC2, "PlatformType", &["Linux"], "Equal")],
            vec![],
        ),
    ];

    for (case, filters, want) in table {
        let reported =
            reported_titles(&router, case, &json!({ "Filters": filters })).await;
        assert_eq!(
            reported,
            sorted(&want),
            "case {case:?}: expected {want:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_aggregator_counts_the_selected_ops_data_per_value_of_the_attribute_it_names() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    // One row per axis: which attribute is counted (text, number), how the buckets fall,
    // and an aggregator carrying filters of its own.
    let table: Vec<(&str, Value, Vec<AggregatedRow>)> = vec![
        (
            "counted by a value every OpsItem of the Region shares",
            json!({ "Aggregators": [aggregator(TYPE_OPS_ITEM, "Status")] }),
            vec![row(&[("Status", "Open"), ("Count", "3")])],
        ),
        (
            "counted by a value that splits them",
            json!({ "Aggregators": [aggregator(TYPE_OPS_ITEM, "Source")] }),
            vec![
                row(&[("Source", "fleet-monitoring"), ("Count", "2")]),
                row(&[("Source", "opscenter-insights"), ("Count", "1")]),
            ],
        ),
        (
            "counted by a number",
            json!({ "Aggregators": [aggregator(TYPE_OPS_ITEM, "Priority")] }),
            vec![
                row(&[("Priority", "2"), ("Count", "1")]),
                row(&[("Priority", "4"), ("Count", "1")]),
                row(&[("Priority", "5"), ("Count", "1")]),
            ],
        ),
        (
            "an aggregator that names no aggregation counts",
            json!({ "Aggregators": [{ "TypeName": TYPE_OPS_ITEM, "AttributeName": "OpsItemType" }] }),
            vec![
                row(&[("OpsItemType", "/aws/insight"), ("Count", "1")]),
                row(&[("OpsItemType", "/aws/issue"), ("Count", "2")]),
            ],
        ),
        (
            "an aggregator's own filters narrow what it counts",
            json!({ "Aggregators": [{
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Filters": [filter(TYPE_OPS_ITEM, "Category", &["Cost"], "Equal")],
            }] }),
            vec![row(&[("Status", "Open"), ("Count", "1")])],
        ),
        (
            "the request's own filters narrow what is counted",
            json!({
                "Filters": [filter(TYPE_OPS_ITEM, "Source", &["fleet-monitoring"], "Equal")],
                "Aggregators": [aggregator(TYPE_OPS_ITEM, "Status")],
            }),
            vec![row(&[("Status", "Open"), ("Count", "2")])],
        ),
        (
            "a nested aggregator refines each count",
            json!({ "Aggregators": [{
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Source",
                "Aggregators": [aggregator(TYPE_OPS_ITEM, "Severity")],
            }] }),
            vec![
                row(&[
                    ("Source", "fleet-monitoring"),
                    ("Severity", "2"),
                    ("Count", "1"),
                ]),
                row(&[
                    ("Source", "fleet-monitoring"),
                    ("Severity", "3"),
                    ("Count", "1"),
                ]),
                row(&[
                    ("Source", "opscenter-insights"),
                    ("Severity", "4"),
                    ("Count", "1"),
                ]),
            ],
        ),
        (
            "an attribute nothing reports is counted in no bucket",
            json!({ "Aggregators": [aggregator(TYPE_OPS_ITEM, "ActualStartTime")] }),
            vec![],
        ),
    ];

    for (case, body, want) in table {
        if want.is_empty() {
            let parsed = summary(&router, case, &body).await;
            assert!(
                entities(&parsed).is_empty(),
                "case {case:?}: expected no counted entity, got {parsed}"
            );
            continue;
        }
        let reported = aggregated_rows(&router, case, &body, TYPE_OPS_ITEM).await;
        assert_eq!(
            reported, want,
            "case {case:?}: expected {want:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_managed_node_reports_the_instance_information_ops_data_it_registered_with() {
    let (router, engine) = service();
    seed_ops_items(&router).await;
    register_node(&engine, REGION, "i-0123456789abcdef0", "Linux");
    register_node(&engine, REGION, "i-0abc1234", "Windows");
    register_node(&engine, OTHER_REGION, "i-0eeeeeee", "Linux");

    let parsed = summary(
        &router,
        "the node OpsData of this Region",
        &json!({ "ResultAttributes": [{ "TypeName": TYPE_EC2 }] }),
    )
    .await;
    let reported = entities(&parsed);
    assert_eq!(
        reported.len(),
        2,
        "one entity per node of the Region, got {parsed}"
    );
    assert_eq!(
        reported[0]["Id"].as_str(),
        Some("i-0123456789abcdef0"),
        "the items are walked in ascending id order, got {parsed}"
    );
    let item = &reported[0]["Data"][TYPE_EC2];
    assert_eq!(
        item["CaptureTime"].as_str(),
        Some(NODE_LAST_PING_RENDERED),
        "the OpsData was captured when the node last reported, got {parsed}"
    );
    assert_eq!(
        item["Content"][0]["PlatformType"].as_str(),
        Some("Linux"),
        "the entry is the projection of the registered record, got {parsed}"
    );

    // The counts of that same type, and of the OpsItems, are counted over their own type.
    let rows = aggregated_rows(
        &router,
        "the nodes counted by platform",
        &json!({ "Aggregators": [aggregator(TYPE_EC2, "PlatformType")] }),
        TYPE_EC2,
    )
    .await;
    assert_eq!(
        rows,
        vec![
            row(&[("PlatformType", "Linux"), ("Count", "1")]),
            row(&[("PlatformType", "Windows"), ("Count", "1")]),
        ],
        "the nodes of this Region are counted per platform, got {rows:?}"
    );
}

#[tokio::test]
async fn a_published_type_this_service_has_collected_nothing_for_reports_nothing() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    for type_name in [TYPE_COMPLIANCE, TYPE_TRENDLINE] {
        let parsed = summary(
            &router,
            type_name,
            &json!({ "ResultAttributes": [{ "TypeName": type_name }] }),
        )
        .await;
        assert!(
            entities(&parsed).is_empty(),
            "case {type_name:?}: a type nothing has reported for holds no OpsData, got {parsed}"
        );

        // Nor is an aggregator over it refused: there is simply nothing to count.
        let counted = summary(
            &router,
            type_name,
            &json!({ "Aggregators": [aggregator(type_name, "Anything")] }),
        )
        .await;
        assert!(
            entities(&counted).is_empty(),
            "case {type_name:?}: nothing collected is nothing counted, got {counted}"
        );
    }
}

#[tokio::test]
async fn a_summary_reads_the_ops_data_of_the_region_it_was_signed_for() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    for (region, want) in [
        (
            REGION,
            vec![
                "disk is filling up",
                "latency regression in checkout",
                "duplicate work items about one node",
            ],
        ),
        (OTHER_REGION, vec!["disk is filling up elsewhere"]),
    ] {
        let (status, parsed) = call_in_region(&router, region, OPERATION, &json!({})).await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected HTTP 200, got {status} with {parsed}"
        );
        let mut titles: Vec<String> = entities(&parsed)
            .iter()
            .map(|entity| {
                entity["Data"][TYPE_OPS_ITEM]["Content"][0]["Title"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            sorted(&want),
            "case {region:?}: expected {want:?}, got {titles:?}"
        );
    }
}

#[tokio::test]
async fn a_summary_read_through_a_sync_reads_the_regions_that_sync_names() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    let (status, response) = call(
        &router,
        "CreateResourceDataSync",
        &json!({
            "SyncName": "explorer-opsdata",
            "SyncType": FROM_SOURCE,
            "SyncSource": {
                "SourceType": "SingleAccountMultiRegions",
                "SourceRegions": [REGION, OTHER_REGION],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateResourceDataSync must succeed, got {status} {response}"
    );

    let reported = reported_titles(
        &router,
        "every Region the sync names",
        &json!({ "SyncName": "explorer-opsdata" }),
    )
    .await;
    assert_eq!(
        reported,
        sorted(&[
            "disk is filling up",
            "latency regression in checkout",
            "duplicate work items about one node",
            "disk is filling up elsewhere",
        ]),
        "the OpsData of the Regions the sync's source names is reported, got {reported:?}"
    );

    // A name the Region holds no such sync under names no sync to get.
    for sync_name in ["not-a-sync", "EXPLORER-OPSDATA"] {
        let (status, parsed) =
            call(&router, OPERATION, &json!({ "SyncName": sync_name })).await;
        assert_eq!(
            status, 400,
            "case {sync_name:?}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ResourceDataSyncNotFoundException",
            "case {sync_name:?}: expected the modelled refusal, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            format!("ResourceDataSync with name {sync_name} synctype {FROM_SOURCE} does not exists"),
            "case {sync_name:?}: expected the error class's own sentence, got {parsed}"
        );
    }
}

#[tokio::test]
async fn a_page_holds_max_results_entities_and_the_token_resumes_at_the_next_one() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    let whole = summary(&router, "the whole result", &json!({})).await;
    let want: Vec<String> = entities(&whole)
        .iter()
        .map(|entity| entity["Id"].as_str().unwrap_or_default().to_string())
        .collect();

    // One row per page size, including one as large as the whole result and one larger.
    for page_size in [1, 2, 3, 50] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({ "MaxResults": page_size });
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let parsed = summary(&router, &format!("page size {page_size}"), &body).await;
            let page = entities(&parsed);
            assert!(
                page.len() <= page_size as usize,
                "case {page_size}: a page holds at most MaxResults entities, got {parsed}"
            );
            walked.extend(
                page.iter()
                    .map(|entity| entity["Id"].as_str().unwrap_or_default().to_string()),
            );
            token = parsed["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                walked.len() <= want.len(),
                "case {page_size}: the walk must not revisit an item, got {walked:?}"
            );
        }
        assert_eq!(
            walked, want,
            "case {page_size}: the pages walk the whole result once, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_request_naming_what_this_service_cannot_serve_is_refused_with_its_modelled_error() {
    let (router, engine) = service();
    seed_ops_items(&router).await;
    let before = held_ops_items(&engine);

    // One row per way a request can name something the service cannot serve: the aggregator
    // (its aggregation, its type, its attribute, its nesting), the filter key (its form, its
    // type, its attribute), the reported type name and the resume token.
    let table: Vec<(&str, Value, &str, &str)> = vec![
        (
            "an aggregator over an attribute its type does not carry",
            json!({ "Aggregators": [aggregator(TYPE_OPS_ITEM, "NotAnAttribute")] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "an aggregator over a type no OpsData is published under",
            json!({ "Aggregators": [aggregator("AWS:NotAType", "Status")] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "an aggregator over a caller's own type name",
            json!({ "Aggregators": [aggregator("Custom:Mine", "Status")] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "an aggregator naming no attribute to count by",
            json!({ "Aggregators": [{ "AggregatorType": "count", "TypeName": TYPE_OPS_ITEM }] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "an aggregator naming no type to count over",
            json!({ "Aggregators": [{ "AggregatorType": "count", "AttributeName": "Status" }] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "an aggregator asking for an aggregation this service does not compute",
            json!({ "Aggregators": [{
                "AggregatorType": "sum",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Priority",
            }] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "a nested aggregator held to the same rules",
            json!({ "Aggregators": [{
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Aggregators": [aggregator(TYPE_OPS_ITEM, "NotAnAttribute")],
            }] }),
            "InvalidAggregatorException",
            INVALID_AGGREGATOR,
        ),
        (
            "a filter key that is not the dotted form",
            json!({ "Filters": [{ "Key": "Status", "Values": ["Open"] }] }),
            "InvalidFilter",
            INVALID_FILTER,
        ),
        (
            "a filter key over a type no OpsData is published under",
            json!({ "Filters": [filter("AWS:NotAType", "Status", &["Open"], "Equal")] }),
            "InvalidFilter",
            INVALID_FILTER,
        ),
        (
            "a filter key over an attribute its type does not carry",
            json!({ "Filters": [filter(TYPE_OPS_ITEM, "NotAnAttribute", &["Open"], "Equal")] }),
            "InvalidFilter",
            INVALID_FILTER,
        ),
        (
            "an aggregator's own filter key held to the same rule",
            json!({ "Aggregators": [{
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Filters": [{ "Key": "Category", "Values": ["Cost"] }],
            }] }),
            "InvalidFilter",
            INVALID_FILTER,
        ),
        (
            "a reported type name outside the published list",
            json!({ "ResultAttributes": [{ "TypeName": "AWS:NotAType" }] }),
            "InvalidTypeNameException",
            INVALID_TYPE_NAME,
        ),
        (
            "a caller's own type name, which publishes no OpsData here",
            json!({ "ResultAttributes": [{ "TypeName": "Custom:Mine" }] }),
            "InvalidTypeNameException",
            INVALID_TYPE_NAME,
        ),
        (
            "a resume token naming no item of this result",
            json!({ "NextToken": "oi-000000000000" }),
            "InvalidNextToken",
            "The NextToken value is not valid",
        ),
        (
            "a resume token this service never issued",
            json!({ "NextToken": "not-a-token" }),
            "InvalidNextToken",
            "The NextToken value is not valid",
        ),
    ];

    for (case, body, want_code, want_message) in table {
        let (status, parsed) = call(&router, OPERATION, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            want_code,
            "case {case:?}: expected {want_code}, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            want_message,
            "case {case:?}: expected the error class's own sentence, got {parsed}"
        );
    }

    assert_eq!(
        held_ops_items(&engine),
        before,
        "a refused summary persisted nothing"
    );
}

#[tokio::test]
async fn a_value_violating_two_modelled_constraints_states_both_clauses() {
    let (router, _engine) = service();

    // `OpsAggregatorType` is `len 1..20, pattern ^(range|count|sum)$`
    // (`smithy_query(query="shape", shape="OpsAggregatorType")`), so a longer value that is
    // also outside the pattern violates BOTH, and the refusal states one clause per
    // violation under the envelope that counts them. The clause quotes the expression the
    // service PUBLISHES for the shape, which for this one keeps the anchor it opens with and
    // drops the one the model closes it with (`crate::service::refusal`).
    let value = "not-a-real-aggregator-type";
    let (status, parsed) = call(
        &router,
        OPERATION,
        &json!({ "Aggregators": [{
            "AggregatorType": value,
            "TypeName": TYPE_OPS_ITEM,
            "AttributeName": "Status",
        }] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a value outside its modelled constraints is refused, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "ValidationException",
        "a modelled constraint is refused by the boundary, got {parsed}"
    );
    let path = "aggregators.1.member.aggregatorType";
    assert_eq!(
        error_message(&parsed),
        format!(
            "2 validation errors detected: Value '{value}' at '{path}' failed to satisfy \
             constraint: Member must have length less than or equal to 20; Value '{value}' at \
             '{path}' failed to satisfy constraint: Member must satisfy regular expression \
             pattern: ^(range|count|sum)"
        ),
        "both violated constraints are stated, got {parsed}"
    );
}

/// RULE: a `@pattern` clause of this service names the expression the service PUBLISHES for
/// the member's shape, which is reference data about the SHAPE rather than a reading of the
/// anchors in the model's text — so one shape's expression is rendered the same way at every
/// member path that carries it, and another shape's is rendered as that shape is published.
///
/// The rows span the three forms this service publishes, each over a shape the model declares
/// a `@pattern` for: `OpsAggregatorType` with the anchor it opens with and without the one it
/// closes with, `DocumentName` with neither anchor, and `InventoryItemTypeName` exactly as the
/// model declares it. The first expression is asserted at two member paths — a top-level
/// aggregator and one nested inside it — because what is published belongs to the shape.
#[tokio::test]
async fn a_pattern_clause_names_the_expression_the_service_publishes() {
    let (router, _engine) = service();

    // (case, the operation, the request, the clause the message must end with)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "an aggregator type outside its expression",
            OPERATION,
            json!({ "Aggregators": [{
                "AggregatorType": "RANGE",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
            }] }),
            clause(
                "aggregators.1.member.aggregatorType",
                "RANGE",
                "^(range|count|sum)",
            ),
        ),
        (
            "the same shape nested inside another aggregator",
            OPERATION,
            json!({ "Aggregators": [{
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Aggregators": [{
                    "AggregatorType": "Count",
                    "TypeName": TYPE_OPS_ITEM,
                    "AttributeName": "Source",
                }],
            }] }),
            clause(
                "aggregators.1.member.aggregators.1.member.aggregatorType",
                "Count",
                "^(range|count|sum)",
            ),
        ),
        (
            "a document name outside its expression, published without either anchor",
            "GetDocument",
            json!({ "Name": "no" }),
            clause("name", "no", r"[a-zA-Z0-9_\-.:/]{3,128}"),
        ),
        (
            "an inventory type name, published exactly as the model declares it",
            "ListInventoryEntries",
            json!({ "InstanceId": "i-0123456789abcdef0", "TypeName": "Application" }),
            clause("typeName", "Application", "^(AWS|Custom):.*$"),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, parsed) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a value outside its modelled expression is refused by the boundary, \
             got {status} with {parsed}"
        );
        let message = error_message(&parsed);
        assert!(
            message.ends_with(&expected),
            "case {case:?}: expected the clause {expected:?}, got {message:?}"
        );
    }
}

/// One constraint clause as a refusal of this service words it: the value, the member path it
/// filled and the expression it had to satisfy.
fn clause(member_path: &str, value: &str, expression: &str) -> String {
    format!(
        "Value '{value}' at '{member_path}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {expression}"
    )
}

#[tokio::test]
async fn an_aggregator_nested_deeper_than_this_emulator_walks_is_refused() {
    let (router, _engine) = service();
    seed_ops_items(&router).await;

    // The emulator's own bound on the nesting it walks
    // (`crate::ops::get_ops_summary::MAX_AGGREGATOR_NESTING`): the depth at the bound is
    // served, one past it is refused.
    let nested = |levels: usize| {
        let mut built = aggregator(TYPE_OPS_ITEM, "Status");
        for _ in 1..levels {
            built = json!({
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Aggregators": [built],
            });
        }
        json!({ "Aggregators": [built] })
    };

    let (status, parsed) = call(&router, OPERATION, &nested(10)).await;
    assert_eq!(
        status, 200,
        "the deepest nesting this emulator walks is served, got {status} with {parsed}"
    );

    let (status, parsed) = call(&router, OPERATION, &nested(11)).await;
    assert_eq!(
        status, 400,
        "one level past it is refused, got {status} with {parsed}"
    );
    assert_eq!(
        error_code(&parsed),
        "InvalidAggregatorException",
        "expected the modelled refusal, got {parsed}"
    );
}
