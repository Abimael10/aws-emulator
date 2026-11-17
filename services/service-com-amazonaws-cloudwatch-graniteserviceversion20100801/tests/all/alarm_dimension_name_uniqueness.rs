// SPDX-License-Identifier: CC0-1.0

//! A dimension NAME may appear at most once in the dimension set an alarm is configured with.
//!
//! A dimension "is a name/value pair that is part of the identity of a metric. Because
//! dimensions are part of the unique identifier for a metric, whenever you add a unique
//! name/value pair to one of your metrics, you are creating a new variation of that metric."
//! (`smithy_query(query="doc",shape="Dimension")`) — so a dimension name is one coordinate of a
//! metric's identity and carries ONE value. A set that names the same dimension twice states two
//! values for one coordinate and therefore identifies no single metric, so `PutMetricAlarm`
//! refuses it, naming the repeated dimension, and stores nothing: an accepted request would bind
//! the alarm to one of the two values invisibly.
//!
//! The rule is about the SET, not about any particular dimension, so the table's rows differ
//! along where in the set the repeat falls, how many times a name repeats, how many dimensions
//! the set carries, whether the repeated entries share a value — and along which of the two
//! forms states the series the alarm watches, since a `Metrics` query array carries a dimension
//! set per metric it retrieves rather than on the request itself.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    build_router, call, described_alarm, error_code, error_message, merged, metric_alarm_request,
    AlarmKind,
};

/// The error class a refused configuration carries, as the body of the answer names it: the
/// request-validation class, whose legacy query code (`ValidationError`) travels in the
/// `x-amzn-query-error` header instead.
const REFUSAL_CODE: &str = "ValidationException";

/// The sentence this refusal is made in, for a set repeating `name`.
fn refusal(name: &str) -> String {
    format!("Dimension names must be unique (duplicate: {name})")
}

/// One dimension, as the wire carries it.
fn dimension(name: &str, value: &str) -> Value {
    json!({"Name": name, "Value": value})
}

/// Assert that `request` is refused with HTTP 400 `ValidationError` carrying `expected`, and
/// that it left no alarm under `name`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    request: &Value,
    expected: &str,
) {
    let (status, body) = call(router, "PutMetricAlarm", request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the configuration to be refused with HTTP 400, got {status} \
         with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, REFUSAL_CODE,
        "case {case:?}: expected the error code {REFUSAL_CODE}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal {expected:?}, got {message:?}"
    );
    let stored = described_alarm(router, AlarmKind::Metric, name).await;
    assert_eq!(
        stored, None,
        "case {case:?}: a refused configuration must store nothing, got {stored:?}"
    );
}

#[tokio::test]
async fn an_alarm_whose_dimension_set_repeats_a_name_is_refused_naming_that_dimension() {
    // (case, the request's Dimensions, the repeated name the refusal must report, or None when
    //  the set names each dimension once and the alarm is created)
    //
    // Rows differ along where the repeat falls, how long the set is, how often a name repeats
    // and whether the repeated entries share a value — never along the dimension's name, which
    // the rule does not speak about.
    let rows: [(&str, Vec<Value>, Option<&str>); 11] = [
        (
            "one dimension",
            vec![dimension("BucketName", "estate")],
            None,
        ),
        (
            "two distinct names",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            None,
        ),
        (
            "two names differing only in case",
            // "Dimension names and values are case sensitive", so these are two dimensions.
            vec![
                dimension("FilterId", "ImportantPrefix"),
                dimension("filterid", "ImportantTag"),
            ],
            None,
        ),
        (
            "the same name twice, different values",
            vec![
                dimension("FilterId", "ImportantTag"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            Some("FilterId"),
        ),
        (
            "the same name and the same value twice",
            vec![
                dimension("FilterId", "ImportantTag"),
                dimension("FilterId", "ImportantTag"),
            ],
            Some("FilterId"),
        ),
        (
            "a repeat in the leading pair of a longer set",
            vec![
                dimension("FilterId", "ImportantTag"),
                dimension("FilterId", "ImportantPrefix"),
                dimension("BucketName", "estate"),
            ],
            Some("FilterId"),
        ),
        (
            "a repeat in the trailing pair of a longer set",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantTag"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            Some("FilterId"),
        ),
        (
            "a repeat straddling another dimension",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantTag"),
                dimension("BucketName", "other-estate"),
            ],
            Some("BucketName"),
        ),
        (
            "the first of two repeated names",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantTag"),
                dimension("BucketName", "other-estate"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            Some("BucketName"),
        ),
        (
            "one name three times",
            vec![
                dimension("FilterId", "EntireBucket"),
                dimension("FilterId", "ImportantPrefix"),
                dimension("FilterId", "ImportantTag"),
            ],
            Some("FilterId"),
        ),
        (
            // The member's own maximum is 30 (`Dimensions`, `@length max 30`), so a set at the
            // cap whose names are all distinct is accepted: the rule bounds repeats, not size.
            "thirty distinct names",
            (0..30)
                .map(|index| dimension(&format!("Coordinate{index}"), "v"))
                .collect(),
            None,
        ),
    ];

    let router = build_router();
    for (index, (case, dimensions, duplicate)) in rows.into_iter().enumerate() {
        let name = format!("dimension-uniqueness-{index}");
        let request = merged(
            metric_alarm_request(&name),
            &json!({"Dimensions": dimensions}),
        );
        match duplicate {
            Some(repeated) => {
                assert_refused(&router, case, &name, &request, &refusal(repeated)).await
            }
            None => {
                let (status, body) = call(&router, "PutMetricAlarm", &request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the configuration to be accepted with HTTP 200, got \
                     {status} with body {body}"
                );
                let stored = described_alarm(&router, AlarmKind::Metric, &name)
                    .await
                    .unwrap_or_default();
                assert_eq!(
                    stored.get("Dimensions"),
                    request.get("Dimensions"),
                    "case {case:?}: the stored alarm must carry the dimension set it was created \
                     with, got {stored}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_query_arrays_own_metric_dimensions_are_judged_by_the_same_rule() {
    // The series an alarm watches may be stated as a `Metrics` query array instead, which
    // carries a dimension set per metric it RETRIEVES — the same identity, so the same rule.
    //
    // (case, the dimension set of the retrieved metric, the repeated name, or None when the
    //  array is one CloudWatch creates an alarm from)
    let rows: [(&str, Vec<Value>, Option<&str>); 4] = [
        (
            "distinct names in the retrieved metric",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            None,
        ),
        (
            "a repeated name in the retrieved metric",
            vec![
                dimension("FilterId", "ImportantTag"),
                dimension("FilterId", "ImportantPrefix"),
            ],
            Some("FilterId"),
        ),
        (
            "a repeat straddling another dimension in the retrieved metric",
            vec![
                dimension("BucketName", "estate"),
                dimension("FilterId", "ImportantTag"),
                dimension("BucketName", "other-estate"),
            ],
            Some("BucketName"),
        ),
        (
            "one name three times in the retrieved metric",
            vec![
                dimension("FilterId", "EntireBucket"),
                dimension("FilterId", "ImportantPrefix"),
                dimension("FilterId", "ImportantTag"),
            ],
            Some("FilterId"),
        ),
    ];

    let router = build_router();
    for (index, (case, dimensions, duplicate)) in rows.into_iter().enumerate() {
        let name = format!("query-dimension-uniqueness-{index}");
        let request = json!({
            "AlarmName": name,
            "Metrics": [{
                "Id": "requests",
                "ReturnData": true,
                "MetricStat": {
                    "Metric": {
                        "Namespace": "AWS/S3",
                        "MetricName": "AllRequests",
                        "Dimensions": dimensions,
                    },
                    "Period": 60,
                    "Stat": "Sum",
                },
            }],
            "EvaluationPeriods": 1,
            "Threshold": 10.0,
            "ComparisonOperator": "GreaterThanThreshold",
        });
        match duplicate {
            Some(repeated) => {
                assert_refused(&router, case, &name, &request, &refusal(repeated)).await
            }
            None => {
                let (status, body) = call(&router, "PutMetricAlarm", &request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the configuration to be accepted with HTTP 200, got \
                     {status} with body {body}"
                );
                let stored = described_alarm(&router, AlarmKind::Metric, &name)
                    .await
                    .unwrap_or_default();
                assert_eq!(
                    stored.get("Metrics"),
                    request.get("Metrics"),
                    "case {case:?}: the stored alarm must carry the query array it was created \
                     with, got {stored}"
                );
            }
        }
    }
}
