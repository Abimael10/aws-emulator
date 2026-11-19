// SPDX-License-Identifier: CC0-1.0

//! HOW LARGE a write that did not commit is billed, and against which item.
//!
//! The rule under test: a write the service turns down because the condition it stated was not
//! met consumes write capacity sized by the item the addressed key ALREADY HOLDS — one unit
//! per kilobyte of that item, never less than one unit, and the one-unit minimum when the key
//! holds nothing — and NOT by the payload of the write it attempted; the same figure doubled
//! when the refused write is an element of a transaction, which is the scale every
//! transactional write is charged on. So a cancelled transaction and a condition-refused item
//! write cost exactly what the write each was shaped like would have cost had it committed,
//! and the `AWS/DynamoDB` `ConsumedWriteCapacityUnits` series of the table carries that cost.
//!
//! The rows span what the rule generalises over: how large the STORED item is (either side of
//! several capacity-unit boundaries, and absent altogether), how large the ATTEMPTED write is
//! relative to it (far smaller, far larger, none at all), which surface refused the write (the
//! item API's three write verbs and the transactional surface), and how many refusals were
//! made. The sizing is what these rows add to the surface catalog in
//! [`super::refused_conditional_write_metering`], whose rows all weigh a refusal and the write
//! it attempted at ONE size and so cannot tell the two apart.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::emulator::{
    call, describe_when_active, emulator_with_cloudwatch, error_code, ok, ok_cloudwatch,
};

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the write capacity units a table consumed.
const CONSUMED_WRITE: &str = "ConsumedWriteCapacityUnits";

/// The version a stored item carries, which a winning write states.
const LIVE_VERSION: i64 = 0;

/// The version a LOSING writer believes the item carries: one no item stored here ever holds,
/// so every write guarded by it is refused and every transaction carrying it is cancelled.
const STALE_VERSION: i64 = 7;

/// A transactional write — refused or applied — is charged at twice what the same single-item
/// write costs: "Transactional write requests require two write capacity units".
const TRANSACTIONAL_FACTOR: f64 = 2.0;

/// The largest difference two capacity figures may show and still be the same figure: both
/// sides are sums of whole units, so anything above rounding is a real divergence.
const TOLERANCE: f64 = 1e-9;

/// Which surface the refused write came in through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// A put of a whole item, whose own payload the row varies independently of the stored
    /// one.
    Put,
    /// An update, whose attempted write is one small clause whatever the stored item weighs.
    Update,
    /// A delete, which attempts no payload at all.
    Delete,
    /// A single-element `TransactWriteItems` put, charged on the transactional scale.
    Transact,
}

impl Surface {
    /// The operation a row of this surface calls.
    fn operation(self) -> &'static str {
        match self {
            Self::Put => "PutItem",
            Self::Update => "UpdateItem",
            Self::Delete => "DeleteItem",
            Self::Transact => "TransactWriteItems",
        }
    }

    /// What one refused write of this surface is charged, given what one single-item write of
    /// the stored item costs.
    fn charge(self, single_item_units: f64) -> f64 {
        match self {
            Self::Transact => single_item_units * TRANSACTIONAL_FACTOR,
            _ => single_item_units,
        }
    }
}

/// One row: what the key holds, what the refused write attempts, and what the refusal costs.
struct Row {
    case: &'static str,
    /// The partition key the row addresses — one per row, so the rows share one table and the
    /// item each stands on is its own.
    key: &'static str,
    /// Bytes of payload the STORED item carries, or `None` for a key that holds no item.
    stored_bytes: Option<usize>,
    /// Bytes of payload the REFUSED write attempts to store.
    attempted_bytes: usize,
    /// The surface the write is refused on.
    surface: Surface,
    /// What ONE single-item write of the stored item costs: one unit per kilobyte of that
    /// item, never less than one. Every `stored_bytes` here sits well inside its band, so the
    /// handful of bytes the key and version attributes add cannot move the figure.
    stored_units: f64,
    /// How many times the row's refused write is made.
    refusals: usize,
}

/// The catalog: the stored size and the attempted size varied independently, over the surfaces
/// that refuse a conditional write.
const ROWS: &[Row] = &[
    Row {
        case: "a tiny put refused against a three-kilobyte stored item",
        key: "small-write-large-item",
        stored_bytes: Some(2_800),
        attempted_bytes: 0,
        surface: Surface::Put,
        stored_units: 3.0,
        refusals: 5,
    },
    Row {
        case: "a five-kilobyte put refused against a key that holds a tiny item",
        key: "large-write-small-item",
        stored_bytes: Some(0),
        attempted_bytes: 4_900,
        surface: Surface::Put,
        stored_units: 1.0,
        refusals: 3,
    },
    Row {
        case: "a five-kilobyte put refused against a two-kilobyte stored item",
        key: "large-write-medium-item",
        stored_bytes: Some(1_200),
        attempted_bytes: 4_900,
        surface: Surface::Put,
        stored_units: 2.0,
        refusals: 2,
    },
    Row {
        case: "a tiny put refused against a five-kilobyte stored item",
        key: "small-write-huge-item",
        stored_bytes: Some(4_900),
        attempted_bytes: 0,
        surface: Surface::Put,
        stored_units: 5.0,
        refusals: 2,
    },
    Row {
        case: "a three-kilobyte put refused on a key that holds no item",
        key: "no-item-at-all",
        stored_bytes: None,
        attempted_bytes: 2_800,
        surface: Surface::Put,
        stored_units: 1.0,
        refusals: 2,
    },
    Row {
        case: "a one-clause update refused against a three-kilobyte stored item",
        key: "update-large-item",
        stored_bytes: Some(2_800),
        attempted_bytes: 0,
        surface: Surface::Update,
        stored_units: 3.0,
        refusals: 2,
    },
    Row {
        case: "a delete refused against a five-kilobyte stored item",
        key: "delete-huge-item",
        stored_bytes: Some(4_900),
        attempted_bytes: 0,
        surface: Surface::Delete,
        stored_units: 5.0,
        refusals: 2,
    },
    Row {
        case: "a cancelled transactional put against a three-kilobyte stored item",
        key: "transact-large-item",
        stored_bytes: Some(2_800),
        attempted_bytes: 0,
        surface: Surface::Transact,
        stored_units: 3.0,
        refusals: 5,
    },
    Row {
        case: "a cancelled transactional put against a one-kilobyte stored item",
        key: "transact-kilobyte-item",
        stored_bytes: Some(900),
        attempted_bytes: 0,
        surface: Surface::Transact,
        stored_units: 1.0,
        refusals: 5,
    },
];

/// The table every row of the catalog addresses, with a key of its own per row.
const TABLE: &str = "contended";

#[tokio::test]
async fn a_refused_write_is_sized_by_the_item_the_key_holds() {
    let router = emulator_with_cloudwatch();
    create_table(&router).await;

    for row in ROWS {
        let case = row.case;
        if let Some(padding) = row.stored_bytes {
            // The item the refusals are weighed on. It lands BEFORE the sums this row reads,
            // so what storing it consumed is outside the delta under test.
            ok(
                &router,
                "PutItem",
                json!({"TableName": TABLE, "Item": item(row.key, padding)}),
            )
            .await;
        }
        let before = series_sum(&router, TABLE).await;

        for attempt in 0..row.refusals {
            let (status, answered) = call(&router, row.surface.operation(), refused(row)).await;
            assert_refused(case, attempt, row, status, &answered);
        }

        let counted = series_sum(&router, TABLE).await - before;
        let expected = row.surface.charge(row.stored_units) * row.refusals as f64;
        assert!(
            (counted - expected).abs() < TOLERANCE,
            "case {case:?}: expected the {} refusals of a {}-byte write to count {expected} \
             units into the {CONSUMED_WRITE} series of {TABLE:?} — the stored item's \
             {} units each — got {counted}",
            row.refusals,
            row.attempted_bytes,
            row.surface.charge(row.stored_units)
        );
        // The figure is the stored item's, so a row whose attempted write is of a different
        // size must NOT be billed at what that write would have weighed.
        let attempted_units = row.surface.charge(units_of(row.attempted_bytes));
        if (attempted_units - row.surface.charge(row.stored_units)).abs() > TOLERANCE {
            let attempted_total = attempted_units * row.refusals as f64;
            assert!(
                (counted - attempted_total).abs() > TOLERANCE,
                "case {case:?}: expected the refusals to be sized by the stored item rather \
                 than by the {}-byte write they attempted ({attempted_total} units), got \
                 {counted}",
                row.attempted_bytes
            );
        }
        // And nothing was stored: the key still holds what it held, and a key that held
        // nothing still holds nothing.
        let held = stored_payload(&router, row.key).await;
        assert_eq!(
            held,
            row.stored_bytes,
            "case {case:?}: expected the refused writes to leave the key as it was, holding \
             {:?} bytes of payload, got {held:?}",
            row.stored_bytes
        );
    }
}

/// Cause of the whole rule, read the other way round: a CANCELLED transaction costs what the
/// COMMITTED transaction it was shaped like costs — the same single-item figure, doubled —
/// over several item sizes, so the equality is not one number.
///
/// The committed control reports its figure in band, and the cancellation reports none at all,
/// so the metric is the only plane the two can be compared on.
#[tokio::test]
async fn a_cancelled_transaction_costs_what_the_committed_one_it_was_shaped_like_costs() {
    let router = emulator_with_cloudwatch();
    create_table(&router).await;

    // Payloads either side of the capacity-unit boundaries, so the control figure moves.
    for (index, padding) in [0_usize, 900, 1_200, 2_800].into_iter().enumerate() {
        let case = format!("a single-put transaction over a {padding}-byte payload");
        let key = format!("shaped-{index}");
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": item(&key, padding)}),
        )
        .await;

        // The control: the same transactional put, stating the version the item really
        // carries, so it commits.
        let before = series_sum(&router, TABLE).await;
        let committed = ok(
            &router,
            "TransactWriteItems",
            json!({
                "TransactItems": [transact_put(&key, padding, LIVE_VERSION)],
                "ReturnConsumedCapacity": "TOTAL",
            }),
        )
        .await;
        let reported = committed["ConsumedCapacity"][0]["CapacityUnits"]
            .as_f64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the committed control to report its capacity, \
                        got {committed}")
            });
        let control = series_sum(&router, TABLE).await - before;
        assert!(
            (control - reported).abs() < TOLERANCE,
            "case {case:?}: expected the committed control's {reported} reported units in the \
             {CONSUMED_WRITE} series, got {control}"
        );
        assert!(
            (control - TRANSACTIONAL_FACTOR * units_of(padding)).abs() < TOLERANCE,
            "case {case:?}: expected the committed control to cost twice the {} units one \
             single-item write of the item costs, got {control}",
            units_of(padding)
        );

        // The cancellations: the same transaction, stating a version no item holds.
        let cancellations = 5;
        let before = series_sum(&router, TABLE).await;
        for attempt in 0..cancellations {
            let (status, answered) = call(
                &router,
                "TransactWriteItems",
                json!({
                    "TransactItems": [transact_put(&key, padding, STALE_VERSION)],
                    "ReturnConsumedCapacity": "TOTAL",
                }),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered)),
                (400, "TransactionCanceledException".to_string()),
                "case {case:?}, attempt {attempt}: expected the transaction to be cancelled, \
                 got {status} {answered:?}"
            );
            assert!(
                answered["CancellationReasons"]
                    .to_string()
                    .contains("ConditionalCheckFailed"),
                "case {case:?}, attempt {attempt}: expected a reason naming the unmet \
                 condition, got {answered:?}"
            );
            assert!(
                answered.get("ConsumedCapacity").is_none(),
                "case {case:?}, attempt {attempt}: a cancellation reports no ConsumedCapacity \
                 in band, got {answered:?}"
            );
        }
        let cancelled = series_sum(&router, TABLE).await - before;
        let expected = control * cancellations as f64;
        assert!(
            (cancelled - expected).abs() < TOLERANCE,
            "case {case:?}: expected the {cancellations} cancellations to cost what the \
             committed control cost — {control} units each, {expected} in all — got {cancelled}"
        );
    }
}

/// The request one row refuses: the write guarded on a version no stored item holds, stated as
/// the surface the row came in through states a condition.
fn refused(row: &Row) -> Value {
    let stale = json!({":expected": {"N": STALE_VERSION.to_string()}});
    match row.surface {
        Surface::Put => json!({
            "TableName": TABLE,
            "Item": item(row.key, row.attempted_bytes),
            "ConditionExpression": "version = :expected",
            "ExpressionAttributeValues": stale,
            "ReturnConsumedCapacity": "TOTAL",
        }),
        Surface::Update => json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": row.key}},
            "UpdateExpression": "SET touched = :expected",
            "ConditionExpression": "version = :expected",
            "ExpressionAttributeValues": stale,
            "ReturnConsumedCapacity": "TOTAL",
        }),
        Surface::Delete => json!({
            "TableName": TABLE,
            "Key": {"pk": {"S": row.key}},
            "ConditionExpression": "version = :expected",
            "ExpressionAttributeValues": stale,
            "ReturnConsumedCapacity": "TOTAL",
        }),
        Surface::Transact => json!({
            "TransactItems": [transact_put(row.key, row.attempted_bytes, STALE_VERSION)],
            "ReturnConsumedCapacity": "TOTAL",
        }),
    }
}

/// One `Put` element of a transaction, guarded on the version `expected`.
fn transact_put(key: &str, padding: usize, expected: i64) -> Value {
    json!({
        "Put": {
            "TableName": TABLE,
            "Item": item(key, padding),
            "ConditionExpression": "version = :expected",
            "ExpressionAttributeValues": {":expected": {"N": expected.to_string()}},
        },
    })
}

/// One item under `key` with `padding` bytes of payload beside the version a guarded write
/// states.
fn item(key: &str, padding: usize) -> Value {
    let mut written = json!({"pk": {"S": key}, "version": {"N": LIVE_VERSION.to_string()}});
    if padding > 0 {
        written["payload"] = json!({"S": "x".repeat(padding)});
    }
    written
}

/// The capacity units ONE single-item write of an item with `padding` bytes of payload costs:
/// one unit per kilobyte of the stored item, never less than one ("Capacity unit consumption
/// for writes": a write of up to 1 KB consumes one write capacity unit). Every padding a row
/// states sits well inside its band, so the bytes the key and version attributes add beside
/// the payload cannot move the figure.
fn units_of(padding: usize) -> f64 {
    let kilobytes = padding.div_ceil(1_024);
    kilobytes.max(1) as f64
}

/// Every surface reports its refusal the way it reports an unmet condition, and none of them
/// reports capacity in band.
fn assert_refused(case: &str, attempt: usize, row: &Row, status: u16, answered: &Value) {
    let expected_code = if row.surface == Surface::Transact {
        "TransactionCanceledException"
    } else {
        "ConditionalCheckFailedException"
    };
    assert_eq!(
        (status, error_code(answered)),
        (400, expected_code.to_string()),
        "case {case:?}, attempt {attempt}: expected the {} refusal, got {status} {answered:?}",
        row.surface.operation()
    );
    assert!(
        answered.get("ConsumedCapacity").is_none(),
        "case {case:?}, attempt {attempt}: a refusal reports no ConsumedCapacity in band, got \
         {answered:?}"
    );
}

/// How many bytes of payload the key HOLDS, or `None` when it holds no item — read
/// consistently, so it is what the refusals left behind.
async fn stored_payload(router: &MultiServiceRouter, key: &str) -> Option<usize> {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
    )
    .await;
    let stored = read.get("Item")?;
    Some(
        stored
            .get("payload")
            .and_then(|value| value["S"].as_str())
            .map_or(0, str::len),
    )
}

/// Create the one table the rows share, in on-demand mode so nothing here is throttled, and
/// wait for it to serve.
async fn create_table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
}

/// The `Sum` CloudWatch reports for a table's write-consumption series, over a window wide
/// enough to hold everything this suite published.
async fn series_sum(router: &MultiServiceRouter, table: &str) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = ok_cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": CONSUMED_WRITE,
            "Dimensions": [{"Name": "TableName", "Value": table}],
            "StartTime": now - 3600.0,
            "EndTime": now + 3600.0,
            "Period": 60,
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
