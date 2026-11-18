// SPDX-License-Identifier: CC0-1.0

//! The TABLE FUNCTION a statement reads its rows from, where the engine's own dialect has
//! no such production.
//!
//! Athena's query language is Trino's, so a statement may read its rows from `UNNEST`: the
//! table function that turns an ARRAY into rows, one row per element — the array written
//! out (`UNNEST(ARRAY[…])`) or one `sequence()` generates (`UNNEST(sequence(1, 9))`), and
//! `CROSS JOIN`ed with another relation as often as the statement likes. The engine under
//! [`super`] reads SQLite, whose grammar has no `UNNEST` at all, so such a statement
//! reached it as written and failed to parse — a statement Athena runs.
//!
//! What this module does is name the SAME rows in a production the engine does have: the
//! elements of an array become the rows of a derived table (`VALUES`), and a generated
//! sequence becomes the rows of a recursive common table expression counting from the
//! sequence's start to its stop by its step. One row per element either way, in the same
//! order, under the column name the statement's own alias gives it — so what the engine
//! computes over those rows is what Athena computes over them.
//!
//! The rewrite is on the PARSE: the statement is re-rendered from the tree the expansion
//! changed, so nothing is spliced into the submitted text, and a statement carrying no
//! table function the engine lacks is handed on byte for byte. Nothing here bounds how many
//! rows a sequence names, and nothing needs to: a statement can already ask the engine to
//! count rows out for itself — a recursive common table expression is a statement of both
//! languages — so these rows are counted under exactly the bounds that hold every other run
//! here, the deadline and step budget [`super::run_over`] arms and the heap and page limits
//! the database is opened with.

use std::ops::ControlFlow;

use ::sqlparser::ast::{
    Expr, Statement as SqlStatement, TableAlias, TableFactor, VisitMut, VisitorMut,
};

use crate::state::catalog_sql::statement::{parse_statements, unnamed_column};

/// The POSITION in the select list of the one column an expanded table function is read
/// through, which is what names it when the statement's own alias gives it no name: Trino
/// names a column no alias names after its position ([`unnamed_column`]), and a statement
/// reading one table function projects it first.
const UNALIASED_COLUMN_POSITION: usize = 0;

/// The function whose result `UNNEST` is read over here beside an array written out:
/// Trino's `sequence(start, stop[, step])`, "a sequence of integers from start to stop,
/// incrementing by step" (by 1, or by -1 when stop is before start, where the statement
/// names no step).
const SEQUENCE_FUNCTION: &str = "sequence";

/// `sql` with every table function the engine's dialect lacks replaced by the rows it
/// stands for, or `None` when it carries none — in which case the submitted text is what
/// the engine runs.
pub(super) fn expanded(sql: &str) -> Option<String> {
    // Parsed through the one reader this crate reads a submitted statement with, so the
    // bounds it holds a parse to (the token count, the recursion limit) hold here too.
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    let mut statement = statements.remove(0);
    let mut expansion = Expansion::default();
    let _ = statement.visit(&mut expansion);
    expansion.expanded.then(|| statement.to_string())
}

/// Replaces each `UNNEST` table factor it walks with the derived table of the rows that
/// factor stands for.
#[derive(Debug, Default)]
struct Expansion {
    /// Whether anything was replaced, which is what decides whether the statement is
    /// re-rendered at all.
    expanded: bool,
    /// How many factors have been replaced, so each sequence's own table expression is
    /// named apart from the one beside it.
    replaced: usize,
}

impl VisitorMut for Expansion {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        let TableFactor::UNNEST {
            alias,
            array_exprs,
            with_offset,
            with_offset_alias,
            with_ordinality,
        } = factor
        else {
            return ControlFlow::Continue(());
        };
        // One array, read as rows on its own: the forms that number the rows
        // (`WITH OFFSET`, `WITH ORDINALITY`) carry a second column, which is not the shape
        // this expansion names, so such a factor is left as the statement wrote it.
        if *with_offset || with_offset_alias.is_some() || *with_ordinality {
            return ControlFlow::Continue(());
        }
        let [array] = &array_exprs[..] else {
            return ControlFlow::Continue(());
        };
        let column = alias
            .as_ref()
            .and_then(|alias| alias.columns.first())
            .map_or_else(
                || unnamed_column(UNALIASED_COLUMN_POSITION),
                |column| column.to_string(),
            );
        let Some(rows) = rows_of(array, &column, self.replaced) else {
            return ControlFlow::Continue(());
        };
        let Some(subquery) = query_of(&rows) else {
            return ControlFlow::Continue(());
        };
        // The derived table keeps the factor's own name and drops its column list: the
        // column is named inside the subquery instead, which is where the engine's grammar
        // names the columns of a derived table.
        let named = alias.as_ref().map(|alias| TableAlias {
            name: alias.name.clone(),
            columns: Vec::new(),
        });
        *factor = TableFactor::Derived {
            lateral: false,
            subquery,
            alias: named,
        };
        self.expanded = true;
        self.replaced = self.replaced.saturating_add(1);
        ControlFlow::Continue(())
    }
}

/// The SQL naming the rows `array` stands for, with its one column called `column`, or
/// `None` for an array this expansion does not name rows for.
///
/// `ordinal` tells one expanded factor from the next, so the table expression a sequence is
/// counted in is named apart from the one a factor beside it is counted in.
fn rows_of(array: &Expr, column: &str, ordinal: usize) -> Option<String> {
    match array {
        // An array written out: its elements ARE the rows, one per element, in the order
        // the statement wrote them.
        Expr::Array(array) => {
            let values: Vec<String> = array
                .elem
                .iter()
                .map(|element| format!("({element})"))
                .collect();
            (!values.is_empty()).then(|| {
                format!(
                    "SELECT column1 AS \"{column}\" FROM (VALUES {})",
                    values.join(", ")
                )
            })
        }
        Expr::Function(function) => sequence_rows(function, column, ordinal),
        _ => None,
    }
}

/// The SQL counting out the rows a `sequence(start, stop[, step])` call generates, or
/// `None` for a call this expansion does not count out: another function, a sequence over
/// anything but whole numbers written out — a sequence of dates or of computed bounds is
/// not counted by adding one to an integer — or one whose step cannot reach its stop.
fn sequence_rows(
    function: &::sqlparser::ast::Function,
    column: &str,
    ordinal: usize,
) -> Option<String> {
    if !function
        .name
        .to_string()
        .eq_ignore_ascii_case(SEQUENCE_FUNCTION)
    {
        return None;
    }
    let arguments = function_arguments(function)?;
    let bounds: Vec<i64> = arguments
        .iter()
        .map(whole_number)
        .collect::<Option<Vec<i64>>>()?;
    let (start, stop, step) = match bounds[..] {
        // "incrementing by 1 if start is less than or equal to stop, otherwise -1".
        [start, stop] => (start, stop, if start <= stop { 1 } else { -1 }),
        [start, stop, step] => (start, stop, step),
        _ => return None,
    };
    // A step that cannot reach the stop generates no sequence at all, which Trino refuses
    // rather than answers; this expansion names rows only where there are rows to name.
    if step == 0 || (step > 0 && start > stop) || (step < 0 && start < stop) {
        return None;
    }
    let counted = format!("\"__athena_sequence_{ordinal}\"");
    // The rows are COUNTED rather than listed: one row for the start, then each next row a
    // step on from the one before it, while the step still reaches the stop.
    let reaches = if step > 0 { "<=" } else { ">=" };
    Some(format!(
        "WITH RECURSIVE {counted}(\"{column}\") AS (SELECT {start} UNION ALL \
         SELECT \"{column}\" + ({step}) FROM {counted} \
         WHERE \"{column}\" + ({step}) {reaches} {stop}) \
         SELECT \"{column}\" FROM {counted}"
    ))
}

/// The arguments of `function` as expressions, or `None` for a call whose arguments are not
/// a plain list of them.
fn function_arguments(function: &::sqlparser::ast::Function) -> Option<Vec<Expr>> {
    use ::sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgumentList, FunctionArguments};

    let FunctionArguments::List(FunctionArgumentList { args, .. }) = &function.args else {
        return None;
    };
    args.iter()
        .map(|argument| match argument {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => Some(expression.clone()),
            _ => None,
        })
        .collect()
}

/// `expression` as the whole number it is written as, or `None` when it is written as
/// anything else.
fn whole_number(expression: &Expr) -> Option<i64> {
    use ::sqlparser::ast::{UnaryOperator, Value, ValueWithSpan};

    match expression {
        Expr::Value(ValueWithSpan {
            value: Value::Number(digits, _),
            ..
        }) => digits.parse::<i64>().ok(),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => whole_number(expr).and_then(i64::checked_neg),
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => whole_number(expr),
        Expr::Nested(inner) => whole_number(inner),
        _ => None,
    }
}

/// `sql` as the query a derived table reads, or `None` when it does not parse as one.
fn query_of(sql: &str) -> Option<Box<::sqlparser::ast::Query>> {
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    match statements.remove(0) {
        SqlStatement::Query(query) => Some(query),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A statement carrying no table function the engine lacks is handed on as written, and
    /// one carrying `UNNEST` names the same rows in a production the engine has.
    #[test]
    fn unnest_is_expanded_and_every_other_statement_is_left_alone() {
        // (case, the statement submitted, what the engine must be handed: `None` for the
        // text as written). The rows differ along what the array is (written out, a
        // sequence counting up, one counting down, one with a step), how many factors the
        // statement reads, and whether there is a table function to expand at all.
        let cases: Vec<(&str, &str, Option<Vec<&str>>)> = vec![
            (
                "no table function at all",
                "SELECT 1 AS one",
                None,
            ),
            (
                "a read of a table",
                "SELECT n FROM \"t\"",
                None,
            ),
            (
                "an array written out",
                "SELECT n FROM UNNEST(ARRAY[1, 2, 3]) AS a(n)",
                Some(vec!["VALUES (1), (2), (3)", "column1 AS \"n\""]),
            ),
            (
                "a sequence counting up",
                "SELECT n FROM UNNEST(SEQUENCE(1, 9)) AS a(n)",
                Some(vec!["WITH RECURSIVE", "SELECT 1 UNION ALL", "+ (1) <= 9"]),
            ),
            (
                "a sequence counting down",
                "SELECT n FROM UNNEST(SEQUENCE(9, 1)) AS a(n)",
                Some(vec!["SELECT 9 UNION ALL", "+ (-1) >= 1"]),
            ),
            (
                "a sequence with a step of its own",
                "SELECT n FROM UNNEST(SEQUENCE(0, 10, 5)) AS a(n)",
                Some(vec!["SELECT 0 UNION ALL", "+ (5) <= 10"]),
            ),
            (
                "two sequences crossed",
                "SELECT COUNT(*) AS c FROM UNNEST(SEQUENCE(1, 3)) AS a(n) CROSS JOIN \
                 UNNEST(SEQUENCE(1, 3)) AS b(m)",
                Some(vec!["__athena_sequence_0", "__athena_sequence_1"]),
            ),
            (
                "a sequence with no alias of its own",
                "SELECT * FROM UNNEST(SEQUENCE(1, 2))",
                Some(vec!["_col0"]),
            ),
            (
                "a sequence whose bounds are not written out",
                "SELECT n FROM UNNEST(SEQUENCE(1, length('abc'))) AS a(n)",
                None,
            ),
            (
                "a step that cannot reach the stop",
                "SELECT n FROM UNNEST(SEQUENCE(1, 9, -1)) AS a(n)",
                None,
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = expanded(sql);
            match expected {
                None => assert!(
                    actual.is_none(),
                    "case {case:?}: expected the statement to be handed on as written, got \
                     {actual:?}"
                ),
                Some(fragments) => {
                    let rewritten = actual.unwrap_or_else(|| {
                        panic!("case {case:?}: expected the statement to be expanded, got none")
                    });
                    assert!(
                        !rewritten.contains("UNNEST"),
                        "case {case:?}: no table function the engine lacks may be left, got \
                         {rewritten:?}"
                    );
                    for fragment in fragments {
                        assert!(
                            rewritten.contains(fragment),
                            "case {case:?}: expected the expansion to carry {fragment:?}, got \
                             {rewritten:?}"
                        );
                    }
                }
            }
        }
    }
}
