// SPDX-License-Identifier: CC0-1.0

//! The data warehouse TYPES the engine behind this emulator does not carry: the
//! geometric types and the currency type, driven end-to-end over the wire.
//!
//! The RULE under test: a statement that denotes a value of one of the
//! warehouse's geometric types (`point`, `lseg`, `box`, `path`, `polygon`,
//! `circle`) or of its currency type (`money`) — written as that type's
//! constructor, as a typed literal or as a cast — runs, and the column it
//! projects reports the TYPE's own name and the VALUE's own printed form rather
//! than the text the statement carried.
//!
//! The printed form follows the type:
//!
//! * a coordinate of a point, a line segment, a rectangle, a circle or a
//!   polygon is reported as a double, so it carries a fractional part even when
//!   it is whole;
//! * a path is reported as the warehouse printed it, which is the plain decimal
//!   of each coordinate;
//! * a `box` holds the same pair of corners however a statement names them, so
//!   the corner with the greater coordinates is printed first;
//! * a `path` reports the openness its input carried;
//! * an amount of money is a number of cents, so it is reported under
//!   `doubleValue` and the currency symbol and thousands separators an amount
//!   may be written with are not part of it.
//!
//! The rows span what the rule generalises over — every one of the seven types,
//! each of the spellings a value can be written in (constructor, typed literal,
//! `::` cast, with and without the brackets the syntax makes optional), whole,
//! fractional and negative coordinates, an open and a closed path, and the ways
//! an amount of money is written — so no row is the statement the issue was
//! recorded against alone.

use serde_json::{json, Value};

use super::emulator::{flag, list, Emulator, Warehouses, WAIT_SECONDS};

/// One statement, and what the single column of its single row must be.
struct Row {
    label: &'static str,
    sql: &'static str,
    /// `ColumnMetadata.typeName`: the warehouse type the value is of.
    type_name: &'static str,
    /// The one cell the statement's one row must carry, as `GetStatementResult`
    /// reports it.
    expected: fn() -> Value,
}

/// Every geometric spelling and every currency spelling, in one table: the
/// class the rule is stated over.
fn rows() -> Vec<Row> {
    vec![
        // `point`: the constructor taking the two coordinates, the typed
        // literal, the cast, and the bare spelling the syntax admits.
        Row {
            label: "the point constructor",
            sql: "SELECT point(3, 4)",
            type_name: "point",
            expected: || json!({"stringValue": "(3.0,4.0)"}),
        },
        Row {
            label: "a point literal",
            sql: "SELECT point '(3,4)'",
            type_name: "point",
            expected: || json!({"stringValue": "(3.0,4.0)"}),
        },
        Row {
            label: "a point cast",
            sql: "SELECT '(3,4)'::point",
            type_name: "point",
            expected: || json!({"stringValue": "(3.0,4.0)"}),
        },
        Row {
            label: "a point written without its parentheses",
            sql: "SELECT point '3,4'",
            type_name: "point",
            expected: || json!({"stringValue": "(3.0,4.0)"}),
        },
        Row {
            label: "a point with negative and fractional coordinates",
            sql: "SELECT point(-1.5, 2.25)",
            type_name: "point",
            expected: || json!({"stringValue": "(-1.5,2.25)"}),
        },
        // `lseg`: its two endpoints, in the order the statement names them.
        Row {
            label: "an lseg literal",
            sql: "SELECT lseg '[(1,2),(8,9)]'",
            type_name: "lseg",
            expected: || json!({"stringValue": "[(1.0,2.0),(8.0,9.0)]"}),
        },
        Row {
            label: "the lseg constructor over two points",
            sql: "SELECT lseg(point(1, 2), point(8, 9))",
            type_name: "lseg",
            expected: || json!({"stringValue": "[(1.0,2.0),(8.0,9.0)]"}),
        },
        // `box`: the same pair of corners whichever order they are named in.
        Row {
            label: "a box literal",
            sql: "SELECT box '((2,3),(9,7))'",
            type_name: "box",
            expected: || json!({"stringValue": "(9.0,7.0),(2.0,3.0)"}),
        },
        Row {
            label: "a box whose corners are named the other way round",
            sql: "SELECT box '((9,7),(2,3))'",
            type_name: "box",
            expected: || json!({"stringValue": "(9.0,7.0),(2.0,3.0)"}),
        },
        Row {
            label: "a box written as four bare coordinates",
            sql: "SELECT box '2,3,9,7'",
            type_name: "box",
            expected: || json!({"stringValue": "(9.0,7.0),(2.0,3.0)"}),
        },
        Row {
            label: "the box constructor over two points",
            sql: "SELECT box(point(2, 3), point(9, 7))",
            type_name: "box",
            expected: || json!({"stringValue": "(9.0,7.0),(2.0,3.0)"}),
        },
        // `path`: open or closed, as its input said.
        Row {
            label: "an open path literal",
            sql: "SELECT path '[(1,1),(4,2),(6,5)]'",
            type_name: "path",
            expected: || json!({"stringValue": "[(1,1),(4,2),(6,5)]"}),
        },
        Row {
            label: "a closed path literal",
            sql: "SELECT path '((1,1),(4,2),(6,5))'",
            type_name: "path",
            expected: || json!({"stringValue": "((1,1),(4,2),(6,5))"}),
        },
        Row {
            label: "a path with a fractional coordinate",
            sql: "SELECT path '[(1.5,1),(4,2.25)]'",
            type_name: "path",
            expected: || json!({"stringValue": "[(1.5,1),(4,2.25)]"}),
        },
        // `polygon`: closed by definition.
        Row {
            label: "a polygon literal",
            sql: "SELECT polygon '((0,0),(4,0),(4,3),(0,3))'",
            type_name: "polygon",
            expected: || json!({"stringValue": "((0.0,0.0),(4.0,0.0),(4.0,3.0),(0.0,3.0))"}),
        },
        // `circle`: a centre and a radius.
        Row {
            label: "a circle literal",
            sql: "SELECT circle '<(5,6),4>'",
            type_name: "circle",
            expected: || json!({"stringValue": "<(5.0,6.0),4.0>"}),
        },
        Row {
            label: "the circle constructor over a centre and a radius",
            sql: "SELECT circle(point(5, 6), 4)",
            type_name: "circle",
            expected: || json!({"stringValue": "<(5.0,6.0),4.0>"}),
        },
        // `money`: an amount, reported as the number it is.
        Row {
            label: "a money cast",
            sql: "SELECT '256.78'::money",
            type_name: "money",
            expected: || json!({"doubleValue": 256.78}),
        },
        Row {
            label: "a money literal",
            sql: "SELECT money '256.78'",
            type_name: "money",
            expected: || json!({"doubleValue": 256.78}),
        },
        Row {
            label: "an amount written with its currency symbol and thousands separators",
            sql: "SELECT '$1,234.50'::money",
            type_name: "money",
            expected: || json!({"doubleValue": 1234.5}),
        },
        Row {
            label: "a negative amount written in parentheses",
            sql: "SELECT '(42.50)'::money",
            type_name: "money",
            expected: || json!({"doubleValue": -42.5}),
        },
        Row {
            label: "a negative amount written with a sign",
            sql: "SELECT '-$7'::money",
            type_name: "money",
            expected: || json!({"doubleValue": -7.0}),
        },
        Row {
            label: "an amount cast from a number rather than from text",
            sql: "SELECT 19.995::money",
            type_name: "money",
            expected: || json!({"doubleValue": 20.0}),
        },
    ]
}

/// Submit `sql` against one database, wait for it, and answer the record the
/// service reports for it — already in its terminal state.
async fn describe(emulator: &Emulator, sql: &str) -> Value {
    let submitted = emulator
        .execute_and_wait(&json!({
            "ClusterIdentifier": "my-cluster",
            "Database": "dev",
            "DbUser": "awsuser",
            "Sql": sql,
        }))
        .await;
    emulator
        .ok("DescribeStatement", &json!({"Id": submitted["Id"]}))
        .await
}

/// Each warehouse type's value RUNS, and is reported under the type's own name
/// with the value's own printed form.
#[tokio::test]
async fn a_warehouse_type_value_is_reported_by_its_type_and_its_printed_form() {
    let emulator = Emulator::new();
    for row in rows() {
        let label = row.label;
        let described = describe(&emulator, row.sql).await;
        assert_eq!(
            described["Status"], "FINISHED",
            "case {label:?}: {} must run on the warehouse's type system, got {described}",
            row.sql
        );
        assert!(
            flag(&described, "HasResultSet"),
            "case {label:?}: {} produces a result set, got {described}",
            row.sql
        );

        let fetched = emulator
            .ok("GetStatementResult", &json!({"Id": described["Id"]}))
            .await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns.len(),
            1,
            "case {label:?}: expected exactly one described column, got {fetched}"
        );
        assert_eq!(
            columns[0]["typeName"],
            json!(row.type_name),
            "case {label:?}: expected typeName {:?}, got {}",
            row.type_name,
            columns[0]
        );
        // The warehouse names such a column after the type itself, and reports
        // that name as both the column's name and its label.
        assert_eq!(
            columns[0]["name"],
            json!(row.type_name),
            "case {label:?}: expected the column to be named after its type, got {}",
            columns[0]
        );
        assert_eq!(
            columns[0]["label"], columns[0]["name"],
            "case {label:?}: a column reports one name as both name and label, got {}",
            columns[0]
        );
        // "A value that indicates whether the column contains currency values":
        // an amount of money does, a position in the plane does not.
        assert_eq!(
            flag(&columns[0], "isCurrency"),
            row.type_name == "money",
            "case {label:?}: isCurrency must say whether the column holds an amount of money, \
             got {}",
            columns[0]
        );

        let records = list(&fetched, "Records");
        assert_eq!(
            records.len(),
            1,
            "case {label:?}: expected one row, got {fetched}"
        );
        let cell = records[0][0].clone();
        let expected = (row.expected)();
        assert_eq!(
            cell, expected,
            "case {label:?}: expected {expected}, got {cell}"
        );
    }
}

/// A value of one of these types survives a ROUND TRIP through a stored column:
/// the column reports the type its declaration names, and the value read back
/// is the one that was written.
#[tokio::test]
async fn a_stored_column_of_a_warehouse_type_round_trips_its_value() {
    let emulator = Emulator::new();
    for ddl in [
        "CREATE TABLE places (label VARCHAR(16), spot point, area polygon, price money)",
        "INSERT INTO places VALUES ('depot', point(3, 4), polygon '((0,0),(4,0),(4,3))', \
         '$1,000.25'::money)",
    ] {
        let described = describe(&emulator, ddl).await;
        assert_eq!(
            described["Status"], "FINISHED",
            "{ddl} must finish before the stored value can be read back, got {described}"
        );
    }

    let described = describe(&emulator, "SELECT spot, area, price FROM places").await;
    assert_eq!(
        described["Status"], "FINISHED",
        "reading the stored columns back must finish, got {described}"
    );
    let fetched = emulator
        .ok("GetStatementResult", &json!({"Id": described["Id"]}))
        .await;
    let columns = list(&fetched, "ColumnMetadata");
    let names: Vec<&Value> = columns.iter().map(|column| &column["typeName"]).collect();
    assert_eq!(
        names,
        vec![&json!("point"), &json!("polygon"), &json!("money")],
        "a stored column reports the type its declaration names, got {fetched}"
    );
    let records = list(&fetched, "Records");
    assert_eq!(
        records.len(),
        1,
        "the one stored row must be read back, got {fetched}"
    );
    assert_eq!(
        records[0],
        json!([
            {"stringValue": "(3.0,4.0)"},
            {"stringValue": "((0.0,0.0),(4.0,0.0),(4.0,3.0))"},
            {"doubleValue": 1000.25},
        ]),
        "a stored value reads back as the value that was written, got {fetched}"
    );
}

/// Text that is not a value of the type it is read as is REFUSED, worded as the
/// warehouse words it: the type it was read as and the text it could not read.
///
/// The rows differ in how the input fails — a text carrying no coordinates at
/// all, too few and too many of them, a bracket mismatch that leaves the wrong
/// count, a circle whose radius is not a length, and an amount that is not a
/// number.
#[tokio::test]
async fn text_that_is_not_a_value_of_the_type_is_refused_as_the_warehouse_words_it() {
    let emulator = Emulator::new();
    let cases = [
        ("no coordinates at all", "SELECT point 'origin'", "point", "origin"),
        ("one coordinate where two are needed", "SELECT point '(3)'", "point", "(3)"),
        (
            "three coordinates where two are needed",
            "SELECT '(1,2,3)'::point",
            "point",
            "(1,2,3)",
        ),
        (
            "an lseg carrying one endpoint",
            "SELECT lseg '[(1,2)]'",
            "lseg",
            "[(1,2)]",
        ),
        (
            "a polygon carrying half a point",
            "SELECT polygon '((0,0),(4))'",
            "polygon",
            "((0,0),(4))",
        ),
        (
            "a circle whose radius is negative",
            "SELECT circle '<(5,6),-4>'",
            "circle",
            "<(5,6),-4>",
        ),
        (
            "a point whose bracket is not closed",
            "SELECT point '(3,4'",
            "point",
            "(3,4",
        ),
        (
            "an amount that is not a number",
            "SELECT 'free'::money",
            "money",
            "free",
        ),
    ];

    for (label, sql, type_name, text) in cases {
        let described = describe(&emulator, sql).await;
        assert_eq!(
            described["Status"], "FAILED",
            "case {label:?}: {sql} names no value of {type_name}, got {described}"
        );
        let error = described["Error"].as_str().unwrap_or_default();
        let expected = format!("invalid input syntax for type {type_name}: \"{text}\"");
        assert!(
            error.contains(&expected),
            "case {label:?}: expected the refusal to carry {expected:?}, got {error:?}"
        );
    }
}

/// The same rule on a SERVERLESS WORKGROUP: the engine's type system is the
/// warehouse's whichever compute target a statement names, so a workgroup
/// evaluates these values exactly as a provisioned cluster does.
#[tokio::test]
async fn a_serverless_workgroup_evaluates_the_same_warehouse_types() {
    let emulator = Warehouses::new();
    emulator.create_workgroup("geometry-workgroup").await;

    for row in rows() {
        let label = row.label;
        let submitted = emulator
            .data_ok(
                "ExecuteStatement",
                &json!({
                    "WorkgroupName": "geometry-workgroup",
                    "Database": "dev",
                    "Sql": row.sql,
                    "WaitTimeSeconds": WAIT_SECONDS,
                }),
            )
            .await;
        let described = emulator
            .data_ok("DescribeStatement", &json!({"Id": submitted["Id"]}))
            .await;
        assert_eq!(
            described["Status"], "FINISHED",
            "case {label:?}: {} must run on a workgroup's engine too, got {described}",
            row.sql
        );
        let fetched = emulator
            .data_ok("GetStatementResult", &json!({"Id": described["Id"]}))
            .await;
        let columns = list(&fetched, "ColumnMetadata");
        assert_eq!(
            columns[0]["typeName"],
            json!(row.type_name),
            "case {label:?}: expected typeName {:?} on a workgroup too, got {fetched}",
            row.type_name
        );
        let expected = (row.expected)();
        assert_eq!(
            list(&fetched, "Records")[0][0],
            expected,
            "case {label:?}: expected {expected} on a workgroup too, got {fetched}"
        );
    }
}
