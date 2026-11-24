// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SCOPE a batched read's parameter generations are read in.
//!
//! Driven over the wire (`POST /` with `X-Amz-Target: DynamoDB_20120810.BatchGetItem`)
//! through the same `MultiServiceRouter` the emulator's front doors use, so every assertion
//! is about what a client sees.
//!
//! RULE — `AttributesToGet` is "a legacy parameter. Use ProjectionExpression instead"
//! (`KeysAndAttributes$AttributesToGet`), and a request may not state one ask in both
//! generations: the refusal says the two may not be used "in the same request", so a
//! `BatchGetItem` is held to it over its WHOLE `RequestItems` map rather than entry by
//! entry — any entry's legacy projection beside any entry's `ProjectionExpression` is
//! refused, and a request that stays inside one generation is served however many entries
//! it addresses. The same scope settles the substitution table that resolves a
//! projection's aliases: `ExpressionAttributeNames` exists to be substituted into, so a
//! batch that sends one and names no `ProjectionExpression` anywhere is told to add the
//! expression.
//!
//! The rows span which entry carries which member, how many entries the batch addresses,
//! and — for the substitution table — whether any entry projects at all and whether the
//! entry that projects is the one that carries the table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The tables the rows address, each with one string partition key and the attributes the
/// projections name.
const FIRST: &str = "BatchGenerationsFirst";
const SECOND: &str = "BatchGenerationsSecond";
const THIRD: &str = "BatchGenerationsThird";

/// The sentence a request stating one ask in both generations earns, with both sides filled
/// in from the members it carried.
const MIXED: &str = "Can not use both expression and non-expression parameters in the same \
                     request: Non-expression parameters: {AttributesToGet} Expression \
                     parameters: {ProjectionExpression}";

/// The sentence a substitution table with no expression to be substituted into earns.
const NEEDS_AN_EXPRESSION: &str =
    "ExpressionAttributeNames can only be specified when using expressions";

/// An emulator holding the three active tables, each carrying one item with two non-key
/// attributes, so every projection has something to pick and something to leave out.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    for name in [FIRST, SECOND, THIRD] {
        ok(
            &router,
            "CreateTable",
            json!({
                "TableName": name,
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        )
        .await;
        describe_when_active(&router, name, name).await;
        ok(
            &router,
            "PutItem",
            json!({"TableName": name, "Item": {
                "pk": {"S": "k1"},
                "a": {"S": "A"},
                "b": {"S": "B"},
            }}),
        )
        .await;
    }
    router
}

/// One entry of a `RequestItems` map, addressing the one seeded key.
fn entry(members: Value) -> Value {
    let mut entry = json!({"Keys": [{"pk": {"S": "k1"}}]});
    for (member, value) in members.as_object().cloned().unwrap_or_default() {
        entry[member] = value;
    }
    entry
}

#[tokio::test]
async fn a_batch_read_states_its_asks_in_one_generation_over_the_whole_request() {
    let router = seeded().await;

    // Each row: the case, the `RequestItems` map and the sentence it earns. They span which
    // entry carries the legacy member, which carries the expression member, whether they
    // are the same entry, how many entries the batch addresses, and — for the substitution
    // table — whether the batch projects anywhere.
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "one entry carrying both members",
            json!({FIRST: entry(json!({"AttributesToGet": ["a"],
                                       "ProjectionExpression": "a"}))}),
            MIXED,
        ),
        (
            "the legacy member first and the expression member second",
            json!({
                FIRST: entry(json!({"AttributesToGet": ["a"]})),
                SECOND: entry(json!({"ProjectionExpression": "a"})),
            }),
            MIXED,
        ),
        (
            "the expression member first and the legacy member second",
            json!({
                FIRST: entry(json!({"ProjectionExpression": "a"})),
                SECOND: entry(json!({"AttributesToGet": ["a"]})),
            }),
            MIXED,
        ),
        (
            "three entries, the generations split across the outer two",
            json!({
                FIRST: entry(json!({"AttributesToGet": ["a"]})),
                SECOND: entry(json!({})),
                THIRD: entry(json!({"ProjectionExpression": "b"})),
            }),
            MIXED,
        ),
        (
            "a names table in a batch that projects nowhere",
            json!({FIRST: entry(json!({"ExpressionAttributeNames": {"#a": "a"}}))}),
            NEEDS_AN_EXPRESSION,
        ),
        (
            "a names table beside an entry projecting the legacy way",
            json!({
                FIRST: entry(json!({"ExpressionAttributeNames": {"#a": "a"}})),
                SECOND: entry(json!({"AttributesToGet": ["a"]})),
            }),
            NEEDS_AN_EXPRESSION,
        ),
        (
            "a names table beside the legacy projection of its own entry",
            json!({FIRST: entry(json!({"AttributesToGet": ["a"],
                                       "ExpressionAttributeNames": {"#a": "a"}}))}),
            NEEDS_AN_EXPRESSION,
        ),
    ];

    for (case, request_items, expected) in rows {
        let (status, answered) = call(
            &router,
            "BatchGetItem",
            json!({"RequestItems": request_items}),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", expected),
            "case {case:?}: expected {expected:?}, got {status} with {answered}"
        );
    }
}

/// One row of the served table: the case, the `RequestItems` map it sends, and what each
/// addressed table must report of its item.
type ServedRow = (&'static str, Value, Vec<(&'static str, Value)>);

#[tokio::test]
async fn a_batch_read_inside_one_generation_is_served_whatever_its_entries_are() {
    let router = seeded().await;

    // Each row: the case, the `RequestItems` map, and what each addressed table must report
    // of its item. Every row states its asks in ONE generation, so every one is served —
    // which is what makes the rows above the MIXING being refused and not the members
    // themselves. They span the generation, the number of entries, and which entry the
    // substitution table the projection dereferences sits in.
    let rows: Vec<ServedRow> = vec![
        (
            "one entry projecting the legacy way",
            json!({FIRST: entry(json!({"AttributesToGet": ["a"]}))}),
            vec![(FIRST, json!({"a": {"S": "A"}}))],
        ),
        (
            "two entries projecting the legacy way",
            json!({
                FIRST: entry(json!({"AttributesToGet": ["a"]})),
                SECOND: entry(json!({"AttributesToGet": ["b"]})),
            }),
            vec![
                (FIRST, json!({"a": {"S": "A"}})),
                (SECOND, json!({"b": {"S": "B"}})),
            ],
        ),
        (
            "two entries projecting the expression way",
            json!({
                FIRST: entry(json!({"ProjectionExpression": "a"})),
                SECOND: entry(json!({"ProjectionExpression": "b"})),
            }),
            vec![
                (FIRST, json!({"a": {"S": "A"}})),
                (SECOND, json!({"b": {"S": "B"}})),
            ],
        ),
        (
            "one entry projecting through its own names table beside one that projects bare",
            json!({
                FIRST: entry(json!({"ProjectionExpression": "#a",
                                    "ExpressionAttributeNames": {"#a": "a"}})),
                SECOND: entry(json!({"ProjectionExpression": "b"})),
            }),
            vec![
                (FIRST, json!({"a": {"S": "A"}})),
                (SECOND, json!({"b": {"S": "B"}})),
            ],
        ),
        (
            // The scope seam: the names table has no projection of ITS OWN, and what
            // admits it is the `ProjectionExpression` the OTHER entry states — the request
            // carries an expression, which is the scope the rule is read in. The
            // recordings pin the REFUSALS of this class, not this combination, so this row
            // states the seam this fix chose rather than an observed answer.
            "a names table with no projection of its own beside an entry that projects",
            json!({
                FIRST: entry(json!({"ExpressionAttributeNames": {"#a": "a"}})),
                SECOND: entry(json!({"ProjectionExpression": "b"})),
            }),
            vec![
                (
                    FIRST,
                    json!({"pk": {"S": "k1"}, "a": {"S": "A"}, "b": {"S": "B"}}),
                ),
                (SECOND, json!({"b": {"S": "B"}})),
            ],
        ),
        (
            "three entries, none of them projecting at all",
            json!({
                FIRST: entry(json!({})),
                SECOND: entry(json!({})),
                THIRD: entry(json!({})),
            }),
            vec![
                (
                    FIRST,
                    json!({"pk": {"S": "k1"}, "a": {"S": "A"}, "b": {"S": "B"}}),
                ),
                (
                    SECOND,
                    json!({"pk": {"S": "k1"}, "a": {"S": "A"}, "b": {"S": "B"}}),
                ),
                (
                    THIRD,
                    json!({"pk": {"S": "k1"}, "a": {"S": "A"}, "b": {"S": "B"}}),
                ),
            ],
        ),
    ];

    for (case, request_items, reported) in rows {
        let served = ok(
            &router,
            "BatchGetItem",
            json!({"RequestItems": request_items}),
        )
        .await;
        for (table, item) in reported {
            assert_eq!(
                served["Responses"][table][0], item,
                "case {case:?}: expected {table} to report {item}, got {served}"
            );
        }
        // "A map of tables and their respective keys that were not processed": empty when
        // every key the request addressed was read.
        assert_eq!(
            served["UnprocessedKeys"],
            json!({}),
            "case {case:?}: expected an empty UnprocessedKeys map, got {served}"
        );
    }
}
