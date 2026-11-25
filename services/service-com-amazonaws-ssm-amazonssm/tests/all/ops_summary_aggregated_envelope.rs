// SPDX-License-Identifier: CC0-1.0

//! What an AGGREGATED `GetOpsSummary` reports, and how a filter key of the wrong form is
//! refused.
//!
//! The rules under test, each over a TABLE whose rows differ along what the rule generalises
//! over rather than over one recorded request:
//!
//! 1. **An aggregation always reports its envelope.** `GetOpsSummaryResult.Entities` carries
//!    one entity item per OpsData type the request's aggregators name — the counts in its
//!    `Content`, and the EMPTY list when no selected item fell into a bucket — so a client
//!    indexing the counts of the type it asked about reads an empty list rather than a
//!    missing element. A count is the OpsData of no single resource, so that entity names
//!    none.
//! 2. **A filter key that is not the dotted form says so.** An OpsData filter key names an
//!    attribute of an OpsData type (`<TypeName>.<AttributeName>`); a `Key` that is not that
//!    form is refused `InvalidFilter` stating the form a key must take and the offending key
//!    the request carried, wherever the key stands — the request's own filters or an
//!    aggregator's.
//!
//! Every call goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router — and every OpsItem a count is of was opened by a
//! `CreateOpsItem` of this suite, so a response that is not computed from persisted state
//! cannot pass.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The operation under test.
const OPERATION: &str = "GetOpsSummary";

/// The OpsData type whose items are OpsCenter's OpsItems.
const TYPE_OPS_ITEM: &str = "AWS:OpsItem";

/// The OpsData type whose items are the managed nodes of a Region. This suite registers
/// none, so it is a type the account holds no OpsData of.
const TYPE_EC2: &str = "AWS:EC2InstanceInformation";

/// A published OpsData type this emulator has collected nothing for
/// (`OpsResultAttribute$TypeName` publishes it beside the two above).
const TYPE_COMPLIANCE: &str = "AWS:ComplianceSummary";

/// The key an aggregated entry carries its count under.
const COUNT: &str = "Count";

/// One counting aggregator over an attribute of an OpsData type.
fn aggregator(type_name: &str, attribute: &str) -> Value {
    json!({ "AggregatorType": "count", "TypeName": type_name, "AttributeName": attribute })
}

/// How a filter key that is not the dotted form is refused, as this service words that
/// class: the rule, and the key the request carried.
fn invalid_filter_key(key: &str) -> String {
    format!(
        "Invalid filter key {key}. The key format must be TypeName.AttributeName, such as \
         AWS:Application.Name."
    )
}

/// Open one OpsItem in the harness's Region through the front door.
async fn open(router: &MultiServiceRouter, body: &Value) {
    let (status, response) = call(router, "CreateOpsItem", body).await;
    assert_eq!(
        status, 200,
        "the OpsItem {body} must be opened first, got {status} {response}"
    );
}

/// The OpsItems every count in this suite is of: two of one source and one of another, so a
/// count splits them.
async fn seed(router: &MultiServiceRouter) {
    for body in [
        json!({
            "Title": "disk is filling up",
            "Description": "the data volume is at 91%",
            "Source": "fleet-monitoring",
            "Severity": "2",
            "Category": "Availability",
            "Priority": 2,
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
        }),
    ] {
        open(router, &body).await;
    }
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

/// The `Content` of the aggregated entity's item for `type_name`, each entry as an ordered
/// map — asserting the envelope itself is there and names no resource.
fn counted(case: &str, parsed: &Value, type_name: &str) -> Vec<AggregatedRow> {
    let reported = parsed["Entities"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a summary carries Entities, got {parsed}"));
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: the counts are reported as one entity, got {parsed}"
    );
    let entity = &reported[0];
    assert!(
        entity.get("Id").is_none(),
        "case {case:?}: a count belongs to no resource, got {parsed}"
    );
    let item = entity["Data"].get(type_name).unwrap_or_else(|| {
        panic!("case {case:?}: the aggregated type {type_name} carries an item, got {parsed}")
    });
    item["Content"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the item carries a Content list, got {parsed}"))
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
                                panic!("case {case:?}: every entry value is text, got {parsed}")
                            })
                            .to_string(),
                    )
                })
                .collect()
        })
        .collect()
}

/// RULE 1: the entity item of every aggregated type is reported, its `Content` holding what
/// was counted — empty where nothing was. The rows differ along what the account holds for
/// the counted type (items in several buckets, items in one, items reporting nothing for the
/// attribute, no OpsData of that type at all, a type nothing is ever collected for).
#[tokio::test]
async fn an_aggregation_reports_an_entity_item_for_every_type_it_counts_over() {
    let router = router();
    seed(&router).await;

    // (case, the aggregated type, the aggregator, the entries its Content holds)
    let rows: Vec<(&str, &str, Value, Vec<AggregatedRow>)> = vec![
        (
            "an attribute that splits the OpsData into buckets",
            TYPE_OPS_ITEM,
            aggregator(TYPE_OPS_ITEM, "Source"),
            vec![
                row(&[("Source", "fleet-monitoring"), (COUNT, "2")]),
                row(&[("Source", "opscenter-insights"), (COUNT, "1")]),
            ],
        ),
        (
            "an attribute every item shares",
            TYPE_OPS_ITEM,
            aggregator(TYPE_OPS_ITEM, "Status"),
            vec![row(&[("Status", "Open"), (COUNT, "3")])],
        ),
        (
            "an attribute no item of the type reports",
            TYPE_OPS_ITEM,
            aggregator(TYPE_OPS_ITEM, "ActualStartTime"),
            Vec::new(),
        ),
        (
            "a type the account holds no OpsData of",
            TYPE_EC2,
            aggregator(TYPE_EC2, "PlatformType"),
            Vec::new(),
        ),
        (
            "a type nothing reports OpsData for at all",
            TYPE_COMPLIANCE,
            aggregator(TYPE_COMPLIANCE, "Status"),
            Vec::new(),
        ),
        (
            "an aggregator narrowed to nothing by its own filters",
            TYPE_OPS_ITEM,
            json!({
                "AggregatorType": "count",
                "TypeName": TYPE_OPS_ITEM,
                "AttributeName": "Status",
                "Filters": [{
                    "Key": format!("{TYPE_OPS_ITEM}.Source"),
                    "Values": ["nothing-reports-this"],
                    "Type": "Equal",
                }],
            }),
            Vec::new(),
        ),
    ];

    for (case, type_name, asked, want) in rows {
        let (status, parsed) = call(&router, OPERATION, &json!({ "Aggregators": [asked] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the aggregation to be served, got {status} {parsed}"
        );
        let reported = counted(case, &parsed, type_name);
        assert_eq!(
            reported, want,
            "case {case:?}: expected {want:?}, got {reported:?}"
        );
    }

    // Two aggregators over two types report an item each, inside the one entity: each type
    // the request asked about is answered, whichever of them the account holds OpsData of.
    let (status, parsed) = call(
        &router,
        OPERATION,
        &json!({
            "Aggregators": [
                aggregator(TYPE_OPS_ITEM, "Status"),
                aggregator(TYPE_EC2, "PlatformType"),
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"two types at once\": expected the aggregation to be served, got {status} {parsed}"
    );
    assert_eq!(
        counted("two types at once", &parsed, TYPE_OPS_ITEM),
        vec![row(&[("Status", "Open"), (COUNT, "3")])],
        "case \"two types at once\": the counted type reports its buckets, got {parsed}"
    );
    assert_eq!(
        counted("two types at once", &parsed, TYPE_EC2),
        Vec::<AggregatedRow>::new(),
        "case \"two types at once\": the type holding nothing reports the empty list, got \
         {parsed}"
    );

    // A NESTED aggregator counts over the type it names too, so its type is reported as
    // well: here the nesting refines the OpsItem counts and names no second type.
    let nested = json!({
        "AggregatorType": "count",
        "TypeName": TYPE_OPS_ITEM,
        "AttributeName": "Source",
        "Aggregators": [aggregator(TYPE_OPS_ITEM, "Severity")],
    });
    let (status, parsed) = call(&router, OPERATION, &json!({ "Aggregators": [nested] })).await;
    assert_eq!(
        status, 200,
        "case \"a nested aggregator\": expected the aggregation to be served, got {status} \
         {parsed}"
    );
    assert_eq!(
        counted("a nested aggregator", &parsed, TYPE_OPS_ITEM),
        vec![
            row(&[("Source", "fleet-monitoring"), ("Severity", "2"), (COUNT, "1")]),
            row(&[("Source", "fleet-monitoring"), ("Severity", "3"), (COUNT, "1")]),
            row(&[("Source", "opscenter-insights"), ("Severity", "4"), (COUNT, "1")]),
        ],
        "case \"a nested aggregator\": each count is refined by the nested attribute, got \
         {parsed}"
    );
}

/// RULE 2: a filter `Key` outside the dotted `<TypeName>.<AttributeName>` form is refused
/// `InvalidFilter`, stating the form and the key the request carried. The rows differ along
/// the way the key misses the form, and along where the filter stands.
#[tokio::test]
async fn a_filter_key_of_the_wrong_form_is_refused_naming_it() {
    let router = router();
    seed(&router).await;

    // (case, the offending key)
    let keys: [(&str, &str); 6] = [
        ("an attribute name alone", "Status"),
        ("an account attribute a caller might expect", "AccountId"),
        ("a type name with no attribute", TYPE_OPS_ITEM),
        ("a dotted name of no namespace", "OpsItem.Status"),
        ("a namespace with an empty attribute", "AWS:OpsItem."),
        ("a dot and nothing else", "."),
    ];

    for (case, key) in keys {
        // The same key is refused the same way wherever the filter it keys stands: among the
        // request's own filters, and among an aggregator's.
        let bodies: [(&str, Value); 2] = [
            (
                "the request's own filters",
                json!({ "Filters": [{ "Key": key, "Values": ["Open"] }] }),
            ),
            (
                "an aggregator's filters",
                json!({ "Aggregators": [{
                    "AggregatorType": "count",
                    "TypeName": TYPE_OPS_ITEM,
                    "AttributeName": "Status",
                    "Filters": [{ "Key": key, "Values": ["Open"] }],
                }] }),
            ),
        ];
        for (where_it_stands, body) in bodies {
            let (status, parsed) = call(&router, OPERATION, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?} in {where_it_stands:?}: expected HTTP 400, got {status} {parsed}"
            );
            assert_eq!(
                error_code(&parsed),
                "InvalidFilter",
                "case {case:?} in {where_it_stands:?}: expected the modelled filter refusal, \
                 got {parsed}"
            );
            assert_eq!(
                error_message(&parsed),
                invalid_filter_key(key),
                "case {case:?} in {where_it_stands:?}: expected the refusal to state the form \
                 and the key, got {parsed}"
            );
        }
    }

    // A key that IS the form but names a type or an attribute no published OpsData type
    // carries is a different fault of the same class, stated by that class's own sentence:
    // the form rule above is about the form alone.
    for key in [
        format!("{TYPE_OPS_ITEM}.NotAnAttribute"),
        "AWS:NotAType.Status".to_string(),
    ] {
        let (status, parsed) = call(
            &router,
            OPERATION,
            &json!({ "Filters": [{ "Key": key, "Values": ["Open"] }] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "InvalidFilter"),
            "case {key:?}: expected the modelled filter refusal, got {status} {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            "The filter name isn't valid. Verify that you entered the correct name and try \
             again.",
            "case {key:?}: expected the error class's own sentence, got {parsed}"
        );
    }

    // A key that names an attribute of a published type is served: the refusals above are
    // about the key rather than about the filter list.
    let (status, parsed) = call(
        &router,
        OPERATION,
        &json!({ "Filters": [{
            "Key": format!("{TYPE_OPS_ITEM}.Status"),
            "Values": ["Open"],
            "Type": "Equal",
        }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a key naming an attribute of a published type is served, got {status} {parsed}"
    );
    assert_eq!(
        parsed["Entities"].as_array().map(Vec::len),
        Some(3),
        "the three OpsItems this suite opened satisfy that filter, got {parsed}"
    );
}
