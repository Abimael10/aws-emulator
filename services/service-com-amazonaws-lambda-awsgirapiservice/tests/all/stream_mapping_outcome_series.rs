// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/Lambda` series of a function driven by an event source mapping on an Amazon
//! DynamoDB table's STREAM say about what each attempt CAME TO, end to end over the wire, with
//! DynamoDB, DynamoDB Streams and CloudWatch mounted beside Lambda on ONE engine — so the item
//! is written where a client writes it and every attempt is counted where a client counts it.
//!
//! RULE: Lambda meters each invocation attempt on the invoked function's own series by what
//! that attempt came to, whichever surface drove it — an attempt whose handler raised is one
//! `Errors` count, and an attempt Lambda refused because the function's reserved concurrency
//! holds no slot ("The number of simultaneous executions to reserve for the function",
//! `smithy_query(query="doc", shape="PutFunctionConcurrencyRequest",
//! member="ReservedConcurrentExecutions")`) is one `Throttles` count and no `Invocations` at all,
//! because it never reached the function. So the attempts a mapping makes with no client action
//! at all, and each retry its `MaximumRetryAttempts` allows ("Discard records after the
//! specified number of retries", `smithy_query(query="doc",
//! shape="CreateEventSourceMappingRequest", member="MaximumRetryAttempts")`), are counted on
//! those series exactly as a client's own `Invoke` is. The series themselves are a recorded
//! behaviour of the service and are not in the Smithy model; what they are named and counted in
//! is the AWS Lambda metric reference.
//!
//! The first suite's rows differ along what the rule generalises over: the retry allowance the
//! mapping states (none, one, several), the `BatchSize` the records are read under (one at a
//! time, and a ceiling above the whole backlog) and how many records the backlog holds — so no
//! row's figure is a constant, and the count each row expects is the attempts its own allowance
//! bought.
//!
//! The second suite's rows differ along the reservation in force when the mapping's attempt is
//! made: a reservation with no slot in it at all, under two batching ceilings and two backlogs,
//! and — the other side of the same rule — a reservation that does admit the attempt, which is
//! metered as an invocation and not as a throttle.
//!
//! The fixtures are [`crate::dynamodb_stream_event_source_delivery`]'s, so this suite drives
//! the same wire as the rest of the stream-mapping family: that suite pins the HAND-OVER, and
//! this one pins what the attempts it forces are metered as. The direct-invocation half of the
//! same namespace is [`crate::invocation_outcome_series`]'s, and how many attempts an allowance
//! buys is [`crate::stream_mapping_retry_allowance`]'s.
//!
//! Every wait here is for a CONDITION, looked at again until it holds or a generous deadline
//! passes; nothing is asserted about how long any of it took, so how fast or how loaded the host
//! is decides no outcome.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    emulator, given_function, given_table, invocation_count, stop_mapping, write_item, Write,
    LOOK_AGAIN, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};

/// How long a wait for work a mapping has to do is given before the case is a failure. Well
/// above the poller's own interval, the growing waits it puts between two attempts at one batch
/// and the spans the table, the function and the mapping settle over, so a case fails only when
/// the mapping did not do the work at all rather than when the host it runs on is loaded.
const PATIENCE: Duration = Duration::from_secs(120);

/// The region every call below signs for.
const REGION: &str = "us-east-1";

/// Where `CreateEventSourceMapping` is bound.
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// Where `PutFunctionConcurrency` is bound:
/// `PUT /2017-10-31/functions/{FunctionName}/concurrency`.
const CONCURRENCY_URI_PREFIX: &str = "/2017-10-31/functions";

/// The `X-Amz-Target` prefix of the CloudWatch service.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The namespace Lambda's own metrics live in, the two series one ATTEMPT is counted on by what
/// it came to, and the dimension they are counted per (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";
/// See [`NAMESPACE`].
const ERRORS: &str = "Errors";
/// See [`NAMESPACE`].
const THROTTLES: &str = "Throttles";
/// See [`NAMESPACE`].
const FUNCTION_DIMENSION: &str = "FunctionName";

/// A SigV4 credential scope naming `signing_name` in [`REGION`]. Nothing in it is verified: the
/// harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda call, over its own restJson1 routes.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudWatch call.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("monitoring"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `Sum` the series `metric` holds for `function`, over a window around now — the read a
/// client makes, which is [`invocation_count`]'s for the other series of the same family.
async fn series_sum(router: &MultiServiceRouter, case: &str, metric: &str, function: &str) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": function }],
            "StartTime": now - 600.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMetricStatistics answered {status} {stats}"
    );
    stats["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// Wait until the series `metric` of `function` holds at least `wanted`, answering the sum it
/// came to; fails only once a generous deadline has passed with it still short.
async fn series_reaching(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    function: &str,
    wanted: f64,
) -> f64 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let held = series_sum(router, case, metric, function).await;
        if held >= wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the {metric} series of {function:?} to reach {wanted}, got \
             {held}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The handler of the first suite: it RAISES on whatever batch it is handed, so every attempt a
/// mapping makes at it is an attempt whose handler failed.
fn raising_handler() -> String {
    r#"
def lambda_handler(event, context):
    records = event["Records"]
    raise RuntimeError("the handler cannot process any of its " + str(len(records)) + " record(s)")
"#
    .to_string()
}

/// The handler of the second suite: it RETURNS on whatever batch it is handed, so an attempt
/// that is metered as anything but an invocation was refused before the function was reached.
fn returning_handler() -> String {
    r#"
def lambda_handler(event, context):
    return {"handled": len(event["Records"])}
"#
    .to_string()
}

/// Create the mapping between `function` and `stream_arn`, reading `batch_size` records per
/// batch and allowing `retries` retries of one batch; answers its UUID.
///
/// `BisectBatchOnFunctionError` is stated as false, which is the member's own default, so a
/// batch its function errors on is attempted again as it stands rather than split.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: i64,
    retries: i64,
) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": batch_size,
            "MaximumRetryAttempts": retries,
            "BisectBatchOnFunctionError": false,
            "Enabled": true,
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["MaximumRetryAttempts"],
        json!(retries),
        "case {case:?}: the mapping must report the allowance it was created with, got {created}"
    );
    created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string()
}

/// `PutFunctionConcurrency`: reserve `reserved` simultaneous executions for `function`.
async fn reserve_concurrency(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    reserved: i64,
) {
    let (status, reserved_answer) = lambda(
        router,
        "PUT",
        &format!("{CONCURRENCY_URI_PREFIX}/{function}/concurrency"),
        &json!({ "ReservedConcurrentExecutions": reserved }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutFunctionConcurrency({reserved}) answered {status} {reserved_answer}"
    );
    assert_eq!(
        reserved_answer["ReservedConcurrentExecutions"],
        json!(reserved),
        "case {case:?}: the function must report the reservation it was given, got \
         {reserved_answer}"
    );
}

/// The backlog of `records` items, written into `table` before the mapping exists, so what the
/// mapping reads from [`TRIM_HORIZON`] is the whole backlog.
async fn given_backlog(router: &MultiServiceRouter, case: &str, table: &str, records: usize) {
    for at in 0..records {
        write_item(router, case, table, &format!("item-{at:02}"), Write::Insert).await;
    }
}

/// One row of the first suite: the allowance the mapping states, the ceiling it reads under and
/// how many records the backlog holds.
struct ErrorRow {
    case: &'static str,
    /// `MaximumRetryAttempts`: the retries of ONE batch the mapping allows.
    retries: i64,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// How many items are written before the mapping exists, one stream record each.
    records: usize,
    /// How many batches this backlog is read in under that ceiling, which is what makes the row's
    /// expected count its own: a ceiling at or above the whole backlog reads it as ONE batch, and
    /// a ceiling of one record reads it as one batch per record. Nothing in between is stated,
    /// because how many batches a ceiling between the two splits a backlog into depends on what a
    /// single read off the shard found, which is not this suite's rule.
    batches: usize,
}

/// The rows: each allowance the rule generalises over, over two batching ceilings and two backlog
/// sizes, so the count a row expects is the one its own allowance bought on its own batches.
const ERROR_ROWS: &[ErrorRow] = &[
    ErrorRow {
        case: "no retry at all, one record under a ceiling above it",
        retries: 0,
        batch_size: 5,
        records: 1,
        batches: 1,
    },
    ErrorRow {
        case: "one retry, one record under a ceiling above it",
        retries: 1,
        batch_size: 5,
        records: 1,
        batches: 1,
    },
    ErrorRow {
        case: "two retries, one record at a time",
        retries: 2,
        batch_size: 1,
        records: 1,
        batches: 1,
    },
    ErrorRow {
        case: "one retry, two records read one at a time",
        retries: 1,
        batch_size: 1,
        records: 2,
        batches: 2,
    },
];

/// RULE: every attempt a stream mapping makes whose handler RAISED is metered as one `Errors`
/// count of the invoked function, and as one `Invocations` count — so the `Errors` sum over a
/// backlog its function cannot process is the first attempt plus the retries the mapping's
/// `MaximumRetryAttempts` allows, on each batch it spends them on, and it settles there once
/// those records are discarded.
#[tokio::test]
async fn each_stream_mapping_attempt_whose_handler_raised_is_metered_as_one_error() {
    for row in ERROR_ROWS {
        let case = row.case;
        let router = emulator();
        let table = format!("errors-{}-{}-{}", row.retries, row.batch_size, row.records);
        let function = format!("{table}-handler");
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        given_function(&router, case, &function, &raising_handler(), json!({})).await;
        given_backlog(&router, case, &table, row.records).await;
        let uuid =
            given_mapping(&router, case, &function, &stream_arn, row.batch_size, row.retries).await;

        // What the drain is metered as: the attempts one batch's allowance bought — the first one
        // plus each retry it allows — on each batch the backlog is read in. Every one of them is
        // one invocation whose handler raised.
        let per_batch = u32::try_from(row.retries).expect("a row's allowance is a count") + 1;
        let batches = u32::try_from(row.batches).expect("a row's batch count is a count");
        let attempts = f64::from(per_batch * batches);
        series_reaching(&router, case, ERRORS, &function, attempts).await;

        // The trigger is torn down before the figures are compared, so nothing can add to them
        // while they are being read: the mapping has spent its allowance on every batch of the
        // backlog and discarded them, so what the series hold is what the whole drain came to —
        // and a build that attempted a discarded batch once more is read here, not raced past.
        stop_mapping(&router, case, &uuid).await;
        let errors = series_sum(&router, case, ERRORS, &function).await;
        assert!(
            (errors - attempts).abs() < f64::EPSILON,
            "case {case:?}: {} record(s) a handler raises on, read in {} batch(es) and retried {} \
             time(s) each, must be metered as exactly {attempts} failed attempt(s), got {errors}",
            row.records,
            row.batches,
            row.retries
        );
        let invocations = invocation_count(&router, case, &function).await;
        assert!(
            (invocations - attempts).abs() < f64::EPSILON,
            "case {case:?}: each of the {attempts} failed attempt(s) must also be metered as one \
             invocation, got {invocations}"
        );
        // Nothing was refused: every attempt reached the function and ran.
        let throttles = series_sum(&router, case, THROTTLES, &function).await;
        assert!(
            throttles.abs() < f64::EPSILON,
            "case {case:?}: an attempt the function ran is no throttle, got {throttles}"
        );
    }
}

/// One row of the second suite: the reservation in force when the mapping attempts its batch,
/// the ceiling it reads under and how many records the backlog holds.
struct ThrottleRow {
    case: &'static str,
    /// `ReservedConcurrentExecutions` the function is given before the mapping exists.
    reserved: i64,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// How many items are written before the mapping exists, one stream record each.
    records: usize,
}

/// The rows: a reservation with no slot in it at all, under two ceilings and two backlogs, and
/// one that does admit the attempt.
const THROTTLE_ROWS: &[ThrottleRow] = &[
    ThrottleRow {
        case: "no slot at all, one record under a ceiling above it",
        reserved: 0,
        batch_size: 5,
        records: 1,
    },
    ThrottleRow {
        case: "no slot at all, two records read one at a time",
        reserved: 0,
        batch_size: 1,
        records: 2,
    },
    ThrottleRow {
        case: "one slot, so the attempt is admitted",
        reserved: 1,
        batch_size: 5,
        records: 1,
    },
];

/// RULE: an attempt a stream mapping makes that Lambda REFUSES because the function's reserved
/// concurrency holds no slot is metered as one `Throttles` count of that function and as no
/// `Invocations` and no `Errors` at all, because it never reached the function — while a
/// reservation that admits the attempt is metered as an invocation and as no throttle.
#[tokio::test]
async fn a_stream_mapping_attempt_reserved_concurrency_refused_is_metered_as_a_throttle() {
    for row in THROTTLE_ROWS {
        let case = row.case;
        let router = emulator();
        let table = format!("throttles-{}-{}-{}", row.reserved, row.batch_size, row.records);
        let function = format!("{table}-handler");
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        given_function(&router, case, &function, &returning_handler(), json!({})).await;
        // The reservation is in force BEFORE the trigger is wired, so every attempt the mapping
        // makes is made under it.
        reserve_concurrency(&router, case, &function, row.reserved).await;
        given_backlog(&router, case, &table, row.records).await;
        let uuid = given_mapping(
            &router,
            case,
            &function,
            &stream_arn,
            row.batch_size,
            // The allowance is irrelevant to a refusal the function never answered: the rows
            // state the member's own stream default of infinite retries.
            -1,
        )
        .await;

        if row.reserved == 0 {
            // A reservation with no slot in it refuses every attempt, so the function runs
            // nothing: the attempts are counted where a client reads back-pressure, and nowhere
            // else. The trigger is torn down once the series has risen, and the figures are read
            // after it, so what they hold is what the refused attempts came to.
            series_reaching(&router, case, THROTTLES, &function, 1.0).await;
            stop_mapping(&router, case, &uuid).await;
            let throttles = series_sum(&router, case, THROTTLES, &function).await;
            assert!(
                throttles >= 1.0,
                "case {case:?}: the refused attempt(s) must be metered as at least one throttle, \
                 got {throttles}"
            );
            let invocations = invocation_count(&router, case, &function).await;
            assert!(
                invocations.abs() < f64::EPSILON,
                "case {case:?}: an attempt that never reached the function is no invocation, got \
                 {invocations}"
            );
            let errors = series_sum(&router, case, ERRORS, &function).await;
            assert!(
                errors.abs() < f64::EPSILON,
                "case {case:?}: an attempt that ran nothing came to no function error, got \
                 {errors}"
            );
            continue;
        }

        // The other side of the rule: a reservation that admits the attempt is metered as an
        // invocation, and the refusal series stays empty.
        let deadline = Instant::now() + PATIENCE;
        loop {
            let invocations = invocation_count(&router, case, &function).await;
            if invocations >= 1.0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: an admitted attempt must be metered as an invocation, got \
                 {invocations}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        stop_mapping(&router, case, &uuid).await;
        let throttles = series_sum(&router, case, THROTTLES, &function).await;
        assert!(
            throttles.abs() < f64::EPSILON,
            "case {case:?}: an attempt the reservation admitted is no throttle, got {throttles}"
        );
    }
}
