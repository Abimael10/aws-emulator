// SPDX-License-Identifier: CC0-1.0

//! `ANALYZE COMPRESSION` and the catalogue the encoding it recommends shows up
//! in.
//!
//! The rules under test, each over a table of inputs rather than one recorded
//! statement:
//!
//! 1. `ANALYZE COMPRESSION <table>` reports one record per analysed column, in
//!    a result set whose columns are the four Amazon Redshift names, and the
//!    encoding it names for a column follows from that column's own values: a
//!    low-cardinality column is recommended a dictionary, a column stored in
//!    runs a run-length encoding, a column of consecutive numbers a delta
//!    encoding, and a column nothing shrinks is left raw with no reduction;
//! 2. `PG_TABLE_DEF` reports every column's declared type and the compression it
//!    is stored under — the encoding its `CREATE TABLE` declared, `none` for one
//!    stored raw, and the encoding the warehouse assigns by type for a column
//!    that declared none.
//!
//! The harness is the one `warehouse_data_plane` owns, so these suites drive the
//! same emulator every other suite of this crate does.

use serde_json::{json, Value};

use crate::warehouse_data_plane::{field_text, Harness};

/// One analysed column: the values it holds and what the analysis must make of
/// them.
struct AnalysisRow {
    case: &'static str,
    /// The column's declared type.
    declared: &'static str,
    /// The values the table is filled with, as SQL row constructors.
    values: String,
    /// The encoding the report must name.
    encoding: &'static str,
    /// The reduction it must report, as a percentage.
    reduction: f64,
}

/// The encoding `ANALYZE COMPRESSION` names for a column is the one the column's
/// own values support, with the reduction that encoding really achieves.
///
/// The rows differ in the TYPE analysed, the number of distinct values, the
/// ORDER they are stored in and how many rows there are, so the rule is asserted
/// over the class of columns rather than at one of them.
#[tokio::test]
async fn the_compression_report_follows_the_values_the_column_holds() {
    let harness = Harness::new();
    let cluster = "compression-report";
    harness.create_cluster(cluster).await;

    // Five distinct five-character values, so the dictionary a block holds is
    // 25 bytes beside one index byte per row.
    const WORDS: [&str; 5] = ["alpha", "bravo", "charl", "delta", "ethan"];

    let rows = [
        AnalysisRow {
            case: "a low-cardinality text column, its values interleaved",
            declared: "v VARCHAR(64)",
            values: (0..200)
                .map(|row| format!("('{}')", WORDS[row % WORDS.len()]))
                .collect::<Vec<String>>()
                .join(", "),
            // 200 rows of 5 bytes is 1000 raw; a 25-byte dictionary and 200
            // one-byte indexes is 225.
            encoding: "bytedict",
            reduction: 77.50,
        },
        AnalysisRow {
            case: "the same values stored in runs",
            declared: "v VARCHAR(64)",
            values: (0..200)
                .map(|row| format!("('{}')", WORDS[row / 40]))
                .collect::<Vec<String>>()
                .join(", "),
            // Five runs, each a token of its five-byte value and a count byte.
            encoding: "runlength",
            reduction: 97.00,
        },
        AnalysisRow {
            case: "consecutive integers",
            declared: "n INTEGER",
            values: (1..=200)
                .map(|row| format!("({row})"))
                .collect::<Vec<String>>()
                .join(", "),
            // 200 four-byte integers is 800 raw; one full value and 199
            // one-byte differences is 204.
            encoding: "delta",
            reduction: 74.50,
        },
        AnalysisRow {
            case: "one integer repeated",
            declared: "n INTEGER",
            values: (0..200)
                .map(|_row| "(7)".to_string())
                .collect::<Vec<String>>()
                .join(", "),
            // One run of 200: a four-byte value and its count byte.
            encoding: "runlength",
            reduction: 99.38,
        },
        AnalysisRow {
            case: "a column no encoding shrinks",
            declared: "v VARCHAR(8)",
            values: (0..10)
                .map(|row| format!("('v{row:02}')"))
                .collect::<Vec<String>>()
                .join(", "),
            // Ten distinct three-byte values: a dictionary of all of them plus
            // an index per row, and a run per row, each cost more than the 30
            // bytes the values take as they are.
            encoding: "raw",
            reduction: 0.0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let table = format!("analysed_{index}");
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("CREATE TABLE {table} ({})", row.declared),
            )
            .await;
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("INSERT INTO {table} VALUES {}", row.values),
            )
            .await;
        let report = harness
            .query_rows(
                row.case,
                cluster,
                &format!("ANALYZE COMPRESSION {table}"),
            )
            .await;
        let record = report
            .first()
            .unwrap_or_else(|| panic!("case {:?}: the analysis must report the column", row.case));
        let cells: Vec<String> = record.iter().map(field_text).collect();
        assert_eq!(
            cells.first().map(String::as_str),
            Some(table.as_str()),
            "case {:?}: the record must name the analysed table, got {cells:?}",
            row.case
        );
        assert_eq!(
            cells.get(2).map(String::as_str),
            Some(row.encoding),
            "case {:?}: expected the encoding {}, got {cells:?}",
            row.case,
            row.encoding
        );
        let reported: f64 = cells
            .get(3)
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
        assert!(
            (reported - row.reduction).abs() < 0.01,
            "case {:?}: expected an estimated reduction of {}, got {reported}",
            row.case,
            row.reduction
        );
    }
}

/// The report is a result set of the four columns Amazon Redshift names, one
/// record per analysed column — and what the statement narrows the analysis to
/// is what it reports on.
#[tokio::test]
async fn the_compression_report_is_served_as_the_four_named_columns() {
    let harness = Harness::new();
    let cluster = "compression-shape";
    harness.create_cluster(cluster).await;
    harness
        .run_ok(
            "the analysed table",
            cluster,
            "CREATE TABLE shaped (id INTEGER, label VARCHAR(16), amt DECIMAL(9,2))",
        )
        .await;
    harness
        .run_ok(
            "the rows analysed",
            cluster,
            "INSERT INTO shaped VALUES (1, 'a', 1.50), (1, 'a', 1.50), (2, 'b', 2.50)",
        )
        .await;

    let described = harness
        .run_ok("the whole table", cluster, "ANALYZE COMPRESSION shaped")
        .await;
    assert_eq!(
        described["HasResultSet"],
        json!(true),
        "the analysis produces a result set, got {described}"
    );
    let fetched = harness
        .data_ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    let columns: Vec<String> = fetched["ColumnMetadata"]
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|column| column["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        columns,
        vec!["Table", "Column", "Encoding", "Est_reduction_pct"],
        "the report's columns are the four the service names, got {fetched}"
    );

    /// One narrowing of the analysis: the statement and the columns it must
    /// report on.
    struct Narrowed {
        case: &'static str,
        sql: &'static str,
        columns: &'static [&'static str],
    }

    let narrowed = [
        Narrowed {
            case: "every column of the table",
            sql: "ANALYZE COMPRESSION shaped",
            columns: &["id", "label", "amt"],
        },
        Narrowed {
            case: "one column of it",
            sql: "ANALYZE COMPRESSION shaped (label)",
            columns: &["label"],
        },
        Narrowed {
            case: "two of its columns, in the order the statement names them",
            sql: "ANALYZE COMPRESSION shaped (amt, id)",
            columns: &["amt", "id"],
        },
        Narrowed {
            case: "a sample size of its own",
            sql: "ANALYZE COMPRESSION shaped COMPROWS 2",
            columns: &["id", "label", "amt"],
        },
        Narrowed {
            case: "a sample size named for one column",
            sql: "ANALYZE COMPRESSION shaped (id) COMPROWS 1",
            columns: &["id"],
        },
    ];

    for row in &narrowed {
        let report = harness.query_rows(row.case, cluster, row.sql).await;
        let analysed: Vec<String> = report
            .iter()
            .map(|record| field_text(&record.get(1).cloned().unwrap_or(Value::Null)))
            .collect();
        assert_eq!(
            analysed,
            row.columns.iter().map(ToString::to_string).collect::<Vec<String>>(),
            "case {:?}: the analysis must report these columns, got {report:?}",
            row.case
        );
    }

    // A statement that names no table at all analyses every table the database
    // holds.
    let every = harness
        .query_rows("no table named", cluster, "ANALYZE COMPRESSION")
        .await;
    assert_eq!(
        every.len(),
        3,
        "the analysis of the whole database must report every column it holds, got {every:?}"
    );

    for (case, sql, expected) in [
        (
            "a table the database does not hold",
            "ANALYZE COMPRESSION absent",
            "ERROR: relation \"absent\" does not exist",
        ),
        (
            "a column the table does not declare",
            "ANALYZE COMPRESSION shaped (absent)",
            "ERROR: column \"absent\" does not exist",
        ),
    ] {
        let error = harness.run_failing(case, cluster, sql).await;
        assert_eq!(
            error, expected,
            "case {case:?}: the analysis must report what it cannot resolve"
        );
    }
}

/// One described column: how it is declared and what the catalogue reports for
/// it.
struct CatalogRow {
    case: &'static str,
    /// The column as the `CREATE TABLE` declares it.
    declared: &'static str,
    /// `type`, as the catalogue spells it.
    column_type: &'static str,
    /// `encoding`.
    encoding: &'static str,
    /// `notnull`.
    not_null: bool,
}

/// `PG_TABLE_DEF` reports each column's declared type and the compression it is
/// stored under: the encoding the table declared, `none` for a column stored
/// raw, and the encoding the warehouse assigns by type for one that declared
/// none.
///
/// The rows differ in the type declared and in whether the column declares an
/// encoding of its own, so the rule is asserted over the class of columns.
#[tokio::test]
async fn the_catalogue_reports_the_type_and_encoding_of_each_column() {
    let harness = Harness::new();
    let cluster = "table-def";
    harness.create_cluster(cluster).await;

    let rows = [
        CatalogRow {
            case: "a text column stored uncompressed",
            declared: "v VARCHAR(64) ENCODE RAW",
            column_type: "character varying(64)",
            encoding: "none",
            not_null: false,
        },
        CatalogRow {
            case: "a text column stored under a dictionary",
            declared: "v VARCHAR(64) ENCODE BYTEDICT",
            column_type: "character varying(64)",
            encoding: "bytedict",
            not_null: false,
        },
        CatalogRow {
            case: "a text column that declares no encoding",
            declared: "v VARCHAR(32)",
            column_type: "character varying(32)",
            encoding: "lzo",
            not_null: false,
        },
        CatalogRow {
            case: "an integer column that declares none",
            declared: "n INTEGER",
            column_type: "integer",
            encoding: "az64",
            not_null: false,
        },
        CatalogRow {
            case: "an integer column that declares one",
            declared: "n BIGINT ENCODE DELTA",
            column_type: "bigint",
            encoding: "delta",
            not_null: false,
        },
        CatalogRow {
            case: "a floating point column, which is not compressed",
            declared: "f DOUBLE PRECISION",
            column_type: "double precision",
            encoding: "none",
            not_null: false,
        },
        CatalogRow {
            case: "a boolean column, which is not either",
            declared: "b BOOLEAN",
            column_type: "boolean",
            encoding: "none",
            not_null: false,
        },
        CatalogRow {
            case: "an exact numeric column",
            declared: "m DECIMAL(9,2)",
            column_type: "numeric(9,2)",
            encoding: "az64",
            not_null: false,
        },
        CatalogRow {
            case: "a timestamp column that may not be null",
            declared: "t TIMESTAMP NOT NULL",
            column_type: "timestamp without time zone",
            encoding: "az64",
            not_null: true,
        },
        CatalogRow {
            case: "a column whose encoding is managed for it",
            declared: "v VARCHAR(16) ENCODE AUTO",
            column_type: "character varying(16)",
            encoding: "lzo",
            not_null: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let table = format!("described_{index}");
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("CREATE TABLE {table} ({})", row.declared),
            )
            .await;
        let described = harness
            .query_rows(
                row.case,
                cluster,
                &format!(
                    "SELECT schemaname, tablename, \"column\", type, encoding, \"notnull\" \
                     FROM pg_table_def WHERE tablename = '{table}'"
                ),
            )
            .await;
        let record = described.first().unwrap_or_else(|| {
            panic!("case {:?}: the catalogue must describe the column", row.case)
        });
        let cells: Vec<String> = record.iter().map(field_text).collect();
        assert_eq!(
            cells,
            vec![
                "public".to_string(),
                table.clone(),
                row.declared
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                row.column_type.to_string(),
                row.encoding.to_string(),
                if row.not_null { "true" } else { "false" }.to_string(),
            ],
            "case {:?}: the catalogue must report the column this way",
            row.case
        );
    }
}

/// The encoding the analysis recommends is one a `CREATE TABLE` can be declared
/// with, and the catalogue reports it back on the table that declared it.
#[tokio::test]
async fn a_recommended_encoding_is_declarable_and_reads_back_from_the_catalogue() {
    let harness = Harness::new();
    let cluster = "compression-round-trip";
    harness.create_cluster(cluster).await;
    harness
        .run_ok(
            "the analysed table",
            cluster,
            "CREATE TABLE raw_column (v VARCHAR(64) ENCODE RAW)",
        )
        .await;
    let values: Vec<String> = (0..500)
        .map(|row| format!("('value-{}')", row % 4))
        .collect();
    harness
        .run_ok(
            "the rows analysed",
            cluster,
            &format!("INSERT INTO raw_column VALUES {}", values.join(", ")),
        )
        .await;

    // Before anything is recommended, the column reads back as stored raw.
    let stored = harness
        .one_value(
            "the column as declared",
            cluster,
            "SELECT encoding FROM pg_table_def WHERE tablename = 'raw_column'",
        )
        .await;
    assert_eq!(
        stored, "none",
        "a column declared ENCODE RAW reads back as uncompressed, got {stored:?}"
    );

    let report = harness
        .query_rows(
            "the recommendation",
            cluster,
            "ANALYZE COMPRESSION raw_column",
        )
        .await;
    let record = report
        .first()
        .expect("the analysis must report the column");
    let recommended = field_text(&record.get(2).cloned().unwrap_or(Value::Null));
    let reduction: f64 = field_text(&record.get(3).cloned().unwrap_or(Value::Null))
        .parse()
        .unwrap_or_default();
    assert_ne!(
        recommended, "raw",
        "a column of four distinct values over 500 rows is compressible, got {report:?}"
    );
    assert!(
        reduction > 0.0,
        "a recommended encoding must save space, got {reduction}"
    );

    // The recommendation is usable as the storage of a table, and that is what
    // the catalogue then reports for it.
    harness
        .run_ok(
            "the recommended encoding declared",
            cluster,
            &format!("CREATE TABLE encoded_column (v VARCHAR(64) ENCODE {recommended})"),
        )
        .await;
    let read_back = harness
        .one_value(
            "the declared encoding",
            cluster,
            "SELECT encoding FROM pg_table_def WHERE tablename = 'encoded_column'",
        )
        .await;
    assert_eq!(
        read_back, recommended,
        "the catalogue must report the encoding the table declared"
    );
}
