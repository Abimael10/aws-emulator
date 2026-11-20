// SPDX-License-Identifier: CC0-1.0

//! The API usage CloudWatch Logs publishes about itself, read back through CloudWatch.
//!
//! The rule under test: **every call CloudWatch Logs answers is counted as one observation of 1
//! into the `CallCount` series of the account — in the reserved `AWS/Usage` namespace and in
//! CloudWatch Logs' own `AWS/Logs` namespace, both under the usage dimension set
//! `{Class, Resource, Service, Type}` = `{None, <the operation called>, Logs, API}`, and once
//! more into the `AWS/Logs` series carrying NO dimensions, which is the account's figure across
//! every operation — and a call answered as a FAILURE is counted the same two ways into
//! `AWS/Logs` `ErrorCount` beside it, which the usage namespace does NOT publish.** Two crates
//! have to agree for that to hold (CloudWatch Logs counts, CloudWatch answers for the series),
//! so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: which operation was called, how the
//! call was answered (accepted, refused by the handler's own modelled error, refused before any
//! handler ran), and how many times it was called.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// CloudWatch Logs' `X-Amz-Target` prefix and the signing name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// CloudWatch's `X-Amz-Target` prefix and signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace an account's API usage is published in, and CloudWatch Logs' own namespace.
const USAGE_NAMESPACE: &str = "AWS/Usage";
const LOGS_NAMESPACE: &str = "AWS/Logs";

/// The metric counting the calls an account made, and the one counting those answered as a
/// failure.
const CALL_COUNT: &str = "CallCount";
const ERROR_COUNT: &str = "ErrorCount";

/// The name an account's usage metrics publish CloudWatch Logs' calls under.
const USAGE_SERVICE: &str = "Logs";

/// The granularity a call count is read at: one minute.
const USAGE_PERIOD: i64 = 60;

/// The log group the rows that need one work in, and the stream inside it.
const GROUP: &str = "router-api-usage/group";
const STREAM: &str = "events";

/// A log group name no `logGroupName` may carry, so an ingest naming it is refused before any
/// handler runs.
const ILL_FORMED_GROUP: &str = "router api usage bad name!*";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// The SigV4 header a request carries, naming the signing name the router routes on.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
    )
}

/// Call one operation of either service over its awsJson wire.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Call one CloudWatch Logs operation, returning the status it answered with.
async fn logs(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        router,
        LOGS_SIGNING_NAME,
        LOGS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        request,
    )
    .await
}

/// Call one CloudWatch Logs operation that must succeed.
async fn logs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) {
    let (status, parsed) = logs(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from logs {operation}, got {status} with body {parsed}"
    );
}

/// Call one CloudWatch operation, requiring it to succeed.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, parsed) = call(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        request,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {parsed}"
    );
    parsed
}

/// Epoch seconds, the unit a CloudWatch window is named in.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch")
        .as_secs_f64()
}

/// Epoch milliseconds, the unit a CloudWatch Logs timestamp carries.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}

/// The usage dimension set of one operation's series, as `GetMetricData` names dimensions.
fn usage_dimensions(operation: &str) -> Value {
    json!([
        {"Name": "Class", "Value": "None"},
        {"Name": "Resource", "Value": operation},
        {"Name": "Service", "Value": USAGE_SERVICE},
        {"Name": "Type", "Value": "API"},
    ])
}

/// What `GetMetricData` answers for one series over the last few minutes: the `Sum` of its
/// datapoints, and `None` for a series that holds none at all.
///
/// The window is several periods wide and summed across them, because a run of calls can
/// straddle a minute boundary: how many calls were counted is the total, not one window's.
async fn read_series(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> Option<f64> {
    let now = now_secs();
    let body = cloudwatch(
        router,
        case,
        "GetMetricData",
        &json!({
            "MetricDataQueries": [{
                "Id": "usage",
                "MetricStat": {
                    "Metric": {
                        "Namespace": namespace,
                        "MetricName": metric,
                        "Dimensions": dimensions,
                    },
                    "Period": USAGE_PERIOD,
                    "Stat": "Sum",
                },
            }],
            "StartTime": now - 600.0,
            "EndTime": now + 120.0,
        }),
    )
    .await;
    let values: Vec<f64> = body
        .get("MetricDataResults")
        .and_then(Value::as_array)
        .and_then(|results| results.first().cloned())
        .and_then(|result| result.get("Values").cloned())
        .and_then(|values| values.as_array().cloned())
        .map(|values| values.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum())
}

/// One row: the case, the operation called, the request it carries, the status it answers, and
/// how many times the row calls it.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
    status: u16,
    times: usize,
}

/// An ingest of one event into the row's group and stream.
fn ingest(group: &'static str) -> Value {
    json!({
        "logGroupName": group,
        "logStreamName": STREAM,
        "logEvents": [{"timestamp": now_ms() - 60_000, "message": "a line"}],
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "a read, called several times",
        operation: "DescribeLogGroups",
        request: || json!({}),
        status: 200,
        times: 3,
    },
    Row {
        case: "an ingest that stores its events",
        operation: "PutLogEvents",
        request: || ingest(GROUP),
        status: 200,
        times: 2,
    },
    Row {
        case: "a read the handler refuses with its modelled not-found error",
        operation: "GetLogEvents",
        request: || json!({"logGroupName": "router-api-usage/absent", "logStreamName": STREAM}),
        status: 400,
        times: 2,
    },
    Row {
        case: "an ingest refused before any handler ran, for a name the model does not admit",
        operation: "PutLogEvents",
        request: || ingest(ILL_FORMED_GROUP),
        status: 400,
        times: 3,
    },
];

#[tokio::test]
async fn a_logs_call_is_counted_into_the_accounts_api_usage_and_read_back_through_cloudwatch() {
    for row in ROWS {
        // A fresh emulator per row, so the only calls counted are the row's own and the two the
        // set-up makes.
        let router = build_router();
        logs_ok(&router, row.case, "CreateLogGroup", &json!({"logGroupName": GROUP})).await;
        logs_ok(
            &router,
            row.case,
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
        )
        .await;
        for _ in 0..row.times {
            let (status, body) = logs(&router, row.operation, &(row.request)()).await;
            assert_eq!(
                status, row.status,
                "case {:?}: expected logs {} to answer HTTP {}, got {status} with body {body}",
                row.case, row.operation, row.status
            );
        }

        #[allow(clippy::cast_precision_loss)]
        let calls = row.times as f64;
        // Both namespaces publish the call count of the operation that was called.
        for namespace in [USAGE_NAMESPACE, LOGS_NAMESPACE] {
            let read = read_series(
                &router,
                row.case,
                namespace,
                CALL_COUNT,
                &usage_dimensions(row.operation),
            )
            .await;
            assert_eq!(
                read,
                Some(calls),
                "case {:?}: expected {namespace} {CALL_COUNT} of {} to read {calls}, got {read:?}",
                row.case,
                row.operation
            );
        }

        // The account's own figure counts every call, the row's and the set-up's two.
        #[allow(clippy::cast_precision_loss)]
        let account_calls = (row.times + 2) as f64;
        let read = read_series(&router, row.case, LOGS_NAMESPACE, CALL_COUNT, &json!([])).await;
        assert_eq!(
            read,
            Some(account_calls),
            "case {:?}: expected the account's call count to read {account_calls}, got {read:?}",
            row.case
        );

        // The errors: counted in this service's own namespace, per operation and for the
        // account, and never in the usage namespace.
        let errors = if row.status >= 400 { Some(calls) } else { None };
        let read = read_series(
            &router,
            row.case,
            LOGS_NAMESPACE,
            ERROR_COUNT,
            &usage_dimensions(row.operation),
        )
        .await;
        assert_eq!(
            read, errors,
            "case {:?}: expected the error count of {} to read {errors:?}, got {read:?}",
            row.case, row.operation
        );
        let read = read_series(&router, row.case, LOGS_NAMESPACE, ERROR_COUNT, &json!([])).await;
        assert_eq!(
            read, errors,
            "case {:?}: expected the account's error count to read {errors:?}, got {read:?}",
            row.case
        );
        let read = read_series(
            &router,
            row.case,
            USAGE_NAMESPACE,
            ERROR_COUNT,
            &usage_dimensions(row.operation),
        )
        .await;
        assert_eq!(
            read, None,
            "case {:?}: the usage namespace publishes no error count, got {read:?}",
            row.case
        );

        // And an operation nothing called has no series at all rather than a zero.
        let read = read_series(
            &router,
            row.case,
            USAGE_NAMESPACE,
            CALL_COUNT,
            &usage_dimensions("DeleteLogGroup"),
        )
        .await;
        assert_eq!(
            read, None,
            "case {:?}: an operation nothing called holds no datapoint, got {read:?}",
            row.case
        );
    }
}
