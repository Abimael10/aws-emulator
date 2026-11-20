// SPDX-License-Identifier: CC0-1.0

//! What a secondary index's `Projection.ProjectionType` has to say for the index to be
//! declarable at all.
//!
//! The rule under test: `Projection$ProjectionType` targets the `ProjectionType` enum,
//! whose values are `ALL`, `KEYS_ONLY` and `INCLUDE` (`smithy_query(query="shape",
//! shape="Projection,ProjectionType")`), so a projection that states no member of that set
//! describes an index the service cannot build: the declaration is refused with HTTP 400
//! `ValidationException` "One or more parameter values were invalid: Unknown
//! ProjectionType: " and the value the request carried — `null` for a member it sent as
//! JSON `null` or left out — and nothing of the request is stored, so the table a refused
//! `CreateTable` would have made is not there to find.
//!
//! The rows span the class the rule generalises over rather than one declaration: the two
//! ways a request states no type, each kind of index that carries a `Projection` (global,
//! local and vector), and each operation that declares one (the create, and the update that
//! adds an index to a table that already exists). The control rows are every value the
//! enum does name, so the refusal is of the value and not of the member.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message};

/// AWS's fixed text for a `ProjectionType` outside the enum's value set, with the value
/// the request carried appended after it.
const UNKNOWN_PROJECTION_TYPE: &str = "One or more parameter values were invalid: Unknown \
                                       ProjectionType: ";

/// The two ways a request states no `ProjectionType`: the member sent as JSON `null`, and
/// the member left out — one value to a reader of the request, which AWS names `null`.
fn typeless_projections() -> Vec<(&'static str, Value)> {
    vec![
        ("the member sent as null", json!({"ProjectionType": null})),
        ("the member left out", json!({})),
        (
            "the member left out beside an attribute list",
            json!({"NonKeyAttributes": ["note"]}),
        ),
    ]
}

/// The values the enum names, each with the `Projection` that states it.
fn declared_projections() -> Vec<(&'static str, Value)> {
    vec![
        ("ALL", json!({"ProjectionType": "ALL"})),
        ("KEYS_ONLY", json!({"ProjectionType": "KEYS_ONLY"})),
        (
            "INCLUDE",
            json!({"ProjectionType": "INCLUDE", "NonKeyAttributes": ["note"]}),
        ),
    ]
}

/// A table name made from the case, so each row drives a table of its own.
fn table_name(prefix: &str, case: &str) -> String {
    let mut name = String::from(prefix);
    name.extend(case.chars().map(|character| {
        if character.is_ascii_alphanumeric() {
            character
        } else {
            '-'
        }
    }));
    name
}

/// A create whose one GLOBAL secondary index carries `projection`.
fn create_with_global_index(name: &str, projection: Value) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "gk", "AttributeType": "S"},
        ],
        "BillingMode": "PAY_PER_REQUEST",
        "GlobalSecondaryIndexes": [{
            "IndexName": "by-gk",
            "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
            "Projection": projection,
        }],
    })
}

/// A create whose one LOCAL secondary index carries `projection`.
fn create_with_local_index(name: &str, projection: Value) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                      {"AttributeName": "sk", "KeyType": "RANGE"}],
        "AttributeDefinitions": [
            {"AttributeName": "pk", "AttributeType": "S"},
            {"AttributeName": "sk", "AttributeType": "S"},
            {"AttributeName": "lk", "AttributeType": "S"},
        ],
        "BillingMode": "PAY_PER_REQUEST",
        "LocalSecondaryIndexes": [{
            "IndexName": "by-lk",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                          {"AttributeName": "lk", "KeyType": "RANGE"}],
            "Projection": projection,
        }],
    })
}

/// A create whose one VECTOR index carries `projection`.
fn create_with_vector_index(name: &str, projection: Value) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
        "VectorIndexes": [{
            "IndexName": "by-emb",
            "VectorAttribute": {"AttributeName": "emb"},
            "Dimensions": 3,
            "DistanceFunction": "COSINE",
            "Projection": projection,
        }],
    })
}

/// The three kinds of index a create declares a `Projection` for, each as the create that
/// carries one.
type Declaration = (&'static str, fn(&str, Value) -> Value);
const DECLARATIONS: [Declaration; 3] = [
    ("a global secondary index", create_with_global_index),
    ("a local secondary index", create_with_local_index),
    ("a vector index", create_with_vector_index),
];

#[tokio::test]
async fn a_create_declaring_a_typeless_projection_is_refused_and_stores_nothing() {
    let router = emulator();
    for (kind, create) in DECLARATIONS {
        for (form, projection) in typeless_projections() {
            let case = format!("{kind} with {form}");
            let name = table_name("proj-", &case);
            let (status, refused) = call(&router, "CreateTable", create(&name, projection)).await;
            assert_eq!(
                (status, error_code(&refused), message(&refused)),
                (
                    400,
                    "ValidationException".to_string(),
                    format!("{UNKNOWN_PROJECTION_TYPE}null")
                ),
                "case {case:?}: expected the unknown-projection-type refusal, got {status} \
                 with {refused}"
            );

            // The refusal stored nothing, so the table it would have made is not there:
            // "Requested resource not found" is what a probe for it reads.
            let (status, described) =
                call(&router, "DescribeTable", json!({"TableName": name})).await;
            assert_eq!(
                (status, error_code(&described), message(&described)),
                (
                    400,
                    "ResourceNotFoundException".to_string(),
                    format!("Requested resource not found: Table: {name} not found")
                ),
                "case {case:?}: expected the table the refused create did not make to be \
                 missing, got {status} with {described}"
            );
        }
    }
}

#[tokio::test]
async fn a_create_declaring_a_projection_type_the_enum_names_is_served() {
    // The control: the same declarations, with each value the enum does name — including
    // the two the refusals above are not about — are accepted and reported back.
    let router = emulator();
    for (kind, create) in DECLARATIONS {
        for (projection_type, projection) in declared_projections() {
            let case = format!("{kind} projecting {projection_type}");
            let name = table_name("ok-", &case);
            let (status, created) =
                call(&router, "CreateTable", create(&name, projection.clone())).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a declared projection type is served, got {status} with {created}"
            );
            let described = describe_when_active(&router, &case, &name).await;
            let reported = [
                "GlobalSecondaryIndexes",
                "LocalSecondaryIndexes",
                "VectorIndexes",
            ]
            .into_iter()
            .filter_map(|member| described[member].as_array().cloned())
            .flatten()
            .map(|index| index["Projection"].clone())
            .collect::<Vec<Value>>();
            assert_eq!(
                reported,
                vec![projection.clone()],
                "case {case:?}: expected the stored index to report the projection it was \
                 declared with, got {described}"
            );
        }
    }
}

#[tokio::test]
async fn an_index_added_with_a_typeless_projection_is_refused_and_adds_nothing() {
    // The same rule for the operation that ADDS an index to a table that already exists:
    // the update is refused and the table keeps the indexes it had.
    let router = emulator();
    let name = "proj-update-target";
    let (status, created) = call(
        &router,
        "CreateTable",
        create_with_global_index(name, json!({"ProjectionType": "ALL"})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the update's target\": the table must be created, got {status} with {created}"
    );
    describe_when_active(&router, "the update's target", name).await;

    for (form, projection) in typeless_projections() {
        let case = format!("an index added with {form}");
        let (status, refused) = call(
            &router,
            "UpdateTable",
            json!({
                "TableName": name,
                "AttributeDefinitions": [
                    {"AttributeName": "pk", "AttributeType": "S"},
                    {"AttributeName": "tag", "AttributeType": "S"},
                ],
                "GlobalSecondaryIndexUpdates": [{"Create": {
                    "IndexName": "by-tag",
                    "KeySchema": [{"AttributeName": "tag", "KeyType": "HASH"}],
                    "Projection": projection,
                }}],
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused), message(&refused)),
            (
                400,
                "ValidationException".to_string(),
                format!("{UNKNOWN_PROJECTION_TYPE}null")
            ),
            "case {case:?}: expected the same refusal as for a create, got {status} with \
             {refused}"
        );

        let described = describe_when_active(&router, &case, name).await;
        let names: Vec<Value> = described["GlobalSecondaryIndexes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|index| index["IndexName"].clone())
            .collect();
        assert_eq!(
            names,
            vec![json!("by-gk")],
            "case {case:?}: expected the refused update to add no index, got {described}"
        );
    }
}
