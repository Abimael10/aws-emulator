// SPDX-License-Identifier: CC0-1.0

//! The read capacity a PROVISIONED table is provisioned for bounds the reads it serves.
//!
//! RULE: "provisioned throughput is the maximum amount of capacity that an application can
//! consume from a table or index" per second (DynamoDB Developer Guide, "Read/write
//! capacity mode"), in reads as in writes, with up to 300 seconds of UNUSED capacity kept
//! for a burst ("Using burst capacity effectively") — so what a table has behind it at any
//! instant is the capacity that went unused since it was created, held at that ceiling. A
//! read past what that credit can cover is refused with
//! `ProvisionedThroughputExceededException` (HTTP 400, a modelled error of every data-plane
//! read) carrying the `ThrottlingReason` of the ceiling that bound, instead of being served;
//! a table on-demand (`PAY_PER_REQUEST`) is provisioned for no capacity to exceed, so no
//! read of one is refused for capacity.
//!
//! The rows span the surfaces the rule holds for — `GetItem`, `Query`, `Scan`,
//! `BatchGetItem`, `TransactGetItems` and the PartiQL `SELECT` — and each is driven against
//! BOTH capacity modes, because what the rule turns on is the capacity the table is
//! provisioned for and not the call.
//!
//! A throttled read changes nothing a later caller sees: once the budget has refilled, the
//! same read is served and reports the same item, which the last case here pins.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call, describe_when_active, emulator, emulator_with_cloudwatch, ok, ok_cloudwatch,
};

/// The read capacity the provisioned table under test is given: the smallest capacity a
/// table can carry, so what little credit it has accumulated is spent in a handful of reads.
const PROVISIONED_READ_UNITS: i64 = 1;

/// The bytes one read capacity unit covers for a STRONGLY consistent read: "one read
/// request unit represents one strongly consistent read request ... for an item up to 4 KB
/// in size" (DynamoDB Developer Guide, "Read/write capacity mode"). An item of
/// [`ITEM_BYTES`] therefore costs about 100 units, so a 1-RCU table could not hold more
/// than three such reads' worth of credit even after sitting unused for the whole
/// 300-second burst window.
const ITEM_BYTES: usize = 399 * 1024;

/// How many of those items a batch below asks for at once: four of them cost more than the
/// MOST credit a 1-RCU table can ever hold, so such a batch always has a key it has no
/// capacity for, whatever the table had banked when it arrived.
const KEYS_PAST_THE_CEILING: usize = 4;

/// How long the loops that wait for capacity to cover a read go on for: generous, because
/// what they wait for is a rate running, and a loaded machine is not a defect.
const DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long such a loop waits between attempts — the "exponential backoff" a retry is
/// documented to use, flattened. A retry that asked again at once would spend the credit the
/// raised capacity had just earned on part of the work and never accumulate enough for the
/// rest, which is a property of a caller hammering a table rather than of the table.
const DRAIN_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);

/// How many reads a loop drives before it gives up on ever being throttled: far past the
/// burst a 1-RCU table can hold, so a table that throttles nothing is reported as such
/// rather than looping.
const MOST_READS: usize = 24;

/// The table for a row, created in `mode` with an item big enough that each read of it
/// costs about a hundred read capacity units.
async fn seeded_table(router: &MultiServiceRouter, name: &str, mode: &str) {
    let mut create = json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": mode,
    });
    if mode == "PROVISIONED" {
        create["ProvisionedThroughput"] = json!({
            "ReadCapacityUnits": PROVISIONED_READ_UNITS,
            // Write capacity enough that seeding the item is never the refusal under test.
            "WriteCapacityUnits": 4000,
        });
    }
    ok(router, "CreateTable", create).await;
    describe_when_active(router, name, name).await;
    ok(
        router,
        "PutItem",
        json!({
            "TableName": name,
            "Item": {"pk": {"S": "k"}, "blob": {"S": "x".repeat(ITEM_BYTES)}},
        }),
    )
    .await;
}

/// The wire error code (`__type`) of a refusal.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The read each row drives, as a request body against `table`.
fn request(operation: &str, table: &str) -> Value {
    match operation {
        "GetItem" => json!({
            "TableName": table,
            "Key": {"pk": {"S": "k"}},
            "ConsistentRead": true,
        }),
        "Query" => json!({
            "TableName": table,
            "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "k"}},
            "ConsistentRead": true,
        }),
        "Scan" => json!({"TableName": table, "ConsistentRead": true}),
        "BatchGetItem" => json!({
            "RequestItems": {table: {"Keys": [{"pk": {"S": "k"}}], "ConsistentRead": true}},
        }),
        "TransactGetItems" => json!({
            "TransactItems": [{"Get": {"TableName": table, "Key": {"pk": {"S": "k"}}}}],
        }),
        "ExecuteStatement" => json!({
            "Statement": format!("SELECT * FROM \"{table}\" WHERE pk = 'k'"),
            "ConsistentRead": true,
        }),
        other => panic!("no request shape for {other}"),
    }
}

/// Drive `operation` against `table` until it is refused, or until [`MOST_READS`] reads
/// have been served. Answers the refusal body, or `None` when every read was served.
async fn read_until_throttled(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    table: &str,
) -> Option<Value> {
    for attempt in 0..MOST_READS {
        let (status, body) = call(router, operation, request(operation, table)).await;
        if status == 200 {
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: read {attempt} was refused with an unexpected status, got \
             {status} {body}"
        );
        return Some(body);
    }
    None
}

#[tokio::test]
async fn a_read_past_the_provisioned_read_capacity_is_refused() {
    let router = emulator();
    // Each row gets its OWN table: a budget is per table, and a row that spent one must
    // not decide the next row's.
    let operations = [
        "GetItem",
        "Query",
        "Scan",
        "BatchGetItem",
        "TransactGetItems",
        "ExecuteStatement",
    ];

    for operation in operations {
        let provisioned = format!("Provisioned{operation}");
        seeded_table(&router, &provisioned, "PROVISIONED").await;
        let case = format!("{operation} against a {PROVISIONED_READ_UNITS}-RCU table");
        let refused = read_until_throttled(&router, &case, operation, &provisioned)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a ProvisionedThroughputExceededException within \
                     {MOST_READS} reads of a {ITEM_BYTES}-byte item, every read was served"
                )
            });
        assert_eq!(
            error_code(&refused),
            "ProvisionedThroughputExceededException",
            "case {case:?}: expected the modelled throughput refusal, got {refused}"
        );
        let message = refused["message"]
            .as_str()
            .or_else(|| refused["Message"].as_str())
            .unwrap_or_default();
        assert!(
            message.contains("provisioned throughput for the table was exceeded"),
            "case {case:?}: expected the sentence DynamoDB states for a table past its \
             provisioning, got {refused}"
        );
        // "The throttling reason follows a specific format: ResourceType + OperationType +
        // LimitType" (`ThrottlingReason$reason`), naming the ARN of the table that
        // experienced the event.
        assert_eq!(
            refused["ThrottlingReasons"][0]["reason"],
            json!("TableReadProvisionedThroughputExceeded"),
            "case {case:?}: expected the read ceiling's own reason, got {refused}"
        );
        assert_eq!(
            refused["ThrottlingReasons"][0]["resource"],
            json!(format!(
                "arn:aws:dynamodb:us-east-1:000000000000:table/{provisioned}"
            )),
            "case {case:?}: expected the throttled table's ARN, got {refused}"
        );

        // The same load against an ON-DEMAND table is never refused for capacity: it is
        // provisioned for none to exceed.
        let on_demand = format!("OnDemand{operation}");
        seeded_table(&router, &on_demand, "PAY_PER_REQUEST").await;
        let case = format!("{operation} against an on-demand table");
        let throttled = read_until_throttled(&router, &case, operation, &on_demand).await;
        assert!(
            throttled.is_none(),
            "case {case:?}: an on-demand table has no read capacity to exceed, got {:?}",
            throttled
        );
    }
}

#[tokio::test]
async fn a_refused_transactional_read_debits_no_tables_budget() {
    // CloudWatch is mounted beside DynamoDB because what a request CONSUMED is published as
    // the table's own `ConsumedReadCapacityUnits` series — the only place a caller can read
    // what a table was debited, which is what this case is about.
    let router = emulator_with_cloudwatch();
    // Two provisioned tables: one with capacity to spare, one held to a single read unit
    // whose credit a read of its item cannot cover.
    seeded_table(&router, "TxFull", "PROVISIONED").await;
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "TxFull",
            "ProvisionedThroughput": {"ReadCapacityUnits": 4000, "WriteCapacityUnits": 4000},
        }),
    )
    .await;
    seeded_table(&router, "TxSpent", "PROVISIONED").await;
    // A second item in the table with capacity, so the read that follows the refusal asks
    // for more than the refused transaction would have taken from it.
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": "TxFull",
            "Item": {"pk": {"S": "k2"}, "blob": {"S": "x".repeat(ITEM_BYTES)}},
        }),
    )
    .await;
    read_until_throttled(&router, "spending TxSpent", "GetItem", "TxSpent")
        .await
        .expect("a 1-RCU table has no credit for a hundred-unit read within the bounded loop");
    let (full_before, spent_before) = (
        consumed_read_sum(&router, "TxFull").await,
        consumed_read_sum(&router, "TxSpent").await,
    );

    // One transaction over both: "the entire transaction is rejected" because one of its
    // tables has no capacity, and the refusal names that table.
    let (status, refused) = call(
        &router,
        "TransactGetItems",
        json!({
            "TransactItems": [
                {"Get": {"TableName": "TxFull", "Key": {"pk": {"S": "k"}}}},
                {"Get": {"TableName": "TxSpent", "Key": {"pk": {"S": "k"}}}},
            ],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ProvisionedThroughputExceededException"),
        "case a transaction one of whose tables is out of capacity: expected the modelled \
         throughput refusal, got {status} {refused}"
    );
    assert_eq!(
        refused["ThrottlingReasons"][0]["resource"],
        json!("arn:aws:dynamodb:us-east-1:000000000000:table/TxSpent"),
        "case a transaction one of whose tables is out of capacity: the refusal names the \
         table that had none, got {refused}"
    );
    // Rejected WHOLE: the refusal carries no part of what the transaction would have read,
    // not even the leg whose table had the capacity for it.
    assert!(
        refused.get("Responses").is_none(),
        "case a transaction one of whose tables is out of capacity: a rejected transaction \
         serves no leg at all, got {refused}"
    );

    // And it DEBITED NEITHER TABLE: a transaction that did nothing consumed nothing
    // anywhere, which the capacity each table reports as consumed is what shows — the
    // figures stand exactly where they stood before the refusal.
    let (full_after, spent_after) = (
        consumed_read_sum(&router, "TxFull").await,
        consumed_read_sum(&router, "TxSpent").await,
    );
    assert_eq!(
        (full_after, spent_after),
        (full_before, spent_before),
        "case the tables of a refused transaction: neither is debited by a transaction that \
         read nothing, TxFull moved from {full_before} to {full_after} and TxSpent from \
         {spent_before} to {spent_after}"
    );

    // And the table that did have the capacity is untouched by the refusal: it still reports
    // both the items it holds.
    let (status, scanned) = call(
        &router,
        "Scan",
        json!({"TableName": "TxFull", "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        (status, scanned["Count"].clone()),
        (200, json!(2)),
        "case the table the refused transaction did not read: it still reports its items, \
         got {status} {scanned}"
    );
}

/// The `Sum` of the read capacity `table` reports as consumed, over a window wide enough to
/// hold everything this suite spent on it.
///
/// What a request consumed is published as that table's own `AWS/DynamoDB`
/// `ConsumedReadCapacityUnits` series, so this is what a client reads to see whether a
/// request was charged at all.
async fn consumed_read_sum(router: &MultiServiceRouter, table: &str) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs_f64();
    let answered = ok_cloudwatch(
        router,
        "GetMetricStatistics",
        json!({
            "Namespace": "AWS/DynamoDB",
            "MetricName": "ConsumedReadCapacityUnits",
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

#[tokio::test]
async fn a_batch_get_hands_back_exactly_the_keys_it_had_no_capacity_for() {
    let router = emulator();
    seeded_table(&router, "Residue", "PROVISIONED").await;
    // A tiny item any credit at all covers, read FIRST, plus four ~100-unit items — which
    // together cost more than the MOST credit a 1-RCU table can ever hold. So a batch over
    // all five always reads some of its keys and always has keys left over, whatever the
    // table had banked when it arrived.
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": "Residue",
            "Item": {"pk": {"S": "tiny"}, "blob": {"S": "x"}},
        }),
    )
    .await;
    let big: Vec<String> = std::iter::once("k".to_string())
        .chain((2..=KEYS_PAST_THE_CEILING).map(|slot| format!("k{slot}")))
        .collect();
    for key in big.iter().skip(1) {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": "Residue",
                "Item": {"pk": {"S": key}, "blob": {"S": "x".repeat(ITEM_BYTES)}},
            }),
        )
        .await;
    }

    let requested: Vec<String> = std::iter::once("tiny".to_string())
        .chain(big.iter().cloned())
        .collect();
    let keys: Vec<Value> = requested
        .iter()
        .map(|key| json!({"pk": {"S": key}}))
        .collect();
    let requested_set: std::collections::BTreeSet<String> = requested.iter().cloned().collect();
    let requested = json!({
        "RequestItems": {"Residue": {"Keys": keys, "ConsistentRead": true}},
    });
    let answered = ok(&router, "BatchGetItem", requested.clone()).await;
    // "A map of tables and their respective keys that were not processed": the keys the
    // table had no capacity left for, in the form they were sent, so the map can be
    // handed straight to a later BatchGetItem.
    let residue = answered["UnprocessedKeys"]["Residue"]["Keys"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let read = answered["Responses"]["Residue"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !read.is_empty() && !residue.is_empty(),
        "case a batch past the credit the table can hold: expected the keys that fit \
         answered and the rest handed back, got {answered}"
    );
    let served: std::collections::BTreeSet<String> = read
        .iter()
        .map(|item| item["pk"]["S"].as_str().unwrap_or_default().to_string())
        .collect();
    let held: std::collections::BTreeSet<String> = residue
        .iter()
        .map(|key| {
            assert_eq!(
                key.as_object().map(serde_json::Map::len),
                Some(1),
                "case a batch past the credit the table can hold: a handed-back key keeps \
                 the spelling it was sent in, got {key}"
            );
            key["pk"]["S"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    // The keys READ and the keys handed back partition the requested keys exactly: none is
    // both, and none is neither, which is what lets a client retry exactly the residue.
    assert!(
        served.is_disjoint(&held),
        "case a batch past the credit the table can hold: no key is both read and handed \
         back, {served:?} and {held:?} overlap"
    );
    let accounted: std::collections::BTreeSet<String> =
        served.union(&held).cloned().collect();
    assert_eq!(
        accounted, requested_set,
        "case a batch past the credit the table can hold: every requested key is either \
         read or handed back, got {accounted:?} for {requested_set:?}"
    );
    assert!(
        served.contains("tiny"),
        "case a batch past the credit the table can hold: the key any credit covers is read \
         before the ones it does not, got {served:?}"
    );
    assert_eq!(
        answered["UnprocessedKeys"]["Residue"]["ConsistentRead"],
        json!(true),
        "case a batch past the credit the table can hold: the residue carries the read \
         options it was sent with, got {answered}"
    );

    // The residue DRAINS: resubmitted once the table has the capacity, it reports the
    // items and hands nothing further back — which is what lets a client's retry loop
    // terminate.
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "Residue",
            "ProvisionedThroughput": {"ReadCapacityUnits": 4000, "WriteCapacityUnits": 4000},
        }),
    )
    .await;
    // Capacity is a RATE, so the raised capacity has to run for a moment before its credit
    // covers the whole residue: the loop retries the residue exactly as the doc tells a
    // client to, until nothing is held back.
    let deadline = std::time::Instant::now() + DRAIN_DEADLINE;
    let drained = loop {
        tokio::time::sleep(DRAIN_PAUSE).await;
        let (status, answer) = call(
            &router,
            "BatchGetItem",
            json!({"RequestItems": answered["UnprocessedKeys"].clone()}),
        )
        .await;
        let left = answer["UnprocessedKeys"]
            .as_object()
            .map(serde_json::Map::len)
            .unwrap_or_default();
        if status == 200 && left == 0 {
            break answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case the residue resubmitted: the retry loop must drain against capacity that \
             covers it, got {status} with {answer}"
        );
    };
    let drained_keys: std::collections::BTreeSet<String> = drained["Responses"]["Residue"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| item["pk"]["S"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        drained_keys, held,
        "case the residue resubmitted: expected exactly the keys the first call handed back, \
         got {drained}"
    );
}

#[tokio::test]
async fn a_batch_statement_read_reports_the_throttle_in_its_own_slot() {
    let router = emulator();
    seeded_table(&router, "SlotThrottle", "PROVISIONED").await;

    // Spend the burst through the single-statement surface, so what is left to assert is
    // only how the BATCH surface reports a read the capacity cannot cover.
    read_until_throttled(&router, "spending the burst", "GetItem", "SlotThrottle")
        .await
        .expect("the burst of a 1-RCU table is spent within the bounded loop");

    // "The entire batch must consist of either read statements or write statements": one
    // read statement, which the table has no capacity left for. A batch reports a
    // per-statement refusal in that statement's own slot, with HTTP 200 for the call.
    let (status, answered) = call(
        &router,
        "BatchExecuteStatement",
        json!({
            "Statements": [{
                "Statement": "SELECT * FROM \"SlotThrottle\" WHERE pk = 'k'",
                "ConsistentRead": true,
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a throttled batch statement: the CALL is answered, got {status} {answered}"
    );
    assert_eq!(
        answered["Responses"][0]["Error"]["Code"],
        json!("ProvisionedThroughputExceeded"),
        "case a throttled batch statement: expected the throttling code in the slot, got \
         {answered}"
    );
    // A PartiQL read TRANSACTION is bounded by the same capacity, and like
    // `TransactGetItems` it is refused whole.
    let (status, refused) = call(
        &router,
        "ExecuteTransaction",
        json!({
            "TransactStatements": [{
                "Statement": "SELECT * FROM \"SlotThrottle\" WHERE pk = 'k'",
            }],
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ProvisionedThroughputExceededException"),
        "case a read transaction against a table out of capacity: expected the modelled \
         throughput refusal for the whole call, got {status} {refused}"
    );
}

#[tokio::test]
async fn a_throttled_read_changes_nothing_a_later_read_sees() {
    let router = emulator();
    seeded_table(&router, "Refills", "PROVISIONED").await;

    // Spend the burst.
    let refused = read_until_throttled(&router, "spending the burst", "GetItem", "Refills")
        .await
        .expect("the burst of a 1-RCU table is spent within the bounded loop");
    assert_eq!(
        error_code(&refused),
        "ProvisionedThroughputExceededException",
        "case spending the burst: expected the modelled throughput refusal, got {refused}"
    );

    // A read the budget cannot cover takes nothing from it, and changes nothing the table
    // HOLDS. Raising the capacity is what lets the same read through — the budget is
    // measured against the capacity in force, not the one the table was created with — and
    // what it then reports is the item that was there all along.
    ok(
        &router,
        "UpdateTable",
        json!({
            "TableName": "Refills",
            "ProvisionedThroughput": {"ReadCapacityUnits": 4000, "WriteCapacityUnits": 4000},
        }),
    )
    .await;
    // Capacity is a RATE, so the raised capacity has to run for a moment before its credit
    // covers a read of this size: the read is retried until it is served rather than waited
    // on for a fixed time.
    let deadline = std::time::Instant::now() + DRAIN_DEADLINE;
    let (status, body) = loop {
        tokio::time::sleep(DRAIN_PAUSE).await;
        let answered = call(&router, "GetItem", request("GetItem", "Refills")).await;
        if answered.0 == 200 {
            break answered;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case capacity raised: the read the new capacity covers must be served, got {} \
             with {}",
            answered.0,
            answered.1
        );
    };
    assert_eq!(
        (status, body["Item"]["pk"].clone()),
        (200, json!({"S": "k"})),
        "case capacity raised: the read the new capacity covers reports the item the \
         throttled read did not change, got {status} {body}"
    );
}
