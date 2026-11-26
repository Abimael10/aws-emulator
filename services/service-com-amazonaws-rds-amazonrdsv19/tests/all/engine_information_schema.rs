// SPDX-License-Identifier: CC0-1.0

//! The SQL-standard `information_schema` views a PostgreSQL engine endpoint's database
//! publishes — what every driver, ORM, migration tool and admin client reads the shape of the
//! database out of before it does anything else.
//!
//! RULE: PostgreSQL creates the information schema in EVERY database, so a session on a
//! PostgreSQL DB instance reads the database's own shape out of it: `information_schema.tables`
//! carries "one row for each table or view in the current database" (`table_type` `BASE TABLE`
//! for a table and `VIEW` for a view, `table_catalog` the database it belongs to) and
//! `information_schema.columns` "one row for each column in a table or view of the current
//! database", at the position its relation declares it, under the type name the catalog holds
//! that type under, with the length or the precision and scale the declaration carried,
//! whether it admits a null and the default it declares. Every row is read out of the
//! engine's own catalog, so a relation nothing created is in no answer and a view of the
//! standard this endpoint does not serve stays the undefined relation it is (SQLSTATE 42P01).
//!
//! The rows of each table below span what the rule generalises over: which view is read,
//! whether the relation is a table or a view, which SCHEMA the relation is reported in (the
//! client's own, or the one the server's catalogs live in), the declared type of the column
//! (each integer width, the character types with and without a length, an exact numeric, a
//! boolean, a timestamp and a type of the client's own), its nullability, its default, and a
//! relation and a view that are not there — asserted through the simple query protocol and
//! through the parameterised form a driver sends.
//!
//! Sources: PostgreSQL's "The Information Schema" for the two views and their columns, and its
//! "Data Types" chapter for the name the catalog reports a declared type under.

use crate::db_instance_engine_endpoint::{
    available_instance, postgres_login, PostgresSession, Rows, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::emulator;

/// PostgreSQL's own code for a statement naming a relation the database does not hold
/// ("undefined_table", its "Error Codes" appendix).
const SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

/// The schema a relation a client created is reported in.
const CLIENT_SCHEMA: &str = "public";

/// The statements the session runs before it reads the catalog: two tables whose columns
/// differ in type, in nullability and in whether they declare a default, and a view over one
/// of them.
const SEED: &[&str] = &[
    "CREATE TABLE sim_rows (id BIGINT PRIMARY KEY, label TEXT NOT NULL, qty INT)",
    "CREATE TABLE shapes (code VARCHAR(8) NOT NULL, price NUMERIC(9,2) DEFAULT 0, \
     wide BIGINT, narrow SMALLINT, flag BOOLEAN, seen TIMESTAMP, blobby BYTEA, \
     custom GEOMETRY)",
    "CREATE VIEW shape_codes AS SELECT code FROM shapes",
];

/// Open a session as the master user on the instance's published endpoint.
async fn session(address: &str, port: u16) -> PostgresSession {
    postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted")
}

/// The rows a statement that must be served answered with.
async fn served(case: &str, session: &mut PostgresSession, sql: &str) -> Rows {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must be served, got {refusal:?}"))
}

/// Every row of one column of an answer, as the server sent it (`""` for a NULL).
fn column(rows: &Rows) -> Vec<String> {
    rows.rows
        .iter()
        .map(|row| row.first().cloned().flatten().unwrap_or_default())
        .collect()
}

/// One row of the catalog table: a statement that reads a view, and the first column of every
/// record it must answer with.
struct Row {
    case: &'static str,
    sql: &'static str,
    expected: &'static [&'static str],
}

#[tokio::test]
async fn the_information_schema_reports_the_shape_of_the_database() {
    let router = emulator();
    let (address, port) =
        available_instance(&router, "emu-infoschema-pg", "postgres", &[]).await;
    let mut session = session(&address, port).await;
    for ddl in SEED {
        served("the seed", &mut session, ddl).await;
    }

    let rows: &[Row] = &[
        Row {
            case: "every relation the client created, in name order",
            sql: "SELECT table_name FROM information_schema.tables \
                  WHERE table_schema = 'public' ORDER BY table_name",
            expected: &["shape_codes", "shapes", "sim_rows"],
        },
        Row {
            case: "a table reports itself as a base table",
            sql: "SELECT table_type FROM information_schema.tables \
                  WHERE table_name = 'sim_rows'",
            expected: &["BASE TABLE"],
        },
        Row {
            case: "a view reports itself as a view",
            sql: "SELECT table_type FROM information_schema.tables \
                  WHERE table_name = 'shape_codes'",
            expected: &["VIEW"],
        },
        Row {
            case: "a relation nothing created is in no answer",
            sql: "SELECT table_name FROM information_schema.tables \
                  WHERE table_name = 'absent'",
            expected: &[],
        },
        Row {
            case: "the relation reports the database it belongs to",
            sql: "SELECT table_catalog FROM information_schema.tables \
                  WHERE table_name = 'shapes'",
            expected: &[DATABASE],
        },
        Row {
            case: "the server's own catalogs are relations of the catalog schema",
            sql: "SELECT table_schema FROM information_schema.tables \
                  WHERE table_name = 'pg_settings'",
            expected: &["pg_catalog"],
        },
        Row {
            case: "the catalog views report themselves as views of the information schema",
            sql: "SELECT table_schema, table_name FROM information_schema.tables \
                  WHERE table_name = 'columns'",
            expected: &["information_schema"],
        },
        Row {
            case: "the columns a table declares, in declaration order",
            sql: "SELECT column_name FROM information_schema.columns \
                  WHERE table_schema = 'public' AND table_name = 'sim_rows' \
                  ORDER BY ordinal_position",
            expected: &["id", "label", "qty"],
        },
        Row {
            case: "the type the catalog holds each declared type under",
            sql: "SELECT data_type FROM information_schema.columns \
                  WHERE table_name = 'sim_rows' ORDER BY ordinal_position",
            expected: &["bigint", "text", "integer"],
        },
        Row {
            case: "the catalog's own spelling of every other declared type",
            sql: "SELECT data_type FROM information_schema.columns \
                  WHERE table_name = 'shapes' ORDER BY ordinal_position",
            expected: &[
                "character varying",
                "numeric",
                "bigint",
                "smallint",
                "boolean",
                "timestamp without time zone",
                "bytea",
                // A type of the client's own, reported as it was declared.
                "geometry",
            ],
        },
        Row {
            case: "the position each column is declared at",
            sql: "SELECT ordinal_position FROM information_schema.columns \
                  WHERE table_name = 'sim_rows' ORDER BY ordinal_position",
            expected: &["1", "2", "3"],
        },
        Row {
            case: "a column declared NOT NULL admits no null",
            sql: "SELECT is_nullable FROM information_schema.columns \
                  WHERE table_name = 'sim_rows' AND column_name = 'label'",
            expected: &["NO"],
        },
        Row {
            case: "a column declared without NOT NULL admits one",
            sql: "SELECT is_nullable FROM information_schema.columns \
                  WHERE table_name = 'sim_rows' AND column_name = 'qty'",
            expected: &["YES"],
        },
        Row {
            case: "the default a column declares",
            sql: "SELECT column_default FROM information_schema.columns \
                  WHERE table_name = 'shapes' AND column_name = 'price'",
            expected: &["0"],
        },
        Row {
            case: "a column that declares no default",
            sql: "SELECT column_default FROM information_schema.columns \
                  WHERE table_name = 'shapes' AND column_name = 'wide'",
            expected: &[""],
        },
        Row {
            case: "the declared length of a character column",
            sql: "SELECT character_maximum_length FROM information_schema.columns \
                  WHERE table_name = 'shapes' AND column_name = 'code'",
            expected: &["8"],
        },
        Row {
            case: "a column of a type that carries no length",
            sql: "SELECT character_maximum_length FROM information_schema.columns \
                  WHERE table_name = 'shapes' AND column_name = 'wide'",
            expected: &[""],
        },
        Row {
            case: "the precision and scale of an exact numeric column",
            sql: "SELECT numeric_precision, numeric_scale FROM information_schema.columns \
                  WHERE table_name = 'shapes' AND column_name = 'price'",
            expected: &["9"],
        },
        Row {
            case: "the columns of a view are the columns of its query",
            sql: "SELECT column_name FROM information_schema.columns \
                  WHERE table_name = 'shape_codes' ORDER BY ordinal_position",
            expected: &["code"],
        },
        Row {
            case: "a column of a relation nothing created is in no answer",
            sql: "SELECT column_name FROM information_schema.columns \
                  WHERE table_name = 'absent'",
            expected: &[],
        },
        Row {
            case: "a count over the catalog",
            sql: "SELECT count(*) FROM information_schema.columns \
                  WHERE table_schema = 'public' AND table_name = 'sim_rows'",
            expected: &["3"],
        },
        Row {
            case: "the view is addressable in any letter case",
            sql: "SELECT table_name FROM INFORMATION_SCHEMA.TABLES \
                  WHERE table_name = 'sim_rows'",
            expected: &["sim_rows"],
        },
    ];

    for row in rows {
        let answered = served(row.case, &mut session, row.sql).await;
        let reported = column(&answered);
        assert_eq!(
            reported, row.expected,
            "case {:?}: expected {:?}, got {reported:?} from {:?}",
            row.case, row.expected, row.sql
        );
    }

    // The scale of the exact numeric column, read beside the precision above.
    let numeric = served(
        "the scale of an exact numeric column",
        &mut session,
        "SELECT numeric_scale FROM information_schema.columns \
         WHERE table_name = 'shapes' AND column_name = 'price'",
    )
    .await;
    assert_eq!(
        column(&numeric),
        vec!["2".to_string()],
        "case \"the scale of an exact numeric column\": got {numeric:?}"
    );

    // The form a DRIVER sends: the relation and its columns asked for with the table name
    // BOUND rather than written into the statement, which is how every introspecting client
    // reads a table's columns.
    let described = session
        .query_with_params(
            "SELECT column_name, data_type FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position",
            &[CLIENT_SCHEMA, "sim_rows"],
        )
        .await
        .unwrap_or_else(|refusal| {
            panic!("case \"the parameterised catalog read\": must be served, got {refusal:?}")
        });
    let pairs: Vec<(String, String)> = described
        .rows
        .iter()
        .map(|row| {
            (
                row.first().cloned().flatten().unwrap_or_default(),
                row.get(1).cloned().flatten().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("id".to_string(), "bigint".to_string()),
            ("label".to_string(), "text".to_string()),
            ("qty".to_string(), "integer".to_string()),
        ],
        "case \"the parameterised catalog read\": expected one row per column of the table, \
         got {described:?}"
    );

    // A relation the client DROPS leaves the catalog with it, and one it adds appears: the
    // view is the catalog as it stands, not as it stood when a session first read it.
    served(
        "a relation added after the catalog was read",
        &mut session,
        "CREATE TABLE added_later (only_column INT)",
    )
    .await;
    served(
        "a relation dropped after the catalog was read",
        &mut session,
        "DROP TABLE sim_rows",
    )
    .await;
    let after = served(
        "the catalog after the schema changed",
        &mut session,
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .await;
    assert_eq!(
        column(&after),
        vec![
            "added_later".to_string(),
            "shape_codes".to_string(),
            "shapes".to_string()
        ],
        "case \"the catalog after the schema changed\": the view reports the relations the \
         database holds NOW, got {after:?}"
    );

    // A view of the standard this endpoint does not serve is the relation the database holds
    // none of, in PostgreSQL's own words — never an invented row.
    let refusal = session
        .query("SELECT constraint_name FROM information_schema.table_constraints")
        .await
        .expect_err("a view this endpoint does not serve must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        (
            SQLSTATE_UNDEFINED_TABLE,
            "relation \"information_schema.table_constraints\" does not exist"
        ),
        "case \"a view of the standard this endpoint does not serve\": expected the \
         undefined-relation refusal, got {refusal:?}"
    );
}
