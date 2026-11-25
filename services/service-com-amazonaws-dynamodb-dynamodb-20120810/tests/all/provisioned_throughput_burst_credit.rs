// SPDX-License-Identifier: CC0-1.0

//! What a PROVISIONED table's burst credit IS, and what a request past it is answered with.
//!
//! RULE: the credit behind a provisioned table is the capacity that went UNUSED — "DynamoDB
//! currently retains up to 300 seconds (5 minutes) of unused read and write capacity"
//! (DynamoDB Developer Guide, "Using burst capacity effectively") — so a table accumulates
//! it at the rate it is provisioned for, from the moment it exists, and holds at most that
//! window's worth. A table therefore cannot absorb a run of requests asking for more units
//! than its whole window could ever hold: the ones the credit does not cover are refused
//! with `ProvisionedThroughputExceededException` (HTTP 400) carrying the fixed sentence the
//! Developer Guide states for that quota and the `ThrottlingReason` of the ceiling that
//! bound, and they store nothing; `BatchWriteItem` hands the operations it did not admit
//! back in `UnprocessedItems` with HTTP 200 instead. Every request a ceiling turned down —
//! refused outright or served with part of its work handed back — counts ONE into that
//! table's `AWS/DynamoDB` `ThrottledRequests` series under the `{TableName, Operation}`
//! pair: "ThrottledRequests is incremented by one if any event within a request exceeds a
//! provisioned throughput limit" (Amazon CloudWatch metrics for DynamoDB). A table on-demand
//! (`PAY_PER_REQUEST`) is provisioned for no capacity to exceed, so the same run is served
//! whole and counts nothing.
//!
//! The rows are a TABLE spanning what the rule generalises over: the surface the request is
//! made with (the three item writes, the batch write, and a read), the capacity mode, the
//! units the table is provisioned for, the bytes of each item — and so the units one request
//! costs — and how many requests the run makes. Each run asks for more units than the
//! documented window could hold at the row's rate, which is what makes the answer the same
//! however long the table happened to exist before it arrived.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call, describe_when_active, emulator_with_cloudwatch, error_code, message, ok, ok_cloudwatch,
    REGION,
};

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the requests a throughput ceiling turned down.
const THROTTLED_REQUESTS: &str = "ThrottledRequests";

/// The fixed sentence a request past the throughput its table is provisioned for is refused
/// with, as the Developer Guide states it for that quota: the whole sentence pair, with no
/// value from the request or from state in it.
const THROUGHPUT_EXCEEDED: &str = "The level of configured provisioned throughput for the \
                                   table was exceeded. Consider increasing your \
                                   provisioning level with the UpdateTable API.";

/// How many seconds of its rate a provisioned budget can hold at most ("DynamoDB currently
/// retains up to 300 seconds (5 minutes) of unused read and write capacity").
const BURST_SECONDS: usize = 300;

/// One write capacity unit covers a write of up to 1 KB, and one read capacity unit a
/// strongly consistent read of up to 4 KB (Developer Guide, "Read/write capacity mode").
const WRITE_UNIT_BYTES: usize = 1024;

/// How many requests one `BatchWriteItem` carries at most: `BatchWriteItem$RequestItems` is
/// `@length(1..25)`.
const MAX_BATCH_REQUESTS: usize = 25;

/// One byte of payload beside the key: enough to make an item with a non-key attribute,
/// which still costs the one-unit minimum a write is never charged less than.
const ONE_UNIT_PAYLOAD: usize = 1;

/// One row: the surface a run is made with, the capacity behind the table, the size of each
/// item and the length of the run.
struct Row {
    case: &'static str,
    /// The operation every request of the run is made with.
    operation: &'static str,
    /// `None` for an on-demand table, which is provisioned for no capacity to exceed;
    /// otherwise the units the CEILING under test is provisioned for (write units for a
    /// write row, read units for a read row). The other half is ample, so what the row
    /// measures is the one ceiling it names.
    units: Option<i64>,
    /// The bytes of the one non-key attribute of each item, which decides how many units one
    /// request costs.
    payload: usize,
    /// How many requests the run makes.
    requests: usize,
    /// Whether the rule guarantees part of the run is SERVED: one request of this row costs
    /// the one-unit minimum, which the credit a table has accumulated by the time it is
    /// `ACTIVE` covers. A row whose single request costs more than that may meet a table
    /// whose credit covers none of them, so only the turning-down is asserted for it.
    partly_served: bool,
}

/// Whether a row's requests spend READ capacity; everything else spends the other half.
fn reads(operation: &str) -> bool {
    operation == "GetItem"
}

/// The `ThrottlingReason$reason` the ceiling of that half states: "the throttling reason
/// follows a specific format: ResourceType + OperationType + LimitType".
fn expected_reason(operation: &str) -> &'static str {
    if reads(operation) {
        "TableReadProvisionedThroughputExceeded"
    } else {
        "TableWriteProvisionedThroughputExceeded"
    }
}

/// The rows: every surface the rule holds for, both capacity modes, and item sizes from the
/// one-unit minimum to a nine-unit item.
///
/// Each provisioned run asks for more than [`BURST_SECONDS`] of the row's rate — 400 one-unit
/// writes, 40 nine-unit writes, 400 one-unit reads — so the credit a table could ever hold
/// cannot cover the whole run, whatever it had accumulated when the run arrived.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one write unit, four hundred one-unit PutItems",
            operation: "PutItem",
            units: Some(1),
            payload: ONE_UNIT_PAYLOAD,
            requests: 400,
            partly_served: true,
        },
        Row {
            case: "one write unit, forty nine-unit PutItems",
            operation: "PutItem",
            units: Some(1),
            payload: 8 * WRITE_UNIT_BYTES,
            requests: 40,
            partly_served: false,
        },
        Row {
            case: "two write units, eight hundred one-unit UpdateItems",
            operation: "UpdateItem",
            units: Some(2),
            payload: ONE_UNIT_PAYLOAD,
            requests: 800,
            partly_served: true,
        },
        Row {
            case: "one write unit, four hundred one-unit DeleteItems",
            operation: "DeleteItem",
            units: Some(1),
            payload: ONE_UNIT_PAYLOAD,
            requests: 400,
            partly_served: true,
        },
        Row {
            case: "one write unit, sixteen batches of twenty-five one-unit writes",
            operation: "BatchWriteItem",
            units: Some(1),
            payload: ONE_UNIT_PAYLOAD,
            requests: 16,
            partly_served: false,
        },
        Row {
            case: "one read unit, four hundred one-unit GetItems",
            operation: "GetItem",
            units: Some(1),
            payload: ONE_UNIT_PAYLOAD,
            requests: 400,
            partly_served: true,
        },
        Row {
            case: "on-demand, four hundred one-unit PutItems",
            operation: "PutItem",
            units: None,
            payload: ONE_UNIT_PAYLOAD,
            requests: 400,
            partly_served: true,
        },
        Row {
            case: "on-demand, sixteen batches of twenty-five one-unit writes",
            operation: "BatchWriteItem",
            units: None,
            payload: ONE_UNIT_PAYLOAD,
            requests: 16,
            partly_served: true,
        },
    ]
}

/// Ample capacity for the half of the throughput a row does not measure, so the only ceiling
/// a row can hit is the one it names.
const AMPLE_UNITS: i64 = 4000;

/// Create the table a row runs against and wait out its `CREATING` span.
async fn table_for(router: &MultiServiceRouter, name: &str, row: &Row) {
    let mut body = json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    });
    match row.units {
        Some(units) => {
            let (read, write) = if reads(row.operation) {
                (units, AMPLE_UNITS)
            } else {
                (AMPLE_UNITS, units)
            };
            body["BillingMode"] = json!("PROVISIONED");
            body["ProvisionedThroughput"] =
                json!({"ReadCapacityUnits": read, "WriteCapacityUnits": write});
        }
        None => body["BillingMode"] = json!("PAY_PER_REQUEST"),
    }
    ok(router, "CreateTable", body).await;
    describe_when_active(router, name, name).await;
}

/// The request one run makes at `slot`, against `table`.
fn request_at(row: &Row, table: &str, slot: usize) -> Value {
    let key = format!("k{slot}");
    let payload = "x".repeat(row.payload);
    match row.operation {
        "PutItem" => json!({"TableName": table,
                            "Item": {"pk": {"S": key}, "payload": {"S": payload}}}),
        "UpdateItem" => json!({"TableName": table, "Key": {"pk": {"S": key}},
                               "UpdateExpression": "SET payload = :p",
                               "ExpressionAttributeValues": {":p": {"S": payload}}}),
        "DeleteItem" => json!({"TableName": table, "Key": {"pk": {"S": key}}}),
        // The read row reads ONE item, so what every request of the run costs is the same.
        "GetItem" => json!({"TableName": table, "Key": {"pk": {"S": "read"}},
                            "ConsistentRead": true}),
        "BatchWriteItem" => {
            let requests: Vec<Value> = (0..MAX_BATCH_REQUESTS)
                .map(|within| {
                    json!({"PutRequest": {"Item": {
                        "pk": {"S": format!("k{slot}-{within}")},
                        "payload": {"S": payload}}}})
                })
                .collect();
            json!({"RequestItems": {table: requests}})
        }
        other => panic!("this suite makes no {other} request"),
    }
}

/// Whether `table` holds an item at `key`, read back through a strongly consistent `GetItem`.
async fn stored(router: &MultiServiceRouter, table: &str, key: &str) -> bool {
    let answered = ok(
        router,
        "GetItem",
        json!({"TableName": table, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
    )
    .await;
    answered.get("Item").is_some()
}

/// The `Sum` CloudWatch reports for `table`'s `ThrottledRequests` series under `dimensions`,
/// over a window wide enough to hold everything a row published.
async fn throttled_sum(router: &MultiServiceRouter, dimensions: Value) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = ok_cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": NAMESPACE,
            "MetricName": THROTTLED_REQUESTS,
            "Dimensions": dimensions,
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

/// The `{TableName, Operation}` pair a throttled request is counted under.
fn pair(table: &str, operation: &str) -> Value {
    json!([{"Name": "TableName", "Value": table},
           {"Name": "Operation", "Value": operation}])
}

#[tokio::test]
async fn a_run_past_the_credit_a_provisioned_table_can_hold_is_throttled_and_counted() {
    for row in rows() {
        let case = row.case;
        let table = "burst";
        let router = emulator_with_cloudwatch();
        table_for(&router, table, &row).await;
        // The read row's one item, written against the ample write capacity beside the read
        // ceiling under test.
        if reads(row.operation) {
            ok(
                &router,
                "PutItem",
                json!({"TableName": table, "Item": {"pk": {"S": "read"}}}),
            )
            .await;
        }

        let mut served = 0usize;
        let mut turned_down = 0usize;
        let mut first_refusal: Option<Value> = None;
        for slot in 0..row.requests {
            let (status, answered) = call(&router, row.operation, request_at(&row, table, slot))
                .await;
            if status == 200 {
                // A batch is SERVED with the operations it did not admit handed back: "the
                // failed operations are returned in the UnprocessedItems response parameter",
                // which is a request one ceiling turned an event of down just the same.
                let residue = answered["UnprocessedItems"][table]
                    .as_array()
                    .map(Vec::len)
                    .unwrap_or_default();
                if residue > 0 {
                    turned_down += 1;
                } else {
                    served += 1;
                }
                continue;
            }
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (
                    400,
                    "ProvisionedThroughputExceededException",
                    THROUGHPUT_EXCEEDED.to_string()
                ),
                "case {case:?}, request {slot}: a request past the provisioned throughput is \
                 refused with the provisioning sentence, got {status} with {answered}"
            );
            assert_eq!(
                answered["ThrottlingReasons"],
                json!([{"reason": expected_reason(row.operation),
                        "resource":
                            format!("arn:aws:dynamodb:{REGION}:000000000000:table/{table}")}]),
                "case {case:?}, request {slot}: the refusal names the ceiling that bound and \
                 the table it bound on, got {answered}"
            );
            turned_down += 1;
            if first_refusal.is_none() {
                first_refusal = Some(json!(format!("k{slot}")));
            }
        }

        let counted = throttled_sum(&router, pair(table, row.operation)).await;
        let Some(units) = row.units else {
            // On-demand: nothing to exceed, so the same run is served whole and the series
            // has no datapoint at all.
            assert_eq!(
                (turned_down, served, counted),
                (0, row.requests, 0.0),
                "case {case:?}: a table provisioned for no capacity throttles nothing, {} of \
                 {} requests were turned down and {counted} counted",
                turned_down,
                row.requests
            );
            continue;
        };

        // The run asks for more units than this rate's window could ever hold, so some of it
        // is turned down whatever the table had accumulated.
        let ceiling = usize::try_from(units).unwrap_or(1) * BURST_SECONDS;
        assert!(
            turned_down > 0,
            "case {case:?}: a run asking for more than the {ceiling} units this rate's \
             window can hold must be turned down in part, all {served} of its requests were \
             served"
        );
        // And a run whose single request costs the one-unit minimum is served in part: a
        // table that has existed long enough to be ACTIVE has accumulated that much.
        assert!(
            served > 0 || !row.partly_served,
            "case {case:?}: a request costing the one-unit minimum is covered by the credit \
             behind an ACTIVE table, yet all {turned_down} requests were turned down"
        );
        // Every request a ceiling turned down counts exactly ONE into the series — "it is
        // incremented by one if any event within a request exceeds a provisioned throughput
        // limit" — so the figure is a count of REQUESTS and not of the writes inside them.
        assert!(
            (counted - turned_down as f64).abs() < f64::EPSILON,
            "case {case:?}: the ThrottledRequests series counts one per request a ceiling \
             turned down, {turned_down} were turned down and it reports {counted}"
        );
        // Under that pair and under NO other: a series "is uniquely defined by a name, a
        // namespace, and zero or more dimensions", so the table's name alone names a series
        // DynamoDB does not have, and so does the pair of another operation.
        let table_only = throttled_sum(&router, json!([{"Name": "TableName", "Value": table}]))
            .await;
        let other_operation = throttled_sum(&router, pair(table, "TransactWriteItems")).await;
        assert_eq!(
            (table_only, other_operation),
            (0.0, 0.0),
            "case {case:?}: the family is published under the table-and-operation pair and \
             under nothing else, got {table_only} under the table alone and \
             {other_operation} under another operation"
        );
        // A refused WRITE stored nothing: the item it addressed is not there.
        if let Some(key) = first_refusal.as_ref().and_then(Value::as_str) {
            if !reads(row.operation) && row.operation != "BatchWriteItem" {
                assert!(
                    !stored(&router, table, key).await,
                    "case {case:?}: a refused write leaves nothing behind, {key} is stored"
                );
            }
        }
    }
}

/// The bytes of an item used to fill a batch read's answer: just inside the documented
/// maximum item size, so a handful of them carry a response to its own ceiling.
const BIG_ITEM_BYTES: usize = 399 * 1024;

/// How many such items a batch read asks for in the row below: enough that they cannot all
/// fit the documented 16 MB response ceiling, so the call must defer some of them.
const ITEMS_PAST_THE_RESPONSE_CEILING: usize = 45;

/// How many keys a 1-RCU table has no read capacity for below: four ~100-unit reads cost more
/// than the MOST credit such a table can ever hold, so one of them is always handed back.
const KEYS_PAST_THE_READ_CREDIT: usize = 4;

#[tokio::test]
async fn only_a_batch_read_a_ceiling_bound_counts_a_throttled_request() {
    // A batch read hands keys back for TWO reasons, and only one of them is a throttled
    // event: the capacity its table had, and the size of the answer it may carry
    // ("BatchGetItem returns a partial result if the response size limit is exceeded ... the
    // operation returns a value for UnprocessedKeys"). The two arms below drive one each,
    // against the same metric.
    let router = emulator_with_cloudwatch();

    // Arm one: a table held to one read unit, asked for more ~100-unit reads than its credit
    // could ever cover, in an answer nowhere near the response ceiling.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "bound",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 1, "WriteCapacityUnits": AMPLE_UNITS},
        }),
    )
    .await;
    describe_when_active(&router, "bound", "bound").await;
    for slot in 0..KEYS_PAST_THE_READ_CREDIT {
        ok(
            &router,
            "PutItem",
            json!({"TableName": "bound", "Item": {
                "pk": {"S": format!("k{slot}")},
                "pad": {"S": "x".repeat(BIG_ITEM_BYTES)}}}),
        )
        .await;
    }
    // A tiny item any credit at all covers, read FIRST, so the call SERVES something and
    // hands the rest back rather than being refused outright for having done nothing.
    ok(
        &router,
        "PutItem",
        json!({"TableName": "bound", "Item": {"pk": {"S": "tiny"}, "pad": {"S": "x"}}}),
    )
    .await;
    let keys: Vec<Value> = std::iter::once(json!({"pk": {"S": "tiny"}}))
        .chain(
            (0..KEYS_PAST_THE_READ_CREDIT).map(|slot| json!({"pk": {"S": format!("k{slot}")}})),
        )
        .collect();
    let answered = ok(
        &router,
        "BatchGetItem",
        json!({"RequestItems": {"bound": {"Keys": keys, "ConsistentRead": true}}}),
    )
    .await;
    assert!(
        !answered["UnprocessedKeys"]["bound"]["Keys"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true),
        "case a batch read past the credit its table can hold: expected keys handed back, got \
         {answered}"
    );
    let counted = throttled_sum(&router, pair("bound", "BatchGetItem")).await;
    assert!(
        (counted - 1.0).abs() < f64::EPSILON,
        "case a batch read past the credit its table can hold: the ceiling turned an event of \
         this one request down, so the series counts one, got {counted}"
    );

    // Arm two: an ON-DEMAND table — provisioned for no capacity to exceed, so no ceiling can
    // turn anything of this request down — asked for more items than one answer may carry.
    // The keys it defers are the response budget's work, and no throttled event at all.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "roomy",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "roomy", "roomy").await;
    for slot in 0..ITEMS_PAST_THE_RESPONSE_CEILING {
        ok(
            &router,
            "PutItem",
            json!({"TableName": "roomy", "Item": {
                "pk": {"S": format!("k{slot}")},
                "pad": {"S": "x".repeat(BIG_ITEM_BYTES)}}}),
        )
        .await;
    }
    let keys: Vec<Value> = (0..ITEMS_PAST_THE_RESPONSE_CEILING)
        .map(|slot| json!({"pk": {"S": format!("k{slot}")}}))
        .collect();
    let answered = ok(
        &router,
        "BatchGetItem",
        json!({"RequestItems": {"roomy": {"Keys": keys, "ConsistentRead": true}}}),
    )
    .await;
    assert!(
        !answered["UnprocessedKeys"]["roomy"]["Keys"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(true),
        "case a batch read past what one answer may carry: expected keys deferred for the \
         response ceiling, got {} items and no residue",
        answered["Responses"]["roomy"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default()
    );
    let counted = throttled_sum(&router, pair("roomy", "BatchGetItem")).await;
    assert_eq!(
        counted, 0.0,
        "case a batch read past what one answer may carry: a key the RESPONSE's size deferred \
         is no throttled event, so the series counts nothing, got {counted}"
    );
}
