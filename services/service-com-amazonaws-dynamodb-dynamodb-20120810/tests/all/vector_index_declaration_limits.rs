// SPDX-License-Identifier: CC0-1.0

//! How the VECTOR-INDEX declarations a table is created or updated with are refused, end
//! to end over the wire.
//!
//! Three rules, each over a TABLE of inputs rather than one request:
//!
//! 1. a table carries at most [`LIMIT`] vector indexes. A declaration that would leave it
//!    carrying more is refused with the per-table quota's own sentence, which states the
//!    limit in force and nothing of the table or of the count sent — and the table is NOT
//!    created, so a `DescribeTable` on that name answers `ResourceNotFoundException`. The
//!    rule is the TABLE's, so it holds whether the indexes arrive with the create or one at
//!    a time through `UpdateTable`;
//! 2. a `SearchSchema` element naming an attribute the declaration states no
//!    `AttributeDefinition` for is refused with one entirely fixed sentence — the schema
//!    partitions and filters by that attribute's VALUE, so its type has to be declared. The
//!    sentence interpolates nothing: `SearchSchemaElement$AttributeName` is `@length
//!    0..65535` and so admits any name, the empty string included
//!    (`smithy_query(query="shape", shape="SearchSchemaElement")`);
//! 3. every vector index over one vector attribute shares that attribute's dimension
//!    count: "If multiple vector indexes reference the same attribute, they must all use
//!    the same number of dimensions" (`CreateVectorIndexAction$VectorAttribute`), so a
//!    declaration stating two counts for one attribute is refused, naming that attribute.
//!
//! The rows differ along what each rule generalises over: how many vector indexes the
//! declaration carries (at the limit, one past it and far past it) and whether they arrive
//! with the create or afterwards, the attribute name a search-schema element names and the
//! role it takes, and which pair of dimension counts disagrees and which index of the list
//! carries the second of them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The most vector indexes one table may carry, which the service states in the refusal of
/// a table that would carry more.
const LIMIT: usize = 5;

/// The sentence a declaration past that limit earns: fixed but for the limit in force.
fn count_limit(limit: usize) -> String {
    format!(
        "One or more parameter values were invalid: VectorIndex count exceeds the per-table limit \
         of {limit}"
    )
}

/// The entirely fixed sentence a `SearchSchema` element nothing declares earns.
const SEARCH_SCHEMA_UNDECLARED: &str =
    "One or more parameter values were invalid: One element in SearchSchema is not defined in \
     attribute definitions";

/// The sentence two indexes disagreeing about one vector attribute's dimensions earn.
fn conflicting_dimensions(attribute: &str) -> String {
    format!(
        "One or more parameter values were invalid: Conflicting attribute definition for \
         '{attribute}'. All VectorIndexes on the same vector attribute must use the same dimensions."
    )
}

/// One `KeySchemaElement`.
fn key(name: &str, key_type: &str) -> Value {
    json!({"AttributeName": name, "KeyType": key_type})
}

/// The `AttributeDefinitions` declaring every name in `names` as a string attribute.
fn definitions(names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .map(|name| json!({"AttributeName": name, "AttributeType": "S"}))
        .collect()
}

/// One vector index declaration: `name` over the vector attribute `attribute`, holding
/// vectors of `dimensions` dimensions.
fn vector_index(name: &str, attribute: &str, dimensions: i64) -> Value {
    json!({
        "IndexName": name,
        "VectorAttribute": {"AttributeName": attribute},
        "Dimensions": dimensions,
        "DistanceFunction": "COSINE",
        "Projection": {"ProjectionType": "KEYS_ONLY"},
    })
}

/// A `CreateTable` body for an on-demand table keyed on `pk` carrying `vectors`, declaring
/// `declared` beside `pk` in `AttributeDefinitions`.
///
/// An EMPTY `vectors` leaves the member out altogether: a list a request carries has to
/// name an index, so an empty one is a different request from one that declares no vector
/// index at all.
fn create_with(table: &str, declared: &[&str], vectors: Vec<Value>) -> Value {
    let mut names: Vec<&str> = vec!["pk"];
    names.extend_from_slice(declared);
    let mut request = json!({
        "TableName": table,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [key("pk", "HASH")],
        "AttributeDefinitions": definitions(&names),
    });
    if !vectors.is_empty() {
        request["VectorIndexes"] = json!(vectors);
    }
    request
}

/// `count` vector indexes, each over a vector attribute of its own.
fn vectors_of(count: usize) -> Vec<Value> {
    (0..count)
        .map(|position| vector_index(&format!("idx{position}"), &format!("emb{position}"), 8))
        .collect()
}

/// Assert `body` is a 400 `ValidationException` and answer the message it carries.
fn refusal(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected a 400 ValidationException, got {status} with {body}"
    );
    message(body)
}

/// Assert `DescribeTable` does not resolve `table`, with the not-found sentence that names
/// it — what proves a refused create stored nothing.
async fn absent(router: &MultiServiceRouter, case: &str, table: &str) {
    let (status, body) = call(router, "DescribeTable", json!({"TableName": table})).await;
    assert_eq!(
        (
            status,
            error_code(&body),
            message(&body),
        ),
        (
            400,
            "ResourceNotFoundException".to_string(),
            format!("Requested resource not found: Table: {table} not found"),
        ),
        "case {case:?}: a refused create must leave no table behind, got {status} with {body}"
    );
}

/// Rule 1: the per-table vector-index count is a quota of the TABLE, stated with the limit
/// in force, and a declaration past it creates nothing.
#[tokio::test]
async fn a_table_carries_no_more_vector_indexes_than_the_per_table_limit() {
    let router = emulator();

    // Rows differ along how many vector indexes the create declares: at the limit, one
    // past it, and well past it.
    for (case, count) in [
        ("one past the limit", LIMIT + 1),
        ("two past the limit", LIMIT + 2),
        ("far past the limit", LIMIT * 4),
    ] {
        let table = format!("too-many-{count}");
        let (status, body) = call(
            &router,
            "CreateTable",
            create_with(&table, &[], vectors_of(count)),
        )
        .await;
        assert_eq!(
            refusal(case, status, &body),
            count_limit(LIMIT),
            "case {case:?}: expected the per-table limit's own sentence, got {body}"
        );
        absent(&router, case, &table).await;
    }

    // Every count up to the limit is accepted, and the stored table reports each index.
    for count in 1..=LIMIT {
        let case = format!("{count} vector indexes");
        let table = format!("within-{count}");
        ok(
            &router,
            "CreateTable",
            create_with(&table, &[], vectors_of(count)),
        )
        .await;
        let described = describe_when_active(&router, &case, &table).await;
        let stored: Vec<Value> = described["VectorIndexes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            stored.len(),
            count,
            "case {case:?}: expected every declared index stored, got {described}"
        );
        assert_eq!(
            stored
                .iter()
                .map(|index| index["IndexName"].clone())
                .collect::<Vec<Value>>(),
            (0..count)
                .map(|position| json!(format!("idx{position}")))
                .collect::<Vec<Value>>(),
            "case {case:?}: expected the declared index names, got {described}"
        );
    }
}

/// Rule 1, continued: the limit counts the indexes the table ENDS UP carrying, so an index
/// added later is held to it as well.
#[tokio::test]
async fn an_added_vector_index_is_held_to_the_same_per_table_limit() {
    let router = emulator();

    // The table starts one index short of the limit, so the first add is accepted and the
    // next one — which would take it past — is refused.
    let table = "added-one-at-a-time";
    ok(
        &router,
        "CreateTable",
        create_with(table, &[], vectors_of(LIMIT - 1)),
    )
    .await;
    describe_when_active(&router, "a table under the limit", table).await;

    let added = vector_index("idx-added", "emb-added", 16);
    ok(
        &router,
        "UpdateTable",
        json!({"TableName": table, "VectorIndexUpdates": [{"Create": added}]}),
    )
    .await;
    let described = describe_when_active(&router, "the index added at the limit", table).await;
    assert_eq!(
        described["VectorIndexes"]
            .as_array()
            .map(|indexes| indexes.len()),
        Some(LIMIT),
        "case \"the index added at the limit\": expected the table at the limit, got {described}"
    );

    // One more would take the table past the limit, and earns the same sentence the create
    // path states.
    let refused = vector_index("idx-refused", "emb-refused", 16);
    let (status, body) = call(
        &router,
        "UpdateTable",
        json!({"TableName": table, "VectorIndexUpdates": [{"Create": refused}]}),
    )
    .await;
    assert_eq!(
        refusal("an index added past the limit", status, &body),
        count_limit(LIMIT),
        "case \"an index added past the limit\": expected the same sentence, got {body}"
    );
    // And the refused update changed nothing: the table still carries the indexes it had.
    let described = describe_when_active(&router, "after the refused add", table).await;
    assert_eq!(
        described["VectorIndexes"]
            .as_array()
            .map(|indexes| indexes.len()),
        Some(LIMIT),
        "case \"after the refused add\": expected the stored indexes untouched, got {described}"
    );
}

/// Rule 2: a search-schema element naming an attribute nothing declares earns one fixed
/// sentence, whatever the name is and whichever role the element takes.
#[tokio::test]
async fn a_search_schema_attribute_nothing_declares_is_refused_with_one_sentence() {
    let router = emulator();

    // Rows differ along the attribute name the element names — the empty string the member
    // admits, a name never declared, a name differing from a declared one only in case, a
    // name declared on ANOTHER table, and one at the member's own length — and along the
    // role the element takes.
    let rows: [(&str, &str, &str); 6] = [
        ("the empty string", "", "INLINE_FILTER"),
        ("the empty string as the partition key", "", "HASH"),
        ("a name nothing declares", "tier", "INLINE_FILTER"),
        ("that name as the partition key", "tier", "HASH"),
        ("a declared name in another case", "PK", "INLINE_FILTER"),
        ("the vector attribute itself", "emb", "HASH"),
    ];

    for (ordinal, (case, attribute, role)) in rows.iter().enumerate() {
        let table = format!("undeclared-{ordinal}");
        let mut index = vector_index("idx", "emb", 4);
        index["SearchSchema"] =
            json!([{"AttributeName": attribute, "SearchSchemaElementType": role}]);
        let (status, body) = call(
            &router,
            "CreateTable",
            create_with(&table, &[], vec![index]),
        )
        .await;
        assert_eq!(
            refusal(case, status, &body),
            SEARCH_SCHEMA_UNDECLARED,
            "case {case:?}: expected the fixed search-schema sentence, got {body}"
        );
        absent(&router, case, &table).await;
    }

    // The control: the same schema over an attribute the request DOES declare is accepted,
    // and the stored index reports it back.
    let mut index = vector_index("idx", "emb", 4);
    index["SearchSchema"] =
        json!([{"AttributeName": "tier", "SearchSchemaElementType": "INLINE_FILTER"}]);
    ok(
        &router,
        "CreateTable",
        create_with("declared-schema", &["tier"], vec![index]),
    )
    .await;
    let described = describe_when_active(&router, "a declared schema", "declared-schema").await;
    assert_eq!(
        described["VectorIndexes"][0]["SearchSchema"],
        json!([{"AttributeName": "tier", "SearchSchemaElementType": "INLINE_FILTER"}]),
        "case \"a declared schema\": expected the schema stored as declared, got {described}"
    );

    // The same sentence for an index `UpdateTable` adds: it is one rule about one member,
    // not a rule of the operation that carried it.
    ok(
        &router,
        "CreateTable",
        create_with("schema-added-later", &[], vec![]),
    )
    .await;
    describe_when_active(&router, "the table an index is added to", "schema-added-later").await;
    let mut create = vector_index("idx-added", "emb", 4);
    create["SearchSchema"] =
        json!([{"AttributeName": "tier", "SearchSchemaElementType": "INLINE_FILTER"}]);
    let (status, body) = call(
        &router,
        "UpdateTable",
        json!({"TableName": "schema-added-later",
               "VectorIndexUpdates": [{"Create": create}]}),
    )
    .await;
    assert_eq!(
        refusal("an added index naming an undeclared attribute", status, &body),
        SEARCH_SCHEMA_UNDECLARED,
        "case \"an added index naming an undeclared attribute\": expected the same sentence, got \
         {body}"
    );
}

/// Rule 3: the dimensions belong to the vector ATTRIBUTE, so every index over it has to
/// agree about them.
#[tokio::test]
async fn vector_indexes_over_one_attribute_must_agree_about_its_dimensions() {
    let router = emulator();

    // Rows differ along the attribute the pair shares, the two dimension counts that
    // disagree (including each published bound of the range), and which index of the list
    // carries the second count.
    let rows: [(&str, &str, &[i64]); 5] = [
        ("two counts for one attribute", "emb", &[4, 8]),
        ("the larger count first", "emb", &[8, 4]),
        ("the two published bounds", "vector", &[1, 4096]),
        ("the third index disagreeing", "emb", &[4, 4, 16]),
        ("a later pair disagreeing", "emb", &[4, 4, 4, 9]),
    ];

    for (ordinal, (case, attribute, dimensions)) in rows.iter().enumerate() {
        let table = format!("conflicting-{ordinal}");
        let vectors: Vec<Value> = dimensions
            .iter()
            .enumerate()
            .map(|(position, count)| {
                vector_index(&format!("idx{position}"), attribute, *count)
            })
            .collect();
        let (status, body) = call(
            &router,
            "CreateTable",
            create_with(&table, &[], vectors),
        )
        .await;
        assert_eq!(
            refusal(case, status, &body),
            conflicting_dimensions(attribute),
            "case {case:?}: expected the conflicting-attribute sentence naming {attribute:?}, got \
             {body}"
        );
        absent(&router, case, &table).await;
    }

    // The control: several indexes over ONE attribute that agree about its dimensions are
    // accepted, and each reports that count.
    ok(
        &router,
        "CreateTable",
        create_with(
            "agreeing",
            &[],
            vec![
                vector_index("idx0", "emb", 12),
                vector_index("idx1", "emb", 12),
                vector_index("idx2", "other", 4),
            ],
        ),
    )
    .await;
    let described = describe_when_active(&router, "indexes that agree", "agreeing").await;
    assert_eq!(
        described["VectorIndexes"]
            .as_array()
            .map(|indexes| indexes
                .iter()
                .map(|index| index["Dimensions"].clone())
                .collect::<Vec<Value>>()),
        Some(vec![json!(12), json!(12), json!(4)]),
        "case \"indexes that agree\": expected every index stored with its own count, got \
         {described}"
    );

    // And the same rule holds for an index added later: one that disagrees with the stored
    // index over the same attribute is refused, while one that agrees is stored.
    let (status, body) = call(
        &router,
        "UpdateTable",
        json!({"TableName": "agreeing",
               "VectorIndexUpdates": [{"Create": vector_index("idx-added", "emb", 13)}]}),
    )
    .await;
    assert_eq!(
        refusal("an added index that disagrees", status, &body),
        conflicting_dimensions("emb"),
        "case \"an added index that disagrees\": expected the same sentence, got {body}"
    );
    ok(
        &router,
        "UpdateTable",
        json!({"TableName": "agreeing",
               "VectorIndexUpdates": [{"Create": vector_index("idx-added", "emb", 12)}]}),
    )
    .await;
    let described = describe_when_active(&router, "an added index that agrees", "agreeing").await;
    assert_eq!(
        described["VectorIndexes"]
            .as_array()
            .map(|indexes| indexes.len()),
        Some(4),
        "case \"an added index that agrees\": expected the added index stored, got {described}"
    );
}
