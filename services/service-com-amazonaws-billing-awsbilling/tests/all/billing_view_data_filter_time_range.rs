// SPDX-License-Identifier: CC0-1.0

//! The two bounds of a billing view's data filter `timeRange`, over the
//! awsJson1_0 wire.
//!
//! `TimeRange` "specifies a time range with inclusive begin and end dates", so
//! the period it names is the instants from its inclusive begin to its
//! inclusive end: a begin LATER than the end names no instant, and is refused
//! rather than stored on the view and handed back by every later read. The
//! rows below span the relation the rule generalises over — the end a second, a
//! fraction of a second, a month and a year before the begin; the end equal to
//! the begin (the one instant two inclusive bounds still select, which is
//! allowed); the end after the begin by a second, a fraction, a month and a
//! decade; and a range left open at either end or at both — and every row is
//! sent through BOTH writers of the field, the create that mints a view over a
//! filter and the update that replaces one, so neither can disagree with the
//! other. A refused call writes nothing: the create leaves no view behind and
//! the update leaves the stored filter and the instant it was last updated at
//! exactly as they were.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{call, emulator_with_engine, error_code, ACCOUNT_ID, PRIMARY_ARN};
use crate::credit_allocation_history::at;
use crate::rejection_fidelity::message;

/// The relation this service refuses between the two bounds of a range whose
/// ends are both inclusive, stated the way the crate states it for the other
/// such range a request carries (`activeTimeRange`).
const INVERTED_RANGE_SENTENCE: &str =
    "beginDateInclusive must not be later than endDateInclusive";

/// One row of the data-filter range table: the bounds it sends and whether the
/// writers must refuse them.
struct Row {
    case: &'static str,
    /// `beginDateInclusive`, as epoch seconds, or `None` to omit the member.
    begin: Option<f64>,
    /// `endDateInclusive`, as epoch seconds, or `None` to omit the member.
    end: Option<f64>,
    /// The other `Expression` members the row carries beside `timeRange` — the
    /// filter kinds the model lets an expression combine with a period. Source:
    /// `smithy_query(query="shape", shape="Expression")`.
    siblings: Value,
    refused: bool,
}

/// The rows: every relation between an inclusive begin and an inclusive end,
/// each side of the boundary and at several distances from it, plus the ranges
/// that leave a bound out.
fn rows() -> Vec<Row> {
    let march = at(2024, 3, 1, 0) as f64;
    vec![
        Row {
            case: "the end one second before the begin",
            begin: Some(march),
            end: Some(march - 1.0),
            siblings: json!({}),
            refused: true,
        },
        Row {
            case: "the end a fraction of a second before the begin",
            begin: Some(march + 0.5),
            end: Some(march),
            siblings: json!({}),
            refused: true,
        },
        Row {
            case: "the end a calendar month before the begin",
            begin: Some(march),
            end: Some(at(2024, 2, 1, 0) as f64),
            siblings: json!({}),
            refused: true,
        },
        Row {
            case: "the end a year before the begin, with tags beside the range",
            begin: Some(at(2025, 1, 1, 0) as f64),
            end: Some(at(2024, 1, 1, 0) as f64),
            siblings: json!({ "tags": { "key": "team", "values": ["finance"] } }),
            refused: true,
        },
        Row {
            case: "the end a day before the begin, both instants in the future",
            begin: Some(at(2030, 6, 15, 12) as f64),
            end: Some(at(2030, 6, 14, 12) as f64),
            siblings: json!({}),
            refused: true,
        },
        Row {
            case: "the end equal to the begin — the one instant both bounds select",
            begin: Some(march),
            end: Some(march),
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "the end one second after the begin",
            begin: Some(march),
            end: Some(march + 1.0),
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "the end a fraction of a second after the begin",
            begin: Some(march),
            end: Some(march + 0.25),
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "the end a month after the begin, with dimensions beside the range",
            begin: Some(march),
            end: Some(at(2024, 4, 1, 0) as f64),
            siblings: json!({ "dimensions": { "key": "LINKED_ACCOUNT", "values": [ACCOUNT_ID] } }),
            refused: false,
        },
        Row {
            case: "the end a decade after the begin",
            begin: Some(march),
            end: Some(at(2034, 3, 1, 0) as f64),
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "a begin only — a range open at its end",
            begin: Some(march),
            end: None,
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "an end only — a range open at its begin",
            begin: None,
            end: Some(march),
            siblings: json!({}),
            refused: false,
        },
        Row {
            case: "neither bound — a range that is open at both ends",
            begin: None,
            end: None,
            siblings: json!({}),
            refused: false,
        },
    ]
}

/// The `dataFilterExpression` one row sends.
fn expression(row: &Row) -> Value {
    let mut members: Map<String, Value> = row
        .siblings
        .as_object()
        .cloned()
        .expect("a row's sibling members are an object");
    let mut range: Map<String, Value> = Map::new();
    if let Some(begin) = row.begin {
        range.insert("beginDateInclusive".to_string(), json!(begin));
    }
    if let Some(end) = row.end {
        range.insert("endDateInclusive".to_string(), json!(end));
    }
    members.insert("timeRange".to_string(), Value::Object(range));
    Value::Object(members)
}

/// The two bounds a reported `dataFilterExpression` carries, on the scale the
/// request named them on.
fn bounds(expression: &Value) -> (Option<f64>, Option<f64>) {
    let range = &expression["timeRange"];
    (
        range["beginDateInclusive"].as_f64(),
        range["endDateInclusive"].as_f64(),
    )
}

/// Assert one refusal: 400 `ValidationException`, the relation stated in the
/// message, and one `fieldList` entry naming the member that carried the
/// expression.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
    );
    assert_eq!(
        message(body),
        INVERTED_RANGE_SENTENCE,
        "case {case:?}: expected the inverted-range sentence, got {:?}",
        message(body)
    );
    assert_eq!(
        body["fieldList"][0]["name"].as_str(),
        Some("dataFilterExpression"),
        "case {case:?}: expected the fieldList to name dataFilterExpression, got {:?}",
        body["fieldList"]
    );
}

/// The stored view, read back through `GetBillingView`.
async fn read_back(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the read to answer, got {status} {got:?}"
    );
    got["billingView"].clone()
}

/// The `CUSTOM` views the account holds, by name — the control that says
/// whether a refused create left a view behind.
async fn custom_view_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(
        router,
        "ListBillingViews",
        &json!({ "billingViewTypes": ["CUSTOM"] }),
    )
    .await;
    assert_eq!(status, 200, "the control list answers, got {listed:?}");
    listed["billingViews"]
        .as_array()
        .expect("billingViews is a list")
        .iter()
        .map(|view| view["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn the_create_refuses_a_data_filter_range_whose_begin_is_later_than_its_end() {
    let (router, _engine) = emulator_with_engine();

    for (index, row) in rows().iter().enumerate() {
        let name = format!("filter-create-{index}");
        let (status, created) = call(
            &router,
            "CreateBillingView",
            &json!({
                "name": name,
                "sourceViews": [PRIMARY_ARN],
                "dataFilterExpression": expression(row),
            }),
        )
        .await;

        if row.refused {
            assert_refused(row.case, status, &created);
            let held = custom_view_names(&router).await;
            assert!(
                !held.contains(&name),
                "case {:?}: a refused create stores no view, listed {held:?}",
                row.case
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {:?}: expected the create to answer 200, got {status} {created:?}",
            row.case
        );
        let arn = created["arn"].as_str().unwrap_or_default().to_string();
        let view = read_back(&router, row.case, &arn).await;
        assert_eq!(
            bounds(&view["dataFilterExpression"]),
            (row.begin, row.end),
            "case {:?}: expected the stored range to be ({:?}, {:?}), got {:?}",
            row.case,
            row.begin,
            row.end,
            view["dataFilterExpression"]
        );
    }
}

#[tokio::test]
async fn the_update_refuses_a_data_filter_range_whose_begin_is_later_than_its_end() {
    let (router, _engine) = emulator_with_engine();

    // One view, updated row by row: a refused update must leave the filter the
    // view held BEFORE it — and the instant that definition was last updated at
    // — untouched, so each row compares against what the read reported just
    // before its own call.
    let (status, created) = call(
        &router,
        "CreateBillingView",
        &json!({
            "name": "filter-update",
            "sourceViews": [PRIMARY_ARN],
            "dataFilterExpression": {
                "dimensions": { "key": "LINKED_ACCOUNT", "values": [ACCOUNT_ID] },
                "timeRange": { "beginDateInclusive": 1, "endDateInclusive": 2 },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the harness create must answer 200, got {status} {created:?}"
    );
    let arn = created["arn"].as_str().unwrap_or_default().to_string();

    for row in rows() {
        let before = read_back(&router, row.case, &arn).await;
        let (status, updated) = call(
            &router,
            "UpdateBillingView",
            &json!({ "arn": arn, "dataFilterExpression": expression(&row) }),
        )
        .await;

        if row.refused {
            assert_refused(row.case, status, &updated);
            let after = read_back(&router, row.case, &arn).await;
            assert_eq!(
                after["dataFilterExpression"], before["dataFilterExpression"],
                "case {:?}: a refused update leaves the stored filter alone, \
                 expected {:?}, got {:?}",
                row.case, before["dataFilterExpression"], after["dataFilterExpression"]
            );
            assert_eq!(
                after["viewDefinitionLastUpdatedAt"], before["viewDefinitionLastUpdatedAt"],
                "case {:?}: a refused update moves no definition instant, \
                 expected {:?}, got {:?}",
                row.case,
                before["viewDefinitionLastUpdatedAt"],
                after["viewDefinitionLastUpdatedAt"]
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {:?}: expected the update to answer 200, got {status} {updated:?}",
            row.case
        );
        let after = read_back(&router, row.case, &arn).await;
        assert_eq!(
            bounds(&after["dataFilterExpression"]),
            (row.begin, row.end),
            "case {:?}: expected the stored range to be ({:?}, {:?}), got {:?}",
            row.case,
            row.begin,
            row.end,
            after["dataFilterExpression"]
        );
    }
}
