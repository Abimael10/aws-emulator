// SPDX-License-Identifier: CC0-1.0

//! What a `COPY` REPORTS: the statement record it leaves behind, the session
//! counter that holds the rows it ingested, and the threshold that decides
//! whether a dirty load lands at all.
//!
//! The rules under test, each over a table of inputs rather than one recorded
//! load:
//!
//! 1. a load reports no rows and a definite zero size on its statement record —
//!    `ResultRows` is the rows a statement RETURNED or AFFECTED, and the rows a
//!    load ingested are neither — while the rows themselves are in the table and
//!    the count is in the session, where `PG_LAST_COPY_COUNT()` reports it;
//! 2. `MAXERROR` is the number of rejected rows at which the load FAILS: a load
//!    that reaches it commits nothing and reports the warehouse's own
//!    load-failure line, and the same load under a higher threshold finishes
//!    with the rows that parsed.
//!
//! The harness is the one `warehouse_data_plane` owns, so these suites drive the
//! same emulator every other suite of this crate does.

use serde_json::{json, Value};

use crate::warehouse_data_plane::{allow_policy, create_role, field_text, Harness};

/// The bucket every object these suites load lives in.
const BUCKET: &str = "load-accounting";

/// One load: the object it reads, the statement that reads it, and the rows it
/// puts in the table.
struct LoadRow {
    case: &'static str,
    /// The columns the target declares.
    declared: &'static str,
    /// The object's body.
    body: &'static str,
    /// The clause the `COPY` carries after its `IAM_ROLE`.
    options: &'static str,
    /// The columns the statement names, written as it writes them.
    columns: &'static str,
    /// The rows the load must put in the table.
    loaded: i64,
}

/// A load reports no rows on its statement record, whatever it loaded: the count
/// is the SESSION's, and the rows are the table's.
///
/// The rows differ in the format read, the number of rows in the object, whether
/// the statement names a column list, and whether the load writes at all
/// (`NOLOAD`), so the rule is asserted over the class of loads and not over one
/// of them.
#[tokio::test]
async fn a_load_reports_no_rows_and_a_zero_size_on_its_statement_record() {
    let harness = Harness::new();
    let cluster = "load-record";
    harness.create_cluster(cluster).await;
    harness.create_bucket(BUCKET).await;
    let role = granting_role(&harness, "load-record-role").await;

    let rows = [
        LoadRow {
            case: "three CSV rows",
            declared: "id INTEGER, name VARCHAR(32), amt DECIMAL(9,2)",
            body: "1,alpha,10.50\n2,beta,20.25\n3,gamma,30.00\n",
            options: "CSV",
            columns: " (id, name, amt)",
            loaded: 3,
        },
        LoadRow {
            case: "one CSV row",
            declared: "id INTEGER, name VARCHAR(32)",
            body: "7,solo\n",
            options: "CSV",
            columns: "",
            loaded: 1,
        },
        LoadRow {
            case: "five rows of the default character-delimited format",
            declared: "id INTEGER, name VARCHAR(32)",
            body: "1|a\n2|b\n3|c\n4|d\n5|e\n",
            options: "",
            columns: "",
            loaded: 5,
        },
        LoadRow {
            case: "a column list that leaves a column out",
            declared: "id INTEGER, name VARCHAR(32), note VARCHAR(32)",
            body: "1,alpha\n2,beta\n",
            options: "CSV",
            columns: " (id, name)",
            loaded: 2,
        },
        LoadRow {
            case: "a load that only checks the object",
            declared: "id INTEGER, name VARCHAR(32)",
            body: "1,alpha\n2,beta\n",
            options: "CSV NOLOAD",
            columns: "",
            loaded: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let table = format!("record_{index}");
        let key = format!("record-{index}.txt");
        harness.put_object(BUCKET, &key, row.body).await;
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("CREATE TABLE {table} ({})", row.declared),
            )
            .await;
        let described = harness
            .run_ok(
                row.case,
                cluster,
                &format!(
                    "COPY {table}{} FROM 's3://{BUCKET}/{key}' IAM_ROLE '{role}' {}",
                    row.columns, row.options
                ),
            )
            .await;
        assert_eq!(
            described["HasResultSet"],
            json!(false),
            "case {:?}: a load produces no result set, got {described}",
            row.case
        );
        assert_eq!(
            described["ResultRows"].as_i64(),
            Some(0),
            "case {:?}: a load reports no rows on its statement record, got {described}",
            row.case
        );
        assert_eq!(
            described["ResultSize"].as_i64(),
            Some(0),
            "case {:?}: a load reports a definite zero result size, got {described}",
            row.case
        );
        let count = harness
            .one_value(
                row.case,
                cluster,
                &format!("SELECT count(*) FROM {table}"),
            )
            .await;
        assert_eq!(
            count,
            row.loaded.to_string(),
            "case {:?}: the rows the load read must be in the table",
            row.case
        );
    }
}

/// The rows a load ingested are the SESSION's to report:
/// `PG_LAST_COPY_COUNT()` on the connection that ran the load answers with its
/// count, and a connection that has run no load answers with none.
///
/// The rows differ in how many rows the load ingested — several, one, and none at
/// all — so what is asserted is that the counter follows the load.
#[tokio::test]
async fn the_session_reports_what_the_last_load_ingested() {
    let harness = Harness::new();
    let cluster = "load-session-count";
    harness.create_cluster(cluster).await;
    harness.create_bucket(BUCKET).await;
    let role = granting_role(&harness, "load-session-role").await;

    let before = harness
        .one_value(
            "a connection that has run no load",
            cluster,
            "SELECT pg_last_copy_count()",
        )
        .await;
    assert_eq!(
        before, "0",
        "a session that has loaded nothing reports no rows loaded, got {before:?}"
    );

    for (index, (case, body, options, loaded)) in [
        ("four rows", "1,a\n2,b\n3,c\n4,d\n", "CSV", "4"),
        ("one row", "9,z\n", "CSV", "1"),
        ("a load that only checks the object", "1,a\n2,b\n", "CSV NOLOAD", "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let table = format!("counted_{index}");
        let key = format!("counted-{index}.csv");
        harness.put_object(BUCKET, &key, body).await;
        harness
            .run_ok(
                case,
                cluster,
                &format!("CREATE TABLE {table} (id INTEGER, name VARCHAR(8))"),
            )
            .await;
        // ONE submission, because the counter belongs to the CONNECTION the
        // load and the read of it share.
        let reported = harness
            .one_value(
                case,
                cluster,
                &format!(
                    "COPY {table} FROM 's3://{BUCKET}/{key}' IAM_ROLE '{role}' {options}; \
                     SELECT pg_last_copy_count()"
                ),
            )
            .await;
        assert_eq!(
            reported, loaded,
            "case {case:?}: the session must report the rows the load ingested"
        );
        // And nothing of it leaks to another connection: a submission of its
        // own is a connection of its own.
        let elsewhere = harness
            .one_value(case, cluster, "SELECT pg_last_copy_count()")
            .await;
        assert_eq!(
            elsewhere, "0",
            "case {case:?}: another connection has run no load, so it reports none"
        );
    }
}

/// One threshold case: the object, the `MAXERROR` the statement names, and what
/// the load must do.
struct ThresholdRow {
    case: &'static str,
    /// The object's body; the third field is an `INTEGER` column.
    body: &'static str,
    /// The `MAXERROR` clause, or empty for a load that names none.
    maxerror: &'static str,
    /// Whether the load must FAIL and commit nothing.
    fails: bool,
    /// The rows that must be in the table afterwards.
    loaded: i64,
    /// The rejected rows that must be recorded, by the line of the object each
    /// one is.
    rejected_lines: &'static [i64],
}

/// `MAXERROR` is the number of rejected rows at which a load fails: reaching it
/// fails the load and commits nothing, and anything below it skips the rejected
/// rows and loads the rest.
///
/// The rows differ in how many rows the object has the loader cannot convert and
/// in the threshold the statement names — none named at all, one at the number
/// of bad rows, one above it, and one that tolerates more than the object breaks
/// — so the rule is asserted over the threshold rather than at one value of it.
#[tokio::test]
async fn maxerror_decides_whether_a_load_with_rejected_rows_lands() {
    let harness = Harness::new();
    let cluster = "load-threshold";
    harness.create_cluster(cluster).await;
    harness.create_bucket(BUCKET).await;
    let role = granting_role(&harness, "load-threshold-role").await;

    /// Ten lines whose third field is not an integer on lines 3 and 7.
    const TEN_LINES_TWO_BAD: &str = "1,a,10\n2,b,20\n3,c,thirty\n4,d,40\n5,e,50\n6,f,60\n\
                                     7,g,seventy\n8,h,80\n9,i,90\n10,j,100\n";

    /// Three lines whose third field is not an integer on line 2.
    const THREE_LINES_ONE_BAD: &str = "1,a,10\n2,b,twenty\n3,c,30\n";

    let rows = [
        ThresholdRow {
            case: "no threshold named, so the first rejected row fails the load",
            body: THREE_LINES_ONE_BAD,
            maxerror: "",
            fails: true,
            loaded: 0,
            rejected_lines: &[2],
        },
        ThresholdRow {
            case: "a threshold of zero, which is the default written out",
            body: THREE_LINES_ONE_BAD,
            maxerror: " MAXERROR 0",
            fails: true,
            loaded: 0,
            rejected_lines: &[2],
        },
        ThresholdRow {
            case: "one rejected row reaching a threshold of one",
            body: THREE_LINES_ONE_BAD,
            maxerror: " MAXERROR 1",
            fails: true,
            loaded: 0,
            rejected_lines: &[2],
        },
        ThresholdRow {
            case: "one rejected row below a threshold of two",
            body: THREE_LINES_ONE_BAD,
            maxerror: " MAXERROR 2",
            fails: false,
            loaded: 2,
            rejected_lines: &[2],
        },
        ThresholdRow {
            case: "two rejected rows reaching a threshold of two",
            body: TEN_LINES_TWO_BAD,
            maxerror: " MAXERROR 2",
            fails: true,
            loaded: 0,
            rejected_lines: &[3, 7],
        },
        ThresholdRow {
            case: "two rejected rows below a threshold of three",
            body: TEN_LINES_TWO_BAD,
            maxerror: " MAXERROR 3",
            fails: false,
            loaded: 8,
            rejected_lines: &[3, 7],
        },
        ThresholdRow {
            case: "a threshold with nothing to tolerate",
            body: "1,a,10\n2,b,20\n",
            maxerror: " MAXERROR 5",
            fails: false,
            loaded: 2,
            rejected_lines: &[],
        },
        ThresholdRow {
            case: "the noise word the clause admits",
            body: THREE_LINES_ONE_BAD,
            maxerror: " MAXERROR AS 2",
            fails: false,
            loaded: 2,
            rejected_lines: &[2],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let table = format!("threshold_{index}");
        let key = format!("threshold-{index}.csv");
        harness.put_object(BUCKET, &key, row.body).await;
        harness
            .run_ok(
                row.case,
                cluster,
                &format!("CREATE TABLE {table} (id INTEGER, name VARCHAR(32), qty INTEGER)"),
            )
            .await;
        let statement = format!(
            "COPY {table} (id, name, qty) FROM 's3://{BUCKET}/{key}' IAM_ROLE '{role}' CSV{}",
            row.maxerror
        );
        if row.fails {
            let error = harness.run_failing(row.case, cluster, &statement).await;
            assert_eq!(
                error,
                format!(
                    "ERROR: Load into table '{table}' failed.  Check 'stl_load_errors' system \
                     table for details."
                ),
                "case {:?}: a failed load points the client at the load errors",
                row.case
            );
        } else {
            harness.run_ok(row.case, cluster, &statement).await;
        }
        let count = harness
            .one_value(
                row.case,
                cluster,
                &format!("SELECT count(*) FROM {table}"),
            )
            .await;
        assert_eq!(
            count,
            row.loaded.to_string(),
            "case {:?}: the rows the load committed must be these",
            row.case
        );
        let recorded = harness
            .query_rows(
                row.case,
                cluster,
                &format!(
                    "SELECT line_number, colname, raw_field_value FROM stl_load_errors \
                     WHERE filename LIKE '%{key}%' ORDER BY line_number"
                ),
            )
            .await;
        let lines: Vec<String> = recorded
            .iter()
            .map(|cells| field_text(&cells.first().cloned().unwrap_or(Value::Null)))
            .collect();
        let expected: Vec<String> = row
            .rejected_lines
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            lines, expected,
            "case {:?}: every row the loader could not convert must be recorded, got {recorded:?}",
            row.case
        );
        for cells in &recorded {
            assert_eq!(
                field_text(&cells.get(1).cloned().unwrap_or(Value::Null)),
                "qty",
                "case {:?}: the load error must name the column that rejected the value",
                row.case
            );
            assert!(
                !field_text(&cells.get(2).cloned().unwrap_or(Value::Null)).is_empty(),
                "case {:?}: the load error must carry the field value it rejected, got {cells:?}",
                row.case
            );
        }
    }
}

/// A role allowed to read the objects these loads name.
async fn granting_role(harness: &Harness, name: &str) -> String {
    create_role(
        harness,
        name,
        Some(&allow_policy(
            &["s3:GetObject", "s3:ListBucket"],
            &[
                &format!("arn:aws:s3:::{BUCKET}"),
                &format!("arn:aws:s3:::{BUCKET}/*"),
            ],
        )),
    )
    .await
}
