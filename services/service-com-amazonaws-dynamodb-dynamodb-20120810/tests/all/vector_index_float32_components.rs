// SPDX-License-Identifier: CC0-1.0

//! What a vector index HOLDS for a coordinate, and what a search through it computes from
//! it, driven end to end.
//!
//! RULE under test: "each element is a 32-bit IEEE-754 floating point number"
//! (`SearchVectors$SearchVector`, the same shape a vector index's `VectorAttribute` names
//! on an item), so what the index stores for a coordinate is the float that type can hold
//! and not the decimal the request spelled: a number no 32-bit float represents exactly is
//! held as the one it rounds to, and the similarity score a search through the index reports
//! is computed from THAT. The table's own item is untouched by this — the item store holds
//! the decimal the write carried, and `GetItem` answers it — so the two differ exactly where
//! the rounding does.
//!
//! The score is how the index's held copy is observed: a search answers "the similarity
//! score for this item relative to the search vector" (`SearchResultItem$Score`) beside the
//! attributes the index PROJECTS of the item (`SearchResultItem$Item`), which never include
//! the vector it searched by — that one reaches the index as the value it is built on
//! (`VectorIndex$VectorAttribute`) rather than as an attribute copied into it. Over
//! `DOT_PRODUCT` against the all-ones search vector the score is the sum of the held
//! coordinates, so a coordinate the type cannot represent and the coordinate it rounds to
//! score IDENTICALLY while every neighbour of them scores apart — which is the rounding,
//! observed.
//!
//! The rows span what the rule generalises over: a coordinate a 32-bit float holds exactly
//! (a whole number, a power of two, a half), the first whole number it does NOT hold
//! (2^24 + 1, which rounds down to 2^24) and its neighbours, a value with more decimal
//! digits than that type carries, a negative value, and zero — each written as the decimal
//! a client sends and read back through the score the search computed.

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

/// The second coordinate every row's vector carries, so the vector has the index's own
/// `Dimensions` — and the value of both coordinates of the search vector, so the inner
/// product the index computes is the sum of the held coordinates.
const COMPANION: &str = "1";

/// One row: the case, the decimal the item is written with, and the decimal the index is
/// expected to hold for it.
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

impl Row {
    /// The score a `DOT_PRODUCT` search with the all-ones vector computes for this row: the
    /// sum of the coordinates the INDEX holds, which is the held value plus the companion.
    /// Read as the 32-bit float the index stores, so the figure is the one the index's own
    /// copy produces and not the one the decimal text would.
    fn score(&self) -> f64 {
        let held: f32 = self.held.parse().expect("the held decimal is a float32");
        let companion: f32 = COMPANION.parse().expect("the companion is a float32");
        f64::from(held) + f64::from(companion)
    }
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

/// The result the search answered for `pk`, or `Value::Null` where it answered none.
fn result_for(answered: &Value, pk: &str) -> Value {
    answered["SearchResults"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|result| result["Item"]["pk"]["S"] == json!(pk))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Create the table and its index, wait for both, and write one item per row: the
/// coordinate the row writes, beside a second coordinate every row shares, so the vector has
/// the index's own `Dimensions`.
async fn stored_rows(router: &smithy_server_runtime::MultiServiceRouter) {
    ok(
        router,
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
    ready(router).await;
    for (at, row) in rows().iter().enumerate() {
        ok(
            router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": key_of(at)},
                "emb": {"L": [{"N": row.written}, {"N": COMPANION}]},
            }}),
        )
        .await;
    }
}

/// One `SearchVectors` through [`INDEX`] for every row stored.
async fn search_every_row(router: &smithy_server_runtime::MultiServiceRouter) -> Value {
    ok(
        router,
        "SearchVectors",
        json!({
            "TableName": TABLE,
            "IndexName": INDEX,
            "SearchVector": [{"N": COMPANION}, {"N": COMPANION}],
            "TopK": rows().len(),
        }),
    )
    .await
}

#[tokio::test]
async fn a_vector_index_scores_each_coordinate_as_the_float32_it_is_held_as() {
    let router = emulator();
    stored_rows(&router).await;

    let answered = search_every_row(&router).await;

    for (at, row) in rows().iter().enumerate() {
        let result = result_for(&answered, &key_of(at));
        let reported = result["Score"].as_f64();
        let expected = row.score();
        assert_eq!(
            reported,
            Some(expected),
            "case {:?}: the index holds the coordinate written as {:?} as {:?}, so the search \
             scores it {expected}, got {reported:?} from {result} of {answered}",
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

    // The rounding itself, stated as the two rows it makes indistinguishable and the
    // neighbours it does not: 2^24 + 1 is held as 2^24, so those two score alike, while
    // 2^24 + 2 — which the type holds exactly — scores apart from both.
    let score_of = |at: usize| result_for(&answered, &key_of(at))["Score"].as_f64();
    assert_eq!(
        score_of(4),
        score_of(5),
        "case \"the first whole number the type cannot represent\": it is held as the \
         coordinate it rounds to, so the two score alike, got {answered}"
    );
    assert_ne!(
        score_of(5),
        score_of(6),
        "case \"the neighbour the type holds exactly\": it is held as itself, so it scores \
         apart from the rounded one, got {answered}"
    );
}

#[tokio::test]
async fn a_search_reports_no_vector_attribute_of_the_index_it_searched() {
    let router = emulator();
    stored_rows(&router).await;
    let answered = search_every_row(&router).await;

    // The score is the ONE thing the index's held copy is observed through: a result
    // "contains the projected attributes and a similarity score"
    // (`SearchVectors$SearchResults`), and the vector attribute reaches the index in its own
    // member as the value it is built on rather than as an attribute copied into it
    // (`VectorIndex$VectorAttribute` beside `VectorIndex$Projection`) — so no result carries
    // it, not even from an index whose `Projection` is `ALL`.
    for (at, row) in rows().iter().enumerate() {
        let result = result_for(&answered, &key_of(at));
        assert!(
            result["Item"]["emb"].is_null(),
            "case {:?}: a search hands back no vector attribute, got {result} of {answered}",
            row.case
        );
        // Read back off the TABLE's item, the coordinate is the decimal the write carried:
        // the two stores hold different things, which is this suite's whole rule.
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

    // A `ProjectionExpression` naming the vector attribute reports nothing for it: a request
    // may only narrow what the index projects, and the vector is not among those attributes
    // ("only attributes projected into the vector index can be retrieved",
    // `SearchVectors$ProjectionExpression`). The rows are still SCORED and answered — the
    // projection decides what each result carries, not which items a search finds.
    let narrowed = ok(
        &router,
        "SearchVectors",
        json!({
            "TableName": TABLE,
            "IndexName": INDEX,
            "SearchVector": [{"N": COMPANION}, {"N": COMPANION}],
            "TopK": rows().len(),
            "ProjectionExpression": "emb",
        }),
    )
    .await;
    for (at, row) in rows().iter().enumerate() {
        let result = result_for(&narrowed, &key_of(at));
        assert!(
            result.is_null(),
            "case {:?}: a result narrowed to the vector carries no pk to find it by, got \
             {result}",
            row.case
        );
    }
    let reported: Vec<Vec<String>> = narrowed["SearchResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            let mut names: Vec<String> = result["Item"]
                .as_object()
                .cloned()
                .unwrap_or_default()
                .keys()
                .cloned()
                .collect();
            names.sort();
            names
        })
        .collect();
    assert_eq!(
        reported.len(),
        rows().len(),
        "every stored row is still answered when the projection names the vector, got \
         {narrowed}"
    );
    assert!(
        reported.iter().all(Vec::is_empty),
        "case \"a projection naming the vector attribute\": it reports nothing for it, got \
         {reported:?} from {narrowed}"
    );
}
