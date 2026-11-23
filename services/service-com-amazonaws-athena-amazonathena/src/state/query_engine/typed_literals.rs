// SPDX-License-Identifier: CC0-1.0

//! The TYPED LITERAL a statement writes a date or a time as, which this engine's dialect has
//! no production for.
//!
//! Athena's SQL is Trino's, in which a value of a date/time type is written as the TYPE NAME
//! followed by the text of the value — `DATE '2024-03-05'`, `TIME '06:07:08'`,
//! `TIMESTAMP '2024-03-05 06:07:08.123'` — and that literal is an expression wherever an
//! expression may stand. The engine under [`super`] (SQLite) has no such production and no
//! such type: it reads `TIMESTAMP '…'` as the column `TIMESTAMP` followed by a string, which
//! is either a column alias or a syntax error, so a statement AWS runs is refused instead of
//! answered.
//!
//! What this module does is put that literal where the engine HAS a production for it, in the
//! two halves the readings beside it are built from ([`super::exact_decimal`],
//! [`super::strict_casts`]):
//!
//! * the engine function this registers on every connection
//!   ([`register_typed_literals`]): one that answers the text of the value a typed literal
//!   names, and refuses in Athena's own words when the text names no value of that type;
//! * the rewriting that puts the literal in its place ([`as_values`]): every typed date/time
//!   literal in the statement — in the select list, in a predicate, in a list of values —
//!   becomes a call of that function over the SAME text.
//!
//! The rewrite is on the PARSE — the statement is re-rendered from the tree it changed, so
//! nothing is spliced into the submitted text — and the column the call leaves behind is
//! typed by it ([`super::projection`]), so a result set reports `date`, `time` or `timestamp`
//! for such a column rather than the character type the engine holds the value's text in.
//!
//! The value is carried as the CALENDAR TEXT of the instant it names, which is also what
//! Athena renders a value of these types as, zero-padded and at the number of fractional
//! digits the literal was written with. The ZONED forms (`TIMESTAMP WITH TIME ZONE`,
//! `TIME WITH TIME ZONE`) are deliberately left alone: a zone offset is a part of such a
//! value that neither the engine holds nor this crate carries a zone database for.

use ::chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use ::rusqlite::functions::{Context, FunctionFlags};
use ::rusqlite::types::{Value, ValueRef};
use ::sqlparser::ast::{
    DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, TimezoneInfo, Value as SqlValue,
};
use std::ops::ControlFlow;

use super::ColumnType;
use crate::state::catalog_sql::statement::parse_statements;

/// The name the engine function is registered and called under.
///
/// It carries the prefix every name this crate puts in a statement of its own carries (see
/// `super::table_functions`), so it is no name of Athena's own SQL.
pub(super) const TEMPORAL_LITERAL_FUNCTION: &str = "__athena_temporal_literal";

/// How many arguments that function takes: the type the literal names, and its text.
const TEMPORAL_LITERAL_ARGUMENTS: i32 = 2;

/// What Athena's message for a literal it cannot read opens with: the class of a text that
/// names no value of the type its literal names.
const INVALID_LITERAL: &str = "INVALID_LITERAL";

/// How many digits a `TIME` or `TIMESTAMP` value carries after the point at most.
///
/// Source: Athena's data-type reference, whose SQL is Trino's — a `time(p)` and a
/// `timestamp(p)` carry a precision `p` of up to 12 digits, and the precision of a LITERAL is
/// the number of fractional digits it was written with. Text with more digits than that names
/// a value of no such type.
const MAX_FRACTIONAL_DIGITS: usize = 12;

/// The separator between the calendar day and the time of day of a timestamp, as a literal
/// may be written with either: Trino's own date-time literal takes the space and the `T` of
/// ISO 8601 alike.
const TIMESTAMP_SEPARATORS: [char; 2] = [' ', 'T'];

/// The date/time types a literal of which is read here, each under the name Athena's own
/// result metadata reports it by.
///
/// Source: Athena's data-type reference — `date` is a calendar day, `time` a time of day and
/// `timestamp` an instant written as a day and a time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Temporal {
    /// A calendar day.
    Date,
    /// A time of day.
    Time,
    /// A calendar day with a time of day.
    Timestamp,
}

impl Temporal {
    /// The type `data_type` names, or `None` for a type whose literal is left as written.
    ///
    /// Only the forms carrying NO time zone and no declared precision are read: a zoned value
    /// is one this engine holds no zone for (see the module docs), and a precision declared on
    /// the type itself is no literal of Athena's own grammar, whose precision comes from the
    /// digits the text carries.
    fn of(data_type: &DataType) -> Option<Self> {
        let unzoned =
            |zone: &TimezoneInfo| matches!(zone, TimezoneInfo::None | TimezoneInfo::WithoutTimeZone);
        match data_type {
            DataType::Date => Some(Self::Date),
            DataType::Time(None, zone) if unzoned(zone) => Some(Self::Time),
            DataType::Timestamp(None, zone) if unzoned(zone) => Some(Self::Timestamp),
            _ => None,
        }
    }

    /// The name a result set and a refusal name this type by.
    fn named(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Time => "time",
            Self::Timestamp => "timestamp",
        }
    }

    /// The same, read back off the name the rewriting wrote into the statement.
    fn by_name(named: &str) -> Option<Self> {
        [Self::Date, Self::Time, Self::Timestamp]
            .into_iter()
            .find(|target| target.named() == named)
    }

    /// The type a result set reports for a column this literal produced.
    fn reported(self) -> ColumnType {
        match self {
            Self::Date => ColumnType::Date,
            Self::Time => ColumnType::Time,
            Self::Timestamp => ColumnType::Timestamp,
        }
    }

    /// The VALUE `text` names at this type, as the calendar text Athena renders it as — and
    /// `None` where it names no value of this type at all.
    ///
    /// The text is read as the day, the time of day and the fractional digits it carries, and
    /// rendered zero-padded at exactly the number of fractional digits it was written with:
    /// that count is the literal's own precision, so a value written with none carries none
    /// and one written with three carries three.
    fn value_of(self, text: &str) -> Option<String> {
        let text = text.trim();
        match self {
            Self::Date => {
                let day = calendar_day(text)?;
                Some(rendered_day(day))
            }
            Self::Time => {
                let (base, fraction) = split_fraction(text)?;
                let time = time_of_day(base)?;
                Some(format!("{}{fraction}", rendered_time(time)))
            }
            Self::Timestamp => {
                let (base, fraction) = split_fraction(text)?;
                let (day, time) = base
                    .split_once(TIMESTAMP_SEPARATORS)
                    .map(|(day, time)| (day.trim_end(), time.trim_start()))?;
                let day = calendar_day(day)?;
                let time = time_of_day(time)?;
                Some(format!(
                    "{} {}{fraction}",
                    rendered_day(day),
                    rendered_time(time)
                ))
            }
        }
    }
}

/// The FRACTIONAL digits `text` ends with, split off from the rest of it: the text in front of
/// the point, and the fractional part as it is rendered back (the point and those digits, or
/// nothing at all).
///
/// `None` where the text carries a point that is followed by something other than digits, or
/// by more of them than a value of these types holds ([`MAX_FRACTIONAL_DIGITS`]): both name no
/// value.
fn split_fraction(text: &str) -> Option<(&str, String)> {
    let Some((base, digits)) = text.rsplit_once('.') else {
        return Some((text, String::new()));
    };
    let held = !digits.is_empty()
        && digits.len() <= MAX_FRACTIONAL_DIGITS
        && digits.chars().all(|digit| digit.is_ascii_digit());
    held.then(|| (base, format!(".{digits}")))
}

/// The calendar day `text` names, read as the year, month and day of a date literal.
fn calendar_day(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// The time of day `text` names, with the seconds a literal may leave out taken as none.
fn time_of_day(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M"))
        .ok()
}

/// One calendar day as Athena renders a `date`: the year, month and day, zero-padded.
///
/// Rendered from the day's own parts rather than through a format of the date library, which
/// writes a year outside four digits with a sign in front of it.
fn rendered_day(day: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", day.year(), day.month(), day.day())
}

/// One time of day as Athena renders the whole-second part of a `time`; see [`rendered_day`].
fn rendered_time(time: NaiveTime) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// Register the typed-literal reading on `connection`, so a statement the rewriting below has
/// put it in is planned and run by the engine like any other call.
pub(super) fn register_typed_literals(
    connection: &::rusqlite::Connection,
) -> ::rusqlite::Result<()> {
    connection.create_scalar_function(
        TEMPORAL_LITERAL_FUNCTION,
        TEMPORAL_LITERAL_ARGUMENTS,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        temporal_value,
    )
}

/// The value one typed literal names, answered as the text Athena renders it as, and refused
/// in Athena's own words when its text names no value of the type the literal names.
fn temporal_value(context: &Context<'_>) -> ::rusqlite::Result<Value> {
    // The call SHAPE first: the rewriting calls this with the two arguments it takes, and the
    // name is also one a submitted statement can write.
    if context.len() != 2 {
        return Err(refused(format!(
            "{TEMPORAL_LITERAL_FUNCTION} takes the name of a type and the text of a value"
        )));
    }
    let named = context.get::<String>(0).unwrap_or_default();
    let Some(target) = Temporal::by_name(&named) else {
        return Err(refused(format!(
            "{TEMPORAL_LITERAL_FUNCTION} was handed {named:?}, which names no type it reads"
        )));
    };
    match context.get_raw(1) {
        // A literal of NULL is no text at all, and NULL is a value of every type.
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            match target.value_of(&text) {
                Some(value) => Ok(Value::Text(value)),
                None => Err(refused(not_a_literal(&text, target))),
            }
        }
        ValueRef::Integer(number) => Err(refused(not_a_literal(&number.to_string(), target))),
        ValueRef::Real(number) => Err(refused(not_a_literal(&number.to_string(), target))),
        ValueRef::Blob(bytes) => Err(refused(not_a_literal(
            &String::from_utf8_lossy(bytes),
            target,
        ))),
    }
}

/// Athena's message for a text that names no value of the type its literal names: the class it
/// belongs to, then fixed text either side of the text as it reads and the type it was written
/// at.
fn not_a_literal(text: &str, target: Temporal) -> String {
    format!(
        "{INVALID_LITERAL}: '{text}' is not a valid {} literal",
        target.named()
    )
}

/// The engine's own refusal, carrying the sentence the execution reports.
fn refused(message: String) -> ::rusqlite::Error {
    ::rusqlite::Error::UserFunctionError(message.into())
}

/// `sql` with every typed date/time literal read as the value it names, or `None` when it
/// carries none — in which case the submitted text is what the engine runs.
pub(super) fn as_values(sql: &str) -> Option<String> {
    // Nothing to read where the text names neither type: the check is on the submitted text,
    // so a statement carrying no such literal is not parsed again here. `TIMESTAMP` carries
    // `TIME`, so the two words below cover all three type names.
    let upper = sql.to_ascii_uppercase();
    if !upper.contains("DATE") && !upper.contains("TIME") {
        return None;
    }
    // Parsed through the one reader this crate reads a submitted statement with, so the
    // bounds it holds a parse to (the token count, the recursion limit) hold here too.
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    let mut statement = statements.remove(0);
    let mut rewritten = false;
    let _ = ::sqlparser::ast::visit_expressions_mut(&mut statement, |expression: &mut Expr| {
        if let Some(reading) = reading_of(expression) {
            *expression = reading;
            rewritten = true;
        }
        ControlFlow::<()>::Continue(())
    });
    rewritten.then(|| statement.to_string())
}

/// The call that reads `expression` as the value its typed literal names, and `None` for every
/// other expression — which is left exactly as it was written.
fn reading_of(expression: &Expr) -> Option<Expr> {
    let Expr::TypedString(typed) = expression else {
        return None;
    };
    let target = Temporal::of(&typed.data_type)?;
    // Only a literal written as TEXT is one of these: the type name in front of a number or a
    // keyword is no date/time literal of Athena's grammar.
    if !matches!(
        typed.value.value,
        SqlValue::SingleQuotedString(..)
            | SqlValue::DoubleQuotedString(..)
            | SqlValue::EscapedStringLiteral(..)
            | SqlValue::UnicodeStringLiteral(..)
    ) {
        return None;
    }
    // Built by PARSING the call, over the literal as it was written: the text reaches the
    // reading as the one string the statement carried, so nothing re-quotes it on the way.
    let call = format!(
        "{TEMPORAL_LITERAL_FUNCTION}('{}', {})",
        target.named(),
        typed.value
    );
    ::sqlparser::parser::Parser::new(&::sqlparser::dialect::GenericDialect {})
        .try_with_sql(&call)
        .ok()?
        .parse_expr()
        .ok()
}

/// The type a column one of these readings produced reports: the type the call names, read
/// back off the call the rewriting left behind.
pub(super) fn projected_type(function: &Function) -> Option<ColumnType> {
    let FunctionArguments::List(FunctionArgumentList { args, .. }) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(named)), ..] = &args[..] else {
        return None;
    };
    let Expr::Value(value) = named else {
        return None;
    };
    let SqlValue::SingleQuotedString(named) = &value.value else {
        return None;
    };
    Temporal::by_name(named).map(Temporal::reported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typed date/time literal is read as the value it names wherever it stands, and every
    /// other statement is handed on as written. The rows differ along the type the literal
    /// names, where in the statement it stands, how many of them there are, and whether there
    /// is such a literal at all.
    #[test]
    fn a_typed_date_or_time_literal_is_read_where_the_engine_has_a_production_for_it() {
        // (case, the statement submitted, the fragments the rewrite must carry: `None` for
        // the text as written)
        let cases: Vec<(&str, &str, Option<Vec<&str>>)> = vec![
            ("no literal of either type", "SELECT 1 AS one", None),
            (
                "a column whose NAME carries one of the type words",
                "SELECT updated_date FROM \"t\"",
                None,
            ),
            (
                "a cast to a date, which the engine reads as its own",
                "SELECT CAST(day AS date) FROM \"t\"",
                None,
            ),
            (
                "a date literal",
                "SELECT DATE '2024-03-05' AS d",
                Some(vec!["__athena_temporal_literal('date', '2024-03-05')"]),
            ),
            (
                "a time literal",
                "SELECT TIME '06:07:08' AS t",
                Some(vec!["__athena_temporal_literal('time', '06:07:08')"]),
            ),
            (
                "a timestamp literal",
                "SELECT TIMESTAMP '2024-03-05 06:07:08.123' AS ts",
                Some(vec![
                    "__athena_temporal_literal('timestamp', '2024-03-05 06:07:08.123')",
                ]),
            ),
            (
                "the same literal written in lower case",
                "select timestamp '2024-03-05 06:07:08' as ts",
                Some(vec![
                    "__athena_temporal_literal('timestamp', '2024-03-05 06:07:08')",
                ]),
            ),
            (
                "a literal in a predicate rather than in the select list",
                "SELECT id FROM \"t\" WHERE day > DATE '2024-03-05'",
                Some(vec!["__athena_temporal_literal('date', '2024-03-05')"]),
            ),
            (
                "a literal inside a cast",
                "SELECT CAST(DATE '2024-03-05' AS varchar) AS d",
                Some(vec!["__athena_temporal_literal('date', '2024-03-05')"]),
            ),
            (
                "two literals of different types in one statement",
                "SELECT DATE '2024-03-05' AS d, TIME '06:07:08' AS t",
                Some(vec![
                    "__athena_temporal_literal('date', '2024-03-05')",
                    "__athena_temporal_literal('time', '06:07:08')",
                ]),
            ),
            (
                "a zoned literal, which carries a zone this engine holds none of",
                "SELECT TIMESTAMP WITH TIME ZONE '2024-03-05 06:07:08 +01:00' AS ts",
                None,
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = as_values(sql);
            match expected {
                None => assert!(
                    actual.is_none(),
                    "case {case:?}: expected the statement to be handed on as written, got \
                     {actual:?}"
                ),
                Some(fragments) => {
                    let rewritten = actual.unwrap_or_else(|| {
                        panic!("case {case:?}: expected the statement to be rewritten, got none")
                    });
                    for fragment in fragments {
                        assert!(
                            rewritten.contains(fragment),
                            "case {case:?}: expected the rewrite to carry {fragment:?}, got \
                             {rewritten:?}"
                        );
                    }
                }
            }
        }
    }

    /// RULE: the text of a typed literal names a value of that type exactly where it reads as
    /// one on the calendar, and the value is rendered zero-padded at the fractional digits the
    /// literal carries; every other text names no value and is refused naming it and the type.
    ///
    /// The rows span the three types, the separators and the padding a literal may be written
    /// with, the published bound on the fractional digits and one past it, and the texts that
    /// name no instant at all.
    #[test]
    fn the_text_of_a_typed_literal_names_a_value_of_that_type_or_no_value_at_all() {
        // (the type name, the text, the value it names: `None` where it names none)
        let cases: Vec<(&str, &str, Option<&str>)> = vec![
            ("date", "2024-03-05", Some("2024-03-05")),
            // A literal written without the zeros names the same day.
            ("date", "2024-3-5", Some("2024-03-05")),
            ("date", " 2024-03-05 ", Some("2024-03-05")),
            ("date", "2024-02-29", Some("2024-02-29")),
            // 2023 is no leap year, so that day is on no calendar.
            ("date", "2023-02-29", None),
            ("date", "2024-13-01", None),
            ("date", "2024-03-05 06:07:08", None),
            ("date", "not-a-date", None),
            ("date", "", None),
            ("time", "06:07:08", Some("06:07:08")),
            ("time", "6:7:8", Some("06:07:08")),
            ("time", "06:07", Some("06:07:00")),
            ("time", "06:07:08.1", Some("06:07:08.1")),
            ("time", "06:07:08.123456", Some("06:07:08.123456")),
            ("time", "24:00:00", None),
            ("time", "06:60", None),
            ("timestamp", "2024-03-05 06:07:08", Some("2024-03-05 06:07:08")),
            // ISO 8601 writes the two parts either side of a `T`.
            ("timestamp", "2024-03-05T06:07:08", Some("2024-03-05 06:07:08")),
            (
                "timestamp",
                "2024-03-05 06:07:08.123",
                Some("2024-03-05 06:07:08.123"),
            ),
            // The digits are the literal's own precision, so trailing zeros are kept.
            (
                "timestamp",
                "2024-03-05 06:07:08.1230",
                Some("2024-03-05 06:07:08.1230"),
            ),
            ("timestamp", "2024-3-5 6:7:8", Some("2024-03-05 06:07:08")),
            ("timestamp", "2024-03-05 06:07", Some("2024-03-05 06:07:00")),
            // The published bound on the fractional digits, and one digit past it.
            (
                "timestamp",
                "2024-03-05 06:07:08.123456789012",
                Some("2024-03-05 06:07:08.123456789012"),
            ),
            ("timestamp", "2024-03-05 06:07:08.1234567890123", None),
            ("timestamp", "2024-03-05 06:07:08.", None),
            ("timestamp", "2024-03-05", None),
            ("timestamp", "2024-03-05 25:07:08", None),
            ("timestamp", "yesterday", None),
        ];

        for (named, text, value) in &cases {
            let target = Temporal::by_name(named)
                .unwrap_or_else(|| panic!("case {named:?}: the type must be one read here"));
            let actual = target.value_of(text);
            assert_eq!(
                actual.as_deref(),
                *value,
                "case ({named:?}, {text:?}): expected {value:?}, got {actual:?}"
            );
            if value.is_none() {
                let message = not_a_literal(text, target);
                assert_eq!(
                    message,
                    format!("INVALID_LITERAL: '{text}' is not a valid {named} literal"),
                    "case ({named:?}, {text:?}): unexpected refusal {message:?}"
                );
            }
        }
    }
}
