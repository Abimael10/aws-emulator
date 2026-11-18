// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH filter patterns a metric filter may declare `dimensions`
//! with, and for what the log group reports afterwards.
//!
//! The rule under test: a metric transformation may declare `dimensions` only when the
//! `filterPattern` it is put with NAMES fields of the log event — one of the filter-pattern
//! syntax's structured forms, a `{ … }` JSON pattern or a `[ … ]` space-delimited list that
//! names at least one field — because a dimension value is a selector of such a field
//! (`MetricTransformation$dimensions`, "The fields to use as dimensions for the metric").
//! A pattern that names no field of the event — an unstructured one, which only states the
//! terms a message carries, the empty one, or a field list of nothing but the `...` wildcard
//! — supports no dimension, so the put is refused with `InvalidParameterException` and the
//! service's fixed sentence, and NOTHING is stored: the group reports no such filter and its
//! `metricFilterCount` ("The number of metric filters") counts only the filters that exist.
//!
//! The rows span what the rule generalises over: which form the pattern takes (JSON,
//! space-delimited with and without the wildcard, a bare term, several terms, an excluded
//! term, alternatives, a quoted string, a regular expression, the empty pattern, a field list
//! that names nothing), how many dimensions the transformation declares and in which selector
//! form, and whether the put would CREATE the filter or UPDATE one the group already holds.
//! Each refused pattern is also put WITHOUT dimensions and accepted, so what the rule refuses
//! is the combination and never the pattern on its own.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a refusal
//! or a count a handler does not actually compute cannot pass.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The code CloudWatch Logs answers when a parameter is specified incorrectly. Source:
/// `PutMetricFilter`'s `InvalidParameterException`, errorKind client, httpError 400.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The whole message a put whose filter pattern supports no dimensions is refused with:
/// fixed text for the error class, interpolating nothing of the request.
const PATTERN_WITHOUT_DIMENSIONS: &str = "The specified filter pattern does not support dimensions";

/// The patterns that NAME fields of the log event, so a dimension has something to select.
/// Source: the dimensions documentation `MetricTransformation$dimensions` points at —
/// dimensions are published "with metrics from values in JSON or space-delimited log events".
const PATTERNS_WITH_DIMENSION_SUPPORT: &[(&str, &str)] = &[
    ("a JSON condition on one field", "{ $.status = \"fail\" }"),
    (
        "a JSON condition on a nested path",
        "{ $.detail.tenant.id = \"acme\" }",
    ),
    (
        "a JSON condition over several fields",
        "{ ($.level = \"ERROR\") && ($.code = 500) }",
    ),
    (
        "a space-delimited list of named fields",
        "[host, request, status]",
    ),
    (
        "a space-delimited list naming fields around the wildcard",
        "[ip, user, ..., status]",
    ),
    (
        "a space-delimited list with a condition on a named field",
        "[host, status = 500]",
    ),
];

/// The patterns that name NO field of the log event, so they support no dimension at all.
const PATTERNS_WITHOUT_DIMENSION_SUPPORT: &[(&str, &str)] = &[
    ("a single bare term", "ERROR"),
    ("two bare terms", "ERROR timeout"),
    ("a term that must not appear", "ERROR -INFO"),
    ("alternatives", "?ERROR ?WARN"),
    ("a quoted string term", "\"ERROR 500\""),
    ("a regular-expression term", "%ERROR[0-9]+%"),
    ("the empty pattern, which matches everything", ""),
    ("a field list of nothing but the wildcard", "[...]"),
];

/// The dimension maps a row declares, in the selector forms a dimension value takes. Source:
/// the dimension value is a selector — `$.path` in a JSON event, `$field` or `$position` in a
/// space-delimited one.
const DIMENSION_SETS: &[(&str, &[(&str, &str)])] = &[
    ("one JSON selector", &[("Service", "$.service")]),
    (
        "two selectors, one of them a nested path",
        &[("Tenant", "$.tenant.id"), ("Shard", "$shard")],
    ),
    (
        "the documented maximum of three selectors",
        &[("A", "$.a"), ("B", "$host"), ("C", "$2")],
    ),
];

/// A `dimensions` map from `(name, value)` pairs.
fn dimensions_of(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), json!(value)))
            .collect(),
    )
}

/// A `PutMetricFilter` body, with `dimensions` only when the row declares any.
fn put_body(group: &str, filter: &str, pattern: &str, dimensions: &[(&str, &str)]) -> Value {
    let mut transformation = json!({
        "metricName": "Observed",
        "metricNamespace": "Custom/PatternDimensions",
        "metricValue": "1",
    });
    if !dimensions.is_empty() {
        transformation["dimensions"] = dimensions_of(dimensions);
    }
    json!({
        "logGroupName": group,
        "filterName": filter,
        "filterPattern": pattern,
        "metricTransformations": [transformation],
    })
}

/// The filters `DescribeMetricFilters` reports for one log group, by name.
async fn filter_names(logs: &Logs, group: &str) -> Vec<String> {
    let page = logs
        .ok("DescribeMetricFilters", &json!({"logGroupName": group}))
        .await;
    page.get("metricFilters")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("DescribeMetricFilters answers metricFilters, got {page}"))
        .iter()
        .filter_map(|filter| {
            filter
                .get("filterName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The `metricFilterCount` `DescribeLogGroups` reports for one log group.
async fn metric_filter_count(logs: &Logs, group: &str) -> i64 {
    let described = logs
        .describe_group(group)
        .await
        .unwrap_or_else(|| panic!("case {group}: the log group must exist"));
    described
        .get("metricFilterCount")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {group}: a log group reports metricFilterCount, got {described}"))
}

/// Create one log group.
async fn create_group(logs: &Logs, group: &str) {
    logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
        .await;
}

/// A put whose filter pattern names fields of the log event stores the dimensions it
/// declares, in every selector form and at every count up to the documented maximum.
#[tokio::test]
async fn a_pattern_that_names_event_fields_accepts_dimensions() {
    let logs = Logs::new();
    const GROUP: &str = "/mf/pattern-dimensions-supported";
    create_group(&logs, GROUP).await;

    let mut stored = 0i64;
    for (pattern_case, pattern) in PATTERNS_WITH_DIMENSION_SUPPORT {
        for (dimension_case, dimensions) in DIMENSION_SETS {
            let case = format!("{pattern_case} with {dimension_case}");
            let filter = format!("{pattern_case}/{dimension_case}").replace([' ', ','], "-");
            let body = put_body(GROUP, &filter, pattern, dimensions);
            let (status, answered) = logs.call("PutMetricFilter", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the put to be accepted, got {status} with {answered}"
            );

            // The stored filter reports the dimensions it was put with, so the accepted put
            // really configured them.
            let page = logs
                .ok(
                    "DescribeMetricFilters",
                    &json!({"logGroupName": GROUP, "filterNamePrefix": filter}),
                )
                .await;
            let reported = page["metricFilters"][0]["metricTransformations"][0]["dimensions"]
                .clone();
            assert_eq!(
                reported,
                dimensions_of(dimensions),
                "case {case:?}: expected the dimensions as they were put, got {page}"
            );

            stored += 1;
            assert_eq!(
                metric_filter_count(&logs, GROUP).await,
                stored,
                "case {case:?}: the group counts every filter it holds"
            );
        }
    }
}

/// A put whose filter pattern names no field of the log event is refused with the service's
/// fixed sentence, and the SAME pattern without dimensions is accepted — so the refusal is
/// the combination of pattern and dimensions.
#[tokio::test]
async fn a_pattern_that_names_no_event_field_refuses_dimensions() {
    let logs = Logs::new();
    const GROUP: &str = "/mf/pattern-dimensions-unsupported";
    create_group(&logs, GROUP).await;

    let mut accepted = 0i64;
    for (pattern_case, pattern) in PATTERNS_WITHOUT_DIMENSION_SUPPORT {
        for (dimension_case, dimensions) in DIMENSION_SETS {
            let case = format!("{pattern_case} with {dimension_case}");
            let filter = format!("{pattern_case}/{dimension_case}").replace([' ', ','], "-");
            let (code, message) = logs
                .refusal(
                    "PutMetricFilter",
                    &put_body(GROUP, &filter, pattern, dimensions),
                )
                .await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?}: expected {INVALID_PARAMETER}, got {code} {message:?}"
            );
            assert_eq!(
                message, PATTERN_WITHOUT_DIMENSIONS,
                "case {case:?}: expected {PATTERN_WITHOUT_DIMENSIONS:?}, got {message:?}"
            );
            // Fixed text for the class: nothing the request stated is interpolated.
            let mut carries_nothing_of = vec![filter.clone(), (*pattern).to_string()];
            for (name, value) in *dimensions {
                carries_nothing_of.push((*name).to_string());
                carries_nothing_of.push((*value).to_string());
            }
            for carried in carries_nothing_of {
                if carried.is_empty() {
                    continue;
                }
                assert!(
                    !message.contains(carried.as_str()),
                    "case {case:?}: the fixed text carries nothing of the request, got \
                     {carried:?} in {message:?}"
                );
            }
            assert!(
                !filter_names(&logs, GROUP).await.contains(&filter),
                "case {case:?}: a refused put stores nothing"
            );
        }

        // The boundary: the same pattern with NO dimensions is accepted and counted.
        let filter = format!("{pattern_case}/no-dimensions").replace([' ', ','], "-");
        let (status, answered) = logs
            .call("PutMetricFilter", &put_body(GROUP, &filter, pattern, &[]))
            .await;
        assert_eq!(
            status, 200,
            "case {pattern_case:?}: the same pattern without dimensions is accepted, got \
             {status} with {answered}"
        );
        accepted += 1;
        assert_eq!(
            metric_filter_count(&logs, GROUP).await,
            accepted,
            "case {pattern_case:?}: the group counts only the filters it holds"
        );
    }
}

/// What the group reports after a refused put: `metricFilterCount` counts the filters that
/// EXIST, so a refused put neither adds to the count nor changes the filter a put under the
/// same name had already stored.
#[tokio::test]
async fn a_refused_put_leaves_the_groups_filters_and_count_alone() {
    let logs = Logs::new();
    const GROUP: &str = "/mf/pattern-dimensions-count";
    const KEPT: &str = "dimensioned-json";
    create_group(&logs, GROUP).await;
    assert_eq!(
        metric_filter_count(&logs, GROUP).await,
        0,
        "a new log group holds no metric filter"
    );

    // One filter AWS creates: its pattern names the fields its dimensions select.
    let valid = put_body(
        GROUP,
        KEPT,
        "{ $.eventType = \"UpdateTrail\" }",
        &[("EventType", "$.eventType")],
    );
    logs.ok("PutMetricFilter", &valid).await;
    assert_eq!(
        metric_filter_count(&logs, GROUP).await,
        1,
        "the group counts the one filter it holds"
    );

    // One AWS refuses: dimensions under a pattern that names no field of the event.
    let (code, message) = logs
        .refusal(
            "PutMetricFilter",
            &put_body(GROUP, "dimensioned-terms", "ERROR", &[("Service", "$.service")]),
        )
        .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "the refused put answers {INVALID_PARAMETER}, got {code} {message:?}"
    );
    assert_eq!(
        metric_filter_count(&logs, GROUP).await,
        1,
        "a refused put is not counted, got the count after it"
    );
    assert_eq!(
        filter_names(&logs, GROUP).await,
        vec![KEPT.to_string()],
        "a refused put adds no filter"
    );

    // An UPDATE of the stored filter that trips the rule leaves the stored filter as it was.
    let stored = logs
        .ok("DescribeMetricFilters", &json!({"logGroupName": GROUP}))
        .await;
    let (code, message) = logs
        .refusal(
            "PutMetricFilter",
            &put_body(GROUP, KEPT, "ERROR", &[("EventType", "$.eventType")]),
        )
        .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "the refused update answers {INVALID_PARAMETER}, got {code} {message:?}"
    );
    assert_eq!(
        message, PATTERN_WITHOUT_DIMENSIONS,
        "the refused update states the rule, got {message:?}"
    );
    assert_eq!(
        logs.ok("DescribeMetricFilters", &json!({"logGroupName": GROUP}))
            .await,
        stored,
        "a refused update leaves the stored filter unchanged"
    );
    assert_eq!(
        metric_filter_count(&logs, GROUP).await,
        1,
        "a refused update does not change the count"
    );
}
