// SPDX-License-Identifier: CC0-1.0

//! Which operand TYPES the legacy subsequence comparisons take.
//!
//! RULE: `CONTAINS` and `NOT_CONTAINS` look for ONE value inside the value an item holds —
//! "checks for a subsequence, or value in a set"
//! (`smithy_query(query="doc",shape="ComparisonOperator")`) — so every operand that IS a
//! single scalar is admitted, and an operand that is itself a COLLECTION (a document type, or
//! a set type, which the operator's doc excludes) names no single value to look for and is
//! refused HTTP 400 `ValidationException` naming the comparison and the operand's own
//! `AttributeValue` type. An admitted operand the stored value cannot hold simply matches
//! nothing: the read is served and selects no item, rather than being refused.
//!
//! The rows span what the rule generalises over: every member of the `AttributeValue` union
//! (`smithy_query(query="shape",shape="AttributeValue")`) as the operand's type, both
//! subsequence comparisons, and the member the comparison travels in (a `Scan`'s `ScanFilter`
//! and a `Query`'s `QueryFilter`). The matching rows are the other half: an operand of a type
//! the stored value CAN hold selects the item it is in, and the negated comparison selects it
//! exactly when the plain one does not.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row reads.
const TABLE: &str = "SubsequenceOperands";

/// The clause every refusal about one parameter's value opens with.
const INVALID: &str = "One or more parameter values were invalid";

/// The one item every row is evaluated against: a string, a number, a string set and a list,
/// so a `CONTAINS` has something of each shape to look inside.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "only"},
            "s": {"S": "hello"},
            "n": {"N": "5"},
            "tags": {"SS": ["red", "blue"]},
            "parts": {"L": [{"S": "zero"}, {"BOOL": true}]},
        }}),
    )
    .await;
    router
}

/// One legacy condition map entry: the comparison and the single operand it carries.
fn entry(operator: &str, operand: Value) -> Value {
    json!({"ComparisonOperator": operator, "AttributeValueList": [operand]})
}

/// Drive the entry `{attribute: entry}` through `member`, the legacy filter of `operation`.
async fn filtered(
    router: &MultiServiceRouter,
    operation: &str,
    attribute: &str,
    entry: Value,
) -> (u16, Value) {
    let mut request = json!({"TableName": TABLE, "ConsistentRead": true});
    if operation == "Scan" {
        request["ScanFilter"] = json!({attribute: entry});
    } else {
        request["KeyConditions"] =
            json!({"pk": {"ComparisonOperator": "EQ", "AttributeValueList": [{"S": "only"}]}});
        request["QueryFilter"] = json!({attribute: entry});
    }
    call(router, operation, request).await
}

/// The sentence a comparison that cannot take an operand of `datatype` is refused with.
fn not_valid_for(operator: &str, datatype: &str) -> String {
    format!("{INVALID}: ComparisonOperator {operator} is not valid for {datatype} AttributeValue type")
}

/// One operand type: how the wire spells a value of it, its `AttributeValue` type tag, and
/// whether a subsequence comparison takes it.
struct Operand {
    case: &'static str,
    datatype: &'static str,
    value: Value,
    scalar: bool,
}

/// Every member of the `AttributeValue` union, each as an operand of one subsequence test.
/// The scalar half is admitted; the collections — the two document types and the three set
/// types — are refused.
fn operands() -> Vec<Operand> {
    vec![
        Operand {
            case: "a string",
            datatype: "S",
            value: json!({"S": "red"}),
            scalar: true,
        },
        Operand {
            case: "a number",
            datatype: "N",
            value: json!({"N": "5"}),
            scalar: true,
        },
        Operand {
            case: "a binary value",
            datatype: "B",
            value: json!({"B": "AQID"}),
            scalar: true,
        },
        Operand {
            case: "a boolean",
            datatype: "BOOL",
            value: json!({"BOOL": true}),
            scalar: true,
        },
        Operand {
            case: "a null",
            datatype: "NULL",
            value: json!({"NULL": true}),
            scalar: true,
        },
        Operand {
            case: "a map",
            datatype: "M",
            value: json!({"M": {"a": {"N": "1"}}}),
            scalar: false,
        },
        Operand {
            case: "a list",
            datatype: "L",
            value: json!({"L": [{"S": "red"}]}),
            scalar: false,
        },
        Operand {
            case: "a string set",
            datatype: "SS",
            value: json!({"SS": ["red"]}),
            scalar: false,
        },
        Operand {
            case: "a number set",
            datatype: "NS",
            value: json!({"NS": ["1"]}),
            scalar: false,
        },
        Operand {
            case: "a binary set",
            datatype: "BS",
            value: json!({"BS": ["AQID"]}),
            scalar: false,
        },
    ]
}

#[tokio::test]
async fn a_subsequence_test_takes_every_scalar_operand_and_no_collection() {
    let router = seeded().await;

    for operand in operands() {
        for operator in ["CONTAINS", "NOT_CONTAINS"] {
            for operation in ["Scan", "Query"] {
                let case = format!("{} as the {operator} operand of a {operation}", operand.case);
                let (status, body) = filtered(
                    &router,
                    operation,
                    "tags",
                    entry(operator, operand.value.clone()),
                )
                .await;
                if operand.scalar {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: a scalar operand must be served, got {status} with {body}"
                    );
                } else {
                    let expected = not_valid_for(operator, operand.datatype);
                    assert_eq!(
                        (status, error_code(&body).as_str(), message(&body)),
                        (400, "ValidationException", expected.clone()),
                        "case {case:?}: expected {expected:?}, got {status} with {body}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn an_admitted_operand_selects_by_what_the_item_holds() {
    let router = seeded().await;

    // Each row: the attribute searched, the `CONTAINS` operand, and whether the stored item
    // is selected. The negated comparison is asserted against the opposite answer on the same
    // row, since the item carries every attribute these rows name.
    for (case, attribute, operand, selects) in [
        (
            "a member of the stored string set",
            "tags",
            json!({"S": "red"}),
            true,
        ),
        (
            "a string the set does not hold",
            "tags",
            json!({"S": "green"}),
            false,
        ),
        (
            "a substring of the stored string",
            "s",
            json!({"S": "ell"}),
            true,
        ),
        (
            "a string the stored string does not carry",
            "s",
            json!({"S": "xyz"}),
            false,
        ),
        (
            "an element of the stored list",
            "parts",
            json!({"S": "zero"}),
            true,
        ),
        (
            // A BOOLEAN is admitted and looked for: the stored list holds one, so it is
            // found there and nowhere else.
            "a boolean the stored list holds",
            "parts",
            json!({"BOOL": true}),
            true,
        ),
        (
            "a boolean against the stored string set",
            "tags",
            json!({"BOOL": true}),
            false,
        ),
        (
            "a null against the stored string set",
            "tags",
            json!({"NULL": true}),
            false,
        ),
        (
            "a number against the stored number",
            "n",
            json!({"N": "5"}),
            false,
        ),
    ] {
        for operation in ["Scan", "Query"] {
            let (status, body) = filtered(
                &router,
                operation,
                attribute,
                entry("CONTAINS", operand.clone()),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} ({operation}): the read must be served, got {status} with {body}"
            );
            assert_eq!(
                body["Count"],
                json!(i64::from(selects)),
                "case {case:?} ({operation}): expected the item to be {}, got {body}",
                if selects { "selected" } else { "left out" }
            );
            let (status, body) = filtered(
                &router,
                operation,
                attribute,
                entry("NOT_CONTAINS", operand.clone()),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} ({operation}, negated): the read must be served, got {status} \
                 with {body}"
            );
            assert_eq!(
                body["Count"],
                json!(i64::from(!selects)),
                "case {case:?} ({operation}, negated): the negated comparison selects the item \
                 exactly when the plain one does not, got {body}"
            );
        }
    }
}
