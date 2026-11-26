// SPDX-License-Identifier: CC0-1.0

//! Which OPERAND TYPES a PartiQL operator is defined over, driven end to end through
//! `ExecuteStatement`.
//!
//! The rule this suite pins, in one sentence: a statement operator is defined only over
//! the operand types it can order or test, so a statement whose operand is a value of
//! another type is refused with HTTP 400 `ValidationException` naming the operator and
//! that operand's type before any item is read — the ordering comparisons (`<`, `<=`,
//! `>`, `>=`) and `BETWEEN` order only the scalar types `S`, `N` and `B`, and
//! `attribute_type` tests only a string naming one of the attribute-type codes — while
//! the same statement over an operand the operator IS defined over is served.
//!
//! Each test states the rule over a TABLE of inputs rather than one statement: the rows
//! span every `AttributeValue` type the operator is not defined over, every ordering
//! operator, each of `BETWEEN`'s three operand positions, and both spellings an operand
//! arrives in (a literal the statement text carries and a `?` the `Parameters` list
//! binds) — with the operand types the operator IS defined over driven beside every
//! refusal, so no row passes by refusing everything.
//!
//! The type codes and their order come from the refusal the service states for an
//! unknown one (`crate::state` holds the same closed set as
//! `ATTRIBUTE_TYPE_NAMES`: the members of the `AttributeValue` union,
//! `smithy_query(query="shape", shape="AttributeValue")`).
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row addresses: one string partition key, and one item carrying a
/// value of each type an operand may be compared against.
const TABLE: &str = "StatementOperandTypes";

/// The attribute-type codes `attribute_type` tests for, in the order this service lists
/// them in its refusal of a name that is not one of them — the members of the
/// `AttributeValue` union (`smithy_query(query="shape", shape="AttributeValue")`).
const TYPE_CODES: [&str; 10] = ["N", "BS", "L", "B", "NULL", "M", "S", "SS", "NS", "BOOL"];

/// Create the table and store the one item the served rows read back.
async fn stored(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
    ok(
        router,
        "PutItem",
        json!({
            "TableName": TABLE,
            "Item": {
                "pk": {"S": "p"},
                "n": {"N": "5"},
                "s": {"S": "beta"},
                "b": {"B": "YWI="},
                "l": {"L": [{"N": "1"}]},
                "m": {"M": {"a": {"N": "1"}}},
                "ss": {"SS": ["a"]},
                "ns": {"NS": ["1"]},
                "bs": {"BS": ["YQ=="]},
                "flag": {"BOOL": true},
                "nothing": {"NULL": true},
            },
        }),
    )
    .await;
}

/// Drive one statement that must be refused as the request's own defect, answering the
/// message it was refused with.
async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    statement: &str,
    parameters: Option<Value>,
) -> String {
    let mut body = json!({"Statement": statement});
    if let Some(parameters) = parameters {
        body["Parameters"] = parameters;
    }
    let (status, answered) = call(router, "ExecuteStatement", body).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException for {statement:?}, got {status} with \
         {answered}"
    );
    message(&answered)
}

/// Drive one statement that must be SERVED, answering the items it reported.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    statement: &str,
    parameters: Option<Value>,
) -> Vec<Value> {
    let mut body = json!({"Statement": statement});
    if let Some(parameters) = parameters {
        body["Parameters"] = parameters;
    }
    let answered = ok(router, "ExecuteStatement", body).await;
    let items = answered["Items"].as_array().cloned().unwrap_or_default();
    assert!(
        answered.get("Items").is_some(),
        "case {case:?}: a served statement reports Items, got {answered}"
    );
    items
}

/// AWS's fixed sentence for an operand whose type the operator or function it was given
/// to is not defined over, with the operator and the operand's type substituted.
fn incorrect_operand(operator_or_function: &str, operand_type: &str) -> String {
    format!(
        "Incorrect operand type for operator or function; operator or function: \
         {operator_or_function}, operand type: {operand_type}"
    )
}

/// The types no ordering operator is defined over, each with the literal a statement
/// spells it with — a list, a tuple, the two bags that denote a set, and the two keyword
/// literals. One row per `AttributeValue` member that is not an ordered scalar, except
/// the binary set, which no literal spells and which rides in through `Parameters`
/// below.
const UNORDERED_LITERALS: [(&str, &str); 6] = [
    ("L", "[1]"),
    ("M", "{'a': 1}"),
    ("SS", "<<'a'>>"),
    ("NS", "<<1>>"),
    ("BOOL", "true"),
    ("NULL", "null"),
];

/// The ordering comparisons, spelled as the statement spells them and named as the
/// refusal names them.
const ORDERING_OPERATORS: [&str; 4] = ["<", "<=", ">", ">="];

#[tokio::test]
async fn an_ordering_comparison_refuses_every_operand_type_it_cannot_order() {
    let router = emulator();
    stored(&router).await;

    for (code, literal) in UNORDERED_LITERALS {
        for operator in ORDERING_OPERATORS {
            // The offending operand on the RIGHT of the comparison…
            let statement = format!("SELECT * FROM \"{TABLE}\" WHERE n {operator} {literal}");
            let case = format!("{code} literal right of {operator}");
            let stated = refused(&router, &case, &statement, None).await;
            let expected = incorrect_operand(operator, code);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );

            // …and on the LEFT, which is the same operand position rule.
            let statement = format!("SELECT * FROM \"{TABLE}\" WHERE {literal} {operator} n");
            let case = format!("{code} literal left of {operator}");
            let stated = refused(&router, &case, &statement, None).await;
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn between_refuses_an_unorderable_operand_in_each_of_its_three_positions() {
    let router = emulator();
    stored(&router).await;

    for (code, literal) in UNORDERED_LITERALS {
        for (position, statement) in [
            (
                "the compared value",
                format!("SELECT * FROM \"{TABLE}\" WHERE {literal} BETWEEN 1 AND 9"),
            ),
            (
                "the lower bound",
                format!("SELECT * FROM \"{TABLE}\" WHERE n BETWEEN {literal} AND 9"),
            ),
            (
                "the upper bound",
                format!("SELECT * FROM \"{TABLE}\" WHERE n BETWEEN 1 AND {literal}"),
            ),
        ] {
            let case = format!("{code} literal as {position} of BETWEEN");
            let stated = refused(&router, &case, &statement, None).await;
            let expected = incorrect_operand("BETWEEN", code);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_bound_parameter_is_held_to_the_same_operand_domain_as_a_literal() {
    let router = emulator();
    stored(&router).await;

    // The same rule through the OTHER spelling an operand arrives in, over the types a
    // parameter can carry that no literal spells (`BS`) and over those that both can.
    for (code, value) in [
        ("BS", json!({"BS": ["YQ=="]})),
        ("SS", json!({"SS": ["a"]})),
        ("NS", json!({"NS": ["1"]})),
        ("L", json!({"L": [{"N": "1"}]})),
        ("M", json!({"M": {"a": {"N": "1"}}})),
        ("BOOL", json!({"BOOL": true})),
        ("NULL", json!({"NULL": true})),
    ] {
        for operator in ORDERING_OPERATORS {
            let statement = format!("SELECT * FROM \"{TABLE}\" WHERE n {operator} ?");
            let case = format!("{code} parameter right of {operator}");
            let stated = refused(&router, &case, &statement, Some(json!([value]))).await;
            let expected = incorrect_operand(operator, code);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }

        let statement = format!("SELECT * FROM \"{TABLE}\" WHERE n BETWEEN ? AND 9");
        let case = format!("{code} parameter as the lower bound of BETWEEN");
        let stated = refused(&router, &case, &statement, Some(json!([value]))).await;
        let expected = incorrect_operand("BETWEEN", code);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

#[tokio::test]
async fn an_ordering_comparison_over_the_scalar_types_it_orders_is_served() {
    let router = emulator();
    stored(&router).await;

    // The CONTROL for every refusal above: the three types DynamoDB documents an order
    // for — "For type Number, value comparisons are numeric. String value comparisons
    // … are based on ASCII character code values. For Binary, DynamoDB treats each byte
    // of the binary data as unsigned when it compares binary values" (the
    // `AttributeValueList` doc) — are served in each operand position, and the
    // comparison DECIDES the row rather than being ignored.
    for (code, attribute, below, above, parameter) in [
        ("N", "n", "1", "9", json!({"N": "9"})),
        ("S", "s", "'alpha'", "'gamma'", json!({"S": "gamma"})),
        ("B", "b", "?", "?", json!({"B": "YWJj"})),
    ] {
        for operator in ORDERING_OPERATORS {
            // A bound that must HOLD of the stored item, so a served row is one the
            // comparison selected rather than one it dropped.
            let (statement, parameters) = if attribute == "b" {
                (
                    format!("SELECT * FROM \"{TABLE}\" WHERE {attribute} {operator} ?"),
                    Some(json!([parameter])),
                )
            } else {
                (
                    format!("SELECT * FROM \"{TABLE}\" WHERE {attribute} {operator} {above}"),
                    None,
                )
            };
            let case = format!("{code} compared with {operator} against a greater value");
            let selected = operator.starts_with('<');
            let items = served(&router, &case, &statement, parameters).await;
            assert_eq!(
                items.len(),
                usize::from(selected),
                "case {case:?}: expected {} row(s), got {items:?}",
                usize::from(selected)
            );
        }

        // And the reverse bound, so each operator is exercised on both sides of the
        // stored value.
        if attribute != "b" {
            for operator in ORDERING_OPERATORS {
                let statement =
                    format!("SELECT * FROM \"{TABLE}\" WHERE {attribute} {operator} {below}");
                let case = format!("{code} compared with {operator} against a smaller value");
                let selected = operator.starts_with('>');
                let items = served(&router, &case, &statement, None).await;
                assert_eq!(
                    items.len(),
                    usize::from(selected),
                    "case {case:?}: expected {} row(s), got {items:?}",
                    usize::from(selected)
                );
            }
        }
    }

    // `BETWEEN` over the same scalar types, inside and outside the range.
    for (case, statement, expected) in [
        ("a number inside the range", "n BETWEEN 1 AND 9", 1),
        ("a number below the range", "n BETWEEN 6 AND 9", 0),
        ("a string inside the range", "s BETWEEN 'alpha' AND 'gamma'", 1),
        ("a string outside the range", "s BETWEEN 'x' AND 'z'", 0),
    ] {
        let items = served(
            &router,
            case,
            &format!("SELECT * FROM \"{TABLE}\" WHERE {statement}"),
            None,
        )
        .await;
        assert_eq!(
            items.len(),
            expected,
            "case {case:?}: expected {expected} row(s), got {items:?}"
        );
    }
}

#[tokio::test]
async fn the_type_test_takes_a_string_naming_one_of_the_published_type_codes() {
    let router = emulator();
    stored(&router).await;

    // An operand of any type other than a string names no type at all, so the test is
    // refused for that operand's type — the same sentence the ordering operators state.
    for (code, literal, parameters) in [
        ("N", "1", None),
        ("L", "[1]", None),
        ("M", "{'a': 1}", None),
        ("SS", "<<'a'>>", None),
        ("NS", "<<1>>", None),
        ("BOOL", "true", None),
        ("NULL", "null", None),
        ("BS", "?", Some(json!([{"BS": ["YQ=="]}]))),
        ("B", "?", Some(json!([{"B": "YQ=="}]))),
    ] {
        let statement = format!("SELECT * FROM \"{TABLE}\" WHERE attribute_type(n, {literal})");
        let case = format!("attribute_type given a {code} operand");
        let stated = refused(&router, &case, &statement, parameters).await;
        let expected = incorrect_operand("attribute_type", code);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // A STRING that names no published code is refused for the name, listing the closed
    // set — including a code the issue's own examples never name.
    for unknown in ["X", "n", "Number", "SSS", ""] {
        let statement = format!("SELECT * FROM \"{TABLE}\" WHERE attribute_type(n, '{unknown}')");
        let case = format!("attribute_type given the name {unknown:?}");
        let stated = refused(&router, &case, &statement, None).await;
        let expected = format!(
            "Invalid attribute type name found; type: {unknown}, valid types: {{{}}}",
            TYPE_CODES.join(",")
        );
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // Every published code is served, and the test decides the row by the type the item
    // actually holds: the attribute of that type matches and the number does not.
    for code in TYPE_CODES {
        let attribute = match code {
            "N" => "n",
            "S" => "s",
            "B" => "b",
            "L" => "l",
            "M" => "m",
            "SS" => "ss",
            "NS" => "ns",
            "BS" => "bs",
            "BOOL" => "flag",
            _ => "nothing",
        };
        let case = format!("attribute_type({attribute}, '{code}')");
        let items = served(
            &router,
            &case,
            &format!("SELECT * FROM \"{TABLE}\" WHERE attribute_type({attribute}, '{code}')"),
            None,
        )
        .await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: the attribute of that type matches, got {items:?}"
        );

        // The same code against an attribute of ANOTHER type selects nothing, so the
        // served rows above are the type test and not a statement that matches anything.
        let other = if attribute == "s" { "n" } else { "s" };
        let case = format!("attribute_type({other}, '{code}')");
        let items = served(
            &router,
            &case,
            &format!("SELECT * FROM \"{TABLE}\" WHERE attribute_type({other}, '{code}')"),
            None,
        )
        .await;
        let expected = usize::from(
            (other == "n" && code == "N") || (other == "s" && code == "S"),
        );
        assert_eq!(
            items.len(),
            expected,
            "case {case:?}: expected {expected} row(s), got {items:?}"
        );
    }
}
