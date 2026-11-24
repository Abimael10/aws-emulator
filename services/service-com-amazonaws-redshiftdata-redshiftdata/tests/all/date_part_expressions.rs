// SPDX-License-Identifier: CC0-1.0

//! The DATE PART keyword a Redshift date or time function takes as its first
//! argument, and the value each of those functions computes from it.
//!
//! The rule under test: in the date-part position of `DATEDIFF`, `DATEADD`,
//! `DATE_PART` and `EXTRACT`, a bare word the dialect publishes as a date part
//! names the UNIT the function works in — never a column — and the function
//! computes over the instants it is given at the microsecond resolution a
//! Redshift timestamp carries. A word the vocabulary does not publish stays a
//! name, and a part the published table admits for `DATE_PART` alone names no
//! span for the functions that count one.
//!
//! Each rule is asserted over a TABLE of inputs: the rows differ in the unit,
//! in which published spelling of that unit they use (name, abbreviation or
//! alias), in the sign and size of the span, and in whether the operands are
//! stored values or literals.
//!
//! The harness is the one `warehouse_data_plane` owns, so these suites drive
//! the same emulator every other suite of this pair does.

use crate::warehouse_data_plane::Harness;

/// Two instants stored with MICROSECOND precision, and the spans between them:
/// the whole point of the date-part functions is that the engine computes the
/// span itself, so the stored values are read back through aggregates over the
/// table rather than written into the statement.
#[tokio::test]
async fn datediff_computes_the_span_between_two_stored_instants() {
    let harness = Harness::new();
    let cluster = "date-part-spans";
    harness.create_cluster(cluster).await;

    harness
        .run_ok(
            "the table the spans are computed over",
            cluster,
            "CREATE TABLE spans (k INT, ts TIMESTAMP, amt DECIMAL(12,4))",
        )
        .await;
    harness
        .run_ok(
            "rows written with microsecond precision",
            cluster,
            "INSERT INTO spans (k, ts, amt) VALUES \
             (1, '2024-05-01 12:00:00.000001', 10.5000), \
             (2, '2024-05-01 12:00:01.250000', 0.2500), \
             (3, '2024-05-01 12:00:03.500000', 4.0000)",
        )
        .await;

    // The whole statement the recorded check runs: the two spans, the exact sum
    // and the row count in one row.
    let rows = harness
        .query_rows(
            "the two spans, the exact sum and the count",
            cluster,
            "SELECT datediff(microsecond, min(ts), max(ts)), datediff(second, min(ts), max(ts)), \
             sum(amt), count(*) FROM spans",
        )
        .await;
    let row = rows.first().cloned().unwrap_or_default();
    let cells: Vec<String> = row.iter().map(crate::warehouse_data_plane::field_text).collect();
    // 12:00:00.000001 to 12:00:03.500000 is 3 499 999 microseconds, and three
    // whole-second boundaries.
    assert_eq!(
        cells,
        vec![
            "3499999".to_string(),
            "3".to_string(),
            "14.75".to_string(),
            "3".to_string()
        ],
        "the statement must report the microsecond span, the second span, the exact sum and the \
         row count, got {cells:?}"
    );
}

/// One date-part case: the statement, and the single value it must answer.
struct Row {
    case: &'static str,
    sql: &'static str,
    expected: &'static str,
}

/// `DATEDIFF` counts the boundaries of its unit between two instants.
///
/// The rows span the units from a millennium down to a microsecond, use a
/// published abbreviation or alias as well as the full name, and run in both
/// directions, so what is asserted is the rule and not one recorded statement.
#[tokio::test]
async fn datediff_counts_the_boundaries_of_every_published_unit() {
    let harness = Harness::new();
    let cluster = "date-part-units";
    harness.create_cluster(cluster).await;

    let rows = [
        Row {
            case: "a year boundary one day apart",
            sql: "SELECT datediff(year, '2008-12-31', '2009-01-01')",
            expected: "1",
        },
        Row {
            case: "the published example of a week span",
            sql: "SELECT datediff(week, '2009-01-01', '2009-12-31')",
            expected: "52",
        },
        Row {
            case: "a month boundary one day apart",
            sql: "SELECT datediff(month, '2024-01-31', '2024-02-01')",
            expected: "1",
        },
        Row {
            case: "quarters, under the published alias qtrs",
            sql: "SELECT datediff(qtrs, '2024-01-15', '2024-10-01')",
            expected: "3",
        },
        Row {
            case: "days across a leap day",
            sql: "SELECT datediff(day, '2024-02-01', '2024-03-01')",
            expected: "29",
        },
        Row {
            case: "hours, under the published abbreviation hrs",
            sql: "SELECT datediff(hrs, '2024-05-01 01:00:00', '2024-05-02 04:00:00')",
            expected: "27",
        },
        Row {
            case: "minutes, under the published alias mins",
            sql: "SELECT datediff(mins, '2024-05-01 00:00:00', '2024-05-01 02:30:00')",
            expected: "150",
        },
        Row {
            case: "milliseconds between two sub-second instants",
            sql: "SELECT datediff(ms, '2024-05-01 00:00:00.000500', '2024-05-01 00:00:00.002500')",
            expected: "2",
        },
        Row {
            case: "microseconds between two sub-second instants",
            sql: "SELECT datediff(microsecond, '2024-05-01 00:00:00.000500', \
                  '2024-05-01 00:00:00.002500')",
            expected: "2000",
        },
        Row {
            case: "a span backwards is the same span negated",
            sql: "SELECT datediff(day, '2024-03-01', '2024-02-01')",
            expected: "-29",
        },
        Row {
            case: "a decade boundary",
            sql: "SELECT datediff(decade, '2019-12-31', '2020-01-01')",
            expected: "1",
        },
        Row {
            case: "a century boundary, which this calendar numbers from year 1",
            sql: "SELECT datediff(century, '2000-12-31', '2001-01-01')",
            expected: "1",
        },
        Row {
            case: "a millennium boundary, numbered the same way",
            sql: "SELECT datediff(millennium, '2000-12-31', '2001-01-01')",
            expected: "1",
        },
        Row {
            case: "epoch, which counts seconds",
            sql: "SELECT datediff(epoch, '2024-05-01 00:00:00', '2024-05-01 00:01:30')",
            expected: "90",
        },
        Row {
            case: "a date operand, which is midnight of its day",
            sql: "SELECT datediff(hour, '2024-05-01', '2024-05-01 05:00:00')",
            expected: "5",
        },
        Row {
            case: "the same unit with its one-letter abbreviation",
            sql: "SELECT datediff(d, '2024-05-01', '2024-05-08')",
            expected: "7",
        },
    ];

    for row in &rows {
        let answered = harness.one_value(row.case, cluster, row.sql).await;
        assert_eq!(
            answered, row.expected,
            "case {:?}: {} must answer {}, got {answered:?}",
            row.case, row.sql, row.expected
        );
    }
}

/// `DATEADD` moves an instant by a count of its unit, and `DATE_PART`/`EXTRACT`
/// report one field of an instant.
#[tokio::test]
async fn dateadd_moves_an_instant_and_date_part_reports_a_field() {
    let harness = Harness::new();
    let cluster = "date-part-fields";
    harness.create_cluster(cluster).await;

    let rows = [
        Row {
            case: "a month added to the last day of a longer month is clamped",
            sql: "SELECT dateadd(month, 1, '2024-01-31')",
            expected: "2024-02-29 00:00:00",
        },
        Row {
            case: "microseconds added keep the fraction",
            sql: "SELECT dateadd(microsecond, 1500, '2024-05-01 00:00:00.000500')",
            expected: "2024-05-01 00:00:00.002000",
        },
        Row {
            case: "a negative count moves the instant back",
            sql: "SELECT dateadd(day, -1, '2024-03-01 06:30:00')",
            expected: "2024-02-29 06:30:00",
        },
        Row {
            case: "weeks added, under the published abbreviation w",
            sql: "SELECT dateadd(w, 2, '2024-05-01')",
            expected: "2024-05-15 00:00:00",
        },
        Row {
            case: "the day of the week a date falls on, Sunday being 0",
            sql: "SELECT date_part(dow, '2024-01-07')",
            expected: "0",
        },
        Row {
            case: "the day of the year",
            sql: "SELECT date_part(doy, '2024-03-01')",
            expected: "61",
        },
        Row {
            case: "the month field",
            sql: "SELECT date_part(month, '2024-03-01 10:20:30')",
            expected: "3",
        },
        Row {
            case: "the microsecond field, which carries the seconds into it",
            sql: "SELECT date_part(microsecond, '2024-03-01 10:20:30.123456')",
            expected: "30123456",
        },
        Row {
            case: "the same field through EXTRACT, which asks for the same value",
            sql: "SELECT extract(microsecond from '2024-03-01 10:20:30.123456')",
            expected: "30123456",
        },
        Row {
            case: "EXTRACT of the hour",
            sql: "SELECT extract(hour from '2024-03-01 10:20:30')",
            expected: "10",
        },
        Row {
            case: "the epoch field, in seconds since 1970",
            sql: "SELECT date_part(epoch, '1970-01-02 00:00:00')",
            expected: "86400",
        },
    ];

    for row in &rows {
        let answered = harness.one_value(row.case, cluster, row.sql).await;
        assert_eq!(
            answered, row.expected,
            "case {:?}: {} must answer {}, got {answered:?}",
            row.case, row.sql, row.expected
        );
    }
}

/// What is NOT a date part stays what it was: a word no published date part
/// spells, and a part that names no span asked of a function that counts one.
#[tokio::test]
async fn a_word_that_is_not_a_published_date_part_is_not_read_as_one() {
    let harness = Harness::new();
    let cluster = "date-part-refusals";
    harness.create_cluster(cluster).await;

    harness
        .run_ok(
            "a table with a column named like a date part",
            cluster,
            "CREATE TABLE marks (day_label VARCHAR(8), ts TIMESTAMP)",
        )
        .await;
    harness
        .run_ok(
            "one row to read it from",
            cluster,
            "INSERT INTO marks (day_label, ts) VALUES ('first', '2024-05-01 00:00:00')",
        )
        .await;

    for (case, sql, named) in [(
        "a word no published date part spells is a column reference",
        "SELECT datediff(frobnicate, ts, ts) FROM marks",
        "frobnicate",
    )] {
        let error = harness.run_failing(case, cluster, sql).await;
        assert!(
            error.contains(named),
            "case {case:?}: {sql} must fail over the name {named}, got {error:?}"
        );
    }

    // A part the vocabulary publishes for DATE_PART alone names no span, so a
    // function that counts one refuses it rather than counting something else.
    let error = harness
        .run_failing(
            "a field-only part asked of the function that counts a span",
            cluster,
            "SELECT datediff(dow, '2024-05-01', '2024-05-08')",
        )
        .await;
    assert!(
        error.contains("dow"),
        "a part that names no span must be refused over its own spelling, got {error:?}"
    );

    // A column of the statement is still a column, even where a date part is
    // admitted: the first argument is the only date-part position.
    let answered = harness
        .one_value(
            "a column read beside a date part",
            cluster,
            "SELECT day_label FROM marks WHERE datediff(day, ts, ts) = 0",
        )
        .await;
    assert_eq!(
        answered, "first",
        "a date-part function must not change how the rest of the statement resolves, got \
         {answered:?}"
    );
}
