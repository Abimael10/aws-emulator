// SPDX-License-Identifier: CC0-1.0

//! What this service answers for three request defects whose diagnosis is the SERVICE's
//! sentence rather than the position a reader stopped at.
//!
//! Three rules are under test, each over a TABLE of inputs rather than one request:
//!
//! 1. a document path may name at most as many levels as a stored attribute may nest (32),
//!    and a path naming more is refused with the quota's own sentence, which reports the
//!    depth the REQUEST wrote rather than the limit — one wording for every expression
//!    member a path can be written in;
//! 2. an `AttributeValue` carrying no datatype, or more than one, is refused behind the
//!    sentence of the member it stands in where that member states one: a resume token is the
//!    invalid token it is, and a substitution table carries an invalid value under the key the
//!    value sits at;
//! 3. a key condition compares one key attribute against a VALUE, so an operand position
//!    carrying another attribute NAME states a condition between two attributes, which the
//!    member expresses no form of and names as its own defect.
//!
//! The rows differ along what each rule generalises over: the depth and the KIND of the steps
//! a path is built from, the expression member and the operation carrying it, the datatype
//! defect and the position in the value it sits at, the key the substitution table holds it
//! under, and the comparison form the two names were written in. Everything is driven over
//! the wire, so nothing is asserted that a client could not observe.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row below is addressed to, keyed on a partition key and a sort key so a
/// key condition has both halves to name.
const TABLE: &str = "Diagnosed";

/// How deeply a stored attribute may nest, which is the longest document path a request may
/// name: "DynamoDB supports nested attributes up to 32 levels deep" ("Service, Account, and
/// Table Quotas").
const MAX_LEVELS: usize = 32;

/// AWS's fixed text for an `AttributeValue` that carries no datatype at all.
const EMPTY_VALUE: &str =
    "Supplied AttributeValue is empty, must contain exactly one of the supported datatypes";

/// AWS's fixed text for one that carries more than one.
const AMBIGUOUS_VALUE: &str = "Supplied AttributeValue has more than one datatypes set, must \
                               contain exactly one of the supported datatypes";

/// Call `operation` and report the refusal's message, requiring a 400 `ValidationException`.
async fn refusal(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> String {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case {case:?} ({operation}): expected 400 ValidationException, got {status} with {body}"
    );
    message(&body)
}

/// A document path of `levels` levels, built from `step`s after the first: `"a"` names one
/// level, and every step after it adds one.
fn path_of(levels: usize, step: Step) -> String {
    let mut path = String::from("a");
    for index in 1..levels {
        match step {
            Step::Attribute => path.push_str(&format!(".l{index}")),
            Step::Index => path.push_str(&format!("[{index}]")),
        }
    }
    path
}

/// The two kinds of step a document path is built from.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// `.name`, which reaches into a map.
    Attribute,
    /// `[index]`, which reaches into a list.
    Index,
}

#[tokio::test]
async fn a_document_path_reports_the_nesting_levels_it_named() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "one"}, "sk": {"S": "one"}}}),
    )
    .await;

    // Every expression member a document path can be written in, as the request that carries
    // one: the member, the operation, a request builder taking the path, and whether a path
    // INSIDE the quota is served by that request. The projection members are read by three
    // operations, the write members by one each, so the rows span both planes of the
    // expression language. An UPDATE's path is the one that is not served inside the quota: a
    // path through a parent the item does not hold is a different refusal of its own
    // (`crate::state::INVALID_DOCUMENT_PATH_MESSAGE`), which is not this rule.
    type Carrier = (&'static str, &'static str, fn(&str) -> Value, bool);
    let carriers: Vec<Carrier> = vec![
        (
            "ProjectionExpression",
            "GetItem",
            |path| {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}, "sk": {"S": "one"}},
                       "ProjectionExpression": path})
            },
            true,
        ),
        (
            "ProjectionExpression",
            "Scan",
            |path| json!({"TableName": TABLE, "ProjectionExpression": path}),
            true,
        ),
        (
            "ProjectionExpression",
            "Query",
            |path| {
                json!({"TableName": TABLE, "ProjectionExpression": path,
                       "KeyConditionExpression": "pk = :p",
                       "ExpressionAttributeValues": {":p": {"S": "one"}}})
            },
            true,
        ),
        (
            "FilterExpression",
            "Scan",
            |path| {
                json!({"TableName": TABLE,
                       "FilterExpression": format!("attribute_exists({path})")})
            },
            true,
        ),
        (
            "ConditionExpression",
            "PutItem",
            |path| {
                json!({"TableName": TABLE, "Item": {"pk": {"S": "two"}, "sk": {"S": "two"}},
                       "ConditionExpression": format!("attribute_not_exists({path})")})
            },
            true,
        ),
        (
            "UpdateExpression",
            "UpdateItem",
            |path| {
                json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}, "sk": {"S": "one"}},
                       "UpdateExpression": format!("REMOVE {path}")})
            },
            false,
        ),
    ];

    for (member, operation, build, served_inside_the_quota) in &carriers {
        // The published bound and one level below it are inside the quota, whichever kind of
        // step the path is built from, so each of them is SERVED rather than refused.
        if *served_inside_the_quota {
            for levels in [1, MAX_LEVELS - 1, MAX_LEVELS] {
                for step in [Step::Attribute, Step::Index] {
                    let path = path_of(levels, step);
                    let case =
                        format!("{member} on {operation}: {levels} levels of {step:?} steps");
                    let (status, body) = call(&router, operation, build(&path)).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: a path inside the quota must be served, got {status} \
                         with {body}"
                    );
                }
            }
        }
        // One level past it, and well past it, are refused with the counted depth. The
        // envelope follows the member, exactly as every other refusal about that member's
        // expression does: the two members a write states its intent through carry it.
        for levels in [MAX_LEVELS + 1, MAX_LEVELS + 8] {
            for step in [Step::Attribute, Step::Index] {
                let path = path_of(levels, step);
                let case = format!("{member} on {operation}: {levels} levels of {step:?} steps");
                let stated = refusal(&router, &case, operation, build(&path)).await;
                let envelope = if ["UpdateExpression", "ConditionExpression"].contains(member) {
                    "1 validation error detected: "
                } else {
                    ""
                };
                let expected = format!(
                    "{envelope}Invalid {member}: The document path has too many nesting levels; \
                     nesting levels: {levels}"
                );
                assert_eq!(
                    stated, expected,
                    "case {case:?}: expected {expected:?}, got {stated:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_unreadable_attribute_value_is_stated_behind_its_members_own_sentence() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;

    // A resume token carrying a value with no datatype is no key of the schema being paged:
    // the whole token is invalid, with the datatype fact as the reason and no entry of it
    // named. The rows span the two table walks that take a token, the two defects, and the
    // position in the value the defect sits at.
    let tokens: Vec<(&str, Value, &str)> = vec![
        ("an empty value", json!({"pk": {}}), EMPTY_VALUE),
        (
            "an empty value beside a well-formed one",
            json!({"pk": {"S": "one"}, "sk": {}}),
            EMPTY_VALUE,
        ),
        (
            "an empty value inside a map",
            json!({"pk": {"M": {"inner": {}}}}),
            EMPTY_VALUE,
        ),
        (
            "an empty value inside a list",
            json!({"pk": {"L": [{"S": "ok"}, {}]}}),
            EMPTY_VALUE,
        ),
        (
            "a value carrying two datatypes",
            json!({"pk": {"S": "one", "N": "1"}}),
            AMBIGUOUS_VALUE,
        ),
    ];
    for (case, start_key, defect) in tokens {
        for (operation, rest) in [
            ("Scan", json!({})),
            (
                "Query",
                json!({"KeyConditionExpression": "pk = :p",
                       "ExpressionAttributeValues": {":p": {"S": "one"}}}),
            ),
        ] {
            let mut request = rest;
            request["TableName"] = json!(TABLE);
            request["ExclusiveStartKey"] = start_key.clone();
            let case = format!("{case} in a resume token, on {operation}");
            let stated = refusal(&router, &case, operation, request).await;
            let expected = format!("The provided starting key is invalid: {defect}");
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    // A substitution table carries an invalid VALUE, and the refusal says which key it sits
    // under. The rows span the operations that read that table, the two defects, the position
    // in the value, and the key itself — which is echoed as the request spelled it.
    let substitutions: Vec<(&str, &str, Value, &str)> = vec![
        ("an empty value", ":v", json!({}), EMPTY_VALUE),
        (
            "an empty value under another key",
            ":other",
            json!({}),
            EMPTY_VALUE,
        ),
        (
            "an empty value inside a map",
            ":v",
            json!({"M": {"inner": {}}}),
            EMPTY_VALUE,
        ),
        (
            "an empty value inside a list",
            ":v",
            json!({"L": [{"N": "1"}, {}]}),
            EMPTY_VALUE,
        ),
        (
            "a value carrying two datatypes",
            ":v",
            json!({"S": "one", "BOOL": true}),
            AMBIGUOUS_VALUE,
        ),
    ];
    for (case, key, value, defect) in substitutions {
        let values = Value::Object(
            [(key.to_string(), value)]
                .into_iter()
                .collect::<serde_json::Map<String, Value>>(),
        );
        let requests: Vec<(&str, Value)> = vec![
            (
                "PutItem",
                json!({"TableName": TABLE,
                       "Item": {"pk": {"S": "one"}, "sk": {"S": "one"}},
                       "ConditionExpression": format!("v = {key}"),
                       "ExpressionAttributeValues": values.clone()}),
            ),
            (
                "UpdateItem",
                json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}, "sk": {"S": "one"}},
                       "UpdateExpression": format!("SET v = {key}"),
                       "ExpressionAttributeValues": values.clone()}),
            ),
            (
                "DeleteItem",
                json!({"TableName": TABLE, "Key": {"pk": {"S": "one"}, "sk": {"S": "one"}},
                       "ConditionExpression": format!("v = {key}"),
                       "ExpressionAttributeValues": values.clone()}),
            ),
            (
                "Scan",
                json!({"TableName": TABLE, "FilterExpression": format!("v = {key}"),
                       "ExpressionAttributeValues": values.clone()}),
            ),
            (
                "Query",
                json!({"TableName": TABLE, "KeyConditionExpression": format!("pk = {key}"),
                       "ExpressionAttributeValues": values.clone()}),
            ),
        ];
        for (operation, request) in requests {
            let case = format!("{case} under {key:?}, on {operation}");
            let stated = refusal(&router, &case, operation, request).await;
            let expected = format!(
                "ExpressionAttributeValues contains invalid value: {defect} for key {key}"
            );
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_key_condition_compares_one_attribute_against_a_value() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S"), ("sk", "RANGE", "S")]).await;

    // Each row: the case and the condition. They span the comparison forms a key condition
    // has — equality, an ordering comparison, `BETWEEN`'s two bounds and `begins_with`'s
    // prefix — and the two ways a name is written: out, and behind an `ExpressionAttributeNames`
    // alias.
    let conditions: Vec<(&str, &str)> = vec![
        ("equality between two names", "pk = sk"),
        ("an ordering comparison between two names", "pk = :p AND sk > pk"),
        ("a name as the lower bound of a BETWEEN", "pk = :p AND sk BETWEEN pk AND :hi"),
        ("a name as the upper bound of a BETWEEN", "pk = :p AND sk BETWEEN :lo AND pk"),
        ("a name as a begins_with prefix", "pk = :p AND begins_with(sk, pk)"),
        ("two aliased names", "#p = #s"),
        ("an aliased name as the operand", "pk = :p AND #s = #p"),
    ];
    let expected = "Invalid KeyConditionExpression: Invalid condition in KeyConditionExpression: \
                    Multiple attribute names used in one condition";
    for (case, condition) in conditions {
        let stated = refusal(
            &router,
            case,
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": condition,
                "ExpressionAttributeNames": {"#p": "pk", "#s": "sk"},
                "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"},
                                              ":hi": {"S": "z"}},
            }),
        )
        .await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // The neighbouring defect, so the two are told apart rather than collapsed: an operand
    // that is itself an OPERATION earns the member's nested-operation sentence, whether the
    // operation is written as this grammar's own `begins_with` or as a name applied to
    // arguments.
    let nested = "Invalid KeyConditionExpression: KeyConditionExpressions cannot contain nested \
                  operations";
    for (case, condition) in [
        ("begins_with in an operand position", "pk = begins_with(sk, :lo)"),
        ("the same as a BETWEEN bound", "pk = :p AND sk BETWEEN begins_with(sk, :lo) AND :hi"),
        ("a function written as a name applied to arguments", "pk = :p AND sk > size(sk)"),
    ] {
        let stated = refusal(
            &router,
            case,
            "Query",
            json!({
                "TableName": TABLE,
                "KeyConditionExpression": condition,
                "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"},
                                              ":hi": {"S": "z"}},
            }),
        )
        .await;
        assert_eq!(
            stated, nested,
            "case {case:?}: expected {nested:?}, got {stated:?}"
        );
    }

    // The boundary: the same forms with a VALUE in the operand position are the conditions
    // the member does take, and each is served. Every row carries exactly the substitutions
    // its condition writes, because a table entry no expression uses is a defect of its own.
    let served: Vec<(&str, Value)> = vec![
        (
            "equality on the partition key",
            json!({"KeyConditionExpression": "pk = :p",
                   "ExpressionAttributeValues": {":p": {"S": "one"}}}),
        ),
        (
            "an ordering comparison on the sort key",
            json!({"KeyConditionExpression": "pk = :p AND sk > :lo",
                   "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"}}}),
        ),
        (
            "a BETWEEN over the sort key",
            json!({"KeyConditionExpression": "pk = :p AND sk BETWEEN :lo AND :hi",
                   "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"},
                                                 ":hi": {"S": "z"}}}),
        ),
        (
            "a begins_with over the sort key",
            json!({"KeyConditionExpression": "pk = :p AND begins_with(sk, :lo)",
                   "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"}}}),
        ),
        (
            "the same, written through aliases",
            json!({"KeyConditionExpression": "#p = :p AND #s > :lo",
                   "ExpressionAttributeNames": {"#p": "pk", "#s": "sk"},
                   "ExpressionAttributeValues": {":p": {"S": "one"}, ":lo": {"S": "a"}}}),
        ),
    ];
    for (case, rest) in served {
        let mut request = rest;
        request["TableName"] = json!(TABLE);
        let (status, body) = call(&router, "Query", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a condition over a value must be served, got {status} with {body}"
        );
    }
}
