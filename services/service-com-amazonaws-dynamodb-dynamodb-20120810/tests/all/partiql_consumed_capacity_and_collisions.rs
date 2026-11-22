// SPDX-License-Identifier: CC0-1.0

//! What the PartiQL surfaces report for what they CONSUMED, and how a statement that
//! collides on a primary key inside a transaction is cancelled.
//!
//! The rules asserted here, each over a set of inputs rather than one call:
//!
//! 1. `ConsumedCapacityMultiple` — the member `BatchExecuteStatement$ConsumedCapacity`
//!    and `ExecuteTransaction$ConsumedCapacity` are declared as — carries ONE entry per
//!    table the call reached, whose figure is the SUM over the statements that reached
//!    it. The rows span reads and writes, one table and two, how many statements reach a
//!    table, and the consistency each read of a batch asked for.
//! 2. A PartiQL `INSERT` is a put that must not replace, so a key that already holds an
//!    item is a collision on that key: a transaction carrying one is cancelled reporting
//!    `DuplicateItem` for that slot — with the sentence that collision is stated in — and
//!    `None` for the slots nothing was wrong with, while a predicate the stored item does
//!    not satisfy stays `ConditionalCheckFailed`. The rows span which slot collides, how
//!    many statements the transaction carries and which table the collision is in.
//! 3. A `SELECT` is charged as the PLAN that serves it, on the arm of the source it read:
//!    one that pins the partition key of its source — the index's own where the `FROM`
//!    target names an index — is charged once over the bytes that partition held, at the
//!    rate the statement asked for; one that pins none walks the whole source and is
//!    charged per entry; and the TABLE's arm carries only what the read spent on the
//!    table, which is nothing unless a local index had to fetch what it does not project.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's
//! front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row of the totals and collision tables addresses first.
const BOOKS: &str = "books";

/// The second table, so a row can spread its statements over two of them.
const MOVIES: &str = "movies";

/// The table the capacity-arm rows read, with one index of each kind.
const SHELF: &str = "shelf";

/// The global secondary index of [`SHELF`], which projects one non-key attribute.
const GSI: &str = "by-genre";

/// The KEYS_ONLY local secondary index of [`SHELF`]: an attribute named through it is
/// fetched from the parent table.
const LSI: &str = "by-rank";

/// One read capacity unit covers 4 KB of a strongly consistent read of an item, and "one
/// read capacity unit represents ... two eventually consistent reads per second, for an
/// item up to 4 KB" ("Read/write capacity mode", Amazon DynamoDB Developer Guide). Every
/// item these tables store is far inside one unit.
const READ_STRONG: f64 = 1.0;

/// The same read, eventually consistent.
const READ_EVENTUAL: f64 = 0.5;

/// "One write capacity unit represents one write per second for an item up to 1 KB" —
/// which every item these tables write is inside.
const WRITE: f64 = 1.0;

/// "Transactional read requests require two read capacity units" and transactional writes
/// two write capacity units, so a statement of a transaction costs twice the same
/// single-item one.
const TRANSACTIONAL: f64 = 2.0;

/// The sentence a key collision is reported with.
const DUPLICATE_ITEM_MESSAGE: &str = "Duplicate primary key exists in table";

/// The sentence an unmet predicate is reported with.
const CONDITION_FAILED_MESSAGE: &str = "The conditional request failed";

/// Create the two single-key tables the totals and collision rows address, and seed two
/// items in each.
async fn two_tables(router: &MultiServiceRouter) {
    for name in [BOOKS, MOVIES] {
        ok(
            router,
            "CreateTable",
            json!({
                "TableName": name,
                "BillingMode": "PAY_PER_REQUEST",
                "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
            }),
        )
        .await;
    }
    // Both creates are in flight before either is waited on, so the two tables settle
    // together rather than one after the other.
    for name in [BOOKS, MOVIES] {
        describe_when_active(router, name, name).await;
        for id in ["one", "two", "three"] {
            ok(
                router,
                "PutItem",
                json!({
                    "TableName": name,
                    "Item": {"id": {"S": id}, "note": {"S": "seeded"}},
                }),
            )
            .await;
        }
    }
}

/// One statement of a batch, with the consistency it asks to read at.
fn slot(text: &str, consistent_read: Option<bool>) -> Value {
    let mut entry = json!({"Statement": text});
    if let Some(consistent_read) = consistent_read {
        entry["ConsistentRead"] = json!(consistent_read);
    }
    entry
}

/// One statement of a transaction.
fn transact_statement(text: &str) -> Value {
    json!({"Statement": text})
}

/// The `(TableName, CapacityUnits)` pairs a `ConsumedCapacityMultiple` answer carries,
/// sorted by table name so a row states what each table was charged without pinning the
/// order the list happens to be built in.
fn reported_totals(answered: &Value) -> Vec<(String, f64)> {
    let mut totals: Vec<(String, f64)> = answered["ConsumedCapacity"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["TableName"].as_str().unwrap_or_default().to_string(),
                entry["CapacityUnits"].as_f64().unwrap_or_default(),
            )
        })
        .collect();
    totals.sort_by(|left, right| left.0.cmp(&right.0));
    totals
}

/// One row of the totals table: the call, the statements it carries, what each table it
/// reached must be charged, and whether those statements CHANGE what the next row would
/// see.
struct TotalsRow {
    case: &'static str,
    operation: &'static str,
    statements: Vec<Value>,
    expected: Vec<(&'static str, f64)>,
    writes: bool,
}

/// The rows differ in the surface (a batch and a transaction), in whether the statements
/// read or write, in how many of them reach ONE table, in whether a second table is
/// reached at all, and — for the batch, whose statements each carry their own
/// `ConsistentRead` — in the rate each read asked for.
fn totals_table() -> Vec<TotalsRow> {
    vec![
        TotalsRow {
            case: "a batch of two reads of one table, one strong and one eventual",
            operation: "BatchExecuteStatement",
            statements: vec![
                slot("SELECT * FROM books WHERE id = 'one'", Some(true)),
                slot("SELECT * FROM books WHERE id = 'two'", Some(false)),
            ],
            expected: vec![(BOOKS, READ_STRONG + READ_EVENTUAL)],
            writes: false,
        },
        TotalsRow {
            case: "a batch of three eventual reads of one table",
            operation: "BatchExecuteStatement",
            statements: vec![
                slot("SELECT * FROM books WHERE id = 'one'", None),
                slot("SELECT * FROM books WHERE id = 'two'", None),
                slot("SELECT * FROM books WHERE id = 'three'", None),
            ],
            expected: vec![(BOOKS, 3.0 * READ_EVENTUAL)],
            writes: false,
        },
        TotalsRow {
            case: "a batch of reads spread over two tables",
            operation: "BatchExecuteStatement",
            statements: vec![
                slot("SELECT * FROM books WHERE id = 'one'", None),
                slot("SELECT * FROM movies WHERE id = 'one'", Some(true)),
                slot("SELECT * FROM books WHERE id = 'two'", None),
            ],
            expected: vec![
                (BOOKS, 2.0 * READ_EVENTUAL),
                (MOVIES, READ_STRONG),
            ],
            writes: false,
        },
        TotalsRow {
            case: "a batch of two writes of one table",
            operation: "BatchExecuteStatement",
            statements: vec![
                slot("INSERT INTO books VALUE {'id': 'four'}", None),
                slot("INSERT INTO books VALUE {'id': 'five'}", None),
            ],
            expected: vec![(BOOKS, 2.0 * WRITE)],
            writes: true,
        },
        TotalsRow {
            case: "a batch of writes spread over two tables",
            operation: "BatchExecuteStatement",
            statements: vec![
                slot("INSERT INTO books VALUE {'id': 'four'}", None),
                slot("UPDATE movies SET note = 'changed' WHERE id = 'one'", None),
            ],
            expected: vec![(BOOKS, WRITE), (MOVIES, WRITE)],
            writes: true,
        },
        TotalsRow {
            case: "a transaction of two reads of one table",
            operation: "ExecuteTransaction",
            statements: vec![
                transact_statement("SELECT * FROM books WHERE id = 'one'"),
                transact_statement("SELECT * FROM books WHERE id = 'two'"),
            ],
            expected: vec![(BOOKS, 2.0 * TRANSACTIONAL * READ_STRONG)],
            writes: false,
        },
        TotalsRow {
            case: "a transaction of reads spread over two tables",
            operation: "ExecuteTransaction",
            statements: vec![
                transact_statement("SELECT * FROM books WHERE id = 'one'"),
                transact_statement("SELECT * FROM movies WHERE id = 'one'"),
                transact_statement("SELECT * FROM books WHERE id = 'two'"),
                transact_statement("SELECT * FROM books WHERE id = 'three'"),
            ],
            expected: vec![
                (BOOKS, 3.0 * TRANSACTIONAL * READ_STRONG),
                (MOVIES, TRANSACTIONAL * READ_STRONG),
            ],
            writes: false,
        },
        TotalsRow {
            case: "a transaction of two writes of one table",
            operation: "ExecuteTransaction",
            statements: vec![
                transact_statement("INSERT INTO books VALUE {'id': 'four'}"),
                transact_statement("UPDATE books SET note = 'changed' WHERE id = 'one'"),
            ],
            expected: vec![(BOOKS, 2.0 * TRANSACTIONAL * WRITE)],
            writes: true,
        },
        TotalsRow {
            case: "a transaction of writes spread over two tables",
            operation: "ExecuteTransaction",
            statements: vec![
                transact_statement("INSERT INTO books VALUE {'id': 'four'}"),
                transact_statement("DELETE FROM movies WHERE id = 'one'"),
            ],
            expected: vec![
                (BOOKS, TRANSACTIONAL * WRITE),
                (MOVIES, TRANSACTIONAL * WRITE),
            ],
            writes: true,
        },
    ]
}

#[tokio::test]
async fn consumed_capacity_carries_one_entry_per_table_summing_its_statements() {
    // The rows that only READ share one store, because a read changes nothing a later row
    // would see; each row that writes gets its own, since what it stores would decide what
    // the next one reads.
    let shared = emulator();
    two_tables(&shared).await;

    for row in totals_table() {
        let case = row.case;
        let own = if row.writes {
            let own = emulator();
            two_tables(&own).await;
            Some(own)
        } else {
            None
        };
        let router = own.as_ref().unwrap_or(&shared);

        let member = if row.operation == "ExecuteTransaction" {
            "TransactStatements"
        } else {
            "Statements"
        };
        let answered = ok(
            router,
            row.operation,
            json!({
                member: row.statements,
                "ReturnConsumedCapacity": "TOTAL",
            }),
        )
        .await;
        let expected: Vec<(String, f64)> = row
            .expected
            .iter()
            .map(|(table, units)| ((*table).to_string(), *units))
            .collect();
        let reported = reported_totals(&answered);
        assert_eq!(
            reported, expected,
            "case {case}: one entry per table, summed over the statements that reached it, \
             expected {expected:?}, got {answered}"
        );
    }
}

/// One row of the collision table: the statements, the reason code each slot must report,
/// and a key no slot of it may have stored.
struct CollisionRow {
    case: &'static str,
    statements: Vec<Value>,
    codes: Vec<&'static str>,
    unwritten: Option<(&'static str, &'static str)>,
}

/// The rows differ in WHICH slot fails, in how many statements the transaction carries,
/// in which table the failing statement addresses, and in whether the failure is a key
/// collision or a predicate the stored item does not satisfy — the two report different
/// reasons.
fn collision_table() -> Vec<CollisionRow> {
    vec![
        CollisionRow {
            case: "a collision in the first of two slots",
            statements: vec![
                transact_statement("INSERT INTO books VALUE {'id': 'one'}"),
                transact_statement("INSERT INTO books VALUE {'id': 'fresh'}"),
            ],
            codes: vec!["DuplicateItem", "None"],
            unwritten: Some((BOOKS, "fresh")),
        },
        CollisionRow {
            case: "a collision in the second of three slots",
            statements: vec![
                transact_statement("INSERT INTO books VALUE {'id': 'fresh'}"),
                transact_statement("INSERT INTO books VALUE {'id': 'two'}"),
                transact_statement("UPDATE books SET note = 'changed' WHERE id = 'three'"),
            ],
            codes: vec!["None", "DuplicateItem", "None"],
            unwritten: Some((BOOKS, "fresh")),
        },
        CollisionRow {
            case: "a collision in the other table, alone",
            statements: vec![transact_statement("INSERT INTO movies VALUE {'id': 'one'}")],
            codes: vec!["DuplicateItem"],
            unwritten: None,
        },
        CollisionRow {
            case: "two collisions, one in each table",
            statements: vec![
                transact_statement("INSERT INTO books VALUE {'id': 'one'}"),
                transact_statement("INSERT INTO movies VALUE {'id': 'two'}"),
            ],
            codes: vec!["DuplicateItem", "DuplicateItem"],
            unwritten: None,
        },
        CollisionRow {
            // A predicate the stored item does not satisfy is a different failure from a
            // key collision, and keeps its own reason.
            case: "a predicate the stored item does not satisfy, beside a collision",
            statements: vec![
                transact_statement("UPDATE books SET note = 'changed' WHERE id = 'one' AND note = 'absent'"),
                transact_statement("INSERT INTO books VALUE {'id': 'two'}"),
            ],
            codes: vec!["ConditionalCheckFailed", "DuplicateItem"],
            unwritten: None,
        },
        CollisionRow {
            case: "a predicate the stored item does not satisfy, alone",
            statements: vec![transact_statement(
                "UPDATE movies SET note = 'changed' WHERE id = 'one' AND note = 'absent'",
            )],
            codes: vec!["ConditionalCheckFailed"],
            unwritten: None,
        },
    ]
}

#[tokio::test]
async fn a_transaction_reports_a_key_collision_as_a_duplicate_item() {
    // Every row of this table is CANCELLED, so none of them writes anything: one store
    // serves them all, and the `unwritten` check below is what holds that to account.
    let router = emulator();
    two_tables(&router).await;

    for row in collision_table() {
        let case = row.case;
        let (status, refused) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": row.statements}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a cancelled transaction is refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "TransactionCanceledException",
            "case {case}: expected TransactionCanceledException, got {refused}"
        );
        let reasons = refused["CancellationReasons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let codes: Vec<String> = reasons
            .iter()
            .map(|reason| reason["Code"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            codes, row.codes,
            "case {case}: expected the reasons {:?}, got {refused}",
            row.codes
        );
        // The message lists the same codes, in the same order, and nothing else of the
        // request: that list is the only variable part of it.
        assert_eq!(
            message(&refused),
            format!(
                "Transaction cancelled, please refer cancellation reasons for specific reasons \
                 [{}]",
                row.codes.join(", ")
            ),
            "case {case}: the message lists the reason codes in request order, got {refused}"
        );
        for (reason, code) in reasons.iter().zip(&row.codes) {
            let stated = reason["Message"].as_str();
            let expected = match *code {
                "DuplicateItem" => Some(DUPLICATE_ITEM_MESSAGE),
                "ConditionalCheckFailed" => Some(CONDITION_FAILED_MESSAGE),
                _ => None,
            };
            assert_eq!(
                stated, expected,
                "case {case}: the {code} reason states its own sentence, got {reason}"
            );
        }
        if let Some((table, id)) = row.unwritten {
            let read = ok(
                &router,
                "GetItem",
                json!({"TableName": table, "Key": {"id": {"S": id}}}),
            )
            .await;
            assert!(
                read.get("Item").is_none(),
                "case {case}: a cancelled transaction writes nothing, got {read}"
            );
        }
    }
}

/// Create the indexed table the capacity-arm rows read, and seed three items that share
/// one table partition and one index partition.
async fn indexed(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": SHELF,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "genre", "AttributeType": "S"},
                {"AttributeName": "rank", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": GSI,
                "KeySchema": [
                    {"AttributeName": "genre", "KeyType": "HASH"},
                    {"AttributeName": "sk", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "INCLUDE", "NonKeyAttributes": ["shown"]},
            }],
            "LocalSecondaryIndexes": [{
                "IndexName": LSI,
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "rank", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
        }),
    )
    .await;
    describe_when_active(router, "the indexed table", SHELF).await;
    for sk in ["a", "b", "c"] {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": SHELF,
                "Item": {
                    "pk": {"S": "p1"},
                    "sk": {"S": sk},
                    "genre": {"S": "g1"},
                    "rank": {"S": sk},
                    "shown": {"S": "projected"},
                    "hidden": {"S": "unprojected"},
                },
            }),
        )
        .await;
    }
}

/// How many items the indexed table holds, which is how many entries each of its indexes
/// holds and how many rows a walk of either evaluates.
const SEEDED: f64 = 3.0;

/// One row of the arm table: the statement, the consistency it asks for, and what each
/// arm of its `ConsumedCapacity` must carry.
struct ArmRow {
    case: &'static str,
    statement: String,
    consistent_read: Option<bool>,
    /// The units the TABLE's own arm carries: what the read spent on the table.
    table_arm: f64,
    /// The index the read was served from and the units its arm carries, or `None` for a
    /// statement that read the table itself.
    index_arm: Option<(&'static str, f64)>,
}

/// The rows differ in the SOURCE the statement reads (the table, a global index, a local
/// one), in whether its clause pins that source's partition key, in whether it names an
/// attribute the index does not project, and in the consistency it asks for.
fn arm_table() -> Vec<ArmRow> {
    vec![
        ArmRow {
            case: "a clause pinning the index's partition key",
            statement: format!("SELECT * FROM \"{SHELF}\".\"{GSI}\" WHERE genre = 'g1'"),
            consistent_read: None,
            table_arm: 0.0,
            index_arm: Some((GSI, READ_EVENTUAL)),
        },
        ArmRow {
            case: "a clause pinning the index's whole key",
            statement: format!(
                "SELECT * FROM \"{SHELF}\".\"{GSI}\" WHERE genre = 'g1' AND sk = 'a'"
            ),
            consistent_read: None,
            table_arm: 0.0,
            index_arm: Some((GSI, READ_EVENTUAL)),
        },
        ArmRow {
            // A clause that pins no partition key of the index walks every entry of it,
            // and a walk is charged per entry.
            case: "a clause pinning nothing of the index",
            statement: format!("SELECT * FROM \"{SHELF}\".\"{GSI}\""),
            consistent_read: None,
            table_arm: 0.0,
            index_arm: Some((GSI, SEEDED * READ_STRONG)),
        },
        ArmRow {
            case: "a local index asked for an attribute it does not project",
            statement: format!("SELECT hidden FROM \"{SHELF}\".\"{LSI}\" WHERE pk = 'p1'"),
            consistent_read: None,
            table_arm: SEEDED * READ_EVENTUAL,
            index_arm: Some((LSI, READ_EVENTUAL)),
        },
        ArmRow {
            // The same fetch, read strongly consistently: both arms are charged at the
            // rate the statement asked for.
            case: "the same fetch, read strongly consistently",
            statement: format!("SELECT hidden FROM \"{SHELF}\".\"{LSI}\" WHERE pk = 'p1'"),
            consistent_read: Some(true),
            table_arm: SEEDED * READ_STRONG,
            index_arm: Some((LSI, READ_STRONG)),
        },
        ArmRow {
            case: "a local index asked only for what it projects",
            statement: format!("SELECT * FROM \"{SHELF}\".\"{LSI}\" WHERE pk = 'p1'"),
            consistent_read: None,
            table_arm: 0.0,
            index_arm: Some((LSI, READ_EVENTUAL)),
        },
        ArmRow {
            case: "a clause pinning the table's own partition key",
            statement: format!("SELECT * FROM \"{SHELF}\" WHERE pk = 'p1'"),
            consistent_read: None,
            table_arm: READ_EVENTUAL,
            index_arm: None,
        },
        ArmRow {
            case: "a clause pinning nothing of the table",
            statement: format!("SELECT * FROM \"{SHELF}\""),
            consistent_read: None,
            table_arm: SEEDED * READ_EVENTUAL,
            index_arm: None,
        },
    ]
}

#[tokio::test]
async fn a_statement_is_charged_as_the_plan_that_serves_it_on_its_sources_arm() {
    let router = emulator();
    indexed(&router).await;

    for row in arm_table() {
        let case = row.case;
        let mut body = json!({
            "Statement": row.statement,
            "ReturnConsumedCapacity": "INDEXES",
        });
        if let Some(consistent_read) = row.consistent_read {
            body["ConsistentRead"] = json!(consistent_read);
        }
        let answered = ok(&router, "ExecuteStatement", body).await;
        let consumed = &answered["ConsumedCapacity"];
        assert_eq!(
            consumed["Table"]["CapacityUnits"].as_f64(),
            Some(row.table_arm),
            "case {case}: the table's arm carries what the read spent on the table, expected \
             {}, got {consumed}",
            row.table_arm
        );
        let mut aggregate = row.table_arm;
        match row.index_arm {
            Some((index, units)) => {
                let arm = if index == GSI {
                    &consumed["GlobalSecondaryIndexes"][index]
                } else {
                    &consumed["LocalSecondaryIndexes"][index]
                };
                assert_eq!(
                    arm["CapacityUnits"].as_f64(),
                    Some(units),
                    "case {case}: the index the statement read carries what reading it cost, \
                     expected {units}, got {consumed}"
                );
                aggregate += units;
            }
            None => {
                assert!(
                    consumed.get("GlobalSecondaryIndexes").is_none()
                        && consumed.get("LocalSecondaryIndexes").is_none(),
                    "case {case}: a statement that read the table reaches no index arm, got \
                     {consumed}"
                );
            }
        }
        assert_eq!(
            consumed["CapacityUnits"].as_f64(),
            Some(aggregate),
            "case {case}: the aggregate is what the arms add up to, expected {aggregate}, got \
             {consumed}"
        );
    }
}
