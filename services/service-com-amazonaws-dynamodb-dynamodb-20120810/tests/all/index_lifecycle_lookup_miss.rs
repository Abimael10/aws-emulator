// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the error class `UpdateTable` answers an index-lifecycle entry that
//! names an index the addressed table does not carry.
//!
//! The RULE under test, one level above any one call:
//!
//! > An `UpdateTable` entry that updates or deletes an index names that index as a resource
//! > of the table. "The operation tried to access a nonexistent table or index" is what
//! > `ResourceNotFoundException` reports, and `UpdateTable` models it
//! > (`smithy_query(query="operation", operation="UpdateTable")`), so an entry naming an
//! > index that is not on the table is answered with `ResourceNotFoundException` at HTTP 400,
//! > "Requested resource not found: Index {index} for table {table}", whichever member
//! > carried the entry and whichever action it asked for. An index that IS on the table but
//! > is a LOCAL secondary index is a different defect: `GlobalSecondaryIndexUpdates` acts on
//! > global secondary indexes only, so a `Delete` of it is the `ValidationException` "Cannot
//! > delete an index that is not a GlobalSecondaryIndex: {index}".
//!
//! The rows span what the rule generalises over: which MEMBER carries the entry
//! (`GlobalSecondaryIndexUpdates`, `VectorIndexUpdates`), which ACTION it asks for (a
//! `Delete`, an `Update`), and how the name misses (a name nothing carries, a stored name in
//! another case, a stored name with a trailing character, the name of an index of another
//! KIND on this very table) — beside the control that an entry naming an index the table does
//! carry is applied.
//!
//! Every case drives the operations over the wire through the same `MultiServiceRouter` the
//! emulator's front doors use, with this crate's shared harness.

use serde_json::{json, Value};

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below drives: one global secondary index, one local secondary index
/// and one vector index, so a row can name an index of ANOTHER kind than the member it sends.
const TABLE: &str = "IndexLifecycleLookups";

/// The global secondary index the table carries.
const GLOBAL_INDEX: &str = "by-owner";

/// The local secondary index the table carries — on the table, but not a global one.
const LOCAL_INDEX: &str = "by-rank";

/// The vector index the table carries.
const VECTOR_INDEX: &str = "by-embedding";

/// Which member of `UpdateTable` carries the entry, and the body that sends `action` for
/// `index` through it.
type Member = (&'static str, fn(&str, Action) -> Value);

/// Which action an index-lifecycle entry asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// The index is to be removed.
    Delete,
    /// The index's own settings are to be moved.
    Update,
}

/// The `GlobalSecondaryIndexUpdates` body for one entry.
fn global_entry(index: &str, action: Action) -> Value {
    let entry = match action {
        Action::Delete => json!({"Delete": {"IndexName": index}}),
        Action::Update => json!({"Update": {
            "IndexName": index,
            "ProvisionedThroughput": {"ReadCapacityUnits": 9, "WriteCapacityUnits": 9},
        }}),
    };
    json!({"TableName": TABLE, "GlobalSecondaryIndexUpdates": [entry]})
}

/// The `VectorIndexUpdates` body for one entry. The member models a `Delete` and a `Create`
/// only (`smithy_query(query="shape", shape="VectorIndexUpdate")`), so an `Update` of a
/// vector index is sent through the secondary-index member instead, where the name still
/// names no index of that kind.
fn vector_entry(index: &str, action: Action) -> Value {
    match action {
        Action::Delete => {
            json!({"TableName": TABLE, "VectorIndexUpdates": [{"Delete": {"IndexName": index}}]})
        }
        Action::Update => global_entry(index, action),
    }
}

/// Create [`TABLE`] and wait for it to report `ACTIVE`.
async fn create_table(router: &smithy_server_runtime::MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
                {"AttributeName": "rank", "AttributeType": "N"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": GLOBAL_INDEX,
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "LocalSecondaryIndexes": [{
                "IndexName": LOCAL_INDEX,
                "KeySchema": [
                    {"AttributeName": "pk", "KeyType": "HASH"},
                    {"AttributeName": "rank", "KeyType": "RANGE"},
                ],
                "Projection": {"ProjectionType": "KEYS_ONLY"},
            }],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 4,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(router, "the table every row drives", TABLE).await;
}

#[tokio::test]
async fn an_entry_naming_an_index_the_table_does_not_carry_is_the_lookup_miss() {
    let router = emulator();
    create_table(&router).await;

    let members: [Member; 2] = [
        ("GlobalSecondaryIndexUpdates", global_entry),
        ("VectorIndexUpdates", vector_entry),
    ];
    // How the name misses the names the table does carry: a name of its own, a stored name
    // in another case, a stored name with a trailing character, and the name of an index of
    // the OTHER kind on this very table — which the member that sent it still carries no
    // index by.
    let names: [(&str, &str); 6] = [
        ("a name no index of the table carries", "by-nothing"),
        ("the shortest name the model allows", "zzz"),
        ("a stored name in another case", "BY-OWNER"),
        ("a stored name with a trailing character", "by-ownerr"),
        ("a stored name with a separator in it", "by.owner"),
        ("the name of an index of another kind", VECTOR_INDEX),
    ];

    for (member, build) in members {
        for action in [Action::Delete, Action::Update] {
            for (row, missing) in names {
                // The vector index IS the index the vector member carries, so that one pair
                // is the control below rather than a miss.
                if member == "VectorIndexUpdates"
                    && missing == VECTOR_INDEX
                    && action == Action::Delete
                {
                    continue;
                }
                let case = format!("{member} {action:?} of {missing:?} ({row})");
                let (status, body) = call(&router, "UpdateTable", build(missing, action)).await;
                assert_eq!(
                    (status, error_code(&body), message(&body)),
                    (
                        400,
                        "ResourceNotFoundException".to_string(),
                        format!("Requested resource not found: Index {missing} for table {TABLE}")
                    ),
                    "case {case}: expected the lookup miss on the index, got {status} with {body}"
                );
            }
        }
    }

    // Not one of those refusals wrote anything: the table never left `ACTIVE` and still
    // carries each index it was created with.
    let described = describe_when_active(&router, "after every refusal", TABLE).await;
    assert_eq!(
        index_names(&described, "GlobalSecondaryIndexes"),
        vec![GLOBAL_INDEX.to_string()],
        "case \"after every refusal\": expected the created global secondary index, got \
         {described}"
    );
    assert_eq!(
        index_names(&described, "LocalSecondaryIndexes"),
        vec![LOCAL_INDEX.to_string()],
        "case \"after every refusal\": expected the created local secondary index, got \
         {described}"
    );
    assert_eq!(
        index_names(&described, "VectorIndexes"),
        vec![VECTOR_INDEX.to_string()],
        "case \"after every refusal\": expected the created vector index, got {described}"
    );
}

#[tokio::test]
async fn a_delete_of_an_index_that_is_not_a_global_secondary_one_states_that_rule() {
    let router = emulator();
    create_table(&router).await;

    // The index is ON the table, so the entry is no lookup miss: the member it was sent
    // through acts on global secondary indexes only.
    let (status, body) = call(
        &router,
        "UpdateTable",
        global_entry(LOCAL_INDEX, Action::Delete),
    )
    .await;
    assert_eq!(
        (status, error_code(&body), message(&body)),
        (
            400,
            "ValidationException".to_string(),
            format!("Cannot delete an index that is not a GlobalSecondaryIndex: {LOCAL_INDEX}")
        ),
        "case \"a Delete of the local secondary index\": expected the rule the name broke, got \
         {status} with {body}"
    );

    // An `Update` of that same name is the lookup miss: the member carries no global
    // secondary index by it, and `Update` is not the action the rule above is about.
    let (status, body) = call(
        &router,
        "UpdateTable",
        global_entry(LOCAL_INDEX, Action::Update),
    )
    .await;
    assert_eq!(
        (status, error_code(&body), message(&body)),
        (
            400,
            "ResourceNotFoundException".to_string(),
            format!("Requested resource not found: Index {LOCAL_INDEX} for table {TABLE}")
        ),
        "case \"an Update of the local secondary index\": expected the lookup miss, got {status} \
         with {body}"
    );

    let described = describe_when_active(&router, "after the refusals", TABLE).await;
    assert_eq!(
        index_names(&described, "LocalSecondaryIndexes"),
        vec![LOCAL_INDEX.to_string()],
        "case \"after the refusals\": expected the local secondary index untouched, got \
         {described}"
    );
}

#[tokio::test]
async fn an_entry_naming_an_index_the_table_does_carry_is_applied() {
    let router = emulator();
    create_table(&router).await;

    // The control for both members: the index each DOES carry is removed, so the refusals
    // above are about the name and not about the member.
    ok(
        &router,
        "UpdateTable",
        global_entry(GLOBAL_INDEX, Action::Delete),
    )
    .await;
    // Index lifecycle work leaves the table `UPDATING`, and a table in that state takes no
    // second update, so the next entry waits for the clock to settle this one.
    describe_when_active(&router, "after the global secondary index delete", TABLE).await;
    ok(
        &router,
        "UpdateTable",
        vector_entry(VECTOR_INDEX, Action::Delete),
    )
    .await;

    let described = describe_when_active(&router, "after the deletes", TABLE).await;
    assert!(
        index_names(&described, "GlobalSecondaryIndexes").is_empty(),
        "case \"after the deletes\": expected the global secondary index gone, got {described}"
    );
    assert!(
        index_names(&described, "VectorIndexes").is_empty(),
        "case \"after the deletes\": expected the vector index gone, got {described}"
    );

    // And the name that is gone now misses exactly as every other absent name does.
    let (status, body) = call(
        &router,
        "UpdateTable",
        global_entry(GLOBAL_INDEX, Action::Delete),
    )
    .await;
    assert_eq!(
        (status, error_code(&body), message(&body)),
        (
            400,
            "ResourceNotFoundException".to_string(),
            format!("Requested resource not found: Index {GLOBAL_INDEX} for table {TABLE}")
        ),
        "case \"a second delete of the removed index\": expected the lookup miss, got {status} \
         with {body}"
    );
}

/// The `IndexName`s a description reports under `member`, in name order.
fn index_names(description: &Value, member: &str) -> Vec<String> {
    let mut names: Vec<String> = description[member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|index| {
            index["IndexName"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    names.sort();
    names
}
