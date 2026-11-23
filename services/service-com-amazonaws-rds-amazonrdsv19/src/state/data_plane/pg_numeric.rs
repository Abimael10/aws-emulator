// SPDX-License-Identifier: CC0-1.0

//! The EXACT DECIMAL type both engines call `numeric` (and `decimal`), and the arithmetic
//! over it.
//!
//! `numeric` is not floating point: PostgreSQL documents it as "especially recommended for
//! storing monetary amounts and other quantities where exactness is required", with
//! "calculations … done exactly" and "very slow" compared with the binary floating types.
//! The embedded engine (SQLite) has no such type — its `NUMERIC` affinity holds an integer
//! or a binary float — so a value of this type is held as the DIGITS it has, and the
//! arithmetic over it is done on those digits by [`::bigdecimal`], an arbitrary-precision
//! decimal.
//!
//! Two halves, which is why this is one module:
//!
//! * the engine functions this registers on every connection
//!   ([`register_numeric_functions`]): one that READS a value as an exact decimal (a cast)
//!   and one per arithmetic operator;
//! * the rewriting that puts them in a statement's place ([`exact_decimal_arithmetic`]): a
//!   cast to `numeric` becomes the reading function, and an operator one of whose operands
//!   is a `numeric` expression becomes that operator's function, so `0.1::numeric +
//!   0.2::numeric` is added as decimals rather than as doubles. The statement is REWRITTEN
//!   through its parse, so the operators keep their own precedence and nothing is matched in
//!   its text.
//!
//! The result column is reported under the `numeric` type the value has
//! (`super::pg_server::function_result_type`), so a client decodes it as its own decimal
//! type rather than as a float.
//!
//! What stays binary floating point, stated rather than hidden: a value this reaches is one
//! a statement CASTS to `numeric`, and the arithmetic over the expression that cast roots. A
//! column of a table declared `numeric` is held by the engine in its own affinity, so a sum
//! over such a column is the engine's.

use bigdecimal::{BigDecimal, RoundingMode, Zero};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use sqlparser::ast::{DataType, Expr, ExactNumberInfo, UnaryOperator};
use sqlparser::dialect::GenericDialect;

use super::sql_engine::EngineError;

/// How many digits beyond the point a QUOTIENT is carried to before it is rounded.
///
/// PostgreSQL computes a numeric division to a scale of at least sixteen significant
/// digits (`select_div_scale` in its own `numeric.c`), which for operands of small scale is
/// what `1::numeric / 3` answers with: twenty digits after the point. A quotient that
/// divides exactly is reported with its trailing zeros removed, so this is a ceiling and
/// not a fixed scale.
const DIVISION_SCALE: i64 = 20;

/// How many DIGITS one value may carry, which bounds the work one statement can ask of the
/// decimal arithmetic: every value read, computed or rendered here is held to it, so no
/// number a statement names — however short the text that names it — can grow the process.
/// A product of two values at this bound is twice as long, and a statement that would build
/// a longer value is refused rather than computed.
const MAX_DIGITS: i64 = 1_000;

/// The largest PRECISION a declared `numeric(p, s)` may carry: "the precision must be
/// positive, [and] up to 1000" (PostgreSQL, "Numeric Types"). A declaration past it is
/// refused as the invalid declaration it is, which is also what the real service answers.
const MAX_PRECISION: i64 = 1_000;

/// The range a declared SCALE may lie in: "the scale … can be specified as between
/// -1000 and 1000" (PostgreSQL, "Numeric Types").
const MAX_SCALE: i64 = 1_000;

/// The functions this module's rewriting puts in a statement's place, each under the name
/// the rewriting uses: the reading of a value as an exact decimal and the arithmetic over
/// it.
///
/// `super::pg_server::function_result_type` reports each as `numeric`, which is the type the
/// wire then reports for a column one of them produced.
pub const NUMERIC_FUNCTIONS: [&str; 10] = [
    "pg_numeric",
    "pg_numeric_add",
    "pg_numeric_sub",
    "pg_numeric_mul",
    "pg_numeric_div",
    "pg_numeric_neg",
    // The AGGREGATES, each over the values of an exact-decimal column ([`EXACT_AGGREGATES`]).
    "pg_numeric_sum",
    "pg_numeric_avg",
    "pg_numeric_min",
    "pg_numeric_max",
];

/// The DIVISION the engines' own `/` is, which is not exact-decimal arithmetic: its value is
/// an integer where both operands are integers and a double otherwise, exactly as the
/// embedded engine computes it, and what it adds is the refusal a divisor of zero gets. It is
/// therefore not one of [`NUMERIC_FUNCTIONS`] — the column it produces carries the type of
/// the value it answers with — while the engine still has to admit it
/// ([`is_engine_function`]).
pub const DIVISION_FUNCTION: &str = "pg_divide";

/// Whether `function` is one of [`NUMERIC_FUNCTIONS`].
#[must_use]
pub fn is_numeric_function(function: &str) -> bool {
    NUMERIC_FUNCTIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(function))
}

/// Whether `function` is one this module registers on the engine's connections, which is
/// what the engine's own function allow-list admits (`super::sql_engine`).
#[must_use]
pub fn is_engine_function(function: &str) -> bool {
    is_numeric_function(function)
        || function.eq_ignore_ascii_case(DIVISION_FUNCTION)
        || function.eq_ignore_ascii_case(SCALED_TEXT_FUNCTION)
}

/// Register the exact-decimal functions on `conn`, so a statement the rewriting below has
/// put one in is planned and run by the engine like any other.
pub fn register_numeric_functions(conn: &Connection) -> Result<(), EngineError> {
    // `pg_numeric(value [, precision, scale])` — a value READ as an exact decimal, which is
    // what a cast to `numeric` is. With a precision and a scale it is rounded to that scale
    // ("the value will be rounded to the specified number of fractional digits") and refused
    // where it no longer fits the precision, which is PostgreSQL's `numeric field overflow`.
    scalar(conn, "pg_numeric", -1, |context| {
        // The call shape FIRST: the function takes a value, or a value with the precision
        // and the scale its declaration carries, and nothing else is a call of it.
        if !matches!(context.len(), 1 | 3) {
            return Err(failed(
                "pg_numeric takes a value, or a value with a precision and a scale".to_string(),
            ));
        }
        let Some(value) = decimal_argument(context, 0)? else {
            return Ok(None);
        };
        if context.len() < 3 {
            return Ok(Some(rendered(&value)));
        }
        // The precision and the scale the DECLARATION carries, which are the client's own
        // numbers: a declaration outside the range PostgreSQL publishes for them is refused
        // as the invalid declaration it is, BEFORE the value is rescaled to it — rescaling
        // to a scale of a client's choosing is what would grow the process.
        let precision = context.get::<i64>(1).unwrap_or_default();
        let scale = context.get::<i64>(2).unwrap_or_default();
        if let Err(refusal) = declaration_within_its_range(precision, scale) {
            return Err(failed(refusal));
        }
        let rounded = value.with_scale_round(scale, RoundingMode::HalfUp);
        if digits_before_the_point(&rounded) > precision - scale {
            // PostgreSQL's own `numeric_value_out_of_range` sentence; what a client reads
            // beside it is the precision and the scale it declared.
            return Err(failed(
                super::sql_engine::NUMERIC_FIELD_OVERFLOW.to_string(),
            ));
        }
        Ok(Some(rounded.to_plain_string()))
    })?;
    scalar(conn, "pg_numeric_add", 2, |context| {
        binary(context, |left, right| Ok(left + right))
    })?;
    scalar(conn, "pg_numeric_sub", 2, |context| {
        binary(context, |left, right| Ok(left - right))
    })?;
    scalar(conn, "pg_numeric_mul", 2, |context| {
        binary(context, |left, right| Ok(left * right))
    })?;
    scalar(conn, "pg_numeric_div", 2, |context| {
        binary(context, |left, right| {
            if right.is_zero() {
                // PostgreSQL's own `division_by_zero` sentence.
                return Err(failed(super::sql_engine::DIVISION_BY_ZERO.to_string()));
            }
            Ok((left / right).with_scale_round(DIVISION_SCALE, RoundingMode::HalfUp))
        })
    })?;
    // `pg_divide(dividend, divisor)` — the division the ENGINE itself does, with the one
    // thing PostgreSQL does differently: a divisor of zero is its `division_by_zero` error
    // where the embedded engine answers NULL. Integer operands divide to an integer and
    // anything else to a double, which is what PostgreSQL and the embedded engine both do —
    // so the value it answers with carries the same type the operator's own answer would.
    conn.create_scalar_function(
        DIVISION_FUNCTION,
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let (dividend, divisor) = (context.get_raw(0), context.get_raw(1));
            if matches!(dividend, ValueRef::Null) || matches!(divisor, ValueRef::Null) {
                return Ok(rusqlite::types::Value::Null);
            }
            if let (ValueRef::Integer(dividend), ValueRef::Integer(divisor)) = (dividend, divisor)
            {
                if divisor == 0 {
                    return Err(failed(super::sql_engine::DIVISION_BY_ZERO.to_string()));
                }
                return Ok(rusqlite::types::Value::Integer(
                    dividend.wrapping_div(divisor),
                ));
            }
            let (dividend, divisor) = (as_double(dividend), as_double(divisor));
            if divisor == 0.0 {
                return Err(failed(super::sql_engine::DIVISION_BY_ZERO.to_string()));
            }
            Ok(rusqlite::types::Value::Real(dividend / divisor))
        },
    )
    .map_err(|error| {
        EngineError::new(
            super::sql_engine::EngineErrorKind::Internal,
            error.to_string(),
        )
    })?;
    scalar(conn, "pg_numeric_neg", 1, |context| {
        let Some(value) = decimal_argument(context, 0)? else {
            return Ok(None);
        };
        Ok(Some(rendered(&(-value))))
    })?;
    // `pg_numeric_text(value, scale)` — the TEXT FORM of a value a column declared with that
    // scale holds. A column declared `numeric(p, s)` carries exactly `s` fractional digits —
    // "the value will be rounded to the specified number of fractional digits" — and both
    // engines render such a column with all of them, so the text form of one is the value
    // rescaled to its declaration rather than the shortest digits that denote it.
    scalar(conn, SCALED_TEXT_FUNCTION, 2, |context| {
        let scale = context.get::<i64>(1).unwrap_or_default();
        if !(-MAX_SCALE..=MAX_SCALE).contains(&scale) {
            return Err(failed(format!(
                "NUMERIC scale {scale} must be between {} and {MAX_SCALE}",
                -MAX_SCALE
            )));
        }
        let Some(value) = decimal_argument(context, 0)? else {
            return Ok(None);
        };
        Ok(Some(at_the_declared_scale(&value, scale)))
    })?;
    // The AGGREGATES, each folding the values of an exact-decimal column exactly
    // ([`EXACT_AGGREGATES`]). Each takes the value and the scale its column declares.
    for (_, named, fold) in EXACT_AGGREGATES {
        conn.create_aggregate_function(
            named,
            2,
            FunctionFlags::SQLITE_UTF8,
            ExactAggregate(fold),
        )
        .map_err(|error| {
            EngineError::new(
                super::sql_engine::EngineErrorKind::Internal,
                error.to_string(),
            )
        })?;
    }
    Ok(())
}

/// The function that renders a value at the SCALE its column's declaration carries, under the
/// name [`exact_decimal_arithmetic`] puts it in a statement's place.
///
/// Not one of [`NUMERIC_FUNCTIONS`]: the value it answers with is the column's TEXT FORM, so
/// the column it produces carries the text type the cast it replaced asked for
/// (`super::pg_server::function_result_type`).
pub const SCALED_TEXT_FUNCTION: &str = "pg_numeric_text";

/// The AGGREGATES both engines compute over an exact-decimal column exactly, each paired with
/// the function this module registers for it.
///
/// PostgreSQL's `sum`, `avg`, `min` and `max` over a `numeric` argument ANSWER a `numeric` —
/// "sum(numeric) → numeric", and the same for the others — so an aggregate over such a column
/// is computed on the digits its values carry rather than on the binary floating point the
/// embedded engine would add them up in.
const EXACT_AGGREGATES: [(&str, &str, Fold); 4] = [
    ("sum", "pg_numeric_sum", Fold::Total),
    ("avg", "pg_numeric_avg", Fold::Average),
    ("min", "pg_numeric_min", Fold::Least),
    ("max", "pg_numeric_max", Fold::Greatest),
];

/// How one exact-decimal aggregate folds each row's value into what it already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// `sum`: the total of the values.
    Total,
    /// `avg`: that total divided by how many rows carried a value.
    Average,
    /// `min`: the least of them.
    Least,
    /// `max`: the greatest.
    Greatest,
}

/// What one exact-decimal aggregate has accumulated so far.
#[derive(Debug, Clone, Default)]
struct Accumulated {
    /// What the fold holds, or `None` while no row has carried a value — which is what makes
    /// an aggregate over no rows answer NULL, as both engines do.
    value: Option<BigDecimal>,
    /// How many rows carried one, which is what an average divides by.
    rows: i64,
    /// The scale the column's declaration carries, which the call's second argument names.
    scale: i64,
}

/// One exact-decimal aggregate: the fold it computes over the values it is stepped with.
struct ExactAggregate(Fold);

impl rusqlite::functions::Aggregate<Accumulated, Option<String>> for ExactAggregate {
    fn init(&self, _: &mut Context<'_>) -> rusqlite::Result<Accumulated> {
        Ok(Accumulated::default())
    }

    /// One row's value, folded in at the scale its column declares — a NULL contributes
    /// nothing, exactly as both engines' aggregates skip one.
    fn step(&self, context: &mut Context<'_>, acc: &mut Accumulated) -> rusqlite::Result<()> {
        let scale = context.get::<i64>(1).unwrap_or_default();
        if !(-MAX_SCALE..=MAX_SCALE).contains(&scale) {
            return Err(failed(format!(
                "NUMERIC scale {scale} must be between {} and {MAX_SCALE}",
                -MAX_SCALE
            )));
        }
        acc.scale = scale;
        let Some(value) = decimal_argument(context, 0)? else {
            return Ok(());
        };
        let value = value.with_scale_round(scale, RoundingMode::HalfUp);
        acc.rows += 1;
        let folded = match (acc.value.take(), self.0) {
            (None, _) => value,
            (Some(held), Fold::Total | Fold::Average) => held + value,
            (Some(held), Fold::Least) => held.min(value),
            (Some(held), Fold::Greatest) => held.max(value),
        };
        within_the_bound(&folded)?;
        acc.value = Some(folded);
        Ok(())
    }

    fn finalize(
        &self,
        _: &mut Context<'_>,
        acc: Option<Accumulated>,
    ) -> rusqlite::Result<Option<String>> {
        let Some((value, acc)) = acc.and_then(|acc| acc.value.clone().map(|value| (value, acc)))
        else {
            return Ok(None);
        };
        if self.0 != Fold::Average {
            // The total, the least and the greatest of values of that scale are values of
            // that scale.
            return Ok(Some(at_the_declared_scale(&value, acc.scale)));
        }
        if acc.rows <= 0 {
            return Ok(None);
        }
        // The average is a QUOTIENT, which carries the digits a division carries rather than
        // the scale of its operands — the same reading `pg_numeric_div` takes of one.
        let average = (value / BigDecimal::from(acc.rows))
            .with_scale_round(DIVISION_SCALE, RoundingMode::HalfUp);
        Ok(Some(rendered(&average)))
    }
}

/// `value` with exactly `scale` fractional digits, which is what a column declared with that
/// scale holds and what both engines render such a column as.
fn at_the_declared_scale(value: &BigDecimal, scale: i64) -> String {
    value
        .with_scale_round(scale, RoundingMode::HalfUp)
        .to_plain_string()
}

/// One of this module's functions, registered on `conn`.
fn scalar<F>(conn: &Connection, name: &str, arguments: i32, body: F) -> Result<(), EngineError>
where
    F: Fn(&Context<'_>) -> rusqlite::Result<Option<String>> + Send + 'static,
{
    conn.create_scalar_function(
        name,
        arguments,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |context| body(context),
    )
    .map_err(|error| {
        EngineError::new(
            super::sql_engine::EngineErrorKind::Internal,
            error.to_string(),
        )
    })
}

/// One arithmetic operator over two exact decimals: `NULL` where either operand is SQL
/// NULL, as every arithmetic operator of both engines answers.
///
/// The RESULT is held to [`MAX_DIGITS`] as the operands are, so a product or a quotient
/// cannot grow past the bound either.
fn binary<F>(context: &Context<'_>, apply: F) -> rusqlite::Result<Option<String>>
where
    F: Fn(BigDecimal, BigDecimal) -> rusqlite::Result<BigDecimal>,
{
    let (Some(left), Some(right)) = (decimal_argument(context, 0)?, decimal_argument(context, 1)?)
    else {
        return Ok(None);
    };
    let computed = apply(left, right)?;
    within_the_bound(&computed)?;
    Ok(Some(rendered(&computed)))
}

/// Whether a DECLARED `numeric(precision, scale)` lies in the range PostgreSQL publishes for
/// it, as the sentence it refuses one outside it with.
///
/// Checked BEFORE a value is rescaled to the declaration, because the scale decides how many
/// digits the rescaled value has: a declaration a client chose is not a width this endpoint
/// will build.
fn declaration_within_its_range(precision: i64, scale: i64) -> Result<(), String> {
    if !(1..=MAX_PRECISION).contains(&precision) {
        return Err(format!(
            "NUMERIC precision {precision} must be between 1 and {MAX_PRECISION}"
        ));
    }
    if !(-MAX_SCALE..=MAX_SCALE).contains(&scale) || scale > precision {
        return Err(format!(
            "NUMERIC scale {scale} must be between {} and {precision}",
            -MAX_SCALE
        ));
    }
    Ok(())
}

/// Whether `value` carries no more than [`MAX_DIGITS`] digits, as the refusal that says so.
///
/// The MAGNITUDE is what is bounded, not the length of any text: a value's digits are the
/// digits of its mantissa plus the zeros its exponent puts before or after them, which is
/// what rendering it has to write out.
fn within_the_bound(value: &BigDecimal) -> rusqlite::Result<()> {
    let significant = i64::try_from(value.digits()).unwrap_or(i64::MAX);
    let scale = value.fractional_digit_count().saturating_abs();
    if significant.saturating_add(scale) > MAX_DIGITS {
        return Err(failed(format!(
            "this endpoint serves numeric values of at most {MAX_DIGITS} digits"
        )));
    }
    Ok(())
}

/// The argument at `index` as an exact decimal: `None` for SQL NULL, and the engine's own
/// refusal for a value that is no decimal at all — PostgreSQL's `invalid input syntax` — or
/// one past [`MAX_DIGITS`].
fn decimal_argument(
    context: &Context<'_>,
    index: usize,
) -> rusqlite::Result<Option<BigDecimal>> {
    // The argument has to BE there: a function of this module is reached through the
    // rewriting above, which calls it with the arguments it takes, but the name is also one
    // a client's own statement can write.
    if index >= context.len() {
        return Err(failed(format!(
            "this function takes more arguments than the {} it was called with",
            context.len()
        )));
    }
    let text = match context.get_raw(index) {
        ValueRef::Null => return Ok(None),
        ValueRef::Integer(number) => number.to_string(),
        ValueRef::Real(number) => number.to_string(),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            String::from_utf8_lossy(bytes).trim().to_string()
        }
    };
    // The text first, so an operand longer than any value this serves is refused before it
    // is parsed at all, and then the VALUE it denotes: a short text can name a number with
    // more digits than the process should hold (`1e1000000000`).
    if i64::try_from(text.len()).unwrap_or(i64::MAX) > MAX_DIGITS {
        return Err(failed(format!(
            "this endpoint serves numeric values of at most {MAX_DIGITS} digits"
        )));
    }
    let value = text.parse::<BigDecimal>().map_err(|_| {
        failed(format!(
            "invalid input syntax for type numeric: \"{text}\""
        ))
    })?;
    within_the_bound(&value)?;
    Ok(Some(value))
}

/// A value as the wire carries it: the digits it has, with the trailing zeros a computation
/// left behind removed — the form both engines report an exact decimal in.
fn rendered(value: &BigDecimal) -> String {
    value.normalized().to_plain_string()
}

/// How many digits `value` has BEFORE the point, which is what a declared precision bounds
/// together with its scale.
fn digits_before_the_point(value: &BigDecimal) -> i64 {
    if value.is_zero() {
        return 0;
    }
    // `digits()` is how many significant digits the value's own mantissa carries, and the
    // fractional count is how many of them are after the point.
    let significant = i64::try_from(value.digits()).unwrap_or(i64::MAX);
    (significant - value.fractional_digit_count()).max(0)
}

/// A value as the engine reads it in arithmetic that is not exact-decimal: the number it
/// is, and the number its digits spell for a text value — which is the coercion the embedded
/// engine applies to the operands of an arithmetic operator.
fn as_double(value: ValueRef<'_>) -> f64 {
    match value {
        ValueRef::Integer(number) => number as f64,
        ValueRef::Real(number) => number,
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => String::from_utf8_lossy(bytes)
            .trim()
            .parse()
            .unwrap_or_default(),
        ValueRef::Null => 0.0,
    }
}

/// The engine's own refusal, carrying the sentence a client reads.
fn failed(message: String) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(message.into())
}

/// The statement with its arithmetic as the ENGINES compute it, or `None` for a statement
/// that carries none — which is handed to the embedded engine exactly as it was.
///
/// Two rewritings, both of them over the statement's PARSE, so each operator keeps the
/// operands its own precedence gives it and nothing is matched in the statement's text:
///
/// * a cast to `numeric` or `decimal` becomes the reading function, and an operator one of
/// whose operands is such an expression becomes that operator's exact-decimal function;
/// * on the PostgreSQL wire, every other DIVISION becomes the engine's own division with the
///   refusal PostgreSQL answers a divisor of zero with (`division by zero`), which the
///   embedded engine answers NULL for. The MySQL wire keeps the embedded engine's own answer:
///   `SELECT 1/0` is NULL there, because MySQL's `ERROR_FOR_DIVISION_BY_ZERO` governs the
///   statements that CHANGE data, not a query.
///
/// And one more, for the columns whose DECLARATION carries a scale (`scales`): a cast of such
/// a column to text is the column's own text form, which carries every fractional digit its
/// declaration does ([`SCALED_TEXT_FUNCTION`]), and an aggregate over such a column is folded
/// exactly ([`EXACT_AGGREGATES`]) — the embedded engine holds the value in its own affinity
/// and would render the shortest digits that denote it and add them up as binary floats.
///
/// A statement the parser does not read is left alone, and the engine's own refusal is what
/// the client is told. So is a statement LARGER or DEEPER than this pass serves
/// ([`MAX_REWRITTEN_BYTES`], [`MAX_EXPRESSION_DEPTH`], [`MAX_REWRITES`]): the rewriting is
/// bounded in the statement's size, in the depth it follows and in how many nodes it
/// replaces, so no statement can drive it to the depth of its own operator chain or make it
/// re-render a subtree without limit.
#[must_use]
pub fn exact_decimal_arithmetic(
    wire: super::EngineWire,
    sql: &str,
    scales: &DeclaredScales,
) -> Option<String> {
    let upper = sql.to_ascii_uppercase();
    if sql.len() > MAX_REWRITTEN_BYTES
        || (!upper.contains("NUMERIC")
            && !upper.contains("DECIMAL")
            && !sql.contains('/')
            && scales.is_empty())
    {
        return None;
    }
    let mut statements = sqlparser::parser::Parser::parse_sql(&GenericDialect {}, sql).ok()?;
    let [statement] = statements.as_mut_slice() else {
        return None;
    };
    let mut budget = Budget {
        rewrites: 0,
        rewritten: false,
        guard_divisions: wire == super::EngineWire::Postgres,
    };
    let _ = sqlparser::ast::visit_expressions_mut(statement, |expr| {
        // The visitor reaches every expression of the statement, so the budget is what ends
        // the pass: once it is spent nothing more is rendered or parsed.
        if budget.spent() {
            return std::ops::ControlFlow::Break(());
        }
        exactly(expr, 0, &mut budget, scales);
        std::ops::ControlFlow::<()>::Continue(())
    });
    budget.rewritten.then(|| statement.to_string())
}

/// The SCALE each exact-decimal column of the relations a statement names carries, by
/// lowercased column name: what [`exact_decimal_arithmetic`] renders such a column's text
/// form at.
pub type DeclaredScales = ::std::collections::BTreeMap<String, i64>;

/// Whether `upper` — the statement, uppercased — could read an exact-decimal COLUMN as a
/// value of its own, which is what the declared scales are needed for: its text form, or an
/// aggregate over it. A statement that does neither pays nothing for them.
fn reads_a_declared_column(upper: &str) -> bool {
    ["TEXT", "VARCHAR", "CHAR", "SUM", "AVG", "MIN", "MAX"]
        .iter()
        .any(|named| upper.contains(named))
}

/// The declared scales of the exact-decimal columns the relations `relations` of the database
/// `target` hold, for the statement `sql` — EMPTY for a statement that reads none of them as a
/// value of its own, which is what makes this cost nothing for one.
///
/// The declarations come from the ENGINE itself, which reports the type each column of a
/// relation was declared with, so the scale is the one the client's own `CREATE TABLE` gave
/// the column. A relation the session cannot see contributes nothing.
#[must_use]
pub fn declared_scales(
    database: &super::sql_engine::InstanceDatabase,
    facts: &super::wire_session::SessionFacts,
    target: &str,
    relations: &[String],
    sql: &str,
) -> DeclaredScales {
    let mut scales = DeclaredScales::new();
    if sql.len() > MAX_REWRITTEN_BYTES || !reads_a_declared_column(&sql.to_ascii_uppercase()) {
        return scales;
    }
    // A column NAME two of the statement's relations declare with DIFFERENT scales says
    // nothing about the value the statement reads: whichever relation it belongs to, the
    // scale is one of two. Such a name is left out, so the engine answers for it as it
    // stands rather than at a scale that may be the other column's.
    let mut conflicting: Vec<String> = Vec::new();
    for relation in relations.iter().take(MAX_DESCRIBED_RELATIONS) {
        let Ok(columns) = database.describe(
            facts.connection_id,
            target,
            &format!("SELECT * FROM {}", super::pg_copy::quoted(relation)),
            &[],
        ) else {
            continue;
        };
        for column in columns {
            let named = column.name.to_ascii_lowercase();
            let Some(scale) = column
                .declared_type
                .as_deref()
                .and_then(declared_scale_of)
            else {
                // A column of that name with no declared scale at all is the same
                // disagreement: one of the two carries whatever scale its values have.
                if scales.contains_key(&named) {
                    conflicting.push(named);
                }
                continue;
            };
            if scales.get(&named).is_some_and(|held| *held != scale) {
                conflicting.push(named.clone());
            }
            scales.insert(named, scale);
        }
    }
    for named in conflicting {
        scales.remove(&named);
    }
    scales
}

/// How many relations of one statement are described for their declared scales, so a
/// statement naming relation after relation cannot make this pass ask the engine without
/// bound. Well above the relations a client joins in one statement.
const MAX_DESCRIBED_RELATIONS: usize = 16;

/// The SCALE a column declaration carries, or `None` for a declaration that is not an
/// exact-decimal type with a scale — which includes a bare `numeric`, whose values carry
/// whatever scale each of them has.
///
/// The names are the ones both engines declare the type under: PostgreSQL's `numeric` and
/// `decimal` (and the standard's `dec`), and MySQL's `decimal`, `numeric` and `fixed`.
#[must_use]
pub fn declared_scale_of(declared: &str) -> Option<i64> {
    let lowered = declared.trim().to_ascii_lowercase();
    let (base, arguments) = lowered.split_once('(')?;
    if !matches!(base.trim(), "numeric" | "decimal" | "dec" | "fixed") {
        return None;
    }
    let (_, scale) = arguments.trim_end_matches(')').split_once(',')?;
    let scale = scale.trim().parse::<i64>().ok()?;
    (-MAX_SCALE..=MAX_SCALE).contains(&scale).then_some(scale)
}

/// `value` as a column declared `declared` HOLDS it: the digits its declaration carries, for
/// a declaration with a scale, and `None` for every other column or value — whose value is
/// the one the engine answered with.
///
/// This is the same rule as [`SCALED_TEXT_FUNCTION`] on the read path a result set takes: a
/// column declared `numeric(p, s)` carries exactly `s` fractional digits, so that is what a
/// client reading the column is sent (`super::sql_engine`).
#[must_use]
pub fn as_the_declaration_holds_it(declared: &str, value: ValueRef<'_>) -> Option<String> {
    let scale = declared_scale_of(declared)?;
    let text = match value {
        ValueRef::Null => return None,
        ValueRef::Integer(number) => number.to_string(),
        ValueRef::Real(number) => number.to_string(),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            String::from_utf8_lossy(bytes).trim().to_string()
        }
    };
    if i64::try_from(text.len()).unwrap_or(i64::MAX) > MAX_DIGITS {
        return None;
    }
    let value = text.parse::<BigDecimal>().ok()?;
    // The MAGNITUDE too, not only the length of the text that names it: a short text can name
    // a number with more digits than the process should hold (`1e1000000000`), and rescaling
    // one to a declared scale would have to write every one of them out. The same bound
    // [`decimal_argument`] holds an operand to, so the two readings of a value cannot drift.
    within_the_bound(&value).ok()?;
    Some(at_the_declared_scale(&value, scale))
}

/// How many BYTES of statement this pass rewrites. A longer statement is handed to the
/// engine as the client wrote it: the pass re-renders the subtree it replaces, so its work is
/// this bound times [`MAX_EXPRESSION_DEPTH`] and no statement can make it more. Well above
/// any statement whose arithmetic a client writes by hand, and below the statement ceiling
/// the session already carries (`super::wire_session::MAX_STATEMENT_BYTES`) — which the
/// extended protocol's `Describe` path does not apply, so the bound has to live here.
const MAX_REWRITTEN_BYTES: usize = 16 * 1024;

/// How DEEP into an expression this pass follows. An operator chain is a left-deep tree, so
/// its depth is its length: past this the expression is left as the client wrote it rather
/// than recursed into, which is what keeps this pass off the native stack's limit.
const MAX_EXPRESSION_DEPTH: usize = 32;

/// How many nodes one statement may have REPLACED. A replacement re-renders the subtree it
/// covers, and the budget is taken BEFORE that rendering, so this is what bounds the pass's
/// total work rather than the node count of the statement.
const MAX_REWRITES: usize = 256;

/// What one pass of the rewriting has spent and whether it changed anything; see
/// [`MAX_REWRITES`].
struct Budget {
    rewrites: usize,
    rewritten: bool,
    /// Whether a division that is not over exact decimals carries the refusal a divisor of
    /// zero gets, which is the PostgreSQL wire's answer and not the MySQL wire's.
    guard_divisions: bool,
}

impl Budget {
    /// Whether the budget is spent, which is what ends the pass: checked BEFORE a
    /// replacement is built, so no subtree is rendered for a replacement that would not be
    /// made.
    fn spent(&self) -> bool {
        self.rewrites >= MAX_REWRITES
    }

    /// Take one replacement out of the budget, answering whether there was one to take.
    fn spend(&mut self) -> bool {
        if self.spent() {
            return false;
        }
        self.rewrites += 1;
        self.rewritten = true;
        true
    }
}

/// Rewrite `expr` so that its arithmetic is the engines' own, answering whether its VALUE is
/// an exact decimal.
///
/// The answer is what decides the arithmetic around it: an operator with one exact-decimal
/// operand is an exact-decimal operator, which is the rule both engines follow for a
/// `numeric` operand (PostgreSQL: the numeric types are resolved to the exact one, so
/// `numeric + integer` is numeric).
///
/// `depth` is how far into the expression this already is, and `budget` what the pass has
/// left to spend: past either bound the expression is left exactly as the client wrote it.
/// `scales` are the declared scales of the columns the statement's relations hold.
fn exactly(
    expr: &mut Expr,
    depth: usize,
    budget: &mut Budget,
    scales: &DeclaredScales,
) -> bool {
    if depth >= MAX_EXPRESSION_DEPTH || budget.spent() {
        return false;
    }
    match expr {
        // A cast TO the exact-decimal type is the reading of its operand as one.
        Expr::Cast {
            expr: value,
            data_type,
            ..
        } => {
            let inner = exactly(value, depth + 1, budget, scales);
            let Some(info) = exact_decimal(data_type) else {
                // A cast of an exact-decimal COLUMN to text is that column's own text form,
                // which carries the fractional digits its declaration does.
                if let Some(scale) = text_form_of_a_declared_column(data_type, value, scales) {
                    if !budget.spend() {
                        return inner;
                    }
                    if let Some(read) =
                        call(SCALED_TEXT_FUNCTION, &[value.to_string(), scale.to_string()])
                    {
                        *expr = read;
                    }
                }
                return false;
            };
            // The budget comes before the work: the replacement below renders the operand,
            // so an exhausted budget leaves the cast as the client wrote it without
            // rendering anything.
            if !budget.spend() {
                return inner;
            }
            let read = match info {
                Some((precision, scale)) => {
                    call("pg_numeric", &[value.to_string(), precision, scale])
                }
                None => call("pg_numeric", &[value.to_string()]),
            };
            match read {
                Some(read) => {
                    *expr = read;
                    true
                }
                // The call would not parse: the cast is left as it was and the engine
                // answers for it.
                None => inner,
            }
        }
        Expr::Nested(inner) => exactly(inner, depth + 1, budget, scales),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr: value,
        } => {
            if !exactly(value, depth + 1, budget, scales) {
                return false;
            }
            if !budget.spend() {
                return true;
            }
            if let Some(negated) = call("pg_numeric_neg", &[value.to_string()]) {
                *expr = negated;
            }
            true
        }
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr: value,
        } => exactly(value, depth + 1, budget, scales),
        Expr::BinaryOp { left, op, right } => {
            let exact = exactly(left, depth + 1, budget, scales)
                || exactly(right, depth + 1, budget, scales);
            let function = arithmetic_function(op);
            if exact {
                let Some(function) = function else {
                    return false;
                };
                if !budget.spend() {
                    return true;
                }
                if let Some(applied) = call(function, &[left.to_string(), right.to_string()]) {
                    *expr = applied;
                }
                return true;
            }
            // A DIVISION that is not over exact decimals is still the engine's own, which on
            // the PostgreSQL wire refuses a divisor of zero where the embedded engine answers
            // NULL: the engine's division with that refusal in it. Its value is not an exact
            // decimal.
            if budget.guard_divisions
                && matches!(op, sqlparser::ast::BinaryOperator::Divide)
                && budget.spend()
            {
                if let Some(guarded) =
                    call(DIVISION_FUNCTION, &[left.to_string(), right.to_string()])
                {
                    *expr = guarded;
                }
            }
            false
        }
        // A call of one of this module's own functions is already an exact decimal, which is
        // what makes the rewriting idempotent. The division guard is not one: its value is
        // the engine's own.
        Expr::Function(function) => {
            if is_numeric_function(&function.name.to_string()) {
                return true;
            }
            // An AGGREGATE over an exact-decimal column is exact: both engines answer one
            // with a value of that type, so it is folded on the digits the column's values
            // carry rather than added up in binary floating point.
            let Some((exact, argument, scale)) = exact_aggregate(function, scales) else {
                return false;
            };
            if !budget.spend() {
                return true;
            }
            if let Some(applied) = call(exact, &[argument, scale.to_string()]) {
                *expr = applied;
            }
            true
        }
        _ => false,
    }
}

/// The exact-decimal aggregate `function` is, with the column it folds and that column's
/// declared scale — or `None` for a call that is not one of [`EXACT_AGGREGATES`] over a column
/// whose declaration carries a scale.
///
/// Only a plain call of one argument is read: `DISTINCT`, a `FILTER`, an `OVER` or an ordering
/// clause makes the call something else, which the engine answers for as the client wrote it.
fn exact_aggregate(
    function: &sqlparser::ast::Function,
    scales: &DeclaredScales,
) -> Option<(&'static str, String, i64)> {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
    let named = function.name.to_string().to_ascii_lowercase();
    let (_, exact, _) = EXACT_AGGREGATES
        .iter()
        .find(|(aggregate, _, _)| *aggregate == named)?;
    if function.over.is_some() || function.filter.is_some() {
        return None;
    }
    let FunctionArguments::List(arguments) = &function.args else {
        return None;
    };
    if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
        return None;
    }
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(value))] = arguments.args.as_slice() else {
        return None;
    };
    let scale = column_scale(value, scales)?;
    Some((exact, value.to_string(), scale))
}

/// The scale the column `value` names carries, or `None` where `value` is not a bare column
/// reference of one the statement's relations declared with a scale.
///
/// A qualified reference (`t.amount`) names the same column; the qualifier says which relation
/// it is of, and a column name the statement's relations do not share resolves to the one
/// declaration they have for it.
fn column_scale(value: &Expr, scales: &DeclaredScales) -> Option<i64> {
    let named = match value {
        Expr::Identifier(name) => name.value.clone(),
        Expr::CompoundIdentifier(parts) => parts.last()?.value.clone(),
        _ => return None,
    };
    scales.get(&named.to_ascii_lowercase()).copied()
}

/// The SCALE the column `value` names carries, where this is a cast of such a column to a
/// TEXT type — and `None` for every other cast.
///
/// Only a bare column reference is read: the value of one is held in the engine's own
/// affinity, so its text form is the only place the declared scale has to be put back. An
/// expression COMPUTED from such a column is a value of its own, which the engine renders as
/// what it computed.
fn text_form_of_a_declared_column(
    data_type: &DataType,
    value: &Expr,
    scales: &DeclaredScales,
) -> Option<i64> {
    if !matches!(
        data_type,
        DataType::Text
            | DataType::String(_)
            | DataType::Varchar(_)
            | DataType::Char(_)
            | DataType::CharVarying(_)
            | DataType::CharacterVarying(_)
            | DataType::Character(_)
    ) {
        return None;
    }
    column_scale(value, scales)
}

/// The function that computes `op` over exact decimals, or `None` for an operator that is
/// not arithmetic over them (a comparison, a concatenation, a logical operator): the engine
/// compares and concatenates the digits the value carries.
fn arithmetic_function(op: &sqlparser::ast::BinaryOperator) -> Option<&'static str> {
    use sqlparser::ast::BinaryOperator;
    match op {
        BinaryOperator::Plus => Some("pg_numeric_add"),
        BinaryOperator::Minus => Some("pg_numeric_sub"),
        BinaryOperator::Multiply => Some("pg_numeric_mul"),
        BinaryOperator::Divide => Some("pg_numeric_div"),
        _ => None,
    }
}

/// Whether `data_type` is the EXACT-DECIMAL type, and the precision and scale it declares
/// where it declares both: `Some(None)` for a bare `numeric`, which carries a value of any
/// precision, and `None` for every other type.
fn exact_decimal(data_type: &DataType) -> Option<Option<(String, String)>> {
    let info = match data_type {
        DataType::Numeric(info) | DataType::Decimal(info) | DataType::Dec(info) => info,
        _ => return None,
    };
    Some(match info {
        ExactNumberInfo::PrecisionAndScale(precision, scale) => {
            Some((precision.to_string(), scale.to_string()))
        }
        // A precision with no scale is a scale of zero, which is what the standard and both
        // engines read it as.
        ExactNumberInfo::Precision(precision) => Some((precision.to_string(), "0".to_string())),
        ExactNumberInfo::None => None,
    })
}

/// One call of `function` over `arguments`, built by parsing it: `None` where an operand
/// does not round-trip through the parser, which leaves the statement as it was.
fn call(function: &str, arguments: &[String]) -> Option<Expr> {
    let sql = format!("{function}({})", arguments.join(", "));
    sqlparser::parser::Parser::new(&GenericDialect {})
        .try_with_sql(&sql)
        .ok()?
        .parse_expr()
        .ok()
}

/// The statement is rewritten only where its parse says so; these pin the two halves of
/// that reading — which types are the exact-decimal one, and that the rewriting keeps the
/// operators' own precedence.
#[cfg(test)]
mod tests {
    use super::*;

    /// A statement read with NO declared column scales, which is every case below that is not
    /// about them: the rewriting of a cast to text is what reads them.
    fn no_scales() -> DeclaredScales {
        DeclaredScales::new()
    }

    /// RULE: a cast of a column DECLARED with an exact-decimal scale to a text type is that
    /// column's own text form, which carries every fractional digit the declaration does;
    /// every other cast to text is left exactly as the client wrote it.
    #[test]
    fn the_text_form_of_a_declared_exact_decimal_column_carries_its_scale() {
        let mut scales = DeclaredScales::new();
        scales.insert("amount".to_string(), 4);
        scales.insert("rate".to_string(), 2);
        // (the statement, what it is rewritten to)
        let cases: [(&str, Option<&str>); 6] = [
            (
                "SELECT CAST(amount AS TEXT) FROM t",
                Some("SELECT pg_numeric_text(amount, 4) FROM t"),
            ),
            (
                // A qualified reference names the same column.
                "SELECT CAST(t.rate AS VARCHAR) FROM t",
                Some("SELECT pg_numeric_text(t.rate, 2) FROM t"),
            ),
            (
                // Inside a concatenation and an aggregate, which is where a client
                // checksumming its rows reads it.
                "SELECT md5(string_agg(CAST(amount AS TEXT), ',')) FROM t",
                Some("SELECT md5(string_agg(pg_numeric_text(amount, 4), ',')) FROM t"),
            ),
            // A column the relations declare no scale for, and a cast of one to a type that
            // is not text: left as it was.
            ("SELECT CAST(label AS TEXT) FROM t", None),
            ("SELECT CAST(amount AS INT) FROM t", None),
            // An expression COMPUTED from such a column is a value of its own.
            ("SELECT CAST(amount + 1 AS TEXT) FROM t", None),
        ];
        for (sql, expected) in cases {
            let rewritten =
                exact_decimal_arithmetic(super::super::EngineWire::Postgres, sql, &scales);
            assert_eq!(
                rewritten.as_deref(),
                expected,
                "case {sql:?}: expected {expected:?}, got {rewritten:?}"
            );
        }
    }

    #[test]
    fn a_statements_exact_decimal_arithmetic_is_rewritten_keeping_its_precedence() {
        // The rows span the forms the rule generalises over: a cast with and without a
        // precision, both spellings of the type, an operator per arithmetic operator, mixed
        // precedence, a comparison (which is not arithmetic), a nested cast and a statement
        // that carries no exact decimal at all.
        let cases: [(&str, Option<&str>); 11] = [
            (
                "SELECT CAST(0.1 AS NUMERIC) + CAST(0.2 AS NUMERIC)",
                Some("SELECT pg_numeric_add(pg_numeric(0.1), pg_numeric(0.2))"),
            ),
            (
                "SELECT CAST(CAST(1 AS NUMERIC) / 3 AS NUMERIC(10,4))",
                Some("SELECT pg_numeric(pg_numeric_div(pg_numeric(1), 3), 10, 4)"),
            ),
            (
                "SELECT CAST(2 AS DECIMAL) * 3 - 1",
                Some("SELECT pg_numeric_sub(pg_numeric_mul(pg_numeric(2), 3), 1)"),
            ),
            (
                // Precedence: the multiplication binds tighter than the addition, so the
                // exact operator is the one over the product.
                "SELECT 1 + CAST(2 AS NUMERIC) * 3",
                Some("SELECT pg_numeric_add(1, pg_numeric_mul(pg_numeric(2), 3))"),
            ),
            (
                "SELECT -CAST(2.5 AS NUMERIC)",
                Some("SELECT pg_numeric_neg(pg_numeric(2.5))"),
            ),
            (
                // A comparison is not arithmetic: the digits are compared as they are.
                "SELECT CAST(1 AS NUMERIC) = 1",
                Some("SELECT pg_numeric(1) = 1"),
            ),
            (
                "SELECT CAST(amount AS NUMERIC(12,2)) FROM t WHERE id = 1",
                Some("SELECT pg_numeric(amount, 12, 2) FROM t WHERE id = 1"),
            ),
            // A division that is not over exact decimals is the engine's own, with the
            // refusal a divisor of zero gets.
            ("SELECT 1 / 0", Some("SELECT pg_divide(1, 0)")),
            (
                "SELECT a / b FROM t",
                Some("SELECT pg_divide(a, b) FROM t"),
            ),
            // A statement with no exact-decimal cast and no division is left exactly as it
            // was.
            ("SELECT 0.1 + 0.2", None),
            ("SELECT count(*) FROM t", None),
        ];
        for (sql, expected) in cases {
            let rewritten =
                exact_decimal_arithmetic(super::super::EngineWire::Postgres, sql, &no_scales());
            assert_eq!(
                rewritten.as_deref(),
                expected,
                "case {sql:?}: expected {expected:?}, got {rewritten:?}"
            );
        }
        // The MySQL wire keeps the embedded engine's own division, which answers NULL for a
        // divisor of zero as MySQL does for a query; its exact-decimal casts are rewritten
        // exactly as PostgreSQL's are.
        for (sql, expected) in [
            ("SELECT 1 / 0", None),
            (
                "SELECT CAST(0.1 AS DECIMAL) + CAST(0.2 AS DECIMAL)",
                Some("SELECT pg_numeric_add(pg_numeric(0.1), pg_numeric(0.2))"),
            ),
        ] {
            let rewritten =
                exact_decimal_arithmetic(super::super::EngineWire::MySql, sql, &no_scales());
            assert_eq!(
                rewritten.as_deref(),
                expected,
                "case {sql:?} on the MySQL wire: expected {expected:?}, got {rewritten:?}"
            );
        }
    }

    #[test]
    fn a_value_or_a_declaration_past_what_this_endpoint_serves_is_refused() {
        // The rows span what the bounds generalise over: a value whose MAGNITUDE is past the
        // bound although its text is short, one whose text is long, one that fits, and the
        // declarations outside the range PostgreSQL publishes.
        for (value, within) in [
            ("1", true),
            ("0.000000001", true),
            ("1e999", true),
            ("1e1000000000", false),
            ("1e-1000000000", false),
        ] {
            let parsed = value.parse::<BigDecimal>().expect("the value parses");
            let answered = within_the_bound(&parsed).is_ok();
            assert_eq!(
                answered, within,
                "case {value:?}: expected within={within}, got {answered}"
            );
        }
        for (precision, scale, declared) in [
            (10i64, 4i64, true),
            (1, 0, true),
            (1_000, 1_000, true),
            (0, 0, false),
            (1_001, 2, false),
            (10, 11, false),
            (10, -1_001, false),
            (2_000_000_000, 1_000_000_000, false),
        ] {
            let answered = declaration_within_its_range(precision, scale).is_ok();
            assert_eq!(
                answered, declared,
                "case numeric({precision},{scale}): expected declared={declared}, got \
                 {answered}"
            );
        }
    }

    #[test]
    fn a_value_past_the_precision_its_cast_declares_is_refused() {
        // The rows span what the bound generalises over: a value that fits, one whose
        // rounding makes it fit, one past the precision, a negative one, and zero.
        let cases: [(&str, i64, i64, bool); 5] = [
            ("123.45", 5, 2, true),
            ("123.456", 5, 2, true),
            ("12345.678", 5, 2, false),
            ("-1234.5", 5, 2, false),
            ("0", 5, 2, true),
        ];
        for (value, precision, scale, fits) in cases {
            let rounded = value
                .parse::<BigDecimal>()
                .expect("the value parses")
                .with_scale_round(scale, RoundingMode::HalfUp);
            let within = digits_before_the_point(&rounded) <= precision - scale;
            assert_eq!(
                within, fits,
                "case {value:?} as numeric({precision},{scale}): expected fits={fits}, got \
                 {within} for {rounded}"
            );
        }
    }

    #[test]
    fn an_exact_decimal_is_rendered_with_the_digits_it_has() {
        // The rows span what the rendering generalises over: a sum that is exact in decimal
        // and not in binary floating point, a quotient that does not divide, a value with
        // trailing zeros, and a negative value.
        let cases: [(&str, &str); 4] = [
            ("0.30", "0.3"),
            ("0.33333333333333333333", "0.33333333333333333333"),
            ("12.500", "12.5"),
            ("-0.10", "-0.1"),
        ];
        for (value, expected) in cases {
            let rendered = rendered(&value.parse::<BigDecimal>().expect("the value parses"));
            assert_eq!(
                rendered, expected,
                "case {value:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }
}
