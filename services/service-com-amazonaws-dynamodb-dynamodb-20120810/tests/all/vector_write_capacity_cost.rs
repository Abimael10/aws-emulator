// SPDX-License-Identifier: CC0-1.0

//! What a write to a table carrying VECTOR INDEXES is charged for.
//!
//! The rules pinned here:
//!
//! 1. what a vector index METERS for a write is the ENTRY it holds for the item — the
//!    table's key, the vector attribute and whatever the index's `Projection` adds — each
//!    attribute counted by its name and its value, with a vector counted at the four bytes
//!    per dimension it is stored as rather than at the decimal text the request spelled it
//!    with, and the figure held to a one-kilobyte per-write floor
//!    ("VectorWriteRequestBytes", `ConsumedCapacity$VectorIndexes`, reported under
//!    `VectorWriteRequestUnits` as well);
//! 2. only the indexes a write AFFECTS are metered: one whose stored entry the write leaves
//!    exactly as it stood is charged nothing, so a write that changes an attribute only
//!    some of the table's indexes project is charged against those alone;
//! 3. the capacity the item's own write consumes counts the vector the same way
//!    (`ConsumedCapacity$CapacityUnits` and `$Table`, one unit per kilobyte), so what a
//!    write costs never depends on how many digits a coordinate was written with;
//! 4. the per-index figures are part of the itemisation `INDEXES` asks for: a `TOTAL`
//!    answer carries the aggregate alone.
//!
//! The rows differ along what the rules generalise over: the dimension count of the index
//! the item reaches, which of the table's vector indexes the item reaches at all, what each
//! index projects, which attribute a rewrite changes, and the number of digits each
//! coordinate carries (which moves the item's stored text by kilobytes while leaving the
//! metered figures where they are).
//!
//! `ConsumedCapacity$VectorIndexes` is a map of `VectorCapacity`, whose modelled members
//! are `VectorSearchRequestBytes` and `VectorWriteRequestBytes`
//! (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#VectorCapacity")`), and
//! whose recorded entries carry the write figure under `VectorWriteRequestUnits` beside the
//! byte member as well: ONE meter, so a write reports the same figure under both names and
//! no search figure at all.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{describe_when_active, emulator, ok};

/// The table every row writes to.
const TABLE: &str = "VectorWriteCost";

/// The dimensions of the indexes over the attribute whose vector is smaller than the
/// per-write floor, so their entries meter the floor however the coordinates were spelled.
const SMALL_DIMENSIONS: usize = 4;

/// The dimensions of the index whose entry is far past that floor, so its bytes grow with
/// the dimension count.
const LARGE_DIMENSIONS: usize = 1024;

/// What one dimension of a stored vector costs: a vector attribute is a list of 32-bit
/// IEEE-754 floating point numbers (`SearchVectors$SearchVector`), so each dimension is
/// four bytes once it is held as the index holds it.
const BYTES_PER_DIMENSION: i64 = 4;

/// The bytes one write capacity unit covers, and the per-write floor a vector write is
/// held to: "one write capacity unit represents one write per second for an item up to 1
/// KB in size" (the DynamoDB Developer Guide, "Read/write capacity mode").
const UNIT_BYTES: i64 = 1024;

/// The stored size of the partition key every row is written under: the attribute's name
/// (`pk`) plus its two-character value, by the sizing rule of the Developer Guide's "Item
/// sizes and formats". Each row writes its OWN key, so the capacity a write consumes is
/// what its own item costs rather than the larger of it and whatever stood there before
/// ("the item size that matters is the larger of the two").
const KEY_BYTES: i64 = 2 + 2;

/// The attribute the `INCLUDE` index below projects, and the one the `KEYS_ONLY` index
/// beside it does not: long enough that an entry carrying it passes the per-write floor,
/// so which indexes project it is visible in the figures.
const EXTRA_BYTES: usize = 2_000;

/// The two-character partition key row `row` writes under.
fn key_of(row: usize) -> String {
    format!("k{row}")
}

/// One vector of `dimensions` coordinates, each spelled with `digits` significant digits,
/// so the TEXT of the attribute grows with `digits` while the vector it denotes does not.
fn vector(dimensions: usize, digits: usize) -> Value {
    let coordinate = format!("0.{}", "1".repeat(digits));
    Value::Array(
        (0..dimensions)
            .map(|_| json!({"N": coordinate.clone()}))
            .collect(),
    )
}

/// What one vector attribute of `dimensions` dimensions costs an entry that holds it: the
/// attribute's name plus four bytes a dimension.
fn vector_bytes(attribute: &str, dimensions: usize) -> i64 {
    let dimensions = i64::try_from(dimensions).expect("the dimension count fits");
    let name = i64::try_from(attribute.len()).expect("the attribute name fits");
    name + dimensions * BYTES_PER_DIMENSION
}

/// What the `extra` attribute costs an entry that projects it: its name plus its value.
fn extra_bytes(payload: usize) -> i64 {
    let payload = i64::try_from(payload).expect("the payload size fits");
    i64::try_from("extra".len()).expect("the attribute name fits") + payload
}

/// One index of the table below: the name, the vector attribute it indexes with its
/// dimension count, and the `Projection` it holds its entries with.
struct Index {
    name: &'static str,
    attribute: &'static str,
    dimensions: usize,
    projection: Projection,
}

/// Which of a covered item's attributes one index stores
/// (`Projection$ProjectionType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Projection {
    /// "ALL - All of the table attributes are projected into the index".
    All,
    /// "KEYS_ONLY - Only the index and primary key attributes are projected".
    KeysOnly,
    /// "INCLUDE - ... the attributes that are projected are those in NonKeyAttributes",
    /// which for the table below is `extra`.
    IncludeExtra,
}

impl Projection {
    /// The `Projection` body a declaration carries.
    fn body(self) -> Value {
        match self {
            Self::All => json!({"ProjectionType": "ALL"}),
            Self::KeysOnly => json!({"ProjectionType": "KEYS_ONLY"}),
            Self::IncludeExtra => {
                json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["extra"]})
            }
        }
    }
}

/// The table's indexes: two over the same small vector with different projections, and one
/// over a far wider vector, so one write can reach any of them.
const INDEXES: [Index; 4] = [
    Index {
        name: "idx-all",
        attribute: "emb_small",
        dimensions: SMALL_DIMENSIONS,
        projection: Projection::All,
    },
    Index {
        name: "idx-keys",
        attribute: "emb_small",
        dimensions: SMALL_DIMENSIONS,
        projection: Projection::KeysOnly,
    },
    Index {
        name: "idx-inc",
        attribute: "emb_small",
        dimensions: SMALL_DIMENSIONS,
        projection: Projection::IncludeExtra,
    },
    Index {
        name: "idx-large",
        attribute: "emb_large",
        dimensions: LARGE_DIMENSIONS,
        projection: Projection::All,
    },
];

/// The item one row writes: its key, the vector attributes it carries with their dimension
/// counts, how many digits each coordinate is spelled with, and the size of the `extra`
/// payload beside them (`None` for an item that carries none).
struct Written {
    row: usize,
    vectors: Vec<(&'static str, usize)>,
    digits: usize,
    extra: Option<usize>,
}

impl Written {
    /// The `Item` map this write carries.
    fn item(&self) -> Value {
        let mut item = json!({"pk": {"S": key_of(self.row)}});
        for (attribute, dimensions) in &self.vectors {
            item[*attribute] = json!({"L": vector(*dimensions, self.digits)});
        }
        if let Some(payload) = self.extra {
            item["extra"] = json!({"S": "x".repeat(payload)});
        }
        item
    }

    /// Whether this item reaches `index` at all: it must carry the attribute that index
    /// indexes.
    fn reaches(&self, index: &Index) -> bool {
        self.vectors
            .iter()
            .any(|(attribute, _)| *attribute == index.attribute)
    }

    /// The bytes `index` meters for a write of this item: the entry it holds for it — the
    /// key, the vector attribute, and whatever its projection adds — held to the per-write
    /// floor.
    fn index_bytes(&self, index: &Index) -> f64 {
        let mut bytes = KEY_BYTES + vector_bytes(index.attribute, index.dimensions);
        if index.projection == Projection::All {
            for (attribute, dimensions) in &self.vectors {
                if *attribute != index.attribute {
                    bytes += vector_bytes(attribute, *dimensions);
                }
            }
        }
        if index.projection != Projection::KeysOnly {
            if let Some(payload) = self.extra {
                bytes += extra_bytes(payload);
            }
        }
        bytes.max(UNIT_BYTES) as f64
    }

    /// The capacity units the write of this item consumes: the item as the store holds it,
    /// with every vector counted at four bytes per dimension, at one unit per kilobyte.
    fn units(&self) -> f64 {
        let mut size = KEY_BYTES;
        for (attribute, dimensions) in &self.vectors {
            size += vector_bytes(attribute, *dimensions);
        }
        if let Some(payload) = self.extra {
            size += extra_bytes(payload);
        }
        let units = (size / UNIT_BYTES + i64::from(size % UNIT_BYTES != 0)).max(1);
        units as f64
    }
}

/// Create the table the rows write to, with [`INDEXES`] over two attributes of very
/// different dimension counts and with three different projections.
async fn vector_table(router: &MultiServiceRouter) {
    let declarations: Vec<Value> = INDEXES
        .iter()
        .map(|index| {
            json!({
                "IndexName": index.name,
                "VectorAttribute": {"AttributeName": index.attribute},
                "Dimensions": index.dimensions,
                "DistanceFunction": "COSINE",
                "Projection": index.projection.body(),
            })
        })
        .collect();
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": declarations,
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
}

/// A number the response carried, or `None` where the member is absent.
fn figure(consumed: &Value, path: &[&str]) -> Option<f64> {
    let mut value = consumed;
    for member in path {
        value = value.get(*member)?;
    }
    value.as_f64()
}

#[tokio::test]
async fn a_write_is_charged_for_the_entry_each_index_holds() {
    let router = emulator();
    vector_table(&router).await;

    // Rows differ in which indexes the item reaches, in what those indexes project of it,
    // and in how many digits each coordinate carries: the two 18-digit rows write kilobytes
    // more TEXT than their 1-digit counterparts for the same vectors, and must be metered
    // and charged identically.
    let both = vec![
        ("emb_small", SMALL_DIMENSIONS),
        ("emb_large", LARGE_DIMENSIONS),
    ];
    let small = vec![("emb_small", SMALL_DIMENSIONS)];
    let large = vec![("emb_large", LARGE_DIMENSIONS)];
    let cases: [(&str, Written); 7] = [
        (
            "both vectors, one digit a coordinate",
            Written { row: 0, vectors: both.clone(), digits: 1, extra: None },
        ),
        (
            "both vectors, eighteen digits a coordinate",
            Written { row: 1, vectors: both, digits: 18, extra: None },
        ),
        (
            "the small vector alone, one digit",
            Written { row: 2, vectors: small.clone(), digits: 1, extra: None },
        ),
        (
            "the small vector alone, eighteen digits",
            Written { row: 3, vectors: small.clone(), digits: 18, extra: None },
        ),
        (
            "the large vector alone, one digit",
            Written { row: 4, vectors: large.clone(), digits: 1, extra: None },
        ),
        (
            "the large vector alone, eighteen digits",
            Written { row: 5, vectors: large, digits: 18, extra: None },
        ),
        (
            "the small vector beside a projected payload",
            Written { row: 6, vectors: small, digits: 1, extra: Some(EXTRA_BYTES) },
        ),
    ];

    for (case, written) in cases {
        let answered = ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": written.item(),
                   "ReturnConsumedCapacity": "INDEXES"}),
        )
        .await;
        let consumed = &answered["ConsumedCapacity"];

        // Each index the write reached reports the bytes of the entry it holds — which is
        // what its projection stores, and nothing of the text the vector was written as —
        // under both the byte member and the request-units member, since the two report one
        // meter; an index the item does not reach reports nothing at all.
        for index in &INDEXES {
            let expected = written.reaches(index).then(|| written.index_bytes(index));
            for member in ["VectorWriteRequestBytes", "VectorWriteRequestUnits"] {
                let reported = figure(consumed, &["VectorIndexes", index.name, member]);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected {} to meter {expected:?} under {member}, got \
                     {reported:?} in {consumed}",
                    index.name
                );
            }
            assert_eq!(
                figure(
                    consumed,
                    &["VectorIndexes", index.name, "VectorSearchRequestBytes"]
                ),
                None,
                "case {case:?}: a write searches nothing, so {} reports no search figure, got \
                 {consumed}",
                index.name
            );
        }

        // And the capacity the write consumed counts the vector at that same stored size,
        // on the aggregate and on the table's own line alike.
        let expected = written.units();
        for member in [
            vec!["CapacityUnits"],
            vec!["Table", "CapacityUnits"],
            vec!["WriteCapacityUnits"],
        ] {
            let reported = figure(consumed, &member);
            assert_eq!(
                reported,
                Some(expected),
                "case {case:?}: expected {member:?} to be {expected} unit(s) for the stored \
                 vector, got {reported:?} in {consumed}"
            );
        }
    }
}

#[tokio::test]
async fn only_the_indexes_whose_entry_the_write_changes_are_metered() {
    let router = emulator();
    vector_table(&router).await;

    // One item reaching the three indexes over the small vector, with the payload only two
    // of them project.
    let stored = Written {
        row: 7,
        vectors: vec![("emb_small", SMALL_DIMENSIONS)],
        digits: 1,
        extra: Some(EXTRA_BYTES),
    };
    let put = |item: Value| {
        let router = &router;
        async move {
            let answered = ok(
                router,
                "PutItem",
                json!({"TableName": TABLE, "Item": item, "ReturnConsumedCapacity": "INDEXES"}),
            )
            .await;
            answered["ConsumedCapacity"]["VectorIndexes"]
                .as_object()
                .map(|indexes| {
                    let mut names: Vec<String> = indexes.keys().cloned().collect();
                    names.sort();
                    names
                })
                .unwrap_or_default()
        }
    };

    // Rows differ in what the rewrite CHANGES: nothing at all, the payload only two
    // indexes project, and the vector every one of them is built over. Each row states the
    // indexes that must be metered for it.
    let mut item = stored.item();
    let metered = put(item.clone()).await;
    assert_eq!(
        metered,
        vec![
            "idx-all".to_string(),
            "idx-inc".to_string(),
            "idx-keys".to_string()
        ],
        "the first write puts an entry in every index the item reaches, got {metered:?}"
    );

    let metered = put(item.clone()).await;
    assert!(
        metered.is_empty(),
        "a rewrite that stores what already stood there changes no entry, so no index is \
         metered, got {metered:?}"
    );

    // A number respelt is the same number, so a rewrite that only changes how the vector
    // was spelled changes no entry either.
    item["emb_small"] = json!({"L": (0..SMALL_DIMENSIONS)
        .map(|_| json!({"N": "1e-1"}))
        .collect::<Vec<Value>>()});
    let respelt = put(item.clone()).await;
    assert!(
        respelt.is_empty(),
        "a rewrite that respells the same coordinates changes no entry, got {respelt:?}"
    );

    item["extra"] = json!({"S": "y".repeat(EXTRA_BYTES)});
    let metered = put(item.clone()).await;
    assert_eq!(
        metered,
        vec!["idx-all".to_string(), "idx-inc".to_string()],
        "changing the payload must meter the indexes that project it and no others, got \
         {metered:?}"
    );

    item["emb_small"] = json!({"L": vector(SMALL_DIMENSIONS, 1)});
    item["emb_small"]["L"][0] = json!({"N": "0.5"});
    let metered = put(item).await;
    assert_eq!(
        metered,
        vec![
            "idx-all".to_string(),
            "idx-inc".to_string(),
            "idx-keys".to_string()
        ],
        "changing the vector must meter every index built over it, got {metered:?}"
    );
}

#[tokio::test]
async fn the_per_index_figures_are_the_itemisation_indexes_asks_for() {
    let router = emulator();
    vector_table(&router).await;

    // Rows differ along `ReturnConsumedCapacity`: `TOTAL` reports "only the aggregate
    // ConsumedCapacity for the operation" while `INDEXES` reports it "together with
    // ConsumedCapacity for each table and secondary index that was accessed" (the
    // `ReturnConsumedCapacity` doc), so only the latter carries the per-index figures.
    let cases: [(&str, Value, bool); 3] = [
        ("NONE", json!("NONE"), false),
        ("TOTAL", json!("TOTAL"), false),
        ("INDEXES", json!("INDEXES"), true),
    ];
    for (row, (case, requested, itemised)) in cases.into_iter().enumerate() {
        let written = Written {
            row: 10 + row,
            vectors: vec![("emb_small", SMALL_DIMENSIONS)],
            digits: 1,
            extra: None,
        };
        let answered = ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": written.item(),
                   "ReturnConsumedCapacity": requested}),
        )
        .await;
        let consumed = &answered["ConsumedCapacity"];
        let reported = figure(consumed, &["VectorIndexes", "idx-all", "VectorWriteRequestBytes"]);
        assert_eq!(
            reported,
            itemised.then(|| written.index_bytes(&INDEXES[0])),
            "case {case:?}: expected the per-index figure only where the itemisation was asked \
             for, got {answered}"
        );
    }
}

#[tokio::test]
async fn every_write_that_reaches_a_vector_index_meters_it_the_same_way() {
    let router = emulator();
    vector_table(&router).await;

    // Rows differ in the OPERATION that reaches the index: what is metered is the index
    // ENTRY a write puts there, changes or removes, so each of the three write operations
    // reports the same figure for the same index and item.
    let written = Written {
        row: 20,
        vectors: vec![("emb_small", SMALL_DIMENSIONS)],
        digits: 1,
        extra: None,
    };
    let vector = json!({"L": vector(SMALL_DIMENSIONS, 1)});
    let expected = written.index_bytes(&INDEXES[1]);
    let cases: [(&str, &str, Value); 3] = [
        (
            "a put that stores the entry",
            "PutItem",
            json!({"TableName": TABLE, "ReturnConsumedCapacity": "INDEXES",
                   "Item": written.item()}),
        ),
        (
            "an update that sets the vector attribute",
            "UpdateItem",
            json!({"TableName": TABLE, "ReturnConsumedCapacity": "INDEXES",
                   "Key": {"pk": {"S": key_of(21)}},
                   "UpdateExpression": "SET emb_small = :v",
                   "ExpressionAttributeValues": {":v": vector.clone()}}),
        ),
        (
            "a delete that removes the entry the put stored",
            "DeleteItem",
            json!({"TableName": TABLE, "ReturnConsumedCapacity": "INDEXES",
                   "Key": {"pk": {"S": key_of(20)}}}),
        ),
    ];

    for (case, operation, request) in cases {
        let answered = ok(&router, operation, request).await;
        let consumed = &answered["ConsumedCapacity"];
        for member in ["VectorWriteRequestBytes", "VectorWriteRequestUnits"] {
            let reported = figure(consumed, &["VectorIndexes", "idx-keys", member]);
            assert_eq!(
                reported,
                Some(expected),
                "case {case:?}: expected {operation} to meter {expected} under {member}, got \
                 {reported:?} in {consumed}"
            );
        }
        // The index the item never reached is metered by nobody, whichever write it was.
        let untouched = figure(
            consumed,
            &["VectorIndexes", "idx-large", "VectorWriteRequestBytes"],
        );
        assert_eq!(
            untouched, None,
            "case {case:?}: {operation} reached no entry of idx-large, so it reports none, got \
             {consumed}"
        );
    }
}
