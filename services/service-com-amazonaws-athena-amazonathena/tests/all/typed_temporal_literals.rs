// SPDX-License-Identifier: CC0-1.0

//! The TYPED LITERAL a statement writes a date or a time as.
//!
//! The rule under test, from Athena's data types (`date`, `time(p)`, `timestamp(p)`) and its
//! SQL, which is Trino's: a value of a date/time type is written as the TYPE NAME followed by
//! the text of the value — `DATE '2024-03-05'`, `TIME '06:07:08'`,
//! `TIMESTAMP '2024-03-05 06:07:08.123'` — and such a literal is an expression wherever an
//! expression may stand. It denotes the value its text names on the calendar: the statement
//! SUCCEEDS, the column reports the type the literal names (`com.amazonaws.athena#ColumnInfo`
//! `Type`), and its cells carry that value rendered zero-padded at exactly the fractional
//! digits the literal was written with, which is that literal's own precision. Text that
//! names no value of the type is no literal of it, so the execution FAILS carrying Athena's
//! invalid-literal class.
//!
//! Beside it, the rule a results object is written under: a present value is quoted, with an
//! embedded quote doubled, and SQL NULL is an empty unquoted field — so a reader of the saved
//! CSV tells a NULL from the empty string.
//!
//! Each suite asserts its rule over a TABLE whose rows differ along what the rule generalises
//! over: the type the literal names, the separator and the padding its text is written with,
//! how many fractional digits it carries (including the published bound and one past it),
//! where in the statement the literal stands (the select list, inside a cast, a predicate),
//! and the texts that name no value at all.
//!
//! Athena and Amazon S3 are mounted on ONE shared engine and driven over the wire, through
//! the harness the results-object suite already builds, so what is asserted is what an SDK
//! client observes of both services.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};

use crate::harness::{call, settled};
use crate::query_result_objects::{create_bucket, emulator, s3_served, start, succeeded};

/// The bucket these suites write their results into — their own, so the keys read back here
/// are only theirs.
const BUCKET: &str = "athena-temporal-literals";

/// The prefix under it the results objects land at.
const PREFIX: &str = "typed";

/// The database the statements of these suites run in, so they see each other's writes and
/// another suite's do not.
const DATABASE: &str = "temporal_literals";

/// The table the predicate rows read, and the days in it.
const SETUP: &[&str] = &[
    "CREATE TABLE bookings (id integer, day varchar)",
    "INSERT INTO bookings VALUES (1, '2024-03-04'), (2, '2024-03-05'), (3, '2024-03-06')",
];

/// The `StartQueryExecution` request for `sql`, writing its results into this suite's own
/// prefix.
fn request(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "QueryExecutionContext": { "Database": DATABASE },
        "ResultConfiguration": { "OutputLocation": format!("s3://{BUCKET}/{PREFIX}/") },
    })
}

/// Run the setup statements in order, failing the suite if any of them did not succeed.
async fn seed(router: &MultiServiceRouter) {
    for sql in SETUP {
        let id = start(router, "setup", &request(sql)).await;
        let found = settled(router, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "setup: {sql:?} must succeed, got {found:?}"
        );
    }
}

/// The first column's metadata and the first data row's value in it, as `GetQueryResults`
/// reports them.
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

/// The lines of the results object `id` saved, as a reader of the saved CSV parses them.
async fn saved_lines(router: &MultiServiceRouter, case: &str, id: &str) -> Vec<String> {
    let key = format!("{PREFIX}/{id}.csv");
    let (status, _media_type, saved) = s3_served(router, "GET", &format!("/{BUCKET}/{key}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetObject(s3://{BUCKET}/{key}) must answer 200, got {status} with \
         {saved:?}"
    );
    saved.lines().map(str::to_string).collect()
}

/// One row of the typed-literal table: a statement projecting one such literal, and what its
/// one column must report.
struct Literal {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The `ColumnInfo$Type` its one column must report.
    column_type: &'static str,
    /// The `ColumnInfo$Precision` that column must report.
    precision: i64,
    /// The value of that column in the first data row.
    value: &'static str,
}

/// The rows: a typed literal of each of the three types, written with and without the zeros
/// and the seconds a text may leave out, with either ISO separator, with the fractional
/// digits that are the literal's own precision, and read through an expression over it.
fn literal_cases() -> Vec<Literal> {
    vec![
        Literal {
            case: "a date literal",
            sql: "SELECT DATE '2024-03-05' AS d",
            column_type: "date",
            precision: 0,
            value: "2024-03-05",
        },
        Literal {
            case: "a date literal written without its zeros",
            sql: "SELECT DATE '2024-3-5' AS d",
            column_type: "date",
            precision: 0,
            value: "2024-03-05",
        },
        Literal {
            case: "a date literal on a leap day",
            sql: "SELECT DATE '2024-02-29' AS d",
            column_type: "date",
            precision: 0,
            value: "2024-02-29",
        },
        Literal {
            case: "a time literal",
            sql: "SELECT TIME '06:07:08' AS t",
            column_type: "time",
            precision: 0,
            value: "06:07:08",
        },
        Literal {
            case: "a time literal carrying fractional digits",
            sql: "SELECT TIME '06:07:08.25' AS t",
            column_type: "time",
            precision: 0,
            value: "06:07:08.25",
        },
        Literal {
            case: "a timestamp literal",
            sql: "SELECT TIMESTAMP '2024-03-05 06:07:08.123' AS ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:08.123",
        },
        Literal {
            case: "the same statement written in lower case",
            sql: "select timestamp '2024-03-05 06:07:08.123' as ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:08.123",
        },
        Literal {
            case: "a timestamp literal written with the ISO 8601 separator",
            sql: "SELECT TIMESTAMP '2024-03-05T06:07:08' AS ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:08",
        },
        Literal {
            case: "a timestamp literal that leaves the seconds out",
            sql: "SELECT TIMESTAMP '2024-03-05 06:07' AS ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:00",
        },
        Literal {
            // The digits are the literal's own precision, so its trailing zero is kept.
            case: "a timestamp literal whose fractional digits end in a zero",
            sql: "SELECT TIMESTAMP '2024-03-05 06:07:08.1230' AS ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:08.1230",
        },
        Literal {
            // The published bound on a `timestamp(p)`: twelve fractional digits.
            case: "a timestamp literal at the widest precision the type carries",
            sql: "SELECT TIMESTAMP '2024-03-05 06:07:08.123456789012' AS ts",
            column_type: "timestamp",
            precision: 0,
            value: "2024-03-05 06:07:08.123456789012",
        },
        Literal {
            case: "a literal read through a cast, which reports the type it names",
            sql: "SELECT CAST(TIMESTAMP '2024-03-05 06:07:08' AS varchar) AS written",
            column_type: "varchar",
            precision: 2_147_483_647,
            value: "2024-03-05 06:07:08",
        },
    ]
}

/// A typed date/time literal is the value its text names: the statement succeeds, the column
/// reports the type the literal names, and the value is reported in both places a result set
/// is observable — the metadata and cells a read answers, and the saved results object.
#[tokio::test]
async fn a_typed_date_or_time_literal_is_the_value_its_text_names() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;

    // Every row's query is started before any of them is read back, so the span a completion
    // is owed after elapses once for the whole table instead of once per row.
    let cases = literal_cases();
    let mut started: Vec<(&Literal, String)> = Vec::new();
    for literal in &cases {
        started.push((literal, start(&router, literal.case, &request(literal.sql)).await));
    }

    for (literal, id) in &started {
        let case = literal.case;
        succeeded(&router, case, id).await;

        let (column, value) = first_column(&router, case, id).await;
        assert_eq!(
            column["Type"].as_str(),
            Some(literal.column_type),
            "case {case:?}: expected Type {:?}, got {column:?}",
            literal.column_type
        );
        assert_eq!(
            column["Precision"].as_i64(),
            Some(literal.precision),
            "case {case:?}: expected Precision {}, got {column:?}",
            literal.precision
        );
        assert_eq!(
            column["Scale"].as_i64(),
            Some(0),
            "case {case:?}: expected Scale 0, got {column:?}"
        );
        assert_eq!(
            value.as_str(),
            Some(literal.value),
            "case {case:?}: expected the value {:?}, got {value:?}",
            literal.value
        );

        // The same value, as a reader of the saved results object parses it.
        let lines = saved_lines(&router, case, id).await;
        let expected = vec![
            format!("\"{}\"", column["Name"].as_str().unwrap_or_default()),
            format!("\"{}\"", literal.value),
        ];
        assert_eq!(
            lines, expected,
            "case {case:?}: expected the saved CSV {expected:?}, got {lines:?}"
        );
    }
}

/// One row of the refusal table: a literal whose text names no value of the type it is
/// written at.
struct Refused {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// What the reason the failed execution reports must carry.
    reason: &'static str,
}

/// The rows: a day no calendar has, a month and an hour outside their range, a text of
/// another shape entirely, one carrying more fractional digits than the type holds, and the
/// text of a value of another of these types.
fn refused_cases() -> Vec<Refused> {
    vec![
        Refused {
            // 2023 is no leap year, so that day is on no calendar.
            case: "a day the calendar does not have",
            sql: "SELECT DATE '2023-02-29' AS d",
            reason: "INVALID_LITERAL: '2023-02-29' is not a valid date literal",
        },
        Refused {
            case: "a month outside the year",
            sql: "SELECT DATE '2024-13-01' AS d",
            reason: "INVALID_LITERAL: '2024-13-01' is not a valid date literal",
        },
        Refused {
            case: "a text that names no instant at all",
            sql: "SELECT TIMESTAMP 'yesterday' AS ts",
            reason: "INVALID_LITERAL: 'yesterday' is not a valid timestamp literal",
        },
        Refused {
            case: "an hour outside the day",
            sql: "SELECT TIME '24:00:00' AS t",
            reason: "INVALID_LITERAL: '24:00:00' is not a valid time literal",
        },
        Refused {
            // One digit past the widest precision a `timestamp(p)` carries.
            case: "more fractional digits than the type holds",
            sql: "SELECT TIMESTAMP '2024-03-05 06:07:08.1234567890123' AS ts",
            reason: "INVALID_LITERAL: '2024-03-05 06:07:08.1234567890123' is not a valid \
                     timestamp literal",
        },
        Refused {
            case: "the text of a day, written at the instant type",
            sql: "SELECT TIMESTAMP '2024-03-05' AS ts",
            reason: "INVALID_LITERAL: '2024-03-05' is not a valid timestamp literal",
        },
    ]
}

/// Text that names no value of the type its literal is written at is no literal of that type:
/// the execution FAILS carrying Athena's invalid-literal class, the text and the type.
#[tokio::test]
async fn a_literal_whose_text_names_no_value_of_its_type_fails_the_statement() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;

    let cases = refused_cases();
    let mut started: Vec<(&Refused, String)> = Vec::new();
    for refused in &cases {
        started.push((refused, start(&router, refused.case, &request(refused.sql)).await));
    }

    for (refused, id) in &started {
        let case = refused.case;
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: the statement {:?} must fail, got {found:?}",
            refused.sql
        );
        let reason = found["Status"]["StateChangeReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            reason.contains(refused.reason),
            "case {case:?}: expected a reason carrying {:?}, got {reason:?}",
            refused.reason
        );
    }
}

/// One row of the predicate table: a statement whose predicate compares against a typed
/// literal, and the rows that comparison holds for.
struct Selecting {
    /// What the row is about.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// The ids it must select, in the order the statement orders them.
    ids: &'static [&'static str],
}

/// The rows: the comparison forms a predicate is written with, and a literal whose text
/// leaves its zeros out.
fn selecting_cases() -> Vec<Selecting> {
    vec![
        Selecting {
            case: "a strict comparison against a date literal",
            sql: "SELECT id FROM bookings WHERE day > DATE '2024-03-05' ORDER BY id",
            ids: &["3"],
        },
        Selecting {
            case: "an equality against a date literal",
            sql: "SELECT id FROM bookings WHERE day = DATE '2024-03-05' ORDER BY id",
            ids: &["2"],
        },
        Selecting {
            case: "a range between two date literals",
            sql: "SELECT id FROM bookings WHERE day BETWEEN DATE '2024-03-04' AND \
                  DATE '2024-03-05' ORDER BY id",
            ids: &["1", "2"],
        },
        Selecting {
            case: "an equality against a literal the zeros are left out of",
            sql: "SELECT id FROM bookings WHERE day = DATE '2024-3-4' ORDER BY id",
            ids: &["1"],
        },
    ]
}

/// A typed literal stands wherever an expression may: one in a PREDICATE selects the rows the
/// comparison holds for, which is what says the reading is not a property of the select list.
#[tokio::test]
async fn a_typed_literal_in_a_predicate_selects_the_rows_it_holds_for() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    seed(&router).await;

    let cases = selecting_cases();
    let mut started: Vec<(&Selecting, String)> = Vec::new();
    for selecting in &cases {
        started.push((
            selecting,
            start(&router, selecting.case, &request(selecting.sql)).await,
        ));
    }

    for (selecting, id) in &started {
        let case = selecting.case;
        succeeded(&router, case, id).await;
        let lines = saved_lines(&router, case, id).await;
        let mut wanted = vec!["\"id\"".to_string()];
        wanted.extend(selecting.ids.iter().map(|id| format!("\"{id}\"")));
        assert_eq!(
            lines, wanted,
            "case {case:?}: {:?} must select {wanted:?}, got {lines:?}",
            selecting.sql
        );
    }
}

/// What a results object holds for a projection of texts and typed literals: a present value
/// quoted with its embedded quote doubled, SQL NULL as an empty unquoted field, and each
/// typed literal as the value its text names.
///
/// One statement, because the subject is the WHOLE record a reader parses: the fields either
/// side of a NULL are what says the NULL is a field of its own and not a missing one.
#[tokio::test]
async fn a_results_object_distinguishes_null_from_the_empty_string_beside_typed_literals() {
    let router = emulator();
    create_bucket(&router, BUCKET).await;

    let case = "a projection of texts, a NULL and typed literals";
    let sql = "SELECT 'plain' AS plain, 'has,comma' AS comma_val, 'has\"quote' AS quote_val, \
               CAST(NULL AS varchar) AS null_str, '' AS empty_str, \
               TIMESTAMP '2024-03-05 06:07:08.123' AS ts, DATE '2024-03-05' AS d, \
               CAST(1.05 AS DECIMAL(10,4)) AS dec_val";
    let id = start(&router, case, &request(sql)).await;
    succeeded(&router, case, &id).await;

    let lines = saved_lines(&router, case, &id).await;
    let expected = vec![
        "\"plain\",\"comma_val\",\"quote_val\",\"null_str\",\"empty_str\",\"ts\",\"d\",\
         \"dec_val\""
            .to_string(),
        "\"plain\",\"has,comma\",\"has\"\"quote\",,\"\",\"2024-03-05 06:07:08.123\",\
         \"2024-03-05\",\"1.0500\""
            .to_string(),
    ];
    assert_eq!(
        lines, expected,
        "case {case:?}: expected the saved CSV {expected:?}, got {lines:?}"
    );

    // The same record as the read reports it: the NULL is a datum with no value, which is
    // what the empty unquoted field above writes down, and the empty string is a value.
    let (status, body) = call(
        &router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body:?}"
    );
    let data = body["ResultSet"]["Rows"][1]["Data"].clone();
    assert_eq!(
        data[3],
        json!({}),
        "case {case:?}: the NULL must be a datum carrying no value, got {data:?}"
    );
    assert_eq!(
        data[4]["VarCharValue"].as_str(),
        Some(""),
        "case {case:?}: the empty string must be a value, got {data:?}"
    );
}
