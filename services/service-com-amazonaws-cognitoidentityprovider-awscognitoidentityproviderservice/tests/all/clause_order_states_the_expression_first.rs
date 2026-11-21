// SPDX-License-Identifier: CC0-1.0

//! The ORDER the clauses of one constraint refusal of this service are stated in, over the
//! wire.
//!
//! The rule: a refusal that carries more than one clause about the SAME member states the
//! clause about the expression that member's value has to match (`@pattern`) before the
//! clauses about any bound on it (`@length`) — the form of the value first, then the
//! measurements of it. Which clauses are stated, which member each names, which value each
//! quotes and how each requirement is worded are unchanged: only the order is this rule's.
//!
//! The rows carry a value with TEXT. A member the request submitted EMPTY is stated the
//! other way round, and with no value at all, which is its own recorded refusal and its own
//! suite (`empty_value_constraint_clause.rs`).
//!
//! The rows span what the rule generalises over: the member and the shape it targets
//! (`UserPoolId` -> `UserPoolIdType`, `ClientId` -> `ClientIdType`, `Username` ->
//! `UsernameType`, `GroupName` -> `GroupNameType`, from
//! `smithy_query(query="shape",shape="UserPoolIdType,ClientIdType,UsernameType,GroupNameType")`),
//! the operation carrying the member, and where the member sits in the request (the member
//! the request is addressed by, and a member beside a well-formed pool id).
//!
//! The boundary table is the other half: a refusal carrying ONE clause is stated exactly as
//! it was, and a request breaking two DIFFERENT members' rules still carries a clause for
//! each.
//!
//! The harness is the one `log_delivery_configuration.rs` builds for this service, reached
//! through the test crate root rather than copied.

use serde_json::{json, Value};

use super::log_delivery_configuration::{call, error_code, error_message, ok, pool, router};

/// The error code every refusal of a bad parameter of this service carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A `UserPoolId` inside `UserPoolIdType`'s `@length` 1..55 and its `^[\w-]+_[0-9a-zA-Z]+$`,
/// carried by every row whose refused member is NOT the pool id.
const WELL_FORMED_POOL_ID: &str = "us-east-1_examplepool";

/// The envelope the framework states one broken constraint in.
fn one_validation_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The envelope the framework states SEVERAL broken constraints in: the count it announces,
/// and the clauses joined the way it joins them, in the order this endpoint states them.
fn validation_errors(clauses: &[String]) -> String {
    format!(
        "{} validation errors detected: {}",
        clauses.len(),
        clauses.join("; ")
    )
}

/// The clause a refused member earns: the value the request carried, the member, the
/// requirement.
fn clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement half of a `@pattern` clause: the expression as this service quotes one,
/// which is the modelled text without the whole-value anchors.
fn must_match(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The requirement half of a length MAXIMUM clause.
fn at_most(bound: usize) -> String {
    format!("Member must have length less than or equal to {bound}")
}

/// One row: a request whose refusal carries two clauses about one member, and the two
/// requirements it breaks — the expression first.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The member path the two clauses name.
    path: &'static str,
    /// The value both clauses quote.
    value: String,
    /// The expression the value does not match.
    expression: &'static str,
    /// The bound clause that follows it.
    bound: String,
}

#[tokio::test]
async fn a_members_expression_clause_precedes_the_bound_it_also_broke() {
    let router = router();

    // A pool id shaped like a pool's ARN: outside `UserPoolIdType`'s expression (an ARN
    // carries colons and a slash) and past its 55-character maximum.
    let arn_shaped =
        "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_examplepool".to_string();
    // An id one character past `UserPoolIdType`'s maximum that also carries a character the
    // expression admits nowhere.
    let over_long_pool_id = format!("us-east-1_{}!", "p".repeat(45));
    // A client id past `ClientIdType`'s 128-character maximum, carrying spaces its
    // expression admits nowhere.
    let over_long_client_id = format!("not a client id {}", "c".repeat(113));
    // A username past `UsernameType`'s 128-character maximum, carrying a space.
    let over_long_username = format!("jane doe {}", "u".repeat(120));
    // A group name past `GroupNameType`'s 128-character maximum, carrying a space.
    let over_long_group_name = format!("the group {}", "g".repeat(120));

    let pool_id_expression = r"[\w-]+_[0-9a-zA-Z]+";
    let client_id_expression = r"[\w+]+";
    let username_expression = r"[\p{L}\p{M}\p{S}\p{N}\p{P}]+";

    let rows = vec![
        Row {
            case: "a pool id shaped like an ARN, on a read of the pool",
            operation: "DescribeUserPool",
            body: json!({ "UserPoolId": arn_shaped }),
            path: "userPoolId",
            value: arn_shaped.clone(),
            expression: pool_id_expression,
            bound: at_most(55),
        },
        Row {
            case: "a pool id one character past its maximum, on a read of the import header",
            operation: "GetCSVHeader",
            body: json!({ "UserPoolId": over_long_pool_id }),
            path: "userPoolId",
            value: over_long_pool_id.clone(),
            expression: pool_id_expression,
            bound: at_most(55),
        },
        Row {
            case: "an over-long client id beside a well-formed pool id",
            operation: "DescribeUserPoolClient",
            body: json!({
                "UserPoolId": WELL_FORMED_POOL_ID,
                "ClientId": over_long_client_id,
            }),
            path: "clientId",
            value: over_long_client_id.clone(),
            expression: client_id_expression,
            bound: at_most(128),
        },
        Row {
            case: "an over-long username on an administrative read",
            operation: "AdminGetUser",
            body: json!({
                "UserPoolId": WELL_FORMED_POOL_ID,
                "Username": over_long_username,
            }),
            path: "username",
            value: over_long_username.clone(),
            expression: username_expression,
            bound: at_most(128),
        },
        Row {
            case: "an over-long group name on a read of one group",
            operation: "GetGroup",
            body: json!({
                "UserPoolId": WELL_FORMED_POOL_ID,
                "GroupName": over_long_group_name,
            }),
            path: "groupName",
            value: over_long_group_name.clone(),
            expression: username_expression,
            bound: at_most(128),
        },
    ];

    for row in rows {
        let expected = validation_errors(&[
            clause(&row.value, row.path, &must_match(row.expression)),
            clause(&row.value, row.path, &row.bound),
        ]);
        let (status, payload) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, INVALID_PARAMETER),
            "case {:?}: expected a 400 {INVALID_PARAMETER}, got {status} with {payload}",
            row.case
        );
        let message = error_message(&payload);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn one_broken_rule_is_stated_exactly_as_the_walk_reported_it() {
    let router = router();

    // Each of these breaks ONE rule, so there is nothing to order: the clause, its member
    // and its requirement are the ones the framework's own walk wrote.
    let over_long_but_well_formed = format!("us-east-1_{}", "p".repeat(46));
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a pool id outside its expression and inside its bounds",
            "DescribeUserPool",
            json!({ "UserPoolId": "no-underscore-here" }),
            one_validation_error(&clause(
                "no-underscore-here",
                "userPoolId",
                &must_match(r"[\w-]+_[0-9a-zA-Z]+"),
            )),
        ),
        (
            "a pool id past its maximum that still matches its expression",
            "DescribeUserPool",
            json!({ "UserPoolId": over_long_but_well_formed.clone() }),
            one_validation_error(&clause(
                &over_long_but_well_formed,
                "userPoolId",
                &at_most(55),
            )),
        ),
        (
            "a member the request left out, which has no value to quote",
            "DescribeUserPoolClient",
            json!({ "UserPoolId": WELL_FORMED_POOL_ID }),
            one_validation_error(
                "Value null at 'clientId' failed to satisfy constraint: Member must not be null",
            ),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, payload) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, INVALID_PARAMETER),
            "case {case:?}: expected a 400 {INVALID_PARAMETER}, got {status} with {payload}"
        );
        let message = error_message(&payload);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn two_members_each_breaking_one_rule_still_earn_a_clause_each() {
    let router = router();

    // The ordering is WITHIN one member: a request that breaks a rule of each of two
    // members is refused for both, and neither clause is dropped or doubled.
    let (status, payload) = call(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": "no-underscore-here", "Username": "two words" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (400, INVALID_PARAMETER),
        "expected a 400 {INVALID_PARAMETER}, got {status} with {payload}"
    );
    let message = error_message(&payload);
    assert!(
        message.starts_with("2 validation errors detected: "),
        "the envelope counts one clause per broken member, got {message:?}"
    );
    for (case, stated) in [
        (
            "the pool id's expression",
            clause(
                "no-underscore-here",
                "userPoolId",
                &must_match(r"[\w-]+_[0-9a-zA-Z]+"),
            ),
        ),
        (
            "the username's expression",
            clause(
                "two words",
                "username",
                &must_match(r"[\p{L}\p{M}\p{S}\p{N}\p{P}]+"),
            ),
        ),
    ] {
        assert_eq!(
            message.matches(stated.as_str()).count(),
            1,
            "case {case:?}: the report states {stated:?} exactly once, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_request_breaking_no_rule_is_still_served() {
    let router = router();
    let user_pool_id = pool(&router, "a pool the well-formed read reaches", None).await;

    // The refusals above are the answer to a broken rule and not to every request: the
    // same operation, asked with a pool id inside both of the member's rules, reports the
    // pool.
    let described = ok(
        &router,
        "DescribeUserPool",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    assert_eq!(
        described["UserPool"]["Id"], json!(user_pool_id),
        "the read reports the pool it was asked about, got {described}"
    );
}
