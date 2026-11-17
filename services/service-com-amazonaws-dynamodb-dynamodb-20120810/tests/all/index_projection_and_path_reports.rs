// SPDX-License-Identifier: CC0-1.0

//! What a secondary index can be ASKED for, and what a projection's document paths
//! REPORT.
//!
//! The rules these suites pin:
//!
//! * a read served from a GLOBAL secondary index can only request attributes the index
//!   projects — "If you query or scan a global secondary index, you can only request
//!   attributes that are projected into the index. Global secondary index queries cannot
//!   fetch attributes from the parent table" (`Query$Select`, repeated by `Scan$Select`)
//!   — so a `ProjectionExpression` naming one it does not project is refused, while a
//!   LOCAL index, which "fetches each of these attributes from the parent table", serves
//!   it;
//! * a QUERY of either index kind cannot decide a filter predicate over an attribute the
//!   index does not project, and is refused; the SCAN of the same index evaluates the
//!   predicate against the entry the index holds, which simply matches nothing;
//! * two projection paths that step through the SAME container report under ONE entry
//!   for it, and a container is the same container when the same STEP addresses it — so
//!   paths through one list index merge into one element and a list reports one element
//!   per index named, in index order;
//! * a reported list is therefore DENSE and re-indexed from zero: it carries exactly one
//!   element per index the projection matched, in ascending index order, with no
//!   placeholder standing in for a position no path named — at every depth a document path
//!   reaches, and whichever reader reports it;
//! * the legacy `AttributesToGet` is a list of attribute NAMES, so a name it repeats is a
//!   duplicate value in that list rather than two paths addressing one location, and an
//!   overlapping `ProjectionExpression` is the defect a request is refused for even when
//!   it also defines a substitution nothing uses;
//! * a key condition cannot state more comparisons than the key schema it reads declares
//!   key attributes, and the refusal states both counts.
//!
//! Each suite checks its rule over a TABLE of inputs: every projection type on both
//! index kinds crossed with `Query` and `Scan`, the projection and the filter surfaces,
//! the path forms that share a container and the ones that do not, the index positions and
//! nesting depths a projected list element is named at, and each key-schema width with the
//! comparison counts around its bound.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every suite here reads. One table name per emulator, and each suite builds
/// its own emulator, so the name is a constant.
const TABLE: &str = "ProjectionRules";

/// The items every suite seeds.
///
/// `note` is projected into the `INCLUDE` index and `extra`/`spare` into none of them,
/// so a read naming them is a read of an attribute its index may not hold. `parts` is a
/// list of maps whose first element carries a third member no projection below names,
/// and `deep` nests a list inside a map inside a list, which is what two paths sharing
/// more than one container step through.
fn stored_items() -> Vec<Value> {
    vec![
        json!({"pk": {"S": "p1"}, "sk": {"S": "s1"}, "gpk": {"S": "g1"}, "lsk": {"S": "l1"},
               "note": {"S": "n1"}, "extra": {"S": "e1"}, "spare": {"S": "o1"},
               "parts": {"L": [
                   {"M": {"a": {"S": "a0"}, "b": {"S": "b0"}, "c": {"S": "c0"}}},
                   {"M": {"a": {"S": "a1"}, "b": {"S": "b1"}}},
               ]},
               "doc": {"M": {"a": {"S": "da"}, "b": {"S": "db"}, "c": {"S": "dc"}}},
               "deep": {"L": [{"M": {"twigs": {"L": [
                   {"M": {"leaf": {"S": "deep-leaf"}, "sibling": {"S": "deep-sibling"}}},
               ]}}}]}}),
        json!({"pk": {"S": "p1"}, "sk": {"S": "s2"}, "gpk": {"S": "g1"}, "lsk": {"S": "l2"},
               "note": {"S": "n2"}, "extra": {"S": "e2"},
               "cells": {"L": [{"M": {"a": {"S": "a-was"}, "b": {"S": "b-was"},
                                      "c": {"S": "c-was"}}}]}}),
    ]
}

/// Create [`TABLE`] with a global index of each projection type and a `KEYS_ONLY` local
/// one, wait out the `CREATING` -> `ACTIVE` transition, and write every seeded item.
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
                {"AttributeName": "sk", "AttributeType": "S"},
                {"AttributeName": "gpk", "AttributeType": "S"},
                {"AttributeName": "lsk", "AttributeType": "S"},
            ],
            "BillingMode": "PAY_PER_REQUEST",
            "GlobalSecondaryIndexes": [
                {
                    "IndexName": "GsiKeysOnly",
                    "KeySchema": [{"AttributeName": "gpk", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                },
                {
                    "IndexName": "GsiInclude",
                    "KeySchema": [{"AttributeName": "gpk", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "INCLUDE",
                                   "NonKeyAttributes": ["note"]},
                },
                {
                    "IndexName": "GsiAll",
                    "KeySchema": [{"AttributeName": "gpk", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": "LsiKeysOnly",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "lsk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                },
            ],
        }),
    )
    .await;
    describe_when_active(router, "seed the projection table", TABLE).await;
    for item in stored_items() {
        ok(router, "PutItem", json!({"TableName": TABLE, "Item": item})).await;
    }
}

/// The key condition that selects one partition of `index` — the index's own partition
/// key when it has one of its own, and the table's when the read is a base-table read or
/// goes through the local index.
fn condition_for(index: Option<&str>) -> Value {
    match index {
        Some("LsiKeysOnly") | None => json!({"KeyConditionExpression": "pk = :p",
                                             "ExpressionAttributeValues": {":p": {"S": "p1"}}}),
        Some(_) => json!({"KeyConditionExpression": "gpk = :g",
                          "ExpressionAttributeValues": {":g": {"S": "g1"}}}),
    }
}

/// One read request: the operation, the index it names (`None` for the table itself), and
/// the members that narrow it, merged over the key condition a `Query` needs.
fn read_request(operation: &str, index: Option<&str>, members: &Value) -> Value {
    let mut body = json!({"TableName": TABLE});
    if let Some(index) = index {
        body["IndexName"] = json!(index);
    }
    if operation == "Query" {
        for (name, value) in condition_for(index).as_object().cloned().unwrap_or_default() {
            body[name] = value;
        }
    }
    for (name, value) in members.as_object().cloned().unwrap_or_default() {
        // A request that states its own values merges them into the condition's, so a
        // filter may compare against a placeholder of its own.
        if name == "ExpressionAttributeValues" {
            let mut values = body
                .get("ExpressionAttributeValues")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (placeholder, held) in value.as_object().cloned().unwrap_or_default() {
                values.insert(placeholder, held);
            }
            body["ExpressionAttributeValues"] = Value::Object(values);
            continue;
        }
        body[name] = value;
    }
    body
}

// ===========================================================================
// 1. What a secondary index can be asked for
// ===========================================================================

/// One row: the case, the operation, the index it names, the members that narrow the
/// read, and the message the service must refuse it with (empty where the read is
/// served).
struct IndexReadRow {
    case: &'static str,
    operation: &'static str,
    index: Option<&'static str>,
    members: Value,
    refusal: String,
}

/// How a projection a global secondary index cannot answer is refused, over the index and
/// the attributes it does not project.
fn unprojected_projection(index: &str, attributes: &str) -> String {
    format!(
        "One or more parameter values were invalid: Global secondary index {index} does not \
         project [{attributes}]"
    )
}

/// How a filter the index a query reads cannot decide is refused, over that index and the
/// filter attributes it does not project.
fn unprojected_filter(index: &str, attributes: &str) -> String {
    format!(
        "One or more parameter values were invalid: Secondary index {index} does not project \
         one or more filter attributes: [{attributes}]"
    )
}

/// The projection and filter cases, spanning each projection type on both index kinds and
/// the base table, for both readers.
fn index_read_rows() -> Vec<IndexReadRow> {
    let projecting = |expression: &str| json!({"ProjectionExpression": expression});
    let filtering = |expression: &str| {
        json!({"FilterExpression": expression,
               "ExpressionAttributeValues": {":f": {"S": "n1"}}})
    };
    vec![
        IndexReadRow {
            case: "a query projecting a KEYS_ONLY global index's own keys",
            operation: "Query",
            index: Some("GsiKeysOnly"),
            members: projecting("gpk, pk, sk"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a query projecting an attribute a KEYS_ONLY global index drops",
            operation: "Query",
            index: Some("GsiKeysOnly"),
            members: projecting("note"),
            refusal: unprojected_projection("GsiKeysOnly", "note"),
        },
        IndexReadRow {
            case: "a query projecting the attribute an INCLUDE global index names",
            operation: "Query",
            index: Some("GsiInclude"),
            members: projecting("note"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a query projecting an attribute an INCLUDE global index excludes",
            operation: "Query",
            index: Some("GsiInclude"),
            members: projecting("extra"),
            refusal: unprojected_projection("GsiInclude", "extra"),
        },
        IndexReadRow {
            case: "a query projecting two attributes the index excludes",
            operation: "Query",
            index: Some("GsiInclude"),
            members: projecting("spare, extra"),
            // Reported in name order, whichever order the expression listed them in.
            refusal: unprojected_projection("GsiInclude", "extra, spare"),
        },
        IndexReadRow {
            case: "a query projecting a path INSIDE an attribute the index excludes",
            operation: "Query",
            index: Some("GsiInclude"),
            members: projecting("doc.a"),
            refusal: unprojected_projection("GsiInclude", "doc"),
        },
        IndexReadRow {
            case: "a query projecting anything at all from an ALL global index",
            operation: "Query",
            index: Some("GsiAll"),
            members: projecting("extra, doc.a"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a query projecting an attribute a KEYS_ONLY LOCAL index drops",
            operation: "Query",
            index: Some("LsiKeysOnly"),
            members: projecting("note"),
            // "DynamoDB fetches each of these attributes from the parent table".
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a base-table query projecting anything at all",
            operation: "Query",
            index: None,
            members: projecting("extra, doc.a"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a scan projecting an attribute a KEYS_ONLY global index drops",
            operation: "Scan",
            index: Some("GsiKeysOnly"),
            members: projecting("note"),
            refusal: unprojected_projection("GsiKeysOnly", "note"),
        },
        IndexReadRow {
            case: "a scan projecting the attribute an INCLUDE global index names",
            operation: "Scan",
            index: Some("GsiInclude"),
            members: projecting("note"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a scan projecting an attribute a KEYS_ONLY LOCAL index drops",
            operation: "Scan",
            index: Some("LsiKeysOnly"),
            members: projecting("note"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a query filtering on an attribute a KEYS_ONLY global index drops",
            operation: "Query",
            index: Some("GsiKeysOnly"),
            members: filtering("note = :f"),
            refusal: unprojected_filter("GsiKeysOnly", "note"),
        },
        IndexReadRow {
            case: "a query filtering on an attribute an INCLUDE global index excludes",
            operation: "Query",
            index: Some("GsiInclude"),
            members: filtering("extra = :f"),
            refusal: unprojected_filter("GsiInclude", "extra"),
        },
        IndexReadRow {
            case: "a query filtering on the attribute an INCLUDE global index names",
            operation: "Query",
            index: Some("GsiInclude"),
            members: filtering("note = :f"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a query filtering on an attribute a KEYS_ONLY LOCAL index drops",
            operation: "Query",
            index: Some("LsiKeysOnly"),
            members: filtering("note = :f"),
            refusal: unprojected_filter("LsiKeysOnly", "note"),
        },
        IndexReadRow {
            case: "a counting query filtering on an attribute the index drops",
            operation: "Query",
            index: Some("GsiKeysOnly"),
            members: json!({"Select": "COUNT", "FilterExpression": "note = :f",
                            "ExpressionAttributeValues": {":f": {"S": "n1"}}}),
            refusal: unprojected_filter("GsiKeysOnly", "note"),
        },
        IndexReadRow {
            case: "a query filtering on anything at all over an ALL global index",
            operation: "Query",
            index: Some("GsiAll"),
            members: filtering("note = :f"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a base-table query filtering on anything at all",
            operation: "Query",
            index: None,
            members: filtering("note = :f"),
            refusal: String::new(),
        },
        IndexReadRow {
            case: "a scan filtering on an attribute a KEYS_ONLY global index drops",
            operation: "Scan",
            index: Some("GsiKeysOnly"),
            // The entry carries nothing at that name, so the predicate matches nothing —
            // the scan is served.
            members: filtering("note = :f"),
            refusal: String::new(),
        },
    ]
}

#[tokio::test]
async fn a_read_of_an_index_can_only_ask_for_what_the_index_projects() {
    let router = emulator();
    seeded(&router).await;

    for row in index_read_rows() {
        let request = read_request(row.operation, row.index, &row.members);
        let (status, body) = call(&router, row.operation, request.clone()).await;
        if row.refusal.is_empty() {
            assert_eq!(
                status, 200,
                "case {:?}: expected the read to be served, got {status} with {body}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {:?}: expected a ValidationException, got {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            row.refusal,
            "case {:?}: expected the message {:?}, got {body}",
            row.case,
            row.refusal
        );
        assert!(
            body.get("Items").is_none_or(Value::is_null),
            "case {:?}: a refused read reports no items, got {body}",
            row.case
        );
    }

    // The scan a global index CAN serve reports the entries, and the one whose filter
    // names an attribute the index does not hold matches nothing rather than everything.
    let served = ok(
        &router,
        "Scan",
        read_request(
            "Scan",
            Some("GsiKeysOnly"),
            &json!({"FilterExpression": "note = :f",
                    "ExpressionAttributeValues": {":f": {"S": "n1"}}}),
        ),
    )
    .await;
    assert_eq!(
        served["Count"], json!(0),
        "case a scan filtering on an unprojected attribute: the entries match nothing, got \
         {served}"
    );
    assert_eq!(
        served["ScannedCount"], json!(2),
        "case a scan filtering on an unprojected attribute: every entry was still read, got \
         {served}"
    );

    // The local index fetches what it does not project from the parent table, so the
    // attribute it drops is still reported.
    let fetched = ok(
        &router,
        "Query",
        read_request(
            "Query",
            Some("LsiKeysOnly"),
            &json!({"ProjectionExpression": "note"}),
        ),
    )
    .await;
    assert_eq!(
        fetched["Items"],
        json!([{"note": {"S": "n1"}}, {"note": {"S": "n2"}}]),
        "case a local index projecting an attribute it drops: it is fetched from the table, \
         got {fetched}"
    );
}

// ===========================================================================
// 2. What a projection's document paths report
// ===========================================================================

/// One projection form: the expression and the item every read path must report for the
/// seeded item `p1/s1`.
struct PathReportRow {
    case: &'static str,
    expression: &'static str,
    reported: Value,
}

/// The path forms, spanning the containers two paths can share and the ones they cannot:
/// one list index, two different indexes, a map entry, nested containers, an index the
/// item holds nothing at, and a whole container beside a path into another.
fn path_report_rows() -> Vec<PathReportRow> {
    vec![
        PathReportRow {
            case: "two paths through the SAME list index",
            expression: "parts[0].a, parts[0].b",
            // ONE element carrying both leaves, and not the sibling `c` no path named.
            reported: json!({"parts": {"L": [{"M": {"a": {"S": "a0"}, "b": {"S": "b0"}}}]}}),
        },
        PathReportRow {
            case: "two paths through DIFFERENT list indexes",
            expression: "parts[0].a, parts[1].b",
            reported: json!({"parts": {"L": [{"M": {"a": {"S": "a0"}}},
                                             {"M": {"b": {"S": "b1"}}}]}}),
        },
        PathReportRow {
            case: "two list indexes named out of order",
            // The elements report in INDEX order, whichever order the expression listed.
            expression: "parts[1].b, parts[0].a",
            reported: json!({"parts": {"L": [{"M": {"a": {"S": "a0"}}},
                                             {"M": {"b": {"S": "b1"}}}]}}),
        },
        PathReportRow {
            case: "three paths, two of them through one index",
            expression: "parts[0].a, parts[0].c, parts[1].a",
            reported: json!({"parts": {"L": [{"M": {"a": {"S": "a0"}, "c": {"S": "c0"}}},
                                             {"M": {"a": {"S": "a1"}}}]}}),
        },
        PathReportRow {
            case: "one whole list element",
            expression: "parts[1]",
            reported: json!({"parts": {"L": [{"M": {"a": {"S": "a1"}, "b": {"S": "b1"}}}]}}),
        },
        PathReportRow {
            case: "a list index the item holds nothing at",
            expression: "parts[7].a",
            reported: json!({}),
        },
        PathReportRow {
            case: "two paths through the same map entry",
            expression: "doc.a, doc.b",
            reported: json!({"doc": {"M": {"a": {"S": "da"}, "b": {"S": "db"}}}}),
        },
        PathReportRow {
            case: "two paths sharing a list index INSIDE a map inside a list",
            expression: "deep[0].twigs[0].leaf, deep[0].twigs[0].sibling",
            reported: json!({"deep": {"L": [{"M": {"twigs": {"L": [{"M": {
                "leaf": {"S": "deep-leaf"}, "sibling": {"S": "deep-sibling"}}}]}}}]}}),
        },
        PathReportRow {
            case: "a whole attribute beside a path into another",
            expression: "note, parts[0].b",
            reported: json!({"note": {"S": "n1"},
                             "parts": {"L": [{"M": {"b": {"S": "b0"}}}]}}),
        },
    ]
}

#[tokio::test]
async fn paths_that_share_a_container_report_under_one_entry_for_it() {
    let router = emulator();
    seeded(&router).await;

    for row in path_report_rows() {
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}},
                   "ProjectionExpression": row.expression}),
        )
        .await;
        let from_get = read.get("Item").cloned().unwrap_or(json!({}));
        assert_eq!(
            from_get, row.reported,
            "case {:?} on GetItem: expected {}, got {from_get}",
            row.case, row.reported
        );

        let queried = ok(
            &router,
            "Query",
            json!({"TableName": TABLE, "ProjectionExpression": row.expression,
                   "KeyConditionExpression": "pk = :p AND sk = :s",
                   "ExpressionAttributeValues": {":p": {"S": "p1"}, ":s": {"S": "s1"}}}),
        )
        .await;
        let from_query = queried["Items"]
            .as_array()
            .and_then(|items| items.first().cloned())
            .unwrap_or(json!({}));
        assert_eq!(
            from_query, row.reported,
            "case {:?} on Query: expected {}, got {from_query}",
            row.case, row.reported
        );
    }

    // The same reporter answers an update's `UPDATED_NEW`, which reports the paths the
    // update CHANGED: two clauses through one list index report one element too.
    let changed = ok(
        &router,
        "UpdateItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s2"}},
               "UpdateExpression": "SET cells[0].a = :a, cells[0].b = :b",
               "ExpressionAttributeValues": {":a": {"S": "A"}, ":b": {"S": "B"}},
               "ReturnValues": "UPDATED_NEW"}),
    )
    .await;
    assert_eq!(
        changed["Attributes"],
        json!({"cells": {"L": [{"M": {"a": {"S": "A"}, "b": {"S": "B"}}}]}}),
        "case an update changing two paths through one list index: one element carries both, \
         got {changed}"
    );
}

// ===========================================================================
// 3. Which defect a narrowing request is refused for
// ===========================================================================

/// One refusal row: the case, the operation, the request body, and the exact message.
struct NarrowingRefusalRow {
    case: &'static str,
    operation: &'static str,
    body: Value,
    message: String,
}

/// The duplicate-name refusal, over the repeated name the request sent.
fn duplicate_name(name: &str) -> String {
    format!("One or more parameter values were invalid: Duplicate value in attribute name: {name}")
}

/// The overlapping-paths refusal, over the two paths as AWS prints them.
fn overlap(one: &str, two: &str) -> String {
    format!(
        "Invalid ProjectionExpression: Two document paths overlap with each other; must remove \
         or rewrite one of these paths; path one: {one}, path two: {two}"
    )
}

/// The rows: a repeated legacy name on each surface that takes the legacy member, an
/// overlap stated beside a substitution nothing uses on each surface that takes a
/// projection, and the `Select` that lists nothing to return.
fn narrowing_refusal_rows() -> Vec<NarrowingRefusalRow> {
    let key = json!({"pk": {"S": "p1"}, "sk": {"S": "s1"}});
    vec![
        NarrowingRefusalRow {
            case: "a read listing one legacy name twice",
            operation: "GetItem",
            body: json!({"TableName": TABLE, "Key": key,
                         "AttributesToGet": ["note", "note"]}),
            message: duplicate_name("note"),
        },
        NarrowingRefusalRow {
            case: "a read whose legacy list repeats its THIRD name",
            operation: "GetItem",
            body: json!({"TableName": TABLE, "Key": key,
                         "AttributesToGet": ["pk", "note", "extra", "note"]}),
            message: duplicate_name("note"),
        },
        NarrowingRefusalRow {
            case: "a query listing one legacy name twice",
            operation: "Query",
            // The legacy projection travels with the legacy key condition: "Can not use
            // both expression and non-expression parameters in the same request".
            body: json!({"TableName": TABLE, "AttributesToGet": ["note", "note"],
                         "KeyConditions": {"pk": {"ComparisonOperator": "EQ",
                                                  "AttributeValueList": [{"S": "p1"}]}}}),
            message: duplicate_name("note"),
        },
        NarrowingRefusalRow {
            case: "a scan listing one legacy name twice",
            operation: "Scan",
            body: json!({"TableName": TABLE, "AttributesToGet": ["extra", "extra"]}),
            message: duplicate_name("extra"),
        },
        NarrowingRefusalRow {
            case: "a batch read listing one legacy name twice",
            operation: "BatchGetItem",
            body: json!({"RequestItems": {TABLE: {"Keys": [key],
                                                  "AttributesToGet": ["note", "note"]}}}),
            message: duplicate_name("note"),
        },
        NarrowingRefusalRow {
            case: "a read whose projection overlaps beside an unused name",
            operation: "GetItem",
            body: json!({"TableName": TABLE, "Key": key,
                         "ProjectionExpression": "doc, doc.a",
                         "ExpressionAttributeNames": {"#z": "note"}}),
            message: overlap("[doc]", "[doc, a]"),
        },
        NarrowingRefusalRow {
            case: "a query whose projection overlaps beside an unused name",
            operation: "Query",
            body: json!({"TableName": TABLE, "ProjectionExpression": "doc, doc.a",
                         "ExpressionAttributeNames": {"#z": "note"},
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "p1"}}}),
            message: overlap("[doc]", "[doc, a]"),
        },
        NarrowingRefusalRow {
            case: "a scan whose projection overlaps beside an unused name",
            operation: "Scan",
            body: json!({"TableName": TABLE, "ProjectionExpression": "parts[0], parts[0].a",
                         "ExpressionAttributeNames": {"#z": "note"}}),
            message: overlap("[parts, [0]]", "[parts, [0], a]"),
        },
        NarrowingRefusalRow {
            case: "a batch read whose projection overlaps beside an unused name",
            operation: "BatchGetItem",
            body: json!({"RequestItems": {TABLE: {
                "Keys": [key],
                "ProjectionExpression": "doc, doc.a",
                "ExpressionAttributeNames": {"#z": "note"},
            }}}),
            message: overlap("[doc]", "[doc, a]"),
        },
        NarrowingRefusalRow {
            case: "a transactional read whose projection overlaps beside an unused name",
            operation: "TransactGetItems",
            body: json!({"TransactItems": [{"Get": {
                "TableName": TABLE,
                "Key": key,
                "ProjectionExpression": "doc, doc.a",
                "ExpressionAttributeNames": {"#z": "note"},
            }}]}),
            message: overlap("[doc]", "[doc, a]"),
        },
        NarrowingRefusalRow {
            case: "a query of specific attributes listing none",
            operation: "Query",
            body: json!({"TableName": TABLE, "Select": "SPECIFIC_ATTRIBUTES",
                         "KeyConditionExpression": "pk = :p",
                         "ExpressionAttributeValues": {":p": {"S": "p1"}}}),
            message: "1 validation error detected: Must specify the AttributesToGet or \
                      ProjectionExpression when choosing to get SPECIFIC_ATTRIBUTES"
                .to_string(),
        },
        NarrowingRefusalRow {
            case: "a scan of specific attributes listing none",
            operation: "Scan",
            body: json!({"TableName": TABLE, "Select": "SPECIFIC_ATTRIBUTES"}),
            message: "1 validation error detected: Must specify the AttributesToGet or \
                      ProjectionExpression when choosing to get SPECIFIC_ATTRIBUTES"
                .to_string(),
        },
    ]
}

#[tokio::test]
async fn a_narrowing_request_is_refused_for_the_defect_in_its_projection() {
    let router = emulator();
    seeded(&router).await;

    for row in narrowing_refusal_rows() {
        let (status, body) = call(&router, row.operation, row.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {:?}: expected a ValidationException, got {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            row.message,
            "case {:?}: expected the message {:?}, got {body}",
            row.case,
            row.message
        );
    }

    // A legacy list that repeats NOTHING is served, and reports the names it listed.
    let served = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "p1"}, "sk": {"S": "s1"}},
               "AttributesToGet": ["note", "extra"]}),
    )
    .await;
    assert_eq!(
        served["Item"],
        json!({"note": {"S": "n1"}, "extra": {"S": "e1"}}),
        "case a legacy list with no repeated name: it is served, got {served}"
    );
}

// ===========================================================================
// 4. How many comparisons one key condition may state
// ===========================================================================

/// One key-condition row: the case, the index the query reads, the members stating the
/// condition, and the message the service must refuse it with (empty where it is served).
struct KeyConditionRow {
    case: &'static str,
    index: Option<&'static str>,
    members: Value,
    refusal: String,
}

/// The refusal for a condition stating more comparisons than the schema has key
/// attributes, over both counts.
fn too_many_conditions(conditions: usize, key_attributes: usize) -> String {
    format!(
        "The number of query conditions ({conditions}) exceeds the number of key attributes \
         defined in the schema ({key_attributes})"
    )
}

/// The rows: each key-schema width this table offers — the base table's two, a global
/// index's one — with the comparison count at its bound and past it, stated in both
/// members a key condition travels in.
fn key_condition_rows() -> Vec<KeyConditionRow> {
    vec![
        KeyConditionRow {
            case: "one comparison on a two-key schema",
            index: None,
            members: json!({"KeyConditionExpression": "pk = :p",
                            "ExpressionAttributeValues": {":p": {"S": "p1"}}}),
            refusal: String::new(),
        },
        KeyConditionRow {
            case: "as many comparisons as the two-key schema declares",
            index: None,
            members: json!({"KeyConditionExpression": "pk = :p AND sk = :s",
                            "ExpressionAttributeValues": {":p": {"S": "p1"},
                                                          ":s": {"S": "s1"}}}),
            refusal: String::new(),
        },
        KeyConditionRow {
            case: "one comparison past the two-key schema",
            index: None,
            members: json!({"KeyConditionExpression": "pk = :p AND sk = :s AND note = :n",
                            "ExpressionAttributeValues": {":p": {"S": "p1"},
                                                          ":s": {"S": "s1"},
                                                          ":n": {"S": "n1"}}}),
            refusal: too_many_conditions(3, 2),
        },
        KeyConditionRow {
            case: "two comparisons past the two-key schema",
            index: None,
            members: json!({
                "KeyConditionExpression": "pk = :p AND sk = :s AND note = :n AND extra = :e",
                "ExpressionAttributeValues": {":p": {"S": "p1"}, ":s": {"S": "s1"},
                                              ":n": {"S": "n1"}, ":e": {"S": "e1"}},
            }),
            refusal: too_many_conditions(4, 2),
        },
        KeyConditionRow {
            case: "as many comparisons as a one-key index schema declares",
            index: Some("GsiAll"),
            members: json!({"KeyConditionExpression": "gpk = :g",
                            "ExpressionAttributeValues": {":g": {"S": "g1"}}}),
            refusal: String::new(),
        },
        KeyConditionRow {
            case: "one comparison past the one-key index schema",
            index: Some("GsiAll"),
            members: json!({"KeyConditionExpression": "gpk = :g AND sk = :s",
                            "ExpressionAttributeValues": {":g": {"S": "g1"},
                                                          ":s": {"S": "s1"}}}),
            refusal: too_many_conditions(2, 1),
        },
        KeyConditionRow {
            case: "the same count past the schema, stated in the legacy member",
            index: None,
            members: json!({"KeyConditions": {
                "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "p1"}]},
                "sk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "s1"}]},
                "note": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "n1"}]},
            }}),
            refusal: too_many_conditions(3, 2),
        },
    ]
}

#[tokio::test]
async fn a_key_condition_states_no_more_comparisons_than_the_schema_has_key_attributes() {
    let router = emulator();
    seeded(&router).await;

    for row in key_condition_rows() {
        let mut body = json!({"TableName": TABLE});
        if let Some(index) = row.index {
            body["IndexName"] = json!(index);
        }
        for (name, value) in row.members.as_object().cloned().unwrap_or_default() {
            body[name] = value;
        }
        let (status, answered) = call(&router, "Query", body).await;
        if row.refusal.is_empty() {
            assert_eq!(
                status, 200,
                "case {:?}: expected the query to be served, got {status} with {answered}",
                row.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {answered}",
            row.case
        );
        assert_eq!(
            message(&answered),
            row.refusal,
            "case {:?}: expected the message {:?}, got {answered}",
            row.case,
            row.refusal
        );
    }
}

// ===========================================================================
// 5. A projection that names list elements reports a DENSE, re-indexed list
// ===========================================================================

/// The table this section reads: its own, and its own item, so the rows below vary nothing
/// but the depth a projected list sits at and the indexes the projection names it at.
const DENSE_TABLE: &str = "DenseProjectedLists";

/// The key of the one item every row of this section projects out of.
fn dense_key() -> Value {
    json!({"pk": {"S": "dense"}})
}

/// The item the rows project.
///
/// Four list attributes, one per depth a document path reaches a list at: `grid` is a
/// top-level list of maps, `labels` a top-level list of scalars of two types, `bundle.cells`
/// a list reached THROUGH a map, and `boxes[*].slots` a list inside a map inside a list —
/// two index steps in one path.
fn dense_item() -> Value {
    json!({
        "pk": {"S": "dense"},
        "grid": {"L": [
            {"M": {"a": {"S": "a0"}, "b": {"S": "b0"}}},
            {"M": {"a": {"S": "a1"}, "b": {"S": "b1"}}},
            {"M": {"a": {"S": "a2"}, "b": {"S": "b2"}}},
            {"M": {"a": {"S": "a3"}, "b": {"S": "b3"}}},
        ]},
        "labels": {"L": [{"S": "z0"}, {"N": "1"}, {"S": "z2"}]},
        "bundle": {"M": {"cells": {"L": [
            {"M": {"x": {"S": "x0"}, "y": {"S": "y0"}}},
            {"M": {"x": {"S": "x1"}, "y": {"S": "y1"}}},
            {"M": {"x": {"S": "x2"}, "y": {"S": "y2"}}},
        ]}}},
        "boxes": {"L": [
            {"M": {"slots": {"L": [{"M": {"p": {"S": "p00"}}},
                                   {"M": {"p": {"S": "p01"}}},
                                   {"M": {"p": {"S": "p02"}}}]}}},
            {"M": {"slots": {"L": [{"M": {"p": {"S": "p10"}}},
                                   {"M": {"p": {"S": "p11"}}},
                                   {"M": {"p": {"S": "p12"}}}]}}},
        ]},
    })
}

/// An emulator holding [`DENSE_TABLE`], active, with [`dense_item`] written to it.
async fn dense_seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": DENSE_TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "seed the dense projection table", DENSE_TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": DENSE_TABLE, "Item": dense_item()}),
    )
    .await;
    router
}

/// The readers that report a `ProjectionExpression` of this service, so the rule is pinned
/// on the reporter rather than on one operation's handler.
const DENSE_READERS: [&str; 5] = [
    "GetItem",
    "Query",
    "Scan",
    "BatchGetItem",
    "TransactGetItems",
];

/// The request `operation` carries to project `expression` out of [`dense_key`].
fn dense_request(operation: &str, expression: &str) -> Value {
    match operation {
        "GetItem" => json!({"TableName": DENSE_TABLE, "Key": dense_key(),
                            "ProjectionExpression": expression}),
        "Query" => json!({"TableName": DENSE_TABLE, "ProjectionExpression": expression,
                          "KeyConditionExpression": "pk = :p",
                          "ExpressionAttributeValues": {":p": {"S": "dense"}}}),
        "Scan" => json!({"TableName": DENSE_TABLE, "ProjectionExpression": expression}),
        "BatchGetItem" => json!({"RequestItems": {DENSE_TABLE: {
            "Keys": [dense_key()], "ProjectionExpression": expression}}}),
        "TransactGetItems" => json!({"TransactItems": [{"Get": {
            "TableName": DENSE_TABLE, "Key": dense_key(),
            "ProjectionExpression": expression}}]}),
        other => panic!("case {other}: not one of the readers this section drives"),
    }
}

/// What `operation` reported of [`dense_key`] for `expression` — the empty map where the
/// projection matched no location at all, however that reader spells reporting nothing.
async fn dense_reported(router: &MultiServiceRouter, operation: &str, expression: &str) -> Value {
    let answered = ok(router, operation, dense_request(operation, expression)).await;
    let reported = match operation {
        "GetItem" => answered.get("Item").cloned(),
        "Query" | "Scan" => answered["Items"]
            .as_array()
            .and_then(|items| items.first().cloned()),
        "BatchGetItem" => answered["Responses"][DENSE_TABLE]
            .as_array()
            .and_then(|items| items.first().cloned()),
        _ => answered["Responses"][0].get("Item").cloned(),
    };
    reported.unwrap_or_else(|| json!({}))
}

/// One row: the projection and the item every reader must report for it.
struct DenseRow {
    case: &'static str,
    expression: &'static str,
    reported: Value,
}

/// The rows, spanning what the rule generalises over: WHICH index of a list a path names
/// (the first, a middle one, the last, one past the end), HOW MANY it names (one, two,
/// three), the ORDER the expression listed them in, WHAT the element contributes (one
/// member, several, a whole element, a scalar), and the DEPTH the list sits at (top level,
/// through a map, through a list into a map into a list).
fn dense_rows() -> Vec<DenseRow> {
    vec![
        DenseRow {
            case: "a member of the FIRST element of a top-level list",
            expression: "grid[0].b",
            reported: json!({"grid": {"L": [{"M": {"b": {"S": "b0"}}}]}}),
        },
        DenseRow {
            case: "a member of a MIDDLE element, which reports at index 0",
            expression: "grid[1].b",
            reported: json!({"grid": {"L": [{"M": {"b": {"S": "b1"}}}]}}),
        },
        DenseRow {
            case: "a member of the LAST element, which reports at index 0",
            expression: "grid[3].a",
            reported: json!({"grid": {"L": [{"M": {"a": {"S": "a3"}}}]}}),
        },
        DenseRow {
            case: "a WHOLE element at a non-zero index",
            expression: "grid[2]",
            reported: json!({"grid": {"L": [{"M": {"a": {"S": "a2"}, "b": {"S": "b2"}}}]}}),
        },
        DenseRow {
            case: "a scalar element of a list of scalars",
            expression: "labels[2]",
            reported: json!({"labels": {"L": [{"S": "z2"}]}}),
        },
        DenseRow {
            case: "a scalar element of another type, at another index",
            expression: "labels[1]",
            reported: json!({"labels": {"L": [{"N": "1"}]}}),
        },
        DenseRow {
            case: "two NON-ADJACENT indexes, which report as two adjacent elements",
            expression: "grid[1].b, grid[3].a",
            reported: json!({"grid": {"L": [{"M": {"b": {"S": "b1"}}},
                                            {"M": {"a": {"S": "a3"}}}]}}),
        },
        DenseRow {
            case: "the same two indexes named largest-first",
            expression: "grid[3].a, grid[1].b",
            reported: json!({"grid": {"L": [{"M": {"b": {"S": "b1"}}},
                                            {"M": {"a": {"S": "a3"}}}]}}),
        },
        DenseRow {
            case: "three paths over two indexes, the second carrying both its members",
            expression: "grid[0].a, grid[2].b, grid[2].a",
            reported: json!({"grid": {"L": [{"M": {"a": {"S": "a0"}}},
                                            {"M": {"a": {"S": "a2"}, "b": {"S": "b2"}}}]}}),
        },
        DenseRow {
            case: "an index past the end of the stored list, which reports nothing",
            expression: "grid[9].a",
            reported: json!({}),
        },
        DenseRow {
            case: "an index past the end beside one in range, which reports only the match",
            expression: "grid[9].a, grid[2].b",
            reported: json!({"grid": {"L": [{"M": {"b": {"S": "b2"}}}]}}),
        },
        DenseRow {
            case: "a member of an element of a list reached THROUGH a map",
            expression: "bundle.cells[1].y",
            reported: json!({"bundle": {"M": {"cells": {"L": [{"M": {"y": {"S": "y1"}}}]}}}}),
        },
        DenseRow {
            case: "a whole element of a list reached through a map",
            expression: "bundle.cells[2]",
            reported: json!({"bundle": {"M": {"cells": {"L": [
                {"M": {"x": {"S": "x2"}, "y": {"S": "y2"}}}]}}}}),
        },
        DenseRow {
            case: "two indexes of a list reached through a map, named largest-first",
            expression: "bundle.cells[2].x, bundle.cells[1].y",
            reported: json!({"bundle": {"M": {"cells": {"L": [{"M": {"y": {"S": "y1"}}},
                                                            {"M": {"x": {"S": "x2"}}}]}}}}),
        },
        DenseRow {
            case: "an index past the end of a list reached through a map",
            expression: "bundle.cells[7].y",
            reported: json!({}),
        },
        DenseRow {
            case: "two index steps in one path: BOTH lists report re-indexed from zero",
            expression: "boxes[1].slots[2].p",
            reported: json!({"boxes": {"L": [{"M": {"slots": {"L": [
                {"M": {"p": {"S": "p12"}}}]}}}]}}),
        },
        DenseRow {
            case: "two outer indexes, each naming one index of its own nested list",
            expression: "boxes[0].slots[1].p, boxes[1].slots[2].p",
            reported: json!({"boxes": {"L": [
                {"M": {"slots": {"L": [{"M": {"p": {"S": "p01"}}}]}}},
                {"M": {"slots": {"L": [{"M": {"p": {"S": "p12"}}}]}}}]}}),
        },
        DenseRow {
            // The rule is about paths that name an INDEX: a path that stops at the
            // attribute reports the list exactly as it is stored, every element included.
            case: "the whole list attribute, which no index step narrows",
            expression: "grid",
            reported: json!({"grid": dense_item()["grid"].clone()}),
        },
    ]
}

#[tokio::test]
async fn a_projected_list_element_reports_in_a_dense_list_re_indexed_from_zero() {
    let router = dense_seeded().await;

    for row in dense_rows() {
        for reader in DENSE_READERS {
            let got = dense_reported(&router, reader, row.expression).await;
            assert_eq!(
                got, row.reported,
                "case {:?} projecting {:?} through {reader}: expected {}, got {got}",
                row.case, row.expression, row.reported
            );
        }
    }

    // Every call above was a read: the item still carries every element it was written
    // with, so the next caller sees the whole list and not a narrowed one.
    let whole = ok(
        &router,
        "GetItem",
        json!({"TableName": DENSE_TABLE, "Key": dense_key()}),
    )
    .await;
    assert_eq!(
        whole["Item"],
        dense_item(),
        "the projected reads must leave the stored item whole: expected {}, got {whole}",
        dense_item()
    );
}

/// The indexes [`dense_item`]'s `grid` holds elements at — the generator axis of the
/// invariant below, which spans every non-empty subset of them.
const DENSE_GRID_INDEXES: usize = 4;

#[tokio::test]
async fn a_projected_list_carries_one_element_per_index_named_in_index_order() {
    let router = dense_seeded().await;

    // Every non-empty subset of the stored indexes, each named smallest-first and then
    // largest-first: neither how many indexes a projection names, which ones they are, nor
    // the order it lists them in changes that the report carries exactly one element per
    // named index, in ascending index order.
    for subset in 1u8..(1 << DENSE_GRID_INDEXES) {
        let ascending: Vec<usize> = (0..DENSE_GRID_INDEXES)
            .filter(|index| subset & (1u8 << index) != 0)
            .collect();
        let mut descending = ascending.clone();
        descending.reverse();
        let expected = json!({"grid": {"L": ascending
            .iter()
            .map(|index| json!({"M": {"a": {"S": format!("a{index}")}}}))
            .collect::<Vec<Value>>()}});

        for named in [&ascending, &descending] {
            let expression = named
                .iter()
                .map(|index| format!("grid[{index}].a"))
                .collect::<Vec<String>>()
                .join(", ");
            let got = dense_reported(&router, "GetItem", &expression).await;
            assert_eq!(
                got, expected,
                "case projecting {expression:?}: expected {} element(s) in index order, \
                 {expected}, got {got}",
                ascending.len()
            );
        }
    }
}
