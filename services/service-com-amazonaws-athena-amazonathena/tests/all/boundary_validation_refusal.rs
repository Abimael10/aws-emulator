// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal of a request that breaks a MODELLED input constraint.
//!
//! Athena's `InvalidRequestException` is the shape for a request whose input is wrong —
//! "a required parameter may be missing or out of range" — and the model binds it to every
//! operation of the service, so that is the shape such a request is refused with: HTTP 400,
//! `__type` `InvalidRequestException`, the class `INVALID_INPUT` under both of the names the
//! wire gives a refusal's code (`AthenaErrorCode` and `ErrorCode`), and AWS's validation
//! sentence under `Message`.
//!
//! The sentence is `N validation error(s) detected: ` over one clause per broken constraint,
//! each naming the member as the API spells it and the requirement it broke. A clause states
//! no value — except for a member that was not there at all, which is stated as
//! `Value null at '<member>'`.
//!
//! The rule is asserted over a TABLE rather than one recorded request: the rows differ along
//! the operation refused, the constraint CLASS (a member absent, a value outside a `@range`,
//! a collection outside its `@length`, a string outside its `@length`, a string outside its
//! `@pattern`), the member, and the depth of the member (a top-level one and one nested in a
//! structure). The published bounds themselves are a table of their own: each bound and one
//! step past it, so the refusal is seen to start exactly where the model says it does.
//!
//! Every bound and pattern below is the model's, read with
//! `smithy_query(query="operation",operation="ListTableMetadata,GetTableMetadata,BatchGetQueryExecution,StartQueryExecution,GetPreparedStatement,BatchGetNamedQuery")`
//! and `smithy_query(query="shape",shape="QueryExecutionIdList,NamedQueryIdList,StatementName,MaxTableMetadataCount,ExpressionString,QueryExecutionContext")`.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::ERROR_CODE_INVALID_INPUT;

use crate::harness::{call, error_code, message, router};

/// The one envelope AWS states a single broken constraint in.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// A list of `count` well-formed ids, each satisfying the element constraints of
/// `QueryExecutionIdList` / `NamedQueryIdList` (1 to 128 characters, no whitespace), so the
/// only constraint a row can break is the LIST's own `@length`.
fn ids(count: usize) -> Value {
    Value::Array((0..count).map(|n| json!(format!("id-{n}"))).collect())
}

/// Assert one refusal: HTTP 400, this service's refusal shape, the class under both names,
/// and `expected` word for word.
fn assert_invalid_input(case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(body),
        "InvalidRequestException",
        "case {case:?}: expected InvalidRequestException, got {body:?}"
    );
    for member in ["AthenaErrorCode", "ErrorCode"] {
        assert_eq!(
            body[member].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected {member} {ERROR_CODE_INVALID_INPUT:?}, got {body:?}"
        );
    }
    assert_eq!(
        body["Message"].as_str(),
        Some(expected),
        "case {case:?}: expected the message {expected:?}, got {:?}",
        message(body)
    );
}

/// Every class of broken input constraint is refused as this service's own
/// `InvalidRequestException`, stating the member and the requirement.
#[tokio::test]
async fn a_broken_input_constraint_is_refused_with_the_class_and_awss_sentence() {
    let router = router();
    // `StatementName` is 1 to 256 characters and must match
    // `^[a-zA-Z_][a-zA-Z0-9_@:]{1,256}$`, so a name of 257 admitted characters satisfies the
    // pattern and breaks the length alone.
    let overlong_statement = format!("s{}", "x".repeat(256));
    // `ExpressionString` is 0 to 256 characters.
    let overlong_expression = "e".repeat(257);

    // (case, operation, request, the clause the refusal states)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a page size above its range",
            "ListTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db", "MaxResults": 51 }),
            "Value at 'maxResults' failed to satisfy constraint: \
             Member must have value less than or equal to 50"
                .to_string(),
        ),
        (
            "a page size below its range",
            "ListTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db", "MaxResults": 0 }),
            "Value at 'maxResults' failed to satisfy constraint: \
             Member must have value greater than or equal to 1"
                .to_string(),
        ),
        (
            "a string longer than its length",
            "ListTableMetadata",
            json!({
                "CatalogName": "AwsDataCatalog",
                "DatabaseName": "db",
                "Expression": overlong_expression,
            }),
            "Value at 'expression' failed to satisfy constraint: \
             Member must have length less than or equal to 256"
                .to_string(),
        ),
        (
            "a required member absent",
            "GetTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "TableName": "t" }),
            "Value null at 'databaseName' failed to satisfy constraint: \
             Member must not be null"
                .to_string(),
        ),
        (
            "another required member of the same operation absent",
            "GetTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db" }),
            "Value null at 'tableName' failed to satisfy constraint: Member must not be null"
                .to_string(),
        ),
        (
            "a required member absent from the submission path",
            "StartQueryExecution",
            json!({ "ResultConfiguration": { "OutputLocation": "s3://results/" } }),
            "Value null at 'queryString' failed to satisfy constraint: Member must not be null"
                .to_string(),
        ),
        (
            "a member nested in a structure, below its length",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "QueryExecutionContext": { "Database": "" },
                "ResultConfiguration": { "OutputLocation": "s3://results/" },
            }),
            "Value at 'queryExecutionContext.database' failed to satisfy constraint: \
             Member must have length greater than or equal to 1"
                .to_string(),
        ),
        (
            "a token shorter than its length",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "ClientRequestToken": "too-short",
                "ResultConfiguration": { "OutputLocation": "s3://results/" },
            }),
            "Value at 'clientRequestToken' failed to satisfy constraint: \
             Member must have length greater than or equal to 32"
                .to_string(),
        ),
        (
            "a batch of more ids than the list admits",
            "BatchGetQueryExecution",
            json!({ "QueryExecutionIds": ids(51) }),
            "Value at 'queryExecutionIds' failed to satisfy constraint: \
             Member must have length less than or equal to 50"
                .to_string(),
        ),
        (
            "a batch of no ids at all",
            "BatchGetQueryExecution",
            json!({ "QueryExecutionIds": [] }),
            "Value at 'queryExecutionIds' failed to satisfy constraint: \
             Member must have length greater than or equal to 1"
                .to_string(),
        ),
        (
            "a batch whose id list is absent",
            "BatchGetQueryExecution",
            json!({}),
            "Value null at 'queryExecutionIds' failed to satisfy constraint: \
             Member must not be null"
                .to_string(),
        ),
        (
            "a named-query batch of more ids than the list admits",
            "BatchGetNamedQuery",
            json!({ "NamedQueryIds": ids(51) }),
            "Value at 'namedQueryIds' failed to satisfy constraint: \
             Member must have length less than or equal to 50"
                .to_string(),
        ),
        (
            "a name outside its pattern",
            "GetPreparedStatement",
            json!({ "StatementName": "1bad-name", "WorkGroup": "primary" }),
            "Value at 'statementName' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: [a-zA-Z_][a-zA-Z0-9_@:]{1,256}"
                .to_string(),
        ),
        (
            "a name of one character, which that pattern's own minimum excludes",
            "GetPreparedStatement",
            json!({ "StatementName": "s", "WorkGroup": "primary" }),
            "Value at 'statementName' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: [a-zA-Z_][a-zA-Z0-9_@:]{1,256}"
                .to_string(),
        ),
        (
            "a name longer than its length",
            "GetPreparedStatement",
            json!({ "StatementName": overlong_statement, "WorkGroup": "primary" }),
            "Value at 'statementName' failed to satisfy constraint: \
             Member must have length less than or equal to 256"
                .to_string(),
        ),
        (
            "a workgroup name outside its pattern",
            "GetPreparedStatement",
            json!({ "StatementName": "stmt_one", "WorkGroup": "not a work group" }),
            "Value at 'workGroup' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: [a-zA-Z0-9._-]{1,128}"
                .to_string(),
        ),
        (
            "a value outside an enumerated type, nested two structures deep",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "ResultConfiguration": {
                    "OutputLocation": "s3://results/",
                    "EncryptionConfiguration": { "EncryptionOption": "SSE_S4" },
                },
            }),
            // `EncryptionOption` admits SSE_S3, SSE_KMS and CSE_KMS
            // (`smithy_query(query="shape",shape="EncryptionOption")`), listed out of the
            // front door's own table over those values rather than in declaration order.
            "Value at 'resultConfiguration.encryptionConfiguration.encryptionOption' failed \
             to satisfy constraint: Member must satisfy enum value set: \
             [CSE_KMS, SSE_KMS, SSE_S3]"
                .to_string(),
        ),
        (
            "a value outside an enumerated type whose table order is its declared one",
            "UpdateWorkGroup",
            json!({ "WorkGroup": "primary", "State": "SUSPENDED" }),
            // `WorkGroupState` admits ENABLED and DISABLED
            // (`smithy_query(query="shape",shape="WorkGroupState")`).
            "Value at 'state' failed to satisfy constraint: \
             Member must satisfy enum value set: [ENABLED, DISABLED]"
                .to_string(),
        ),
    ];

    for (case, operation, request, clause) in rows {
        let (status, body) = call(&router, operation, &request).await;
        assert_invalid_input(case, status, &body, &one_error(&clause));
    }
}

/// A request that breaks TWO constraints is refused with one clause per constraint, under
/// the envelope's own count.
#[tokio::test]
async fn two_broken_constraints_are_stated_as_two_clauses() {
    let router = router();
    let (status, body) = call(
        &router,
        "ListTableMetadata",
        &json!({
            "CatalogName": "AwsDataCatalog",
            "DatabaseName": "db",
            "MaxResults": 51,
            "Expression": "e".repeat(257),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected HTTP 400, got {status} with {body:?}"
    );
    assert_eq!(
        body["AthenaErrorCode"].as_str(),
        Some(ERROR_CODE_INVALID_INPUT),
        "expected the input class, got {body:?}"
    );
    let stated = message(&body);
    assert!(
        stated.starts_with("2 validation errors detected: "),
        "expected the envelope to count both clauses, got {stated:?}"
    );
    for member in ["'maxResults'", "'expression'"] {
        assert!(
            stated.contains(member),
            "expected {member} among the clauses, got {stated:?}"
        );
    }
}

/// A value AT a published bound is not refused as invalid input: the refusal starts one step
/// past the bound, not at it.
#[tokio::test]
async fn a_value_at_a_published_bound_is_not_refused_as_invalid_input() {
    let router = router();
    // (case, operation, request) — each value is a bound the model publishes:
    // `MaxTableMetadataCount` is 1 to 50, both id lists are 1 to 50 elements, `StatementName`
    // is 1 to 256 characters, `ExpressionString` is 0 to 256, and `IdempotencyToken` is 32 to
    // 128.
    let longest_statement = format!("s{}", "x".repeat(255));
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "the smallest page size",
            "ListTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db", "MaxResults": 1 }),
        ),
        (
            "the largest page size",
            "ListTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db", "MaxResults": 50 }),
        ),
        (
            "the longest expression",
            "ListTableMetadata",
            json!({
                "CatalogName": "AwsDataCatalog",
                "DatabaseName": "db",
                "Expression": "e".repeat(256),
            }),
        ),
        (
            "the shortest expression",
            "ListTableMetadata",
            json!({ "CatalogName": "AwsDataCatalog", "DatabaseName": "db", "Expression": "" }),
        ),
        (
            "a batch of one id",
            "BatchGetQueryExecution",
            json!({ "QueryExecutionIds": ids(1) }),
        ),
        (
            "a batch of the most ids the list admits",
            "BatchGetQueryExecution",
            json!({ "QueryExecutionIds": ids(50) }),
        ),
        (
            "a named-query batch of the most ids the list admits",
            "BatchGetNamedQuery",
            json!({ "NamedQueryIds": ids(50) }),
        ),
        (
            "the longest statement name",
            "GetPreparedStatement",
            json!({ "StatementName": longest_statement, "WorkGroup": "primary" }),
        ),
        (
            "the shortest token",
            "StartQueryExecution",
            json!({
                "QueryString": "SELECT 1",
                "ClientRequestToken": "t".repeat(32),
                "ResultConfiguration": { "OutputLocation": "s3://results/" },
            }),
        ),
    ];

    for (case, operation, request) in rows {
        let (status, body) = call(&router, operation, &request).await;
        // Whatever the call then answers — served, refused for a reason of its own (a
        // resource this account does not hold, a rule this service enforces), or the 501 of
        // an operation still scaffolded — it is not refused for the input constraint the
        // value is inside, which the three members of that refusal state together.
        let refused_as_invalid_input = error_code(&body) == "InvalidRequestException"
            && body["AthenaErrorCode"].as_str() == Some(ERROR_CODE_INVALID_INPUT)
            && message(&body).contains("validation error");
        assert!(
            !refused_as_invalid_input,
            "case {case:?}: a value at a published bound must not be refused as invalid \
             input, got {status} with {body:?}"
        );
    }
}
