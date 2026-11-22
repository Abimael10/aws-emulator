// SPDX-License-Identifier: CC0-1.0

//! What a DOCUMENT PATH may address once the addressed table's key schema is known, and
//! the two other rules a list of paths is held to.
//!
//! The rules pinned here:
//!
//! 1. a key attribute is declared as a scalar type, so a path that reaches INSIDE one —
//!    with a `.` name step or a `[]` index step, written out or behind an
//!    `ExpressionAttributeNames` alias — addresses a location the table cannot hold, and
//!    the request is refused naming that key attribute. It holds for every key attribute
//!    the table DECLARES a type for, its own key schema's and its secondary indexes', and
//!    in every member a request addresses document paths through, so the table below
//!    crosses the key attributes with the members and the operations that carry them;
//! 2. two paths that step through ONE location in two different ways — by name, which
//!    reads a map, and by index, which reads a list — cannot both be about one item, so
//!    the pair is refused naming both paths whole, in the order the expression stated
//!    them;
//! 3. a key condition that pins the whole PRIMARY key names one item, so the query
//!    evaluates at most that one and has no page for an `ExclusiveStartKey` to resume —
//!    while the same condition read through a SECONDARY INDEX, whose key is not unique,
//!    still pages.
//!
//! Each rule carries its CONTROL: a nested path into a non-key attribute, two paths that
//! agree about every location they share, and a condition that leaves the sort key free
//! are all served.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's
//! front doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses: a composite key schema, one global secondary
/// index keyed by `gpk` and one local secondary index keyed by `lsk`, so the rows can
/// name a key attribute of each kind.
const TABLE: &str = "KeyAttributePaths";

/// AWS's fixed sentence for a path that reaches inside a key attribute, with the key
/// attribute it dereferenced appended.
fn not_a_scalar_key(attribute: &str) -> String {
    format!(
        "Key attributes must be scalars; list random access '[]' and map lookup '.' are not \
         allowed: Key: {attribute}"
    )
}

/// AWS's fixed sentence for two paths that disagree about one location, with both paths
/// rendered as the token lists its refusals print a document path as.
fn conflicting(member: &str, one: &str, two: &str) -> String {
    format!(
        "Invalid {member}: Two document paths conflict with each other; must remove or rewrite \
         one of these paths; path one: {one}, path two: {two}"
    )
}

/// The primary key every row addresses.
fn key() -> Value {
    json!({"pk": {"S": "p"}, "sk": {"N": "1"}})
}

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write
/// three items of one partition, so a paged read has a page to walk.
async fn seeded(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "gpk", "AttributeType": "S"},
                {"AttributeName": "gsk", "AttributeType": "N"},
                {"AttributeName": "lsk", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": "ByGpk",
                    "KeySchema": [
                        {"AttributeName": "gpk", "KeyType": "HASH"},
                        {"AttributeName": "gsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": "ByLsk",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "lsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;
    describe_when_active(router, "seed the indexed table", TABLE).await;
    for sort in 1..=3 {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {
                    "pk": {"S": "p"},
                    "sk": {"N": sort.to_string()},
                    // Every item carries ONE global-index key, so that index's key is
                    // shared by three entries: an index key is not unique, which is what
                    // makes a fully pinned index query a page rather than one item.
                    "gpk": {"S": "g"},
                    "gsk": {"N": "1"},
                    "note": {"M": {"leaf": {"S": "n"}}},
                },
            }),
        )
        .await;
    }
}

/// One request that states a document path, and what the path breaks.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    /// The whole message the row earns.
    stated: String,
}

#[tokio::test]
async fn a_document_path_into_a_key_attribute_is_refused_naming_that_key() {
    let router = emulator();
    seeded(&router).await;

    // Rows differ in the KEY ATTRIBUTE the path dereferences (the partition key, the sort
    // key, a global index's key, a local index's key), in the STEP that dereferences it (a
    // name, an index, and a name behind an alias), and in the MEMBER and OPERATION that
    // carried the path — the rule is the table's key schema, so it holds across all of
    // them, and the sentence names the key attribute the path resolved to.
    let rows: Vec<Row> = vec![
        Row {
            case: "a projection reaching into the partition key",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "pk.leaf"}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a projection indexing the partition key as a list",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "pk[0]"}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a projection reaching into the partition key behind an alias",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "#alias.leaf",
                            "ExpressionAttributeNames": {"#alias": "pk"}}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a projection reaching into the sort key",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "sk.leaf"}),
            stated: not_a_scalar_key("sk"),
        },
        Row {
            case: "a projection reaching into a global index's key",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "gpk.leaf"}),
            stated: not_a_scalar_key("gpk"),
        },
        Row {
            case: "a projection reaching through a local index's key",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "lsk[1].leaf"}),
            stated: not_a_scalar_key("lsk"),
        },
        Row {
            case: "a query's projection reaching into the sort key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "p"}},
                            "ProjectionExpression": "sk.leaf"}),
            stated: not_a_scalar_key("sk"),
        },
        Row {
            case: "a query's filter reaching into a global index's key",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "FilterExpression": "gpk.leaf = :v",
                            "ExpressionAttributeValues": {":p": {"S": "p"},
                                                          ":v": {"S": "x"}}}),
            stated: not_a_scalar_key("gpk"),
        },
        Row {
            case: "a scan's projection reaching into the partition key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "ProjectionExpression": "pk.leaf"}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a scan's filter reaching into the sort key",
            operation: "Scan",
            request: json!({"TableName": TABLE, "FilterExpression": "sk.leaf = :n",
                            "ExpressionAttributeValues": {":n": {"N": "1"}}}),
            stated: not_a_scalar_key("sk"),
        },
        Row {
            case: "a batched read's projection reaching into the partition key",
            operation: "BatchGetItem",
            request: json!({"RequestItems": {TABLE: {"Keys": [key()],
                                                     "ProjectionExpression": "pk.leaf"}}}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a transactional read's projection reaching into a global index's key",
            operation: "TransactGetItems",
            request: json!({"TransactItems": [{"Get": {"TableName": TABLE, "Key": key(),
                                                       "ProjectionExpression": "gpk.leaf"}}]}),
            stated: not_a_scalar_key("gpk"),
        },
        Row {
            case: "a put's condition asking whether a location inside the partition key exists",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": key(),
                            "ConditionExpression": "attribute_exists(pk.leaf)"}),
            stated: not_a_scalar_key("pk"),
        },
        Row {
            case: "a delete's condition comparing a location inside a local index's key",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ConditionExpression": "lsk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: not_a_scalar_key("lsk"),
        },
        Row {
            case: "an update's condition indexing the sort key as a list",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET note = :v",
                            "ConditionExpression": "sk[0] = :n",
                            "ExpressionAttributeValues": {":v": {"S": "x"},
                                                          ":n": {"N": "1"}}}),
            stated: not_a_scalar_key("sk"),
        },
        Row {
            case: "an update writing into a global index's key",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET gpk.leaf = :v",
                            "ExpressionAttributeValues": {":v": {"S": "x"}}}),
            stated: not_a_scalar_key("gpk"),
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", row.stated.as_str()),
            "case {:?} on {}: expected the refusal {:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.stated
        );
    }

    // The CONTROL: the rule is about KEY attributes, so a path reaching inside an ordinary
    // attribute is as ordinary as naming the key attribute itself, in every member.
    let served: Vec<(&str, &str, Value)> = vec![
        (
            "a projection reaching into a non-key attribute",
            "GetItem",
            json!({"TableName": TABLE, "Key": key(), "ProjectionExpression": "note.leaf"}),
        ),
        (
            "a projection naming the key attributes whole",
            "GetItem",
            json!({"TableName": TABLE, "Key": key(), "ProjectionExpression": "pk, sk"}),
        ),
        (
            "a scan's filter reaching into a non-key attribute",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "note.leaf = :v",
                   "ExpressionAttributeValues": {":v": {"S": "n"}}}),
        ),
        (
            "an update writing into a non-key attribute",
            "UpdateItem",
            json!({"TableName": TABLE, "Key": key(),
                   "UpdateExpression": "SET note.leaf = :v",
                   "ExpressionAttributeValues": {":v": {"S": "x"}}}),
        ),
    ];
    for (case, operation, request) in &served {
        let (status, body) = call(&router, operation, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?} on {operation}: a path into a non-key attribute is served, got \
             {status} with {body}"
        );
    }
}

#[tokio::test]
async fn two_paths_that_disagree_about_one_location_conflict() {
    let router = emulator();
    seeded(&router).await;

    // Rows differ in the ORDER the two paths were stated (which decides which is `path
    // one`), in the DEPTH of the location they disagree about, and in the member that
    // listed them — the member a WRITE states its intent through carries the validation
    // envelope, which is the one difference between the two sentences.
    let rows: Vec<Row> = vec![
        Row {
            case: "a projection reading one attribute as a map and as a list",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "a.b, a[1]"}),
            stated: conflicting("ProjectionExpression", "[a, b]", "[a, [1]]"),
        },
        Row {
            case: "the same pair the other way round",
            operation: "GetItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "ProjectionExpression": "a[1], a.b"}),
            stated: conflicting("ProjectionExpression", "[a, [1]]", "[a, b]"),
        },
        Row {
            case: "a disagreement two steps inside the item",
            operation: "Scan",
            request: json!({"TableName": TABLE,
                            "ProjectionExpression": "m.child[0].x, m.child.y"}),
            stated: conflicting("ProjectionExpression", "[m, child, [0], x]", "[m, child, y]"),
        },
        Row {
            case: "a query's projection reading one attribute both ways",
            operation: "Query",
            request: json!({"TableName": TABLE,
                            "KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "p"}},
                            "ProjectionExpression": "a.b, a[1]"}),
            stated: conflicting("ProjectionExpression", "[a, b]", "[a, [1]]"),
        },
        Row {
            case: "an update writing one attribute as a map and as a list",
            operation: "UpdateItem",
            request: json!({"TableName": TABLE, "Key": key(),
                            "UpdateExpression": "SET a.b = :v, a[1] = :w",
                            "ExpressionAttributeValues": {":v": {"S": "x"},
                                                          ":w": {"S": "y"}}}),
            stated: format!(
                "1 validation error detected: {}",
                conflicting("UpdateExpression", "[a, b]", "[a, [1]]")
            ),
        },
    ];

    for row in &rows {
        let (status, body) = call(&router, row.operation, row.request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", row.stated.as_str()),
            "case {:?} on {}: expected the refusal {:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.stated
        );
    }

    // The CONTROL: paths that step through every shared location the SAME way say nothing
    // contradictory, however many of them there are.
    let served: Vec<(&str, Value)> = vec![
        (
            "two map lookups inside one attribute",
            json!({"TableName": TABLE, "Key": key(), "ProjectionExpression": "a.b, a.c"}),
        ),
        (
            "two indexes of one list",
            json!({"TableName": TABLE, "Key": key(), "ProjectionExpression": "a[0], a[1]"}),
        ),
        (
            "two leaves under one list element",
            json!({"TableName": TABLE, "Key": key(),
                   "ProjectionExpression": "m.child[0].x, m.child[0].y"}),
        ),
    ];
    for (case, request) in &served {
        let (status, body) = call(&router, "GetItem", request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: paths that agree about every shared location are served, got \
             {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_query_pinning_the_whole_key_has_no_page_to_resume() {
    let router = emulator();
    seeded(&router).await;

    // A condition pinning both halves of the key names ONE key of the schema, so there is
    // no page past it: the token is refused as the invalid member it is, whichever way the
    // condition spells the two equalities.
    let pinned: Vec<(&str, Value)> = vec![
        (
            "a full key equality with the selected key as the token",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "pk = :p AND sk = :s",
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":s": {"N": "1"}},
                   "ExclusiveStartKey": key()}),
        ),
        (
            "the same condition with its operands written the other way round",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "pk = :p AND :s = sk",
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":s": {"N": "2"}},
                   "ExclusiveStartKey": {"pk": {"S": "p"}, "sk": {"N": "2"}}}),
        ),
        (
            "the same condition behind an alias for the sort key",
            json!({"TableName": TABLE,
                   "KeyConditionExpression": "pk = :p AND #alias = :s",
                   "ExpressionAttributeNames": {"#alias": "sk"},
                   "ExpressionAttributeValues": {":p": {"S": "p"}, ":s": {"N": "3"}},
                   "ExclusiveStartKey": {"pk": {"S": "p"}, "sk": {"N": "3"}}}),
        ),
    ];
    for (case, request) in &pinned {
        let (status, body) = call(&router, "Query", request.clone()).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (
                400,
                "ValidationException",
                "The provided exclusive start key is invalid"
            ),
            "case {case:?}: a query naming one key has no page to resume, got {status} with \
             {body}"
        );
    }

    // The CONTROL: a condition that leaves the sort key free selects a RANGE of keys, which
    // is what a page walks — so the token resumes it, and the page it reports resumes again.
    let first = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "KeyConditionExpression": "pk = :p",
               "ExpressionAttributeValues": {":p": {"S": "p"}},
               "Limit": 1}),
    )
    .await;
    assert_eq!(
        first["Count"],
        json!(1),
        "the first page of a range query carries its one item, got {first}"
    );
    let resumed = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "KeyConditionExpression": "pk = :p",
               "ExpressionAttributeValues": {":p": {"S": "p"}},
               "ExclusiveStartKey": first["LastEvaluatedKey"].clone()}),
    )
    .await;
    assert_eq!(
        resumed["Count"],
        json!(2),
        "the page resumed from the token carries the rest of the partition, got {resumed}"
    );
    // And a condition that narrows the sort key without pinning it pages too.
    let narrowed = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "KeyConditionExpression": "pk = :p AND sk > :s",
               "ExpressionAttributeValues": {":p": {"S": "p"}, ":s": {"N": "1"}},
               "ExclusiveStartKey": {"pk": {"S": "p"}, "sk": {"N": "2"}}}),
    )
    .await;
    assert_eq!(
        narrowed["Count"],
        json!(1),
        "a range condition resumes strictly after the token, got {narrowed}"
    );

    // And the control the rule's PREMISE rests on: a SECONDARY INDEX's key is not unique,
    // so the same full-equality condition read through one selects a page of entries — the
    // three items share this index's key — and the token this query itself reported resumes
    // it.
    let index_page = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "IndexName": "ByGpk",
               "KeyConditionExpression": "gpk = :g AND gsk = :g2",
               "ExpressionAttributeValues": {":g": {"S": "g"}, ":g2": {"N": "1"}},
               "Limit": 1}),
    )
    .await;
    assert_eq!(
        index_page["Count"],
        json!(1),
        "the first page of a fully pinned index query carries its one item, got {index_page}"
    );
    let index_resumed = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "IndexName": "ByGpk",
               "KeyConditionExpression": "gpk = :g AND gsk = :g2",
               "ExpressionAttributeValues": {":g": {"S": "g"}, ":g2": {"N": "1"}},
               "ExclusiveStartKey": index_page["LastEvaluatedKey"].clone()}),
    )
    .await;
    assert_eq!(
        index_resumed["Count"],
        json!(2),
        "a fully pinned INDEX query resumes from its own token, got {index_resumed}"
    );
}
