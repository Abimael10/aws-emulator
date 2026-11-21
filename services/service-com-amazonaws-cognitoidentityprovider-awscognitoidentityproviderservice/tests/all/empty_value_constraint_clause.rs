// SPDX-License-Identifier: CC0-1.0

//! What a constraint refusal of this service states about a member the request submitted
//! EMPTY, over the wire, driven exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: **a clause quotes the value the request carried only when that
//! value has text — a member submitted as the empty string has none to quote, so its clause
//! reads `Value at '<member>' failed to satisfy constraint: <requirement>` with no `''`
//! between the two halves — and the `@length` FLOOR such a value falls short of is stated
//! before the `@pattern` expression it does not match.** Everything else is unchanged: the
//! envelope, the member path and the requirement are the ones every other clause carries, a
//! value WITH text is quoted and keeps the expression-first order
//! (`clause_order_states_the_expression_first.rs`), and an ABSENT member keeps the
//! `Value null at …` an absence is stated with.
//!
//! The rows span what the rule generalises over: the member and the shape it targets
//! (`Username` -> `UsernameType`, `UserPoolId` -> `UserPoolIdType`, `ClientId` ->
//! `ClientIdType`, `GroupName` -> `GroupNameType`, `SecretHash` -> `SecretHashType`,
//! `Password` -> `PasswordType`, `TermsName` -> `TermsNameType`, an attribute's `Name` ->
//! `AttributeNameType`, from `smithy_query(query="shape",shape="UsernameType,UserPoolIdType,\
//! ClientIdType,GroupNameType,SecretHashType,PasswordType,TermsNameType,AttributeNameType")`),
//! how many requirements the empty value breaks (a floor AND an expression, an expression
//! alone where the model declares no floor above zero), whether the model marks the member
//! `@sensitive`, where the member sits (a top-level member and a member inside a list
//! element), and the operation carrying it.
//!
//! The harness is the one `log_delivery_configuration.rs` builds for this service, reached
//! through the test crate root rather than copied.

use serde_json::{json, Value};

use super::log_delivery_configuration::{call, error_code, error_message, ok, pool, router};

/// The error code every refusal of a bad parameter of this service carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A `UserPoolId` inside `UserPoolIdType`'s `@length` 1..55 and its
/// `^[\w-]+_[0-9a-zA-Z]+$`, carried by every row whose refused member is not the pool id.
const WELL_FORMED_POOL_ID: &str = "us-east-1_examplepool";

/// A `ClientId` inside `ClientIdType`'s `@length` 1..128 and its `^[\w+]+$`.
const WELL_FORMED_CLIENT_ID: &str = "1h57kf5cpq17m0eml12EXAMPLE";

/// A `Username` inside `UsernameType`'s rules.
const WELL_FORMED_USERNAME: &str = "jane.doe";

/// A password a default pool's policy accepts, for the row that is served.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

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

/// The clause a member with NO text to quote earns: the member and the requirement, and no
/// value at all.
fn valueless(path: &str, requirement: &str) -> String {
    format!("Value at '{path}' failed to satisfy constraint: {requirement}")
}

/// The clause a member whose value HAS text earns: that value, the member, the requirement.
fn quoted(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement half of a `@pattern` clause: the expression as this service quotes one.
fn must_match(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The requirement half of a length MINIMUM clause.
fn at_least(bound: usize) -> String {
    format!("Member must have length greater than or equal to {bound}")
}

/// The requirement half of a length MAXIMUM clause.
fn at_most(bound: usize) -> String {
    format!("Member must have length less than or equal to {bound}")
}

/// One row: a request carrying an empty value, and the whole message its refusal states.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    expected: String,
}

#[tokio::test]
async fn an_empty_value_is_stated_with_no_value_and_its_floor_first() {
    let router = router();

    // The expressions, as this service quotes one: the modelled text without the
    // whole-value anchors, except for a terms document's own members, which are quoted as
    // the model writes them.
    let username_expression = r"[\p{L}\p{M}\p{S}\p{N}\p{P}]+";
    let pool_id_expression = r"[\w-]+_[0-9a-zA-Z]+";
    let client_id_expression = r"[\w+]+";
    let secret_hash_expression = r"[\w+=/]+";
    let password_expression = r"[\S]+";
    let terms_name_expression = "^(terms-of-use|privacy-policy)$";
    let attribute_name_expression = r"[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r ]+";

    let rows = vec![
        Row {
            case: "an empty username on a sign-up",
            operation: "SignUp",
            body: json!({ "ClientId": WELL_FORMED_CLIENT_ID, "Username": "" }),
            expected: validation_errors(&[
                valueless("username", &at_least(1)),
                valueless("username", &must_match(username_expression)),
            ]),
        },
        Row {
            case: "an empty pool id on a listing",
            operation: "ListUserPoolClients",
            body: json!({ "UserPoolId": "" }),
            expected: validation_errors(&[
                valueless("userPoolId", &at_least(1)),
                valueless("userPoolId", &must_match(pool_id_expression)),
            ]),
        },
        Row {
            case: "an empty client id beside a well-formed pool id",
            operation: "DescribeUserPoolClient",
            body: json!({ "UserPoolId": WELL_FORMED_POOL_ID, "ClientId": "" }),
            expected: validation_errors(&[
                valueless("clientId", &at_least(1)),
                valueless("clientId", &must_match(client_id_expression)),
            ]),
        },
        Row {
            case: "an empty group name on a read of one group",
            operation: "GetGroup",
            body: json!({ "UserPoolId": WELL_FORMED_POOL_ID, "GroupName": "" }),
            expected: validation_errors(&[
                valueless("groupName", &at_least(1)),
                valueless("groupName", &must_match(username_expression)),
            ]),
        },
        Row {
            case: "an empty secret hash beside well-formed required members",
            operation: "SignUp",
            body: json!({
                "ClientId": WELL_FORMED_CLIENT_ID,
                "Username": WELL_FORMED_USERNAME,
                "SecretHash": "",
            }),
            expected: validation_errors(&[
                valueless("secretHash", &at_least(1)),
                valueless("secretHash", &must_match(secret_hash_expression)),
            ]),
        },
        Row {
            case: "an empty password, whose declared floor is zero, so only its expression \
                   is broken",
            operation: "SignUp",
            body: json!({
                "ClientId": WELL_FORMED_CLIENT_ID,
                "Username": WELL_FORMED_USERNAME,
                "Password": "",
            }),
            expected: one_validation_error(&valueless(
                "password",
                &must_match(password_expression),
            )),
        },
        Row {
            case: "an empty terms name, a member the model declares no length for at all",
            operation: "DescribeTermsByClient",
            body: json!({
                "UserPoolId": WELL_FORMED_POOL_ID,
                "ClientId": WELL_FORMED_CLIENT_ID,
                "TermsName": "",
            }),
            expected: one_validation_error(&valueless(
                "termsName",
                &must_match(terms_name_expression),
            )),
        },
        Row {
            case: "an empty attribute name inside a list element",
            operation: "SignUp",
            body: json!({
                "ClientId": WELL_FORMED_CLIENT_ID,
                "Username": WELL_FORMED_USERNAME,
                "UserAttributes": [{ "Name": "", "Value": "jane@example.test" }],
            }),
            expected: validation_errors(&[
                valueless("userAttributes.1.member.name", &at_least(1)),
                valueless(
                    "userAttributes.1.member.name",
                    &must_match(attribute_name_expression),
                ),
            ]),
        },
    ];

    for row in rows {
        let (status, payload) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, INVALID_PARAMETER),
            "case {:?}: expected a 400 {INVALID_PARAMETER}, got {status} with {payload}",
            row.case
        );
        let message = error_message(&payload);
        assert_eq!(
            message, row.expected,
            "case {:?}: expected {:?}, got {message:?}",
            row.case, row.expected
        );
        assert!(
            !message.contains("''"),
            "case {:?}: an empty value is quoted nowhere in the report, got {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_value_with_text_keeps_the_half_it_quotes_and_the_order_it_is_stated_in() {
    let router = router();

    // The boundary of the rule: what it does NOT change. A value with text is quoted
    // however short it is, including one under its member's floor, and a member breaking
    // both its expression and a bound still states the expression first.
    let short_session = "abcde";
    let over_long_username = format!("jane doe {}", "u".repeat(120));
    let username_expression = r"[\p{L}\p{M}\p{S}\p{N}\p{P}]+";

    let rows = vec![
        Row {
            case: "a session of five characters, under SessionType's floor of 20",
            operation: "RespondToAuthChallenge",
            body: json!({
                "ClientId": WELL_FORMED_CLIENT_ID,
                "ChallengeName": "SMS_MFA",
                "Session": short_session,
            }),
            expected: one_validation_error(&quoted(short_session, "session", &at_least(20))),
        },
        Row {
            case: "an over-long username carrying a space, breaking its expression and its \
                   ceiling",
            operation: "AdminGetUser",
            body: json!({
                "UserPoolId": WELL_FORMED_POOL_ID,
                "Username": over_long_username.clone(),
            }),
            expected: validation_errors(&[
                quoted(
                    &over_long_username,
                    "username",
                    &must_match(username_expression),
                ),
                quoted(&over_long_username, "username", &at_most(128)),
            ]),
        },
        Row {
            case: "a member the request left out, which has no value and is no empty one",
            operation: "DescribeUserPoolClient",
            body: json!({ "UserPoolId": WELL_FORMED_POOL_ID }),
            expected: one_validation_error(
                "Value null at 'clientId' failed to satisfy constraint: Member must not be null",
            ),
        },
    ];

    for row in rows {
        let (status, payload) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, INVALID_PARAMETER),
            "case {:?}: expected a 400 {INVALID_PARAMETER}, got {status} with {payload}",
            row.case
        );
        let message = error_message(&payload);
        assert_eq!(
            message, row.expected,
            "case {:?}: expected {:?}, got {message:?}",
            row.case, row.expected
        );
    }
}

#[tokio::test]
async fn a_request_whose_values_all_have_text_is_served() {
    let router = router();
    let user_pool_id = pool(&router, "a pool the served sign-up registers into", None).await;
    let created = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "empty-value-client",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = created["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted client id: {created}"))
        .to_string();

    // The refusals above answer a broken rule and not every request: the same operation,
    // with a value for each member, registers the user and reports the subject it minted.
    let registered = ok(
        &router,
        "SignUp",
        &json!({
            "ClientId": client_id,
            "Username": WELL_FORMED_USERNAME,
            "Password": GOOD_PASSWORD,
        }),
    )
    .await;
    assert!(
        registered["UserSub"].as_str().is_some_and(|sub| !sub.is_empty()),
        "the sign-up reports the subject it minted, got {registered}"
    );
}
