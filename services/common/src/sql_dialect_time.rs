// SPDX-License-Identifier: CC0-1.0

//! The DATE AND TIME functions each SQL engine FAMILY defines, for every data plane in this
//! emulator that runs a client's statement on the in-process SQLite engine.
//!
//! Both families answer "what time is it" with built-in functions of their own, and they
//! spell them differently: PostgreSQL has `now()` and the transaction, statement and clock
//! timestamps, while the MySQL family has `NOW()`, `UTC_TIMESTAMP()`, `SYSDATE()`,
//! `CURDATE()`, `CURTIME()`, `UTC_DATE()`, `UTC_TIME()` and `UNIX_TIMESTAMP()`. A client
//! timestamping a row server-side, computing a window or comparing its own clock to the
//! engine's calls one of them, so a statement that does is RUN rather than refused — and the
//! engine behind these planes has none of them.
//!
//! One home for the table and the rendering, because one DB cluster is served through
//! SEVERAL doors (its engine endpoints and the RDS Data API's HTTP front end) and the dialect
//! a statement is read in is the cluster's, not the door's: the two surfaces would otherwise
//! disagree about which built-ins the same cluster has.
//!
//! * [`TIME_FUNCTIONS`] is the table — the name a client calls, how many arguments it takes,
//!   WHICH families define it and WHAT it reads.
//! * [`register_time_functions`] defines every one of them on a connection, each reading ONE
//!   clock — this process's — so the value a statement writes is the instant it ran at.
//!   Nothing is derived from the statement's text.
//! * A function belongs to a DIALECT: `UTC_TIMESTAMP()` is the MySQL family's and no
//!   PostgreSQL server defines it, so a statement calling it on a PostgreSQL engine is
//!   answered as the undefined function it is ([`UNDEFINED_FUNCTION_PREFIX`]) rather than by
//!   an engine that happens to have both families' functions registered on one connection.
//!   What the CLIENT is told for that condition is the words of the engine the statement ran
//!   on, which the calling plane words.

use ::rusqlite::functions::FunctionFlags;
use ::rusqlite::Connection;

/// Which engine family a statement runs on, as far as its SQL dialect is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlEngineFamily {
    /// MySQL and MariaDB (Aurora MySQL among them).
    MySql,
    /// PostgreSQL (Aurora PostgreSQL among them).
    PostgreSql,
}

/// WHICH families define a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialects {
    /// Both families define it, each rendering its value its own way.
    Both,
    /// Only the engines of one family define it.
    Only(SqlEngineFamily),
}

impl Dialects {
    /// Whether a statement running on `family` has this function at all.
    ///
    /// `None` is a statement running under no client session — one a server runs for itself
    /// — which calls none of the functions only one family defines.
    #[must_use]
    pub fn defined_on(self, family: Option<SqlEngineFamily>) -> bool {
        match (self, family) {
            (Self::Both, _) => true,
            (Self::Only(mine), Some(theirs)) => mine == theirs,
            (Self::Only(_), None) => false,
        }
    }
}

/// WHAT a function answers with, from one reading of the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// The whole timestamp, as the family's own engine renders one.
    Timestamp,
    /// The date alone, `YYYY-MM-DD`.
    Date,
    /// The time of day alone, `HH:MM:SS[.<fraction>]`.
    Time,
    /// The instant as whole seconds since the epoch, which is what MySQL's
    /// `UNIX_TIMESTAMP()` answers.
    EpochSeconds,
}

/// One date-and-time function, as the engines' own references define it.
#[derive(Debug, Clone, Copy)]
pub struct TimeFunction {
    /// The name a client calls it by, lower case; a statement may spell it in any case.
    pub name: &'static str,
    /// How many arguments it takes, with `-1` for a variable number.
    pub arguments: i32,
    /// Which families define it.
    pub dialects: Dialects,
    /// What it reads.
    pub reading: Reading,
}

/// The date-and-time functions these data planes define.
///
/// Sources: the MySQL Reference Manual's "Date and Time Functions" — `NOW([fsp])` and
/// `SYSDATE([fsp])` ("returns the current date and time"), `UTC_TIMESTAMP([fsp])`,
/// `UTC_DATE()`, `UTC_TIME([fsp])`, `CURDATE()`, `CURTIME([fsp])` and `UNIX_TIMESTAMP()`
/// ("returns a Unix timestamp … seconds since '1970-01-01 00:00:00' UTC") — and the
/// PostgreSQL documentation's "Date/Time Functions and Operators": `now()`,
/// `transaction_timestamp()`, `statement_timestamp()` and `clock_timestamp()`.
///
/// `CURRENT_TIMESTAMP`, `CURRENT_DATE` and `CURRENT_TIME` are absent on purpose: both
/// families spell those as KEYWORDS of the SQL standard, which the engine behind these
/// planes already answers in that form.
///
/// The optional argument of the MySQL functions is the `fsp`, "the fractional seconds
/// precision" — how many digits of the second the value carries, 0 to 6 — which is what a
/// client asking for `UTC_TIMESTAMP(6)` is asking for.
pub const TIME_FUNCTIONS: &[TimeFunction] = &[
    TimeFunction {
        name: "now",
        arguments: -1,
        dialects: Dialects::Both,
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "clock_timestamp",
        arguments: 0,
        dialects: Dialects::Only(SqlEngineFamily::PostgreSql),
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "transaction_timestamp",
        arguments: 0,
        dialects: Dialects::Only(SqlEngineFamily::PostgreSql),
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "statement_timestamp",
        arguments: 0,
        dialects: Dialects::Only(SqlEngineFamily::PostgreSql),
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "utc_timestamp",
        arguments: -1,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "sysdate",
        arguments: -1,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Timestamp,
    },
    TimeFunction {
        name: "utc_date",
        arguments: 0,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Date,
    },
    TimeFunction {
        name: "curdate",
        arguments: 0,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Date,
    },
    TimeFunction {
        name: "utc_time",
        arguments: -1,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Time,
    },
    TimeFunction {
        name: "curtime",
        arguments: -1,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::Time,
    },
    TimeFunction {
        name: "unix_timestamp",
        arguments: -1,
        dialects: Dialects::Only(SqlEngineFamily::MySql),
        reading: Reading::EpochSeconds,
    },
];

/// The largest `fsp` the MySQL functions take: "the fsp argument … may be given to specify a
/// fractional seconds precision from 0 to 6". It is also how many digits of the second
/// PostgreSQL prints, which is the precision the instant is rendered at.
pub const MAX_FRACTIONAL_SECONDS_PRECISION: usize = 6;

/// How the engine behind these planes introduces a function it does not have, which is the
/// sentence a call of the OTHER family's function is refused with: the condition every plane
/// then words in the sentence of the engine its statement ran on.
pub const UNDEFINED_FUNCTION_PREFIX: &str = "no such function: ";

/// The function `function` names, or `None` for a name this table does not carry. Names are
/// compared without regard to case, because SQL function names are.
#[must_use]
pub fn lookup(function: &str) -> Option<&'static TimeFunction> {
    TIME_FUNCTIONS
        .iter()
        .find(|known| known.name.eq_ignore_ascii_case(function))
}

/// Whether `function` is one of the date-and-time functions these planes define, whichever
/// family defines it — so a statement that calls one is planned and run by the engine.
#[must_use]
pub fn is_time_function(function: &str) -> bool {
    lookup(function).is_some()
}

/// Whether a statement running on `family` has the function `function` at all, or `None` for
/// a name that is not one of them.
#[must_use]
pub fn defined_on(function: &str, family: SqlEngineFamily) -> Option<bool> {
    Some(lookup(function)?.dialects.defined_on(Some(family)))
}

/// What `function` reads, or `None` for a function this table does not carry.
#[must_use]
pub fn reading_of(function: &str) -> Option<Reading> {
    Some(lookup(function)?.reading)
}

/// One value a date-and-time function answers with, in the storage class it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeValue {
    /// A rendered date, time or timestamp.
    Text(String),
    /// A count of seconds.
    Integer(i64),
}

/// The value a function of `reading` answers with on `family`, at `epoch_seconds`, with the
/// fractional-seconds precision the call asked for.
#[must_use]
pub fn reading_at(
    epoch_seconds: f64,
    family: Option<SqlEngineFamily>,
    reading: Reading,
    precision: Option<usize>,
) -> TimeValue {
    if reading == Reading::EpochSeconds {
        return TimeValue::Integer(
            ::aws_smithy_types::DateTime::from_secs_f64(epoch_seconds).secs(),
        );
    }
    // One rendering of the instant, in the form PostgreSQL prints a `timestamp with time
    // zone` in: `YYYY-MM-DD HH:MM:SS.ffffff+00`. Every value below is a slice of it, which
    // is why the two families cannot disagree about the instant they report.
    let rendered = postgres_timestamp_text(epoch_seconds);
    let (date, rest) = rendered.split_once(' ').unwrap_or((rendered.as_str(), ""));
    let clock = rest.trim_end_matches("+00");
    let (seconds, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    // How many digits of the second the value carries: what the call asked for, else what
    // the engine prints by default — PostgreSQL prints the microseconds it holds, and a
    // MySQL function called without an `fsp` answers whole seconds.
    let digits = match (precision, family) {
        (Some(asked), _) => asked,
        (None, Some(SqlEngineFamily::MySql)) => 0,
        (None, _) => MAX_FRACTIONAL_SECONDS_PRECISION,
    };
    let padded = format!("{fraction:0<6}");
    let fraction = match digits.min(MAX_FRACTIONAL_SECONDS_PRECISION) {
        0 => String::new(),
        digits => format!(".{}", &padded[..digits]),
    };
    TimeValue::Text(match reading {
        Reading::Date => date.to_string(),
        Reading::Time => format!("{seconds}{fraction}"),
        // PostgreSQL renders the offset it ran the value at; the MySQL family renders the
        // date and time alone, in the session's own time zone.
        Reading::Timestamp => match family {
            Some(SqlEngineFamily::MySql) => format!("{date} {seconds}{fraction}"),
            _ => format!("{date} {seconds}{fraction}+00"),
        },
        Reading::EpochSeconds => String::new(),
    })
}

/// A timestamp in the text form PostgreSQL renders `timestamp with time zone` in: the date
/// and time to microseconds, separated by a space, with the UTC offset the server runs at.
///
/// The calendar is the SDK timestamp type's: `aws_smithy_types::DateTime` holds the instant
/// and renders it as RFC 3339, which is the same fields in the same order — this only
/// re-spells the two separators PostgreSQL writes differently (a space instead of `T`, and
/// the offset instead of `Z`) and pads the fractional seconds to the six digits it always
/// prints.
#[must_use]
pub fn postgres_timestamp_text(epoch_seconds: f64) -> String {
    let at = ::aws_smithy_types::DateTime::from_secs_f64(epoch_seconds);
    let Ok(rendered) = at.fmt(::aws_smithy_types::date_time::Format::DateTime) else {
        // Only reachable for an instant outside the range the format covers at all, which
        // no clock of this emulator produces.
        return epoch_seconds.to_string();
    };
    let (date, rest) = rendered.split_once('T').unwrap_or((rendered.as_str(), ""));
    let time = rest.trim_end_matches('Z');
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let micros = format!("{fraction:0<6}");
    format!("{date} {clock}.{}+00", &micros[..6])
}

/// The clock every function here reads: this process's own, as epoch seconds.
#[must_use]
pub fn now_epoch_seconds() -> f64 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The seconds since the epoch the date and time `written` names, read in the forms a client
/// writes one in — the date and the time separated by a space or by ISO 8601's `T`, with or
/// without a fraction of a second, and a date alone, which names that day's midnight.
///
/// `None` for text that is no date and time at all, which has no such count. Read by the
/// maintained calendar crate this workspace already reads calendar fields with, so no date
/// arithmetic is written here.
#[must_use]
pub fn epoch_seconds_of(written: &str) -> Option<i64> {
    const DATE_AND_TIME: [&str; 4] = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
    ];
    let written = written.trim().trim_end_matches('Z');
    for description in DATE_AND_TIME {
        if let Ok(at) = ::chrono::NaiveDateTime::parse_from_str(written, description) {
            return Some(at.and_utc().timestamp());
        }
    }
    ::chrono::NaiveDate::parse_from_str(written, "%Y-%m-%d")
        .ok()
        .map(|day| day.and_time(::chrono::NaiveTime::MIN).and_utc().timestamp())
}

/// Define every function of [`TIME_FUNCTIONS`] on `conn`, answering for the family
/// `family_of_statement` reports at the moment a statement calls one.
///
/// The family is read PER CALL rather than captured, because the plane that owns the
/// connection may learn which engine it serves after the connection was opened — and a
/// statement a server runs for itself belongs to no client session at all (`None`).
///
/// A call of a function the family does not define is refused with the engine's own sentence
/// for a function it does not have ([`UNDEFINED_FUNCTION_PREFIX`]), so a client probing which
/// engine it is talking to cannot see the other family's built-ins.
pub fn register_time_functions<F>(conn: &Connection, family_of_statement: F) -> ::rusqlite::Result<()>
where
    F: Fn() -> Option<SqlEngineFamily> + Clone + Send + 'static,
{
    for function in TIME_FUNCTIONS {
        let TimeFunction {
            name,
            arguments,
            dialects,
            reading,
        } = *function;
        let family_of_statement = family_of_statement.clone();
        conn.create_scalar_function(
            name,
            arguments,
            // NOT deterministic: two calls in one statement may read the clock twice, and
            // the engine must not fold them into one.
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let family = family_of_statement();
                if !dialects.defined_on(family) {
                    return Err(::rusqlite::Error::UserFunctionError(
                        format!("{UNDEFINED_FUNCTION_PREFIX}{name}").into(),
                    ));
                }
                // `UNIX_TIMESTAMP([date])` is the one function here whose argument is not a
                // precision but a VALUE: "if called with a date argument, returns the value
                // of the argument as seconds since '1970-01-01 00:00:00' UTC". A value that
                // is no date and time at all has no such count, which that engine answers
                // as no value.
                if reading == Reading::EpochSeconds && context.len() == 1 {
                    return Ok(match context.get::<String>(0) {
                        Ok(written) => match epoch_seconds_of(&written) {
                            Some(seconds) => ::rusqlite::types::Value::Integer(seconds),
                            None => ::rusqlite::types::Value::Null,
                        },
                        Err(_) => ::rusqlite::types::Value::Null,
                    });
                }
                let precision = match context.len() {
                    0 => None,
                    _ => context.get::<i64>(0).ok().map(|asked| {
                        usize::try_from(asked)
                            .unwrap_or_default()
                            .min(MAX_FRACTIONAL_SECONDS_PRECISION)
                    }),
                };
                Ok(
                    match reading_at(now_epoch_seconds(), family, reading, precision) {
                        TimeValue::Text(text) => ::rusqlite::types::Value::Text(text),
                        TimeValue::Integer(number) => ::rusqlite::types::Value::Integer(number),
                    },
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant to render, with microseconds, so every expectation below is exact:
    /// 2026-02-03 04:05:06.123456 UTC.
    const AT: f64 = 1_770_091_506.123_456;

    /// One row of the rendering table: the case, the family, what the function reads, the
    /// precision the call asked for and the expected value.
    type RenderRow = (
        &'static str,
        Option<SqlEngineFamily>,
        Reading,
        Option<usize>,
        TimeValue,
    );

    /// RULE: each function answers the reading its own reference documents, rendered in the
    /// form the family's engine renders that reading in, with the fractional-seconds
    /// precision the call asked for.
    #[test]
    fn each_reading_is_rendered_as_its_own_family_renders_it() {
        let rows: Vec<RenderRow> = vec![
            (
                "a MySQL timestamp with no precision asked for",
                Some(SqlEngineFamily::MySql),
                Reading::Timestamp,
                None,
                TimeValue::Text("2026-02-03 04:05:06".to_string()),
            ),
            (
                "a MySQL timestamp to microseconds",
                Some(SqlEngineFamily::MySql),
                Reading::Timestamp,
                Some(6),
                TimeValue::Text("2026-02-03 04:05:06.123456".to_string()),
            ),
            (
                "a MySQL timestamp to milliseconds",
                Some(SqlEngineFamily::MySql),
                Reading::Timestamp,
                Some(3),
                TimeValue::Text("2026-02-03 04:05:06.123".to_string()),
            ),
            (
                "a precision past the one the engine serves",
                Some(SqlEngineFamily::MySql),
                Reading::Timestamp,
                Some(9),
                TimeValue::Text("2026-02-03 04:05:06.123456".to_string()),
            ),
            (
                "a MySQL date",
                Some(SqlEngineFamily::MySql),
                Reading::Date,
                None,
                TimeValue::Text("2026-02-03".to_string()),
            ),
            (
                "a MySQL time of day",
                Some(SqlEngineFamily::MySql),
                Reading::Time,
                None,
                TimeValue::Text("04:05:06".to_string()),
            ),
            (
                "a MySQL Unix timestamp, which is whole seconds",
                Some(SqlEngineFamily::MySql),
                Reading::EpochSeconds,
                None,
                TimeValue::Integer(1_770_091_506),
            ),
            (
                "a PostgreSQL timestamp, which carries its offset",
                Some(SqlEngineFamily::PostgreSql),
                Reading::Timestamp,
                None,
                TimeValue::Text("2026-02-03 04:05:06.123456+00".to_string()),
            ),
        ];
        for (case, family, reading, precision, expected) in rows {
            let actual = reading_at(AT, family, reading, precision);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a function only one family defines is defined on that family's statements
    /// alone, whatever case the statement spells it in.
    #[test]
    fn a_functions_dialect_decides_which_statements_have_it() {
        // (case, function, family, defined)
        let rows: &[(&str, &str, Option<SqlEngineFamily>, bool)] = &[
            (
                "now, on a MySQL statement",
                "now",
                Some(SqlEngineFamily::MySql),
                true,
            ),
            (
                "now, on a PostgreSQL statement",
                "NOW",
                Some(SqlEngineFamily::PostgreSql),
                true,
            ),
            (
                "UTC_TIMESTAMP, on a MySQL statement",
                "UTC_TIMESTAMP",
                Some(SqlEngineFamily::MySql),
                true,
            ),
            (
                "UTC_TIMESTAMP, on a PostgreSQL statement",
                "utc_timestamp",
                Some(SqlEngineFamily::PostgreSql),
                false,
            ),
            (
                "clock_timestamp, on a MySQL statement",
                "clock_timestamp",
                Some(SqlEngineFamily::MySql),
                false,
            ),
            (
                "clock_timestamp, on a PostgreSQL statement",
                "Clock_Timestamp",
                Some(SqlEngineFamily::PostgreSql),
                true,
            ),
            (
                "a server statement of its own, under no session",
                "utc_timestamp",
                None,
                false,
            ),
        ];
        for (case, function, family, defined) in rows {
            let dialects = lookup(function)
                .unwrap_or_else(|| panic!("case {case:?}: {function} must be a time function"))
                .dialects;
            let actual = dialects.defined_on(*family);
            assert_eq!(
                actual, *defined,
                "case {case:?}: expected defined={defined}, got {actual}"
            );
        }
        assert!(
            !is_time_function("to_timestamp"),
            "case \"a function this table does not carry\": expected it to be absent"
        );
    }

    /// RULE: the seconds a date and time names are counted from the value, in each of the
    /// forms a client writes one in, and text that is no date and time at all names no count.
    #[test]
    fn the_seconds_a_written_date_and_time_names_are_counted_from_it() {
        for (case, written, expected) in [
            ("a date and time", "2026-02-03 04:05:06", Some(1_770_091_506)),
            (
                "a date and time to microseconds",
                "2026-02-03 04:05:06.123456",
                Some(1_770_091_506),
            ),
            (
                "the ISO 8601 separator",
                "2026-02-03T04:05:06",
                Some(1_770_091_506),
            ),
            (
                "a value carrying the UTC designator",
                "2026-02-03T04:05:06Z",
                Some(1_770_091_506),
            ),
            ("a date alone, which is its midnight", "2026-02-03", Some(1_770_076_800)),
            ("the epoch itself", "1970-01-01 00:00:00", Some(0)),
            ("an instant before the epoch", "1969-12-31 23:59:59", Some(-1)),
            ("text that is no date and time", "not a date", None),
            ("a day no calendar has", "2026-02-30", None),
            ("nothing at all", "", None),
        ] {
            let actual = epoch_seconds_of(written);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for {written:?}, got {actual:?}"
            );
        }
    }

    /// RULE: every function of the table is DEFINED on a connection and computes from the
    /// clock, and one the family does not define is refused as the engine refuses a function
    /// it does not have.
    #[test]
    fn the_functions_are_defined_on_the_connection_and_computed_by_the_engine() {
        for (case, family, calls) in [
            (
                "a MySQL engine",
                SqlEngineFamily::MySql,
                vec![
                    ("SELECT UTC_TIMESTAMP(6)", true),
                    ("SELECT utc_timestamp()", true),
                    ("SELECT NOW()", true),
                    ("SELECT CURDATE()", true),
                    ("SELECT UNIX_TIMESTAMP()", true),
                    // The documented one-argument form: the seconds the value names.
                    ("SELECT UNIX_TIMESTAMP('2026-02-03 04:05:06')", true),
                    ("SELECT clock_timestamp()", false),
                ],
            ),
            (
                "a PostgreSQL engine",
                SqlEngineFamily::PostgreSql,
                vec![
                    ("SELECT now()", true),
                    ("SELECT clock_timestamp()", true),
                    ("SELECT UTC_TIMESTAMP(6)", false),
                    ("SELECT CURDATE()", false),
                ],
            ),
        ] {
            let conn = Connection::open_in_memory().expect("the engine opens");
            register_time_functions(&conn, move || Some(family)).expect("the functions define");
            for (sql, defined) in calls {
                let answered: ::rusqlite::Result<::rusqlite::types::Value> =
                    conn.query_row(sql, [], |row| row.get(0));
                assert_eq!(
                    answered.is_ok(),
                    defined,
                    "case {case:?} {sql:?}: expected defined={defined}, got {answered:?}"
                );
                match answered {
                    Ok(value) => assert!(
                        !matches!(value, ::rusqlite::types::Value::Null),
                        "case {case:?} {sql:?}: expected a value from the clock, got {value:?}"
                    ),
                    Err(error) => assert!(
                        error.to_string().contains(UNDEFINED_FUNCTION_PREFIX),
                        "case {case:?} {sql:?}: expected the engine's undefined-function \
                         sentence, got {error}"
                    ),
                }
            }
        }
    }
}
