// SPDX-License-Identifier: CC0-1.0

//! Parentheses in the condition and filter grammar GROUP what the grammar reads at the
//! position they stand in.
//!
//! The rules pinned here:
//!
//! 1. an operand position and a function ARGUMENT position each read an operand, so a pair
//!    of parentheses written around one states that operand or argument itself: the request
//!    is read, evaluated and answered exactly as the same expression without the pair is.
//!    The parentheses a CALL opens carry its arguments rather than grouping anything, so a
//!    single pair around an argument is not a pair around a group — while a pair around
//!    such a pair groups what is already grouped and is still refused;
//! 2. grouping does not change what an operand IS: a value written in a position whose
//!    signature declares a document path still names no location, however many pairs are
//!    written around it, and the operand rules of the other positions are read through the
//!    grouping just the same;
//! 3. `begins_with` reads BOTH of its operands the way `contains` does — a value the
//!    request carried, or a location the value is read from — so a prefix test whose tested
//!    text is a value is evaluated against every item the scan reads and answered as the
//!    page it selects, which is an EMPTY page when it selects nothing. Only a literal of a
//!    type that has no prefix, and a test of one location against itself, are refused.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The table every row below addresses.
const TABLE: &str = "GroupedOperands";

/// How many items a full scan of [`TABLE`] reads, which is what `ScannedCount` reports
/// whatever the filter then selects.
const ITEMS: u64 = 2;

/// Create [`TABLE`], wait out the documented `CREATING` -> `ACTIVE` transition and write the
/// two items every row is evaluated against.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "seed the table", TABLE).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "one"}, "s": {"S": "abc"}, "b": {"S": "ab"},
                        "n": {"N": "5"}, "tags": {"SS": ["x", "y"]},
                        "m": {"M": {"child": {"L": [{"S": "one"}, {"S": "two"}]}}}}}),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE,
               "Item": {"pk": {"S": "two"}, "s": {"S": "zed"}, "b": {"S": "ze"},
                        "n": {"N": "9"}}}),
    )
    .await;
    router
}

/// Whether `expression` states the substitution token `token`: the token's text, ending
/// where the token does — `:a` is not the `:abc` another row carries.
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
    let values: Vec<(&str, Value)> = vec![
        (":abc", json!({"S": "abc"})),
        (":zed", json!({"S": "zed"})),
        (":a", json!({"S": "a"})),
        (":x", json!({"S": "x"})),
        (":two", json!({"S": "two"})),
        (":type_s", json!({"S": "S"})),
        (":three", json!({"N": "3"})),
        (":four", json!({"N": "4"})),
        (":six", json!({"N": "6"})),
    ];
    let mut stated = serde_json::Map::new();
    for (token, value) in values {
        if states_token(expression, token) {
            stated.insert(token.to_string(), value);
        }
    }
    let mut names = serde_json::Map::new();
    if states_token(expression, "#s") {
        names.insert("#s".to_string(), json!("s"));
    }
    (Value::Object(stated), Value::Object(names))
}

/// `request` with `expression` in `member`, beside the substitution maps that expression
/// dereferences.
fn stating(request: Value, member: &str, expression: &str) -> Value {
    let mut request = request;
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

/// One `Scan` whose filter is `filter`: the status and the whole page it answers.
async fn scan(router: &MultiServiceRouter, filter: &str) -> (u16, Value) {
    call(
        router,
        "Scan",
        stating(json!({"TableName": TABLE}), "FilterExpression", filter),
    )
    .await
}

/// One expression written twice: without the grouping parentheses, and with a pair around
/// each operand or argument it states.
struct Grouped {
    case: &'static str,
    bare: &'static str,
    grouped: &'static str,
    /// How many of the two items the filter selects, so the row pins the EVALUATION and not
    /// only the status.
    selects: u64,
}

#[tokio::test]
async fn parentheses_around_an_operand_or_an_argument_state_that_operand() {
    let router = seeded().await;

    // Rows differ in the position the parentheses stand in — both sides of a comparison, the
    // bounds of a range, the members of a membership list, the operand of `size`, and the
    // arguments of each condition function the language has — and in the shape of what they
    // wrap: a top-level name, a path two steps into the item, an `#alias`, and a value the
    // request carried.
    let rows: Vec<Grouped> = vec![
        Grouped {
            case: "both operands of an equality",
            bare: "s = :abc",
            grouped: "(s) = (:abc)",
            selects: 1,
        },
        Grouped {
            case: "both operands of an ordering comparison",
            bare: "n > :four",
            grouped: "(n) > (:four)",
            selects: 2,
        },
        Grouped {
            case: "an operand that reaches two steps into the item",
            bare: "m.child[1] = :two",
            grouped: "(m.child[1]) = (:two)",
            selects: 1,
        },
        Grouped {
            case: "an operand written as an alias",
            bare: "#s = :abc",
            grouped: "(#s) = (:abc)",
            selects: 1,
        },
        Grouped {
            case: "the operand of size, and the value it is compared with",
            bare: "size(s) = :three",
            grouped: "size((s)) = (:three)",
            selects: 2,
        },
        Grouped {
            case: "the argument of attribute_exists",
            bare: "attribute_exists(m)",
            grouped: "attribute_exists((m))",
            selects: 1,
        },
        Grouped {
            case: "the argument of attribute_not_exists",
            bare: "attribute_not_exists(m)",
            grouped: "attribute_not_exists((m))",
            selects: 1,
        },
        Grouped {
            case: "both arguments of attribute_type",
            bare: "attribute_type(s, :type_s)",
            grouped: "attribute_type((s), (:type_s))",
            selects: 2,
        },
        Grouped {
            case: "both arguments of begins_with",
            bare: "begins_with(s, :a)",
            grouped: "begins_with((s), (:a))",
            selects: 1,
        },
        Grouped {
            case: "both arguments of contains",
            bare: "contains(tags, :x)",
            grouped: "contains((tags), (:x))",
            selects: 1,
        },
        Grouped {
            case: "the bounds of a range",
            bare: "n BETWEEN :four AND :six",
            grouped: "(n) BETWEEN (:four) AND (:six)",
            selects: 1,
        },
        Grouped {
            case: "the members of a membership list",
            bare: "s IN (:abc, :zed)",
            grouped: "(s) IN ((:abc), (:zed))",
            selects: 2,
        },
        Grouped {
            case: "the operands inside a negated clause",
            bare: "NOT (s = :abc)",
            grouped: "NOT ((s) = (:abc))",
            selects: 1,
        },
        Grouped {
            case: "the operands of two clauses joined by AND",
            bare: "s = :abc AND begins_with(b, :a)",
            grouped: "(s) = (:abc) AND begins_with((b), (:a))",
            selects: 1,
        },
    ];

    for row in &rows {
        let (bare_status, bare_page) = scan(&router, row.bare).await;
        assert_eq!(
            bare_status, 200,
            "case {:?}: the ungrouped filter {:?} is served, got {bare_status} with {bare_page}",
            row.case, row.bare
        );
        let (status, page) = scan(&router, row.grouped).await;
        assert_eq!(
            status, 200,
            "case {:?}: the grouped filter {:?} states the same operands, got {status} with \
             {page}",
            row.case, row.grouped
        );
        assert_eq!(
            (&page["Count"], &page["ScannedCount"], &page["Items"]),
            (&json!(row.selects), &json!(ITEMS), &bare_page["Items"]),
            "case {:?}: the grouped filter {:?} must answer the page {:?} answers (Count {}, \
             ScannedCount {ITEMS}), got {page}",
            row.case,
            row.grouped,
            row.bare,
            row.selects
        );
    }
}

/// The clause a member's refusal carries, inside the `<N> validation error(s) detected: `
/// envelope for the members a WRITE states its intent through and on its own for the members
/// a READ narrows its result with.
fn stated_for(member: &str, clause: &str) -> String {
    let clause = format!("Invalid {member}: {clause}");
    if member == "ConditionExpression" || member == "UpdateExpression" {
        return format!("1 validation error detected: {clause}");
    }
    clause
}

/// One request that states an expression, and the whole message it earns.
struct Refused {
    case: &'static str,
    operation: &'static str,
    /// Everything the request states besides the expression and its substitution maps.
    request: Value,
    member: &'static str,
    expression: &'static str,
    clause: String,
}

/// Send every row and hold each to the whole message it must earn.
async fn each_refused(router: &MultiServiceRouter, rows: &[Refused]) {
    for row in rows {
        let request = stating(row.request.clone(), row.member, row.expression);
        let stated = stated_for(row.member, &row.clause);
        let (status, body) = call(router, row.operation, request).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", stated.as_str()),
            "case {:?} on {} ({:?}): expected the refusal {stated:?}, got {status} with {body}",
            row.case,
            row.operation,
            row.expression
        );
    }
}

#[tokio::test]
async fn a_pair_around_a_grouped_operand_or_argument_is_still_redundant() {
    let router = seeded().await;

    let redundant = "The expression has redundant parentheses;".to_string();
    // Rows differ in the position the second pair stands in — an operand of a comparison, the
    // argument of a one-argument call, one argument of a two-argument call, and a whole call
    // — and in the member that carried the expression.
    let rows: Vec<Refused> = vec![
        Refused {
            case: "a filter wrapping an operand's own group",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "((s)) = :abc",
            clause: redundant.clone(),
        },
        Refused {
            case: "a filter wrapping the group around a call's only argument",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "attribute_exists(((m)))",
            clause: redundant.clone(),
        },
        Refused {
            case: "a filter wrapping the group around one argument of two",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "begins_with(((s)), :a)",
            clause: redundant.clone(),
        },
        Refused {
            case: "a filter wrapping the group around a whole call",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "((attribute_exists(m)))",
            clause: redundant.clone(),
        },
        Refused {
            case: "a write's condition wrapping an operand's own group",
            operation: "PutItem",
            request: json!({"TableName": TABLE, "Item": {"pk": {"S": "one"}}}),
            member: "ConditionExpression",
            expression: "((s)) = :abc",
            clause: redundant.clone(),
        },
        Refused {
            case: "a write's condition wrapping a call argument's group",
            operation: "DeleteItem",
            request: json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}}}),
            member: "ConditionExpression",
            expression: "attribute_exists(((s)))",
            clause: redundant,
        },
    ];

    each_refused(&router, &rows).await;
}

#[tokio::test]
async fn grouping_does_not_change_what_an_operand_is() {
    let router = seeded().await;

    // The functions whose signature declares a document path require one however many pairs
    // of parentheses are written around the value put there, and every other operand rule is
    // read through the grouping just the same.
    let rows: Vec<Refused> = vec![
        Refused {
            case: "a grouped value where attribute_exists needs a path",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "attribute_exists((:abc))",
            clause: "Operator or function requires a document path; operator or function: \
                     attribute_exists"
                .to_string(),
        },
        Refused {
            case: "a grouped value where size needs a path",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "size((:abc)) = :three",
            clause: "Operator or function requires a document path; operator or function: size"
                .to_string(),
        },
        Refused {
            case: "a grouped prefix of a type that has none",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "begins_with(s, (:four))",
            clause: "Incorrect operand type for operator or function; operator or function: \
                     begins_with, operand type: N"
                .to_string(),
        },
        Refused {
            case: "a grouped type name that names no type",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "attribute_type(s, (:abc))",
            clause: "Invalid attribute type name found; type: abc, valid types: \
                     {S,SS,N,NS,B,BS,BOOL,NULL,L,M}"
                .to_string(),
        },
        Refused {
            case: "a grouped operand that reads the location it is compared with",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "(s) = (s)",
            clause: "The first operand must be distinct from the remaining operands for this \
                     operator or function; operator: =, first operand: [s]"
                .to_string(),
        },
        Refused {
            case: "a grouped prefix read from the location it is tested against",
            operation: "Scan",
            request: json!({"TableName": TABLE}),
            member: "FilterExpression",
            expression: "begins_with((s), (s))",
            clause: "The first operand must be distinct from the remaining operands for this \
                     operator or function; operator: begins_with, first operand: [s]"
                .to_string(),
        },
    ];

    each_refused(&router, &rows).await;
}

#[tokio::test]
async fn a_prefix_test_reads_both_of_its_operands_as_operands() {
    let router = seeded().await;

    // Rows differ in WHICH operand the request carried as a value and which it read off the
    // item, and in how many items the test then selects — the page a filter that selects
    // nothing answers is an empty page, never a refusal.
    let rows: Vec<Grouped> = vec![
        Grouped {
            case: "a location tested for a value's prefix, the documented order",
            bare: "begins_with(s, :a)",
            grouped: "begins_with((s), (:a))",
            selects: 1,
        },
        Grouped {
            case: "a value tested for a prefix one item holds",
            bare: "begins_with(:abc, b)",
            grouped: "begins_with((:abc), (b))",
            selects: 1,
        },
        Grouped {
            case: "a value tested for a prefix no item holds",
            bare: "begins_with(:a, s)",
            grouped: "begins_with((:a), (s))",
            selects: 0,
        },
        Grouped {
            case: "two values the request carried that share a prefix",
            bare: "begins_with(:abc, :a)",
            grouped: "begins_with((:abc), (:a))",
            selects: 2,
        },
        Grouped {
            case: "two values the request carried that share no prefix",
            bare: "begins_with(:abc, :zed)",
            grouped: "begins_with((:abc), (:zed))",
            selects: 0,
        },
        Grouped {
            case: "a location tested for a prefix read from another location",
            bare: "begins_with(s, b)",
            grouped: "begins_with((s), (b))",
            selects: 2,
        },
    ];

    for row in &rows {
        for filter in [row.bare, row.grouped] {
            let (status, page) = scan(&router, filter).await;
            assert_eq!(
                (status, &page["Count"], &page["ScannedCount"]),
                (200, &json!(row.selects), &json!(ITEMS)),
                "case {:?} ({filter:?}): expected a 200 page selecting {} of {ITEMS} read, got \
                 {status} with {page}",
                row.case,
                row.selects
            );
            let items = page["Items"].as_array().map(Vec::len).unwrap_or_default() as u64;
            assert_eq!(
                items, row.selects,
                "case {:?} ({filter:?}): Items must carry the {} selected items, got {page}",
                row.case, row.selects
            );
        }
    }
}
