// SPDX-License-Identifier: CC0-1.0

//! The COLUMNS a relation alias names, where the engine's own dialect names them nowhere.
//!
//! Athena's query language is Trino's, in which a relation read in a `FROM` clause may be
//! given both a name and the names of its COLUMNS — `FROM (VALUES (1, 'a'), (2, 'b')) AS
//! t(id, name)` is an inline table of two rows whose columns are `id` and `name`, and
//! `FROM (SELECT …) AS t(a, b)` renames the columns of a derived query positionally. The
//! engine under [`super`] (SQLite) has no such production at all: its `AS <name>` takes the
//! relation's name and nothing else, so a statement carrying one reached it as written and
//! failed to parse — a statement Athena runs.
//!
//! What this module does is name the SAME columns in a production the engine does have: the
//! alias's column list is moved INSIDE the relation, where the projection of a derived query
//! names each column. An inline table's rows become the rows of a derived query that
//! projects them under those names, and a derived query that already projects one expression
//! per name has each expression aliased to its name. One relation either way, with the same
//! rows and the same column names in the same order, so what the engine computes over them
//! is what Athena computes over them.
//!
//! The rewrite is on the PARSE: the statement is re-rendered from the tree the rewrite
//! changed, so nothing is spliced into the submitted text, and a statement whose relations
//! name no columns of their own is handed on byte for byte.

use std::ops::ControlFlow;

use ::sqlparser::ast::{
    Query, Select, SelectItem, SetExpr, Statement as SqlStatement, TableAlias, TableFactor,
    VisitMut, VisitorMut,
};

use crate::state::catalog_sql::statement::parse_statements;

/// How the engine names the columns of an inline table read as a derived relation: the
/// position of each column, counted from one, after this fixed prefix. The one place that
/// name is spelled, because a projection that renames those columns has to address them by
/// exactly the names the engine gave them.
const ENGINE_VALUES_COLUMN_PREFIX: &str = "column";

/// `sql` with every relation alias that names its relation's COLUMNS replaced by a relation
/// that names them where the engine's grammar does, or `None` when it carries none — in
/// which case the submitted text is what the engine runs.
pub(super) fn named_columns(sql: &str) -> Option<String> {
    // Parsed through the one reader this crate reads a submitted statement with, so the
    // bounds it holds a parse to (the token count, the recursion limit) hold here too.
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    let mut statement = statements.remove(0);
    let mut rewrite = Rewrite::default();
    let _ = statement.visit(&mut rewrite);
    rewrite.rewritten.then(|| statement.to_string())
}

/// Moves each relation alias's column list inside the relation it names.
#[derive(Debug, Default)]
struct Rewrite {
    /// Whether anything was replaced, which is what decides whether the statement is
    /// re-rendered at all.
    rewritten: bool,
}

impl VisitorMut for Rewrite {
    type Break = ();

    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        let TableFactor::Derived {
            subquery, alias, ..
        } = factor
        else {
            return ControlFlow::Continue(());
        };
        let Some(TableAlias { columns, .. }) = alias.as_mut() else {
            return ControlFlow::Continue(());
        };
        if columns.is_empty() {
            return ControlFlow::Continue(());
        }
        let names: Vec<String> = columns.iter().map(|column| column.name.value.clone()).collect();
        if !project_as(subquery, &names) {
            return ControlFlow::Continue(());
        }
        // The relation keeps its own name and names its columns inside, which is where the
        // engine's grammar names the columns of a derived relation.
        columns.clear();
        self.rewritten = true;
        ControlFlow::Continue(())
    }
}

/// Make `query` project one column per entry of `names`, under that name, and say whether it
/// now does.
///
/// Two relations carry the columns a Trino alias names: an inline table, whose rows are
/// written out and whose columns the engine itself names by position, and a query that
/// projects one expression per name. A relation of any other shape is left exactly as the
/// statement wrote it — a rename this reader cannot carry out is not one it guesses at.
fn project_as(query: &mut Query, names: &[String]) -> bool {
    match query.body.as_mut() {
        // An inline table: the rows are kept as they are and read through a projection that
        // names each column, since the engine names them by position and Trino by the alias.
        SetExpr::Values(values) => {
            let Some(first) = values.rows.first() else {
                return false;
            };
            if first.len() != names.len() || values.rows.iter().any(|row| row.len() != names.len())
            {
                return false;
            }
            let projection = names
                .iter()
                .enumerate()
                .map(|(position, name)| {
                    format!(
                        "{ENGINE_VALUES_COLUMN_PREFIX}{} AS \"{}\"",
                        position + 1,
                        name.replace('"', "\"\"")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let Some(projecting) = query_of(&format!("SELECT {projection} FROM ({values})")) else {
                return false;
            };
            // Only the BODY is replaced, never the query around it: every clause the
            // statement wrote beside the rows — a `WITH`, an `ORDER BY`, a `LIMIT`, and any
            // other the grammar attaches to a query — stays exactly where it was, so no
            // clause can be dropped by a rewrite that did not know to look for it.
            *query.body = *projecting.body;
            true
        }
        // A query that projects one expression per name: each expression is aliased to the
        // name the relation's alias gives its column, which is the same positional rename.
        SetExpr::Select(select) => aliased(select, names),
        _ => false,
    }
}

/// Alias each projected expression of `select` to the name at its position, and say whether
/// every one of them could be.
///
/// A projection whose width is not the number of names, and one carrying a wildcard — whose
/// width is the relation's own and not the select list's — names no column per name, so
/// nothing is renamed at all.
fn aliased(select: &mut Select, names: &[String]) -> bool {
    use ::sqlparser::ast::Ident;

    if select.projection.len() != names.len() {
        return false;
    }
    if select.projection.iter().any(|item| {
        matches!(
            item,
            SelectItem::Wildcard(..) | SelectItem::QualifiedWildcard(..)
        )
    }) {
        return false;
    }
    for (item, name) in select.projection.iter_mut().zip(names) {
        let expression = match item {
            SelectItem::UnnamedExpr(expression) => expression.clone(),
            SelectItem::ExprWithAlias { expr, .. } => expr.clone(),
            _ => return false,
        };
        *item = SelectItem::ExprWithAlias {
            expr: expression,
            alias: Ident::with_quote('"', name),
        };
    }
    true
}

/// `sql` as the query a derived relation reads, or `None` when it does not parse as one.
fn query_of(sql: &str) -> Option<Query> {
    let mut statements = parse_statements(sql)?;
    if statements.len() != 1 {
        return None;
    }
    match statements.remove(0) {
        SqlStatement::Query(query) => Some(*query),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relation alias that names its columns is rewritten to name them where the engine
    /// does, and every other statement is handed on as written. The rows differ along the
    /// shape of the relation (an inline table, a derived query), how many columns it names,
    /// whether the widths agree, and whether there is an alias list at all.
    #[test]
    fn relation_column_aliases_are_named_inside_the_relation() {
        // (case, the statement submitted, the fragments the rewrite must carry: `None` for
        // the text as written)
        let cases: Vec<(&str, &str, Option<Vec<&str>>)> = vec![
            (
                "no relation alias at all",
                "SELECT 1 AS one",
                None,
            ),
            (
                "a relation named without naming its columns",
                "SELECT * FROM (VALUES (1), (2)) AS t",
                None,
            ),
            (
                "an inline table of one column",
                "SELECT x FROM (VALUES (1), (2), (3)) AS t(x)",
                Some(vec!["column1 AS \"x\"", "VALUES (1), (2), (3)"]),
            ),
            (
                "an inline table of two columns",
                "SELECT * FROM (VALUES (1, 'alpha'), (2, 'beta')) AS t(id, name) ORDER BY id",
                Some(vec![
                    "column1 AS \"id\"",
                    "column2 AS \"name\"",
                    "ORDER BY id",
                ]),
            ),
            (
                "an inline table whose rows are wider than the names it gives",
                "SELECT * FROM (VALUES (1, 'alpha')) AS t(id)",
                None,
            ),
            (
                "a derived query whose columns are renamed",
                "SELECT a, b FROM (SELECT 1, 'x') AS t(a, b)",
                Some(vec!["1 AS \"a\"", "'x' AS \"b\""]),
            ),
            (
                "a derived query that projects everything it reads",
                "SELECT a FROM (SELECT * FROM \"s\") AS t(a)",
                None,
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = named_columns(sql);
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
}
