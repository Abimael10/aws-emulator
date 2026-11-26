// SPDX-License-Identifier: CC0-1.0

//! The documents a run SAVES, reproduced object by object: what each one holds, line by line.
//!
//! THE RULE UNDER TEST: every object this service saves for a client to read back is a
//! line-oriented document whose records are one per line — the results of a run (its column
//! labels, then one line per row), its text output, its data manifest and the metadata
//! information beside its results (one line per described member). That is what makes such an
//! object readable by anything that reads a prefix record by record, which is the position a
//! results location INSIDE a table's own location puts it in
//! (`crate::root_located_table_scan`).
//!
//! The rows differ along what the rule generalises over: how many rows the run returned and
//! how many columns the result set has.

use serde_json::Value;
use service_com_amazonaws_athena_amazonathena::state::{
    query_results::RESULT_METADATA_SUFFIX, RESULT_OBJECT_ROWS_SUFFIX,
};

use crate::root_located_table_scan::{
    emulator, given_bucket, given_database, lines_in, object_body, settled,
};

/// The bucket the runs of this suite save their results in.
const BUCKET: &str = "athena-saved-documents";

/// The prefix under it they land at.
const PREFIX: &str = "saved";

/// One statement whose saved pair is read back.
struct Saved {
    /// What the row is.
    case: &'static str,
    /// The statement submitted.
    sql: &'static str,
    /// How many rows its result set holds.
    rows: usize,
    /// How many columns it has.
    columns: usize,
}

/// A run's results object holds its column labels and then one line per row, and the metadata
/// information beside it holds one line per described member — so both are documents a reader
/// reads record by record rather than one line holding everything.
#[tokio::test]
async fn every_object_a_run_saves_holds_its_records_one_per_line() {
    let saved: &[Saved] = &[
        Saved {
            case: "one row of one column",
            sql: "SELECT 1 AS n",
            rows: 1,
            columns: 1,
        },
        Saved {
            case: "three rows of one column",
            sql: "SELECT 1 AS n UNION ALL SELECT 2 AS n UNION ALL SELECT 3 AS n",
            rows: 3,
            columns: 1,
        },
        Saved {
            case: "one row of three columns",
            sql: "SELECT 1 AS a, 'b' AS b, 2.5 AS c",
            rows: 1,
            columns: 3,
        },
    ];

    let router = emulator();
    given_database(&router, "setup").await;
    given_bucket(&router, "setup", BUCKET).await;
    let output_location = format!("s3://{BUCKET}/{PREFIX}/");

    // How long the metadata document of each width turned out to be, in declaration order.
    let mut widths: Vec<(usize, usize)> = Vec::new();
    for row in saved {
        let case = row.case;
        let id = settled(&router, case, row.sql, &output_location).await;

        // The results object: the label line, then one line per row.
        let key = format!("{PREFIX}/{id}{RESULT_OBJECT_ROWS_SUFFIX}");
        let results = object_body(&router, case, BUCKET, &key).await;
        let held = lines_in(&results);
        assert_eq!(
            held,
            row.rows + 1,
            "case {case:?}: expected the results object to hold {} lines (the labels and one \
             per row), got {held} in {:?}",
            row.rows + 1,
            String::from_utf8_lossy(&results)
        );
        assert!(
            String::from_utf8_lossy(&results)
                .lines()
                .all(|line| !line.is_empty()),
            "case {case:?}: no line of a saved document may be empty, got {:?}",
            String::from_utf8_lossy(&results)
        );

        // The metadata beside it: the schema it describes, held one described member per line.
        let metadata_key = format!("{key}{RESULT_METADATA_SUFFIX}");
        let metadata = object_body(&router, case, BUCKET, &metadata_key).await;
        let described: Value = serde_json::from_slice(&metadata).unwrap_or(Value::Null);
        let columns = described["ColumnInfo"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            columns.len(),
            row.columns,
            "case {case:?}: expected the metadata to describe {} columns, got {:?}",
            row.columns,
            String::from_utf8_lossy(&metadata)
        );
        let metadata_lines = lines_in(&metadata);
        assert!(
            metadata_lines > row.columns,
            "case {case:?}: expected the metadata document to hold a line per described \
             member, so more than {} in all, got {metadata_lines} in {:?}",
            row.columns,
            String::from_utf8_lossy(&metadata)
        );
        assert!(
            String::from_utf8_lossy(&metadata)
                .lines()
                .all(|line| !line.trim().is_empty()),
            "case {case:?}: no line of a saved document may be empty, got {:?}",
            String::from_utf8_lossy(&metadata)
        );
        widths.push((row.columns, metadata_lines));
    }

    // A wider schema is a longer document, because the records are per described member.
    let narrowest = widths
        .iter()
        .min_by_key(|(columns, _)| *columns)
        .copied()
        .expect("a row");
    let widest = widths
        .iter()
        .max_by_key(|(columns, _)| *columns)
        .copied()
        .expect("a row");
    assert!(
        widest.1 > narrowest.1,
        "expected the metadata of a {}-column result set to hold more lines than that of a \
         {}-column one, got {widths:?}",
        widest.0,
        narrowest.0
    );
}
