// SPDX-License-Identifier: CC0-1.0

//! Whether the expression a `@pattern` clause of this service QUOTES agrees with the refusal
//! it accompanies — the half of the clause rule that cannot be read off the sentence alone.
//!
//! `all/patch_group_pattern_clause.rs` and `all/constraint_set_refusal_wording.rs` pin the
//! TEXT a clause states for a shape. This suite pins what that text has to MEAN: the quoted
//! expression, compiled and applied as the match the service states it as, admits none of the
//! values the service refused for it, and admits every value the same operation serves. A
//! clause quoting a form the rejected value satisfies — a run of admitted characters recovered
//! out of a whole-value expression, say — reads as a requirement the refused request already
//! met, so the sentence would contradict its own refusal while every word of it still matched.
//!
//! The rows span the shapes this service publishes as a whole-value match, each reached
//! through an operation that carries it, with the offending character varied over its class
//! and its position in the value (leading, interior, trailing, the whole value), plus the two
//! clause forms this service states — one quoting the value the request carried and one
//! stating the member alone.
//!
//! Expressions, from the model unless noted
//! (`smithy_query(query="shape",shape="PatchGroup,OpsItemId,InventoryItemTypeName")`):
//! the chosen-name text `PatchGroup` and `DefaultInstanceName` share, an OpsItem id, and an
//! inventory type name. `StartSession`'s own `target` parameter is held to an expression the
//! model declares for no shape (`SessionTarget` carries a length band alone), stated by this
//! service in `service::refusal::stated_constraints`.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.

use regex::Regex;
use serde_json::{json, Value};

use crate::parameter_store::{call, error_code, error_message, router};

/// The run of characters a name a caller chooses is held to, as the model declares it for
/// `PatchGroup`, which `DefaultInstanceName` carries too.
const CHOSEN_NAME_EXPRESSION: &str = r"^([\p{L}\p{Z}\p{N}_.:/=+\-@]*)$";

/// The form an OpsItem id has, as the model declares it for `OpsItemId`.
const OPS_ITEM_ID_EXPRESSION: &str = "^(oi)-[0-9a-f]{12}$";

/// The form an inventory type name has, as the model declares it for
/// `InventoryItemTypeName`.
const INVENTORY_TYPE_NAME_EXPRESSION: &str = "^(AWS|Custom):.*$";

/// The characters the target of a session start is held to, which this service states for
/// that parameter (`service::refusal::stated_constraints`).
const SESSION_TARGET_EXPRESSION: &str = "^[a-zA-Z0-9_:-]{1,400}$";

/// A managed node id every constraint of the node-id set admits, so a row about another
/// member's expression is refused for that member alone.
const A_NODE: &str = "i-0abcdef0123456789";

/// Where a clause's expression starts, for reading back the text the service quoted.
const PATTERN_REQUIREMENT: &str = "Member must satisfy regular expression pattern: ";

/// One clause, in the form that quotes the value the request carried.
fn clause_quoting_the_value(value: &str, path: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy \
         constraint: {PATTERN_REQUIREMENT}{expression}"
    )
}

/// One clause, in the form that states the member alone.
fn clause_stating_the_member(path: &str, expression: &str) -> String {
    format!(
        "1 validation error detected: Value at '{path}' failed to satisfy constraint: \
         {PATTERN_REQUIREMENT}{expression}"
    )
}

/// The expression a clause quoted, read back out of the sentence.
fn quoted_expression(case: &str, message: &str) -> String {
    let (_, quoted) = message
        .split_once(PATTERN_REQUIREMENT)
        .unwrap_or_else(|| panic!("case {case:?}: expected a pattern clause, got {message:?}"));
    quoted.to_string()
}

/// Refuse `body` under `operation` and answer the message, asserting the status and the code
/// a broken constraint of this service is refused with.
async fn refusal(case: &str, operation: &str, body: &Value) -> String {
    let router = router();
    let (status, response) = call(&router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a value outside its expression is refused with 400, got {status} \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case {case:?}: the refusal is a ValidationException, got {response}"
    );
    error_message(&response)
}

/// One row: the request that is refused, and the clause it is refused with.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The operation the value travels to.
    operation: &'static str,
    /// The member the value travels in.
    member: &'static str,
    /// The path a clause names that member at.
    path: &'static str,
    /// The value put outside the expression.
    value: &'static str,
    /// The expression this service publishes for the member's shape.
    expression: &'static str,
    /// Whether this operation's clause quotes the value it refused.
    quotes_the_value: bool,
    /// The rest of the request, so nothing but the expression is broken.
    rest: fn() -> Value,
}

/// The rows: every shape this service publishes as a whole-value match that an operation of
/// it carries, with the offending character varied over its class and its position.
fn rows() -> Vec<Row> {
    let nothing: fn() -> Value = || json!({});
    vec![
        Row {
            case: "a patch group with a number sign inside it",
            operation: "DescribePatchGroupState",
            member: "PatchGroup",
            path: "patchGroup",
            value: "patchgroup#bad",
            expression: CHOSEN_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: nothing,
        },
        Row {
            case: "a patch group opening with a dollar sign",
            operation: "DescribePatchGroupState",
            member: "PatchGroup",
            path: "patchGroup",
            value: "$patchgroup",
            expression: CHOSEN_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: nothing,
        },
        Row {
            case: "a patch group closing with a percent sign",
            operation: "DescribeInstancePatchStatesForPatchGroup",
            member: "PatchGroup",
            path: "patchGroup",
            value: "patchgroup%",
            expression: CHOSEN_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: nothing,
        },
        Row {
            case: "a patch group that is one disallowed character",
            operation: "GetPatchBaselineForPatchGroup",
            member: "PatchGroup",
            path: "patchGroup",
            value: "*",
            expression: CHOSEN_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: || json!({ "OperatingSystem": "AMAZON_LINUX_2" }),
        },
        Row {
            case: "a default node name with a comma in it",
            operation: "CreateActivation",
            member: "DefaultInstanceName",
            path: "defaultInstanceName",
            value: "west,east",
            expression: CHOSEN_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: || json!({ "IamRole": "SSMServiceRole" }),
        },
        Row {
            case: "an OpsItem id whose twelve characters are not hexadecimal",
            operation: "GetOpsItem",
            member: "OpsItemId",
            path: "opsItemId",
            value: "oi-zzzzzzzzzzzz",
            expression: OPS_ITEM_ID_EXPRESSION,
            quotes_the_value: true,
            rest: nothing,
        },
        Row {
            case: "an OpsItem id carrying a suffix past the twelve",
            operation: "GetOpsItem",
            member: "OpsItemId",
            path: "opsItemId",
            value: "oi-0123456789ab-1",
            expression: OPS_ITEM_ID_EXPRESSION,
            quotes_the_value: true,
            rest: nothing,
        },
        Row {
            case: "an inventory type name under neither published prefix",
            operation: "ListInventoryEntries",
            member: "TypeName",
            path: "typeName",
            value: "Application",
            expression: INVENTORY_TYPE_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: || json!({ "InstanceId": A_NODE }),
        },
        Row {
            case: "an inventory type name whose prefix is only a prefix of the value",
            operation: "ListInventoryEntries",
            member: "TypeName",
            path: "typeName",
            value: "my:AWS:Application",
            expression: INVENTORY_TYPE_NAME_EXPRESSION,
            quotes_the_value: true,
            rest: || json!({ "InstanceId": A_NODE }),
        },
        Row {
            // The session start states the member alone, with nothing of the value echoed,
            // so the rule holds across both clause forms this service writes.
            case: "a session target with a number sign inside it",
            operation: "StartSession",
            member: "Target",
            path: "target",
            value: "i-0abcdef0123456789#1",
            expression: SESSION_TARGET_EXPRESSION,
            quotes_the_value: false,
            rest: nothing,
        },
    ]
}

/// THE RULE: the expression a clause quotes, applied as the service states it, admits NONE of
/// the values that clause refused — so the sentence states the match the value was refused
/// for instead of a form the value already satisfies.
#[tokio::test]
async fn a_pattern_clause_quotes_an_expression_the_refused_value_does_not_satisfy() {
    for row in rows() {
        let Row {
            case,
            operation,
            member,
            path,
            value,
            expression,
            quotes_the_value,
            rest,
        } = row;

        let mut body = rest();
        body[member] = json!(value);
        let stated = refusal(case, operation, &body).await;

        let expected = if quotes_the_value {
            clause_quoting_the_value(value, path, expression)
        } else {
            clause_stating_the_member(path, expression)
        };
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );

        let quoted = quoted_expression(case, &stated);
        let compiled = Regex::new(&quoted).unwrap_or_else(|error| {
            panic!("case {case:?}: the quoted expression {quoted:?} must compile, got {error}")
        });
        assert!(
            !compiled.is_match(value),
            "case {case:?}: the clause's expression {quoted:?} must not admit the value it \
             refused, but it matched {value:?}"
        );
    }
}

/// THE OTHER DIRECTION: the expression the service QUOTED admits every value the same
/// operation SERVES, so a clause states no requirement the service does not hold a value to.
///
/// The expression is read back out of a refusal of that very operation rather than written
/// out here, so both halves of this test are about what the service stated.
#[tokio::test]
async fn a_pattern_clause_quotes_an_expression_the_served_values_satisfy() {
    let router = router();
    let case = "the clause a disallowed character is refused with";
    let stated = refusal(
        case,
        "DescribePatchGroupState",
        &json!({ "PatchGroup": "patchgroup#bad" }),
    )
    .await;
    let quoted = quoted_expression(case, &stated);
    let compiled = Regex::new(&quoted).unwrap_or_else(|error| {
        panic!("case {case:?}: the quoted expression {quoted:?} must compile, got {error}")
    });

    // Each class the expression names: the listed punctuation, a separator, letters outside
    // ASCII and digits.
    let served: &[(&str, &str)] = &[
        ("the listed punctuation marks", "a_b.c:d/e=f+g-h@i"),
        ("a separator between words", "web servers"),
        ("letters outside the Latin script", "パッチグループ"),
        ("digits alone", "2024"),
    ];

    for (case, name) in served {
        let (status, response) = call(
            &router,
            "DescribePatchGroupState",
            &json!({ "PatchGroup": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a name the published expression admits is served, got {status} \
             {response}"
        );
        assert!(
            compiled.is_match(name),
            "case {case:?}: the expression the clause quoted ({quoted:?}) must admit the name \
             the read served, but it did not match {name:?}"
        );
    }
}
