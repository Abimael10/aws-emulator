// SPDX-License-Identifier: CC0-1.0

//! What a `Query` is refused with when the schema it reads is keyed on SEVERAL partition
//! key attributes and the condition leaves one of them unconstrained.
//!
//! RULE: a key schema declaring more than one `HASH` element — "each global secondary index
//! supports up to 4 partition keys and up to 4 sort keys"
//! (`smithy_query(query="doc",shape="CreateTableInput",member="GlobalSecondaryIndexes")`) —
//! selects its entries by ALL of those attributes together, so a `Query` of it is answered
//! only when EVERY one of them carries an equality condition; one that does not is HTTP 400
//! `ValidationException` naming the partition key attributes that carry none, in the order the
//! schema declares them, inside fixed text. A schema keyed on ONE partition key attribute has
//! no such rule and keeps reporting the single key schema element the condition missed, which
//! is what tells the two refusals apart.
//!
//! The rows span what the rule generalises over: how many partition keys the schema declares
//! (two and three), WHICH of them the condition supplies (each half on its own, none of them,
//! and all but the middle one), and the number of attributes left unconstrained (one and
//! several). The controls span the conditions the same indexes DO serve and the one-partition-
//! key schemas — a global index, a local index and the table's own primary key — whose missed
//! element keeps its own sentence.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row reads.
const TABLE: &str = "MultiPartitionKeys";

/// A global secondary index keyed on TWO partition key attributes and one sort key.
const PAIR: &str = "ByTenantAndRegion";

/// A global secondary index keyed on THREE partition key attributes, so the rule is read over
/// more than the one count the pair above has.
const TRIPLE: &str = "ByThree";

/// A global secondary index keyed on ONE partition key attribute: the schema this rule does
/// NOT speak about.
const SINGLE: &str = "ByTenant";

/// A local secondary index, whose schema also declares one partition key attribute.
const LOCAL: &str = "ByLabel";

/// Create [`TABLE`] with those four indexes, wait out the documented `CREATING` -> `ACTIVE`
/// transition, and write the one item every index covers.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "tenant", "AttributeType": "S"},
                {"AttributeName": "region", "AttributeType": "S"},
                {"AttributeName": "shard", "AttributeType": "S"},
                {"AttributeName": "label", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": PAIR,
                    "KeySchema": [
                        {"AttributeName": "tenant", "KeyType": "HASH"},
                        {"AttributeName": "region", "KeyType": "HASH"},
                        {"AttributeName": "sk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": TRIPLE,
                    "KeySchema": [
                        {"AttributeName": "tenant", "KeyType": "HASH"},
                        {"AttributeName": "region", "KeyType": "HASH"},
                        {"AttributeName": "shard", "KeyType": "HASH"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": SINGLE,
                    "KeySchema": [
                        {"AttributeName": "tenant", "KeyType": "HASH"},
                        {"AttributeName": "sk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": LOCAL,
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "label", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;
    describe_when_active(&router, "seed the multi-partition-key table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "p1"}, "sk": {"S": "s1"},
            "tenant": {"S": "t1"}, "region": {"S": "r1"}, "shard": {"S": "h1"},
            "label": {"S": "l1"},
        }}),
    )
    .await;
    router
}

/// The sentence AWS states for a key schema whose partition key attributes `missing` carry no
/// equality condition: the names inside fixed text, in schema order.
fn hash_keys_need_equality(missing: &str) -> String {
    format!("HASH key attributes {missing} must have equality conditions specified in the query")
}

/// The aliases the conditions below reach their key attributes through.
///
/// `region` and `shard` are both DynamoDB RESERVED words, so an expression reaches them
/// through `ExpressionAttributeNames` — "to access an attribute whose name conflicts with a
/// DynamoDB reserved word" (`Query$ExpressionAttributeNames`) — while the refusal names the
/// ATTRIBUTE the alias stands for, which is what the key schema declares.
fn aliases() -> Value {
    json!({"#region": "region", "#shard": "shard"})
}

/// Drive one `Query` over `index` with `condition`, binding `values` and the aliases above.
async fn query(
    router: &MultiServiceRouter,
    index: Option<&str>,
    condition: &str,
    values: Value,
) -> (u16, Value) {
    let mut request = json!({"TableName": TABLE,
                             "KeyConditionExpression": condition,
                             "ExpressionAttributeValues": values});
    if let Some(index) = index {
        request["IndexName"] = json!(index);
    }
    // Only the aliases the condition actually writes: an entry nothing references is a
    // defect of its own.
    let referenced: Vec<(String, Value)> = aliases()
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(alias, _)| condition.contains(alias.as_str()))
        .collect();
    if !referenced.is_empty() {
        request["ExpressionAttributeNames"] = Value::Object(referenced.into_iter().collect());
    }
    call(router, "Query", request).await
}

/// One refused read: the index it addresses, the condition it states, the values it binds and
/// the sentence the service must answer with.
struct Row {
    case: &'static str,
    index: Option<&'static str>,
    condition: &'static str,
    values: Value,
    message: String,
}

#[tokio::test]
async fn a_multi_partition_key_schema_names_the_keys_that_need_an_equality() {
    let router = seeded().await;

    let rows = vec![
        Row {
            case: "the pair index with only its first partition key",
            index: Some(PAIR),
            condition: "tenant = :t",
            values: json!({":t": {"S": "t1"}}),
            message: hash_keys_need_equality("region"),
        },
        Row {
            // Symmetrically: the same index names the OTHER attribute when the other half
            // is the one supplied.
            case: "the pair index with only its second partition key",
            index: Some(PAIR),
            condition: "#region = :r",
            values: json!({":r": {"S": "r1"}}),
            message: hash_keys_need_equality("tenant"),
        },
        Row {
            // Neither of them: both names, in the order the schema declares them.
            case: "the pair index with neither partition key, on its sort key alone",
            index: Some(PAIR),
            condition: "sk = :s",
            values: json!({":s": {"S": "s1"}}),
            message: hash_keys_need_equality("tenant, region"),
        },
        Row {
            case: "the three-key index with only its first partition key",
            index: Some(TRIPLE),
            condition: "tenant = :t",
            values: json!({":t": {"S": "t1"}}),
            message: hash_keys_need_equality("region, shard"),
        },
        Row {
            // All but the MIDDLE one: the missing name is the one in the middle, so the
            // refusal is read off which attributes carry an equality rather than off how
            // many the condition stated.
            case: "the three-key index with its first and last partition keys",
            index: Some(TRIPLE),
            condition: "tenant = :t AND #shard = :h",
            values: json!({":t": {"S": "t1"}, ":h": {"S": "h1"}}),
            message: hash_keys_need_equality("region"),
        },
        Row {
            // A condition that names a NON-key attribute beside one partition key still
            // broke this schema's rule: every partition key attribute needs its equality.
            case: "the pair index with one partition key and a non-key attribute",
            index: Some(PAIR),
            condition: "tenant = :t AND label = :l",
            values: json!({":t": {"S": "t1"}, ":l": {"S": "l1"}}),
            message: hash_keys_need_equality("region"),
        },
        Row {
            // The schemas keyed on ONE partition key attribute keep the single-element
            // sentence, whichever kind of index declares them.
            case: "the single-key global index without its partition key",
            index: Some(SINGLE),
            condition: "sk = :s",
            values: json!({":s": {"S": "s1"}}),
            message: "Query condition missed key schema element: tenant".to_string(),
        },
        Row {
            case: "the local index without its partition key",
            index: Some(LOCAL),
            condition: "label = :l",
            values: json!({":l": {"S": "l1"}}),
            message: "Query condition missed key schema element: pk".to_string(),
        },
        Row {
            case: "the table's own primary key without its partition key",
            index: None,
            condition: "sk = :s",
            values: json!({":s": {"S": "s1"}}),
            message: "Query condition missed key schema element: pk".to_string(),
        },
    ];

    for row in rows {
        let (status, body) = query(&router, row.index, row.condition, row.values.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body)),
            (400, "ValidationException", row.message.clone()),
            "case {:?}: expected {:?}, got {status} with {body}",
            row.case,
            row.message
        );
    }
}

#[tokio::test]
async fn a_condition_covering_every_partition_key_is_served() {
    let router = seeded().await;

    // The CONTROLS: a condition that pins every partition key attribute of the schema is
    // READ, with and without a condition on the sort key beside it — and the legacy
    // `KeyConditions` spelling of the same rule is refused and served the same way, since the
    // rule is about the key schema rather than about which member stated the condition.
    for (case, index, condition, values) in [
        (
            "the pair index with both partition keys",
            PAIR,
            "tenant = :t AND #region = :r",
            json!({":t": {"S": "t1"}, ":r": {"S": "r1"}}),
        ),
        (
            "the pair index with both partition keys and its sort key",
            PAIR,
            "tenant = :t AND #region = :r AND sk = :s",
            json!({":t": {"S": "t1"}, ":r": {"S": "r1"}, ":s": {"S": "s1"}}),
        ),
        (
            "the three-key index with all three partition keys",
            TRIPLE,
            "tenant = :t AND #region = :r AND #shard = :h",
            json!({":t": {"S": "t1"}, ":r": {"S": "r1"}, ":h": {"S": "h1"}}),
        ),
    ] {
        let (status, answered) = query(&router, Some(index), condition, values).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read to be served, got {status} with {answered}"
        );
        assert_eq!(
            answered["Count"],
            json!(1),
            "case {case:?}: expected the covered item, got {answered}"
        );
        assert_eq!(
            answered["Items"][0]["pk"],
            json!({"S": "p1"}),
            "case {case:?}: expected the stored item's key, got {answered}"
        );
    }

    // The legacy member states the same condition, and the same schema rule: one entry per
    // partition key attribute is served, and leaving one out earns the same sentence.
    let answered = ok(
        &router,
        "Query",
        json!({"TableName": TABLE, "IndexName": PAIR, "KeyConditions": {
            "tenant": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "t1"}]},
            "region": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "r1"}]},
        }}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case a legacy KeyConditions covering both partition keys: expected the covered item, \
         got {answered}"
    );
    let (status, body) = call(
        &router,
        "Query",
        json!({"TableName": TABLE, "IndexName": PAIR, "KeyConditions": {
            "tenant": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "t1"}]},
        }}),
    )
    .await;
    let expected = hash_keys_need_equality("region");
    assert_eq!(
        (status, message(&body)),
        (400, expected.clone()),
        "case a legacy KeyConditions missing a partition key: expected {expected:?}, got \
         {status} with {body}"
    );
}
