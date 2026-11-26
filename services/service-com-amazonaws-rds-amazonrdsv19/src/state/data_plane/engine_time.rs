// SPDX-License-Identifier: CC0-1.0

//! The DATE AND TIME functions each engine defines, served by the engine the statement runs
//! on.
//!
//! Both engines answer "what time is it" with functions of their own, and the two families
//! spell them differently: PostgreSQL has `now()` and the transaction and statement
//! timestamps, while the MySQL family has `NOW()`, `UTC_TIMESTAMP()`, `CURDATE()`,
//! `CURTIME()`, `SYSDATE()` and `UNIX_TIMESTAMP()`. A client timestamping a row server-side
//! calls one of them, so a statement that does is run rather than refused.
//!
//! WHICH functions those are, and what each one answers, is
//! [`::service_common::sql_dialect_time`] — one table for every surface of a DB cluster, this
//! endpoint and the cluster's RDS Data API HTTP front end alike, so the two cannot disagree
//! about the built-ins the same cluster has. What lives here is this endpoint's own seam onto
//! it: the SESSION whose wire decides which family a statement runs on.
//!
//! Each function is registered on the engine's own connection ([`register_time_functions`])
//! and reads ONE clock — this process's — so the value a statement writes is the instant the
//! statement ran at. Nothing is derived from the statement's text.
//!
//! A function belongs to a DIALECT: `UTC_TIMESTAMP()` is the MySQL family's and no
//! PostgreSQL server defines it, so calling it on a PostgreSQL session is answered as the
//! undefined function it is, exactly as the other dialect's syntax is
//! ([`super::dialect::dialect_refusal`]). The session's wire comes from the view the
//! statement runs under ([`super::sql_engine::ServerView`]).

use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

use ::service_common::sql_dialect_time::{self as shared, Reading, SqlEngineFamily};

use super::sql_engine::{EngineError, EngineErrorKind, ServerView};
use super::EngineWire;

/// The engine FAMILY a session on `wire` runs on, which is what decides a function's
/// dialect. A statement running under no session view is one of the SERVER's own
/// ([`super::sql_engine::InstanceDatabase::execute`]), which belongs to no family.
#[must_use]
pub fn family_of(wire: Option<EngineWire>) -> Option<SqlEngineFamily> {
    match wire? {
        EngineWire::MySql => Some(SqlEngineFamily::MySql),
        EngineWire::Postgres => Some(SqlEngineFamily::PostgreSql),
    }
}

/// Whether `function` is one of the time functions this endpoint defines, whichever engine
/// defines it — so a statement that calls one is planned and run by the engine rather than
/// substituted into ([`super::dialect`]).
#[must_use]
pub fn is_time_function(function: &str) -> bool {
    shared::is_time_function(function)
}

/// The type the wire reports for a column a call of `function` produced, for the functions
/// whose value is not text; see [`super::pg_server::function_result_type`], which reads this.
#[must_use]
pub fn function_result_type(function: &str) -> Option<&'static str> {
    match shared::reading_of(function)? {
        Reading::Timestamp => Some("timestamptz"),
        Reading::EpochSeconds => Some("bigint"),
        Reading::Date => Some("date"),
        Reading::Time => Some("time"),
    }
}

/// Whether a session on `wire` has the time function `function` at all, or `None` for a name
/// that is not one of them.
///
/// What it is for: a statement that calls a function of the OTHER family is refused as that
/// engine refuses an undefined function ([`super::dialect::dialect_refusal`]), rather than
/// being answered by an engine that happens to have both families' functions registered.
#[must_use]
pub fn is_defined_on(function: &str, wire: EngineWire) -> Option<bool> {
    let family = family_of(Some(wire))?;
    shared::defined_on(function, family)
}

/// Register every time function on `conn`, each reading the session view the statement runs
/// under.
pub fn register_time_functions(
    conn: &Connection,
    view: &Arc<Mutex<ServerView>>,
) -> Result<(), EngineError> {
    let view = Arc::clone(view);
    shared::register_time_functions(conn, move || {
        family_of(view.lock().unwrap_or_else(PoisonError::into_inner).wire)
    })
    .map_err(|error| EngineError::new(EngineErrorKind::Internal, error.to_string()))
}

/// The clock every function here reads: this process's own, as epoch seconds. The one
/// reading of it on this data plane, so a session's own timestamps and the values its
/// statements compute come from the same clock.
#[must_use]
pub fn now_epoch_seconds() -> f64 {
    shared::now_epoch_seconds()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a function's dialect decides which sessions have it, read through the family a
    /// session's own WIRE runs — so each wire has exactly its own family's functions, and a
    /// statement the server runs for itself has neither family's.
    #[test]
    fn a_sessions_wire_decides_which_familys_functions_it_has() {
        // (case, function, wire, defined)
        let rows: &[(&str, &str, EngineWire, Option<bool>)] = &[
            ("now, on a MySQL session", "now", EngineWire::MySql, Some(true)),
            (
                "now, on a PostgreSQL session",
                "now",
                EngineWire::Postgres,
                Some(true),
            ),
            (
                "UTC_TIMESTAMP, on a MySQL session",
                "UTC_TIMESTAMP",
                EngineWire::MySql,
                Some(true),
            ),
            (
                "UTC_TIMESTAMP, on a PostgreSQL session",
                "utc_timestamp",
                EngineWire::Postgres,
                Some(false),
            ),
            (
                "clock_timestamp, on a MySQL session",
                "clock_timestamp",
                EngineWire::MySql,
                Some(false),
            ),
            (
                "clock_timestamp, on a PostgreSQL session",
                "clock_timestamp",
                EngineWire::Postgres,
                Some(true),
            ),
            (
                "a name that is no time function",
                "to_timestamp",
                EngineWire::Postgres,
                None,
            ),
        ];
        for (case, function, wire, expected) in rows {
            let actual = is_defined_on(function, *wire);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(
            family_of(None),
            None,
            "case \"a server statement of its own\": it belongs to no family"
        );
    }

    /// RULE: the type a column of one of these functions is described under is the type its
    /// reading has, and a name that is no time function has none.
    #[test]
    fn each_functions_column_carries_the_type_of_its_reading() {
        for (case, function, expected) in [
            ("a timestamp", "now", Some("timestamptz")),
            ("a MySQL timestamp", "utc_timestamp", Some("timestamptz")),
            ("a date", "curdate", Some("date")),
            ("a time of day", "curtime", Some("time")),
            ("whole seconds since the epoch", "unix_timestamp", Some("bigint")),
            ("a name that is no time function", "to_timestamp", None),
        ] {
            let actual = function_result_type(function);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
