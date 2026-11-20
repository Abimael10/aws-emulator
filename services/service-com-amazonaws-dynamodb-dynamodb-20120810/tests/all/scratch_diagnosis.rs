// SPDX-License-Identifier: CC0-1.0

//! The two clause orders a PartiQL write may state its source in, on EVERY surface that
//! runs a statement: `ExecuteStatement` on its own, one slot of a `BatchExecuteStatement`
//! and one statement of an `ExecuteTransaction`.
//!
//! The rule is the dialect's, not one surface's: a write states its source after the verb
//! (`UPDATE source SET … WHERE …`) or first, in a `FROM` clause
//! (`FROM source WHERE … SET …`), and runs as the same write either way — so the item it
//! leaves is the same whichever surface ran it, which is what a later read reports.
//!
//! Driven over the wire through the same `MultiServiceRouter` the emulator's front doors
//! use, and asserted by READING THE ITEM BACK: a statement a surface reports as applied has
//! to have left its change in the store.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, ok};

/// The table every row addresses: one string partition key.
const TABLE: &str = "ClauseOrders";

/// Create [`TABLE`] and poll until it is `ACTIVE`.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, TABLE, TABLE).await;
    router
}

/// Store the item a row's statement changes, with `status` as it stands before the write.
async fn stored(router: &MultiServiceRouter, key: &str) {
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": key}, "status": {"S": "active"}}}),
    )
    .await;
}

/// What `status` holds at `key` now.
async fn status(router: &MultiServiceRouter, key: &str) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
    )
    .await;
    read["Item"]["status"].clone()
}

#[tokio::test]
async fn either_clause_order_runs_as_the_same_write_on_every_statement_surface() {
    let router = seeded().await;

    // Each row: the surface, the request it carries one statement in, and the key that
    // statement changes. The statements span both orders — the verb naming the target and
    // the source stated first in a `FROM` clause — on each of the three surfaces.
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "ExecuteStatement, the verb naming the target",
            "ExecuteStatement",
            "verb-single",
            format!("UPDATE \"{TABLE}\" SET status = 'confirmed' WHERE pk = 'verb-single'"),
        ),
        (
            "ExecuteStatement, the source stated first",
            "ExecuteStatement",
            "from-single",
            format!("FROM \"{TABLE}\" WHERE pk = 'from-single' SET status = 'confirmed'"),
        ),
        (
            "BatchExecuteStatement, the verb naming the target",
            "BatchExecuteStatement",
            "verb-batch",
            format!("UPDATE \"{TABLE}\" SET status = 'confirmed' WHERE pk = 'verb-batch'"),
        ),
        (
            "BatchExecuteStatement, the source stated first",
            "BatchExecuteStatement",
            "from-batch",
            format!("FROM \"{TABLE}\" WHERE pk = 'from-batch' SET status = 'confirmed'"),
        ),
        (
            "ExecuteTransaction, the verb naming the target",
            "ExecuteTransaction",
            "verb-txn",
            format!("UPDATE \"{TABLE}\" SET status = 'confirmed' WHERE pk = 'verb-txn'"),
        ),
        (
            "ExecuteTransaction, the source stated first",
            "ExecuteTransaction",
            "from-txn",
            format!("FROM \"{TABLE}\" WHERE pk = 'from-txn' SET status = 'confirmed'"),
        ),
    ];

    for (case, operation, key, statement) in rows {
        stored(&router, key).await;
        let request = match operation {
            "ExecuteStatement" => json!({"Statement": statement}),
            "BatchExecuteStatement" => json!({"Statements": [{"Statement": statement}]}),
            _ => json!({"TransactStatements": [{"Statement": statement}]}),
        };
        let answered = ok(&router, operation, request).await;
        if operation == "BatchExecuteStatement" {
            assert_eq!(
                answered["Responses"][0].get("Error"),
                None,
                "case {case:?}: expected the slot served, got {answered}"
            );
        }
        let stated = status(&router, key).await;
        assert_eq!(
            stated,
            json!({"S": "confirmed"}),
            "case {case:?}: expected the write to have landed, got {stated}"
        );
    }

    // The BOUNDARY, on each surface: the clauses in neither order state a clause past a
    // complete statement, so nothing is written — which is what makes the rows above the
    // two orders being read rather than every order being accepted.
    for (case, operation, key, statement) in [
        (
            "ExecuteStatement, the predicate after the actions of a FROM clause",
            "ExecuteStatement",
            "from-single",
            format!("FROM \"{TABLE}\" SET status = 'moved' WHERE pk = 'from-single'"),
        ),
        (
            "BatchExecuteStatement, the predicate before the actions of the verb form",
            "BatchExecuteStatement",
            "verb-batch",
            format!("UPDATE \"{TABLE}\" WHERE pk = 'verb-batch' SET status = 'moved'"),
        ),
    ] {
        let request = match operation {
            "ExecuteStatement" => json!({"Statement": statement.clone()}),
            _ => json!({"Statements": [{"Statement": statement.clone()}]}),
        };
        let (status_code, answered) = call(&router, operation, request).await;
        if operation == "BatchExecuteStatement" {
            // "A HTTP 200 response does not mean that all statements in the
            // BatchExecuteStatement succeeded": the statement's own defect is its slot's.
            assert_eq!(
                (status_code, answered["Responses"][0]["Error"]["Code"].as_str()),
                (200, Some("ValidationError")),
                "case {case:?}: expected the slot to carry the refusal, got {answered}"
            );
        } else {
            assert_eq!(
                status_code, 400,
                "case {case:?}: expected the statement refused, got {status_code} with \
                 {answered}"
            );
        }
        let stated = status(&router, key).await;
        assert_eq!(
            stated,
            json!({"S": "confirmed"}),
            "case {case:?}: a refused statement writes nothing, got {stated}"
        );
    }
}
