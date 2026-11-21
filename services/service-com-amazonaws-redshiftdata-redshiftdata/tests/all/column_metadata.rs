// SPDX-License-Identifier: CC0-1.0

//! What `ColumnMetadata` reports for a column, driven end-to-end over the wire:
//! every call below is a real `POST /` with `X-Amz-Target:
//! RedshiftData.<Operation>` dispatched through the router.
//!
//! The RULE under test: a column is described from its TYPE and from its
//! ORIGIN, and the same description serves a result set (`GetStatementResult`)
//! and a table of the catalog (`DescribeTable`).
//!
//! * `precision` is "the precision value of a decimal number column, or the
//!   column length for a non-numeric column": the size the column's type
//!   declares, that type's documented default size when the declaration named
//!   none, and the width of the type's widest value for a type that declares no
//!   size at all. A computed column's type is the one the statement CASTS it to.
//! * `length` carries nothing — the width is reported in `precision`.
//! * `scale` is the declared number of decimal places of an exact-numeric
//!   column, and the fractional-seconds precision of a datetime one.
//! * `nullable` is 0 for a column its relation declares `NOT NULL` and 1 for
//!   every other column.
//! * `columnDefault` is the default its relation declares for the column.
//! * `schemaName` and `tableName` name the relation a column is read from, and
//!   are absent for a column no relation includes.
//!
//! The expected sizes come from the Amazon Redshift *Data types* documentation
//! (<https://docs.aws.amazon.com/redshift/latest/dg/c_Supported_data_types.html>),
//! whose per-type rows publish each type's range, resolution and default size.

use serde_json::{json, Value};

use super::emulator::{list, number, Emulator};

/// The schema this emulator's engine holds every table of a database in, which
/// is the schema a column's metadata names.
const DEFAULT_SCHEMA: &str = "public";

/// One row of the type table: how a column is declared, and the `precision` and
/// `scale` that declaration calls for.
struct TypeRow {
    declared: &'static str,
    precision: i64,
    scale: i64,
}

/// The type table, spanning the families the rule generalises over: integers of
/// each width, the exact and the approximate numerics, the character types
/// bounded and unbounded, the datetime types with and without a zone, and the
/// binary and boolean ones. None of these is the declaration the issue was
/// recorded against alone.
const TYPES: &[TypeRow] = &[
    // The integer ranges, in digits.
    TypeRow { declared: "SMALLINT", precision: 5, scale: 0 },
    TypeRow { declared: "INTEGER", precision: 10, scale: 0 },
    TypeRow { declared: "BIGINT", precision: 19, scale: 0 },
    // An exact numeric carries its declared precision and scale; declared with
    // neither it carries the documented default precision of 18.
    TypeRow { declared: "DECIMAL(9,2)", precision: 9, scale: 2 },
    TypeRow { declared: "NUMERIC(12,4)", precision: 12, scale: 4 },
    TypeRow { declared: "DECIMAL(38,38)", precision: 38, scale: 38 },
    TypeRow { declared: "DECIMAL", precision: 18, scale: 0 },
    // The approximate numerics carry their significant digits.
    TypeRow { declared: "REAL", precision: 6, scale: 0 },
    TypeRow { declared: "DOUBLE PRECISION", precision: 15, scale: 0 },
    // A character type carries its declared width, and the documented default
    // length — 1 for CHAR, 256 for VARCHAR — when it declares none.
    TypeRow { declared: "CHAR(8)", precision: 8, scale: 0 },
    TypeRow { declared: "CHAR", precision: 1, scale: 0 },
    TypeRow { declared: "VARCHAR(32)", precision: 32, scale: 0 },
    TypeRow { declared: "VARCHAR(1)", precision: 1, scale: 0 },
    TypeRow { declared: "VARCHAR", precision: 256, scale: 0 },
    // The datetime types: the width of their widest rendering, at the
    // microsecond fractional-seconds precision they hold.
    TypeRow { declared: "DATE", precision: 13, scale: 0 },
    TypeRow { declared: "TIME", precision: 15, scale: 6 },
    TypeRow { declared: "TIMETZ", precision: 21, scale: 6 },
    TypeRow { declared: "TIMESTAMP", precision: 29, scale: 6 },
    TypeRow { declared: "TIMESTAMPTZ", precision: 35, scale: 6 },
    // A boolean renders as one character; VARBYTE declared with no size takes
    // its documented default of 64000 bytes.
    TypeRow { declared: "BOOLEAN", precision: 1, scale: 0 },
    TypeRow { declared: "VARBYTE", precision: 64000, scale: 0 },
];

/// The connection every statement in this suite runs through.
fn cluster() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// Assert the type facts of one column.
fn assert_type(case: &str, column: &Value, precision: i64, scale: i64) {
    assert_eq!(
        number(column, "precision"),
        precision,
        "case {case}: expected precision {precision}, got {column}"
    );
    assert_eq!(
        number(column, "scale"),
        scale,
        "case {case}: expected scale {scale}, got {column}"
    );
    assert_eq!(
        number(column, "length"),
        0,
        "case {case}: a column's width is reported in precision, so length carries 0, got {column}"
    );
}

/// A STORED column reports the size its declaration calls for, through both
/// readers of a column's metadata — the result set it appears in, and the
/// catalog entry of the table that declares it.
#[tokio::test]
async fn a_stored_column_reports_the_size_its_declaration_calls_for() {
    for row in TYPES {
        let case = row.declared;
        let emulator = Emulator::new();
        emulator
            .run(&format!("CREATE TABLE t (col {case})"))
            .await;

        let id = emulator.run("SELECT col FROM t").await;
        let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns.len(),
            1,
            "case {case}: expected one described column, got {fetched}"
        );
        assert_type(case, &columns[0], row.precision, row.scale);

        let mut request = cluster();
        request["Table"] = json!("t");
        let described = emulator.ok("DescribeTable", &request).await;
        let catalogued = list(&described, "ColumnList");
        assert_eq!(
            catalogued.len(),
            1,
            "case {case}: expected one catalogued column, got {described}"
        );
        assert_type(case, &catalogued[0], row.precision, row.scale);
        assert_eq!(
            catalogued[0], columns[0],
            "case {case}: the result set and the catalog describe the column the same way, got \
             {described}"
        );
    }
}

/// A COMPUTED column reports the size of the type the statement CASTS it to —
/// the same sizes a stored column of that declaration reports, since it is the
/// same declaration.
#[tokio::test]
async fn a_cast_column_reports_the_size_of_the_type_it_is_cast_to() {
    let emulator = Emulator::new();
    for row in TYPES {
        let case = row.declared;
        // Every type casts NULL, so one expression spans the whole table
        // without needing a value each type accepts.
        let id = emulator
            .run(&format!("SELECT CAST(NULL AS {case}) AS col"))
            .await;
        let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns.len(),
            1,
            "case {case}: expected one described column, got {fetched}"
        );
        assert_type(case, &columns[0], row.precision, row.scale);
    }

    // And the `::` spelling of the same cast is the same cast.
    let id = emulator.run("SELECT 'x'::VARCHAR(8) AS col").await;
    let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
    let columns = list(&fetched, "ColumnMetadata");
    assert_type("'x'::VARCHAR(8)", &columns[0], 8, 0);
}

/// One row of the origin table: a statement, and for each of its result columns
/// the table it must name (or `None`) and the `nullable` it must report.
struct OriginRow {
    label: &'static str,
    sql: &'static str,
    expected: &'static [(Option<&'static str>, i64)],
}

/// `schemaName`/`tableName` name the relation a column is read from, and
/// `nullable` is 0 exactly for a column that relation declares `NOT NULL`.
#[tokio::test]
async fn a_column_read_from_a_relation_reports_that_relation_and_its_nullability() {
    let emulator = Emulator::new();
    emulator
        .run("CREATE TABLE orders (id INTEGER NOT NULL, note VARCHAR(32), total DECIMAL(9,2))")
        .await;
    emulator
        .run("CREATE TABLE shipments (order_id INTEGER NOT NULL, carrier VARCHAR(16))")
        .await;
    emulator.run("INSERT INTO orders VALUES (1, 'a', 2.50)").await;
    emulator
        .run("INSERT INTO shipments VALUES (1, 'post')")
        .await;

    // The rows differ in how the statement names its columns, how many
    // relations it reads, and whether a column belongs to a relation at all.
    let rows: &[OriginRow] = &[
        OriginRow {
            label: "bare column names",
            sql: "SELECT id, note, total FROM orders",
            expected: &[(Some("orders"), 0), (Some("orders"), 1), (Some("orders"), 1)],
        },
        OriginRow {
            label: "a wildcard",
            sql: "SELECT * FROM orders",
            expected: &[(Some("orders"), 0), (Some("orders"), 1), (Some("orders"), 1)],
        },
        OriginRow {
            label: "an aliased column keeps its origin",
            sql: "SELECT id AS order_id FROM orders",
            expected: &[(Some("orders"), 0)],
        },
        OriginRow {
            label: "a qualified column name",
            sql: "SELECT orders.note FROM orders",
            expected: &[(Some("orders"), 1)],
        },
        OriginRow {
            label: "a column of an aliased relation",
            sql: "SELECT o.id FROM orders AS o",
            expected: &[(Some("orders"), 0)],
        },
        OriginRow {
            label: "columns of two relations",
            sql: "SELECT orders.id, shipments.carrier FROM orders, shipments",
            expected: &[(Some("orders"), 0), (Some("shipments"), 1)],
        },
        OriginRow {
            label: "a computed column belongs to no relation",
            sql: "SELECT count(*) AS counted FROM orders",
            expected: &[(None, 1)],
        },
        OriginRow {
            label: "an expression over a column belongs to no relation",
            sql: "SELECT total + 1 AS bumped FROM orders",
            expected: &[(None, 1)],
        },
        OriginRow {
            label: "a cast of a NOT NULL column is a computed column",
            sql: "SELECT CAST(id AS VARCHAR(8)) AS text_id FROM orders",
            expected: &[(None, 1)],
        },
        OriginRow {
            label: "a statement that reads no relation",
            sql: "SELECT 1 AS one",
            expected: &[(None, 1)],
        },
    ];

    for row in rows {
        let label = row.label;
        let id = emulator.run(row.sql).await;
        let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns.len(),
            row.expected.len(),
            "case {label}: expected {} described columns, got {fetched}",
            row.expected.len()
        );
        for (index, (table, nullable)) in row.expected.iter().enumerate() {
            let column = &columns[index];
            assert_eq!(
                number(column, "nullable"),
                *nullable,
                "case {label}: column {index} must report nullable {nullable}, got {column}"
            );
            match table {
                Some(table) => assert_eq!(
                    (column["tableName"].clone(), column["schemaName"].clone()),
                    (json!(table), json!(DEFAULT_SCHEMA)),
                    "case {label}: column {index} is included by {table}, got {column}"
                ),
                None => assert_eq!(
                    (column["tableName"].clone(), column["schemaName"].clone()),
                    (Value::Null, Value::Null),
                    "case {label}: column {index} belongs to no relation, so it names none, got \
                     {column}"
                ),
            }
        }
    }

    // The catalog reports the same nullability for the same columns, since both
    // readers describe a column the same way.
    let mut request = cluster();
    request["Table"] = json!("orders");
    let described = emulator.ok("DescribeTable", &request).await;
    let catalogued: Vec<i64> = list(&described, "ColumnList")
        .iter()
        .map(|column| number(column, "nullable"))
        .collect();
    assert_eq!(
        catalogued,
        vec![0, 1, 1],
        "the catalog reports the declared nullability of each column, got {described}"
    );
}

/// One row of the default table: how a column is declared, and the
/// `columnDefault` that declaration calls for.
struct DefaultRow {
    declared: &'static str,
    column_default: Value,
}

/// `columnDefault` is "the default value of the column": the default the
/// relation declares for it, and nothing for a column that declares none.
#[tokio::test]
async fn a_column_reports_the_default_its_relation_declares() {
    let emulator = Emulator::new();

    // The rows differ in the kind of default declared — text, numeric, boolean,
    // an expression — and in whether one is declared at all.
    let rows: &[DefaultRow] = &[
        DefaultRow {
            declared: "VARCHAR(32) DEFAULT 'sim2real'",
            column_default: json!("'sim2real'"),
        },
        DefaultRow {
            declared: "INTEGER DEFAULT 7",
            column_default: json!("7"),
        },
        DefaultRow {
            declared: "INTEGER NOT NULL DEFAULT 0",
            column_default: json!("0"),
        },
        DefaultRow {
            declared: "BOOLEAN DEFAULT true",
            column_default: json!("true"),
        },
        DefaultRow {
            declared: "INTEGER",
            column_default: Value::Null,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.declared;
        let table = format!("defaulted{index}");
        emulator
            .run(&format!("CREATE TABLE {table} (col {case})"))
            .await;

        let id = emulator.run(&format!("SELECT col FROM {table}")).await;
        let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns[0]["columnDefault"], row.column_default,
            "case {case}: expected columnDefault {}, got {fetched}",
            row.column_default
        );

        let mut request = cluster();
        request["Table"] = json!(table);
        let described = emulator.ok("DescribeTable", &request).await;
        let catalogued = list(&described, "ColumnList");
        assert_eq!(
            catalogued[0]["columnDefault"], row.column_default,
            "case {case}: the catalog reports the same default, got {described}"
        );
    }
}

/// A relation of a schema of its own is described under THAT schema: the schema
/// a column reports is the schema that contains the table including it, through
/// both readers, and `Schema` narrows the catalogue to it.
#[tokio::test]
async fn a_column_of_a_schema_of_its_own_reports_that_schema() {
    let emulator = Emulator::new();
    emulator.run("CREATE SCHEMA sales").await;
    emulator
        .run("CREATE TABLE sales.regions (id INTEGER NOT NULL, name VARCHAR(16))")
        .await;
    // A table of the default schema, under the same bare name, so the two are
    // told apart by their schema and not by their name.
    emulator
        .run("CREATE TABLE regions (id INTEGER, note VARCHAR(8))")
        .await;

    // The result set: a column of the schema's table reports that schema.
    let id = emulator.run("SELECT id FROM sales.regions").await;
    let fetched = emulator.ok("GetStatementResult", &json!({"Id": id})).await;
    let columns = list(&fetched, "ColumnMetadata");
    assert_eq!(
        (
            columns[0]["schemaName"].clone(),
            columns[0]["tableName"].clone(),
            number(&columns[0], "nullable")
        ),
        (json!("sales"), json!("regions"), 0),
        "a column of sales.regions is included by regions in sales, got {fetched}"
    );

    // The catalogue: `Schema` picks which of the two same-named tables is
    // described, and the columns carry their own schema.
    for (schema, expected) in [("sales", vec!["id", "name"]), ("public", vec!["id", "note"])] {
        let mut request = cluster();
        request["Schema"] = json!(schema);
        request["Table"] = json!("regions");
        let described = emulator.ok("DescribeTable", &request).await;
        let names: Vec<String> = list(&described, "ColumnList")
            .iter()
            .map(|column| column["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            names, expected,
            "case {schema}: expected the columns of {schema}.regions, got {described}"
        );
        assert_eq!(
            described["TableName"], json!(format!("dev.{schema}.regions")),
            "case {schema}: the described path carries the schema it was asked about, so the \
             two same-named tables are told apart, got {described}"
        );
        for column in list(&described, "ColumnList") {
            assert_eq!(
                (column["schemaName"].clone(), column["tableName"].clone()),
                (json!(schema), json!("regions")),
                "case {schema}: every column names the table that includes it, got {column}"
            );
        }
    }

    // And a schema the database does not hold matches nothing, which is the
    // empty answer rather than a refusal.
    let mut request = cluster();
    request["Schema"] = json!("absent");
    request["Table"] = json!("regions");
    let described = emulator.ok("DescribeTable", &request).await;
    assert!(
        list(&described, "ColumnList").is_empty(),
        "a schema the database does not hold matches no column, got {described}"
    );
}
