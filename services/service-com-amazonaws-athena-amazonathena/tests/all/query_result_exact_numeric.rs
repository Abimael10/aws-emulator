// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TYPE a projected column reports and the VALUE its cells carry,
//! for the expressions whose type is a property of the expression itself.
//!
//! The rules under test, from `com.amazonaws.athena#ColumnInfo` (`Type`, `Precision`,
//! `Scale` — "meaningful for DECIMAL data types") and from Athena's own data types:
//!
//! * a projected column's TYPE is the one its own expression has by Athena's signature for
//!   it, never the storage class the query engine happened to hold the value in — a
//!   boolean-valued expression is `boolean` and its cells are `true`/`false`, and a cast
//!   reports the type it names;
//! * a value of the EXACT-NUMERIC type `DECIMAL(precision, scale)` — a number written with a
//!   decimal point, or a cast to that type — carries exactly the digits it has, reported at
//!   that scale: no rounding beyond it, no digits dropped and never an exponent rendering,
//!   whatever a binary double could hold. A value that does not FIT the declaration at that
//!   scale is no value of the type, so the statement fails rather than reporting digits the
//!   column's own metadata contradicts.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the form of
//! the expression (a literal, a comparison, a connective, a test, a cast), the type cast to,
//! the number of digits and the scale, the sign, and the forms the rule does NOT cover (a
//! literal written with an exponent, which is an approximate double).
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, router, settled, start};

/// Where the queries in these tests write their results. Nothing is read back out of it.
const OUTPUT_LOCATION: &str = "s3://athena-results/exact-numeric/";

/// The database every statement of a suite runs in, so one suite's writes are the ones its
/// own statements see.
const CONTEXT_DATABASE: &str = "exact_numeric";

/// The table the cast-of-a-column rows read, and the row in it.
const SETUP: &[&str] = &[
    "CREATE TABLE amounts (id integer, price double, label varchar)",
    "INSERT INTO amounts VALUES (1, 10.5, 'desk')",
];

/// A `StartQueryExecution` body for `sql`.
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": CONTEXT_DATABASE },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// Run `sql` to completion and assert it SUCCEEDED, answering its execution id.
async fn ran(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let id = start(router, &query(sql)).await;
    let found = settled(router, &id).await;
    assert_eq!(
        found["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the statement {sql:?} must succeed, got {found:?}"
    );
    id
}

/// The first column's metadata and the first data row's value in it, as the read reports
/// them.
async fn first_column(router: &MultiServiceRouter, case: &str, id: &str) -> (Value, Value) {
    let (status, body) = call(
        router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body:?}"
    );
    let column = body["ResultSet"]["ResultSetMetadata"]["ColumnInfo"][0].clone();
    assert!(
        column.is_object(),
        "case {case:?}: expected one column's metadata, got {body:?}"
    );
    // A DML statement's results are read with the column labels in front of the data rows,
    // so the value under test is in the SECOND row.
    let value = body["ResultSet"]["Rows"][1]["Data"][0]["VarCharValue"].clone();
    (column, value)
}

/// One row of the typing table.
struct Case {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `ColumnInfo$Type` its one column must report.
    column_type: &'static str,
    /// The `ColumnInfo$Precision` that column must report.
    precision: i64,
    /// The `ColumnInfo$Scale` that column must report.
    scale: i64,
    /// The value of that column in the first data row.
    value: &'static str,
}

/// The rows of the BOOLEAN table: every expression whose value is a truth value is typed
/// `boolean` and its cells carry Athena's own `true`/`false`, whatever the engine holds the
/// answer as — it has no boolean type and answers 1 and 0.
///
/// The precision beside the type is that type's own width, which for a boolean is none.
fn boolean_cases() -> Vec<Case> {
    vec![
        Case {
            case: "a boolean literal",
            sql: "SELECT true AS flag",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "true",
        },
        Case {
            case: "the other boolean literal",
            sql: "SELECT false AS flag",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "false",
        },
        Case {
            case: "an equality comparison",
            sql: "SELECT 1 = 1 AS same",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "true",
        },
        Case {
            case: "an ordering comparison that does not hold",
            sql: "SELECT 2 < 1 AS ordered",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "false",
        },
        Case {
            case: "a logical connective",
            sql: "SELECT true AND false AS both",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "false",
        },
        Case {
            case: "a negation",
            sql: "SELECT NOT true AS negated",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "false",
        },
        Case {
            case: "a null test",
            sql: "SELECT NULL IS NULL AS absent",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "true",
        },
        Case {
            case: "a comparison over a column of a table",
            sql: "SELECT price > 10 AS dear FROM amounts",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "true",
        },
        Case {
            case: "a cast to the boolean type",
            sql: "SELECT CAST(1 AS BOOLEAN) AS flag",
            column_type: "boolean",
            precision: 0,
            scale: 0,
            value: "true",
        },
        Case {
            // A cast reports the type it names, so a one-digit value cast to the 64-bit
            // integer type is that type and not the narrower one its value would fit.
            case: "a cast to the 64-bit integer type",
            sql: "SELECT CAST(1 AS BIGINT) AS big",
            column_type: "bigint",
            precision: 19,
            scale: 0,
            value: "1",
        },
        Case {
            case: "a cast to the character type",
            sql: "SELECT CAST(42 AS VARCHAR) AS written",
            column_type: "varchar",
            precision: 2_147_483_647,
            scale: 0,
            value: "42",
        },
        Case {
            case: "a cast to the approximate-numeric type",
            sql: "SELECT CAST(1 AS DOUBLE) AS approximate",
            column_type: "double",
            precision: 17,
            scale: 0,
            value: "1.0",
        },
    ]
}

/// The rows of the EXACT-NUMERIC table: a decimal value keeps the digits it has, reported at
/// the scale its type carries.
///
/// The decimal literals span a value no double holds exactly, a short one, a negative one,
/// one below one and one whose digits would not fit the type at all — the last of which is an
/// approximate double in Athena's own reading, as is a literal written with an exponent.
fn decimal_cases() -> Vec<Case> {
    vec![
        Case {
            // The digits of this literal are more than a double's 17, so a double rendering
            // of it would lose them and write an exponent.
            case: "a decimal literal of more digits than a double holds",
            sql: "SELECT 123456789012345.123456789 AS n",
            column_type: "decimal",
            precision: 24,
            scale: 9,
            value: "123456789012345.123456789",
        },
        Case {
            case: "a cast of a string of 33 digits to the widest decimal",
            sql: "SELECT CAST('123456789012345678901234.123456789' AS DECIMAL(38,9)) AS dec_val",
            column_type: "decimal",
            precision: 38,
            scale: 9,
            value: "123456789012345678901234.123456789",
        },
        Case {
            case: "a cast of a string of 37 digits",
            sql: "SELECT CAST('1234567890123456789012345678.123456789' AS DECIMAL(38,9)) AS big",
            column_type: "decimal",
            precision: 38,
            scale: 9,
            value: "1234567890123456789012345678.123456789",
        },
        Case {
            case: "a short decimal literal",
            sql: "SELECT 2.5 AS d",
            column_type: "decimal",
            precision: 2,
            scale: 1,
            value: "2.5",
        },
        Case {
            case: "a negative decimal literal, with a trailing zero of its own",
            sql: "SELECT -0.0500 AS owed",
            column_type: "decimal",
            precision: 4,
            scale: 4,
            value: "-0.0500",
        },
        Case {
            // A value of fewer digits than the scale is reported AT the scale, which is how
            // a fixed-point type reports every value of it.
            case: "a cast whose scale is wider than the value's digits",
            sql: "SELECT CAST('1.5' AS DECIMAL(10,4)) AS padded",
            column_type: "decimal",
            precision: 10,
            scale: 4,
            value: "1.5000",
        },
        Case {
            // Rounded to the declared scale, half away from zero, which is how a cast to a
            // fixed-point type rounds.
            case: "a cast whose scale is narrower than the value's digits",
            sql: "SELECT CAST('1.235' AS DECIMAL(10,2)) AS rounded",
            column_type: "decimal",
            precision: 10,
            scale: 2,
            value: "1.24",
        },
        Case {
            case: "a cast naming a precision alone, which is a scale of none",
            sql: "SELECT CAST('2.5' AS DECIMAL(10)) AS whole",
            column_type: "decimal",
            precision: 10,
            scale: 0,
            value: "3",
        },
        Case {
            case: "a cast of a NEGATIVE string to the exact type",
            sql: "SELECT CAST('-12.3456' AS DECIMAL(20,2)) AS owed",
            column_type: "decimal",
            precision: 20,
            scale: 2,
            value: "-12.35",
        },
        Case {
            case: "a cast of a column of a table",
            sql: "SELECT CAST(price AS DECIMAL(12,3)) AS exact FROM amounts",
            column_type: "decimal",
            precision: 12,
            scale: 3,
            value: "10.500",
        },
        Case {
            case: "a cast of the other spelling of the type",
            sql: "SELECT CAST('7.125' AS NUMERIC(8,2)) AS exact",
            column_type: "decimal",
            precision: 8,
            scale: 2,
            value: "7.13",
        },
        Case {
            // The boundary of the declaration: three digits before the point is exactly
            // `precision - scale`, so this value fills the type and is reported.
            case: "a value that fills the declared precision exactly",
            sql: "SELECT CAST('123.45' AS DECIMAL(5,2)) AS filled",
            column_type: "decimal",
            precision: 5,
            scale: 2,
            value: "123.45",
        },
        Case {
            // NOT an exact decimal: a number written in scientific notation is a DOUBLE —
            // "numbers in scientific notation are of the DOUBLE type" (Athena's data-type
            // reference) — so this column reports the engine's own approximate type.
            case: "a literal written with an exponent",
            sql: "SELECT 1.5e3 AS approximate",
            column_type: "double",
            precision: 17,
            scale: 0,
            value: "1500.0",
        },
    ]
}

/// Run `cases` and assert the type, the precision, the scale and the value each row demands.
async fn assert_cases(router: &MultiServiceRouter, cases: &[Case]) {
    for case in cases {
        let name = case.case;
        let id = ran(router, name, case.sql).await;
        let (column, value) = first_column(router, name, &id).await;
        assert_eq!(
            column["Type"].as_str(),
            Some(case.column_type),
            "case {name:?}: expected Type {:?}, got {column:?}",
            case.column_type
        );
        assert_eq!(
            column["Precision"].as_i64(),
            Some(case.precision),
            "case {name:?}: expected Precision {}, got {column:?}",
            case.precision
        );
        assert_eq!(
            column["Scale"].as_i64(),
            Some(case.scale),
            "case {name:?}: expected Scale {}, got {column:?}",
            case.scale
        );
        assert_eq!(
            value.as_str(),
            Some(case.value),
            "case {name:?}: expected the value {:?}, got {value:?}",
            case.value
        );
    }
}

/// Seed the table the column rows read.
async fn seed(router: &MultiServiceRouter) {
    for sql in SETUP {
        ran(router, "setup", sql).await;
    }
}

/// Every expression whose value is a truth value reports the boolean type and Athena's own
/// `true`/`false`, and a cast reports the type it names.
#[tokio::test]
async fn an_expression_is_typed_by_its_own_signature_and_not_by_the_engines_storage_class() {
    let router = router();
    seed(&router).await;
    assert_cases(&router, &boolean_cases()).await;
}

/// An exact-numeric value keeps every digit it has, reported at the scale its type carries.
#[tokio::test]
async fn an_exact_decimal_keeps_its_digits_through_a_read_of_the_results() {
    let router = router();
    seed(&router).await;
    assert_cases(&router, &decimal_cases()).await;
}

/// One row of the refusal table: a statement whose cast cannot produce a value of the type
/// it declares, and what the execution's reason has to say.
struct RefusedCase {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// What the reason the failed execution reports must carry.
    reason: &'static str,
}

/// A value that does not FIT the declared `DECIMAL(precision, scale)` is no value of that
/// type, so the statement FAILS rather than reporting digits the column's own metadata
/// contradicts.
///
/// The rows differ along why it does not fit: more digits before the point than the
/// precision leaves for them, at a scale of none, after the rounding carried into another
/// digit, and a value past the digits this emulator reads at all.
#[tokio::test]
async fn a_value_that_does_not_fit_the_declared_decimal_fails_the_statement() {
    let router = router();
    let cases = vec![
        RefusedCase {
            case: "more digits before the point than the precision leaves for them",
            sql: "SELECT CAST('12345.6' AS DECIMAL(5,2)) AS over",
            reason: "Cannot cast VARCHAR '12345.6' to DECIMAL(5, 2)",
        },
        RefusedCase {
            case: "a whole number past a declaration that names no scale",
            sql: "SELECT CAST('100' AS DECIMAL(2)) AS over",
            reason: "Cannot cast VARCHAR '100' to DECIMAL(2, 0)",
        },
        RefusedCase {
            // The rounding comes FIRST: 9.996 at a scale of two is 10.00, which no longer
            // fits the one digit `decimal(3,2)` leaves before the point.
            case: "a value the rounding to the declared scale carries out of range",
            sql: "SELECT CAST('9.996' AS DECIMAL(3,2)) AS over",
            reason: "Cannot cast VARCHAR '9.996' to DECIMAL(3, 2)",
        },
        RefusedCase {
            // Thirteen characters naming a billion digits: refused as the bound this
            // emulator reads a decimal to, before anything is rescaled.
            case: "a short text naming more digits than this emulator reads",
            sql: "SELECT CAST('1E-1000000000' AS DECIMAL(38,9)) AS over",
            reason: "at most 1024 digits",
        },
    ];

    for case in &cases {
        let name = case.case;
        let id = start(&router, &query(case.sql)).await;
        let found = settled(&router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {name:?}: the statement {:?} must fail, got {found:?}",
            case.sql
        );
        let reason = found["Status"]["StateChangeReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            reason.contains(case.reason),
            "case {name:?}: expected a reason carrying {:?}, got {reason:?}",
            case.reason
        );
    }
}
