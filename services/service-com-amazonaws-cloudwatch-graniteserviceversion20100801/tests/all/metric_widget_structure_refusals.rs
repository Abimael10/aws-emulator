// SPDX-License-Identifier: CC0-1.0

//! How `GetMetricWidgetImage` refuses a widget definition whose `metrics` STRUCTURE is not the
//! shape the widget syntax requires, and what it answers for one that is.
//!
//! The rule under test: "metrics — … Each item in the array is itself an array that defines one
//! metric or expression" (the metric widget structure reference), and inside an entry the
//! namespace, the metric name and the dimension name/value pairs are names, optionally followed
//! by one options object — which is also the only element an entry naming no metric can carry.
//! A node of that structure whose JSON type is not the one required at its position is refused
//! with HTTP 400 in this service's own refusal code, in a sentence naming the node by its
//! pointer from the request member — the members and array positions that lead to it, joined by
//! `/` with each position written as its ZERO-based index — and the type that node must have.
//! A member the structure REQUIRES, and an array holding fewer entries than it requires, are
//! stated in the same vocabulary: the condition the node failed, after the pointer that leads to
//! it, and with no pointer at all when that node is the document's own root.
//!
//! Every table row is a distinct input along what the rule generalises over (which position in
//! the structure the wrong-shaped node sits at, how deep it is, which index it carries, and which
//! type the structure requires there), so one recorded call is one row among many. The definition
//! that IS well formed is rendered, not refused, even when the series it names has no datapoint:
//! the rows below assert the image a client receives is a PNG.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::harness::{build_router, call, error_code, error_message, ok};

/// The first eight bytes of every PNG (the signature in the PNG specification).
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The shape a refusal of this service names in the body of a JSON answer: the
/// request-validation class, whose legacy query code (`ValidationError`) travels in the
/// `x-amzn-query-error` header instead (`query_compatible_error_envelope`).
const REFUSAL_CODE: &str = "ValidationException";

/// One well-formed entry, so a row can place its own malformed node beside entries that are
/// fine and assert the index the refusal names.
fn good_entry() -> Value {
    json!(["AWS/EC2", "CPUUtilization"])
}

#[tokio::test]
async fn a_node_of_the_metrics_structure_of_the_wrong_type_is_named_by_its_pointer() {
    let router = build_router();

    // (case, the widget definition, the sentence the refusal is worded in).
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "the metrics member as an object",
            json!({"metrics": {"m1": ["AWS/EC2", "CPUUtilization"]}}),
            "MetricWidget/metrics should be array",
        ),
        (
            "the metrics member as a string",
            json!({"metrics": "AWS/EC2"}),
            "MetricWidget/metrics should be array",
        ),
        (
            "the metrics member as a number",
            json!({"metrics": 7}),
            "MetricWidget/metrics should be array",
        ),
        (
            "the first entry as a string",
            json!({"metrics": ["AWS/EC2"]}),
            "MetricWidget/metrics/0 should be array",
        ),
        (
            "the second entry as a string",
            json!({"metrics": [good_entry(), "AWS/EC2"]}),
            "MetricWidget/metrics/1 should be array",
        ),
        (
            "the third entry as a number",
            json!({"metrics": [good_entry(), good_entry(), 3]}),
            "MetricWidget/metrics/2 should be array",
        ),
        (
            // The recorded case: a one-element entry names no metric, so its only element had to
            // be the options object an expression is written as.
            "a one-element entry whose element is a string",
            json!({"metrics": [["OnlyOneElement"]], "width": 400, "height": 300}),
            "MetricWidget/metrics/0/0 should be object",
        ),
        (
            "a one-element entry at the second position",
            json!({"metrics": [good_entry(), ["OnlyOneElement"]]}),
            "MetricWidget/metrics/1/0 should be object",
        ),
        (
            "a one-element entry at the third position, after a dimensioned entry",
            json!({"metrics": [
                ["AWS/EC2", "CPUUtilization", "InstanceId", "i-0123456789abcdef0"],
                good_entry(),
                ["OnlyOneElement"],
            ]}),
            "MetricWidget/metrics/2/0 should be object",
        ),
        (
            "a one-element entry whose element is a number",
            json!({"metrics": [[42]]}),
            // A number is no name either, so what the structure requires at that position is
            // reported before the entry is judged for naming no metric.
            "MetricWidget/metrics/0/0 should be string",
        ),
        (
            "an entry whose first name is a number",
            json!({"metrics": [[1, "CPUUtilization"]]}),
            "MetricWidget/metrics/0/0 should be string",
        ),
        (
            "an entry whose second name is a number",
            json!({"metrics": [["AWS/EC2", 5]]}),
            "MetricWidget/metrics/0/1 should be string",
        ),
        (
            "an entry whose dimension value is a boolean",
            json!({"metrics": [["AWS/EC2", "CPUUtilization", "InstanceId", true]]}),
            "MetricWidget/metrics/0/3 should be string",
        ),
        (
            "a name of the wrong type inside the second entry",
            json!({"metrics": [good_entry(), ["AWS/EC2", 5]]}),
            "MetricWidget/metrics/1/1 should be string",
        ),
        (
            "a name of the wrong type before the entry's options object",
            json!({"metrics": [["AWS/EC2", 5, {"stat": "Sum"}]]}),
            "MetricWidget/metrics/0/1 should be string",
        ),
        (
            "an entry whose only element is a nested array",
            json!({"metrics": [[["AWS/EC2", "CPUUtilization"]]]}),
            "MetricWidget/metrics/0/0 should be string",
        ),
    ];

    for (case, widget, expected) in rows {
        let (status, answered) = call(
            &router,
            "GetMetricWidgetImage",
            &json!({"MetricWidget": widget.to_string()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, REFUSAL_CODE),
            "case {case:?}: expected HTTP 400 {REFUSAL_CODE}, got {status} with {answered}"
        );
        let got = error_message(&answered);
        assert_eq!(
            got, expected,
            "case {case:?}: expected the refusal {expected:?}, got {got:?} in {answered}"
        );
    }
}

/// A member the widget structure REQUIRES of the definition, and a `metrics` array holding
/// fewer entries than a graph is of, are refused in the same document vocabulary as a node of
/// the wrong type: the condition the node failed, after the pointer that leads to it — and with
/// no pointer at all when the node that failed is the document's own root.
///
/// The rows differ along how the definition falls short and what else it carries, so neither
/// diagnostic depends on the rest of the document: a document with no members at all, one
/// carrying every other member, one naming the member as null, and the empty array alone and
/// beside other members.
#[tokio::test]
async fn a_definition_missing_what_the_structure_requires_is_refused_by_the_condition_it_failed() {
    let router = build_router();

    // (case, the widget definition, the sentence the refusal is worded in).
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            // The recorded case.
            "a document with no members at all",
            json!({}),
            "Should have required property 'metrics'",
        ),
        (
            "a document carrying every member but metrics",
            json!({"width": 400, "height": 300, "title": "cpu", "view": "timeSeries"}),
            "Should have required property 'metrics'",
        ),
        (
            "a document naming the metrics member as null",
            json!({"metrics": Value::Null}),
            "Should have required property 'metrics'",
        ),
        (
            "an empty metrics array",
            json!({"metrics": []}),
            "MetricWidget/metrics should NOT have fewer than 1 items",
        ),
        (
            "an empty metrics array beside the graph's size",
            json!({"metrics": [], "width": 400, "height": 300}),
            "MetricWidget/metrics should NOT have fewer than 1 items",
        ),
    ];

    for (case, widget, expected) in rows {
        let (status, answered) = call(
            &router,
            "GetMetricWidgetImage",
            &json!({"MetricWidget": widget.to_string()}),
        )
        .await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, REFUSAL_CODE),
            "case {case:?}: expected HTTP 400 {REFUSAL_CODE}, got {status} with {answered}"
        );
        let got = error_message(&answered);
        assert_eq!(
            got, expected,
            "case {case:?}: expected the refusal {expected:?}, got {got:?} in {answered}"
        );
    }
}

#[tokio::test]
async fn a_well_formed_definition_is_rendered_even_when_its_series_has_no_datapoint() {
    let router = build_router();

    // Rows differ along the FORM the well-formed structure takes: the bare metric entry, one
    // carrying dimensions, one carrying an options object, an expression entry — which is the
    // one-element entry whose element IS the options object — and a definition with several
    // entries. None of the series named has ever been published, so what each row proves is that
    // a well-formed definition is answered with an image rather than refused.
    let rows: Vec<(&str, Value)> = vec![
        (
            "one metric entry",
            json!({
                "metrics": [good_entry()],
                "width": 400,
                "height": 300,
                "start": "-PT1H",
                "end": "PT0H",
            }),
        ),
        (
            "a dimensioned metric entry",
            json!({"metrics": [
                ["AWS/EC2", "CPUUtilization", "InstanceId", "i-0123456789abcdef0"],
            ]}),
        ),
        (
            "a metric entry carrying an options object",
            json!({"metrics": [
                ["AWS/EC2", "CPUUtilization", {"stat": "Sum", "label": "cpu"}],
            ]}),
        ),
        (
            "a one-element entry whose element is the options object",
            json!({"metrics": [[{"expression": "1 + 1", "id": "e1"}]]}),
        ),
        (
            "several entries at once",
            json!({"metrics": [
                good_entry(),
                ["AWS/EC2", "NetworkIn", "InstanceId", "i-0123456789abcdef0"],
                [{"expression": "2 * 3", "id": "e2"}],
            ]}),
        ),
    ];

    for (case, widget) in rows {
        let answered = ok(
            &router,
            "GetMetricWidgetImage",
            &json!({"MetricWidget": widget.to_string()}),
        )
        .await;
        let encoded = answered
            .get("MetricWidgetImage")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let image = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: MetricWidgetImage must be base64-encoded, got {error}")
            });
        assert_eq!(
            image.get(..PNG_MAGIC.len()),
            Some(&PNG_MAGIC[..]),
            "case {case:?}: expected an image opening with the PNG signature, got {:?}",
            image.get(..PNG_MAGIC.len())
        );
    }
}
