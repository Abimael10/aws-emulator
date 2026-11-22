// SPDX-License-Identifier: CC0-1.0

//! How a refusal about the SHAPE of a legacy `Expected` entry is stated, driven end to end
//! over the three conditional writes.
//!
//! The rule under test: an `Expected` entry states its expectation through `Value`, `Exists`,
//! `ComparisonOperator` and `AttributeValueList` ("Value and Exists are incompatible with
//! AttributeValueList and ComparisonOperator. Note that if you use both sets of parameters at
//! once, DynamoDB will return a ValidationException"), and an entry whose members cannot be
//! read as an expectation is refused 400 `ValidationException` with the clause for the rule it
//! broke, opening with the fixed `One or more parameter values were invalid: ` and naming the
//! attribute whose entry carried it — the one part taken from the request. Whether that clause
//! is stated inside the `<N> validation error(s) detected: ` envelope follows the OPERATION the
//! map arrived on: the writes that carry the item or the update the condition guards state it
//! inside the envelope, and `DeleteItem`, whose request carries only the key the condition
//! guards, states the clause on its own.
//!
//! The rows span what the rule generalises over:
//!
//! * every clause of the family — an expectation of existence with no value, a value beside an
//!   expectation of absence, an entry stating neither member, the two pairs that state one half
//!   of the condition twice, a comparison with nothing to compare, and operands naming no
//!   comparison;
//! * all three conditional writes, each carrying the payload its own model requires;
//! * the ATTRIBUTE the entry is keyed by, which is the clause's only variable part — one row
//!   per clause keys the entry by a different name, so a clause that hard-codes one fails;
//! * and the control: an entry this service CAN read states a condition that is evaluated, so
//!   the refusals above are about the entry's shape rather than about the legacy map itself.
//!
//! Nothing here is written back: every row reads the addressed item afterwards to show the
//! refused write stored nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{emulator, ok};
use crate::table_items::create_active_table;

/// The table every row addresses.
const TABLE: &str = "LegacyExpectedShape";

/// The envelope AWS states a request-validation clause inside.
const ENVELOPE: &str = "1 validation error detected: ";

/// The clause every refusal of a parameter value opens with.
const INVALID: &str = "One or more parameter values were invalid";

/// The one seeded item's key, which every row addresses.
fn key() -> Value {
    json!({"pk": {"S": "k1"}})
}

/// The emulator with [`TABLE`] holding one item: `s` carries text, so a readable `Expected`
/// entry has something to compare against.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "k1"}, "s": {"S": "beta"}}}),
    )
    .await;
    router
}

/// The seeded item as it stands right now, for the assertion that a refused write stored
/// nothing.
async fn stored(router: &MultiServiceRouter) -> Value {
    ok(router, "GetItem", json!({"TableName": TABLE, "Key": key()}))
        .await
        .get("Item")
        .cloned()
        .unwrap_or(Value::Null)
}

/// One conditional write, with the payload beside its condition that its own model requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    Put,
    Update,
    Delete,
}

/// Every conditional write that carries the legacy `Expected` map.
const WRITES: [Write; 3] = [Write::Put, Write::Update, Write::Delete];

impl Write {
    fn operation(self) -> &'static str {
        match self {
            Self::Put => "PutItem",
            Self::Update => "UpdateItem",
            Self::Delete => "DeleteItem",
        }
    }

    /// Does this write state a clause about an `Expected` entry's shape inside the
    /// request-validation envelope? The two that carry a payload beside the key do; the
    /// delete, which carries only the key, states the clause on its own.
    fn states_the_envelope(self) -> bool {
        self != Self::Delete
    }

    /// The whole request this write sends with `expected` as its condition: the item it
    /// stores, the legacy update it applies, or the key alone — `Expected` and the expression
    /// members cannot be used in one request, so the update states its write the legacy way.
    fn request(self, expected: Value) -> Value {
        let mut request = match self {
            Self::Put => json!({"Item": {"pk": {"S": "k1"}, "s": {"S": "written"}}}),
            Self::Update => json!({
                "Key": key(),
                "AttributeUpdates": {"s": {"Action": "PUT", "Value": {"S": "gamma"}}},
            }),
            Self::Delete => json!({"Key": key()}),
        };
        request["TableName"] = json!(TABLE);
        request["Expected"] = expected;
        request
    }

    /// The whole message this write answers `clause` with.
    fn message(self, clause: &str) -> String {
        if self.states_the_envelope() {
            format!("{ENVELOPE}{clause}")
        } else {
            clause.to_string()
        }
    }
}

/// RULE: every clause of the `Expected`-entry shape family is stated inside the
/// request-validation envelope by the writes that carry a payload beside the key, and on its
/// own by the delete — with the attribute the entry was keyed by as the one variable part.
#[tokio::test]
async fn an_expected_entrys_shape_clause_follows_the_operation_it_arrived_on() {
    let router = seeded().await;
    let before = stored(&router).await;

    // (case, the attribute the entry is keyed by, the entry, the clause it earns). Every row
    // keys its entry by its own attribute name, so a clause that states some other name — or
    // none — fails here.
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
    ];

    for (case, attribute, entry, clause) in rows {
        for write in WRITES {
            let case = format!("{case} on {}", write.operation());
            let expected = write.message(&clause);
            let stated = crate::probe::refusal(
                &router,
                &case,
                write.operation(),
                write.request(json!({attribute: entry.clone()})),
            )
            .await;
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the message {expected:?}, got {stated:?}"
            );
        }
    }

    let after = stored(&router).await;
    assert_eq!(
        after, before,
        "case the seeded item: a refused write must store nothing, expected {before:?}, got \
         {after:?}"
    );
}

/// The CONTROL for the rule: an entry this service CAN read states a condition that is
/// EVALUATED, so each write above was refused for the entry's shape rather than for carrying a
/// legacy map at all.
#[tokio::test]
async fn an_expected_entry_the_service_can_read_states_a_condition_that_is_evaluated() {
    // A condition that HOLDS for the stored item, through each write: the request is served.
    // Each write gets its own seeded store, so one row's write cannot decide the next row's
    // condition.
    for write in [Write::Put, Write::Update] {
        let router = seeded().await;
        let case = format!("a readable entry on {}", write.operation());
        ok(
            &router,
            write.operation(),
            write.request(json!({"s": {"Value": {"S": "beta"}}})),
        )
        .await;
        let item = stored(&router).await;
        assert!(
            item.is_object(),
            "case {case:?}: the write the condition admitted must leave the item in place, got \
             {item:?}"
        );
    }

    let router = seeded().await;
    // And a condition the stored item does NOT satisfy is the evaluated refusal, which is a
    // different error class from every row above.
    let stated = crate::probe::refusal_coded(
        &router,
        "a readable entry the item does not satisfy",
        "DeleteItem",
        Write::Delete.request(json!({"s": {"Value": {"S": "not-what-is-stored"}}})),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, "The conditional request failed",
        "case a readable entry the item does not satisfy: expected the evaluated refusal, got \
         {stated:?}"
    );
    let item = stored(&router).await;
    assert!(
        item.is_object(),
        "case the seeded item: the refused delete must remove nothing, got {item:?}"
    );
}
