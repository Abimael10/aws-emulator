// SPDX-License-Identifier: CC0-1.0

//! The MySQL front end's DIAGNOSTICS: how an Aurora MySQL cluster words an error its
//! database raised.
//!
//! The Data API serves Aurora MySQL DB clusters as well as Aurora PostgreSQL ones, and an
//! error the DATABASE raised reaches the caller in the words of the engine the addressed
//! cluster RUNS: that engine's own sentence, its own vendor ERROR CODE and its own
//! SQLSTATE, which is what a client branches, retries and logs on. This module is that
//! wording for MySQL, beside [`super::postgres::postgresql_diagnostic`], which is
//! PostgreSQL's; both are about the conditions [`super::condition`] reads out of the
//! engine's answer, so the two cannot disagree about what happened.
//!
//! Every sentence here is MySQL's own, from its "Server Error Message Reference": the
//! condition's fixed text, with the names and values the statement and the database supply
//! — and MySQL's own error number and SQLSTATE for that condition.

use super::condition::{Condition, condition_of, covers_the_same_columns};
use super::postgres::NamedIn;

/// How the Data API renders an error a MySQL database raised: the engine's message, the
/// MySQL error number of the condition and its SQLSTATE — the frame a MySQL client reads a
/// failure's class out of.
fn mysql_error(message: &str, code: u16, sqlstate: &str) -> String {
    format!("{message}; Error code: {code}; SQLState: {sqlstate}")
}

/// MySQL's error numbers for the conditions this front end words (its "Server Error
/// Message Reference"): a duplicate entry for a key, a duplicate key the statement's own
/// values do not name, a NULL in a column that admits none, a table that is not there and
/// a column nothing declares.
const ER_DUP_ENTRY: u16 = 1062;
const ER_DUP_KEY: u16 = 1022;
const ER_BAD_NULL_ERROR: u16 = 1048;
const ER_NO_SUCH_TABLE: u16 = 1146;
const ER_BAD_FIELD_ERROR: u16 = 1054;

/// And their SQLSTATEs, as MySQL's own reference lists them for each error number:
/// class 23 for an integrity-constraint violation, `42S02` for a base table that is not
/// there and `42S22` for a column that is not there.
const SQLSTATE_INTEGRITY_CONSTRAINT: &str = "23000";
const SQLSTATE_BASE_TABLE_NOT_FOUND: &str = "42S02";
const SQLSTATE_COLUMN_NOT_FOUND: &str = "42S22";

/// What MySQL calls the index that enforces a table's PRIMARY KEY: every table has at most
/// one, and it is named `PRIMARY` whatever its columns are called.
const PRIMARY_KEY_NAME: &str = "PRIMARY";

/// How MySQL names the CLAUSE a column was written in, which its unknown-column diagnostic
/// reports: the wordings its "Server Error Message Reference" gives for that error.
///
/// A name the parse does not place — one the engine resolved out of a wildcard, or a name
/// of the engine's own — is reported where MySQL reports a selected column, which is the
/// clause a statement names a column in when it is not comparing, grouping or ordering by
/// it.
fn clause(named_in: Option<NamedIn>) -> &'static str {
    match named_in {
        Some(NamedIn::Where) => "where clause",
        Some(NamedIn::GroupBy) => "group statement",
        Some(NamedIn::Having) => "having clause",
        Some(NamedIn::OrderBy) => "order clause",
        Some(NamedIn::JoinOn) => "on clause",
        Some(NamedIn::Projection) | None => FIELD_LIST_CLAUSE,
    }
}

/// Where MySQL says an unknown column was named when the column is one the statement
/// SELECTS or assigns.
const FIELD_LIST_CLAUSE: &str = "field list";

/// How a composite key's colliding value is written: MySQL joins the key's parts with a
/// hyphen, so a two-column key collides on `<first>-<second>`.
const COMPOSITE_KEY_SEPARATOR: &str = "-";

/// What the DATABASE and the STATEMENT hold about the failure that the engine's own
/// message does not carry, which only the caller of this module can answer.
pub(super) struct About<'a> {
    /// The database the statement ran against, which MySQL qualifies a table name by.
    /// Empty for the connection's own database, which is then named alone.
    pub database: &'a str,
    /// The columns the PRIMARY KEY of a table covers, which is what decides whether the
    /// key a condition is about is that key.
    pub primary_key_of: &'a dyn Fn(&str) -> Vec<String>,
    /// The value the statement ASSIGNED to one of its columns, as text, where the
    /// statement and its parameters settle one — which is the value MySQL's duplicate-key
    /// diagnostic names.
    pub assigned: &'a dyn Fn(&str) -> Option<String>,
    /// The CLAUSE the statement writes a column name in, which is what MySQL's
    /// unknown-column diagnostic reports — the engine's own answer names the column alone.
    pub named_in: &'a dyn Fn(&str) -> Option<NamedIn>,
}

/// The diagnostic an Aurora MySQL cluster words the engine's error with, or `None` for a
/// condition this front end does not word — whose own text the caller sees.
pub(super) fn mysql_diagnostic(engine_message: &str, about: &About<'_>) -> Option<String> {
    Some(match condition_of(engine_message)? {
        Condition::UniqueViolation { table, columns } => duplicate_key(&table, &columns, about),
        // "Column '<column>' cannot be null": MySQL's sentence names the column alone.
        Condition::NotNullViolation { column, .. } => mysql_error(
            &format!("Column '{column}' cannot be null"),
            ER_BAD_NULL_ERROR,
            SQLSTATE_INTEGRITY_CONSTRAINT,
        ),
        // "Table '<database>.<table>' doesn't exist": MySQL qualifies the table by the
        // database it looked in.
        Condition::UndefinedTable { table } => mysql_error(
            &format!("Table '{}' doesn't exist", qualified(about.database, &table)),
            ER_NO_SUCH_TABLE,
            SQLSTATE_BASE_TABLE_NOT_FOUND,
        ),
        // "Unknown column '<column>' in '<clause>'": the clause is the one the statement
        // wrote the column in.
        Condition::UndefinedColumn { column, .. } => mysql_error(
            &format!(
                "Unknown column '{column}' in '{}'",
                clause((about.named_in)(&column))
            ),
            ER_BAD_FIELD_ERROR,
            SQLSTATE_COLUMN_NOT_FOUND,
        ),
    })
}

/// MySQL's duplicate-key diagnostic: "Duplicate entry '<value>' for key '<table>.<key>'",
/// naming the VALUE that collided and the index it collided in.
///
/// The index: MySQL names the index enforcing a table's PRIMARY KEY `PRIMARY`, and gives an
/// index a declaration did not name "the same name as the first indexed column" — so a
/// unique key over other columns is named after the first of them. Since MySQL 8.0.19 the
/// key is reported qualified by its table.
///
/// The VALUE comes from the statement that was refused, which is the only place it exists:
/// a value the statement wrote itself, or the one a parameter of the request bound. A
/// statement that assigns no such value DIRECTLY (it inserts the rows a query produced, or
/// several rows at once, so which row collided is not one assignment) is reported with
/// MySQL's OTHER duplicate-key error — "Can't write; duplicate key in table '<table>'",
/// which names no value — rather than with a value this emulator would have had to invent.
fn duplicate_key(table: &str, columns: &[String], about: &About<'_>) -> String {
    let key = if covers_the_same_columns(&(about.primary_key_of)(table), columns) {
        PRIMARY_KEY_NAME.to_string()
    } else {
        columns.first().cloned().unwrap_or_default()
    };
    let colliding: Option<Vec<String>> = columns
        .iter()
        .map(|column| (about.assigned)(column))
        .collect();
    match colliding {
        Some(values) => mysql_error(
            &format!(
                "Duplicate entry '{}' for key '{table}.{key}'",
                values.join(COMPOSITE_KEY_SEPARATOR)
            ),
            ER_DUP_ENTRY,
            SQLSTATE_INTEGRITY_CONSTRAINT,
        ),
        None => mysql_error(
            &format!("Can't write; duplicate key in table '{table}'"),
            ER_DUP_KEY,
            SQLSTATE_INTEGRITY_CONSTRAINT,
        ),
    }
}

/// A table as MySQL names it in a diagnostic: qualified by the database it was looked for
/// in, and alone where the call named no database — the connection's own, which has no name
/// to report.
fn qualified(database: &str, table: &str) -> String {
    if database.is_empty() {
        return table.to_string();
    }
    format!("{database}.{table}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an error the database raised is worded as MySQL words that condition — its own
    /// sentence, its own error number and its own SQLSTATE — with the names and values the
    /// statement and the database supply, and a condition this front end does not word
    /// keeps the engine's own text.
    ///
    /// The rows differ along the condition, along whether the violated key is the table's
    /// PRIMARY KEY or a unique key over another column, along how many columns that key
    /// covers, and along whether the statement settles the colliding value at all.
    #[test]
    fn an_error_the_database_raised_is_worded_as_mysql_words_it() {
        let primary = |table: &str| -> Vec<String> {
            match table {
                "ledger" => vec!["id".to_string()],
                "pairs" => vec!["year".to_string(), "seq".to_string()],
                _ => Vec::new(),
            }
        };
        let assigned = |column: &str| -> Option<String> {
            match column {
                "id" => Some("4".to_string()),
                "label" => Some("taken".to_string()),
                "year" => Some("2026".to_string()),
                "seq" => Some("7".to_string()),
                _ => None,
            }
        };
        let named_in = |column: &str| -> Option<NamedIn> {
            match column {
                "missing" => Some(NamedIn::Projection),
                "compared" => Some(NamedIn::Where),
                "ordered" => Some(NamedIn::OrderBy),
                _ => None,
            }
        };
        let about = About {
            database: "appdb",
            primary_key_of: &primary,
            assigned: &assigned,
            named_in: &named_in,
        };

        // (case, the engine's diagnostic, the sentence a MySQL client is answered with)
        let rows: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "a value the primary key already holds",
                "UNIQUE constraint failed: ledger.id",
                Some("Duplicate entry '4' for key 'ledger.PRIMARY'; Error code: 1062; \
                      SQLState: 23000"),
            ),
            (
                "a value a unique key over another column already holds",
                "UNIQUE constraint failed: ledger.label",
                Some("Duplicate entry 'taken' for key 'ledger.label'; Error code: 1062; \
                      SQLState: 23000"),
            ),
            (
                "a value a composite primary key already holds",
                "UNIQUE constraint failed: pairs.year, pairs.seq",
                Some("Duplicate entry '2026-7' for key 'pairs.PRIMARY'; Error code: 1062; \
                      SQLState: 23000"),
            ),
            (
                "a duplicate key whose value the statement does not settle",
                "UNIQUE constraint failed: ledger.note",
                Some("Can't write; duplicate key in table 'ledger'; Error code: 1022; \
                      SQLState: 23000"),
            ),
            (
                "a NULL in a column that admits none",
                "NOT NULL constraint failed: ledger.note",
                Some("Column 'note' cannot be null; Error code: 1048; SQLState: 23000"),
            ),
            (
                "a table that is not there",
                "no such table: absent",
                Some("Table 'appdb.absent' doesn't exist; Error code: 1146; SQLState: 42S02"),
            ),
            (
                "a column nothing declares",
                "no such column: missing",
                Some("Unknown column 'missing' in 'field list'; Error code: 1054; \
                      SQLState: 42S22"),
            ),
            (
                "a column nothing declares, compared in the condition",
                "no such column: compared",
                Some("Unknown column 'compared' in 'where clause'; Error code: 1054; \
                      SQLState: 42S22"),
            ),
            (
                "a column nothing declares, named in the ordering",
                "no such column: ordered",
                Some("Unknown column 'ordered' in 'order clause'; Error code: 1054; \
                      SQLState: 42S22"),
            ),
            (
                "a column the statement writes nowhere this places",
                "no such column: expanded",
                Some("Unknown column 'expanded' in 'field list'; Error code: 1054; \
                      SQLState: 42S22"),
            ),
            (
                "the engine's other spelling of an undefined column",
                "table ledger has no column named missing",
                Some("Unknown column 'missing' in 'field list'; Error code: 1054; \
                      SQLState: 42S22"),
            ),
            ("a condition this front end does not word", "interrupted", None),
        ];
        for (case, engine_message, expected) in rows {
            let worded = mysql_diagnostic(engine_message, &about);
            assert_eq!(
                worded.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {worded:?}"
            );
        }

        // A call that named no database names the table alone, because the connection's
        // own database has no name to report.
        let unnamed = About {
            database: "",
            primary_key_of: &primary,
            assigned: &assigned,
            named_in: &named_in,
        };
        let worded = mysql_diagnostic("no such table: absent", &unnamed);
        assert_eq!(
            worded.as_deref(),
            Some("Table 'absent' doesn't exist; Error code: 1146; SQLState: 42S02"),
            "case \"a table of the connection's own database\": got {worded:?}"
        );
    }
}
