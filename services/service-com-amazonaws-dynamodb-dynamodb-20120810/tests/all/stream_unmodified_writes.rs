// SPDX-License-Identifier: CC0-1.0

//! What a write that MODIFIES NOTHING leaves on a table's change feed.
//!
//! The rule pinned here: a stream is a time-ordered sequence of item-level MODIFICATIONS, so
//! a landed write publishes a record only when it left the item different from the one it
//! replaced — a write that stores exactly the values the item already holds, and a delete
//! that removed nothing, publish none — while a write that does change the item publishes
//! exactly one. An attribute is compared as the VALUE it is, so a number respelt is the same
//! number and a write that only restates one is no modification either.
//!
//! The table below spans what the rule generalises over: the operation the write arrives
//! through (`PutItem`, `UpdateItem` with each of its actions, `BatchWriteItem`,
//! `TransactWriteItems`, `DeleteItem`), the way the stored value can be restated rather than
//! changed, and — as the control the rule needs to be more than "nothing is ever published" —
//! the same operations making a real change. Every row is followed by one write that DOES
//! change its item, so a record the emulator merely delayed would still fail the row.
//!
//! Everything is driven over the wire against DynamoDB and DynamoDB Streams on one engine,
//! and every verdict is read off the streams surface a change-data-capture consumer reads.

use std::time::Duration;

use serde_json::{json, Value};

use crate::emulator::{call, drain_stream, emulator_with_streams};

/// The table every row below writes to, and the attribute it is keyed on.
const TABLE: &str = "modifications";
/// See [`TABLE`].
const KEY: &str = "Pk";

/// How long a poll for the documented `CREATING` -> `ACTIVE` transition may take before this
/// suite calls it a hang. A ceiling on PATIENCE, generous over the emulator's own span,
/// because a loaded machine is not a defect.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(30);

/// How long one look at the table's status waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One row: the item its key is first given, the write under test, and whether that write
/// leaves the item different from the one it replaced.
struct Row {
    case: &'static str,
    /// The key this row's item is written under, which no other row uses.
    id: &'static str,
    /// The attributes the item is created with, beside its key.
    held: Value,
    /// The operation the write under test arrives through.
    operation: &'static str,
    /// Whether that write changes the item, which is whether the stream owes a record for
    /// it.
    modifies: bool,
}

impl Row {
    /// The item this row's key holds after its creating write.
    fn item(&self) -> Value {
        let mut item = json!({ KEY: { "S": self.id } });
        for (name, value) in self.held.as_object().cloned().unwrap_or_default() {
            item[name] = value;
        }
        item
    }

    /// The body of the write under test, built over this row's own key.
    fn body(&self) -> Value {
        let key = json!({ KEY: { "S": self.id } });
        match self.case {
            "a PutItem storing exactly the item already held" => {
                json!({ "TableName": TABLE, "Item": self.item() })
            }
            "a PutItem restating a stored number in another spelling" => json!({
                "TableName": TABLE,
                "Item": { KEY: { "S": self.id }, "n": { "N": "1e0" } },
            }),
            "a PutItem storing another value" => json!({
                "TableName": TABLE,
                "Item": { KEY: { "S": self.id }, "n": { "N": "2" } },
            }),
            "an UpdateItem setting an attribute to the value it holds" => json!({
                "TableName": TABLE,
                "Key": key,
                "UpdateExpression": "SET #n = :same",
                "ExpressionAttributeNames": { "#n": "n" },
                "ExpressionAttributeValues": { ":same": { "N": "1" } },
            }),
            "an UpdateItem setting an attribute to another value" => json!({
                "TableName": TABLE,
                "Key": key,
                "UpdateExpression": "SET #n = :other",
                "ExpressionAttributeNames": { "#n": "n" },
                "ExpressionAttributeValues": { ":other": { "N": "7" } },
            }),
            "an UpdateItem removing an attribute the item does not carry" => json!({
                "TableName": TABLE,
                "Key": key,
                "UpdateExpression": "REMOVE #absent",
                "ExpressionAttributeNames": { "#absent": "never-written" },
            }),
            "an UpdateItem adding nothing to a stored number" => json!({
                "TableName": TABLE,
                "Key": key,
                "UpdateExpression": "ADD #n :nothing",
                "ExpressionAttributeNames": { "#n": "n" },
                "ExpressionAttributeValues": { ":nothing": { "N": "0" } },
            }),
            "a BatchWriteItem put of exactly the item already held" => json!({
                "RequestItems": { TABLE: [{ "PutRequest": { "Item": self.item() } }] },
            }),
            "a TransactWriteItems put of exactly the item already held" => json!({
                "TransactItems": [{ "Put": { "TableName": TABLE, "Item": self.item() } }],
            }),
            "a DeleteItem of a key that holds no item" => json!({
                "TableName": TABLE,
                "Key": { KEY: { "S": format!("{}-never-written", self.id) } },
            }),
            other => panic!("the row {other:?} states no write"),
        }
    }
}

/// The rows: each operation an item write lands through, restating what is stored as well as
/// changing it, and the two writes that remove nothing at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a PutItem storing exactly the item already held",
            id: "put-same",
            held: json!({ "n": { "N": "1" }, "s": { "S": "x" } }),
            operation: "PutItem",
            modifies: false,
        },
        Row {
            case: "a PutItem restating a stored number in another spelling",
            id: "put-respelt",
            held: json!({ "n": { "N": "1" } }),
            operation: "PutItem",
            modifies: false,
        },
        Row {
            case: "a PutItem storing another value",
            id: "put-other",
            held: json!({ "n": { "N": "1" } }),
            operation: "PutItem",
            modifies: true,
        },
        Row {
            case: "an UpdateItem setting an attribute to the value it holds",
            id: "set-same",
            held: json!({ "n": { "N": "1" } }),
            operation: "UpdateItem",
            modifies: false,
        },
        Row {
            case: "an UpdateItem setting an attribute to another value",
            id: "set-other",
            held: json!({ "n": { "N": "1" } }),
            operation: "UpdateItem",
            modifies: true,
        },
        Row {
            case: "an UpdateItem removing an attribute the item does not carry",
            id: "remove-absent",
            held: json!({ "n": { "N": "1" } }),
            operation: "UpdateItem",
            modifies: false,
        },
        Row {
            case: "an UpdateItem adding nothing to a stored number",
            id: "add-zero",
            held: json!({ "n": { "N": "5" } }),
            operation: "UpdateItem",
            modifies: false,
        },
        Row {
            case: "a BatchWriteItem put of exactly the item already held",
            id: "batch-same",
            held: json!({ "n": { "N": "1" }, "l": { "L": [{ "BOOL": true }] } }),
            operation: "BatchWriteItem",
            modifies: false,
        },
        Row {
            case: "a TransactWriteItems put of exactly the item already held",
            id: "transact-same",
            held: json!({ "n": { "N": "1" }, "m": { "M": { "inner": { "S": "y" } } } }),
            operation: "TransactWriteItems",
            modifies: false,
        },
        Row {
            case: "a DeleteItem of a key that holds no item",
            id: "delete-absent",
            held: json!({ "n": { "N": "1" } }),
            operation: "DeleteItem",
            modifies: false,
        },
    ]
}

/// Send one operation that must answer 200, answering its body.
async fn ok(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// Create [`TABLE`] with a stream capturing both images, wait until it serves writes, and
/// answer the ARN of the stream it registered.
async fn streamed_table(router: &smithy_server_runtime::MultiServiceRouter, case: &str) -> String {
    let created = ok(
        router,
        case,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {
                "StreamEnabled": true,
                "StreamViewType": "NEW_AND_OLD_IMAGES",
            },
        }),
    )
    .await;
    let stream_arn = created["TableDescription"]["LatestStreamArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !stream_arn.is_empty(),
        "case {case:?}: an enabled stream must be reported, got {created}"
    );
    let deadline = std::time::Instant::now() + ACTIVE_DEADLINE;
    loop {
        let described = ok(router, case, "DescribeTable", json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            return stream_arn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {TABLE} never reached ACTIVE, last saw {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The item `GetItem` reports for `id` — the table's own account of what it stores, which is
/// what a write that modified nothing must have left alone.
async fn stored(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    id: &str,
) -> Value {
    let read = ok(
        router,
        case,
        "GetItem",
        json!({
            "TableName": TABLE,
            "Key": { KEY: { "S": id } },
            "ConsistentRead": true,
        }),
    )
    .await;
    read["Item"].clone()
}

/// The records of `id`'s own item, in the order the stream holds them: a drain carries every
/// row's records, and one row is about one key.
fn records_of<'feed>(records: &'feed [Value], id: &str) -> Vec<&'feed Value> {
    records
        .iter()
        .filter(|record| record["dynamodb"]["Keys"][KEY]["S"] == json!(id))
        .collect()
}

/// RULE: a landed write publishes a stream record only when it left the item different from
/// the one it replaced — a write storing exactly the values already held, and a delete that
/// removed nothing, publish none — and one that did change the item publishes exactly one.
#[tokio::test]
async fn only_a_write_that_modifies_the_item_publishes_a_record() {
    let suite = "writes that modify nothing";
    let router = emulator_with_streams();
    let stream_arn = streamed_table(&router, suite).await;

    for row in rows() {
        let case = row.case;
        // The item the row's write is made against, which is one INSERT on the stream.
        ok(
            &router,
            case,
            "PutItem",
            json!({ "TableName": TABLE, "Item": row.item() }),
        )
        .await;
        let before = stored(&router, case, row.id).await;

        // The write under test.
        ok(&router, case, row.operation, row.body()).await;
        let after = stored(&router, case, row.id).await;
        assert_eq!(
            after == before,
            !row.modifies,
            "case {case:?}: the write must leave the item {}, held {before} and now holds \
             {after}",
            if row.modifies { "different" } else { "as it was" }
        );

        // And one write that DOES change the item, so a record the emulator only delayed
        // would still leave the row's expectation unmet.
        let mut changed = row.item();
        changed["marker"] = json!({ "S": "last" });
        ok(
            &router,
            case,
            "PutItem",
            json!({ "TableName": TABLE, "Item": changed }),
        )
        .await;
    }

    let drained = drain_stream(&router, suite, &stream_arn).await;
    for row in rows() {
        let case = row.case;
        let mine = records_of(&drained, row.id);
        let names: Vec<String> = mine
            .iter()
            .map(|record| record["eventName"].as_str().unwrap_or_default().to_string())
            .collect();
        let mut expected = vec!["INSERT".to_string()];
        if row.modifies {
            expected.push("MODIFY".to_string());
        }
        expected.push("MODIFY".to_string());
        assert_eq!(
            names, expected,
            "case {case:?}: the item's stream records must be {expected:?}, got {names:?} from \
             {mine:?}"
        );
        // The record of a modification carries the images either side of it, so a record
        // published for a write that changed nothing would be one whose two images are the
        // same document.
        for record in &mine {
            if record["eventName"] != json!("MODIFY") {
                continue;
            }
            let change = &record["dynamodb"];
            assert_ne!(
                change["OldImage"], change["NewImage"],
                "case {case:?}: every record must describe a modification, got {record}"
            );
        }
    }
    // A delete of a key that holds no item removed nothing, so that key is on the stream
    // nowhere at all.
    let never_written = records_of(&drained, "delete-absent-never-written");
    assert!(
        never_written.is_empty(),
        "case {:?}: a delete that removed nothing publishes no record, got {never_written:?}",
        "a DeleteItem of a key that holds no item"
    );
}
