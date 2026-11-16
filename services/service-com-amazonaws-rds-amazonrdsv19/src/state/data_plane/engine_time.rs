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
//! Each is registered on the engine's own connection ([`register_time_functions`]) and reads
//! ONE clock — this process's — so the value a statement writes is the instant the statement
//! ran at. Nothing is derived from the statement's text.
//!
//! A function belongs to a DIALECT: `UTC_TIMESTAMP()` is the MySQL family's and no
//! PostgreSQL server defines it, so calling it on a PostgreSQL session is answered as the
//! undefined function it is, exactly as the other dialect's syntax is
//! ([`super::dialect::dialect_refusal`]). The session's wire comes from the view the
//! statement runs under ([`super::sql_engine::ServerView`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use rusqlite::functions::FunctionFlags;
use rusqlite::Connection;

use super::sql_engine::{EngineError, EngineErrorKind, ServerView};
use super::EngineWire;

/// WHICH engines define a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialects {
    /// Both families define it, each rendering its value its own way.
    Both,
    /// Only the engines that speak one wire define it.
    Only(EngineWire),
}

impl Dialects {
    /// Whether a session on `wire` has this function at all. A statement running under no
    /// session view is one of the SERVER's own ([`super::sql_engine::InstanceDatabase::execute`]),
    /// which calls none of these.
    fn defined_on(self, wire: Option<EngineWire>) -> bool {
        match (self, wire) {
            (Self::Both, _) => true,
            (Self::Only(mine), Some(theirs)) => mine == theirs,
            (Self::Only(_), None) => false,
        }
    }
}

/// WHAT a function answers with, from one reading of the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// The whole timestamp, as the session's own engine renders one.
    Timestamp,
    /// The date alone, `YYYY-MM-DD`.
    Date,
    /// The time of day alone, `HH:MM:SS[.<fraction>]`.
    Time,
    /// The instant as whole seconds since the epoch, which is what MySQL's
    /// `UNIX_TIMESTAMP()` answers.
    EpochSeconds,
}

/// The time functions this endpoint defines: the name a client calls, how many arguments it
/// takes (`-1` for a variable number), which engines define it and what it answers.
///
/// Sources: the MySQL Reference Manual's "Date and Time Functions" — `NOW([fsp])` and
/// `SYSDATE([fsp])` ("returns the current date and time"), `UTC_TIMESTAMP([fsp])`,
/// `UTC_DATE()`, `UTC_TIME([fsp])`, `CURDATE()`, `CURTIME([fsp])` and `UNIX_TIMESTAMP()`
/// ("returns a Unix timestamp … seconds since '1970-01-01 00:00:00' UTC") — and the
/// PostgreSQL documentation's "Date/Time Functions and Operators": `now()`,
/// `transaction_timestamp()`, `statement_timestamp()` and `clock_timestamp()`.
///
/// `CURRENT_TIMESTAMP`, `CURRENT_DATE` and `CURRENT_TIME` are absent on purpose: both engines
/// spell those as KEYWORDS of the SQL standard, and the embedded engine already answers them
/// in that form.
///
/// The optional argument of the MySQL functions is the `fsp`, "the fractional seconds
/// precision" — how many digits of the second the value carries, 0 to 6 — which is what a
/// client asking for `UTC_TIMESTAMP(6)` is asking for.
const TIME_FUNCTIONS: &[(&str, i32, Dialects, Reading)] = &[
    ("now", -1, Dialects::Both, Reading::Timestamp),
    (
        "clock_timestamp",
        0,
        Dialects::Only(EngineWire::Postgres),
        Reading::Timestamp,
    ),
    (
        "transaction_timestamp",
        0,
        Dialects::Only(EngineWire::Postgres),
        Reading::Timestamp,
    ),
    (
        "statement_timestamp",
        0,
        Dialects::Only(EngineWire::Postgres),
        Reading::Timestamp,
    ),
    (
        "utc_timestamp",
        -1,
        Dialects::Only(EngineWire::MySql),
        Reading::Timestamp,
    ),
    (
        "sysdate",
        -1,
        Dialects::Only(EngineWire::MySql),
        Reading::Timestamp,
    ),
    (
        "utc_date",
        0,
        Dialects::Only(EngineWire::MySql),
        Reading::Date,
    ),
    (
        "curdate",
        0,
        Dialects::Only(EngineWire::MySql),
        Reading::Date,
    ),
    (
        "utc_time",
        -1,
        Dialects::Only(EngineWire::MySql),
        Reading::Time,
    ),
    (
        "curtime",
        -1,
        Dialects::Only(EngineWire::MySql),
        Reading::Time,
    ),
    (
        "unix_timestamp",
        0,
        Dialects::Only(EngineWire::MySql),
        Reading::EpochSeconds,
    ),
];

/// The largest `fsp` the MySQL functions take: "the fsp argument … may be given to specify a
/// fractional seconds precision from 0 to 6". It is also how many digits of the second
/// PostgreSQL prints, which is the precision the instant is rendered at.
const MAX_FRACTIONAL_SECONDS_PRECISION: usize = 6;

/// Whether `function` is one of the time functions this endpoint defines, whichever engine
/// defines it — so a statement that calls one is planned and run by the engine rather than
/// substituted into ([`super::dialect`]).
#[must_use]
pub fn is_time_function(function: &str) -> bool {
    TIME_FUNCTIONS
        .iter()
        .any(|(name, ..)| name.eq_ignore_ascii_case(function))
}

/// The type the wire reports for a column a call of `function` produced, for the functions
/// whose value is not text; see [`super::pg_server::function_result_type`], which reads this.
#[must_use]
pub fn function_result_type(function: &str) -> Option<&'static str> {
    match reading_of(function)? {
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
    TIME_FUNCTIONS
        .iter()
        .find(|(name, ..)| name.eq_ignore_ascii_case(function))
        .map(|(_, _, dialects, _)| dialects.defined_on(Some(wire)))
}

/// What `function` answers with, or `None` for a function this module does not define.
fn reading_of(function: &str) -> Option<Reading> {
    TIME_FUNCTIONS
        .iter()
        .find(|(name, ..)| name.eq_ignore_ascii_case(function))
        .map(|(.., reading)| *reading)
}

/// Register every time function on `conn`, each reading the session view the statement runs
/// under.
pub fn register_time_functions(
    conn: &Connection,
    view: &Arc<Mutex<ServerView>>,
) -> Result<(), EngineError> {
    for (name, arguments, dialects, reading) in TIME_FUNCTIONS {
        let name = *name;
        let dialects = *dialects;
        let reading = *reading;
        let view = Arc::clone(view);
        conn.create_scalar_function(
            name,
            *arguments,
            // NOT deterministic: two calls in one statement may read the clock twice, and
            // the engine must not fold them into one.
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let wire = view
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .wire;
                if !dialects.defined_on(wire) {
                    // The sentence the engine itself uses for a function it does not have,
                    // which is the class a client branches on.
                    return Err(rusqlite::Error::UserFunctionError(
                        format!("no such function: {name}").into(),
                    ));
                }
                let precision = match context.len() {
                    0 => None,
                    _ => context.get::<i64>(0).ok().map(|asked| {
                        usize::try_from(asked)
                            .unwrap_or_default()
                            .min(MAX_FRACTIONAL_SECONDS_PRECISION)
                    }),
                };
                Ok(answer(now_epoch_seconds(), wire, reading, precision))
            },
        )
        .map_err(|error| EngineError::new(EngineErrorKind::Internal, error.to_string()))?;
    }
    Ok(())
}

/// The value a function of `reading` answers with for a session on `wire`, at
/// `epoch_seconds`, with the fractional-seconds precision the call asked for.
fn answer(
    epoch_seconds: f64,
    wire: Option<EngineWire>,
    reading: Reading,
    precision: Option<usize>,
) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    if reading == Reading::EpochSeconds {
        return Value::Integer(::aws_smithy_types::DateTime::from_secs_f64(epoch_seconds).secs());
    }
    // One rendering of the instant, in the form PostgreSQL prints a `timestamp with time
    // zone` in: `YYYY-MM-DD HH:MM:SS.ffffff+00`. Every value below is a slice of it, which
    // is why the two engines cannot disagree about the instant they report.
    let rendered = super::pg_server::timestamptz_text(epoch_seconds);
    let (date, rest) = rendered.split_once(' ').unwrap_or((rendered.as_str(), ""));
    let clock = rest.trim_end_matches("+00");
    let (seconds, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    // How many digits of the second the value carries: what the call asked for, else what
    // the engine prints by default — PostgreSQL prints the microseconds it holds, and a
    // MySQL function called without an `fsp` answers whole seconds.
    let digits = match (precision, wire) {
        (Some(asked), _) => asked,
        (None, Some(EngineWire::MySql)) => 0,
        (None, _) => MAX_FRACTIONAL_SECONDS_PRECISION,
    };
    let padded = format!("{fraction:0<6}");
    let fraction = match digits.min(MAX_FRACTIONAL_SECONDS_PRECISION) {
        0 => String::new(),
        digits => format!(".{}", &padded[..digits]),
    };
    Value::Text(match reading {
        Reading::Date => date.to_string(),
        Reading::Time => format!("{seconds}{fraction}"),
        // PostgreSQL renders the offset it ran the value at; the MySQL family renders the
        // date and time alone, in the session's own time zone.
        Reading::Timestamp => match wire {
            Some(EngineWire::MySql) => format!("{date} {seconds}{fraction}"),
            _ => format!("{date} {seconds}{fraction}+00"),
        },
        Reading::EpochSeconds => String::new(),
    })
}

/// The clock every function here reads: this process's own, as epoch seconds. The one
/// reading of it on this data plane, so a session's own timestamps and the values its
/// statements compute come from the same clock.
#[must_use]
pub fn now_epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant to render, with microseconds, so every expectation below is exact:
    /// 2026-02-03 04:05:06.123456 UTC.
    const AT: f64 = 1_770_091_506.123_456;

    /// One row of the rendering table below: the case, the session's wire, what the
    /// function answers, the precision the call asked for and the expected value.
    type Row = (&'static str, Option<EngineWire>, Reading, Option<usize>, &'static str);

    /// RULE: each function answers the reading it is documented to answer, in the form the
    /// session's own engine renders it, with the fractional-seconds precision the call
    /// asked for.
    #[test]
    fn each_reading_is_rendered_as_its_own_engine_renders_it() {
        let rows: &[Row] = &[
            // (case, wire, reading, fsp, expected)
            (
                "a MySQL timestamp with no precision asked for",
                Some(EngineWire::MySql),
                Reading::Timestamp,
                None,
                "2026-02-03 04:05:06",
            ),
            (
                "a MySQL timestamp to microseconds",
                Some(EngineWire::MySql),
                Reading::Timestamp,
                Some(6),
                "2026-02-03 04:05:06.123456",
            ),
            (
                "a MySQL timestamp to milliseconds",
                Some(EngineWire::MySql),
                Reading::Timestamp,
                Some(3),
                "2026-02-03 04:05:06.123",
            ),
            (
                "a precision past the one the engine serves",
                Some(EngineWire::MySql),
                Reading::Timestamp,
                Some(9),
                "2026-02-03 04:05:06.123456",
            ),
            (
                "a MySQL date",
                Some(EngineWire::MySql),
                Reading::Date,
                None,
                "2026-02-03",
            ),
            (
                "a MySQL time of day",
                Some(EngineWire::MySql),
                Reading::Time,
                None,
                "04:05:06",
            ),
            (
                "a PostgreSQL timestamp, which carries its offset",
                Some(EngineWire::Postgres),
                Reading::Timestamp,
                None,
                "2026-02-03 04:05:06.123456+00",
            ),
        ];
        for (case, wire, reading, precision, expected) in rows {
            let answered = answer(AT, *wire, *reading, *precision);
            let actual = match &answered {
                rusqlite::types::Value::Text(text) => text.clone(),
                other => format!("{other:?}"),
            };
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a function only one family defines is defined on that family's sessions alone.
    #[test]
    fn a_functions_dialect_decides_which_sessions_have_it() {
        let rows: &[(&str, &str, Option<EngineWire>, bool)] = &[
            // (case, function, wire, defined)
            ("now, on a MySQL session", "now", Some(EngineWire::MySql), true),
            (
                "now, on a PostgreSQL session",
                "now",
                Some(EngineWire::Postgres),
                true,
            ),
            (
                "UTC_TIMESTAMP, on a MySQL session",
                "utc_timestamp",
                Some(EngineWire::MySql),
                true,
            ),
            (
                "UTC_TIMESTAMP, on a PostgreSQL session",
                "utc_timestamp",
                Some(EngineWire::Postgres),
                false,
            ),
            (
                "clock_timestamp, on a MySQL session",
                "clock_timestamp",
                Some(EngineWire::MySql),
                false,
            ),
            (
                "a server statement of its own, under no session",
                "utc_timestamp",
                None,
                false,
            ),
        ];
        for (case, function, wire, defined) in rows {
            let dialects = TIME_FUNCTIONS
                .iter()
                .find(|(name, ..)| name.eq_ignore_ascii_case(function))
                .map(|(_, _, dialects, _)| *dialects)
                .unwrap_or_else(|| panic!("case {case:?}: {function} must be a time function"));
            let actual = dialects.defined_on(*wire);
            assert_eq!(
                actual, *defined,
                "case {case:?}: expected defined={defined}, got {actual}"
            );
        }
    }
}
