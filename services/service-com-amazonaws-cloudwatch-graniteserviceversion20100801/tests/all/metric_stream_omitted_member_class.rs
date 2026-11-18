// SPDX-License-Identifier: CC0-1.0

//! How the metric-stream WRITE refuses a required member the request never supplied.
//!
//! The rule under test: **a member the model declares `@required` that the request carries no
//! value for is refused by the request-validation layer in front of the service — HTTP 400,
//! `__type` `com.amazon.coral.validate#ValidationException`, the legacy report
//! `ValidationError;Sender` for a query-compatible client, and the numbered clause naming the
//! member path in lower camel case: `1 validation error detected: Value null at '<member>'
//! failed to satisfy constraint: Member must not be null`** — rather than the
//! `MissingRequiredParameterException` the operation's model also declares, which this service
//! words only where an operation is recorded answering it.
//!
//! The rows are the four members `PutMetricStream` requires
//! (`smithy_query(query="operation",operation="PutMetricStream")`: `Name`, `FirehoseArn`,
//! `RoleArn`, `OutputFormat` — each `@required`), because the clause is built from the MEMBER
//! and nothing else: the output format a recorded request happened to leave out is one row of
//! four, and the stream's destination, its role and its name are the others. A refused write
//! also stores nothing, which is asserted beside each row so the refusal is not a 400 answered
//! after the fact.

use serde_json::{json, Value};

use crate::harness::{build_router, call, call_in_query_mode, error_message};

/// The shape id the body names the request-validation class under: the class belongs to the
/// validation layer in front of the service, which is the package the id names.
const VALIDATION_SHAPE_ID: &str = "com.amazon.coral.validate#ValidationException";

/// The header a query-compatible client reads the legacy code and the fault side from.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The legacy report of the request-validation class.
const VALIDATION_REPORT: &str = "ValidationError;Sender";

/// A `PutMetricStream` request carrying every member the operation requires.
fn complete_request(name: &str) -> Value {
    json!({
        "Name": name,
        "FirehoseArn": "arn:aws:firehose:us-east-1:000000000000:deliverystream/metrics",
        "RoleArn": "arn:aws:iam::000000000000:role/MetricStreamRole",
        "OutputFormat": "json",
    })
}

#[tokio::test]
async fn an_omitted_required_member_is_the_request_validation_class() {
    // (case, the member the row leaves out, the path the clause names it by). The path is the
    // member in lower camel case, which is how the validation layer spells a request member.
    let rows: [(&str, &str, &str); 4] = [
        ("the output format the stream writes", "OutputFormat", "outputFormat"),
        ("the destination the stream delivers to", "FirehoseArn", "firehoseArn"),
        ("the role the stream assumes", "RoleArn", "roleArn"),
        ("the name the stream is created under", "Name", "name"),
    ];

    for (case, member, path) in rows {
        let router = build_router();
        let name = "omitted-member-stream";
        let mut request = complete_request(name);
        request
            .as_object_mut()
            .expect("the request is an object")
            .remove(member);

        let (status, headers, body) = call_in_query_mode(&router, "PutMetricStream", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            body.get("__type").and_then(Value::as_str),
            Some(VALIDATION_SHAPE_ID),
            "case {case:?}: expected the request-validation class, got {body}"
        );
        let reported = headers
            .get(QUERY_ERROR_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(
            reported, VALIDATION_REPORT,
            "case {case:?}: expected the legacy validation report, got {reported:?}"
        );
        let expected = format!(
            "1 validation error detected: Value null at '{path}' failed to satisfy constraint: \
             Member must not be null"
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // A refused write leaves no stream behind, so the refusal is the whole answer.
        let (read_status, read_body) = call(&router, "GetMetricStream", &json!({"Name": name})).await;
        assert_eq!(
            read_status, 404,
            "case {case:?}: a refused write must store nothing, got {read_status} with \
             {read_body}"
        );
    }
}

#[tokio::test]
async fn a_request_carrying_every_required_member_is_served() {
    // The other half of the rule: the clause above is about a member with no value, so a
    // request that supplies all four is answered and the stream it creates reads back.
    let router = build_router();
    let name = "complete-stream";
    let (status, body) = call(&router, "PutMetricStream", &complete_request(name)).await;
    assert_eq!(
        status, 200,
        "case \"every required member supplied\": expected HTTP 200, got {status} with {body}"
    );
    let (read_status, read_body) = call(&router, "GetMetricStream", &json!({"Name": name})).await;
    assert_eq!(
        read_status, 200,
        "case \"every required member supplied\": the created stream must read back, got \
         {read_status} with {read_body}"
    );
    assert_eq!(
        read_body.get("OutputFormat").and_then(Value::as_str),
        Some("json"),
        "case \"every required member supplied\": expected the configured output format, got \
         {read_body}"
    );
}
