// SPDX-License-Identifier: CC0-1.0

//! What TYPE one expression of a submitted statement has, resolved against the Data Catalog
//! while the statement is PLANNED.
//!
//! A query's `typeof(<expression>)` answers the type of the expression, not of the value it
//! evaluated to: it is resolved once, when the statement is analysed, so it is the same
//! answer for every row — including for a column whose every field reads as NULL, which has
//! the type the catalog declares for it all the same.
//!
//! The engine under [`crate::state::query_engine`] cannot answer that question: it is handed
//! the rows a catalog table was materialised as and knows only the storage class each value
//! is held in. So the question is answered HERE, where the catalog has just said what each
//! column of each relation the statement reads is declared as
//! ([`super::hive_types::reported_type`]), by resolving each such call in the PARSE of the
//! statement before the engine is handed anything.
//!
//! What is resolved is only what the catalog decides: a reference to a column of a relation
//! the statement reads, and the type a `CAST` names. An expression whose type neither of
//! those fixes — a literal, an arithmetic or function result — is left exactly as the
//! statement wrote it, because nothing here may invent a type the catalog did not state.

use ::std::collections::HashMap;
use ::std::ops::ControlFlow;

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Statement as SqlStatement,
    Value as SqlValue,
};

use super::hive_types;
use super::statement::DeclaredColumn;

/// The function whose answer is the TYPE of its one argument, as Athena's SQL names it.
const TYPE_OF: &str = "typeof";

/// What type each column the statement can read is declared as.
///
/// Keyed by the column name folded the way the catalog folds one, because a statement
/// resolves a column name without regard to case. The value is the type name a query
/// reports for that column, and `None` where no single name is reported for it — which is
/// also what a name two relations of the statement declare DIFFERENTLY resolves to, since
/// such a reference is not one column's.
#[derive(Debug, Default)]
pub struct ColumnTypes {
    /// Folded column name -> the reported type name, where exactly one is known.
    held: HashMap<String, Option<String>>,
}

impl ColumnTypes {
    /// The types the columns `columns` declare, as the catalog recorded them.
    pub fn of<'c>(columns: impl IntoIterator<Item = &'c DeclaredColumn>) -> Self {
        let mut types = Self::default();
        for column in columns {
            types.declare(&column.name, &column.data_type);
        }
        types
    }

    /// Record that the column `name` is declared as the catalog type `data_type`.
    ///
    /// A name already recorded under ANOTHER type carries no type after this: a reference to
    /// it names no one column, so the statement's own text decides nothing about its type.
    fn declare(&mut self, name: &str, data_type: &str) {
        let reported = hive_types::reported_type(data_type);
        self.held
            .entry(glue_state::fold_name(name))
            .and_modify(|held| {
                if *held != reported {
                    *held = None;
                }
            })
            .or_insert(reported);
    }

    /// The type name reported for the column `name`, where exactly one is known.
    fn reported(&self, name: &str) -> Option<&str> {
        self.held.get(&glue_state::fold_name(name))?.as_deref()
    }
}

/// Resolve every `typeof(<expression>)` in `statement` whose answer the catalog fixes to the
/// type name itself.
///
/// The call is replaced in the PARSE, by a character literal of that one type name, so the
/// statement the engine is handed carries the answer the analysis reached and nothing is
/// spliced into the submitted text. A call this resolution cannot answer is left as it
/// stands.
pub fn resolve(statement: &mut SqlStatement, types: &ColumnTypes) {
    let _ = ::sqlparser::ast::visit_expressions_mut(statement, |expression: &mut Expr| {
        if let Some(named) = resolved_call(expression, types) {
            *expression = Expr::Value(SqlValue::SingleQuotedString(named).with_empty_span());
        }
        ControlFlow::<()>::Continue(())
    });
}

/// The type name `expression` answers, when it is a `typeof` call this resolution can
/// answer.
fn resolved_call(expression: &Expr, types: &ColumnTypes) -> Option<String> {
    let Expr::Function(function) = expression else {
        return None;
    };
    // The LAST part of the name names the function — a qualified name names the same one —
    // read without its quoting and folded, because a function name is matched without
    // regard to case.
    let name = function
        .name
        .0
        .last()?
        .to_string()
        .trim_matches('"')
        .to_ascii_lowercase();
    if name != TYPE_OF {
        return None;
    }
    let FunctionArguments::List(arguments) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(argument))] = arguments.args.as_slice() else {
        return None;
    };
    type_of(argument, types)
}

/// The type `expression` has by what the catalog declared or the statement wrote, or `None`
/// where neither fixes one.
fn type_of(expression: &Expr, types: &ColumnTypes) -> Option<String> {
    match expression {
        // A parenthesised expression is the expression inside it.
        Expr::Nested(inner) => type_of(inner, types),
        Expr::Identifier(ident) => types.reported(&ident.value).map(str::to_string),
        // A qualified name's LAST part is the column; the ones in front of it name the
        // relation it is read from.
        Expr::CompoundIdentifier(parts) => {
            types.reported(&parts.last()?.value).map(str::to_string)
        }
        // A conversion has the type it converts TO, which the statement wrote down.
        Expr::Cast { data_type, .. } => hive_types::reported_type(&data_type.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::statement::parse_statements;

    /// The columns the cases below read, as a catalog table declares them.
    fn declared() -> Vec<DeclaredColumn> {
        [
            ("day", "date"),
            ("sc_bytes", "bigint"),
            ("time_taken", "double"),
            ("sc_status", "int"),
            ("label", "string"),
            ("tags", "array<string>"),
        ]
        .iter()
        .map(|(name, data_type)| DeclaredColumn {
            name: (*name).to_string(),
            data_type: (*data_type).to_string(),
            comment: None,
        })
        .collect()
    }

    /// The statement `sql` with every answerable `typeof` resolved, rendered back as SQL.
    fn resolved(sql: &str) -> String {
        let columns = declared();
        let types = ColumnTypes::of(columns.iter());
        let mut statement = parse_statements(sql)
            .and_then(|mut statements| (statements.len() == 1).then(|| statements.remove(0)))
            .expect("the case parses as one statement");
        resolve(&mut statement, &types);
        statement.to_string()
    }

    /// RULE: the type of a reference to a catalog column is the type the catalog declares
    /// for that column, resolved when the statement is planned — so the call is answered
    /// before a single value is read.
    ///
    /// The rows span what the rule generalises over: the declared type (a date, the two
    /// integer widths, an approximate type, the unbounded character type), how the column is
    /// written (bare, quoted, qualified, parenthesised), where the call sits in the statement
    /// (the projection, a predicate), how many calls one statement carries, and the
    /// expressions whose type the catalog does NOT fix.
    #[test]
    fn a_typeof_over_a_catalog_column_answers_the_declared_type() {
        // (case, the statement, the statement after the resolution)
        let cases: Vec<(&str, &str, &str)> = vec![
            (
                "a column of dates",
                "SELECT typeof(day) FROM logs",
                "SELECT 'date' FROM logs",
            ),
            (
                "a quoted column name",
                "SELECT typeof(\"sc_bytes\") FROM logs",
                "SELECT 'bigint' FROM logs",
            ),
            (
                "a column written in another case",
                "SELECT typeof(SC_Status) FROM logs",
                "SELECT 'integer' FROM logs",
            ),
            (
                "a qualified column reference",
                "SELECT typeof(logs.time_taken) FROM logs",
                "SELECT 'double' FROM logs",
            ),
            (
                "the function written in upper case",
                "SELECT TYPEOF(label) FROM logs",
                "SELECT 'varchar' FROM logs",
            ),
            (
                "a parenthesised column reference",
                "SELECT typeof((sc_bytes)) FROM logs",
                "SELECT 'bigint' FROM logs",
            ),
            (
                "every column of the table at once",
                "SELECT typeof(day), typeof(sc_bytes), typeof(time_taken), typeof(sc_status) \
                 FROM logs",
                "SELECT 'date', 'bigint', 'double', 'integer' FROM logs",
            ),
            (
                "a call in a predicate rather than the projection",
                "SELECT label FROM logs WHERE typeof(sc_bytes) = 'bigint'",
                "SELECT label FROM logs WHERE 'bigint' = 'bigint'",
            ),
            (
                "the type a conversion names",
                "SELECT typeof(CAST(label AS bigint)) FROM logs",
                "SELECT 'bigint' FROM logs",
            ),
            (
                // A collection type's own name is written out of its element types, which
                // this resolution does not render, so the call is left to the engine.
                "a column of a collection type",
                "SELECT typeof(tags) FROM logs",
                "SELECT typeof(tags) FROM logs",
            ),
            (
                "a column no relation of the statement declares",
                "SELECT typeof(unknown_column) FROM logs",
                "SELECT typeof(unknown_column) FROM logs",
            ),
            (
                "an expression whose type nothing declared",
                "SELECT typeof(sc_bytes + 1) FROM logs",
                "SELECT typeof(sc_bytes + 1) FROM logs",
            ),
            (
                "another function over the same column",
                "SELECT max(sc_bytes) FROM logs",
                "SELECT max(sc_bytes) FROM logs",
            ),
            (
                "a call carrying no argument at all",
                "SELECT typeof() FROM logs",
                "SELECT typeof() FROM logs",
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = resolved(sql);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the planned statement {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a column name two relations of one statement declare under DIFFERENT types
    /// names no one column, so nothing is resolved for it — while one they declare alike
    /// still carries that type.
    #[test]
    fn a_column_two_relations_declare_differently_resolves_to_no_type() {
        let mut types = ColumnTypes::default();
        types.declare("id", "int");
        types.declare("id", "string");
        types.declare("agreed", "bigint");
        types.declare("agreed", "bigint");

        assert_eq!(
            types.reported("id"),
            None,
            "case \"two declarations\": a name declared under two types reports none, got {:?}",
            types.reported("id")
        );
        assert_eq!(
            types.reported("agreed"),
            Some("bigint"),
            "case \"one declaration twice\": expected bigint, got {:?}",
            types.reported("agreed")
        );
    }
}
