// SPDX-License-Identifier: CC0-1.0

//! What a vector index HOLDS for a coordinate, and what a search through it reports,
//! driven end to end.
//!
//! RULE under test: "each element is a 32-bit IEEE-754 floating point number"
//! (`SearchVectors$SearchVector`, the same shape a vector index's `VectorAttribute` names
//! on an item), so what the index stores for a coordinate is the float that type can hold
//! and not the decimal the request spelled: a number no 32-bit float represents exactly is
//! held as the one it rounds to, and a search through the index reports THAT. The table's
//! own item is untouched by this — the item store holds the decimal the write carried, and
//! `GetItem` answers it — so the two differ exactly where the rounding does.
//!
//! The rows span what the rule generalises over: a coordinate a 32-bit float holds exactly
//! (a whole number, a power of two, a half), the first whole number it does NOT hold
//! (2^24 + 1, which rounds down to 2^24) and its neighbours, a value with more decimal
//! digits than that type carries, a negative value, and zero — each written as the decimal
//! a client sends and read back from the search result.

use serde_json::{json, Value};

use crate::emulator::{emulator, ok, POLL_INTERVAL};

/// The table every row below searches.
const TABLE: &str = "VectorFloat32Components";

/// The index over the `emb` attribute, which projects everything so a search reports the
/// whole entry it holds.
const INDEX: &str = "float32-index";

/// How long a poll for the `CREATING` -> `ACTIVE` transitions the clock owes may take
/// before this suite calls it a hang. Generous slack over the emulator's own span: a loaded
/// machine is not a defect, and what the bound is for is that the wait ENDS.
const READY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// One row: the case, the decimal the item is written with, and the decimal the index is
/// expected to report for it.
struct Row {
    case: &'static str,
    written: &'static str,
    held: &'static str,
}

/// The rows. The float32 value of each written decimal is the nearest `f32` to it, which is
/// what the index holds: 2^24 + 1 (16777217) is the first whole number that type cannot
/// represent, so it is held as 2^24 (16777216), while 2^24 itself is exact.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "zero",
            written: "0",
            held: "0.0",
        },
        Row {
            case: "a whole number a 32-bit float holds exactly",
            written: "7",
            held: "7.0",
        },
        Row {
            case: "a half, which that type holds exactly",
            written: "0.5",
            held: "0.5",
        },
        Row {
            case: "a negative whole number",
            written: "-3",
            held: "-3.0",
        },
        Row {
            case: "2^24, the largest power of two still exact beside its neighbours",
            written: "16777216",
            held: "16777216.0",
        },
        Row {
            case: "2^24 + 1, the first whole number the type cannot represent",
            written: "16777217",
            held: "16777216.0",
        },
        Row {
            case: "2^24 + 2, which the type holds exactly again",
            written: "16777218",
            held: "16777218.0",
        },
        Row {
            case: "a value with more decimal digits than the type carries",
            written: "1.2345678901234567",
            held: "1.2345679",
        },
    ]
}

/// The `pk` the row at `at` is stored under.
fn key_of(at: usize) -> String {
    format!("row{at}")
}

/// Poll `DescribeTable` until the table and its vector index both report `ACTIVE`.
async fn ready(router: &smithy_server_runtime::MultiServiceRouter) {
    let deadline = std::time::Instant::now() + READY_DEADLINE;
    loop {
        let described = ok(router, "DescribeTable", json!({"TableName": TABLE})).await;
        let table_active = described["Table"]["TableStatus"] == json!("ACTIVE");
        let index_active = described["Table"]["VectorIndexes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|declared| {
                declared["IndexName"] == json!(INDEX) && declared["IndexStatus"] == json!("ACTIVE")
            });
        if table_active && index_active {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the table and its vector index never reached ACTIVE, last {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The item the search answered for `pk`, or `Value::Null` where it answered none.
fn result_for(answered: &Value, pk: &str) -> Value {
    answered["SearchResults"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|result| result["Item"]["pk"]["S"] == json!(pk))
        .map(|result| result["Item"].clone())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_vector_index_reports_the_float32_each_coordinate_is_held_as() {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [{
                "IndexName": INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 2,
                "DistanceFunction": "DOT_PRODUCT",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    ready(&router).await;

    // One item per row: the coordinate the row writes, beside a second coordinate every
    // row shares, so the vector has the index's own `Dimensions`.
    for (at, row) in rows().iter().enumerate() {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": key_of(at)},
                "emb": {"L": [{"N": row.written}, {"N": "1"}]},
            }}),
        )
        .await;
    }

    let answered = ok(
        &router,
        "SearchVectors",
        json!({
            "TableName": TABLE,
            "IndexName": INDEX,
            "SearchVector": [{"N": "1"}, {"N": "1"}],
            "TopK": rows().len(),
        }),
    )
    .await;

    for (at, row) in rows().iter().enumerate() {
        let item = result_for(&answered, &key_of(at));
        let reported = item["emb"]["L"][0]["N"].clone();
        assert_eq!(
            reported,
            json!(row.held),
            "case {:?}: the index holds the coordinate written as {:?} as {:?}, got {reported} \
             from {item} of {answered}",
            row.case,
            row.written,
            row.held
        );

        // The ITEM the table holds keeps the decimal the write carried: only the index's
        // own copy is a 32-bit float.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": key_of(at)}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["emb"]["L"][0]["N"],
            json!(row.written),
            "case {:?}: the stored item keeps the decimal it was written with, got {read}",
            row.case
        );
    }
}
