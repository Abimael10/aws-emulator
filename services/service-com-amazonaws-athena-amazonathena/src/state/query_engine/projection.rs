// SPDX-License-Identifier: CC0-1.0

//! The type a projected column has by the STATEMENT'S OWN signature, whatever values the
//! run produced for it.
//!
//! A column's type is a property of the EXPRESSION projected, not of the values that
//! expression happened to produce: Athena types `count(...)` as a 64-bit integer whether it
//! counts three rows or none, and an average as a double even where every value averaged was
//! NULL. The engine under [`super`] answers only the storage class of the values it produced
//! — all its integers are 64-bit, and it narrows nothing — so a column it computed is typed
//! here, off the parse of the submitted statement, and only the columns no signature fixes
//! are left to the values (see [`super::ColumnType::widened`]).
//!
//! Nothing here decides an ANSWER from the statement's text: the rows and the values are the
//! engine's, and what this reading contributes is the type the projected expression has.

use ::sqlparser::ast::{Expr, Function, SelectItem, SetExpr, Statement as SqlStatement};

use super::ColumnType;
use crate::state::catalog_sql::statement::parse_statements;

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

/// The type each column of the statement `sql` projects has by its own expression, in
/// projection order — `None` for a column whose type the statement does not fix, which is
/// the one left to the values the run produced.
///
/// The answer is EMPTY where the projection's columns cannot be matched up with the result's
/// one for one: text that is no single query, a set operation, and a projection carrying a
/// wildcard, which stands for as many columns as the relations it reads hold. A caller uses
/// the answer only when it is as long as the result set is wide.
pub(super) fn fixed_column_types(sql: &str) -> Vec<Option<ColumnType>> {
    // Parsed through the one reader this crate reads a submitted statement with, so the
    // bounds it holds a parse to (the token count, the recursion limit) hold here too.
    let parsed = parse_statements(sql)
        .and_then(|mut statements| (statements.len() == 1).then(|| statements.remove(0)));
    let Some(SqlStatement::Query(query)) = parsed else {
        return Vec::new();
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Vec::new();
    };
    let mut types = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        let projected = match item {
            SelectItem::UnnamedExpr(expression) => expression,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..) => return Vec::new(),
        };
        types.push(fixed_type_of(projected));
    }
    types
}

/// The type `expression` has whatever it evaluates to, where Athena's own signature for it
/// gives it one.
fn fixed_type_of(expression: &Expr) -> Option<ColumnType> {
    match expression {
        // A parenthesised expression is the expression inside it.
        Expr::Nested(inner) => fixed_type_of(inner),
        Expr::Function(function) => fixed_result_type(function),
        _ => None,
    }
}

/// The fixed result type of `function`, if it is one of [`FIXED_RESULT_FUNCTIONS`].
///
/// The name is the LAST part of the function's name — a qualified name names the same
/// function — read without its quoting and folded, because a function name is matched
/// without regard to case.
fn fixed_result_type(function: &Function) -> Option<ColumnType> {
    let name = function
        .name
        .0
        .last()?
        .to_string()
        .trim_matches('"')
        .to_ascii_lowercase();
    FIXED_RESULT_FUNCTIONS
        .iter()
        .find(|(fixed, _)| *fixed == name.as_str())
        .map(|(_, column_type)| *column_type)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let actual = fixed_column_types(sql);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the fixed types {expected:?}, got {actual:?}"
            );
        }
    }
}
