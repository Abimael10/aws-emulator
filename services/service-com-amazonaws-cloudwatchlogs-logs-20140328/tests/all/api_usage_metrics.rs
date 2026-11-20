// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the usage telemetry this service publishes about its OWN API: the call
//! and error counts of the calls it serves.
//!
//! The rule under test: **every call this service answers is counted as one observation of 1
//! into the `CallCount` series of this account — in the reserved `AWS/Usage` namespace and in
//! this service's own `AWS/Logs` namespace, both under the usage dimension set
//! `{Class, Resource, Service, Type}` = `{None, <the operation called>, Logs, API}`, and once
//! more into the `AWS/Logs` series carrying NO dimensions, which is the account's figure across
//! every operation — and a call answered as a FAILURE is counted the same two ways into
//! `AWS/Logs` `ErrorCount` beside it, which the usage namespace does NOT publish: that namespace
//! reports how many calls were made, and how they were ANSWERED is reported in the service's own
//! namespace.** So a `Sum` over a window is how many calls that window held and how many of them
//! failed, per operation or for the account, and an operation nothing called has no datapoint at
//! all rather than a zero.
//!
//! Every call goes over the wire — `POST /` with `X-Amz-Target: Logs_20140328.<Operation>`
//! through the router that mounts this service — so a count that was not published by the
//! answer cannot appear. What the counts are READ from is the CloudWatch emulator's own metric
//! store on the shared engine, which is where they have to land for `GetMetricData` to answer
//! with them; that cross-service read path is asserted through CloudWatch itself in
//! `core/router/tests/all/logs_api_usage_metrics.rs`.
//!
//! The rows differ along what the rule generalises over: which operation was called (a write, a
//! read, a listing, an ingest), how the call was ANSWERED (accepted, refused by the handler's
//! own modelled error, refused before any handler ran), and how MANY times it was called. The
//! ingest the recorded case happens to count is one row among them.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::harness::{base_ms, group_with_stream, Logs};
use crate::metric_filter_extraction::published;

/// The namespace an Amazon Web Services service publishes its own API usage in, and this
/// service's own namespace, which publishes the same counters.
const USAGE_NAMESPACE: &str = "AWS/Usage";
const LOGS_NAMESPACE: &str = "AWS/Logs";

/// The usage metric counting the API requests an account made, and the one counting those of
/// them that were answered as failures.
const CALL_COUNT: &str = "CallCount";
const ERROR_COUNT: &str = "ErrorCount";

/// The name an account's usage metrics publish this service's calls under — the `Service`
/// dimension of every usage series of it, which is what a reader has to name to find them.
///
/// SOURCE: the service as the model names it for a consumer outside its own API
/// (`smithy_query(query="service_info")`, `cloudFormationName` `Logs`).
const USAGE_SERVICE: &str = "Logs";

/// The four dimensions a usage series carries, as the store keys them (ascending by name).
const USAGE_DIMENSION_NAMES: [&str; 4] = ["Class", "Resource", "Service", "Type"];

/// The `StandardUnit` value a datapoint carries when its publisher declared none, which a call
/// count does: a count of calls is a bare number.
const UNIT_NONE: &str = "None";

/// The dimension set one operation's usage series is published under.
fn usage_dimensions(operation: &str) -> BTreeMap<String, String> {
    [
        ("Class", "None"),
        ("Resource", operation),
        ("Service", USAGE_SERVICE),
        ("Type", "API"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect()
}

/// The values the metric `metric` of `namespace` holds under `dimensions`: one entry per
/// datapoint published into that one series.
fn series(
    logs: &Logs,
    namespace: &str,
    metric: &str,
    dimensions: &BTreeMap<String, String>,
) -> Vec<f64> {
    published(logs, namespace, metric)
        .into_iter()
        .filter(|datapoint| &datapoint.dimensions == dimensions)
        .map(|datapoint| datapoint.value)
        .collect()
}

/// Every series one operation's calls are counted into: the two per-operation ones, and the
/// account-wide one. Named so a failure says which of them disagreed.
fn counted_series(logs: &Logs, metric: &str, operation: &str) -> Vec<(&'static str, Vec<f64>)> {
    vec![
        (
            "the usage namespace, per operation",
            series(logs, USAGE_NAMESPACE, metric, &usage_dimensions(operation)),
        ),
        (
            "this service's namespace, per operation",
            series(logs, LOGS_NAMESPACE, metric, &usage_dimensions(operation)),
        ),
        (
            "this service's namespace, for the account",
            series(logs, LOGS_NAMESPACE, metric, &BTreeMap::new()),
        ),
    ]
}

/// The series an ERROR is counted into: this service's own namespace alone, per operation and
/// for the account.
fn error_series(logs: &Logs, operation: &str) -> Vec<(&'static str, Vec<f64>)> {
    counted_series(logs, ERROR_COUNT, operation)
        .into_iter()
        .filter(|(where_, _)| where_.starts_with("this service's namespace"))
        .collect()
}

/// One row: the case, the operation called, the request, the status it answers, and how many
/// times the row calls it.
struct Row {
    /// What the row varies, named in every assertion.
    case: &'static str,
    /// The operation called, which is the `Resource` dimension of the series it is counted in.
    operation: &'static str,
    /// The request body, built per row because one row's needs a timestamp.
    request: fn() -> Value,
    /// The status the call answers, which decides whether it is also counted as an error.
    status: u16,
    /// How many times the row makes the call.
    times: usize,
}

/// The log group every row that needs one works in.
const GROUP: &str = "api-usage/group";

/// The stream inside it.
const STREAM: &str = "events";

/// How many calls the set-up makes before a row's own: `CreateLogGroup` and `CreateLogStream`.
const SET_UP_CALLS: usize = 2;

/// A log group name no `logGroupName` may carry, so the ingest naming it is refused before any
/// handler runs.
const ILL_FORMED_GROUP: &str = "api usage bad name!*";

#[tokio::test]
async fn every_served_call_is_counted_into_this_services_own_api_usage() {
    let rows = [
        Row {
            case: "a write, called twice",
            operation: "PutRetentionPolicy",
            request: || json!({"logGroupName": GROUP, "retentionInDays": 7}),
            status: 200,
            times: 2,
        },
        Row {
            case: "a read, called several times",
            operation: "DescribeLogGroups",
            request: || json!({}),
            status: 200,
            times: 4,
        },
        Row {
            case: "an ingest that stores its events",
            operation: "PutLogEvents",
            request: || {
                json!({
                    "logGroupName": GROUP,
                    "logStreamName": STREAM,
                    "logEvents": crate::harness::batch(&[(base_ms(), "a line")]),
                })
            },
            status: 200,
            times: 3,
        },
        Row {
            case: "a read the handler refuses with its modelled not-found error",
            operation: "GetLogEvents",
            request: || json!({"logGroupName": "api-usage/absent", "logStreamName": STREAM}),
            status: 400,
            times: 2,
        },
        Row {
            case: "an ingest refused before any handler ran, for a name the model does not admit",
            operation: "PutLogEvents",
            request: || {
                json!({
                    "logGroupName": ILL_FORMED_GROUP,
                    "logStreamName": STREAM,
                    "logEvents": crate::harness::batch(&[(base_ms(), "a line")]),
                })
            },
            status: 400,
            times: 3,
        },
        Row {
            case: "a listing of an account resource",
            operation: "DescribeMetricFilters",
            request: || json!({}),
            status: 200,
            times: 1,
        },
    ];

    for row in rows {
        // A fresh emulator per row, so the only calls counted are the row's own and the two
        // the set-up makes.
        let logs = Logs::new();
        group_with_stream(&logs, GROUP, STREAM, None).await;
        for _ in 0..row.times {
            let (status, body) = logs.call(row.operation, &(row.request)()).await;
            assert_eq!(
                status, row.status,
                "case {:?}: expected {} to answer HTTP {}, got {status} with body {body}",
                row.case, row.operation, row.status
            );
        }

        #[allow(clippy::cast_precision_loss)]
        let expected_calls = row.times as f64;
        for (where_, values) in counted_series(&logs, CALL_COUNT, row.operation) {
            // The account-wide series carries the set-up's calls too; the per-operation ones
            // carry only the row's own, because the set-up calls other operations.
            let expected = if where_.ends_with("for the account") {
                #[allow(clippy::cast_precision_loss)]
                let all = (row.times + SET_UP_CALLS) as f64;
                all
            } else {
                expected_calls
            };
            let counted: f64 = values.iter().sum();
            assert!(
                (counted - expected).abs() < f64::EPSILON,
                "case {:?} in {where_}: expected {expected} calls of {} counted, got {values:?}",
                row.case,
                row.operation
            );
        }

        let expected_errors = if row.status >= 400 { expected_calls } else { 0.0 };
        for (where_, values) in error_series(&logs, row.operation) {
            let counted: f64 = values.iter().sum();
            assert!(
                (counted - expected_errors).abs() < f64::EPSILON,
                "case {:?} in {where_}: expected {expected_errors} errors counted for {} \
                 answered HTTP {}, got {values:?}",
                row.case,
                row.operation,
                row.status
            );
        }
        // The usage namespace publishes how many calls were made and not how they were
        // answered, so no error of this account is counted there whatever a call answered.
        let in_the_usage_namespace = published(&logs, USAGE_NAMESPACE, ERROR_COUNT);
        assert!(
            in_the_usage_namespace.is_empty(),
            "case {:?}: the usage namespace publishes no error count, got \
             {in_the_usage_namespace:?}",
            row.case
        );

        // The set-up's own calls are counted under their own operations, and an operation
        // nothing called has no datapoint at all rather than a zero.
        for (where_, values) in counted_series(&logs, CALL_COUNT, "CreateLogGroup") {
            let expected = if where_.ends_with("for the account") {
                #[allow(clippy::cast_precision_loss)]
                let all = (row.times + SET_UP_CALLS) as f64;
                all
            } else {
                1.0
            };
            let counted: f64 = values.iter().sum();
            assert!(
                (counted - expected).abs() < f64::EPSILON,
                "case {:?} in {where_}: expected the set-up's CreateLogGroup counted {expected} \
                 times, got {values:?}",
                row.case
            );
        }
        for (where_, values) in counted_series(&logs, CALL_COUNT, "DeleteLogGroup") {
            if where_.ends_with("for the account") {
                continue;
            }
            assert!(
                values.is_empty(),
                "case {:?} in {where_}: an operation nothing called has no usage datapoint, got \
                 {values:?}",
                row.case
            );
        }
    }
}

/// One operation called a mix of ways: the two counters read side by side, which is what a
/// client computes an error RATE from.
#[tokio::test]
async fn the_calls_that_failed_are_counted_beside_the_calls_that_were_made() {
    let logs = Logs::new();
    group_with_stream(&logs, GROUP, STREAM, None).await;

    let accepted: i64 = 2;
    let refused: i64 = 3;
    for index in 0..accepted {
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": crate::harness::batch(&[(base_ms() + index, "a line")]),
            }),
        )
        .await;
    }
    for _ in 0..refused {
        let (status, body) = logs
            .call(
                "PutLogEvents",
                &json!({
                    "logGroupName": ILL_FORMED_GROUP,
                    "logStreamName": STREAM,
                    "logEvents": crate::harness::batch(&[(base_ms(), "a line")]),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case \"an ill-formed log group name\": expected HTTP 400, got {status} with body \
             {body}"
        );
    }

    #[allow(clippy::cast_precision_loss)]
    let made = (accepted + refused) as f64;
    #[allow(clippy::cast_precision_loss)]
    let failed = refused as f64;
    for (where_, values) in counted_series(&logs, CALL_COUNT, "PutLogEvents") {
        if where_.ends_with("for the account") {
            continue;
        }
        let counted: f64 = values.iter().sum();
        assert!(
            (counted - made).abs() < f64::EPSILON,
            "case \"two accepted and three refused ingests\" in {where_}: expected {made} calls, \
             got {values:?}"
        );
    }
    for (where_, values) in error_series(&logs, "PutLogEvents") {
        let counted: f64 = values.iter().sum();
        assert!(
            (counted - failed).abs() < f64::EPSILON,
            "case \"two accepted and three refused ingests\" in {where_}: expected {failed} \
             errors, got {values:?}"
        );
    }
}

/// The series' own shape: exactly the four usage dimensions where they are published per
/// operation, none at all for the account's own figure, and the unit a bare count carries.
#[tokio::test]
async fn a_usage_series_carries_the_four_usage_dimensions_and_no_unit_of_its_own() {
    let logs = Logs::new();
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    let (status, _body) = logs
        .call("GetLogEvents", &json!({"logGroupName": "api-usage/absent"}))
        .await;
    assert_eq!(
        status, 400,
        "case \"a read of a group that does not exist\": expected HTTP 400, got {status}"
    );

    for (metric, namespace) in [
        (CALL_COUNT, USAGE_NAMESPACE),
        (CALL_COUNT, LOGS_NAMESPACE),
        (ERROR_COUNT, LOGS_NAMESPACE),
    ] {
        let datapoints = published(&logs, namespace, metric);
        assert!(
            !datapoints.is_empty(),
            "case {namespace:?}/{metric:?}: the calls above publish this series, got none"
        );
        for datapoint in datapoints {
            let names: Vec<&str> = datapoint.dimensions.keys().map(String::as_str).collect();
            // The account's own figure is the series with no dimensions, which only this
            // service's own namespace publishes.
            let admissible: Vec<Vec<&str>> = if namespace == LOGS_NAMESPACE {
                vec![USAGE_DIMENSION_NAMES.to_vec(), Vec::new()]
            } else {
                vec![USAGE_DIMENSION_NAMES.to_vec()]
            };
            assert!(
                admissible.contains(&names),
                "case {namespace:?}/{metric:?}: expected one of {admissible:?} as the \
                 dimension set, got {:?}",
                datapoint.dimensions
            );
            if !names.is_empty() {
                assert_eq!(
                    datapoint.dimensions.get("Service").map(String::as_str),
                    Some(USAGE_SERVICE),
                    "case {namespace:?}/{metric:?}: the Service dimension names this \
                     service, got {:?}",
                    datapoint.dimensions
                );
            }
            assert_eq!(
                datapoint.unit, UNIT_NONE,
                "case {namespace:?}/{metric:?}: a count of calls carries no unit of its own, \
                 got {:?}",
                datapoint.unit
            );
            assert!(
                (datapoint.value - 1.0).abs() < f64::EPSILON,
                "case {namespace:?}/{metric:?}: one call is one observation of 1, got {}",
                datapoint.value
            );
        }
    }
}
