// SPDX-License-Identifier: CC0-1.0

//! An alarm written and read back over the `awsQuery` form wire, and the refusal that wire
//! answers a value the model does not admit with.
//!
//! Two rules, both of the PROTOCOL rather than of any one shape or operation:
//!
//! 1. **A member this service's model types as a floating-point number leaves an `awsQuery`
//!    answer as a decimal carrying a fractional part** — a whole value reads `5.0`, never `5`
//!    — whatever form the request wrote the value in, so a client that publishes a number on
//!    the form wire and reads it back there compares one text against itself. `Threshold`
//!    targets `Threshold`, a `double`, on both the write and every read that reports an alarm
//!    (`smithy_query(query="operation",operation="PutMetricAlarm,DescribeAlarmsForMetric")`).
//! 2. **A refusal answered on the `awsQuery` wire names its class by that class's QUERY code**
//!    in `<ErrorResponse><Error><Type>Sender</Type><Code>…</Code></Error></ErrorResponse>` —
//!    the token a query-era client branches on, `ValidationError` for the request-validation
//!    class — while the same refusal names the class's SHAPE, `ValidationException`, in a JSON
//!    body, and both wires word one sentence.
//!
//! The rows differ along what the rules generalise over: for rule 1 the value's sign,
//! magnitude, whether it is whole, how many fractional digits the request spelled, and which
//! read reports it; for rule 2 which enumerated member of one write carries the value outside
//! its type.

use serde_json::{json, Value};

use crate::harness::{
    build_router, call_with_headers, element, elements, error_code, error_message, merged,
    metric_alarm_request, query_call, QUERY_API_VERSION,
};

/// The media type an `awsQuery` client of this service sends its form body under.
const FORM: &str = "application/x-www-form-urlencoded";

/// The namespace and metric every alarm below watches, named so the metric-scoped read finds
/// exactly the alarm the row wrote.
const NAMESPACE: &str = "Custom/QueryRoundTrip";
const METRIC: &str = "Latency";

/// The text of the first `<tag>` element of an XML answer, with XML's entities resolved: a
/// refusal's sentence quotes the value it refused in single quotes, which the envelope's
/// writer escapes, so the sentence a client reads is the unescaped one.
fn worded(body: &str, tag: &str) -> String {
    quick_xml::escape::unescape(&element(body, tag))
        .unwrap_or_else(|error| panic!("<{tag}> must resolve its entities ({error}): {body}"))
        .into_owned()
}

/// The form fields a `PutMetricAlarm` call carries for an alarm watching [`METRIC`] with a
/// static threshold — the request of the round trip, spelled as a form client spells it.
fn alarm_fields<'a>(name: &'a str, threshold: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("AlarmName", name),
        ("Namespace", NAMESPACE),
        ("MetricName", METRIC),
        ("ComparisonOperator", "GreaterThanThreshold"),
        ("EvaluationPeriods", "1"),
        ("Period", "60"),
        ("Statistic", "Average"),
        ("Threshold", threshold),
    ]
}

#[tokio::test]
async fn a_threshold_written_on_the_query_wire_is_read_back_there_as_a_decimal() {
    // (case, the Threshold text the form body carries, the text every XML read must answer)
    let rows: [(&str, &str, &str); 8] = [
        ("a whole value", "5", "5.0"),
        ("a whole value already spelled as a decimal", "5.0", "5.0"),
        ("zero", "0", "0.0"),
        ("a negative whole value", "-2", "-2.0"),
        ("a fraction below one", "0.5", "0.5"),
        ("a negative fraction", "-0.25", "-0.25"),
        ("several fractional digits", "2048.25", "2048.25"),
        ("a large whole value", "1000000", "1000000.0"),
    ];

    for (case, written, expected) in rows {
        // A fresh emulator per row, so the only alarm either read reports is this row's.
        let router = build_router();
        let name = "query-round-trip-alarm";

        let (status, media_type, body) = query_call(
            &router,
            FORM,
            &[],
            "PutMetricAlarm",
            &alarm_fields(name, written),
        )
        .await;
        assert_eq!(
            (status, media_type.as_str()),
            (200, "text/xml"),
            "case {case:?}: expected the form write to be served in the query envelope, got \
             {status} / {media_type:?} with {body}"
        );

        // Both reads that report a metric alarm, so the rendering is the PROTOCOL's rather
        // than one read's: the name-scoped read and the metric-scoped one.
        let reads: [(&str, Vec<(&str, &str)>); 2] = [
            ("DescribeAlarms", vec![("AlarmNames.member.1", name)]),
            (
                "DescribeAlarmsForMetric",
                vec![("Namespace", NAMESPACE), ("MetricName", METRIC)],
            ),
        ];
        for (action, fields) in reads {
            let (status, media_type, body) =
                query_call(&router, FORM, &[], action, &fields).await;
            assert_eq!(
                (status, media_type.as_str()),
                (200, "text/xml"),
                "case {case:?}: expected {action} to be served in the query envelope, got \
                 {status} / {media_type:?} with {body}"
            );
            let rendered = elements(&body, "Threshold");
            assert_eq!(
                rendered,
                vec![expected.to_string()],
                "case {case:?}: expected {action} to render the threshold as {expected:?}, got \
                 {rendered:?} in {body}"
            );
            // The same answer names the operation it answered and the request it answered,
            // so the decimal above is read out of the envelope the protocol wraps a success
            // in rather than out of some other body.
            assert!(
                body.contains(&format!("<{action}Response")),
                "case {case:?}: expected the {action} answer in its own response envelope, got \
                 {body}"
            );
        }

        // The number the same alarm reports on the JSON wire is a NUMBER: the decimal text is
        // the XML protocol's form, not a change to the value the write stored.
        let (status, _headers, parsed) =
            call_with_headers(&router, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the JSON read to be served, got {status} with {parsed}"
        );
        let on_json = parsed["MetricAlarms"][0]
            .get("Threshold")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            on_json.as_f64(),
            written.parse::<f64>().ok(),
            "case {case:?}: expected the JSON wire to carry the written number, got {on_json}"
        );
        assert!(
            on_json.is_number(),
            "case {case:?}: expected the JSON wire to carry a number, got {on_json}"
        );
    }
}

#[tokio::test]
async fn a_value_outside_an_enumerated_member_is_refused_under_the_querys_own_code() {
    // The three members of this write whose target is an enumerated type
    // (`smithy_query(query="operation",operation="PutMetricAlarm")`: `Statistic` ->
    // `Statistic`, `Unit` -> `StandardUnit`, `ComparisonOperator` -> `ComparisonOperator`),
    // each carrying a value outside it. The member path the sentence quotes is the member in
    // lower camel case.
    // (case, member, the value sent, the path the sentence quotes)
    let rows: [(&str, &str, &str, &str); 4] = [
        (
            "an operator naming no comparison",
            "ComparisonOperator",
            "Exceeds",
            "comparisonOperator",
        ),
        (
            "a declared operator mis-cased",
            "ComparisonOperator",
            "greaterthanthreshold",
            "comparisonOperator",
        ),
        (
            "a statistic outside the modelled type",
            "Statistic",
            "Median",
            "statistic",
        ),
        ("a unit no type declares", "Unit", "Furlongs", "unit"),
    ];

    for (case, member, submitted, path) in rows {
        let router = build_router();
        let name = "query-refusal-alarm";
        let mut fields = alarm_fields(name, "5");
        // The row's own value replaces the one the base request carries for that member — or
        // is added when the base request leaves that member out — so exactly one member of the
        // request carries a value outside its type.
        match fields.iter_mut().find(|field| field.0 == member) {
            Some(field) => field.1 = submitted,
            None => fields.push((member, submitted)),
        }

        let (status, media_type, body) =
            query_call(&router, FORM, &[], "PutMetricAlarm", &fields).await;
        assert_eq!(
            (status, media_type.as_str()),
            (400, "text/xml"),
            "case {case:?}: expected the form write to be refused in the query envelope, got \
             {status} / {media_type:?} with {body}"
        );
        assert_eq!(
            (
                element(&body, "Type").as_str(),
                element(&body, "Code").as_str(),
            ),
            ("Sender", "ValidationError"),
            "case {case:?}: expected a Sender fault named by the query code, got {body}"
        );
        let opening = format!(
            "1 validation error detected: Value '{submitted}' at '{path}' failed to satisfy \
             constraint: "
        );
        let sentence = worded(&body, "Message");
        assert!(
            sentence.starts_with(&opening),
            "case {case:?}: expected a sentence opening {opening:?}, got {sentence:?} in {body}"
        );

        // The same refusal on the JSON wire names the class's SHAPE and words the same
        // sentence: the query code is how the XML envelope states the class, not a different
        // class.
        let json_request = merged(
            metric_alarm_request(name),
            &json!({"Namespace": NAMESPACE, "MetricName": METRIC, member: submitted}),
        );
        let (status, _headers, parsed) =
            call_with_headers(&router, "PutMetricAlarm", &json_request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the JSON write to be refused too, got {status} with \
             {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case:?}: expected the JSON body to name the class's shape, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            sentence,
            "case {case:?}: both wires word one class with one sentence, got {parsed}"
        );

        // Nothing was stored: the refused write left no alarm for the name it carried.
        let (status, _media_type, read) = query_call(
            &router,
            FORM,
            &[],
            "DescribeAlarms",
            &[("AlarmNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read to be served, got {status} with {read}"
        );
        assert!(
            elements(&read, "AlarmName").is_empty(),
            "case {case:?}: a refused write stores nothing, got {read}"
        );
    }
}

#[tokio::test]
async fn an_action_no_operation_of_this_service_names_is_refused_in_the_same_envelope() {
    // The envelope of rule 2 is the protocol's, so it also words a refusal no handler made:
    // a form body naming an action outside this service's API.
    let router = build_router();
    let (status, media_type, body) =
        query_call(&router, FORM, &[], "DescribeNothingAtAll", &[]).await;
    assert_eq!(
        (status, media_type.as_str()),
        (400, "text/xml"),
        "expected an unmodelled action to be refused in the query envelope, got {status} / \
         {media_type:?} with {body}"
    );
    assert_eq!(
        element(&body, "Code").as_str(),
        "InvalidAction",
        "expected the query code for an action this API does not declare, got {body}"
    );
    assert!(
        body.contains(QUERY_API_VERSION) || body.contains("DescribeNothingAtAll"),
        "expected the refusal to name what the request asked for, got {body}"
    );
}
