// SPDX-License-Identifier: CC0-1.0

//! Which SIDE of a transaction the 4 MB aggregate refusal speaks about.
//!
//! The rule under test: a transaction refused for "the aggregate size of the items in the
//! transaction" ("Service, Account, and Table Quotas") reports the sentence AWS words for
//! the side the ceiling is on. A transactional READ's aggregate is what the call would hand
//! BACK, so its refusal is the fixed sentence "Payload in response is too large." with
//! nothing of the call interpolated — no byte count, no item count, no table or key name —
//! which is what lets a client key on it and split its read set. A transactional WRITE's
//! aggregate is the payload the request CARRIED, so its own refusal names that payload, and
//! keeps doing so.
//!
//! Both are top-level `ValidationException`s (HTTP 400) with no `CancellationReasons`,
//! because no action of the transaction was applicable; neither reads nor writes anything.
//!
//! The rows span what the rule generalises over — the per-item size, how many items the call
//! addresses, how far past the aggregate it goes, how many tables it spans, and whether
//! every key it addresses holds an item — and assert the READ sentence is the same fixed
//! text for all of them.
//!
//! Each item's stored size is computed from the published sizing rule ("Item sizes and
//! formats": an item's size is "the sum of the lengths of its attribute names and values").

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The documented aggregate, 4 MB.
const MAX_TRANSACTION_BYTES: usize = 4 * 1024 * 1024;

/// AWS's fixed sentence for the aggregate a transactional READ would answer.
const RESPONSE_TOO_LARGE: &str = "Payload in response is too large.";

/// The opening of the transactional WRITE's own refusal, which names the payload the request
/// carried; the payload itself is read back off the message rather than transcribed.
const REQUEST_TOO_LARGE_PREFIX: &str = "Transaction payload size cannot exceed 4MB. Payload Size: ";

/// The bytes an item of this suite stores beside its payload value: the key attribute's name
/// and value and the payload attribute's name.
fn overhead(key: &str) -> usize {
    "pk".len() + key.len() + "v".len()
}

/// Create `table` with one string partition key and wait until it is ACTIVE.
async fn partitioned(router: &MultiServiceRouter, table: &str) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": table,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, table, table).await;
}

/// Store an item under `key` whose stored size is `stored` bytes.
async fn seed(router: &MultiServiceRouter, table: &str, key: &str, stored: usize) {
    let payload = stored - overhead(key);
    ok(
        router,
        "PutItem",
        json!({"TableName": table, "Item": {
            "pk": {"S": key},
            "v": {"S": "x".repeat(payload)},
        }}),
    )
    .await;
}

/// A `Get` of `key` in `table`.
fn get(table: &str, key: &str) -> Value {
    json!({"Get": {"TableName": table, "Key": {"pk": {"S": key}}}})
}

/// One row of the read arm: the case, the `(table, key, stored size)` triples to seed, and
/// the `(table, key)` pairs the transaction addresses — some of which may hold nothing.
struct Row {
    case: &'static str,
    seeded: Vec<(&'static str, String, usize)>,
    addressed: Vec<(&'static str, String)>,
}

/// `count` keys named for `row`.
fn keys(row: &str, count: usize) -> Vec<String> {
    (0..count).map(|at| format!("{row}-{at:03}")).collect()
}

#[tokio::test]
async fn a_transactional_read_past_the_aggregate_states_the_response_side_sentence() {
    const ONE: &str = "RefusalSideOne";
    const TWO: &str = "RefusalSideTwo";

    // Eleven items a little under 400 KB each: each inside the published item ceiling, only
    // their sum past the aggregate.
    let many = keys("many", 11);
    // Fifty items of 90 KB: many more items, each a quarter the size.
    let few = keys("small", 50);
    // Twelve items spread over two tables, six each.
    let spread = keys("spread", 12);
    // Eleven stored items addressed together with four keys that hold nothing.
    let holes = keys("holes", 11);

    let rows = vec![
        Row {
            case: "eleven items of ~390 KB, just past the aggregate",
            seeded: many
                .iter()
                .map(|key| (ONE, key.clone(), 390 * 1024))
                .collect(),
            addressed: many.iter().map(|key| (ONE, key.clone())).collect(),
        },
        Row {
            case: "fifty items of 90 KB, a third of a megabyte past it",
            seeded: few
                .iter()
                .map(|key| (ONE, key.clone(), 90 * 1024))
                .collect(),
            addressed: few.iter().map(|key| (ONE, key.clone())).collect(),
        },
        Row {
            case: "twelve items of ~390 KB spread over two tables",
            seeded: spread
                .iter()
                .enumerate()
                .map(|(at, key)| (if at % 2 == 0 { ONE } else { TWO }, key.clone(), 390 * 1024))
                .collect(),
            addressed: spread
                .iter()
                .enumerate()
                .map(|(at, key)| (if at % 2 == 0 { ONE } else { TWO }, key.clone()))
                .collect(),
        },
        Row {
            case: "eleven stored items beside four keys that hold nothing",
            seeded: holes
                .iter()
                .map(|key| (TWO, key.clone(), 390 * 1024))
                .collect(),
            addressed: holes
                .iter()
                .map(|key| (TWO, key.clone()))
                .chain(keys("absent", 4).into_iter().map(|key| (TWO, key)))
                .collect(),
        },
    ];

    let router = emulator();
    partitioned(&router, ONE).await;
    partitioned(&router, TWO).await;

    for row in &rows {
        for (table, key, stored) in &row.seeded {
            seed(&router, table, key, *stored).await;
        }
        // The aggregate the call would answer follows from the sizing rule: only the keys
        // that hold an item contribute.
        let aggregate: usize = row.seeded.iter().map(|(_, _, stored)| stored).sum();
        assert!(
            aggregate > MAX_TRANSACTION_BYTES,
            "case {:?}: the row must address more than the aggregate, got {aggregate}",
            row.case
        );

        let items: Vec<Value> = row
            .addressed
            .iter()
            .map(|(table, key)| get(table, key))
            .collect();
        let (status, body) = call(&router, "TransactGetItems", json!({"TransactItems": items})).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", RESPONSE_TOO_LARGE.to_string()),
            "case {:?}: expected the response-side refusal {RESPONSE_TOO_LARGE:?}, got {status} \
             with {body}",
            row.case
        );
        assert!(
            body.get("CancellationReasons").is_none(),
            "case {:?}: a top-level refusal has nothing to cancel, got {body}",
            row.case
        );
        assert!(
            body.get("Responses").is_none(),
            "case {:?}: a refused transaction reports no item, got {body}",
            row.case
        );
        // The refusal reports nothing of the run, so none of the sizes it measured leaks
        // into the sentence.
        let stated = message(&body);
        for figure in [
            aggregate.to_string(),
            row.addressed.len().to_string(),
            MAX_TRANSACTION_BYTES.to_string(),
        ] {
            assert!(
                !stated.contains(&figure),
                "case {:?}: the sentence is fixed text, but {figure:?} is in {stated:?}",
                row.case
            );
        }

        // Nothing was read: every stored item the refused call addressed still reads back
        // whole, and the keys that held nothing still hold nothing.
        for (table, key, stored) in &row.seeded {
            let read = ok(
                &router,
                "GetItem",
                json!({"TableName": table, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
            )
            .await;
            let len = read["Item"]["v"]["S"].as_str().map(str::len);
            assert_eq!(
                len,
                Some(stored - overhead(key)),
                "case {:?}: {key:?} must still be readable whole after the refusal, got \
                 a payload of {len:?} bytes",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_transactional_write_past_the_aggregate_keeps_naming_the_payload_it_carried() {
    const TABLE: &str = "RefusalSideWrite";
    let router = emulator();
    partitioned(&router, TABLE).await;

    // Each row: the case, the keys it writes, and the stored size of each item it carries.
    let rows: Vec<(&str, Vec<String>, usize)> = vec![
        ("eleven puts of ~390 KB", keys("write-many", 11), 390 * 1024),
        ("fifty puts of 90 KB", keys("write-small", 50), 90 * 1024),
    ];

    for (case, written, stored) in &rows {
        let puts: Vec<Value> = written
            .iter()
            .map(|key| {
                json!({"Put": {"TableName": TABLE, "Item": {
                    "pk": {"S": key},
                    "v": {"S": "x".repeat(stored - overhead(key))},
                }}})
            })
            .collect();
        let carried: usize = written.len() * stored;
        assert!(
            carried > MAX_TRANSACTION_BYTES,
            "case {case:?}: the row must carry more than the aggregate, got {carried}"
        );

        let (status, body) = call(&router, "TransactWriteItems", json!({"TransactItems": puts})).await;
        let stated = message(&body);
        let reported: Option<usize> = stated
            .strip_prefix(REQUEST_TOO_LARGE_PREFIX)
            .and_then(|tail| tail.parse().ok());
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the request-payload refusal, got {status} with {body}"
        );
        assert!(
            reported.is_some_and(|bytes| bytes > MAX_TRANSACTION_BYTES),
            "case {case:?}: expected {REQUEST_TOO_LARGE_PREFIX:?} followed by a payload past \
             the aggregate, got {stated:?}"
        );
        assert_ne!(
            stated, RESPONSE_TOO_LARGE,
            "case {case:?}: the write side's ceiling is on the request it carried, so it does \
             not report the response-side sentence, got {stated:?}"
        );

        for key in written {
            let read = ok(
                &router,
                "GetItem",
                json!({"TableName": TABLE, "Key": {"pk": {"S": key}}, "ConsistentRead": true}),
            )
            .await;
            assert_eq!(
                read.get("Item"),
                None,
                "case {case:?}: nothing of a refused transaction is stored, but {key:?} is now \
                 readable, got {read}"
            );
        }
    }
}
