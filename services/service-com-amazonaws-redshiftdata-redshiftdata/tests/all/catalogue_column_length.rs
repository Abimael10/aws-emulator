// SPDX-License-Identifier: CC0-1.0

//! `ColumnMetadata.length` across the two surfaces that report a column,
//! driven end-to-end over the wire: every call below is a real `POST /` with
//! `X-Amz-Target: RedshiftData.<Operation>` dispatched through the router.
//!
//! The RULE under test: on the CATALOGUE surface (`DescribeTable`) a column's
//! `length` — "the length of the column" — is the width that column's
//! declaration gives it, the same width `precision` carries for it; on the
//! DATA-PATH surface (`GetStatementResult`), which describes whatever the
//! statement produced rather than a declaration, `length` is 0.
//!
//! The declared widths below come from the Amazon Redshift *Data types*
//! documentation
//! (<https://docs.aws.amazon.com/redshift/latest/dg/c_Supported_data_types.html>),
//! whose per-type rows publish each type's range, resolution and default size:
//! the digits of an integer range, the declared `(n)` of a character type and
//! its default length when none is declared, the declared precision of an exact
//! numeric and its default of 18, and the widest rendering of a datetime value.

use serde_json::{json, Value};

use super::emulator::{list, number, Emulator};

/// One row of the declaration table: how a column is declared, the type name it
/// resolves to, and the width the catalogue must report for it.
struct WidthRow {
    declared: &'static str,
    type_name: &'static str,
    width: i64,
}

/// The declaration table, spanning what the rule generalises over — the
/// integer widths, the exact and approximate numerics, the character types
/// bounded and unbounded, a datetime, a binary and a boolean one — and
/// deliberately including declarations the recorded case did not name.
const WIDTHS: &[WidthRow] = &[
    // The integer ranges, in digits: -32768..32767, -2147483648..2147483647 and
    // -9223372036854775808..9223372036854775807.
    WidthRow { declared: "SMALLINT", type_name: "int2", width: 5 },
    WidthRow { declared: "INT", type_name: "int4", width: 10 },
    WidthRow { declared: "BIGINT", type_name: "int8", width: 19 },
    // An exact numeric carries its declared precision, and the documented
    // default precision of 18 when it declares none.
    WidthRow { declared: "DECIMAL(9,3)", type_name: "numeric", width: 9 },
    WidthRow { declared: "NUMERIC(38,10)", type_name: "numeric", width: 38 },
    WidthRow { declared: "DECIMAL", type_name: "numeric", width: 18 },
    // The approximate numerics carry their significant digits, 6 and 15.
    WidthRow { declared: "REAL", type_name: "float4", width: 6 },
    WidthRow { declared: "DOUBLE PRECISION", type_name: "float8", width: 15 },
    // A character type carries its declared width, and its documented default
    // length — 1 for CHAR, 256 for VARCHAR — when it declares none.
    WidthRow { declared: "VARCHAR(32)", type_name: "varchar", width: 32 },
    WidthRow { declared: "VARCHAR(65535)", type_name: "varchar", width: 65535 },
    WidthRow { declared: "VARCHAR", type_name: "varchar", width: 256 },
    WidthRow { declared: "CHAR(8)", type_name: "bpchar", width: 8 },
    WidthRow { declared: "CHAR", type_name: "bpchar", width: 1 },
    // A boolean renders as one character; a timestamp as its widest value at
    // the microsecond resolution the datetime types hold; VARBYTE declared with
    // no size takes its documented default of 64000 bytes.
    WidthRow { declared: "BOOLEAN", type_name: "bool", width: 1 },
    WidthRow { declared: "DATE", type_name: "date", width: 13 },
    WidthRow { declared: "TIMESTAMP", type_name: "timestamp", width: 29 },
    WidthRow { declared: "VARBYTE", type_name: "varbyte", width: 64000 },
];

/// The connection every statement in this suite runs through.
fn cluster() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// The catalogue reports each column's declared width as its `length`, and the
/// result set for the very same columns reports 0.
#[tokio::test]
async fn the_catalogue_reports_each_column_declared_width_and_the_result_set_reports_none() {
    let emulator = Emulator::new();

    // One table per declaration, so a column's width cannot be read off its
    // neighbour, and every column of every table is described in one request.
    for (index, row) in WIDTHS.iter().enumerate() {
        emulator
            .run(&format!(
                "CREATE TABLE widths{index} (col {})",
                row.declared
            ))
            .await;
    }

    for (index, row) in WIDTHS.iter().enumerate() {
        let case = row.declared;
        let table = format!("widths{index}");

        let mut request = cluster();
        request["Table"] = json!(table);
        let described = emulator.ok("DescribeTable", &request).await;
        let catalogued = list(&described, "ColumnList");
        assert_eq!(
            catalogued.len(),
            1,
            "case {case}: expected one catalogued column, got {described}"
        );
        let column = &catalogued[0];
        assert_eq!(
            column["typeName"], row.type_name,
            "case {case}: expected typeName {}, got {column}",
            row.type_name
        );
        assert_eq!(
            number(column, "length"),
            row.width,
            "case {case}: the catalogue reports the declared width as length, expected {}, got \
             {column}",
            row.width
        );
        // The width is the one `precision` carries for the same column, so the
        // two members cannot drift apart.
        assert_eq!(
            number(column, "length"),
            number(column, "precision"),
            "case {case}: length is the width precision carries, got {column}"
        );

        let id = emulator.run(&format!("SELECT col FROM {table}")).await;
        let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
        let read = list(&fetched, "ColumnMetadata");
        assert_eq!(
            read.len(),
            1,
            "case {case}: expected one described column, got {fetched}"
        );
        assert_eq!(
            number(&read[0], "length"),
            0,
            "case {case}: the data-path read reports no length, got {}",
            read[0]
        );
        assert_eq!(
            number(&read[0], "precision"),
            row.width,
            "case {case}: the data-path read still reports the width in precision, expected {}, \
             got {}",
            row.width,
            read[0]
        );
    }
}

/// Every column of a MULTI-column table reports its own width, in declaration
/// order — one table's answer is not one width repeated.
#[tokio::test]
async fn every_column_of_a_table_reports_its_own_declared_width() {
    let emulator = Emulator::new();
    emulator
        .run(
            "CREATE TABLE inventory (id INT, sku CHAR(12), name VARCHAR(64), \
             amt DECIMAL(9,3), weight REAL, added TIMESTAMP)",
        )
        .await;

    let mut request = cluster();
    request["Table"] = json!("inventory");
    let described = emulator.ok("DescribeTable", &request).await;
    let catalogued = list(&described, "ColumnList");
    let reported: Vec<(String, i64)> = catalogued
        .iter()
        .map(|column| {
            (
                column["name"].as_str().unwrap_or_default().to_string(),
                number(column, "length"),
            )
        })
        .collect();
    let expected: Vec<(String, i64)> = [
        ("id", 10),
        ("sku", 12),
        ("name", 64),
        ("amt", 9),
        ("weight", 6),
        ("added", 29),
    ]
    .iter()
    .map(|(name, width)| ((*name).to_string(), *width))
    .collect();
    assert_eq!(
        reported, expected,
        "case inventory: each column reports its own declared width, got {described}"
    );
}

/// A table of a schema of its own is described the same way: narrowing the
/// catalogue to a schema does not change what a column reports for its width.
#[tokio::test]
async fn a_column_of_a_named_schema_reports_its_declared_width() {
    let emulator = Emulator::new();
    emulator.run("CREATE SCHEMA sales").await;
    emulator
        .run("CREATE TABLE sales.regions (id BIGINT, name VARCHAR(16))")
        .await;
    // A same-named table of the default schema, declared with other widths, so
    // the answer cannot come from the table's name alone.
    emulator
        .run("CREATE TABLE regions (id SMALLINT, note CHAR(4))")
        .await;

    for (schema, expected) in [
        ("sales", vec![("id", 19), ("name", 16)]),
        ("public", vec![("id", 5), ("note", 4)]),
    ] {
        let mut request = cluster();
        request["Schema"] = json!(schema);
        request["Table"] = json!("regions");
        let described = emulator.ok("DescribeTable", &request).await;
        let reported: Vec<(String, i64)> = list(&described, "ColumnList")
            .iter()
            .map(|column| {
                (
                    column["name"].as_str().unwrap_or_default().to_string(),
                    number(column, "length"),
                )
            })
            .collect();
        let expected: Vec<(String, i64)> = expected
            .iter()
            .map(|(name, width)| ((*name).to_string(), *width))
            .collect();
        assert_eq!(
            reported, expected,
            "case {schema}: the columns of {schema}.regions report their own widths, got \
             {described}"
        );
    }
}
