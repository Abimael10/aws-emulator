// SPDX-License-Identifier: CC0-1.0

//! Parentheses in a `KeyConditionExpression` GROUP what the grammar reads at the position
//! they stand in, exactly as they do in the condition and filter grammar beside it.
//!
//! The rules pinned here:
//!
//! 1. every operand position of a key condition — either side of a comparator, each bound
//!    of a range and each argument of `begins_with` — reads an operand, so a pair of
//!    parentheses written around one states that operand itself: the `Query` is read,
//!    evaluated and answered exactly as the same key condition without the pair is;
//! 2. grouping does not change what an operand IS: a second attribute name, or an operand
//!    that is itself an operation, is still the defect the member names however many pairs
//!    are written around it, and a pair that wraps nothing but another pair still groups
//!    what is already grouped.
//!
//! The comparison forms the rows span are the member's own catalog —
//! `=`, `<`, `<=`, `>`, `>=`, `BETWEEN` and `begins_with`
//! (`smithy_query(query="doc",shape="QueryInput",member="KeyConditionExpression")`) — each
//! written both key-first and value-first.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses: a partition key and a sort key, so the member's
/// whole comparison catalog can be written against it.
const TABLE: &str = "KeyConditionGroupedOperands";

/// The sort-key values of the items [`TABLE`] holds under the partition every row queries,
/// in the order a forward `Query` reads them.
const SORT_KEYS: [&str; 3] = ["a", "b", "zz"];

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write the
/// items every row is evaluated against: one partition the rows query and one they do not,
/// so a key condition that selected the wrong partition would show up as a count.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"},
                          {"AttributeName": "sk", "KeyType": "RANGE"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"},
                                     {"AttributeName": "sk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "seed the table", TABLE).await;
    for sort_key in SORT_KEYS {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE,
                   "Item": {"pk": {"S": "p1"}, "sk": {"S": sort_key},
                            "mark": {"S": format!("p1-{sort_key}")}}}),
        )
        .await;
    }
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "p2"}, "sk": {"S": "a"}, "mark": {"S": "p2-a"}}}),
    )
    .await;
    router
}

/// Whether `expression` states the substitution token `token`: the token's text, ending
/// where the token does — `:b` is not the `:bb` another row could carry.
fn states_token(expression: &str, token: &str) -> bool {
    expression.match_indices(token).any(|(at, _)| {
        expression[at + token.len()..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_ascii_alphanumeric() && next != '_')
    })
}

/// The substitution maps `expression` needs, taken from the catalog the rows share: a
/// request carries exactly the tokens its expression dereferences, because a key no
/// expression reads is refused as unused.
fn substitutions(expression: &str) -> (Value, Value) {
    let mut values = serde_json::Map::new();
    for (token, value) in [
        (":p1", json!({"S": "p1"})),
        (":a", json!({"S": "a"})),
        (":b", json!({"S": "b"})),
        (":z", json!({"S": "z"})),
    ] {
        if states_token(expression, token) {
            values.insert(token.to_string(), value);
        }
    }
    let mut names = serde_json::Map::new();
    for (token, attribute) in [("#p", "pk"), ("#s", "sk")] {
        if states_token(expression, token) {
            names.insert(token.to_string(), json!(attribute));
        }
    }
    (Value::Object(values), Value::Object(names))
}

/// A `Query` on [`TABLE`] stating `expression` in `member`, beside the substitution maps
/// that expression dereferences.
fn querying(member: &str, expression: &str) -> Value {
    let mut request = json!({"TableName": TABLE});
    request[member] = json!(expression);
    let (values, names) = substitutions(expression);
    if values.as_object().is_some_and(|stated| !stated.is_empty()) {
        request["ExpressionAttributeValues"] = values;
    }
    if names.as_object().is_some_and(|stated| !stated.is_empty()) {
        request["ExpressionAttributeNames"] = names;
    }
    request
}

/// One `Query` whose key condition is `condition`: the status and the whole page it answers.
async fn query(router: &MultiServiceRouter, condition: &str) -> (u16, Value) {
    call(
        router,
        "Query",
        querying("KeyConditionExpression", condition),
    )
    .await
}

/// One key condition written twice: without the grouping parentheses, and with a pair around
/// each operand it states.
struct Grouped {
    case: &'static str,
    bare: &'static str,
    grouped: &'static str,
    /// How many items the condition selects, so the row pins the EVALUATION and not only
    /// the status.
    selects: u64,
}

#[tokio::test]
async fn parentheses_around_a_key_condition_operand_state_that_operand() {
    let router = seeded().await;

    // Rows differ along the comparison the condition states (the member's whole catalog),
    // along which side carries the key attribute, along how the attribute is written (out,
    // or through an `ExpressionAttributeNames` alias), and along how many items the
    // condition then selects.
    let rows: Vec<Grouped> = vec![
        Grouped {
            case: "both operands of the partition-key equality",
            bare: "pk = :p1",
            grouped: "(pk) = (:p1)",
            selects: 3,
        },
        Grouped {
            case: "the partition key written as an alias",
            bare: "#p = :p1",
            grouped: "(#p) = (:p1)",
            selects: 3,
        },
        Grouped {
            case: "the partition-key equality written value-first",
            bare: ":p1 = pk",
            grouped: "(:p1) = (pk)",
            selects: 3,
        },
        Grouped {
            case: "an equality on the sort key",
            bare: "pk = :p1 AND sk = :a",
            grouped: "(pk) = (:p1) AND (sk) = (:a)",
            selects: 1,
        },
        Grouped {
            case: "a less-than on the sort key",
            bare: "pk = :p1 AND sk < :b",
            grouped: "(pk) = (:p1) AND (sk) < (:b)",
            selects: 1,
        },
        Grouped {
            case: "a less-or-equal on the sort key",
            bare: "pk = :p1 AND sk <= :b",
            grouped: "(pk) = (:p1) AND (sk) <= (:b)",
            selects: 2,
        },
        Grouped {
            case: "a greater-than on the sort key",
            bare: "pk = :p1 AND sk > :a",
            grouped: "(pk) = (:p1) AND (sk) > (:a)",
            selects: 2,
        },
        Grouped {
            case: "a greater-or-equal on the sort key",
            bare: "pk = :p1 AND sk >= :b",
            grouped: "(pk) = (:p1) AND (sk) >= (:b)",
            selects: 2,
        },
        Grouped {
            case: "a sort-key comparison written value-first",
            bare: "pk = :p1 AND :a < sk",
            grouped: "(pk) = (:p1) AND (:a) < (sk)",
            selects: 2,
        },
        Grouped {
            case: "the bounds of a sort-key range",
            bare: "pk = :p1 AND sk BETWEEN :a AND :b",
            grouped: "(pk) = (:p1) AND (sk) BETWEEN (:a) AND (:b)",
            selects: 2,
        },
        Grouped {
            case: "both arguments of begins_with",
            bare: "pk = :p1 AND begins_with(sk, :z)",
            grouped: "(pk) = (:p1) AND begins_with((sk), (:z))",
            selects: 1,
        },
        Grouped {
            case: "the sort key written as an alias beside a grouped clause",
            bare: "(pk = :p1) AND #s = :a",
            grouped: "(pk = :p1) AND (#s) = (:a)",
            selects: 1,
        },
        Grouped {
            case: "every clause and every operand grouped",
            bare: "pk = :p1 AND sk = :a",
            grouped: "((pk) = (:p1) AND (sk) = (:a))",
            selects: 1,
        },
    ];

    for row in &rows {
        let (bare_status, bare_page) = query(&router, row.bare).await;
        assert_eq!(
            bare_status, 200,
            "case {:?}: the ungrouped key condition {:?} is served, got {bare_status} with \
             {bare_page}",
            row.case, row.bare
        );
        let (status, page) = query(&router, row.grouped).await;
        assert_eq!(
            status, 200,
            "case {:?}: the grouped key condition {:?} states the same operands, got {status} \
             with {page}",
            row.case, row.grouped
        );
        assert_eq!(
            (&page["Count"], &page["ScannedCount"], &page["Items"]),
            (
                &json!(row.selects),
                &json!(row.selects),
                &bare_page["Items"]
            ),
            "case {:?}: the grouped key condition {:?} must answer the page {:?} answers \
             (Count {}, ScannedCount {}), got {page}",
            row.case,
            row.grouped,
            row.bare,
            row.selects,
            row.selects
        );
    }
}

/// One `Query` whose key condition is refused, and the whole message it earns.
struct Refused {
    case: &'static str,
    condition: &'static str,
    clause: &'static str,
}

#[tokio::test]
async fn grouping_does_not_change_what_a_key_condition_operand_is() {
    let router = seeded().await;

    // Rows differ along the defect the grouped text states and along the position the
    // grouping stands in: a pair around a pair, a grouped operand naming a second
    // attribute (on either side of the comparator and as a range bound), and a grouped
    // operand that is itself an operation.
    let rows: Vec<Refused> = vec![
        Refused {
            case: "a pair around an operand's own group",
            condition: "((pk)) = (:p1)",
            clause: "The expression has redundant parentheses;",
        },
        Refused {
            case: "a pair around the group of a value-first operand",
            condition: "((:p1)) = pk",
            clause: "The expression has redundant parentheses;",
        },
        Refused {
            case: "a grouped operand naming a second attribute",
            condition: "(pk) = (sk)",
            clause: "Invalid condition in KeyConditionExpression: Multiple attribute names used \
                     in one condition",
        },
        Refused {
            case: "a grouped range bound naming a second attribute",
            condition: "pk = :p1 AND (sk) BETWEEN (:a) AND (pk)",
            clause: "Invalid condition in KeyConditionExpression: Multiple attribute names used \
                     in one condition",
        },
        Refused {
            case: "a grouped operand that is itself an operation",
            condition: "pk = (begins_with(sk, :z))",
            clause: "KeyConditionExpressions cannot contain nested operations",
        },
    ];

    for row in &rows {
        // A key condition travels in a member a READ narrows its result with, so its
        // refusals carry the clause on its own rather than inside the counted envelope the
        // write members' refusals carry.
        let stated = format!("Invalid KeyConditionExpression: {}", row.clause);
        let (status, body) = query(&router, row.condition).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", stated.as_str()),
            "case {:?} ({:?}): expected the refusal {stated:?}, got {status} with {body}",
            row.case,
            row.condition
        );
    }
}

#[tokio::test]
async fn a_grouped_filter_narrows_a_grouped_key_condition() {
    let router = seeded().await;

    // The two expression members a `Query` reads are parsed by two grammars of this
    // service, so one request states a grouped operand in each and the page it answers
    // pins both.
    let request = json!({
        "TableName": TABLE,
        "KeyConditionExpression": "(pk) = (:p1)",
        "FilterExpression": "(mark) = (:mark)",
        "ExpressionAttributeValues": {":p1": {"S": "p1"}, ":mark": {"S": "p1-b"}},
    });
    let (status, page) = call(&router, "Query", request).await;
    assert_eq!(
        (status, &page["Count"], &page["ScannedCount"]),
        (200, &json!(1), &json!(3)),
        "grouped operands in both expression members: expected a 200 page selecting 1 of the 3 \
         items the key condition reads, got {status} with {page}"
    );
    assert_eq!(
        page["Items"],
        json!([{"pk": {"S": "p1"}, "sk": {"S": "b"}, "mark": {"S": "p1-b"}}]),
        "grouped operands in both expression members: expected the one item the filter selects, \
         got {page}"
    );
}
