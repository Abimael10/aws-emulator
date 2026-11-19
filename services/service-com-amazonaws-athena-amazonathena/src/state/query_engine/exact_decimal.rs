// SPDX-License-Identifier: CC0-1.0

//! The EXACT-NUMERIC (`DECIMAL`) value a statement projects, carried as the digits it has.
//!
//! Athena's `DECIMAL(precision, scale)` is an EXACT type: a value of it carries exactly the
//! digits its precision and scale name, and a number written with a decimal point is a value
//! of that type — a literal with a point and no exponent is `DECIMAL`, not an approximate
//! double, and a cast names the precision and scale the value is read to. The engine under
//! [`super`] (SQLite) has no such type at all: its numeric reading of a value is a 64-bit
//! integer or a binary double, so a value of more significant digits than a double holds came
//! back through that double — the exact digits replaced by a 17-digit approximation in
//! exponent notation.
//!
//! What this module does is read such a value as the DIGITS it has, by
//! [`::bigdecimal`] (an arbitrary-precision decimal), in two halves:
//!
//! * the engine function this registers on every connection
//!   ([`register_exact_decimal`]): one that reads a value as an exact decimal at a declared
//!   precision and scale, which is what a cast to `DECIMAL` is;
//! * the rewriting that puts it in a projected expression's place
//!   ([`exactly_projected`]): a cast to `DECIMAL` and a decimal literal in the select list
//!   become a call of that function over the SAME value, so the digits reach it whole
//!   instead of through the engine's double.
//!
//! The rewrite is on the PARSE — the statement is re-rendered from the tree it changed, so
//! nothing is spliced into the submitted text — and it covers the expressions the statement
//! PROJECTS, which are the ones the result set reports. An expression anywhere else is left
//! to the engine, which compares and aggregates numerically: a predicate over a decimal is
//! the engine's own comparison, as it was.
//!
//! The column the rewriting produces is typed by the call it leaves behind
//! ([`super::projection`]), so a result set reports `decimal` with that precision and scale
//! for it rather than the type of the storage class the value came back in.

use ::bigdecimal::{BigDecimal, RoundingMode};
use ::rusqlite::functions::{Context, FunctionFlags};
use ::rusqlite::types::ValueRef;
use ::sqlparser::ast::{
    DataType, ExactNumberInfo, Expr, SelectItem, SetExpr, Statement as SqlStatement, UnaryOperator,
    Value as SqlValue,
};

use crate::state::catalog_sql::statement::parse_statements;

/// The name the engine function is registered and called under.
///
/// It carries the prefix every name this crate puts in a statement of its own carries (see
/// `super::table_functions`), so it is no name of Athena's own SQL.
pub(super) const EXACT_DECIMAL_FUNCTION: &str = "__athena_decimal";

/// How many arguments that function takes: the value, and the precision and scale it is read
/// to.
const EXACT_DECIMAL_ARGUMENTS: i32 = 3;

/// The largest PRECISION an Athena `DECIMAL` carries — "DECIMAL(precision, scale) … the
/// maximum precision is 38" (Athena's data-type reference, whose SQL is Trino's). A
/// declaration past it is no Athena decimal, so it is left to the engine, and a literal
/// whose digits do not fit it is an approximate double rather than a decimal.
const MAX_DECIMAL_PRECISION: i64 = 38;

/// The SCALE a declaration that names a precision alone carries: none, which the standard
/// and both Athena's and this engine's readings of such a declaration take as zero.
const DEFAULT_DECIMAL_SCALE: i64 = 0;

/// How deep into a projected expression the rewriting follows, so a statement cannot drive
/// it to the depth of its own parenthesis chain; the parse this reads is already held to
/// `MAX_PARSER_RECURSION`, and this bounds the walk over what it produced.
const MAX_READING_DEPTH: usize = 32;

/// How many DIGITS one value this reading may be handed carries, counted as the text that
/// names it and then as the number that text denotes.
///
/// The EMULATOR's own bound, and the one that makes the reading's work bounded rather than
/// caller-sized: the engine admits a value as long as `MAX_VALUE_BYTES`, and a short text can
/// name a number of a billion digits (`1E-1000000000`), so both the text and the magnitude it
/// denotes are refused past this before anything is rescaled. Far above the 38 digits the
/// widest Athena decimal holds — so no value of the type is refused by it — and low enough
/// that reading one is arithmetic over at most a few hundred digits, which matters because a
/// scalar function runs between two of the engine's own instructions and the run's progress
/// handler cannot interrupt it.
const MAX_READ_DIGITS: i64 = 1_024;

/// The type name the refusal of a cast reports the value's own type by, which is the type the
/// engine held the value in: character data, an approximate double and a 64-bit integer are
/// the three a value reaches this reading as.
const SOURCE_VARCHAR: &str = "VARCHAR";
/// See [`SOURCE_VARCHAR`].
const SOURCE_DOUBLE: &str = "DOUBLE";
/// See [`SOURCE_VARCHAR`].
const SOURCE_BIGINT: &str = "BIGINT";

/// Register the exact-decimal reading on `connection`, so a statement the rewriting below
/// has put it in is planned and run by the engine like any other call.
pub(super) fn register_exact_decimal(connection: &::rusqlite::Connection) -> ::rusqlite::Result<()> {
    connection.create_scalar_function(
        EXACT_DECIMAL_FUNCTION,
        EXACT_DECIMAL_ARGUMENTS,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        read_exactly,
    )
}

/// One value READ as an exact decimal at a declared precision and scale: the digits it has,
/// rounded to that scale as a cast to a fixed-point type rounds (half away from zero), and
/// `None` for SQL NULL — which no cast turns into a value.
///
/// A value that does not FIT the declaration once it is at that scale — more digits before
/// the point than `precision - scale` — is no value of the type, so the cast fails rather
/// than reporting digits the column's own metadata contradicts. A value or a text past
/// [`MAX_READ_DIGITS`] is refused as the bound this emulator reads a decimal to.
fn read_exactly(context: &Context<'_>) -> ::rusqlite::Result<Option<String>> {
    // The call SHAPE first: the rewriting calls this with the three arguments it takes, and
    // the name is also one a submitted statement can write.
    if context.len() != 3 {
        return Err(refused(format!(
            "{EXACT_DECIMAL_FUNCTION} takes a value with a precision and a scale"
        )));
    }
    let precision = context.get::<i64>(1).unwrap_or_default();
    let scale = context.get::<i64>(2).unwrap_or_default();
    let within =
        (1..=MAX_DECIMAL_PRECISION).contains(&precision) && (0..=precision).contains(&scale);
    if !within {
        return Err(refused(format!(
            "decimal(precision, scale) carries a precision of 1 to {MAX_DECIMAL_PRECISION} \
             and a scale no larger than it"
        )));
    }
    let (source, text) = match context.get_raw(0) {
        ValueRef::Null => return Ok(None),
        ValueRef::Integer(number) => (SOURCE_BIGINT, number.to_string()),
        ValueRef::Real(number) => (SOURCE_DOUBLE, number.to_string()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            (SOURCE_VARCHAR, String::from_utf8_lossy(bytes).trim().to_string())
        }
    };
    // The TEXT first, so a value longer than any decimal this reads is refused before it is
    // parsed at all: reading digits into a number is more than linear in how many there are.
    if i64::try_from(text.chars().count()).unwrap_or(i64::MAX) > MAX_READ_DIGITS {
        return Err(refused(bounded_value()));
    }
    // Text that names no number at all reads as zero, which is what the engine's own
    // numeric reading of such a value answers.
    let value = text.parse::<BigDecimal>().unwrap_or_default();
    // Then the NUMBER it denotes, before it is rescaled, because the value's own scale
    // decides how many digits rescaling it writes out: `1E-1000000000` is thirteen characters
    // naming a billion of them.
    if value_digits(&value) > MAX_READ_DIGITS {
        return Err(refused(bounded_value()));
    }
    let rounded = value.with_scale_round(scale, RoundingMode::HalfUp);
    // The DECLARED precision is what the value has to fit at that scale, which is the same
    // reading of a precision and a scale the other exact-decimal endpoint of this emulator
    // applies: the digits before the point are the precision less the scale.
    if digits_before_the_point(&rounded) > precision.saturating_sub(scale) {
        return Err(refused(format!(
            "Cannot cast {source} '{text}' to DECIMAL({precision}, {scale})"
        )));
    }
    Ok(Some(rounded.to_plain_string()))
}

/// The refusal for a value past [`MAX_READ_DIGITS`], which is this emulator's own bound and
/// not a limit of the type.
fn bounded_value() -> String {
    format!("this emulator reads a decimal value of at most {MAX_READ_DIGITS} digits")
}

/// How many digits `value` carries in all: the digits of its mantissa plus the zeros its
/// exponent puts before or after them, which is what rendering or rescaling it writes out.
fn value_digits(value: &BigDecimal) -> i64 {
    let significant = i64::try_from(value.digits()).unwrap_or(i64::MAX);
    significant.saturating_add(value.fractional_digit_count().saturating_abs())
}

/// How many digits `value` has BEFORE the point, which is what the precision of a decimal
/// bounds together with its scale.
fn digits_before_the_point(value: &BigDecimal) -> i64 {
    // `digits()` is how many significant digits the value's own mantissa carries, and the
    // fractional count is how many of them are after the point.
    let significant = i64::try_from(value.digits()).unwrap_or(i64::MAX);
    significant
        .saturating_sub(value.fractional_digit_count())
        .max(0)
}

/// The engine's own refusal, carrying the sentence the execution reports.
fn refused(message: String) -> ::rusqlite::Error {
    ::rusqlite::Error::UserFunctionError(message.into())
}

/// `sql` with each exact-numeric value its projection reports read as the digits it has, or
/// `None` when the projection carries none — in which case the submitted text is what the
/// engine runs.
///
/// The expressions covered are the ones the select list PROJECTS: a cast to `DECIMAL`
/// (`NUMERIC`, `DEC`) at a precision Athena's own type carries, and a number written with a
/// point and no exponent, which is a decimal literal of its own digits. Everything else —
/// an exponent literal (an approximate double), a declaration past the largest precision, a
/// literal of more digits than it holds, and every expression outside the select list — is
/// left exactly as it was.
pub(super) fn exactly_projected(sql: &str) -> Option<String> {
    // Nothing to read where the text carries neither a point nor a name of the type: the
    // check is on the submitted text, so a statement carrying none is not parsed again here.
    let upper = sql.to_ascii_uppercase();
    if !sql.contains('.')
        && !upper.contains("DECIMAL")
        && !upper.contains("NUMERIC")
        && !upper.contains("DEC(")
    {
        return None;
    }
    // Parsed through the one reader this crate reads a submitted statement with, so the
    // bounds it holds a parse to (the token count, the recursion limit) hold here too.
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    let mut statement = statements.remove(0);
    let SqlStatement::Query(query) = &mut statement else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_mut() else {
        return None;
    };
    let mut rewritten = false;
    for item in &mut select.projection {
        let projected = match item {
            SelectItem::UnnamedExpr(expression) => expression,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..) => continue,
        };
        rewritten |= exact_reading(projected, 0);
    }
    rewritten.then(|| statement.to_string())
}

/// Rewrite `expression` into the exact-decimal reading of the same value, answering whether
/// it was.
fn exact_reading(expression: &mut Expr, depth: usize) -> bool {
    if depth >= MAX_READING_DEPTH {
        return false;
    }
    // A decimal LITERAL, whose digits are the ones written: they are handed to the reading
    // as text, so the engine never parses them into a double on the way.
    if let Some((digits, precision, scale)) = decimal_literal(expression) {
        if let Some(reading) = call(&quoted(&digits), precision, scale) {
            *expression = reading;
            return true;
        }
        return false;
    }
    match expression {
        // A cast TO the exact type is the reading of its operand at that precision and
        // scale. The OPERAND is read first, so a decimal written inside the cast reaches it
        // as its own digits rather than through the engine's double.
        Expr::Cast {
            expr: operand,
            data_type,
            ..
        } => {
            let Some((precision, scale)) = athena_decimal(data_type) else {
                return false;
            };
            exact_reading(operand, depth.saturating_add(1));
            match call(&operand.to_string(), precision, scale) {
                Some(reading) => {
                    *expression = reading;
                    true
                }
                // The call would not parse: the cast stays as it was and the engine answers
                // for it.
                None => false,
            }
        }
        // A parenthesised expression is the expression inside it.
        Expr::Nested(inner) => exact_reading(inner, depth.saturating_add(1)),
        _ => false,
    }
}

/// `expression` as the DECIMAL LITERAL it is written as — its digits (sign included), its
/// precision and its scale — and `None` for everything else.
///
/// A number written with a decimal point and no exponent is a decimal of its own digits: its
/// scale is how many digits follow the point and its precision how many significant digits it
/// has in all. A literal of more digits than the largest precision holds is NOT one: Athena
/// reads such a number as an approximate double, which is what the engine does with it
/// already.
fn decimal_literal(expression: &Expr) -> Option<(String, i64, i64)> {
    let (negated, number) = signed_number(expression, 0)?;
    let (whole, fraction) = number.split_once('.')?;
    if fraction.is_empty()
        || !whole.chars().all(|digit| digit.is_ascii_digit())
        || !fraction.chars().all(|digit| digit.is_ascii_digit())
    {
        return None;
    }
    let scale = i64::try_from(fraction.len()).ok()?;
    // The leading zeros of the whole part are no significant digits of the value, so
    // `0.045` is a decimal of three digits and not of four.
    let significant = i64::try_from(whole.trim_start_matches('0').len()).ok()?;
    let precision = significant.saturating_add(scale).max(1);
    if precision > MAX_DECIMAL_PRECISION {
        return None;
    }
    let sign = if negated { "-" } else { "" };
    Some((format!("{sign}{number}"), precision, scale))
}

/// `expression` as the NUMBER it is written as, with the sign read off the expression rather
/// than off the characters: a negative literal parses as a negation of a positive one.
fn signed_number(expression: &Expr, depth: usize) -> Option<(bool, String)> {
    if depth >= MAX_READING_DEPTH {
        return None;
    }
    match expression {
        Expr::Value(value) => match &value.value {
            SqlValue::Number(digits, _) => Some((false, digits.clone())),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => signed_number(expr, depth.saturating_add(1))
            .map(|(negated, digits)| (!negated, digits)),
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => signed_number(expr, depth.saturating_add(1)),
        Expr::Nested(inner) => signed_number(inner, depth.saturating_add(1)),
        _ => None,
    }
}

/// The PRECISION and SCALE `data_type` names where it is the exact-numeric type Athena
/// carries, and `None` for every other type — including a declaration past the largest
/// precision Athena's own decimal has, which is no type of its SQL.
fn athena_decimal(data_type: &DataType) -> Option<(i64, i64)> {
    let info = match data_type {
        DataType::Decimal(info) | DataType::Numeric(info) | DataType::Dec(info) => info,
        _ => return None,
    };
    let (precision, scale) = match info {
        ExactNumberInfo::PrecisionAndScale(precision, scale) => {
            (i64::try_from(*precision).ok()?, *scale)
        }
        ExactNumberInfo::Precision(precision) => {
            (i64::try_from(*precision).ok()?, DEFAULT_DECIMAL_SCALE)
        }
        // A declaration that names neither is one of the largest precision the type has, at
        // the scale a declaration without one carries.
        ExactNumberInfo::None => (MAX_DECIMAL_PRECISION, DEFAULT_DECIMAL_SCALE),
    };
    let declared = (1..=MAX_DECIMAL_PRECISION).contains(&precision) && (0..=precision).contains(&scale);
    declared.then_some((precision, scale))
}

/// One SQL string literal holding `digits`, with a quote character inside it doubled so the
/// literal reads back as the text itself.
fn quoted(digits: &str) -> String {
    format!("'{}'", digits.replace('\'', "''"))
}

/// One call of the reading over `argument` at `precision` and `scale`, built by PARSING it —
/// `None` where it does not parse, which leaves the expression as it was.
fn call(argument: &str, precision: i64, scale: i64) -> Option<Expr> {
    let sql = format!("{EXACT_DECIMAL_FUNCTION}({argument}, {precision}, {scale})");
    ::sqlparser::parser::Parser::new(&::sqlparser::dialect::GenericDialect {})
        .try_with_sql(&sql)
        .ok()?
        .parse_expr()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A projected exact-numeric value is rewritten into the reading of its own digits, and
    /// everything the type does not cover is left exactly as it was.
    #[test]
    fn a_projected_decimal_is_read_as_its_own_digits_and_nothing_else_is_rewritten() {
        // (case, the statement submitted, what the engine must be handed: `None` for the
        // text as written). The rows differ along the FORM of the exact-numeric value (a
        // literal, a cast of a string, of a column, of a literal), its scale, its sign, the
        // spelling of the type, how many columns the projection carries, and the forms that
        // are no decimal at all (an exponent literal, a whole number, a precision past the
        // largest one, a value outside the select list).
        let cases: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "a decimal literal of more digits than a double holds",
                "SELECT 123456789012345.123456789 AS n",
                Some("SELECT __athena_decimal('123456789012345.123456789', 24, 9) AS n"),
            ),
            (
                "a short decimal literal",
                "SELECT 2.5 AS d",
                Some("SELECT __athena_decimal('2.5', 2, 1) AS d"),
            ),
            (
                "a negative decimal literal",
                "SELECT -2.50 AS d",
                Some("SELECT __athena_decimal('-2.50', 3, 2) AS d"),
            ),
            (
                "a decimal literal below one",
                "SELECT 0.045 AS d",
                Some("SELECT __athena_decimal('0.045', 3, 3) AS d"),
            ),
            (
                "a cast of a string to the exact type",
                "SELECT CAST('1.5' AS DECIMAL(38,9)) AS d",
                Some("SELECT __athena_decimal('1.5', 38, 9) AS d"),
            ),
            (
                "a cast of a column",
                "SELECT CAST(price AS DECIMAL(10,2)) AS d FROM items",
                Some("SELECT __athena_decimal(price, 10, 2) AS d FROM items"),
            ),
            (
                "a cast written as the other spelling of the type",
                "SELECT CAST(price AS NUMERIC(10,2)) FROM items",
                Some("SELECT __athena_decimal(price, 10, 2) FROM items"),
            ),
            (
                "a cast naming a precision alone, which is a scale of zero",
                "SELECT CAST('1.5' AS DECIMAL(10)) AS d",
                Some("SELECT __athena_decimal('1.5', 10, 0) AS d"),
            ),
            (
                "a cast of a decimal literal, read as its own digits first",
                "SELECT CAST(123456789012345.123456789 AS DECIMAL(38,4)) AS d",
                Some(
                    "SELECT __athena_decimal(__athena_decimal('123456789012345.123456789', 24, \
                     9), 38, 4) AS d",
                ),
            ),
            (
                "a decimal beside a column the rule does not cover",
                "SELECT label, 1.25 AS rate FROM items",
                Some("SELECT label, __athena_decimal('1.25', 3, 2) AS rate FROM items"),
            ),
            (
                "a literal written with an exponent, which is an approximate double",
                "SELECT 1.5e3 AS d",
                None,
            ),
            ("a whole number, which is an integer", "SELECT 25 AS n", None),
            (
                "a literal of more digits than the largest precision holds",
                "SELECT 123456789012345678901234567890123456789.5 AS n",
                None,
            ),
            (
                "a cast past the largest precision the type has",
                "SELECT CAST('1.5' AS DECIMAL(39,9)) AS d",
                None,
            ),
            (
                "a decimal in a predicate, which the engine compares",
                "SELECT label FROM items WHERE price > 1.5",
                None,
            ),
            (
                "a statement that is no query at all",
                "INSERT INTO items VALUES (1.5)",
                None,
            ),
            ("text that parses as nothing", "not a statement", None),
        ];

        for (case, sql, expected) in &cases {
            let actual = exactly_projected(sql);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
