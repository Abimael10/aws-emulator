// SPDX-License-Identifier: CC0-1.0

//! Two rules about an `UpdateExpression`'s arithmetic: the grouping its grammar admits, and
//! what the item-size gate charges the clause for.
//!
//! RULE 1 — the member groups with parentheses, so it is held to the grouping rule every
//! expression grammar of this service is held to: a pair that wraps nothing but another pair
//! groups what is already grouped, states nothing about how the expression is read, and is
//! refused HTTP 400 `ValidationException` with fixed text inside the envelope the members a
//! WRITE states its intent through carry — before anything is applied, so the addressed item
//! is left exactly as it stood. A SINGLE pair — the grouping this member's grammar does admit,
//! around the value a `SET` assigns — is read and the update applied.
//!
//! RULE 2 — an assignment whose operand is ARITHMETIC is charged, against the documented item
//! ceiling ("The maximum item size in DynamoDB is 400 KB"), for the OPERANDS it reads and
//! never for the digit width of the number it computes: the operands are what the caller sent,
//! while the result is the service's own. So the largest filler one such write may carry is
//! the same whether the arithmetic's result is narrower than its operands or carries into a
//! digit they do not have.
//!
//! The rows span what each rule generalises over: for the grouping, how deep the doubled pair
//! sits (one pair too many and two), which clause of a multi-clause update carries it, whether
//! it wraps a single operand or an arithmetic pair, and the single pairs that are not
//! redundant; for the ceiling, the operator (`+` and `-`) and the width of the computed result
//! against its operands' (narrower, equal, one digit wider), with the stored result read back
//! so the write is known to have computed it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table the grouping rows write to.
const GROUPED: &str = "UpdateGrouping";

/// The table the item-ceiling rows write to: a one-character partition key, so an item's own
/// overhead is as small as it can be.
const SIZED: &str = "ArithmeticCeiling";

/// The sentence the update path states for an item past the quota.
const OVER_THE_CEILING: &str = "Item size to update has exceeded the maximum allowed size";

/// The whole message a redundant pair of parentheses in an `UpdateExpression` earns: fixed
/// text, inside the envelope this member's refusals carry.
fn redundant_parentheses() -> String {
    "1 validation error detected: Invalid UpdateExpression: The expression has redundant \
     parentheses;"
        .to_string()
}

/// One stored item per row, so a refusal's "nothing was written" is read off that row's own
/// key.
fn key_of(row: usize) -> String {
    format!("k{row}")
}

/// Seed [`GROUPED`] with one item per row, each holding the same two numbers.
async fn seeded_grouping(rows: usize) -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, GROUPED, &[("pk", "HASH", "S")]).await;
    for row in 0..rows {
        ok(
            &router,
            "PutItem",
            json!({"TableName": GROUPED, "Item": {
                "pk": {"S": key_of(row)},
                "n": {"N": "1"},
                "m": {"N": "2"},
            }}),
        )
        .await;
    }
    router
}

/// The values a grouping row binds: only the placeholders its expression references, since a
/// value entry nothing dereferences is a defect of its own.
fn bound(expression: &str) -> Value {
    let every = json!({":v": {"N": "7"}, ":a": {"N": "3"}, ":b": {"N": "4"}});
    let referenced: Vec<(String, Value)> = every
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(placeholder, _)| expression.contains(placeholder.as_str()))
        .collect();
    Value::Object(referenced.into_iter().collect())
}

/// Drive one `UpdateItem` against the item of `row`.
async fn update(router: &MultiServiceRouter, row: usize, expression: &str) -> (u16, Value) {
    call(
        router,
        "UpdateItem",
        json!({"TableName": GROUPED, "Key": {"pk": {"S": key_of(row)}},
               "UpdateExpression": expression,
               "ExpressionAttributeValues": bound(expression)}),
    )
    .await
}

/// The item of `row` as the implemented reader reports it.
async fn stored(router: &MultiServiceRouter, row: usize) -> Value {
    ok(
        router,
        "GetItem",
        json!({"TableName": GROUPED, "Key": {"pk": {"S": key_of(row)}}, "ConsistentRead": true}),
    )
    .await["Item"]
        .clone()
}

#[tokio::test]
async fn doubled_parentheses_in_an_update_expression_are_refused_and_write_nothing() {
    // Each row: the expression, and where its doubled pair sits.
    let rows = [
        ("a whole operand wrapped twice", "SET n = ((:v))"),
        ("an arithmetic pair wrapped twice", "SET n = ((:a + :b))"),
        ("three pairs around one operand", "SET n = (((:v)))"),
        (
            "the second clause of two carrying the doubled pair",
            "SET m = :v, n = ((:v))",
        ),
        (
            "a doubled pair beside another section",
            "SET n = ((:v)) ADD m :a",
        ),
    ];
    let router = seeded_grouping(rows.len()).await;

    let expected = redundant_parentheses();
    for (at, (case, expression)) in rows.iter().enumerate() {
        let (status, body) = update(&router, at, expression).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {body}"
        );
        let item = stored(&router, at).await;
        assert_eq!(
            (item["n"].clone(), item["m"].clone()),
            (json!({"N": "1"}), json!({"N": "2"})),
            "case {case:?}: a refused update writes nothing, got {item}"
        );
    }
}

#[tokio::test]
async fn parentheses_that_say_something_are_read_and_the_update_applied() {
    // The CONTROLS: one pair is not a pair around a pair, whatever it groups — an arithmetic
    // operand pair, a single operand, a function call's argument — and the value each row
    // leaves at `n` is read back through the implemented reader.
    let rows = [
        ("an arithmetic pair grouped once", "SET n = (:a + :b)", "7"),
        ("a single operand grouped once", "SET n = (:v)", "7"),
        ("no parentheses at all", "SET n = :a + :b", "7"),
    ];
    let router = seeded_grouping(rows.len()).await;

    for (at, (case, expression, left)) in rows.iter().enumerate() {
        let (status, body) = update(&router, at, expression).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update must be applied, got {status} with {body}"
        );
        let item = stored(&router, at).await;
        assert_eq!(
            item["n"],
            json!({"N": left}),
            "case {case:?}: expected the clause's value at n, got {item}"
        );
    }
}

#[tokio::test]
async fn the_transactional_update_states_both_rules_the_single_item_one_does() {
    // The SECOND surface that states a change as an `UpdateExpression`: the `Update` action of
    // a transaction parses the same member and sizes the item it leaves the same way, so the
    // grouping rule and the arithmetic charge hold there too — the doubled pair writes
    // nothing, and the arithmetic one lands and is read back.
    let router = seeded_grouping(2).await;

    let refused = |expression: &str| {
        json!({"TransactItems": [{"Update": {
            "TableName": GROUPED,
            "Key": {"pk": {"S": key_of(0)}},
            "UpdateExpression": expression,
            "ExpressionAttributeValues": bound(expression),
        }}]})
    };
    // The clause is the same sentence; an ELEMENT's refusal is stated without the counted
    // envelope the single-item request carries, which is this service's rule for every
    // expression defect inside a transaction's element.
    let in_an_element = "Invalid UpdateExpression: The expression has redundant parentheses;";
    let (status, body) = call(&router, "TransactWriteItems", refused("SET n = ((:v))")).await;
    assert_eq!(
        (status, error_code(&body).as_str(), message(&body)),
        (400, "ValidationException", in_an_element.to_string()),
        "case a transactional update wrapping an operand twice: expected {in_an_element:?}, got \
         {status} with {body}"
    );
    let item = stored(&router, 0).await;
    assert_eq!(
        (item["n"].clone(), item["m"].clone()),
        (json!({"N": "1"}), json!({"N": "2"})),
        "case a transactional update wrapping an operand twice: nothing is written, got {item}"
    );

    // The CONTROL on the same surface: the single pair the grammar admits around an
    // arithmetic operand pair is read, and the number it computes is stored.
    let applied = "SET n = (:a + :b)";
    let (status, body) = call(
        &router,
        "TransactWriteItems",
        json!({"TransactItems": [{"Update": {
            "TableName": GROUPED,
            "Key": {"pk": {"S": key_of(1)}},
            "UpdateExpression": applied,
            "ExpressionAttributeValues": bound(applied),
        }}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a transactional arithmetic update: expected it to be applied, got {status} with \
         {body}"
    );
    let item = stored(&router, 1).await;
    assert_eq!(
        item["n"],
        json!({"N": "7"}),
        "case a transactional arithmetic update: expected the computed number at n, got {item}"
    );
}

// ===========================================================================
// The item-size ceiling an arithmetic clause is held to
// ===========================================================================

/// The alphabet the size probes draw their partition keys from, each key ONE character long
/// so every probe's item carries the same key bytes.
///
/// A probe gets a key of its own: an item this size is hundreds of the write units one
/// partition may take in a second, so writing every probe to one key would measure the
/// throughput ceiling instead of the size one.
const PROBE_KEYS: &str = "abcdefghijklmnopqrstuvwxyz0123456789";

/// The probes of one boundary measurement.
struct Prober<'r> {
    router: &'r MultiServiceRouter,
    taken: usize,
}

impl Prober<'_> {
    /// A key no earlier probe wrote to.
    fn key(&mut self) -> String {
        let at = self.taken;
        self.taken += 1;
        PROBE_KEYS
            .chars()
            .nth(at)
            .expect("a boundary is read off fewer probes than this alphabet has keys")
            .to_string()
    }

    /// One probe: `SET v = :v, n = <left> <operator> <right>` with a filler of `length`
    /// bytes, against a fresh key. The key it used comes back, so a caller can read the item
    /// the probe left.
    async fn probe(
        &mut self,
        operator: &str,
        left: &str,
        right: &str,
        length: usize,
    ) -> (String, u16, Value) {
        let key = self.key();
        let (status, body) = call(
            self.router,
            "UpdateItem",
            json!({"TableName": SIZED, "Key": {"pk": {"S": key.clone()}},
                   "UpdateExpression": format!("SET v = :v, n = :x {operator} :y"),
                   "ExpressionAttributeValues": {
                       ":v": {"S": "x".repeat(length)},
                       ":x": {"N": left},
                       ":y": {"N": right},
                   }}),
        )
        .await;
        (key, status, body)
    }

    /// The largest filler the clause accepts, bisected inside a bracket every row below falls
    /// in.
    async fn largest_accepted(&mut self, operator: &str, left: &str, right: &str) -> usize {
        let (mut low, mut high) = (409_400usize, 409_650usize);
        while low < high {
            let at = low + (high - low).div_ceil(2);
            let (_, status, _) = self.probe(operator, left, right, at).await;
            if status == 200 {
                low = at;
            } else {
                high = at - 1;
            }
        }
        low
    }
}

#[tokio::test]
async fn an_arithmetic_clauses_ceiling_does_not_move_with_the_result_it_computes() {
    let router = emulator();
    create_active_table(&router, SIZED, &[("pk", "HASH", "S")]).await;
    let mut prober = Prober {
        router: &router,
        taken: 0,
    };

    // Each row: the operator and its two operands, every operand at most two significant
    // digits — so the operands are charged the same bytes throughout ("1 byte per two
    // significant digits + 1 byte", the Developer Guide's number sizing) — and the result
    // they compute, which spans one digit fewer than the operands, as many, and one more.
    let rows = [
        ("two 2-digit operands summing to 2 digits", "+", "49", "49", "98"),
        ("two 2-digit operands summing to 3 digits", "+", "99", "99", "198"),
        ("a 2-digit and a 1-digit operand carrying over", "+", "99", "1", "100"),
        ("two 1-digit operands summing to 1 digit", "+", "1", "1", "2"),
        ("a difference of two 2-digit operands", "-", "99", "1", "98"),
        ("a difference that computes a single digit", "-", "50", "49", "1"),
    ];

    // The boundary, read off the first row: the largest filler it accepts.
    let (case, operator, left, right, _) = rows[0];
    let boundary = prober.largest_accepted(operator, left, right).await;
    assert!(
        (409_400..409_650).contains(&boundary),
        "case {case:?}: expected a boundary inside the bracketed range, got {boundary}"
    );

    // Every row's boundary is THAT boundary: the filler at it is stored and one byte more is
    // refused, whichever number the arithmetic computed.
    for (case, operator, left, right, computed) in rows {
        let (key, status, body) = prober.probe(operator, left, right, boundary).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a filler of {boundary} bytes must be accepted, as it is for every \
             other result width, got {status} with {body}"
        );
        // The clause really computed its number, read back through the implemented reader.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": SIZED, "Key": {"pk": {"S": key}},
                   "ProjectionExpression": "n", "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            read["Item"]["n"],
            json!({"N": computed}),
            "case {case:?}: expected the computed number at n, got {read}"
        );
        let (_, status, body) = prober.probe(operator, left, right, boundary + 1).await;
        assert_eq!(
            (status, message(&body)),
            (400, OVER_THE_CEILING.to_string()),
            "case {case:?}: one byte more must be refused, so the boundary sits exactly at \
             {boundary}, got {status} with {body}"
        );
    }
}
