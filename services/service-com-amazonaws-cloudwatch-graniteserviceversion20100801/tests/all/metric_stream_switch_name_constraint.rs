// SPDX-License-Identifier: CC0-1.0

//! How the two metric-stream switches refuse a `Names` entry outside the bounds the list's
//! element shape declares.
//!
//! ONE rule, asserted over a table: **a constraint broken by an ENTRY of `Names` is stated at the
//! LIST member — `Value '<the list>' at 'names' failed to satisfy constraint: Member must satisfy
//! constraint: [<the element shape's whole declared set>]` inside the request-validation
//! envelope — rather than at the entry that broke it, and it travels under the
//! request-validation class, whose legacy query code is what a client branches on.** The element
//! shape's set is the model's own (`MetricStreamNames`' member carries `@length` 1..255,
//! `smithy_query(query="operation",operation="StartMetricStreams")`), and the list a clause
//! quotes is the one the request carried.
//!
//! The rows differ along what the rule generalises over: which of the two switches was called,
//! which bound the entry broke (over the maximum, and the minimum an empty entry breaks), how
//! far past it the entry is, and how many entries the list carries — a list whose entries are
//! all admissible is the control, which both switches serve.

use serde_json::{json, Value};

use crate::harness::{build_router, call, call_in_query_mode, error_code, error_message, ok};

/// The two operations the rule holds for: "Starts the streaming of metrics for one or more of
/// your metric streams" and its mirror, both taking `Names`.
const SWITCHES: [&str; 2] = ["StartMetricStreams", "StopMetricStreams"];

/// The element shape's declared bounds, as the model writes them (`MetricStreamName`:
/// `@length` min 1 max 255).
const MAX_NAME_LEN: usize = 255;

/// The requirement a clause states at the list member: the element shape's WHOLE declared set,
/// nested as AWS nests one, in the order a clause states it (the maximum, then the minimum).
const ELEMENT_SET: &str = "Member must satisfy constraint: [Member must have length less than or \
                           equal to 255, Member must have length greater than or equal to 1]";

/// The whole message such a refusal carries, for the list `names` the request sent.
fn refusal_message(names: &[String]) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at 'names' failed to satisfy constraint: \
         {ELEMENT_SET}",
        names.join(", ")
    )
}

/// A name of `length` characters, inside the character set a stream name admits.
fn name_of(length: usize) -> String {
    "x".repeat(length)
}

#[tokio::test]
async fn a_names_entry_outside_the_element_bounds_is_refused_at_the_list_member() {
    // (the case, the Names list the request carries)
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("one entry one character past the maximum", vec![name_of(MAX_NAME_LEN + 1)]),
        ("one entry far past the maximum", vec![name_of(300)]),
        ("one entry at the empty string the minimum excludes", vec![String::new()]),
        (
            "an admissible entry beside one past the maximum",
            vec!["metrics".to_string(), name_of(MAX_NAME_LEN + 1)],
        ),
        (
            "two entries past the maximum",
            vec![name_of(MAX_NAME_LEN + 2), name_of(MAX_NAME_LEN + 9)],
        ),
    ];

    for operation in SWITCHES {
        for (case, names) in &rows {
            let case = format!("{operation}: {case}");
            let router = build_router();
            let (status, answered) =
                call(&router, operation, &json!({"Names": names})).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {answered}"
            );
            let message = error_message(&answered);
            assert_eq!(
                message,
                refusal_message(names),
                "case {case:?}: expected the clause stated at the list member, got {message:?}"
            );
            assert_eq!(
                error_code(&answered),
                "ValidationException",
                "case {case:?}: expected the request-validation class, got {answered}"
            );
        }
    }
}

/// The class the refusal travels under, as a query-compatible client reads it: the legacy query
/// code of the request-validation class.
#[tokio::test]
async fn the_names_refusal_reports_the_request_validation_query_code() {
    for operation in SWITCHES {
        let router = build_router();
        let names = vec![name_of(300)];
        let (status, headers, answered) =
            call_in_query_mode(&router, operation, &json!({"Names": names})).await;
        let case = format!("{operation}: a name past the maximum, asked for in query mode");
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered}"
        );
        let reported = headers
            .get("x-amzn-query-error")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reported, "ValidationError;Sender",
            "case {case:?}: expected the request-validation class's legacy code, got {reported:?}"
        );
    }
}

/// The control: a list whose entries the element shape admits is SERVED, whether or not a stream
/// of that name exists — "A syntactically valid but nonexistent stream name is a permissive
/// no-op": the switches model no not-found fault at all.
#[tokio::test]
async fn a_list_of_admissible_names_is_served_by_both_switches() {
    // (the case, the Names list)
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("one name of a stream that exists", vec!["metrics".to_string()]),
        ("one name no stream carries", vec!["no-such-stream".to_string()]),
        ("a name exactly at the maximum", vec![name_of(MAX_NAME_LEN)]),
        ("an empty list, which names no stream", Vec::new()),
    ];

    for operation in SWITCHES {
        for (case, names) in &rows {
            let case = format!("{operation}: {case}");
            let router = build_router();
            ok(
                &router,
                "PutMetricStream",
                &json!({
                    "Name": "metrics",
                    "FirehoseArn": "arn:aws:firehose:us-east-1:000000000000:deliverystream/m",
                    "RoleArn": "arn:aws:iam::000000000000:role/MetricStreamRole",
                    "OutputFormat": "json",
                }),
            )
            .await;
            let (status, answered) =
                call(&router, operation, &json!({"Names": names})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} with {answered}"
            );
            assert_eq!(
                answered,
                Value::Object(serde_json::Map::new()),
                "case {case:?}: expected an empty answer, got {answered}"
            );
        }
    }
}
