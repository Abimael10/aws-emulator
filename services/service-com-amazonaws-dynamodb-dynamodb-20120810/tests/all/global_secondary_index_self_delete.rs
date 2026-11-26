// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateTable` that DELETES one of a table's global secondary indexes leaves
//! behind, driven end to end — including a delete of the index whose build is still
//! running.
//!
//! RULE under test: an accepted `Delete` of a global secondary index is ASYNCHRONOUS work
//! on the table — the description the call answers still carries that index, reported
//! "`DELETING` - The index is being deleted"
//! (`GlobalSecondaryIndexDescription$IndexStatus`), the index disappears from the table
//! once the removal that call owed falls due, and the attribute definitions no remaining key
//! schema uses go with it. From the instant the delete is accepted the index serves nothing:
//! a read naming it is refused as a name the table does not have. The index's own STATUS
//! when the delete arrives does not change any of that: a `Delete` naming the index that is
//! still being BUILT cancels that build, which is how a caller undoes an index it
//! mis-specified, while any OTHER index change asked of a table that is still building one
//! is past the one-online-index limit.
//!
//! The rows differ along what the rule generalises over: how many items the index had to
//! read when it was added (none, one, and enough that the build outlives the table's own
//! update), whether the table carries another index that stays, how many attributes the
//! deleted index keyed on (one and two), and the status the index was in when the delete
//! arrived (`CREATING` and `ACTIVE`).
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<Operation>`) through the same `MultiServiceRouter` the
//! emulator's front doors use, and nothing is waited for by the clock: the suite polls
//! `DescribeTable` for the condition it needs ([`crate::emulator::table_when`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, index_named, message, ok, table_when, REGION};
use crate::table_items::create_active_table;

/// The partition key every table below is created with.
const KEY: &str = "pk";

/// The index whose delete is under test.
const BUILDING: &str = "idx-a";

/// The index a second change in the same span would add.
const SECOND: &str = "idx-b";

/// AWS's fixed sentence for the limit on how many of a table's indexes may be created or
/// deleted at once.
const ONLINE_INDEX_LIMIT_MESSAGE: &str =
    "Subscriber limit exceeded: Only 1 online index can be created or deleted simultaneously \
     per table";

/// The attributes a row's items carry beside the table key, one per key element of the
/// index it adds.
const INDEX_ATTRIBUTES: [&str; 2] = ["g1", "g2"];

/// One row: how many items the table holds, and how many attributes the added index keys on.
struct Row {
    case: &'static str,
    table: &'static str,
    items: usize,
    key_elements: usize,
}

/// The rows, over the item counts and key shapes the rule holds across.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an index over no items at all",
            table: "GsiSelfDeleteEmpty",
            items: 0,
            key_elements: 1,
        },
        Row {
            case: "an index over one item",
            table: "GsiSelfDeleteOne",
            items: 1,
            key_elements: 1,
        },
        Row {
            case: "an index over enough items that the build outlives the table's update",
            table: "GsiSelfDeleteFilled",
            items: 40,
            key_elements: 1,
        },
        Row {
            case: "an index keyed on two attributes",
            table: "GsiSelfDeleteComposite",
            items: 40,
            key_elements: 2,
        },
    ]
}

/// The `AttributeDefinitions` entries the index of `key_elements` elements needs.
fn definitions(key_elements: usize) -> Vec<Value> {
    INDEX_ATTRIBUTES[..key_elements]
        .iter()
        .map(|attribute| json!({"AttributeName": attribute, "AttributeType": "S"}))
        .collect()
}

/// The key schema of that index: the first attribute partitions it, the second sorts it.
fn index_key_schema(key_elements: usize) -> Vec<Value> {
    INDEX_ATTRIBUTES[..key_elements]
        .iter()
        .enumerate()
        .map(|(at, attribute)| {
            json!({
                "AttributeName": attribute,
                "KeyType": if at == 0 { "HASH" } else { "RANGE" },
            })
        })
        .collect()
}

/// The `UpdateTable` body that ADDS `index` to `table`, keyed on `key_elements` attributes.
fn add_index(table: &str, index: &str, key_elements: usize) -> Value {
    json!({
        "TableName": table,
        "AttributeDefinitions": definitions(key_elements),
        "GlobalSecondaryIndexUpdates": [{"Create": {
            "IndexName": index,
            "KeySchema": index_key_schema(key_elements),
            "Projection": {"ProjectionType": "ALL"},
        }}],
    })
}

/// The `UpdateTable` body that DELETES `index` from `table`.
fn delete_index(table: &str, index: &str) -> Value {
    json!({
        "TableName": table,
        "GlobalSecondaryIndexUpdates": [{"Delete": {"IndexName": index}}],
    })
}

/// Create `table` and write `items` items, each carrying every attribute an index below
/// keys on.
async fn seeded(router: &MultiServiceRouter, table: &str, items: usize) {
    create_active_table(router, table, &[(KEY, "HASH", "S")]).await;
    for item in 0..items {
        let mut written = json!({KEY: {"S": format!("k{item}")}});
        for attribute in INDEX_ATTRIBUTES {
            written[attribute] = json!({"S": format!("{attribute}-{item}")});
        }
        ok(
            router,
            "PutItem",
            json!({"TableName": table, "Item": written}),
        )
        .await;
    }
}

/// The attribute names the table's description declares, in the order it declares them.
fn declared_attributes(description: &Value) -> Vec<String> {
    description["AttributeDefinitions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|definition| definition["AttributeName"].as_str().map(str::to_string))
        .collect()
}

/// The index names the table's description reports.
fn reported_indexes(description: &Value) -> Vec<String> {
    description["GlobalSecondaryIndexes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|index| index["IndexName"].as_str().map(str::to_string))
        .collect()
}

/// Poll `DescribeTable` until the table reports itself `ACTIVE` carrying no global
/// secondary index at all — the condition a client waiting out an index deletion waits for.
async fn when_no_index_remains(router: &MultiServiceRouter, case: &str, table: &str) -> Value {
    table_when(router, case, REGION, table, |described| {
        described["TableStatus"] == json!("ACTIVE") && reported_indexes(described).is_empty()
    })
    .await
}

#[tokio::test]
async fn deleting_the_index_being_built_cancels_that_build() {
    let router = emulator();

    for row in rows() {
        let Row {
            case,
            table,
            items,
            key_elements,
        } = row;
        seeded(&router, table, items).await;

        // The index is added, and the table reports it as the build it now is.
        let added = ok(&router, "UpdateTable", add_index(table, BUILDING, key_elements)).await;
        assert_eq!(
            index_named(&added["TableDescription"], BUILDING)["IndexStatus"],
            json!("CREATING"),
            "case {case:?}: the added index must be reported CREATING, got {added}"
        );

        // A SECOND index, while that one is still building, is past the one-online-index
        // limit — the arm that is not a cancellation.
        let (status, refused) = call(
            &router,
            "UpdateTable",
            add_index(table, SECOND, key_elements),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str(), message(&refused)),
            (
                400,
                "LimitExceededException",
                ONLINE_INDEX_LIMIT_MESSAGE.to_string()
            ),
            "case {case:?}: a second index build must be refused as the limit it exceeds, got \
             {status} with {refused}"
        );
        let described = ok(&router, "DescribeTable", json!({"TableName": table})).await;
        assert_eq!(
            reported_indexes(&described["Table"]),
            vec![BUILDING.to_string()],
            "case {case:?}: the refused create must leave the index set as it was, got \
             {described}"
        );

        // The delete of the very index being built is SERVED, and reports it on its way out.
        let answered = ok(&router, "UpdateTable", delete_index(table, BUILDING)).await;
        assert_eq!(
            reported_indexes(&answered["TableDescription"]),
            vec![BUILDING.to_string()],
            "case {case:?}: the accepted delete must still report the index, got {answered}"
        );
        assert_eq!(
            index_named(&answered["TableDescription"], BUILDING)["IndexStatus"],
            json!("DELETING"),
            "case {case:?}: the cancelled build must be reported DELETING, got {answered}"
        );

        // And once the removal the call owed falls due, the table carries no such index and
        // no definition only that index needed.
        let settled = when_no_index_remains(&router, case, table).await;
        assert_eq!(
            declared_attributes(&settled),
            vec![KEY.to_string()],
            "case {case:?}: the settled table must declare the table key alone, got {settled}"
        );

        // A read naming the index is the request's own mistake from then on.
        let (status, read) = call(
            &router,
            "Query",
            json!({
                "TableName": table,
                "IndexName": BUILDING,
                "KeyConditionExpression": "g1 = :g",
                "ExpressionAttributeValues": {":g": {"S": "g1-0"}},
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str(), message(&read)),
            (
                400,
                "ValidationException",
                format!("The table does not have the specified index: {BUILDING}")
            ),
            "case {case:?}: a read naming the removed index must be refused, got {status} with \
             {read}"
        );

        // The base table kept every item: cancelling an index build is not a change to the
        // data the index was reading.
        let scanned = ok(
            &router,
            "Scan",
            json!({"TableName": table, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            scanned["Count"],
            json!(items),
            "case {case:?}: the base table must keep every item, got {scanned}"
        );
    }
}

#[tokio::test]
async fn deleting_a_settled_index_follows_the_same_lifecycle() {
    let router = emulator();
    let table = "GsiSelfDeleteSettled";
    // Two indexes declared WITH the table, so one is deleted while the other stays: the
    // definitions the surviving index keys on are kept, and only the departing index's own
    // are dropped.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": KEY, "AttributeType": "S"},
                {"AttributeName": "g1", "AttributeType": "S"},
                {"AttributeName": "g2", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": BUILDING,
                    "KeySchema": [{"AttributeName": "g1", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": "idx-keep",
                    "KeySchema": [{"AttributeName": "g2", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;
    let active = table_when(&router, "settled", REGION, table, |described| {
        described["TableStatus"] == json!("ACTIVE")
            && index_named(described, BUILDING)["IndexStatus"] == json!("ACTIVE")
    })
    .await;
    assert_eq!(
        index_named(&active, BUILDING)["IndexStatus"],
        json!("ACTIVE"),
        "the index must be settled for this test to be about an ACTIVE one, got {active}"
    );

    let answered = ok(&router, "UpdateTable", delete_index(table, BUILDING)).await;
    assert_eq!(
        index_named(&answered["TableDescription"], BUILDING)["IndexStatus"],
        json!("DELETING"),
        "the accepted delete must report the index on its way out, got {answered}"
    );

    // Not addressable from that instant, even while the table still reports it.
    let (status, read) = call(
        &router,
        "Scan",
        json!({"TableName": table, "IndexName": BUILDING}),
    )
    .await;
    assert_eq!(
        (status, error_code(&read).as_str(), message(&read)),
        (
            400,
            "ValidationException",
            format!("The table does not have the specified index: {BUILDING}")
        ),
        "a read naming an index on its way out must be refused, got {status} with {read}"
    );

    // Once the removal falls due the table carries the OTHER index alone, with its own
    // definition kept and the departing index's dropped.
    let settled = table_when(&router, "after the delete", REGION, table, |described| {
        described["TableStatus"] == json!("ACTIVE")
            && reported_indexes(described) == vec!["idx-keep".to_string()]
    })
    .await;
    let mut declared = declared_attributes(&settled);
    declared.sort();
    assert_eq!(
        declared,
        vec!["g2".to_string(), KEY.to_string()],
        "the settled table must declare the live keys alone, got {settled}"
    );
    // The surviving index still serves the read it was declared for.
    let queried = ok(
        &router,
        "Query",
        json!({
            "TableName": table,
            "IndexName": "idx-keep",
            "KeyConditionExpression": "g2 = :g",
            "ExpressionAttributeValues": {":g": {"S": "g2-0"}},
        }),
    )
    .await;
    assert_eq!(
        queried["Count"], json!(0),
        "the surviving index must still answer a read of it, got {queried}"
    );
}

#[tokio::test]
async fn a_second_update_inside_the_deletion_span_is_the_table_s_own_refusal() {
    let router = emulator();
    let table = "GsiSelfDeleteRepeated";
    seeded(&router, table, 2).await;
    ok(&router, "UpdateTable", add_index(table, BUILDING, 1)).await;
    ok(&router, "UpdateTable", delete_index(table, BUILDING)).await;

    // The accepted delete left the table `UPDATING` for the span the removal falls due in,
    // and "while it's UPDATING, you can't issue another UpdateTable request" (the
    // operation's doc) — so a repeat of that very delete is the table's in-use refusal
    // rather than a second cancellation.
    let (status, refused) = call(&router, "UpdateTable", delete_index(table, BUILDING)).await;
    assert_eq!(
        (status, error_code(&refused).as_str(), message(&refused)),
        (
            400,
            "ResourceInUseException",
            format!("Attempt to change a resource which is still in use: Table is being updated: {table}")
        ),
        "a repeated delete inside the span must be the table's in-use refusal, got {status} with \
         {refused}"
    );
    let described = ok(&router, "DescribeTable", json!({"TableName": table})).await;
    assert_eq!(
        index_named(&described["Table"], BUILDING)["IndexStatus"],
        json!("DELETING"),
        "the refused second delete must leave the first one standing, got {described}"
    );
    // And the removal the first delete owed still falls due.
    let settled = when_no_index_remains(&router, "after the refused repeat", table).await;
    assert!(
        reported_indexes(&settled).is_empty(),
        "the first delete must still remove the index, got {settled}"
    );
}
