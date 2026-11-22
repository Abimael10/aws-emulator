// SPDX-License-Identifier: CC0-1.0

//! The SQL SURFACE of the endpoint a provisioned cluster publishes: the dialect
//! a session on it speaks, the catalogue it reads the database's shape out of,
//! and the words a refusal carries.
//!
//! The rule under test: **a session on `Cluster$Endpoint` is served by AMAZON
//! REDSHIFT's SQL surface — the statements of this warehouse's dialect are run,
//! the server identifies itself as Amazon Redshift, the database's own
//! catalogue (`PG_TABLE_DEF`, `information_schema`) reports the columns the
//! database really holds as the warehouse names them, and a refusal carries the
//! `SQLSTATE` and the sentence the warehouse reports it under** — not the
//! surface of whatever SQL engine happens to be behind the endpoint.
//!
//! The client here is the one the sibling suite drives the wire with
//! (`cluster_engine_endpoint`), written against the protocol itself, so what
//! these cases exercise is what a stock driver (psql, libpq, psycopg, a JDBC
//! driver) sends — and everything runs in ONE process.
//!
//! The rows span what each rule generalises over: every spelling of a session
//! function a client may write bare, a `CREATE TABLE` carrying each storage
//! attribute of this dialect, the types a declaration may name and the catalogue
//! spelling of each, and the refusals a statement is answered with.

use crate::cluster_engine_endpoint::{
    available_cluster, endpoint_of, postgres_login, PostgresSession, Rows, Transport,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::cluster_lifecycle::Emulator;

/// The cluster every case below runs against, created once per test.
const DATABASE: &str = "devdb";

/// Open a session on a freshly created, serving cluster's published endpoint.
async fn session_on(emulator: &Emulator, identifier: &str) -> PostgresSession {
    let described = available_cluster(
        emulator,
        identifier,
        &[("ClusterType", "single-node"), ("DBName", DATABASE)],
    )
    .await;
    let (address, port) = endpoint_of(identifier, &described);
    postgres_login(
        identifier,
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| panic!("case {identifier}: the admin user must be admitted, got {refusal:?}"))
}

/// Run a statement that must succeed.
async fn run(session: &mut PostgresSession, case: &str, sql: &str) -> Rows {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must be served, got {refusal:?}"))
}

/// RULE: the session functions a PostgreSQL client writes as reserved words —
/// without parentheses — are answered from the session itself, and the sentence
/// `version()` answers names Amazon Redshift.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_functions_answer_from_the_session() {
    let emulator = Emulator::new();
    let mut session = session_on(&emulator, "sql-session-functions").await;

    // Every spelling PostgreSQL documents as callable without parentheses, and
    // what each one answers for THIS session: the user it was admitted as and
    // the database it is on.
    for (case, sql, expected) in [
        ("the bare user keyword", "SELECT current_user", MASTER_USER),
        ("the session user", "SELECT session_user", MASTER_USER),
        ("the current role", "SELECT current_role", MASTER_USER),
        ("the parenthesised call", "SELECT current_user()", MASTER_USER),
        ("the bare catalog keyword", "SELECT current_catalog", DATABASE),
        ("the current database", "SELECT current_database()", DATABASE),
    ] {
        let answered = run(&mut session, case, sql).await;
        assert_eq!(
            answered.only(),
            Some(expected),
            "case {case:?}: expected {sql:?} to answer {expected:?}, got {answered:?}"
        );
    }

    // A keyword inside a string literal is part of the literal, not a call.
    let quoted = run(&mut session, "a quoted keyword", "SELECT 'current_user'").await;
    assert_eq!(
        quoted.only(),
        Some("current_user"),
        "case a quoted keyword: expected the literal, got {quoted:?}"
    );

    let version = run(&mut session, "the version sentence", "SELECT version()").await;
    let sentence = version.only().unwrap_or_default().to_string();
    assert!(
        sentence.contains("Redshift"),
        "case the version sentence: the endpoint must identify as Amazon Redshift, got \
         {sentence:?}"
    );
    assert!(
        sentence.starts_with("PostgreSQL "),
        "case the version sentence: it must open with the PostgreSQL version the warehouse is \
         based on, got {sentence:?}"
    );
    // The `server_version` PARAMETER is the version itself, which is what a
    // driver parses; the sentence above is what `version()` answers.
    assert_eq!(
        session.parameter("server_version").as_deref(),
        Some(service_com_amazonaws_redshift_redshiftserviceversion20121201::state::CLUSTER_SERVER_VERSION),
        "case the version parameter: expected the version the endpoint publishes, got {:?}",
        session.parameter("server_version")
    );
}

/// One table a case declares, and what the catalogue must then report for its
/// columns.
struct ColumnRow {
    case: &'static str,
    /// The column's declaration, as a client of this dialect writes it.
    declared: &'static str,
    /// `PG_TABLE_DEF.type`: the type under the name the warehouse reports it by,
    /// with the modifier its declaration carried.
    reported: &'static str,
    /// `information_schema.columns.data_type`: the standard's own name, with the
    /// modifier reported beside it instead.
    data_type: &'static str,
    /// `character_maximum_length`, `numeric_precision` and `numeric_scale`.
    length: Option<&'static str>,
    precision: Option<&'static str>,
    scale: Option<&'static str>,
    /// Whether the declaration refuses a null.
    not_null: bool,
}

/// The column cases: one per type family a `CREATE TABLE` of this dialect may
/// name, in both the alias and the standard spelling, with and without a
/// modifier.
///
/// Source for the aliases and the type each one names: the Amazon Redshift *Data
/// types* reference.
fn column_rows() -> Vec<ColumnRow> {
    vec![
        ColumnRow {
            case: "an integer under its alias, NOT NULL",
            declared: "id INT NOT NULL",
            reported: "integer",
            data_type: "integer",
            length: None,
            precision: None,
            scale: None,
            not_null: true,
        },
        ColumnRow {
            case: "a big integer",
            declared: "total BIGINT",
            reported: "bigint",
            data_type: "bigint",
            length: None,
            precision: None,
            scale: None,
            not_null: false,
        },
        ColumnRow {
            case: "a character varying under its alias, with a length",
            declared: "label VARCHAR(64)",
            reported: "character varying(64)",
            data_type: "character varying",
            length: Some("64"),
            precision: None,
            scale: None,
            not_null: false,
        },
        ColumnRow {
            case: "an exact numeric under its alias, with precision and scale",
            declared: "amount DECIMAL(9,2)",
            reported: "numeric(9,2)",
            data_type: "numeric",
            length: None,
            precision: Some("9"),
            scale: Some("2"),
            not_null: false,
        },
        ColumnRow {
            case: "a boolean under its alias",
            declared: "active BOOL",
            reported: "boolean",
            data_type: "boolean",
            length: None,
            precision: None,
            scale: None,
            not_null: false,
        },
        ColumnRow {
            case: "a timestamp",
            declared: "seen TIMESTAMP",
            reported: "timestamp without time zone",
            data_type: "timestamp without time zone",
            length: None,
            precision: None,
            scale: None,
            not_null: false,
        },
    ]
}

/// RULE: a `CREATE TABLE` of this dialect — storage attributes and all — creates
/// the table it declares, and the database's own catalogue then reports that
/// table's columns: `PG_TABLE_DEF` with the types the warehouse names, and
/// `information_schema` with the standard's own names and modifiers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalogue_reports_the_columns_the_database_holds() {
    let emulator = Emulator::new();
    let mut session = session_on(&emulator, "sql-catalogue").await;
    let rows = column_rows();
    let declared: Vec<&str> = rows.iter().map(|row| row.declared).collect();
    // The storage attributes of this dialect, which say how a cluster stores the
    // table and nothing about what it holds: the table they are declared on is
    // created.
    let create = format!(
        "CREATE TABLE sales ({}) DISTSTYLE KEY DISTKEY(id) SORTKEY(id)",
        declared.join(", ")
    );
    run(&mut session, "a Redshift CREATE TABLE", &create).await;

    for row in &rows {
        let case = row.case;
        let column = row
            .declared
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let reported = run(
            &mut session,
            case,
            &format!(
                "SELECT type, \"notnull\" FROM pg_table_def WHERE tablename = 'sales' \
                 AND \"column\" = '{column}'"
            ),
        )
        .await;
        assert_eq!(
            reported.rows,
            vec![vec![
                Some(row.reported.to_string()),
                Some(if row.not_null { "t" } else { "f" }.to_string())
            ]],
            "case {case:?}: expected PG_TABLE_DEF to report {:?} for {column:?}, got {reported:?}",
            row.reported
        );

        let standard = run(
            &mut session,
            case,
            &format!(
                "SELECT data_type, character_maximum_length, numeric_precision, numeric_scale, \
                 is_nullable FROM information_schema.columns WHERE table_name = 'sales' \
                 AND column_name = '{column}'"
            ),
        )
        .await;
        assert_eq!(
            standard.rows,
            vec![vec![
                Some(row.data_type.to_string()),
                row.length.map(str::to_string),
                row.precision.map(str::to_string),
                row.scale.map(str::to_string),
                Some(if row.not_null { "NO" } else { "YES" }.to_string()),
            ]],
            "case {case:?}: expected information_schema.columns to report {:?} for {column:?}, \
             got {standard:?}",
            row.data_type
        );
    }

    // The catalogue reports the relations the database holds, and nothing of the
    // engine's own.
    let tables = run(
        &mut session,
        "the relations the database holds",
        "SELECT table_name, table_type FROM information_schema.tables ORDER BY table_name",
    )
    .await;
    assert_eq!(
        tables.rows,
        vec![vec![
            Some("sales".to_string()),
            Some("BASE TABLE".to_string())
        ]],
        "case the relations the database holds: expected the one table the session created, got \
         {tables:?}"
    );
}

/// RULE: a statement this warehouse REFUSES carries the `SQLSTATE` and the
/// sentence the warehouse reports it under — a refused row in its own
/// internal-error class, and everything else in the class the protocol names —
/// and the row is not in the table afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_carries_the_warehouses_own_wording() {
    let emulator = Emulator::new();
    let mut session = session_on(&emulator, "sql-refusals").await;
    run(
        &mut session,
        "the table a refusal is read against",
        "CREATE TABLE guarded (id INT NOT NULL, label VARCHAR(16) NOT NULL) DISTSTYLE EVEN",
    )
    .await;
    run(
        &mut session,
        "the row that is accepted",
        "INSERT INTO guarded (id, label) VALUES (1, 'kept')",
    )
    .await;

    for (case, sql, code, message) in [
        (
            "a null into a NOT NULL column",
            "INSERT INTO guarded (id, label) VALUES (2, NULL)",
            "XX000",
            Some("Cannot insert a NULL value into column label".to_string()),
        ),
        (
            "a null into another NOT NULL column",
            "INSERT INTO guarded (id, label) VALUES (NULL, 'x')",
            "XX000",
            Some("Cannot insert a NULL value into column id".to_string()),
        ),
        (
            "a relation the database does not hold",
            "SELECT * FROM absent_relation",
            "42P01",
            Some("relation \"absent_relation\" does not exist".to_string()),
        ),
        (
            "a table that is already there",
            "CREATE TABLE guarded (id INT)",
            "42710",
            None,
        ),
        (
            "a statement the engine cannot parse",
            "SELCT 1",
            "42601",
            Some("syntax error at or near \"SELCT\"".to_string()),
        ),
    ] {
        let refusal = session
            .query(sql)
            .await
            .expect_err(&format!("case {case:?}: {sql:?} must be refused"));
        assert_eq!(
            refusal.code, code,
            "case {case:?}: expected SQLSTATE {code}, got {refusal:?}"
        );
        if let Some(message) = message {
            assert_eq!(
                refusal.message, message,
                "case {case:?}: expected the warehouse's own sentence, got {refusal:?}"
            );
        }
    }

    // Every refused row is absent: the table holds what was accepted and
    // nothing else.
    let held = run(
        &mut session,
        "what the table holds",
        "SELECT id, label FROM guarded ORDER BY id",
    )
    .await;
    assert_eq!(
        held.rows,
        vec![vec![Some("1".to_string()), Some("kept".to_string())]],
        "case what the table holds: expected only the accepted row, got {held:?}"
    );
}

/// RULE: a transaction control with nothing to do is answered as this warehouse
/// answers it — a `BEGIN` while a transaction is in progress and a `COMMIT` or
/// `ROLLBACK` outside one change nothing and are not refused — and what a
/// session writes is readable by the next session on the same cluster.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transaction_control_with_nothing_to_do_is_not_refused() {
    let emulator = Emulator::new();
    let identifier = "sql-transactions";
    let described = available_cluster(
        &emulator,
        identifier,
        &[("ClusterType", "single-node"), ("DBName", DATABASE)],
    )
    .await;
    let (address, port) = endpoint_of(identifier, &described);
    let open = || async {
        postgres_login(
            identifier,
            &address,
            port,
            Transport::Plaintext,
            MASTER_USER,
            MASTER_PASSWORD,
            Some(DATABASE),
        )
        .await
        .unwrap_or_else(|refusal| panic!("case {identifier}: the admin user must be admitted, got {refusal:?}"))
    };

    let mut first = open().await;
    run(
        &mut first,
        "the table the sessions share",
        "CREATE TABLE ledger (id INT NOT NULL)",
    )
    .await;
    for (case, sql) in [
        ("a commit outside a transaction", "COMMIT"),
        ("a rollback outside a transaction", "ROLLBACK"),
        ("the transaction this session opens", "BEGIN"),
        ("a second begin inside it", "BEGIN"),
        ("the row it writes", "INSERT INTO ledger (id) VALUES (1)"),
        ("the commit that ends it", "COMMIT"),
        ("a rollback after that commit", "ROLLBACK"),
    ] {
        run(&mut first, case, sql).await;
    }
    // A second session of the same cluster reads the committed row: both
    // sessions are served by that cluster's own database.
    let mut second = open().await;
    let read = run(
        &mut second,
        "a second session's read",
        "SELECT id FROM ledger ORDER BY id",
    )
    .await;
    assert_eq!(
        read.rows,
        vec![vec![Some("1".to_string())]],
        "case a second session's read: expected the row the first session committed, got {read:?}"
    );
    // And a `BEGIN` on the second session, while the first is open, is answered
    // rather than refused.
    run(&mut second, "a begin on another session", "BEGIN").await;
    run(&mut second, "the rollback that ends it", "ROLLBACK").await;
}

/// RULE: a session's temporary relation belongs to the connection that created
/// it, and a statement of this dialect that names a schema the database holds
/// reaches that schema's own table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schema_and_a_temporary_relation_resolve_as_this_dialect_resolves_them() {
    let emulator = Emulator::new();
    let mut session = session_on(&emulator, "sql-schemas").await;
    for (case, sql) in [
        ("the schema the database holds", "CREATE SCHEMA reporting"),
        (
            "a table of that schema",
            "CREATE TABLE reporting.totals (id INT NOT NULL, amount DECIMAL(9,2))",
        ),
        (
            "the row it holds",
            "INSERT INTO reporting.totals (id, amount) VALUES (1, 2.50)",
        ),
        (
            "a table of the default schema with the same name",
            "CREATE TABLE totals (id INT NOT NULL)",
        ),
        (
            "a temporary relation of this connection",
            "CREATE TEMP TABLE staging (id INT NOT NULL)",
        ),
        ("the row the temporary holds", "INSERT INTO staging (id) VALUES (7)"),
    ] {
        run(&mut session, case, sql).await;
    }

    let qualified = run(
        &mut session,
        "the qualified read",
        "SELECT id FROM reporting.totals ORDER BY id",
    )
    .await;
    assert_eq!(
        qualified.rows,
        vec![vec![Some("1".to_string())]],
        "case the qualified read: expected the row of the schema's own table, got {qualified:?}"
    );
    let unqualified = run(
        &mut session,
        "the unqualified read",
        "SELECT count(*) FROM totals",
    )
    .await;
    assert_eq!(
        unqualified.only(),
        Some("0"),
        "case the unqualified read: the table of the default schema is a table of its own, got \
         {unqualified:?}"
    );
    let temporary = run(
        &mut session,
        "the temporary read",
        "SELECT id FROM staging",
    )
    .await;
    assert_eq!(
        temporary.only(),
        Some("7"),
        "case the temporary read: expected the row this connection's temporary relation holds, \
         got {temporary:?}"
    );
    // The catalogue reports the schema's table under its own schema, and the
    // connection's temporary relation is not a relation of the database's
    // catalogue at all.
    let catalogued = run(
        &mut session,
        "the catalogue of the schemas",
        "SELECT table_schema, table_name FROM information_schema.tables \
         ORDER BY table_schema, table_name",
    )
    .await;
    assert_eq!(
        catalogued.rows,
        vec![
            vec![Some("public".to_string()), Some("totals".to_string())],
            vec![Some("reporting".to_string()), Some("totals".to_string())],
        ],
        "case the catalogue of the schemas: expected the two tables under their own schemas, got \
         {catalogued:?}"
    );
    // Creating a schema the database already holds is refused, and dropping one
    // that still holds objects is too.
    for (case, sql, code) in [
        (
            "a schema that is already there",
            "CREATE SCHEMA reporting",
            "42710",
        ),
        (
            "a schema that still holds objects",
            "DROP SCHEMA reporting",
            "XX000",
        ),
        (
            "a schema the database does not hold",
            "DROP SCHEMA nowhere",
            "42P01",
        ),
    ] {
        let refusal = session
            .query(sql)
            .await
            .expect_err(&format!("case {case:?}: {sql:?} must be refused"));
        assert_eq!(
            refusal.code, code,
            "case {case:?}: expected SQLSTATE {code}, got {refusal:?}"
        );
    }
    // `IF NOT EXISTS` and `CASCADE` are the forms that are not refused.
    run(
        &mut session,
        "a schema that is already there, with IF NOT EXISTS",
        "CREATE SCHEMA IF NOT EXISTS reporting",
    )
    .await;
    run(
        &mut session,
        "the schema and its objects",
        "DROP SCHEMA reporting CASCADE",
    )
    .await;
    let dropped = session
        .query("SELECT id FROM reporting.totals")
        .await
        .expect_err("case the dropped schema: its table must be gone");
    assert_eq!(
        dropped.code, "42P01",
        "case the dropped schema: expected the undefined-relation class, got {dropped:?}"
    );
}
