// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a calculated-attribute definition response REPORTS, over the
//! two operations that project a stored definition (`CreateCalculatedAttributeDefinition`
//! and `GetCalculatedAttributeDefinition`).
//!
//! The rule under test: every projection of a stored calculated attribute definition
//! reports the members the service keeps on the record — the display name (the one the
//! create supplied, or the definition's own name when it supplied none), the two
//! configuration structures `Conditions` and `Filter` PRESENT for as long as the
//! definition exists (each member the definition left unset carried as JSON null), and
//! the tag map PRESENT (empty for a definition that carries no tags) — so a caller can
//! tell "nothing was configured" from "nothing was reported", and the create's answer and
//! the read's answer are the same definition. `Readiness$Message` for a definition whose
//! historical data is all indexed is the service's fixed ready sentence.
//!
//! Each case asserts that over a TABLE spanning what the rule generalises over: how much
//! configuration the request carries (none, conditions only, a filter only, tags only,
//! every member), which members of a partly configured structure are set, and whether the
//! definition had historical data to index.
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing is this crate's one test harness, in
//! `tests/all/domain_profile_lifecycle.rs`, and the request helpers are the ones
//! `tests/all/calculated_attribute_definitions.rs` already defines.

use serde_json::{json, Map, Value};

use crate::calculated_attribute_definitions::{
    attribute_details, create, create_domain_with_object_type, definition, get,
};
use crate::domain_profile_lifecycle::Emulator;

/// `Readiness$Message` for a definition whose historical data is all indexed: the
/// service's fixed sentence for the ready state, which interpolates nothing.
const READY_MESSAGE: &str = "Calculated Attribute is ready to use.";

/// Every member the `Conditions` structure declares, in the order the model lists them.
/// Source: `smithy_query(query="shape", shape="Conditions")` — `Range`, `ObjectCount`,
/// `Threshold`.
const CONDITIONS_MEMBERS: [&str; 3] = ["Range", "ObjectCount", "Threshold"];

/// Every member the `Filter` structure declares. Source:
/// `smithy_query(query="shape", shape="Filter")` — `Include` and `Groups`, both
/// `@required` of a filter that IS configured.
const FILTER_MEMBERS: [&str; 2] = ["Include", "Groups"];

/// A filter over one attribute dimension: the smallest `Filter` the model accepts, with
/// every member of every structure under it set (each is `@required`).
fn filter(include: &str, dimension_type: &str) -> Value {
    json!({
        "Include": include,
        "Groups": [{
            "Type": "ANY",
            "Dimensions": [{
                "Attributes": {
                    "Amount": { "DimensionType": dimension_type, "Values": ["10"] },
                },
            }],
        }],
    })
}

/// `Conditions` as a projection of a definition created by `request` must report it: the
/// conditions the request configured, with every condition member it left unset present
/// and null.
fn expected_conditions(request: &Value) -> Value {
    let configured = request.get("Conditions");
    let mut reported = Map::new();
    for name in CONDITIONS_MEMBERS {
        reported.insert(
            name.to_string(),
            configured
                .and_then(|conditions| conditions.get(name))
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    Value::Object(reported)
}

/// `Filter` as a projection of a definition created by `request` must report it: the
/// filter the request configured, or the structure with every member null for a definition
/// that configured none.
fn expected_filter(request: &Value) -> Value {
    match request.get("Filter") {
        Some(configured) => configured.clone(),
        None => {
            let mut reported = Map::new();
            for name in FILTER_MEMBERS {
                reported.insert(name.to_string(), Value::Null);
            }
            Value::Object(reported)
        }
    }
}

/// `DisplayName` as a projection of a definition created by `request` under `name` must
/// report it: the display name the request supplied, or the definition's own name for a
/// request that supplied none — every definition carries a label.
fn expected_display_name(request: &Value, name: &str) -> Value {
    request
        .get("DisplayName")
        .cloned()
        .unwrap_or_else(|| json!(name))
}

/// `Tags` as a projection of a definition created by `request` must report it: the tags it
/// supplied, or the empty map.
fn expected_tags(request: &Value) -> Value {
    request.get("Tags").cloned().unwrap_or_else(|| json!({}))
}

/// One row of the table: a create request, and the case it stands for.
struct Row {
    case: &'static str,
    name: &'static str,
    request: Value,
}

/// The definitions the table creates, spanning how much configuration a request carries
/// and which members of a partly configured structure are set.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the required members only, so nothing is configured",
            name: "Reported_Minimal",
            request: definition("COUNT"),
        },
        Row {
            case: "a display name and nothing else",
            name: "Reported_Display_Name",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "SUM",
                "DisplayName": "Order total",
            }),
        },
        Row {
            case: "a display name carrying the spaces and hyphens the pattern allows",
            name: "Reported_Display_Name_Spaced",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "AVERAGE",
                "DisplayName": "Order total - last 30 days",
            }),
        },
        Row {
            case: "one condition member set, the other two left unset",
            name: "Reported_Object_Count_Only",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "COUNT",
                "Conditions": { "ObjectCount": 10 },
            }),
        },
        Row {
            case: "a different single condition member set",
            name: "Reported_Range_Only",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "SUM",
                "Conditions": { "Range": { "Value": 30, "Unit": "DAYS" } },
            }),
        },
        Row {
            case: "two condition members set, one left unset",
            name: "Reported_Two_Conditions",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "MAXIMUM",
                "Conditions": {
                    "ObjectCount": 5,
                    "Threshold": { "Value": "100", "Operator": "GREATER_THAN" },
                },
            }),
        },
        Row {
            case: "a filter but no conditions",
            name: "Reported_Filter_Only",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "MINIMUM",
                "Filter": filter("ALL", "GREATER_THAN"),
            }),
        },
        Row {
            case: "a filter under the other Include value and another dimension type",
            name: "Reported_Filter_Any",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount", "Discount"]),
                "Statistic": "SUM",
                "Filter": filter("ANY", "EQUAL"),
            }),
        },
        Row {
            case: "tags but no conditions and no filter",
            name: "Reported_Tags_Only",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "LAST_OCCURRENCE",
                "Tags": { "team": "crm", "env": "dev" },
            }),
        },
        Row {
            case: "every optional member a request may carry",
            name: "Reported_Fully_Configured",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "SUM",
                "DisplayName": "Order total",
                "Description": "the total of every order",
                "Conditions": {
                    "Range": { "Value": 30, "Unit": "DAYS" },
                    "ObjectCount": 10,
                    "Threshold": { "Value": "100", "Operator": "GREATER_THAN" },
                },
                "Filter": filter("ALL", "GREATER_THAN"),
                "UseHistoricalData": false,
                "Tags": { "team": "crm" },
            }),
        },
        Row {
            case: "a definition with historical data to index",
            name: "Reported_Historical",
            request: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "SUM",
                "UseHistoricalData": true,
                "DisplayName": "Order total",
            }),
        },
    ]
}

/// RULE: the create's answer and the read's answer both report the definition's display
/// name — the one supplied, or the definition's own name — `Conditions` and `Filter`
/// present with every unset member null, and the tag map present (empty when the request
/// carried no tags).
#[tokio::test]
async fn both_projections_report_every_member_the_definition_keeps() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, "reportdom", 30).await;

    for row in rows() {
        let Row {
            case,
            name,
            request,
        } = row;

        let (status, created) = create(&emulator, "reportdom", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {request:?}, got {status} {created:?}"
        );
        let (status, read) = get(&emulator, "reportdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetCalculatedAttributeDefinition 200, got {status} {read:?}"
        );

        for (reader, body) in [("the create", &created), ("the read", &read)] {
            assert_eq!(
                body.get("DisplayName"),
                Some(&expected_display_name(&request, name)),
                "case {case:?}: expected {reader} to report DisplayName {:?}, got {:?}",
                expected_display_name(&request, name),
                body.get("DisplayName")
            );
            assert_eq!(
                body.get("Conditions"),
                Some(&expected_conditions(&request)),
                "case {case:?}: expected {reader} to report Conditions {:?}, got {:?}",
                expected_conditions(&request),
                body.get("Conditions")
            );
            assert_eq!(
                body.get("Filter"),
                Some(&expected_filter(&request)),
                "case {case:?}: expected {reader} to report Filter {:?}, got {:?}",
                expected_filter(&request),
                body.get("Filter")
            );
            assert_eq!(
                body.get("Tags"),
                Some(&expected_tags(&request)),
                "case {case:?}: expected {reader} to report Tags {:?}, got {:?}",
                expected_tags(&request),
                body.get("Tags")
            );
        }

        // One definition, so the two projections agree member for member on everything
        // but what the clock moves.
        for member in ["DisplayName", "Conditions", "Filter", "Tags"] {
            assert_eq!(
                created.get(member),
                read.get(member),
                "case {case:?}: expected the create and the read to agree on {member}, \
                 got {:?} then {:?}",
                created.get(member),
                read.get(member)
            );
        }
    }
}

/// RULE, the readiness sentence: a definition whose historical data is all indexed reports
/// the service's fixed ready sentence, and one still indexing reports a different sentence
/// (never the ready one) alongside the transient status.
#[tokio::test]
async fn the_readiness_message_is_the_services_fixed_ready_sentence() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, "readymsgdom", 30).await;

    for row in rows() {
        let Row {
            case,
            name,
            request,
        } = row;
        let indexing = request.get("UseHistoricalData") == Some(&json!(true));

        let (status, created) = create(&emulator, "readymsgdom", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {request:?}, got {status} {created:?}"
        );
        let (status, read) = get(&emulator, "readymsgdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetCalculatedAttributeDefinition 200, got {status} {read:?}"
        );

        for (reader, body) in [("the create", &created), ("the read", &read)] {
            let message = body
                .get("Readiness")
                .and_then(|readiness| readiness.get("Message"))
                .and_then(Value::as_str);
            if indexing {
                // The definition has historical data still to index, so it is not ready
                // and must not claim the ready sentence.
                assert!(
                    message.is_some_and(|message| message != READY_MESSAGE),
                    "case {case:?}: expected {reader} to report a readiness message other \
                     than the ready sentence while the indexing is owed, got {message:?}"
                );
            } else {
                assert_eq!(
                    message,
                    Some(READY_MESSAGE),
                    "case {case:?}: expected {reader} to report the fixed ready sentence, \
                     got {message:?}"
                );
            }
        }
    }
}
