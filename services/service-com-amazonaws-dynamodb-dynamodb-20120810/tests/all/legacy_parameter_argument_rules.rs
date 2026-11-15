// SPDX-License-Identifier: CC0-1.0

//! The legacy (pre-expression) parameter surface, read over the rules it is held to.
//!
//! Three rules, each asserted over a TABLE spanning what it generalises over rather than
//! over one recorded request:
//!
//! * a refusal about a legacy parameter names THAT parameter — the member the request
//!   carried — so a `Query` whose legacy filter map mentions a key attribute is told about
//!   the map it sent, and only a request that filtered through the expression member is
//!   told about that member;
//! * an illegal legacy argument is a client error (HTTP 400 `ValidationException`) with the
//!   sentence for the rule it broke, whichever legacy member carried it: never a server
//!   fault, which is what an SDK would retry;
//! * a legacy filter answers the SAME items the expression form of the same intent answers
//!   — the comparison evaluates over the attribute an item HOLDS, so a comparator matches
//!   `attribute_exists` plus the comparison and `NE` is the documented exception, and a
//!   binary value is compared as bytes, "each byte of the binary data as unsigned"
//!   (`Condition$AttributeValueList`).
//!
//! Everything is driven over the wire, so nothing is asserted that a client could not
//! observe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table the key-attribute and argument rows drive: a composite key, so a `Query` can
/// state a key condition, plus a global index with a key of its own.
const TABLE: &str = "LegacyArguments";

/// That table's global secondary index, whose own key attribute an index query may not
/// filter on either.
const INDEX: &str = "ByGsk";

/// The table the equivalence rows scan: one partition key, and items that differ in
/// whether they carry the filtered attributes at all.
const SPARSE: &str = "LegacySparseRows";

/// The envelope a WRITE's refusal carries, and a read's does not.
const ENVELOPE: &str = "1 validation error detected: ";

/// An emulator holding [`TABLE`] with its index, and one item in it.
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
                {"AttributeName": "gsk", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "gsk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the legacy-arguments table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "k1"}, "sk": {"S": "s1"}, "gsk": {"S": "g1"},
                     "a": {"S": "x"}, "b": {"S": "y"}},
        }),
    )
    .await;
    router
}

/// One legacy comparison over `attribute`, as an entry of a legacy condition map.
fn condition(attribute: &str, operator: &str, operands: Value) -> Value {
    let mut entry = json!({"ComparisonOperator": operator});
    if !operands.is_null() {
        entry["AttributeValueList"] = operands;
    }
    json!({attribute: entry})
}

/// An `EQ` comparison over `attribute` against one string.
fn equals(attribute: &str, value: &str) -> Value {
    condition(attribute, "EQ", json!([{"S": value}]))
}

#[tokio::test]
async fn a_filter_over_a_key_attribute_names_the_member_it_was_stated_through() {
    let router = seeded().await;

    // "A FilterExpression does not allow key attributes. You cannot define a filter
    // expression based on a partition key or a sort key" (`Query$FilterExpression`), and
    // the legacy map states the same filter one generation back — so the rule holds for
    // both, and what differs is the member the refusal NAMES. The rows vary the generation
    // the filter was stated in, which key attribute it mentioned (the table's partition
    // key, its sort key, the index's own key) and whether the read was addressed at the
    // table or at the index.
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "a legacy filter on the table's partition key",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": equals("pk", "k1"),
            }),
            "QueryFilter",
            "pk",
        ),
        (
            "a legacy filter on the table's sort key",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": equals("sk", "s1"),
            }),
            "QueryFilter",
            "sk",
        ),
        (
            "a legacy filter on the index's own key, on an index query",
            json!({
                "TableName": TABLE,
                "IndexName": INDEX,
                "KeyConditions": equals("gsk", "g1"),
                "QueryFilter": equals("gsk", "g1"),
            }),
            "QueryFilter",
            "gsk",
        ),
        (
            "a legacy filter on the table's key, on an index query",
            json!({
                "TableName": TABLE,
                "IndexName": INDEX,
                "KeyConditions": equals("gsk", "g1"),
                "QueryFilter": equals("pk", "k1"),
            }),
            "QueryFilter",
            "pk",
        ),
        (
            "a legacy filter mentioning a key beside a non-key entry",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": {
                    "a": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "x"}]},
                    "sk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "s1"}]},
                },
                "ConditionalOperator": "AND",
            }),
            "QueryFilter",
            "sk",
        ),
        // The other generation keeps its own name: a request that filtered through the
        // expression member is told about that member, spelled as the sentence about it
        // spells it.
        (
            "an expression filter on the table's partition key",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": "pk = :k",
                "FilterExpression": "pk = :k",
                "ExpressionAttributeValues": {":k": {"S": "k1"}},
            }),
            "Filter Expression",
            "pk",
        ),
        (
            "an expression filter on the index's own key, on an index query",
            json!({
                "TableName": TABLE,
                "IndexName": INDEX,
                "KeyConditionExpression": "gsk = :g",
                "FilterExpression": "gsk = :g",
                "ExpressionAttributeValues": {":g": {"S": "g1"}},
            }),
            "Filter Expression",
            "gsk",
        ),
    ];

    for (case, request, member, key) in rows {
        let expected = format!(
            "{member} can only contain non-primary key attributes: Primary key attribute: {key}"
        );
        let (status, answered) = call(&router, "Query", request).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {answered}"
        );
    }

    // The CONTROLS: the rule is about a filter naming a key of the read it narrows, so a
    // legacy filter over a non-key attribute is served, and a scan — which states no key
    // condition at all — may filter on a key attribute.
    for (case, operation, request) in [
        (
            "a legacy filter over a non-key attribute",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": equals("a", "x"),
            }),
        ),
        (
            "a scan's legacy filter over a key attribute",
            "Scan",
            json!({"TableName": TABLE, "ScanFilter": equals("pk", "k1")}),
        ),
    ] {
        let served = ok(&router, operation, request).await;
        assert_eq!(
            served["Count"].clone(),
            json!(1),
            "case {case:?}: expected the one matching item, got {served}"
        );
    }
}

#[tokio::test]
async fn an_illegal_legacy_argument_is_a_client_error_with_the_sentence_for_its_rule() {
    let router = seeded().await;

    // The rows span the legacy argument rules: the operand count each comparison takes,
    // the shorter comparison vocabulary a KEY may be stated with, how many conditions a
    // key schema admits and which of its elements they must cover, the map
    // `ConditionalOperator` needs beside it, the member a reader cannot run without, the
    // two generations not mixing, and what an `Expected` entry must state. They span the
    // surfaces those members arrive on too — both readers and two writes — and EVERY row
    // is a 400 `ValidationException`: an illegal argument is the client's defect, never a
    // server fault an SDK would retry.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "an existence comparison given a value to compare",
            "Scan",
            json!({
                "TableName": TABLE,
                "ScanFilter": condition("a", "NOT_NULL", json!([{"S": "v"}])),
            }),
            "One or more parameter values were invalid: Invalid number of argument(s) for the \
             NOT_NULL ComparisonOperator"
                .to_string(),
        ),
        (
            "the other existence comparison given one",
            "Scan",
            json!({
                "TableName": TABLE,
                "ScanFilter": condition("a", "NULL", json!([{"S": "v"}])),
            }),
            "One or more parameter values were invalid: Invalid number of argument(s) for the \
             NULL ComparisonOperator"
                .to_string(),
        ),
        (
            "a range comparison given one bound",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": condition("a", "BETWEEN", json!([{"S": "v"}])),
            }),
            "One or more parameter values were invalid: Invalid number of argument(s) for the \
             BETWEEN ComparisonOperator"
                .to_string(),
        ),
        (
            "a key condition stated with a comparison no index can be read through",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": condition("pk", "NE", json!([{"S": "k1"}])),
            }),
            "One or more parameter values were invalid: Attempted conditional constraint is not \
             an indexable operation"
                .to_string(),
        ),
        (
            "more key conditions than the schema has key attributes",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": {
                    "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]},
                    "sk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "s1"}]},
                    "a": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "x"}]},
                },
            }),
            "The number of query conditions (3) exceeds the number of key attributes defined in \
             the schema (2)"
                .to_string(),
        ),
        (
            "a key condition that leaves the partition key unconstrained",
            "Query",
            json!({"TableName": TABLE, "KeyConditions": equals("sk", "s1")}),
            "Query condition missed key schema element: pk".to_string(),
        ),
        (
            "a legacy filter map with one entry to combine",
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditions": equals("pk", "k1"),
                "QueryFilter": equals("a", "x"),
                "ConditionalOperator": "OR",
            }),
            "ConditionalOperator can only be used when Filter or Expected has two or more \
             elements"
                .to_string(),
        ),
        (
            "the combining operator with no map at all",
            "Scan",
            json!({"TableName": TABLE, "ConditionalOperator": "AND"}),
            "ConditionalOperator cannot be used without Filter or Expected".to_string(),
        ),
        (
            "a reader that states no key condition in either generation",
            "Query",
            json!({"TableName": TABLE, "QueryFilter": equals("a", "x")}),
            "Either the KeyConditions or KeyConditionExpression parameter must be specified in \
             the request."
                .to_string(),
        ),
        (
            "a legacy filter beside the expression member that replaced it",
            "Scan",
            json!({
                "TableName": TABLE,
                "ScanFilter": {
                    "a": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "x"}]},
                    "b": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "y"}]},
                },
                "ConditionalOperator": "AND",
                "FilterExpression": "a = :v",
                "ExpressionAttributeValues": {":v": {"S": "x"}},
            }),
            "Can not use both expression and non-expression parameters in the same request: \
             Non-expression parameters: {ScanFilter, ConditionalOperator} Expression parameters: \
             {FilterExpression}"
                .to_string(),
        ),
        (
            "a projection stated in both generations, one table entry of a batch read",
            "BatchGetItem",
            json!({
                "RequestItems": {
                    TABLE: {
                        "Keys": [{"pk": {"S": "k1"}, "sk": {"S": "s1"}}],
                        "AttributesToGet": ["a"],
                        "ProjectionExpression": "a",
                    },
                },
            }),
            "Can not use both expression and non-expression parameters in the same request: \
             Non-expression parameters: {AttributesToGet} Expression parameters: \
             {ProjectionExpression}"
                .to_string(),
        ),
        (
            "a write expecting an attribute to exist without saying what it holds",
            "PutItem",
            json!({
                "TableName": TABLE,
                "Item": {"pk": {"S": "k9"}, "sk": {"S": "s9"}},
                "Expected": {"a": {"Exists": true}},
            }),
            format!(
                "{ENVELOPE}One or more parameter values were invalid: Value must be provided \
                 when Exists is true for Attribute: a"
            ),
        ),
        (
            "a write stating one half of an Expected entry twice",
            "DeleteItem",
            json!({
                "TableName": TABLE,
                "Key": {"pk": {"S": "k1"}, "sk": {"S": "s1"}},
                "Expected": {"a": {"Exists": true, "ComparisonOperator": "NOT_NULL"}},
            }),
            format!(
                "{ENVELOPE}One or more parameter values were invalid: Exists and \
                 ComparisonOperator cannot be used together for Attribute: a"
            ),
        ),
    ];

    for (case, operation, request, expected) in rows {
        let (status, answered) = call(&router, operation, request).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (400, "ValidationException", expected.clone()),
            "case {case:?}: expected 400 ValidationException {expected:?}, got {status} with \
             {answered}"
        );
    }
}

/// An emulator holding [`SPARSE`]: four items that differ in whether they carry the
/// filtered attributes at all, one of them carrying a binary value with a zero byte
/// inside it.
async fn sparse() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": SPARSE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the sparse legacy-filter table", SPARSE).await;
    for (pk, status, blob) in [
        ("holds-active", Some("active"), Some(vec![1u8, 0, 2])),
        ("holds-idle", Some("idle"), Some(vec![0x80u8, 0xFF])),
        ("holds-nothing", None, None),
        ("holds-nothing-either", None, None),
    ] {
        let mut item = json!({"pk": {"S": pk}});
        if let Some(status) = status {
            item["status"] = json!({"S": status});
        }
        if let Some(blob) = blob {
            item["blob"] = json!({"B": ::aws_smithy_types::base64::encode(&blob)});
        }
        ok(&router, "PutItem", json!({"TableName": SPARSE, "Item": item})).await;
    }
    router
}

/// The partition keys a scan reported, sorted, so two scans of the same data compare as
/// the item SETS they answered.
fn reported(answered: &Value) -> Vec<String> {
    let mut keys: Vec<String> = answered["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["pk"]["S"].as_str().map(ToString::to_string))
        .collect();
    keys.sort();
    keys
}

/// One row of the equivalence table below: the case it names, the attribute the filter is
/// over, the legacy comparison and the operands it compares against, the expression form
/// of the same intent and the values that form binds, and the items the filter selects.
type EquivalenceRow = (
    &'static str,
    &'static str,
    &'static str,
    Value,
    &'static str,
    Value,
    Vec<&'static str>,
);

#[tokio::test]
async fn a_legacy_filter_answers_the_items_the_expression_form_answers() {
    let router = sparse().await;

    // Each row states one intent BOTH ways — through a legacy `ScanFilter` entry and
    // through the `FilterExpression` form of the same intent — and the items it selects.
    // The rows span the comparison vocabulary `Condition$ComparisonOperator` declares
    // (equality, its negation, the two substring forms, a prefix, the two existence
    // tests), the two attribute types a substring search is defined over (string and
    // binary), and whether the comparison is one an item carrying NO value for the name
    // satisfies: only `NE` and `NULL` are.
    let rows: Vec<EquivalenceRow> = vec![
        (
            "equality",
            "status",
            "EQ",
            json!([{"S": "active"}]),
            "#n = :v",
            json!({":v": {"S": "active"}}),
            vec!["holds-active"],
        ),
        (
            "its negation, which the items holding no value satisfy",
            "status",
            "NE",
            json!([{"S": "active"}]),
            "NOT #n = :v",
            json!({":v": {"S": "active"}}),
            vec!["holds-idle", "holds-nothing", "holds-nothing-either"],
        ),
        (
            "a substring search",
            "status",
            "CONTAINS",
            json!([{"S": "act"}]),
            "contains(#n, :v)",
            json!({":v": {"S": "act"}}),
            vec!["holds-active"],
        ),
        (
            "its negation, which excludes the items holding no value",
            "status",
            "NOT_CONTAINS",
            json!([{"S": "act"}]),
            "attribute_exists(#n) AND NOT contains(#n, :v)",
            json!({":v": {"S": "act"}}),
            vec!["holds-idle"],
        ),
        (
            "a prefix",
            "status",
            "BEGINS_WITH",
            json!([{"S": "id"}]),
            "begins_with(#n, :v)",
            json!({":v": {"S": "id"}}),
            vec!["holds-idle"],
        ),
        (
            "the attribute being there",
            "status",
            "NOT_NULL",
            Value::Null,
            "attribute_exists(#n)",
            Value::Null,
            vec!["holds-active", "holds-idle"],
        ),
        (
            "the attribute being absent",
            "status",
            "NULL",
            Value::Null,
            "attribute_not_exists(#n)",
            Value::Null,
            vec!["holds-nothing", "holds-nothing-either"],
        ),
        // The same two substring forms over a BINARY value, whose bytes are compared as
        // unsigned: the needle is the zero byte one stored value holds inside it, which no
        // text encoding could carry.
        (
            "a binary subsequence holding the zero byte",
            "blob",
            "CONTAINS",
            json!([{"B": ::aws_smithy_types::base64::encode([0u8])}]),
            "contains(#n, :v)",
            json!({":v": {"B": ::aws_smithy_types::base64::encode([0u8])}}),
            vec!["holds-active"],
        ),
        (
            "its negation over the same bytes",
            "blob",
            "NOT_CONTAINS",
            json!([{"B": ::aws_smithy_types::base64::encode([0u8])}]),
            "attribute_exists(#n) AND NOT contains(#n, :v)",
            json!({":v": {"B": ::aws_smithy_types::base64::encode([0u8])}}),
            vec!["holds-idle"],
        ),
        (
            "a binary byte past the signed range",
            "blob",
            "CONTAINS",
            json!([{"B": ::aws_smithy_types::base64::encode([0x80u8])}]),
            "contains(#n, :v)",
            json!({":v": {"B": ::aws_smithy_types::base64::encode([0x80u8])}}),
            vec!["holds-idle"],
        ),
    ];

    for (case, attribute, operator, operands, expression, values, expected) in rows {
        let legacy = ok(
            &router,
            "Scan",
            json!({
                "TableName": SPARSE,
                "ScanFilter": condition(attribute, operator, operands),
            }),
        )
        .await;
        let mut request = json!({
            "TableName": SPARSE,
            "FilterExpression": expression,
            "ExpressionAttributeNames": {"#n": attribute},
        });
        if !values.is_null() {
            request["ExpressionAttributeValues"] = values;
        }
        let expressed = ok(&router, "Scan", request).await;

        let mut want: Vec<String> = expected.iter().map(ToString::to_string).collect();
        want.sort();
        let (got_legacy, got_expressed) = (reported(&legacy), reported(&expressed));
        assert_eq!(
            got_legacy, want,
            "case {case:?} stated the legacy way: expected the items {want:?}, got \
             {got_legacy:?} from {legacy}"
        );
        assert_eq!(
            got_expressed, want,
            "case {case:?} stated the expression way: expected the items {want:?}, got \
             {got_expressed:?} from {expressed}"
        );
    }
}
