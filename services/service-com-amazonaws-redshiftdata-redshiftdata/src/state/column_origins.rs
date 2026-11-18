// SPDX-License-Identifier: CC0-1.0

//! Where one statement's result columns come from, and what they are declared
//! as.
//!
//! `ColumnMetadata` reports a column's ORIGIN — the schema and the table that
//! include it, and whether that table allows a NULL in it — and the type a
//! statement CASTS a computed column to. The engine's prepared statement
//! exposes neither, so both are read from the two things that do carry them:
//! the statement's own text, and the `CREATE` the relation it reads was made
//! with, as the engine's catalog holds it. Both are read by the dialect-aware
//! parser this crate already depends on, never by a scan of the text.
//!
//! Nothing here is guessed: a column reports an origin only when the statement
//! really reads it from a relation that really declares it, and a column whose
//! relation cannot be told apart from another's — one no relation in the
//! statement declares, or two of them do — reports none.

use std::collections::BTreeMap;

use sqlparser::ast::{Expr, Select, SelectItem, SetExpr, Statement, TableFactor};
use sqlparser::dialect::RedshiftSqlDialect;
use sqlparser::parser::Parser;

use super::column_metadata::{ColumnOrigin, ColumnType, DeclaredColumn};
use super::warehouse_types;

/// What a statement's text says about one of its result columns, beyond what
/// the engine's own prepared-statement metadata carries.
#[derive(Debug, Default, Clone)]
pub(super) struct ResultColumn {
    /// The type the statement CASTS this column to, when it casts it. The
    /// engine reports a declared type for a stored column and none for a
    /// computed one, so this is where a computed column's declared type — and
    /// with it its width and its scale — comes from.
    pub declared: Option<ColumnType>,
    /// The relation this column is read from, when it is read from one.
    pub origin: Option<ColumnOrigin>,
}

/// What the statement `sql` says about each of the result columns it produced,
/// which the engine named `columns`.
///
/// One entry per column, in the same order. A statement this cannot read — one
/// the parser does not accept, one that is not a single query — says nothing
/// about its columns, and every one of them gets the empty answer.
pub(super) fn result_columns(
    connection: &rusqlite::Connection,
    sql: &str,
    columns: &[String],
) -> Vec<ResultColumn> {
    let mut reported = vec![ResultColumn::default(); columns.len()];
    let Some(select) = sole_select(sql) else {
        return reported;
    };
    let relations = read_relations(connection, &select);
    for (projected, reported) in projected_columns(&select, columns)
        .into_iter()
        .zip(reported.iter_mut())
    {
        match projected {
            Projected::Cast(column_type) => reported.declared = Some(column_type),
            Projected::Column { qualifier, name } => {
                reported.origin = resolve(&relations, qualifier.as_deref(), &name);
            }
            Projected::Computed => {}
        }
    }
    reported
}

/// One relation a statement reads, and what it declares.
#[derive(Debug)]
struct Relation {
    /// The relation's ENGINE name, which `tableName` reports.
    table: String,
    /// The name the statement refers to it by: the alias it gave it, or the
    /// relation's own name.
    referred_to_as: String,
    /// What it declares about each of its columns, by lower-case column name
    /// ([`declared_columns`]).
    declared: BTreeMap<String, DeclaredColumn>,
}

/// The origin of the column a projection refers to, or `None` when no relation
/// of the statement declares it, or more than one does — in which case which
/// relation includes the column is not decided here.
fn resolve(relations: &[Relation], qualifier: Option<&str>, column: &str) -> Option<ColumnOrigin> {
    let column = column.to_ascii_lowercase();
    let mut declaring = relations.iter().filter(|relation| {
        qualifier.is_none_or(|qualifier| relation.referred_to_as.eq_ignore_ascii_case(qualifier))
            && relation.declared.contains_key(&column)
    });
    let relation = declaring.next()?;
    if declaring.next().is_some() {
        return None;
    }
    Some(ColumnOrigin {
        table: relation.table.clone(),
        declared: relation.declared.get(&column).cloned().unwrap_or_default(),
    })
}

/// The relations `select` reads, each with what the engine's catalog says it
/// declares. A relation the catalog holds no column declarations for — a view,
/// a subquery — contributes none, so no column is attributed to it.
fn read_relations(connection: &rusqlite::Connection, select: &Select) -> Vec<Relation> {
    let mut relations = Vec::new();
    for from in &select.from {
        for factor in std::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation)) {
            let TableFactor::Table { name, alias, .. } = factor else {
                continue;
            };
            let [part] = name.0.as_slice() else {
                continue;
            };
            let Some(table) = part.as_ident().map(|ident| ident.value.clone()) else {
                continue;
            };
            let referred_to_as = alias
                .as_ref()
                .map_or_else(|| table.clone(), |alias| alias.name.value.clone());
            let declared = declared_columns(relation_definition(connection, &table).as_deref());
            if declared.is_empty() {
                continue;
            }
            relations.push(Relation {
                table,
                referred_to_as,
                declared,
            });
        }
    }
    relations
}

/// What a relation DECLARES about each of its columns, by lower-case column
/// name.
///
/// Read from the `CREATE` statement the relation was made with, parsed as this
/// dialect — so a column is reported not nullable exactly when its declaration
/// carries `NOT NULL`, and carries the default exactly when it names one. Empty
/// for a relation with no column declarations to read: a view, and one whose
/// definition the parser does not accept.
pub(super) fn declared_columns(definition: Option<&str>) -> BTreeMap<String, DeclaredColumn> {
    let mut declared = BTreeMap::new();
    let Some(definition) = definition else {
        return declared;
    };
    let dialect = RedshiftSqlDialect {};
    let Ok(statements) = Parser::parse_sql(&dialect, definition) else {
        return declared;
    };
    let [Statement::CreateTable(created)] = statements.as_slice() else {
        return declared;
    };
    for column in &created.columns {
        let nullable = !column
            .options
            .iter()
            .any(|option| matches!(option.option, sqlparser::ast::ColumnOption::NotNull));
        // The expression the column's `DEFAULT` names, rendered by the parser
        // from what it read rather than cut out of the text.
        let column_default = column.options.iter().find_map(|option| match &option.option {
            sqlparser::ast::ColumnOption::Default(expr) => Some(expr.to_string()),
            _ => None,
        });
        declared.insert(
            column.name.value.to_ascii_lowercase(),
            DeclaredColumn {
                nullable,
                column_default,
            },
        );
    }
    declared
}

/// What one projected result column is.
#[derive(Debug, Clone)]
enum Projected {
    /// A column read from a relation, named as the statement refers to it and
    /// qualified as the statement qualified it.
    Column {
        qualifier: Option<String>,
        name: String,
    },
    /// A column the statement CASTS to a type, which is that column's declared
    /// type.
    Cast(ColumnType),
    /// A column the statement computes some other way, which belongs to no
    /// relation and declares no type.
    Computed,
}

/// Classify each of the result columns the engine named `columns`.
///
/// The statement's projection is read POSITIONALLY, which is the order the
/// engine answers the columns in. A projection carrying a wildcard expands to
/// however many columns its relations hold, which this list cannot count, so
/// each column is then read by the NAME the engine gave it — the name a
/// wildcard projects a relation's column under.
fn projected_columns(select: &Select, columns: &[String]) -> Vec<Projected> {
    let positional: Option<Vec<Projected>> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                Some(projected_expression(expr))
            }
            // A wildcard projects columns this list cannot count.
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => None,
        })
        .collect();
    match positional {
        Some(positional) if positional.len() == columns.len() => positional,
        // Either a wildcard, or a projection that did not line up with what the
        // engine answered: each column is read as the relation column of its
        // own name, which is what a wildcard projects, and a name no relation
        // declares resolves to no origin.
        _ => columns
            .iter()
            .map(|name| Projected::Column {
                qualifier: None,
                name: name.clone(),
            })
            .collect(),
    }
}

/// Classify one projected expression.
fn projected_expression(expr: &Expr) -> Projected {
    match expr {
        // The type the statement casts to is this column's declared type,
        // whichever way the cast is spelled; it is re-read through the same
        // resolution a stored column's declared type goes through.
        Expr::Cast { data_type, .. } => Projected::Cast(ColumnType::from_declared(
            &data_type.to_string().to_ascii_lowercase(),
        )),
        Expr::Identifier(name) => Projected::Column {
            qualifier: None,
            name: name.value.clone(),
        },
        // `<relation>.<column>`: the column is the last part, and the part
        // before it is the relation it is qualified with.
        Expr::CompoundIdentifier(parts) => match parts.as_slice() {
            [.., qualifier, name] => Projected::Column {
                qualifier: Some(qualifier.value.clone()),
                name: name.value.clone(),
            },
            _ => Projected::Computed,
        },
        // A call to the constructor of a warehouse TYPE the engine does not
        // carry ([`warehouse_types`]) produces a value of that type, so the
        // column it projects is declared that type — the same resolution a
        // cast to it goes through, because the dialect reader re-spells such a
        // cast as exactly this call.
        Expr::Function(function) => {
            match warehouse_types::type_named(&function.name.to_string()) {
                Some(type_name) => Projected::Cast(ColumnType::from_declared(type_name)),
                None => Projected::Computed,
            }
        }
        Expr::Nested(inner) => projected_expression(inner),
        _ => Projected::Computed,
    }
}

/// The single `SELECT` a statement is, or `None` for anything else — a
/// statement the parser does not accept, a submission carrying more than one, a
/// set operation or a query with a `WITH` clause, each of which draws its
/// columns from more than one query body.
///
/// A text longer than the engine itself accepts ([`super::MAX_SQL_BYTES`], its
/// `SQLITE_LIMIT_SQL_LENGTH`) is not read at all: the engine is what refuses
/// it, so the one pass this makes is bounded by the length of text the engine
/// accepts rather than by whatever a request carries.
fn sole_select(sql: &str) -> Option<Select> {
    if !usize::try_from(super::MAX_SQL_BYTES).is_ok_and(|max| sql.len() <= max) {
        return None;
    }
    let dialect = RedshiftSqlDialect {};
    let statements = Parser::parse_sql(&dialect, sql).ok()?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return None;
    };
    if query.with.is_some() {
        return None;
    }
    match query.body.as_ref() {
        SetExpr::Select(select) => Some(select.as_ref().clone()),
        _ => None,
    }
}

/// The definition the engine's catalog holds for one relation, matched the way
/// the engine matches an unquoted identifier: without regard to case.
fn relation_definition(connection: &rusqlite::Connection, table: &str) -> Option<String> {
    let mut listing = connection
        .prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table'")
        .ok()?;
    let mut rows = listing.query([]).ok()?;
    while let Some(row) = rows.next().ok()? {
        let name: String = row.get(0).ok()?;
        if name.eq_ignore_ascii_case(table) {
            return row.get::<_, Option<String>>(1).ok()?;
        }
    }
    None
}
