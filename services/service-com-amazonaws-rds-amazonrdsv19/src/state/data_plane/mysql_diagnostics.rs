// SPDX-License-Identifier: CC0-1.0

//! The DIAGNOSTICS AREA of a MySQL session: what the server has to say about the statement it
//! has just run, which `SHOW WARNINGS` and `SHOW ERRORS` report.
//!
//! A MySQL server keeps the conditions of the LAST statement — "SHOW WARNINGS … displays
//! information about the conditions resulting from the most recent statement in the current
//! session" — each as a level, a server error number and that condition's own sentence. A
//! statement that was REFUSED leaves its refusal there as an `Error`; one that succeeded
//! leaves nothing behind it; and the statements that READ the list leave it as it is, so a
//! client may read it twice.
//!
//! Nothing here is a canned diagnostic: the one row a refusal leaves is that refusal, under
//! the number and in the words the wire reported it to the client with.
//!
//! The list belongs to the SESSION, so each carries its own
//! ([`SessionDiagnostics`])—interior-mutable, because it is written by the statement that just
//! ran and read by the next one.

use std::sync::{Arc, Mutex, PoisonError};

use super::sql_engine::{Cell, ColumnInfo, EngineError, ResultSet};

/// The level a REFUSAL is reported under: MySQL's own word for a condition that stopped the
/// statement.
const ERROR_LEVEL: &str = "Error";

/// The level a condition the statement RAN WITH is reported under: the server adjusted
/// something it was given and says so.
const WARNING_LEVEL: &str = "Warning";

/// One condition of a statement, as the diagnostics area holds it.
#[derive(Debug, Clone)]
struct Condition {
    /// `Error`, `Warning` or `Note`.
    level: &'static str,
    /// The server error number the condition carries, which is the number the client was
    /// refused under.
    code: u16,
    /// The condition's own sentence.
    message: String,
}

/// The conditions of ONE session's last statement.
#[derive(Debug, Clone, Default)]
pub struct SessionDiagnostics(Arc<Mutex<Vec<Condition>>>);

impl SessionDiagnostics {
    /// Replace the list with the one condition `error` is: the statement was refused, and its
    /// refusal is what the server has to say about it.
    pub fn record(&self, error: &EngineError) {
        let (code, _) = error
            .mysql_error
            .unwrap_or_else(|| super::mysql_wire::statement_error(error.kind));
        *self.held() = vec![Condition {
            level: ERROR_LEVEL,
            code,
            message: error.message.clone(),
        }];
    }

    /// Leave nothing behind: the statement had nothing to say about itself.
    pub fn clear(&self) {
        self.held().clear();
    }

    /// Add the WARNING `code` to the list: the statement ran, and this is what the server did
    /// to a value it was given.
    pub fn warn(&self, code: u16, message: impl Into<String>) {
        self.held().push(Condition {
            level: WARNING_LEVEL,
            code,
            message: message.into(),
        });
    }

    /// The conditions, under the lock — recovering a list a panicking session poisoned, as
    /// every shared cell of this endpoint does.
    fn held(&self) -> std::sync::MutexGuard<'_, Vec<Condition>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The `SHOW` forms that READ the diagnostics area rather than replacing it; see this
/// module's own notes.
const DIAGNOSTIC_READS: [&str; 2] = ["WARNINGS", "ERRORS"];

/// Whether the statement whose leading words are `words` reads the diagnostics area, and so
/// leaves it as it is.
///
/// The form decides it, not the words it carries anywhere: `SHOW WARNINGS` reads the list,
/// while `SHOW VARIABLES LIKE 'WARNINGS'` is a statement of its own and replaces it.
#[must_use]
pub fn reads_the_list(words: &[String]) -> bool {
    words.first().is_some_and(|first| first == "SHOW")
        && words
            .get(1)
            .is_some_and(|subject| DIAGNOSTIC_READS.contains(&subject.as_str()))
}

/// `SHOW WARNINGS` / `SHOW ERRORS`: the conditions of the session's last statement, in the
/// three columns MySQL reports them in.
///
/// `SHOW ERRORS` reports "only errors, not warnings or notes", which is the same list under
/// that one filter.
#[must_use]
pub fn show(diagnostics: &SessionDiagnostics, errors_only: bool) -> ResultSet {
    let columns = ["Level", "Code", "Message"]
        .into_iter()
        .map(|name| ColumnInfo {
            name: name.to_string(),
            // The number a client reads back as a number, as the server reports it.
            declared_type: Some(if name == "Code" { "int" } else { "text" }.to_string()),
        })
        .collect();
    let rows = diagnostics
        .held()
        .iter()
        .filter(|condition| !errors_only || condition.level == ERROR_LEVEL)
        .map(|condition| {
            vec![
                Cell::Text(condition.level.to_string()),
                Cell::Integer(i64::from(condition.code)),
                Cell::Text(condition.message.clone()),
            ]
        })
        .collect();
    ResultSet::of_rows(columns, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::sql_engine::EngineErrorKind;

    /// RULE: the statements that read the diagnostics area are the two `SHOW` forms that
    /// report it, and nothing else that happens to name one of their words.
    #[test]
    fn only_the_forms_that_report_the_list_read_it() {
        let rows: &[(&str, &[&str], bool)] = &[
            // (case, the statement's leading words, whether it reads the list)
            ("SHOW WARNINGS", &["SHOW", "WARNINGS"], true),
            ("SHOW ERRORS", &["SHOW", "ERRORS"], true),
            (
                "SHOW VARIABLES LIKE 'WARNINGS'",
                &["SHOW", "VARIABLES", "LIKE"],
                false,
            ),
            ("SHOW COUNT(*) WARNINGS", &["SHOW", "COUNT", "("], false),
            ("SHOW VARIABLES", &["SHOW", "VARIABLES"], false),
            ("a statement of its own", &["SELECT"], false),
            ("an insert", &["INSERT", "INTO"], false),
        ];
        for (case, words, expected) in rows {
            let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let actual = reads_the_list(&words);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: a refusal leaves ONE condition — its own number and sentence — and a statement
    /// that said nothing leaves none.
    #[test]
    fn a_refusal_is_the_one_condition_the_list_holds() {
        let diagnostics = SessionDiagnostics::default();
        assert!(
            show(&diagnostics, false).rows.is_empty(),
            "case \"a session that has run nothing\": the list must be empty"
        );
        diagnostics.record(
            &EngineError::new(
                EngineErrorKind::DataError,
                "Data too long for column 'name' at row 1",
            )
            .with_mysql_error(1406, "22001"),
        );
        let reported = show(&diagnostics, false);
        assert_eq!(
            reported.rows,
            vec![vec![
                Cell::Text("Error".to_string()),
                Cell::Integer(1406),
                Cell::Text("Data too long for column 'name' at row 1".to_string()),
            ]],
            "case \"a refused statement\": the list must hold its own refusal, got {reported:?}"
        );
        // A later refusal REPLACES it: the list is the last statement's.
        diagnostics.record(
            &EngineError::new(EngineErrorKind::UniqueViolation, "Duplicate entry '1' for key 'PRIMARY'")
                .with_mysql_error(1062, "23000"),
        );
        let reported = show(&diagnostics, true);
        assert_eq!(
            reported.rows.len(),
            1,
            "case \"a second refusal\": the list must hold only the last statement's, got \
             {reported:?}"
        );
        assert_eq!(
            reported.rows[0][1],
            Cell::Integer(1062),
            "case \"a second refusal\": the condition must be the last one, got {reported:?}"
        );
        diagnostics.clear();
        assert!(
            show(&diagnostics, false).rows.is_empty(),
            "case \"a statement that said nothing\": the list must be empty again"
        );
    }
}
