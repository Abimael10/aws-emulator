// SPDX-License-Identifier: CC0-1.0

//! What a LEGACY condition's `AttributeValueList` and an expression FUNCTION's operands may
//! be, driven end to end.
//!
//! Four rules, each asserted over a table of inputs rather than over one request:
//!
//! 1. the members of one legacy `AttributeValueList` share one `AttributeValue` type:
//!    "AttributeValueList must contain two AttributeValue elements of the same type, either
//!    String, Number, or Binary" (`BETWEEN` in the `ComparisonOperator` doc) and
//!    "AttributeValueList can contain one or more AttributeValue elements of type String,
//!    Number, or Binary" (`IN`) — a list mixing two is refused 400 `ValidationException`
//!    before the condition is evaluated, whichever member carried it, and each member is of
//!    a type that comparison compares at all, which the same doc states per operator (only
//!    `EQ` and `NE` are "supported for all data types");
//! 2. a legacy `BETWEEN` states its bounds low first: "BETWEEN : Greater than or equal to
//!    the first value, and less than or equal to the second value" (`ComparisonOperator`),
//!    so a reversed range is the request's own defect — a `ValidationException`, never the
//!    `ConditionalCheckFailedException` an evaluated condition would earn;
//! 3. an `Expected` entry carrying an `AttributeValueList` names the comparison those
//!    values are the operands of: "Use AttributeValueList to specify one or more values to
//!    compare against an attribute. Use ComparisonOperator to specify how you want to
//!    perform the comparison" (`ExpectedAttributeValue`);
//! 4. an operand position the function reference calls an OPERAND takes a document path as
//!    readily as a literal — "contains (path, operand)", "begins_with (path, substr)" — so
//!    a filter or condition whose comparison operand READS the item is evaluated, with the
//!    one documented exception that `contains`'s two operands must be distinct.
//!
//! Rule 1 and rule 2 are refusals of the REQUEST, so each row also reads the addressed item
//! back: a refused write stores nothing. The rows differ along what each rule generalises
//! over — the member the condition arrived in (`Expected`, `ScanFilter`, `QueryFilter`,
//! `KeyConditions`), the operation carrying it (all three single-item writes and both table
//! walks), the comparison (`IN`, `BETWEEN`, `EQ`), the operand types (`S`, `N`, `B`, `SS`)
//! and the list's length — and every row states the whole message it must earn, since
//! whether a refusal carries the `<N> validation error(s) detected: ` envelope follows the
//! member it is about.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, ok};
use crate::table_items::create_active_table;

/// The table the legacy rows address: a composite key, so a `Query` has a sort key to state
/// a key condition on.
const TABLE: &str = "LegacyOperandLists";

/// The envelope a refusal about the member a WRITE states its condition through carries, and
/// a refusal about a member a READ narrows its result with does not.
const ENVELOPE: &str = "1 validation error detected: ";

/// The message a conditional write's refusal carries, which is what a control row asserts
/// to show the condition was EVALUATED rather than refused as malformed.
const CONDITION_FAILED: &str = "The conditional request failed";

/// The clause every refusal of a parameter value opens with.
const INVALID: &str = "One or more parameter values were invalid";

/// The one seeded item's key, which every write row addresses.
fn key() -> Value {
    json!({"pk": {"S": "k1"}, "sk": {"S": "s1"}})
}

/// Base64 of the bytes given, as a binary attribute value is written on the wire.
fn binary(bytes: &[u8]) -> Value {
    json!({"B": ::aws_smithy_types::base64::encode(bytes)})
}

/// The emulator with [`TABLE`] holding one item: `s` and `peer` carry the same text, `n` a
/// number, `tags` a string set and `blob` two bytes.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "k1"}, "sk": {"S": "s1"}, "n": {"N": "3"},
                     "s": {"S": "beta"}, "peer": {"S": "beta"}, "tags": {"SS": ["red", "beta"]},
                     "bstore": binary(&[0x01, 0x80])},
        }),
    )
    .await;
    router
}

/// The seeded item as it stands right now, for the assertion that a refused write stored
/// nothing.
async fn stored(router: &MultiServiceRouter) -> Value {
    ok(router, "GetItem", json!({"TableName": TABLE, "Key": key()}))
        .await
        .get("Item")
        .cloned()
        .unwrap_or(Value::Null)
}

/// One legacy condition entry, as `Expected`, `ScanFilter`, `QueryFilter` and
/// `KeyConditions` all spell one.
fn entry(operator: &str, operands: Value) -> Value {
    json!({"ComparisonOperator": operator, "AttributeValueList": operands})
}

/// A `Query` that pins the partition key, so a row about its filter or its sort-key
/// condition is not refused for a missing key condition instead.
fn pinned_key() -> Value {
    json!({"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]}})
}

/// The legacy update one row applies beside its `Expected` entry: `Expected` and the
/// expression members cannot be used in one request ("Can not use both expression and
/// non-expression parameters in the same request"), so an update row states its write the
/// legacy way too.
fn legacy_update() -> Value {
    json!({"s": {"Action": "PUT", "Value": {"S": "gamma"}}})
}

/// One row of the refusal tables: the case, the operation, the request beside `TableName`,
/// and the whole message it must earn.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected: String,
}

/// Drive every row through the suite's shared refusal driver — which owes the 400 and the
/// wire error class every refusal of a request's own members carries
/// ([`crate::probe::refusal`]) — asserting the whole message each row must earn, and that
/// the seeded item is untouched afterwards.
async fn refused(router: &MultiServiceRouter, rows: Vec<Row>) {
    let before = stored(router).await;
    for row in rows {
        let mut request = row.request.clone();
        request["TableName"] = json!(TABLE);
        let stated = crate::probe::refusal(router, row.case, row.operation, request).await;
        assert_eq!(
            stated, row.expected,
            "case {:?} ({}): expected {:?}, got {stated:?}",
            row.case, row.operation, row.expected
        );
    }
    let after = stored(router).await;
    assert_eq!(
        after, before,
        "a refused request must store nothing: expected {before:?}, got {after:?}"
    );
}

#[tokio::test]
async fn a_legacy_attribute_value_list_holds_one_attribute_value_type() {
    let router = seeded().await;
    // The sentence is fixed: it names neither the attribute, the operator nor the types.
    let clause = format!("{INVALID}: AttributeValues inside AttributeValueList must be of same type");
    let enveloped = format!("{ENVELOPE}{clause}");

    refused(
        &router,
        vec![
            Row {
                case: "an update's Expected IN mixing a string and a number",
                operation: "UpdateItem",
                request: json!({"Key": key(), "AttributeUpdates": legacy_update(),
                                "Expected": {"s": entry("IN", json!([{"S": "beta"}, {"N": "3"}]))}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "a put's Expected IN with the two types the other way round",
                operation: "PutItem",
                request: json!({"Item": key(),
                                "Expected": {"s": entry("IN", json!([{"N": "3"}, {"S": "beta"}]))}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "a delete's Expected BETWEEN whose bounds are of two types",
                operation: "DeleteItem",
                request: json!({"Key": key(),
                                "Expected": {"n": entry("BETWEEN", json!([{"N": "1"}, {"S": "9"}]))}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "an update's Expected IN whose THIRD member differs",
                operation: "UpdateItem",
                request: json!({"Key": key(), "AttributeUpdates": legacy_update(),
                                "Expected": {"s": entry(
                                    "IN",
                                    json!([{"S": "beta"}, {"S": "gamma"}, binary(&[0x01])]),
                                )}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "a put's Expected IN mixing a string and a string SET",
                operation: "PutItem",
                request: json!({"Item": key(),
                                "Expected": {"tags": entry(
                                    "IN",
                                    json!([{"S": "red"}, {"SS": ["red"]}]),
                                )}}),
                expected: enveloped.clone(),
            },
            // The members a READ narrows its result with state the same sentence on their
            // own, which is this service's existing envelope split.
            Row {
                case: "a scan's ScanFilter IN mixing a number and a string",
                operation: "Scan",
                request: json!({"ScanFilter": {"s": entry(
                    "IN",
                    json!([{"N": "3"}, {"S": "beta"}]),
                )}}),
                expected: clause.clone(),
            },
            Row {
                case: "a query's QueryFilter BETWEEN mixing a string and binary",
                operation: "Query",
                request: json!({"KeyConditions": pinned_key(),
                                "QueryFilter": {"s": entry(
                                    "BETWEEN",
                                    json!([{"S": "a"}, binary(&[0x7F])]),
                                )}}),
                expected: clause.clone(),
            },
            Row {
                case: "a query's legacy KeyConditions BETWEEN over two types on the sort key",
                operation: "Query",
                request: json!({"KeyConditions": {
                    "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]},
                    "sk": entry("BETWEEN", json!([{"S": "s0"}, {"N": "9"}])),
                }}),
                expected: clause.clone(),
            },
        ],
    )
    .await;

    // The CONTROL: a list whose members ARE of one type states a comparison this service
    // evaluates — on a read and on a write, which is what makes the rows above a type rule
    // and not a blanket refusal of a multi-value list.
    let answered = ok(
        &router,
        "Scan",
        json!({"TableName": TABLE,
               "ScanFilter": {"s": entry("IN", json!([{"S": "beta"}, {"S": "gamma"}]))}}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case a legacy IN over one type: expected the seeded item, got {answered}"
    );
    let stated = crate::probe::refusal_coded(
        &router,
        "an Expected IN over one type",
        "DeleteItem",
        json!({"TableName": TABLE, "Key": key(),
               "Expected": {"s": entry("IN", json!([{"S": "nothing"}, {"S": "either"}]))}}),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, CONDITION_FAILED,
        "case an Expected IN over one type: expected the condition to be EVALUATED and to fail, \
         got {stated:?}"
    );
}

#[tokio::test]
async fn a_legacy_between_range_runs_from_its_lower_bound_up() {
    let router = seeded().await;
    let clause = format!(
        "{INVALID}: The BETWEEN condition was provided a range where the lower bound is \
         greater than the upper bound"
    );
    let enveloped = format!("{ENVELOPE}{clause}");

    refused(
        &router,
        vec![
            Row {
                case: "a delete's Expected ranging from a higher number to a lower one",
                operation: "DeleteItem",
                request: json!({"Key": key(),
                                "Expected": {"n": entry(
                                    "BETWEEN",
                                    json!([{"N": "5"}, {"N": "1"}]),
                                )}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "an update's Expected ranging from a later string to an earlier one",
                operation: "UpdateItem",
                request: json!({"Key": key(), "AttributeUpdates": legacy_update(),
                                "Expected": {"s": entry(
                                    "BETWEEN",
                                    json!([{"S": "c"}, {"S": "b"}]),
                                )}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "a put's Expected ranging over binary bytes the wrong way round",
                operation: "PutItem",
                request: json!({"Item": key(),
                                "Expected": {"bstore": entry(
                                    "BETWEEN",
                                    json!([binary(&[0x80]), binary(&[0x01])]),
                                )}}),
                expected: enveloped.clone(),
            },
            Row {
                case: "a scan's ScanFilter ranging the wrong way round",
                operation: "Scan",
                request: json!({"ScanFilter": {"n": entry(
                    "BETWEEN",
                    json!([{"N": "10"}, {"N": "2"}]),
                )}}),
                expected: clause.clone(),
            },
            Row {
                case: "a query's QueryFilter ranging the wrong way round",
                operation: "Query",
                request: json!({"KeyConditions": pinned_key(),
                                "QueryFilter": {"s": entry(
                                    "BETWEEN",
                                    json!([{"S": "z"}, {"S": "a"}]),
                                )}}),
                expected: clause.clone(),
            },
            Row {
                case: "a query's legacy KeyConditions ranging the wrong way round",
                operation: "Query",
                request: json!({"KeyConditions": {
                    "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]},
                    "sk": entry("BETWEEN", json!([{"S": "s9"}, {"S": "s0"}])),
                }}),
                expected: clause.clone(),
            },
        ],
    )
    .await;

    // The CONTROLS: the rule is "the lower bound is greater than the upper bound", so a
    // range whose bounds are EQUAL is legal, and a range the right way round is evaluated —
    // on a read, on a key condition and on a write.
    for (case, request) in [
        (
            "a scan's range the right way round",
            json!({"TableName": TABLE,
                   "ScanFilter": {"n": entry("BETWEEN", json!([{"N": "1"}, {"N": "5"}]))}}),
        ),
        (
            "a scan's range whose two bounds are equal",
            json!({"TableName": TABLE,
                   "ScanFilter": {"n": entry("BETWEEN", json!([{"N": "3"}, {"N": "3"}]))}}),
        ),
    ] {
        let answered = ok(&router, "Scan", request).await;
        assert_eq!(
            answered["Count"],
            json!(1),
            "case {case:?}: expected the seeded item, got {answered}"
        );
    }
    let answered = ok(
        &router,
        "Query",
        json!({"TableName": TABLE, "KeyConditions": {
            "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]},
            "sk": entry("BETWEEN", json!([{"S": "s0"}, {"S": "s9"}])),
        }}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case a key condition's range the right way round: expected the seeded item, got \
         {answered}"
    );
    let stated = crate::probe::refusal_coded(
        &router,
        "an Expected range the right way round",
        "PutItem",
        json!({"TableName": TABLE, "Item": key(),
               "Expected": {"n": entry("BETWEEN", json!([{"N": "10"}, {"N": "20"}]))}}),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, CONDITION_FAILED,
        "case an Expected range the right way round: expected the condition to be EVALUATED and \
         to fail, got {stated:?}"
    );
}

#[tokio::test]
async fn a_legacy_comparison_takes_only_the_operand_types_it_compares() {
    let router = seeded().await;
    // AWS names the comparison and the `AttributeValue` type it does not take, both read
    // off the request; the rest of the sentence is fixed.
    let sentence = |operator: &str, datatype: &str| {
        format!("{INVALID}: ComparisonOperator {operator} is not valid for {datatype} \
                 AttributeValue type")
    };

    refused(
        &router,
        vec![
            Row {
                // The ordering comparisons take "only one AttributeValue element of type
                // String, Number, or Binary (not a set type)" (`ComparisonOperator`).
                case: "a put's Expected ordering comparison against a string set",
                operation: "PutItem",
                request: json!({"Item": key(),
                                "Expected": {"tags": entry("GT", json!([{"SS": ["red"]}]))}}),
                expected: format!("{ENVELOPE}{}", sentence("GT", "SS")),
            },
            Row {
                case: "a delete's Expected ordering comparison against a boolean",
                operation: "DeleteItem",
                request: json!({"Key": key(),
                                "Expected": {"n": entry("LT", json!([{"BOOL": true}]))}}),
                expected: format!("{ENVELOPE}{}", sentence("LT", "BOOL")),
            },
            Row {
                case: "an update's Expected subsequence test against a number set",
                operation: "UpdateItem",
                request: json!({"Key": key(), "AttributeUpdates": legacy_update(),
                                "Expected": {"tags": entry("CONTAINS", json!([{"NS": ["1"]}]))}}),
                expected: format!("{ENVELOPE}{}", sentence("CONTAINS", "NS")),
            },
            Row {
                // "BEGINS_WITH ... can contain only one AttributeValue of type String or
                // Binary (not a Number or a set type)".
                case: "a scan's prefix comparison against a number",
                operation: "Scan",
                request: json!({"ScanFilter": {"s": entry("BEGINS_WITH", json!([{"N": "1"}]))}}),
                expected: sentence("BEGINS_WITH", "N"),
            },
            Row {
                case: "a scan's ordering comparison against a list",
                operation: "Scan",
                request: json!({"ScanFilter": {"n": entry("GE", json!([{"L": [{"N": "1"}]}]))}}),
                expected: sentence("GE", "L"),
            },
            Row {
                // "IN ... can contain one or more AttributeValue elements of type String,
                // Number, or Binary", so a list of two maps breaks the TYPE rule rather
                // than the one-type rule.
                case: "a query's filter IN over two maps",
                operation: "Query",
                request: json!({"KeyConditions": pinned_key(),
                                "QueryFilter": {"s": entry(
                                    "IN",
                                    json!([{"M": {"a": {"N": "1"}}}, {"M": {"b": {"N": "2"}}}]),
                                )}}),
                expected: sentence("IN", "M"),
            },
            Row {
                case: "a query's legacy KeyConditions prefix comparison against a number",
                operation: "Query",
                request: json!({"KeyConditions": {
                    "pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "k1"}]},
                    "sk": entry("BEGINS_WITH", json!([{"N": "1"}])),
                }}),
                expected: sentence("BEGINS_WITH", "N"),
            },
        ],
    )
    .await;

    // The CONTROLS: "EQ is supported for all data types, including lists and maps" and so
    // is `NE`, so those two compare whatever the request carries — and a comparison given
    // a type it DOES take is evaluated.
    for (case, filter, selected) in [
        (
            "an equality against a boolean",
            json!({"s": entry("EQ", json!([{"BOOL": true}]))}),
            0,
        ),
        (
            "an equality against a list",
            json!({"s": entry("EQ", json!([{"L": [{"N": "1"}]}]))}),
            0,
        ),
        (
            "its negation against a map",
            json!({"s": entry("NE", json!([{"M": {"a": {"N": "1"}}}]))}),
            1,
        ),
        (
            "an ordering comparison against a number",
            json!({"n": entry("GT", json!([{"N": "1"}]))}),
            1,
        ),
        (
            "a prefix comparison against a string",
            json!({"s": entry("BEGINS_WITH", json!([{"S": "bet"}]))}),
            1,
        ),
    ] {
        let answered = ok(
            &router,
            "Scan",
            json!({"TableName": TABLE, "ScanFilter": filter}),
        )
        .await;
        assert_eq!(
            answered["Count"],
            json!(selected),
            "case {case:?}: expected {selected} item(s) selected, got {answered}"
        );
    }
}

#[tokio::test]
async fn an_expected_entry_s_value_list_names_the_comparison_it_is_the_operands_of() {
    let router = seeded().await;
    // AWS names the attribute whose entry carried the list; the rest of the sentence is
    // fixed. The rows vary the operation, the attribute and what else the entry states.
    // Whether the clause is stated inside the `<N> validation error(s) detected: ` envelope
    // follows the OPERATION: the writes that carry the item or the update the condition
    // guards state these clauses inside it, and the delete, whose request carries only the
    // key, states the clause on its own.
    let sentence = |attribute: &str| {
        format!(
            "{ENVELOPE}{INVALID}: AttributeValueList can only be used with a ComparisonOperator \
             for Attribute: {attribute}"
        )
    };
    let bare_sentence = |attribute: &str| {
        format!(
            "{INVALID}: AttributeValueList can only be used with a ComparisonOperator \
             for Attribute: {attribute}"
        )
    };

    refused(
        &router,
        vec![
            Row {
                case: "a put stating one value and no comparison",
                operation: "PutItem",
                request: json!({"Item": key(),
                                "Expected": {"s": {"AttributeValueList": [{"S": "beta"}]}}}),
                expected: sentence("s"),
            },
            Row {
                case: "a delete stating two values and no comparison",
                operation: "DeleteItem",
                request: json!({"Key": key(),
                                "Expected": {"n": {"AttributeValueList": [{"N": "1"},
                                                                          {"N": "2"}]}}}),
                expected: bare_sentence("n"),
            },
            Row {
                case: "an update stating values beside an existence expectation",
                operation: "UpdateItem",
                request: json!({"Key": key(), "AttributeUpdates": legacy_update(),
                                "Expected": {"peer": {"Exists": false,
                                                      "AttributeValueList": [{"S": "beta"}]}}}),
                expected: sentence("peer"),
            },
        ],
    )
    .await;

    // The CONTROL: the same entry WITH a comparison states a condition that is evaluated.
    let stated = crate::probe::refusal_coded(
        &router,
        "an Expected list with its comparison",
        "DeleteItem",
        json!({"TableName": TABLE, "Key": key(),
               "Expected": {"s": entry("EQ", json!([{"S": "nothing"}]))}}),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, CONDITION_FAILED,
        "case an Expected list with its comparison: expected the condition to be EVALUATED and \
         to fail, got {stated:?}"
    );
}

#[tokio::test]
async fn a_function_operand_may_read_the_item_it_is_compared_against() {
    let router = seeded().await;

    // Each row is a filter whose second operand is a DOCUMENT PATH rather than a literal:
    // the seeded item carries `s` and `peer` both as "beta", `tags` as a set holding
    // "beta", and `blob` as two bytes. The rows span the two functions whose second
    // position the reference calls an operand, the attribute TYPES the comparison is made
    // over (string, string set, binary), and an operand that is a nested read (`size`).
    for (case, filter, selected) in [
        (
            "a substring search for what another attribute holds",
            "contains(s, peer)",
            1,
        ),
        (
            "a prefix read from another attribute",
            "begins_with(s, peer)",
            1,
        ),
        (
            "a set membership test whose member is read from the item",
            "contains(tags, peer)",
            1,
        ),
        (
            "a substring search the item does not satisfy",
            "contains(s, pk)",
            0,
        ),
        (
            "a binary subsequence search over a path that holds no bytes",
            "contains(bstore, peer)",
            0,
        ),
        (
            "an operand that is the SIZE of another attribute",
            "n = size(s)",
            0,
        ),
    ] {
        let answered = ok(
            &router,
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": filter}),
        )
        .await;
        assert_eq!(
            answered["Count"],
            json!(selected),
            "case {case:?}: expected {selected} item(s) selected, got {answered}"
        );
    }

    // The same operand form on the members a WRITE and a QUERY state their condition
    // through: each is evaluated against the item rather than refused.
    let stated = crate::probe::refusal_coded(
        &router,
        "a write's condition searching for what a path holds",
        "PutItem",
        json!({"TableName": TABLE, "Item": key(),
               "ConditionExpression": "contains(s, pk)"}),
        "ConditionalCheckFailedException",
    )
    .await;
    assert_eq!(
        stated, CONDITION_FAILED,
        "case a write's condition searching for what a path holds: expected it EVALUATED and \
         unmet, got {stated:?}"
    );
    let answered = ok(
        &router,
        "Query",
        json!({"TableName": TABLE, "KeyConditions": pinned_key(),
               "FilterExpression": "contains(s, peer)"}),
    )
    .await;
    assert_eq!(
        answered["Count"],
        json!(1),
        "case a query's filter searching for what a path holds: expected the seeded item, got \
         {answered}"
    );

    // The one documented exception: "The path and the operand must be distinct; for
    // example, contains (a, a) returns an error" (the `ConditionExpression` function
    // reference) — a call comparing ONE location with itself is the request's defect, and
    // AWS states it over the operator and that operand, the path rendered as the token list
    // its refusals print one as. The rows span the member the call arrived in, which is
    // what decides the envelope, and a path that reaches inside an attribute.
    for (case, operation, request, expected) in [
        (
            "a filter searching one location for itself",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "contains(s, s)"}),
            "Invalid FilterExpression: The first operand must be distinct from the remaining \
             operands for this operator or function; operator: contains, first operand: [s]"
                .to_string(),
        ),
        (
            "a write's condition searching one location for itself",
            "PutItem",
            json!({"TableName": TABLE, "Item": key(),
                   "ConditionExpression": "contains(tags, tags)"}),
            format!(
                "{ENVELOPE}Invalid ConditionExpression: The first operand must be distinct from \
                 the remaining operands for this operator or function; operator: contains, first \
                 operand: [tags]"
            ),
        ),
        (
            "a filter searching one nested location for itself",
            "Scan",
            json!({"TableName": TABLE, "FilterExpression": "contains(m.inner[1], m.inner[1])"}),
            "Invalid FilterExpression: The first operand must be distinct from the remaining \
             operands for this operator or function; operator: contains, first operand: \
             [m, inner, [1]]"
                .to_string(),
        ),
    ] {
        let stated = crate::probe::refusal(&router, case, operation, request).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// The table the search rows address, and the vector index over its `emb` attribute.
const VECTOR_TABLE: &str = "LegacyOperandVectors";
const VECTOR_INDEX: &str = "by-emb";

#[tokio::test]
async fn a_search_vector_of_anything_but_32_bit_floats_states_the_services_own_sentence() {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": VECTOR_TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "VectorIndexes": [{
                "IndexName": VECTOR_INDEX,
                "VectorAttribute": {"AttributeName": "emb"},
                "Dimensions": 2,
                "DistanceFunction": "COSINE",
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    describe_when_active(&router, "the vector table", VECTOR_TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": VECTOR_TABLE,
               "Item": {"pk": {"S": "v1"}, "emb": {"L": [{"N": "1"}, {"N": "0"}]}}}),
    )
    .await;

    // AWS's fixed sentence for this refusal, stated on its own: it is a rule the SERVICE
    // enforces rather than a `@length` or type constraint the model declares for the member
    // (`smithy_query(query="shape", shape="SearchVectorList")` declares a list of
    // `AttributeValue` with a length of 1 to 4096 and nothing about the elements), so it
    // carries neither the validation envelope nor a member path.
    let expected = "Search vector contains invalid values. All values in the search vector must \
                    be a 32-bit floating-point number attribute";

    // The rows span what an element can be other than a 32-bit float: a value of another
    // type (string, boolean, null, a set, a nested list, a map) and a NUMBER no 32-bit
    // float can hold, in either position of the vector.
    for (case, vector) in [
        ("a string element", json!([{"S": "1.0"}, {"N": "0"}])),
        ("a boolean element", json!([{"N": "1"}, {"BOOL": true}])),
        ("a null element", json!([{"NULL": true}, {"N": "0"}])),
        ("a number-set element", json!([{"NS": ["1"]}, {"N": "0"}])),
        ("a nested list element", json!([{"L": [{"N": "1"}]}, {"N": "0"}])),
        ("a map element", json!([{"M": {"a": {"N": "1"}}}, {"N": "0"}])),
        (
            "a magnitude past what a 32-bit float holds",
            json!([{"N": "1e39"}, {"N": "0"}]),
        ),
        (
            "the same magnitude, negative",
            json!([{"N": "0"}, {"N": "-1e39"}]),
        ),
    ] {
        let stated = crate::probe::refusal(
            &router,
            case,
            "SearchVectors",
            json!({"TableName": VECTOR_TABLE, "IndexName": VECTOR_INDEX,
                   "SearchVector": vector, "TopK": 1}),
        )
        .await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // The CONTROL: a vector of numbers a 32-bit float holds searches the index. The index
    // becomes searchable once the transition its creation owes is due, so the search is
    // polled for rather than timed.
    let started = std::time::Instant::now();
    let searched = loop {
        let (status, body) = call(
            &router,
            "SearchVectors",
            json!({"TableName": VECTOR_TABLE, "IndexName": VECTOR_INDEX,
                   "SearchVector": [{"N": "1"}, {"N": "0"}], "TopK": 1}),
        )
        .await;
        if status == 200 {
            break body;
        }
        assert!(
            started.elapsed() < crate::emulator::ACTIVE_DEADLINE,
            "case a well-formed search vector: the index never served a search within {:?}, last \
             {status} with {body}",
            crate::emulator::ACTIVE_DEADLINE
        );
        tokio::time::sleep(crate::emulator::POLL_INTERVAL).await;
    };
    assert_eq!(
        searched["SearchResults"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "case a well-formed search vector: expected the one indexed item, got {searched}"
    );
}
