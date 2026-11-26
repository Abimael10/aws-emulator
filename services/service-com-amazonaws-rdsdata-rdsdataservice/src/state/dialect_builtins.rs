// SPDX-License-Identifier: CC0-1.0

//! The BUILT-IN FUNCTIONS of the engine a DB cluster RUNS, defined on the engine behind this
//! data plane.
//!
//! `ExecuteStatement` "runs a SQL statement against a database" of the cluster the call
//! addresses, so the statement is read in the dialect of the engine that cluster runs: a
//! function that engine's own reference documents as a built-in is a function the statement
//! may call, and the engine behind this data plane — which is neither of those products — has
//! none of them. This module is where they are given to it, and nothing else is: every value
//! below is computed by the engine, from this process's clock or from the DATABASE the call
//! addressed, and nothing from the statement's text.
//!
//! Two kinds live here:
//!
//! * the DATE AND TIME functions, which are the one table every surface of a cluster reads
//!   ([`::service_common::sql_dialect_time`]) — the cluster's engine endpoints serve exactly
//!   the same built-ins, so the two doors cannot disagree about which ones that cluster has;
//! * the SYSTEM INFORMATION functions that answer "which database am I in", whose value is
//!   the call's own `database` member and so belongs to this front end.
//!
//! A function belongs to a DIALECT: `DATABASE()` is the MySQL family's and no PostgreSQL
//! server has it, `current_database()` is PostgreSQL's and no MySQL server has it. So a call
//! of the OTHER family's function is refused as the engine refuses an undefined function, and
//! the caller is told it in the words of the engine the cluster runs
//! ([`super::Database::in_the_engines_words`]) — never in the backing engine's.

use ::rusqlite::functions::FunctionFlags;
use ::rusqlite::types::Value;
use ::rusqlite::Connection;
use ::service_common::sql_dialect_time::{
    self as dialect_time, SqlEngineFamily, UNDEFINED_FUNCTION_PREFIX,
};

use super::EngineFamily;

/// What a system-information function answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The DATABASE the statement runs against, under the name the cluster holds it by.
    Database,
    /// The SCHEMA an unqualified name is resolved in: `public` on PostgreSQL, which creates
    /// every database with it, and the database itself on MySQL, where a schema IS a
    /// database.
    Schema,
}

/// The system-information functions these engines define: the name a client calls, which
/// family defines it and what it answers.
///
/// Sources: the PostgreSQL documentation's "System Information Functions and Operators" —
/// `current_database()` ("name of current database") and `current_schema()` ("name of current
/// schema") — and the MySQL Reference Manual's "Information Functions": `DATABASE()` ("returns
/// the default (current) database name") and `SCHEMA()`, "a synonym for DATABASE()".
const INFORMATION_FUNCTIONS: &[(&str, SqlEngineFamily, Answer)] = &[
    (
        "current_database",
        SqlEngineFamily::PostgreSql,
        Answer::Database,
    ),
    (
        "current_schema",
        SqlEngineFamily::PostgreSql,
        Answer::Schema,
    ),
    ("database", SqlEngineFamily::MySql, Answer::Database),
    ("schema", SqlEngineFamily::MySql, Answer::Schema),
];

/// Whether `function` is one of the built-ins this module defines, whichever family defines
/// it — which is what admits it through the data plane's own function allow-list
/// ([`super::confine_to_data_plane`]), a call the family does not define being refused by the
/// definition itself rather than by that list.
#[must_use]
pub(super) fn is_engine_builtin(function: &str) -> bool {
    dialect_time::is_time_function(function)
        || INFORMATION_FUNCTIONS
            .iter()
            .any(|(name, ..)| name.eq_ignore_ascii_case(function))
}

/// Define every built-in of the engine `engine` on `conn`, for a connection serving the
/// database `database` (empty for the one a call that names none addresses).
///
/// Called where a database's connection is opened and again when the control plane answers
/// which engine the cluster runs ([`super::Database::runs_engine`]): defining a function again
/// replaces the previous definition, so a connection carries exactly one family's built-ins at
/// a time.
pub(super) fn register_engine_builtins(
    conn: &Connection,
    engine: EngineFamily,
    database: &str,
) -> ::rusqlite::Result<()> {
    dialect_time::register_time_functions(conn, move || Some(engine.dialect()))?;
    for (name, family, answer) in INFORMATION_FUNCTIONS {
        let name = *name;
        let family = *family;
        let answer = *answer;
        // The value is the DATABASE the call addressed, resolved once here: it is a fact of
        // the connection, not of the statement.
        let value = information_value(answer, family, database);
        let dialect = engine.dialect();
        conn.create_scalar_function(
            name,
            0,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |_context| {
                if dialect != family {
                    return Err(::rusqlite::Error::UserFunctionError(
                        format!("{UNDEFINED_FUNCTION_PREFIX}{name}").into(),
                    ));
                }
                Ok(value.clone())
            },
        )?;
    }
    Ok(())
}

/// The value a system-information function of `family` answers with on a connection serving
/// `database`.
///
/// A MySQL session that has selected no database answers NULL for `DATABASE()` — "returns
/// NULL if there is no default database" — which is the position a call that names none is in
/// on that family, and a MySQL SCHEMA *is* a database, so `SCHEMA()` answers the same name.
/// PostgreSQL always has a current database: a call that names none addresses the cluster's
/// own default one, which is the database this front end reports as the catalog a relation
/// belongs to ([`super::postgres::catalog_name`]), so the two answer one name; and its
/// `current_schema()` is the schema every database is created with.
fn information_value(answer: Answer, family: SqlEngineFamily, database: &str) -> Value {
    match (answer, family) {
        (Answer::Schema, SqlEngineFamily::PostgreSql) => {
            Value::Text(super::postgres::PUBLIC_SCHEMA.to_string())
        }
        (_, SqlEngineFamily::PostgreSql) => {
            Value::Text(super::postgres::catalog_name(database).to_string())
        }
        (_, SqlEngineFamily::MySql) => match database.is_empty() {
            true => Value::Null,
            false => Value::Text(database.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the engine the cluster runs, the database the
    /// call addressed, the call, and what it must answer — `None` for a call that must be
    /// refused.
    type Row = (
        &'static str,
        EngineFamily,
        &'static str,
        &'static str,
        Option<&'static str>,
    );

    /// RULE: a statement may call the built-ins of the engine the cluster RUNS — the clock
    /// functions and the "which database am I in" functions of that family — and a call of the
    /// other family's is refused as the engine refuses a function it does not have.
    ///
    /// The rows span both families and both kinds of built-in, and the database the connection
    /// serves is the value the information functions answer.
    #[test]
    fn a_statement_calls_the_builtins_of_the_engine_the_cluster_runs() {
        let rows: &[Row] = &[
            (
                "PostgreSQL's own name for the current database",
                EngineFamily::PostgreSql,
                "appdb",
                "SELECT current_database()",
                Some("appdb"),
            ),
            (
                "PostgreSQL's current schema",
                EngineFamily::PostgreSql,
                "appdb",
                "SELECT current_schema()",
                Some("public"),
            ),
            (
                "MySQL's own name for the current database",
                EngineFamily::MySql,
                "appdb",
                "SELECT DATABASE()",
                Some("appdb"),
            ),
            (
                "MySQL's synonym for it",
                EngineFamily::MySql,
                "appdb",
                "SELECT SCHEMA()",
                Some("appdb"),
            ),
            (
                "MySQL's information function on a PostgreSQL cluster",
                EngineFamily::PostgreSql,
                "appdb",
                "SELECT DATABASE()",
                None,
            ),
            (
                "PostgreSQL's information function on a MySQL cluster",
                EngineFamily::MySql,
                "appdb",
                "SELECT current_database()",
                None,
            ),
            (
                "the MySQL clock built-in on a MySQL cluster",
                EngineFamily::MySql,
                "appdb",
                "SELECT CAST(length(UTC_TIMESTAMP(6)) AS TEXT)",
                Some("26"),
            ),
            (
                "the MySQL clock built-in on a PostgreSQL cluster",
                EngineFamily::PostgreSql,
                "appdb",
                "SELECT UTC_TIMESTAMP(6)",
                None,
            ),
        ];
        for (case, engine, database, sql, expected) in rows {
            let conn = Connection::open_in_memory().expect("the engine opens");
            register_engine_builtins(&conn, *engine, database).expect("the built-ins define");
            let answered: ::rusqlite::Result<Option<String>> =
                conn.query_row(sql, [], |row| row.get(0));
            match expected {
                Some(value) => assert_eq!(
                    answered.as_ref().ok().cloned().flatten().as_deref(),
                    Some(*value),
                    "case {case:?}: expected {value:?} from {sql:?}, got {answered:?}"
                ),
                None => {
                    let error = answered
                        .as_ref()
                        .err()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    assert!(
                        error.contains(UNDEFINED_FUNCTION_PREFIX),
                        "case {case:?}: expected the engine's undefined-function sentence from \
                         {sql:?}, got {answered:?}"
                    );
                }
            }
        }

        // A MySQL session that selected no database has none to report, which is the position
        // a call naming no `database` is in; a PostgreSQL one always has a current database —
        // the cluster's own default, which is the catalog this front end reports.
        let conn = Connection::open_in_memory().expect("the engine opens");
        register_engine_builtins(&conn, EngineFamily::MySql, "").expect("the built-ins define");
        let answered: Option<String> = conn
            .query_row("SELECT DATABASE()", [], |row| row.get(0))
            .expect("the function answers");
        assert_eq!(
            answered, None,
            "case \"a call that named no database\": expected no database, got {answered:?}"
        );
        let conn = Connection::open_in_memory().expect("the engine opens");
        register_engine_builtins(&conn, EngineFamily::PostgreSql, "")
            .expect("the built-ins define");
        let answered: Option<String> = conn
            .query_row("SELECT current_database()", [], |row| row.get(0))
            .expect("the function answers");
        assert_eq!(
            answered.as_deref(),
            Some(super::super::postgres::catalog_name("")),
            "case \"a PostgreSQL call that named no database\": expected the cluster's own \
             default database, got {answered:?}"
        );
    }
}
