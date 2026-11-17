// SPDX-License-Identifier: CC0-1.0

//! The value a MySQL session's `LAST_INSERT_ID()` answers with: the AUTO_INCREMENT key the
//! session's own last row-inserting statement was assigned.
//!
//! It is a value of the SESSION and of nothing else — "the first automatically generated
//! value successfully inserted for an AUTO_INCREMENT column as a result of the most recently
//! executed INSERT statement" (MySQL Reference Manual, "Information Functions") — so each
//! session carries its own cell ([`SessionInsertId`]), shared with the engine connection its
//! statements run on so that the call the engine plans reads the session that made it. A
//! session no insert of which has been assigned a key answers 0, as a server that has
//! generated none does.
//!
//! The one-argument form sets it: `LAST_INSERT_ID(expr)` "returns expr and sets the value
//! returned by LAST_INSERT_ID() to expr", which is a write to that same session value — and
//! the reason the cell is shared rather than copied in.
//!
//! What UPDATES it is a key the server GENERATED ([`assigned_key`]): a statement that supplied
//! the auto-increment column's value itself, and one over a table that has no such column,
//! leave the session's value as it was.
//!
//! The same value is what the statement's own answer carries on the wire (the insert id of a
//! MySQL `OK` packet), so the two readings of one session's last key cannot disagree.
//!
//! The function belongs to the MySQL family: no PostgreSQL server defines it, so a session on
//! that wire is answered the undefined function it is
//! ([`super::dialect::dialect_refusal`]).

use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::functions::FunctionFlags;
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use sqlparser::ast::{Expr, Insert, SetExpr, Statement, Value as SqlValue};

use super::mysql_columns::{one_statement, unqualified};
use super::sql_engine::{EngineError, EngineErrorKind, InstanceDatabase, ResultSet, ServerView};
use super::wire_session::SessionFacts;
use super::EngineWire;

/// The name a client calls this function by.
const FUNCTION: &str = "last_insert_id";

/// Whether `function` is the one this module defines, which only a MySQL session has; see
/// [`super::dialect::dialect_refusal`], which refuses it on the other wire.
#[must_use]
pub fn is_mysql_only_function(function: &str) -> bool {
    function.eq_ignore_ascii_case(FUNCTION)
}

/// The value ONE session's `LAST_INSERT_ID()` answers with, shared by that session and the
/// engine connection its statements run on.
#[derive(Debug, Clone, Default)]
pub struct SessionInsertId(Arc<Mutex<i64>>);

impl SessionInsertId {
    /// What this session's `LAST_INSERT_ID()` answers now: 0 until a statement of it has been
    /// assigned a key.
    #[must_use]
    pub fn get(&self) -> i64 {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record `key` as the value this session's `LAST_INSERT_ID()` answers with from here on.
    pub fn set(&self, key: i64) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = key;
    }
}

/// The statements that INSERT rows, and so are the ones an AUTO_INCREMENT key is generated
/// for: MySQL's `INSERT` and its `REPLACE`, which "inserts the new row" where it deletes a
/// conflicting one. Every other statement generates none, however many rows it changes.
const INSERTING_VERBS: [&str; 2] = ["INSERT", "REPLACE"];

/// The AUTO_INCREMENT key the statement `sql` had GENERATED for it in the database `target`,
/// or `None` where the server generated none — which leaves the session's value as it was.
///
/// A key is generated for a statement that inserted rows into a table that HAS an
/// auto-increment column and left that column to the server. MySQL's two conditions, both
/// from the Reference Manual's `AUTO_INCREMENT` and `LAST_INSERT_ID()` entries:
///
/// * "the first automatically generated value successfully inserted for an AUTO_INCREMENT
///   column as a result of the most recently executed INSERT statement" — so a statement that
///   inserted no row, and one into a table with no such column, generate nothing;
/// * "the value of LAST_INSERT_ID() is not changed if you set the AUTO_INCREMENT column of a
///   row to a non-'magic' value (that is, a value that is not NULL and not 0)" — so a
///   statement that supplied the key did not have one generated.
///
/// Where several rows were inserted it is the FIRST key that is reported ("if you insert
/// multiple rows using a single INSERT statement, LAST_INSERT_ID() returns the value generated
/// for the first inserted row only"), while the engine answers with the last key it assigned:
/// one statement's rows are assigned consecutive keys, so the first is counted back off the
/// last by the rows the statement wrote.
#[must_use]
pub fn assigned_key(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    verb: &str,
    target: &str,
    sql: &str,
    result: &ResultSet,
) -> Option<i64> {
    if !INSERTING_VERBS
        .iter()
        .any(|inserting| inserting.eq_ignore_ascii_case(verb))
    {
        return None;
    }
    let rows = i64::try_from(result.affected).unwrap_or(i64::MAX);
    if result.last_insert_id <= 0 || rows <= 0 {
        return None;
    }
    let insert = inserted(sql)?;
    let written = unqualified(&insert.table.to_string());
    let columns = super::mysql_columns::declared(database, facts, target, &written);
    let key = super::mysql_columns::key_column(&columns)?;
    if supplies_the_key(&insert, key) {
        return None;
    }
    Some(first_of(result.affected, result.last_insert_id))
}

/// The FIRST of the keys one statement was assigned, from the rows it wrote and the LAST key
/// the engine assigned it: one statement's rows are assigned consecutive keys.
fn first_of(affected: u64, last_insert_id: i64) -> i64 {
    let rows = i64::try_from(affected).unwrap_or(i64::MAX);
    last_insert_id.saturating_sub(rows.max(1) - 1).max(1)
}

/// The `INSERT` the statement `sql` is, or `None` where it is not one — read as the one
/// statement it has to be ([`super::mysql_columns::one_statement`]).
fn inserted(sql: &str) -> Option<Insert> {
    match one_statement(sql)? {
        Statement::Insert(insert) => Some(insert),
        _ => None,
    }
}

/// Whether the `INSERT` SUPPLIED the key column's value itself, rather than leaving it to the
/// server to generate.
///
/// The value is the one in the FIRST row it writes, which is the row whose key is reported.
/// `NULL` and `0` are MySQL's own request for a generated key and are not a value supplied; a
/// column the statement does not write at all is not one either.
fn supplies_the_key(insert: &Insert, key: &super::mysql_columns::DeclaredColumn) -> bool {
    let at = if insert.columns.is_empty() {
        // No column list: the values are the table's own columns, in the order they were
        // declared.
        key.position
    } else {
        match insert
            .columns
            .iter()
            .position(|named| named.value.eq_ignore_ascii_case(&key.name))
        {
            Some(at) => at,
            // The statement does not write the key column, so the server generates its value.
            None => return false,
        }
    };
    let Some(source) = &insert.source else {
        return false;
    };
    let SetExpr::Values(values) = source.body.as_ref() else {
        // The rows come from a QUERY: what it writes into the column is that query's own
        // value, which the server did not generate.
        return true;
    };
    match values.rows.first().and_then(|row| row.get(at)) {
        Some(value) => !asks_for_a_generated_key(value),
        None => false,
    }
}

/// Whether the value written for an auto-increment column ASKS the server to generate one:
/// MySQL's "magic" values, `NULL` and `0`, and the `DEFAULT` that stands for the same.
fn asks_for_a_generated_key(value: &Expr) -> bool {
    match value {
        Expr::Value(value) => match &value.value {
            SqlValue::Null => true,
            SqlValue::Number(number, _) => number.trim().parse::<i64>() == Ok(0),
            _ => false,
        },
        Expr::Identifier(named) => named.value.eq_ignore_ascii_case("default"),
        _ => false,
    }
}

/// Register `LAST_INSERT_ID()` on this connection, so a statement that calls it is planned
/// and run by the engine like any other — which is what keeps the column it produces named
/// and typed as the server reports it.
///
/// The call answers from, and the one-argument form writes to, the session the statement runs
/// under ([`ServerView`]) and nothing else.
pub fn register_insert_id_function(
    conn: &Connection,
    view: &Arc<Mutex<ServerView>>,
) -> Result<(), EngineError> {
    let view = Arc::clone(view);
    conn.create_scalar_function(
        FUNCTION,
        // The two forms MySQL defines: the reading takes no argument, the write takes one.
        -1,
        // NOT deterministic: the value belongs to the session, so two calls in one statement
        // must not be folded into one — the second may read what the first set.
        FunctionFlags::SQLITE_UTF8,
        move |context| {
            let session = {
                let view = view.lock().unwrap_or_else(PoisonError::into_inner);
                match view.wire {
                    Some(EngineWire::MySql) => view.insert_id.clone(),
                    // A session on the other wire, and a statement of the server's own, have
                    // no such function.
                    _ => None,
                }
            };
            let Some(session) = session else {
                // The sentence the engine itself uses for a function it does not have, which
                // is the class a client branches on.
                return Err(rusqlite::Error::UserFunctionError(
                    format!("no such function: {FUNCTION}").into(),
                ));
            };
            match context.len() {
                0 => Ok(session.get()),
                _ => {
                    let asked = whole_number(context.get_raw(0));
                    session.set(asked);
                    Ok(asked)
                }
            }
        },
    )
    .map_err(|error| EngineError::new(EngineErrorKind::Internal, error.to_string()))
}

/// The whole number `value` carries, however the engine holds it.
///
/// `LAST_INSERT_ID(expr)` takes a number, and a value that is not one at all names no key:
/// such a call sets 0, which is then what the function answers.
fn whole_number(value: ValueRef<'_>) -> i64 {
    let parsed = |text: std::borrow::Cow<'_, str>| text.trim().parse().unwrap_or_default();
    match value {
        ValueRef::Integer(number) => number,
        // Through the engine's own rendering of the number, so its whole part is read rather
        // than converted.
        ValueRef::Real(number) => parsed(number.trunc().to_string().into()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => parsed(String::from_utf8_lossy(bytes)),
        ValueRef::Null => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a key is generated for the row unless the statement SUPPLIED one — a value for
    /// the key column that is neither `NULL` nor `0`.
    #[test]
    fn a_statement_that_supplies_the_key_has_none_generated() {
        let key = super::super::mysql_columns::DeclaredColumn {
            name: "id".to_string(),
            declared_type: "integer".to_string(),
            primary_key: true,
            position: 0,
        };
        let rows: &[(&str, &str, bool)] = &[
            // (case, the statement, whether it supplied the key)
            (
                "a column list without the key column",
                "INSERT INTO gauges (label) VALUES ('pressure')",
                false,
            ),
            (
                "the key column written as NULL",
                "INSERT INTO gauges (id, label) VALUES (NULL, 'pressure')",
                false,
            ),
            (
                "the key column written as zero",
                "INSERT INTO gauges (id, label) VALUES (0, 'pressure')",
                false,
            ),
            (
                "the key column written as DEFAULT",
                "INSERT INTO gauges (id, label) VALUES (DEFAULT, 'pressure')",
                false,
            ),
            (
                "the key column written with a value of its own",
                "INSERT INTO gauges (id, label) VALUES (100, 'pressure')",
                true,
            ),
            (
                "the key column written out of order",
                "INSERT INTO gauges (label, id) VALUES ('pressure', 100)",
                true,
            ),
            (
                "no column list, with a value for the key column",
                "INSERT INTO gauges VALUES (100, 'pressure')",
                true,
            ),
            (
                "no column list, asking for a generated key",
                "INSERT INTO gauges VALUES (NULL, 'pressure')",
                false,
            ),
            (
                "several rows, the first of which asks for a generated key",
                "INSERT INTO gauges (id, label) VALUES (NULL, 'a'), (100, 'b')",
                false,
            ),
            (
                "rows from a query that writes the key column",
                "INSERT INTO gauges (id, label) SELECT id, label FROM archive",
                true,
            ),
            (
                "rows from a query that does not write it",
                "INSERT INTO gauges (label) SELECT label FROM archive",
                false,
            ),
        ];
        for (case, sql, expected) in rows {
            let insert = inserted(sql)
                .unwrap_or_else(|| panic!("case {case:?}: {sql:?} must read as an insert"));
            let actual = supplies_the_key(&insert, &key);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: the key reported for a statement that inserted several rows is the FIRST of the
    /// keys it was assigned.
    #[test]
    fn the_first_of_the_keys_one_statement_was_assigned_is_the_one_reported() {
        let rows: &[(&str, u64, i64, i64)] = &[
            // (case, rows written, the engine's last key, the first key)
            ("one row", 1, 1, 1),
            ("a later single row", 1, 7, 7),
            ("three rows in one statement", 3, 9, 7),
        ];
        for (case, affected, last, expected) in rows {
            let actual = first_of(*affected, *last);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: the value the one-argument form sets is the whole number its argument carries.
    #[test]
    fn the_one_argument_form_sets_the_whole_number_it_is_given() {
        let rows: &[(&str, ValueRef<'_>, i64)] = &[
            // (case, the argument, expected)
            ("an integer", ValueRef::Integer(99), 99),
            ("a number with a fraction", ValueRef::Real(12.75), 12),
            ("a numeric string", ValueRef::Text(b"41"), 41),
            ("a value that names no key", ValueRef::Text(b"later"), 0),
            ("no value at all", ValueRef::Null, 0),
        ];
        for (case, argument, expected) in rows {
            let actual = whole_number(*argument);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }
}
