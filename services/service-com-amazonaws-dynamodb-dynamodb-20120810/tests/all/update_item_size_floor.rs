// SPDX-License-Identifier: CC0-1.0

//! The item-size ceiling an UPDATE is held to, which is FLAT over the bytes the item stores:
//! no part of the item — its key portion least of all — is charged at a floor of its own.
//!
//! The rule under test: a change is refused when the item it would leave behind crosses the
//! documented 400 KB item ceiling, and what is charged against that ceiling is the bytes the
//! item stores — its attribute names and values, the key attributes among them — with nothing
//! added for the shape of the item. So the largest change an item accepts is the ceiling less
//! what the rest of the item already holds, the item it leaves tops out AT the ceiling
//! whatever it is keyed by, and one byte more is refused: the boundary moves with the key's
//! own bytes and with nothing else. A refused change leaves the stored item where it was, and
//! the refusal states the sentence the update path words that quota with.
//!
//! The rows span what the rule generalises over: key portions of four different sizes (a
//! one-byte name with a one-byte value, a longer short key, a longer one still and a long
//! name beside a long value) plus a composite-key table whose two key attributes are charged
//! together — each crossed with every surface that applies a stated change to a stored item
//! (`UpdateItem`, `TransactWriteItems`' `Update`, and a PartiQL `UPDATE` through
//! `ExecuteStatement`, `BatchExecuteStatement` and `ExecuteTransaction`), at its own computed
//! boundary and one byte past it.
//!
//! Source of the ceiling: "Service, Account, and Table Quotas" in the Amazon DynamoDB
//! Developer Guide ("The maximum item size in DynamoDB is 400 KB"); the service states it
//! once (`state::update_size_exceeded`) for every update surface.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, message, ok};
use crate::table_items::create_active_table;

/// The documented item ceiling, 400 KB.
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// AWS's fixed sentence for a change that would grow the stored item past the ceiling.
const UPDATE_TOO_BIG: &str = "Item size to update has exceeded the maximum allowed size";

/// The attribute every row's change writes, one byte of name.
const WRITTEN: &str = "v";

/// The value a row seeds its item with before the update under test.
const SEEDED: &str = "start";

/// One surface that applies a stated CHANGE to a stored item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The single-item update.
    UpdateItem,
    /// The `Update` action of a transaction, whose over-ceiling element fails the CALL.
    TransactWriteItems,
    /// A PartiQL `UPDATE` run on its own.
    ExecuteStatement,
    /// The same statement in a batch, which answers 200 and states the refusal in the slot
    /// that carried the statement.
    BatchExecuteStatement,
    /// The same statement in a PartiQL transaction.
    ExecuteTransaction,
}

const SURFACES: &[Surface] = &[
    Surface::UpdateItem,
    Surface::TransactWriteItems,
    Surface::ExecuteStatement,
    Surface::BatchExecuteStatement,
    Surface::ExecuteTransaction,
];

impl Surface {
    /// The operation name a row calls.
    fn operation(self) -> &'static str {
        match self {
            Surface::UpdateItem => "UpdateItem",
            Surface::TransactWriteItems => "TransactWriteItems",
            Surface::ExecuteStatement => "ExecuteStatement",
            Surface::BatchExecuteStatement => "BatchExecuteStatement",
            Surface::ExecuteTransaction => "ExecuteTransaction",
        }
    }

    /// The suffix a row's key value carries, so each surface writes its own item and no two
    /// rows read what the other wrote.
    fn suffix(self) -> &'static str {
        match self {
            Surface::UpdateItem => "u",
            Surface::TransactWriteItems => "t",
            Surface::ExecuteStatement => "s",
            Surface::BatchExecuteStatement => "b",
            Surface::ExecuteTransaction => "x",
        }
    }

    /// The request that writes an operand of `operand` single-byte characters into `key`'s
    /// item on this surface.
    fn request(self, shape: &Shape, key: &Value, operand: usize) -> Value {
        let value = json!({"S": "x".repeat(operand)});
        match self {
            Surface::UpdateItem => json!({
                "TableName": shape.table, "Key": key,
                "UpdateExpression": "SET v = :v",
                "ExpressionAttributeValues": {":v": value},
            }),
            Surface::TransactWriteItems => json!({"TransactItems": [{"Update": {
                "TableName": shape.table, "Key": key,
                "UpdateExpression": "SET v = :v",
                "ExpressionAttributeValues": {":v": value},
            }}]}),
            Surface::ExecuteStatement => json!({
                "Statement": shape.partiql_update(key),
                "Parameters": [value],
            }),
            Surface::BatchExecuteStatement => json!({"Statements": [{
                "Statement": shape.partiql_update(key),
                "Parameters": [value],
            }]}),
            Surface::ExecuteTransaction => json!({"TransactStatements": [{
                "Statement": shape.partiql_update(key),
                "Parameters": [value],
            }]}),
        }
    }
}

/// One table shape: the key attributes and the key VALUES a row addresses its item by.
struct Shape {
    case: &'static str,
    table: &'static str,
    /// The partition key attribute's name, and the sort key's where the table has one.
    partition: &'static str,
    sort: Option<&'static str>,
    /// The key values the row's item carries, as single-byte strings.
    partition_value: &'static str,
    sort_value: Option<&'static str>,
}

impl Shape {
    /// The bytes this shape's key portion stores: each key attribute's name plus its value.
    fn key_bytes(&self) -> usize {
        self.partition.len()
            + self.partition_value.len()
            + self.sort.map_or(0, str::len)
            + self.sort_value.map_or(0, str::len)
    }

    fn key_schema(&self) -> Vec<(&'static str, &'static str, &'static str)> {
        let mut schema = vec![(self.partition, "HASH", "S")];
        if let Some(sort) = self.sort {
            schema.push((sort, "RANGE", "S"));
        }
        schema
    }

    /// The key of this shape's item for `surface`, whose own suffix keeps the surfaces apart.
    fn key_for(&self, surface: Surface) -> Value {
        let mut key = serde_json::Map::new();
        key.insert(
            self.partition.to_string(),
            json!({"S": format!("{}{}", self.partition_value, surface.suffix())}),
        );
        if let (Some(name), Some(value)) = (self.sort, self.sort_value) {
            key.insert(name.to_string(), json!({"S": value}));
        }
        Value::Object(key)
    }

    /// The PartiQL statement that writes the parameter into `key`'s item, addressing the item
    /// by every key attribute the table declares.
    fn partiql_update(&self, key: &Value) -> String {
        let mut predicate = format!(
            "{} = '{}'",
            self.partition,
            key[self.partition]["S"].as_str().unwrap_or_default()
        );
        if let Some(sort) = self.sort {
            predicate.push_str(&format!(
                " AND {sort} = '{}'",
                key[sort]["S"].as_str().unwrap_or_default()
            ));
        }
        format!(
            "UPDATE \"{}\" SET {WRITTEN} = ? WHERE {predicate}",
            self.table
        )
    }
}

/// Four key portions of different sizes below, plus a composite key charged across its two
/// attributes.
const SHAPES: &[Shape] = &[
    Shape {
        case: "a one-byte key name and a one-byte key value",
        table: "FlatCeilingTiny",
        partition: "k",
        sort: None,
        partition_value: "1",
        sort_value: None,
    },
    Shape {
        case: "a key portion of eight bytes",
        table: "FlatCeilingShort",
        partition: "pk",
        sort: None,
        partition_value: "123456",
        sort_value: None,
    },
    Shape {
        case: "a key portion of twenty-one bytes",
        table: "FlatCeilingMedium",
        partition: "pk",
        sort: None,
        partition_value: "1234567890123456789",
        sort_value: None,
    },
    Shape {
        case: "a long key name beside a long key value",
        table: "FlatCeilingLong",
        partition: "partitionkeyname",
        sort: None,
        partition_value: "0123456789012345",
        sort_value: None,
    },
    Shape {
        case: "a composite key charged across its two attributes",
        table: "FlatCeilingComposite",
        partition: "pk",
        sort: Some("sk"),
        partition_value: "abcd",
        sort_value: Some("efgh"),
    },
];

/// RULE: on every update surface, the largest change an item accepts is the ceiling less what
/// the rest of the item holds — that change is applied whole, the TOTAL it leaves is the
/// ceiling itself whatever the item is keyed by, and one byte more is refused with the
/// update-path sentence while the stored item keeps its value.
#[tokio::test]
async fn an_update_tops_out_at_the_flat_item_ceiling() {
    let router = emulator();

    for shape in SHAPES {
        create_active_table(&router, shape.table, &shape.key_schema()).await;
        for surface in SURFACES {
            let surface = *surface;
            let key = shape.key_for(surface);
            let mut seed = key.clone();
            seed[WRITTEN] = json!({"S": SEEDED});
            ok(
                &router,
                "PutItem",
                json!({"TableName": shape.table, "Item": seed}),
            )
            .await;

            // The key portion of THIS item: the surface's suffix makes its value one byte
            // longer than the shape's own.
            let key_bytes = shape.key_bytes() + surface.suffix().len();
            let largest = MAX_ITEM_BYTES - key_bytes - WRITTEN.len();
            let case = format!("{} on {}", surface.operation(), shape.case);

            // At the boundary the change is applied...
            let answered = call(
                &router,
                surface.operation(),
                surface.request(shape, &key, largest),
            )
            .await;
            assert_served(&case, surface, &answered, largest, key_bytes);
            let stored = stored_length(&router, shape, &key).await;
            assert_eq!(
                stored,
                Some(largest),
                "case {case:?}: expected the whole operand to be stored, got {stored:?} byte(s)"
            );
            // ...and the TOTAL the item then holds is the ceiling itself, which is what a
            // FLAT quota over the stored bytes means.
            let total = key_bytes + WRITTEN.len() + largest;
            assert_eq!(
                total, MAX_ITEM_BYTES,
                "case {case:?}: expected the stored item to top out at the ceiling over its \
                 {key_bytes}-byte key, got {total}"
            );

            // One byte past it the change is refused, with the item left as it was.
            let answered = call(
                &router,
                surface.operation(),
                surface.request(shape, &key, largest + 1),
            )
            .await;
            assert_refused(&case, surface, &answered);
            let stored = stored_length(&router, shape, &key).await;
            assert_eq!(
                stored,
                Some(largest),
                "case {case:?}: a refused change must leave the stored item where it was, got \
                 {stored:?} byte(s)"
            );
        }
    }
}

/// The length of the written attribute as a `ConsistentRead` `GetItem` reports it.
async fn stored_length(
    router: &smithy_server_runtime::MultiServiceRouter,
    shape: &Shape,
    key: &Value,
) -> Option<usize> {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": shape.table, "Key": key, "ConsistentRead": true}),
    )
    .await;
    read["Item"][WRITTEN]["S"].as_str().map(str::len)
}

/// Assert the change at the boundary was SERVED on this surface: every surface but the batch
/// answers 200 with no error, and the batch answers 200 with no error in its slot.
fn assert_served(
    case: &str,
    surface: Surface,
    answered: &(u16, Value),
    operand: usize,
    key_bytes: usize,
) {
    let (status, body) = answered;
    assert_eq!(
        *status, 200,
        "case {case:?}: expected the change at the boundary ({operand} operand bytes over a \
         {key_bytes}-byte key) to be applied, got {status} with {}",
        message(body)
    );
    if surface == Surface::BatchExecuteStatement {
        assert_eq!(
            body["Responses"][0].get("Error"),
            None,
            "case {case:?}: expected the batch's slot to carry no error, got {}",
            body["Responses"][0]
        );
    }
}

/// Assert the change one byte past the boundary was REFUSED with the update path's sentence:
/// as the CALL's own failure on every surface but the batch, which answers 200 and states it
/// in the slot that carried the statement.
fn assert_refused(case: &str, surface: Surface, answered: &(u16, Value)) {
    let (status, body) = answered;
    if surface == Surface::BatchExecuteStatement {
        assert_eq!(
            *status, 200,
            "case {case:?}: a batch answers 200 whatever its slots say, got {status} with {body:?}"
        );
        assert_eq!(
            body["Responses"][0]["Error"]["Message"].as_str(),
            Some(UPDATE_TOO_BIG),
            "case {case:?}: expected the slot to state the update-path ceiling refusal, got {}",
            body["Responses"][0]
        );
        return;
    }
    assert_eq!(
        (*status, message(body).as_str()),
        (400, UPDATE_TOO_BIG),
        "case {case:?}: expected the update-path ceiling refusal one byte past the boundary, \
         got {status}"
    );
}
