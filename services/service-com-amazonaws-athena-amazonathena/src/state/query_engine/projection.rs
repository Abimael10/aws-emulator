// SPDX-License-Identifier: CC0-1.0

//! The NAME and the TYPE a projected column has by the STATEMENT'S OWN projection,
//! whatever the engine under [`super`] answered for it.
//!
//! A column's type is a property of the EXPRESSION projected, not of the values that
//! expression happened to produce: Athena types `count(...)` as a 64-bit integer whether it
//! counts three rows or none, and an average as a double even where every value averaged was
//! NULL. The engine under [`super`] answers only the storage class of the values it produced
//! — all its integers are 64-bit, and it narrows nothing — so a column it computed is typed
//! here, off the parse of the submitted statement, and only the columns no signature fixes
//! are left to the values (see [`super::ColumnType::widened`]).
//!
//! A column's NAME is a property of the projection in the same way: Athena names a projected
//! EXPRESSION the query does not alias after that expression's POSITION in the select list
//! ([`crate::state::catalog_sql::statement::unnamed_column`], the one place this tree spells
//! that name), never after the text of the expression, while a projected column REFERENCE
//! keeps the name of the column it reads. The engine under [`super`] answers a computed
//! column's name as the SQL text that produced it, so the projected name is read here and
//! that name is what a result set reports — in its metadata and in the header line of the
//! results object written from it.
//!
//! Nothing here decides an ANSWER from the statement's text: the rows and the values are the
//! engine's, and what this reading contributes is the name and the type the projected
//! expression has.

use ::sqlparser::ast::{
    BinaryOperator, DataType, Expr, Function, Query, SelectItem, SetExpr, Statement as SqlStatement,
    UnaryOperator, Value as SqlValue,
};

use super::exact_decimal::EXACT_DECIMAL_FUNCTION;
use super::{ColumnType, SourceColumnTypes};
use crate::state::catalog_sql::statement::{parse_statements, unnamed_column};

/// The aggregate and window functions whose result type Athena fixes whatever they are
/// applied to, each with that type.
///
/// Source: Athena's SQL reference, which is Trino's — its aggregate functions chapter gives
/// `count` and `count_if` the 64-bit integer type and `avg` the double type over every
/// numeric argument, and its window functions chapter gives the ranking functions
/// (`row_number`, `rank`, `dense_rank`, `ntile`) the 64-bit integer type. A function whose
/// result type DEPENDS on its argument — `sum`, `min`, `max`, the value window functions — is
/// deliberately absent: its type is the argument's, which the values the engine produced
/// already report.
const FIXED_RESULT_FUNCTIONS: &[(&str, ColumnType)] = &[
    ("count", ColumnType::BigInt),
    ("count_if", ColumnType::BigInt),
    ("row_number", ColumnType::BigInt),
    ("rank", ColumnType::BigInt),
    ("dense_rank", ColumnType::BigInt),
    ("ntile", ColumnType::BigInt),
    ("avg", ColumnType::Double),
];

/// The aggregate and window functions whose result type is their ARGUMENT's, so a call of
/// one over a column of stored data has the type that column is declared as.
///
/// Source: the same SQL reference — its aggregate functions chapter words `min(x)` and
/// `max(x)` as "the minimum/maximum value of all input values" and `arbitrary(x)` as "an
/// arbitrary non-null value", and its window functions chapter words the value functions
/// (`first_value`, `last_value`, `nth_value`, `lag`, `lead`) as a value of the window's own
/// rows: each answers one of the values it was given, which is a value of the argument's own
/// type. `sum` is NOT one of them: it accumulates, which is why it has a rule of its own
/// ([`SUMMED_RESULT_FUNCTION`]).
const ARGUMENT_TYPED_FUNCTIONS: &[&str] = &[
    "min",
    "max",
    "arbitrary",
    "first_value",
    "last_value",
    "nth_value",
    "lag",
    "lead",
];

/// The aggregate that ADDS its argument up, whose result type is the argument's own except
/// over the exact-integer types: the same reference gives the sum over an integer argument
/// the 64-bit integer type, because that is the width the sum accumulates in — a total of
/// 32-bit values is not held to 32 bits.
const SUMMED_RESULT_FUNCTION: &str = "sum";

/// The type each column of the statement `sql` projects has by its own expression, in
/// projection order — `None` for a column whose type the statement does not fix, which is
/// the one left to the values the run produced.
///
/// The answer is EMPTY where the projection's columns cannot be matched up with the result's
/// one for one: text that is no single query, a set operation, and a projection carrying a
/// wildcard, which stands for as many columns as the relations it reads hold. A caller uses
/// the answer only when it is as long as the result set is wide.
pub(super) fn fixed_column_types(
    sql: &str,
    source_types: &SourceColumnTypes,
) -> Vec<Option<ColumnType>> {
    let Some(items) = projected_items(sql) else {
        return Vec::new();
    };
    let mut types = Vec::with_capacity(items.len());
    for item in &items {
        let projected = match item {
            SelectItem::UnnamedExpr(expression) => expression,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..) => return Vec::new(),
        };
        types.push(fixed_type_of(projected, source_types));
    }
    types
}

/// The name the statement `sql` PROJECTS each of its columns under, in projection order —
/// `None` for a column the projection names nothing of its own for, which is the one the
/// engine's own name already reports.
///
/// A projected EXPRESSION the query does not alias is named by its POSITION in the select
/// list ([`unnamed_column`]); a projected column reference carries the name of the column it
/// reads, and an aliased projection the alias — both of which the engine answers already, so
/// neither is renamed here.
///
/// EMPTY under exactly the conditions [`fixed_column_types`] is empty, and for the same
/// reason: the projection's columns cannot be matched up with the result's one for one. A
/// caller uses the answer only when it is as long as the result set is wide.
pub(super) fn projected_names(sql: &str) -> Vec<Option<String>> {
    let Some(items) = projected_items(sql) else {
        return Vec::new();
    };
    let mut names = Vec::with_capacity(items.len());
    for (position, item) in items.iter().enumerate() {
        let named = match item {
            // A column reference projects the column it reads, under that column's own name.
            SelectItem::UnnamedExpr(Expr::Identifier(..) | Expr::CompoundIdentifier(..))
            | SelectItem::ExprWithAlias { .. } => None,
            SelectItem::UnnamedExpr(..) => Some(unnamed_column(position)),
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..) => return Vec::new(),
        };
        names.push(named);
    }
    names
}

/// The items ONE query's select list projects, or `None` for text that is no single query of
/// one select list — which is what leaves both readings above empty.
///
/// Parsed through the one reader this crate reads a submitted statement with, so the bounds
/// it holds a parse to (the token count, the recursion limit) hold here too.
fn projected_items(sql: &str) -> Option<Vec<SelectItem>> {
    let parsed = parse_statements(sql)
        .and_then(|mut statements| (statements.len() == 1).then(|| statements.remove(0)))?;
    let SqlStatement::Query(query) = parsed else {
        return None;
    };
    let query: &Query = query.as_ref();
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    Some(select.projection.clone())
}

/// The type `expression` has whatever it evaluates to, where Athena's own signature for it
/// gives it one.
///
/// Five readings, each a property of the EXPRESSION and not of a value:
///
/// * a CAST names the type its value is read to, which is the type the column has;
/// * a BOOLEAN-valued expression — the `TRUE`/`FALSE` literal, a comparison, a logical
///   operator, a null or range test — is boolean, however the engine under [`super`] stores
///   the answer (it has no boolean type and answers 1 and 0);
/// * an exact-decimal reading ([`super::exact_decimal`]) is the `DECIMAL` of the precision
///   and scale that reading names;
/// * a function whose result type Athena fixes ([`FIXED_RESULT_FUNCTIONS`]) has that type;
/// * a function whose result type is its ARGUMENT's ([`ARGUMENT_TYPED_FUNCTIONS`],
///   [`SUMMED_RESULT_FUNCTION`]) over a column of stored data has the type that column is
///   DECLARED as, which is a property of the catalog entry and not of the values the run
///   happened to read.
fn fixed_type_of(expression: &Expr, source_types: &SourceColumnTypes) -> Option<ColumnType> {
    match expression {
        // A parenthesised expression is the expression inside it.
        Expr::Nested(inner) => fixed_type_of(inner, source_types),
        Expr::Function(function) => fixed_result_type(function)
            .or_else(|| argument_typed_result(function, source_types)),
        Expr::Cast { data_type, .. } => cast_type(data_type),
        Expr::Value(value) => matches!(value.value, SqlValue::Boolean(..))
            .then_some(ColumnType::Boolean),
        // Every operator below answers a truth value: the comparisons, the logical
        // connectives and the tests a predicate is written with.
        Expr::BinaryOp { op, .. } if answers_a_truth_value(op) => Some(ColumnType::Boolean),
        Expr::UnaryOp {
            op: UnaryOperator::Not,
            ..
        } => Some(ColumnType::Boolean),
        Expr::IsNull(..)
        | Expr::IsNotNull(..)
        | Expr::IsTrue(..)
        | Expr::IsNotTrue(..)
        | Expr::IsFalse(..)
        | Expr::IsNotFalse(..)
        | Expr::IsDistinctFrom(..)
        | Expr::IsNotDistinctFrom(..)
        | Expr::Between { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::Exists { .. } => Some(ColumnType::Boolean),
        _ => None,
    }
}

/// Whether `op` answers a TRUTH VALUE whatever it is applied to: the comparison operators
/// and the logical connectives, each of which Athena types boolean.
fn answers_a_truth_value(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
            | BinaryOperator::And
            | BinaryOperator::Or
            | BinaryOperator::Xor
    )
}

/// The type a CAST's target names, as the result set reports it — `None` for a type this
/// engine computes no value of, which is left to the value the run produced.
///
/// The integer widths are reported as the width this engine holds them at, which is the type
/// a column DECLARED in one of them reports as well
/// (`crate::state::catalog_sql::data::engine_type`), so one type name is reported for a value
/// whichever of the two named it. The exact-numeric type is not here: a cast to it is read as
/// the digits it has ([`super::exact_decimal`]), and the reading that replaces it is what
/// types the column.
fn cast_type(data_type: &DataType) -> Option<ColumnType> {
    match data_type {
        DataType::Boolean | DataType::Bool => Some(ColumnType::Boolean),
        DataType::TinyInt(..)
        | DataType::SmallInt(..)
        | DataType::Int(..)
        | DataType::Integer(..) => Some(ColumnType::Integer),
        DataType::BigInt(..) => Some(ColumnType::BigInt),
        DataType::Float(..)
        | DataType::Real
        | DataType::Double(..)
        | DataType::DoublePrecision => Some(ColumnType::Double),
        DataType::Char(..)
        | DataType::Varchar(..)
        | DataType::CharVarying(..)
        | DataType::Text
        | DataType::String(..) => Some(ColumnType::VarChar),
        DataType::Binary(..) | DataType::Varbinary(..) | DataType::Blob(..) => {
            Some(ColumnType::VarBinary)
        }
        _ => None,
    }
}

/// The fixed result type of `function`, if it is one of [`FIXED_RESULT_FUNCTIONS`] or the
/// exact-decimal reading, whose type is the precision and scale it was called with.
///
/// The name is the LAST part of the function's name — a qualified name names the same
/// function — read without its quoting and folded, because a function name is matched
/// without regard to case.
fn fixed_result_type(function: &Function) -> Option<ColumnType> {
    let name = folded_function_name(function)?;
    if name == EXACT_DECIMAL_FUNCTION {
        return exact_decimal_type(function);
    }
    FIXED_RESULT_FUNCTIONS
        .iter()
        .find(|(fixed, _)| *fixed == name.as_str())
        .map(|(_, column_type)| *column_type)
}

/// The type a call whose result type is its ARGUMENT's has, read off the declared type of the
/// column that argument names — `None` for any other function, for a call over an expression
/// that is no column reference, and for a column the run read no declaration of.
///
/// The argument is read as a column REFERENCE alone: an expression computed from a column has
/// the type of that computation, which no declaration decides. A `sum` over one of the
/// exact-integer types is the one call whose type is not literally its argument's — it
/// accumulates into the 64-bit integer type ([`SUMMED_RESULT_FUNCTION`]).
fn argument_typed_result(
    function: &Function,
    source_types: &SourceColumnTypes,
) -> Option<ColumnType> {
    let name = folded_function_name(function)?;
    let summed = name == SUMMED_RESULT_FUNCTION;
    if !summed && !ARGUMENT_TYPED_FUNCTIONS.contains(&name.as_str()) {
        return None;
    }
    let declared = declared_argument_type(function, source_types)?;
    Some(match declared {
        ColumnType::Integer | ColumnType::BigInt if summed => ColumnType::BigInt,
        declared => declared,
    })
}

/// The declared type of the column one call's FIRST argument names, where that argument is a
/// reference to a column of the stored data the run read.
fn declared_argument_type(
    function: &Function,
    source_types: &SourceColumnTypes,
) -> Option<ColumnType> {
    use ::sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments};

    let FunctionArguments::List(FunctionArgumentList { args, .. }) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument)), ..] = &args[..] else {
        return None;
    };
    declared_column_type(argument, source_types)
}

/// The declared type of the column `expression` REFERS to, and `None` where it refers to no
/// one column of the stored data the run read.
///
/// A qualified name's LAST part is the column; the ones in front of it name the relation it is
/// read from. The name is folded, because a column name resolves without regard to case.
fn declared_column_type(
    expression: &Expr,
    source_types: &SourceColumnTypes,
) -> Option<ColumnType> {
    let named = match expression {
        Expr::Nested(inner) => return declared_column_type(inner, source_types),
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => parts.last()?.value.clone(),
        _ => return None,
    };
    *source_types.get(&named.to_ascii_lowercase())?
}

/// The name of `function` as a call is matched by: the LAST part of its name — a qualified name
/// names the same function — read without its quoting and folded, because a function name is
/// matched without regard to case.
fn folded_function_name(function: &Function) -> Option<String> {
    Some(
        function
            .name
            .0
            .last()?
            .to_string()
            .trim_matches('"')
            .to_ascii_lowercase(),
    )
}

/// The `DECIMAL` type an exact-decimal reading produces: the precision and the scale its
/// call names, which the rewriting that built the call wrote as whole numbers.
fn exact_decimal_type(function: &Function) -> Option<ColumnType> {
    use ::sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments};

    let FunctionArguments::List(FunctionArgumentList { args, .. }) = &function.args else {
        return None;
    };
    let [_, precision, scale] = &args[..] else {
        return None;
    };
    let whole = |argument: &FunctionArg| -> Option<i32> {
        let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) = argument else {
            return None;
        };
        let SqlValue::Number(digits, _) = &value.value else {
            return None;
        };
        digits.parse::<i32>().ok()
    };
    Some(ColumnType::Decimal {
        precision: whole(precision)?,
        scale: whole(scale)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared types of the columns the cases below read, as the stored objects a run
    /// materialises declare them.
    fn source_types() -> SourceColumnTypes {
        [
            ("amount", Some(ColumnType::BigInt)),
            ("quantity", Some(ColumnType::Integer)),
            ("price", Some(ColumnType::Double)),
            ("label", Some(ColumnType::VarChar)),
            ("active", Some(ColumnType::Boolean)),
            // A name two of the objects declare differently carries no type.
            ("mixed", None),
        ]
        .into_iter()
        .map(|(name, column_type)| (name.to_string(), column_type))
        .collect()
    }

    /// RULE: an aggregate or window function whose result type is its ARGUMENT's reports the
    /// type the stored column that argument names is declared as — except for the sum of an
    /// exact-integer column, which accumulates in the 64-bit integer type.
    ///
    /// The rows span what the rule generalises over: which function is called, the declared
    /// type of the argument (both integer widths, an approximate type, character data, a
    /// boolean), how the argument is written (bare, qualified, quoted, parenthesised), the
    /// case the call is written in, whether the call carries a window, a column no object
    /// declares and one two objects declare differently, and the expressions the rule does
    /// not reach (a computed argument, another function).
    #[test]
    fn an_argument_typed_aggregate_reports_the_declared_type_of_its_argument() {
        // (case, the statement, the types its projection fixes)
        let cases: Vec<(&str, &str, Vec<Option<ColumnType>>)> = vec![
            (
                "the sum of a 64-bit integer column",
                "SELECT sum(amount) AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "the sum of a 32-bit integer column, which accumulates wider",
                "SELECT sum(quantity) AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "the sum of an approximate column, which stays approximate",
                "SELECT sum(price) AS total FROM items",
                vec![Some(ColumnType::Double)],
            ),
            (
                "the least and greatest of an integer column",
                "SELECT min(amount) AS lo, max(amount) AS hi FROM items",
                vec![Some(ColumnType::BigInt), Some(ColumnType::BigInt)],
            ),
            (
                "the least of a 32-bit integer column, which stays as declared",
                "SELECT min(quantity) AS lo FROM items",
                vec![Some(ColumnType::Integer)],
            ),
            (
                "the greatest of a character column",
                "SELECT max(label) AS last FROM items",
                vec![Some(ColumnType::VarChar)],
            ),
            (
                "the greatest of a boolean column",
                "SELECT max(active) AS any_active FROM items",
                vec![Some(ColumnType::Boolean)],
            ),
            (
                "a windowed sum",
                "SELECT sum(amount) OVER (PARTITION BY label) AS running FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a value window function",
                "SELECT first_value(price) OVER (ORDER BY amount) AS first FROM items",
                vec![Some(ColumnType::Double)],
            ),
            (
                "a qualified argument",
                "SELECT sum(items.amount) AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a quoted argument",
                "SELECT sum(\"amount\") AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a parenthesised argument",
                "SELECT sum((amount)) AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "the call written in upper case",
                "SELECT SUM(amount) AS total FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a column no stored object declares",
                "SELECT sum(unknown_column) AS total FROM items",
                vec![None],
            ),
            (
                "a column two stored objects declare differently",
                "SELECT max(mixed) AS latest FROM items",
                vec![None],
            ),
            (
                "an argument computed from a column, whose type no declaration decides",
                "SELECT sum(amount * 2) AS total FROM items",
                vec![None],
            ),
            (
                "a counting aggregate, whose type its own signature fixes",
                "SELECT count(amount) AS n FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "an average over an integer column, which is approximate",
                "SELECT avg(amount) AS mean FROM items",
                vec![Some(ColumnType::Double)],
            ),
        ];

        let source_types = source_types();
        for (case, sql, expected) in &cases {
            let actual = fixed_column_types(sql, &source_types);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the fixed types {expected:?}, got {actual:?}"
            );
        }
    }

    /// A projected expression whose signature fixes its type is typed by that signature, one
    /// whose type depends on its argument is left to the values, and a projection that
    /// cannot be matched up with the result's columns one for one is not typed at all.
    #[test]
    fn the_types_read_off_a_projection_are_the_ones_its_signatures_fix() {
        // (case, the statement, the types its projection fixes). The rows differ along the
        // kind of projected expression, the number of columns, whether the function is
        // written in another case or qualified, and whether the projection can be matched up
        // with the result's columns at all.
        let cases: Vec<(&str, &str, Vec<Option<ColumnType>>)> = vec![
            (
                "a counting aggregate over every row",
                "SELECT count(*) AS n FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "the same aggregate written without an alias",
                "SELECT count(*) FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a counting aggregate in upper case over distinct values",
                "SELECT COUNT(DISTINCT name) AS names FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "an average, whose type no value decides",
                "SELECT avg(price) AS mean FROM items",
                vec![Some(ColumnType::Double)],
            ),
            (
                "a ranking window function",
                "SELECT row_number() OVER (ORDER BY id) AS position FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "an aggregate whose type is its argument's",
                "SELECT sum(price) AS total, max(price) AS dearest FROM items",
                vec![None, None],
            ),
            (
                "a mix of fixed and value-decided columns",
                "SELECT id, count(*) AS n, price * 2 AS doubled FROM items",
                vec![None, Some(ColumnType::BigInt), None],
            ),
            (
                "a parenthesised aggregate",
                "SELECT (count(*)) AS n FROM items",
                vec![Some(ColumnType::BigInt)],
            ),
            (
                "a projection carrying a wildcard",
                "SELECT *, count(*) AS n FROM items",
                Vec::new(),
            ),
            (
                "a statement that is no query at all",
                "INSERT INTO items VALUES (1)",
                Vec::new(),
            ),
            ("text that parses as nothing", "not a statement", Vec::new()),
        ];

        for (case, sql, expected) in &cases {
            let actual = fixed_column_types(sql, &SourceColumnTypes::new());
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the fixed types {expected:?}, got {actual:?}"
            );
        }
    }

    /// A projected expression the query does not alias is named by its POSITION in the
    /// select list; a projected column reference and an aliased projection are named by the
    /// projection itself, so neither is renamed.
    #[test]
    fn an_unaliased_projected_expression_is_named_by_its_position() {
        // (case, the statement, the name the projection gives each column). The rows differ
        // along the kind of projected expression (aggregate, arithmetic, literal, function,
        // column reference), the position the unaliased one sits at, how wide the projection
        // is, and whether it can be matched up with the result's columns at all.
        let cases: Vec<(&str, &str, Vec<Option<String>>)> = vec![
            (
                "an unaliased aggregate, the only column",
                "SELECT count(*) FROM items",
                vec![Some("_col0".to_string())],
            ),
            (
                "the same aggregate under an alias",
                "SELECT count(*) AS n FROM items",
                vec![None],
            ),
            (
                "an unaliased expression after a named column",
                "SELECT id, price * 2 FROM items",
                vec![None, Some("_col1".to_string())],
            ),
            (
                "two unaliased literals",
                "SELECT 1, 'two'",
                vec![Some("_col0".to_string()), Some("_col1".to_string())],
            ),
            (
                "an unaliased expression between two aliased ones",
                "SELECT min(id) AS lowest, max(id), count(*) AS n FROM items",
                vec![None, Some("_col1".to_string()), None],
            ),
            (
                "a qualified column reference",
                "SELECT items.id FROM items",
                vec![None],
            ),
            (
                "a projection carrying a wildcard",
                "SELECT *, count(*) FROM items",
                Vec::new(),
            ),
            (
                "a statement that is no query at all",
                "INSERT INTO items VALUES (1)",
                Vec::new(),
            ),
            ("text that parses as nothing", "not a statement", Vec::new()),
        ];

        for (case, sql, expected) in &cases {
            let actual = projected_names(sql);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the projected names {expected:?}, got {actual:?}"
            );
        }
    }
}
