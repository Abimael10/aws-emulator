// SPDX-License-Identifier: CC0-1.0

//! How `DeleteItem` states a refusal about the legacy `Expected` map it guards the delete
//! with, driven end to end.
//!
//! The rule under test: whether a refusal about a legacy condition map carries the
//! `<N> validation error(s) detected: ` envelope follows the MEMBER the request stated its
//! condition through — the map a WRITE guards its payload with is validated inside the
//! envelope, the maps a READ narrows its result with are stated on their own — and not the
//! operation that carried the map. `DeleteItem` carries only the key its condition guards,
//! and states every clause its `Expected` map earns inside the envelope exactly as the two
//! writes that carry a payload beside the key do.
//!
//! The rows span what the rule generalises over on this one operation:
//!
//! * which part of an entry the clause is about — the members an expectation is stated
//!   through (the shape family) and the operands of a comparison it named;
//! * every clause of the shape family, each keyed by its own attribute name, so a clause
//!   that hard-codes one name or drops the envelope on one branch fails;
//! * and the CONTROLS: the same operand list inside a READ's legacy filter is stated
//!   WITHOUT the envelope (the member decides, not the operation), and an entry this service
//!   can read states a condition that is evaluated rather than refused.
//!
//! Nothing here is written back: the addressed item is read after every row to show the
//! refused delete removed nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table every row addresses.
const TABLE: &str = "DeleteExpectedEnvelope";

/// The envelope AWS states a request-validation clause inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The clause every refusal of a parameter value opens with.
const INVALID: &str = "One or more parameter values were invalid";

/// The one seeded item's key, which every row addresses.
fn key() -> Value {
    json!({"pk": {"S": "k1"}})
}

/// The emulator with [`TABLE`] holding one item carrying text and a number, so a readable
/// `Expected` entry has something to compare against.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "k1"}, "s": {"S": "beta"}, "n": {"N": "3"}}}),
    )
    .await;
    router
}

/// The seeded item as it stands right now, for the assertion that a refused delete removed
/// nothing.
async fn stored(router: &MultiServiceRouter) -> Value {
    ok(router, "GetItem", json!({"TableName": TABLE, "Key": key()}))
        .await
        .get("Item")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The whole `DeleteItem` request for one `Expected` map: the key alone, which is all this
/// operation carries beside its condition.
fn delete(expected: Value) -> Value {
    json!({"TableName": TABLE, "Key": key(), "Expected": expected})
}

/// RULE: every clause a `DeleteItem` request's `Expected` map earns is stated inside the
/// request-validation envelope, whichever part of an entry it is about.
#[tokio::test]
async fn every_clause_a_deletes_expected_map_earns_is_stated_inside_the_envelope() {
    let router = seeded().await;
    let before = stored(&router).await;

    // (case, the entry's attribute name, the entry, the clause it earns)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "an expectation of existence with no value",
            "alpha",
            json!({"Exists": true}),
            format!("{INVALID}: Value must be provided when Exists is true for Attribute: alpha"),
        ),
        (
            "a value beside an expectation of absence",
            "bravo",
            json!({"Exists": false, "Value": {"S": "beta"}}),
            format!("{INVALID}: Value cannot be used when Exists is false for Attribute: bravo"),
        ),
        (
            "an entry stating neither member",
            "charlie",
            json!({}),
            format!("{INVALID}: Value must be provided when Exists is null for Attribute: charlie"),
        ),
        (
            "an existence test beside a comparison",
            "delta",
            json!({"Exists": true, "ComparisonOperator": "NOT_NULL"}),
            format!(
                "{INVALID}: Exists and ComparisonOperator cannot be used together for \
                 Attribute: delta"
            ),
        ),
        (
            "a value beside the operands of a comparison",
            "echo",
            json!({"ComparisonOperator": "EQ", "Value": {"S": "beta"},
                   "AttributeValueList": [{"S": "beta"}]}),
            format!(
                "{INVALID}: Value and AttributeValueList cannot be used together for \
                 Attribute: echo"
            ),
        ),
        (
            "a comparison with nothing to compare",
            "foxtrot",
            json!({"ComparisonOperator": "EQ"}),
            format!(
                "{INVALID}: Value or AttributeValueList must be used with ComparisonOperator: \
                 EQ for Attribute: foxtrot"
            ),
        ),
        (
            "operands naming no comparison",
            "golf",
            json!({"AttributeValueList": [{"S": "beta"}]}),
            format!(
                "{INVALID}: AttributeValueList can only be used with a ComparisonOperator for \
                 Attribute: golf"
            ),
        ),
        (
            "operands of two different types",
            "hotel",
            json!({"ComparisonOperator": "IN",
                   "AttributeValueList": [{"S": "beta"}, {"N": "3"}]}),
            format!("{INVALID}: AttributeValues inside AttributeValueList must be of same type"),
        ),
    ];

    for (case, attribute, entry, clause) in rows {
        let expected = format!("{ENVELOPE}{clause}");
        let stated = crate::probe::refusal(
            &router,
            case,
            "DeleteItem",
            delete(json!({attribute: entry})),
        )
        .await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the message {expected:?}, got {stated:?}"
        );
    }

    let after = stored(&router).await;
    assert_eq!(
        after, before,
        "case the seeded item: a refused delete must remove nothing, expected {before:?}, got \
         {after:?}"
    );
}

/// The CONTROL on the rule: the same operand list in the member a READ narrows its result
/// with earns the same clause WITHOUT the envelope, so the envelope follows the member and
/// not the operation.
#[tokio::test]
async fn a_reads_legacy_filter_states_the_same_clause_on_its_own() {
    let router = seeded().await;
    let mixed = json!({"ComparisonOperator": "IN",
                       "AttributeValueList": [{"S": "beta"}, {"N": "3"}]});
    let clause = format!("{INVALID}: AttributeValues inside AttributeValueList must be of same type");

    let stated = crate::probe::refusal(
        &router,
        "a scan filtering on operands of two different types",
        "Scan",
        json!({"TableName": TABLE, "ScanFilter": {"s": mixed.clone()}}),
    )
    .await;
    assert_eq!(
        stated, clause,
        "case a read's legacy filter: expected the clause on its own, got {stated:?}"
    );

    // And the same list inside the write's map carries the envelope, in the very same
    // emulator: one list, two members, two forms of the same sentence.
    let on_the_write = crate::probe::refusal(
        &router,
        "a delete whose Expected map carries the same operands",
        "DeleteItem",
        delete(json!({"s": mixed})),
    )
    .await;
    assert_eq!(
        on_the_write,
        format!("{ENVELOPE}{clause}"),
        "case a write's condition map: expected the clause inside the envelope, got \
         {on_the_write:?}"
    );
}

/// The CONTROL for the family: an entry this service CAN read states a condition that is
/// EVALUATED, so the rows above are about how a refusal is stated and not about carrying a
/// legacy map at all.
#[tokio::test]
async fn an_expected_entry_the_service_can_read_states_a_condition_that_is_evaluated() {
    let router = seeded().await;

    // A condition the stored item does not satisfy: the evaluated refusal, a different error
    // class from every row above, and the item stays where it was.
    let stated = crate::probe::refusal_coded(
        &router,
        "a readable entry the item does not satisfy",
        "DeleteItem",
        delete(json!({"s": {"Value": {"S": "not-what-is-stored"}}})),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, "The conditional request failed",
        "case a readable entry the item does not satisfy: expected the evaluated refusal, got \
         {stated:?}"
    );
    assert!(
        stored(&router).await.is_object(),
        "case the seeded item: the refused delete must remove nothing"
    );

    // And a condition that HOLDS deletes the item, so the map really is being evaluated.
    ok(
        &router,
        "DeleteItem",
        delete(json!({"s": {"Value": {"S": "beta"}}})),
    )
    .await;
    let after = stored(&router).await;
    assert_eq!(
        after,
        Value::Null,
        "case a readable entry the item satisfies: expected the item to be deleted, got {after:?}"
    );
}
