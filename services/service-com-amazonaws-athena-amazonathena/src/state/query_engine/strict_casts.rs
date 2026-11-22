// SPDX-License-Identifier: CC0-1.0

//! What a CAST does with a value that is NOT a value of the type it names.
//!
//! Athena's SQL is Trino's, in which a cast is CHECKED: `CAST('abc' AS integer)` is not a
//! zero, it is a failed query — Trino refuses the cast with its `INVALID_CAST_ARGUMENT`
//! class, naming the value it could not read and the type it was asked to read it as. The
//! engine under [`super`] (SQLite) casts by its own rules instead: text that names no number
//! reads as zero, and a number too large for the type is truncated — so a statement AWS fails
//! answered with a value, silently.
//!
//! What this module does is put the CHECK where the engine has none, in two halves, the same
//! way the exact-decimal reading beside it does ([`super::exact_decimal`]):
//!
//! * the engine function this registers on every connection ([`register_strict_casts`]): one
//!   that hands a value back unchanged when it IS a value of the named type, and refuses in
//!   Athena's own words when it is not;
//! * the rewriting that puts it inside a cast ([`checked`]): the argument of a cast to a
//!   number or a boolean becomes a call of that function over the SAME value, so the cast
//!   itself is still the engine's and only its argument is judged first.
//!
//! The rewrite is on the PARSE — the statement is re-rendered from the tree it changed, so
//! nothing is spliced into the submitted text — and it keeps the cast where it was, so what a
//! result set reports about the column's type is unchanged ([`super::projection`]). The
//! fixed-point type is left out: a cast to `DECIMAL` is already read, and refused, by the
//! exact-decimal reading.

use ::rusqlite::functions::{Context, FunctionFlags};
use ::rusqlite::types::{Value, ValueRef};
use ::sqlparser::ast::{
    CastKind, DataType, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArgumentList,
    FunctionArguments, Ident, ObjectName, ObjectNamePart, Value as SqlValue,
};
use std::ops::ControlFlow;

use crate::state::catalog_sql::statement::parse_statements;

/// The name the engine function is registered and called under.
///
/// It carries the prefix every name this crate puts in a statement of its own carries (see
/// `super::table_functions`), so it is no name of Athena's own SQL.
pub(super) const CAST_ARGUMENT_FUNCTION: &str = "__athena_cast_argument";

/// How many arguments that function takes: the type the cast names, and the value.
const CAST_ARGUMENT_ARGUMENTS: i32 = 2;

/// What Athena's message for a cast it cannot perform opens with: the class of a value that
/// is not a value of the type the cast names.
const INVALID_CAST_ARGUMENT: &str = "INVALID_CAST_ARGUMENT";

/// The types a cast is CHECKED against here, each under the name Athena's own refusal names
/// it by, with the whole numbers it holds where it holds whole numbers.
///
/// Source: Athena's data-type reference — the integer types and their widths (`tinyint` is
/// 8-bit, `smallint` 16-bit, `integer` 32-bit and `bigint` 64-bit), the two approximate
/// types, and the boolean. The fixed-point type is not here: see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A whole number of this many bits.
    Whole {
        /// The type name a refusal names it by.
        named: &'static str,
        /// The smallest value it holds.
        low: i64,
        /// The largest value it holds.
        high: i64,
    },
    /// An approximate number.
    Approximate {
        /// The type name a refusal names it by.
        named: &'static str,
    },
    /// A boolean.
    Boolean,
}

impl Target {
    /// The type `data_type` names, or `None` for a type a cast to which is left to the
    /// engine.
    fn of(data_type: &DataType) -> Option<Self> {
        let whole = |named, low, high| Some(Self::Whole { named, low, high });
        match data_type {
            DataType::TinyInt(..) | DataType::Int8(..) => {
                whole("TINYINT", i64::from(i8::MIN), i64::from(i8::MAX))
            }
            DataType::SmallInt(..) | DataType::Int16 => {
                whole("SMALLINT", i64::from(i16::MIN), i64::from(i16::MAX))
            }
            DataType::Int(..) | DataType::Integer(..) | DataType::Int32 => {
                whole("INT", i64::from(i32::MIN), i64::from(i32::MAX))
            }
            DataType::BigInt(..) | DataType::Int64 => whole("BIGINT", i64::MIN, i64::MAX),
            DataType::Real | DataType::Float4 | DataType::Float(..) => {
                Some(Self::Approximate { named: "REAL" })
            }
            DataType::Double(..) | DataType::DoublePrecision | DataType::Float8 => {
                Some(Self::Approximate { named: "DOUBLE" })
            }
            DataType::Boolean | DataType::Bool => Some(Self::Boolean),
            _ => None,
        }
    }

    /// The name a refusal about this type names it by.
    fn named(self) -> &'static str {
        match self {
            Self::Whole { named, .. } | Self::Approximate { named } => named,
            Self::Boolean => "BOOLEAN",
        }
    }

    /// The same, read back off the name the rewriting wrote into the statement.
    fn by_name(named: &str) -> Option<Self> {
        [
            DataType::TinyInt(None),
            DataType::SmallInt(None),
            DataType::Int(None),
            DataType::BigInt(None),
            DataType::Real,
            DataType::DoublePrecision,
            DataType::Boolean,
        ]
        .iter()
        .filter_map(Self::of)
        .find(|target| target.named() == named)
    }

    /// Whether `text` names a value of this type.
    fn reads(self, text: &str) -> bool {
        let text = text.trim();
        match self {
            Self::Whole { low, high, .. } => text
                .parse::<i64>()
                .is_ok_and(|number| (low..=high).contains(&number)),
            Self::Approximate { .. } => text.parse::<f64>().is_ok_and(f64::is_finite),
            // Trino reads the two words and the two digits as a boolean, and nothing else.
            Self::Boolean => matches!(
                text.to_ascii_lowercase().as_str(),
                "true" | "false" | "1" | "0"
            ),
        }
    }

    /// Whether this type holds the WHOLE number `value`, which is what decides a cast of a
    /// value the engine already holds as one.
    fn holds_whole(self, value: i64) -> bool {
        match self {
            Self::Whole { low, high, .. } => (low..=high).contains(&value),
            // Every whole number is a value of an approximate type, and a boolean reads
            // from the number this engine holds one as.
            Self::Approximate { .. } | Self::Boolean => true,
        }
    }

    /// Whether this type holds the APPROXIMATE number `value`; see [`Self::holds_whole`].
    fn holds_real(self, value: f64) -> bool {
        match self {
            // A whole-number type holds an approximate value only where the number it would
            // be read as is inside the type's own width.
            Self::Whole { low, high, .. } => {
                value.is_finite() && value >= low as f64 && value <= high as f64
            }
            Self::Approximate { .. } => value.is_finite(),
            Self::Boolean => true,
        }
    }
}

/// Register the checked-cast reading on `connection`, so a statement the rewriting below has
/// put it in is planned and run by the engine like any other call.
pub(super) fn register_strict_casts(connection: &::rusqlite::Connection) -> ::rusqlite::Result<()> {
    connection.create_scalar_function(
        CAST_ARGUMENT_FUNCTION,
        CAST_ARGUMENT_ARGUMENTS,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        checked_argument,
    )
}

/// One value a cast is about to read, handed back unchanged when it IS a value of the named
/// type and refused in Athena's own words when it is not.
///
/// NULL is a value of every type — a cast of it is NULL — and a value the engine already
/// holds as a number is one the cast can read. A value held as TEXT is the one that has to be
/// read, which is exactly where this engine's own cast would answer a zero instead of
/// failing. Binary data is a value of none of these types.
fn checked_argument(context: &Context<'_>) -> ::rusqlite::Result<Value> {
    if context.len() != 2 {
        return Err(refused(format!(
            "{CAST_ARGUMENT_FUNCTION} takes the name of a type and a value"
        )));
    }
    let named = context.get::<String>(0).unwrap_or_default();
    let Some(target) = Target::by_name(&named) else {
        return Err(refused(format!(
            "{CAST_ARGUMENT_FUNCTION} was handed {named:?}, which names no type it checks"
        )));
    };
    match context.get_raw(1) {
        ValueRef::Null => Ok(Value::Null),
        // A value the engine already holds as a number is one the cast can read, as long as
        // the type it is being read AS holds it: this engine's own cast would narrow a whole
        // number too large for the type instead of refusing it.
        ValueRef::Integer(number) => {
            if target.holds_whole(number) {
                return Ok(Value::Integer(number));
            }
            Err(refused(cannot_cast(&number.to_string(), target)))
        }
        ValueRef::Real(number) => {
            if target.holds_real(number) {
                return Ok(Value::Real(number));
            }
            Err(refused(cannot_cast(&number.to_string(), target)))
        }
        ValueRef::Text(bytes) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            if !target.reads(&text) {
                return Err(refused(cannot_cast(&text, target)));
            }
            // A BOOLEAN is the one type this engine has no reading of at all: its own cast
            // of a value to it is a numeric one, under which the WORD `true` is zero. So the
            // word is read here, into the number the engine holds a boolean as.
            if let Target::Boolean = target {
                return Ok(Value::Integer(i64::from(truthy(&text))));
            }
            Ok(Value::Text(text))
        }
        ValueRef::Blob(bytes) => Err(refused(cannot_cast(
            &String::from_utf8_lossy(bytes),
            target,
        ))),
    }
}

/// Whether the text of a boolean names TRUE: the word and the digit Trino reads as it; see
/// [`Target::reads`], which is what admits the four texts a boolean reads from at all.
fn truthy(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "1")
}

/// Athena's message for a value that is not a value of the type a cast names: the class it
/// belongs to, then fixed text either side of the value as it reads and the type the cast
/// named.
fn cannot_cast(text: &str, target: Target) -> String {
    format!(
        "{INVALID_CAST_ARGUMENT}: Cannot cast '{text}' to {}",
        target.named()
    )
}

/// The engine's own refusal, carrying the sentence the execution reports.
fn refused(message: String) -> ::rusqlite::Error {
    ::rusqlite::Error::UserFunctionError(message.into())
}

/// `sql` with the argument of every cast to a number or a boolean judged before the cast
/// reads it, or `None` when it carries no such cast — in which case the submitted text is
/// what the engine runs.
pub(super) fn checked(sql: &str) -> Option<String> {
    // Nothing to judge where the text names no cast at all: the check is on the submitted
    // text, so a statement carrying none is not parsed again here.
    if !sql.to_ascii_uppercase().contains("CAST") {
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
        if judged(expression) {
            rewritten = true;
        }
        ControlFlow::<()>::Continue(())
    });
    rewritten.then(|| statement.to_string())
}

/// Put the check inside `expression` where it is a cast to a type checked here, and say
/// whether it was put there.
///
/// A cast whose argument is ALREADY the check — one this walk has just rewritten, which the
/// walk reaches again on its way into the tree — is left alone, so the call is never nested.
fn judged(expression: &mut Expr) -> bool {
    let Expr::Cast {
        kind,
        expr,
        data_type,
        ..
    } = expression
    else {
        return false;
    };
    // `TRY_CAST` and `SAFE_CAST` are the forms that answer NULL rather than failing, which
    // is what they are FOR: a value they cannot read is not a failure of the statement.
    if !matches!(kind, CastKind::Cast | CastKind::DoubleColon) {
        return false;
    }
    let Some(target) = Target::of(data_type) else {
        return false;
    };
    if is_the_check(expr) {
        return false;
    }
    **expr = check_of(target, expr.as_ref().clone());
    true
}

/// Whether `expression` is already a call of the check.
fn is_the_check(expression: &Expr) -> bool {
    let Expr::Function(function) = expression else {
        return false;
    };
    function
        .name
        .0
        .last()
        .is_some_and(|part| part.to_string() == CAST_ARGUMENT_FUNCTION)
}

/// The call that judges `argument` against `target` before a cast reads it.
fn check_of(target: Target, argument: Expr) -> Expr {
    let unnamed = |expression: Expr| FunctionArg::Unnamed(FunctionArgExpr::Expr(expression));
    Expr::Function(Function {
        name: ObjectName(vec![ObjectNamePart::Identifier(Ident::new(
            CAST_ARGUMENT_FUNCTION,
        ))]),
        uses_odbc_syntax: false,
        parameters: FunctionArguments::None,
        args: FunctionArguments::List(FunctionArgumentList {
            duplicate_treatment: None,
            args: vec![
                unnamed(Expr::Value(
                    SqlValue::SingleQuotedString(target.named().to_string()).with_empty_span(),
                )),
                unnamed(argument),
            ],
            clauses: Vec::new(),
        }),
        filter: None,
        null_treatment: None,
        over: None,
        within_group: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cast to a number or a boolean has its argument judged first, and every other
    /// statement is handed on as written. The rows differ along the type the cast names,
    /// where in the statement the cast sits, the form of the cast, and whether there is a
    /// checked cast at all.
    #[test]
    fn a_cast_to_a_number_is_judged_before_the_engine_reads_it() {
        // (case, the statement submitted, the fragments the rewrite must carry: `None` for
        // the text as written)
        let cases: Vec<(&str, &str, Option<Vec<&str>>)> = vec![
            ("no cast at all", "SELECT 1 AS one", None),
            (
                "a cast to a character type, which this engine reads as Athena does",
                "SELECT CAST(id AS varchar) FROM \"t\"",
                None,
            ),
            (
                "a cast to the fixed-point type, which the exact-decimal reading judges",
                "SELECT CAST(amount AS DECIMAL(10, 2)) FROM \"t\"",
                None,
            ),
            (
                "a cast to a 32-bit number",
                "SELECT CAST(label AS integer) FROM \"t\"",
                Some(vec!["__athena_cast_argument('INT', label)"]),
            ),
            (
                "a cast to a 64-bit number",
                "SELECT CAST(label AS bigint) FROM \"t\"",
                Some(vec!["__athena_cast_argument('BIGINT', label)"]),
            ),
            (
                "a cast to an approximate number",
                "SELECT CAST(label AS double) FROM \"t\"",
                Some(vec!["__athena_cast_argument('DOUBLE', label)"]),
            ),
            (
                "a cast to a boolean",
                "SELECT CAST(label AS boolean) FROM \"t\"",
                Some(vec!["__athena_cast_argument('BOOLEAN', label)"]),
            ),
            (
                "a cast in a predicate rather than in the select list",
                "SELECT id FROM \"t\" WHERE CAST(label AS integer) > 2",
                Some(vec!["__athena_cast_argument('INT', label)"]),
            ),
            (
                "a cast of an expression, judged as the one value it produces",
                "SELECT CAST(label || 'x' AS integer) FROM \"t\"",
                Some(vec!["__athena_cast_argument('INT', label || 'x')"]),
            ),
            (
                "a form that answers NULL rather than failing",
                "SELECT TRY_CAST(label AS integer) FROM \"t\"",
                None,
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = checked(sql);
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

    /// RULE: a value is a value of the type a cast names exactly where its text reads as one
    /// at that type's own width, and the refusal for every other value names the value and
    /// the type.
    ///
    /// The rows span the type families, each type's published bounds and one value past them,
    /// and the texts that name no number at all.
    #[test]
    fn a_value_reads_as_the_type_a_cast_names_within_that_type_s_own_width() {
        // (the type name, the text, whether it is a value of that type)
        let cases: Vec<(&str, &str, bool)> = vec![
            ("INT", "42", true),
            ("INT", " 42 ", true),
            ("INT", "-2147483648", true),
            ("INT", "2147483647", true),
            ("INT", "2147483648", false),
            ("INT", "-2147483649", false),
            ("INT", "1.5", false),
            ("INT", "abc", false),
            ("INT", "", false),
            ("TINYINT", "127", true),
            ("TINYINT", "128", false),
            ("SMALLINT", "32767", true),
            ("SMALLINT", "32768", false),
            ("BIGINT", "9223372036854775807", true),
            ("BIGINT", "9223372036854775808", false),
            ("DOUBLE", "1.5", true),
            ("DOUBLE", "-1e3", true),
            ("DOUBLE", "abc", false),
            ("REAL", "0.25", true),
            ("REAL", "x", false),
            ("BOOLEAN", "true", true),
            ("BOOLEAN", "FALSE", true),
            ("BOOLEAN", "1", true),
            ("BOOLEAN", "yes", false),
        ];

        for (named, text, reads) in &cases {
            let target = Target::by_name(named)
                .unwrap_or_else(|| panic!("case {named:?}: the type must be one checked here"));
            let actual = target.reads(text);
            assert_eq!(
                actual, *reads,
                "case ({named:?}, {text:?}): expected reads={reads}, got {actual}"
            );
            if !reads {
                let message = cannot_cast(text, target);
                assert_eq!(
                    message,
                    format!("INVALID_CAST_ARGUMENT: Cannot cast '{text}' to {named}"),
                    "case ({named:?}, {text:?}): unexpected refusal {message:?}"
                );
            }
        }
    }
}
