// SPDX-License-Identifier: CC0-1.0

//! The columns a MySQL table was DECLARED with, as the engine stored its definition.
//!
//! A MySQL client declares a column with a TYPE that says what the column holds and how much
//! of it — `VARCHAR(5)`, `TINYINT UNSIGNED`, `INT AUTO_INCREMENT` — and the engine stores the
//! definition it was handed, with only what its own grammar spells differently translated
//! ([`super::dialect`]). Reading that definition back is how this endpoint answers the two
//! questions only the declaration can answer:
//!
//! * WHICH column the engine generates keys for, which is the column a MySQL
//!   `AUTO_INCREMENT` declaration became ([`super::mysql_insert_id`]);
//! * WHICH VALUES a column admits at all, which is what a MySQL server in strict mode refuses
//!   a statement for ([`super::mysql_strict_mode`]).
//!
//! Nothing here is kept beside the engine: the definition is the engine's own, read for the
//! SESSION, so a table a transaction created and has not committed is the table its own
//! statements see.

use sqlparser::ast::{ColumnOption, Statement};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use super::sql_engine::{Cell, InstanceDatabase};
use super::wire_session::SessionFacts;

/// One column of a table, as its declaration wrote it.
#[derive(Debug, Clone)]
pub struct DeclaredColumn {
    pub name: String,
    /// The type the declaration gave it, as it was written: `varchar(5)`, `tinyint unsigned`.
    pub declared_type: String,
    /// Whether the declaration made this column the table's primary key.
    pub primary_key: bool,
    /// Its position in the declaration, which is the position a value for it takes in an
    /// `INSERT` that names no columns at all.
    pub position: usize,
}

/// The columns the database `target` holds `table` with, in the order they were declared.
/// Empty for a table this session cannot see, and for a definition this parser cannot read.
#[must_use]
pub fn declared(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &str,
    table: &str,
) -> Vec<DeclaredColumn> {
    stored_definition(database, facts, target, table)
        .map(|ddl| of_definition(&ddl))
        .unwrap_or_default()
}

/// The one statement `sql` is, or `None` where it is not one statement this parser reads.
///
/// Read in the MySQL dialect, which is the dialect the statements this module's readers are
/// given were written in: the client's own, on a MySQL session.
#[must_use]
pub fn one_statement(sql: &str) -> Option<Statement> {
    let mut statements = Parser::parse_sql(&MySqlDialect {}, sql).ok()?;
    if statements.len() != 1 {
        return None;
    }
    Some(statements.remove(0))
}

/// One identifier without the qualifier a statement may write it with, and without the
/// quoting it may carry: ``​`appdb`.`gauges`​`` is the table `gauges`.
#[must_use]
pub fn unqualified(written: &str) -> String {
    written
        .rsplit('.')
        .next()
        .unwrap_or(written)
        .trim_matches(['`', '"'])
        .to_string()
}

/// The `CREATE TABLE` statement the database `target` holds for `table`, as the engine stored
/// it.
fn stored_definition(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &str,
    table: &str,
) -> Option<String> {
    let answer = database
        .execute_in(
            facts.connection_id,
            target,
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND lower(name) = lower(?1)",
            &[Some(table.to_string())],
        )
        .ok()?;
    match answer.rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(ddl)) => Some(ddl.clone()),
        _ => None,
    }
}

/// The columns the definition `ddl` declares; see [`declared`].
fn of_definition(ddl: &str) -> Vec<DeclaredColumn> {
    let Some(Statement::CreateTable(created)) = one_statement(ddl) else {
        return Vec::new();
    };
    created
        .columns
        .iter()
        .enumerate()
        .map(|(position, column)| DeclaredColumn {
            name: column.name.value.clone(),
            declared_type: column.data_type.to_string().to_ascii_lowercase(),
            primary_key: column.options.iter().any(|option| {
                matches!(
                    option.option,
                    ColumnOption::Unique {
                        is_primary: true,
                        ..
                    }
                )
            }),
            position,
        })
        .collect()
}

/// The engine's own rowid type, which is the type a MySQL `AUTO_INCREMENT` column is declared
/// to the engine as ([`super::dialect`]) and the only type it generates keys for.
const KEY_COLUMN_TYPE: &str = "integer";

/// The column of `columns` the engine generates keys for, or `None` for a table that has none
/// — a table whose rows carry no generated key.
#[must_use]
pub fn key_column(columns: &[DeclaredColumn]) -> Option<&DeclaredColumn> {
    columns
        .iter()
        .find(|column| column.primary_key && column.declared_type == KEY_COLUMN_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the key-column table: the case, the definition the engine stored, and the
    /// key column with its position.
    type KeyColumnRow = (&'static str, &'static str, Option<(&'static str, usize)>);

    /// RULE: the column the engine generates keys for is the one declared with its rowid type
    /// and the primary key — which is what a MySQL `AUTO_INCREMENT` column is declared as —
    /// and a table that declares none has none.
    #[test]
    fn the_key_column_is_the_one_the_engine_generates_keys_for() {
        let rows: &[KeyColumnRow] = &[
            (
                "an auto-increment column, declared first",
                "CREATE TABLE gauges (id INTEGER PRIMARY KEY AUTOINCREMENT, label VARCHAR(16))",
                Some(("id", 0)),
            ),
            (
                "an auto-increment column after another",
                "CREATE TABLE gauges (label VARCHAR(16), gauge_id INTEGER PRIMARY KEY)",
                Some(("gauge_id", 1)),
            ),
            (
                "a primary key the engine generates no value for",
                "CREATE TABLE gauges (id INT PRIMARY KEY, label VARCHAR(16))",
                None,
            ),
            (
                "an integer column that is not the primary key",
                "CREATE TABLE gauges (id INTEGER, label VARCHAR(16) PRIMARY KEY)",
                None,
            ),
            (
                "no primary key at all",
                "CREATE TABLE gauges (label VARCHAR(16))",
                None,
            ),
        ];
        for (case, ddl, expected) in rows {
            let columns = of_definition(ddl);
            let actual = key_column(&columns).map(|key| (key.name.as_str(), key.position));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a column's declared type is read back as the declaration wrote it, which is what
    /// says how much the column holds.
    #[test]
    fn a_columns_declared_type_is_read_back_as_it_was_written() {
        let declared = of_definition(
            "CREATE TABLE gauges (id INTEGER PRIMARY KEY AUTOINCREMENT, \
             name VARCHAR(5) NOT NULL, reading TINYINT UNSIGNED, note TEXT)",
        );
        let actual: Vec<(&str, &str)> = declared
            .iter()
            .map(|column| (column.name.as_str(), column.declared_type.as_str()))
            .collect();
        assert_eq!(
            actual,
            vec![
                ("id", "integer"),
                ("name", "varchar(5)"),
                ("reading", "tinyint unsigned"),
                ("note", "text"),
            ],
            "the declared types must be read back as written, got {actual:?}"
        );
    }
}
