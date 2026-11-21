// SPDX-License-Identifier: CC0-1.0

//! The documented 4 MB aggregate a transactional READ is held to.
//!
//! The rule under test: "the aggregate size of the items in the transaction cannot exceed 4
//! MB" and "DynamoDB rejects the entire TransactGetItems request if [...] the aggregate size
//! of the items in the transaction exceeded 4 MB" (the `TransactGetItems` doc). So a call
//! whose items add up to exactly the aggregate is served and reports every one of them,
//! while one byte more is refused 400 `ValidationException` naming that quota and the
//! payload the call measured, with no item reported — and the quota is on the items the
//! transaction READS, so a key that holds no item adds nothing to it.
//!
//! The rows span what the rule generalises over: the per-item size (items at the stored size
//! this suite writes them at, and small ones), the number of items the call addresses, how
//! far past the aggregate the call goes (one byte, and a quarter of a megabyte), and whether
//! every key it addresses holds an item.
//!
//! Each item's stored size is computed from the published sizing rule ("Item sizes and
//! formats": an item's size is "the sum of the lengths of its attribute names and values"),
//! so the aggregate a row asserts follows from the rule rather than from a measurement.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row reads from: one string partition key.
const TABLE: &str = "TransactGetAggregate";

/// The documented aggregate, 4 MB ("the aggregate size of the items in the transaction
/// cannot exceed 4 MB").
const MAX_TRANSACTION_BYTES: i64 = 4 * 1024 * 1024;

/// The sentence a transaction past that aggregate is refused with, over the payload the
/// call measured: the same sentence a transactional WRITE states for the same quota
/// (`state::transaction_size_exceeded_message`), where the payload a READ measures is the
/// size of the items the store held for the keys it named.
fn too_large(payload: i64) -> String {
    format!("Transaction payload size cannot exceed 4MB. Payload Size: {payload}")
}

/// How many bytes each key VALUE carries, so every item's key portion is the same length.
const KEY_VALUE_BYTES: usize = 10;

/// The bytes one item stores beside its payload's value: its two-byte key name and
/// [`KEY_VALUE_BYTES`] of key value, plus the payload attribute's one-byte name.
const ITEM_WITHOUT_PAYLOAD: i64 = 2 + KEY_VALUE_BYTES as i64 + 1;

/// The stored size of the items the boundary rows read: a sixteenth of the aggregate, so
/// sixteen of them come to exactly the documented ceiling.
const PART_OF_THE_AGGREGATE: i64 = MAX_TRANSACTION_BYTES / 16;

/// The stored size of the small items the control row reads.
const SMALL_ITEM_BYTES: i64 = 1024;

/// The key of item `at`, as [`KEY_VALUE_BYTES`] bytes.
fn key_value(at: usize) -> String {
    format!("item-{at:05}")
}

/// A `Get` of item `at`.
fn get(at: usize) -> Value {
    json!({"Get": {"TableName": TABLE, "Key": {"pk": {"S": key_value(at)}}}})
}

/// Store item `at` with a payload that leaves it `stored` bytes long.
async fn seed(router: &MultiServiceRouter, at: usize, stored: i64) {
    let payload = usize::try_from(stored - ITEM_WITHOUT_PAYLOAD).expect("a positive payload");
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": key_value(at)},
            "v": {"S": "x".repeat(payload)},
        }}),
    )
    .await;
}

/// The emulator with the table in place and the items every row reads stored in it: sixteen
/// at a sixteenth of the aggregate, one of them a byte longer, and two small ones.
///
/// The keys are laid out so each row below addresses a run of them: `0..16` are the
/// sixteenth-sized items, `16` is the one that is a byte longer, and `17..19` are small.
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
    describe_when_active(&router, "the seeded table", TABLE).await;
    for at in 0..16 {
        seed(&router, at, PART_OF_THE_AGGREGATE).await;
    }
    seed(&router, 16, PART_OF_THE_AGGREGATE + 1).await;
    for at in 17..19 {
        seed(&router, at, SMALL_ITEM_BYTES).await;
    }
    router
}

#[tokio::test]
async fn a_transactional_read_at_the_documented_aggregate_is_served() {
    let router = seeded().await;

    // Each row: the case, the items it addresses, and how many of them hold an item.
    let rows: Vec<(&str, Vec<usize>, usize)> = vec![
        (
            "sixteen items summing to exactly the aggregate",
            (0..16).collect(),
            16,
        ),
        (
            "a handful of small items, far under it",
            (17..19).collect(),
            2,
        ),
        (
            // The quota counts the items the transaction READ, so the keys that hold
            // nothing leave the call well under it however large the rest are.
            "the same sixteen keys with two of them holding nothing",
            (0..14).chain([40, 41]).collect(),
            14,
        ),
    ];

    for (case, addressed, holding) in rows {
        let items: Vec<Value> = addressed.iter().map(|at| get(*at)).collect();
        let answered = ok(
            &router,
            "TransactGetItems",
            json!({"TransactItems": items}),
        )
        .await;
        let responses = answered["Responses"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            responses.len(),
            addressed.len(),
            "case {case:?}: one response per addressed key, got {}",
            responses.len()
        );
        let found = responses
            .iter()
            .filter(|response| response.get("Item").is_some())
            .count();
        assert_eq!(
            found, holding,
            "case {case:?}: expected {holding} of the keys to report an item, got {found}"
        );
    }
}

#[tokio::test]
async fn a_transactional_read_past_the_documented_aggregate_is_refused() {
    let router = seeded().await;

    // Each row: the case, the items it addresses, and the aggregate those items come to —
    // one byte past the ceiling, and a sixteenth of the aggregate past it.
    let rows: Vec<(&str, Vec<usize>, i64)> = vec![
        (
            "sixteen items, one of which is a single byte longer",
            (0..15).chain([16]).collect(),
            15 * PART_OF_THE_AGGREGATE + PART_OF_THE_AGGREGATE + 1,
        ),
        (
            "seventeen items, a whole part past the ceiling",
            (0..17).collect(),
            16 * PART_OF_THE_AGGREGATE + PART_OF_THE_AGGREGATE + 1,
        ),
    ];

    for (case, addressed, aggregate) in rows {
        let items: Vec<Value> = addressed.iter().map(|at| get(*at)).collect();
        let (status, body) = call(
            &router,
            "TransactGetItems",
            json!({"TransactItems": items}),
        )
        .await;
        assert!(
            aggregate > MAX_TRANSACTION_BYTES,
            "case {case:?}: the row must address more than the aggregate, got {aggregate}"
        );
        let expected = too_large(aggregate);
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected the aggregate refusal {expected:?}, got {status} with \
             {body}"
        );
        assert!(
            body.get("Responses").is_none(),
            "case {case:?}: a refused transaction reports no item, got {body}"
        );
    }

    // The CONTROL: the ceiling is a quota on the aggregate and not on the call, so the
    // largest run of these items that stays inside it is still served after both refusals.
    let items: Vec<Value> = (0..16).map(get).collect();
    let answered = ok(
        &router,
        "TransactGetItems",
        json!({"TransactItems": items}),
    )
    .await;
    assert_eq!(
        answered["Responses"].as_array().map(Vec::len),
        Some(16),
        "case \"the served control\": the run inside the aggregate is answered whole, got \
         {answered}"
    );
}
