// SPDX-License-Identifier: CC0-1.0

//! What the SQL ENGINE behind this data plane said, read once.
//!
//! An error the engine raises while running a statement reaches the caller in the words of
//! the engine the addressed cluster RUNS — [`super::postgres::postgresql_diagnostic`] for
//! an Aurora PostgreSQL cluster, [`super::mysql::mysql_diagnostic`] for an Aurora MySQL one
//! — and both wordings are about the same CONDITIONS. This module is the reading of the
//! engine's own diagnostic into those conditions, written once, so the two wordings cannot
//! disagree about what happened.
//!
//! Nothing here inspects the caller's SQL: the only input is the engine's answer.

/// One condition the engine raised, with the names its diagnostic carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Condition {
    /// A value a unique key of the table already holds.
    UniqueViolation {
        table: String,
        /// The columns the violated key covers, in the order the engine named them.
        columns: Vec<String>,
    },
    /// A NULL in a column whose declaration admits none.
    NotNullViolation { table: String, column: String },
    /// A relation the database does not hold.
    UndefinedTable { table: String },
    /// A column the statement names and nothing declares; the engine names the relation
    /// too where its own diagnostic carried one.
    UndefinedColumn {
        table: Option<String>,
        column: String,
    },
}

/// How the SQL engine behind this front end introduces each condition in its own
/// diagnostics, which is what is read to recognise the condition.
const ENGINE_UNIQUE_VIOLATION: &str = "UNIQUE constraint failed: ";
const ENGINE_NOT_NULL_VIOLATION: &str = "NOT NULL constraint failed: ";
const ENGINE_UNDEFINED_TABLE: &str = "no such table: ";
const ENGINE_UNDEFINED_COLUMN: &str = "no such column: ";
/// The engine's other spelling of an undefined column, which names the table too: "table
/// <table> has no column named <column>".
const ENGINE_UNDEFINED_COLUMN_OF: &str = "table ";
const ENGINE_UNDEFINED_COLUMN_OF_MIDDLE: &str = " has no column named ";

/// The condition the engine's diagnostic is about, and `None` for one this data plane does
/// not recognise — whose own text the caller sees.
pub(super) fn condition_of(engine_message: &str) -> Option<Condition> {
    if let Some(columns) = engine_message.strip_prefix(ENGINE_UNIQUE_VIOLATION) {
        let qualified: Vec<(&str, &str)> = columns.split(',').filter_map(split_qualified).collect();
        let (table, _) = *qualified.first()?;
        return Some(Condition::UniqueViolation {
            table: table.to_string(),
            columns: qualified
                .iter()
                .map(|(_, column)| (*column).to_string())
                .collect(),
        });
    }
    if let Some(column) = engine_message.strip_prefix(ENGINE_NOT_NULL_VIOLATION) {
        let (table, column) = split_qualified(column)?;
        return Some(Condition::NotNullViolation {
            table: table.to_string(),
            column: column.to_string(),
        });
    }
    if let Some(table) = engine_message.strip_prefix(ENGINE_UNDEFINED_TABLE) {
        return Some(Condition::UndefinedTable {
            table: named_in(table),
        });
    }
    if let Some(column) = engine_message.strip_prefix(ENGINE_UNDEFINED_COLUMN) {
        return Some(Condition::UndefinedColumn {
            table: None,
            column: named_in(column),
        });
    }
    if let Some((table, column)) = engine_message
        .strip_prefix(ENGINE_UNDEFINED_COLUMN_OF)
        .and_then(|rest| rest.split_once(ENGINE_UNDEFINED_COLUMN_OF_MIDDLE))
    {
        return Some(Condition::UndefinedColumn {
            table: Some(named_in(table)),
            column: named_in(column),
        });
    }
    None
}

/// The NAME the engine's diagnostic is about, read out of the text that follows its
/// prefix: the engine adds a hint and the statement it was parsing to some of its
/// messages, where a database's own sentence carries the name alone.
fn named_in(rest: &str) -> String {
    let head = rest.split(" - ").next().unwrap_or(rest).trim();
    let head = head.split_whitespace().next().unwrap_or(head);
    head.trim_matches('"').to_string()
}

/// One column a constraint covers, as the engine names it: `table.column`.
fn split_qualified(qualified: &str) -> Option<(&str, &str)> {
    qualified.trim().rsplit_once('.')
}

/// Whether two lists of column names cover the same columns, compared the way the engine
/// and the catalog spell them (without regard to case) and without regard to order.
///
/// What decides whether the key a condition is about is the table's PRIMARY KEY, which
/// both wordings name that key by.
pub(super) fn covers_the_same_columns(one: &[String], other: &[String]) -> bool {
    if one.is_empty() || one.len() != other.len() {
        return false;
    }
    let fold = |names: &[String]| {
        let mut folded: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
        folded.sort_unstable();
        folded
    };
    fold(one) == fold(other)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the engine's own diagnostic is read into the condition it is about, and a
    /// diagnostic this data plane does not recognise is read as none.
    ///
    /// The rows differ along which condition the engine raised and along the shape of the
    /// diagnostic it raised it with: the two spellings of an undefined column, a
    /// single-column key and a composite one, and a message carrying the hint and the
    /// statement the engine adds to some of them.
    #[test]
    fn the_engines_diagnostic_is_read_into_the_condition_it_is_about() {
        let rows: Vec<(&str, &str, Option<Condition>)> = vec![
            (
                "a value a single-column key already holds",
                "UNIQUE constraint failed: ledger.id",
                Some(Condition::UniqueViolation {
                    table: "ledger".to_string(),
                    columns: vec!["id".to_string()],
                }),
            ),
            (
                "a value a composite key already holds",
                "UNIQUE constraint failed: ledger.year, ledger.seq",
                Some(Condition::UniqueViolation {
                    table: "ledger".to_string(),
                    columns: vec!["year".to_string(), "seq".to_string()],
                }),
            ),
            (
                "a NULL in a column that admits none",
                "NOT NULL constraint failed: ledger.note",
                Some(Condition::NotNullViolation {
                    table: "ledger".to_string(),
                    column: "note".to_string(),
                }),
            ),
            (
                "a relation the database does not hold",
                "no such table: absent",
                Some(Condition::UndefinedTable {
                    table: "absent".to_string(),
                }),
            ),
            (
                "a column nothing declares",
                "no such column: missing",
                Some(Condition::UndefinedColumn {
                    table: None,
                    column: "missing".to_string(),
                }),
            ),
            (
                "a column nothing declares, with the engine's hint appended",
                "no such column: missing - should this be a string literal in single-quotes?",
                Some(Condition::UndefinedColumn {
                    table: None,
                    column: "missing".to_string(),
                }),
            ),
            (
                "the engine's other spelling of an undefined column",
                "table ledger has no column named missing",
                Some(Condition::UndefinedColumn {
                    table: Some("ledger".to_string()),
                    column: "missing".to_string(),
                }),
            ),
            (
                "a diagnostic about something else",
                "interrupted",
                None,
            ),
        ];
        for (case, engine_message, expected) in rows {
            let read = condition_of(engine_message);
            assert!(
                read == expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: the key a condition is about is the table's PRIMARY KEY exactly when the two
    /// cover the same columns, whatever their order or case, and a table that declares no
    /// primary key covers nothing.
    #[test]
    fn a_violated_key_is_the_primary_key_when_it_covers_the_same_columns() {
        let declared = |names: &[&str]| -> Vec<String> {
            names.iter().map(|name| (*name).to_string()).collect()
        };
        // (case, the primary key, the violated columns, whether they are the same key)
        let rows = [
            ("the same single column", vec!["id"], vec!["id"], true),
            (
                "the same column in another case",
                vec!["ID"],
                vec!["id"],
                true,
            ),
            (
                "the same two columns in another order",
                vec!["year", "seq"],
                vec!["seq", "year"],
                true,
            ),
            ("another column", vec!["id"], vec!["label"], false),
            (
                "a key covering more columns",
                vec!["id"],
                vec!["id", "label"],
                false,
            ),
            ("a table with no primary key", vec![], vec!["label"], false),
        ];
        for (case, primary, violated, expected) in rows {
            let answered = covers_the_same_columns(&declared(&primary), &declared(&violated));
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected}, got {answered}"
            );
        }
    }
}
