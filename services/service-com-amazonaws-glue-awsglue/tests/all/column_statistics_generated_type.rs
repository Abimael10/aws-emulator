// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TYPE a generated `ColumnStatistics` entry is reported under:
//! `ColumnStatistics$ColumnType`, "the data type of the column", as
//! `GetColumnStatisticsForTable` answers it once a column statistics task run has computed
//! the statistics of a table's data.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the objects
//! the run reads are staged through Amazon S3's own `PutObject`, so what is asserted is HTTP
//! behaviour over state the data plane really holds. The harness that stages a table and
//! drives one run to its terminal status is the computation suite's
//! ([`crate::column_statistics_computation`]), so both suites stage a run the same way.
//!
//! The rule under test: a GENERATED entry reports the column's type as the name of the
//! statistics category its figures were computed under — the `ColumnStatisticsType` the
//! column's declared Hive type maps to, written in lower case — whatever alias, letter case
//! or parameter list the catalog's `Column$Type` declared it under, while a SUBMITTED entry
//! keeps the `ColumnType` the caller stated on it. So a reported entry's `ColumnType` and
//! its `StatisticsData$Type` always name the same category, and a client that keys off
//! either picks the same `ColumnStatisticsData` member.
//!
//! The recorded case — one column declared `bigint`, whose generated statistics report
//! `long` — is ONE row of the table below; the rows differ along the declared spelling
//! (every category, reached through an alias, in upper, mixed and padded spellings, with and
//! without a parameter list) and, in the second test, along where the entry came from
//! (computed by a run, stated by a caller, stated again after a run).
//!
//! The due instant a run's terminal status falls at is moved THROUGH THE STORE rather than
//! slept for ([`crate::column_statistics_task_runs::set_due_instant`]), so every transition
//! here is deterministic.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::COLUMN_STATISTICS_TASK_RUN_STATE_SUCCEEDED;

use crate::column_statistics_computation::{
    bucket_of, data_member, emulator_with_s3, get_run, get_statistics, number, stage_objects,
    start_run, statistics_by_column, store_table, text_descriptor,
};
use crate::column_statistics_task_runs::{already_past, set_due_instant};
use crate::wire::{ok, ACCOUNT_ID};

/// The Hive null marker a delimited-text table carries by default, written in a cell that
/// holds no value.
const NULL_MARKER: &str = "\\N";

/// One row of the spelling table: a column of a stored table, the `Column$Type` the table
/// declares it under, the statistics category that declaration maps to, and one cell of
/// data for it.
struct SpellingCase<'a> {
    case: &'a str,
    column: &'a str,
    /// `Column$Type`, the Hive type name the table declares — client-written text, so its
    /// letter case, its blanks and its parameter list are whatever the client sent.
    declared: &'a str,
    /// The `ColumnStatisticsType` the declaration maps to: the modelled enum
    /// (`smithy_query shape=ColumnStatisticsType`), whose member of `ColumnStatisticsData`
    /// carries the figures (`smithy_query shape=ColumnStatisticsData`).
    category: &'a str,
    /// The value this column holds in the one data row of the staged object.
    cell: &'a str,
}

/// Every statistics category, reached through declarations whose spelling is not the
/// category's own name: an alias, a letter case, a parameter list, surrounding blanks.
///
/// The declared names are the Hive type names a Data Catalog table states; which category
/// each maps to is the modelled `ColumnStatisticsType` whose data member describes that
/// kind of column (`smithy_query shape=ColumnStatisticsData`).
fn spelling_cases() -> Vec<SpellingCase<'static>> {
    vec![
        SpellingCase {
            case: "the recorded declaration, in upper case",
            column: "upper_big",
            declared: "BIGINT",
            category: "LONG",
            cell: "5",
        },
        SpellingCase {
            case: "an integral alias in mixed case",
            column: "mixed_int",
            declared: "Int",
            category: "LONG",
            cell: "3",
        },
        SpellingCase {
            case: "the narrowest integral alias, padded with blanks",
            column: "padded_tiny",
            declared: " tinyint ",
            category: "LONG",
            cell: "1",
        },
        SpellingCase {
            case: "a 16-bit integral alias in upper case",
            column: "upper_small",
            declared: "SMALLINT",
            category: "LONG",
            cell: "2",
        },
        SpellingCase {
            case: "a declaration that already spells its own category",
            column: "plain_long",
            declared: "long",
            category: "LONG",
            cell: "9",
        },
        SpellingCase {
            case: "a single-precision floating alias in upper case",
            column: "upper_float",
            declared: "FLOAT",
            category: "DOUBLE",
            cell: "1.5",
        },
        SpellingCase {
            case: "another floating alias in mixed case",
            column: "mixed_real",
            declared: "Real",
            category: "DOUBLE",
            cell: "2.5",
        },
        SpellingCase {
            case: "a fixed-point declaration with a parameter list and blanks",
            column: "padded_decimal",
            declared: " Decimal(12, 3) ",
            category: "DECIMAL",
            cell: "9.125",
        },
        SpellingCase {
            case: "a fixed-point alias in upper case with a parameter list",
            column: "upper_numeric",
            declared: "NUMERIC(5,1)",
            category: "DECIMAL",
            cell: "7.5",
        },
        SpellingCase {
            case: "a bounded character declaration in mixed case",
            column: "mixed_char",
            declared: "Char(5)",
            category: "STRING",
            cell: "abcde",
        },
        SpellingCase {
            case: "a bounded character alias in upper case",
            column: "upper_varchar",
            declared: "VARCHAR(10)",
            category: "STRING",
            cell: "hello",
        },
        SpellingCase {
            case: "an unbounded character declaration in mixed case",
            column: "mixed_string",
            declared: "String",
            category: "STRING",
            cell: "world",
        },
        SpellingCase {
            case: "a date declaration in upper case",
            column: "upper_date",
            declared: "DATE",
            category: "DATE",
            cell: "2026-01-02",
        },
        SpellingCase {
            case: "a timestamp declaration in mixed case",
            column: "mixed_timestamp",
            declared: "TimeStamp",
            category: "DATE",
            cell: "2026-01-02T03:04:05",
        },
        SpellingCase {
            case: "a Boolean declaration in upper case",
            column: "upper_boolean",
            declared: "BOOLEAN",
            category: "BOOLEAN",
            cell: "true",
        },
        SpellingCase {
            case: "a bit-sequence declaration in mixed case",
            column: "mixed_binary",
            declared: "Binary",
            category: "BINARY",
            cell: "QUJD",
        },
    ]
}

/// Which members `ColumnStatisticsData` carries on the wire: its discriminator and the one
/// data object the discriminator selects — "only one data object should be set and indicated
/// by the `Type` attribute" (`smithy_query shape=ColumnStatisticsData`).
fn data_members(data: &Value) -> Vec<String> {
    let mut members: Vec<String> = data
        .as_object()
        .unwrap_or_else(|| panic!("StatisticsData must be a structure, got {data}"))
        .keys()
        .cloned()
        .collect();
    members.sort();
    members
}

/// A generated entry reports the statistics category its figures were computed under,
/// whatever spelling the table declared the column with.
#[tokio::test]
async fn a_generated_entry_reports_the_category_rather_than_the_declared_spelling() {
    let cases = spelling_cases();
    // One table holding one column per row, over one object whose first row carries each
    // row's value and whose second holds the null marker in every cell — so every column
    // reports one value and one null, in the data member its category selects.
    let name = "declared_spellings";
    let values: Vec<&str> = cases.iter().map(|row| row.cell).collect();
    let nulls: Vec<&str> = cases.iter().map(|_| NULL_MARKER).collect();
    let body = format!("{}\n{}\n", values.join(","), nulls.join(","));
    let columns: Vec<(&str, &str)> = cases
        .iter()
        .map(|row| (row.column, row.declared))
        .collect();

    let (router, engine) = emulator_with_s3();
    stage_objects(&router, &bucket_of(name), &[("rows", body.as_str())]).await;
    store_table(
        &router,
        name,
        name,
        &columns,
        text_descriptor(name, json!({ "field.delim": "," }), json!({})),
    )
    .await;

    let run_id = start_run(&router, name, name, json!({})).await;
    set_due_instant(&engine, already_past());
    let run = get_run(&router, &run_id).await;
    assert_eq!(
        run.get("Status").and_then(Value::as_str),
        Some(COLUMN_STATISTICS_TASK_RUN_STATE_SUCCEEDED),
        "a run that read the table's data succeeds, got {run}"
    );

    let requested: Vec<&str> = cases.iter().map(|row| row.column).collect();
    let answer = get_statistics(&router, name, name, &requested).await;
    assert_eq!(
        answer.get("Errors").and_then(Value::as_array),
        Some(&Vec::new()),
        "every requested column is one the table declares, so nothing failed, got {answer}"
    );
    let reported = statistics_by_column(&answer);
    for SpellingCase {
        case,
        column,
        declared,
        category,
        ..
    } in &cases
    {
        let (_, entry) = reported
            .iter()
            .find(|(reported_column, _)| reported_column == column)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the answer must carry column {column:?}, got {answer}")
            });
        let generated_type = category.to_ascii_lowercase();
        assert_eq!(
            entry.get("ColumnType").and_then(Value::as_str),
            Some(generated_type.as_str()),
            "case {case:?}: column {column:?} declared {declared:?} must report the category \
             its figures were computed under, expected {generated_type:?}, got {entry}"
        );
        let data = entry
            .get("StatisticsData")
            .unwrap_or_else(|| panic!("case {case:?}: column {column:?} carries no data"));
        assert_eq!(
            data.get("Type").and_then(Value::as_str),
            Some(*category),
            "case {case:?}: column {column:?} reports its figures under {category}, got {data}"
        );
        let member = data_member(category);
        let mut expected_members = vec![member.to_string(), "Type".to_string()];
        expected_members.sort();
        assert_eq!(
            data_members(data),
            expected_members,
            "case {case:?}: column {column:?} states exactly the data object its Type \
             selects, got {data}"
        );
        let figures = data
            .get(member)
            .unwrap_or_else(|| panic!("case {case:?}: expected {member} in {data}"));
        let nulls = number(figures, "NumberOfNulls");
        assert!(
            (nulls - 1.0).abs() < f64::EPSILON,
            "case {case:?}: column {column:?} holds one cell with no value of its type, so it \
             reports NumberOfNulls 1, got {nulls} in {figures}"
        );
    }
}

/// Where an entry came from, as the second test varies it.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// A caller submitted the entry through `UpdateColumnStatisticsForTable`, stating this
    /// `ColumnType`.
    Submitted(&'a str),
    /// A column statistics task run computed the entry from the table's data.
    Computed,
}

/// A submitted entry keeps the type the caller stated; a run's generated entry reports the
/// category it computed the figures under, replacing whatever the column held.
#[tokio::test]
async fn a_submitted_entry_keeps_its_stated_type_and_a_generated_one_reports_the_category() {
    let name = "generated_vs_submitted";
    let column = "id";
    // The declaration of the recorded case, in a spelling of its own: what the column holds
    // is integral however the catalog writes the type down.
    let declared = "BigInt";
    let (router, engine) = emulator_with_s3();
    stage_objects(&router, &bucket_of(name), &[("rows", "10\n20\n")]).await;
    store_table(
        &router,
        name,
        name,
        &[(column, declared)],
        text_descriptor(name, json!({ "field.delim": "," }), json!({})),
    )
    .await;

    // Each step states where the entry the catalog then holds came from, and the
    // `ColumnType` a read of it must report.
    let steps: Vec<(&str, Source<'_>, &str)> = vec![
        (
            "a caller states the table's own declaration",
            Source::Submitted(declared),
            declared,
        ),
        (
            "a caller states another spelling of it",
            Source::Submitted("bigint"),
            "bigint",
        ),
        (
            "a run computes the statistics of the data",
            Source::Computed,
            "long",
        ),
        (
            "a caller writes over the generated entry",
            Source::Submitted("BIGINT"),
            "BIGINT",
        ),
    ];

    for (case, source, expected_type) in steps {
        match source {
            Source::Submitted(stated) => {
                let written = ok(
                    &router,
                    "UpdateColumnStatisticsForTable",
                    &json!({
                        "CatalogId": ACCOUNT_ID,
                        "DatabaseName": name,
                        "TableName": name,
                        "ColumnStatisticsList": [{
                            "ColumnName": column,
                            "ColumnType": stated,
                            "AnalyzedTime": 1_700_000_000,
                            "StatisticsData": {
                                "Type": "LONG",
                                "LongColumnStatisticsData": {
                                    "MinimumValue": 1,
                                    "MaximumValue": 7,
                                    "NumberOfNulls": 0,
                                    "NumberOfDistinctValues": 3
                                }
                            }
                        }]
                    }),
                )
                .await;
                assert_eq!(
                    written.get("Errors").and_then(Value::as_array),
                    Some(&Vec::new()),
                    "case {case:?}: the submitted entry must be stored, got {written}"
                );
            }
            Source::Computed => {
                let run_id = start_run(&router, name, name, json!({})).await;
                set_due_instant(&engine, already_past());
                let run = get_run(&router, &run_id).await;
                assert_eq!(
                    run.get("Status").and_then(Value::as_str),
                    Some(COLUMN_STATISTICS_TASK_RUN_STATE_SUCCEEDED),
                    "case {case:?}: a run that read the table's data succeeds, got {run}"
                );
            }
        }

        let answer = get_statistics(&router, name, name, &[column]).await;
        let reported = statistics_by_column(&answer);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: the catalog holds statistics for exactly one column, got {answer}"
        );
        let (_, entry) = &reported[0];
        assert_eq!(
            entry.get("ColumnType").and_then(Value::as_str),
            Some(expected_type),
            "case {case:?}: expected ColumnType {expected_type:?}, got {entry}"
        );
        let figures = entry
            .get("StatisticsData")
            .and_then(|data| data.get("LongColumnStatisticsData"))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the entry must carry LONG figures, got {entry}")
            });
        // The figures of a generated entry are the data's own: 10 and 20 across two rows.
        let (minimum, maximum) = match source {
            Source::Computed => (10.0, 20.0),
            Source::Submitted(_) => (1.0, 7.0),
        };
        assert!(
            (number(figures, "MinimumValue") - minimum).abs() < f64::EPSILON
                && (number(figures, "MaximumValue") - maximum).abs() < f64::EPSILON,
            "case {case:?}: expected extremes {minimum} and {maximum}, got {figures}"
        );
    }
}
