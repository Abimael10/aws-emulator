// SPDX-License-Identifier: CC0-1.0

//! An alarm over the `AWS/DynamoDB` `ConditionalCheckFailedRequests` series a TABLE's own
//! refused conditional writes publish, end to end: CloudWatch, Amazon DynamoDB, SNS and SQS on
//! one engine, so what is asserted is what a client sees — the state `DescribeAlarms` reports
//! and the notification waiting on the queue subscribed to the alarm's topic.
//!
//! The RULE asserted here: a metric alarm grades a series ANOTHER service published about its
//! own work exactly as it grades one a client published, and DynamoDB counts every write a
//! condition turned down into that table's `ConditionalCheckFailedRequests` series under the
//! `TableName` dimension, one observation of one per refusal, whichever write surface the
//! refusal came in through. So, with the alarm carrying the missing-data arm under which a
//! period holding no datapoint is within the threshold — `notBreaching`, one of the values
//! `PutMetricAlarm` admits for it (`smithy_query(query="doc",shape="PutMetricAlarmInput",
//! member="TreatMissingData")`):
//!
//! * a table whose refusals cross the alarm's threshold takes it into `ALARM`, which executes
//!   its `AlarmActions` and delivers the notification to the subscribers of the topic they
//!   name;
//! * a table whose refusals stay under the threshold takes it into `OK` and delivers nothing;
//! * a table whose writes all APPLY publishes nothing on that series at all, so the alarm has
//!   only the missing-data arm to grade and settles into `OK` on it.
//!
//! The rows differ along what the rule generalises over: which write surface refused (the item
//! API's three write verbs, the statement surface and the transactional surface), how many
//! writes were refused, how many applied, the threshold the refusals are compared against and
//! the comparison they are compared with — so the same wiring breaches for one row and stays
//! quiet for another, and every row's expected state and expected series sum are computed from
//! the row rather than taken from a recording.
//!
//! Every wait here is for a CONDITION — the table a create owes, the state an evaluation owes,
//! the notification a transition owes — looked at again until it holds or a generous ceiling
//! passes, so how fast the host runs decides nothing asserted here.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    error_code, given_topic_with_subscriber, notifications, rpc, states_over_the_watch,
    until_notified, until_state, Emulator, LOOK_INTERVAL, REGION, TARGET_PREFIX, WORK_WAIT,
};

/// The namespace Amazon DynamoDB publishes a table's own metrics under, the series it counts a
/// turned-down conditional write on and the dimension it reports that series per — written as
/// literals so a series published under a name no AWS reader asks for fails here instead of
/// agreeing with itself.
pub(crate) const NAMESPACE: &str = "AWS/DynamoDB";
pub(crate) const CONDITIONAL_CHECK_FAILED_REQUESTS: &str = "ConditionalCheckFailedRequests";
pub(crate) const TABLE_DIMENSION: &str = "TableName";

/// The `X-Amz-Target` prefix Amazon DynamoDB names its operations under.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The three states an alarm reports (`StateValue`).
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The missing-data arm every alarm here carries, as `TreatMissingData` spells it.
const NOT_BREACHING: &str = "notBreaching";

/// The alarm's period and how many of them it grades: the shortest cadence an alarm may be
/// evaluated on for a series stored at STANDARD resolution — "Valid values are 10, 20, 30, and
/// any multiple of 60. … Be sure to specify 10, 20, or 30 only for metrics that are stored by a
/// PutMetricData call with a StorageResolution of 1"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Period")`) — with the
/// one-period look-back a client writes for "any refusal now".
///
/// The series DynamoDB publishes is stored at standard resolution, which holds no sub-minute
/// detail at all, so a shorter cadence would grade no datapoint of it: one minute is the finest
/// period it can be read or alarmed at. Nothing asserted here depends on how wide the period is
/// — the refusals of a row fall in ONE window of it.
const PERIOD: i64 = 60;
const EVALUATION_PERIODS: i64 = 1;

/// The partition key every row addresses, the version the seeded item carries, and the version
/// a LOSING writer believes it carries — one no stored item here ever holds, so every write
/// guarded by it is turned down.
const KEY: &str = "race";
const LIVE_VERSION: i64 = 0;
const STALE_VERSION: i64 = 99;

/// The largest difference two counts of refusals may show and still be the same count: both
/// sides are sums of whole observations, so anything above rounding is a real divergence.
const TOLERANCE: f64 = 1e-9;

/// CloudWatch, Amazon DynamoDB, SNS and SQS over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the series DynamoDB publishes about the writes it turned down is the one
/// CloudWatch grades, and the notification an alarm action publishes reaches the queue
/// subscribed to its topic.
pub(crate) fn emulator_with_dynamodb_and_notifications() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// One Amazon DynamoDB call over its `awsJson1.0` wire, answering its HTTP status and parsed
/// body — the seam a row needs, since a write a condition turns down is a REFUSAL and the
/// shared `rpc` helper requires a 200.
pub(crate) async fn dynamodb(
    emulator: &Emulator,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/dynamodb/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    (status, parsed)
}

/// The same call, required to be served.
pub(crate) async fn dynamodb_ok(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = dynamodb(emulator, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} ({answered})"
    );
    answered
}

/// One CloudWatch call that must answer 200.
async fn monitoring(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "monitoring", TARGET_PREFIX, operation, body).await
}

/// Which write surface a row's refusals come in through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The item API's three write verbs, each guarded by a `ConditionExpression`.
    Put,
    Update,
    Delete,
    /// The statement surface, guarded by the `WHERE` clause's predicate.
    Statement,
    /// The transactional surface, whose refused element cancels the transaction.
    Transaction,
}

impl Surface {
    /// The operation a refusal of this surface is made with.
    pub(crate) fn operation(self) -> &'static str {
        match self {
            Self::Put => "PutItem",
            Self::Update => "UpdateItem",
            Self::Delete => "DeleteItem",
            Self::Statement => "ExecuteStatement",
            Self::Transaction => "TransactWriteItems",
        }
    }

    /// The error code this surface reports an unmet condition as.
    pub(crate) fn refusal_code(self) -> &'static str {
        match self {
            Self::Transaction => "TransactionCanceledException",
            _ => "ConditionalCheckFailedException",
        }
    }

    /// The request that is turned down: the write guarded on a version no stored item holds.
    pub(crate) fn refused_request(self, table: &str) -> Value {
        match self {
            Self::Put => json!({
                "TableName": table,
                "Item": seeded_item(),
                "ConditionExpression": "version = :expected",
                "ExpressionAttributeValues": {":expected": {"N": STALE_VERSION.to_string()}},
            }),
            Self::Update => json!({
                "TableName": table,
                "Key": {"pk": {"S": KEY}},
                "UpdateExpression": "SET version = :live",
                "ConditionExpression": "version = :expected",
                "ExpressionAttributeValues": {
                    ":expected": {"N": STALE_VERSION.to_string()},
                    ":live": {"N": LIVE_VERSION.to_string()},
                },
            }),
            Self::Delete => json!({
                "TableName": table,
                "Key": {"pk": {"S": KEY}},
                "ConditionExpression": "version = :expected",
                "ExpressionAttributeValues": {":expected": {"N": STALE_VERSION.to_string()}},
            }),
            Self::Statement => json!({
                "Statement": format!(
                    "UPDATE \"{table}\" SET version = {LIVE_VERSION} WHERE pk = '{KEY}' AND \
                     version = {STALE_VERSION}"
                ),
            }),
            Self::Transaction => json!({
                "TransactItems": [{
                    "Put": {
                        "TableName": table,
                        "Item": seeded_item(),
                        "ConditionExpression": "version = :expected",
                        "ExpressionAttributeValues": {
                            ":expected": {"N": STALE_VERSION.to_string()},
                        },
                    },
                }],
            }),
        }
    }
}

/// The item the seeded key holds: the version a winning write states and a losing one does not.
pub(crate) fn seeded_item() -> Value {
    json!({"pk": {"S": KEY}, "version": {"N": LIVE_VERSION.to_string()}})
}

/// One row: which surface refuses, how many writes are refused, how many apply, and the
/// threshold and comparison their count is graded against — and therefore whether the alarm
/// breaches.
struct Row {
    case: &'static str,
    surface: Surface,
    refusals: usize,
    applied: usize,
    threshold: f64,
    comparison: &'static str,
    breaches: bool,
}

/// The rows: a burst of refused puts at the threshold, one refused update over a threshold of
/// none, a refused transaction over a threshold of one, refusals that stay under their
/// threshold on two further surfaces, and a table whose writes all applied.
const ROWS: &[Row] = &[
    Row {
        case: "twelve refused puts, at a threshold of ten",
        surface: Surface::Put,
        refusals: 12,
        applied: 0,
        threshold: 10.0,
        comparison: "GreaterThanOrEqualToThreshold",
        breaches: true,
    },
    Row {
        case: "one refused update, over a threshold of none",
        surface: Surface::Update,
        refusals: 1,
        applied: 0,
        threshold: 0.0,
        comparison: "GreaterThanThreshold",
        breaches: true,
    },
    Row {
        case: "two refused transactional writes, over a threshold of one",
        surface: Surface::Transaction,
        refusals: 2,
        applied: 0,
        threshold: 1.0,
        comparison: "GreaterThanThreshold",
        breaches: true,
    },
    Row {
        case: "three refused deletes, under a threshold of five",
        surface: Surface::Delete,
        refusals: 3,
        applied: 0,
        threshold: 5.0,
        comparison: "GreaterThanOrEqualToThreshold",
        breaches: false,
    },
    Row {
        case: "two refused statements beside two writes that applied, under a threshold of ten",
        surface: Surface::Statement,
        refusals: 2,
        applied: 2,
        threshold: 10.0,
        comparison: "GreaterThanOrEqualToThreshold",
        breaches: false,
    },
    Row {
        case: "a table whose writes all applied, over a threshold of none",
        surface: Surface::Put,
        refusals: 0,
        applied: 3,
        threshold: 0.0,
        comparison: "GreaterThanThreshold",
        breaches: false,
    },
];

/// RULE: an alarm over the series DynamoDB publishes about the conditional writes a table
/// turned down transitions on those refusals — into ALARM when their count crosses its
/// threshold, which delivers its notification to the subscribers of the topic its action names,
/// and into OK when it does not, which delivers nothing.
#[tokio::test]
async fn an_alarm_over_a_tables_refused_conditional_writes_transitions_on_those_refusals() {
    let emulator = emulator_with_dynamodb_and_notifications();
    // Every table is created before any of them is waited on, so the whole catalog becomes
    // ACTIVE over one span rather than one span each.
    let tables: Vec<String> = (0..ROWS.len()).map(|index| format!("ccf-{index}")).collect();
    for table in &tables {
        create_table(&emulator, table).await;
    }
    for (row, table) in ROWS.iter().zip(&tables) {
        table_when_active(&emulator, row.case, table).await;
        // The key the guarded writes address holds an item, so a refusal is a lost race rather
        // than a missing item. It applies, so it publishes nothing on the series under test.
        dynamodb_ok(
            &emulator,
            row.case,
            "PutItem",
            json!({"TableName": table, "Item": seeded_item()}),
        )
        .await;
    }

    // Every row's refusals are published BEFORE any alarm is waited on, so the window that
    // holds them is one window for all of them: the first wait below is the one that waits for
    // it to close, and every row after it is graded on the same boundary rather than on one of
    // its own.
    let mut watched: Vec<(&Row, &String, String, String)> = Vec::new();
    for (index, (row, table)) in ROWS.iter().zip(&tables).enumerate() {
        let case = row.case;
        let (topic, queue_url) =
            given_topic_with_subscriber(&emulator, &format!("ccf-notify-{index}")).await;
        let alarm = format!("ccf-alarm-{index}");
        given_alarm(&emulator, &alarm, table, row.threshold, row.comparison, &topic).await;

        for attempt in 0..row.refusals {
            let (status, answered) = dynamodb(
                &emulator,
                row.surface.operation(),
                row.surface.refused_request(table),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered)),
                (400, row.surface.refusal_code().to_string()),
                "case {case:?}, attempt {attempt}: expected the guarded {} to be turned down, \
                 got {status} ({answered})",
                row.surface.operation()
            );
        }
        for _ in 0..row.applied {
            dynamodb_ok(
                &emulator,
                case,
                "PutItem",
                json!({"TableName": table, "Item": seeded_item()}),
            )
            .await;
        }

        // What the table published is exactly what it turned down: one observation of one per
        // refusal, and nothing for a write that applied.
        let counted = until_series_sum(&emulator, table, row.refusals as f64).await;
        assert!(
            (counted - row.refusals as f64).abs() < TOLERANCE,
            "case {case:?}: expected the {CONDITIONAL_CHECK_FAILED_REQUESTS} series of {table:?} \
             to carry {} refusal(s), got {counted}",
            row.refusals
        );
        watched.push((row, table, alarm, queue_url));
    }

    for (row, table, alarm, queue_url) in watched {
        let case = row.case;
        if row.breaches {
            let state = until_state(&emulator, &alarm, ALARM).await;
            assert_eq!(
                state, ALARM,
                "case {case:?}: {} refused write(s) against a threshold of {} must take the \
                 alarm to {ALARM}, it reports {state:?}",
                row.refusals, row.threshold
            );
            let delivered = until_notified(&emulator, &queue_url).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {case:?}: the transition must deliver exactly one notification to the \
                 subscribed queue, got {delivered:?}"
            );
            let notification = &delivered[0];
            assert_eq!(
                notification["AlarmName"].as_str(),
                Some(alarm.as_str()),
                "case {case:?}: the notification must name the alarm, got {notification}"
            );
            assert_eq!(
                notification["NewStateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: the notification must carry the state moved to, got \
                 {notification}"
            );
            assert_eq!(
                (
                    notification["Trigger"]["Namespace"].as_str(),
                    notification["Trigger"]["MetricName"].as_str(),
                ),
                (Some(NAMESPACE), Some(CONDITIONAL_CHECK_FAILED_REQUESTS)),
                "case {case:?}: the notification must carry the series it fired on, got \
                 {notification}"
            );
            assert_eq!(
                notification["Trigger"]["Dimensions"],
                json!([{"name": TABLE_DIMENSION, "value": table}]),
                "case {case:?}: the notification must carry the table the series belongs to, \
                 got {notification}"
            );
            continue;
        }

        // The quiet rows: the writes WERE graded — the alarm leaves its initial state for OK,
        // on the refusals it read or on the missing-data arm — and nothing is delivered, the
        // alarm carrying no action for the state it moved into.
        let state = until_state(&emulator, &alarm, OK).await;
        assert_eq!(
            state, OK,
            "case {case:?}: {} refused write(s) against a threshold of {} must take the alarm to \
             {OK}, it reports {state:?}",
            row.refusals, row.threshold
        );
        let held = states_over_the_watch(&emulator, &alarm).await;
        assert!(
            held.iter().all(|state| state == OK),
            "case {case:?}: the alarm must HOLD {OK} over the watch, it reported {held:?}"
        );
        let delivered = notifications(&emulator, &queue_url).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: an alarm that never breached must deliver nothing, got {delivered:?}"
        );
    }
}

/// RULE (the waypoints in order, which is what a client's alerting runbook watches): an alarm
/// over a table that has refused nothing settles into OK on the `notBreaching` arm, and the
/// burst of refusals that follows is what takes that same alarm into ALARM and delivers the
/// notification naming the state it moved FROM as well as the one it moved to.
#[tokio::test]
async fn the_refusal_burst_moves_a_settled_alarm_into_alarm_and_notifies() {
    let case = "an alarm settled on no refusals, then a burst of twelve";
    let refusals = 12;
    let threshold = 10.0;
    let emulator = emulator_with_dynamodb_and_notifications();
    let (topic, queue_url) = given_topic_with_subscriber(&emulator, "ccf-notify-burst").await;

    let table = "ccf-burst";
    create_table(&emulator, table).await;
    table_when_active(&emulator, case, table).await;
    dynamodb_ok(
        &emulator,
        case,
        "PutItem",
        json!({"TableName": table, "Item": seeded_item()}),
    )
    .await;

    let alarm = "ccf-alarm-burst";
    given_alarm(
        &emulator,
        alarm,
        table,
        threshold,
        "GreaterThanOrEqualToThreshold",
        &topic,
    )
    .await;

    // Waypoint one: a table that has refused nothing publishes nothing on the series, so the
    // alarm has only the missing-data arm to grade and settles on it.
    let settled = until_state(&emulator, alarm, OK).await;
    assert_eq!(
        settled, OK,
        "case {case:?}: an alarm whose series holds no refusal must settle into {OK} under \
         {NOT_BREACHING}, it reports {settled:?}"
    );
    let delivered = notifications(&emulator, &queue_url).await;
    assert!(
        delivered.is_empty(),
        "case {case:?}: settling into {OK} from the initial state must deliver nothing, got \
         {delivered:?}"
    );

    // Waypoint two: the burst of refusals is published on the table's own series.
    for attempt in 0..refusals {
        let (status, answered) =
            dynamodb(&emulator, "PutItem", Surface::Put.refused_request(table)).await;
        assert_eq!(
            (status, error_code(&answered)),
            (400, "ConditionalCheckFailedException".to_string()),
            "case {case:?}, attempt {attempt}: expected the guarded put to be turned down, got \
             {status} ({answered})"
        );
    }
    let counted = until_series_sum(&emulator, table, refusals as f64).await;
    assert!(
        (counted - refusals as f64).abs() < TOLERANCE,
        "case {case:?}: expected the {CONDITIONAL_CHECK_FAILED_REQUESTS} series of {table:?} to \
         carry {refusals} refusals, got {counted}"
    );

    // Waypoint three: the alarm grades them and moves, and waypoint four: the notification
    // lands on the queue subscribed to the topic the action names.
    let state = until_state(&emulator, alarm, ALARM).await;
    assert_eq!(
        state, ALARM,
        "case {case:?}: {refusals} refusals at a threshold of {threshold} must take the settled \
         alarm to {ALARM}, it reports {state:?}"
    );
    let delivered = until_notified(&emulator, &queue_url).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: the transition must deliver exactly one notification, got {delivered:?}"
    );
    let notification = &delivered[0];
    assert_eq!(
        (
            notification["AlarmName"].as_str(),
            notification["OldStateValue"].as_str(),
            notification["NewStateValue"].as_str(),
        ),
        (Some(alarm), Some(OK), Some(ALARM)),
        "case {case:?}: the notification must name the alarm and both states, got {notification}"
    );
    assert_eq!(
        notification["Trigger"]["Dimensions"],
        json!([{"name": TABLE_DIMENSION, "value": table}]),
        "case {case:?}: the notification must carry the table the series belongs to, got \
         {notification}"
    );
}

/// Create one table on-demand, with the one partition key every row addresses. The caller waits
/// for it to serve, so a suite that creates several tables can have them become ACTIVE over one
/// span rather than one each.
pub(crate) async fn create_table(emulator: &Emulator, name: &str) {
    let (status, created) = dynamodb(
        emulator,
        "CreateTable",
        json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DynamoDB CreateTable for {name:?}, got {status} ({created})"
    );
}

/// Look at `DescribeTable` until DynamoDB reports the table ACTIVE — the state a write needs.
/// The wait is for that service's own report rather than for a span.
pub(crate) async fn table_when_active(emulator: &Emulator, case: &str, name: &str) {
    let deadline = Instant::now() + WORK_WAIT;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let described =
            dynamodb_ok(emulator, case, "DescribeTable", json!({"TableName": name})).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return;
        }
        last = described;
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: DynamoDB never reported {name:?} ACTIVE, last read {last}");
}

/// A metric alarm over the refusal series of `table`, comparing the refusals of a period
/// against `threshold` with `comparison`, with `topic` as its one alarm action and the
/// `notBreaching` missing-data arm.
async fn given_alarm(
    emulator: &Emulator,
    name: &str,
    table: &str,
    threshold: f64,
    comparison: &str,
    topic: &str,
) {
    monitoring(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": CONDITIONAL_CHECK_FAILED_REQUESTS,
            "Dimensions": [{"Name": TABLE_DIMENSION, "Value": table}],
            "Statistic": "Sum",
            "Period": PERIOD,
            "EvaluationPeriods": EVALUATION_PERIODS,
            "Threshold": threshold,
            "ComparisonOperator": comparison,
            "TreatMissingData": NOT_BREACHING,
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// The `Sum` CloudWatch reports for a table's refusal series, over a window wide enough to hold
/// everything this suite published.
async fn series_sum(emulator: &Emulator, table: &str) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = monitoring(
        emulator,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": CONDITIONAL_CHECK_FAILED_REQUESTS,
            "Dimensions": [{"Name": TABLE_DIMENSION, "Value": table}],
            "StartTime": now - 3600.0,
            "EndTime": now + 3600.0,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// Look at the series until it carries `wanted`, and answer the last sum it carried: a
/// published observation is owed by the call that counted it, so the read waits for the
/// condition rather than for a span.
async fn until_series_sum(emulator: &Emulator, table: &str, wanted: f64) -> f64 {
    let deadline = Instant::now() + WORK_WAIT;
    let mut last = series_sum(emulator, table).await;
    while (last - wanted).abs() >= TOLERANCE && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        last = series_sum(emulator, table).await;
    }
    last
}
